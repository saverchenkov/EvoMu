//! `fitting_saved_multifit` — `insuranceSvc`, `dynamicItemService`, `repackagingSvc`, `ship`, `repairSvc`, & `invbroker.SetLabel`.

#![forbid(unsafe_code)]

use super::*;

#[path = "fitting_insurance_repair.rs"]
mod fitting_insurance_repair;

#[inline]
fn rpc_err(code: &str, msg: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: msg.into() }
}

pub(crate) async fn handle_ships_and_services(
    ctx: &ServerContext,
    session: &mut SessionState,
    bound_entry: Option<&BoundObjectEntry>,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("insuranceSvc", "InsureShip") => {
            Some(fitting_insurance_repair::handle_insure_ship(
                ctx, session, args, kwargs,
            ))
        }

        ("insuranceSvc", "GetInsurancePrice") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(arg_id) = args.first().and_then(|a| a.as_i64()).or(session.ship_id) else {
                return Some(Err(rpc_err("NoActiveShip", "GetInsurancePrice requires shipID/typeID argument or active ship in session")));
            };
            let corp_id = session.corporation_id;
            let item_check = ctx.db.with_snapshot(|snap| {
                snap.items.get(&arg_id).map(|i| (i.owner_id, i.type_id))
            });
            let type_id = match item_check {
                Some((owner, tid)) => {
                    if owner != char_id && Some(owner) != corp_id {
                        return Some(Err(rpc_err("OwnershipViolation", format!("Ship {arg_id} is not owned by caller"))));
                    }
                    tid
                }
                None => {
                    let tid = if arg_id > 0 { arg_id as u32 } else { 0 };
                    let is_ship_type = ctx.sde.types.get(&tid).is_some_and(|t| t.published && t.category_id == 6);
                    if is_ship_type {
                        tid
                    } else {
                        return Some(Err(rpc_err("ShipNotFound", format!("Ship/type {arg_id} not found"))));
                    }
                }
            };
            let ref_val = crate::services::industry_pi::industry_rigs::compute_ship_insurance_reference_value_for_ctx(ctx, type_id);
            Some(Ok(PyRep::Real(ref_val)))
        }

        ("insuranceSvc", "GetContractForShip") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = args.first().and_then(|a| a.as_i64()).or(session.ship_id) else {
                return Some(Err(rpc_err("NoActiveShip", "GetContractForShip requires shipID argument or active ship in session")));
            };
            let item_info = ctx.db.with_snapshot(|snap| {
                snap.items.get(&ship_id).map(|i| (i.owner_id, i.type_id, i.insured_payout_isk))
            });
            let Some((owner_id, _type_id, current_payout)) = item_info else {
                return Some(Err(rpc_err("ShipNotFound", format!("Ship {ship_id} not found"))));
            };
            let is_corp_owner = session.corporation_id == Some(owner_id);
            if owner_id != char_id && !is_corp_owner {
                return Some(Err(rpc_err("OwnershipViolation", format!("Ship {ship_id} is not owned by character {char_id}"))));
            }
            if current_payout <= 0.0 {
                return Some(Ok(PyRep::None));
            }
            crate::services::combat_crimewatch::combat_wrecks_killmails::sync_ship_insurance_contracts(ctx, owner_id);
            let contract = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.insurance_contracts.get(&ship_id).cloned()
            });
            let now_unix = ctx.now_unix();
            if let Some(c) = contract {
                if c.end_unix > now_unix {
                    return Some(Ok(PyRep::keyval([
                        ("shipID", PyRep::Integer(c.ship_id)),
                        ("shipTypeID", PyRep::Integer(c.ship_type_id as i64)),
                        ("ownerID", PyRep::Integer(c.owner_id)),
                        ("level", PyRep::String(c.level_name.clone())),
                        ("levelName", PyRep::String(c.level_name.clone())),
                        ("fraction", PyRep::Real(c.level_fraction)),
                        ("costISK", PyRep::Real(c.cost_isk)),
                        ("payoutISK", PyRep::Real(c.payout_isk)),
                        ("startDate", PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(c.start_unix))),
                        ("endDate", PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(c.end_unix))),
                    ])));
                }
            }
            Some(Ok(PyRep::None))
        }

        ("insuranceSvc", "UnInsureShip") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = args.first().and_then(|a| a.as_i64()).or(session.ship_id) else {
                return Some(Err(rpc_err("NoActiveShip", "UnInsureShip requires shipID argument or active ship")));
            };
            let owns_ship = ctx.db.with_snapshot(|snap| {
                snap.items.get(&ship_id).is_some_and(|it| it.owner_id == char_id)
            }) || crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.insurance_contracts.get(&ship_id).is_some_and(|c| c.owner_id == char_id)
            });
            if !owns_ship {
                return Some(Err(rpc_err("OwnershipViolation", format!("Ship {ship_id} is not owned by character {char_id}"))));
            }
            crate::services::combat_crimewatch::combat_wrecks_killmails::sync_ship_insurance_contracts(ctx, char_id);
            let voided = crate::services::combat_crimewatch::combat_wrecks_killmails::void_ship_insurance_contract(ctx, ship_id);
            Some(Ok(PyRep::keyval([
                ("shipID", PyRep::Integer(ship_id)),
                ("voided", PyRep::Bool(voided)),
            ])))
        }

        ("insuranceSvc", "GetContracts") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let is_corp = args.first().and_then(|a| a.as_bool()).unwrap_or(false);
            let owner = if is_corp {
                let corp_id = session.corporation_id.unwrap_or(0);
                if corp_id == 0 {
                    return Some(Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Character does not belong to a corporation".into(),
                    }));
                }
                let has_privs = ctx.db.with_snapshot(|snap| {
                    let (is_ceo, roles) = snap.effective_corp_roles_at(
                        corp_id,
                        char_id,
                        None,
                        session.corp_role,
                        ctx.now_unix(),
                    );
                    is_ceo
                        || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR)
                        || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_ACCOUNTANT)
                        || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT)
                });
                if !has_privs {
                    return Some(Err(BridgeRpcError {
                        code: "CorpRoleDenied".into(),
                        message: "Director or accountant role required to view corporation insurance contracts".into(),
                    }));
                }
                corp_id
            } else {
                char_id
            };
            crate::services::combat_crimewatch::combat_wrecks_killmails::sync_ship_insurance_contracts(ctx, owner);
            let rows = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.insurance_contracts
                    .values()
                    .filter(|c| c.owner_id == owner)
                    .map(|c| {
                        PyRep::keyval([
                            ("shipID", PyRep::Integer(c.ship_id)),
                            ("shipTypeID", PyRep::Integer(c.ship_type_id as i64)),
                            ("ownerID", PyRep::Integer(c.owner_id)),
                            ("fraction", PyRep::Real(c.level_fraction)),
                            ("costISK", PyRep::Real(c.cost_isk)),
                            ("payoutISK", PyRep::Real(c.payout_isk)),
                            ("startDate", PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(c.start_unix))),
                            ("endDate", PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(c.end_unix))),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(rows)))
        }

        ("insuranceSvc", "GetInsurancePrices") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if let Some(dock_id) = session.station_id.or(session.structure_id) {
                if dock_id < 1_000_000_000_000
                    && !crate::services::station_structure::npc_station_offers_service(&ctx.sde, dock_id, 21)
                {
                    return Some(Err(BridgeRpcError {
                        code: "StationServiceUnavailable".into(),
                        message: format!("Station {dock_id} does not offer insurance"),
                    }));
                }
            }
            let mut ship_type_ids: Vec<TypeId> = match args.first() {
                Some(PyRep::List(items)) | Some(PyRep::Tuple(items)) => items
                    .iter()
                    .filter_map(|v| v.as_i64().map(|id| id as TypeId))
                    .collect(),
                Some(PyRep::Integer(id)) => vec![*id as TypeId],
                _ => Vec::new(),
            };
            if ship_type_ids.is_empty() {
                let owned_types = ctx.db.with_snapshot(|snap| {
                    snap.items
                        .values()
                        .filter(|i| i.owner_id == char_id && i.singleton)
                        .map(|i| i.type_id)
                        .collect::<Vec<_>>()
                });
                if !owned_types.is_empty() {
                    ship_type_ids = owned_types;
                } else if let Some(active_ship_id) = session.ship_id {
                    if let Some(t) = ctx.db.with_snapshot(|snap| snap.items.get(&active_ship_id).map(|i| i.type_id)) {
                        ship_type_ids.push(t);
                    }
                }
            }
            if ship_type_ids.is_empty() {
                let mut types: Vec<_> = ctx
                    .sde
                    .types
                    .values()
                    .filter(|t| t.category_id == 6 && t.published && t.base_price > 0.0)
                    .map(|t| t.type_id)
                    .collect();
                types.sort();
                ship_type_ids = types.into_iter().take(50).collect();
            }

            // V24 `InsuranceSvc.PrimeInsurancePrices` calls `self.insurancePrice.update(pricesByTypeIDs)`,
            // expecting `{typeID: referencePrice}` matching `GetInsurancePrice(typeID)`.
            let mut seen_types = std::collections::HashSet::new();
            ship_type_ids.retain(|tid| seen_types.insert(*tid));
            let live_prices = ctx.db.with_snapshot(|snap| crate::services::industry_pi::industry_rigs::collect_live_market_adjusted_prices(snap));
            let entries: Vec<(PyRep, PyRep)> = ship_type_ids
                .into_iter()
                .filter_map(|type_id| {
                    let ref_val = crate::services::industry_pi::industry_rigs::compute_ship_insurance_reference_value_with_prices(&ctx.sde, &live_prices, type_id);
                    if ref_val <= 0.0 {
                        return None;
                    }
                    Some((PyRep::Integer(type_id as i64), PyRep::Real(ref_val)))
                })
                .collect();

            Some(Ok(PyRep::Dict(entries)))
        }

        ("dynamicItemService", "CreateDynamicItem") | ("dynamicItemSvc", "CreateDynamicItem") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(raw_muta) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMutaplasmid".into(),
                    message: "CreateDynamicItem requires mutaplasmid typeID".into(),
                }));
            };
            let mutaplasmid_type_id = if let Some(it) = ctx.db.with_snapshot(|s| s.items.get(&raw_muta).cloned()) {
                it.type_id
            } else {
                raw_muta as TypeId
            };
            let Some(target_item_id) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "TargetItemNotFound".into(),
                    message: "CreateDynamicItem requires target itemID".into(),
                }));
            };

            let Some(muta_def) = ctx.sde.dynamic_item_attributes.get(&mutaplasmid_type_id) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMutaplasmid".into(),
                    message: format!("Unknown mutaplasmid type {mutaplasmid_type_id}"),
                }));
            };

            let current_dock = session.station_id.or(session.structure_id);
            let current_ship = session.ship_id;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                // Verify target item exists, belongs to char_id, is unlocked & reachable, and has an applicable type_id
                let Some(target_item) = snap.items.get(&target_item_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "TargetItemNotFound".into(),
                        message: format!("Target item {target_item_id} not found"),
                    });
                };
                if target_item.owner_id != char_id || target_item.quantity <= 0 {
                    return Err(BridgeRpcError {
                        code: "OwnershipViolation".into(),
                        message: format!(
                            "Target item {target_item_id} is not owned by character {char_id}"
                        ),
                    });
                }
                let active_ship_colocated = current_ship
                    .map(|sid| snap.is_ship_colocated_with_character(char_id, sid, current_dock))
                    .unwrap_or(false);
                let is_fitted_slot = matches!(
                    target_item.flag as u16,
                    11..=34 | 87 | 92..=94 | 125..=132 | 158
                );
                if is_fitted_slot {
                    return Err(BridgeRpcError {
                        code: "CannotMutateFittedItem".into(),
                        message: format!(
                            "Target item {target_item_id} is currently fitted in slot {:?} and must be unfitted to Cargo or Hangar before mutating",
                            target_item.flag
                        ),
                    });
                }
                let target_reachable = (current_dock.is_some()
                    && Some(target_item.location_id) == current_dock
                    && target_item.flag == InventoryFlag::Hangar)
                    || (active_ship_colocated
                        && Some(target_item.location_id) == current_ship
                        && target_item.flag != InventoryFlag::Locked);
                if target_item.flag == InventoryFlag::Locked
                    || !target_reachable
                    || snap.is_item_locked_in_contract_or_job(target_item_id)
                {
                    return Err(BridgeRpcError {
                        code: "TargetItemLockedOrUnreachable".into(),
                        message: format!(
                            "Target item {target_item_id} is locked in a contract/job or not at the caller's current station/structure or active ship"
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

                // Locate and consume 1 unlocked, reachable mutaplasmid item owned by char_id
                let muta_item_id = snap
                    .items
                    .values()
                    .find(|i| {
                        i.type_id == mutaplasmid_type_id
                            && i.quantity >= 1
                            && snap.is_item_consumable_by(i, char_id, current_dock, current_ship)
                    })
                    .map(|i| i.item_id);
                let Some(mid) = muta_item_id else {
                    return Err(BridgeRpcError {
                        code: "MutaplasmidItemNotFound".into(),
                        message: format!(
                            "Character does not own an unlocked, reachable mutaplasmid type {mutaplasmid_type_id}"
                        ),
                    });
                };

                let mut muta_opts = eve_db::ConsumeExactOptions::for_owner_exact(Some(char_id), char_id);
                muta_opts.active_ship_id = current_ship;
                snap.consume_exact_item_quantity(mid, 1, muta_opts).map_err(|e| BridgeRpcError {
                    code: "MutaplasmidItemNotFound".into(),
                    message: e.to_string(),
                })?;

                let base_attrs = ctx
                    .sde
                    .types
                    .get(&target_item.type_id)
                    .map(|t| t.default_attributes.clone())
                    .unwrap_or_default();
                let now_unix = ctx.now_unix();
                let mut sorted_attr_keys: Vec<AttributeId> =
                    muta_def.attribute_ranges.keys().copied().collect();
                sorted_attr_keys.sort_unstable();
                let rolls: HashMap<AttributeId, f64> = sorted_attr_keys
                    .into_iter()
                    .map(|k| {
                        (
                            k,
                            ctx.next_invention_roll(
                                (target_item_id as u64).wrapping_add(k as u64),
                                char_id as u64,
                                now_unix,
                            ),
                        )
                    })
                    .collect();
                let rolled =
                    abyssal_attribute_roll(&base_attrs, &muta_def.attribute_ranges, &rolls);

                let mutated_item_id = if target_item.quantity > 1 {
                    if let Some(orig) = snap.items.get_mut(&target_item_id) {
                        orig.quantity -= 1;
                        orig.version += 1;
                    }
                    snap.next_entity_id += 1;
                    let new_id = snap.next_entity_id;
                    snap.items.insert(
                        new_id,
                        eve_db::InventoryItemRecord {
                            item_id: new_id,
                            type_id: muta_def.resulting_type_id,
                            owner_id: char_id,
                            location_id: target_item.location_id,
                            flag: target_item.flag,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            dynamic_attributes: rolled.clone(),
                            source_mutaplasmid_id: Some(mutaplasmid_type_id),
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        },
                    );
                    new_id
                } else {
                    if let Some(item) = snap.items.get_mut(&target_item_id) {
                        item.type_id = muta_def.resulting_type_id;
                        item.quantity = 1;
                        item.singleton = true;
                        item.dynamic_attributes = rolled.clone();
                        item.source_mutaplasmid_id = Some(mutaplasmid_type_id);
                        item.version += 1;
                    }
                    target_item_id
                };
                Ok((mutated_item_id, rolled))
            });

            match res {
                Ok((mutated_item_id, rolled)) => {
                    let spd = rolled
                        .get(&attr_ids::SPEED_FACTOR)
                        .copied()
                        .unwrap_or(150.0);
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(mutated_item_id)),
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

        ("dynamicItemService", "GetDynamicItemInfo") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(item_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: "GetDynamicItemInfo requires itemID".into(),
                }));
            };
            let res = ctx.db.with_snapshot(|snap| {
                let item = snap.items.get(&item_id).cloned()?;
                let in_contract = snap
                    .contracts
                    .values()
                    .any(|c| c.item_ids.contains(&item_id));
                Some((item, in_contract))
            });
            match res {
                Some((item, in_contract)) => {
                    if item.owner_id != char_id && !in_contract {
                        return Some(Err(BridgeRpcError {
                            code: "OwnershipViolation".into(),
                            message: format!(
                                "Item {item_id} is not owned by character {char_id}"
                            ),
                        }));
                    }
                    let mut attr_entries: Vec<(PyRep, PyRep)> = item
                        .dynamic_attributes
                        .iter()
                        .map(|(&k, &v)| (PyRep::Integer(k as i64), PyRep::Real(v)))
                        .collect();
                    attr_entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
                    let source_type_id = item
                        .source_mutaplasmid_id
                        .and_then(|mid| ctx.sde.dynamic_item_attributes.get(&mid))
                        .and_then(|d| d.applicable_type_ids.first().copied())
                        .unwrap_or(item.type_id);
                    let mutator_type_id = item.source_mutaplasmid_id.unwrap_or(47740) as i64;
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(item.item_id)),
                        ("typeID", PyRep::Integer(item.type_id as i64)),
                        ("sourceTypeID", PyRep::Integer(source_type_id as i64)),
                        ("characterID", PyRep::Integer(item.owner_id)),
                        ("mutatorTypeID", PyRep::Integer(mutator_type_id)),
                        (
                            "mutaplasmidTypeID",
                            item.source_mutaplasmid_id
                                .map(|m| PyRep::Integer(m as i64))
                                .unwrap_or(PyRep::None),
                        ),
                        ("attributes", PyRep::Dict(attr_entries)),
                    ])))
                }
                None => Some(Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Item {item_id} not found"),
                })),
            }
        }

        ("repackagingSvc", "RepackageItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Repackaging items requires being docked".into(),
                }));
            };
            let extract_ids = |list: &[PyRep]| -> Vec<i64> {
                list.iter().filter_map(|v| v.as_i64().or_else(|| match v {
                    PyRep::Tuple(t) | PyRep::List(t) => t.first().and_then(|x| x.as_i64()),
                    _ => None,
                })).collect()
            };
            let item_ids: Vec<i64> = match args.first() {
                Some(PyRep::Integer(id)) => vec![*id],
                Some(PyRep::List(l) | PyRep::Tuple(l)) => extract_ids(l),
                Some(PyRep::Dict(entries)) => entries.iter().flat_map(|(_, val)| match val {
                    PyRep::List(l) | PyRep::Tuple(l) => extract_ids(l),
                    PyRep::Integer(id) => vec![*id],
                    _ => Vec::new(),
                }).collect(),
                _ => vec![0],
            };
            if item_ids.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: "RepackageItems requires at least one itemID".into(),
                }));
            }
            for &item_id in &item_ids {
                if Some(item_id) == session.ship_id {
                    return Some(Err(BridgeRpcError {
                        code: "CannotRepackageActiveShip".into(),
                        message: "Cannot repackage your currently active ship".into(),
                    }));
                }
                let has_module_damage = ctx
                    .ship_fittings
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .get(&item_id)
                    .is_some_and(|fit| fit.rack_heat.iter().any(|&h| h > 0.0) || fit.modules.values().any(|m| m.damage_hp > 0.0));
                if has_module_damage {
                    return Some(Err(BridgeRpcError {
                        code: "CantRepackageDamagedItem".into(),
                        message: format!(
                            "Item {item_id} has heat or module damage and must be repaired before repackaging"
                        ),
                    }));
                }
            }
            let now_unix = ctx.now_unix();
            let confirm_reset = args.get(1).and_then(|a| a.as_bool()).unwrap_or(false);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let active_ship_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.active_ship_id)
                    .unwrap_or(0);
                let (wrap_ids, normal_ids): (Vec<i64>, Vec<i64>) =
                    item_ids.iter().partition(|&&id| snap.items.get(&id).is_some_and(|it| it.type_id == 3468));

                // 1. Validate all normal items before any mutations
                for &item_id in &normal_ids {
                    if item_id == active_ship_id || snap.is_character_boarded_or_saved_ship(char_id, session.ship_id, item_id) {
                        return Err(BridgeRpcError {
                            code: "CannotRepackageActiveShip".into(),
                            message: "Cannot repackage your currently active ship".into(),
                        });
                    }
                    let Some(item) = snap.items.get(&item_id) else {
                        return Err(BridgeRpcError {
                            code: "ItemNotFound".into(),
                            message: format!("Item {item_id} not found"),
                        });
                    };
                    if item.owner_id != char_id {
                        return Err(BridgeRpcError {
                            code: "OwnershipOrLocationViolation".into(),
                            message: format!("Item {item_id} must be owned by character {char_id} at station {station_id}"),
                        });
                    }
                    if matches!(item.flag as u16, 11..=34 | 92..=94 | 125..=132 | 158) {
                        return Err(BridgeRpcError {
                            code: "CanNotUnassembleInThisItemLoc".into(),
                            message: format!("Cannot repackage item {item_id} while it is fitted in slot {:?}", item.flag),
                        });
                    }
                    if snap.items.get(&item.location_id).is_some_and(|p| fitting_containers_ships::is_container_group(ctx.sde.types.get(&p.type_id).map(|t| t.group_id).unwrap_or(0), p.type_id)) {
                        return Err(BridgeRpcError { code: "ItemInsideContainer".into(), message: format!("Item {item_id} is inside a container and must be moved to item hangar before repackaging") });
                    }
                    if item.location_id != station_id {
                        return Err(BridgeRpcError {
                            code: "OwnershipOrLocationViolation".into(),
                            message: format!("Item {item_id} must be owned by character {char_id} at station {station_id}"),
                        });
                    }
                    if item.flag != InventoryFlag::Hangar {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!("Item {item_id} must be in your station Hangar (current flag: {:?}) to repackage", item.flag),
                        });
                    }
                    if snap.is_item_locked_in_contract_or_job(item_id) {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!("Item {item_id} is locked in an active contract or industry job"),
                        });
                    }
                    if fitting_containers_ships::container_has_active_audit_log_snap(snap, item_id, now_unix as u64) {
                        return Err(BridgeRpcError { code: "ContainerHasActiveAuditLog".into(), message: format!("Container {item_id} has audit log entries younger than 3 weeks and cannot be repackaged") });
                    }
                    let has_contained_items = snap.items.values().any(|o| o.item_id != item_id && o.location_id == item_id && o.quantity > 0);
                    if has_contained_items {
                        return Err(BridgeRpcError {
                            code: "CannotRepackageNonEmptyItem".into(),
                            message: format!("Cannot repackage item {item_id} while it contains fitted modules, rigs, or cargo"),
                        });
                    }
                    if item.is_blueprint_copy {
                        return Err(BridgeRpcError {
                            code: "CannotRepackageBlueprintCopy".into(),
                            message: format!("Blueprint copy {item_id} cannot be repackaged"),
                        });
                    }
                    let hull_or_armor_damage = item.dynamic_attributes.get(&attr_ids::DAMAGE).copied().unwrap_or(0.0) > 0.0
                        || item.dynamic_attributes.get(&attr_ids::ARMOR_DAMAGE).copied().unwrap_or(0.0) > 0.0;
                    if hull_or_armor_damage {
                        return Err(BridgeRpcError {
                            code: "CantRepackageDamagedItem".into(),
                            message: format!("Item {item_id} has structural or armor damage and must be repaired before repackaging"),
                        });
                    }
                    let is_researched_bpo = !item.is_blueprint_copy
                        && (item.dynamic_attributes.get(&1112).copied().unwrap_or(0.0) > 0.0
                            || item.dynamic_attributes.get(&1113).copied().unwrap_or(0.0) > 0.0);
                    let has_non_damage_dyn_attrs = item.dynamic_attributes.keys().any(|&k| {
                        k != attr_ids::DAMAGE
                            && k != attr_ids::ARMOR_DAMAGE
                            && k != attr_ids::SHIELD_CHARGE
                            && k != 1112
                            && k != 1113
                    });
                    if item.source_mutaplasmid_id.is_some()
                        || has_non_damage_dyn_attrs
                        || (is_researched_bpo && !confirm_reset)
                    {
                        return Err(BridgeRpcError {
                            code: "CannotRepackageMutatedItem".into(),
                            message: format!("Abyssal mutated or researched item {item_id} cannot be repackaged without confirming reset"),
                        });
                    }
                }

                // 2. Validate and break all wraps atomically
                let mut broken_wraps = Vec::new();
                for &wrap_id in &wrap_ids {
                    let (cid, issuer_id, extracted) =
                        crate::services::contracts_service::break_courier_plastic_wrap(
                            snap, char_id, wrap_id, now_unix,
                        )?;
                    broken_wraps.push((wrap_id, cid, issuer_id, extracted));
                }

                // 3. Mutate normal items
                let mut last_ver = 1_u64;
                for &item_id in &normal_ids {
                    if let Some(item) = snap.items.get_mut(&item_id) {
                        item.dynamic_attributes.remove(&1112);
                        item.dynamic_attributes.remove(&1113);
                        item.singleton = false;
                        item.insured_payout_isk = 0.0;
                        item.version += 1;
                        last_ver = item.version;
                    }
                }

                let first_normal_id = normal_ids.first().copied().unwrap_or(0);
                Ok((first_normal_id, last_ver, normal_ids, broken_wraps))
            });

            match res {
                Ok((first_normal_id, last_ver, normal_ids, broken_wraps)) => {
                    for &(_wrap_id, cid, issuer_id, ref extracted) in &broken_wraps {
                        if !extracted.is_empty() {
                            crate::services::quasar_cosmetics_live::strip_ship_cosmetics_for_ships(&ctx.db, extracted);
                        }
                        if cid > 0 {
                            crate::services::contracts_service::notify_contract(ctx, issuer_id, "OnContractFailed", cid);
                        }
                    }
                    for &item_id in &normal_ids {
                        ctx.invalidate_ship_fitting(item_id);
                        crate::sp6_lifecycle::abort_self_destruct_for_ship(ctx, item_id);
                        let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::void_ship_insurance_contract(ctx, item_id);
                    }
                    if !normal_ids.is_empty() {
                        crate::services::quasar_cosmetics_live::strip_ship_cosmetics_for_ships(&ctx.db, &normal_ids);
                    }
                    let total_count = normal_ids.len() + broken_wraps.len();
                    let (first_wrap, first_cid, first_extracted) = broken_wraps.into_iter().next()
                        .map(|(w, c, _, ext)| (w, c, ext))
                        .unwrap_or((0, 0, Vec::new()));
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(if first_normal_id > 0 { first_normal_id } else { first_wrap })),
                        ("singleton", PyRep::Bool(false)),
                        ("version", PyRep::Integer(last_ver as i64)),
                        ("repackagedCount", PyRep::Integer(total_count as i64)),
                        ("contractID", PyRep::Integer(first_cid)),
                        ("status", PyRep::String(if first_cid > 0 { "Failed".into() } else { "Completed".into() })),
                        ("extractedItemIDs", PyRep::List(first_extracted.into_iter().map(PyRep::Integer).collect())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("ship", "AssembleShip") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Assembling a ship requires being docked".into(),
                }));
            };
            let first_item_id = match args.first() {
                Some(PyRep::Integer(id)) => Some(*id),
                Some(PyRep::List(list) | PyRep::Tuple(list)) => {
                    list.first().and_then(|v| v.as_i64())
                }
                _ => None,
            }
            .or_else(|| kwargs.get("itemID").and_then(|v| v.as_i64()));
            let Some(item_id) = first_item_id.or(session.ship_id) else {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: "AssembleShip requires itemID argument or active ship in session"
                        .into(),
                }));
            };
            let cs_state = fitting_containers_ships::read_container_ship_state(ctx);
            if cs_state.alsc_locked_items.contains(&item_id) {
                return Some(Err(BridgeRpcError {
                    code: "ItemLocked".into(),
                    message: format!("Item {item_id} is locked in an Audit Log Secure Container"),
                }));
            }
            if let Some(corp_id) = ctx.db.with_snapshot(|snap| {
                let mut cur = snap.items.get(&item_id);
                for _ in 0..8 {
                    let Some(it) = cur else { break };
                    if snap.corporations.contains_key(&it.owner_id) {
                        return Some(it.owner_id);
                    }
                    cur = snap.items.get(&it.location_id);
                }
                None
            }) {
                crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(
                    ctx, corp_id,
                );
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if let Some(persisted_dock) = snap
                    .characters
                    .get(&char_id)
                    .and_then(|c| c.station_id.or(c.structure_id))
                {
                    if persisted_dock != station_id {
                        return Err(BridgeRpcError {
                            code: "NotDocked".into(),
                            message: format!(
                                "Character {char_id} is not physically docked at {station_id}"
                            ),
                        });
                    }
                }
                let Some(item) = snap.items.get(&item_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Ship item {item_id} not found"),
                    });
                };
                let corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .or(session.corporation_id)
                    .unwrap_or(0);
                let is_corp_item = corp_id > 0 && item.owner_id == corp_id;
                if item.owner_id != char_id && !is_corp_item {
                    return Err(BridgeRpcError {
                        code: "OwnershipOrLocationViolation".into(),
                        message: format!(
                            "Ship item {item_id} must be owned by character {char_id} at station {station_id}"
                        ),
                    });
                }
                if matches!(item.flag as u16, 11..=34 | 92..=94 | 125..=132 | 158) {
                    return Err(BridgeRpcError {
                        code: "CanNotUnassembleInThisItemLoc".into(),
                        message: format!(
                            "Item {item_id} is currently fitted in slot {:?} and cannot be assembled/repackaged in this location",
                            item.flag
                        ),
                    });
                }
                if item.location_id != station_id || snap.items.contains_key(&station_id) {
                    return Err(BridgeRpcError {
                        code: "OwnershipOrLocationViolation".into(),
                        message: format!(
                            "Ship item {item_id} must be owned by character {char_id} at station {station_id}"
                        ),
                    });
                }
                if item.flag == InventoryFlag::Locked
                    || snap.is_item_locked_in_contract_or_job(item_id)
                {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!("Item {item_id} is locked"),
                    });
                }
                let ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
                if ext.asset_safety_wraps.values().any(|w| {
                    !w.retrieval_fee_paid
                        && (w.wrap_id == item_id
                            || w.wrap_id == item.location_id
                            || w.item_ids.contains(&item_id))
                }) {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!("Item {item_id} is sealed inside an Asset Safety wrap"),
                    });
                }
                if is_corp_item {
                    if !(116..=122).contains(&(item.flag as i64)) {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!(
                                "Corporation ship {item_id} cannot be assembled in flag {:?}",
                                item.flag
                            ),
                        });
                    }
                    crate::services::inventory_bound::validate_corp_source_item_take_access_in_snap(
                        ctx, snap, char_id, corp_id, &item,
                    )?;
                } else if !matches!(item.flag, InventoryFlag::Hangar | InventoryFlag::ShipHangar) {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!(
                            "Ship {item_id} must be in Hangar or ShipHangar to assemble (flag {:?})",
                            item.flag
                        ),
                    });
                }
                let hull_or_armor_damage = item
                    .dynamic_attributes
                    .get(&attr_ids::DAMAGE)
                    .copied()
                    .unwrap_or(0.0)
                    > 0.0
                    || item
                        .dynamic_attributes
                        .get(&attr_ids::ARMOR_DAMAGE)
                        .copied()
                        .unwrap_or(0.0)
                        > 0.0;
                if hull_or_armor_damage {
                    return Err(BridgeRpcError {
                        code: "CantRepackageDamagedItem".into(),
                        message: format!("Item {item_id} is damaged and must be repaired first"),
                    });
                }
                let is_ship = ctx
                    .sde
                    .types
                    .get(&item.type_id)
                    .map(|t| t.category_id == 6)
                    .unwrap_or(false);
                if !is_ship {
                    return Err(BridgeRpcError {
                        code: "NotAShip".into(),
                        message: format!("Item {item_id} (type {}) is not a ship", item.type_id),
                    });
                }
                if !item.singleton && item.quantity > 1 {
                    if let Some(orig) = snap.items.get_mut(&item_id) {
                        orig.quantity -= 1;
                        orig.version += 1;
                    }
                    snap.next_entity_id += 1;
                    let new_ship_id = snap.next_entity_id;
                    snap.items.insert(
                        new_ship_id,
                        eve_db::InventoryItemRecord {
                            item_id: new_ship_id,
                            type_id: item.type_id,
                            owner_id: item.owner_id,
                            location_id: station_id,
                            flag: item.flag,
                            quantity: 1,
                            singleton: true,
                            version: 1,
                            dynamic_attributes: HashMap::new(),
                            source_mutaplasmid_id: None,
                            insured_payout_isk: 0.0,
                            is_blueprint_copy: false,
                            blueprint_runs_remaining: None,
                        },
                    );
                    Ok((new_ship_id, 1_u64, item.type_id))
                } else {
                    let item_mut = snap.items.get_mut(&item_id).unwrap();
                    item_mut.singleton = true;
                    item_mut.version += 1;
                    Ok((item_id, item_mut.version, item_mut.type_id))
                }
            });
            match res {
                Ok((assembled_id, ver, ship_type_id)) => {
                    if ctx
                        .sde
                        .types
                        .get(&ship_type_id)
                        .is_some_and(|t| matches!(t.group_id, 29 | 237))
                    {
                        crate::services::notify_emitters_skills_trade_godma::emit_pod_or_corvette_assembly(
                            ctx,
                            char_id,
                            assembled_id,
                            ship_type_id,
                        );
                    }
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(assembled_id)), ("singleton", PyRep::Bool(true)), ("version", PyRep::Integer(ver as i64)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("repairSvc", "GetRepairQuotes") => {
            Some(crate::services::sov_services::handle_repair_get_quotes(ctx, session, args))
        }
        ("repairSvc", "RepairItems")
        | ("repairSvc", "RepairItemsInStation")
        | ("repairSvc", "RepairItemsInStructure") => {
            let in_struct = method == "RepairItemsInStructure"
                || session.structure_id.is_some();
            Some(crate::services::sov_services::handle_repair_items_in_location(
                ctx,
                session,
                in_struct,
                args,
            ))
        }
        ("repairSvc", "RepairAll") => {
            Some(fitting_insurance_repair::handle_repair_all(ctx, session, args))
        }


        ("invbroker", "SetLabel") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (item_id, raw_label) = if args.len() >= 2 {
                (
                    args.first().and_then(|a| a.as_i64()).or(session.ship_id).unwrap_or(0),
                    args.get(1).and_then(|a| a.as_str()).unwrap_or(""),
                )
            } else {
                (
                    bound_entry
                        .map(|b| b.object_id)
                        .or(session.ship_id)
                        .unwrap_or(0),
                    args.first().and_then(|a| a.as_str()).unwrap_or(""),
                )
            };
            let label = raw_label.trim();
            if label.is_empty() || label.chars().count() > 100 {
                return Some(Err(BridgeRpcError {
                    code: "InvalidLabel".into(),
                    message: "Item/ship custom name must be 1..=100 characters".into(),
                }));
            }
            if label.starts_with("courier_cargo:") {
                return Some(Err(BridgeRpcError {
                    code: "CustomNameNotAllowed".into(),
                    message: "Courier cargo prefix is reserved".into(),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(item) = snap.items.get(&item_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Item {item_id} not found"),
                    });
                };
                if snap.item_custom_names.get(&item_id).map_or(false, |s| s.starts_with("courier_cargo:")) {
                    return Err(BridgeRpcError {
                        code: "CustomNameNotAllowed".into(),
                        message: "Cannot rename courier mission cargo".into(),
                    });
                }
                let is_ship = ctx.sde.types.get(&item.type_id).is_some_and(|t| t.category_id == 6);
                if is_ship && label.chars().count() > 32 {
                    return Err(BridgeRpcError {
                        code: "InvalidLabel".into(),
                        message: "Ship custom name must be 1..=32 characters".into(),
                    });
                }
                if item.owner_id != char_id {
                    let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id);
                    let has_corp_rename_role = corp_id == Some(item.owner_id)
                        && snap.corporations.contains_key(&item.owner_id)
                        && {
                            let (is_ceo, roles) = snap.effective_corp_roles_at(
                                item.owner_id,
                                char_id,
                                None,
                                0,
                                ctx.now_unix(),
                            );
                            // corpRoleDirector (1) or corpRoleEquipmentConfig (2199023255552)
                            is_ceo || (roles & (1 | 2_199_023_255_552)) != 0
                        };
                    if !has_corp_rename_role {
                        return Err(BridgeRpcError {
                            code: "OwnershipViolation".into(),
                            message: format!(
                                "Character {char_id} lacks permission to rename item {item_id}"
                            ),
                        });
                    }
                }
                let space_sys = u32::try_from(item.location_id)
                    .ok()
                    .filter(|s| is_ship && ctx.sde.solar_systems.contains_key(s));
                snap.item_custom_names.insert(item_id, label.to_string());
                Ok(space_sys)
            });
            match res {
                Ok(space_sys) => {
                    ctx.esi_state.set_custom_item_name(item_id, label.to_string());
                    if let Some(sys_id) = space_sys {
                        ctx.notify(
                            crate::router::NotificationTarget::SolarSystem(sys_id),
                            "OnSlimItemChange",
                            "solarsystemid",
                            vec![PyRep::Integer(item_id), PyRep::String(label.to_string())],
                        );
                    }
                    if let Some(it) = ctx.db.with_snapshot(|s| s.items.get(&item_id).cloned()) {
                        let grp = ctx.sde.types.get(&it.type_id).map(|t| t.group_id).unwrap_or(0);
                        if fitting_containers_ships::is_container_group(grp, it.type_id) {
                            fitting_containers_ships::record_container_rename_log(ctx, char_id, item_id, it.type_id);
                        }
                    }
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(item_id)),
                        ("name", PyRep::String(label.to_string())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }


        _ => None,
    }
}

pub const CLIENT_DERIVED_DOGMA_SCATTER_EVENTS: &[&str] = &[
    "OnModuleActivated",
    "OnModuleOnlineChange",
    "OnModuleUnfitted",
    "OnTargets",
];

#[cfg(test)]
#[path = "fitting_saved_multifit_tests.rs"]
mod fitting_saved_multifit_tests;

#[path = "fitting_saved_multifit_cov_unit_tests.rs"]
#[cfg(test)]
mod fitting_saved_multifit_cov_unit_tests;

