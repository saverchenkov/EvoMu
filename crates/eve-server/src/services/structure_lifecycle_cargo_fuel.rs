//! `structure_lifecycle_cargo_fuel` — Upwell structure fuel jettison, structure deliveries,
//! and in-space cargo dropoff (`structureCargoDelivery.DropOffItems`).
#![forbid(unsafe_code)]

use super::err;
use crate::router::ServerContext;
use crate::services::station_structure::structure_lifecycle as life;
use crate::services::station_structure::structure_upwell_ops::{self as ops, load_ext_state, save_ext_state};
use eve_db::InventoryFlag;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

pub(crate) fn is_pilot_docked_or_controlling_structure(
    snap: &eve_db::WorldStateSnapshot,
    ext: &ops::StructureExtendedState,
    session: &SessionState,
    char_id: i64,
    structure_id: i64,
) -> (bool, bool) {
    let docked_here = session.station_id.is_none()
        && (session.structure_id == Some(structure_id)
            || (session.structure_id.is_none()
                && snap
                    .characters
                    .get(&char_id)
                    .and_then(|c| c.structure_id)
                    == Some(structure_id)));
    let controlling_here = ext.active_pilot_by_structure.get(&structure_id) == Some(&char_id);
    (docked_here, controlling_here)
}

/// `structure.JettisonStructureFuel(itemIDs)` (`invItemFunctions.pyj:77`): validates that the
/// caller is docked in (or actively controlling) the structure, has Director/Station Manager/Fuel
/// Technician role, removes fuel blocks from the fuel bay, synchronizes low-power service shutdown,
/// and places the jettisoned fuel into a cargo container in space outside the structure.
pub async fn handle_jettison_structure_fuel(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now = ctx.now_unix();
    let bound_struct_id = ctx
        .bound_objects
        .read()
        .ok()
        .and_then(|map| {
            map.values()
                .find(|b| b.session_id == session.session_id && b.service == "structure")
                .map(|b| b.object_id)
        });
    let mut explicit_item_ids = Vec::new();
    for a in args {
        if let PyRep::List(list) | PyRep::Tuple(list) = a {
            for v in list {
                if let Some(id) = v.as_i64() {
                    explicit_item_ids.push(id);
                }
            }
        }
    }
    use crate::services::structure_acl_defense_rules as rules;
    let (structure_id, sys_id, container_id, jettisoned_ids, remaining_fuel, pos, released, service_changed) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let mut st_id_opt = args.first().and_then(|a| a.as_i64()).filter(|id| snap.structures.contains_key(id));
            if st_id_opt.is_none() {
                if let Some(&first_iid) = explicit_item_ids.first() {
                    st_id_opt = snap.items.get(&first_iid).map(|it| it.location_id).filter(|id| snap.structures.contains_key(id));
                } else if let Some(first_int) = args.first().and_then(|a| a.as_i64()) {
                    if let Some(loc) = snap.items.get(&first_int).map(|it| it.location_id).filter(|id| snap.structures.contains_key(id)) {
                        explicit_item_ids.push(first_int);
                        st_id_opt = Some(loc);
                    }
                }
            }
            let structure_id = st_id_opt.or(bound_struct_id).or(session.structure_id)
                .ok_or_else(|| err("MissingStructureId", "JettisonStructureFuel requires an active or bound structureID"))?;
            rules::sync_seeded_structure_fuel_bay(ctx, snap, structure_id);
            let st = snap.structures.get(&structure_id).cloned()
                .ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
            if st.state == life::STATE_DESTROYED {
                return Err(err("StructureDestroyed", format!("Structure {structure_id} is destroyed")));
            }
            let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
            if caller_corp != st.owner_corp_id {
                return Err(err("StructureOwnershipViolation", format!("Character {char_id} does not belong to structure owning corp {}", st.owner_corp_id)));
            }
            let mut ext = load_ext_state(snap);
            let (docked_here, controlling_here) =
                is_pilot_docked_or_controlling_structure(snap, &ext, session, char_id, structure_id);
            if !docked_here && !controlling_here {
                return Err(err("NotDockedInStructure", format!("Must be docked in or actively controlling structure {structure_id} to jettison fuel")));
            }
            if !rules::has_structure_fuel_bay_role(snap, session, char_id, caller_corp) {
                return Err(err("InsufficientRoles", "Jettisoning structure fuel requires Director, Station Manager, or Fuel Technician role"));
            }
            let power_before = life::power_state(ctx, snap, &ext, &st, now);
            snap.next_entity_id += 1;
            let container_id = snap.next_entity_id;
            snap.items.insert(container_id, eve_db::InventoryItemRecord {
                item_id: container_id, type_id: 23, owner_id: st.owner_corp_id,
                location_id: i64::from(st.solar_system_id), flag: InventoryFlag::None,
                quantity: 1, singleton: true, version: 1, ..Default::default()
            });
            let mut moved_ids = Vec::new();
            let mut moved_blocks = 0u32;
            if !explicit_item_ids.is_empty() {
                for iid in &explicit_item_ids {
                    let (it_loc, it_owner, it_flag, it_type, it_qty) = snap
                        .items
                        .get(iid)
                        .map(|it| (it.location_id, it.owner_id, it.flag, it.type_id, it.quantity))
                        .ok_or_else(|| err("ItemNotFound", format!("Fuel item {iid} not found")))?;
                    if it_loc != structure_id {
                        return Err(err("ItemNotInFuelBay", format!("Item {iid} is not located in structure {structure_id}")));
                    }
                    if it_owner != st.owner_corp_id {
                        return Err(err("ItemOwnershipViolation", format!("Item {iid} is not owned by structure owner corp {}", st.owner_corp_id)));
                    }
                    let in_fuel_bay = it_flag == InventoryFlag::SpecializedFuelBay || it_flag == InventoryFlag::StructureFuel || it_flag as u16 == 172 || it_flag as u16 == 163;
                    if !in_fuel_bay || !rules::is_valid_structure_fuel_bay_item(ctx, it_type) {
                        return Err(err("ItemNotFuel", format!("Item {iid} (type {it_type}) is not structure fuel in fuel bay")));
                    }
                    if rules::is_fuel_block_type(ctx, it_type) {
                        moved_blocks = moved_blocks.saturating_add(it_qty.max(0) as u32);
                    }
                    // Fuel Technician / Station Manager / Director authorization already checked by `has_structure_fuel_bay_role`.
                    let mut spec = eve_db::TransferItemSpec::move_whole(
                        None,
                        Some(st.owner_corp_id),
                        None,
                        container_id,
                        InventoryFlag::Cargo,
                    );
                    spec.reject_fitted = false;
                    snap.transfer_inventory_item(*iid, spec)
                        .map_err(|e| err("InventoryError", e.to_string()))?;
                    moved_ids.push(*iid);
                }
                if let Some(s) = snap.structures.get_mut(&structure_id) {
                    s.fuel_blocks = s.fuel_blocks.saturating_sub(moved_blocks);
                }
            } else {
                let default_tid = snap.items.values()
                    .filter(|i| i.location_id == structure_id && rules::is_structure_fuel_flag(i.flag) && i.quantity > 0 && rules::is_fuel_block_type(ctx, i.type_id))
                    .min_by_key(|i| i.item_id).map(|i| i64::from(i.type_id)).unwrap_or(4247);
                let (fuel_tid, req_qty) = if args.len() >= 3 {
                    (args.get(1).and_then(|a| a.as_i64()).unwrap_or(default_tid), args.get(2).and_then(|a| a.as_i64()).unwrap_or(0))
                } else if args.len() == 2 {
                    (default_tid, args.get(1).and_then(|a| a.as_i64()).unwrap_or(0))
                } else {
                    (default_tid, i64::from(st.fuel_blocks))
                };
                let fuel_tid_u32 = u32::try_from(fuel_tid).map_err(|_| err("InvalidArgument", format!("Invalid fuel typeID {fuel_tid}")))?;
                if !rules::is_valid_structure_fuel_bay_item(ctx, fuel_tid_u32) {
                    return Err(err("ItemNotFuel", format!("TypeID {fuel_tid} is not a valid structure fuel type")));
                }
                let req_qty_u32 = u32::try_from(req_qty).map_err(|_| err("InvalidArgument", format!("Invalid fuel quantity {req_qty}")))?;
                let mut bay_matching: Vec<i64> = snap.items.values()
                    .filter(|i| i.location_id == structure_id && rules::is_structure_fuel_flag(i.flag) && i.type_id == fuel_tid_u32 && i.quantity > 0)
                    .map(|i| i.item_id).collect();
                bay_matching.sort_unstable();
                let matching_qty: u32 = bay_matching.iter().filter_map(|id| snap.items.get(id)).map(|i| i.quantity.max(0) as u32).fold(0, u32::saturating_add);
                let is_block = rules::is_fuel_block_type(ctx, fuel_tid_u32);
                let avail_for_tid = if is_block {
                    let backed_all: u32 = snap.items.values()
                        .filter(|i| i.location_id == structure_id && rules::is_structure_fuel_flag(i.flag) && i.quantity > 0 && rules::is_fuel_block_type(ctx, i.type_id))
                        .map(|i| i.quantity as u32).fold(0, u32::saturating_add);
                    matching_qty.saturating_add(st.fuel_blocks.saturating_sub(backed_all)).min(st.fuel_blocks)
                } else {
                    matching_qty
                };
                if req_qty_u32 == 0 || req_qty_u32 > avail_for_tid {
                    return Err(err("InsufficientStructureFuel", format!("Cannot jettison {req_qty} of type {fuel_tid_u32} from structure {structure_id} (available: {avail_for_tid})")));
                }
                let mut rem = req_qty_u32;
                for iid in bay_matching {
                    if rem == 0 { break; }
                    let Some((it_owner, avail)) = snap.items.get(&iid).map(|it| (it.owner_id, it.quantity.max(0) as u32)) else { continue; };
                    let take = rem.min(avail);
                    if take == 0 { continue; }
                    let mut opts = eve_db::ConsumeExactOptions::for_owner_exact(None, it_owner);
                    opts.reject_singleton = true;
                    if snap.consume_exact_item_quantity(iid, take as i32, opts).is_ok() {
                        rem -= take;
                    }
                }
                if is_block {
                    if let Some(s) = snap.structures.get_mut(&structure_id) {
                        s.fuel_blocks = s.fuel_blocks.saturating_sub(req_qty_u32);
                    }
                }
                snap.next_entity_id += 1;
                let fuel_item_id = snap.next_entity_id;
                snap.items.insert(fuel_item_id, eve_db::InventoryItemRecord {
                    item_id: fuel_item_id, type_id: fuel_tid_u32, owner_id: st.owner_corp_id,
                    location_id: container_id, flag: InventoryFlag::Cargo,
                    quantity: i32::try_from(req_qty_u32).unwrap_or(i32::MAX), singleton: false, version: 1, ..Default::default()
                });
                moved_ids.push(fuel_item_id);
            }
            let mut released = Vec::new();
            life::sync_structure_with_released(ctx, snap, &mut ext, structure_id, now, &mut released);
            let st_after = snap.structures.get(&structure_id).cloned().unwrap_or(st.clone());
            let power_after = life::power_state(ctx, snap, &ext, &st_after, now);
            if let Some(profile) = life::structure_profile(ctx, st_after.type_id) {
                let max_after = life::effective_max_shield(ctx, snap, &profile, structure_id, power_after);
                if let Some(s) = snap.structures.get_mut(&structure_id) {
                    s.shield_hp = s.shield_hp.min(max_after);
                }
            }
            save_ext_state(snap, &ext);
            crate::services::industry_pi::industry_facilities_bps::sync_industry_job_pause_states_on_service_change(snap, now);
            let rem = snap.structures.get(&structure_id).map(|s| s.fuel_blocks).unwrap_or(0);
            Ok((structure_id, st.solar_system_id, container_id, moved_ids, rem, st.position, released, power_before != power_after))
        })?;

    crate::services::structure_control_ops::notify_released_structure_controllers(ctx, &released);
    if service_changed {
        ctx.notify(
            crate::router::NotificationTarget::Station(structure_id),
            "OnStructureServiceChanged",
            "stationid",
            vec![PyRep::Integer(structure_id)],
        );
    }
    let ball = eve_destiny::Ball {
        id: container_id,
        type_id: 23,
        position: DVec3::from_array(pos) + DVec3::new(500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 1000.0,
        radius: 25.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let _ = ctx.systems.get_or_spawn(sys_id).spawn_ball(ball).await;
    Ok(PyRep::keyval([
        ("structureID", PyRep::Integer(structure_id)),
        ("containerID", PyRep::Integer(container_id)),
        ("jettisonedItemIDs", PyRep::List(jettisoned_ids.into_iter().map(PyRep::Integer).collect())),
        ("remainingFuelBlocks", PyRep::Integer(i64::from(remaining_fuel))),
    ]))
}

const CARGO_DROPOFF_EXTRA_RANGE_M: f64 = 2_500.0;

pub(crate) fn handle_structure_deliver(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let structure_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| err("MissingStructureId", "structureDeliveries.Deliver requires structureID"))?;
    let item_id = args.get(1).and_then(|a| a.as_i64()).ok_or_else(|| err("MissingItemId", "structureDeliveries.Deliver requires itemID"))?;
    let recipient_id = args.get(2).and_then(|a| a.as_i64()).ok_or_else(|| err("MissingRecipientId", "structureDeliveries.Deliver requires recipient characterID"))?;
    if session.structure_id != Some(structure_id) {
        return Err(err("NotDockedInStructure", format!("Must be docked in structure {structure_id} to deliver items")));
    }
    if session.ship_id == Some(item_id) {
        return Err(err("CannotDeliverActiveShip", "Cannot deliver your active boarded ship"));
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        if snap.is_character_boarded_or_saved_ship(char_id, session.ship_id, item_id) {
            return Err(err("CannotDeliverActiveShip", "Cannot deliver your active boarded ship"));
        }
        if !snap.characters.contains_key(&recipient_id) {
            return Err(err("RecipientNotFound", format!("Recipient character {recipient_id} not found")));
        }
        let Some(it) = snap.items.get(&item_id) else {
            return Err(err("ItemNotFound", format!("Item {item_id} not found")));
        };
        if it.owner_id != char_id || it.location_id != structure_id || it.flag != InventoryFlag::Hangar {
            return Err(err("ItemNotInStructureHangar", format!("Item {item_id} must be owned by {char_id} inside structure {structure_id}")));
        }
        if snap.is_item_locked_in_contract_or_job(item_id) {
            return Err(err("ItemLocked", format!("Item {item_id} is locked in a contract or job")));
        }
        if snap.items.values().any(|child| child.location_id == item_id) {
            return Err(err("CannotDeliverNonEmptyContainer", format!("Item {item_id} contains nested items and must be repackaged or emptied before delivery")));
        }
        if let Some(it) = snap.items.get_mut(&item_id) {
            it.owner_id = recipient_id;
            it.flag = InventoryFlag::Deliveries;
            it.version += 1;
        }
        Ok(PyRep::None)
    })
}

pub(crate) async fn handle_cargo_dropoff(ctx: &ServerContext, session: &mut SessionState, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(err("MustBeInSpace", "structureCargoDelivery.DropOffItems requires a ship in space"));
    }
    let ship_id = session.ship_id.ok_or_else(|| err("NoActiveShip", "No active ship in space"))?;
    let structure_id = args.first().and_then(|a| a.as_i64()).ok_or_else(|| err("MissingStructureId", "DropOffItems requires structureID"))?;
    let item_ids: Vec<i64> = match args.get(1) {
        Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => items.iter().filter_map(|x| x.as_i64()).collect(),
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    if item_ids.is_empty() {
        return Err(err("NoItemsSpecified", "DropOffItems requires at least one itemID"));
    }
    let sys_id = session.solar_system_id.unwrap_or(0);
    ops::sync_pending_profiles_in_ctx(ctx);
    let (st_pos, st_type) = ctx.db.with_snapshot(|snap| {
        let st = snap
            .structures
            .get(&structure_id)
            .filter(|s| s.solar_system_id == sys_id && s.state != "Destroyed")
            .ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not in solar system {sys_id}")))?;
        let prof = life::structure_profile(ctx, st.type_id);
        if prof.as_ref().is_some_and(|p| p.hull_class == life::HullClass::Flex) {
            return Err(err("StructureNotDockable", format!("Structure {structure_id} is a FLEX structure and has no cargo dropoff bay")));
        }
        if !crate::services::structure_control_ops::is_structure_combat_operational(Some(ctx), st)
            || st.state == life::STATE_HULL_REINFORCED
        {
            return Err(err("StructureNotOperational", format!("Structure {structure_id} is in state '{}' (cored={}) and cannot accept cargo dropoff", st.state, st.quantum_core_installed)));
        }
        if !ops::evaluate_character_structure_access(ctx, snap, structure_id, char_id) {
            return Err(err("StructureDockingAccessDenied", format!("Character {char_id} is denied cargo dropoff access by structure {structure_id} ACL")));
        }
        Ok((DVec3::from_array(st.position), st.type_id))
    })?;
    let st_radius = ctx.sde.types.get(&st_type).map(|t| t.radius).unwrap_or(0.0);
    let (_, ship_ball) = ctx.systems.get_or_spawn(sys_id).get_ball_snapshot(ship_id).await.map_err(|e| err("DestinyQueryFailed", e))?.ok_or_else(|| err("ShipNotInSpace", "Active ship not in space"))?;
    if ship_ball.position.distance(st_pos) > st_radius + CARGO_DROPOFF_EXTRA_RANGE_M {
        return Err(err("OutsideCargoDropoffRange", "Ship is too far from the structure to drop off cargo"));
    }
    ctx.db.try_with_mut_snapshot(|snap| {
        let st = snap
            .structures
            .get(&structure_id)
            .ok_or_else(|| err("StructureNotFound", format!("Structure {structure_id} not found")))?;
        if !crate::services::structure_control_ops::is_structure_combat_operational(Some(ctx), st)
            || st.state == life::STATE_HULL_REINFORCED
        {
            return Err(err("StructureNotOperational", format!("Structure {structure_id} is not operational for cargo dropoff")));
        }
        if !ops::evaluate_character_structure_access(ctx, snap, structure_id, char_id) {
            return Err(err("StructureDockingAccessDenied", format!("Character {char_id} is denied cargo dropoff access by structure {structure_id} ACL")));
        }
        for &iid in &item_ids {
            let valid_cargo = snap.items.get(&iid).is_some_and(|it| {
                it.owner_id == char_id
                    && it.location_id == ship_id
                    && matches!(
                        it.flag,
                        InventoryFlag::Cargo
                            | InventoryFlag::SpecializedOreHold
                            | InventoryFlag::SpecializedGasHold
                            | InventoryFlag::SpecializedIceHold
                            | InventoryFlag::SpecializedMineralHold
                            | InventoryFlag::SpecializedAmmoHold
                            | InventoryFlag::SpecializedFuelBay
                    )
            });
            if !valid_cargo {
                return Err(err("ItemNotInShipCargo", format!("Item {iid} is not in active ship {ship_id} cargo")));
            }
            if snap.is_item_locked_in_contract_or_job(iid) {
                return Err(err("ItemLocked", format!("Item {iid} is locked in a contract or job")));
            }
        }
        for &iid in &item_ids {
            if let Some(it) = snap.items.get_mut(&iid) {
                it.location_id = structure_id;
                it.flag = InventoryFlag::Deliveries;
                it.version += 1;
            }
        }
        Ok(PyRep::None)
    })
}
