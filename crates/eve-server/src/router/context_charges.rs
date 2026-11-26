//! Weapon charge loading and compatibility routines (`impl ServerContext`).

use super::context::ServerContext;
use eve_proto::{BridgeRpcError, ItemQty};
use std::collections::HashMap;

fn is_crystal_charge_group(group_id: u32) -> bool {
    matches!(group_id, 86 | 374 | 375 | 482 | 663)
}

fn is_script_charge_group(group_id: u32) -> bool {
    matches!(
        group_id,
        907..=911 | 1153 | 1400 | 1549..=1551 | 1559 | 1569 | 1701 | 1702
    )
}

fn is_wear_only_crystal_attrs(item: &eve_db::InventoryItemRecord) -> bool {
    item.source_mutaplasmid_id.is_none()
        && item.insured_payout_isk.is_finite()
        && item.insured_payout_isk == 0.0
        && item
            .dynamic_attributes
            .keys()
            .all(|k| *k == eve_sde::attr_ids::DAMAGE || *k == eve_sde::attr_ids::HP_STRUCT)
}

fn is_pristine_stackable_charge(item: &eve_db::InventoryItemRecord) -> bool {
    !item.singleton
        && item.dynamic_attributes.is_empty()
        && item.source_mutaplasmid_id.is_none()
        && !item.is_blueprint_copy
        && item.insured_payout_isk.is_finite()
        && item.insured_payout_isk == 0.0
}

fn map_charge_consume_err(
    err: eve_db::DbError,
    ship_id: i64,
    item_id: i64,
    type_id: u32,
    type_name: &str,
    char_id: i64,
) -> BridgeRpcError {
    match err {
        eve_db::DbError::ItemOwnershipViolation { .. } => {
            BridgeRpcError::ownership_or_location_violation(
                item_id,
                None,
                None,
                Some(char_id),
                None,
                err.to_string(),
            )
        }
        other => BridgeRpcError::fitting_validation_failed(
            Some(ship_id),
            Some(item_id),
            Some(i64::from(type_id)),
            type_name,
            other.to_string(),
        ),
    }
}

impl ServerContext {
    pub fn is_charge_compatible_with_weapon(
        weapon_type: &eve_sde::InvType,
        charge_type: &eve_sde::InvType,
    ) -> bool {
        if charge_type.category_id != 8 {
            return false;
        }
        // Modulated miners carry chargeGroup1/2 (attrs 604/605) in SDE; ORE Deep Core Mining Laser
        // (28748) is the single deep-core laser whose SDE entry omits chargeGroup1.
        let mining_crystal_ok = weapon_type.type_id == 28_748
            && matches!(charge_type.group_id, 482 | 663);
        let group_ok = mining_crystal_ok
            || [604_u16, 605, 606, 609, 610].iter().any(|attr_id| {
                weapon_type
                    .default_attributes
                    .get(attr_id)
                    .map(|g| (*g as u32) == charge_type.group_id)
                    .unwrap_or(false)
            })
            || (![604_u16, 605, 606, 609, 610]
                .iter()
                .any(|attr_id| weapon_type.default_attributes.contains_key(attr_id))
                && charge_type
                    .default_attributes
                    .get(&137)
                    .map(|lg| *lg as u32 == weapon_type.group_id)
                    .unwrap_or(false));
        let size_ok = match (
            weapon_type.default_attributes.get(&128).copied(),
            charge_type.default_attributes.get(&128).copied(),
        ) {
            (Some(rs), Some(cs)) => (rs - cs).abs() < 0.5,
            _ => true,
        };
        group_ok && size_ok
    }

    pub fn weapon_slot_max_charge_capacity(
        weapon_type: &eve_sde::InvType,
        charge_type: &eve_sde::InvType,
    ) -> i32 {
        let cap_m3 = if weapon_type.capacity > 0.0 {
            weapon_type.capacity
        } else {
            5.0
        };
        let vol_m3 = charge_type.volume.max(1e-6);
        ((cap_m3 / vol_m3).floor() as i32).max(1)
    }

    /// Loads or swaps an ammunition/missile charge stack into a fitted high-slot weapon (`flag 27..=34`),
    /// enforcing weapon/charge group & size compatibility, character skill prerequisites (`182..=184`/`277..=279`),
    /// and `floor(weapon.capacity / charge.volume)` clip bounds (splitting oversized stacks and unloading
    /// any previously loaded charge of a different type back into `ship_id` `Cargo`).
    pub fn load_charge_into_weapon_slot(
        &self,
        char_id: i64,
        ship_id: i64,
        flag: eve_db::InventoryFlag,
        charge_item_id: i64,
        current_dock: Option<i64>,
    ) -> Result<(i64, u32, i32, u64), BridgeRpcError> {
        let loaded = self.db.try_with_mut_snapshot(|snap| {
            self.load_charge_into_weapon_slot_in_snapshot(
                snap,
                char_id,
                ship_id,
                flag,
                charge_item_id,
                current_dock,
            )
        })?;
        self.invalidate_ship_fitting(ship_id);
        Ok(loaded)
    }

    pub fn load_charge_into_weapon_slot_in_snapshot(
        &self,
        snap: &mut eve_db::WorldStateSnapshot,
        char_id: i64,
        ship_id: i64,
        flag: eve_db::InventoryFlag,
        charge_item_id: i64,
        current_dock: Option<i64>,
    ) -> Result<(i64, u32, i32, u64), BridgeRpcError> {
        let flag_u16 = flag as u16;
        if !(11..=34).contains(&flag_u16) {
            return Err(BridgeRpcError {
                code: "InvalidWeaponSlot".into(),
                message: format!(
                    "Charges/scripts can only be loaded into module slots 11..=34 (got {flag_u16})"
                ),
                client_error: None,
            });
        }
        {
            let Some(ship_item) = snap.items.get(&ship_id).cloned() else {
                return Err(BridgeRpcError {
                    code: "ShipNotFound".into(),
                    message: format!("Target ship {ship_id} not found"),
                    client_error: None,
                });
            };
            let is_ship = self
                .sde
                .types
                .get(&ship_item.type_id)
                .map(|t| t.category_id == 6)
                .unwrap_or(false);
            if ship_item.owner_id != char_id
                || !is_ship
                || !snap.is_ship_colocated_with_character(char_id, ship_id, current_dock)
            {
                return Err(BridgeRpcError {
                    code: "InvalidFittingTarget".into(),
                    message: format!("Target {ship_id} is not an owned co-located ship"),
                    client_error: None,
                });
            }

            let weapon_type_id = snap
                .items
                .values()
                .find(|i| {
                    i.location_id == ship_id
                        && i.flag == flag
                        && self
                            .sde
                            .types
                            .get(&i.type_id)
                            .map(|t| t.category_id == 7)
                            .unwrap_or(false)
                })
                .map(|i| i.type_id);
            let Some(wtid) = weapon_type_id else {
                return Err(BridgeRpcError {
                    code: "NoWeaponFitted".into(),
                    message: format!(
                        "High slot {flag_u16} on ship {ship_id} has no fitted weapon module"
                    ),
                    client_error: None,
                });
            };
            let Some(weapon_type) = self.sde.types.get(&wtid) else {
                return Err(BridgeRpcError {
                    code: "UnknownWeaponType".into(),
                    message: format!("Unknown weapon type {wtid}"),
                    client_error: None,
                });
            };

            let Some(src_charge) = snap.items.get(&charge_item_id).cloned() else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Charge item {charge_item_id} not found"),
                    client_error: None,
                });
            };
            if src_charge.location_id == ship_id && src_charge.flag == flag {
                return Ok((
                    src_charge.item_id,
                    src_charge.type_id,
                    src_charge.quantity,
                    src_charge.version,
                ));
            }
            if !snap.is_item_consumable_by(&src_charge, char_id, current_dock, Some(ship_id)) {
                return Err(BridgeRpcError {
                    code: "AmmoNotReachable".into(),
                    message: format!(
                        "Charge item {charge_item_id} must be owned, unlocked, and in ship {ship_id} Cargo or current station Hangar"
                    ),
                    client_error: None,
                });
            }
            let Some(charge_type) = self.sde.types.get(&src_charge.type_id) else {
                return Err(BridgeRpcError {
                    code: "UnknownChargeType".into(),
                    message: format!("Unknown charge type {}", src_charge.type_id),
                    client_error: None,
                });
            };
            if !Self::is_charge_compatible_with_weapon(weapon_type, charge_type) {
                return Err(BridgeRpcError {
                    code: "IncompatibleAmmo".into(),
                    message: format!(
                        "Charge {} (type {}) is not compatible with weapon {} (type {wtid})",
                        charge_type.name, src_charge.type_id, weapon_type.name
                    ),
                    client_error: None,
                });
            }
            self.check_type_skill_prerequisites_in_snapshot(snap, char_id, src_charge.type_id)?;

            let is_crystal_type = is_crystal_charge_group(charge_type.group_id);
            let is_script_type = is_script_charge_group(charge_type.group_id);
            let is_wear_only_crystal = crate::services::inventory_fitting::fitting_ewar_capital::is_wear_only_non_singleton_crystal(&src_charge);
            let is_single_crystal_move = is_crystal_type
                && src_charge.quantity == 1
                && (src_charge.singleton || is_wear_only_crystal);
            let is_single_script_move =
                is_script_type && src_charge.quantity == 1 && src_charge.singleton;

            if is_single_crystal_move && !is_wear_only_crystal_attrs(&src_charge) {
                return Err(BridgeRpcError::fitting_validation_failed(
                    Some(ship_id),
                    Some(charge_item_id),
                    Some(i64::from(src_charge.type_id)),
                    &charge_type.name,
                    format!("Cannot load mutated or insured crystal item {charge_item_id}"),
                ));
            }

            let policy = if is_single_crystal_move {
                eve_db::ItemClassPolicy::ExactAssembledItem
            } else if is_single_script_move {
                eve_db::ItemClassPolicy::ExactBlueprintOrSingleton
            } else {
                eve_db::ItemClassPolicy::StackableCommodity
            };
            let consume_opts = if matches!(policy, eve_db::ItemClassPolicy::StackableCommodity) {
                eve_db::ConsumeExactOptions::for_character_stack(char_id)
            } else {
                eve_db::ConsumeExactOptions {
                    reject_blueprint_copy: true,
                    ..eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), char_id, policy)
                }
            };
            snap.validate_item_for_consumption(&src_charge, &consume_opts)
                .map_err(|e| {
                    map_charge_consume_err(
                        e,
                        ship_id,
                        charge_item_id,
                        src_charge.type_id,
                        &charge_type.name,
                        char_id,
                    )
                })?;

            let max_cap = Self::weapon_slot_max_charge_capacity(weapon_type, charge_type);
            let slot_cap = if is_crystal_type { 1 } else { max_cap };

            // Check if a charge is already in `(ship_id, flag)`
            let existing_loaded: Option<eve_db::InventoryItemRecord> = snap
                .items
                .values()
                .find(|i| {
                    i.location_id == ship_id
                        && i.flag == flag
                        && self
                            .sde
                            .types
                            .get(&i.type_id)
                            .map(|t| t.category_id == 8)
                            .unwrap_or(false)
                })
                .cloned();

            let mut current_same_id_qty: Option<(i64, i32)> = None;
            if let Some(ex_item) = existing_loaded {
                let ex_id = ex_item.item_id;
                let ex_type_id = ex_item.type_id;
                let ex_qty = ex_item.quantity;
                let ex_is_crystal = self
                    .sde
                    .types
                    .get(&ex_type_id)
                    .is_some_and(|t| is_crystal_charge_group(t.group_id));
                let can_top_up_same = ex_type_id == src_charge.type_id
                    && !is_crystal_type
                    && !ex_is_crystal
                    && is_pristine_stackable_charge(&ex_item)
                    && is_pristine_stackable_charge(&src_charge);
                if can_top_up_same {
                    current_same_id_qty = Some((ex_id, ex_qty));
                } else {
                    // ChangeAmmo or crystal/singleton/worn swap: unload previous charge back to `(ship_id, Cargo)`
                    let ex_mergeable = is_pristine_stackable_charge(&ex_item)
                        && !snap.is_courier_cargo_item(ex_id)
                        && !snap.is_item_locked_in_contract_or_job(ex_id);
                    let merge_cargo_id = if ex_mergeable {
                        snap.items
                            .values()
                            .find(|i| {
                                i.item_id != ex_id
                                    && i.owner_id == char_id
                                    && i.location_id == ship_id
                                    && i.flag == eve_db::InventoryFlag::Cargo
                                    && i.type_id == ex_type_id
                                    && is_pristine_stackable_charge(i)
                                    && !snap.is_courier_cargo_item(i.item_id)
                                    && !snap.is_item_locked_in_contract_or_job(i.item_id)
                                    && i.quantity.checked_add(ex_qty).is_some()
                            })
                            .map(|i| i.item_id)
                    } else {
                        None
                    };
                    if let Some(mc_id) = merge_cargo_id {
                        let mc_qty = snap.items.get(&mc_id).map(|m| m.quantity).unwrap_or(0);
                        let new_mc_qty = ItemQty::try_from_i32(mc_qty)
                            .and_then(|q| q.checked_add(ItemQty::try_from_i32(ex_qty)?))
                            .and_then(|q| q.try_into_stack_i32())
                            .map_err(|e| {
                                BridgeRpcError::fitting_validation_failed(
                                    Some(ship_id),
                                    Some(mc_id),
                                    Some(i64::from(ex_type_id)),
                                    &charge_type.name,
                                    e.to_string(),
                                )
                            })?;
                        snap.items.remove(&ex_id);
                        if let Some(mc) = snap.items.get_mut(&mc_id) {
                            mc.quantity = new_mc_qty;
                            mc.version += 1;
                        }
                    } else if let Some(ex_mut) = snap.items.get_mut(&ex_id) {
                        ex_mut.location_id = ship_id;
                        ex_mut.flag = eve_db::InventoryFlag::Cargo;
                        if ex_mut.quantity == 1 && !ex_mut.dynamic_attributes.is_empty() && ex_is_crystal {
                            ex_mut.singleton = true;
                        }
                        ex_mut.version += 1;
                    }
                }
            }

            let cur_qty = current_same_id_qty.map(|(_, q)| q).unwrap_or(0);
            let room = slot_cap.checked_sub(cur_qty).unwrap_or(0).max(0);
            if !is_crystal_type && room == 0 {
                if let Some((ex_id, ex_q)) = current_same_id_qty {
                    let ver = snap.items.get(&ex_id).map(|i| i.version).unwrap_or(1);
                    return Ok((ex_id, src_charge.type_id, ex_q, ver));
                }
            }
            let to_load = if is_crystal_type {
                1
            } else {
                src_charge.quantity.min(room)
            };
            let qty_to_load = ItemQty::try_from_i32(to_load).map_err(|e| {
                BridgeRpcError::fitting_validation_failed(
                    Some(ship_id),
                    Some(charge_item_id),
                    Some(i64::from(src_charge.type_id)),
                    &charge_type.name,
                    e.to_string(),
                )
            })?;

            if let Some((ex_id, ex_qty)) = current_same_id_qty {
                let new_ex_qty = ItemQty::try_from_i32(ex_qty)
                    .and_then(|q| q.checked_add(qty_to_load))
                    .and_then(|q| q.try_into_stack_i32())
                    .map_err(|e| {
                        BridgeRpcError::fitting_validation_failed(
                            Some(ship_id),
                            Some(ex_id),
                            Some(i64::from(src_charge.type_id)),
                            &charge_type.name,
                            e.to_string(),
                        )
                    })?;
                snap.consume_exact_item_quantity(charge_item_id, to_load, consume_opts)
                    .map_err(|e| {
                        map_charge_consume_err(
                            e,
                            ship_id,
                            charge_item_id,
                            src_charge.type_id,
                            &charge_type.name,
                            char_id,
                        )
                    })?;
                let ex_mut = snap.items.get_mut(&ex_id).ok_or_else(|| {
                    BridgeRpcError::fitting_validation_failed(
                        Some(ship_id),
                        Some(ex_id),
                        Some(i64::from(src_charge.type_id)),
                        &charge_type.name,
                        format!("Loaded charge {ex_id} missing during top-up"),
                    )
                })?;
                ex_mut.quantity = new_ex_qty;
                ex_mut.version += 1;
                Ok((ex_id, ex_mut.type_id, ex_mut.quantity, ex_mut.version))
            } else {
                let saved_custom_name = if src_charge.quantity == to_load {
                    snap.item_custom_names.get(&charge_item_id).cloned()
                } else {
                    None
                };
                let consumed = snap
                    .consume_exact_item_quantity(charge_item_id, to_load, consume_opts)
                    .map_err(|e| {
                        map_charge_consume_err(
                            e,
                            ship_id,
                            charge_item_id,
                            src_charge.type_id,
                            &charge_type.name,
                            char_id,
                        )
                    })?;
                let (loaded_id, new_rec) = if consumed.is_none() {
                    let mut moved = src_charge.clone();
                    moved.location_id = ship_id;
                    moved.flag = flag;
                    moved.quantity = to_load;
                    moved.singleton = moved.singleton || is_crystal_type;
                    moved.version += 1;
                    if let Some(name) = saved_custom_name {
                        snap.item_custom_names.insert(charge_item_id, name);
                    }
                    (charge_item_id, moved)
                } else {
                    let loaded_id = snap.alloc_entity_id();
                    let split_rec = eve_db::InventoryItemRecord {
                        item_id: loaded_id,
                        type_id: src_charge.type_id,
                        owner_id: char_id,
                        location_id: ship_id,
                        flag,
                        quantity: to_load,
                        singleton: is_crystal_type,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    };
                    (loaded_id, split_rec)
                };
                let ver = new_rec.version;
                snap.items.insert(loaded_id, new_rec);
                Ok((loaded_id, src_charge.type_id, to_load, ver))
            }
        }
    }
}

#[cfg(test)]
#[path = "context_charges_tests.rs"]
mod context_charges_tests;

