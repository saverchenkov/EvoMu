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
    pub timestamp_unix: u64,
    pub actor_id: i64,
    pub action: String,
    pub item_id: i64,
    pub item_type_id: u32,
    pub quantity: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct InsuranceContractRecord {
    pub ship_id: i64,
    pub ship_type_id: u32,
    pub owner_id: i64,
    pub level_name: String,
    pub level_fraction: f64,
    pub cost_isk: f64,
    pub payout_isk: f64,
    pub start_unix: u64,
    pub end_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ShipAccessConfig {
    pub allow_fleet_smb: bool,
    pub allow_corp_smb: bool,
    pub allow_fleet_hangar: bool,
    pub allow_corp_hangar: bool,
    pub password: Option<String>,
}

impl Default for ShipAccessConfig {
    fn default() -> Self {
        Self {
            allow_fleet_smb: true,
            allow_corp_smb: true,
            allow_fleet_hangar: true,
            allow_corp_hangar: true,
            password: None,
        }
    }
}

#[derive(Default, Clone, Serialize, Deserialize)]
pub(crate) struct ContainerShipRuntimeState {
    pub container_passwords: HashMap<i64, String>,
    pub unlocked_containers: HashSet<(i64, i64)>,
    pub alsc_locked_items: HashSet<i64>,
    pub alsc_logs: HashMap<i64, Vec<AlscLogEntry>>,
    pub ship_configs: HashMap<i64, ShipAccessConfig>,
    pub custom_item_flags: HashMap<i64, u16>,
    pub insurance_contracts: HashMap<i64, InsuranceContractRecord>,
    #[serde(default)]
    pub ship_passwords: HashMap<i64, String>,
}

pub(crate) fn read_container_ship_state_snap(snap: &eve_db::WorldStateSnapshot) -> ContainerShipRuntimeState {
    snap.owner_notes
        .get(&CONTAINER_SHIP_NOTE_ID)
        .and_then(|n| serde_json::from_str::<ContainerShipRuntimeState>(&n.note).ok())
        .unwrap_or_default()
}

pub(crate) fn read_container_ship_state(ctx: &ServerContext) -> ContainerShipRuntimeState {
    ctx.db.with_snapshot(read_container_ship_state_snap)
}

pub(crate) fn write_container_ship_state_snap<R>(
    snap: &mut eve_db::WorldStateSnapshot,
    f: impl FnOnce(&mut ContainerShipRuntimeState) -> R,
) -> R {
    let mut state = read_container_ship_state_snap(snap);
    let out = f(&mut state);
    if let Ok(json) = serde_json::to_string(&state) {
        let ts = snap
            .owner_notes
            .get(&CONTAINER_SHIP_NOTE_ID)
            .map(|n| n.timestamp_unix)
            .unwrap_or(0);
        snap.owner_notes.insert(
            CONTAINER_SHIP_NOTE_ID,
            eve_db::OwnerNoteRecord {
                note_id: CONTAINER_SHIP_NOTE_ID,
                character_id: CONTAINER_SHIP_NOTE_OWNER_ID,
                item_id: 0,
                label: "S:CONTAINER_SHIP_RUNTIME_STATE".to_string(),
                note: json,
                timestamp_unix: ts,
            },
        );
    }
    out
}

pub(crate) fn save_container_ship_state_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    now_unix: u64,
    state: &ContainerShipRuntimeState,
) {
    if let Ok(json) = serde_json::to_string(state) {
        snap.owner_notes.insert(
            CONTAINER_SHIP_NOTE_ID,
            eve_db::OwnerNoteRecord {
                note_id: CONTAINER_SHIP_NOTE_ID,
                character_id: CONTAINER_SHIP_NOTE_OWNER_ID,
                item_id: 0,
                label: "S:CONTAINER_SHIP_RUNTIME_STATE".to_string(),
                note: json,
                timestamp_unix: now_unix,
            },
        );
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
    type_id == 23 || matches!(group_id, 12 | 340 | 448 | 649)
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

/// Checks if `ship_id` in `sys_id` is within 3,000m of a Mobile Depot (`group_id == 1246`)
/// or another ship providing a Ship Maintenance Bay fitting service (`attr 907` / `908` > 0).
pub(crate) async fn is_near_in_space_fitting_service(
    ctx: &ServerContext,
    sys_id: u32,
    ship_id: i64,
) -> bool {
    let provider_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| {
                i.item_id != ship_id
                    && i.location_id == sys_id as i64
                    && ctx.sde.types.get(&i.type_id).is_some_and(|t| {
                        t.group_id == 1246
                            || t.default_attributes.get(&907).copied().unwrap_or(0.0) > 0.0
                            || t.default_attributes.get(&908).copied().unwrap_or(0.0) > 0.0
                    })
            })
            .map(|i| i.item_id)
            .collect()
    });
    if provider_ids.is_empty() {
        return false;
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Ok(Some((_, ship_ball))) = sys_handle.get_ball_snapshot(ship_id).await else {
        return true;
    };
    for pid in provider_ids {
        if let Ok(Some((_, p_ball))) = sys_handle.get_ball_snapshot(pid).await {
            if (ship_ball.position - p_ball.position).length() <= 3_000.0 {
                return true;
            }
        } else {
            return true;
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
    let c2 = snap.characters.get(&host_owner_id).map(|c| c.corporation_id);
    let in_same_fleet = snap
        .fleets
        .values()
        .any(|fl| fl.members.contains(&char_id) && fl.members.contains(&host_owner_id));
    let same_corp = c1.is_some() && c1 == c2;
    if is_sma {
        (in_same_fleet && cfg.allow_fleet_smb) || (same_corp && cfg.allow_corp_smb)
    } else {
        (in_same_fleet && cfg.allow_fleet_hangar) || (same_corp && cfg.allow_corp_hangar)
    }
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



fn resolve_corp_container_log_target(
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
    let alsc_locked = read_container_ship_state(ctx)
        .alsc_locked_items
        .contains(&item_id);
    if alsc_locked {
        return Err(BridgeRpcError {
            code: "ALSCItemLocked".into(),
            message: format!("Item {item_id} is locked in an Audit Log Secure Container"),
        });
    }

    let snap_check = ctx.db.with_snapshot(|snap| {
        let Some(moving_item) = snap.items.get(&item_id) else {
            return Ok(None);
        };
        if moving_item.flag == InventoryFlag::Locked
            || snap.is_item_locked_in_contract_or_job(item_id)
        {
            return Err(BridgeRpcError {
                code: "ItemLocked".into(),
                message: format!("Item {item_id} is locked by a contract, industry job, or container"),
            });
        }
        let moving_type = ctx.sde.types.get(&moving_item.type_id);
        let moving_grp = moving_type.map(|t| t.group_id).unwrap_or(0);
        let moving_cat = moving_type.map(|t| t.category_id).unwrap_or(0);
        let moving_unit_vol = effective_item_unit_volume(&ctx.sde, moving_item);
        let move_qty = requested_qty.unwrap_or(moving_item.quantity as i64).max(1) as f64;
        let moving_total_vol = moving_unit_vol * move_qty;

        let Some(dest_item) = snap.items.get(&new_loc) else {
            return Ok(None);
        };
        let Some(dest_type) = ctx.sde.types.get(&dest_item.type_id) else {
            return Ok(None);
        };

        // Station Container (`group 448`, unpacked volume >= 500,000 m3) cannot be moved into a ship when assembled
        if moving_grp == 448 && moving_item.singleton && dest_type.category_id == 6 {
            return Err(BridgeRpcError {
                code: "CapacityExceeded".into(),
                message: "Assembled Station Containers cannot be loaded into a ship".into(),
            });
        }

        // Moving into a Container (`group 12 | 340 | 448 | 649` or jetcan `23`)
        if is_container_group(dest_type.group_id, dest_type.type_id) {
            if !dest_item.singleton {
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
            let cap = dest_type.capacity;
            if cap > 0.0 {
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

    if snap_check.is_some() {
        let mut st = read_container_ship_state(ctx);
        if let Some(req_pw) = st.container_passwords.get(&new_loc).cloned() {
            if !req_pw.is_empty() && !st.unlocked_containers.contains(&(new_loc, char_id)) {
                let provided = kwargs.get("password").and_then(|v| v.as_str()).unwrap_or("");
                if provided != req_pw {
                    return Err(BridgeRpcError {
                        code: "ALSCPasswordNeededToOpen".into(),
                        message: format!("Password required to access secure container {new_loc}"),
                    });
                }
                st.unlocked_containers.insert((new_loc, char_id));
                save_container_ship_state(ctx, &st);
            }
        }
    }

    Ok(())
}

fn validate_single_item_can_be_opened(
    ctx: &ServerContext,
    char_id: i64,
    item_id: i64,
) -> Result<(), BridgeRpcError> {
    if item_id <= 0 {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "Valid itemID required".into(),
        });
    }
    let (item_type_id, _owner_id, _location_id, singleton) = ctx.db.with_snapshot(|snap| {
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
    let is_openable = is_container_group(grp, item_type_id)
        || grp == 186
        || cat == 6
        || cat == 2
        || item_type_id == 23;
    if !is_openable {
        return Err(BridgeRpcError {
            code: "NotAContainer".into(),
            message: format!("Item {item_id} (type {item_type_id}) is not a container"),
        });
    }

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
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("itemID").and_then(|v| v.as_i64()))
                .or_else(|| bound_entry.map(|b| b.object_id))
                .unwrap_or(0);
            match validate_single_item_can_be_opened(ctx, char_id, item_id) {
                Ok(()) => Some(Ok(PyRep::Bool(true))),
                Err(e) => Some(Err(e)),
            }
        }

        ("invbroker", "ValidateItemListCanBeOpened") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_ids: Vec<i64> = match args.first().or_else(|| kwargs.get("itemIDs")) {
                Some(PyRep::List(l) | PyRep::Tuple(l)) => l.iter().filter_map(|x| x.as_i64()).collect(),
                Some(PyRep::Integer(id)) => vec![*id],
                _ => Vec::new(),
            };
            if item_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "ValidateItemListCanBeOpened requires a list of itemIDs".into(),
                }));
            }
            for &id in &item_ids {
                if let Err(e) = validate_single_item_can_be_opened(ctx, char_id, id) {
                    return Some(Err(e));
                }
            }
            Some(Ok(PyRep::Bool(true)))
        }

        ("invbroker", "AssembleCargoContainer") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(item_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| bound_entry.map(|b| b.object_id))
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "AssembleCargoContainer requires container itemID".into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(item) = snap.items.get(&item_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Container {item_id} not found"),
                    });
                };
                let corp_id = session.corporation_id.unwrap_or(0);
                if item.owner_id != char_id {
                    let is_ceo = snap.corporations.get(&corp_id).map(|c| c.ceo_id == char_id).unwrap_or(false);
                    if corp_id == 0 || item.owner_id != corp_id {
                        return Err(BridgeRpcError {
                            code: "OwnershipViolation".into(),
                            message: format!("Container {item_id} is not owned by {char_id}"),
                        });
                    }
                    if session.corp_role == 0 && !is_ceo {
                        return Err(BridgeRpcError {
                            code: "CorpRoleRequired".into(),
                            message: "Corporation role required to assemble corp container".into(),
                        });
                    }
                }
                let caller_loc = session.station_id.or(session.structure_id).or_else(|| session.solar_system_id.map(i64::from));
                if Some(item.location_id) != caller_loc && session.ship_id != Some(item.location_id) {
                    return Err(BridgeRpcError {
                        code: "ColocationRequired".into(),
                        message: "Character must be colocated with container to assemble it".into(),
                    });
                }
                let grp = ctx.sde.types.get(&item.type_id).map(|t| t.group_id).unwrap_or(0);
                if !is_container_group(grp, item.type_id) {
                    return Err(BridgeRpcError {
                        code: "NotAContainer".into(),
                        message: format!("Item {item_id} (group {grp}) is not a cargo container"),
                    });
                }
                if !item.singleton && item.quantity > 1 {
                    if let Some(orig) = snap.items.get_mut(&item_id) {
                        orig.quantity -= 1;
                        orig.version += 1;
                    }
                    snap.next_entity_id += 1;
                    let new_id = snap.next_entity_id;
                    snap.items.insert(
                        new_id,
                        eve_db::InventoryItemRecord {
                            item_id: new_id,
                            type_id: item.type_id,
                            owner_id: item.owner_id,
                            location_id: item.location_id,
                            flag: item.flag,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            ..Default::default()
                        },
                    );
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
                        st.alsc_logs
                            .entry(cid)
                            .or_default()
                            .push(AlscLogEntry {
                                timestamp_unix: now_unix,
                                actor_id: char_id,
                                action: "ALSCActionAssemble".into(),
                                item_id: cid,
                                item_type_id: tid,
                                quantity: 1,
                            });
                    });
                    ctx.db.with_mut_snapshot(|snap| {
                        if let Some((corp_id, _, loc_id, loc_flag)) =
                            resolve_corp_container_log_target(snap, cid)
                        {
                            snap.corp_container_logs.entry(corp_id).or_default().push(
                                eve_db::CorpContainerLogEntry {
                                    logged_at_unix: now_unix,
                                    container_id: cid,
                                    container_type_id: tid as i32,
                                    character_id: char_id,
                                    location_id: loc_id,
                                    location_flag: loc_flag,
                                    action: "assemble".into(),
                                    password_type: None,
                                    type_id: None,
                                    quantity: None,
                                    old_config_bitmask: None,
                                    new_config_bitmask: None,
                                },
                            );
                        }
                    });
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(cid)),
                        ("typeID", PyRep::Integer(tid as i64)),
                        ("singleton", PyRep::Bool(true)),
                        ("version", PyRep::Integer(ver as i64)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("invbroker", "SetPassword")
        | ("invbroker", "ALSCConfigSet")
        | ("invbroker", "ConfigureALSC") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let container_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| bound_entry.map(|b| b.object_id))
                .unwrap_or(0);
            let password = args
                .get(1)
                .and_then(|a| a.as_str())
                .or_else(|| args.get(2).and_then(|a| a.as_str()))
                .or_else(|| kwargs.get("password").and_then(|v| v.as_str()))
                .or_else(|| args.first().and_then(|a| a.as_str()))
                .unwrap_or("")
                .to_string();
            let corp_id = session.corporation_id.unwrap_or(0);
            let cont_res = ctx.db.with_snapshot(|snap| {
                let Some(item) = snap.items.get(&container_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Secure container {container_id} not found or not owned"),
                    });
                };
                if item.owner_id != char_id {
                    let is_ceo = snap.corporations.get(&corp_id).map(|c| c.ceo_id == char_id).unwrap_or(false);
                    if corp_id == 0 || item.owner_id != corp_id {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Secure container {container_id} not found or not owned"),
                        });
                    }
                    if session.corp_role == 0 && !is_ceo {
                        return Err(BridgeRpcError {
                            code: "CorpRoleRequired".into(),
                            message: "Corporation role required to configure corp container password".into(),
                        });
                    }
                }
                let caller_loc = session.station_id.or(session.structure_id).or_else(|| session.solar_system_id.map(i64::from));
                if Some(item.location_id) != caller_loc && session.ship_id != Some(item.location_id) {
                    return Err(BridgeRpcError {
                        code: "ColocationRequired".into(),
                        message: "Character must be colocated with container to configure password".into(),
                    });
                }
                let grp = ctx.sde.types.get(&item.type_id).map(|t| t.group_id).unwrap_or(0);
                Ok((item.type_id, grp))
            });
            let (tid, grp_id) = match cont_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if matches!(grp_id, 12 | 649) {
                return Some(Err(BridgeRpcError {
                    code: "ContainerDoesNotSupportPassword".into(),
                    message: format!("Container {container_id} (group {grp_id}) does not support passwords"),
                }));
            }
            let now_unix = ctx.now_unix_secs();
            write_container_ship_state(ctx, |st| {
                st.container_passwords.insert(container_id, password.clone());
                st.unlocked_containers.insert((container_id, char_id));
                st.alsc_logs.entry(container_id).or_default().push(AlscLogEntry {
                    timestamp_unix: now_unix,
                    actor_id: char_id,
                    action: "ALSCActionSetPassword".into(),
                    item_id: container_id,
                    item_type_id: tid,
                    quantity: 1,
                });
            });
            ctx.db.with_mut_snapshot(|snap| {
                if let Some((corp_id, _, loc_id, loc_flag)) =
                    resolve_corp_container_log_target(snap, container_id)
                {
                    snap.corp_container_logs.entry(corp_id).or_default().push(
                        eve_db::CorpContainerLogEntry {
                            logged_at_unix: now_unix,
                            container_id,
                            container_type_id: tid as i32,
                            character_id: char_id,
                            location_id: loc_id,
                            location_flag: loc_flag,
                            action: "set_password".into(),
                            password_type: Some("config".into()),
                            type_id: None,
                            quantity: None,
                            old_config_bitmask: None,
                            new_config_bitmask: None,
                        },
                    );
                }
            });
            Some(Ok(PyRep::keyval([
                ("containerID", PyRep::Integer(container_id)),
                ("passwordSet", PyRep::Bool(!password.is_empty())),
            ])))
        }

        ("invbroker", "ALSCConfigGet") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let container_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| bound_entry.map(|b| b.object_id))
                .unwrap_or(0);
            let has_pw = read_container_ship_state(ctx)
                .container_passwords
                .get(&container_id)
                .is_some_and(|p| !p.is_empty());
            Some(Ok(PyRep::keyval([
                ("containerID", PyRep::Integer(container_id)),
                ("passwordRequired", PyRep::Bool(has_pw)),
                ("hasPassword", PyRep::Bool(has_pw)),
            ])))
        }

        ("invbroker", "ALSCLockItems")
        | ("invbroker", "ALSCLock")
        | ("invbroker", "ALSCUnlockItems")
        | ("invbroker", "ALSCUnlock") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let is_lock = method.contains("ALSCLock");
            let container_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| bound_entry.map(|b| b.object_id))
                .unwrap_or(0);
            if container_id <= 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidContainer".into(),
                    message: "Valid containerID required for ALSC lock/unlock".into(),
                }));
            }
            let item_ids: Vec<i64> = match args.get(1).or_else(|| args.first()) {
                Some(PyRep::List(v) | PyRep::Tuple(v)) => {
                    v.iter().filter_map(|x| x.as_i64()).collect()
                }
                Some(PyRep::Integer(id)) if *id != container_id => vec![*id],
                _ => kwargs
                    .get("itemIDs")
                    .and_then(|v| match v {
                        PyRep::List(l) | PyRep::Tuple(l) => {
                            Some(l.iter().filter_map(|x| x.as_i64()).collect())
                        }
                        _ => None,
                    })
                    .unwrap_or_default(),
            };
            let auth_check = ctx.db.with_snapshot(|snap| {
                let cont = snap.items.get(&container_id).ok_or_else(|| BridgeRpcError {
                    code: "ContainerNotFound".into(),
                    message: format!("Container {container_id} not found"),
                })?;
                let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                let ch_loc = snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id).or(Some(c.solar_system_id as i64))).unwrap_or(0);
                let ch_ship = snap.characters.get(&char_id).map(|c| c.active_ship_id).unwrap_or(0);
                let is_ceo = snap.corporations.get(&corp_id).map(|c| c.ceo_id == char_id).unwrap_or(false);
                let is_owner_or_corp = cont.owner_id == char_id || (corp_id > 0 && cont.owner_id == corp_id && (session.corp_role != 0 || is_ceo));
                if !is_owner_or_corp && cont.location_id != ch_loc && cont.location_id != ch_ship {
                    return Err(BridgeRpcError {
                        code: "ALSCAccessDenied".into(),
                        message: format!("Not authorized to lock/unlock items in container {container_id}"),
                    });
                }
                for &iid in &item_ids {
                    let it = snap.items.get(&iid).ok_or_else(|| BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Item {iid} not found"),
                    })?;
                    if it.location_id != container_id {
                        return Err(BridgeRpcError {
                            code: "ItemNotInContainer".into(),
                            message: format!("Item {iid} is not located inside container {container_id}"),
                        });
                    }
                }
                Ok(is_owner_or_corp)
            });
            let is_owner_or_corp = match auth_check {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix_secs();
            let mut st = read_container_ship_state(ctx);
            let req_pw = st.container_passwords.get(&container_id).cloned().unwrap_or_default();
            if !is_owner_or_corp && req_pw.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "ALSCAccessDenied".into(),
                    message: format!("Not authorized to lock/unlock items in container {container_id}"),
                }));
            }
            if !req_pw.is_empty() {
                let provided = kwargs
                    .get("password")
                    .and_then(|v| v.as_str())
                    .or_else(|| args.get(2).and_then(|a| a.as_str()))
                    .unwrap_or("");
                if provided != req_pw && !st.unlocked_containers.contains(&(container_id, char_id)) {
                    return Some(Err(BridgeRpcError {
                        code: if is_lock {
                            "ALSCPasswordNeededToLock".into()
                        } else {
                            "ALSCPasswordNeededToUnlock".into()
                        },
                        message: "Container password required".into(),
                    }));
                }
                st.unlocked_containers.insert((container_id, char_id));
            }
            for &iid in &item_ids {
                if is_lock {
                    st.alsc_locked_items.insert(iid);
                } else {
                    st.alsc_locked_items.remove(&iid);
                }
                let (tid, qty, owner_id, loc_id) = ctx
                    .db
                    .with_snapshot(|s| s.items.get(&iid).map(|i| (i.type_id, i.quantity, i.owner_id, i.location_id)))
                    .unwrap_or((0, 1, char_id, container_id));
                st.alsc_logs.entry(container_id).or_default().push(AlscLogEntry {
                    timestamp_unix: now_unix,
                    actor_id: char_id,
                    action: if is_lock {
                        "ALSCActionLock".into()
                    } else {
                        "ALSCActionUnlock".into()
                    },
                    item_id: iid,
                    item_type_id: tid,
                    quantity: qty,
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
                                action: if is_lock { "lock".into() } else { "unlock".into() },
                                password_type: None,
                                type_id: Some(tid as i32),
                                quantity: Some(qty),
                                old_config_bitmask: None,
                                new_config_bitmask: None,
                            },
                        );
                    }
                });
                ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnLockedItemChange",
                    "charid",
                    vec![PyRep::Integer(iid), PyRep::Integer(owner_id), PyRep::Integer(loc_id)],
                );
            }
            save_container_ship_state(ctx, &st);
            Some(Ok(PyRep::keyval([
                ("containerID", PyRep::Integer(container_id)),
                ("locked", PyRep::Bool(is_lock)),
                ("count", PyRep::Integer(item_ids.len() as i64)),
            ])))
        }

        ("invbroker", "ALSCLogGet") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let container_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| bound_entry.map(|b| b.object_id))
                .unwrap_or(0);
            let st = read_container_ship_state(ctx);
            if let Some(req_pw) = st.container_passwords.get(&container_id) {
                if !req_pw.is_empty() && !st.unlocked_containers.contains(&(container_id, char_id)) {
                    let provided = kwargs
                        .get("password")
                        .and_then(|v| v.as_str())
                        .or_else(|| args.get(1).and_then(|a| a.as_str()))
                        .unwrap_or("");
                    if provided != req_pw {
                        return Some(Err(BridgeRpcError {
                            code: "ALSCPasswordNeededToViewAuditLog".into(),
                            message: "Container password required to view audit log".into(),
                        }));
                    }
                }
            }
            let entries: Vec<PyRep> = st
                .alsc_logs
                .get(&container_id)
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|e| {
                    PyRep::keyval([
                        ("timestampUnix", PyRep::Integer(e.timestamp_unix as i64)),
                        ("actorID", PyRep::Integer(e.actor_id)),
                        ("action", PyRep::String(e.action)),
                        ("itemID", PyRep::Integer(e.item_id)),
                        ("typeID", PyRep::Integer(e.item_type_id as i64)),
                        ("quantity", PyRep::Integer(e.quantity as i64)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::keyval([
                ("containerID", PyRep::Integer(container_id)),
                ("logCount", PyRep::Integer(entries.len() as i64)),
                ("entries", PyRep::List(entries)),
            ])))
        }

        ("invbroker", "GetDamageForCrystals") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
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
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
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
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let dock_id = session.station_id.or(session.structure_id);
            let at_dock = ctx.db.with_snapshot(|snap| {
                snap.items
                    .get(&item_id)
                    .is_some_and(|i| i.owner_id == char_id && Some(i.location_id) == dock_id)
            });
            Some(Ok(PyRep::Bool(at_dock)))
        }

        ("itemLocking", "GetLockedItemLocations") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id = session.corporation_id.unwrap_or(0);
            let alsc_locked = read_container_ship_state(ctx).alsc_locked_items;
            let locs: Vec<PyRep> = ctx.db.with_snapshot(|snap| {
                let mut set = std::collections::BTreeSet::new();
                for item in snap.items.values() {
                    let in_job = snap.industry_jobs.values().any(|j| j.status == "Active" && j.blueprint_item_id == item.item_id);
                    let vote_locked = corp_id > 0 && item.owner_id == corp_id && item.flag == InventoryFlag::Locked && !in_job;
                    let container_or_escrow = (item.owner_id == char_id || (corp_id > 0 && item.owner_id == corp_id))
                        && (alsc_locked.contains(&item.item_id) || snap.contracts.contains_key(&item.location_id));
                    if vote_locked || container_or_escrow {
                        set.insert(item.location_id);
                    }
                }
                set.into_iter().map(PyRep::Integer).collect()
            });
            Some(Ok(PyRep::List(locs)))
        }

        ("itemLocking", "GetItemsByLocation") => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let corp_id = session.corporation_id.unwrap_or(0);
            let loc_id = args.first().and_then(|a| a.as_i64());
            let alsc_locked = read_container_ship_state(ctx).alsc_locked_items;
            let locked_entries: Vec<(PyRep, PyRep)> = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|item| {
                        if !loc_id.map(|l| item.location_id == l).unwrap_or(true) {
                            return false;
                        }
                        let in_job = snap.industry_jobs.values().any(|j| j.status == "Active" && j.blueprint_item_id == item.item_id);
                        let vote_locked = corp_id > 0 && item.owner_id == corp_id && item.flag == InventoryFlag::Locked && !in_job;
                        let container_or_escrow = (item.owner_id == char_id || (corp_id > 0 && item.owner_id == corp_id))
                            && (alsc_locked.contains(&item.item_id) || snap.contracts.contains_key(&item.location_id));
                        vote_locked || container_or_escrow
                    })
                    .map(|item| {
                        (
                            PyRep::Integer(item.item_id),
                            PyRep::keyval([
                                ("itemID", PyRep::Integer(item.item_id)),
                                ("typeID", PyRep::Integer(item.type_id as i64)),
                                ("locationID", PyRep::Integer(item.location_id)),
                                ("ownerID", PyRep::Integer(item.owner_id)),
                                ("flagID", PyRep::Integer(item.flag as i64)),
                            ]),
                        )
                    })
                    .collect()
            });
            Some(Ok(PyRep::Dict(locked_entries)))
        }

        ("encodedItems", "DecodeItem") => {
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (item_id_opt, fallback_tid_opt) = match args.first() {
                Some(PyRep::Integer(id)) => (Some(*id), args.get(1).and_then(|a| a.as_i64())),
                Some(PyRep::Tuple(v) | PyRep::List(v)) => (
                    v.first().and_then(|x| x.as_i64()),
                    v.get(1).and_then(|x| x.as_i64()),
                ),
                Some(PyRep::String(s)) => {
                    let mut parts = s.split(':');
                    let p1 = parts.next().and_then(|x| x.trim().parse::<i64>().ok());
                    let p2 = parts.next().and_then(|x| x.trim().parse::<i64>().ok());
                    (p1, p2)
                }
                _ => (kwargs.get("itemID").and_then(|v| v.as_i64()), kwargs.get("typeID").and_then(|v| v.as_i64())),
            };
            let Some(raw_id) = item_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "DecodeItem requires itemID or encoded link".into(),
                }));
            };
            let item_opt = ctx.db.with_snapshot(|snap| snap.items.get(&raw_id).cloned());
            let type_id = item_opt
                .as_ref()
                .map(|i| i.type_id)
                .or_else(|| fallback_tid_opt.map(|t| t as TypeId))
                .unwrap_or(raw_id as TypeId);
            let Some(type_info) = ctx.sde.types.get(&type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "TypeNotFound".into(),
                    message: format!("Encoded item type {type_id} not found"),
                }));
            };
            let custom_name = ctx
                .db
                .with_snapshot(|snap| snap.item_custom_names.get(&raw_id).cloned())
                .unwrap_or_default();
            let mut merged_attrs = type_info.default_attributes.clone();
            let mut dyn_entries = Vec::new();
            let mut muta_id_rep = PyRep::None;
            let mut is_bpc = false;
            let mut owner_rep = PyRep::None;
            if let Some(ref item) = item_opt {
                owner_rep = PyRep::Integer(item.owner_id);
                is_bpc = item.is_blueprint_copy;
                if let Some(m) = item.source_mutaplasmid_id {
                    muta_id_rep = PyRep::Integer(m as i64);
                }
                for (&aid, &val) in &item.dynamic_attributes {
                    merged_attrs.insert(aid, val);
                    dyn_entries.push((PyRep::Integer(aid as i64), PyRep::Real(val)));
                }
            }
            let attr_entries: Vec<(PyRep, PyRep)> = merged_attrs
                .into_iter()
                .map(|(k, v)| (PyRep::Integer(k as i64), PyRep::Real(v)))
                .collect();
            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(raw_id)),
                ("typeID", PyRep::Integer(type_id as i64)),
                ("typeName", PyRep::String(type_info.name.clone())),
                ("customName", PyRep::String(custom_name)),
                ("ownerID", owner_rep),
                ("isBlueprintCopy", PyRep::Bool(is_bpc)),
                ("sourceMutaplasmidID", muta_id_rep),
                ("dynamicAttributes", PyRep::Dict(dyn_entries)),
                ("attributes", PyRep::Dict(attr_entries)),
            ])))
        }

        _ => handle_ships_mastery_and_fitting_ext(ctx, session, bound_entry, service, method, args, kwargs).await,
    }
}
