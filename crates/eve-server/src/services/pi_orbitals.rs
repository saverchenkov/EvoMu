//! `pi_orbitals` — Colony extraction/flush, processor & expedited transfers, orbital launch, POCO & Skyhook customs taxes (`E-6`).

// parity: plan:E-6
// parity: feat:pi.expedited-transfer, feat:pi.launch, feat:pi.customs-office
// parity: feat:pi.customs-office-tax, feat:pi.poco-settings, feat:pi.skyhook-pi, feat:pi.storage-overflow
// parity: rpc:planetMgr.GetProgramResultInfo

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

/// Baseline ISK cost per unit for Planetary Interaction customs office import/export tax.
/// Matches EVE Online PI commodity tier base costs (`spaceportPin.pyj:99-120`, `importExportUI.pyj`):
/// - R0 Raw planetary materials (SDE groups `1032`, `1033`, `1035`, `426`): `5.0` ISK
/// - P1 Basic commodities (SDE group `1042`): `400.0` ISK
/// - P2 Refined commodities (SDE group `1034`): `7_200.0` ISK
/// - P3 Specialized commodities (SDE group `1040`): `60_000.0` ISK
/// - P4 Advanced commodities (SDE group `1041`): `1_200_000.0` ISK
pub fn pi_commodity_tier_base_cost(sde: &eve_sde::SdeRegistry, type_id: TypeId) -> f64 {
    if let Some(t) = sde.types.get(&type_id) {
        match t.group_id {
            1032 | 1033 | 1035 | 426 => return 5.0,
            1042 => return 400.0,
            1034 => return 7_200.0,
            1040 => return 60_000.0,
            1041 => return 1_200_000.0,
            _ => {}
        }
        if let Some(&attr_cost) = t
            .default_attributes
            .get(&1641)
            .or_else(|| t.default_attributes.get(&1640))
            .filter(|&&v| v > 0.0)
        {
            return attr_cost;
        }
        if t.base_price > 0.0 {
            return t.base_price;
        }
    }
    400.0
}

fn parse_extractor_heads_rep(rep: Option<&PyRep>) -> Option<Vec<(i64, f64, f64)>> {
    match rep {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => Some(
            list.iter()
                .enumerate()
                .filter_map(|(idx, h)| match h {
                    PyRep::Tuple(t) | PyRep::List(t) if t.len() >= 3 => Some((
                        t[0].as_i64().unwrap_or(idx as i64),
                        t[1].as_f64().unwrap_or(0.0),
                        t[2].as_f64().unwrap_or(0.0),
                    )),
                    PyRep::Tuple(t) | PyRep::List(t) if t.len() == 2 => Some((
                        idx as i64,
                        t[0].as_f64().unwrap_or(0.0),
                        t[1].as_f64().unwrap_or(0.0),
                    )),
                    PyRep::Integer(id) => Some((*id, 0.0, 0.0)),
                    _ => None,
                })
                .collect(),
        ),
        _ => None,
    }
}

pub fn handle_pi_orbitals(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
    bound_entry: Option<&crate::router::BoundObjectEntry>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let bound_planet = bound_entry.and_then(|b| (b.service == "planetMgr").then_some(b.object_id));

    if let Some(res) = crate::services::industry_pi::pi_orbitals_gm::handle_pi_orbitals_gm(
        ctx, session, service, method, args, kwargs, bound_entry,
    ) {
        return Some(res);
    }

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
            let (planet_id_opt, requested_arg) = match bound_planet {
                Some(bp) => {
                    if let Some(first_arg) = args.first() {
                        if let Some(id) = first_arg.as_i64() {
                            if (40_000_000..50_000_000).contains(&id) {
                                (Some(id), args.get(1))
                            } else {
                                (Some(bp), Some(first_arg))
                            }
                        } else {
                            (Some(bp), Some(first_arg))
                        }
                    } else {
                        (Some(bp), None)
                    }
                }
                None => (args.first().and_then(|a| a.as_i64()), args.get(1)),
            };
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "Flush requires an explicit integer planetID or bound planetMgr moniker".into(),
                }));
            };
            let requested_resource_or_pin = match requested_arg {
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
            let current_sys_id = session.solar_system_id;
            let (link_pg, link_cpu) = compute_pi_link_cost(150.0, 1);
            let now_unix = ctx.now_unix();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (_cc_level, _pins_count, col_sys_id) = {
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
                let mut net = load_pi_network(snap, char_id, planet_id, _cc_level);
                let is_ext_pin = |p: &PiPinState| {
                    p.type_id == 2848
                        || p.type_id == 3060
                        || ctx
                            .sde
                            .types
                            .get(&p.type_id)
                            .map(|t| t.group_id == 1026 || t.group_id == 1063)
                            .unwrap_or(false)
                };
                let total_ext = net.pins.iter().filter(|p| is_ext_pin(p)).count();
                let extractor_pins = if total_ext > 0 {
                    net.pins
                        .iter()
                        .filter(|p| is_ext_pin(p) && !p.expiry_time_unix.is_some_and(|exp| now_unix >= exp))
                        .count() as u32
                } else {
                    1
                };
                let raw_produced_qty = p0_qty_per_pin.saturating_mul(u64::from(extractor_pins));
                let ext_idx = net
                    .pins
                    .iter()
                    .position(|p| {
                        p.type_id == 2848
                            || p.type_id == 3060
                            || ctx
                                .sde
                                .types
                                .get(&p.type_id)
                                .map(|t| t.group_id == 1026 || t.group_id == 1063)
                                .unwrap_or(false)
                    })
                    .or_else(|| {
                        net.pins
                            .iter()
                            .position(|p| is_storage_or_cc_or_launchpad_pin(&ctx.sde, p.type_id))
                    });
                let p0_vol_per_unit = ctx
                    .sde
                    .types
                    .get(&p0_type_id)
                    .map(|t| t.volume.max(0.01))
                    .unwrap_or(0.38);
                let produced_qty = if let Some(idx) = ext_idx {
                    let cap_m3 = pin_storage_capacity_m3(&ctx.sde, net.pins[idx].type_id);
                    let cur_vol = pin_contents_volume_m3(&ctx.sde, &net.pins[idx]);
                    let avail_m3 = (cap_m3 - cur_vol).max(0.0);
                    let max_units = (avail_m3 / p0_vol_per_unit).floor() as u64;
                    raw_produced_qty.min(max_units)
                } else {
                    raw_produced_qty
                };
                let _produced_qty_i32 =
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
                let export_tax = if ext_idx.is_some() {
                    0.0
                } else {
                    compute_poco_export_tax(
                        tax_base,
                        npc_customs_tax_rate,
                        customs_code_expertise_level,
                        player_poco_rate,
                    )
                };
                let tax_cents = IskCents::from_f64_isk(export_tax);
                if tax_cents.0 > 0 {
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
                        let bal_after = ch.balance;
                        snap.append_journal(
                            char_id,
                            "CustomOfficeTax",
                            tax_cents,
                            bal_after,
                            format!("POCO export tax for flush on planet {planet_id}"),
                        );
                    }
                    if let Some(cfg) = load_orbital_settings(snap, planet_id) {
                        let player_share = IskCents::from_f64_isk(tax_base * player_poco_rate);
                        if let Some(corp) = snap.corporations.get_mut(&cfg.owner_corp_id) {
                            corp.ensure_wallet_divisions();
                            corp.master_wallet = corp.master_wallet.checked_add(player_share).unwrap_or(corp.master_wallet);
                            corp.wallet_divisions[0].balance = corp.master_wallet;
                        }
                    }
                }

                let delivered_item_id = if let Some(idx) = ext_idx {
                    let pid = net.pins[idx].pin_id;
                    if produced_qty > 0 {
                        *net.pins[idx].contents.entry(p0_type_id).or_insert(0) += produced_qty;
                    }
                    save_pi_network(snap, char_id, planet_id, &net);
                    pid
                } else {
                    if let Some(col_mut) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
                        *col_mut.extracted_commodities.entry(p0_type_id).or_insert(0) +=
                            produced_qty;
                    }
                    planet_id
                };

                let col = snap
                    .planetary_colonies
                    .get_mut(&(char_id, planet_id))
                    .expect("colony checked above");
                col.last_extracted_unix = now_unix;
                let total_stored = col.extracted_commodities.get(&p0_type_id).copied().unwrap_or(0);

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
            let (planet_id_opt, target_level_opt) = match bound_planet {
                Some(bp) => {
                    if (40_000_000..50_000_000).contains(&args.first().and_then(|a| a.as_i64()).unwrap_or(0)) && args.len() >= 2 {
                        (args.first().and_then(|a| a.as_i64()), args.get(1).and_then(|a| a.as_i64()))
                    } else {
                        (Some(bp), args.first().and_then(|a| a.as_i64()))
                    }
                }
                None => (args.first().and_then(|a| a.as_i64()), args.get(1).and_then(|a| a.as_i64())),
            };
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "UpgradeCommandCenter requires an explicit integer planetID or bound planetMgr moniker".into(),
                }));
            };
            let Some(target_level_raw) = target_level_opt else {
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
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "UserTransferCommodities requires an explicit integer planetID or bound planetMgr moniker".into(),
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
            let (planet_id_opt, launch_args) = match bound_planet {
                Some(bp) => {
                    if let Some(first_id) = args.first().and_then(|a| a.as_i64()) {
                        if (40_000_000..50_000_000).contains(&first_id) {
                            (Some(first_id), &args[1..])
                        } else {
                            (Some(bp), args)
                        }
                    } else {
                        (Some(bp), args)
                    }
                }
                None => (args.first().and_then(|a| a.as_i64()), if args.is_empty() { args } else { &args[1..] }),
            };
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "UserLaunchCommodities requires planetID or bound planetMgr moniker".into(),
                }));
            };
            let (launch_pin_id_opt, type_id, qty, destination) = if launch_args.len() >= 3 && launch_args.get(2).and_then(|a| a.as_i64()).is_some() && launch_args.get(3).is_some() {
                let pid = launch_args.first().and_then(|a| a.as_i64());
                let tid = launch_args.get(1).and_then(|a| a.as_i64()).unwrap_or(0) as TypeId;
                let q = launch_args.get(2).and_then(|a| a.as_i64()).unwrap_or(0).max(1) as u64;
                (pid, tid, q, "CommandCenterLaunch")
            } else if launch_args.len() >= 3 && launch_args.get(2).and_then(|a| a.as_str()).is_some() {
                let tid = launch_args.first().and_then(|a| a.as_i64()).unwrap_or(0) as TypeId;
                let q = launch_args.get(1).and_then(|a| a.as_i64()).unwrap_or(0).max(1) as u64;
                let dest = launch_args.get(2).and_then(|a| a.as_str()).unwrap_or("Storage");
                (None, tid, q, dest)
            } else if launch_args.len() >= 3 {
                let pid = launch_args.first().and_then(|a| a.as_i64());
                let tid = launch_args.get(1).and_then(|a| a.as_i64()).unwrap_or(0) as TypeId;
                let q = launch_args.get(2).and_then(|a| a.as_i64()).unwrap_or(0).max(1) as u64;
                (pid, tid, q, "CommandCenterLaunch")
            } else {
                let tid = launch_args.first().and_then(|a| a.as_i64()).unwrap_or(0) as TypeId;
                let q = launch_args.get(1).and_then(|a| a.as_i64()).unwrap_or(0).max(1) as u64;
                let dest = launch_args.get(2).and_then(|a| a.as_str()).unwrap_or("Storage");
                (None, tid, q, dest)
            };
            if type_id == 0 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidRoutingParameters".into(),
                    message: "UserLaunchCommodities requires commodity typeID".into(),
                }));
            }

            let current_sys_id = session.solar_system_id;
            let now_unix = ctx.now_unix();
            let container_type_id: TypeId = ctx
                .sde
                .types
                .values()
                .find(|t| t.name.eq_ignore_ascii_case("Planetary Launch Container"))
                .or_else(|| ctx.sde.types.values().find(|t| t.name.contains("Planetary Launch")))
                .map(|t| t.type_id)
                .unwrap_or(27_912);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!(
                            "No colony found for character {char_id} on planet {planet_id}"
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

                let unit_vol = ctx
                    .sde
                    .types
                    .get(&type_id)
                    .map(|t| t.volume.max(0.005))
                    .unwrap_or(0.38);
                let total_vol_m3 = unit_vol * (qty as f64);
                let unit_base = pi_commodity_tier_base_cost(&ctx.sde, type_id);

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
                                corp.ensure_wallet_divisions();
                                corp.master_wallet = corp.master_wallet.checked_add(player_share).unwrap_or(corp.master_wallet);
                                if let Some(d0) = corp.wallet_divisions.first_mut() {
                                    d0.balance = corp.master_wallet;
                                }
                            }
                        }
                    }

                    *net.pins[target_idx].contents.entry(type_id).or_insert(0) += qty;
                    save_pi_network(snap, char_id, planet_id, &net);

                    return Ok((qty, import_tax_isk, import_tax_isk, orbital_tid, 0_i64, col.solar_system_id, [0.0; 3]));
                }

                let cc_lvl = col.command_center_level;
                let mut net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                let launch_pin_idx = if let Some(pid) = launch_pin_id_opt {
                    let idx = net.pins.iter().position(|p| p.pin_id == pid).ok_or_else(|| BridgeRpcError {
                        code: "PinNotFound".into(),
                        message: format!("Launching pin {pid} not found on planet {planet_id}"),
                    })?;
                    if !is_storage_or_cc_or_launchpad_pin(&ctx.sde, net.pins[idx].type_id) {
                        return Err(BridgeRpcError {
                            code: "InvalidPinTypeForLaunch".into(),
                            message: format!("Pin {pid} is not a storage, command center, or launchpad pin"),
                        });
                    }
                    idx
                } else {
                    net.pins
                        .iter()
                        .position(|p| is_storage_or_cc_or_launchpad_pin(&ctx.sde, p.type_id) && p.contents.get(&type_id).copied().unwrap_or(0) >= qty)
                        .or_else(|| net.pins.iter().position(|p| is_storage_or_cc_or_launchpad_pin(&ctx.sde, p.type_id)))
                        .ok_or_else(|| BridgeRpcError {
                            code: "PinNotFound".into(),
                            message: format!("No launching pin found on planet {planet_id}"),
                        })?
                };

                let mut export_tax_isk = 0.0_f64;
                let mut orbital_tid: TypeId = 2233;
                let is_cc = destination == "CommandCenterLaunch" || destination == "CommandCenter";
                if destination == "CustomsOffice" || destination == "Skyhook" || destination == "Hangar" || is_cc {
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

                if destination == "CustomsOffice" || destination == "Skyhook" || destination == "Hangar" || is_cc {
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
                                corp.ensure_wallet_divisions();
                                corp.master_wallet = corp.master_wallet.checked_add(player_share).unwrap_or(corp.master_wallet);
                                if let Some(d0) = corp.wallet_divisions.first_mut() {
                                    d0.balance = corp.master_wallet;
                                }
                            }
                        }
                    }
                }
                let mut created_launch_id = 0_i64;
                let mut launch_pos = [0.0_f64; 3];
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
                    if destination == "Hangar" {
                        snap.next_entity_id += 1;
                        let nid = snap.next_entity_id;
                        created_launch_id = nid;
                        snap.items.insert(
                            nid,
                            InventoryItemRecord {
                                item_id: nid,
                                type_id,
                                owner_id: char_id,
                                location_id: col.planet_id,
                                flag: InventoryFlag::Hangar,
                                quantity: i32::try_from(qty).unwrap_or(i32::MAX),
                                singleton: false,
                                version: 1,
                                ..Default::default()
                            },
                        );
                    } else {
                        launch_pos = ctx
                            .sde
                            .celestials_in_system(col.solar_system_id)
                            .into_iter()
                            .find(|c| c.celestial_id == col.planet_id)
                            .map(|c| {
                                let offset = c.radius.max(1_000_000.0) + 150_000.0;
                                [
                                    c.position.x + offset,
                                    c.position.y + 25_000.0,
                                    c.position.z - 15_000.0,
                                ]
                            })
                            .unwrap_or([1.5e10, 2.5e8, -1.5e8]);
                        snap.next_entity_id += 1;
                        let canister_id = snap.next_entity_id;
                        created_launch_id = canister_id;
                        snap.items.insert(
                            canister_id,
                            InventoryItemRecord {
                                item_id: canister_id,
                                type_id: container_type_id,
                                owner_id: char_id,
                                location_id: col.solar_system_id as i64,
                                flag: InventoryFlag::None,
                                quantity: 1,
                                singleton: true,
                                version: 1,
                                ..Default::default()
                            },
                        );
                        snap.next_entity_id += 1;
                        let commodity_item_id = snap.next_entity_id;
                        snap.items.insert(
                            commodity_item_id,
                            InventoryItemRecord {
                                item_id: commodity_item_id,
                                type_id,
                                owner_id: char_id,
                                location_id: canister_id,
                                flag: InventoryFlag::Hangar,
                                quantity: i32::try_from(qty).unwrap_or(i32::MAX),
                                singleton: false,
                                version: 1,
                                ..Default::default()
                            },
                        );
                        snap.item_positions.insert(canister_id, launch_pos);
                        let meta = snap
                            .sp6_lifecycle
                            .space_object_meta
                            .entry(canister_id)
                            .or_default();
                        meta.created_unix = Some(now_unix);
                        meta.planet_id = Some(col.planet_id);
                    }
                }
                Ok((qty, export_tax_isk, 0.0, orbital_tid, created_launch_id, col.solar_system_id, launch_pos))
            });

            match res {
                Ok((routed, export_tax_isk, import_tax_isk, orbital_tid, created_launch_id, col_sys_id, launch_pos)) => {
                    if created_launch_id > 0 && destination != "Hangar" {
                        let sys_handle = ctx.systems.get_or_spawn(col_sys_id);
                        let (group_id, category_id, radius, mass) = ctx
                            .sde
                            .types
                            .get(&container_type_id)
                            .map(|t| (t.group_id, t.category_id, t.radius.max(20.0), t.mass.max(1_000.0)))
                            .unwrap_or((12, 2, 25.0, 1_000.0));
                        sys_handle.try_spawn_ball_with_meta(
                            eve_destiny::Ball {
                                id: created_launch_id,
                                type_id: container_type_id,
                                position: glam::DVec3::from_array(launch_pos),
                                velocity: glam::DVec3::ZERO,
                                max_velocity: 0.0,
                                inertia: 1.0,
                                mass,
                                radius,
                                speed_fraction: 0.0,
                                mode: eve_destiny::MovementMode::Stop,
                            },
                            eve_destiny::CombatState::default(),
                            eve_destiny::SlimItemMeta {
                                group_id,
                                category_id,
                                owner_id: char_id,
                                char_id: None,
                                corp_id: -1,
                                alliance_id: -1,
                                security_status: 0.0,
                                is_cloaked: false,
                            },
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("launchID", PyRep::Integer(created_launch_id)),
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
            let now_unix = ctx.now_unix();
            let filter_planet = bound_planet.or_else(|| {
                args.first()
                    .and_then(|a| a.as_i64())
                    .filter(|&id| (40_000_000..50_000_000).contains(&id))
            });
            let session_sys_id = session.solar_system_id.map(i64::from).unwrap_or(0);
            let launches = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter_map(|it| {
                        if it.owner_id != char_id {
                            return None;
                        }
                        let meta = snap.sp6_lifecycle.space_object_meta.get(&it.item_id);
                        let launch_planet = meta
                            .and_then(|m| m.planet_id)
                            .or_else(|| (40_000_000..50_000_000).contains(&it.location_id).then_some(it.location_id))?;
                        if filter_planet.is_some_and(|p| launch_planet != p) {
                            return None;
                        }
                        let sys_id = if (30_000_000..=34_000_000).contains(&it.location_id) {
                            it.location_id
                        } else {
                            snap.planetary_colonies
                                .get(&(char_id, launch_planet))
                                .map(|c| c.solar_system_id as i64)
                                .or_else(|| {
                                    ctx.sde
                                        .solar_systems
                                        .values()
                                        .find(|s| s.planet_ids.contains(&launch_planet))
                                        .map(|s| s.solar_system_id as i64)
                                })
                                .unwrap_or(session_sys_id)
                        };
                        let launch_ts = meta
                            .and_then(|m| m.created_unix)
                            .map(|t| t as i64)
                            .unwrap_or(now_unix as i64);
                        let (comm_tid, comm_qty) = snap
                            .items
                            .values()
                            .find(|c| c.location_id == it.item_id)
                            .map(|c| (c.type_id, c.quantity))
                            .unwrap_or((it.type_id, it.quantity));
                        let pos = snap
                            .item_positions
                            .get(&it.item_id)
                            .copied()
                            .unwrap_or([0.0, 0.0, 0.0]);
                        Some(PyRep::keyval([
                            ("launchID", PyRep::Integer(it.item_id)),
                            ("itemID", PyRep::Integer(it.item_id)),
                            ("containerTypeID", PyRep::Integer(it.type_id as i64)),
                            ("planetID", PyRep::Integer(launch_planet)),
                            ("solarSystemID", PyRep::Integer(sys_id)),
                            ("typeID", PyRep::Integer(comm_tid as i64)),
                            ("quantity", PyRep::Integer(comm_qty as i64)),
                            ("status", PyRep::Integer(1)),
                            ("launchTime", PyRep::Integer(launch_ts)),
                            ("x", PyRep::Real(pos[0])),
                            ("y", PyRep::Real(pos[1])),
                            ("z", PyRep::Real(pos[2])),
                        ]))
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
            let Some(launch_id) = args.first().and_then(|a| a.as_i64()).filter(|&id| id > 0) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLaunchID".into(),
                    message: "DeleteLaunch requires a positive integer launchID".into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(it) = snap.items.get(&launch_id) else {
                    return Err(BridgeRpcError {
                        code: "LaunchNotFound".into(),
                        message: format!("Launch item {launch_id} not found"),
                    });
                };
                if it.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "LaunchAccessDenied".into(),
                        message: format!("Launch item {launch_id} does not belong to character {char_id}"),
                    });
                }
                let launch_planet_opt = snap
                    .sp6_lifecycle
                    .space_object_meta
                    .get(&launch_id)
                    .and_then(|m| m.planet_id)
                    .or_else(|| (40_000_000..50_000_000).contains(&it.location_id).then_some(it.location_id));
                let Some(launch_planet) = launch_planet_opt else {
                    return Err(BridgeRpcError {
                        code: "InvalidLaunchID".into(),
                        message: format!("Item {launch_id} is not a planetary launch container (location_id: {})", it.location_id),
                    });
                };
                if let Some(bp) = bound_planet {
                    if launch_planet != bp {
                        return Err(BridgeRpcError {
                            code: "LaunchNotFound".into(),
                            message: format!("Launch item {launch_id} is on planet {launch_planet}, not bound planet {bp}"),
                        });
                    }
                }
                let sys_id_opt = (30_000_000..=34_000_000)
                    .contains(&it.location_id)
                    .then_some(it.location_id as u32);
                snap.items.remove(&launch_id);
                snap.item_positions.remove(&launch_id);
                snap.sp6_lifecycle.space_object_meta.remove(&launch_id);
                snap.items.retain(|_, child| child.location_id != launch_id);
                Ok(sys_id_opt)
            });
            match res {
                Ok(sys_id_opt) => {
                    if let Some(sys_id) = sys_id_opt {
                        ctx.systems.get_or_spawn(sys_id).try_remove_ball(launch_id);
                    }
                    Some(Ok(PyRep::Bool(true)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("planetMgr", "CancelInstallProgram") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (planet_id_opt, pin_id) = match bound_planet {
                Some(bp) => {
                    let pin = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
                    (Some(bp), pin)
                }
                None => {
                    let p = args.first().and_then(|a| a.as_i64());
                    let pin = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
                    (p, pin)
                }
            };
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "CancelInstallProgram requires planetID or bound planetMgr moniker".into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!("No colony on planet {planet_id}"),
                    });
                };
                let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                let idx = if pin_id > 0 {
                    net.pins.iter().position(|p| p.pin_id == pin_id)
                } else {
                    net.pins.iter().position(|p| {
                        p.extractor_head_count > 0
                            || p.type_id == 2848
                            || ctx.sde.types.get(&p.type_id).map(|t| t.group_id == 1026 || t.group_id == 1063).unwrap_or(false)
                    })
                };
                if pin_id > 0 && idx.is_none() {
                    return Err(BridgeRpcError {
                        code: "PinNotFound".into(),
                        message: format!("Pin {pin_id} not found in colony on planet {planet_id}"),
                    });
                }
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
                Ok(PyRep::Bool(true))
            });
            Some(res)
        }

        ("planetMgr", "RevertChanges") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "RevertChanges requires planetID or bound planetMgr moniker".into(),
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
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "StartTicking requires planetID or bound planetMgr moniker".into(),
                }));
            };
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(mut col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!("No colony found for character {char_id} on planet {planet_id}"),
                    });
                };
                let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                let baseline = if col.last_extracted_unix > 0 {
                    col.last_extracted_unix
                } else {
                    net.last_simulated_unix
                };
                let elapsed = if baseline > 0 && now_unix > baseline {
                    now_unix.saturating_sub(baseline)
                } else {
                    0
                };
                if elapsed > 0 {
                    advance_colony_simulation(ctx, &mut col, &mut net, elapsed, now_unix);
                }
                col.last_extracted_unix = now_unix;
                net.last_simulated_unix = now_unix;
                snap.planetary_colonies.insert((char_id, planet_id), col);
                save_pi_network(snap, char_id, planet_id, &net);
                Ok(PyRep::Bool(true))
            });
            Some(res)
        }

        ("planetMgr", "GetProgramResultInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (planet_id_opt, pin_id, res_type_opt, preview_heads, head_radius_opt) =
                if let Some(bp) = bound_planet {
                    let (pin, res_t, heads_rep, r_opt) = if (40_000_000..50_000_000).contains(&args.first().and_then(|a| a.as_i64()).unwrap_or(0)) && args.len() >= 3 {
                        (
                            args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                            args.get(2).and_then(|a| a.as_i64()).map(|t| t as TypeId),
                            args.get(3),
                            args.get(4).and_then(|a| a.as_f64()),
                        )
                    } else {
                        (
                            args.first().and_then(|a| a.as_i64()).unwrap_or(0),
                            args.get(1).and_then(|a| a.as_i64()).map(|t| t as TypeId),
                            args.get(2),
                            args.get(3).and_then(|a| a.as_f64()),
                        )
                    };
                    let heads = parse_extractor_heads_rep(heads_rep);
                    (Some(bp), pin, res_t, heads, r_opt)
                } else if args.len() >= 3 && matches!(args.get(2), Some(PyRep::List(_) | PyRep::Tuple(_))) {
                    let pin = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
                    let res_t = args.get(1).and_then(|a| a.as_i64()).map(|t| t as TypeId);
                    let heads = parse_extractor_heads_rep(args.get(2));
                    let r_opt = args.get(3).and_then(|a| a.as_f64());
                    let p_id = ctx.db.with_snapshot(|snap| {
                        for (&(cid, pid), col) in &snap.planetary_colonies {
                            if cid == char_id {
                                let net = load_pi_network(snap, char_id, pid, col.command_center_level);
                                if net.pins.iter().any(|p| p.pin_id == pin) {
                                    return Some(pid);
                                }
                            }
                        }
                        None
                    });
                    (p_id, pin, res_t, heads, r_opt)
                } else {
                    let p = args.first().and_then(|a| a.as_i64());
                    let pin = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
                    let res_t = args.get(2).and_then(|a| a.as_i64()).map(|t| t as TypeId);
                    let heads = parse_extractor_heads_rep(args.get(3));
                    let r_opt = args.get(4).and_then(|a| a.as_f64());
                    (p, pin, res_t, heads, r_opt)
                };

            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetProgramResultInfo requires planetID or bound planetMgr moniker".into(),
                }));
            };

            let res = ctx.db.with_snapshot(|snap| {
                let cc_lvl = snap.planetary_colonies.get(&(char_id, planet_id)).map(|c| c.command_center_level).unwrap_or(0);
                let net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                let pin_opt = net.pins.iter().find(|p| p.pin_id == pin_id);
                let empty = Vec::new();
                let head_coords = preview_heads
                    .as_ref()
                    .filter(|h| !h.is_empty())
                    .or_else(|| pin_opt.map(|p| &p.extractor_heads))
                    .unwrap_or(&empty);
                let heads = preview_heads
                    .as_ref()
                    .filter(|h| !h.is_empty())
                    .map(|h| h.len())
                    .or_else(|| pin_opt.map(|p| p.extractor_head_count.max(1) as usize))
                    .unwrap_or(1);
                let head_radius = head_radius_opt
                    .or_else(|| pin_opt.map(|p| p.extractor_head_radius))
                    .filter(|&r| r > 0.0)
                    .unwrap_or(0.02);
                let overlap = crate::services::industry_pi::extractor_overlap_factor(head_coords, head_radius);
                let cyc_secs = pin_opt.map(|p| p.extractor_cycle_seconds.max(300)).unwrap_or(300);
                let _res_tid = res_type_opt
                    .or_else(|| pin_opt.and_then(|p| p.extractor_resource_type_id))
                    .unwrap_or(2268);
                let qty_to_distribute = (1_000.0 * (heads as f64) * overlap).round() as i64;
                let cycle_time = cyc_secs as i64 * 10_000_000;
                let num_cycles = 10_i64;
                // V24 `clientPlanet.pyj:140`:
                // `qtyToDistribute, cycleTime, numCycles = self.remoteHandler.GetProgramResultInfo(pinID, typeID, pin.heads, headRadius)`
                PyRep::Tuple(vec![
                    PyRep::Integer(qty_to_distribute),
                    PyRep::Integer(cycle_time),
                    PyRep::Integer(num_cycles),
                ])
            });
            Some(Ok(res))
        }

        ("planetMgr", "GetLocalDistributionReport") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (planet_id_opt, pin_id) = match bound_planet {
                Some(bp) => (Some(bp), args.first().and_then(|a| a.as_i64()).unwrap_or(0)),
                None => (args.first().and_then(|a| a.as_i64()), args.get(1).and_then(|a| a.as_i64()).unwrap_or(0)),
            };
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "GetLocalDistributionReport requires planetID or bound planetMgr moniker".into(),
                }));
            };
            let (pins_count, report_dict) = ctx.db.with_snapshot(|snap| {
                let p_cnt = snap.planetary_colonies.get(&(char_id, planet_id)).map(|c| c.pins_count as i64).unwrap_or(0);
                let cc_lvl = snap.planetary_colonies.get(&(char_id, planet_id)).map(|c| c.command_center_level).unwrap_or(0);
                let net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                let pin = net.pins.iter().find(|p| p.pin_id == pin_id)
                    .or_else(|| net.pins.iter().find(|p| p.type_id == 2848 || p.extractor_head_count > 0))
                    .or_else(|| net.pins.first());
                let dist_map = if let Some(p) = pin {
                    compute_pin_resource_distribution(ctx, snap, planet_id, p)
                } else {
                    let dummy_pin = PiPinState { pin_id, ..Default::default() };
                    compute_pin_resource_distribution(ctx, snap, planet_id, &dummy_pin)
                };
                let mut dist: Vec<(PyRep, PyRep)> = dist_map
                    .into_iter()
                    .map(|(tid, richness)| (PyRep::Integer(tid as i64), PyRep::Real(richness)))
                    .collect();
                dist.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
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

        ("planetMgr", "append") => {
            // const-ok: client-side list-like proxy method on bound planetMgr object
            Some(Ok(PyRep::None))
        }

        ("planetMgr", "celestialIndex") => {
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "celestialIndex requires planetID or bound planetMgr moniker".into(),
                }));
            };
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
            let (_, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id_opt = bound_planet.or_else(|| args.first().and_then(|a| a.as_i64()));
            let Some(planet_id) = planet_id_opt else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPlanetID".into(),
                    message: "planetID requires bound planetMgr moniker or explicit planetID".into(),
                }));
            };
            Some(Ok(PyRep::Integer(planet_id)))
        }

        ("planetMgr", "remoteHandler") => {
            // const-ok: bound moniker remoteHandler property accessor
            Some(Ok(PyRep::None))
        }

        _ => None,
    }
}
