//! `fitting_containers_ships` — Containers, ALSC, Item Locking, Encoded Items,
//! SMA/Fleet Hangar, Ship Eject/Launch/Config, Newbie Ship, Ship Tree/Masteries, FitShips & Simulation.

#![forbid(unsafe_code)]

use super::*;
// structureAssetSafety is implemented once, in `station_structure::structure_upwell_ops`.

pub(crate) use super::fitting_containers_ships_ops::{
    append_alsc_log_snap, apply_add_move_kv_side_effects_in_snap, clear_container_unlocks_snap,
    container_alsc_logs_snap, container_config_snap, container_password_snap,
    custom_item_flag_snap, edge_distance_in_space_snap, foreign_ship_deny_err,
    is_container_unlocked_snap, list_with_optional_ship_unlock, set_alsc_item_locked_snap,
    set_container_config_snap, set_container_password_snap, set_container_unlocked_snap,
    set_custom_item_flag_snap, set_ship_access_config_snap, ship_access_config_snap,
    ship_password_snap, verify_nested_unit_list_in_snap, verify_ship_proximity_in_snap,
    AlscLogEntry, ContainerConfig, ShipAccessConfig,
};
#[cfg(test)]
pub(crate) use super::fitting_containers_ships_ops::MAX_ALSC_LOGS_PER_CONTAINER;

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
        if snap.items.contains_key(&item_id)
            || snap.structures.contains_key(&item_id)
            || snap.characters.contains_key(&item_id)
            || snap.corporations.contains_key(&item_id)
            || snap.alliances.contains_key(&item_id)
            || snap.wars.contains_key(&item_id)
            || snap.fleets.contains_key(&item_id)
        {
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
    let cfg = ship_access_config_snap(snap, host_ship_id);
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
        let already_unlocked = is_container_unlocked_snap(snap, host_ship_id, char_id);
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

pub(crate) fn record_container_add_log_in_snap(
    sde: &eve_sde::SdeRegistry,
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    item_id: i64,
    moved_tid: u32,
    moved_qty: i32,
    container_id: i64,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    let is_alsc = snap.items.get(&container_id).is_some_and(|i| {
        let grp = sde.types.get(&i.type_id).map(|t| t.group_id).unwrap_or(0);
        matches!(grp, 340 | 448)
    });
    if !is_alsc {
        return Ok(());
    }
    if container_config_snap(snap, container_id).lock_by_default {
        set_alsc_item_locked_snap(snap, item_id, true, now_unix)?;
        append_alsc_log_snap(
            snap,
            container_id,
            AlscLogEntry {
                timestamp_unix: now_unix,
                actor_id: char_id,
                action: "ALSCActionLock".into(),
                item_id,
                item_type_id: moved_tid,
                quantity: moved_qty,
            },
            now_unix,
        )?;
    }
    append_alsc_log_snap(
        snap,
        container_id,
        AlscLogEntry {
            timestamp_unix: now_unix,
            actor_id: char_id,
            action: "ALSCActionAdd".into(),
            item_id,
            item_type_id: moved_tid,
            quantity: moved_qty,
        },
        now_unix,
    )?;
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
    Ok(())
}

#[cfg(test)]
pub(crate) fn record_container_add_log(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    moved_tid: u32,
    moved_qty: i32,
    container_id: i64,
) {
    let now_unix = ctx.now_unix_secs();
    if let Err(err) = ctx.db.try_with_mut_snapshot(|snap| {
        record_container_add_log_in_snap(
            &ctx.sde,
            snap,
            char_id,
            item_id,
            moved_tid,
            moved_qty,
            container_id,
            now_unix,
        )
    }) {
        tracing::error!(error = %err.message, container_id, "record_container_add_log failed");
    }
}

pub(crate) fn record_container_take_log_in_snap(
    sde: &eve_sde::SdeRegistry,
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    item_id: i64,
    moved_tid: u32,
    moved_qty: i32,
    container_id: i64,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    let is_alsc = snap.items.get(&container_id).is_some_and(|i| {
        let grp = sde.types.get(&i.type_id).map(|t| t.group_id).unwrap_or(0);
        matches!(grp, 340 | 448)
    });
    if !is_alsc {
        return Ok(());
    }
    append_alsc_log_snap(
        snap,
        container_id,
        AlscLogEntry {
            timestamp_unix: now_unix,
            actor_id: char_id,
            action: "ALSCActionTake".into(),
            item_id,
            item_type_id: moved_tid,
            quantity: moved_qty,
        },
        now_unix,
    )?;
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
    Ok(())
}

#[cfg(test)]
pub(crate) fn record_container_take_log(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    moved_tid: u32,
    moved_qty: i32,
    container_id: i64,
) {
    let now_unix = ctx.now_unix_secs();
    if let Err(err) = ctx.db.try_with_mut_snapshot(|snap| {
        record_container_take_log_in_snap(
            &ctx.sde,
            snap,
            char_id,
            item_id,
            moved_tid,
            moved_qty,
            container_id,
            now_unix,
        )
    }) {
        tracing::error!(error = %err.message, container_id, "record_container_take_log failed");
    }
}

pub(crate) fn validate_src_container_access_in_snap(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    moving_item: &eve_db::InventoryItemRecord,
    dest_loc: i64,
    supplied_pw: Option<&str>,
) -> Result<(), BridgeRpcError> {
    if moving_item.location_id == dest_loc {
        return Ok(());
    }
    let Some(src_cont) = snap.items.get(&moving_item.location_id) else {
        return Ok(());
    };
    let src_grp = ctx.sde.types.get(&src_cont.type_id).map(|t| t.group_id).unwrap_or(0);
    if !is_container_group(src_grp, src_cont.type_id) {
        return Ok(());
    }
    if src_grp == crate::services::mobile_deployables::GROUP_MOBILE_DEPOT
        && src_cont.owner_id != char_id
    {
        return Err(BridgeRpcError::with_client_error(
            "OwnershipViolation",
            "Only the owner may take items from this Mobile Depot",
            eve_proto::ClientError::CustomInfo(
                "Only the owner may take items from this Mobile Depot".into(),
            ),
        ));
    }
    if matches!(src_grp, 340 | 448) || container_password_snap(snap, src_cont.item_id, 1).is_some()
    {
        validate_container_password_in_snap(snap, src_cont.item_id, char_id, supplied_pw, true)?;
    }
    let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
    let cont_corp = if snap.corporations.contains_key(&src_cont.owner_id) {
        src_cont.owner_id
    } else if (116..=122).contains(&(src_cont.flag as i64))
        && char_corp > 0
        && moving_item.owner_id == char_corp
    {
        char_corp
    } else {
        0
    };
    let is_auth_space_storage = (30_000_000..=39_999_999).contains(&src_cont.location_id)
        && crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.authorized_shipping_storage
                .contains(&(src_cont.item_id, char_id))
        });
    if cont_corp > 0 && cont_corp != char_corp {
        if src_cont.type_id != 23 && !is_auth_space_storage {
            let msg = format!(
                "Character {char_id} is not in corporation {cont_corp} owning container {}",
                src_cont.item_id
            );
            return Err(BridgeRpcError::with_client_error(
                "CorpRoleDenied",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            ));
        }
    } else if cont_corp > 0 && cont_corp == char_corp {
        let (is_ceo, roles) = crate::services::inventory_bound::get_effective_corp_roles_at_location_in_snap_at(
            snap,
            cont_corp,
            char_id,
            Some(src_cont.location_id),
            ctx.now_unix(),
        );
        let src_flag = src_cont.flag as i64;
        let div_idx = if (116..=122).contains(&src_flag) {
            (src_flag - 116) as usize
        } else {
            0
        };
        let req_role = eve_db::corp_roles::ROLE_CONTAINER_CAN_TAKE[div_idx.min(6)];
        if !is_ceo && (roles & (eve_db::corp_roles::ROLE_DIRECTOR | req_role)) == 0 {
            let msg = format!(
                "Corporation container take role denied for division {}",
                div_idx + 1
            );
            return Err(BridgeRpcError::with_client_error(
                "CorpRoleDenied",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_container_password_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    container_id: i64,
    char_id: i64,
    supplied_pw: Option<&str>,
    check_per_action: bool,
) -> Result<(), BridgeRpcError> {
    if let Some(req_pw) = container_password_snap(snap, container_id, 1) {
        let need_check = (check_per_action
            && container_config_snap(snap, container_id).password_per_action)
            || !is_container_unlocked_snap(snap, container_id, char_id);
        if !req_pw.is_empty() && need_check && supplied_pw != Some(req_pw.as_str()) {
            let msg = format!("Password required to access secure container {container_id}");
            return Err(BridgeRpcError::with_client_error(
                "ALSCPasswordNeededToOpen",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            ));
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn finalize_add_move_side_effects_in_snap(
    ctx: &ServerContext,
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    pending_alsc_take: Option<&(i64, AlscLogEntry)>,
    custom_flag_ids: &[i64],
    custom_flag_opt: Option<u16>,
    add_item_id: i64,
    take_item_id: i64,
    moved_tid: u32,
    moved_qty: i32,
    src_loc: i64,
    src_flag_u16: u16,
    new_loc: i64,
    dest_flag_u16: u16,
    supplied_pw: Option<&str>,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    apply_add_move_kv_side_effects_in_snap(
        snap,
        pending_alsc_take,
        custom_flag_ids,
        custom_flag_opt,
        now_unix,
    )?;
    record_inv_move_logs_in_snap(
        &ctx.sde,
        snap,
        char_id,
        add_item_id,
        take_item_id,
        moved_tid,
        moved_qty,
        src_loc,
        new_loc,
        pending_alsc_take.is_none(),
        now_unix,
    )?;
    record_ship_bay_unlock_for_ancestors_snap(
        ctx,
        snap,
        char_id,
        new_loc,
        dest_flag_u16,
        src_loc,
        src_flag_u16,
        supplied_pw,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn record_inv_move_logs_in_snap(
    sde: &eve_sde::SdeRegistry,
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    add_item_id: i64,
    take_item_id: i64,
    moved_tid: u32,
    moved_qty: i32,
    old_loc: i64,
    new_loc: i64,
    record_take: bool,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    record_container_add_log_in_snap(
        sde, snap, char_id, add_item_id, moved_tid, moved_qty, new_loc, now_unix,
    )?;
    if record_take && old_loc != new_loc && old_loc > 0 {
        record_container_take_log_in_snap(
            sde, snap, char_id, take_item_id, moved_tid, moved_qty, old_loc, now_unix,
        )?;
    }
    Ok(())
}

pub(crate) fn record_container_rename_log(
    ctx: &ServerContext,
    char_id: i64,
    container_id: i64,
    tid: u32,
) {
    let now_unix = ctx.now_unix_secs();
    if let Err(err) = ctx.db.try_with_mut_snapshot(|snap| {
        append_alsc_log_snap(
            snap,
            container_id,
            AlscLogEntry {
                timestamp_unix: now_unix,
                actor_id: char_id,
                action: "ALSCActionRename".into(),
                item_id: container_id,
                item_type_id: tid,
                quantity: 1,
            },
            now_unix,
        )
    }) {
        tracing::error!(error = %err.message, container_id, "record_container_rename_log failed");
    }
}

pub(crate) fn container_has_active_audit_log_snap(
    snap: &eve_db::WorldStateSnapshot,
    container_id: i64,
    now_unix: u64,
) -> bool {
    container_alsc_logs_snap(snap, container_id)
        .iter()
        .any(|e| now_unix.saturating_sub(e.timestamp_unix) < 21 * 86_400)
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
                client_error: None,
            });
        }
        if rec.flag as u16 == 36 || rec.type_id == 60 || in_active_wrap(rec.item_id, rec.location_id) {
            return Err(BridgeRpcError {
                code: "AssetSafetyLocked".into(),
                message: format!("Item {} is sealed in Asset Safety until retrieval fees are paid", rec.item_id),
                client_error: None,
            });
        }
        if rec.flag == InventoryFlag::Locked
            || raw_flag == 63
            || snap.is_item_locked_in_contract_or_job(rec.item_id)
        {
            return Err(BridgeRpcError {
                code: "ItemLocked".into(),
                message: format!("Item {} is locked by contract, job, or corporation lockdown", rec.item_id),
                client_error: None,
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

/// Checks whether the target ship has an SMA (`flag 90`) or Fleet Hangar (`flag 155`).
pub(crate) fn validate_ship_bay_existence_in_snap(
    ctx: &ServerContext,
    dest_item: &eve_db::InventoryItemRecord,
    flag_u16: u16,
) -> Result<(), BridgeRpcError> {
    let Some(dest_type) = ctx.sde.types.get(&dest_item.type_id) else {
        return Ok(());
    };
    if dest_type.category_id != 6 {
        return Ok(());
    }
    if flag_u16 == 90 {
        let sma_cap = dest_type.default_attributes.get(&908).copied().unwrap_or(0.0);
        if sma_cap <= 0.0 {
            return Err(BridgeRpcError {
                code: "NoShipMaintenanceBay".into(),
                message: format!("Ship {} does not have a Ship Maintenance Bay", dest_type.name),
                client_error: None,
            });
        }
    } else if flag_u16 == 155 {
        let fh_cap = dest_type.default_attributes.get(&912).copied().unwrap_or(0.0);
        if fh_cap <= 0.0 {
            return Err(BridgeRpcError {
                code: "NoFleetHangar".into(),
                message: format!("Ship {} does not have a Fleet Hangar", dest_type.name),
                client_error: None,
            });
        }
    }
    Ok(())
}

/// Validates per-item SMA rules (`category_id == 6`, assembled singleton, no non-charge cargo).
pub(crate) fn validate_ship_bay_item_rules_in_snap(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    item_id: i64,
    moving_item: &eve_db::InventoryItemRecord,
    dest_item: &eve_db::InventoryItemRecord,
    flag_u16: u16,
) -> Result<(), BridgeRpcError> {
    let Some(dest_type) = ctx.sde.types.get(&dest_item.type_id) else {
        return Ok(());
    };
    if dest_type.category_id != 6 || flag_u16 != 90 {
        return Ok(());
    }
    let moving_cat = ctx
        .sde
        .types
        .get(&moving_item.type_id)
        .map(|t| t.category_id)
        .unwrap_or(0);
    if moving_cat != 6 {
        return Err(BridgeRpcError {
            code: "CannotStoreCargoInSMA".into(),
            message: "Only ships can be stored in a Ship Maintenance Bay".into(),
            client_error: None,
        });
    }
    if !moving_item.singleton {
        return Err(BridgeRpcError {
            code: "OnlyAssembledShipsInSMA".into(),
            message: "Only assembled ships can be stored in a Ship Maintenance Bay".into(),
            client_error: None,
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
            client_error: None,
        });
    }
    Ok(())
}

/// Validates batch volume capacity for SMA (`flag 90 | 138`) and Fleet Hangar (`flag 155`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_ship_bay_batch_capacity_in_snap(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    dest_item: &eve_db::InventoryItemRecord,
    new_loc: i64,
    flag_u16: u16,
    excluded_item_ids: &[i64],
    batch_total_vol: f64,
    typed_capacity_error: bool,
) -> Result<(), BridgeRpcError> {
    let Some(dest_type) = ctx.sde.types.get(&dest_item.type_id) else {
        return Ok(());
    };
    if dest_type.category_id != 6 {
        return Ok(());
    }
    if flag_u16 == 90 {
        let sma_cap = dest_type.default_attributes.get(&908).copied().unwrap_or(0.0);
        if sma_cap <= 0.0 {
            return Err(BridgeRpcError {
                code: "NoShipMaintenanceBay".into(),
                message: format!("Ship {} does not have a Ship Maintenance Bay", dest_type.name),
                client_error: None,
            });
        }
        let used_sma: f64 = snap
            .items
            .values()
            // Stored hulls carry `invFlagShipHangar` (90) or `SpecializedShipHold` (138, set by `ship.StoreVessel`).
            .filter(|i| {
                i.location_id == new_loc
                    && matches!(i.flag as u16, 90 | 138)
                    && !excluded_item_ids.contains(&i.item_id)
            })
            .map(|i| effective_item_unit_volume(&ctx.sde, i) * (i.quantity.max(1) as f64))
            .sum();
        if used_sma + batch_total_vol > sma_cap + 1e-4 {
            return Err(if typed_capacity_error {
                crate::services::inventory_fitting::ship_hold_capacity_error(flag_u16, sma_cap, used_sma, batch_total_vol)
            } else {
                BridgeRpcError {
                    code: "CapacityExceeded".into(),
                    message: format!("Ship Maintenance Bay capacity {sma_cap} m³ exceeded"),
                    client_error: None,
                }
            });
        }
    } else if flag_u16 == 155 {
        let fh_cap = dest_type.default_attributes.get(&912).copied().unwrap_or(0.0);
        if fh_cap <= 0.0 {
            return Err(BridgeRpcError {
                code: "NoFleetHangar".into(),
                message: format!("Ship {} does not have a Fleet Hangar", dest_type.name),
                client_error: None,
            });
        }
        let used_fh: f64 = snap
            .items
            .values()
            .filter(|i| {
                i.location_id == new_loc
                    && (i.flag as u16) == 155
                    && !excluded_item_ids.contains(&i.item_id)
            })
            .map(|i| effective_item_unit_volume(&ctx.sde, i) * (i.quantity.max(1) as f64))
            .sum();
        if used_fh + batch_total_vol > fh_cap + 1e-4 {
            return Err(if typed_capacity_error {
                crate::services::inventory_fitting::ship_hold_capacity_error(flag_u16, fh_cap, used_fh, batch_total_vol)
            } else {
                BridgeRpcError {
                    code: "CapacityExceeded".into(),
                    message: format!("Fleet Hangar capacity {fh_cap} m³ exceeded"),
                    client_error: None,
                }
            });
        }
    }
    Ok(())
}

/// Validates SMA (`flag 90`) and Fleet Hangar (`flag 155`) fleet/corp ACL & password.
pub(crate) fn validate_ship_bay_acl_in_snap(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    new_loc: i64,
    dest_item: &eve_db::InventoryItemRecord,
    flag_u16: u16,
    supplied_pw: Option<&str>,
) -> Result<(), BridgeRpcError> {
    let Some(dest_type) = ctx.sde.types.get(&dest_item.type_id) else {
        return Ok(());
    };
    if dest_type.category_id != 6 {
        return Ok(());
    }
    if flag_u16 == 90 {
        if !can_character_access_ship_bay_with_password_snap(snap, char_id, new_loc, dest_item.owner_id, true, supplied_pw) {
            return Err(BridgeRpcError {
                code: "SMAAccessDenied".into(),
                message: "Ship Maintenance Bay access denied by ship configuration or password".into(),
                client_error: None,
            });
        }
    } else if flag_u16 == 155 {
        if !can_character_access_ship_bay_with_password_snap(snap, char_id, new_loc, dest_item.owner_id, false, supplied_pw) {
            return Err(BridgeRpcError {
                code: "FleetHangarAccessDenied".into(),
                message: "Fleet Hangar access denied by ship configuration or password".into(),
                client_error: None,
            });
        }
    }
    Ok(())
}

/// Shared in-snapshot validation for SMA (`flag 90`) and Fleet Hangar (`flag 155`) rules,
/// capacity, and bay ACL/password. Used by pre-validation and inside `Add`'s write transaction.
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_ship_bay_capacity_and_rules_in_snap(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    item_id: i64,
    moving_item: &eve_db::InventoryItemRecord,
    new_loc: i64,
    dest_item: &eve_db::InventoryItemRecord,
    flag_u16: u16,
    moving_total_vol: f64,
    supplied_pw: Option<&str>,
    typed_capacity_error: bool,
) -> Result<(), BridgeRpcError> {
    validate_ship_bay_existence_in_snap(ctx, dest_item, flag_u16)?;
    validate_ship_bay_item_rules_in_snap(ctx, snap, item_id, moving_item, dest_item, flag_u16)?;
    validate_ship_bay_batch_capacity_in_snap(
        ctx,
        snap,
        dest_item,
        new_loc,
        flag_u16,
        &[item_id],
        moving_total_vol,
        typed_capacity_error,
    )?;
    validate_ship_bay_acl_in_snap(ctx, snap, char_id, new_loc, dest_item, flag_u16, supplied_pw)
}

/// Persists a character's password unlock for any foreign ship shared bay (`90 | 138 | 155`)
/// along the destination or source ancestor chain once a transfer/merge has succeeded.
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_ship_bay_unlock_for_ancestors_snap(
    ctx: &ServerContext,
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    dest_loc: i64,
    dest_flag_u16: u16,
    src_loc: i64,
    src_flag_u16: u16,
    supplied_pw: Option<&str>,
) -> Result<(), BridgeRpcError> {
    let Some(pw) = supplied_pw else {
        return Ok(());
    };
    let mut ship_ids = Vec::new();
    for (start, flag0) in [(dest_loc, dest_flag_u16), (src_loc, src_flag_u16)] {
        let mut cur = start;
        let mut cur_flag = flag0;
        for _ in 0..8 {
            let Some(node) = snap.items.get(&cur) else {
                break;
            };
            let is_ship = ctx.sde.types.get(&node.type_id).is_some_and(|t| t.category_id == 6);
            if is_ship && node.owner_id != char_id && matches!(cur_flag, 90 | 138 | 155) {
                ship_ids.push(node.item_id);
            }
            cur_flag = node.flag as u16;
            cur = node.location_id;
        }
    }
    if ship_ids.is_empty() {
        return Ok(());
    }
    let now_unix = ctx.now_unix_secs();
    for sid in ship_ids {
        if ship_password_snap(snap, sid).as_deref() == Some(pw) {
            set_container_unlocked_snap(snap, sid, char_id, true, now_unix)?;
        }
    }
    Ok(())
}

/// Validates container rules, ALSC locks/passwords, SMA (`flag 90`), and Fleet Hangar (`flag 155`) on item move.
pub(crate) fn validate_container_dest_rules_in_snap(
    ctx: &ServerContext,
    char_id: i64,
    moving_item: &eve_db::InventoryItemRecord,
    dest_item: &eve_db::InventoryItemRecord,
    dest_type: &eve_sde::InvType,
) -> Result<bool, BridgeRpcError> {
    let moving_grp = ctx
        .sde
        .types
        .get(&moving_item.type_id)
        .map(|t| t.group_id)
        .unwrap_or(0);
    // Station Container (`group 448`, unpacked volume >= 500,000 m3) cannot be moved into a ship when assembled
    if moving_grp == 448 && moving_item.singleton && dest_type.category_id == 6 {
        return Err(BridgeRpcError {
            code: "CapacityExceeded".into(),
            message: "Assembled Station Containers cannot be loaded into a ship".into(),
            client_error: None,
        });
    }
    if matches!(dest_type.group_id, 513 | 902)
        && dest_type.category_id == 6
        && moving_item.singleton
        && is_container_group(moving_grp, moving_item.type_id)
        && moving_grp != 649
    {
        return Err(BridgeRpcError {
            code: "OnlyFreightContainersAllowedInFreighters".into(),
            message: "Freight containers are the only containers allowed assembled in freighter holds"
                .into(),
            client_error: None,
        });
    }
    if !is_container_group(dest_type.group_id, dest_type.type_id) {
        return Ok(false);
    }
    if dest_type.group_id == crate::services::mobile_deployables::GROUP_CRAB_BEACON {
        return Err(BridgeRpcError::user_error("CannotPutItemsInCrabBeacon"));
    }
    if dest_type.group_id == 1247 {
        return Err(BridgeRpcError {
            code: "CannotAddToSiphon".into(),
            message: "Items cannot be placed into a Mobile Siphon Unit".into(),
            client_error: None,
        });
    }
    if dest_type.group_id == crate::services::mobile_deployables::GROUP_MOBILE_DEPOT {
        if dest_item.owner_id != char_id {
            return Err(BridgeRpcError {
                code: "OwnershipViolation".into(),
                message: "Only the owner may store items in this Mobile Depot".into(),
                client_error: None,
            });
        }
    } else if !dest_item.singleton {
        return Err(BridgeRpcError {
            code: "ContainerNotAssembled".into(),
            message: format!(
                "Target container {} must be assembled before storing items",
                dest_item.item_id
            ),
            client_error: None,
        });
    }
    if is_container_group(moving_grp, moving_item.type_id) {
        return Err(BridgeRpcError {
            code: "CannotPutContainerInContainer".into(),
            message: "Cannot place a container inside another container".into(),
            client_error: None,
        });
    }
    Ok(true)
}

pub(crate) fn validate_container_capacity_in_snap(
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    dest_item: &eve_db::InventoryItemRecord,
    dest_type: &eve_sde::InvType,
    new_loc: i64,
    excluded_ids: &[i64],
    moving_vol: f64,
) -> Result<(), BridgeRpcError> {
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
            client_error: None,
        });
    }
    let used: f64 = snap
        .items
        .values()
        .filter(|i| i.location_id == new_loc && !excluded_ids.contains(&i.item_id))
        .map(|i| effective_item_unit_volume(sde, i) * (i.quantity as f64))
        .sum();
    if used + moving_vol > cap + 1e-4 {
        return Err(BridgeRpcError {
            code: "CapacityExceeded".into(),
            message: format!(
                "Container capacity of {cap} m³ exceeded (used {used:.1} m³, adding {moving_vol:.1} m³)"
            ),
            client_error: None,
        });
    }
    Ok(())
}

pub(crate) fn validate_container_and_bay_move(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    new_loc: i64,
    flag_u16: u16,
    kwargs: &HashMap<String, PyRep>,
    requested_qty: Option<i64>,
) -> Result<(), BridgeRpcError> {
    validate_container_and_bay_move_impl(ctx, char_id, item_id, new_loc, flag_u16, &[], kwargs, requested_qty, false)
}

/// Same validation as [`validate_container_and_bay_move`] for `invbroker.Add`, emitting typed V24
/// `NotEnoughSpecialBaySpace` user errors for SMA (`flag 90`) and Fleet Hangar (`flag 155`) overflows.
#[allow(dead_code)]
pub(crate) fn validate_container_and_bay_move_for_add(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    new_loc: i64,
    flag_u16: u16,
    kwargs: &HashMap<String, PyRep>,
    requested_qty: Option<i64>,
) -> Result<(), BridgeRpcError> {
    validate_container_and_bay_move_impl(ctx, char_id, item_id, new_loc, flag_u16, &[], kwargs, requested_qty, true)
}

/// Variant of [`validate_container_and_bay_move_for_add`] that also inspects positional `args` for password fallback.
#[allow(clippy::too_many_arguments)]
pub(crate) fn validate_container_and_bay_move_for_add_with_args(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    new_loc: i64,
    flag_u16: u16,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
    requested_qty: Option<i64>,
) -> Result<(), BridgeRpcError> {
    validate_container_and_bay_move_impl(ctx, char_id, item_id, new_loc, flag_u16, args, kwargs, requested_qty, true)
}

#[allow(clippy::too_many_arguments)]
fn validate_container_and_bay_move_impl(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
    new_loc: i64,
    flag_u16: u16,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
    requested_qty: Option<i64>,
    typed_capacity_error: bool,
) -> Result<(), BridgeRpcError> {
    let supplied_pw = kwargs
        .get("password")
        .and_then(|v| v.as_str())
        .or_else(|| args.iter().find_map(|a| a.as_str()));
    if item_id == new_loc {
        return Err(BridgeRpcError { code: "UserError".into(), message: "CannotStoreShipInItself".into(), client_error: None });
    }
    if flag_u16 == 72 {
        return Err(BridgeRpcError {
            code: "ImpoundedItem".into(),
            message: "Cannot move items into Impounded flag".into(),
            client_error: None,
        });
    }
    if flag_u16 == 36 {
        return Err(BridgeRpcError {
            code: "AssetSafetyLocked".into(),
            message: "Cannot move items into Asset Safety flag".into(),
            client_error: None,
        });
    }
    if flag_u16 == 63 {
        return Err(BridgeRpcError {
            code: "ItemLocked".into(),
            message: "Cannot move items into Locked flag".into(),
            client_error: None,
        });
    }
    let alsc_locked = ctx.db.with_snapshot(|snap| -> Result<bool, BridgeRpcError> {
        if let Some(moving_item) = snap.items.get(&item_id) {
            check_item_lock_impound_asset_safety_in_snap(snap, moving_item)?;
        }
        if let Some(dest_item) = snap.items.get(&new_loc) {
            check_item_lock_impound_asset_safety_in_snap(snap, dest_item)?;
        }
        Ok(snap.is_item_in_alsc_lock(item_id))
    })?;
    if alsc_locked {
        return Err(BridgeRpcError {
            code: "ALSCItemLocked".into(),
            message: format!("Item {item_id} is locked in an Audit Log Secure Container"),
            client_error: None,
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
        let src_has_pw = container_password_snap(snap, src_loc, 1).is_some();
        Some((src_loc, src_cont.flag as i64, src_cont.location_id, cont_corp, char_corp, src_grp, src_cont.owner_id, src_has_pw))
    });
    if let Some((src_loc, src_cont_flag, src_cont_station, cont_corp, char_corp, src_grp, src_cont_owner, src_has_pw)) = src_cont_info {
        if src_grp == crate::services::mobile_deployables::GROUP_CRAB_BEACON {
            crate::services::pve_crab_beacons::check_crab_loot_access(ctx, src_loc, char_id, false)?;
        }
        if src_grp == crate::services::mobile_deployables::GROUP_MOBILE_DEPOT {
            if src_cont_owner != char_id {
                return Err(BridgeRpcError { code: "OwnershipViolation".into(), message: "Only the owner may take items from this Mobile Depot".into(), client_error: None });
            }
        }
        if matches!(src_grp, 340 | 448) || src_has_pw {
            let now_unix = ctx.now_unix_secs();
            ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                if let Some(req_pw) = container_password_snap(snap, src_loc, 1) {
                    let per_action = container_config_snap(snap, src_loc).password_per_action;
                    if !req_pw.is_empty()
                        && (per_action || !is_container_unlocked_snap(snap, src_loc, char_id))
                    {
                        let provided = supplied_pw.unwrap_or("");
                        if provided != req_pw {
                            return Err(BridgeRpcError {
                                code: "ALSCPasswordNeededToOpen".into(),
                                message: format!("Password required to take items from secure container {src_loc}"),
                                client_error: None,
                            });
                        }
                        set_container_unlocked_snap(snap, src_loc, char_id, true, now_unix)?;
                    }
                }
                Ok(())
            })?;
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
                    client_error: None,
                });
            }
        }
    }

    let snap_check = ctx.db.with_snapshot(|snap| {
        let Some(moving_item) = snap.items.get(&item_id) else {
            return Ok(None);
        };
        if moving_item.flag == InventoryFlag::Locked
            || snap.is_item_or_ancestor_locked_or_impounded(item_id)
            || (snap.items.contains_key(&new_loc)
                && snap.is_item_or_ancestor_locked_or_impounded(new_loc))
        {
            return Err(BridgeRpcError { code: "ItemLocked".into(), message: format!("Item {item_id} is locked by contract or job"), client_error: None });
        }
        let moving_type = ctx.sde.types.get(&moving_item.type_id);
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

        if validate_container_dest_rules_in_snap(ctx, char_id, moving_item, dest_item, dest_type)? {
            validate_container_capacity_in_snap(
                &ctx.sde,
                snap,
                dest_item,
                dest_type,
                new_loc,
                &[item_id],
                moving_total_vol,
            )?;
            return Ok(Some((dest_type.group_id, moving_item.type_id, moving_item.quantity)));
        }

        if matches!(flag_u16, 90 | 155) && dest_type.category_id == 6 {
            validate_ship_bay_capacity_and_rules_in_snap(
                ctx,
                snap,
                char_id,
                item_id,
                moving_item,
                new_loc,
                dest_item,
                flag_u16,
                moving_total_vol,
                supplied_pw,
                typed_capacity_error,
            )?;
        }

        Ok(None)
    })?;

    if snap_check.is_some() {
        let now_unix = ctx.now_unix_secs();
        ctx.db.try_with_mut_snapshot(|snap| -> Result<Result<(), BridgeRpcError>, BridgeRpcError> {
            let Some(req_pw) = container_password_snap(snap, new_loc, 1) else {
                return Ok(Ok(()));
            };
            let cfg = container_config_snap(snap, new_loc);
            let need_check = cfg.password_per_action || !is_container_unlocked_snap(snap, new_loc, char_id);
            if !req_pw.is_empty() && need_check {
                let provided = supplied_pw.unwrap_or("");
                let tid = snap.items.get(&new_loc).map(|i| i.type_id).unwrap_or(0);
                if provided != req_pw {
                    append_alsc_log_snap(
                        snap,
                        new_loc,
                        AlscLogEntry {
                            timestamp_unix: now_unix,
                            actor_id: char_id,
                            action: "ALSCActionPasswordFailure".into(),
                            item_id: new_loc,
                            item_type_id: tid,
                            quantity: 1,
                        },
                        now_unix,
                    )?;
                    return Ok(Err(BridgeRpcError { code: "ALSCPasswordNeededToOpen".into(), message: format!("Password required to access secure container {new_loc}"), client_error: None }));
                }
                append_alsc_log_snap(
                    snap,
                    new_loc,
                    AlscLogEntry {
                        timestamp_unix: now_unix,
                        actor_id: char_id,
                        action: "ALSCActionPasswordSuccess".into(),
                        item_id: new_loc,
                        item_type_id: tid,
                        quantity: 1,
                    },
                    now_unix,
                )?;
                set_container_unlocked_snap(snap, new_loc, char_id, true, now_unix)?;
            }
            Ok(Ok(()))
        })??;
    }
    if let Some((grp, _, _)) = snap_check {
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
        return Err(BridgeRpcError { code: "InvalidArgument".into(), message: "Valid itemID required".into(), client_error: None });
    }
    let (item_type_id, owner_id, location_id, singleton) = ctx.db.with_snapshot(|snap| {
        let it = snap.items.get(&item_id).ok_or_else(|| BridgeRpcError {
            code: "ItemNotFound".into(),
            message: format!("Container item {item_id} not found"),
            client_error: None,
        })?;
        Ok::<_, BridgeRpcError>((it.type_id, it.owner_id, it.location_id, it.singleton))
    })?;
    if !singleton {
        return Err(BridgeRpcError {
            code: "CantOpenPackagedItem".into(),
            message: format!("Item {item_id} is packaged and cannot be opened. Assemble it first."),
            client_error: None,
        });
    }
    let grp = ctx.sde.types.get(&item_type_id).map(|t| t.group_id).unwrap_or(0);
    let cat = ctx.sde.types.get(&item_type_id).map(|t| t.category_id).unwrap_or(0);
    if !(is_container_group(grp, item_type_id) || grp == 186 || cat == 6 || cat == 2 || item_type_id == 23 || grp == 1246 || grp == 1250) {
        return Err(BridgeRpcError {
            code: "NotAContainer".into(),
            message: format!("Item {item_id} (type {item_type_id}) is not a container"),
            client_error: None,
        });
    }
    if grp == crate::services::mobile_deployables::GROUP_CRAB_BEACON {
        crate::services::pve_crab_beacons::check_crab_loot_access(ctx, item_id, char_id, false)?;
    } else if (grp == 1246 || grp == 1250) && owner_id != char_id {
        return Err(BridgeRpcError {
            code: "OwnershipViolation".into(),
            message: "Only the owner may access this structure's inventory".into(),
            client_error: None,
        });
    }
    if grp == 1246 {
        crate::services::mobile_deployables::touch_mobile_depot_interaction(ctx, item_id);
    }
    crate::services::character_skill_ops::verify_in_space_container_proximity(ctx, char_id, location_id, item_id).await?;
    let pw_locked = ctx.db.with_snapshot(|snap| {
        container_password_snap(snap, item_id, 1).is_some()
            && !is_container_unlocked_snap(snap, item_id, char_id)
    });
    if pw_locked {
        return Err(BridgeRpcError {
            code: "ContainerIsLocked".into(),
            message: format!("Container {item_id} is password-locked. Enter password to unlock."),
            client_error: None,
        });
    }
    Ok(())
}

pub(crate) fn handle_alsc_lock_unlock(
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
        return Err(BridgeRpcError { code: "InvalidContainer".into(), message: "Valid containerID required for ALSC lock/unlock".into(), client_error: None });
    }
    let item_ids: Vec<i64> = match args.get(1).or_else(|| args.first()) {
        Some(PyRep::List(v) | PyRep::Tuple(v)) => v.iter().filter_map(|x| x.as_i64()).collect(),
        Some(PyRep::Integer(id)) if *id != container_id => vec![*id],
        _ => kwargs.get("itemIDs").and_then(|v| match v { PyRep::List(l) | PyRep::Tuple(l) => Some(l.iter().filter_map(|x| x.as_i64()).collect()), _ => None }).unwrap_or_default(),
    };
    let now_unix = ctx.now_unix_secs();
    let supplied_pw = rpc.opt_str(2, "password")?.map(str::to_string);
    let notifications = ctx.db.try_with_mut_snapshot(|snap| -> Result<Result<Vec<(i64, i64, i64)>, BridgeRpcError>, BridgeRpcError> {
        let Some(cont) = snap.items.get(&container_id).cloned() else {
            return Ok(Err(BridgeRpcError { code: "ContainerNotFound".into(), message: format!("Container {container_id} not found"), client_error: None }));
        };
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
            return Ok(Err(BridgeRpcError { code: "ColocationRequired".into(), message: format!("Must be co-located with container {container_id} to lock/unlock items"), client_error: None }));
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
            return Ok(Err(BridgeRpcError { code: "ALSCAccessDenied".into(), message: format!("Not authorized to lock/unlock items in container {container_id}"), client_error: None }));
        }
        for &iid in &item_ids {
            let Some(it) = snap.items.get(&iid) else {
                return Ok(Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Item {iid} not found"), client_error: None }));
            };
            if it.location_id != container_id {
                return Ok(Err(BridgeRpcError { code: "ItemNotInContainer".into(), message: format!("Item {iid} is not located inside container {container_id}"), client_error: None }));
            }
        }

        if let Some(req_pw) = container_password_snap(snap, container_id, 1) {
            let cfg = container_config_snap(snap, container_id);
            let need_check = cfg.password_per_action || !is_container_unlocked_snap(snap, container_id, char_id);
            if need_check {
                let provided = supplied_pw.as_deref().unwrap_or("");
                if provided != req_pw {
                    append_alsc_log_snap(
                        snap,
                        container_id,
                        AlscLogEntry {
                            timestamp_unix: now_unix,
                            actor_id: char_id,
                            action: "ALSCActionPasswordFailure".into(),
                            item_id: container_id,
                            item_type_id: cont.type_id,
                            quantity: 1,
                        },
                        now_unix,
                    )?;
                    return Ok(Err(BridgeRpcError { code: if is_lock { "ALSCPasswordNeededToLock".into() } else { "ALSCPasswordNeededToUnlock".into() }, message: "Container password required".into(), client_error: None }));
                }
                append_alsc_log_snap(
                    snap,
                    container_id,
                    AlscLogEntry {
                        timestamp_unix: now_unix,
                        actor_id: char_id,
                        action: "ALSCActionPasswordSuccess".into(),
                        item_id: container_id,
                        item_type_id: cont.type_id,
                        quantity: 1,
                    },
                    now_unix,
                )?;
                set_container_unlocked_snap(snap, container_id, char_id, true, now_unix)?;
            }
        }

        let mut notifications = Vec::with_capacity(item_ids.len());
        for &iid in &item_ids {
            set_alsc_item_locked_snap(snap, iid, is_lock, now_unix)?;
            let (tid, qty, owner_id, loc_id) = snap
                .items
                .get(&iid)
                .map(|i| (i.type_id, i.quantity, i.owner_id, i.location_id))
                .unwrap_or((0, 1, char_id, container_id));
            append_alsc_log_snap(
                snap,
                container_id,
                AlscLogEntry {
                    timestamp_unix: now_unix,
                    actor_id: char_id,
                    action: if is_lock { "ALSCActionLock".into() } else { "ALSCActionUnlock".into() },
                    item_id: iid,
                    item_type_id: tid,
                    quantity: qty,
                },
                now_unix,
            )?;
            if let Some((corp_id, c_tid, c_loc, loc_flag)) = resolve_corp_container_log_target(snap, container_id) {
                snap.corp_container_logs.entry(corp_id).or_default().push(eve_db::CorpContainerLogEntry {
                    logged_at_unix: now_unix, container_id, container_type_id: c_tid, character_id: char_id,
                    location_id: c_loc, location_flag: loc_flag, action: if is_lock { "lock".into() } else { "unlock".into() },
                    password_type: None, type_id: Some(tid as i32), quantity: Some(qty), old_config_bitmask: None, new_config_bitmask: None,
                });
            }
            notifications.push((iid, owner_id, loc_id));
        }
        Ok(Ok(notifications))
    })??;
    for (iid, owner_id, loc_id) in notifications {
        ctx.emit_typed(
            crate::router::NotificationTarget::Character(char_id),
            crate::router::notify_registry::OnLockedItemChange {
                item_id: iid,
                owner_id,
                location_id: loc_id,
                is_locked: is_lock,
            },
        );
    }
    Ok(PyRep::keyval([("containerID", PyRep::Integer(container_id)), ("locked", PyRep::Bool(is_lock)), ("count", PyRep::Integer(item_ids.len() as i64))]))
}

pub(crate) async fn handle_validate_item_can_be_opened(
    ctx: &ServerContext, session: &SessionState, bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep], kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let parse_arg = |rep: &PyRep, label: &str| -> Result<i64, BridgeRpcError> {
        crate::services::inventory_bound::extract_inventory_item_id(rep)
            .filter(|&id| id > 0)
            .ok_or_else(|| BridgeRpcError {
                code: "InvalidArgument".into(),
                message: format!("Expected positive integer or item descriptor for {label}"),
                client_error: None,
            })
    };
    let item_id = match args.first().filter(|a| !matches!(a, PyRep::None)) {
        Some(rep) => match parse_arg(rep, "'itemID' (arg[0])") {
            Ok(id) => id,
            Err(e) => return Some(Err(e)),
        },
        None => match kwargs.get("itemID").filter(|a| !matches!(a, PyRep::None)) {
            Some(rep) => match parse_arg(rep, "'itemID'") {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            },
            None => bound_entry.map(|b| b.object_id).unwrap_or(0),
        },
    };
    match validate_single_item_can_be_opened(ctx, char_id, item_id).await {
        Ok(()) => Some(Ok(PyRep::Bool(true))),
        Err(e) => Some(Err(e)),
    }
}

pub(crate) async fn handle_validate_item_list_can_be_opened(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let extract = crate::services::inventory_bound::extract_inventory_item_id;
    let item_ids: Vec<i64> = match args.first().or_else(|| kwargs.get("itemIDs")) {
        Some(PyRep::List(l) | PyRep::Tuple(l)) => l.iter().filter_map(extract).collect(),
        Some(rep) => extract(rep).into_iter().collect(),
        _ => Vec::new(),
    };
    if item_ids.is_empty() {
        return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "ValidateItemListCanBeOpened requires a list of itemIDs".into(), client_error: None }));
    }
    for &id in &item_ids {
        if let Err(e) = validate_single_item_can_be_opened(ctx, char_id, id).await {
            return Some(Err(e));
        }
    }
    Some(Ok(PyRep::Bool(true)))
}

pub(crate) fn handle_assemble_cargo_container(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let Some(item_id) = args.first().and_then(|a| a.as_i64()).or_else(|| bound_entry.map(|b| b.object_id)) else {
        return Some(Err(BridgeRpcError { code: "InvalidArgument".into(), message: "AssembleCargoContainer requires container itemID".into(), client_error: None }));
    };
    let now_unix = ctx.now_unix_secs();
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(item) = snap.items.get(&item_id).cloned() else {
            return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Container {item_id} not found"), client_error: None });
        };
        let caller_loc = session.station_id.or(session.structure_id).or_else(|| session.solar_system_id.map(i64::from));
        let corp_id = session.corporation_id.unwrap_or(0);
        if item.owner_id != char_id {
            if corp_id == 0 || item.owner_id != corp_id {
                return Err(BridgeRpcError { code: "OwnershipViolation".into(), message: format!("Container {item_id} is not owned by {char_id}"), client_error: None });
            }
            let (is_ceo, eff_roles) = snap.effective_corp_roles_at_db(corp_id, char_id, caller_loc, ctx.now_unix());
            if !is_ceo && eff_roles == 0 {
                return Err(BridgeRpcError { code: "CorpRoleRequired".into(), message: "Corporation role required to assemble corp container".into(), client_error: None });
            }
        }
        if Some(item.location_id) != caller_loc && session.ship_id != Some(item.location_id) {
            return Err(BridgeRpcError { code: "ColocationRequired".into(), message: "Character must be colocated with container to assemble it".into(), client_error: None });
        }
        let grp = ctx.sde.types.get(&item.type_id).map(|t| t.group_id).unwrap_or(0);
        if !is_container_group(grp, item.type_id) {
            return Err(BridgeRpcError { code: "NotAContainer".into(), message: format!("Item {item_id} (group {grp}) is not a cargo container"), client_error: None });
        }
        if item.flag == InventoryFlag::Locked || snap.is_item_or_ancestor_locked_or_impounded(item_id) {
            return Err(BridgeRpcError { code: "ItemLocked".into(), message: format!("Item {item_id} is locked by contract or job"), client_error: None });
        }
        let (cid, tid, ver) = if !item.singleton && item.quantity > 1 {
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
            (new_id, item.type_id, 1_u64)
        } else {
            let it = snap.items.get_mut(&item_id).unwrap();
            it.singleton = true;
            it.version += 1;
            (item_id, it.type_id, it.version)
        };
        append_alsc_log_snap(
            snap,
            cid,
            AlscLogEntry {
                timestamp_unix: now_unix,
                actor_id: char_id,
                action: "ALSCActionAssemble".into(),
                item_id: cid,
                item_type_id: tid,
                quantity: 1,
            },
            now_unix,
        )?;
        if let Some((corp_id, _, loc_id, loc_flag)) = resolve_corp_container_log_target(snap, cid) {
            snap.corp_container_logs.entry(corp_id).or_default().push(eve_db::CorpContainerLogEntry {
                logged_at_unix: now_unix, container_id: cid, container_type_id: tid as i32, character_id: char_id,
                location_id: loc_id, location_flag: loc_flag, action: "assemble".into(), password_type: None,
                type_id: None, quantity: None, old_config_bitmask: None, new_config_bitmask: None,
            });
        }
        Ok((cid, tid, ver))
    });
    match res {
        Ok((cid, tid, ver)) => {
            Some(Ok(PyRep::keyval([("itemID", PyRep::Integer(cid)), ("typeID", PyRep::Integer(tid as i64)), ("singleton", PyRep::Bool(true)), ("version", PyRep::Integer(ver as i64))])))
        }
        Err(e) => Some(Err(e)),
    }
}

pub(crate) fn handle_alsc_config_get(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let rpc = crate::router::RpcArgs::new(args, kwargs);
    let container_id = match rpc.opt_positive_i64(0, "containerID") {
        Ok(v) => v.or_else(|| bound_entry.map(|b| b.object_id)).unwrap_or(0),
        Err(e) => return Some(Err(e)),
    };
    let (has_pw, cfg) = match ctx.db.with_snapshot(|snap| -> Result<(bool, ContainerConfig), BridgeRpcError> {
        authorize_container_inspect_in_snap(ctx, session, snap, char_id, container_id, false)?;
        let has_pw = container_password_snap(snap, container_id, 1).is_some()
            || container_password_snap(snap, container_id, 2).is_some();
        let cfg = container_config_snap(snap, container_id);
        Ok((has_pw, cfg))
    }) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    Some(Ok(PyRep::keyval([
        ("containerID", PyRep::Integer(container_id)), ("passwordRequired", PyRep::Bool(has_pw)),
        ("hasPassword", PyRep::Bool(has_pw)), ("lockByDefault", PyRep::Bool(cfg.lock_by_default)),
        ("passwordPerAction", PyRep::Bool(cfg.password_per_action)), ("configBitmask", PyRep::Integer(cfg.config_bitmask as i64)),
    ])))
}

pub(crate) fn handle_alsc_log_get(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
    let rpc = crate::router::RpcArgs::new(args, kwargs);
    // Client `AuditLogSecureContainerLogViewer.pyj` calls on a bound container moniker:
    // `ALSCLogGet(fromDate=None, fromLogID=1, forward=None)`.
    // Server/test callers pass `containerID` via `kwargs["containerID"]` or `args[0]`
    // (with `bound_entry` unset or pointing at the station/hangar inventory manager).
    let is_bound_client_call = bound_entry.is_some()
        && !kwargs.contains_key("containerID")
        && (kwargs.contains_key("fromDate")
            || kwargs.contains_key("fromLogID")
            || kwargs.contains_key("forward")
            || args.is_empty()
            || args.first().is_some_and(|v| {
                matches!(v, PyRep::None)
                    || v.as_i64().is_some_and(|n| n >= 10_000_000_000_000_000)
            }));
    let container_id = if is_bound_client_call {
        bound_entry.map(|b| b.object_id).unwrap_or(0)
    } else {
        match rpc.opt_positive_i64(0, "containerID") {
            Ok(v) => v.or_else(|| bound_entry.map(|b| b.object_id)).unwrap_or(0),
            Err(e) => return Some(Err(e)),
        }
    };
    let from_date = if is_bound_client_call {
        kwargs.get("fromDate").or_else(|| args.first())
    } else {
        kwargs.get("fromDate")
    }
    .and_then(|v| if matches!(v, PyRep::None) { None } else { v.as_i64() });
    let from_log_id = if is_bound_client_call {
        kwargs.get("fromLogID").or_else(|| args.get(1))
    } else {
        kwargs.get("fromLogID")
    }
    .and_then(|v| if matches!(v, PyRep::None) { None } else { v.as_i64() });
    let forward = if is_bound_client_call {
        kwargs.get("forward").or_else(|| args.get(2))
    } else {
        kwargs.get("forward")
    }
    .and_then(|v| match v {
        PyRep::Bool(b) => Some(*b),
        PyRep::Integer(n) => Some(*n != 0),
        _ => None,
    });
    let provided = kwargs
        .get("password")
        .and_then(|v| v.as_str())
        .or_else(|| {
            (!is_bound_client_call)
                .then(|| args.get(1).and_then(|a| a.as_str()))
                .flatten()
        })
        .unwrap_or("");
    let now_unix = ctx.now_unix_secs();
    const ALSC_PAGE_LIMIT: usize = 50;
    let (raw_logs, flag_id, location_id, container_owner_id) = match ctx.db.try_with_mut_snapshot(
        |snap| -> Result<Result<(Vec<AlscLogEntry>, i64, i64, i64), BridgeRpcError>, BridgeRpcError> {
            let cont = authorize_container_inspect_in_snap(
                ctx,
                session,
                snap,
                char_id,
                container_id,
                false,
            )?;
            let flag_id = cont.flag as i64;
            let container_owner_id = if cont.owner_id > 0 { cont.owner_id } else { char_id };
            let container_type_id = cont.type_id;
            let mut loc = cont.location_id;
            for _ in 0..16 {
                if loc <= 0 {
                    break;
                }
                if ctx.sde.stations.contains_key(&loc)
                    || snap.structures.contains_key(&loc)
                    || u32::try_from(loc).is_ok_and(|id| ctx.sde.solar_systems.contains_key(&id))
                {
                    break;
                }
                let Some(parent) = snap.items.get(&loc) else {
                    break;
                };
                if parent.location_id <= 0 || parent.location_id == loc {
                    break;
                }
                loc = parent.location_id;
            }
            if loc <= 0 {
                loc = snap
                    .characters
                    .get(&char_id)
                    .and_then(|c| c.station_id.or(Some(i64::from(c.solar_system_id))))
                    .filter(|&v| v > 0)
                    .unwrap_or_else(|| container_id.max(1));
            }
            let cfg = container_config_snap(snap, container_id);
            let need_check = cfg.password_per_action
                || !is_container_unlocked_snap(snap, container_id, char_id);
            if let Some(req_pw) = container_password_snap(snap, container_id, 1) {
                if need_check {
                    if provided != req_pw {
                        append_alsc_log_snap(
                            snap,
                            container_id,
                            AlscLogEntry {
                                timestamp_unix: now_unix,
                                actor_id: char_id,
                                action: "ALSCActionPasswordFailure".into(),
                                item_id: container_id,
                                item_type_id: container_type_id,
                                quantity: 1,
                            },
                            now_unix,
                        )?;
                        return Ok(Err(BridgeRpcError {
                            code: "ALSCPasswordNeededToViewAuditLog".into(),
                            message: "Container password required to view audit log".into(),
                            client_error: None,
                        }));
                    }
                    append_alsc_log_snap(
                        snap,
                        container_id,
                        AlscLogEntry {
                            timestamp_unix: now_unix,
                            actor_id: char_id,
                            action: "ALSCActionPasswordSuccess".into(),
                            item_id: container_id,
                            item_type_id: container_type_id,
                            quantity: 1,
                        },
                        now_unix,
                    )?;
                    set_container_unlocked_snap(snap, container_id, char_id, true, now_unix)?;
                }
            }
            Ok(Ok((
                container_alsc_logs_snap(snap, container_id),
                flag_id,
                loc,
                container_owner_id,
            )))
        },
    ) {
        Ok(Ok(logs)) => logs,
        Ok(Err(e)) => return Some(Err(e)),
        Err(e) => return Some(Err(e)),
    };
    let mut filtered: Vec<(i64, i64, AlscLogEntry)> = raw_logs
        .into_iter()
        .enumerate()
        .filter_map(|(idx, e)| {
            let log_id = (idx as i64) + 1;
            let log_date = ((e.timestamp_unix as i64).saturating_add(11_644_473_600))
                .saturating_mul(10_000_000);
            if let Some(fd) = from_date {
                if log_date < fd {
                    return None;
                }
            }
            match (forward, from_log_id) {
                (Some(true), Some(fl)) if log_id <= fl => return None,
                (Some(false), Some(fl)) if log_id >= fl => return None,
                _ => {}
            }
            Some((log_id, log_date, e))
        })
        .collect();
    if forward == Some(false) || forward.is_none() {
        if filtered.len() > ALSC_PAGE_LIMIT {
            filtered.drain(..filtered.len() - ALSC_PAGE_LIMIT);
        }
    } else {
        filtered.truncate(ALSC_PAGE_LIMIT);
    }
    let entries: Vec<PyRep> = filtered
        .into_iter()
        .map(|(log_id, log_date, e)| {
            let tid = i64::from(e.item_type_id);
            let qty = i64::from(e.quantity.max(0));
            let (action_id, status, arg1, arg2) = match e.action.as_str() {
                "ALSCActionAssemble" => (
                    1,
                    1,
                    if e.actor_id > 0 { e.actor_id } else { container_owner_id.max(1) },
                    tid,
                ),
                "ALSCActionRepackage" => (2, 1, 0, tid),
                "ALSCActionRename" | "ALSCActionSetName" => (3, 1, 0, 0),
                "ALSCActionTake" | "ALSCActionMove" => (4, 1, tid, qty),
                "ALSCActionSetPassword" | "ALSCActionRetrievePassword" => (5, 1, 1, 0),
                "ALSCActionAdd" => (6, 1, tid, qty),
                "ALSCActionLock" => (7, 1, tid, qty),
                "ALSCActionUnlock" => (8, 1, tid, qty),
                "ALSCActionPasswordFailure" => (9, 0, 1, 0),
                "ALSCActionOpen" | "ALSCActionPasswordSuccess" | "ALSCActionEnterPassword" => {
                    (9, 1, 1, 0)
                }
                "ALSCActionConfigChange" | "ALSCActionConfigure" => (10, 1, 0, 0),
                _ => (0, 1, tid, qty),
            };
            PyRep::keyval([
                ("logID", PyRep::Integer(log_id)),
                ("logDate", PyRep::Integer(log_date)),
                ("actionID", PyRep::Integer(action_id)),
                ("actorID", PyRep::Integer(e.actor_id)),
                ("arg1", PyRep::Integer(arg1)),
                ("arg2", PyRep::Integer(arg2)),
                ("flagID", PyRep::Integer(flag_id)),
                ("locationID", PyRep::Integer(location_id)),
                ("inventoryMgrLocationID", PyRep::Integer(location_id)),
                ("status", PyRep::Integer(status)),
            ])
        })
        .collect();
    Some(Ok(PyRep::List(entries)))
}

pub(crate) fn handle_break_plastic_wrap(
    ctx: &ServerContext,
    session: &SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
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
            return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Plastic Wrap {wrap_id} not found"), client_error: None });
        };
        if wrap.type_id != 3468 {
            return Err(BridgeRpcError { code: "InvalidPlasticWrap".into(), message: format!("Item {wrap_id} is not a Plastic Wrap"), client_error: None });
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
                client_error: None,
            });
        }
        let has_courier = snap.contracts.values().any(|c| {
            c.contract_type == "Courier"
                && c.status == "InProgress"
                && c.item_ids.iter().any(|iid| snap.items.get(iid).is_some_and(|it| it.location_id == wrap_id))
        });
        if has_courier {
            let (cid, issuer_id, _extracted) =
                crate::services::contracts_service::break_courier_plastic_wrap(snap, char_id, wrap_id, now_unix)?;
            return Ok(Some((cid, issuer_id)));
        }
        if wrap.owner_id != char_id {
            return Err(BridgeRpcError { code: "NotItemOwner".into(), message: format!("Character {char_id} does not own Plastic Wrap {wrap_id}"), client_error: None });
        }
        let parent_loc = wrap.location_id;
        let parent_flag = wrap.flag;
        for child in snap.items.values_mut() {
            if child.location_id == wrap_id {
                child.location_id = parent_loc;
                child.flag = parent_flag;
                child.version += 1;
            }
        }
        snap.items.remove(&wrap_id);
        Ok(None)
    });
    match res {
        Ok(failed_courier) => {
            if let Some((cid, issuer_id)) = failed_courier {
                crate::services::contracts_service::notify_contract(ctx, issuer_id, "OnContractFailed", cid);
            }
            Some(Ok(PyRep::None))
        }
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
    crate::services::inventory_fitting_rpc::handle_containers_ships_and_fitting(
        ctx,
        session,
        bound_entry,
        service,
        method,
        args,
        kwargs,
    )
    .await
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
                client_error: None,
            });
        }
        if src_item.flag == InventoryFlag::Locked
            || snap.is_item_or_ancestor_locked_or_impounded(src_item.item_id)
        {
            return Err(BridgeRpcError {
                code: "ItemLocked".into(),
                message: format!("Item {} is locked and cannot be unfitted", src_item.item_id),
                client_error: None,
            });
        }
        let Some(ship_item) = snap.items.get(&src_item.location_id) else {
            return Err(BridgeRpcError {
                code: "ShipNotFound".into(),
                message: format!("Source ship {} not found", src_item.location_id),
                client_error: None,
            });
        };
        let ship_cat = ctx.sde.types.get(&ship_item.type_id).map(|t| t.category_id).unwrap_or(0);
        if ship_item.owner_id != char_id || ship_cat != 6 {
            return Err(BridgeRpcError {
                code: "ShipOwnershipViolation".into(),
                message: format!("Source {} is not a ship owned by {char_id}", src_item.location_id),
                client_error: None,
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
                return Err(BridgeRpcError { code: "InvalidFittingTarget".into(), message: format!("Ship {} is not docked at current location {ch_dock}", ship_item.item_id), client_error: None });
            }
            if new_loc != ship_item.item_id && !resolves_to_loc(new_loc, ch_dock) {
                return Err(BridgeRpcError { code: "InvalidItemLocation".into(), message: format!("Destination {new_loc} is not at current dock {ch_dock}"), client_error: None });
            }
            Ok(None)
        } else {
            let caller_ship = session.ship_id.filter(|&id| id > 0).unwrap_or(active_ship_id);
            if ship_item.item_id != caller_ship || (ch_sys > 0 && ship_item.location_id != ch_sys) {
                return Err(BridgeRpcError { code: "InvalidFittingTarget".into(), message: format!("Pilot {char_id} must be aboard ship {} in system {ch_sys} to unfit in space", ship_item.item_id), client_error: None });
            }
            Ok(Some((session.solar_system_id.unwrap_or(ch_sys.max(0) as u32), caller_ship)))
        }
    })?;
    if let Some((sys_id, ship_id)) = in_space_ship {
        let now_unix = ctx.now_unix_secs();
        if ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner()).states.get(&char_id).is_some_and(|cw| cw.is_weapon_timer_active(now_unix)) {
            return Err(BridgeRpcError { code: "CannotRefitWithWeaponsTimer".into(), message: "Cannot refit or unfit modules in space while weapons timer is active".into(), client_error: None });
        }
        if sys_id == 0 || !super::is_near_in_space_fitting_service(ctx, sys_id, ship_id).await {
            return Err(BridgeRpcError { code: "CannotUnfitInSpace".into(), message: "Unfitting modules requires being docked at a station/structure or within range of a Mobile Depot or capital SMA".into(), client_error: None });
        }
    }
    Ok(())
}

pub(crate) fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
        client_error: None,
    }
}

pub(super) fn authorize_container_inspect_in_snap<'a>(
    ctx: &ServerContext,
    session: &SessionState,
    snap: &'a eve_db::WorldStateSnapshot,
    char_id: i64,
    container_id: i64,
    allow_ship: bool,
) -> Result<&'a eve_db::InventoryItemRecord, BridgeRpcError> {
    if container_id <= 0 {
        return Err(rpc_err(
            "InvalidContainerID",
            format!("Invalid container ID {container_id}"),
        ));
    }
    let is_gm = session.has_gm_role()
        || (session.role & eve_machonet::service_roles::ROLE_GM_MASK) != 0;
    let Some(cont) = snap.items.get(&container_id) else {
        return Err(rpc_err(
            "ContainerNotFound",
            format!("Container {container_id} not found"),
        ));
    };
    let type_info = ctx.sde.types.get(&cont.type_id);
    let is_container = type_info.is_some_and(|t| {
        matches!(t.group_id, 12 | 340 | 448 | 649) || (allow_ship && t.category_id == 6)
    });
    if !is_container {
        return Err(rpc_err(
            "ALSCAccessDenied",
            format!("Item {container_id} is not an accessible secure container"),
        ));
    }
    if is_gm || cont.owner_id == char_id {
        return Ok(cont);
    }
    let caller_corp = session
        .corporation_id
        .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id))
        .unwrap_or(0);
    if caller_corp > 0 && cont.owner_id == caller_corp {
        let (is_ceo, eff_role) = snap.effective_corp_roles_at_db(
            caller_corp,
            char_id,
            Some(cont.location_id),
            ctx.now_unix(),
        );
        if is_ceo
            || (eff_role
                & (eve_db::corp_roles::ROLE_DIRECTOR
                    | eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG))
                != 0
        {
            return Ok(cont);
        }
        return Err(rpc_err(
            "ALSCAccessDenied",
            format!("Not authorized to access corporate container {container_id}"),
        ));
    }
    Err(BridgeRpcError {
        code: "ALSCAccessDenied".into(),
        message: format!(
            "Not authorized to view configuration or log for container {container_id}"
        ),
        client_error: None,
    })
}

#[cfg(test)]
#[path = "fitting_containers_ships_edges2_tests.rs"]
mod fitting_containers_ships_edges2_tests;
