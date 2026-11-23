//! Compiled RPC registry table for inventory broker, ship cargo/fitting, and container ALSC handlers.

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};
use eve_sde::attr_ids;

use crate::router::rpc_registry::{rpc_entry, RpcBoxFuture, RpcDispatchHandler, RpcEntry};
use crate::router::{BoundObjectEntry, ServerContext};

#[inline]
pub(crate) fn resolve_launch_probes_alias<'a>(service: &str, method: &'a str) -> &'a str {
    if service == "dogmaIM" && method == "LaunchProbes" {
        "Activate"
    } else {
        method
    }
}

#[inline]
pub(crate) fn is_invbroker_or_ship_async(service: &str, method: &str) -> bool {
    matches!(
        (service, method),
        ("invbroker", "GetContainerContents" | "Add" | "Jettison" | "MultiAdd")
            | ("ship", "Jettison" | "Drop")
    )
}

#[inline(never)]
pub(crate) fn handle_invbroker_sync(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("invbroker", "GetItemDescriptor") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            Some(Ok(DBRowDescriptor::inv_item_header().to_pyrep()))
        }
        ("invbroker", "List") => {
            crate::services::station_structure::structure_upwell_ops::sync_auto_delivered_asset_safety_wraps(ctx);
            Some(crate::services::inventory_stacking::handle_list(
                ctx,
                session,
                bound_entry,
                args,
                kwargs,
            ))
        }
        ("invbroker", "ListByFlags") => {
            crate::services::station_structure::structure_upwell_ops::sync_auto_delivered_asset_safety_wraps(ctx);
            Some(crate::services::inventory_stacking::handle_list_by_flags(
                ctx,
                session,
                bound_entry,
                args,
                kwargs,
            ))
        }
        _ => None,
    }
}

#[inline(never)]
pub(crate) async fn handle_invbroker_and_ship_async(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("invbroker", "GetContainerContents") => Some(
            crate::router::box_fut(|| {
                crate::services::inventory_stacking::handle_get_container_contents(
                    ctx,
                    session,
                    bound_entry,
                    args,
                    kwargs,
                )
            })
            .await,
        ),
        ("invbroker", "Add") => {
            crate::router::box_fut(|| {
                crate::services::inventory_fitting::handle_invbroker_add(
                    ctx,
                    session,
                    bound_entry,
                    args,
                    kwargs,
                )
            })
            .await
        }
        ("ship", "Jettison") | ("invbroker", "Jettison") => Some(
            crate::router::box_fut(|| {
                crate::services::inventory_fitting::fitting_ship_bays::ship_jettison(
                    ctx, session, args, kwargs,
                )
            })
            .await,
        ),
        ("ship", "Drop") => Some(
            crate::router::box_fut(|| {
                crate::services::inventory_fitting::fitting_ship_bays::ship_drop(
                    ctx, session, args, kwargs,
                )
            })
            .await,
        ),
        ("invbroker", "MultiAdd") => Some(
            crate::router::box_fut(|| {
                crate::services::inventory_stacking::handle_multi_add(
                    ctx,
                    session,
                    bound_entry,
                    args,
                    kwargs,
                )
            })
            .await,
        ),
        _ => None,
    }
}

pub(crate) async fn handle_containers_ships_and_fitting(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    use crate::services::inventory_fitting::fitting_containers_ships as fcs;
    match (service, method) {
        ("invbroker", "ValidateItemCanBeOpened") => {
            fcs::handle_validate_item_can_be_opened(ctx, session, bound_entry, args, kwargs).await
        }
        ("invbroker", "ValidateItemListCanBeOpened") => {
            fcs::handle_validate_item_list_can_be_opened(ctx, session, args, kwargs).await
        }
        ("invbroker", "AssembleCargoContainer") => {
            fcs::handle_assemble_cargo_container(ctx, session, bound_entry, args)
        }
        ("invbroker", "SetPassword")
        | ("invbroker", "ALSCConfigSet")
        | ("invbroker", "ConfigureALSC") => Some(
            crate::services::notify_emitters_fleet_combat::handle_set_password(
                ctx,
                session,
                bound_entry,
                method,
                args,
                kwargs,
            )
            .await,
        ),
        ("invbroker", "ALSCConfigGet") => {
            fcs::handle_alsc_config_get(ctx, session, bound_entry, args, kwargs)
        }
        ("invbroker", "ALSCLockItems")
        | ("invbroker", "ALSCLock")
        | ("invbroker", "ALSCUnlockItems")
        | ("invbroker", "ALSCUnlock") => Some(fcs::handle_alsc_lock_unlock(
            ctx,
            session,
            bound_entry,
            method,
            args,
            kwargs,
        )),
        ("invbroker", "ALSCLogGet") => {
            fcs::handle_alsc_log_get(ctx, session, bound_entry, args, kwargs)
        }
        ("invbroker", "GetDamageForCrystals") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let entries: Vec<(PyRep, PyRep)> = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && ctx
                                .sde
                                .types
                                .get(&i.type_id)
                                // SDE `crystalsGetDamaged` (786): frequency/mining crystals that wear out.
                                .is_some_and(|t| {
                                    t.default_attributes.get(&786).is_some_and(|&v| v > 0.0)
                                })
                    })
                    .map(|i| {
                        let dmg = i
                            .dynamic_attributes
                            .get(&attr_ids::DAMAGE)
                            .copied()
                            .or_else(|| {
                                i.dynamic_attributes
                                    .get(&attr_ids::HP_STRUCT)
                                    .map(|&hp| (1.0 - hp).clamp(0.0, 1.0))
                            })
                            .unwrap_or(0.0);
                        (PyRep::Integer(i.item_id), PyRep::Real(dmg))
                    })
                    .collect()
            });
            Some(Ok(PyRep::Dict(entries)))
        }
        ("invbroker", "GetAllItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rpc = crate::router::RpcArgs::new(args, kwargs);
            let search_filter = rpc
                .get_raw(0, "search")
                .and_then(|v| v.as_str())
                .map(|s| s.to_lowercase());
            let loc_filter = kwargs.get("locationID").and_then(|v| v.as_i64());
            let items = ctx.db.get_items_for_character(char_id);
            let mut rows = Vec::new();
            let mut total_volume = 0.0;
            let mut total_value_isk = 0.0;
            let mut locations = HashSet::new();
            for item in items {
                if let Some(lf) = loc_filter {
                    if item.location_id != lf {
                        continue;
                    }
                }
                let type_info = ctx.sde.types.get(&item.type_id);
                if let Some(ref q) = search_filter {
                    let name_matches = type_info
                        .map(|t| t.name.to_lowercase().contains(q))
                        .unwrap_or(false);
                    if !name_matches {
                        continue;
                    }
                }
                locations.insert(item.location_id);
                let unit_vol = fcs::effective_item_unit_volume(&ctx.sde, &item);
                total_volume += unit_vol * (item.quantity as f64);
                total_value_isk +=
                    type_info.map(|t| t.base_price).unwrap_or(100.0) * (item.quantity as f64);
                rows.push(crate::services::inventory_fitting::item_to_packed_cells(
                    ctx, &item,
                ));
            }
            let count = rows.len() as i64;
            Some(Ok(PyRep::keyval([
                ("totalItems", PyRep::Integer(count)),
                ("locationCount", PyRep::Integer(locations.len() as i64)),
                ("totalVolumeM3", PyRep::Real(total_volume)),
                ("estimatedNetWorthISK", PyRep::Real(total_value_isk)),
                (
                    "items",
                    DBRowDescriptor::inv_item_header().build_crowset(rows),
                ),
            ])))
        }
        ("invbroker", "IsMyItemAtDockableLocation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let item_id = crate::router::RpcArgs::new(args, kwargs)
                .get_raw(0, "itemID")
                .and_then(|a| a.as_i64())
                .unwrap_or(0);
            let dock_id = session.station_id.or(session.structure_id);
            let at_dock = ctx.db.with_snapshot(|snap| {
                snap.items
                    .get(&item_id)
                    .is_some_and(|i| i.owner_id == char_id && Some(i.location_id) == dock_id)
            });
            Some(Ok(PyRep::Bool(at_dock)))
        }
        ("invbroker", "BreakPlasticWrap") => {
            fcs::handle_break_plastic_wrap(ctx, session, bound_entry, args)
        }
        ("invbroker", "ClearCharges") => Some(
            crate::services::inventory_fitting::fitting_containers_ships_ops::handle_clear_charges(
                ctx,
                session,
                bound_entry,
                args,
            )
            .await,
        ),
        ("invbroker", "DestroyFitting") => {
            crate::services::inventory_fitting::fitting_dogma_im::handle_dogma_im(
                ctx,
                session,
                None,
                "dogmaIM",
                "DestroyFitting",
                args,
                kwargs,
            )
            .await
        }
        _ => {
            crate::services::inventory_fitting::fitting_containers_ships_ops::handle_ext_tail(
                ctx,
                session,
                bound_entry,
                service,
                method,
                args,
                kwargs,
            )
            .await
        }
    }
}

fn dispatch_rpc<'a>(
    ctx: &'a ServerContext,
    session: &'a mut SessionState,
    bound_entry: Option<&'a BoundObjectEntry>,
    service: &'a str,
    method: &'a str,
    args: &'a [PyRep],
    kwargs: &'a HashMap<String, PyRep>,
) -> RpcBoxFuture<'a> {
    Box::pin(async move {
        if matches!((service, method), ("invbroker", "List" | "Add")) {
            if let Some(res) = crate::services::corp_registry_ops::handle_corp_hangar_invbroker(
                ctx,
                session,
                bound_entry,
                service,
                method,
                args,
                kwargs,
            )
            .await
            {
                return Some(res);
            }
        }
        if (service, method) == ("invbroker", "GetAllItems") {
            crate::services::station_structure::structure_upwell_ops::sync_asset_safety_wraps(ctx);
        }
        let res = if matches!(
            (service, method),
            (
                "invbroker",
                "List" | "ListByFlags" | "GetContainerContents" | "Add" | "Jettison" | "MultiAdd"
            ) | ("ship", "Jettison" | "Drop")
                | ("dogmaIM", "LaunchProbes")
        ) {
            crate::services::inventory_fitting::handle(
                ctx,
                session,
                bound_entry,
                service,
                method,
                args,
                kwargs,
            )
            .await
        } else {
            handle_containers_ships_and_fitting(
                ctx,
                session,
                bound_entry,
                service,
                method,
                args,
                kwargs,
            )
            .await
        };
        if matches!(res, Some(Ok(_))) {
            crate::services::character_skill_ops::post_inventory_fitting_hook(
                ctx,
                session,
                bound_entry,
                method,
                args,
            )
            .await;
        }
        res
    })
}

pub const RPCS: &[RpcEntry] = &[
    rpc_entry("invbroker", "List", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "ListByFlags", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "GetContainerContents", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "Add", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "Jettison", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "MultiAdd", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("ship", "Jettison", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("ship", "Drop", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("dogmaIM", "LaunchProbes", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "ValidateItemCanBeOpened", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "ValidateItemListCanBeOpened", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "AssembleCargoContainer", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "SetPassword", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "ALSCConfigSet", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "ConfigureALSC", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "ALSCConfigGet", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "ALSCLockItems", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "ALSCLock", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "ALSCUnlockItems", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "ALSCUnlock", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "ALSCLogGet", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "GetDamageForCrystals", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "GetAllItems", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "IsMyItemAtDockableLocation", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "BreakPlasticWrap", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "ClearCharges", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
    rpc_entry("invbroker", "DestroyFitting", RpcDispatchHandler::BoundAsync(dispatch_rpc)),
];
