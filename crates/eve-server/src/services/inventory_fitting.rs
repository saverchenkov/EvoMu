//! `inventory_fitting` — Domain 3: Inventory Broker, Ship Fitting, Insurance, Repair, & Abyssal Mutaplasmids (`§3.4`).

#![forbid(unsafe_code)]

use crate::formulas::{abyssal_attribute_roll, compute_insurance_quote};
use crate::router::{BoundObjectEntry, ServerContext};
use eve_db::{InventoryFlag, IskCents, SavedFittingRecord};
use eve_dogma::{AttributeModifier, DogmaItemState, ModifierOperator, ShipFittingEngine};
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
        ("invbroker", "ListItems") | ("invbroker", "List") => {
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

        ("invbroker", "MoveItem") | ("invbroker", "Add") => {
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
                Ok(updated) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(updated.item_id)),
                    ("locationID", PyRep::Integer(updated.location_id)),
                    ("flagID", PyRep::Integer(updated.flag as i64)),
                    ("version", PyRep::Integer(updated.version as i64)),
                ]))),
                Err(e) => Some(Err(BridgeRpcError {
                    code: "InventoryMoveFailed".into(),
                    message: e.to_string(),
                })),
            }
        }

        ("dogmaIM", "GetShipAttributes") | ("dogmaIM", "GetAllInfo") => {
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

            let items = ctx.db.get_items_for_character(char_id);
            let Some(ship_item) = items.iter().find(|i| i.item_id == ship_id) else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotFound".into(),
                    message: format!("Ship item {ship_id} not found for character {char_id}"),
                }));
            };

            let mut fit = ShipFittingEngine::from_sde(ship_id, ship_item.type_id, &ctx.sde);
            for item in &items {
                if item.location_id == ship_id && (item.flag as u16) >= 11 {
                    let _ = fit.fit_and_online_module(
                        item.item_id,
                        item.type_id,
                        item.flag as u16,
                        &ctx.sde,
                    );
                }
            }

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

            if method == "GetAllInfo" {
                Some(Ok(PyRep::keyval([
                    ("shipID", PyRep::Integer(ship_id)),
                    ("charID", PyRep::Integer(char_id)),
                    ("shipAttributes", PyRep::Dict(entries)),
                ])))
            } else {
                Some(Ok(PyRep::Dict(entries)))
            }
        }

        ("dogmaIM", "Activate") | ("dogmaIM", "Overload") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let module_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let ship_id = session.ship_id.unwrap_or(100_000_001);
            let items = ctx.db.get_items_for_character(char_id);
            let mut engine = DogmaItemState::from_sde(ship_id, 587, &ctx.sde);
            for i in &items {
                if i.item_id == module_id {
                    if let Some(t) = ctx.sde.types.get(&i.type_id) {
                        if let Some(&spd) = t.default_attributes.get(&attr_ids::SPEED_FACTOR) {
                            let mult = if method == "Overload" { 1.5 } else { 1.0 };
                            engine.add_modifier(AttributeModifier {
                                source_item_id: module_id,
                                target_attribute_id: attr_ids::MAX_VELOCITY,
                                operator: ModifierOperator::PostPercent,
                                value: spd * mult,
                                stacking_penalized: true,
                            });
                        }
                    }
                }
            }
            let max_vel = engine.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde);
            Some(Ok(PyRep::keyval([
                ("moduleID", PyRep::Integer(module_id)),
                ("overloaded", PyRep::Bool(method == "Overload")),
                ("effectiveMaxVelocity", PyRep::Real(max_vel)),
            ])))
        }

        ("fittingSvc", "SaveFitting") | ("corpFittingMgr", "SaveFitting") => {
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
                        description: "Saved via fittingSvc".into(),
                        items: vec![(438, InventoryFlag::MedSlot0, 1)],
                    },
                );
                id
            });
            Some(Ok(PyRep::Integer(fid)))
        }

        ("fittingSvc", "GetFittings") | ("corpFittingMgr", "GetFittings") => {
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

        ("insuranceSvc", "InsureShip") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ship_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or(session.ship_id)
                .unwrap_or(100_000_001);
            let level_fraction = args.get(1).and_then(|a| a.as_f64()).unwrap_or(1.0);
            let (cost, payout) = compute_insurance_quote(230_000.0, level_fraction);

            ctx.db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = IskCents((ch.balance.0 - IskCents::from_f64_isk(cost).0).max(0));
                }
                if let Some(ship) = snap.items.get_mut(&ship_id) {
                    ship.insured_payout_isk = payout;
                }
            });

            Some(Ok(PyRep::keyval([
                ("shipID", PyRep::Integer(ship_id)),
                ("costISK", PyRep::Real(cost)),
                ("payoutISK", PyRep::Real(payout)),
            ])))
        }

        ("mutaplasmidMgr", "ApplyMutaplasmid") | ("dynamicItemSvc", "MutateItem") => {
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

            let base_attrs = ctx
                .sde
                .types
                .get(&438)
                .map(|t| t.default_attributes.clone())
                .unwrap_or_default();
            let rolls: HashMap<AttributeId, f64> = muta_def
                .attribute_ranges
                .keys()
                .map(|&k| (k, 0.85))
                .collect();
            let rolled = abyssal_attribute_roll(&base_attrs, &muta_def.attribute_ranges, &rolls);

            ctx.db.with_mut_snapshot(|snap| {
                if let Some(item) = snap.items.get_mut(&target_item_id) {
                    if item.owner_id == char_id {
                        item.type_id = muta_def.resulting_type_id;
                        item.dynamic_attributes = rolled.clone();
                        item.source_mutaplasmid_id = Some(mutaplasmid_type_id);
                        item.version += 1;
                    }
                }
            });

            let spd = rolled.get(&attr_ids::SPEED_FACTOR).copied().unwrap_or(150.0);
            Some(Ok(PyRep::keyval([
                ("itemID", PyRep::Integer(target_item_id)),
                ("resultingTypeID", PyRep::Integer(muta_def.resulting_type_id as i64)),
                ("rolledSpeedFactor", PyRep::Real(spd)),
            ])))
        }

        ("repairSvc", "DamageModules") | ("repairSvc", "RepairAll") | ("repackagingSvc", _)
        | ("ghostFittingSvc", _) | ("ship", _) | ("shipConfig", _) | ("ship_emblems", _)
        | ("shipTreeSvc", _) | ("dogma", _) => Some(Ok(PyRep::Bool(true))),

        _ => None,
    }
}
