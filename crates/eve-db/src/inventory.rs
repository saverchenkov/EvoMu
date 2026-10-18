//! Inventory item queries and atomic transactional item movement.

use crate::persistence::WorldRepository;
use crate::types::*;

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

            // Authorize destination: must be the character's current station, structure, or co-located active ship
            let valid_dest = (Some(new_location_id) == ch.station_id)
                || (Some(new_location_id) == ch.structure_id)
                || (active_ship_colocated && new_location_id == ch.active_ship_id);
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
}
