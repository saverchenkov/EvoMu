//! `fitting_saved_apply` — Fitting Ships from Saved Fittings / Multifit (`FitShips`).

#![forbid(unsafe_code)]

use std::collections::{HashMap, HashSet};
use crate::router::ServerContext;
use eve_db::{InventoryFlag, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub async fn handle_fit_ships(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let Some(station_id) = session.station_id.or(session.structure_id) else {
        return Some(Err(BridgeRpcError {
            code: "NotDocked".into(),
            message: "Fitting from saved fitting requires being docked".into(),
        }));
    };
    let mut target_ship_ids: Vec<i64> = Vec::new();
    if let Some(PyRep::List(list) | PyRep::Tuple(list)) = args.first() {
        if list.iter().all(|el| el.as_i64().is_some()) {
            target_ship_ids.extend(list.iter().filter_map(|el| el.as_i64()));
        }
    }
    if target_ship_ids.is_empty() {
        if let Some(sid) = kwargs
            .get("shipID")
            .and_then(|v| v.as_i64())
            .or(session.ship_id)
        {
            target_ship_ids.push(sid);
        }
    }
    let fitting_id_opt = if args.len() >= 2 && !target_ship_ids.is_empty() {
        args.get(1).and_then(|a| a.as_i64())
    } else {
        args.first()
            .and_then(|a| a.as_i64())
            .or_else(|| kwargs.get("fittingID").and_then(|v| v.as_i64()))
    };

    let mut desired_items: Vec<(TypeId, InventoryFlag, i32)> = Vec::new();
    let mut expected_ship_type_id: Option<u32> = None;
    if let Some(fid) = fitting_id_opt {
        if let Some(saved) = ctx.db.with_snapshot(|s| s.saved_fittings.get(&fid).cloned()) {
            desired_items = saved.items;
            if saved.ship_type_id > 0 {
                expected_ship_type_id = Some(saved.ship_type_id);
            }
        }
    }
    if desired_items.is_empty() {
        let fit_data_arg = kwargs
            .get("fitData")
            .or_else(|| args.get(1))
            .or_else(|| args.first());
        if let Some(PyRep::List(list) | PyRep::Tuple(list)) = fit_data_arg {
            for el in list {
                if let PyRep::Tuple(seq) | PyRep::List(seq) = el {
                    if let (Some(tid), Some(flg)) = (
                        seq.first().and_then(|v| v.as_i64()),
                        seq.get(1).and_then(|v| v.as_i64()),
                    ) {
                        let Ok(u16_flg) = u16::try_from(flg) else {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidFittingFlag".into(),
                                message: format!("Invalid flag {flg}"),
                            }));
                        };
                        let is_allowed_flag = matches!(
                            u16_flg,
                            5 | 87 | 11..=34 | 92..=94 | 125..=132 | 158..=162 | 164..=171 | 133..=151 | 179 | 181 | 182 | 185
                        );
                        if !is_allowed_flag {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidFittingFlag".into(),
                                message: format!("Flag {flg} is not a valid fitting destination"),
                            }));
                        }
                        let qty_i64 = seq.get(2).and_then(|v| v.as_i64()).unwrap_or(1);
                        let Ok(qty) = i32::try_from(qty_i64) else {
                            return Some(Err(BridgeRpcError {
                                code: "InvalidQuantity".into(),
                                message: format!("Invalid quantity {qty_i64}"),
                            }));
                        };
                        desired_items.push((
                            tid as TypeId,
                            InventoryFlag::from_u16(u16_flg),
                            qty.max(1),
                        ));
                    }
                }
            }
        }
    }

    let ship_check = ctx.db.with_snapshot(|snap| {
        for &ship_id in &target_ship_ids {
            let Some(sh) = snap.items.get(&ship_id) else {
                return Err(BridgeRpcError {
                    code: "ShipNotFound".into(),
                    message: format!("Target ship {ship_id} not found"),
                });
            };
            let cat = ctx.sde.types.get(&sh.type_id).map(|t| t.category_id).unwrap_or(0);
            if sh.owner_id != char_id || sh.location_id != station_id || cat != 6 {
                return Err(BridgeRpcError {
                    code: "InvalidFittingTarget".into(),
                    message: format!("Target {ship_id} must be an owned ship at station {station_id}"),
                });
            }
            if let Some(exp_type) = expected_ship_type_id {
                if sh.type_id != exp_type {
                    return Err(BridgeRpcError {
                        code: "UserError".into(),
                        message: "ShipTypeMismatch".into(),
                    });
                }
            }
            let ship_type = ctx.sde.types.get(&sh.type_id);
            let low_slots = ship_type
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::LOW_SLOTS))
                .copied()
                .unwrap_or(0.0) as u16;
            let med_slots = ship_type
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::MED_SLOTS))
                .copied()
                .unwrap_or(0.0) as u16;
            let hi_slots = ship_type
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::HI_SLOTS))
                .copied()
                .unwrap_or(0.0) as u16;
            let rig_slots = ship_type
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::RIG_SLOTS))
                .copied()
                .unwrap_or(0.0) as u16;

            let occupied_slots: HashSet<InventoryFlag> = snap
                .items
                .values()
                .filter(|i| i.location_id == ship_id && matches!(i.flag as u16, 11..=34 | 92..=94 | 125..=132))
                .map(|i| i.flag)
                .collect();

            let mut planned_slots = HashSet::new();
            for &(_tid, flag, _) in &desired_items {
                let flag_u16 = flag as u16;
                if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) {
                    if occupied_slots.contains(&flag) || !planned_slots.insert(flag) {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "SlotAlreadyOccupied".into(),
                        });
                    }
                    if (11..=18).contains(&flag_u16) && flag_u16 - 11 >= low_slots {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "SlotIndexExceedsHullSlots".into(),
                        });
                    }
                    if (19..=26).contains(&flag_u16) && flag_u16 - 19 >= med_slots {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "SlotIndexExceedsHullSlots".into(),
                        });
                    }
                    if (27..=34).contains(&flag_u16) && flag_u16 - 27 >= hi_slots {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "SlotIndexExceedsHullSlots".into(),
                        });
                    }
                    if (92..=94).contains(&flag_u16) && flag_u16 - 92 >= rig_slots {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: "SlotIndexExceedsHullSlots".into(),
                        });
                    }
                }
            }
            // SDE slot effects and rig size validation
            for &(tid, flag, req_qty) in &desired_items {
                if req_qty <= 0 {
                    return Err(BridgeRpcError {
                        code: "InvalidQuantity".into(),
                        message: "Fitting item quantity must be positive".into(),
                    });
                }
                let flag_u16 = flag as u16;
                if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) {
                    let Some(m_type) = ctx.sde.types.get(&tid) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Type {tid} not found in SDE"),
                        });
                    };
                    if !matches!(m_type.category_id, 7 | 8 | 32) || (m_type.category_id != 8 && req_qty != 1) {
                        return Err(BridgeRpcError {
                            code: "ItemNotFittableModule".into(),
                            message: format!("Type {tid} (qty {req_qty}) cannot be fitted to slot {flag_u16}"),
                        });
                    }
                    let req_eff = match flag_u16 {
                        11..=18 => 11_u16, // loPower
                        19..=26 => 13_u16, // medPower
                        27..=34 => 12_u16, // hiPower
                        92..=94 => 2663_u16, // rigSlot
                        125..=132 => 3772_u16, // subSystem
                        _ => 0,
                    };
                    if req_eff > 0 && !m_type.effect_ids.contains(&req_eff) {
                        return Err(BridgeRpcError {
                            code: "UserError".into(),
                            message: format!("SlotEffectMismatch: Type {tid} lacks required slot effect {req_eff} for slot {flag_u16}"),
                        });
                    }
                    if (matches!(flag_u16, 92..=94) || m_type.effect_ids.contains(&2663)) && m_type.default_attributes.contains_key(&eve_sde::attr_ids::RIG_SIZE) {
                        let hull_rig_size = ship_type
                            .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::RIG_SIZE))
                            .copied()
                            .unwrap_or(0.0) as u32;
                        let mod_rig_size = m_type
                            .default_attributes
                            .get(&eve_sde::attr_ids::RIG_SIZE)
                            .copied()
                            .unwrap_or(0.0) as u32;
                        if mod_rig_size > 0 && hull_rig_size > 0 && mod_rig_size != hull_rig_size {
                            return Err(BridgeRpcError {
                                code: "UserError".into(),
                                message: format!("RigSizeMismatch: module rigSize {mod_rig_size} != hull rigSize {hull_rig_size}"),
                            });
                        }
                    }
                }
            }

            // Rig calibration budget check
            let hull_upgrade_cap = ship_type
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::UPGRADE_CAPACITY))
                .copied()
                .unwrap_or(400.0);
            let existing_cal: f64 = snap.items.values()
                .filter(|i| i.location_id == ship_id && matches!(i.flag as u16, 92..=94))
                .filter_map(|i| ctx.sde.types.get(&i.type_id))
                .filter_map(|t| t.default_attributes.get(&eve_sde::attr_ids::UPGRADE_COST).copied())
                .sum();
            let new_cal: f64 = desired_items.iter()
                .filter(|(_, flag, _)| matches!(*flag as u16, 92..=94))
                .filter_map(|(tid, _, q)| {
                    ctx.sde.types.get(tid)
                        .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::UPGRADE_COST).copied())
                        .map(|c| c * (*q as f64))
                })
                .sum();
            if existing_cal + new_cal > hull_upgrade_cap + 1e-4 {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("UpgradeCapacityExceeded: total calibration cost {:.1} exceeds capacity {hull_upgrade_cap}", existing_cal + new_cal),
                });
            }

            // Hardpoints limits check (turrets 102, launchers 101)
            let max_turrets = ship_type
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::TURRET_SLOTS_LEFT))
                .copied()
                .unwrap_or(0.0) as usize;
            let existing_turrets = snap.items.values()
                .filter(|i| i.location_id == ship_id && matches!(i.flag as u16, 27..=34))
                .filter(|i| ctx.sde.types.get(&i.type_id).is_some_and(|t| t.effect_ids.contains(&42)))
                .count();
            let new_turrets = desired_items.iter()
                .filter(|(_, flag, _)| matches!(*flag as u16, 27..=34))
                .filter(|(tid, _, _)| ctx.sde.types.get(tid).is_some_and(|t| t.effect_ids.contains(&42)))
                .count();
            if existing_turrets + new_turrets > max_turrets {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("InsufficientTurretHardpoints: turret limit {max_turrets} exceeded"),
                });
            }

            let max_launchers = ship_type
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::LAUNCHER_SLOTS_LEFT))
                .copied()
                .unwrap_or(0.0) as usize;
            let existing_launchers = snap.items.values()
                .filter(|i| i.location_id == ship_id && matches!(i.flag as u16, 27..=34))
                .filter(|i| ctx.sde.types.get(&i.type_id).is_some_and(|t| t.effect_ids.contains(&40)))
                .count();
            let new_launchers = desired_items.iter()
                .filter(|(_, flag, _)| matches!(*flag as u16, 27..=34))
                .filter(|(tid, _, _)| ctx.sde.types.get(tid).is_some_and(|t| t.effect_ids.contains(&40)))
                .count();
            if existing_launchers + new_launchers > max_launchers {
                return Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: format!("InsufficientLauncherHardpoints: launcher limit {max_launchers} exceeded"),
                });
            }

            // maxTypeFitted (2431 / 1544) and maxGroupFitted (1548 / 1544)
            for &(tid, flag, _) in &desired_items {
                let flag_u16 = flag as u16;
                if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) {
                    let Some(m_type) = ctx.sde.types.get(&tid) else { continue; };
                    let type_limit = m_type.default_attributes.get(&2431)
                        .or_else(|| {
                            if m_type.default_attributes.contains_key(&1548) {
                                m_type.default_attributes.get(&1544)
                            } else {
                                None
                            }
                        })
                        .copied();
                    if let Some(limit_f) = type_limit {
                        let limit = limit_f as usize;
                        let existing = snap.items.values()
                            .filter(|i| i.location_id == ship_id && matches!(i.flag as u16, 11..=34 | 92..=94 | 125..=132) && i.type_id == tid)
                            .count();
                        let planned = desired_items.iter()
                            .filter(|(t, f, _)| matches!(*f as u16, 11..=34 | 92..=94 | 125..=132) && *t == tid)
                            .count();
                        if existing + planned > limit {
                            return Err(BridgeRpcError {
                                code: "UserError".into(),
                                message: format!("MaxTypeFittedExceeded for type {tid}: limit {limit}"),
                            });
                        }
                    }
                    let group_limit = m_type.default_attributes.get(&1548)
                        .or_else(|| m_type.default_attributes.get(&1544))
                        .copied();
                    if let Some(limit_f) = group_limit {
                        let limit = limit_f as usize;
                        let existing = snap.items.values()
                            .filter(|i| i.location_id == ship_id && matches!(i.flag as u16, 11..=34 | 92..=94 | 125..=132))
                            .filter(|i| ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == m_type.group_id))
                            .count();
                        let planned = desired_items.iter()
                            .filter(|(_t, f, _)| matches!(*f as u16, 11..=34 | 92..=94 | 125..=132))
                            .filter(|(t, _, _)| ctx.sde.types.get(t).is_some_and(|ty| ty.group_id == m_type.group_id))
                            .count();
                        if existing + planned > limit {
                            return Err(BridgeRpcError {
                                code: "UserError".into(),
                                message: format!("MaxGroupFittedExceeded for group {}: limit {limit}", m_type.group_id),
                            });
                        }
                    }
                }
            }

            // Cargo capacity check (flag 5)
            let ship_cargo_cap = ship_type.map(|t| t.capacity).unwrap_or(0.0);
            let existing_cargo_vol: f64 = snap.items.values()
                .filter(|i| i.location_id == ship_id && i.flag == eve_db::InventoryFlag::Cargo)
                .map(|i| crate::services::inventory_fitting::fitting_containers_ships::effective_item_unit_volume(&ctx.sde, i) * (i.quantity as f64))
                .sum();
            let new_cargo_vol: f64 = desired_items.iter()
                .filter(|(_, flag, _)| *flag == eve_db::InventoryFlag::Cargo)
                .map(|(tid, _, qty)| {
                    let vol = ctx.sde.types.get(tid).map(|t| t.volume).unwrap_or(0.0);
                    vol * (*qty as f64)
                })
                .sum();
            if (new_cargo_vol > 0.0 || existing_cargo_vol > 0.0) && (existing_cargo_vol + new_cargo_vol > ship_cargo_cap + 1e-4) {
                return Err(BridgeRpcError {
                    code: "CapacityExceeded".into(),
                    message: format!("Cargo hold capacity {ship_cargo_cap} m³ exceeded: total cargo volume is {:.1} m³", existing_cargo_vol + new_cargo_vol),
                });
            }

            // Drone bay check (flag 87)
            let ship_drone_cap = ship_type
                .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::DRONE_CAPACITY))
                .copied()
                .unwrap_or(0.0);
            let mut new_drone_vol: f64 = 0.0;
            for &(tid, flag, req_qty) in &desired_items {
                if flag == eve_db::InventoryFlag::DroneBay {
                    let Some(d_type) = ctx.sde.types.get(&tid) else {
                        return Err(BridgeRpcError { code: "ItemNotFound".into(), message: format!("Type {tid} not found") });
                    };
                    if d_type.category_id != 18 {
                        return Err(BridgeRpcError {
                            code: "ItemNotADrone".into(),
                            message: format!("Type {tid} is category {}, not a drone (category 18)", d_type.category_id),
                        });
                    }
                    new_drone_vol += d_type.volume * (req_qty as f64);
                }
            }
            let existing_drone_vol: f64 = snap.items.values()
                .filter(|i| i.location_id == ship_id && i.flag == eve_db::InventoryFlag::DroneBay)
                .map(|i| ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.0) * (i.quantity as f64))
                .sum();
            if (new_drone_vol > 0.0 || existing_drone_vol > 0.0) && (existing_drone_vol + new_drone_vol > ship_drone_cap + 1e-4) {
                return Err(BridgeRpcError {
                    code: "CapacityExceeded".into(),
                    message: format!("Drone bay capacity {ship_drone_cap} m³ exceeded: total drone volume is {:.1} m³", existing_drone_vol + new_drone_vol),
                });
            }

            for &(tid, flag, _) in &desired_items {
                let flag_u16 = flag as u16;
                let is_specialized_hold = matches!(flag_u16, 133..=151 | 179 | 181 | 182 | 185);
                if is_specialized_hold {
                    let Some(item_type) = ctx.sde.types.get(&tid) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Type {tid} not found"),
                        });
                    };
                    let allowed_in_hold = match flag_u16 {
                        133 => matches!(item_type.group_id, 423 | 497 | 1136),
                        134 | 151 => item_type.category_id == 25 || matches!(item_type.group_id, 711 | 4168),
                        135 => matches!(item_type.group_id, 711 | 4168),
                        136 => item_type.group_id == 18,
                        143 => item_type.category_id == 8,
                        148 => item_type.group_id == 1027,
                        149 => matches!(item_type.category_id, 42 | 43),
                        179 => matches!(item_type.group_id, 4168),
                        181 => item_type.category_id == 25,
                        150 | 185 => matches!(item_type.category_id, 23 | 39 | 40 | 41 | 42 | 43 | 46 | 65 | 66 | 2143)
                            || matches!(item_type.group_id, 423 | 1136 | 4729),
                        182 => if ship_type.map(|t| t.default_attributes.get(&5646).copied().unwrap_or(0.0) > 0.0).unwrap_or(false) {
                            matches!(item_type.category_id, 23 | 39 | 40 | 41 | 42 | 43 | 46 | 65 | 66 | 2143)
                                || matches!(item_type.group_id, 423 | 1136 | 4729)
                        } else {
                            item_type.category_id == 25
                        },
                        _ => true,
                    };
                    if !allowed_in_hold {
                        return Err(BridgeRpcError {
                            code: "InvalidItemTypeForHold".into(),
                            message: format!("Type {tid} cannot be placed in specialized hold flag {flag_u16}"),
                        });
                    }
                }
            }
            let mut checked_spec_flags: Vec<u16> = desired_items.iter().map(|(_, f, _)| *f as u16).filter(|&fl| matches!(fl, 133..=151 | 179 | 181 | 182 | 185)).collect();
            checked_spec_flags.sort_unstable();
            checked_spec_flags.dedup();
            for flg in checked_spec_flags {
                let hold_capacity = match flg {
                    133 => ship_type.and_then(|t| t.default_attributes.get(&1549).copied()).unwrap_or(0.0),
                    134 | 151 => ship_type.and_then(|t| t.default_attributes.get(&1556).copied().or_else(|| t.default_attributes.get(&1770).copied())).unwrap_or(0.0),
                    136 => ship_type.and_then(|t| t.default_attributes.get(&1558).copied()).unwrap_or(0.0),
                    135 => ship_type.and_then(|t| t.default_attributes.get(&1557).copied().or_else(|| t.default_attributes.get(&1556).copied())).unwrap_or(0.0),
                    179 => ship_type.and_then(|t| t.default_attributes.get(&5325).copied()).unwrap_or(0.0),
                    181 => ship_type.and_then(|t| t.default_attributes.get(&3136).copied().or_else(|| t.default_attributes.get(&1556).copied())).unwrap_or(0.0),
                    150 | 185 => ship_type.and_then(|t| t.default_attributes.get(&5646).copied()).unwrap_or(0.0),
                    182 => ship_type.and_then(|t| t.default_attributes.get(&5646).copied().or_else(|| t.default_attributes.get(&3227).copied())).unwrap_or(0.0),
                    143 => ship_type.and_then(|t| t.default_attributes.get(&1573).copied()).unwrap_or(0.0),
                    148 => ship_type.and_then(|t| t.default_attributes.get(&1646).copied()).unwrap_or(5_000.0),
                    149 => ship_type.and_then(|t| t.default_attributes.get(&1653).copied()).unwrap_or(0.0),
                    _ => 0.0,
                };
                if hold_capacity <= 0.0 {
                    return Err(BridgeRpcError {
                        code: "CapacityExceeded".into(),
                        message: format!("Ship does not have specialized hold flag {flg}"),
                    });
                }
                let existing_vol: f64 = snap.items.values()
                    .filter(|i| i.location_id == ship_id && i.flag as u16 == flg)
                    .map(|i| ctx.sde.types.get(&i.type_id).map(|t| t.volume).unwrap_or(0.0) * (i.quantity as f64))
                    .sum();
                let new_vol: f64 = desired_items.iter()
                    .filter(|(_, f, _)| *f as u16 == flg)
                    .map(|(tid, _, qty)| ctx.sde.types.get(tid).map(|t| t.volume).unwrap_or(0.0) * (*qty as f64))
                    .sum();
                if existing_vol + new_vol > hold_capacity + 1e-4 {
                    return Err(BridgeRpcError {
                        code: "CapacityExceeded".into(),
                        message: format!("Specialized hold flag {flg} capacity {hold_capacity} m³ exceeded: total {:.1} m³", existing_vol + new_vol),
                    });
                }
            }
        }
        for &(tid, flag, req_qty) in &desired_items {
            if req_qty <= 0 {
                return Err(BridgeRpcError {
                    code: "InvalidQuantity".into(),
                    message: "Fitting item quantity must be positive".into(),
                });
            }
            let flag_u16 = flag as u16;
            if matches!(flag_u16, 11..=34 | 92..=94 | 125..=132) {
                let cat = ctx.sde.types.get(&tid).map(|t| t.category_id).unwrap_or(0);
                if !matches!(cat, 7 | 8 | 32) || (cat != 8 && req_qty != 1) {
                    return Err(BridgeRpcError {
                        code: "ItemNotFittableModule".into(),
                        message: format!("Type {tid} (qty {req_qty}) cannot be fitted to slot {flag_u16}"),
                    });
                }
            }
        }
        Ok(())
    });
    if let Err(e) = ship_check {
        return Some(Err(e));
    }

    let mut fitted_count = 0_i64;
    let mut missing: HashMap<TypeId, i64> = HashMap::new();
    let mut moved_items: Vec<eve_db::InventoryItemRecord> = Vec::new();
    let char_skills = ctx.character_skill_levels(char_id);
    ctx.db.with_mut_snapshot(|snap| {
        for &ship_id in &target_ship_ids {
            let ship_type_id = snap.items.get(&ship_id).map(|s| s.type_id).unwrap_or(0);
            let mut engine = eve_dogma::ShipFittingEngine::from_sde(ship_id, ship_type_id, &ctx.sde)
                .with_character_skills(char_skills.clone());
            for it in snap.items.values() {
                if it.location_id == ship_id && matches!(it.flag as u16, 11..=34 | 92..=94 | 125..=132) {
                    let is_on = snap.module_online_states.get(&it.item_id).copied().unwrap_or(true);
                    let _ = engine.fit_module_with_state(it.item_id, it.type_id, it.flag as u16, is_on, &ctx.sde);
                }
            }

            if let Some(sh) = snap.items.get_mut(&ship_id) {
                sh.singleton = true;
                sh.version += 1;
            }
            let mut ship_fitted = false;
            for &(tid, flag, req_qty) in &desired_items {
                let flag_u16 = flag as u16;
                let is_mod = matches!(flag_u16, 11..=34 | 92..=94 | 125..=132);
                let matching_stacks: Vec<(i64, i32)> = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && i.location_id == station_id
                            && i.flag == InventoryFlag::Hangar
                            && i.type_id == tid
                            && i.quantity > 0
                    })
                    .map(|i| (i.item_id, i.quantity))
                    .collect();
                let total_avail: i64 = matching_stacks.iter().map(|(_, q)| *q as i64).sum();
                if total_avail < req_qty as i64 {
                    *missing.entry(tid).or_insert(0) += (req_qty as i64) - total_avail;
                    continue;
                }
                let mut online = true;
                if is_mod {
                    if let Some(m_type) = ctx.sde.types.get(&tid) {
                        for (sk_attr, lv_attr) in [
                            (eve_sde::attr_ids::REQUIRED_SKILL_1, eve_sde::attr_ids::REQUIRED_SKILL_1_LEVEL),
                            (eve_sde::attr_ids::REQUIRED_SKILL_2, eve_sde::attr_ids::REQUIRED_SKILL_2_LEVEL),
                            (eve_sde::attr_ids::REQUIRED_SKILL_3, eve_sde::attr_ids::REQUIRED_SKILL_3_LEVEL),
                        ] {
                            if let Some(&sk_f) = m_type.default_attributes.get(&sk_attr) {
                                let sk_id = sk_f as u32;
                                if sk_id > 0 {
                                    let req_lvl = m_type.default_attributes.get(&lv_attr).copied().unwrap_or(1.0) as u8;
                                    let trained = char_skills.get(&sk_id).copied().unwrap_or(0);
                                    if trained < req_lvl {
                                        online = false;
                                        break;
                                    }
                                }
                            }
                        }
                    }
                    let dummy_mod_id = snap.next_entity_id + 10_000 + flag_u16 as i64;
                    if online {
                        if engine.fit_module_with_state(dummy_mod_id, tid, flag_u16, true, &ctx.sde).is_err() {
                            online = false;
                            let _ = engine.fit_module_with_state(dummy_mod_id, tid, flag_u16, false, &ctx.sde);
                        }
                    } else {
                        let _ = engine.fit_module_with_state(dummy_mod_id, tid, flag_u16, false, &ctx.sde);
                    }
                }
                let mut to_consume = req_qty;
                for (cid, avail) in matching_stacks {
                    if to_consume <= 0 {
                        break;
                    }
                    let take = avail.min(to_consume);
                    if take == avail && (!is_mod || take == 1) {
                        if let Some(it) = snap.items.get_mut(&cid) {
                            it.location_id = ship_id;
                            it.flag = flag;
                            it.singleton = is_mod || take == 1;
                            it.version += 1;
                            moved_items.push(it.clone());
                            if is_mod {
                                snap.module_online_states.insert(cid, online);
                            }
                        }
                    } else {
                        if let Some(orig) = snap.items.get_mut(&cid) {
                            orig.quantity -= take;
                            orig.version += 1;
                        }
                        snap.next_entity_id += 1;
                        let split_id = snap.next_entity_id;
                        let new_item = eve_db::InventoryItemRecord {
                            item_id: split_id,
                            type_id: tid,
                            owner_id: char_id,
                            location_id: ship_id,
                            flag,
                            quantity: take,
                            singleton: is_mod || take == 1,
                            version: 1,
                            ..Default::default()
                        };
                        snap.items.insert(split_id, new_item.clone());
                        moved_items.push(new_item);
                        if is_mod {
                            snap.module_online_states.insert(split_id, online);
                        }
                    }
                    to_consume -= take;
                }
                ship_fitted = true;
            }
            if ship_fitted {
                fitted_count += 1;
            }
        }
    });

    for &ship_id in &target_ship_ids {
        ctx.invalidate_ship_fitting(ship_id);
    }
    if fitted_count > 0 && !moved_items.is_empty() {
        let items_py: Vec<PyRep> = moved_items
            .iter()
            .map(|item| {
                PyRep::keyval([
                    ("itemID", PyRep::Integer(item.item_id)),
                    ("typeID", PyRep::Integer(item.type_id as i64)),
                    ("locationID", PyRep::Integer(item.location_id)),
                    ("flagID", PyRep::Integer(item.flag as i64)),
                    ("quantity", PyRep::Integer(item.quantity as i64)),
                ])
            })
            .collect();
        ctx.notify(
            crate::router::NotificationTarget::Character(char_id),
            "OnMultipleItemChange",
            "charid",
            vec![
                PyRep::List(items_py),
                PyRep::Dict(vec![
                    (PyRep::Integer(3), PyRep::Integer(station_id)),
                    (PyRep::Integer(4), PyRep::Integer(eve_db::InventoryFlag::Hangar as i64)),
                ]),
            ],
        );
    }
    let first_ship = target_ship_ids.first().copied().unwrap_or(0);
    let missing_entries: Vec<(PyRep, PyRep)> = missing
        .into_iter()
        .map(|(t, q)| (PyRep::Integer(t as i64), PyRep::Integer(q)))
        .collect();
    Some(Ok(PyRep::keyval([
        ("shipID", PyRep::Integer(first_ship)),
        ("fittedCount", PyRep::Integer(fitted_count)),
        ("missingItems", PyRep::Dict(missing_entries)),
    ])))
}
