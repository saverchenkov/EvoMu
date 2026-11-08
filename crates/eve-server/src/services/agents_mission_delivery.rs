//! Mission item delivery verification and consumption helpers (`#389` / `R6-T3-06`).

use eve_proto::BridgeRpcError;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldStateSnapshot};

/// Verifies and consumes mission completion delivery items.
///
/// For courier missions, only items created for the specific courier mission
/// (`courier_cargo:{char_id}:{mission_id}`) at the destination station are valid.
/// For non-courier item-delivery / trade / mining missions, contract-locked items,
/// industry-job-locked items, and borrowed courier mission items (`courier_cargo:*`)
/// are strictly excluded from eligibility and consumption.
#[allow(clippy::too_many_arguments)]
pub(crate) fn verify_and_consume_mission_delivery_items(
    snap: &mut WorldStateSnapshot,
    char_id: i64,
    mission_id: u32,
    req_tid: u32,
    req_units: i32,
    is_courier: bool,
    is_mining_mission: bool,
    current_station: Option<i64>,
    agent_station_id: Option<i64>,
    active_ship_id: Option<i64>,
    destination_station_id: Option<i64>,
    granted_courier_item_id: Option<i64>,
) -> Result<(), BridgeRpcError> {
    if is_courier {
        let dest_station = destination_station_id.ok_or_else(|| BridgeRpcError {
            code: "MissingDestinationStation".into(),
            message: format!("Courier mission {mission_id} has no valid destination station"),
        })?;
        if current_station != Some(dest_station) {
            return Err(BridgeRpcError {
                code: "CourierNotAtDestinationStation".into(),
                message: format!(
                    "Courier mission {mission_id} requires delivery to destination station {dest_station}, character is currently docked at {current_station:?}"
                ),
            });
        }
        let courier_tag = format!("courier_cargo:{char_id}:{mission_id}");
        let is_valid_courier_item = |item: &InventoryItemRecord| -> bool {
            item.owner_id == char_id
                && item.type_id == req_tid
                && item.quantity > 0
                && ((active_ship_id == Some(item.location_id)
                    && item.flag == InventoryFlag::Cargo)
                    || (item.location_id == dest_station && item.flag == InventoryFlag::Hangar))
                && snap
                    .item_custom_names
                    .get(&item.item_id)
                    .is_some_and(|s| s == &courier_tag)
                && granted_courier_item_id.is_some_and(|gid| item.item_id >= gid)
                && !snap.is_item_or_ancestor_locked_or_impounded(item.item_id)
        };
        let found_qty: i32 = snap
            .items
            .values()
            .filter(|it| is_valid_courier_item(it))
            .map(|it| it.quantity)
            .sum();
        if found_qty < req_units {
            return Err(BridgeRpcError {
                code: "MissionIncomplete".into(),
                message: format!(
                    "Courier mission {mission_id} requires delivering {req_units} units of type {req_tid} from ship cargo at destination station, character only has {found_qty}"
                ),
            });
        }
        let mut rem = req_units;
        let mut matching_ids: Vec<i64> = snap
            .items
            .values()
            .filter(|it| is_valid_courier_item(it))
            .map(|it| it.item_id)
            .collect();
        matching_ids.sort_unstable();
        for iid in matching_ids {
            if rem <= 0 {
                break;
            }
            let Some(avail) = snap.items.get(&iid).map(|it| it.quantity) else {
                continue;
            };
            let take = avail.min(rem);
            if take <= 0 {
                continue;
            }
            let opts = eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), char_id);
            snap.consume_exact_item_quantity(iid, take, opts)
                .map_err(|e| BridgeRpcError {
                    code: "InventoryError".into(),
                    message: e.to_string(),
                })?;
            rem -= take;
        }
        return Ok(());
    }

    if let Some(ast_id) = agent_station_id {
        if current_station != Some(ast_id) {
            return Err(BridgeRpcError {
                code: "MissionIncomplete".into(),
                message: format!(
                    "Mission {mission_id} delivery requires character to be docked at agent station {ast_id}, currently at {current_station:?}"
                ),
            });
        }
    }

    let is_valid_delivery_item = |item: &InventoryItemRecord| -> bool {
        item.owner_id == char_id
            && item.type_id == req_tid
            && item.quantity > 0
            && !snap.is_item_or_ancestor_locked_or_impounded(item.item_id)
            && !eve_db::WorldStateSnapshot::is_fitted_slot_flag(item.flag)
            && !snap.is_container_non_empty(item.item_id)
            && !snap
                .item_custom_names
                .get(&item.item_id)
                .is_some_and(|n| n.starts_with("courier_cargo:"))
            && ((item.flag == InventoryFlag::Hangar
                && agent_station_id.is_none_or(|st_id| item.location_id == st_id))
                || ((is_mining_mission || req_tid == 34)
                    && active_ship_id == Some(item.location_id)
                    && matches!(
                        item.flag,
                        InventoryFlag::SpecializedOreHold | InventoryFlag::Cargo
                    )))
    };

    let found_qty: i32 = snap
        .items
        .values()
        .filter(|it| is_valid_delivery_item(it))
        .map(|it| it.quantity)
        .sum();
    if found_qty < req_units {
        return Err(BridgeRpcError {
            code: "MissionIncomplete".into(),
            message: format!(
                "Mission {mission_id} requires delivering {req_units} units of type {req_tid} at agent station, character only has {found_qty}"
            ),
        });
    }

    let mut rem = req_units;
    let mut matching_ids: Vec<i64> = snap
        .items
        .values()
        .filter(|it| is_valid_delivery_item(it))
        .map(|it| it.item_id)
        .collect();
    matching_ids.sort_unstable();
    let delivery_opts = eve_db::ConsumeExactOptions {
        active_ship_id,
        ..eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), char_id)
    };
    for iid in matching_ids {
        if rem <= 0 {
            break;
        }
        let Some(avail) = snap.items.get(&iid).map(|it| it.quantity) else {
            continue;
        };
        let take = rem.min(avail);
        if take > 0 && snap.consume_exact_item_quantity(iid, take, delivery_opts).is_ok() {
            rem -= take;
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "agents_mission_delivery_cov_unit_tests.rs"]
mod agents_mission_delivery_cov_unit_tests;

