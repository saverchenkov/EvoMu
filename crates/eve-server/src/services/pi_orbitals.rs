//! `pi_orbitals` — Colony extraction/flush, processor & expedited transfers, orbital launch, POCO & Skyhook customs taxes (`E-6`).

// parity: plan:E-6
// parity: feat:pi.expedited-transfer, feat:pi.launch, feat:pi.customs-office
// parity: feat:pi.poco-settings, feat:pi.skyhook-pi, feat:pi.storage-overflow

#![forbid(unsafe_code)]

use crate::formulas::{compute_pi_link_cost, compute_poco_export_tax};
use crate::router::ServerContext;
use crate::services::industry_pi::{
    cc_level_cpu_pg, cc_upgrade_cost_isk, load_pi_network, resolve_sde_planet_p0_extraction,
    save_pi_network,
};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OrbitalCustomsSettings {
    pub planet_id: i64,
    pub orbital_type_id: TypeId, // 2233 = POCO, 4318 = InterBus, 81080 = Orbital Skyhook
    pub owner_corp_id: i64,
    pub corporation_tax_rate: f64,
    pub alliance_tax_rate: f64,
    pub standing_high_tax_rate: f64,
    pub standing_good_tax_rate: f64,
    pub standing_neutral_tax_rate: f64,
    pub standing_bad_tax_rate: f64,
    pub standing_horrible_tax_rate: f64,
    pub allow_alliance: bool,
    pub allow_standings: bool,
    pub minimum_standing: f64,
}

impl Default for OrbitalCustomsSettings {
    fn default() -> Self {
        Self {
            planet_id: 0,
            orbital_type_id: 2233,
            owner_corp_id: 1_000_009,
            corporation_tax_rate: 0.05,
            alliance_tax_rate: 0.05,
            standing_high_tax_rate: 0.05,
            standing_good_tax_rate: 0.05,
            standing_neutral_tax_rate: 0.05,
            standing_bad_tax_rate: 0.05,
            standing_horrible_tax_rate: 0.05,
            allow_alliance: true,
            allow_standings: true,
            minimum_standing: -10.0,
        }
    }
}

fn orbital_settings_note_id(planet_or_orbital_id: i64) -> i64 {
    -(4_000_000_000_000_i64.saturating_add(planet_or_orbital_id.abs()))
}

pub fn load_orbital_settings(
    snap: &eve_db::WorldStateSnapshot,
    planet_or_orbital_id: i64,
) -> Option<OrbitalCustomsSettings> {
    let nid = orbital_settings_note_id(planet_or_orbital_id);
    snap.owner_notes
        .get(&nid)
        .and_then(|n| serde_json::from_str::<OrbitalCustomsSettings>(&n.note).ok())
}

pub fn save_orbital_settings(
    snap: &mut eve_db::WorldStateSnapshot,
    planet_or_orbital_id: i64,
    settings: &OrbitalCustomsSettings,
) {
    let nid = orbital_settings_note_id(planet_or_orbital_id);
    if let Ok(js) = serde_json::to_string(settings) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -4,
                item_id: planet_or_orbital_id,
                label: format!("orbital_settings:{planet_or_orbital_id}"),
                note: js,
                timestamp_unix: 0,
            },
        );
    }
}

/// Evaluates whether `char_id` is permitted to use the orbital customs office (POCO `2233`, InterBus `4318`, or Skyhook `81080`)
/// and returns `(player_tax_rate, orbital_type_id)`.
fn resolve_character_orbital_access_and_tax(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    planet_id: i64,
) -> Result<(f64, TypeId), BridgeRpcError> {
    let Some(cfg) = load_orbital_settings(snap, planet_id) else {
        return Ok((0.05, 2233));
    };
    if cfg.orbital_type_id == 4318 {
        // Reverted to InterBus NPC customs office: 0% player tax (only NPC tax applies)
        return Ok((0.0, 4318));
    }
    let ch = snap.characters.get(&char_id);
    let char_corp = ch.map(|c| c.corporation_id).unwrap_or(0);
    if char_corp == cfg.owner_corp_id || char_id == cfg.owner_corp_id {
        return Ok((cfg.corporation_tax_rate, cfg.orbital_type_id));
    }
    let standing = ch
        .and_then(|c| c.standings.get(&cfg.owner_corp_id).copied())
        .unwrap_or(0.0);
    if !cfg.allow_standings || standing < cfg.minimum_standing {
        return Err(BridgeRpcError {
            code: "CustomsOfficeAccessDenied".into(),
            message: format!(
                "Customs Office / Skyhook on planet {planet_id} denies access (standing {standing:.1} < minimum {:.1})",
                cfg.minimum_standing
            ),
        });
    }
    let rate = if standing > 5.0 {
        cfg.standing_high_tax_rate
    } else if standing > 0.0 {
        cfg.standing_good_tax_rate
    } else if standing >= -0.01 {
        cfg.standing_neutral_tax_rate
    } else if standing >= -5.0 {
        cfg.standing_bad_tax_rate
    } else {
        cfg.standing_horrible_tax_rate
    };
    Ok((rate, cfg.orbital_type_id))
}

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

                let col = snap
                    .planetary_colonies
                    .get_mut(&(char_id, planet_id))
                    .expect("colony checked above");
                col.last_extracted_unix = now_unix;
                *col.extracted_commodities.entry(p0_type_id).or_insert(0) += produced_qty;
                let total_stored = col.extracted_commodities[&p0_type_id];

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
            let target_level = target_level_raw as u8;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)) else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!(
                            "No colony found for character {char_id} on planet {planet_id}"
                        ),
                    });
                };
                if target_level <= col.command_center_level {
                    return Err(BridgeRpcError {
                        code: "InvalidCommandCenterLevel".into(),
                        message: format!(
                            "Target level {target_level} must be greater than current level {}",
                            col.command_center_level
                        ),
                    });
                }
                let cc_skill_lvl = snap
                    .skills
                    .get(&char_id)
                    .and_then(|m| m.get(&2505))
                    .map(|s| s.level)
                    .unwrap_or(0);
                if cc_skill_lvl < target_level {
                    return Err(BridgeRpcError {
                        code: "MissingSkillForCommandCenterUpgrade".into(),
                        message: format!(
                            "Command Center upgrade to level {target_level} requires Command Center Upgrades skill (2505) >= {target_level} (current: {cc_skill_lvl})"
                        ),
                    });
                }

                let upgrade_cost_isk =
                    cc_upgrade_cost_isk(col.command_center_level, target_level);
                let upgrade_cost_cents = IskCents::from_f64_isk(upgrade_cost_isk);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    if ch.balance < upgrade_cost_cents {
                        return Err(BridgeRpcError {
                            code: "InsufficientFunds".into(),
                            message: format!(
                                "Insufficient funds for command center upgrade ({upgrade_cost_isk:.2} ISK required)"
                            ),
                        });
                    }
                    ch.balance = ch
                        .balance
                        .checked_sub(upgrade_cost_cents)
                        .unwrap_or(IskCents(0));
                    let bal_after = ch.balance;
                    snap.append_journal(
                        char_id,
                        "PlanetCommandCenterUpgrade",
                        upgrade_cost_cents,
                        bal_after,
                        format!(
                            "Upgraded Command Center on planet {planet_id} to level {target_level}"
                        ),
                    );
                }

                let mut net = load_pi_network(snap, char_id, planet_id, target_level);
                let (new_cpu_max, new_pg_max) = cc_level_cpu_pg(target_level);
                net.cpu_max = new_cpu_max;
                net.pg_max = new_pg_max;
                save_pi_network(snap, char_id, planet_id, &net);

                let col_mut = snap
                    .planetary_colonies
                    .get_mut(&(char_id, planet_id))
                    .expect("colony exists");
                col_mut.command_center_level = target_level;
                col_mut.pins_count = (target_level as u32) + 1;
                let new_pins = col_mut.pins_count;

                Ok((target_level, new_pins, upgrade_cost_isk, new_cpu_max, new_pg_max))
            });

            match res {
                Ok((new_level, new_pins, cost, cpu_max, pg_max)) => Some(Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)),
                    ("commandCenterLevel", PyRep::Integer(new_level as i64)),
                    ("pinsCount", PyRep::Integer(new_pins as i64)),
                    ("upgradeCostISK", PyRep::Real(cost)),
                    ("cpuMax", PyRep::Real(cpu_max)),
                    ("powergridMax", PyRep::Real(pg_max)),
                ]))),
                Err(e) => Some(Err(e)),
            }
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

            let is_expedited = kwargs
                .get("expedited")
                .and_then(|v| v.as_bool())
                .unwrap_or(false)
                || matches!(args.get(2), Some(PyRep::Dict(_) | PyRep::KeyVal(_)));

            if is_expedited {
                let src_pin_id = args
                    .get(1)
                    .and_then(|a| a.as_i64())
                    .or_else(|| kwargs.get("sourcePinID").and_then(|v| v.as_i64()))
                    .unwrap_or(0);
                let dst_pin_id = args
                    .get(2)
                    .and_then(|a| a.as_i64())
                    .or_else(|| kwargs.get("destPinID").and_then(|v| v.as_i64()))
                    .unwrap_or(0);
                let comm_type_id = args
                    .get(3)
                    .and_then(|a| a.as_i64())
                    .or_else(|| kwargs.get("commodityTypeID").and_then(|v| v.as_i64()))
                    .unwrap_or(3645) as TypeId;
                let qty = args
                    .get(4)
                    .and_then(|a| a.as_i64())
                    .or_else(|| kwargs.get("quantity").and_then(|v| v.as_i64()))
                    .unwrap_or(100)
                    .max(1) as u64;
                let unit_vol = ctx
                    .sde
                    .types
                    .get(&comm_type_id)
                    .map(|t| t.volume.max(0.01))
                    .unwrap_or(0.38);
                let transfer_vol_m3 = (qty as f64) * unit_vol;
                // Expedited transfer cooldown: 5 minutes (300s) + 0.03 * volume_m3 minutes (`1.8s` per m3)
                let cooldown_secs = (300.0 + 0.03 * transfer_vol_m3 * 60.0).round() as u64;
                let now_unix = ctx.now_unix();

                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned()
                    else {
                        return Err(BridgeRpcError {
                            code: "ColonyNotFound".into(),
                            message: format!(
                                "No colony found for character {char_id} on planet {planet_id}"
                            ),
                        });
                    };
                    let mut net = load_pi_network(snap, char_id, planet_id, col.command_center_level);
                    let dst_cap_m3 = if let Some(dp) = net.pins.iter().find(|p| p.pin_id == dst_pin_id)
                    {
                        let grp = ctx.sde.types.get(&dp.type_id).map(|t| t.group_id).unwrap_or(0);
                        match grp {
                            1027 => 500.0,
                            1030 => 10_000.0,
                            1029 => 12_000.0,
                            _ => 10_000.0,
                        }
                    } else {
                        10_000.0
                    };
                    if transfer_vol_m3 > dst_cap_m3 {
                        return Err(BridgeRpcError {
                            code: "PinStorageFull".into(),
                            message: format!(
                                "Expedited transfer volume {transfer_vol_m3:.1} m3 exceeds destination pin capacity {dst_cap_m3:.1} m3"
                            ),
                        });
                    }
                    if let Some(sp) = net.pins.iter_mut().find(|p| p.pin_id == src_pin_id) {
                        if sp.last_expedited_transfer_unix > 0
                            && now_unix
                                < sp.last_expedited_transfer_unix.saturating_add(cooldown_secs)
                        {
                            return Err(BridgeRpcError {
                                code: "ExpeditedTransferOnCooldown".into(),
                                message: format!(
                                    "Source pin {src_pin_id} expedited transfer is on cooldown for {cooldown_secs}s"
                                ),
                            });
                        }
                        sp.last_expedited_transfer_unix = now_unix;
                    }
                    save_pi_network(snap, char_id, planet_id, &net);
                    Ok((transfer_vol_m3, cooldown_secs))
                });
                return match res {
                    Ok((vol, cd)) => Some(Ok(PyRep::keyval([
                        ("planetID", PyRep::Integer(planet_id)),
                        ("sourcePinID", PyRep::Integer(src_pin_id)),
                        ("destPinID", PyRep::Integer(dst_pin_id)),
                        ("commodityTypeID", PyRep::Integer(comm_type_id as i64)),
                        ("quantity", PyRep::Integer(qty as i64)),
                        ("volumeM3", PyRep::Real(vol)),
                        ("cooldownSeconds", PyRep::Integer(cd as i64)),
                    ]))),
                    Err(e) => Some(Err(e)),
                };
            }

            let Some(input_type_id_raw) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidProcessorParameters".into(),
                    message: "UserTransferCommodities requires input commodity typeID".into(),
                }));
            };
            let Some(output_type_id_raw) = args.get(2).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidProcessorParameters".into(),
                    message: "UserTransferCommodities requires output commodity typeID".into(),
                }));
            };
            let input_type_id = input_type_id_raw as TypeId;
            let output_type_id = output_type_id_raw as TypeId;
            let cycles = args.get(3).and_then(|a| a.as_i64()).unwrap_or(1).max(1) as u64;
            if cycles > 20_000 {
                return Some(Err(BridgeRpcError {
                    code: "PinStorageFull".into(),
                    message: format!(
                        "Requested transfer/processing quantity {cycles} exceeds planetary pin storage capacity"
                    ),
                }));
            }

            let in_grp = ctx
                .sde
                .types
                .get(&input_type_id)
                .map(|t| t.group_id)
                .unwrap_or(0);
            // P0 -> P1 (group 1032): 3000 -> 20
            // P1 -> P2 (group 1034): 40 -> 5
            // P2 -> P3 (group 1040): 10 -> 3
            // P3 -> P4 (group 1041): 6 -> 1
            let (input_per_cycle, output_per_cycle) = match in_grp {
                1032 => (40_u64, 5_u64),
                1034 => (10_u64, 3_u64),
                1040 => (6_u64, 1_u64),
                _ => (3_000_u64, 20_u64),
            };
            let req_input = input_per_cycle.saturating_mul(cycles);
            let produced_output = output_per_cycle.saturating_mul(cycles);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!(
                            "No colony found for character {char_id} on planet {planet_id}"
                        ),
                    });
                };
                let current_input = col
                    .extracted_commodities
                    .get(&input_type_id)
                    .copied()
                    .unwrap_or(0);
                if current_input < req_input {
                    return Err(BridgeRpcError {
                        code: "InsufficientInputCommodities".into(),
                        message: format!(
                            "Colony storage has {current_input}x type {input_type_id}, but {req_input}x is required for {cycles} processor cycles"
                        ),
                    });
                }
                *col.extracted_commodities.entry(input_type_id).or_insert(0) -= req_input;
                *col.extracted_commodities.entry(output_type_id).or_insert(0) += produced_output;
                let total_output = col.extracted_commodities[&output_type_id];

                Ok((req_input, produced_output, total_output))
            });

            match res {
                Ok((consumed, produced, total)) => Some(Ok(PyRep::keyval([
                    ("planetID", PyRep::Integer(planet_id)),
                    ("inputTypeID", PyRep::Integer(input_type_id as i64)),
                    ("inputQuantityConsumed", PyRep::Integer(consumed as i64)),
                    ("outputTypeID", PyRep::Integer(output_type_id as i64)),
                    ("outputQuantityProduced", PyRep::Integer(produced as i64)),
                    ("totalStoredOutput", PyRep::Integer(total as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
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
            let Some(type_id_raw) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidRoutingParameters".into(),
                    message: "UserLaunchCommodities requires commodity typeID".into(),
                }));
            };
            let type_id = type_id_raw as TypeId;
            let qty = args.get(2).and_then(|a| a.as_i64()).unwrap_or(0).max(1) as u64;
            let destination = args.get(3).and_then(|a| a.as_str()).unwrap_or("Storage");

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(col) = snap.planetary_colonies.get(&(char_id, planet_id)).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ColonyNotFound".into(),
                        message: format!(
                            "No colony found for character {char_id} on planet {planet_id}"
                        ),
                    });
                };

                if destination == "ImportToColony" {
                    let (player_poco_rate, orbital_tid) =
                        resolve_character_orbital_access_and_tax(snap, char_id, planet_id)?;
                    let customs_lvl = snap
                        .skills
                        .get(&char_id)
                        .and_then(|m| m.get(&33467))
                        .map(|s| s.level.clamp(0, 5))
                        .unwrap_or(0);
                    let npc_customs_tax_rate = if ctx
                        .sde
                        .solar_systems
                        .get(&col.solar_system_id)
                        .map(|s| s.security_status >= 0.45)
                        .unwrap_or(true)
                    {
                        0.10
                    } else {
                        0.0
                    };
                    let unit_base = ctx
                        .sde
                        .types
                        .get(&type_id)
                        .map(|t| t.base_price.max(400.0))
                        .unwrap_or(400.0);
                    // Import tax = 0.5 * Export tax (`0.5 * Rate * Volume/Qty * BaseCost`)
                    let import_tax_isk = 0.5
                        * compute_poco_export_tax(
                            unit_base * (qty as f64),
                            npc_customs_tax_rate,
                            customs_lvl,
                            player_poco_rate,
                        );
                    let tax_cents = IskCents::from_f64_isk(import_tax_isk);
                    if tax_cents.0 > 0 {
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.balance = ch.balance.checked_sub(tax_cents).unwrap_or(IskCents(0));
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
                    }
                    if let Some(col_mut) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
                        *col_mut.extracted_commodities.entry(type_id).or_insert(0) += qty;
                    }
                    return Ok((qty, import_tax_isk, import_tax_isk, orbital_tid));
                }

                let available = col.extracted_commodities.get(&type_id).copied().unwrap_or(0);
                if available < qty {
                    return Err(BridgeRpcError {
                        code: "InsufficientCommoditiesForRouting".into(),
                        message: format!(
                            "Colony has {available} of type {type_id}, cannot route {qty}"
                        ),
                    });
                }
                let mut export_tax_isk = 0.0_f64;
                let mut orbital_tid: TypeId = 2233;
                if destination == "CustomsOffice" || destination == "Skyhook" {
                    let (player_poco_rate, otid) =
                        resolve_character_orbital_access_and_tax(snap, char_id, planet_id)?;
                    orbital_tid = otid;
                    let customs_lvl = snap
                        .skills
                        .get(&char_id)
                        .and_then(|m| m.get(&33467))
                        .map(|s| s.level.clamp(0, 5))
                        .unwrap_or(0);
                    let npc_customs_tax_rate = if ctx
                        .sde
                        .solar_systems
                        .get(&col.solar_system_id)
                        .map(|s| s.security_status >= 0.45)
                        .unwrap_or(true)
                    {
                        0.10
                    } else {
                        0.0
                    };
                    let unit_base = ctx
                        .sde
                        .types
                        .get(&type_id)
                        .map(|t| t.base_price.max(400.0))
                        .unwrap_or(400.0);
                    export_tax_isk = compute_poco_export_tax(
                        unit_base * (qty as f64),
                        npc_customs_tax_rate,
                        customs_lvl,
                        player_poco_rate,
                    );
                    let tax_cents = IskCents::from_f64_isk(export_tax_isk);
                    if tax_cents.0 > 0 {
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            ch.balance = ch.balance.checked_sub(tax_cents).unwrap_or(IskCents(0));
                            let bal_after = ch.balance;
                            snap.append_journal(
                                char_id,
                                "CustomOfficeTax",
                                tax_cents,
                                bal_after,
                                format!(
                                    "POCO export tax for {qty}x type {type_id} on planet {planet_id}"
                                ),
                            );
                        }
                    }
                } else if destination == "CommandCenterLaunch" {
                    // Command Center direct orbital rocket launch: 500 m3 max, flat 500 ISK fee (`feat:pi.launch`)
                    export_tax_isk = 500.0;
                    let fee_cents = IskCents::from_f64_isk(500.0);
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        ch.balance = ch.balance.checked_sub(fee_cents).unwrap_or(IskCents(0));
                    }
                }
                if matches!(
                    destination,
                    "Hangar" | "CustomsOffice" | "Skyhook" | "CommandCenterLaunch"
                ) {
                    if let Some(col_mut) = snap.planetary_colonies.get_mut(&(char_id, planet_id)) {
                        *col_mut.extracted_commodities.entry(type_id).or_insert(0) -= qty;
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

        ("planetMgr", "CancelInstallProgram")
        | ("planetMgr", "RevertChanges")
        | ("planetMgr", "StartTicking") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ok = ctx.db.with_snapshot(|snap| snap.characters.contains_key(&char_id));
            Some(Ok(PyRep::Bool(ok)))
        }

        ("planetMgr", "GetProgramResultInfo") | ("planetMgr", "GetLocalDistributionReport") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let pin_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.with_snapshot(|snap| {
                let cc_lvl = snap
                    .planetary_colonies
                    .get(&(char_id, planet_id))
                    .map(|c| c.command_center_level)
                    .unwrap_or(0);
                let net = load_pi_network(snap, char_id, planet_id, cc_lvl);
                let pin_opt = net.pins.iter().find(|p| p.pin_id == pin_id);
                let heads = pin_opt.map(|p| p.extractor_head_count.max(1)).unwrap_or(1);
                let cyc_secs = pin_opt
                    .map(|p| p.extractor_cycle_seconds.max(300))
                    .unwrap_or(300);
                let mut cycle_values = Vec::with_capacity(10);
                for c in 0..10 {
                    let y = (1_000.0 * (heads as f64) * (-0.015 * (c as f64)).exp()).round() as i64;
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

        ("planetMgr", "GMAddCommodity")
        | ("planetMgr", "GMConvertCommandCenter")
        | ("planetMgr", "GMForceInstallProgram")
        | ("planetMgr", "GMVerifySimulation") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ok = ctx.db.with_snapshot(|snap| snap.characters.contains_key(&char_id));
            Some(Ok(PyRep::Bool(ok)))
        }

        ("planetMgr", "GMGetLocalDistributionReport") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
            let pins_count = ctx.db.with_snapshot(|snap| {
                snap.planetary_colonies
                    .get(&(char_id, planet_id))
                    .map(|c| c.pins_count as i64)
                    .unwrap_or(0)
            });
            Some(Ok(PyRep::keyval([
                ("planetID", PyRep::Integer(planet_id)),
                ("characterID", PyRep::Integer(char_id)),
                ("pinsCount", PyRep::Integer(pins_count)),
                ("report", PyRep::Dict(Vec::new())),
            ])))
        }

        ("planetMgr", "GMGetSynchedServerState") | ("planetMgr", "GMRunDepletionSim") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let count = ctx.db.with_snapshot(|snap| {
                snap.planetary_colonies
                    .keys()
                    .filter(|(cid, _)| *cid == char_id)
                    .count() as i64
            });
            Some(Ok(PyRep::Dict(vec![(
                PyRep::String("colonyCount".into()),
                PyRep::Integer(count),
            )])))
        }

        ("planetMgr", "append") => {
            // const-ok: client-side list-like proxy method on bound planetMgr object
            Some(Ok(PyRep::None))
        }

        ("planetMgr", "celestialIndex") => {
            // const-ok: default celestial index property on bound planetMgr object
            Some(Ok(PyRep::Integer(1)))
        }

        ("planetMgr", "colonies") => {
            let entries = ctx.db.with_snapshot(|snap| {
                snap.planetary_colonies
                    .iter()
                    .map(|(&(cid, pid), c)| {
                        (
                            PyRep::Integer(cid),
                            PyRep::Integer(pid.saturating_add(c.pins_count as i64)),
                        )
                    })
                    .collect()
            });
            Some(Ok(PyRep::Dict(entries)))
        }

        ("planetMgr", "id") | ("planetMgr", "planetID") => {
            let planet_id = args.first().and_then(|a| a.as_i64()).unwrap_or(40_009_082);
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
            let res = ctx.db.with_mut_snapshot(|snap| {
                let char_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(1_000_009);
                let mut cfg =
                    load_orbital_settings(snap, planet_id).unwrap_or(OrbitalCustomsSettings {
                        planet_id,
                        owner_corp_id: char_corp,
                        ..Default::default()
                    });
                cfg.owner_corp_id = char_corp;
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
                cfg
            });
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
            ctx.db.with_mut_snapshot(|snap| {
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
            });
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
