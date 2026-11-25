//! Protocol-agnostic inventory item granting and creation domain services (`#1193`).
//!
//! Both GM slash commands (`/giveitem`, `/createitem`) and live gameplay item reward/redeem/loot/LP
//! flows route through [`grant_character_item_tx`] and [`grant_item_in_snap`] so validation,
//! singleton classification, destination selection, and snapshot mutation share one path backed by
//! [`eve_db`] and automatic `ChangeSet` delta projection (`OnItemChange`, `OnBlueprintsUpdated`).

#![forbid(unsafe_code)]

use crate::domain::DomainError;
use crate::router::ServerContext;
use eve_db::{DbError, InventoryFlag, InventoryItemRecord, MintItemSpec, WorldStateSnapshot};
use eve_sde::SdeRegistry;

/// Policy selecting where a granted item is placed for a target character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemDeliveryPolicy {
    /// Place in docked station/structure hangar (`InventoryFlag::Hangar`), or in active ship cargo
    /// (`InventoryFlag::Cargo`) when undocked in space.
    DockedHangarOrActiveShipCargo,
    /// Place in docked station/structure hangar, falling back to `home_station_id` (`InventoryFlag::Hangar`).
    DockedOrHomeHangar,
}

/// Destination specification for [`GrantItemRequest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantItemDestination {
    Explicit {
        location_id: i64,
        flag: InventoryFlag,
    },
    CharacterPolicy(ItemDeliveryPolicy),
}

/// Policy determining the `singleton` state of a newly created item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemSingletonPolicy {
    /// Ships (`category_id == 6`) are always singletons, and single-quantity modules/subsystems
    /// (`category_id` 7 or 32 with `quantity == 1`) are created assembled/singleton.
    AutoFromSde,
    /// Only ships (`category_id == 6`) are singletons; all other types default to unpackaged stacks.
    ShipOnlyFromSde,
    /// Explicit caller-chosen singleton flag.
    Fixed(bool),
}

/// Request describing an item grant operation executed via [`grant_item_in_snap`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantItemRequest {
    pub owner_id: i64,
    pub target_char_id: Option<i64>,
    pub destination: GrantItemDestination,
    pub type_id: u32,
    pub quantity: i32,
    pub singleton_policy: ItemSingletonPolicy,
    pub is_blueprint_copy: bool,
    pub blueprint_runs_remaining: Option<i32>,
    pub merge_existing_stack: bool,
    pub require_sde_type: bool,
}

impl GrantItemRequest {
    /// Constructs an item grant for an explicit `(location_id, flag)` with non-singleton defaults.
    pub fn explicit(
        owner_id: i64,
        location_id: i64,
        flag: InventoryFlag,
        type_id: u32,
        quantity: i32,
    ) -> Self {
        Self {
            owner_id,
            target_char_id: None,
            destination: GrantItemDestination::Explicit { location_id, flag },
            type_id,
            quantity,
            singleton_policy: ItemSingletonPolicy::Fixed(false),
            is_blueprint_copy: false,
            blueprint_runs_remaining: None,
            merge_existing_stack: false,
            require_sde_type: false,
        }
    }

    pub fn with_singleton(mut self, singleton: bool) -> Self {
        self.singleton_policy = ItemSingletonPolicy::Fixed(singleton);
        self
    }

    pub fn with_singleton_policy(mut self, policy: ItemSingletonPolicy) -> Self {
        self.singleton_policy = policy;
        self
    }

    pub fn with_blueprint_copy(mut self, runs_remaining: Option<i32>) -> Self {
        self.is_blueprint_copy = true;
        self.blueprint_runs_remaining = runs_remaining;
        self
    }

    pub fn with_merge_existing_stack(mut self, merge: bool) -> Self {
        self.merge_existing_stack = merge;
        self
    }
}

/// Result returned from a committed or in-snapshot item grant.
#[derive(Debug, Clone, PartialEq)]
pub struct GrantedItemOutcome {
    pub item: InventoryItemRecord,
    pub location_id: i64,
    pub flag: InventoryFlag,
    pub singleton: bool,
}

/// Resolves whether a granted item should be marked `singleton` under `policy`.
pub fn should_grant_as_singleton(
    sde: &SdeRegistry,
    type_id: u32,
    quantity: i32,
    policy: ItemSingletonPolicy,
) -> bool {
    match policy {
        ItemSingletonPolicy::Fixed(b) => b,
        ItemSingletonPolicy::ShipOnlyFromSde => {
            sde.types.get(&type_id).is_some_and(|t| t.category_id == 6)
        }
        ItemSingletonPolicy::AutoFromSde => sde.types.get(&type_id).is_some_and(|t| {
            t.category_id == 6 || (quantity == 1 && matches!(t.category_id, 7 | 32))
        }),
    }
}

/// Resolves the target `(location_id, flag)` for `char_id` inside `snap`.
pub fn resolve_character_item_destination_in_snap(
    snap: &WorldStateSnapshot,
    char_id: i64,
    policy: ItemDeliveryPolicy,
) -> Result<(i64, InventoryFlag), DomainError> {
    let ch = snap.characters.get(&char_id).ok_or_else(|| {
        DomainError::not_found("CharacterNotFound", format!("Character {char_id} not found"))
    })?;
    match policy {
        ItemDeliveryPolicy::DockedHangarOrActiveShipCargo => {
            if let Some(st_id) = ch.station_id.or(ch.structure_id).filter(|&id| id > 0) {
                Ok((st_id, InventoryFlag::Hangar))
            } else if ch.active_ship_id > 0 {
                Ok((ch.active_ship_id, InventoryFlag::Cargo))
            } else {
                Err(DomainError::precondition_failed(
                    "InvalidDestinationLocation",
                    format!("Character {char_id} has no docked hangar or active ship cargo hold"),
                ))
            }
        }
        ItemDeliveryPolicy::DockedOrHomeHangar => {
            if let Some(st_id) = ch
                .station_id
                .or(ch.structure_id)
                .or(Some(ch.home_station_id))
                .filter(|&id| id > 0)
            {
                Ok((st_id, InventoryFlag::Hangar))
            } else {
                Err(DomainError::precondition_failed(
                    "InvalidDestinationLocation",
                    format!("Character {char_id} has no station or home hangar"),
                ))
            }
        }
    }
}

fn map_mint_db_err(err: DbError) -> DomainError {
    match err {
        DbError::CharacterNotFound(id) => {
            DomainError::not_found("CharacterNotFound", format!("Character {id} not found"))
        }
        DbError::InvalidDestinationLocation { .. } | DbError::ValidationError(_) => {
            DomainError::invalid_input("ValidationError", err.to_string())
        }
        other => DomainError::internal("PersistenceFailure", other.to_string()),
    }
}

/// Core shared item grant primitive operating inside a caller's active snapshot transaction.
///
/// Both standalone GM/admin grants ([`grant_character_item_tx`]) and multi-mutation gameplay
/// transactions (redeem claims, crate loots, LP store, campaigns, operation rewards, datacore
/// redemptions) mutate inventory through this function and delegate persistence + `OnItemChange` /
/// `OnBlueprintsUpdated` emission to [`eve_db`] + commit delta projection.
pub fn grant_item_in_snap(
    sde: Option<&SdeRegistry>,
    snap: &mut WorldStateSnapshot,
    explicit_item_id: Option<i64>,
    req: GrantItemRequest,
) -> Result<GrantedItemOutcome, DomainError> {
    if req.quantity <= 0 {
        return Err(DomainError::invalid_input(
            "InvalidQuantity",
            "Quantity must be positive",
        ));
    }
    if req.require_sde_type {
        let sde_ref = sde.ok_or_else(|| {
            DomainError::internal("MissingSdeRegistry", "SDE registry is required for type validation")
        })?;
        if !sde_ref.types.contains_key(&req.type_id) {
            return Err(DomainError::not_found(
                "TypeNotFound",
                format!("Type ID {} not found in SDE", req.type_id),
            ));
        }
    }
    let singleton = match req.singleton_policy {
        ItemSingletonPolicy::Fixed(b) => b,
        policy => {
            let sde_ref = sde.ok_or_else(|| {
                DomainError::internal(
                    "MissingSdeRegistry",
                    "SDE registry is required for singleton policy",
                )
            })?;
            should_grant_as_singleton(sde_ref, req.type_id, req.quantity, policy)
        }
    };
    let (location_id, flag) = match req.destination {
        GrantItemDestination::Explicit { location_id, flag } => (location_id, flag),
        GrantItemDestination::CharacterPolicy(policy) => {
            let char_id = req.target_char_id.unwrap_or(req.owner_id);
            resolve_character_item_destination_in_snap(snap, char_id, policy)?
        }
    };
    let item = snap
        .mint_inventory_item_with_id_in_snap(
            explicit_item_id,
            MintItemSpec {
                type_id: req.type_id,
                owner_id: req.owner_id,
                location_id,
                flag,
                quantity: req.quantity,
                singleton,
                is_blueprint_copy: req.is_blueprint_copy,
                blueprint_runs_remaining: req.blueprint_runs_remaining,
                merge_existing_stack: req.merge_existing_stack,
            },
        )
        .map_err(map_mint_db_err)?;
    Ok(GrantedItemOutcome {
        item,
        location_id,
        flag,
        singleton,
    })
}

/// Validates inputs and atomically grants an inventory item to `target_char_id` inside a
/// `WorldRepository::tx` transaction without touching or waking solar system actors (`#1542`).
pub fn grant_character_item_tx(
    ctx: &ServerContext,
    tx_label: &'static str,
    target_char_id: i64,
    type_id: u32,
    quantity: i32,
    delivery_policy: ItemDeliveryPolicy,
    singleton_policy: ItemSingletonPolicy,
) -> Result<GrantedItemOutcome, DomainError> {
    if quantity <= 0 {
        return Err(DomainError::invalid_input(
            "InvalidQuantity",
            "Quantity must be positive",
        ));
    }
    if !ctx.sde.types.contains_key(&type_id) {
        return Err(DomainError::not_found(
            "TypeNotFound",
            format!("Type ID {type_id} not found in SDE"),
        ));
    }
    ctx.db.tx(tx_label, |wtx| {
        // Pre-validate character and destination before burning a durable entity ID.
        resolve_character_item_destination_in_snap(wtx.snap, target_char_id, delivery_policy)?;
        let item_id = wtx
            .alloc_entity_id()
            .map_err(<DomainError as eve_db::FromTxError>::from_tx_error)?;
        grant_item_in_snap(
            Some(&ctx.sde),
            wtx.snap,
            Some(item_id),
            GrantItemRequest {
                owner_id: target_char_id,
                target_char_id: Some(target_char_id),
                destination: GrantItemDestination::CharacterPolicy(delivery_policy),
                type_id,
                quantity,
                singleton_policy,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
                merge_existing_stack: false,
                require_sde_type: true,
            },
        )
    })
}
