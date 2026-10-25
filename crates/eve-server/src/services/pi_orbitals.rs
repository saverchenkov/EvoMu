//! `pi_orbitals` — Colony extraction/flush, processor & expedited transfers, orbital launch, POCO & Skyhook customs taxes (`E-6`).

// parity: plan:E-6
// parity: feat:pi.expedited-transfer, feat:pi.launch, feat:pi.customs-office
// parity: feat:pi.poco-settings, feat:pi.skyhook-pi, feat:pi.storage-overflow

#![forbid(unsafe_code)]

use crate::formulas::{compute_pi_link_cost, compute_poco_export_tax};
use crate::router::ServerContext;
use crate::services::industry_pi::resolve_sde_planet_p0_extraction;
pub use crate::services::industry_pi::pi_helpers::*;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

pub fn handle_pi_orbitals(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("planetMgr", "Flush") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if args.len() > 2 {
                return Some(Err(BridgeRpcError {
                    code: "ClientSchematicOverrideForbidden".into(),
                    message: "Flush accepts at most (planetID, [planetHarvesterOrResourceTypeID])"
                        .into(),
                }));
            }
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "Flush requires an explicit integer planetID".into(),
                }));
            };
            let requested_resource_or_pin = match args.get(1) {
                None => None,
                Some(arg) => {
                    let Some(raw_id) = arg
                        .as_i64()
                        .filter(|&id| id > 0 && id <= i64::from(u32::MAX))
                        .map(|id| id as u32)
                        .filter(|id| {
                            ctx.sde
                                .types
                                .get(id)
                                .map(|t| {
                                    t.group_id == 1026 || matches!(t.group_id, 1032 | 1033 | 1035)
                                })
                                .unwrap_or(false)
                        })
                    else {
                        return Some(Err(BridgeRpcError {
                            code: "ClientSchematicOverrideForbidden".into(),
                            message: "Flush optional second argument must be a valid planet extractor pin or raw P0 resource typeID".into(),
                        }));
                    };
                    Some(raw_id)
                }
            };
            let Some((delivery_loc_id, delivery_flag)) = session
                .station_id
                .or(session.structure_id)
                .map(|loc| (loc, InventoryFlag::Hangar))
                .or_else(|| session.ship_id.map(|ship| (ship, InventoryFlag::Cargo)))
            else {
                return Some(Err(BridgeRpcError {
                    code: "NoDeliveryLocation".into(),
                    message: "Flushing planetary colony requires an active station, structure, or ship in the colony system".into(),
                }));
            };
            let current_sys_id = session.solar_system_id;
            let (link_pg, link_cpu) = compute_pi_link_cost(150.0, 1);
            let now_unix = ctx.now_unix();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (_cc_level, pins_count, col_sys_id) = {
                    let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
                        return Err(BridgeRpcError {
                            code: "ColonyNotFound".into(),
                            message: format!(
                                "No planetary colony found for character {char_id} on planet {planet_id}"
                            ),
                        });
                    };
                    if current_sys_id != Some(col.solar_system_id) {
                        return Err(BridgeRpcError {
                            code: "ColonyNotInCurrentSystem".into(),
                            message: format!(
                                "Planetary colony {planet_id} is in solar system {}, not {:?}",
                                col.solar_system_id, current_sys_id
                            ),
                        });
                    }
                    (
                        col.command_center_level,
                        col.pins_count,
                        col.solar_system_id,
                    )
                };

                let (p0_type_id, cycle_secs, p0_qty_per_pin, p0_unit_tax_value) =
                    resolve_sde_planet_p0_extraction(
                        ctx,
                        col_sys_id,
                        planet_id,
                        requested_resource_or_pin,
                    )?;
                let extractor_pins = pins_count.saturating_sub(1).max(1);
                let produced_qty = p0_qty_per_pin.saturating_mul(u64::from(extractor_pins));
                let produced_qty_i32 =
                    i32::try_from(produced_qty).map_err(|_| BridgeRpcError {
                        code: "QuantityOverflow".into(),
                        message: format!(
                            "Extracted quantity {produced_qty} overflows i32 inventory stack"
                        ),
                    })?;

                {
                    let col = snap
                        .planetary_colonies
                        .get(&(char_id, planet_id))
                        .expect("colony checked above");
                    if col.last_extracted_unix > 0
                        && now_unix < col.last_extracted_unix.saturating_add(cycle_secs)
                    {
                        return Err(BridgeRpcError {
                            code: "ExtractorCycleNotReady".into(),
                            message: format!(
                                "Extractor cycle on planet {planet_id} requires {cycle_secs}s between flushes"
                            ),
                        });
                    }
                }

                let (player_poco_rate, _) =
                    resolve_character_orbital_access_and_tax(snap, char_id, planet_id)?;
                let customs_code_expertise_level = snap
                    .skills
                    .get(&char_id)
                    .and_then(|m| m.get(&33467))
                    .map(|s| s.level.clamp(0, 5))
                    .unwrap_or(0);
                let npc_customs_tax_rate = if ctx
                    .sde
                    .solar_systems
                    .get(&col_sys_id)
                    .map(|s| s.security_status >= 0.45)
                    .unwrap_or(true)
                {
                    0.10
                } else {
                    0.0
                };
                let tax_base = p0_unit_tax_value * (produced_qty as f64);
                let export_tax = compute_poco_export_tax(
                    tax_base,
                    npc_customs_tax_rate,
                    customs_code_expertise_level,
                    player_poco_rate,
                );
                let tax_cents = IskCents::from_f64_isk(export_tax);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    let new_bal =
                        ch.balance
                            .checked_sub(tax_cents)
                            .ok_or_else(|| BridgeRpcError {
                                code: "InsufficientFunds".into(),
                                message: format!(
                                    "Insufficient ISK to pay POCO export tax ({export_tax:.2} ISK)"
                                ),
                            })?;
                    ch.balance = new_bal;
                }

                let mut net = load_pi_network(snap, char_id, planet_id, _cc_level);
                let ext_idx = net.pins.iter().position(|p| {
                    p.type_id == 2848
                        || p.type_id == 3060
                        || ctx.sde.types.get(&p.type_id).map(|t| t.group_id == 1026).unwrap_or(false)
                }).or_else(|| net.pins.iter().position(|p| is_storage_or_cc_or_launchpad_pin(&ctx.sde, p.type_id)));
                if let Some(idx) = ext_idx {
                    *net.pins[idx].contents.entry(p0_type_id).or_insert(0) += produced_qty;
                    save_pi_network(snap, char_id, planet_id, &net);
                }

                let col = snap
                    .planetary_colonies
                    .get_mut(&(char_id, planet_id))
                    .expect("colony checked above");
                col.last_extracted_unix = now_unix;
                let total_stored = col.extracted_commodities.get(&p0_type_id).copied().unwrap_or(0);

                let existing_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.location_id == delivery_loc_id
                            && i.flag == delivery_flag
                            && i.type_id == p0_type_id
                    })
                    .map(|i| i.item_id);
                let delivered_item_id = if let Some(iid) = existing_item_id {
                    if let Some(item_mut) = snap.items.get_mut(&iid) {
                        item_mut.quantity = item_mut.quantity.saturating_add(produced_qty_i32);
                        item_mut.version += 1;
                    }
                    iid
                } else {
                    snap.next_entity_id += 1;
                    let iid = snap.next_entity_id;
                    snap.items.insert(
                        iid,
                        InventoryItemRecord {
                            item_id: iid,
                            type_id: p0_type_id,
                            owner_id: char_id,
                            location_id: delivery_loc_id,
                            flag: delivery_flag,
                            quantity: produced_qty_i32,
                            singleton: false,
                            version: 1,
                            dynamic_attributes: HashMap::new(),
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        },
                    );
                    iid
                };
                Ok((
                    p0_type_id,
                    produced_qty,
                    total_stored,
                    delivered_item_id,
                    export_tax,
                    cycle_secs,
                ))
            });

            match res {
                Ok((
                    p0_type_id,
                    produced_qty,
                    total_stored,
                    delivered_item_id,
                    export_tax,
                    cycle_secs,
                )) => Some(Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)),
                    ("cycleTimeSeconds", PyRep::Integer(cycle_secs as i64)),
                    ("producedTypeID", PyRep::Integer(p0_type_id as i64)),
                    ("producedQuantity", PyRep::Integer(produced_qty as i64)),
                    ("totalStoredQuantity", PyRep::Integer(total_stored as i64)),
                    ("deliveredItemID", PyRep::Integer(delivered_item_id)),
                    ("linkPowergrid", PyRep::Real(link_pg)),
                    ("linkCPU", PyRep::Real(link_cpu)),
                    ("pocoExportTaxISK", PyRep::Real(export_tax)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("planetMgr", "UpgradeCommandCenter") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "UpgradeCommandCenter requires an explicit integer planetID".into(),
                }));
            };
            let Some(target_level_raw) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidCommandCenterLevel".into(),
                    message: "UpgradeCommandCenter requires target upgrade level (1..=5)".into(),
                }));
            };
            if !(1..=5).contains(&target_level_raw) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidCommandCenterLevel".into(),
                    message: format!("Target level {target_level_raw} must be in range 1..=5"),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                upgrade_command_center_logic(snap, char_id, planet_id, target_level_raw as u8)
            });
            Some(res)
        }

        // Processor cycles, storage overflow, and Expedited Transfer (`feat:pi.factory-cycles`, `feat:pi.storage-overflow`, `feat:pi.expedited-transfer`)
        ("planetMgr", "UserTransferCommodities") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "UserTransferCommodities requires an explicit integer planetID".into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                user_transfer_commodities_logic(ctx, snap, char_id, planet_id, args, kwargs)
            });
            Some(res)
        }

        // Launchpad export/import via Customs Office or Skyhook, or Command Center 500 ISK launch (`feat:pi.launch`, `feat:pi.customs-office`, `feat:pi.skyhook-pi`)
        ("planetMgr", "UserLaunchCommodities") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "UserLaunchCommodities requires planetID".into(),
                }));
            };
            let (launch_pin_id_opt, type_id, qty, destination) = if args.len() >= 4 && args.get(3).and_then(|a| a.as_i64()).is_some() {
                let pid = args.get(1).and_then(|a| a.as_i64());
                let tid = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0) as TypeId;
                let q = args.get(3).and_then(|a| a.as_i64()).unwrap_or(0).max(1) as u64;
                (pid, tid, q, "CommandCenterLaunch")
            } else {
                let tid = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0) as TypeId;
                let q = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0).max(1) as u64;
                let dest = args.get(3).and_then(|a| a.as_str()).unwrap_or("Storage");
                (None, tid, q, dest)
            };
            if type_id == 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidRoutingParameters".into(),
                    message: "UserLaunchCommodities requires commodity typeID".into(),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!(
                            "No colony found for character {char_id} on planet {planet_id}"
                        ),
                    });
                };

                let unit_vol = ctx
                    .sde
                    .types
                    .get(&type_id)
                    .map(|t| t.volume.max(0.005))
                    .unwrap_or(0.38);
                let total_vol_m3 = unit_vol * (qty as f64);
                let unit_base = ctx
                    .sde
                    .types
                    .get(&type_id)
                    .map(|t| {
                        t.default_attributes
                            .get(&1641)
                            .or_else(|| t.default_attributes.get(&1640))
                            .copied()
                            .filter(|&v| v > 0.0)
                            .unwrap_or_else(|| t.base_price.max(400.0))
                    })
                    .unwrap_or(400.0);

                if destination == "ImportToColony" || destination == "Import" {
                    let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                    let target_idx = launch_pin_id_opt
                        .and_then(|pid| net.pins.iter().position(|p| p.pin_id == pid))
                        .or_else(|| net.pins.iter().position(|p| is_launchpad_pin(&ctx.sde, p.type_id)))
                        .or_else(|| net.pins.iter().position(|p| is_storage_pin(&ctx.sde, p.type_id)))
                        .or_else(|| net.pins.iter().position(|p| is_command_center_pin(&ctx.sde, p.type_id)))
                        .ok_or_else(|| BridgeRpcError {
                            code: "PinNotFound".into(),
                            message: format!("No target pin found for import on planet {planet_id}"),
                        })?;

                    let target_cap_m3 = pin_storage_capacity_m3(&ctx.sde, net.pins[target_idx].type_id);
                    let target_curr_vol = pin_contents_volume_m3(&ctx.sde, &net.pins[target_idx]);
                    if target_curr_vol + total_vol_m3 > target_cap_m3 + 1e-6 {
                        return Err(BridgeRpcError {
                            code: "LaunchpadCapacityExceeded".into(),
                            message: format!(
                                "Import volume {total_vol_m3:.1} m3 exceeds pin available capacity ({target_cap_m3:.0} m3)"
                            ),
                        });
                    }

                    consume_pi_import_commodities(
                        snap,
                        char_id,
                        planet_id,
                        type_id,
                        qty,
                        session.ship_id,
                        session.station_id,
                        session.structure_id,
                    )?;
                    let (player_poco_rate, orbital_tid) = resolve_character_orbital_access_and_tax(snap, char_id, planet_id)?;
                    let customs_lvl = snap.skills.get(&char_id).and_then(|m| m.get(&33467)).map(|s| s.level.clamp(0, 5)).unwrap_or(0);
                    let npc_customs_tax_rate = if ctx.sde.solar_systems.get(&col.solar_system_id).map(|s| s.security_status >= 0.45).unwrap_or(true) { 0.10 } else { 0.0 };
                    let import_tax_isk = 0.5 * compute_poco_export_tax(unit_base * (qty as f64), npc_customs_tax_rate, customs_lvl, player_poco_rate);
                    let tax_cents = IskCents::from_f64_isk(import_tax_isk);
                    if tax_cents.0 > 0 {
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            let new_bal = ch.balance.checked_sub(tax_cents).ok_or_else(|| BridgeRpcError {
                                code: "InsufficientFunds".into(),
                                message: format!("Insufficient ISK to pay POCO import tax ({import_tax_isk:.2} ISK)"),
                            })?;
                            ch.balance = new_bal;
                            let bal_after = ch.balance;
                            snap.append_journal(
                                char_id,
                                "CustomOfficeTax",
                                tax_cents,
                                bal_after,
                                format!(
                                    "POCO import tax for {qty}x type {type_id} on planet {planet_id}"
                                ),
                            );
                        }
                        if let Some(cfg) = load_orbital_settings(snap, planet_id) {
                            let player_share = IskCents::from_f64_isk(0.5 * unit_base * (qty as f64) * player_poco_rate);
                            if let Some(corp) = snap.corporations.get_mut(&cfg.owner_corp_id) {
                                corp.master_wallet = corp.master_wallet.checked_add(player_share).unwrap_or(corp.master_wallet);
                            }
                        }
                    }

                    *net.pins[target_idx].contents.entry(type_id).or_insert(0) += qty;
                    save_pi_network(snap, char_id, planet_id, &net);

                    return Ok((qty, import_tax_isk, import_tax_isk, orbital_tid));
                }

                let cc_lvl = col.command_center_level;
                let mut net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                let launch_pin_idx = if let Some(pid) = launch_pin_id_opt {
                    net.pins.iter().position(|p| p.pin_id == pid).ok_or_else(|| BridgeRpcError {
                        code: "PinNotFound".into(),
                        message: format!("Launching pin {pid} not found on planet {planet_id}"),
                    })?
                } else {
                    net.pins
                        .iter()
                        .position(|p| is_storage_or_cc_or_launchpad_pin(&ctx.sde, p.type_id) && p.contents.get(&type_id).copied().unwrap_or(0) >= qty)
                        .or_else(|| net.pins.iter().position(|p| p.contents.get(&type_id).copied().unwrap_or(0) >= qty))
                        .or_else(|| net.pins.iter().position(|p| is_storage_or_cc_or_launchpad_pin(&ctx.sde, p.type_id)))
                        .ok_or_else(|| BridgeRpcError {
                            code: "PinNotFound".into(),
                            message: format!("No launching pin found on planet {planet_id}"),
                        })?
                };

                let mut export_tax_isk = 0.0_f64;
                let mut orbital_tid: TypeId = 2233;
                let is_cc = destination == "CommandCenterLaunch" || destination == "CommandCenter";
                if destination == "CustomsOffice" || destination == "Skyhook" || is_cc {
                    let max_vol = if is_cc { 500.0 } else { 10_000.0 };
                    if total_vol_m3 > max_vol + 1e-6 {
                        let code = if is_cc { "CommandCenterLaunchCapacityExceeded" } else { "LaunchpadCapacityExceeded" };
                        return Err(BridgeRpcError {
                            code: code.into(),
                            message: format!("Transfer volume {total_vol_m3:.1} m3 exceeds {max_vol:.0} m3 limit for {destination}"),
                        });
                    }
                }

                let launch_pin_id = net.pins[launch_pin_idx].pin_id;
                let available = net.pins[launch_pin_idx].contents.get(&type_id).copied().unwrap_or(0);
                if available < qty {
                    return Err(BridgeRpcError {
                        code: "InsufficientCommoditiesForRouting".into(),
                        message: format!(
                            "Launching pin {launch_pin_id} has {available} of type {type_id}, cannot route {qty}"
                        ),
                    });
                }

                if destination == "CustomsOffice" || destination == "Skyhook" || is_cc {
                    let (player_poco_rate, otid) = resolve_character_orbital_access_and_tax(snap, char_id, planet_id)?;
                    orbital_tid = otid;
                    let customs_lvl = snap.skills.get(&char_id).and_then(|m| m.get(&33467)).map(|s| s.level.clamp(0, 5)).unwrap_or(0);
                    let npc_customs_tax_rate = if ctx.sde.solar_systems.get(&col.solar_system_id).map(|s| s.security_status >= 0.45).unwrap_or(true) { 0.10 } else { 0.0 };
                    let tax_mult = if is_cc { 1.5 } else { 1.0 };
                    export_tax_isk = tax_mult * compute_poco_export_tax(unit_base * (qty as f64), npc_customs_tax_rate, customs_lvl, player_poco_rate);
                    let tax_cents = IskCents::from_f64_isk(export_tax_isk);
                    if tax_cents.0 > 0 {
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.balance = ch.balance.checked_sub(tax_cents).ok_or_else(|| BridgeRpcError {
                                code: "InsufficientFunds".into(),
                                message: format!("Insufficient ISK to pay POCO export tax ({export_tax_isk:.2} ISK)"),
                            })?;
                            let bal_after = ch.balance;
                            snap.append_journal(char_id, "CustomOfficeTax", tax_cents, bal_after, format!("POCO export tax for {qty}x type {type_id} on planet {planet_id}"));
                        }
                        if let Some(cfg) = load_orbital_settings(snap, planet_id) {
                            let player_share = IskCents::from_f64_isk(tax_mult * unit_base * (qty as f64) * player_poco_rate);
                            if let Some(corp) = snap.corporations.get_mut(&cfg.owner_corp_id) {
                                corp.master_wallet = corp.master_wallet.checked_add(player_share).unwrap_or(corp.master_wallet);
                            }
                        }
                    }
                }
                if matches!(
                    destination,
                    "Hangar" | "CustomsOffice" | "Skyhook" | "CommandCenterLaunch" | "CommandCenter"
                ) {
                    if let Some(c) = net.pins[launch_pin_idx].contents.get_mut(&type_id) {
                        if *c <= qty {
                            net.pins[launch_pin_idx].contents.remove(&type_id);
                        } else {
                            *c -= qty;
                        }
                    }
                    save_pi_network(snap, char_id, planet_id, &net);
                    if let Some(col_mut) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
                        if let Some(c) = col_mut.extracted_commodities.get_mut(&type_id) {
                            if *c <= qty {
                                col_mut.extracted_commodities.remove(&type_id);
                            } else {
                                *c -= qty;
                            }
                        }
                    }
                    let delivery_loc = session
                        .station_id
                        .or(session.structure_id)
                        .unwrap_or(col.planet_id);
                    snap.next_entity_id += 1;
                    let nid = snap.next_entity_id;
                    snap.items.insert(
                        nid,
                        InventoryItemRecord {
                            item_id: nid,
                            type_id,
                            owner_id: char_id,
                            location_id: delivery_loc,
                            flag: InventoryFlag::Hangar,
                            quantity: qty as i32,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                }
                Ok((qty, export_tax_isk, 0.0, orbital_tid))
            });

            match res {
                Ok((routed, export_tax_isk, import_tax_isk, orbital_tid)) => {
                    Some(Ok(PyRep::keyval([
                        ("planetID", PyRep::Integer(planet_id)),
                        ("orbitalTypeID", PyRep::Integer(orbital_tid as i64)),
                        ("commodityTypeID", PyRep::Integer(type_id as i64)),
                        ("routedQuantity", PyRep::Integer(routed as i64)),
                        ("destination", PyRep::String(destination.to_string())),
                        ("exportTaxISK", PyRep::Real(export_tax_isk)),
                        ("importTaxISK", PyRep::Real(import_tax_isk)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("planetMgr", "GetMyLaunchesDetails") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let launches = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|it| {
                        it.owner_id == char_id && (40_000_000..50_000_000).contains(&it.location_id)
                    })
                    .map(|it| {
                        PyRep::keyval([
                            ("launchID", PyRep::Integer(it.item_id)),
                            ("planetID", PyRep::Integer(it.location_id)),
                            ("typeID", PyRep::Integer(it.type_id as i64)),
                            ("quantity", PyRep::Integer(it.quantity as i64)),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(launches)))
        }

        ("planetMgr", "DeleteLaunch") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let launch_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(it) = snap.items.get(&launch_id) {
                    if it.owner_id == char_id {
                        snap.items.remove(&launch_id);
                    }
                }
            });
            Some(Ok(PyRep::Bool(true)))
        }

        ("planetMgr", "CancelInstallProgram") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let pin_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let ok = ctx.db.with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
                    return false;
                };
                let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                let idx = net.pins.iter().position(|p| p.pin_id == pin_id)
                    .or_else(|| net.pins.iter().position(|p| p.extractor_head_count > 0 || p.type_id == 2848 || ctx.sde.types.get(&p.type_id).map(|t| t.group_id == 1026 || t.group_id == 1063).unwrap_or(false)))
                    .or_else(|| if !net.pins.is_empty() { Some(0) } else { None });
                if let Some(i) = idx {
                    let pin = &mut net.pins[i];
                    pin.extractor_resource_type_id = None;
                    pin.extractor_heads.clear();
                    pin.extractor_head_count = 0;
                    pin.extractor_head_radius = 0.0;
                    pin.extractor_cycle_seconds = 0;
                    pin.program_install_unix = 0;
                    save_pi_network(snap, char_id, planet_id, &net);
                }
                true
            });
            Some(Ok(PyRep::Bool(ok)))
        }

        ("planetMgr", "RevertChanges") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(planet_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "RevertChanges requires planetID".into(),
                }));
            };
            let res = ctx.db.with_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!("No colony found for character {char_id} on planet {planet_id}"),
                    });
                };
                let _net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                Ok(PyRep::Bool(true))
            });
            Some(res)
        }

        ("planetMgr", "StartTicking") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now_unix = ctx.now_unix();
            let ok = ctx.db.with_mut_snapshot(|snap| {
                let Some(mut col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                    return false;
                };
                let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                let elapsed = now_unix.saturating_sub(col.last_extracted_unix).max(300);
                advance_colony_simulation(ctx, &mut col, &mut net, elapsed, now_unix);
                col.last_extracted_unix = now_unix;
                snap.planetary_colonies.insert((char_id, planet_id), col);
                save_pi_network(snap, char_id, planet_id, &net);
                true
            });
            Some(Ok(PyRep::Bool(ok)))
        }

        ("planetMgr", "GetProgramResultInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let pin_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.with_snapshot(|snap| {
                let cc_lvl = snap.planetary_colonies.get(&(char_id, planet_id)).map(|c| c.command_center_level).unwrap_or(0);
                let net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                let pin_opt = net.pins.iter().find(|p| p.pin_id == pin_id);
                let heads = pin_opt.map(|p| p.extractor_head_count.max(1)).unwrap_or(1);
                let head_radius = args.get(4).and_then(|a| a.as_f64()).or_else(|| pin_opt.map(|p| p.extractor_head_radius)).filter(|&r| r > 0.0).unwrap_or(0.02);
                let empty = Vec::new();
                let head_coords = pin_opt.map(|p| &p.extractor_heads).unwrap_or(&empty);
                let overlap = crate::services::industry_pi::extractor_overlap_factor(head_coords, head_radius);
                let cyc_secs = pin_opt.map(|p| p.extractor_cycle_seconds.max(300)).unwrap_or(300);
                let mut cycle_values = Vec::with_capacity(10);
                for c in 0..10 {
                    let y = (1_000.0 * (heads as f64) * overlap * (-0.015 * (c as f64)).exp()).round() as i64;
                    cycle_values.push(PyRep::Integer(y));
                }
                PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)),
                    ("pinID", PyRep::Integer(pin_id)),
                    ("cycleTimeSeconds", PyRep::Integer(cyc_secs as i64)),
                    ("headCount", PyRep::Integer(heads as i64)),
                    ("cycleYields", PyRep::List(cycle_values)),
                ])
            });
            Some(Ok(res))
        }

        ("planetMgr", "GetLocalDistributionReport") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let pin_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let (pins_count, report_dict) = ctx.db.with_snapshot(|snap| {
                let p_cnt = snap.planetary_colonies.get(&(char_id, planet_id)).map(|c| c.pins_count as i64).unwrap_or(0);
                let sys_id = ctx.sde.solar_systems.values().find(|s| s.planet_ids.contains(&planet_id)).map(|s| s.solar_system_id).unwrap_or(30000142);
                let p0_res = crate::services::industry_pi::list_sde_planet_p0_resources(ctx, sys_id, planet_id);
                let dist: Vec<(PyRep, PyRep)> = p0_res.into_iter().map(|tid| {
                    let richness = ((planet_id as u64).wrapping_mul(31).wrapping_add((tid as u64).wrapping_mul(17)) % 100) as f64 / 100.0;
                    (PyRep::Integer(tid as i64), PyRep::Real(richness))
                }).collect();
                (p_cnt, PyRep::Dict(dist))
            });
            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("pinID", PyRep::Integer(pin_id)),
                ("characterID", PyRep::Integer(char_id)),
                ("pinsCount", PyRep::Integer(pins_count)),
                ("report", report_dict),
            ])))
        }

        ("planetMgr", "GMAddCommodity") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMAddCommodity requires GM role".into(),
                }));
            }
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let (pin_id, type_id, qty) = if args.len() >= 4 {
                (
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                    args.get(2).and_then(|a| a.as_i64()).unwrap_or(0) as TypeId,
                    args.get(3).and_then(|a| a.as_i64()).unwrap_or(0),
                )
            } else {
                (
                    0,
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(0) as TypeId,
                    args.get(2).and_then(|a| a.as_i64()).unwrap_or(0),
                )
            };
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(col) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
                    *col.extracted_commodities.entry(type_id).or_insert(0) += qty.max(0) as u64;
                    let cc_lvl = col.command_center_level;
                    let mut net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                    let target = if pin_id > 0 {
                        net.pins.iter_mut().find(|p| p.pin_id == pin_id)
                    } else {
                        net.pins.first_mut()
                    };
                    if let Some(p) = target {
                        *p.contents.entry(type_id).or_insert(0) += qty.max(0) as u64;
                        save_pi_network(snap, char_id, planet_id, &net);
                    }
                }
                let loc_id = if pin_id > 0 { pin_id } else { planet_id };
                let existing = snap.items.values_mut().find(|it| it.owner_id == char_id && it.location_id == loc_id && it.type_id == type_id);
                if let Some(it) = existing {
                    it.quantity = it.quantity.saturating_add(qty as i32);
                    it.version += 1;
                } else {
                    snap.next_entity_id += 1;
                    let iid = snap.next_entity_id;
                    snap.items.insert(iid, InventoryItemRecord {
                        item_id: iid,
                        type_id,
                        owner_id: char_id,
                        location_id: loc_id,
                        flag: InventoryFlag::None,
                        quantity: qty as i32,
                        singleton: false,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    });
                }
            });
            Some(Ok(PyRep::Bool(true)))
        }

        ("planetMgr", "GMConvertCommandCenter") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMConvertCommandCenter requires GM role".into(),
                }));
            }
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let (target_type_id, new_level) = if args.len() >= 3 {
                (
                    args.get(1).and_then(|a| a.as_i64()).map(|v| v as TypeId),
                    args.get(2).and_then(|a| a.as_i64()).unwrap_or(5) as u8,
                )
            } else {
                (
                    None,
                    args.get(1).and_then(|a| a.as_i64()).unwrap_or(5) as u8,
                )
            };
            let ok = ctx.db.with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) else {
                    return false;
                };
                col.command_center_level = new_level;
                let mut net = load_pi_network(snap, char_id, planet_id, new_level);
                let (cpu, pg) = cc_level_cpu_pg(new_level);
                net.cpu_max = cpu;
                net.pg_max = pg;
                if let Some(ttid) = target_type_id {
                    if let Some(cc_pin) = net.pins.iter_mut().find(|p| is_command_center_type(ctx, p.type_id)) {
                        cc_pin.type_id = ttid;
                    }
                }
                save_pi_network(snap, char_id, planet_id, &net);
                true
            });
            Some(Ok(PyRep::Bool(ok)))
        }

        ("planetMgr", "GMForceInstallProgram") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMForceInstallProgram requires GM role".into(),
                }));
            }
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let pin_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let res_tid = args.get(2).and_then(|a| a.as_i64()).unwrap_or(2268) as TypeId;
            let heads_cnt = args.get(3).and_then(|a| a.as_i64()).unwrap_or(3) as u8;
            let now_unix = ctx.now_unix();
            let ok = ctx.db.with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
                    return false;
                };
                let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                let idx = net.pins.iter().position(|p| p.pin_id == pin_id)
                    .or_else(|| net.pins.iter().position(|p| p.type_id == 2848 || ctx.sde.types.get(&p.type_id).map(|t| t.group_id == 1026 || t.group_id == 1063).unwrap_or(false)))
                    .or_else(|| if !net.pins.is_empty() { Some(0) } else { None });
                if let Some(i) = idx {
                    let pin = &mut net.pins[i];
                    pin.extractor_resource_type_id = Some(res_tid);
                    pin.extractor_head_count = heads_cnt;
                    pin.extractor_cycle_seconds = 300;
                    pin.extractor_head_radius = 0.02;
                    pin.program_install_unix = now_unix;
                    save_pi_network(snap, char_id, planet_id, &net);
                    true
                } else {
                    false
                }
            });
            Some(Ok(PyRep::Bool(ok)))
        }

        ("planetMgr", "GMVerifySimulation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMVerifySimulation requires GM role".into(),
                }));
            }
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let (cpu_ok, pg_ok, links_ok) = ctx.db.with_snapshot(|snap| {
                let cc_lvl = snap.planetary_colonies.get(&(char_id, planet_id)).map(|c| c.command_center_level).unwrap_or(0);
                let net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                let c_ok = net.cpu_used <= net.cpu_max;
                let p_ok = net.pg_used <= net.pg_max;
                let l_ok = net.links.iter().all(|l| net.pins.iter().any(|p| p.pin_id == l.source_pin_id) && net.pins.iter().any(|p| p.pin_id == l.dest_pin_id));
                (c_ok, p_ok, l_ok)
            });
            let valid = cpu_ok && pg_ok && links_ok;
            Some(Ok(PyRep::keyval([
                ("valid", PyRep::Bool(valid)),
                ("cpuOk", PyRep::Bool(cpu_ok)),
                ("powergridOk", PyRep::Bool(pg_ok)),
                ("linksOk", PyRep::Bool(links_ok)),
            ])))
        }

        ("planetMgr", "GMGetLocalDistributionReport") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMGetLocalDistributionReport requires GM role".into(),
                }));
            }
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let (pins_count, report_dict) = ctx.db.with_snapshot(|snap| {
                let p_cnt = snap.planetary_colonies.get(&(char_id, planet_id)).map(|c| c.pins_count as i64).unwrap_or(0);
                let sys_id = ctx.sde.solar_systems.values().find(|s| s.planet_ids.contains(&planet_id)).map(|s| s.solar_system_id).unwrap_or(30000142);
                let p0_res = crate::services::industry_pi::list_sde_planet_p0_resources(ctx, sys_id, planet_id);
                let dist: Vec<(PyRep, PyRep)> = p0_res.into_iter().map(|tid| {
                    let richness = ((planet_id as u64).wrapping_mul(31).wrapping_add((tid as u64).wrapping_mul(17)) % 100) as f64 / 100.0;
                    (PyRep::Integer(tid as i64), PyRep::Real(richness))
                }).collect();
                (p_cnt, PyRep::Dict(dist))
            });
            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("characterID", PyRep::Integer(char_id)),
                ("pinsCount", PyRep::Integer(pins_count)),
                ("report", report_dict),
            ])))
        }

        ("planetMgr", "GMGetSynchedServerState") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMGetSynchedServerState requires GM role".into(),
                }));
            }
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let rep = ctx.db.with_snapshot(|snap| {
                let count = snap.planetary_colonies.keys().filter(|(cid, _)| *cid == char_id).count() as i64;
                let col = snap.planetary_colonies.get(&(char_id, planet_id));
                let cc_lvl = col.map(|c| c.command_center_level).unwrap_or(0);
                let net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                let (pins_rep, links_rep, routes_rep) = format_pi_pins_links(&net);
                PyRep::keyval([
                    ("colonyCount", PyRep::Integer(count)),
                    ("planetID", PyRep::Integer(planet_id)),
                    ("lastTick", PyRep::Integer(col.map(|c| c.last_extracted_unix as i64).unwrap_or(0))),
                    ("pins", PyRep::List(pins_rep)),
                    ("links", PyRep::List(links_rep)),
                    ("routes", PyRep::List(routes_rep)),
                ])
            });
            Some(Ok(rep))
        }

        ("planetMgr", "GMRunDepletionSim") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "AdminAccessDenied".into(),
                    message: "planetMgr.GMRunDepletionSim requires GM role".into(),
                }));
            }
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let duration_hours = args.get(1).and_then(|a| a.as_i64()).unwrap_or(24);
            let rep = ctx.db.with_snapshot(|snap| {
                let count = snap.planetary_colonies.keys().filter(|(cid, _)| *cid == char_id).count() as i64;
                let col = snap.planetary_colonies.get(&(char_id, planet_id));
                let cc_lvl = col.map(|c| c.command_center_level).unwrap_or(0);
                let net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                let ext_heads: u32 = net.pins.iter().map(|p| p.extractor_head_count as u32).sum();
                let base_rate = 0.015 * (ext_heads.max(1) as f64);
                let mut curve = Vec::new();
                for h in 0..duration_hours.min(100) {
                    let yield_factor = (-base_rate * (h as f64) * 0.1).exp();
                    curve.push(PyRep::Real(yield_factor));
                }
                PyRep::keyval([
                    ("colonyCount", PyRep::Integer(count)),
                    ("planetID", PyRep::Integer(planet_id)),
                    ("durationHours", PyRep::Integer(duration_hours)),
                    ("depletionRate", PyRep::Real(base_rate)),
                    ("depletionCurve", PyRep::List(curve)),
                ])
            });
            Some(Ok(rep))
        }

        ("planetMgr", "append") => {
            // const-ok: client-side list-like proxy method on bound planetMgr object
            Some(Ok(PyRep::None))
        }

        ("planetMgr", "celestialIndex") => {
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let idx = ctx.sde.solar_systems.values()
                .find(|s| s.planet_ids.contains(&planet_id))
                .and_then(|s| {
                    ctx.sde.celestials_in_system(s.solar_system_id)
                        .into_iter()
                        .find(|c| c.celestial_id == planet_id)
                        .and_then(|c| c.celestial_index)
                })
                .unwrap_or(1);
            Some(Ok(PyRep::Integer(idx as i64)))
        }

        ("planetMgr", "colonies") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let entries = ctx.db.with_snapshot(|snap| {
                let mut cols = Vec::new();
                for (&(cid, pid), c) in &snap.planetary_colonies {
                    if cid == char_id {
                        cols.push(PyRep::keyval([
                            ("planetID", PyRep::Integer(pid)),
                            ("solarSystemID", PyRep::Integer(c.solar_system_id as i64)),
                            ("commandCenterLevel", PyRep::Integer(c.command_center_level as i64)),
                            ("pinsCount", PyRep::Integer(c.pins_count as i64)),
                        ]));
                    }
                }
                vec![(PyRep::Integer(char_id), PyRep::List(cols))]
            });
            Some(Ok(PyRep::Dict(entries)))
        }

        ("planetMgr", "id") | ("planetMgr", "planetID") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or_else(|| {
                ctx.db.with_snapshot(|snap| {
                    snap.planetary_colonies.keys().find(|(cid, _)| *cid == char_id).map(|(_, pid)| *pid).unwrap_or(40_009_082)
                })
            });
            Some(Ok(PyRep::Integer(planet_id)))
        }

        ("planetMgr", "remoteHandler") => {
            // const-ok: bound moniker remoteHandler property accessor
            Some(Ok(PyRep::None))
        }

        // Planetary Customs Office & Skyhook Registry Broker (`planetOrbitalRegistryBroker`, `feat:pi.customs-office`, `feat:pi.poco-settings`, `feat:pi.skyhook-pi`)
        ("planetOrbitalRegistryBroker", "GetTaxRate") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let res = ctx.db.with_snapshot(|snap| {
                let (player_rate, _) =
                    resolve_character_orbital_access_and_tax(snap, char_id, planet_id)?;
                Ok(PyRep::Real(player_rate))
            });
            Some(res)
        }

        ("planetOrbitalRegistryBroker", "GetSettingsInfo") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let cfg = ctx.db.with_snapshot(|snap| {
                load_orbital_settings(snap, planet_id).unwrap_or(OrbitalCustomsSettings {
                    planet_id,
                    ..Default::default()
                })
            });
            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("orbitalTypeID", PyRep::Integer(cfg.orbital_type_id as i64)),
                ("ownerCorpID", PyRep::Integer(cfg.owner_corp_id)),
                ("corporationTaxRate", PyRep::Real(cfg.corporation_tax_rate)),
                ("allianceTaxRate", PyRep::Real(cfg.alliance_tax_rate)),
                (
                    "standingHighTaxRate",
                    PyRep::Real(cfg.standing_high_tax_rate),
                ),
                (
                    "standingGoodTaxRate",
                    PyRep::Real(cfg.standing_good_tax_rate),
                ),
                (
                    "standingNeutralTaxRate",
                    PyRep::Real(cfg.standing_neutral_tax_rate),
                ),
                ("standingBadTaxRate", PyRep::Real(cfg.standing_bad_tax_rate)),
                (
                    "standingHorribleTaxRate",
                    PyRep::Real(cfg.standing_horrible_tax_rate),
                ),
                ("allowAlliance", PyRep::Bool(cfg.allow_alliance)),
                ("allowStandings", PyRep::Bool(cfg.allow_standings)),
                ("minimumStanding", PyRep::Real(cfg.minimum_standing)),
            ])))
        }

        ("planetOrbitalRegistryBroker", "UpdateSettings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let res = match ctx.db.try_with_mut_snapshot(|snap| {
                let char_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(1_000_009);
                let existing_opt = load_orbital_settings(snap, planet_id);
                if !session.has_gm_role() {
                    let Some(existing) = &existing_opt else {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: format!("No player corporation orbital deployed at planet {planet_id}"),
                        });
                    };
                    if existing.owner_corp_id != char_corp {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: format!("Orbital at planet {planet_id} belongs to corporation {}", existing.owner_corp_id),
                        });
                    }
                    let Some(corp) = snap.corporations.get(&char_corp) else {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Updating orbital settings requires a player corporation".into(),
                        });
                    };
                    let role = corp.member_roles.get(&char_id).copied().unwrap_or(session.corp_role);
                    let allowed = eve_db::corp_roles::ROLE_DIRECTOR
                        | eve_db::corp_roles::ROLE_STATION_MANAGER
                        | eve_db::corp_roles::ROLE_EQUIPMENT_CONFIG;
                    if corp.ceo_id != char_id && (role & allowed) == 0 {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Updating orbital settings requires Director or Station Manager role".into(),
                        });
                    }
                }
                let mut cfg = existing_opt.unwrap_or(OrbitalCustomsSettings {
                    planet_id,
                    owner_corp_id: char_corp,
                    ..Default::default()
                });
                if let Some(otid) = kwargs
                    .get("orbitalTypeID")
                    .and_then(|v| v.as_i64())
                    .filter(|&id| matches!(id, 2233 | 4318 | 81080))
                {
                    cfg.orbital_type_id = otid as TypeId;
                }
                // Parse either V24 `(orbitalID, selectedHour, taxRateValues, standingValue, allowAlliance, allowStandings)` or KeyVal/kwargs
                let tax_rep = args
                    .get(2)
                    .or_else(|| args.get(1))
                    .or_else(|| kwargs.get("taxRateValues"));
                if let Some(PyRep::KeyVal(kv)) = tax_rep {
                    if let Some(v) = kv.get("corporation").and_then(|x| x.as_f64()) {
                        cfg.corporation_tax_rate = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = kv.get("alliance").and_then(|x| x.as_f64()) {
                        cfg.alliance_tax_rate = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = kv.get("standingHigh").and_then(|x| x.as_f64()) {
                        cfg.standing_high_tax_rate = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = kv.get("standingGood").and_then(|x| x.as_f64()) {
                        cfg.standing_good_tax_rate = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = kv.get("standingNeutral").and_then(|x| x.as_f64()) {
                        cfg.standing_neutral_tax_rate = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = kv.get("standingBad").and_then(|x| x.as_f64()) {
                        cfg.standing_bad_tax_rate = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = kv.get("standingHorrible").and_then(|x| x.as_f64()) {
                        cfg.standing_horrible_tax_rate = v.clamp(0.0, 1.0);
                    }
                    if let Some(v) = kv.get("orbitalTypeID").and_then(|x| x.as_i64()) {
                        cfg.orbital_type_id = v as TypeId;
                    }
                    if let Some(v) = kv.get("minimumStanding").and_then(|x| x.as_f64()) {
                        cfg.minimum_standing = v.clamp(-10.0, 10.0);
                    }
                    if let Some(v) = kv.get("allowAlliance").and_then(|x| x.as_bool()) {
                        cfg.allow_alliance = v;
                    }
                    if let Some(v) = kv.get("allowStandings").and_then(|x| x.as_bool()) {
                        cfg.allow_standings = v;
                    }
                }
                if let Some(st_val) = args
                    .get(3)
                    .and_then(|a| a.as_f64())
                    .or_else(|| kwargs.get("minimumStanding").and_then(|v| v.as_f64()))
                {
                    cfg.minimum_standing = st_val.clamp(-10.0, 10.0);
                }
                if let Some(aa) = args
                    .get(4)
                    .and_then(|a| a.as_bool())
                    .or_else(|| kwargs.get("allowAlliance").and_then(|v| v.as_bool()))
                {
                    cfg.allow_alliance = aa;
                }
                if let Some(as_flag) = args
                    .get(5)
                    .and_then(|a| a.as_bool())
                    .or_else(|| kwargs.get("allowStandings").and_then(|v| v.as_bool()))
                {
                    cfg.allow_standings = as_flag;
                }
                save_orbital_settings(snap, planet_id, &cfg);
                Ok(cfg)
            }) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // `planetSvc.OnMajorPlanetStateUpdate(planetID, numCommandCentersChanged)`.
            let _ = ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnMajorPlanetStateUpdate",
                "charid",
                vec![PyRep::Integer(planet_id), PyRep::Bool(false)],
            );
            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("orbitalTypeID", PyRep::Integer(res.orbital_type_id as i64)),
                ("ownerCorpID", PyRep::Integer(res.owner_corp_id)),
                ("corporationTaxRate", PyRep::Real(res.corporation_tax_rate)),
                ("allianceTaxRate", PyRep::Real(res.alliance_tax_rate)),
                (
                    "standingNeutralTaxRate",
                    PyRep::Real(res.standing_neutral_tax_rate),
                ),
                ("allowAlliance", PyRep::Bool(res.allow_alliance)),
                ("allowStandings", PyRep::Bool(res.allow_standings)),
                ("minimumStanding", PyRep::Real(res.minimum_standing)),
            ])))
        }

        ("planetOrbitalRegistryBroker", "RevertOrbitalsToInterBus") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            if let Err(e) = ctx.db.try_with_mut_snapshot(|snap| {
                if !session.has_gm_role() {
                    let char_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                    let existing_owner = load_orbital_settings(snap, planet_id)
                        .map(|c| c.owner_corp_id)
                        .unwrap_or(char_corp);
                    if existing_owner != char_corp {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Only the owning corporation or a GM can revert orbital ownership to InterBus".into(),
                        });
                    }
                    let Some(corp) = snap.corporations.get(&char_corp) else {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Reverting orbital ownership to InterBus requires a player corporation Director or Station Manager role".into(),
                        });
                    };
                    let role = corp.member_roles.get(&char_id).copied().unwrap_or(session.corp_role);
                    let allowed = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_STATION_MANAGER;
                    if corp.ceo_id != char_id && (role & allowed) == 0 {
                        return Err(BridgeRpcError {
                            code: "CrpAccessDenied".into(),
                            message: "Reverting orbital ownership to InterBus requires Director or Station Manager role".into(),
                        });
                    }
                }
                let cfg = OrbitalCustomsSettings {
                    planet_id,
                    orbital_type_id: 4318, // InterBus Customs Office
                    owner_corp_id: 1_000_148,
                    corporation_tax_rate: 0.0,
                    alliance_tax_rate: 0.0,
                    standing_high_tax_rate: 0.0,
                    standing_good_tax_rate: 0.0,
                    standing_neutral_tax_rate: 0.0,
                    standing_bad_tax_rate: 0.0,
                    standing_horrible_tax_rate: 0.0,
                    allow_alliance: true,
                    allow_standings: true,
                    minimum_standing: -10.0,
                };
                save_orbital_settings(snap, planet_id, &cfg);
                Ok(())
            }) {
                return Some(Err(e));
            }
            // `planetSvc.OnMajorPlanetStateUpdate(planetID, numCommandCentersChanged)`.
            let _ = ctx.notify(
                crate::router::NotificationTarget::Character(char_id),
                "OnMajorPlanetStateUpdate",
                "charid",
                vec![PyRep::Integer(planet_id), PyRep::Bool(false)],
            );
            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("orbitalTypeID", PyRep::Integer(4318)),
                ("ownerCorpID", PyRep::Integer(1_000_148)),
            ])))
        }

        _ => None,
    }
}
