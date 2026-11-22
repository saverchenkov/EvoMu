//! Inventory item queries and atomic transactional item movement.

use crate::persistence::WorldRepository;
use crate::snapshot_ext::WorldStateSnapshot;
use crate::types::*;
use std::collections::{HashMap, HashSet};

const MAX_IDEMPOTENCY_KEYS: usize = 4_096;
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

    /// Preset for unmodifiable stackable commodity consumption (e.g. PI commodities): rejects singletons, BPCs, fitted items, non-empty containers, and dynamic/mutated items.
    pub fn for_character_commodity(char_id: i64) -> Self {
        Self {
            reject_blueprint_copy: true,
            ..Self::for_character_stack(char_id)
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

/// Updated surviving stack produced by [`WorldRepository::stack_all_items`].
#[derive(Debug, Clone)]
pub struct StackAllSurvivor {
    /// Stack quantity prior to merging, passed as `ixStackSize` (`9`) old value in `OnItemChange`.
    pub old_quantity: i32,
    pub updated: InventoryItemRecord,
}

/// Atomic batch outcome returned by [`WorldRepository::stack_all_items`].
#[derive(Debug, Clone, Default)]
pub struct StackAllOutcome {
    pub merged_count: i64,
    pub deleted_sources: Vec<InventoryItemRecord>,
    pub survivors: Vec<StackAllSurvivor>,
}

/// Precomputed per-snapshot lock and container-occupancy sets for O(1) per-item
/// batch validation in `stack_all_items` and `MultiMerge`.
#[derive(Debug, Clone, Default)]
pub struct InventoryLockIndex {
    pub direct_escrow_locked: HashSet<i64>,
    pub alsc_locked: HashSet<i64>,
    pub unpaid_wrap_items: HashSet<i64>,
    pub non_empty_containers: HashSet<i64>,
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
            if WorldStateSnapshot::has_non_stackable_item_state(&src) {
                return Err(DbError::ValidationError(
                    "Cannot stack singleton, blueprint copy, or mutated item".into(),
                ));
            }
            let tgt_ref = guard
                .items
                .get(&target_item_id)
                .ok_or(DbError::ItemNotFound(target_item_id))?;
            if tgt_ref.owner_id != character_id {
                return Err(DbError::ItemOwnershipViolation {
                    character_id,
                    item_id: target_item_id,
                });
            }
            if WorldStateSnapshot::has_non_stackable_item_state(tgt_ref) {
                return Err(DbError::ValidationError(
                    "Cannot stack onto a singleton, blueprint copy, or mutated item".into(),
                ));
            }
            if tgt_ref.type_id != src.type_id {
                return Err(DbError::ValidationError(format!(
                    "Cannot stack different item types: {} and {}",
                    tgt_ref.type_id, src.type_id
                )));
            }
            if guard.is_courier_cargo_item(target_item_id)
                || guard.is_courier_cargo_item(source_item_id)
            {
                return Err(DbError::ValidationError(
                    "Cannot stack courier mission cargo".into(),
                ));
            }
            if tgt_ref.location_id != src.location_id || tgt_ref.flag != src.flag {
                return Err(DbError::ValidationError(
                    "Cannot stack items at different locations or flags".into(),
                ));
            }
            let new_qty = tgt_ref.quantity.checked_add(src.quantity).ok_or_else(|| {
                DbError::ValidationError("Stack quantity overflow".into())
            })?;
            let tgt = guard
                .items
                .get_mut(&target_item_id)
                .ok_or(DbError::ItemNotFound(target_item_id))?;
            tgt.quantity = new_qty;
            tgt.version += 1;
            let updated = tgt.clone();

            guard.items.remove(&source_item_id);
            Ok(updated)
        })
    }

    /// Atomically merges all eligible stacks owned by `character_id` at
    /// `(target_location_id, target_flag)` in a single snapshot transaction.
    ///
    /// Precomputes escrow, ALSC, Asset Safety wrap, and container-occupancy sets once per
    /// call so hangar stacking runs in linear time with at most one SQLite commit.
    pub fn stack_all_items(
        &self,
        character_id: i64,
        target_location_id: i64,
        target_flag: InventoryFlag,
        extra_excluded_item_ids: &HashSet<i64>,
    ) -> Result<StackAllOutcome, DbError> {
        if WorldStateSnapshot::is_flag_stack_ineligible(target_flag) {
            return Ok(StackAllOutcome::default());
        }
        self.try_with_mut_snapshot(|guard| {
            Ok(guard.stack_all_items_in_snap(
                character_id,
                target_location_id,
                target_flag,
                extra_excluded_item_ids,
            ))
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
        self.try_with_mut_snapshot(|guard| guard.split_stack_in_snap(character_id, item_id, split_quantity))
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
            // Check if existing clean stackable item exists at location/flag (ignoring singletons, BPCs, mutated, and courier cargo stacks)
            let existing_id = guard
                .items
                .values()
                .find(|it| {
                    it.owner_id == owner_id
                        && it.location_id == location_id
                        && it.flag == flag
                        && it.type_id == type_id
                        && guard.is_clean_stackable_item(it)
                })
                .map(|it| it.item_id);
            if let Some(existing_id) = existing_id {
                let updated = {
                    let existing = guard.items.get_mut(&existing_id).unwrap();
                    existing.quantity = existing.quantity.checked_add(qty_i32).ok_or_else(|| {
                        DbError::ValidationError("Stack quantity overflow".into())
                    })?;
                    existing.version += 1;
                    existing.clone()
                };
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

    /// Returns `true` if `flag` cannot hold stackable inventory items (fitted slots,
    /// `Locked`, `Impounded`, or `AssetSafety`).
    pub fn is_flag_stack_ineligible(flag: InventoryFlag) -> bool {
        Self::is_fitted_slot_flag(flag)
            || matches!(flag, InventoryFlag::Locked | InventoryFlag::Impounded)
            || matches!(
                flag as u16,
                FLAG_ASSET_SAFETY | FLAG_LOCKED | FLAG_IMPOUNDED
            )
    }

    /// Returns `true` if `item` carries a locked/impounded/asset-safety flag or is a sealed wrap type (`3468`, `60`).
    pub fn has_locked_flag_or_wrap_type(item: &InventoryItemRecord) -> bool {
        matches!(item.flag, InventoryFlag::Locked | InventoryFlag::Impounded)
            || matches!(
                item.flag as u16,
                FLAG_ASSET_SAFETY | FLAG_LOCKED | FLAG_IMPOUNDED
            )
            || item.type_id == TYPE_PLASTIC_WRAP
            || item.type_id == TYPE_ASSET_SAFETY_WRAP
    }

    /// Returns `true` if `item` has stack-incompatible intrinsic state
    /// (`singleton`, blueprint copy / limited-run BP, dynamic attributes, or mutaplasmid source).
    pub fn has_non_stackable_item_state(item: &InventoryItemRecord) -> bool {
        item.singleton
            || item.is_blueprint_copy
            || item.blueprint_runs_remaining.is_some()
            || !item.dynamic_attributes.is_empty()
            || item.source_mutaplasmid_id.is_some()
    }

    /// Returns `true` if `item_id` is tagged as courier mission cargo (`courier_cargo:`).
    pub fn is_courier_cargo_item(&self, item_id: i64) -> bool {
        self.item_custom_names
            .get(&item_id)
            .is_some_and(|tag| tag.starts_with("courier_cargo:"))
    }

    fn is_item_stack_all_candidate(
        &self,
        item: &InventoryItemRecord,
        lock_index: &InventoryLockIndex,
        extra_excluded_item_ids: &HashSet<i64>,
    ) -> bool {
        item.quantity > 0
            && !Self::has_locked_flag_or_wrap_type(item)
            && !Self::is_fitted_slot_flag(item.flag)
            && self.is_clean_stackable_item(item)
            && !extra_excluded_item_ids.contains(&item.item_id)
            && !lock_index.direct_escrow_locked.contains(&item.item_id)
            && !lock_index.alsc_locked.contains(&item.item_id)
            && !lock_index.unpaid_wrap_items.contains(&item.item_id)
            && !lock_index.non_empty_containers.contains(&item.item_id)
    }

    /// Collects IDs of all containers currently holding at least one child item with positive quantity.
    pub fn non_empty_container_ids(&self) -> HashSet<i64> {
        let mut non_empty = HashSet::new();
        for other in self.items.values() {
            if other.location_id != other.item_id && other.quantity > 0 {
                non_empty.insert(other.location_id);
            }
        }
        non_empty
    }

    /// Returns `true` if `item_id` contains at least one child item with positive quantity (`Inv 5`).
    pub fn is_container_non_empty(&self, item_id: i64) -> bool {
        self.items
            .values()
            .any(|other| other.item_id != item_id && other.location_id == item_id && other.quantity > 0)
    }

    /// Collects all item IDs locked via ALSC container security (`entity_kv` `Ns::AlscLockedItem`).
    pub fn alsc_locked_item_ids(&self) -> HashSet<i64> {
        self.kv(crate::service_domains::Ns::AlscLockedItem)
            .iter_ns()
            .filter_map(|(k, row)| {
                if k.sub_key.is_empty() && matches!(serde_json::from_str::<bool>(&row.value_json), Ok(true)) {
                    Some(k.entity_id)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Returns `true` if `item_id` is locked via ALSC container security (`entity_kv` `Ns::AlscLockedItem`).
    pub fn is_item_in_alsc_lock(&self, item_id: i64) -> bool {
        self.kv(crate::service_domains::Ns::AlscLockedItem)
            .get::<bool>(item_id, "")
            .unwrap_or(false)
    }

    fn collect_wrap_ancestor_chain(&self, item_id: i64) -> Vec<i64> {
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
        chain
    }

    fn visit_unpaid_asset_safety_wrap_ids(&self, mut on_id: impl FnMut(i64) -> bool) -> bool {
        let Some(ext_json) = self.item_custom_names.get(&STRUCTURE_EXT_STATE_KEY) else {
            return false;
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(ext_json) else {
            return false;
        };
        let Some(wraps) = val.get("asset_safety_wraps").and_then(|v| v.as_object()) else {
            return false;
        };
        for (key_str, wrap_val) in wraps {
            if wrap_val.get("retrieval_fee_paid").and_then(|v| v.as_bool()) == Some(true) {
                continue;
            }
            if let Some(wid) = wrap_val
                .get("wrap_id")
                .and_then(|v| v.as_i64())
                .or_else(|| key_str.parse::<i64>().ok())
            {
                if on_id(wid) {
                    return true;
                }
            }
            if let Some(arr) = wrap_val.get("item_ids").and_then(|v| v.as_array()) {
                for iid in arr.iter().filter_map(|v| v.as_i64()) {
                    if on_id(iid) {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Collects all wrap and item IDs belonging to unpaid Asset Safety wraps
    /// (`item_custom_names[-7_700_001]`).
    pub fn unpaid_asset_safety_wrap_item_ids(&self) -> HashSet<i64> {
        let mut ids = HashSet::new();
        self.visit_unpaid_asset_safety_wrap_ids(|id| {
            ids.insert(id);
            false
        });
        ids
    }

    /// Returns `true` if `item_id` is an Asset Safety Wrap (`type_id = 60`), carries flag `AssetSafety`
    /// (`36`), or sits inside an unpaid `asset_safety_wraps` record (`Inv 2`).
    pub fn is_item_in_active_asset_safety_wrap(&self, item_id: i64) -> bool {
        if let Some(it) = self.items.get(&item_id) {
            if it.type_id == TYPE_ASSET_SAFETY_WRAP || (it.flag as u16) == FLAG_ASSET_SAFETY {
                return true;
            }
        }
        let chain = self.collect_wrap_ancestor_chain(item_id);
        self.visit_unpaid_asset_safety_wrap_ids(|id| chain.contains(&id))
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

    fn is_id_or_chain_escrow_locked_with_set(&self, locked: &HashSet<i64>, item_id: i64) -> bool {
        !locked.is_empty()
            && self
                .collect_escrow_ancestor_chain(item_id)
                .into_iter()
                .any(|id| locked.contains(&id))
    }

    fn is_id_or_chain_in_unpaid_wrap(&self, unpaid_wraps: &HashSet<i64>, item_id: i64) -> bool {
        if let Some(it) = self.items.get(&item_id) {
            if it.type_id == TYPE_ASSET_SAFETY_WRAP || (it.flag as u16) == FLAG_ASSET_SAFETY {
                return true;
            }
        }
        !unpaid_wraps.is_empty()
            && self
                .collect_wrap_ancestor_chain(item_id)
                .into_iter()
                .any(|id| unpaid_wraps.contains(&id))
    }

    fn is_ancestor_chain_locked(
        &self,
        start_loc: i64,
        check_item: impl Fn(&InventoryItemRecord) -> bool,
    ) -> bool {
        let mut cur_loc = start_loc;
        for _ in 0..8 {
            let Some(anc) = self.items.get(&cur_loc) else {
                break;
            };
            if check_item(anc) {
                return true;
            }
            cur_loc = anc.location_id;
        }
        false
    }

    fn is_single_item_locked_with_index(
        &self,
        index: &InventoryLockIndex,
        item: &InventoryItemRecord,
    ) -> bool {
        Self::has_locked_flag_or_wrap_type(item)
            || index.alsc_locked.contains(&item.item_id)
            || self.is_id_or_chain_escrow_locked_with_set(&index.direct_escrow_locked, item.item_id)
            || self.is_id_or_chain_in_unpaid_wrap(&index.unpaid_wrap_items, item.item_id)
    }

    /// Builds precomputed per-snapshot lock and container-occupancy sets in a single pass.
    pub fn build_inventory_lock_index(&self) -> InventoryLockIndex {
        InventoryLockIndex {
            direct_escrow_locked: self.direct_escrow_locked_item_ids(None),
            alsc_locked: self.alsc_locked_item_ids(),
            unpaid_wrap_items: self.unpaid_asset_safety_wrap_item_ids(),
            non_empty_containers: self.non_empty_container_ids(),
        }
    }

    /// Returns `true` if `location_id` or its ancestor chain locks all direct child items
    /// at `location_id` under `is_item_or_ancestor_locked_or_impounded`.
    fn is_location_chain_locked_for_children(
        &self,
        index: &InventoryLockIndex,
        location_id: i64,
    ) -> bool {
        if location_id > 0
            && !self.structures.contains_key(&location_id)
            && index.direct_escrow_locked.contains(&location_id)
        {
            return true;
        }
        if index.unpaid_wrap_items.contains(&location_id) {
            return true;
        }
        self.is_ancestor_chain_locked(location_id, |anc| {
            self.is_single_item_locked_with_index(index, anc)
        })
    }

    /// Returns `true` if `item_id` (or an ancestor container up to 8 hops) is directly escrowed
    /// according to `index.direct_escrow_locked`.
    pub fn is_item_escrow_locked_with_index(
        &self,
        index: &InventoryLockIndex,
        item_id: i64,
    ) -> bool {
        self.is_id_or_chain_escrow_locked_with_set(&index.direct_escrow_locked, item_id)
    }

    /// Equivalent to [`Self::is_item_or_ancestor_locked_or_impounded`] using a precomputed
    /// [`InventoryLockIndex`] to avoid repeated full-snapshot scans during batch operations.
    pub fn is_item_or_ancestor_locked_or_impounded_with_index(
        &self,
        index: &InventoryLockIndex,
        item_id: i64,
    ) -> bool {
        if self.is_id_or_chain_escrow_locked_with_set(&index.direct_escrow_locked, item_id)
            || index.alsc_locked.contains(&item_id)
            || self.is_id_or_chain_in_unpaid_wrap(&index.unpaid_wrap_items, item_id)
        {
            return true;
        }
        let Some(item) = self.items.get(&item_id) else {
            return false;
        };
        Self::has_locked_flag_or_wrap_type(item)
            || self.is_ancestor_chain_locked(item.location_id, |anc| {
                self.is_single_item_locked_with_index(index, anc)
            })
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
        if Self::has_locked_flag_or_wrap_type(item) {
            return true;
        }
        self.is_ancestor_chain_locked(item.location_id, |anc| {
            Self::has_locked_flag_or_wrap_type(anc)
                || self.is_escrow_locked_opt(anc.item_id, exclude_trade_lock_id)
                || self.is_item_in_alsc_lock(anc.item_id)
                || self.is_item_in_active_asset_safety_wrap(anc.item_id)
        })
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
            InventoryFlag::Deliveries
                | InventoryFlag::CorpDeliveries
                | InventoryFlag::CapsuleerDeliveries
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

    /// Returns `true` if `item` is an unpackaged, non-blueprint-copy, unmodified,
    /// non-courier stack eligible for merging or commodity stacking.
    pub fn is_clean_stackable_item(&self, item: &InventoryItemRecord) -> bool {
        !Self::has_non_stackable_item_state(item) && !self.is_courier_cargo_item(item.item_id)
    }

    /// Merges all eligible stacks owned by `character_id` at `(target_location_id, target_flag)`
    /// inside an open mutable snapshot transaction.
    pub fn stack_all_items_in_snap(
        &mut self,
        character_id: i64,
        target_location_id: i64,
        target_flag: InventoryFlag,
        extra_excluded_item_ids: &HashSet<i64>,
    ) -> StackAllOutcome {
        if Self::is_flag_stack_ineligible(target_flag) {
            return StackAllOutcome::default();
        }
        let lock_index = self.build_inventory_lock_index();
        if self.is_location_chain_locked_for_children(&lock_index, target_location_id) {
            return StackAllOutcome::default();
        }

        let mut by_type: HashMap<TypeId, Vec<(i64, i32)>> = HashMap::new();
        for item in self.items.values() {
            if item.owner_id == character_id
                && item.location_id == target_location_id
                && item.flag == target_flag
                && self.is_item_stack_all_candidate(item, &lock_index, extra_excluded_item_ids)
            {
                by_type
                    .entry(item.type_id)
                    .or_default()
                    .push((item.item_id, item.quantity));
            }
        }

        let mut type_ids: Vec<TypeId> = by_type.keys().copied().collect();
        type_ids.sort_unstable();

        struct MergePlan {
            dst_id: i64,
            old_dst_qty: i32,
            new_dst_qty: i32,
            src_ids: Vec<i64>,
        }
        let mut plans = Vec::new();
        for type_id in type_ids {
            let Some(group) = by_type.get_mut(&type_id) else {
                continue;
            };
            if group.len() < 2 {
                continue;
            }
            group.sort_unstable_by_key(|&(item_id, _)| item_id);
            let (dst_id, old_dst_qty) = group[0];
            let mut cur_qty = old_dst_qty;
            let mut src_ids = Vec::with_capacity(group.len() - 1);
            for &(src_id, src_qty) in &group[1..] {
                let Some(sum) = cur_qty.checked_add(src_qty) else {
                    continue;
                };
                cur_qty = sum;
                src_ids.push(src_id);
            }
            if !src_ids.is_empty() {
                plans.push(MergePlan {
                    dst_id,
                    old_dst_qty,
                    new_dst_qty: cur_qty,
                    src_ids,
                });
            }
        }

        if plans.is_empty() {
            return StackAllOutcome::default();
        }

        let mut outcome = StackAllOutcome::default();
        for plan in plans {
            for src_id in plan.src_ids {
                if let Some(old_src) = self.remove_system_cleanup_item(src_id) {
                    outcome.deleted_sources.push(old_src);
                    outcome.merged_count += 1;
                }
            }
            if let Some(tgt) = self.items.get_mut(&plan.dst_id) {
                tgt.quantity = plan.new_dst_qty;
                tgt.version += 1;
                outcome.survivors.push(StackAllSurvivor {
                    old_quantity: plan.old_dst_qty,
                    updated: tgt.clone(),
                });
            }
        }
        outcome
    }

    /// Splits `split_quantity` from `item_id` inside an open mutable snapshot transaction (`INV-2`).
    pub fn split_stack_in_snap(
        &mut self,
        character_id: i64,
        item_id: i64,
        split_quantity: i32,
    ) -> Result<(InventoryItemRecord, InventoryItemRecord), DbError> {
        if split_quantity <= 0 {
            return Err(DbError::ValidationError(
                "Split quantity must be positive".into(),
            ));
        }
        if self.is_item_or_ancestor_locked_or_impounded(item_id) {
            return Err(DbError::ItemLocked(item_id));
        }
        {
            let item_ref = self
                .items
                .get(&item_id)
                .ok_or(DbError::ItemNotFound(item_id))?;
            if item_ref.owner_id != character_id {
                return Err(DbError::ItemOwnershipViolation {
                    character_id,
                    item_id,
                });
            }
            if Self::is_fitted_slot_flag(item_ref.flag) {
                return Err(DbError::ItemLocked(item_id));
            }
            if self.is_container_non_empty(item_id) {
                return Err(DbError::ValidationError(
                    "Cannot split a non-empty container".into(),
                ));
            }
            if Self::has_non_stackable_item_state(item_ref) {
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
        let item = self
            .items
            .get_mut(&item_id)
            .ok_or(DbError::ItemNotFound(item_id))?;
        item.quantity -= split_quantity;
        item.version += 1;
        let updated_orig = item.clone();

        let new_id = self.alloc_entity_id();
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
        self.items.insert(new_id, new_item.clone());
        if let Some(custom_name) = self.item_custom_names.get(&item_id).cloned() {
            self.item_custom_names.insert(new_id, custom_name);
        }
        Ok((updated_orig, new_item))
    }

    pub fn validate_item_for_consumption(
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
        if opts.reject_singleton
            && (!item.dynamic_attributes.is_empty() || item.source_mutaplasmid_id.is_some())
        {
            return Err(DbError::ValidationError(format!(
                "Cannot consume mutated item {}",
                item.item_id
            )));
        }
        if opts.reject_singleton
            && self
                .item_custom_names
                .get(&item.item_id)
                .is_some_and(|tag| tag.starts_with("courier_cargo:"))
        {
            return Err(DbError::ValidationError(format!(
                "Cannot consume courier cargo item {}",
                item.item_id
            )));
        }
        if opts.reject_blueprint_copy
            && (item.is_blueprint_copy || item.blueprint_runs_remaining.is_some())
        {
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
            self.ship_fit_inputs.remove(&item_id);
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
                || item.blueprint_runs_remaining.is_some()
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

        if spec.merge_at_destination && self.is_clean_stackable_item(&item) {
            let mut candidates: Vec<&InventoryItemRecord> = self
                .items
                .values()
                .filter(|cand| {
                    cand.item_id != item_id
                        && cand.owner_id == dest_owner
                        && cand.location_id == spec.new_location_id
                        && cand.flag == spec.new_flag
                        && cand.type_id == item.type_id
                        && self.is_clean_stackable_item(cand)
                })
                .collect();
            candidates.sort_by_key(|c| c.item_id);
            let merge_target_id = candidates
                .iter()
                .find(|cand| cand.quantity.checked_add(target_qty).is_some())
                .or_else(|| candidates.first())
                .map(|c| c.item_id);

            if let Some(target_id) = merge_target_id {
                let new_tgt_qty = self
                    .items
                    .get(&target_id)
                    .ok_or(DbError::ItemNotFound(target_id))?
                    .quantity
                    .checked_add(target_qty)
                    .ok_or_else(|| DbError::ValidationError("Destination stack quantity overflow".into()))?;
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
                tgt.quantity = new_tgt_qty;
                tgt.version += 1;
                return Ok(tgt.clone());
            }
        }

        if is_partial {
            if let Some(src_mut) = self.items.get_mut(&item_id) {
                src_mut.quantity -= target_qty;
                src_mut.version += 1;
            }
            let new_id = self.alloc_entity_id();
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
            let out = entry.clone();
            self.set_owner_recursive(item_id, dest_owner);
            Ok(out)
        }
    }

    /// Recursively updates `owner_id = new_owner` on `item_id` and all nested items located inside it
    /// (up to depth 8, cycle-guarded). Returns `(pre, post)` pairs for all updated items.
    pub fn set_owner_recursive(
        &mut self,
        item_id: i64,
        new_owner: i64,
    ) -> Vec<(InventoryItemRecord, InventoryItemRecord)> {
        let mut changed = Vec::new();
        let mut visited = std::collections::HashSet::new();
        visited.insert(item_id);
        if let Some(root) = self.items.get_mut(&item_id) {
            if root.owner_id != new_owner {
                let pre = root.clone();
                root.owner_id = new_owner;
                root.version += 1;
                changed.push((pre, root.clone()));
            }
        }
        let mut frontier = vec![item_id];
        for _ in 0..8 {
            if frontier.is_empty() {
                break;
            }
            let children: Vec<i64> = self
                .items
                .values()
                .filter(|it| frontier.contains(&it.location_id) && !visited.contains(&it.item_id))
                .map(|it| it.item_id)
                .collect();
            frontier.clear();
            for cid in children {
                if visited.insert(cid) {
                    frontier.push(cid);
                    if let Some(child) = self.items.get_mut(&cid) {
                        if child.owner_id != new_owner {
                            let pre = child.clone();
                            child.owner_id = new_owner;
                            child.version += 1;
                            changed.push((pre, child.clone()));
                        }
                    }
                }
            }
        }
        changed
    }

    /// Consumes up to `quantity` units from `item_id`, returning `(pre_record, taken)`
    /// for transactional rollback via [`Self::rollback_restore_consumed_stacks`].
    pub fn reserve_consume_stack_for_rollback(
        &mut self,
        item_id: i64,
        quantity: i32,
    ) -> Option<(InventoryItemRecord, i32)> {
        if quantity <= 0 {
            return None;
        }
        let pre = self.items.get(&item_id)?.clone();
        let take = quantity.min(pre.quantity);
        if take <= 0 {
            return None;
        }
        if take >= pre.quantity {
            self.items.remove(&item_id);
        } else if let Some(entry) = self.items.get_mut(&item_id) {
            entry.quantity -= take;
            entry.version = entry.version.saturating_add(1);
        }
        Some((pre, take))
    }

    fn is_rollback_destination_alive(&self, location_id: i64) -> bool {
        let mut cur = location_id;
        for _ in 0..16 {
            if cur <= 0 {
                return false;
            }
            if (30_000_000..=39_999_999).contains(&cur)
                || (60_000_000..=69_999_999).contains(&cur)
                || self
                    .structures
                    .get(&cur)
                    .is_some_and(|st| st.state != "Destroyed")
            {
                return true;
            }
            let Some(parent) = self.items.get(&cur) else {
                return false;
            };
            if parent.location_id <= 0 {
                return true;
            }
            cur = parent.location_id;
        }
        false
    }

    fn is_eligible_rollback_stack_target(
        &self,
        existing: &InventoryItemRecord,
        pre: &InventoryItemRecord,
        take: i32,
    ) -> bool {
        existing.owner_id == pre.owner_id
            && existing.location_id == pre.location_id
            && existing.flag == pre.flag
            && existing.type_id == pre.type_id
            && self.is_clean_stackable_item(existing)
            && !Self::has_locked_flag_or_wrap_type(existing)
            && !self.is_item_or_ancestor_locked_or_impounded(existing.item_id)
            && existing.quantity.checked_add(take).is_some()
    }

    /// Restores stack quantities previously reserved by [`Self::reserve_consume_stack_for_rollback`],
    /// validating destination existence, ownership, location, flag, type, clean/unlocked state,
    /// and checked arithmetic. Returns `(pre, post)` item-mutation diffs for any stack whose
    /// restored state diverged from `pre` (`invCache` sync on fallback merge/mint/disappearance).
    pub fn rollback_restore_consumed_stacks(
        &mut self,
        consumed: &[(InventoryItemRecord, i32)],
    ) -> Vec<(Option<InventoryItemRecord>, Option<InventoryItemRecord>)> {
        let mut diffs = Vec::new();
        for (pre, take) in consumed {
            if *take <= 0 {
                continue;
            }
            if !self.is_rollback_destination_alive(pre.location_id) {
                diffs.push((Some(pre.clone()), self.items.get(&pre.item_id).cloned()));
                continue;
            }
            let dest_locked = self.is_item_or_ancestor_locked_or_impounded(pre.location_id);

            if let Some(existing) = self.items.get(&pre.item_id).cloned() {
                if self.is_eligible_rollback_stack_target(&existing, pre, *take) {
                    if let Some(entry) = self.items.get_mut(&pre.item_id) {
                        if let Some(new_qty) = entry.quantity.checked_add(*take) {
                            entry.quantity = new_qty;
                            entry.version = entry.version.saturating_add(1);
                            let post = entry.clone();
                            let same_identity_state = post.owner_id == pre.owner_id
                                && post.location_id == pre.location_id
                                && post.flag == pre.flag
                                && post.quantity == pre.quantity
                                && post.singleton == pre.singleton;
                            if !same_identity_state {
                                diffs.push((Some(existing), Some(post)));
                            }
                            continue;
                        }
                    }
                }
                diffs.push((Some(pre.clone()), Some(existing)));
            } else if !dest_locked
                && !Self::has_locked_flag_or_wrap_type(pre)
                && (!pre.singleton || *take == 1)
            {
                let mut restored = pre.clone();
                restored.quantity = *take;
                restored.version = pre.version.saturating_add(1);
                self.items.insert(pre.item_id, restored.clone());
                if *take != pre.quantity {
                    diffs.push((None, Some(restored)));
                }
                continue;
            } else {
                diffs.push((Some(pre.clone()), None));
            }

            if dest_locked || pre.singleton {
                continue;
            }

            let sibling_id = if self.is_clean_stackable_item(pre) {
                self.items
                    .values()
                    .find(|existing| {
                        existing.item_id != pre.item_id
                            && self.is_eligible_rollback_stack_target(existing, pre, *take)
                    })
                    .map(|existing| existing.item_id)
            } else {
                None
            };
            if let Some(target_id) = sibling_id {
                if let Some(existing) = self.items.get_mut(&target_id) {
                    if let Some(new_qty) = existing.quantity.checked_add(*take) {
                        let sib_pre = existing.clone();
                        existing.quantity = new_qty;
                        existing.version = existing.version.saturating_add(1);
                        let sib_post = existing.clone();
                        diffs.push((Some(sib_pre), Some(sib_post)));
                        continue;
                    }
                }
            }

            let mut item_id = self.alloc_entity_id();
            while self.items.contains_key(&item_id) {
                item_id = self.alloc_entity_id();
            }
            let new_item = InventoryItemRecord {
                item_id,
                type_id: pre.type_id,
                owner_id: pre.owner_id,
                location_id: pre.location_id,
                flag: pre.flag,
                quantity: *take,
                singleton: false,
                version: 1,
                ..InventoryItemRecord::default()
            };
            self.items.insert(item_id, new_item.clone());
            diffs.push((None, Some(new_item)));
        }
        diffs
    }
}
#[cfg(test)] #[path = "inventory_unit_tests.rs"] mod inventory_unit_tests;


