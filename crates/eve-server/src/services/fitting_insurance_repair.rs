//! `fitting_insurance_repair` — `insuranceSvc.InsureShip` & `repairSvc.RepairAll`.

#![forbid(unsafe_code)]

use super::*;
use crate::services::station_structure::structure_lifecycle as life;

#[inline]
fn rpc_err(code: &str, msg: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: msg.into(),
    }
}

fn resolve_upwell_repair_access_and_tax_bps(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    sid: i64,
    char_id: i64,
    now_unix: u64,
) -> Result<i64, BridgeRpcError> {
    let bps = crate::services::structure_profile_services::require_structure_service_access_in_snap(
        Some(ctx),
        snap,
        sid,
        char_id,
        crate::services::structure_profile_services::StructureServiceKind::Repair,
        now_unix,
    )?
    .unwrap_or(0);
    Ok(bps.max(0))
}

pub(super) fn handle_insure_ship(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(ship_id) = args.first().and_then(|a| a.as_i64()).or(session.ship_id) else {
        return Err(rpc_err(
            "NoActiveShip",
            "InsureShip requires shipID argument or active ship in session",
        ));
    };
    let Some(arg1) = args.get(1).and_then(|a| a.as_f64()) else {
        return Err(BridgeRpcError::user_error("InsureShipInvalidCost"));
    };
    if let Some(sid) = session.structure_id {
        return Err(BridgeRpcError {
            code: "StationServiceUnavailable".into(),
            message: format!("Upwell structure {sid} does not offer insurance"),
        });
    }
    let Some(station_id) = session.station_id else {
        return Err(rpc_err(
            "NotDocked",
            "Insuring a ship requires being docked at an NPC station",
        ));
    };
    if station_id >= 1_000_000_000_000
        || !crate::services::station_structure::npc_station_offers_service(&ctx.sde, station_id, 21)
    {
        return Err(BridgeRpcError {
            code: "StationServiceUnavailable".into(),
            message: format!("Station {station_id} does not offer insurance"),
        });
    }

    let is_corp_item = args
        .get(2)
        .and_then(|a| a.as_bool())
        .or_else(|| kwargs.get("isCorpItem").and_then(|a| a.as_bool()))
        .unwrap_or(false);
    let void_old = args
        .get(3)
        .and_then(|a| a.as_bool())
        .or_else(|| kwargs.get("voidOld").and_then(|a| a.as_bool()))
        .unwrap_or(false);
    let caller_corp_id_hint = session.corporation_id.unwrap_or(0);
    let corp_acct_key = session.corp_account_key.clamp(1000, 1006);

    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let caller_corp_id = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .filter(|&cid| cid != 0)
            .unwrap_or(caller_corp_id_hint);
        let now_unix = ctx.now_unix();
        let Some(ship) = snap.items.get(&ship_id).cloned() else {
            return Err(rpc_err("ShipNotFound", format!("Ship {ship_id} not found")));
        };
        if ctx.sde.get_type_category(ship.type_id) != Some(6) {
            return Err(BridgeRpcError::user_error("InsureShipFailed"));
        }
        if !ship.singleton {
            return Err(rpc_err(
                "InsureShipNotAssembled",
                "Ship must be assembled to be insured",
            ));
        }
        if matches!(ship.flag, InventoryFlag::Impounded | InventoryFlag::Locked)
            || snap.is_item_locked_in_contract_or_job(ship_id)
        {
            return Err(rpc_err(
                "ShipNotInsureable",
                format!("Ship {ship_id} cannot be insured while impounded, locked, or in asset safety"),
            ));
        }
        let is_corp = is_corp_item || (caller_corp_id != 0 && ship.owner_id == caller_corp_id);
        let ship_at_station = ship.location_id == station_id
            || (ship.flag == InventoryFlag::ShipHangar
                && snap.items.get(&ship.location_id).is_some_and(|carrier| {
                    (carrier.owner_id == char_id
                        || (is_corp && carrier.owner_id == caller_corp_id))
                        && carrier.type_id != 60
                        && carrier.location_id == station_id
                        && !matches!(carrier.flag, InventoryFlag::Impounded | InventoryFlag::Locked)
                }));
        if !ship_at_station {
            return Err(rpc_err(
                "ShipNotAtStation",
                format!(
                    "Ship {ship_id} is at location {}, not docked station {station_id}",
                    ship.location_id
                ),
            ));
        }
        let owner_id = if is_corp {
            if caller_corp_id == 0 || ship.owner_id != caller_corp_id {
                return Err(rpc_err(
                    "OwnershipViolation",
                    format!("Ship {ship_id} is not owned by corporation {caller_corp_id}"),
                ));
            }
            crate::services::market_trading::market_orders_corp_auth::require_corp_accountant_spend_role(
                snap,
                caller_corp_id,
                char_id,
                corp_acct_key.clamp(1000, 1006) as u16,
                now_unix,
                "CorpRoleDenied",
            )?;
            caller_corp_id
        } else {
            if ship.owner_id != char_id {
                return Err(rpc_err(
                    "OwnershipViolation",
                    format!("Ship {ship_id} does not belong to character {char_id}"),
                ));
            }
            char_id
        };
        let existing = fitting_containers_ships::read_container_ship_state_snap(snap)
            .insurance_contracts
            .get(&ship_id)
            .cloned()
            .filter(|c| c.end_unix > now_unix);
        if existing.is_some() && !void_old {
            return Err(BridgeRpcError::user_error("InsureShipFailedSingleContract"));
        }
        let live_prices =
            crate::services::industry_pi::industry_rigs::collect_live_market_adjusted_prices(snap);
        let ref_val =
            crate::services::industry_pi::industry_rigs::compute_ship_insurance_reference_value_with_prices(
                &ctx.sde,
                &live_prices,
                ship.type_id,
            );
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
            let idx = standard_fractions
                .iter()
                .enumerate()
                .min_by(|(_, a), (_, b)| {
                    (arg1 - **a)
                        .abs()
                        .partial_cmp(&(arg1 - **b).abs())
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .map(|(i, _)| i)
                .unwrap_or(0);
            let (lvl_name, lvl_cost, lvl_payout) = levels[idx];
            (lvl_name, lvl_cost, lvl_payout, standard_fractions[idx])
        } else {
            return Err(BridgeRpcError::user_error("InsureShipInvalidCost"));
        };
        let start_unix = now_unix;
        let end_unix = now_unix.saturating_add(
            crate::services::combat_crimewatch::combat_wrecks_killmails::INSURANCE_CONTRACT_DURATION_SECS,
        );

        let cost_cents = IskCents::from_f64_isk(cost);
        let ship_type_id = ship.type_id;
        let new_bal = if is_corp {
            let corp = snap.corporations.get_mut(&caller_corp_id).ok_or_else(|| {
                rpc_err(
                    "CorporationNotFound",
                    format!("Corporation {caller_corp_id} not found"),
                )
            })?;
            let div_id = (corp_acct_key.saturating_sub(999)).clamp(1, 7) as u8;
            corp.debit_division(div_id, cost_cents)
                .ok_or_else(|| rpc_err("InsufficientFunds", "Not enough corporation ISK to insure ship"))?
        } else {
            let ch = snap.characters.get_mut(&char_id).ok_or_else(|| {
                rpc_err("CharacterNotFound", format!("Character {char_id} not found"))
            })?;
            let nb = ch
                .balance
                .checked_sub(cost_cents)
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
            ship_id,
            ship_type_id,
            owner_id,
            level_name: level_name.into(),
            level_fraction,
            cost_isk: cost,
            payout_isk: payout,
            start_unix,
            end_unix,
        };
        fitting_containers_ships::write_container_ship_state_snap(snap, |st| {
            st.insurance_contracts.insert(ship_id, rec.clone());
        });
        Ok((cost, payout, rec))
    })?;

    let (cost, payout, rec) = res;
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        rt.insurance_contracts.insert(
            ship_id,
            crate::services::combat_crimewatch::InsuranceContractState {
                ship_id: rec.ship_id,
                ship_type_id: rec.ship_type_id,
                owner_id: rec.owner_id,
                level_name: rec.level_name.clone(),
                level_fraction: rec.level_fraction,
                cost_isk: rec.cost_isk,
                payout_isk: rec.payout_isk,
                start_unix: rec.start_unix,
                end_unix: rec.end_unix,
            },
        );
    });
    crate::services::combat_crimewatch::combat_wrecks_killmails::sync_ship_insurance_contracts(
        ctx,
        rec.owner_id,
    );
    ctx.notify(
        crate::router::NotificationTarget::Character(char_id),
        "OnShipInsured",
        "charid",
        vec![],
    );
    Ok(PyRep::keyval([
        ("shipID", PyRep::Integer(ship_id)),
        ("typeID", PyRep::Integer(rec.ship_type_id as i64)),
        ("level", PyRep::String(rec.level_name.clone())),
        ("levelName", PyRep::String(rec.level_name.clone())),
        ("fraction", PyRep::Real(rec.level_fraction)),
        ("costISK", PyRep::Real(cost)),
        ("payoutISK", PyRep::Real(payout)),
        (
            "startDate",
            PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(rec.start_unix)),
        ),
        (
            "endDate",
            PyRep::Integer(crate::sp6_lifecycle::unix_to_filetime(rec.end_unix)),
        ),
    ]))
}

pub(super) fn handle_repair_all(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let Some(dock_id) = session.station_id.or(session.structure_id) else {
        return Err(BridgeRpcError {
            code: "NotDocked".into(),
            message: "Repairing ship and modules requires being docked".into(),
        });
    };
    life::sync_all_structures(ctx);
    let now_unix = ctx.now_unix();
    let is_upwell = dock_id >= 1_000_000_000_000
        || ctx.db.with_snapshot(|s| s.structures.contains_key(&dock_id));
    if !is_upwell
        && !crate::services::station_structure::npc_station_offers_service(&ctx.sde, dock_id, 13)
    {
        return Err(BridgeRpcError {
            code: "StationServiceUnavailable".into(),
            message: format!("Station {dock_id} does not offer repair facilities"),
        });
    }
    let Some(ship_id) = session.ship_id else {
        return Err(BridgeRpcError {
            code: "NoActiveShip".into(),
            message: "Session has no active ship".into(),
        });
    };
    let explicit_ids: Vec<i64> = match args.first() {
        Some(PyRep::List(list) | PyRep::Tuple(list)) => {
            list.iter().filter_map(|v| v.as_i64()).collect()
        }
        Some(PyRep::Integer(id)) => vec![*id],
        _ => Vec::new(),
    };
    let (ship_at_dock, tax_bps) = ctx.db.with_snapshot(|snap| {
        let tax_bps = if is_upwell {
            resolve_upwell_repair_access_and_tax_bps(ctx, snap, dock_id, char_id, now_unix)?
        } else {
            0
        };
        let ship_at_dock = snap
            .items
            .get(&ship_id)
            .is_some_and(|sh| sh.owner_id == char_id && sh.location_id == dock_id);
        for &eid in &explicit_ids {
            let Some(it) = snap.items.get(&eid) else {
                return Err(BridgeRpcError {
                    code: "ItemNotFound".into(),
                    message: format!("Item {eid} not found"),
                });
            };
            if it.owner_id != char_id {
                return Err(BridgeRpcError {
                    code: "ItemOwnershipViolation".into(),
                    message: format!("Item {eid} is not owned by character {char_id}"),
                });
            }
            let at_dock = it.location_id == dock_id
                || snap
                    .items
                    .get(&it.location_id)
                    .is_some_and(|parent| parent.owner_id == char_id && parent.location_id == dock_id);
            if !at_dock {
                return Err(BridgeRpcError {
                    code: "ItemNotAtStation".into(),
                    message: format!("Item {eid} is not located at docked facility {dock_id}"),
                });
            }
        }
        Ok((ship_at_dock, tax_bps))
    })?;

    let mut fit = ctx.get_or_rebuild_ship_fitting(char_id, ship_id)?;
    let mut fit_repaired_hp = 0.0;
    if ship_at_dock {
        for m in fit.modules.values_mut() {
            if m.damage_hp > 0.0 {
                fit_repaired_hp += m.damage_hp;
                m.damage_hp = 0.0;
            }
        }
        for h in &mut fit.rack_heat {
            *h = 0.0;
        }
    }
    let (target_item_ids, dyn_repaired_hp, combat_repaired_hp, combat_base_cost_isk) =
        ctx.db.with_snapshot(|snap| {
            let ids: Vec<i64> = snap
                .items
                .values()
                .filter(|i| {
                    if i.owner_id != char_id {
                        return false;
                    }
                    let on_active =
                        ship_at_dock && (i.item_id == ship_id || i.location_id == ship_id);
                    let in_explicit = (explicit_ids.contains(&i.item_id)
                        || explicit_ids.contains(&i.location_id))
                        && (i.location_id == dock_id
                            || snap.items.get(&i.location_id).is_some_and(|parent| {
                                parent.owner_id == char_id && parent.location_id == dock_id
                            }));
                    on_active || in_explicit
                })
                .map(|i| i.item_id)
                .collect();
            let mut dyn_hp = 0.0;
            let mut combat_hp = 0.0;
            let mut combat_cost = 0.0;
            for &id in &ids {
                if let Some(i) = snap.items.get(&id) {
                    if ctx.sde.get_type_category(i.type_id) == Some(6) {
                        let t_opt = ctx.sde.types.get(&i.type_id);
                        let max_hull = t_opt
                            .and_then(|t| t.default_attributes.get(&attr_ids::HP_STRUCT).copied())
                            .unwrap_or(0.0);
                        let max_armor = t_opt
                            .and_then(|t| t.default_attributes.get(&attr_ids::ARMOR_HP).copied())
                            .unwrap_or(0.0);
                        let max_shield = t_opt
                            .and_then(|t| t.default_attributes.get(&attr_ids::SHIELD_CAPACITY).copied())
                            .unwrap_or(0.0);
                        let unified = crate::sp6_lifecycle::read_unified_ship_damage_state(
                            ctx,
                            snap,
                            id,
                            i.type_id,
                            Some((max_hull, max_armor, max_shield)),
                        );
                        let cdmg = unified.hull_damage + unified.armor_damage;
                        if cdmg > 0.0 {
                            combat_hp += cdmg;
                            combat_cost += cdmg * 500.0;
                        }
                    } else {
                        dyn_hp += i
                            .dynamic_attributes
                            .get(&attr_ids::DAMAGE)
                            .copied()
                            .unwrap_or(0.0)
                            + i.dynamic_attributes
                                .get(&attr_ids::ARMOR_DAMAGE)
                                .copied()
                                .unwrap_or(0.0);
                    }
                }
            }
            (ids, dyn_hp, combat_hp, combat_cost)
        });
    let total_repaired_hp = fit_repaired_hp + dyn_repaired_hp + combat_repaired_hp;
    let base_cost_isk = ((fit_repaired_hp + dyn_repaired_hp) * 500.0 + combat_base_cost_isk).round();
    let cost_isk = if is_upwell {
        ((base_cost_isk * (tax_bps as f64)) / 10_000.0).round()
    } else if total_repaired_hp > 0.0 {
        base_cost_isk.max(1.0)
    } else {
        0.0
    };

    ctx.db.try_with_mut_snapshot(|snap| {
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
            let bal = ch.balance;
            snap.append_journal(
                char_id,
                "RepairBill",
                IskCents(-fee.0),
                bal,
                format!("Repaired {} item(s) at {dock_id}", target_item_ids.len()),
            );
            if is_upwell && fee.0 > 0 {
                if let Some(owner_corp_id) = snap.structures.get(&dock_id).map(|st| st.owner_corp_id) {
                    if let Some(corp) = snap.corporations.get_mut(&owner_corp_id) {
                        if let Some(new_corp_bal) = corp.division_balance(1).checked_add(fee) {
                            corp.set_division_balance(1, new_corp_bal);
                            let desc = format!("Structure repairshop tax from {char_id} at {dock_id}");
                            snap.append_journal(owner_corp_id, "RepairBill", fee, new_corp_bal, desc);
                        }
                    }
                }
            }
        }
        for &id in &target_item_ids {
            if snap.items.get(&id).is_some_and(|item| item.owner_id == char_id) {
                crate::sp6_lifecycle::clear_ship_combat_damage_in_snap(snap, id);
            }
        }
        Ok(())
    })?;
    if ship_at_dock {
        ctx.store_ship_fitting(ship_id, fit);
    }
    let repaired_count = target_item_ids.len();
    Ok(PyRep::keyval([
        ("characterID", PyRep::Integer(char_id)),
        ("shipID", PyRep::Integer(ship_id)),
        ("itemsInspected", PyRep::Integer(repaired_count as i64)),
        ("repairedModuleHP", PyRep::Real(total_repaired_hp)),
        ("repairCostISK", PyRep::Real(cost_isk)),
        ("repaired", PyRep::Bool(true)),
    ]))
}
