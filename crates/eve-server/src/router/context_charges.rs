//! Weapon charge loading and compatibility routines (`impl ServerContext`).

use super::context::ServerContext;
use eve_proto::BridgeRpcError;
use std::collections::HashMap;

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
        let res = self.db.try_with_mut_snapshot(|snap| {
            self.load_charge_into_weapon_slot_in_snapshot(
                snap,
                char_id,
                ship_id,
                flag,
                charge_item_id,
                current_dock,
            )
        });
        if res.is_ok() {
            self.invalidate_ship_fitting(ship_id);
        }
        res
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
            });
        }
        {
            let Some(ship_item) = snap.items.get(&ship_id).cloned() else {
                return Err(BridgeRpcError {
                    code: "ShipNotFound".into(),
                    message: format!("Target ship {ship_id} not found"),
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
                });
            };
            let Some(weapon_type) = self.sde.types.get(&wtid) else {
                return Err(BridgeRpcError {
                    code: "UnknownWeaponType".into(),
                    message: format!("Unknown weapon type {wtid}"),
                });
            };

            let Some(src_charge) = snap.items.get(&charge_item_id).cloned() else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Charge item {charge_item_id} not found"),
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
                });
            }
            let Some(charge_type) = self.sde.types.get(&src_charge.type_id) else {
                return Err(BridgeRpcError {
                    code: "UnknownChargeType".into(),
                    message: format!("Unknown charge type {}", src_charge.type_id),
                });
            };
            if !Self::is_charge_compatible_with_weapon(weapon_type, charge_type) {
                return Err(BridgeRpcError {
                    code: "IncompatibleAmmo".into(),
                    message: format!(
                        "Charge {} (type {}) is not compatible with weapon {} (type {wtid})",
                        charge_type.name, src_charge.type_id, weapon_type.name
                    ),
                });
            }
            self.check_type_skill_prerequisites_in_snapshot(snap, char_id, src_charge.type_id)?;

            let max_cap = Self::weapon_slot_max_charge_capacity(weapon_type, charge_type);

            // Check if a charge is already in `(ship_id, flag)`
            let existing_loaded: Option<(i64, u32, i32)> = snap
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
                .map(|i| (i.item_id, i.type_id, i.quantity));

            let mut current_same_id_qty: Option<(i64, i32)> = None;
            if let Some((ex_id, ex_type_id, ex_qty)) = existing_loaded {
                if ex_type_id == src_charge.type_id {
                    current_same_id_qty = Some((ex_id, ex_qty));
                } else {
                    // ChangeAmmo: unload previous charge stack back to `(ship_id, Cargo)`
                    let merge_cargo_id = snap
                        .items
                        .values()
                        .find(|i| {
                            i.item_id != ex_id
                                && i.owner_id == char_id
                                && i.location_id == ship_id
                                && i.flag == eve_db::InventoryFlag::Cargo
                                && i.type_id == ex_type_id
                                && !i.singleton
                        })
                        .map(|i| i.item_id);
                    if let Some(mc_id) = merge_cargo_id {
                        snap.items.remove(&ex_id);
                        if let Some(mc) = snap.items.get_mut(&mc_id) {
                            mc.quantity = mc.quantity.saturating_add(ex_qty);
                            mc.version += 1;
                        }
                    } else if let Some(ex_item) = snap.items.get_mut(&ex_id) {
                        ex_item.location_id = ship_id;
                        ex_item.flag = eve_db::InventoryFlag::Cargo;
                        ex_item.singleton = false;
                        ex_item.version += 1;
                    }
                }
            }

            let cur_qty = current_same_id_qty.map(|(_, q)| q).unwrap_or(0);
            let room = (max_cap - cur_qty).max(0);
            if room == 0 {
                let (ex_id, ex_q) = current_same_id_qty.unwrap();
                let ver = snap.items.get(&ex_id).map(|i| i.version).unwrap_or(1);
                return Ok((ex_id, src_charge.type_id, ex_q, ver));
            }
            let to_load = src_charge.quantity.min(room);

            if let Some((ex_id, _)) = current_same_id_qty {
                if src_charge.quantity > to_load {
                    if let Some(src_mut) = snap.items.get_mut(&charge_item_id) {
                        src_mut.quantity -= to_load;
                        src_mut.version += 1;
                    }
                } else {
                    snap.items.remove(&charge_item_id);
                }
                let ex_mut = snap.items.get_mut(&ex_id).unwrap();
                ex_mut.quantity += to_load;
                ex_mut.version += 1;
                Ok((ex_id, ex_mut.type_id, ex_mut.quantity, ex_mut.version))
            } else if src_charge.quantity > to_load {
                if let Some(src_mut) = snap.items.get_mut(&charge_item_id) {
                    src_mut.quantity -= to_load;
                    src_mut.version += 1;
                }
                snap.next_entity_id += 1;
                let loaded_id = snap.next_entity_id;
                snap.items.insert(
                    loaded_id,
                    eve_db::InventoryItemRecord {
                        item_id: loaded_id,
                        type_id: src_charge.type_id,
                        owner_id: char_id,
                        location_id: ship_id,
                        flag,
                        quantity: to_load,
                        singleton: false,
                        version: 1,
                        dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
                    },
                );
                Ok((loaded_id, src_charge.type_id, to_load, 1))
            } else {
                let src_mut = snap.items.get_mut(&charge_item_id).unwrap();
                src_mut.location_id = ship_id;
                src_mut.flag = flag;
                src_mut.singleton = false;
                src_mut.version += 1;
                Ok((src_mut.item_id, src_mut.type_id, src_mut.quantity, src_mut.version))
            }
        }
    }
}
