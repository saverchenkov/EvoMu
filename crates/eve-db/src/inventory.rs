//! Inventory item queries and atomic transactional item movement.

use crate::persistence::WorldRepository;
use crate::snapshot_ext::WorldStateSnapshot;
use crate::types::*;
use std::collections::{HashMap, HashSet};

const MAX_IDEMPOTENCY_KEYS: usize = 4_096;
const CONTAINER_SHIP_NOTE_ID: i64 = -8_400_000_000_000;
const STRUCTURE_EXT_STATE_KEY: i64 = -7_700_001;
const TYPE_ASSET_SAFETY_WRAP: u32 = 60;
const TYPE_PLASTIC_WRAP: u32 = 3_468;
const FLAG_ASSET_SAFETY: u16 = 36;
const FLAG_LOCKED: u16 = 63;
const FLAG_IMPOUNDED: u16 = 72;

/// Options controlling centralized inventory consumption via [`WorldStateSnapshot::consume_exact_item_quantity`]
/// and [`WorldStateSnapshot::consume_owner_items`].
#[derive(Debug, Clone, Copy)]
pub struct ConsumeExactOptions {
    /// Acting character ID for active-ship, corp-hangar role, and structure ACL checks.
    /// `None` is intended for system/maintenance consumption paths (e.g. structure fuel burn).
    pub actor_char_id: Option<i64>,
    /// Required `item.owner_id` (character ID or corporation ID).
    pub expected_owner_id: i64,
    /// Optional container/station/structure/ship location constraint.
    pub required_location_id: Option<i64>,
    /// Optional flag constraint (on item itself or root container when `allow_nested_in_location` is true).
    pub required_flag: Option<InventoryFlag>,
    /// Allow candidate items nested inside a container located at `(required_location_id, required_flag)`.
    pub allow_nested_in_location: bool,
    /// Reject assembled/packaged singletons (`item.singleton == true`). Default `true` for stackable commodities.
    pub reject_singleton: bool,
    /// Reject items containing nested child items (`is_container_non_empty`).
    pub reject_non_empty_container: bool,
    /// Reject blueprint copies (`item.is_blueprint_copy`).
    pub reject_blueprint_copy: bool,
    /// Reject items fitted to a ship or structure slot (`is_fitted_slot_flag`).
    pub reject_fitted: bool,
    /// Exclude an optional lock scope when verifying `is_item_locked_in_contract_or_job` (e.g., direct trade `-73` / raffle `-74`).
    pub exclude_trade_lock_id: Option<i64>,
    /// Character's currently boarded active ship ID (prevents consuming the active hull).
    pub active_ship_id: Option<i64>,
}

impl ConsumeExactOptions {
    /// Preset for stackable commodity consumption owned by `char_id` (rejects singletons, fitted items, non-empty containers).
    pub fn for_character_stack(char_id: i64) -> Self {
        Self {
            actor_char_id: Some(char_id),
            expected_owner_id: char_id,
            required_location_id: None,
            required_flag: None,
            allow_nested_in_location: false,
            reject_singleton: true,
            reject_non_empty_container: true,
            reject_blueprint_copy: false,
            reject_fitted: true,
            exclude_trade_lock_id: None,
            active_ship_id: None,
        }
    }

    /// Preset for consuming specific singleton-capable items (e.g., skillbooks, implants, crates, BPCs) by exact `item_id`.
    pub fn for_owner_exact(actor_char_id: Option<i64>, expected_owner_id: i64) -> Self {
        Self {
            actor_char_id,
            expected_owner_id,
            required_location_id: None,
            required_flag: None,
            allow_nested_in_location: false,
            reject_singleton: false,
            reject_non_empty_container: true,
            reject_blueprint_copy: false,
            reject_fitted: true,
            exclude_trade_lock_id: None,
            active_ship_id: None,
        }
    }
}

/// Specification for centralized item transfers via [`WorldStateSnapshot::transfer_inventory_item`].
#[derive(Debug, Clone, Copy)]
pub struct TransferItemSpec {
    pub actor_char_id: Option<i64>,
    pub expected_owner_id: Option<i64>,
    pub new_owner_id: Option<i64>,
    pub new_location_id: i64,
    pub new_flag: InventoryFlag,
    /// Quantity to transfer (`None` transfers the whole stack/item; `Some(q)` splits `q` if partial).
    pub quantity: Option<i32>,
    pub reject_singleton: bool,
    pub reject_fitted: bool,
    pub reject_non_empty_container: bool,
    pub merge_at_destination: bool,
    pub exclude_trade_lock_id: Option<i64>,
    pub active_ship_id: Option<i64>,
}

impl TransferItemSpec {
    pub fn move_whole(
        actor_char_id: Option<i64>,
        expected_owner_id: Option<i64>,
        new_owner_id: Option<i64>,
        new_location_id: i64,
        new_flag: InventoryFlag,
    ) -> Self {
        Self {
            actor_char_id,
            expected_owner_id,
            new_owner_id,
            new_location_id,
            new_flag,
            quantity: None,
            reject_singleton: false,
            reject_fitted: true,
            reject_non_empty_container: false,
            merge_at_destination: false,
            exclude_trade_lock_id: None,
            active_ship_id: None,
        }
    }
}

impl WorldRepository {
    pub fn get_item(&self, item_id: i64) -> Option<InventoryItemRecord> {
        let guard = self.read_state();
        guard.items.get(&item_id).cloned()
    }

    pub fn get_items_for_character(&self, character_id: i64) -> Vec<InventoryItemRecord> {
        let guard = self.read_state();
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

            // Reject self-containment, moving the currently boarded/saved ship, and Locked flag manipulation
            if item_id == new_location_id
                || item_id == ch.active_ship_id
                || guard.is_structure_controller_saved_ship(item_id)
            {
                return Err(DbError::InvalidDestinationLocation {
                    character_id,
                    location_id: new_location_id,
                });
            }
            if matches!(new_flag, InventoryFlag::Locked | InventoryFlag::Impounded)
                || matches!(new_flag as u16, 36 | 63 | 72)
            {
                return Err(DbError::ItemLocked(item_id));
            }

            let dock_id = ch.station_id.or(ch.structure_id);
            let active_ship_colocated =
                guard.is_ship_colocated_with_character(character_id, ch.active_ship_id, dock_id);

            // Authorize destination: must be the character's current station, structure, co-located active ship,
            // or a live structure's fuel bay in the character's current solar system.
            let valid_structure_fuel_bay = matches!(new_flag, InventoryFlag::StructureFuel | InventoryFlag::SpecializedFuelBay)
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
                .ok_or(DbError::ItemNotFound(item_id))?
                .clone();
            if existing_item.owner_id != character_id {
                return Err(DbError::ItemOwnershipViolation {
                    character_id,
                    item_id,
                });
            }
            if guard.is_item_or_ancestor_locked_or_impounded(item_id)
                || guard.is_war_hq_structure(item_id)
            {
                return Err(DbError::ItemLocked(item_id));
            }
            let moving_from_fitted_structure_slot =
                guard.is_fitted_structure_slot_flag_at(existing_item.location_id, existing_item.flag);
            let moving_into_structure_rig_or_service =
                guard.structures.contains_key(&new_location_id)
                    && matches!(new_flag as u16, 92..=94 | 164..=171);
            if moving_from_fitted_structure_slot || moving_into_structure_rig_or_service {
                return Err(DbError::ValidationError(
                    "Cannot move fitted structure core, rig, or service module directly".into(),
                ));
            }
            if guard.items.contains_key(&new_location_id)
                && guard.is_item_or_ancestor_locked_or_impounded(new_location_id)
            {
                return Err(DbError::ItemLocked(new_location_id));
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
            if !guard.has_structure_acl_access(existing_item.location_id, character_id) {
                return Err(DbError::InvalidDestinationLocation {
                    character_id,
                    location_id: existing_item.location_id,
                });
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
            if item_id == ch.active_ship_id || guard.is_structure_controller_saved_ship(item_id) {
                return Err(DbError::ValidationError(
                    "Cannot repackage your currently active ship".into(),
                ));
            }
            if guard.is_item_or_ancestor_locked_or_impounded(item_id)
                || guard.is_war_hq_structure(item_id)
            {
                return Err(DbError::ItemLocked(item_id));
            }
            let item_owner_id = guard
                .items
                .get(&item_id)
                .ok_or(DbError::ItemNotFound(item_id))?
                .owner_id;
            if item_owner_id != character_id {
                return Err(DbError::ItemOwnershipViolation {
                    character_id,
                    item_id,
                });
            }
            if guard.is_container_non_empty(item_id) {
                return Err(DbError::ValidationError(
                    "Cannot repackage item while it contains fitted modules or cargo".into(),
                ));
            }
            let item = guard
                .items
                .get_mut(&item_id)
                .ok_or(DbError::ItemNotFound(item_id))?;
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
            if guard.is_item_or_ancestor_locked_or_impounded(target_item_id) {
                return Err(DbError::ItemLocked(target_item_id));
            }
            if guard.is_item_or_ancestor_locked_or_impounded(source_item_id) {
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
            if WorldStateSnapshot::is_fitted_slot_flag(src.flag)
                || guard.is_container_non_empty(source_item_id)
            {
                return Err(DbError::ValidationError(
                    "Cannot stack fitted modules or non-empty containers".into(),
                ));
            }
            if src.singleton
                || src.is_blueprint_copy
                || !src.dynamic_attributes.is_empty()
                || src.source_mutaplasmid_id.is_some()
            {
                return Err(DbError::ValidationError(
                    "Cannot stack singleton, blueprint copy, or mutated item".into(),
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
            if tgt.singleton
                || tgt.is_blueprint_copy
                || !tgt.dynamic_attributes.is_empty()
                || tgt.source_mutaplasmid_id.is_some()
            {
                return Err(DbError::ValidationError(
                    "Cannot stack onto a singleton, blueprint copy, or mutated item".into(),
                ));
            }
            if tgt.type_id != src.type_id {
                return Err(DbError::ValidationError(format!(
                    "Cannot stack different item types: {} and {}",
                    tgt.type_id, src.type_id
                )));
            }
            let is_courier_cargo = |id: i64| -> bool {
                guard
                    .item_custom_names
                    .get(&id)
                    .map(|tag| tag.starts_with("courier_cargo:"))
                    .unwrap_or(false)
            };
            if is_courier_cargo(target_item_id) || is_courier_cargo(source_item_id) {
                return Err(DbError::ValidationError(
                    "Cannot stack courier mission cargo".into(),
                ));
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
            if guard.is_item_or_ancestor_locked_or_impounded(item_id) {
                return Err(DbError::ItemLocked(item_id));
            }
            {
                let item_ref = guard
                    .items
                    .get(&item_id)
                    .ok_or(DbError::ItemNotFound(item_id))?;
                if item_ref.owner_id != character_id {
                    return Err(DbError::ItemOwnershipViolation {
                        character_id,
                        item_id,
                    });
                }
                if WorldStateSnapshot::is_fitted_slot_flag(item_ref.flag) {
                    return Err(DbError::ItemLocked(item_id));
                }
                if guard.is_container_non_empty(item_id) {
                    return Err(DbError::ValidationError(
                        "Cannot split a non-empty container".into(),
                    ));
                }
                if item_ref.singleton
                    || item_ref.is_blueprint_copy
                    || !item_ref.dynamic_attributes.is_empty()
                    || item_ref.source_mutaplasmid_id.is_some()
                {
                    return Err(DbError::ValidationError(
                        "Cannot split a singleton, BPC, or mutated item".into(),
                    ));
                }
                if split_quantity >= item_ref.quantity {
                    return Err(DbError::ValidationError(format!(
                        "Split quantity {} must be strictly less than item quantity {}",
                        split_quantity, item_ref.quantity
                    )));
                }
            }
            let item = guard
                .items
                .get_mut(&item_id)
                .ok_or(DbError::ItemNotFound(item_id))?;
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
            if let Some(custom_name) = guard.item_custom_names.get(&item_id).cloned() {
                guard.item_custom_names.insert(new_id, custom_name);
            }
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
            if item_id == ch.active_ship_id || guard.is_structure_controller_saved_ship(item_id) {
                return Err(DbError::ValidationError(
                    "Cannot delete your currently active ship".into(),
                ));
            }
            if guard.is_item_or_ancestor_locked_or_impounded(item_id)
                || guard.is_war_hq_structure(item_id)
            {
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
            if WorldStateSnapshot::is_fitted_structure_slot_flag(item.flag) {
                return Err(DbError::ValidationError(
                    "Cannot delete fitted structure core, rig, or service module directly".into(),
                ));
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
            for &cid in &to_remove {
                if guard.is_item_or_ancestor_locked_or_impounded(cid)
                    || guard.is_war_hq_structure(cid)
                {
                    return Err(DbError::ItemLocked(cid));
                }
            }
            for cid in to_remove {
                guard.remove_system_cleanup_item(cid);
            }
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
        let qty_i32 = i32::try_from(quantity).map_err(|_| {
            DbError::ValidationError(format!("Quantity {quantity} exceeds i32::MAX"))
        })?;
        self.try_with_mut_snapshot(|guard| {
            // Check if existing non-singleton item stack exists at location/flag (ignoring courier cargo stacks)
            let existing_id = guard.items.values().find(|it| {
                it.owner_id == owner_id
                    && it.location_id == location_id
                    && it.flag == flag
                    && it.type_id == type_id
                    && !it.singleton
                    && !guard.item_custom_names.get(&it.item_id).map_or(false, |tag| tag.starts_with("courier_cargo:"))
            }).map(|it| it.item_id);
            if let Some(existing_id) = existing_id {
                let updated = {
                    let existing = guard.items.get_mut(&existing_id).unwrap();
                    existing.quantity = existing.quantity.checked_add(qty_i32).ok_or_else(|| {
                        DbError::ValidationError("Stack quantity overflow".into())
                    })?;
                    existing.version += 1;
                    existing.clone()
                };
                guard.mark_item_dirty(existing_id);
                return Ok(updated);
            }

            let mut item_id = guard.alloc_entity_id();
            while guard.items.contains_key(&item_id) {
                item_id = guard.alloc_entity_id();
            }
            debug_assert!(!guard.items.contains_key(&item_id));
            let item = InventoryItemRecord {
                item_id,
                type_id,
                owner_id,
                location_id,
                flag,
                quantity: qty_i32,
                singleton: false,
                version: 1,
                dynamic_attributes: std::collections::HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            };
            guard.items.insert(item_id, item.clone());
            guard.mark_item_dirty(item_id);
            Ok(item)
        })
    }
}

impl WorldStateSnapshot {
    /// Returns `true` if `flag` belongs to a fitted Upwell structure slot (`QuantumCoreRoom` 180,
    /// `RigSlot0..=2` 92..=94, or `ServiceSlot0..=7` 164..=171).
    pub fn is_fitted_structure_slot_flag(flag: InventoryFlag) -> bool {
        matches!(flag as u16, 92..=94 | 164..=171 | 180)
    }

    /// Returns `true` if `flag` at `location_id` represents an installed Upwell structure slot
    /// (`QuantumCoreRoom` 180, `ServiceSlot0..=7` 164..=171, or `RigSlot0..=2` 92..=94 on a structure).
    pub fn is_fitted_structure_slot_flag_at(&self, location_id: i64, flag: InventoryFlag) -> bool {
        matches!(flag as u16, 164..=171 | 180)
            || (self.structures.contains_key(&location_id) && matches!(flag as u16, 92..=94))
    }

    /// Returns `true` if `flag` represents a fitted ship, pilot, implant/booster, or Upwell
    /// structure slot (excluding `DroneBay`, `FighterBay`, `Cargo`, and `Hangar` storage flags).
    pub fn is_fitted_slot_flag(flag: InventoryFlag) -> bool {
        matches!(
            flag as u16,
            7 | 11..=34 | 56 | 57 | 88 | 89 | 92..=94 | 125..=129 | 159..=163 | 164..=171 | 180
        )
    }

    /// Returns `true` if `item_id` contains at least one child item with positive quantity (`Inv 5`).
    pub fn is_container_non_empty(&self, item_id: i64) -> bool {
        self.items
            .values()
            .any(|other| other.item_id != item_id && other.location_id == item_id && other.quantity > 0)
    }

    /// Returns `true` if `item_id` is locked via ALSC container security (`owner_notes[-8_400_000_000_000]`).
    pub fn is_item_in_alsc_lock(&self, item_id: i64) -> bool {
        let Some(note) = self.owner_notes.get(&CONTAINER_SHIP_NOTE_ID) else {
            return false;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&note.note) else {
            return false;
        };
        val.get("alsc_locked_items")
            .and_then(|v| v.as_array())
            .is_some_and(|arr| arr.iter().any(|entry| entry.as_i64() == Some(item_id)))
    }

    /// Returns `true` if `item_id` is an Asset Safety Wrap (`type_id = 60`), carries flag `AssetSafety`
    /// (`36`), or sits inside an unpaid `asset_safety_wraps` record (`Inv 2`).
    pub fn is_item_in_active_asset_safety_wrap(&self, item_id: i64) -> bool {
        if let Some(it) = self.items.get(&item_id) {
            if it.type_id == TYPE_ASSET_SAFETY_WRAP || (it.flag as u16) == FLAG_ASSET_SAFETY {
                return true;
            }
        }
        let Some(ext_json) = self.item_custom_names.get(&STRUCTURE_EXT_STATE_KEY) else {
            return false;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(ext_json) else {
            return false;
        };
        let Some(wraps) = val.get("asset_safety_wraps").and_then(|v| v.as_object()) else {
            return false;
        };

        // Collect ancestor item IDs once (self + up to 8 parents) to test against wrap_id / item_ids.
        let mut chain = Vec::with_capacity(9);
        chain.push(item_id);
        let mut cur = self.items.get(&item_id).map(|i| i.location_id);
        for _ in 0..8 {
            let Some(loc) = cur else {
                break;
            };
            chain.push(loc);
            cur = self.items.get(&loc).map(|i| i.location_id);
        }

        wraps.iter().any(|(key_str, wrap_val)| {
            if wrap_val.get("retrieval_fee_paid").and_then(|v| v.as_bool()) == Some(true) {
                return false;
            }
            let wrap_id = wrap_val
                .get("wrap_id")
                .and_then(|v| v.as_i64())
                .or_else(|| key_str.parse::<i64>().ok());
            if let Some(wid) = wrap_id {
                if chain.contains(&wid) {
                    return true;
                }
            }
            wrap_val
                .get("item_ids")
                .and_then(|v| v.as_array())
                .is_some_and(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_i64())
                        .any(|iid| chain.contains(&iid))
                })
        })
    }

    /// Returns `true` if `item_id` is currently designated as the War HQ of an active war (`Inv 3`).
    pub fn is_war_hq_structure(&self, item_id: i64) -> bool {
        if self.war_state.wars.iter().any(|(&war_id, meta)| {
            meta.war_hq_id == Some(item_id)
                && meta.finish_at_unix.is_none()
                && self.wars.get(&war_id).is_none_or(|w| w.active)
        }) {
            return true;
        }
        let Some(ext_json) = self.item_custom_names.get(&STRUCTURE_EXT_STATE_KEY) else {
            return false;
        };
        serde_json::from_str::<serde_json::Value>(ext_json)
            .ok()
            .and_then(|v| {
                v.get("war_hq_structures")
                    .and_then(|arr| arr.as_array())
                    .map(|arr| arr.iter().any(|x| x.as_i64() == Some(item_id)))
            })
            .unwrap_or(false)
    }

    fn is_escrow_locked_opt(&self, item_id: i64, exclude_trade_lock_id: Option<i64>) -> bool {
        match exclude_trade_lock_id {
            Some(tid) => self.is_item_locked_excluding_trade(item_id, Some(tid)),
            None => self.is_item_locked_in_contract_or_job(item_id),
        }
    }

    fn is_item_or_ancestor_locked_or_impounded_opt(
        &self,
        item_id: i64,
        exclude_trade_lock_id: Option<i64>,
    ) -> bool {
        if self.is_escrow_locked_opt(item_id, exclude_trade_lock_id)
            || self.is_item_in_alsc_lock(item_id)
            || self.is_item_in_active_asset_safety_wrap(item_id)
        {
            return true;
        }
        let Some(item) = self.items.get(&item_id) else {
            return false;
        };
        if matches!(item.flag, InventoryFlag::Locked | InventoryFlag::Impounded)
            || matches!(item.flag as u16, FLAG_ASSET_SAFETY | FLAG_LOCKED | FLAG_IMPOUNDED)
            || item.type_id == TYPE_PLASTIC_WRAP
            || item.type_id == TYPE_ASSET_SAFETY_WRAP
        {
            return true;
        }
        let mut cur_loc = item.location_id;
        for _ in 0..8 {
            let Some(anc) = self.items.get(&cur_loc) else {
                break;
            };
            if anc.type_id == TYPE_PLASTIC_WRAP
                || anc.type_id == TYPE_ASSET_SAFETY_WRAP
                || self.is_escrow_locked_opt(anc.item_id, exclude_trade_lock_id)
                || self.is_item_in_alsc_lock(anc.item_id)
                || self.is_item_in_active_asset_safety_wrap(anc.item_id)
                || matches!(anc.flag, InventoryFlag::Locked | InventoryFlag::Impounded)
                || matches!(anc.flag as u16, FLAG_ASSET_SAFETY | FLAG_LOCKED | FLAG_IMPOUNDED)
            {
                return true;
            }
            cur_loc = anc.location_id;
        }
        false
    }

    /// Returns `true` if `item_id` or any ancestor container up its containment chain is locked,
    /// impounded (`72`), under ALSC lock, in Asset Safety (`36` / type `60` / unpaid wrap),
    /// sealed inside a courier plastic wrap (`3468`), or escrowed in an active contract, industry job,
    /// direct trade, or HyperNet raffle (`Inv 1`, `Inv 2`, `Inv 3`).
    pub fn is_item_or_ancestor_locked_or_impounded(&self, item_id: i64) -> bool {
        self.is_item_or_ancestor_locked_or_impounded_opt(item_id, None)
    }

    /// Returns `true` if `char_id` is NOT denied by `structure_docking_acl_denied` or the structure's
    /// `access_profiles` (`Inv 6`). Always returns `true` for non-structure IDs unless explicitly denied.
    pub fn has_structure_acl_access(&self, structure_id: i64, char_id: i64) -> bool {
        if self
            .structure_docking_acl_denied
            .get(&structure_id)
            .is_some_and(|s| s.contains(&char_id))
        {
            return false;
        }
        let Some(ext_json) = self.item_custom_names.get(&STRUCTURE_EXT_STATE_KEY) else {
            return true;
        };
        let Ok(ext) = serde_json::from_str::<serde_json::Value>(ext_json) else {
            return true;
        };
        let struct_key = structure_id.to_string();
        let Some(prof) = ext
            .get("access_profiles")
            .and_then(|v| v.as_object())
            .and_then(|m| m.get(&struct_key))
        else {
            return true;
        };

        let (char_corp, char_ally) = self
            .characters
            .get(&char_id)
            .map(|c| (c.corporation_id, c.alliance_id.unwrap_or(0)))
            .unwrap_or((0, 0));
        let owner_corp = self
            .structures
            .get(&structure_id)
            .map(|s| s.owner_corp_id)
            .unwrap_or(0);

        let arr_contains = |obj: &serde_json::Value, field: &str, id: i64| -> bool {
            obj.get(field)
                .and_then(|v| v.as_array())
                .is_some_and(|a| a.iter().any(|x| x.as_i64() == Some(id)))
        };
        let arr_non_empty = |obj: &serde_json::Value, field: &str| -> bool {
            obj.get(field)
                .and_then(|v| v.as_array())
                .is_some_and(|a| !a.is_empty())
        };

        if arr_contains(prof, "blocked_characters", char_id)
            || arr_contains(prof, "blocked_corporations", char_corp)
            || (char_ally > 0 && arr_contains(prof, "blocked_alliances", char_ally))
        {
            return false;
        }

        let owner_groups = ext.get("owner_groups").and_then(|v| v.as_object());
        let group_ids: Vec<String> = prof
            .get("access_group_ids")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_i64().map(|id| id.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let is_allowed_role = |r: i64| matches!(r, 0 | 1 | 2);
        let is_excluded_role = |r: i64| r == 3 || r == -1;
        let mut has_group_allows = false;
        let mut in_allowed_group = false;

        if let Some(groups_map) = owner_groups {
            for gid in &group_ids {
                if let Some(members) = groups_map
                    .get(gid)
                    .and_then(|g| g.get("members"))
                    .and_then(|m| m.as_object())
                {
                    let member_role = |id: i64| -> Option<i64> {
                        if id == 0 {
                            return None;
                        }
                        members.get(&id.to_string()).and_then(|v| v.as_i64())
                    };
                    if member_role(char_id).is_some_and(is_excluded_role)
                        || member_role(char_corp).is_some_and(is_excluded_role)
                        || (char_ally > 0 && member_role(char_ally).is_some_and(is_excluded_role))
                    {
                        return false;
                    }
                    if members.values().filter_map(|v| v.as_i64()).any(is_allowed_role) {
                        has_group_allows = true;
                    }
                    if member_role(char_id).is_some_and(is_allowed_role)
                        || member_role(char_corp).is_some_and(is_allowed_role)
                        || (char_ally > 0 && member_role(char_ally).is_some_and(is_allowed_role))
                    {
                        in_allowed_group = true;
                    }
                }
            }
        }

        if prof.get("corp_only").and_then(|v| v.as_bool()) == Some(true)
            && owner_corp != 0
            && char_corp != owner_corp
        {
            return false;
        }

        let has_allow = arr_non_empty(prof, "allowed_characters")
            || arr_non_empty(prof, "allowed_corporations")
            || arr_non_empty(prof, "allowed_alliances")
            || has_group_allows;
        if has_allow
            && char_corp != owner_corp
            && !arr_contains(prof, "allowed_characters", char_id)
            && !arr_contains(prof, "allowed_corporations", char_corp)
            && !(char_ally > 0 && arr_contains(prof, "allowed_alliances", char_ally))
            && !in_allowed_group
        {
            return false;
        }

        true
    }

    /// Computes effective matured corporation roles `(is_ceo, role_mask)` for `char_id` in `corp_id`
    /// at `location_id`. Pending 24h grants in `corp_governance.pending_*` are intentionally excluded
    /// until promoted (`Inv 6`).
    pub fn effective_corp_roles_at_location(
        &self,
        corp_id: i64,
        char_id: i64,
        location_id: Option<i64>,
    ) -> (bool, u64) {
        let char_home = self
            .characters
            .get(&char_id)
            .map(|c| c.home_station_id)
            .unwrap_or(0);
        let (is_ceo, hq_station_id, mut role_mask) = self
            .corporations
            .get(&corp_id)
            .map(|c| {
                (
                    c.ceo_id == char_id,
                    c.hq_station_id.unwrap_or(char_home),
                    c.member_roles.get(&char_id).copied().unwrap_or(0),
                )
            })
            .unwrap_or((false, char_home, 0));

        let raw_loc = location_id
            .or_else(|| {
                self.characters
                    .get(&char_id)
                    .and_then(|ch| ch.station_id.or(ch.structure_id))
            })
            .unwrap_or(0);
        let loc_station_id = self
            .items
            .get(&raw_loc)
            .and_then(|it| {
                (it.location_id >= 60_000_000 || self.structures.contains_key(&it.location_id))
                    .then_some(it.location_id)
            })
            .unwrap_or(raw_loc);
        let base_id = self
            .corp_governance
            .get(&corp_id)
            .and_then(|g| g.member_bases.get(&char_id).copied())
            .unwrap_or(0);

        if let Some(ra) = self.corp_role_assignments.get(&(corp_id, char_id)) {
            role_mask |= ra.roles;
            if loc_station_id > 0 {
                if loc_station_id == hq_station_id {
                    role_mask |= ra.roles_at_hq;
                }
                if base_id > 0 && loc_station_id == base_id {
                    role_mask |= ra.roles_at_base;
                }
                if loc_station_id != hq_station_id && (base_id <= 0 || loc_station_id != base_id) {
                    role_mask |= ra.roles_at_other;
                }
            }
        }

        (is_ceo, role_mask)
    }

    fn resolve_corp_root_location(&self, corp_id: i64, item_id: i64) -> (i64, InventoryFlag, bool) {
        let Some(item) = self.items.get(&item_id) else {
            return (0, InventoryFlag::None, false);
        };
        let (mut root_loc, mut root_flag) = (item.location_id, item.flag);
        let mut inside_container = false;
        for _ in 0..8 {
            if (60_000_000..=69_999_999).contains(&root_loc) || self.structures.contains_key(&root_loc)
            {
                break;
            }
            let Some(parent) = self.items.get(&root_loc) else {
                break;
            };
            if parent.owner_id != corp_id {
                break;
            }
            inside_container = true;
            root_loc = parent.location_id;
            root_flag = parent.flag;
        }
        (root_loc, root_flag, inside_container)
    }

    /// Validates that `char_id` holds matured `ROLE_HANGAR_CAN_TAKE` (and `ROLE_CONTAINER_CAN_TAKE` when
    /// nested inside a corporation container) or CEO/Director privileges to take `item_id` owned by
    /// `corp_id`, and is not blocked by structure ACL (`Inv 6`).
    pub fn validate_corp_take_access(
        &self,
        char_id: i64,
        corp_id: i64,
        item_id: i64,
    ) -> Result<(), DbError> {
        let (root_loc, root_flag, inside_container) = self.resolve_corp_root_location(corp_id, item_id);
        if root_loc > 0 && !self.has_structure_acl_access(root_loc, char_id) {
            return Err(DbError::ValidationError(format!(
                "Character {char_id} lacks structure ACL access at {root_loc}"
            )));
        }
        let (is_ceo, role_mask) =
            self.effective_corp_roles_at_location(corp_id, char_id, Some(root_loc));
        if is_ceo || corp_roles::has_role(role_mask, corp_roles::ROLE_DIRECTOR) {
            return Ok(());
        }
        let flag_num = root_flag as i64;
        if (116..=122).contains(&flag_num) {
            let div_idx = (flag_num - 116) as usize;
            if !corp_roles::has_role(role_mask, corp_roles::ROLE_HANGAR_CAN_TAKE[div_idx]) {
                return Err(DbError::ValidationError(format!(
                    "Character {char_id} lacks corporation hangar division {} take role",
                    div_idx + 1
                )));
            }
            if inside_container
                && !corp_roles::has_role(role_mask, corp_roles::ROLE_CONTAINER_CAN_TAKE[div_idx])
            {
                return Err(DbError::ValidationError(format!(
                    "Character {char_id} lacks corporation hangar division {} container take role",
                    div_idx + 1
                )));
            }
            return Ok(());
        }
        if matches!(
            root_flag,
            InventoryFlag::Deliveries | InventoryFlag::CorpDeliveries
        ) {
            let deliveries_roles = corp_roles::ROLE_DELIVERIES_HANGAR_TAKE
                | corp_roles::ROLE_FACTORY_MANAGER
                | corp_roles::ROLE_TRADER;
            if (role_mask & deliveries_roles) == 0 {
                return Err(DbError::ValidationError(format!(
                    "Character {char_id} lacks role to take from corporation deliveries"
                )));
            }
            if inside_container
                && !corp_roles::has_role(role_mask, corp_roles::ROLE_DELIVERIES_HANGAR_CONTAINER_TAKE)
            {
                return Err(DbError::ValidationError(format!(
                    "Character {char_id} lacks corporation deliveries container take role"
                )));
            }
            return Ok(());
        }
        Err(DbError::ValidationError(format!(
            "Director or CEO role required to take corporation item {item_id} from flag {:?}",
            root_flag
        )))
    }

    fn resolve_root_location_id(&self, item_id: i64) -> i64 {
        let Some(item) = self.items.get(&item_id) else {
            return 0;
        };
        let mut cur_loc = item.location_id;
        for _ in 0..8 {
            if self.structures.contains_key(&cur_loc) || (60_000_000..=69_999_999).contains(&cur_loc) {
                break;
            }
            let Some(parent) = self.items.get(&cur_loc) else {
                break;
            };
            cur_loc = parent.location_id;
        }
        cur_loc
    }

    fn matches_location_constraint(&self, item: &InventoryItemRecord, opts: &ConsumeExactOptions) -> bool {
        if let Some(req_loc) = opts.required_location_id {
            if item.location_id == req_loc {
                if let Some(req_flag) = opts.required_flag {
                    return item.flag == req_flag;
                }
                return true;
            }
            if !opts.allow_nested_in_location {
                return false;
            }
            let mut cur_loc = item.location_id;
            for _ in 0..8 {
                let Some(anc) = self.items.get(&cur_loc) else {
                    return false;
                };
                if anc.location_id == req_loc {
                    if let Some(req_flag) = opts.required_flag {
                        return anc.flag == req_flag;
                    }
                    return true;
                }
                cur_loc = anc.location_id;
            }
            return false;
        }
        if let Some(req_flag) = opts.required_flag {
            return item.flag == req_flag;
        }
        true
    }

    fn validate_item_for_consumption(
        &self,
        item: &InventoryItemRecord,
        opts: &ConsumeExactOptions,
    ) -> Result<(), DbError> {
        if item.owner_id != opts.expected_owner_id {
            return Err(DbError::ItemOwnershipViolation {
                character_id: opts.expected_owner_id,
                item_id: item.item_id,
            });
        }
        if item.quantity <= 0 {
            return Err(DbError::ValidationError(format!(
                "Item {} has non-positive quantity {}",
                item.item_id, item.quantity
            )));
        }
        if !self.matches_location_constraint(item, opts) {
            return Err(DbError::ValidationError(format!(
                "Item {} is not in the required location or flag",
                item.item_id
            )));
        }
        if self.is_item_or_ancestor_locked_or_impounded_opt(item.item_id, opts.exclude_trade_lock_id)
            || self.is_war_hq_structure(item.item_id)
        {
            return Err(DbError::ItemLocked(item.item_id));
        }
        if self.is_structure_controller_saved_ship(item.item_id) {
            return Err(DbError::ValidationError(format!(
                "Item {} is currently a structure controller ship",
                item.item_id
            )));
        }
        if let Some(actor_id) = opts.actor_char_id {
            if self.is_character_boarded_or_saved_ship(actor_id, opts.active_ship_id, item.item_id) {
                return Err(DbError::ValidationError(format!(
                    "Cannot consume currently boarded ship {}",
                    item.item_id
                )));
            }
            if item.owner_id != actor_id {
                if self.corporations.contains_key(&item.owner_id) {
                    self.validate_corp_take_access(actor_id, item.owner_id, item.item_id)?;
                } else {
                    return Err(DbError::ItemOwnershipViolation {
                        character_id: actor_id,
                        item_id: item.item_id,
                    });
                }
            }
            let root_loc = self.resolve_root_location_id(item.item_id);
            if root_loc > 0 && !self.has_structure_acl_access(root_loc, actor_id) {
                return Err(DbError::ValidationError(format!(
                    "Character {actor_id} lacks structure ACL access at {root_loc}"
                )));
            }
        }
        if opts.reject_fitted && Self::is_fitted_slot_flag(item.flag) {
            return Err(DbError::ValidationError(format!(
                "Cannot consume fitted item {} in slot {:?}",
                item.item_id, item.flag
            )));
        }
        if opts.reject_singleton && item.singleton {
            return Err(DbError::ValidationError(format!(
                "Cannot consume assembled singleton item {}",
                item.item_id
            )));
        }
        if opts.reject_blueprint_copy && item.is_blueprint_copy {
            return Err(DbError::ValidationError(format!(
                "Cannot consume blueprint copy item {}",
                item.item_id
            )));
        }
        if opts.reject_non_empty_container && self.is_container_non_empty(item.item_id) {
            return Err(DbError::ValidationError(format!(
                "Cannot consume non-empty container {}",
                item.item_id
            )));
        }
        Ok(())
    }

    /// System cleanup helper that removes an item and cleans associated per-item metadata
    /// (`item_custom_names`, `item_positions`, module states, skins) without player-facing transfer
    /// validation. Reserve for ship destruction, combat despawns, expiry, repackage, and admin cleanup.
    pub fn remove_system_cleanup_item(&mut self, item_id: i64) -> Option<InventoryItemRecord> {
        let removed = self.items.remove(&item_id);
        if item_id > 0 {
            self.item_custom_names.remove(&item_id);
            self.item_positions.remove(&item_id);
            self.module_online_states.remove(&item_id);
            self.module_active_states.remove(&item_id);
            self.module_active_charges.remove(&item_id);
            self.applied_ship_skins.remove(&item_id);
        }
        removed
    }

    /// Atomically consumes `quantity` units from a specific `item_id` after verifying all 6 inventory
    /// invariants configured in `opts`. Returns `Ok(None)` if the entire item stack was removed or
    /// `Ok(Some(updated))` if partial quantity remains.
    pub fn consume_exact_item_quantity(
        &mut self,
        item_id: i64,
        quantity: i32,
        opts: ConsumeExactOptions,
    ) -> Result<Option<InventoryItemRecord>, DbError> {
        if quantity <= 0 {
            return Err(DbError::ValidationError(
                "Consumption quantity must be positive".into(),
            ));
        }
        let item_snapshot = self
            .items
            .get(&item_id)
            .ok_or(DbError::ItemNotFound(item_id))?
            .clone();
        self.validate_item_for_consumption(&item_snapshot, &opts)?;
        if item_snapshot.quantity < quantity {
            return Err(DbError::ValidationError(format!(
                "Insufficient quantity on item {}: need {}, have {}",
                item_id, quantity, item_snapshot.quantity
            )));
        }
        if item_snapshot.quantity == quantity {
            self.remove_system_cleanup_item(item_id);
            Ok(None)
        } else {
            let entry = self
                .items
                .get_mut(&item_id)
                .ok_or(DbError::ItemNotFound(item_id))?;
            entry.quantity -= quantity;
            entry.version += 1;
            Ok(Some(entry.clone()))
        }
    }

    /// Atomically consumes `quantity` units of `type_id` across eligible stacks matching `opts`,
    /// skipping ineligible stacks (locked, impounded, singleton, fitted, war-HQ, ACL-denied, or
    /// missing corp hangar roles) and rolling back nothing because all validation completes before
    /// any mutation. Returns the consumed/decremented `item_id`s in deterministic order.
    pub fn consume_owner_items(
        &mut self,
        type_id: u32,
        quantity: i32,
        opts: ConsumeExactOptions,
    ) -> Result<Vec<i64>, DbError> {
        if quantity <= 0 {
            return Err(DbError::ValidationError(
                "Consumption quantity must be positive".into(),
            ));
        }
        let mut candidate_ids: Vec<i64> = self
            .items
            .values()
            .filter(|it| it.owner_id == opts.expected_owner_id && it.type_id == type_id)
            .map(|it| it.item_id)
            .collect();
        candidate_ids.sort_unstable();

        let mut eligible: Vec<(i64, i32)> = Vec::new();
        let mut total_available: i64 = 0;
        let mut first_err: Option<DbError> = None;

        for iid in candidate_ids {
            let Some(item_ref) = self.items.get(&iid) else {
                continue;
            };
            match self.validate_item_for_consumption(item_ref, &opts) {
                Ok(()) => {
                    total_available = total_available.saturating_add(i64::from(item_ref.quantity));
                    eligible.push((iid, item_ref.quantity));
                }
                Err(e) => {
                    if first_err.is_none() {
                        first_err = Some(e);
                    }
                }
            }
        }

        if total_available < i64::from(quantity) {
            return Err(first_err.unwrap_or_else(|| {
                DbError::ValidationError(format!(
                    "Insufficient eligible items of type {type_id}: need {quantity}, have {total_available}"
                ))
            }));
        }

        let mut remaining = quantity;
        let mut touched_ids = Vec::new();
        for (iid, avail) in eligible {
            if remaining <= 0 {
                break;
            }
            let take = remaining.min(avail);
            if take == avail {
                self.remove_system_cleanup_item(iid);
            } else if let Some(entry) = self.items.get_mut(&iid) {
                entry.quantity -= take;
                entry.version += 1;
            }
            touched_ids.push(iid);
            remaining -= take;
        }
        Ok(touched_ids)
    }

    /// Atomically transfers (or splits and transfers) `item_id` according to `spec`, enforcing all 6
    /// inventory invariants before mutating state. Returns the resulting destination `InventoryItemRecord`.
    pub fn transfer_inventory_item(
        &mut self,
        item_id: i64,
        spec: TransferItemSpec,
    ) -> Result<InventoryItemRecord, DbError> {
        let item = self
            .items
            .get(&item_id)
            .ok_or(DbError::ItemNotFound(item_id))?
            .clone();

        if spec.new_location_id == item_id {
            return Err(DbError::InvalidDestinationLocation {
                character_id: spec.actor_char_id.unwrap_or(item.owner_id),
                location_id: spec.new_location_id,
            });
        }
        if spec.new_flag == InventoryFlag::Locked {
            return Err(DbError::ItemLocked(item_id));
        }
        if let Some(exp_owner) = spec.expected_owner_id {
            if item.owner_id != exp_owner {
                return Err(DbError::ItemOwnershipViolation {
                    character_id: exp_owner,
                    item_id,
                });
            }
        }
        if self.is_item_or_ancestor_locked_or_impounded_opt(item_id, spec.exclude_trade_lock_id)
            || self.is_war_hq_structure(item_id)
        {
            return Err(DbError::ItemLocked(item_id));
        }
        if self.items.contains_key(&spec.new_location_id)
            && self.is_item_or_ancestor_locked_or_impounded_opt(
                spec.new_location_id,
                spec.exclude_trade_lock_id,
            )
        {
            return Err(DbError::ItemLocked(spec.new_location_id));
        }
        if self.is_structure_controller_saved_ship(item_id) {
            return Err(DbError::ValidationError(format!(
                "Cannot transfer structure controller ship {item_id}"
            )));
        }
        if spec.reject_fitted
            && (self.is_fitted_structure_slot_flag_at(item.location_id, item.flag)
                || (self.structures.contains_key(&spec.new_location_id)
                    && matches!(spec.new_flag as u16, 92..=94 | 164..=171)))
        {
            return Err(DbError::ValidationError(
                "Cannot transfer fitted structure core, rig, or service module".into(),
            ));
        }
        if spec.reject_fitted && Self::is_fitted_slot_flag(item.flag) {
            return Err(DbError::ValidationError(format!(
                "Cannot transfer fitted item {item_id} from slot {:?}",
                item.flag
            )));
        }
        if spec.reject_non_empty_container && self.is_container_non_empty(item_id) {
            return Err(DbError::ValidationError(format!(
                "Cannot transfer non-empty container {item_id}"
            )));
        }

        if let Some(actor_id) = spec.actor_char_id {
            if self.is_character_boarded_or_saved_ship(actor_id, spec.active_ship_id, item_id) {
                return Err(DbError::ValidationError(format!(
                    "Cannot transfer currently boarded ship {item_id}"
                )));
            }
            if item.owner_id != actor_id {
                if self.corporations.contains_key(&item.owner_id) {
                    self.validate_corp_take_access(actor_id, item.owner_id, item_id)?;
                } else if spec.expected_owner_id.is_none() {
                    return Err(DbError::ItemOwnershipViolation {
                        character_id: actor_id,
                        item_id,
                    });
                }
            }
            let src_root = self.resolve_root_location_id(item_id);
            if src_root > 0 && !self.has_structure_acl_access(src_root, actor_id) {
                return Err(DbError::ValidationError(format!(
                    "Character {actor_id} lacks structure ACL access at {src_root}"
                )));
            }
            if (self.structures.contains_key(&spec.new_location_id)
                || self.structure_docking_acl_denied.contains_key(&spec.new_location_id))
                && !self.has_structure_acl_access(spec.new_location_id, actor_id)
            {
                return Err(DbError::InvalidDestinationLocation {
                    character_id: actor_id,
                    location_id: spec.new_location_id,
                });
            }
        }

        let target_qty = spec.quantity.unwrap_or(item.quantity);
        if target_qty <= 0 || target_qty > item.quantity {
            return Err(DbError::ValidationError(format!(
                "Invalid transfer quantity {target_qty} for item {item_id} (have {})",
                item.quantity
            )));
        }
        let is_partial = target_qty < item.quantity;
        if (spec.reject_singleton || is_partial)
            && (item.singleton
                || item.is_blueprint_copy
                || !item.dynamic_attributes.is_empty()
                || item.source_mutaplasmid_id.is_some())
        {
            return Err(DbError::ValidationError(format!(
                "Cannot split or stack-transfer singleton, BPC, or mutated item {item_id}"
            )));
        }
        if is_partial && self.is_container_non_empty(item_id) {
            return Err(DbError::ValidationError(format!(
                "Cannot partially transfer non-empty container {item_id}"
            )));
        }

        let dest_owner = spec.new_owner_id.unwrap_or(item.owner_id);
        let is_courier_cargo = self
            .item_custom_names
            .get(&item_id)
            .is_some_and(|tag| tag.starts_with("courier_cargo:"));

        if spec.merge_at_destination
            && !item.singleton
            && !item.is_blueprint_copy
            && item.dynamic_attributes.is_empty()
            && item.source_mutaplasmid_id.is_none()
            && !is_courier_cargo
        {
            let merge_target_id = self
                .items
                .values()
                .find(|cand| {
                    cand.item_id != item_id
                        && cand.owner_id == dest_owner
                        && cand.location_id == spec.new_location_id
                        && cand.flag == spec.new_flag
                        && cand.type_id == item.type_id
                        && !cand.singleton
                        && !cand.is_blueprint_copy
                        && cand.dynamic_attributes.is_empty()
                        && cand.source_mutaplasmid_id.is_none()
                        && !self
                            .item_custom_names
                            .get(&cand.item_id)
                            .is_some_and(|tag| tag.starts_with("courier_cargo:"))
                })
                .map(|c| c.item_id);

            if let Some(target_id) = merge_target_id {
                if is_partial {
                    if let Some(src_mut) = self.items.get_mut(&item_id) {
                        src_mut.quantity -= target_qty;
                        src_mut.version += 1;
                    }
                } else {
                    self.remove_system_cleanup_item(item_id);
                }
                let tgt = self
                    .items
                    .get_mut(&target_id)
                    .ok_or(DbError::ItemNotFound(target_id))?;
                tgt.quantity = tgt
                    .quantity
                    .checked_add(target_qty)
                    .ok_or_else(|| DbError::ValidationError("Destination stack quantity overflow".into()))?;
                tgt.version += 1;
                return Ok(tgt.clone());
            }
        }

        if is_partial {
            if let Some(src_mut) = self.items.get_mut(&item_id) {
                src_mut.quantity -= target_qty;
                src_mut.version += 1;
            }
            self.next_entity_id += 1;
            let new_id = self.next_entity_id;
            let mut new_item = InventoryItemRecord {
                item_id: new_id,
                type_id: item.type_id,
                owner_id: dest_owner,
                location_id: spec.new_location_id,
                flag: spec.new_flag,
                quantity: target_qty,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            };
            new_item.version = 1;
            self.items.insert(new_id, new_item.clone());
            Ok(new_item)
        } else {
            let entry = self
                .items
                .get_mut(&item_id)
                .ok_or(DbError::ItemNotFound(item_id))?;
            entry.owner_id = dest_owner;
            entry.location_id = spec.new_location_id;
            entry.flag = spec.new_flag;
            entry.version += 1;
            Ok(entry.clone())
        }
    }
}
#[cfg(test)] #[path = "inventory_unit_tests.rs"] mod inventory_unit_tests;


