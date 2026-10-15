//! `eve-db` — Transactional persistence layer and domain repositories for `EvoMu`.
//!
//! Encapsulates all account, character, and inventory mutations behind atomic repository
//! operations with entity versioning, fixed-point ISK currency (`IskCents`),
//! per-character idempotency keys, and strict ownership/destination authorization.

#![forbid(unsafe_code)]

use eve_sde::{SolarSystemId, TypeId};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::RwLock;
use thiserror::Error;

const MAX_IDEMPOTENCY_KEYS: usize = 4_096;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DbError {
    #[error("Invalid credentials")]
    InvalidCredentials,
    #[error("Character {0} not found")]
    CharacterNotFound(i64),
    #[error("Character {0} is already undocked in space")]
    NotDocked(i64),
    #[error("Character {character_id} is not owned by user {user_id}")]
    CharacterOwnershipViolation { user_id: u64, character_id: i64 },
    #[error("Item {0} not found")]
    ItemNotFound(i64),
    #[error("Item {item_id} is not owned by character {character_id}")]
    ItemOwnershipViolation { character_id: i64, item_id: i64 },
    #[error("Destination location {location_id} is not authorized for character {character_id}")]
    InvalidDestinationLocation { character_id: i64, location_id: i64 },
    #[error("Stale entity version (expected {expected}, found {actual})")]
    VersionConflict { expected: u64, actual: u64 },
}

/// Fixed-point ISK representation in cents (`1 ISK = 100 IskCents`) to prevent `f64` precision drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct IskCents(pub i64);

impl IskCents {
    pub const fn from_isk(whole_isk: i64) -> Self {
        Self(whole_isk * 100)
    }

    pub fn to_wire_f64(self) -> f64 {
        (self.0 as f64) / 100.0
    }
}

/// Standard EVE inventory location flags (`invFlags`).
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InventoryFlag {
    None = 0,
    Hangar = 4,
    Cargo = 5,
    Skill = 7,
    LoSlot0 = 11,
    MedSlot0 = 19,
    HiSlot0 = 27,
    /// `invFlags`: 56 = Capsule, 57 = Pilot.
    Capsule = 56,
    Pilot = 57,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountRecord {
    pub user_id: u64,
    pub username: String,
    pub password_token: String,
    pub role: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterRecord {
    pub character_id: i64,
    pub user_id: u64,
    pub name: String,
    pub corporation_id: i64,
    pub alliance_id: Option<i64>,
    pub station_id: Option<i64>,
    pub solar_system_id: SolarSystemId,
    pub constellation_id: u32,
    pub region_id: u32,
    pub active_ship_id: i64,
    pub balance: IskCents,
    pub skill_points: f64,
    pub version: u64,
    pub system_epoch: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InventoryItemRecord {
    pub item_id: i64,
    pub type_id: TypeId,
    pub owner_id: i64,
    pub location_id: i64,
    pub flag: InventoryFlag,
    pub quantity: i32,
    pub singleton: bool,
    pub version: u64,
}

struct WorldStateInner {
    accounts: HashMap<String, AccountRecord>,
    characters: HashMap<i64, CharacterRecord>,
    items: HashMap<i64, InventoryItemRecord>,
    committed_idempotency_keys: HashSet<(i64, String)>,
}

/// Encapsulated transactional world repository.
pub struct WorldRepository {
    inner: RwLock<WorldStateInner>,
}

impl Default for WorldRepository {
    fn default() -> Self {
        Self::with_demo_seed()
    }
}

impl WorldRepository {
    pub fn with_demo_seed() -> Self {
        let mut accounts = HashMap::new();
        accounts.insert(
            "demo".to_string(),
            AccountRecord {
                user_id: 1,
                username: "demo".to_string(),
                password_token: "demo-secret".to_string(),
                role: 0x0000_0000_0000_0003,
            },
        );
        accounts.insert(
            "other".to_string(),
            AccountRecord {
                user_id: 2,
                username: "other".to_string(),
                password_token: "other-secret".to_string(),
                role: 0x0000_0000_0000_0003,
            },
        );

        let mut characters = HashMap::new();
        characters.insert(
            90_000_001,
            CharacterRecord {
                character_id: 90_000_001,
                user_id: 1,
                name: "Aura Capsuleer".to_string(),
                corporation_id: 1_000_125,
                alliance_id: None,
                station_id: Some(60_003_760), // Jita 4-4
                solar_system_id: 30_000_142,  // Jita
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: 100_000_001,
                balance: IskCents::from_isk(1_000_000_000),
                skill_points: 15_000_000.0,
                version: 1,
                system_epoch: 1,
            },
        );
        characters.insert(
            90_000_002,
            CharacterRecord {
                character_id: 90_000_002,
                user_id: 2,
                name: "Rival Pilot".to_string(),
                corporation_id: 1_000_125,
                alliance_id: None,
                station_id: Some(60_003_760),
                solar_system_id: 30_000_142,
                constellation_id: 20_000_020,
                region_id: 10_000_002,
                active_ship_id: 100_000_002,
                balance: IskCents::from_isk(50_000_000),
                skill_points: 5_000_000.0,
                version: 1,
                system_epoch: 1,
            },
        );

        let mut items = HashMap::new();
        items.insert(
            100_000_001,
            InventoryItemRecord {
                item_id: 100_000_001,
                type_id: 587, // Rifter
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
            },
        );
        items.insert(
            100_000_002,
            InventoryItemRecord {
                item_id: 100_000_002,
                type_id: 587,
                owner_id: 90_000_002,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
            },
        );

        Self {
            inner: RwLock::new(WorldStateInner {
                accounts,
                characters,
                items,
                committed_idempotency_keys: HashSet::new(),
            }),
        }
    }

    pub fn authenticate(
        &self,
        username: &str,
        password_token: &str,
    ) -> Result<AccountRecord, DbError> {
        let guard = self.inner.read().unwrap();
        let acc = guard
            .accounts
            .get(username)
            .ok_or(DbError::InvalidCredentials)?;
        if acc.password_token != password_token {
            return Err(DbError::InvalidCredentials);
        }
        Ok(acc.clone())
    }

    pub fn get_characters_for_user(&self, user_id: u64) -> Vec<CharacterRecord> {
        let guard = self.inner.read().unwrap();
        let mut chars: Vec<CharacterRecord> = guard
            .characters
            .values()
            .filter(|c| c.user_id == user_id)
            .cloned()
            .collect();
        chars.sort_by_key(|c| c.character_id);
        chars
    }

    pub fn select_character_for_user(
        &self,
        user_id: u64,
        character_id: i64,
    ) -> Result<CharacterRecord, DbError> {
        let guard = self.inner.read().unwrap();
        let ch = guard
            .characters
            .get(&character_id)
            .ok_or(DbError::CharacterNotFound(character_id))?;
        if ch.user_id != user_id {
            return Err(DbError::CharacterOwnershipViolation {
                user_id,
                character_id,
            });
        }
        Ok(ch.clone())
    }

    pub fn get_items_for_character(&self, character_id: i64) -> Vec<InventoryItemRecord> {
        let guard = self.inner.read().unwrap();
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
        let mut guard = self.inner.write().unwrap();

        let ch = guard
            .characters
            .get(&character_id)
            .ok_or(DbError::CharacterNotFound(character_id))?
            .clone();

        // Authorize destination: must be the character's current station or active ship
        let valid_dest =
            (Some(new_location_id) == ch.station_id) || (new_location_id == ch.active_ship_id);
        if !valid_dest {
            return Err(DbError::InvalidDestinationLocation {
                character_id,
                location_id: new_location_id,
            });
        }

        // Verify ownership BEFORE checking idempotency cache so keys cannot leak other players' items
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
    }

    /// Atomically transitions a docked character and their active ship into space (`location_id = solar_system_id`, `flag = None`).
    /// Validates both character docked state and ship existence/ownership BEFORE mutating either record.
    pub fn undock_character(
        &self,
        user_id: u64,
        character_id: i64,
    ) -> Result<(i64, CharacterRecord, InventoryItemRecord), DbError> {
        let mut guard = self.inner.write().unwrap();

        // Step 1: Validate character ownership and docked state without mutating
        let (station_id, active_ship_id, solar_system_id) = {
            let ch = guard
                .characters
                .get(&character_id)
                .ok_or(DbError::CharacterNotFound(character_id))?;
            if ch.user_id != user_id {
                return Err(DbError::CharacterOwnershipViolation {
                    user_id,
                    character_id,
                });
            }
            let st_id = ch.station_id.ok_or(DbError::NotDocked(character_id))?;
            (st_id, ch.active_ship_id, ch.solar_system_id)
        };

        // Step 2: Validate ship existence and ownership before mutating character
        {
            let ship = guard
                .items
                .get(&active_ship_id)
                .ok_or(DbError::ItemNotFound(active_ship_id))?;
            if ship.owner_id != character_id {
                return Err(DbError::ItemOwnershipViolation {
                    character_id,
                    item_id: active_ship_id,
                });
            }
        }

        // Step 3: Both checks succeeded — apply atomic mutation to both records
        let ch = guard.characters.get_mut(&character_id).unwrap();
        ch.station_id = None;
        ch.version += 1;
        ch.system_epoch += 1;
        let ch_snapshot = ch.clone();

        let ship = guard.items.get_mut(&active_ship_id).unwrap();
        ship.location_id = solar_system_id as i64;
        ship.flag = InventoryFlag::None;
        ship.version += 1;
        let ship_snapshot = ship.clone();

        Ok((station_id, ch_snapshot, ship_snapshot))
    }

    /// Compensating transaction to restore docked station state if solar-system actor spawn fails.
    pub fn rollback_undock(&self, character_id: i64, station_id: i64, ship_id: i64) {
        let mut guard = self.inner.write().unwrap();
        if let Some(ch) = guard.characters.get_mut(&character_id) {
            ch.station_id = Some(station_id);
            ch.version += 1;
            ch.system_epoch += 1;
        }
        if let Some(ship) = guard.items.get_mut(&ship_id) {
            ship.location_id = station_id;
            ship.flag = InventoryFlag::Hangar;
            ship.version += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_move_item_ownership_idempotency_and_version_conflict() {
        let repo = WorldRepository::with_demo_seed();

        // Cross-character idempotency key probe must fail with ItemOwnershipViolation
        let res = repo.move_item_transactional(
            90_000_002,
            100_000_001,
            60_003_760,
            InventoryFlag::Cargo,
            None,
            Some("shared-key-1"),
        );
        assert!(matches!(res, Err(DbError::ItemOwnershipViolation { .. })));

        // Valid move with idempotency key increments version once
        let moved1 = repo
            .move_item_transactional(
                90_000_001,
                100_000_001,
                60_003_760,
                InventoryFlag::Cargo,
                Some(1),
                Some("move-rifter-to-cargo"),
            )
            .unwrap();
        assert_eq!(moved1.flag, InventoryFlag::Cargo);
        assert_eq!(moved1.version, 2);

        // Replaying the same idempotency key for the same character returns current record without re-incrementing
        let moved2 = repo
            .move_item_transactional(
                90_000_001,
                100_000_001,
                60_003_760,
                InventoryFlag::Cargo,
                Some(1),
                Some("move-rifter-to-cargo"),
            )
            .unwrap();
        assert_eq!(moved2.version, 2);

        // Stale expected_version with a new key fails with VersionConflict
        let stale = repo.move_item_transactional(
            90_000_001,
            100_000_001,
            60_003_760,
            InventoryFlag::Hangar,
            Some(1),
            Some("new-key-stale-version"),
        );
        assert!(matches!(
            stale,
            Err(DbError::VersionConflict {
                expected: 1,
                actual: 2
            })
        ));
    }

    #[test]
    fn test_atomic_undock_and_rollback() {
        let repo = WorldRepository::with_demo_seed();
        let (station_id, ch, ship) = repo.undock_character(1, 90_000_001).unwrap();
        assert_eq!(station_id, 60_003_760);
        assert_eq!(ch.station_id, None);
        assert_eq!(ship.location_id, 30_000_142);
        assert_eq!(ship.flag, InventoryFlag::None);

        // Second undock while in space fails
        assert!(matches!(
            repo.undock_character(1, 90_000_001),
            Err(DbError::NotDocked(90_000_001))
        ));

        // Rollback restores docked state
        repo.rollback_undock(90_000_001, station_id, ship.item_id);
        let restored = repo.select_character_for_user(1, 90_000_001).unwrap();
        assert_eq!(restored.station_id, Some(60_003_760));
    }
}
