//! `inventory_fitting` — Domain 3: Inventory Broker, Ship Fitting, Insurance, Repair, & Abyssal Mutaplasmids (`§3.4`).

#![forbid(unsafe_code)]

use crate::formulas::{abyssal_attribute_roll, compute_insurance_quote};
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{InventoryFlag, IskCents, SavedFittingRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{attr_ids, AttributeId, TypeId};
use std::collections::HashMap;

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("invbroker", "ListItems")
        | ("invbroker", "List")
        | ("invbroker", "GetContainerContents") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let items = ctx.db.get_items_for_character(char_id);
            let rows = items
                .into_iter()
                .map(|i| {
                    vec![
                        PyRep::Integer(i.item_id),
                        PyRep::Integer(i.type_id as i64),
                        PyRep::Integer(i.owner_id),
                        PyRep::Integer(i.location_id),
                        PyRep::Integer(i.flag as i64),
                        PyRep::Integer(i.quantity as i64),
                        PyRep::Integer(i.version as i64),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec![
                    "itemID",
                    "typeID",
                    "ownerID",
                    "locationID",
                    "flagID",
                    "quantity",
                    "version",
                ],
                rows,
            )))
        }

        ("invbroker", "MoveItem")
        | ("invbroker", "Add")
        | ("invbroker", "DeliverToCorpHangar")
        | ("invbroker", "DeliverToCorpMember") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "MoveItem requires itemID".into(),
                }));
            };
            let new_loc = args
                .get(1)
                .and_then(|a| a.as_i64())
                .or_else(|| bound_entry.map(|b| b.object_id))
                .unwrap_or_else(|| session.ship_id.unwrap_or(60_003_760));
            let flag = args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("flag").and_then(|v| v.as_i64()))
                .map(|v| InventoryFlag::from_u16(v as u16))
                .unwrap_or(InventoryFlag::Cargo);
            let expected_version = kwargs
                .get("expected_version")
                .and_then(|v| v.as_i64())
                .map(|v| v as u64);
            let idem_key = kwargs.get("idempotency_key").and_then(|v| v.as_str());

            match ctx.db.move_item_transactional(
                char_id,
                item_id,
                new_loc,
                flag,
                expected_version,
                idem_key,
            ) {
                Ok(updated) => {
                    if let Some(ship_id) = session.ship_id {
                        ctx.invalidate_ship_fitting(ship_id);
                    }
                    ctx.invalidate_ship_fitting(new_loc);
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(updated.item_id)),
                        ("locationID", PyRep::Integer(updated.location_id)),
                        ("flagID", PyRep::Integer(updated.flag as i64)),
                        ("version", PyRep::Integer(updated.version as i64)),
                    ])))
                }
                Err(e) => Some(Err(BridgeRpcError {
                    code: "InventoryMoveFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("dogmaIM", "GetShipAttributes")
        | ("dogmaIM", "GetAllInfo")
        | ("dogmaIM", "CharGetInfo")
        | ("dogmaIM", "ShipGetInfo")
        | ("dogmaIM", "ItemGetInfo")
        | ("dogmaIM", "GetRequiredSkillLevels") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = bound_entry
                .map(|b| b.object_id)
                .or(session.ship_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };

            let fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };

            let base_attrs = fit.ship.base_attributes.clone();
            let mut entries = Vec::with_capacity(base_attrs.len() + 2);
            for (&attr_id, _) in &base_attrs {
                let val = fit.ship.get_attribute(attr_id, &ctx.sde);
                entries.push((PyRep::Integer(attr_id as i64), PyRep::Real(val)));
            }
            entries.push((
                PyRep::Integer(attr_ids::CPU_LOAD as i64),
                PyRep::Real(fit.ship.get_attribute(attr_ids::CPU_LOAD, &ctx.sde)),
            ));
            entries.push((
                PyRep::Integer(attr_ids::POWER_LOAD as i64),
                PyRep::Real(fit.ship.get_attribute(attr_ids::POWER_LOAD, &ctx.sde)),
            ));
            ctx.store_ship_fitting(ship_id, fit);

            if method == "GetAllInfo" || method == "ShipGetInfo" || method == "CharGetInfo" {
                Some(Ok(PyRep::keyval([
                    ("shipID", PyRep::Integer(ship_id)),
                    ("charID", PyRep::Integer(char_id)),
                    ("shipAttributes", PyRep::Dict(entries)),
                ])))
            } else {
                Some(Ok(PyRep::Dict(entries)))
            }
        }

        ("dogmaIM", "Activate")
        | ("dogmaIM", "Overload")
        | ("dogmaIM", "Deactivate")
        | ("dogmaIM", "SetModuleOnline")
        | ("dogmaIM", "LoadAmmo")
        | ("dogmaIM", "LinkWeapons")
        | ("dogmaIM", "LinkAllWeapons") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "No active ship in session".into(),
                }));
            };
            let items = ctx.db.get_items_for_character(char_id);
            let Some(mod_item) = items
                .iter()
                .find(|i| i.item_id == module_id && i.owner_id == char_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "ModuleNotFound".into(),
                    message: format!("Module {module_id} not owned by character {char_id}"),
                }));
            };
            if mod_item.location_id != ship_id || (mod_item.flag as u16) < 11 {
                return Some(Err(BridgeRpcError {
                    code: "ModuleNotFitted".into(),
                    message: format!(
                        "Module {module_id} must be fitted to active ship {ship_id} before activation"
                    ),
                }));
            }

            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };

            if method == "Deactivate" {
                if let Err(e) = fit.deactivate_module(module_id, &ctx.sde) {
                    return Some(Err(BridgeRpcError {
                        code: "DogmaDeactivateFailed".into(),
                        message: e.to_string(),
                    }));
                }
            } else if method == "Activate" || method == "Overload" {
                if let Err(e) = fit.activate_module(module_id, method == "Overload", &ctx.sde) {
                    return Some(Err(BridgeRpcError {
                        code: "DogmaActivateFailed".into(),
                        message: e.to_string(),
                    }));
                }
            }

            let max_vel = fit.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
            ctx.store_ship_fitting(ship_id, fit);

            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("overloaded", PyRep::Bool(method == "Overload")),
                ("effectiveMaxVelocity", PyRep::Real(max_vel)),
            ])))
        }

        ("charFittingMgr", "SaveFitting") | ("corpFittingMgr", "SaveFitting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let name = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("Rifter Combat Fit");
            let ship_type_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(587) as TypeId;
            let fid = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                snap.saved_fittings.insert(
                    id,
                    SavedFittingRecord {
                        fitting_id: id,
                        owner_id: char_id,
                        name: name.to_string(),
                        ship_type_id,
                        description: "Saved via charFittingMgr".into(),
                        items: vec![(438, InventoryFlag::MedSlot0, 1)],
                    },
                );
                id
            });
            Some(Ok(PyRep::Integer(fid)))
        }

        ("charFittingMgr", "GetFittings")
        | ("corpFittingMgr", "GetFittings")
        | ("corpFittingMgr", "GetCommunityFittings") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx.db.with_snapshot(|snap| {
                snap.saved_fittings
                    .values()
                    .filter(|f| f.owner_id == char_id)
                    .map(|f| {
                        vec![
                            PyRep::Integer(f.fitting_id),
                            PyRep::String(f.name.clone()),
                            PyRep::Integer(f.ship_type_id as i64),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec!["fittingID", "name", "shipTypeID"],
                rows,
            )))
        }

        ("charFittingMgr", "DeleteFitting") | ("corpFittingMgr", "DeleteFitting") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let fid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let deleted = ctx.db.with_mut_snapshot(|snap| {
                if snap
                    .saved_fittings
                    .get(&fid)
                    .map(|f| f.owner_id == char_id)
                    .unwrap_or(false)
                {
                    snap.saved_fittings.remove(&fid);
                    true
                } else {
                    false
                }
            });
            Some(Ok(PyRep::Bool(deleted)))
        }

        ("insuranceSvc", "InsureShip")
        | ("insuranceSvc", "GetContractForShip")
        | ("insuranceSvc", "GetContracts")
        | ("insuranceSvc", "GetInsurancePrice")
        | ("insuranceSvc", "GetInsurancePrices") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.ship_id)
                .unwrap_or(100_000_001);
            let level_fraction = args
                .get(1)
                .and_then(|a| a.as_f64())
                .unwrap_or(1.0)
                .clamp(0.0, 1.0);

            let res = ctx.db.with_mut_snapshot(|snap| {
                let Some(ship) = snap.items.get(&ship_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ShipNotFound".into(),
                        message: format!("Ship {ship_id} not found"),
                    });
                };
                if ship.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "OwnershipViolation".into(),
                        message: format!("Ship {ship_id} does not belong to character {char_id}"),
                    });
                }
                let base_price = ctx
                    .sde
                    .types
                    .get(&ship.type_id)
                    .map(|t| t.base_price)
                    .unwrap_or(230_000.0);
                let (cost, payout) = compute_insurance_quote(base_price, level_fraction);
                let cost_cents = IskCents::from_f64_isk(cost);

                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let new_bal = ch.balance.checked_sub(cost_cents).ok_or_else(|| BridgeRpcError {
                    code: "InsufficientFunds".into(),
                    message: "Not enough ISK to insure ship".into(),
                })?;
                ch.balance = new_bal;
                if let Some(ship_mut) = snap.items.get_mut(&ship_id) {
                    ship_mut.insured_payout_isk = payout;
                }
                Ok((cost, payout))
            });

            match res {
                Ok((cost, payout)) => Some(Ok(PyRep::keyval([
                    ("shipID", PyRep::Integer(ship_id)),
                    ("costISK", PyRep::Real(cost)),
                    ("payoutISK", PyRep::Real(payout)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("dynamicItemService", "MutateItem")
        | ("dynamicItemService", "CreateDynamicItem")
        | ("dynamicItemService", "GetDynamicItemInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mutaplasmid_type_id =
                args.first().and_then(|a| a.as_i64()).unwrap_or(47702) as TypeId;
            let target_item_id = args.get(1).and_then(|a| a.as_i64()).unwrap_or(100_000_003);

            let Some(muta_def) = ctx.sde.dynamic_item_attributes.get(&mutaplasmid_type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMutaplasmid".into(),
                    message: format!("Unknown mutaplasmid type {mutaplasmid_type_id}"),
                }));
            };

            let res = ctx.db.with_mut_snapshot(|snap| {
                // Verify target item exists, belongs to char_id, and has an applicable type_id
                let Some(target_item) = snap.items.get(&target_item_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "TargetItemNotFound".into(),
                        message: format!("Target item {target_item_id} not found"),
                    });
                };
                if target_item.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "OwnershipViolation".into(),
                        message: format!(
                            "Target item {target_item_id} is not owned by character {char_id}"
                        ),
                    });
                }
                if !muta_def.applicable_type_ids.contains(&target_item.type_id) {
                    return Err(BridgeRpcError {
                        code: "InapplicableMutaplasmid".into(),
                        message: format!(
                            "Mutaplasmid {mutaplasmid_type_id} cannot be applied to item type {}",
                            target_item.type_id
                        ),
                    });
                }

                // Locate and consume 1 mutaplasmid item owned by char_id
                let muta_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.owner_id == char_id
                            && i.type_id == mutaplasmid_type_id
                            && i.quantity >= 1
                    })
                    .map(|i| i.item_id);
                let Some(mid) = muta_item_id else {
                    return Err(BridgeRpcError {
                        code: "MutaplasmidItemNotFound".into(),
                        message: format!(
                            "Character does not own mutaplasmid type {mutaplasmid_type_id}"
                        ),
                    });
                };

                if let Some(m_item) = snap.items.get_mut(&mid) {
                    if m_item.quantity > 1 {
                        m_item.quantity -= 1;
                        m_item.version += 1;
                    } else {
                        snap.items.remove(&mid);
                    }
                }

                let base_attrs = ctx
                    .sde
                    .types
                    .get(&target_item.type_id)
                    .map(|t| t.default_attributes.clone())
                    .unwrap_or_default();
                let rolls: HashMap<AttributeId, f64> = muta_def
                    .attribute_ranges
                    .keys()
                    .map(|&k| (k, 0.85))
                    .collect();
                let rolled =
                    abyssal_attribute_roll(&base_attrs, &muta_def.attribute_ranges, &rolls);

                if let Some(item) = snap.items.get_mut(&target_item_id) {
                    item.type_id = muta_def.resulting_type_id;
                    item.dynamic_attributes = rolled.clone();
                    item.source_mutaplasmid_id = Some(mutaplasmid_type_id);
                    item.version += 1;
                }
                Ok(rolled)
            });

            match res {
                Ok(rolled) => {
                    let spd = rolled
                        .get(&attr_ids::SPEED_FACTOR)
                        .copied()
                        .unwrap_or(150.0);
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(target_item_id)),
                        (
                            "resultingTypeID",
                            PyRep::Integer(muta_def.resulting_type_id as i64),
                        ),
                        ("rolledSpeedFactor", PyRep::Real(spd)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("repackagingSvc", "RepackageItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() && session.structure_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Repackaging items requires being docked".into(),
                }));
            }
            let item_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let res = ctx.db.with_mut_snapshot(|snap| {
                let Some(item) = snap.items.get_mut(&item_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Item {item_id} not found"),
                    });
                };
                if item.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "OwnershipViolation".into(),
                        message: format!("Item {item_id} is not owned by character {char_id}"),
                    });
                }
                item.singleton = false;
                item.version += 1;
                Ok(item.version)
            });
            match res {
                Ok(ver) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(item_id)),
                    ("singleton", PyRep::Bool(false)),
                    ("version", PyRep::Integer(ver as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("repairSvc", "RepairItems")
        | ("repairSvc", "GetRepairQuote")
        | ("repairSvc", "RepairAll")
        | ("repairSvc", "DamageModules")
        | ("repairSvc", "SetSessionCheck")
        | ("ship", "AssembleShip")
        | ("ship", "Board") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = session.ship_id.unwrap_or(100_000_001);
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("shipID", PyRep::Integer(ship_id)),
                ("repairCostISK", PyRep::Real(0.0)),
                ("repaired", PyRep::Bool(true)),
            ])))
        }

        _ => None,
    }
}
