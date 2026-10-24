//! `fitting_saved_multifit` — `insuranceSvc`, `dynamicItemService`, `repackagingSvc`, `ship`, `repairSvc`, & `invbroker.SetLabel`.

#![forbid(unsafe_code)]

use super::*;

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
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(ship_id) = args.first().and_then(|a| a.as_i64()).or(session.ship_id) else {
                return Some(Err(rpc_err("NoActiveShip", "InsureShip requires shipID argument or active ship in session")));
            };
            let Some(arg1) = args.get(1).and_then(|a| a.as_f64()) else {
                return Some(Err(BridgeRpcError::user_error("InsureShipInvalidCost")));
            };
            let is_corp_item = args.get(2).and_then(|a| a.as_bool())
                .or_else(|| kwargs.get("isCorpItem").and_then(|a| a.as_bool()))
                .unwrap_or(false);
            let void_old = args.get(3).and_then(|a| a.as_bool())
                .or_else(|| kwargs.get("voidOld").and_then(|a| a.as_bool()))
                .unwrap_or(false);
            let caller_corp_id = session.corporation_id.unwrap_or(0);
            let caller_corp_role = session.corp_role;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(ship) = snap.items.get(&ship_id).cloned() else {
                    return Err(rpc_err("ShipNotFound", format!("Ship {ship_id} not found")));
                };
                if ctx.sde.get_type_category(ship.type_id) != Some(6) {
                    return Err(BridgeRpcError::user_error("InsureShipFailed"));
                }
                if !ship.singleton {
                    return Err(rpc_err("InsureShipNotAssembled", "Ship must be assembled to be insured"));
                }
                let is_corp = is_corp_item || (caller_corp_id != 0 && ship.owner_id == caller_corp_id);
                let owner_id = if is_corp {
                    if caller_corp_id == 0 || ship.owner_id != caller_corp_id {
                        return Err(rpc_err("OwnershipViolation", format!("Ship {ship_id} is not owned by corporation {caller_corp_id}")));
                    }
                    let is_ceo = snap.corporations.get(&caller_corp_id).is_some_and(|c| c.ceo_id == char_id);
                    let roles = snap.corporations.get(&caller_corp_id)
                        .and_then(|c| c.member_roles.get(&char_id).copied()).unwrap_or(0) | caller_corp_role;
                    let has_corp_privs = is_ceo
                        || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR)
                        || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_ACCOUNTANT)
                        || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT);
                    if !has_corp_privs {
                        return Err(rpc_err("CorpRoleDenied", "Director or accountant role required for corporate ship insurance"));
                    }
                    caller_corp_id
                } else {
                    if ship.owner_id != char_id {
                        return Err(rpc_err("OwnershipViolation", format!("Ship {ship_id} does not belong to character {char_id}")));
                    }
                    char_id
                };
                let now_unix = ctx.now_unix();
                let existing = fitting_containers_ships::read_container_ship_state_snap(snap)
                    .insurance_contracts
                    .get(&ship_id)
                    .cloned()
                    .filter(|c| c.end_unix > now_unix);
                if existing.is_some() && !void_old {
                    return Err(BridgeRpcError::user_error("InsureShipFailedSingleContract"));
                }
                let ref_val = compute_ship_insurance_reference_value(&ctx.sde, ship.type_id);
                let levels = crate::formulas::compute_insurance_levels(ref_val);
                let standard_fractions = [0.5, 0.6, 0.7, 0.8, 0.9, 1.0];

                let (level_name, cost, payout, level_fraction) = if arg1 > 1.0 {
                    let arg_cents = IskCents::from_f64_isk(arg1).0;
                    let mut matched = None;
                    for (i, &(lvl_name, lvl_cost, lvl_payout)) in levels.iter().enumerate() {
                        let lvl_cents = IskCents::from_f64_isk(lvl_cost).0;
                        if (arg_cents - lvl_cents).abs() <= 1 {
                            matched = Some((lvl_name, lvl_cost, lvl_payout, standard_fractions[i]));
                            break;
                        }
                    }
                    match matched {
                        Some(t) => t,
                        None => return Err(BridgeRpcError::user_error("InsureShipInvalidCost")),
                    }
                } else if arg1 > 0.0 {
                    let idx = standard_fractions.iter().enumerate().min_by(|(_, a), (_, b)| {
                        (arg1 - **a).abs().partial_cmp(&(arg1 - **b).abs()).unwrap_or(std::cmp::Ordering::Equal)
                    }).map(|(i, _)| i).unwrap_or(0);
                    let (lvl_name, lvl_cost, lvl_payout) = levels[idx];
                    (lvl_name, lvl_cost, lvl_payout, standard_fractions[idx])
                } else {
                    return Err(BridgeRpcError::user_error("InsureShipInvalidCost"));
                };
                let start_unix = now_unix;
                let end_unix = now_unix.saturating_add(crate::services::combat_crimewatch::combat_wrecks_killmails::INSURANCE_CONTRACT_DURATION_SECS);

                let cost_cents = IskCents::from_f64_isk(cost);
                let ship_type_id = ship.type_id;
                let new_bal = if is_corp {
                    let corp = snap.corporations.get_mut(&caller_corp_id)
                        .ok_or_else(|| rpc_err("CorporationNotFound", format!("Corporation {caller_corp_id} not found")))?;
                    let nb = corp.master_wallet.checked_sub(cost_cents)
                        .ok_or_else(|| rpc_err("InsufficientFunds", "Not enough corporation ISK to insure ship"))?;
                    corp.master_wallet = nb;
                    if let Some(div) = corp.wallet_divisions.first_mut() { div.balance = nb; }
                    nb
                } else {
                    let ch = snap.characters.get_mut(&char_id)
                        .ok_or_else(|| rpc_err("CharacterNotFound", format!("Character {char_id} not found")))?;
                    let nb = ch.balance.checked_sub(cost_cents)
                        .ok_or_else(|| rpc_err("InsufficientFunds", "Not enough ISK to insure ship"))?;
                    ch.balance = nb;
                    nb
                };

                snap.next_entity_id += 1;
                let jid = snap.next_entity_id;
                snap.wallet_journal.push(eve_db::WalletJournalEntry {
                    entry_id: jid,
                    character_id: owner_id,
                    entry_type: "19".to_string(),
                    amount: IskCents(-cost_cents.0),
                    balance_after: new_bal,
                    reason: format!("Insurance premium for ship {}", ship_id),
                });

                if let Some(ship_mut) = snap.items.get_mut(&ship_id) {
                    ship_mut.insured_payout_isk = payout;
                }
                let rec = fitting_containers_ships::InsuranceContractRecord {
                    ship_id, ship_type_id, owner_id,
                    level_name: level_name.into(),
                    level_fraction, cost_isk: cost, payout_isk: payout, start_unix, end_unix,
                };
                fitting_containers_ships::write_container_ship_state_snap(snap, |st| {
                    st.insurance_contracts.insert(ship_id, rec.clone());
                });
                Ok((cost, payout, rec))
            });

            match res {
                Ok((cost, payout, rec)) => {
                    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                        rt.insurance_contracts.insert(ship_id, crate::services::combat_crimewatch::InsuranceContractState {
                            ship_id: rec.ship_id, ship_type_id: rec.ship_type_id, owner_id: rec.owner_id,
                            level_name: rec.level_name.clone(), level_fraction: rec.level_fraction, cost_isk: rec.cost_isk,
                            payout_isk: rec.payout_isk, start_unix: rec.start_unix, end_unix: rec.end_unix,
                        });
                    });
                    crate::services::combat_crimewatch::combat_wrecks_killmails::sync_ship_insurance_contracts(ctx, rec.owner_id);
                    ctx.notify(crate::router::NotificationTarget::Character(char_id), "OnShipInsured", "charid", vec![]);
                    Some(Ok(PyRep::keyval([
                        ("shipID", PyRep::Integer(ship_id)),
                        ("typeID", PyRep::Integer(rec.ship_type_id as i64)),
                        ("level", PyRep::String(rec.level_name.clone())),
                        ("levelName", PyRep::String(rec.level_name.clone())),
                        ("fraction", PyRep::Real(rec.level_fraction)),
                        ("costISK", PyRep::Real(cost)),
                        ("payoutISK", PyRep::Real(payout)),
                        ("startDate", PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(rec.start_unix))),
                        ("endDate", PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(rec.end_unix))),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
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
            let ref_val = compute_ship_insurance_reference_value(&ctx.sde, type_id);
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
                    let is_ceo = snap.corporations.get(&corp_id).is_some_and(|c| c.ceo_id == char_id);
                    let roles = snap.corporations.get(&corp_id)
                        .and_then(|c| c.member_roles.get(&char_id).copied())
                        .unwrap_or(0) | session.corp_role;
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

            let entries: Vec<PyRep> = ship_type_ids
                .into_iter()
                .filter_map(|type_id| {
                    let ref_val = compute_ship_insurance_reference_value(&ctx.sde, type_id);
                    if ref_val <= 0.0 {
                        return None;
                    }
                    let (platinum_cost, platinum_payout) = compute_insurance_quote(ref_val, 1.0);
                    let levels: Vec<PyRep> = crate::formulas::compute_insurance_levels(ref_val)
                        .into_iter()
                        .map(|(name, cost, payout)| {
                            PyRep::keyval([
                                ("name", PyRep::String(name.to_string())),
                                ("cost", PyRep::Real(cost)),
                                ("costISK", PyRep::Real(cost)),
                                ("payout", PyRep::Real(payout)),
                                ("payoutISK", PyRep::Real(payout)),
                            ])
                        })
                        .collect();
                    Some(PyRep::keyval([
                        ("typeID", PyRep::Integer(type_id as i64)),
                        ("shipTypeID", PyRep::Integer(type_id as i64)),
                        ("referenceValue", PyRep::Real(ref_val)),
                        ("platinumCostISK", PyRep::Real(platinum_cost)),
                        ("platinumPayoutISK", PyRep::Real(platinum_payout)),
                        ("levels", PyRep::List(levels)),
                    ]))
                })
                .collect();

            Some(Ok(PyRep::List(entries)))
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
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(item.item_id)),
                        ("typeID", PyRep::Integer(item.type_id as i64)),
                        ("sourceTypeID", PyRep::Integer(source_type_id as i64)),
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
            let extract_ids_from_list = |list: &[PyRep]| -> Vec<i64> {
                list.iter()
                    .filter_map(|v| {
                        v.as_i64().or_else(|| match v {
                            PyRep::Tuple(t) | PyRep::List(t) => t.first().and_then(|x| x.as_i64()),
                            _ => None,
                        })
                    })
                    .collect()
            };
            let item_ids: Vec<i64> = match args.first() {
                Some(PyRep::Integer(id)) => vec![*id],
                Some(PyRep::List(list) | PyRep::Tuple(list)) => extract_ids_from_list(list),
                Some(PyRep::Dict(entries)) => {
                    let mut ids = Vec::new();
                    for (_, val) in entries {
                        match val {
                            PyRep::List(list) | PyRep::Tuple(list) => {
                                ids.extend(extract_ids_from_list(list));
                            }
                            PyRep::Integer(id) => ids.push(*id),
                            _ => {}
                        }
                    }
                    ids
                }
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
                    .map(|fit| {
                        fit.rack_heat.iter().any(|&h| h > 0.0)
                            || fit.modules.values().any(|m| m.damage_hp > 0.0)
                    })
                    .unwrap_or(false);
                if has_module_damage {
                    return Some(Err(BridgeRpcError {
                        code: "CantRepackageDamagedItem".into(),
                        message: format!(
                            "Item {item_id} has heat or module damage and must be repaired before repackaging"
                        ),
                    }));
                }
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let active_ship_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.active_ship_id)
                    .unwrap_or(0);
                // First pass: validate all items before mutating
                for &item_id in &item_ids {
                    if item_id == active_ship_id {
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
                            message: format!(
                                "Item {item_id} must be owned by character {char_id} at station {station_id}"
                            ),
                        });
                    }
                    // Fitted item check (IsShipFittingFlag: 11..=34 | 92..=94 | 125..=132 | 158) before location_id check
                    if matches!(item.flag as u16, 11..=34 | 92..=94 | 125..=132 | 158) {
                        return Err(BridgeRpcError {
                            code: "CanNotUnassembleInThisItemLoc".into(),
                            message: format!(
                                "Cannot repackage item {item_id} while it is fitted in slot {:?}",
                                item.flag
                            ),
                        });
                    }
                    if item.location_id != station_id {
                        return Err(BridgeRpcError {
                            code: "OwnershipOrLocationViolation".into(),
                            message: format!(
                                "Item {item_id} must be owned by character {char_id} at station {station_id}"
                            ),
                        });
                    }
                    if item.flag != InventoryFlag::Hangar {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!(
                                "Item {item_id} must be in your station Hangar (current flag: {:?}) to repackage",
                                item.flag
                            ),
                        });
                    }
                    if snap.is_item_locked_in_contract_or_job(item_id) {
                        return Err(BridgeRpcError {
                            code: "ItemLocked".into(),
                            message: format!(
                                "Item {item_id} is locked in an active contract or industry job"
                            ),
                        });
                    }
                    let has_contained_items = snap.items.values().any(|other| {
                        other.item_id != item_id
                            && other.location_id == item_id
                            && other.quantity > 0
                    });
                    if has_contained_items {
                        return Err(BridgeRpcError {
                            code: "CannotRepackageNonEmptyItem".into(),
                            message: format!(
                                "Cannot repackage item {item_id} while it contains fitted modules, rigs, or cargo"
                            ),
                        });
                    }
                    if item.is_blueprint_copy {
                        return Err(BridgeRpcError {
                            code: "CannotRepackageBlueprintCopy".into(),
                            message: format!("Blueprint copy {item_id} cannot be repackaged"),
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
                            message: format!(
                                "Item {item_id} has structural or armor damage and must be repaired before repackaging"
                            ),
                        });
                    }
                    let confirm_reset = args.get(1).and_then(|a| a.as_bool()).unwrap_or(false);
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
                let mut last_ver = 1_u64;
                for &item_id in &item_ids {
                    if let Some(item) = snap.items.get_mut(&item_id) {
                        item.dynamic_attributes.remove(&1112);
                        item.dynamic_attributes.remove(&1113);
                        item.singleton = false;
                        item.insured_payout_isk = 0.0;
                        item.version += 1;
                        last_ver = item.version;
                    }
                }
                Ok((item_ids[0], last_ver, item_ids.len()))
            });
            match res {
                Ok((first_id, ver, count)) => {
                    for &item_id in &item_ids {
                        ctx.invalidate_ship_fitting(item_id);
                    }
                    Some(Ok(PyRep::keyval([
                        ("itemID", PyRep::Integer(first_id)),
                        ("singleton", PyRep::Bool(false)),
                        ("version", PyRep::Integer(ver as i64)),
                        ("repackagedCount", PyRep::Integer(count as i64)),
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
            };
            let Some(item_id) = first_item_id.or(session.ship_id) else {
                return Some(Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: "AssembleShip requires itemID argument or active ship in session"
                        .into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(item) = snap.items.get(&item_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Ship item {item_id} not found"),
                    });
                };
                if item.owner_id != char_id {
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
                if item.location_id != station_id {
                    return Err(BridgeRpcError {
                        code: "OwnershipOrLocationViolation".into(),
                        message: format!(
                            "Ship item {item_id} must be owned by character {char_id} at station {station_id}"
                        ),
                    });
                }
                if item.flag == InventoryFlag::Locked {
                    return Err(BridgeRpcError {
                        code: "ItemLocked".into(),
                        message: format!("Item {item_id} is locked"),
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
                            owner_id: char_id,
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
                    Ok((new_ship_id, 1_u64))
                } else {
                    let item_mut = snap.items.get_mut(&item_id).unwrap();
                    item_mut.singleton = true;
                    item_mut.version += 1;
                    Ok((item_id, item_mut.version))
                }
            });
            match res {
                Ok((assembled_id, ver)) => Some(Ok(PyRep::keyval([
                    ("itemID", PyRep::Integer(assembled_id)),
                    ("singleton", PyRep::Bool(true)),
                    ("version", PyRep::Integer(ver as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        ("repairSvc", "RepairAll") | ("repairSvc", "RepairItems") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_none() && session.structure_id.is_none() {
                return Some(Err(BridgeRpcError {
                    code: "NotDocked".into(),
                    message: "Repairing ship and modules requires being docked".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
            };
            let explicit_ids: Vec<i64> = match args.first() {
                Some(PyRep::List(list) | PyRep::Tuple(list)) => {
                    list.iter().filter_map(|v| v.as_i64()).collect()
                }
                Some(PyRep::Integer(id)) => vec![*id],
                _ => Vec::new(),
            };
            let mut fit = match ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            let mut total_repaired_hp = 0.0;
            for m in fit.modules.values_mut() {
                if m.damage_hp > 0.0 {
                    total_repaired_hp += m.damage_hp;
                    m.damage_hp = 0.0;
                }
            }
            for h in &mut fit.rack_heat {
                *h = 0.0;
            }
            let dyn_repaired_hp = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && (i.item_id == ship_id
                                || i.location_id == ship_id
                                || explicit_ids.contains(&i.item_id))
                    })
                    .map(|i| {
                        i.dynamic_attributes
                            .get(&attr_ids::DAMAGE)
                            .copied()
                            .unwrap_or(0.0)
                            + i.dynamic_attributes
                                .get(&attr_ids::ARMOR_DAMAGE)
                                .copied()
                                .unwrap_or(0.0)
                    })
                    .sum::<f64>()
            });
            total_repaired_hp += dyn_repaired_hp;
            let cost_isk = (total_repaired_hp * 500.0).round();
            let debit_res = ctx.db.try_with_mut_snapshot(|snap| {
                if cost_isk > 0.0 {
                    let ch = snap.characters.get_mut(&char_id).ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    })?;
                    let fee = IskCents::from_f64_isk(cost_isk);
                    ch.balance = ch.balance.checked_sub(fee).ok_or_else(|| BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Insufficient ISK to pay repair bill".into(),
                    })?;
                }
                for item in snap.items.values_mut() {
                    if item.owner_id == char_id
                        && (item.item_id == ship_id
                            || item.location_id == ship_id
                            || explicit_ids.contains(&item.item_id))
                    {
                        item.dynamic_attributes.remove(&attr_ids::DAMAGE);
                        item.dynamic_attributes.remove(&attr_ids::ARMOR_DAMAGE);
                    }
                }
                Ok(())
            });
            if let Err(e) = debit_res {
                return Some(Err(e));
            }
            ctx.store_ship_fitting(ship_id, fit);
            let repaired_count = ctx.db.with_snapshot(|snap| {
                snap.items
                    .values()
                    .filter(|i| {
                        i.owner_id == char_id
                            && (i.item_id == ship_id
                                || i.location_id == ship_id
                                || explicit_ids.contains(&i.item_id))
                    })
                    .count()
            });
            Some(Ok(PyRep::keyval([
                ("characterID", PyRep::Integer(char_id)),
                ("shipID", PyRep::Integer(ship_id)),
                ("itemsInspected", PyRep::Integer(repaired_count as i64)),
                ("repairedModuleHP", PyRep::Real(total_repaired_hp)),
                ("repairCostISK", PyRep::Real(cost_isk)),
                ("repaired", PyRep::Bool(true)),
            ])))
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
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(item) = snap.items.get(&item_id) else {
                    return Err(BridgeRpcError {
                        code: "ItemNotFound".into(),
                        message: format!("Item {item_id} not found"),
                    });
                };
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
                        && snap.corporations.get(&item.owner_id).is_some_and(|corp| {
                            if corp.ceo_id == char_id {
                                return true;
                            }
                            let roles = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                            // corpRoleDirector (1) or corpRoleEquipmentConfig (2199023255552)
                            (roles & (1 | 2_199_023_255_552)) != 0
                        });
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
