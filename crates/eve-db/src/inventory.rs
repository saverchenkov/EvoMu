//! Inventory item queries and atomic transactional item movement.

use crate::persistence::WorldRepository;
use crate::types::*;
use std::collections::{HashMap, HashSet};

const MAX_IDEMPOTENCY_KEYS: usize = 4_096;

impl WorldRepository {
    pub fn get_item(&self, item_id: i64) -> Option<InventoryItemRecord> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        guard.items.get(&item_id).cloned()
    }

    pub fn get_items_for_character(&self, character_id: i64) -> Vec<InventoryItemRecord> {
        let guard = self.inner.read().unwrap_or_else(|e| e.into_inner());
        let mut items: Vec<InventoryItemRecord> = guard
            .items
            .values()
            .filter(|i| i.owner_id == character_id)
            .cloned()
            .collect();
        items.sort_by_key(|i| i.item_id);
        items
    }

    /// Atomically moves an inventory item owned by `character_id` to `(new_location_id, new_flag)`
    /// after verifying item ownership, destination authorization, and optional optimistic concurrency.
    pub fn move_item_transactional(
        &self,
        character_id: i64,
        item_id: i64,
        new_location_id: i64,
        new_flag: InventoryFlag,
        expected_version: Option<u64>,
        idempotency_key: Option<&str>,
    ) -> Result<InventoryItemRecord, DbError> {
        self.try_with_mut_snapshot(|guard| {
            let ch = guard
                .characters
                .get(&character_id)
                .ok_or(DbError::CharacterNotFound(character_id))?
                .clone();

            // Reject self-containment, moving the currently boarded ship, and Locked flag manipulation
            if item_id == new_location_id || item_id == ch.active_ship_id {
                return Err(DbError::InvalidDestinationLocation {
                    character_id,
                    location_id: new_location_id,
                });
            }
            if new_flag == InventoryFlag::Locked {
                return Err(DbError::ItemLocked(item_id));
            }

            let dock_id = ch.station_id.or(ch.structure_id);
            let active_ship_colocated =
                guard.is_ship_colocated_with_character(character_id, ch.active_ship_id, dock_id);

            // Authorize destination: must be the character's current station, structure, co-located active ship,
            // or a live structure's fuel bay in the character's current solar system.
            let valid_structure_fuel_bay = new_flag == InventoryFlag::SpecializedFuelBay
                && guard.structures.get(&new_location_id).is_some_and(|st| {
                    st.solar_system_id == ch.solar_system_id && st.state != "Destroyed"
                });
            let valid_dest = (Some(new_location_id) == ch.station_id)
                || (Some(new_location_id) == ch.structure_id)
                || (active_ship_colocated && new_location_id == ch.active_ship_id)
                || valid_structure_fuel_bay;
            if !valid_dest {
                return Err(DbError::InvalidDestinationLocation {
                    character_id,
                    location_id: new_location_id,
                });
            }

            let existing_item = guard
                .items
                .get(&item_id)
                .ok_or(DbError::ItemNotFound(item_id))?;
            if existing_item.owner_id != character_id {
                return Err(DbError::ItemOwnershipViolation {
                    character_id,
                    item_id,
                });
            }
            if existing_item.flag == InventoryFlag::Locked {
                return Err(DbError::ItemLocked(item_id));
            }

            // Verify source location is reachable by the character (current station, structure, or co-located active ship)
            let valid_source = (Some(existing_item.location_id) == ch.station_id)
                || (Some(existing_item.location_id) == ch.structure_id)
                || (active_ship_colocated && existing_item.location_id == ch.active_ship_id);
            if !valid_source {
                return Err(DbError::InvalidDestinationLocation {
                    character_id,
                    location_id: existing_item.location_id,
                });
            }

            // Reject moving items locked in active contracts or active industry jobs
            if guard.is_item_locked_in_contract_or_job(item_id) {
                return Err(DbError::ItemLocked(item_id));
            }

            if let Some(key) = idempotency_key {
                let scoped_key = (character_id, key.to_string());
                if guard.committed_idempotency_keys.contains(&scoped_key) {
                    return Ok(existing_item.clone());
                }
            }

            if let Some(exp_v) = expected_version {
                if existing_item.version != exp_v {
                    return Err(DbError::VersionConflict {
                        expected: exp_v,
                        actual: existing_item.version,
                    });
                }
            }

            let item = guard.items.get_mut(&item_id).unwrap();
            item.location_id = new_location_id;
            item.flag = new_flag;
            item.version += 1;
            let updated = item.clone();

            if let Some(key) = idempotency_key {
                if guard.committed_idempotency_keys.len() >= MAX_IDEMPOTENCY_KEYS {
                    guard.committed_idempotency_keys.clear();
                }
                guard
                    .committed_idempotency_keys
                    .insert((character_id, key.to_string()));
            }

            Ok(updated)
        })
    }

    /// Repackages an assembled item/ship into its packaged singleton=false state (`INV-1`).
    pub fn repackage_item(
        &self,
        character_id: i64,
        item_id: i64,
    ) -> Result<InventoryItemRecord, DbError> {
        self.try_with_mut_snapshot(|guard| {
            let ch = guard
                .characters
                .get(&character_id)
                .ok_or(DbError::CharacterNotFound(character_id))?;
            if item_id == ch.active_ship_id {
                return Err(DbError::ValidationError(
                    "Cannot repackage your currently active ship".into(),
                ));
            }
            if guard.is_item_locked_in_contract_or_job(item_id) {
                return Err(DbError::ItemLocked(item_id));
            }
            let has_contained = guard
                .items
                .values()
                .any(|other| other.item_id != item_id && other.location_id == item_id && other.quantity > 0);
            if has_contained {
                return Err(DbError::ValidationError(
                    "Cannot repackage item while it contains fitted modules or cargo".into(),
                ));
            }
            let item = guard
                .items
                .get_mut(&item_id)
                .ok_or(DbError::ItemNotFound(item_id))?;
            if item.owner_id != character_id {
                return Err(DbError::ItemOwnershipViolation {
                    character_id,
                    item_id,
                });
            }
            if item.flag != InventoryFlag::Hangar {
                return Err(DbError::ValidationError(
                    "Item must be in station Hangar to repackage".into(),
                ));
            }
            if item.is_blueprint_copy {
                return Err(DbError::ValidationError(
                    "Blueprint copy cannot be repackaged".into(),
                ));
            }
            if item.source_mutaplasmid_id.is_some() || !item.dynamic_attributes.is_empty() {
                return Err(DbError::ValidationError(
                    "Abyssal mutated item cannot be repackaged".into(),
                ));
            }
            item.singleton = false;
            item.insured_payout_isk = 0.0;
            item.version += 1;
            Ok(item.clone())
        })
    }

    /// Merges `source_item_id` into `target_item_id`. Both items must have the same type,
    /// location, and flag, be non-singleton, and owned by `character_id` (`INV-2`).
    pub fn stack_items(
        &self,
        character_id: i64,
        target_item_id: i64,
        source_item_id: i64,
    ) -> Result<InventoryItemRecord, DbError> {
        if target_item_id == source_item_id {
            return Err(DbError::ValidationError(
                "Cannot stack an item onto itself".into(),
            ));
        }
        self.try_with_mut_snapshot(|guard| {
            if guard.is_item_locked_in_contract_or_job(target_item_id) {
                return Err(DbError::ItemLocked(target_item_id));
            }
            if guard.is_item_locked_in_contract_or_job(source_item_id) {
                return Err(DbError::ItemLocked(source_item_id));
            }
            let src = guard
                .items
                .get(&source_item_id)
                .ok_or(DbError::ItemNotFound(source_item_id))?
                .clone();
            if src.owner_id != character_id {
                return Err(DbError::ItemOwnershipViolation {
                    character_id,
                    item_id: source_item_id,
                });
            }
            if src.singleton {
                return Err(DbError::ValidationError(
                    "Cannot stack singleton (assembled) item".into(),
                ));
            }
            let tgt = guard
                .items
                .get_mut(&target_item_id)
                .ok_or(DbError::ItemNotFound(target_item_id))?;
            if tgt.owner_id != character_id {
                return Err(DbError::ItemOwnershipViolation {
                    character_id,
                    item_id: target_item_id,
                });
            }
            if tgt.singleton {
                return Err(DbError::ValidationError(
                    "Cannot stack onto a singleton (assembled) item".into(),
                ));
            }
            if tgt.type_id != src.type_id {
                return Err(DbError::ValidationError(format!(
                    "Cannot stack different item types: {} and {}",
                    tgt.type_id, src.type_id
                )));
            }
            if tgt.location_id != src.location_id || tgt.flag != src.flag {
                return Err(DbError::ValidationError(
                    "Cannot stack items at different locations or flags".into(),
                ));
            }
            tgt.quantity = tgt.quantity.checked_add(src.quantity).ok_or_else(|| {
                DbError::ValidationError("Stack quantity overflow".into())
            })?;
            tgt.version += 1;
            let updated = tgt.clone();

            guard.items.remove(&source_item_id);
            Ok(updated)
        })
    }

    /// Splits `split_quantity` from `item_id`, creating a new inventory item with the split quantity (`INV-2`).
    pub fn split_stack(
        &self,
        character_id: i64,
        item_id: i64,
        split_quantity: i32,
    ) -> Result<(InventoryItemRecord, InventoryItemRecord), DbError> {
        if split_quantity <= 0 {
            return Err(DbError::ValidationError(
                "Split quantity must be positive".into(),
            ));
        }
        self.try_with_mut_snapshot(|guard| {
            if guard.is_item_locked_in_contract_or_job(item_id) {
                return Err(DbError::ItemLocked(item_id));
            }
            let item = guard
                .items
                .get_mut(&item_id)
                .ok_or(DbError::ItemNotFound(item_id))?;
            if item.owner_id != character_id {
                return Err(DbError::ItemOwnershipViolation {
                    character_id,
                    item_id,
                });
            }
            if item.singleton {
                return Err(DbError::ValidationError(
                    "Cannot split a singleton item".into(),
                ));
            }
            if split_quantity >= item.quantity {
                return Err(DbError::ValidationError(format!(
                    "Split quantity {} must be strictly less than item quantity {}",
                    split_quantity, item.quantity
                )));
            }
            item.quantity -= split_quantity;
            item.version += 1;
            let updated_orig = item.clone();

            guard.next_entity_id += 1;
            let new_id = guard.next_entity_id;
            let new_item = InventoryItemRecord {
                item_id: new_id,
                type_id: updated_orig.type_id,
                owner_id: character_id,
                location_id: updated_orig.location_id,
                flag: updated_orig.flag,
                quantity: split_quantity,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            };
            guard.items.insert(new_id, new_item.clone());
            Ok((updated_orig, new_item))
        })
    }

    /// Deletes (trashes) an item and all items contained inside it (`INV-2`).
    pub fn delete_item(&self, character_id: i64, item_id: i64) -> Result<(), DbError> {
        self.try_with_mut_snapshot(|guard| {
            let ch = guard
                .characters
                .get(&character_id)
                .ok_or(DbError::CharacterNotFound(character_id))?;
            if item_id == ch.active_ship_id {
                return Err(DbError::ValidationError(
                    "Cannot delete your currently active ship".into(),
                ));
            }
            if guard.is_item_locked_in_contract_or_job(item_id) {
                return Err(DbError::ItemLocked(item_id));
            }
            let item = guard
                .items
                .get(&item_id)
                .ok_or(DbError::ItemNotFound(item_id))?;
            if item.owner_id != character_id {
                return Err(DbError::ItemOwnershipViolation {
                    character_id,
                    item_id,
                });
            }

            let mut to_remove = HashSet::from([item_id]);
            loop {
                let mut added = false;
                for (&id, it) in &guard.items {
                    if !to_remove.contains(&id) && to_remove.contains(&it.location_id) {
                        to_remove.insert(id);
                        added = true;
                    }
                }
                if !added {
                    break;
                }
            }
            guard.items.retain(|id, _| !to_remove.contains(id));
            Ok(())
        })
    }

    /// Atomically adds an item stack to an owner's location and flag, or increments quantity if an un-singleton stack exists.
    pub fn add_or_stack_item(
        &self,
        owner_id: i64,
        location_id: i64,
        flag: InventoryFlag,
        type_id: u32,
        quantity: i64,
    ) -> Result<InventoryItemRecord, DbError> {
        if quantity <= 0 {
            return Err(DbError::ValidationError("Quantity must be positive".into()));
        }
        self.try_with_mut_snapshot(|guard| {
            // Check if existing non-singleton item stack exists at location/flag
            if let Some(existing) = guard.items.values_mut().find(|it| {
                it.owner_id == owner_id
                    && it.location_id == location_id
                    && it.flag == flag
                    && it.type_id == type_id
                    && !it.singleton
            }) {
                existing.quantity = existing.quantity.saturating_add(quantity as i32);
                existing.version += 1;
                return Ok(existing.clone());
            }

            let item_id = guard.next_entity_id;
            guard.next_entity_id += 1;
            let item = InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity: quantity as i32,
                singleton: false,
                version: 1,
                dynamic_attributes: std::collections::HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            };
            guard.items.insert(item_id, item.clone());
            Ok(item)
        })
    }
}

