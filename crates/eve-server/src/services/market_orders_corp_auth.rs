//! Corporate wallet, market order, bill payment, insurance, and hangar take-role authorization (`#489`).

#![forbid(unsafe_code)]

use eve_db::{corp_roles, InventoryFlag, InventoryItemRecord, WorldStateSnapshot};
use eve_proto::BridgeRpcError;

use crate::services::social_corp_fleet::social_corp_projects::effective_corp_member_roles;
use crate::services::station_structure::structure_upwell_ops::StructureExtendedState;

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

/// Evaluates matured base + location-specific corporate roles at `station_id` without re-locking `ctx.db`.
pub(crate) fn effective_corp_roles_at_station_in_snap(
    snap: &WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    station_id: i64,
    now_unix: u64,
) -> (bool, u64) {
    let eff =
        crate::esi::router::effective_esi_corp_role_breakdown(snap, corp_id, char_id, now_unix);
    if eff.is_ceo {
        return (true, !0u64);
    }
    let Some(corp) = snap.corporations.get(&corp_id) else {
        return (false, eff.roles);
    };
    let char_home = snap
        .characters
        .get(&char_id)
        .and_then(|ch| {
            ch.station_id
                .or(ch.structure_id)
                .or(Some(ch.home_station_id))
                .filter(|&id| id > 0)
        })
        .unwrap_or(0);
    let hq_station_id = corp.hq_station_id.unwrap_or(char_home);
    let base_id = snap
        .corp_governance
        .get(&corp_id)
        .and_then(|g| g.member_bases.get(&char_id).copied())
        .unwrap_or(0);
    let mut loc_roles = 0u64;
    if station_id > 0 {
        if station_id == hq_station_id {
            loc_roles |= eff.roles_at_hq;
        }
        if base_id > 0 && station_id == base_id {
            loc_roles |= eff.roles_at_base;
        }
        if station_id != hq_station_id && (base_id <= 0 || station_id != base_id) {
            loc_roles |= eff.roles_at_other;
        }
    }
    (false, eff.roles | loc_roles)
}

/// Enforces `is_ceo || ROLE_DIRECTOR || (ROLE_TRADER && ROLE_ACCOUNT_CAN_TAKE[div_idx])` on corporate market orders.
pub(crate) fn require_corp_market_order_role(
    snap: &WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    account_key: u16,
    now_unix: u64,
) -> Result<(), BridgeRpcError> {
    if !snap.corporations.contains_key(&corp_id) {
        return Err(err(
            "CorporationNotFound",
            format!("Corporation {corp_id} not found"),
        ));
    }
    let (is_ceo, eff_roles) = effective_corp_member_roles(snap, corp_id, char_id, 0, now_unix);
    let div_idx = usize::from(account_key.clamp(1000, 1006).saturating_sub(1000)).min(6);
    let take_bit = corp_roles::ROLE_ACCOUNT_CAN_TAKE[div_idx];
    let allowed = is_ceo
        || corp_roles::has_role(eff_roles, corp_roles::ROLE_DIRECTOR)
        || (corp_roles::has_role(eff_roles, corp_roles::ROLE_TRADER)
            && (eff_roles & take_bit) != 0);
    if !allowed {
        return Err(err(
            "CorpRoleRequired",
            "Character requires CEO, Director, or Trader + division Wallet Take role for corporate market orders",
        ));
    }
    Ok(())
}

/// Enforces `is_ceo || ROLE_DIRECTOR || (ROLE_ACCOUNTANT && ROLE_ACCOUNT_CAN_TAKE[div_idx])` on corporate spend endpoints.
pub(crate) fn require_corp_accountant_spend_role(
    snap: &WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    account_key: u16,
    now_unix: u64,
    denied_code: &str,
) -> Result<(), BridgeRpcError> {
    if !snap.corporations.contains_key(&corp_id) {
        return Err(err(
            "CorporationNotFound",
            format!("Corporation {corp_id} not found"),
        ));
    }
    let (is_ceo, eff_roles) = effective_corp_member_roles(snap, corp_id, char_id, 0, now_unix);
    let div_idx = usize::from(account_key.clamp(1000, 1006).saturating_sub(1000)).min(6);
    let take_bit = corp_roles::ROLE_ACCOUNT_CAN_TAKE[div_idx];
    let allowed = is_ceo
        || corp_roles::has_role(eff_roles, corp_roles::ROLE_DIRECTOR)
        || (corp_roles::has_role(eff_roles, corp_roles::ROLE_ACCOUNTANT)
            && (eff_roles & take_bit) != 0);
    if !allowed {
        return Err(err(
            denied_code,
            "Character requires CEO, Director, or Full Accountant + division Wallet Take role",
        ));
    }
    Ok(())
}

/// Validates whether `item` can be liquidated via `PlaceMultiSellOrder` (`#489`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn is_item_sellable_on_market(
    snap: &WorldStateSnapshot,
    ext: &StructureExtendedState,
    item: &InventoryItemRecord,
    char_id: i64,
    caller_corp_id: i64,
    use_corp: bool,
    station_id: i64,
    active_ship_id: Option<i64>,
    now_unix: u64,
) -> bool {
    is_item_sellable_on_market_inner(
        snap,
        ext,
        item,
        char_id,
        caller_corp_id,
        use_corp,
        station_id,
        active_ship_id,
        now_unix,
        true,
    )
}

/// Validates whether `item` is otherwise sellable in the caller's hangar before checking contents
/// so non-empty containers/ships yield `CannotSellItemWithContents` instead of missing-inventory (`#583`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn is_item_sellable_on_market_ignoring_contents(
    snap: &WorldStateSnapshot,
    ext: &StructureExtendedState,
    item: &InventoryItemRecord,
    char_id: i64,
    caller_corp_id: i64,
    use_corp: bool,
    station_id: i64,
    active_ship_id: Option<i64>,
    now_unix: u64,
) -> bool {
    is_item_sellable_on_market_inner(
        snap,
        ext,
        item,
        char_id,
        caller_corp_id,
        use_corp,
        station_id,
        active_ship_id,
        now_unix,
        false,
    )
}

#[allow(clippy::too_many_arguments)]
fn is_item_sellable_on_market_inner(
    snap: &WorldStateSnapshot,
    ext: &StructureExtendedState,
    item: &InventoryItemRecord,
    char_id: i64,
    caller_corp_id: i64,
    use_corp: bool,
    station_id: i64,
    active_ship_id: Option<i64>,
    now_unix: u64,
    require_empty: bool,
) -> bool {
    if snap.is_structure_controller_saved_ship(item.item_id) {
        return false;
    }
    let raw_flag = item.flag as u16;
    if matches!(item.flag, InventoryFlag::Locked | InventoryFlag::Impounded)
        || raw_flag == 36
        || item.type_id == 60
        || item.type_id == 3_468
        || snap.is_item_locked_in_contract_or_job(item.item_id)
    {
        return false;
    }
    let in_asset_safety = ext.asset_safety_wraps.values().any(|w| {
        w.wrap_id == item.item_id
            || (!w.retrieval_fee_paid
                && (w.wrap_id == item.location_id || w.item_ids.contains(&item.item_id)))
    });
    if in_asset_safety {
        return false;
    }
    let consumable = if require_empty {
        snap.is_item_consumable_by(item, char_id, Some(station_id), active_ship_id)
    } else {
        snap.is_item_consumable_by_ignoring_contents(item, char_id, Some(station_id), active_ship_id)
    };
    if consumable {
        return true;
    }
    if !use_corp || caller_corp_id == 0 || item.owner_id != caller_corp_id || item.location_id != station_id {
        return false;
    }
    let (is_ceo, eff_roles) =
        effective_corp_roles_at_station_in_snap(snap, caller_corp_id, char_id, station_id, now_unix);
    let is_director = is_ceo || corp_roles::has_role(eff_roles, corp_roles::ROLE_DIRECTOR);
    match item.flag {
        InventoryFlag::CorpDeliveries | InventoryFlag::Deliveries => {
            is_director
                || corp_roles::has_role(eff_roles, corp_roles::ROLE_DELIVERIES_HANGAR_TAKE)
        }
        InventoryFlag::Hangar | InventoryFlag::CorpSAG1 => {
            is_director || corp_roles::has_role(eff_roles, corp_roles::ROLE_HANGAR_CAN_TAKE[0])
        }
        InventoryFlag::CorpSAG2
        | InventoryFlag::CorpSAG3
        | InventoryFlag::CorpSAG4
        | InventoryFlag::CorpSAG5
        | InventoryFlag::CorpSAG6
        | InventoryFlag::CorpSAG7 => {
            let idx = (raw_flag as usize).saturating_sub(116).min(6);
            is_director || corp_roles::has_role(eff_roles, corp_roles::ROLE_HANGAR_CAN_TAKE[idx])
        }
        _ => false,
    }
}

#[cfg(test)]
#[path = "market_orders_corp_auth_edges_tests.rs"]
mod market_orders_corp_auth_edges_tests;
