//! `contracts_service` — Contract Lifecycle, Escrow, Courier, ItemExchange & Auctions (`E-3`).
//!
//! Handles `contractProxy` and `contractMgr` RPCs for:
//! - `ItemExchange` (one-way, want-to-buy, and two-way `requested_items`, preserving BPC/mutated/damage attributes)
//! - `Courier` (plastic wrap `3468`, collateral + reward escrow, delivery, failure/expiry settlement, `BreakPlasticWrap`)
//! - `Auction` (starting bid, minimum bid increments, outbid escrow refund, immediate buyout, expiry settlement)
//! - Contract visibility (`Public`, `Personal`, `Corporation`, `Alliance`), slot limits (`Contracting` `25235`),
//!   and search/query endpoints (`GetContractInfo`, `SearchContracts`, `GetMyCurrentContractList`,
//!   `GetMyExpiredContractList`, `GetContractListForOwner`, `GetMyBids`, `RejectContract`, `GM_ExpireContract`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use eve_db::{ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

fn notify_contract(ctx: &ServerContext, char_id: i64, event: &str, cid: i64) {
    ctx.notify(NotificationTarget::Character(char_id), event, "charid", vec![PyRep::Integer(cid)]);
}

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into(), }
}

fn parse_requested_items(
    raw: Option<&PyRep>,
    kwargs: &HashMap<String, PyRep>,
) -> Result<Vec<(TypeId, i32)>, BridgeRpcError> {
    let source = raw.or_else(|| kwargs.get("requestedItems")).or_else(|| kwargs.get("requestItemTypeList"));
    let Some(rep) = source else { return Ok(Vec::new()); };
    let list = match rep {
        PyRep::None => return Ok(Vec::new()),
        PyRep::List(v) | PyRep::Tuple(v) => v,
        _ => {
            return Err(rpc_err(
                "InvalidContractParameters",
                "requestedItems must be a list or tuple of (typeID, quantity) pairs",
            ))
        }
    };
    let mut out = Vec::with_capacity(list.len());
    for entry in list {
        let (tid_opt, qty_opt) = match entry {
            PyRep::Tuple(pair) | PyRep::List(pair) => (
                pair.first().and_then(|v| v.as_i64()),
                pair.get(1).and_then(|v| v.as_i64()),
            ),
            PyRep::KeyVal(kv) => (
                kv.get("typeID").and_then(|v| v.as_i64()),
                kv.get("quantity").and_then(|v| v.as_i64()),
            ),
            _ => (None, None),
        };
        let (Some(tid), Some(qty)) = (tid_opt, qty_opt) else {
            return Err(rpc_err(
                "InvalidContractParameters",
                "Each requested item entry must provide (typeID, quantity)",
            ));
        };
        if tid <= 0 || qty <= 0 || qty > i32::MAX as i64 {
            return Err(rpc_err(
                "InvalidContractParameters",
                format!("Invalid requested item (typeID={tid}, quantity={qty})"),
            ));
        }
        out.push((tid as TypeId, qty as i32));
    }
    Ok(out)
}

fn parse_item_ids(raw: Option<&PyRep>) -> Vec<i64> {
    match raw {
        Some(PyRep::Integer(id)) if *id > 0 => vec![*id],
        Some(PyRep::List(items) | PyRep::Tuple(items)) => items.iter().filter_map(|v| match v {
                PyRep::Integer(id) if *id > 0 => Some(*id),
                PyRep::Tuple(pair) | PyRep::List(pair) => {
                    pair.first().and_then(|x| x.as_i64()).filter(|id| *id > 0)
                }
                _ => None,
            }).collect(),
        _ => Vec::new(),
    }
}

fn contract_to_pyrep(c: &ContractRecord, snap: &eve_db::WorldStateSnapshot) -> PyRep {
    let items_rep = PyRep::List(c.item_ids.iter().map(|&id| PyRep::Integer(id)).collect());
    let detailed_items_rep = PyRep::List(
        c.item_ids.iter().filter_map(|&id| {
                let it = snap.items.get(&id)?;
                let me = it.dynamic_attributes.get(&1112).copied().unwrap_or(0.0) as i64;
                let te = it.dynamic_attributes.get(&1113).copied().unwrap_or(0.0) as i64;
                let dmg = it.dynamic_attributes.get(&3).copied().unwrap_or(0.0);
                let runs_rep = it.blueprint_runs_remaining.map(|r| PyRep::Integer(r as i64))
                    .unwrap_or(PyRep::Integer(-1));
                let mut dyn_entries: Vec<(PyRep, PyRep)> = it.dynamic_attributes.iter()
                    .map(|(&attr_id, &val)| (PyRep::Integer(attr_id as i64), PyRep::Real(val))).collect();
                dyn_entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
                Some(PyRep::keyval([
                    ("itemID", PyRep::Integer(it.item_id)),
                    ("typeID", PyRep::Integer(it.type_id as i64)),
                    ("quantity", PyRep::Integer(it.quantity as i64)),
                    ("singleton", PyRep::Bool(it.singleton)),
                    ("copy", PyRep::Bool(it.is_blueprint_copy)),
                    ("isBlueprintCopy", PyRep::Bool(it.is_blueprint_copy)),
                    ("materialEfficiency", PyRep::Integer(me)),
                    ("timeEfficiency", PyRep::Integer(te)),
                    ("runs", runs_rep.clone()),
                    ("runsRemaining", runs_rep),
                    ("damage", PyRep::Real(dmg)),
                    (
                        "sourceMutaplasmidID",
                        it.source_mutaplasmid_id.map(|m| PyRep::Integer(m as i64)).unwrap_or(PyRep::None),
                    ),
                    ("dynamicAttributes", PyRep::Dict(dyn_entries)),
                ]))
            }).collect(),
    );
    let req_rep = PyRep::List(
        c.requested_items.iter().map(|&(tid, qty)| {
                PyRep::keyval([("typeID", PyRep::Integer(tid as i64)), ("quantity", PyRep::Integer(qty as i64)),])
            }).collect(),
    );
    let bids_rep = PyRep::List(
        c.bid_history.iter().map(|&(bidder_id, amount, ts)| {
                PyRep::keyval([
                    ("bidderID", PyRep::Integer(bidder_id)),
                    ("amountISK", PyRep::Real(amount.to_wire_f64())),
                    ("timestampUnix", PyRep::Integer(ts as i64)),
                ])
            }).collect(),
    );
    let availability = if c.availability.is_empty() { "Public".to_string() } else { c.availability.clone() };
    let base_fields = [
        ("contractID", PyRep::Integer(c.contract_id)),
        ("issuerID", PyRep::Integer(c.issuer_id)),
        ("issuerCorpID", PyRep::Integer(c.issuer_corp_id)),
        ("acceptorID", c.acceptor_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("assigneeID", PyRep::Integer(c.assignee_id)),
        ("availability", PyRep::String(availability)),
        ("type", PyRep::String(c.contract_type.clone())),
        ("status", PyRep::String(c.status.clone())),
        ("startStationID", PyRep::Integer(c.start_station_id)),
        ("endStationID", PyRep::Integer(c.end_station_id)),
        ("price", PyRep::Real(c.price.to_wire_f64())),
        ("reward", PyRep::Real(c.reward.to_wire_f64())),
        ("collateral", PyRep::Real(c.collateral.to_wire_f64())),
        ("buyout", PyRep::Real(c.buyout.to_wire_f64())),
        ("currentBid", PyRep::Real(c.current_bid.to_wire_f64())),
        ("currentBidderID", c.current_bidder_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("itemIDs", items_rep),
        ("items", detailed_items_rep),
        ("requestedItems", req_rep),
        ("bids", bids_rep),
        ("expirationUnix", PyRep::Integer(c.expiration_unix as i64)),
        ("dateIssuedUnix", PyRep::Integer(c.date_issued_unix as i64)),
        ("numDays", PyRep::Integer(c.num_days.max(1) as i64)),
        ("title", PyRep::String(c.title.clone())),
        ("forCorp", PyRep::Bool(c.for_corp)),
    ];
    let contract_kv = PyRep::keyval(base_fields.clone());
    let mut outer: Vec<(&str, PyRep)> = base_fields.into();
    outer.push(("contract", contract_kv));
    PyRep::keyval(outer)
}

fn check_contract_visibility(
    c: &ContractRecord,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
) -> bool {
    if c.issuer_id == char_id { return true; }
    let avail = if c.availability.is_empty() { "Public" } else { c.availability.as_str() };
    if avail.eq_ignore_ascii_case("Public") && c.assignee_id == 0 { return true; }
    if c.assignee_id != 0 {
        return c.assignee_id == char_id
            || c.assignee_id == corp_id
            || alliance_id == Some(c.assignee_id);
    }
    match avail {
        "Corporation" => c.issuer_corp_id != 0 && c.issuer_corp_id == corp_id,
        "Alliance" => alliance_id.is_some() && alliance_id == Some(c.assignee_id),
        "Personal" | "Private" => false,
        _ => true,
    }
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    let is_break_wrap = matches!(service, "invbroker" | "repackagingSvc") && method == "BreakPlasticWrap";
    if service != "contractProxy" && !is_break_wrap { return None; }

    match method {
        "CreateContract" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let contract_type = match args.first() {
                Some(PyRep::Integer(1)) => "ItemExchange",
                Some(PyRep::Integer(2)) => "Auction",
                Some(PyRep::Integer(3)) => "Courier",
                Some(PyRep::Integer(other)) => {
                    return Some(Err(rpc_err(
                        "InvalidContractType",
                        format!("Unsupported contract type code {other}; must be 1, 2, or 3"),
                    )));
                }
                Some(PyRep::String(s))
                    if matches!(s.as_str(), "Courier" | "ItemExchange" | "Auction") => { s.as_str() }
                other => {
                    return Some(Err(rpc_err("InvalidContractType", format!("Unsupported contract type {other:?}"),)));
                }
            };
            let availability = if let Some(av_kw) = kwargs.get("availability").and_then(|v| v.as_str()) {
                av_kw.to_string()
            } else {
                match args.get(1) {
                    Some(PyRep::Bool(false)) | Some(PyRep::Integer(0)) | None | Some(PyRep::None) => {
                        "Public".to_string()
                    }
                    Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)) => "Personal".to_string(),
                    Some(PyRep::Integer(2)) => "Corporation".to_string(),
                    Some(PyRep::Integer(3)) => "Alliance".to_string(),
                    Some(PyRep::String(s)) => s.clone(),
                    _ => "Public".to_string(),
                }
            };
            let assignee_id = args.get(2).and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("assigneeID").and_then(|v| v.as_i64())).unwrap_or(0);
            let num_days = args.get(4).and_then(|a| a.as_i64()).filter(|&d| d > 0).map(|d| d.clamp(1, 28) as u32)
                .unwrap_or(1);
            let duration_secs = if let Some(exp_min) = args.get(3).and_then(|a| a.as_i64()).filter(|&m| m > 0)
            { ((exp_min as u64).saturating_mul(60)).clamp(60, 2_419_200) } else { 86_400 };
            let Some(start_station_id) = args.get(5).and_then(|a| a.as_i64()).filter(|&id| id > 0)
                .or(session.station_id).or(session.structure_id)
            else {
                return Some(Err(rpc_err(
                    "MustBeDockedAtStartStation",
                    "Caller must be docked at start_station_id to create a contract",
                )));
            };
            let end_station_id = match args.get(6).and_then(|a| a.as_i64()).filter(|&id| id > 0) {
                Some(eid) => eid,
                None if matches!(contract_type, "ItemExchange" | "Auction") => start_station_id,
                None => {
                    return Some(Err(rpc_err(
                        "InvalidContractDestination",
                        "Courier contracts require an explicit destination stationID",
                    )));
                }
            };
            let price_arg = args.get(7).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let reward_arg = args.get(8).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let collateral_arg = args.get(9).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let (primary_isk, secondary_isk) = match contract_type {
                "Courier" => (reward_arg, collateral_arg),
                "Auction" => (price_arg, collateral_arg),
                _ => (price_arg, reward_arg),
            };
            let raw_title = args.get(10).and_then(|a| a.as_str())
                .or_else(|| kwargs.get("title").and_then(|v| v.as_str())).unwrap_or("");
            let title = raw_title.chars().take(128).collect::<String>();
            let items_source = args.get(12).or_else(|| kwargs.get("itemList")).or_else(|| kwargs.get("items"));
            let item_ids_req = parse_item_ids(items_source);
            let requested_items = match parse_requested_items(args.get(14), kwargs) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let for_corp = args.get(15).and_then(|a| a.as_bool())
                .or_else(|| kwargs.get("forCorp").and_then(|v| v.as_bool())).unwrap_or(false);

            if session.station_id.or(session.structure_id) != Some(start_station_id) {
                return Some(Err(rpc_err(
                    "MustBeDockedAtStartStation",
                    format!("Caller must be docked at start_station_id {start_station_id}"),
                )));
            }

            if matches!(contract_type, "ItemExchange" | "Auction") {
                if end_station_id != start_station_id {
                    return Some(Err(rpc_err(
                        "InvalidContractDestination",
                        format!("{contract_type} contracts cannot relocate items ({start_station_id} != {end_station_id})"),
                    )));
                }
                let allow_want_to_buy = contract_type == "ItemExchange" && !requested_items.is_empty();
                if item_ids_req.is_empty() && !allow_want_to_buy {
                    return Some(Err(rpc_err(
                        "ContractRequiresItem",
                        format!("{contract_type} contracts require an explicit item_id in station hangar"),
                    )));
                }
            }

            if !primary_isk.is_finite()
                || primary_isk < 0.0
                || !secondary_isk.is_finite()
                || secondary_isk < 0.0
            {
                return Some(Err(rpc_err(
                    "InvalidContractParameters",
                    "Contract price/reward and collateral/buyout must be non-negative finite numbers",
                )));
            }

            if contract_type == "Auction" && secondary_isk > 0.0 && secondary_isk < primary_isk {
                return Some(Err(rpc_err(
                    "InvalidContractParameters",
                    "Auction buyout price cannot be lower than starting bid",
                )));
            }

            let start_sde_valid = ctx.sde.solar_systems.values()
                .any(|s| s.stations.iter().any(|st| st.station_id == start_station_id));
            let end_sde_valid = ctx.sde.solar_systems.values()
                .any(|s| s.stations.iter().any(|st| st.station_id == end_station_id));
            let now_unix = ctx.now_unix();
            let active_ship = session.ship_id;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let start_valid = start_sde_valid
                    || snap.structures.get(&start_station_id).map(|st| st.state != "Destroyed").unwrap_or(false);
                let end_valid = end_sde_valid
                    || snap.structures.get(&end_station_id).map(|st| st.state != "Destroyed").unwrap_or(false);
                if !start_valid || !end_valid {
                    return Err(rpc_err(
                        "InvalidContractStation",
                        format!("Invalid start {start_station_id} or destination {end_station_id}"),
                    ));
                }

                let expired_ids: Vec<i64> = snap.contracts.values().filter(|c| {
                        c.issuer_id == char_id
                            && matches!(c.status.as_str(), "Outstanding" | "InProgress")
                            && now_unix >= c.expiration_unix
                    }).map(|c| c.contract_id).collect();
                for eid in expired_ids { let _ = snap.resolve_expired_contract(eid, now_unix); }

                let active_count = snap.contracts.values().filter(|c| {
                        c.issuer_id == char_id
                            && c.for_corp == for_corp
                            && matches!(c.status.as_str(), "Outstanding" | "InProgress")
                    }).count();
                let max_slots = if for_corp {
                    let lvl = snap.skills.get(&char_id).and_then(|m| m.get(&25233)).map(|s| s.level as usize)
                        .unwrap_or(1);
                    10 + 10 * lvl
                } else if let Some(skill) = snap.skills.get(&char_id).and_then(|m| m.get(&25235)) {
                    1 + 4 * (skill.level as usize)
                } else { 5
                };
                if active_count >= max_slots {
                    return Err(rpc_err(
                        "ContractSlotLimitReached",
                        format!("Character {char_id} has {active_count} active contracts (max {max_slots})"),
                    ));
                }

                let primary_cents = IskCents::from_f64_isk(primary_isk);
                let secondary_cents = IskCents::from_f64_isk(secondary_isk);
                let (price, reward, collateral, buyout) = match contract_type {
                    "Courier" => (IskCents(0), primary_cents, secondary_cents, IskCents(0)),
                    "Auction" => (primary_cents, IskCents(0), IskCents(0), secondary_cents),
                    _ => (primary_cents, secondary_cents, IskCents(0), IskCents(0)),
                };

                let is_public = availability.eq_ignore_ascii_case("Public") && assignee_id == 0;
                let broker_fee_isk = if is_public { (price.to_wire_f64() * 0.01).max(10_000.0) } else { 10_000.0 };
                let deposit_isk = if is_public {
                    (price.to_wire_f64().max(collateral.to_wire_f64()) * 0.01).max(100_000.0)
                } else { 0.0
                };

                let Some(issuer_corp_id) = snap.characters.get(&char_id).map(|c| c.corporation_id) else {
                    return Err(rpc_err("CharacterNotFound", format!("Character {char_id} not found")));
                };
                if for_corp {
                    let Some(corp) = snap.corporations.get(&issuer_corp_id) else {
                        return Err(rpc_err("CrpAccessDenied", "Corporation contracts require a player corporation"));
                    };
                    let roles = corp.member_roles.get(&char_id).copied().unwrap_or(0) | session.corp_role;
                    let mask = eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_CONTRACT_MANAGER;
                    if corp.ceo_id != char_id && (roles & mask) == 0 {
                        return Err(rpc_err("CrpAccessDenied", "Creating corporation contracts requires ROLE_CONTRACT_MANAGER or ROLE_DIRECTOR"));
                    }
                }
                let fee_cents = IskCents::from_f64_isk(broker_fee_isk + deposit_isk);
                let total_upfront = reward
                    .checked_add(fee_cents)
                    .ok_or_else(|| rpc_err("InvalidContractParameters", "Contract reward and fee overflow"))?;
                if total_upfront.0 > 0 {
                    if for_corp {
                        let corp = snap.corporations.get_mut(&issuer_corp_id).unwrap();
                        let new_bal = corp.master_wallet.checked_sub(total_upfront)
                            .ok_or_else(|| rpc_err("InsufficientFunds", "Not enough corporation ISK to escrow contract reward"))?;
                        corp.master_wallet = new_bal;
                        // Fixes R1-P1-2: prevent division-1 balance inflation on insufficient funds
                        if let Some(div) = corp.wallet_divisions.first_mut() {
                            let new_div_bal = div.balance.checked_sub(total_upfront)
                                .ok_or_else(|| rpc_err("InsufficientFunds", "Not enough funds in corporation wallet division 1 to escrow contract reward"))?;
                            div.balance = new_div_bal;
                        }
                    } else if let Some(ch) = snap.characters.get_mut(&char_id) {
                        ch.balance = ch.balance.checked_sub(total_upfront)
                            .ok_or_else(|| rpc_err("InsufficientFunds", "Not enough ISK to escrow contract reward"))?;
                    }
                }

                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                let mut escrowed_items = Vec::new();
                for iid in item_ids_req {
                    if Some(iid) == active_ship
                        || snap.characters.get(&char_id).map(|c| c.active_ship_id == iid).unwrap_or(false)
                    {
                        return Err(rpc_err(
                            "CannotContractActiveShip",
                            "Cannot escrow the currently boarded active ship into a contract",
                        ));
                    }
                    let has_contained = snap.items.values().any(|other| {
                        other.item_id != iid && other.location_id == iid && other.quantity > 0
                    });
                    if has_contained {
                        return Err(rpc_err(
                            "CannotContractNonEmptyContainer",
                            format!("Cannot escrow item {iid} while it contains items"),
                        ));
                    }
                    let already_locked = snap.is_item_locked_in_contract_or_job(iid);
                    if snap.item_custom_names.get(&iid).is_some_and(|name| name.starts_with("courier_cargo:")) {
                        return Err(rpc_err(
                            "ItemCannotBeContracted",
                            "Courier mission items cannot be placed in contracts",
                        ));
                    }
                    let Some(item) = snap.items.get_mut(&iid) else {
                        return Err(rpc_err("ContractItemNotFound", format!("Contract item {iid} not found")));
                    };
                    let valid_item_owner = item.owner_id == char_id
                        || (for_corp && issuer_corp_id != 0 && item.owner_id == issuer_corp_id);
                    if !valid_item_owner || item.location_id != start_station_id {
                        return Err(rpc_err(
                            "ContractItemOwnershipOrLocationInvalid",
                            format!("Item {iid} must be owned by issuer at station {start_station_id}"),
                        ));
                    }
                    if item.flag == InventoryFlag::Locked || already_locked {
                        return Err(rpc_err(
                            "ContractItemLocked",
                            format!("Item {iid} is currently locked in an active job or contract"),
                        ));
                    }
                    item.location_id = id;
                    item.flag = InventoryFlag::Locked;
                    item.version += 1;
                    escrowed_items.push(iid);
                }

                if escrowed_items.is_empty() && contract_type == "Courier" {
                    return Err(rpc_err(
                        "CourierContractRequiresItems",
                        "Courier contracts must contain at least one item to wrap",
                    ));
                }

                let deposit_cents = IskCents::from_f64_isk(deposit_isk);
                if deposit_cents.0 > 0 {
                    snap.service_state_blobs.insert(
                        format!("contract_deposit:{id}"),
                        deposit_cents.0.to_string(),
                    );
                }

                let exp_unix = now_unix.saturating_add(duration_secs);
                snap.contracts.insert(
                    id,
                    ContractRecord {
                        contract_id: id,
                        issuer_id: char_id,
                        issuer_corp_id,
                        acceptor_id: None,
                        assignee_id,
                        availability,
                        contract_type: contract_type.to_string(),
                        status: "Outstanding".into(),
                        start_station_id,
                        end_station_id,
                        price,
                        reward,
                        collateral,
                        buyout,
                        current_bid: IskCents(0),
                        current_bidder_id: None,
                        bid_history: Vec::new(),
                        item_ids: escrowed_items,
                        requested_items,
                        expiration_unix: exp_unix,
                        date_issued_unix: now_unix,
                        title,
                        for_corp,
                        num_days,
                    },
                );
                Ok((id, exp_unix, broker_fee_isk, deposit_isk, max_slots))
            });

            match res {
                Ok((cid, exp_unix, broker_fee_isk, deposit_isk, max_slots)) => {
                    notify_contract(ctx, char_id, "OnContractCreated", cid);
                    if assignee_id > 0 {
                        notify_contract(ctx, assignee_id, "OnContractAssigned", cid);
                    }
                    Some(Ok(PyRep::keyval([
                        ("contractID", PyRep::Integer(cid)),
                        ("status", PyRep::String("Outstanding".into())),
                        ("expirationUnix", PyRep::Integer(exp_unix as i64)),
                        ("numDays", PyRep::Integer(num_days as i64)),
                        ("brokerFeeISK", PyRep::Real(broker_fee_isk)),
                        ("depositISK", PyRep::Real(deposit_isk)),
                        ("maxContractsLimit", PyRep::Integer(max_slots as i64)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "AcceptContract" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let accept_for_corp = args
                .get(1)
                .map(|a| a.as_bool().unwrap_or(false) || a.as_i64().unwrap_or(0) != 0)
                .unwrap_or(false);
            let caller_station = session.station_id.or(session.structure_id);
            let caller_ship = session.ship_id;
            let caller_corp = session.corporation_id.unwrap_or(0);
            let caller_alliance = session.alliance_id;
            let now_unix = ctx.now_unix();

            let expired_resolved = ctx.db.try_with_mut_snapshot(|snap| {
                    Ok::<_, std::convert::Infallible>(snap.resolve_expired_contract(cid, now_unix))
                }).unwrap();
            if expired_resolved.is_some() {
                return Some(Err(rpc_err(
                    "ContractExpired",
                    format!("Contract {cid} has expired; escrowed items and reward returned to issuer"),
                )));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(rpc_err("ContractNotFound", format!("Contract {cid} does not exist")));
                };
                if c.status != "Outstanding" {
                    return Err(rpc_err(
                        "InvalidContractState",
                        format!("Contract {cid} is in state '{}', not Outstanding", c.status),
                    ));
                }
                if !matches!(c.contract_type.as_str(), "Courier" | "ItemExchange") {
                    return Err(rpc_err(
                        "InvalidContractType",
                        format!("Contract type '{}' cannot be directly accepted via AcceptContract", c.contract_type),
                    ));
                }
                let effective_caller_corp = if caller_corp != 0 {
                    caller_corp
                } else {
                    snap.characters.get(&char_id).map(|ch| ch.corporation_id).unwrap_or(0)
                };
                let is_self_corp_contract = c.for_corp
                    && c.issuer_corp_id != 0
                    && c.issuer_corp_id == effective_caller_corp
                    && (accept_for_corp
                        || (c.assignee_id != char_id
                            && c.assignee_id != effective_caller_corp
                            && !c.availability.eq_ignore_ascii_case("Corporation")));
                if c.issuer_id == char_id || is_self_corp_contract {
                    return Err(rpc_err("CannotAcceptOwnContract", "Issuer cannot accept their own contract"));
                }
                if !check_contract_visibility(&c, char_id, effective_caller_corp, caller_alliance) {
                    return Err(rpc_err(
                        "ContractAccessDenied",
                        format!("Character {char_id} is not authorized to accept private contract {cid}"),
                    ));
                }
                if caller_station != Some(c.start_station_id) {
                    return Err(rpc_err(
                        "MustBeDockedAtStartStation",
                        format!("Accepting contract {cid} requires being docked at {}", c.start_station_id),
                    ));
                }
                for iid in &c.item_ids {
                    let valid_escrow = snap.items.get(iid)
                        .map(|item| item.location_id == cid && item.flag == InventoryFlag::Locked).unwrap_or(false);
                    if !valid_escrow {
                        return Err(rpc_err(
                            "ContractEscrowItemMissing",
                            format!("Escrowed item {iid} is missing or no longer locked in contract {cid}"),
                        ));
                    }
                }
                if c.contract_type == "Courier" && caller_ship.is_none() {
                    return Err(rpc_err(
                        "NoActiveShip",
                        "Accepting a Courier contract requires an active ship to load the courier package",
                    ));
                }

                let mut requested_transfers: Vec<(i64, i32)> = Vec::new();
                if c.contract_type == "ItemExchange" && !c.requested_items.is_empty() {
                    let mut aggregated_reqs: Vec<(u32, i32)> = Vec::new();
                    for &(req_tid, req_qty) in &c.requested_items {
                        if req_qty <= 0 {
                            continue;
                        }
                        if let Some(entry) = aggregated_reqs.iter_mut().find(|(t, _)| *t == req_tid) {
                            entry.1 = entry.1.checked_add(req_qty).ok_or_else(|| {
                                rpc_err("ContractOverflow", "Requested item quantity overflow")
                            })?;
                        } else {
                            aggregated_reqs.push((req_tid, req_qty));
                        }
                    }
                    let active_ship = caller_ship.or_else(|| {
                        snap.characters.get(&char_id).map(|ch| ch.active_ship_id).filter(|&id| id > 0)
                    });
                    for (req_tid, req_qty) in aggregated_reqs {
                        let mut needed = req_qty;
                        let mut candidate_ids: Vec<(i64, i32)> = snap.items.values().filter(|it| {
                                it.owner_id == char_id
                                    && it.location_id == c.start_station_id
                                    && it.flag == InventoryFlag::Hangar
                                    && it.type_id == req_tid
                                    && it.quantity > 0
                                    && Some(it.item_id) != active_ship
                                    && !snap.is_item_locked_in_contract_or_job(it.item_id)
                                    && !snap.items.values().any(|child| {
                                        child.item_id != it.item_id
                                            && child.location_id == it.item_id
                                            && child.quantity > 0
                                    })
                                    && !snap.item_custom_names.get(&it.item_id).is_some_and(|name| name.starts_with("courier_cargo:"))
                            }).map(|it| (it.item_id, it.quantity)).collect();
                        candidate_ids.sort_by_key(|(id, _)| *id);
                        for (cand_id, cand_qty) in candidate_ids {
                            if needed <= 0 { break; }
                            let take = needed.min(cand_qty);
                            requested_transfers.push((cand_id, take));
                            needed -= take;
                        }
                        if needed > 0 {
                            return Err(rpc_err(
                                "MissingRequestedContractItems",
                                format!(
                                    "Acceptor lacks {needed} units of requested type {req_tid} in station hangar {}",
                                    c.start_station_id
                                ),
                            ));
                        }
                    }
                }

                let required_isk = c.collateral.checked_add(c.price)
                    .ok_or_else(|| rpc_err("ContractOverflow", "Collateral + price overflow"))?;
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(rpc_err("CharacterNotFound", format!("Character {char_id} not found")));
                };
                let new_bal = ch.balance.checked_sub(required_isk)
                    .ok_or_else(|| rpc_err("InsufficientFunds", "Not enough ISK to cover contract collateral and price"))?;
                ch.balance = new_bal;
                if c.contract_type == "ItemExchange" && c.reward.0 > 0 {
                    ch.balance = ch.balance.checked_add(c.reward).unwrap_or(ch.balance);
                }

                snap.credit_contract_issuer(&c, c.price);

                if c.contract_type == "ItemExchange" {
                    let issuer_entity_id = if c.for_corp && c.issuer_corp_id != 0 {
                        c.issuer_corp_id
                    } else {
                        c.issuer_id
                    };
                    for (cand_id, take_qty) in requested_transfers {
                        let cur_qty = snap.items.get(&cand_id).map(|i| i.quantity).unwrap_or(0);
                        if take_qty >= cur_qty {
                            if let Some(it) = snap.items.get_mut(&cand_id) {
                                it.owner_id = issuer_entity_id;
                                it.location_id = c.start_station_id;
                                it.flag = InventoryFlag::Hangar;
                                it.version += 1;
                            }
                        } else if let Some(it) = snap.items.get_mut(&cand_id) { let tid = it.type_id;
                            it.quantity -= take_qty;
                            it.version += 1;
                            snap.next_entity_id += 1;
                            let split_id = snap.next_entity_id;
                            snap.items.insert(
                                split_id,
                                InventoryItemRecord {
                                    item_id: split_id,
                                    type_id: tid,
                                    owner_id: issuer_entity_id,
                                    location_id: c.start_station_id,
                                    flag: InventoryFlag::Hangar,
                                    quantity: take_qty,
                                    singleton: false,
                                    version: 1,
                                    ..Default::default()
                                },
                            );
                        }
                    }

                    for iid in &c.item_ids {
                        if let Some(item) = snap.items.get_mut(iid) {
                            item.owner_id = char_id;
                            item.location_id = c.start_station_id;
                            item.flag = InventoryFlag::Hangar;
                            item.version += 1;
                        }
                    }
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                        c_mut.acceptor_id = Some(char_id);
                        c_mut.status = "Completed".into();
                    }
                    return Ok(("Completed", c.expiration_unix, None, c.issuer_id));
                }

                snap.next_entity_id += 1;
                let wrap_id = snap.next_entity_id;
                snap.items.insert(
                    wrap_id,
                    InventoryItemRecord {
                        item_id: wrap_id,
                        type_id: 3468, // SDE Plastic Wrap (group 649)
                        owner_id: char_id,
                        location_id: c.start_station_id,
                        flag: InventoryFlag::Hangar,
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
                for iid in &c.item_ids {
                    if let Some(item) = snap.items.get_mut(iid) {
                        item.location_id = wrap_id;
                        item.flag = InventoryFlag::Locked;
                        item.version += 1;
                    }
                }
                let courier_days = (c.num_days.max(1) as u64).min(28);
                let delivery_deadline = now_unix.saturating_add(courier_days.saturating_mul(86_400));
                if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                    c_mut.acceptor_id = Some(char_id);
                    c_mut.status = "InProgress".into();
                    c_mut.expiration_unix = delivery_deadline;
                }
                Ok(("InProgress", delivery_deadline, Some(wrap_id), c.issuer_id))
            });
            match res {
                Ok((new_status, exp_unix, wrap_id_opt, issuer_id)) => {
                    notify_contract(ctx, issuer_id, "OnContractAccepted", cid);
                    if new_status == "Completed" {
                        notify_contract(ctx, issuer_id, "OnContractCompleted", cid);
                    }
                    let mut fields = vec![
                        ("contractID", PyRep::Integer(cid)),
                        ("status", PyRep::String(new_status.into())),
                        ("expirationUnix", PyRep::Integer(exp_unix as i64)),
                    ];
                    if let Some(wid) = wrap_id_opt { fields.push(("courierPackageID", PyRep::Integer(wid))); }
                    Some(Ok(PyRep::keyval(fields)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "PlaceBid" | "Bid" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let bid_isk = args.get(1).and_then(|a| a.as_f64()).unwrap_or(0.0);
            if !bid_isk.is_finite() || bid_isk <= 0.0 {
                return Some(Err(rpc_err("InvalidBidAmount", "Auction bid must be a positive finite ISK amount",)));
            }
            let caller_corp = session.corporation_id.unwrap_or(0);
            let caller_alliance = session.alliance_id;
            let now_unix = ctx.now_unix();

            let expired_resolved = ctx.db.try_with_mut_snapshot(|snap| {
                    Ok::<_, std::convert::Infallible>(snap.resolve_expired_contract(cid, now_unix))
                }).unwrap();
            if expired_resolved.is_some() {
                return Some(Err(rpc_err("ContractExpired", format!("Auction contract {cid} has expired"))));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(rpc_err("ContractNotFound", format!("Contract {cid} does not exist")));
                };
                if c.contract_type != "Auction" {
                    return Err(rpc_err("InvalidContractType", format!("Contract {cid} is not an Auction")));
                }
                if c.status != "Outstanding" {
                    return Err(rpc_err("InvalidContractState", format!("Auction {cid} is no longer Outstanding")));
                }
                if c.issuer_id == char_id {
                    return Err(rpc_err("CannotBidOnOwnContract", "Issuer cannot bid on their own auction"));
                }
                if !check_contract_visibility(&c, char_id, caller_corp, caller_alliance) {
                    return Err(rpc_err(
                        "ContractAccessDenied",
                        format!("Character {char_id} cannot bid on private auction {cid}"),
                    ));
                }

                let raw_bid = IskCents::from_f64_isk(bid_isk);
                let is_buyout = c.buyout.0 > 0 && raw_bid.0 >= c.buyout.0;
                let effective_bid = if is_buyout { c.buyout } else { raw_bid };

                if !is_buyout {
                    if c.current_bidder_id.is_none() {
                        if effective_bid.0 < c.price.0 {
                            return Err(rpc_err(
                                "BidTooLow",
                                format!(
                                    "Opening bid {:.2} ISK is below starting price {:.2} ISK",
                                    effective_bid.to_wire_f64(),
                                    c.price.to_wire_f64()
                                ),
                            ));
                        }
                    } else { let min_inc = (c.current_bid.0 / 20).max(10_000);
                        let min_required = c.current_bid.0.saturating_add(min_inc);
                        if effective_bid.0 < min_required {
                            return Err(rpc_err(
                                "BidTooLow",
                                format!(
                                    "Bid {:.2} ISK does not meet minimum increment above current bid {:.2} ISK",
                                    effective_bid.to_wire_f64(),
                                    c.current_bid.to_wire_f64()
                                ),
                            ));
                        }
                    }
                }

                let prev_self_refund = if c.current_bidder_id == Some(char_id) { c.current_bid } else { IskCents(0) };
                let bidder_avail = snap.characters.get(&char_id)
                    .map(|ch| ch.balance.checked_add(prev_self_refund).unwrap_or(ch.balance))
                    .ok_or_else(|| rpc_err("CharacterNotFound", format!("Character {char_id} not found")))?;
                if bidder_avail.0 < effective_bid.0 {
                    return Err(rpc_err("InsufficientFunds", "Insufficient ISK to place auction bid"));
                }

                let prev_bidder_opt = c.current_bidder_id;
                if let Some(prev_bidder) = prev_bidder_opt {
                    if c.current_bid.0 > 0 {
                        if let Some(prev_ch) = snap.characters.get_mut(&prev_bidder) {
                            prev_ch.balance = prev_ch.balance.checked_add(c.current_bid).unwrap_or(prev_ch.balance);
                            let bal_after = prev_ch.balance;
                            snap.append_journal(
                                prev_bidder,
                                "ContractAuctionBidRefund",
                                c.current_bid,
                                bal_after,
                                format!("Outbid refund on auction contract {cid}"),
                            );
                        }
                    }
                }

                if let Some(bidder_ch) = snap.characters.get_mut(&char_id) {
                    bidder_ch.balance = bidder_ch.balance.checked_sub(effective_bid).unwrap_or(IskCents(0));
                    let bal_after = bidder_ch.balance;
                    snap.append_journal(
                        char_id,
                        "ContractAuctionBid",
                        effective_bid,
                        bal_after,
                        format!("Bid placed on auction contract {cid}"),
                    );
                }

                let mut new_status = "Outstanding";
                if is_buyout {
                    snap.credit_contract_issuer(&c, effective_bid);
                    if c.for_corp {
                        let bal_after = snap
                            .corporations
                            .get(&c.issuer_corp_id)
                            .map(|corp| corp.master_wallet)
                            .unwrap_or(IskCents(0));
                        snap.append_journal(
                            c.issuer_corp_id,
                            "ContractAuctionSold",
                            effective_bid,
                            bal_after,
                            format!("Auction contract {cid} bought out by {char_id}"),
                        );
                    } else if let Some(issuer_ch) = snap.characters.get(&c.issuer_id) {
                        let bal_after = issuer_ch.balance;
                        snap.append_journal(c.issuer_id, "ContractAuctionSold", effective_bid, bal_after, format!("Auction contract {cid} bought out by {char_id}"));
                    }
                    for iid in &c.item_ids {
                        if let Some(item) = snap.items.get_mut(iid) {
                            item.owner_id = char_id;
                            item.location_id = c.start_station_id;
                            item.flag = InventoryFlag::Hangar;
                            item.version += 1;
                        }
                    }
                    new_status = "Completed";
                }

                if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                    c_mut.current_bid = effective_bid;
                    c_mut.current_bidder_id = Some(char_id);
                    c_mut.bid_history.push((char_id, effective_bid, now_unix));
                    if is_buyout { c_mut.acceptor_id = Some(char_id); c_mut.status = "Completed".into(); }
                }
                Ok((new_status, effective_bid, is_buyout, prev_bidder_opt, c.issuer_id))
            });

            match res {
                Ok((new_status, effective_bid, is_buyout, prev_bidder_opt, issuer_id)) => {
                    if let Some(prev_bidder) = prev_bidder_opt {
                        if prev_bidder != char_id {
                            notify_contract(ctx, prev_bidder, "OnContractOutbid", cid);
                        }
                    }
                    if is_buyout {
                        notify_contract(ctx, issuer_id, "OnContractBuyout", cid);
                        notify_contract(ctx, issuer_id, "OnContractCompleted", cid);
                    }
                    Some(Ok(PyRep::keyval([
                        ("contractID", PyRep::Integer(cid)),
                        ("status", PyRep::String(new_status.into())),
                        ("currentBidISK", PyRep::Real(effective_bid.to_wire_f64())),
                        ("highestBidderID", PyRep::Integer(char_id)),
                        ("boughtOut", PyRep::Bool(is_buyout)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "CompleteContract" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let caller_station = session.station_id.or(session.structure_id);
            let caller_ship = session.ship_id;
            let now_unix = ctx.now_unix();
            let expired_in_progress = ctx.db.try_with_mut_snapshot(|snap| {
                    Ok::<_, std::convert::Infallible>(snap.resolve_expired_contract(cid, now_unix))
                }).unwrap();
            if expired_in_progress.is_some() {
                return Some(Err(rpc_err(
                    "ContractExpired",
                    format!("Courier contract {cid} exceeded its delivery deadline; collateral and reward forfeited"),
                )));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(rpc_err("ContractNotFound", format!("Contract {cid} does not exist")));
                };
                if c.status != "InProgress" || c.acceptor_id != Some(char_id) {
                    return Err(rpc_err(
                        "InvalidContractState",
                        format!("Contract {cid} is not InProgress for character {char_id}"),
                    ));
                }
                if c.contract_type != "Courier" {
                    return Err(rpc_err("InvalidContractType", format!("Contract {cid} is not a Courier contract")));
                }
                if caller_station != Some(c.end_station_id) {
                    return Err(rpc_err(
                        "CourierNotAtDestinationStation",
                        format!("Completing Courier contract {cid} requires being docked at {}", c.end_station_id),
                    ));
                }
                let Some(ship_id) = caller_ship else {
                    return Err(rpc_err(
                        "NoActiveShip",
                        "Completing a Courier contract requires the active delivery ship",
                    ));
                };
                let ship_at_dest = snap.items.get(&ship_id).map(|sh| sh.owner_id == char_id && sh.location_id == c.end_station_id).unwrap_or(false);
                if !ship_at_dest {
                    return Err(rpc_err("DeliveryShipNotAtDestination", format!("Active ship {ship_id} is not docked at destination station {}", c.end_station_id)));
                }
                let mut wraps_to_remove = Vec::new();
                for iid in &c.item_ids {
                    let present = snap.items.get(iid).map(|item| {
                        if item.location_id == ship_id || item.location_id == c.end_station_id { return true; }
                        if let Some(wrap_item) = snap.items.get(&item.location_id).filter(|w| w.type_id == 3468 && w.owner_id == char_id && (w.location_id == ship_id || w.location_id == c.end_station_id)) {
                            wraps_to_remove.push(wrap_item.item_id);
                            return true;
                        }
                        false
                    }).unwrap_or(false);
                    if !present {
                        return Err(rpc_err("CourierPackageMissing", format!("Courier Plastic Wrap package for item {iid} is not present at destination")));
                    }
                }
                let issuer_entity_id = if c.for_corp && c.issuer_corp_id != 0 {
                    c.issuer_corp_id
                } else {
                    c.issuer_id
                };
                for iid in &c.item_ids {
                    if let Some(item) = snap.items.get_mut(iid) {
                        item.owner_id = issuer_entity_id;
                        item.location_id = c.end_station_id;
                        item.flag = InventoryFlag::Hangar;
                        item.version += 1;
                    }
                }
                for wid in wraps_to_remove { snap.items.remove(&wid); }
                if let Some(c_mut) = snap.contracts.get_mut(&cid) { c_mut.status = "Completed".into(); }
                if let Some(dep_str) = snap.service_state_blobs.remove(&format!("contract_deposit:{cid}")) {
                    if let Ok(dep_raw) = dep_str.parse::<i64>() {
                        snap.credit_contract_issuer(&c, IskCents(dep_raw));
                    }
                }
                let total_return = c.collateral.checked_add(c.reward).unwrap_or(c.collateral);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = ch.balance.checked_add(total_return).unwrap_or(ch.balance);
                }
                Ok((total_return.to_wire_f64(), c.issuer_id))
            });
            match res {
                Ok((payout, issuer_id)) => {
                    notify_contract(ctx, issuer_id, "OnContractCompleted", cid);
                    Some(Ok(PyRep::keyval([("contractID", PyRep::Integer(cid)), ("status", PyRep::String("Completed".into())), ("totalPayoutISK", PyRep::Real(payout))])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "BreakPlasticWrap" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let wrap_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now_unix = ctx.now_unix();
            let sess_loc = session.station_id.or(session.structure_id);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(wrap_item) = snap.items.get(&wrap_id).cloned() else {
                    return Err(rpc_err("ItemNotFound", format!("Plastic Wrap {wrap_id} not found")));
                };
                if wrap_item.type_id != 3468 || wrap_item.owner_id != char_id {
                    return Err(rpc_err("InvalidPlasticWrap", format!("Item {wrap_id} is not a Courier Plastic Wrap owned by {char_id}")));
                }
                let char_loc = snap.characters.get(&char_id).and_then(|c| c.station_id.or(c.structure_id));
                if sess_loc != Some(wrap_item.location_id) || char_loc != Some(wrap_item.location_id) {
                    return Err(rpc_err("CustomInfo", format!("Must be docked at location {} where Plastic Wrap {wrap_id} is located", wrap_item.location_id)));
                }
                let cid_opt = snap.contracts.values().find(|c| {
                    c.contract_type == "Courier"
                        && c.status == "InProgress"
                        && c.acceptor_id == Some(char_id)
                        && c.item_ids.iter().any(|iid| snap.items.get(iid).is_some_and(|it| it.location_id == wrap_id))
                }).map(|c| (c.contract_id, c.issuer_id, c.item_ids.clone()));
                let Some((cid, issuer_id, item_ids)) = cid_opt else {
                    return Err(rpc_err("ContractNotFound", format!("No active InProgress courier contract found for Plastic Wrap {wrap_id}")));
                };
                snap.fail_courier_contract(cid, now_unix);
                for iid in &item_ids {
                    if let Some(it) = snap.items.get_mut(iid) {
                        it.owner_id = char_id;
                        it.location_id = wrap_item.location_id;
                        it.flag = InventoryFlag::Hangar;
                        it.version += 1;
                    }
                }
                snap.items.remove(&wrap_id);
                Ok((cid, issuer_id, item_ids))
            });
            match res {
                Ok((cid, issuer_id, extracted_ids)) => {
                    notify_contract(ctx, issuer_id, "OnContractFailed", cid);
                    Some(Ok(PyRep::keyval([
                        ("contractID", PyRep::Integer(cid)),
                        ("status", PyRep::String("Failed".into())),
                        ("extractedItemIDs", PyRep::List(extracted_ids.into_iter().map(PyRep::Integer).collect())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "GM_ExpireContract" => {
            let (user_id, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let has_gm_role = snap.accounts.values().any(|a| a.user_id == user_id && a.role > 0);
                if !has_gm_role { return Err(rpc_err("GMRoleRequired", "GM_ExpireContract requires GM role")); }
                let Some(c_mut) = snap.contracts.get_mut(&cid) else {
                    return Err(rpc_err("ContractNotFound", format!("Contract {cid} does not exist")));
                };
                c_mut.expiration_unix = 1;
                let resolved = snap.resolve_expired_contract(cid, now_unix).unwrap_or("Expired");
                Ok(resolved)
            });
            match res {
                Ok(new_status) => {
                    Some(Ok(PyRep::keyval([
                        ("contractID", PyRep::Integer(cid)),
                        ("status", PyRep::String(new_status.into())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "DeleteContract" | "FailContract" | "RejectContract" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let caller_corp = session.corporation_id.unwrap_or(0);
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(rpc_err("ContractNotFound", format!("Contract {cid} does not exist")));
                };
                let issuer_entity_id = if c.for_corp && c.issuer_corp_id != 0 {
                    c.issuer_corp_id
                } else {
                    c.issuer_id
                };
                let has_caller_corp_contract_role = caller_corp != 0
                    && ((session.corp_role
                        & (eve_db::corp_roles::ROLE_CONTRACT_MANAGER
                            | eve_db::corp_roles::ROLE_DIRECTOR))
                        != 0
                        || snap.corporations.get(&caller_corp).is_some_and(|corp| {
                            corp.ceo_id == char_id
                                || corp.member_roles.get(&char_id).is_some_and(|&r| {
                                    (r & (eve_db::corp_roles::ROLE_CONTRACT_MANAGER
                                        | eve_db::corp_roles::ROLE_DIRECTOR))
                                        != 0
                                })
                        }));
                if method == "RejectContract" {
                    let is_assignee = c.assignee_id != 0
                        && (c.assignee_id == char_id || c.assignee_id == caller_corp);
                    if !is_assignee || c.status != "Outstanding" {
                        return Err(rpc_err(
                            "ContractOwnershipViolation",
                            format!("Character {char_id} cannot reject contract {cid} in state '{}'", c.status),
                        ));
                    }
                    if c.assignee_id == caller_corp
                        && c.assignee_id != char_id
                        && !has_caller_corp_contract_role
                    {
                        return Err(rpc_err(
                            "CrpAccessDenied",
                            "Corporate Contract Manager or Director role required to reject a corporate contract",
                        ));
                    }
                    snap.credit_contract_issuer(&c, c.reward);
                    for iid in &c.item_ids {
                        if let Some(item) = snap.items.get_mut(iid) {
                            item.owner_id = issuer_entity_id;
                            item.location_id = c.start_station_id;
                            item.flag = InventoryFlag::Hangar;
                            item.version += 1;
                        }
                    }
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) { c_mut.status = "Rejected".into(); }
                    return Ok(("Rejected", c.issuer_id));
                }

                let can_manage_as_issuer = c.issuer_id == char_id
                    || (c.for_corp
                        && c.issuer_corp_id != 0
                        && c.issuer_corp_id == caller_corp
                        && has_caller_corp_contract_role);
                if !can_manage_as_issuer && c.acceptor_id != Some(char_id) {
                    return Err(rpc_err(
                        "ContractOwnershipViolation",
                        format!("Character {char_id} is neither issuer nor acceptor of contract {cid}"),
                    ));
                }
                if let Some(new_state) = snap.resolve_expired_contract(cid, now_unix) {
                    return Ok((new_state, c.issuer_id));
                }
                if method == "DeleteContract"
                    && (c.status == "Outstanding" || c.status == "Expired")
                    && can_manage_as_issuer
                {
                    if c.status == "Outstanding"
                        && c.contract_type == "Auction"
                        && c.current_bidder_id.is_some()
                    {
                        return Err(rpc_err(
                            "CannotDeleteAuctionWithBids",
                            format!("Auction contract {cid} already has active bids and cannot be deleted"),
                        ));
                    }
                    if c.status == "Outstanding" {
                        snap.credit_contract_issuer(&c, c.reward);
                        if let Some(dep_str) = snap.service_state_blobs.remove(&format!("contract_deposit:{cid}")) {
                            if let Ok(dep_raw) = dep_str.parse::<i64>() {
                                snap.credit_contract_issuer(&c, IskCents(dep_raw));
                            }
                        }
                        for iid in &c.item_ids {
                            if let Some(item) = snap.items.get_mut(iid) {
                                item.owner_id = issuer_entity_id;
                                item.location_id = c.start_station_id;
                                item.flag = InventoryFlag::Hangar;
                                item.version += 1;
                            }
                        }
                    }
                    for item in snap.items.values_mut() {
                        if item.location_id == cid {
                            item.owner_id = issuer_entity_id;
                            item.location_id = c.start_station_id;
                            item.flag = InventoryFlag::Hangar;
                            item.version += 1;
                        }
                    }
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) { c_mut.status = "Deleted".into(); }
                    return Ok(("Deleted", c.issuer_id));
                }
                if method == "FailContract"
                    && c.status == "InProgress"
                    && c.contract_type == "Courier"
                    && c.acceptor_id == Some(char_id)
                { snap.fail_courier_contract(cid, now_unix); return Ok(("Failed", c.issuer_id)); }
                Err(rpc_err(
                    "InvalidContractState",
                    format!("Contract {cid} in state '{}' cannot be resolved via {method}", c.status),
                ))
            });
            match res {
                Ok((new_status, issuer_id)) => {
                    match new_status {
                        "Rejected" => notify_contract(ctx, issuer_id, "OnContractRejected", cid),
                        "Deleted" => {}
                        _ => notify_contract(ctx, issuer_id, "OnContractFailed", cid),
                    }
                    Some(Ok(PyRep::keyval([
                        ("contractID", PyRep::Integer(cid)),
                        ("status", PyRep::String(new_status.into())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "GetContractInfo" | "GetContract" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let caller_corp = session.corporation_id.unwrap_or(0);
            let caller_alliance = session.alliance_id;
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let _ = snap.resolve_expired_contract(cid, now_unix);
                let Some(c) = snap.contracts.get(&cid) else {
                    return Err(rpc_err("ContractNotFound", format!("Contract {cid} does not exist")));
                };
                if !check_contract_visibility(c, char_id, caller_corp, caller_alliance)
                    && c.acceptor_id != Some(char_id)
                {
                    return Err(rpc_err(
                        "ContractAccessDenied",
                        format!("Character {char_id} cannot view private contract {cid}"),
                    ));
                }
                Ok(contract_to_pyrep(c, snap))
            });
            if res.is_ok() {
                notify_contract(ctx, char_id, "OnShowContract", cid);
            }
            Some(res)
        }

        "SearchContracts" | "GetPublicContractList" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let filter_type = args.first().and_then(|a| a.as_str())
                .or_else(|| kwargs.get("contractType").and_then(|v| v.as_str()));
            let filter_station = kwargs.get("stationID").and_then(|v| v.as_i64());
            let filter_region = kwargs.get("regionID").and_then(|v| v.as_i64()).map(|r| r as u32);
            let filter_item_type = kwargs.get("itemTypeID").or_else(|| kwargs.get("typeID")).and_then(|v| v.as_i64())
                .map(|t| t as TypeId);
            let min_price = kwargs.get("minPrice").and_then(|v| v.as_f64()).map(IskCents::from_f64_isk);
            let max_price = kwargs.get("maxPrice").and_then(|v| v.as_f64()).map(IskCents::from_f64_isk);
            let caller_corp = session.corporation_id.unwrap_or(0);
            let caller_alliance = session.alliance_id;
            let now_unix = ctx.now_unix();

            let rows = ctx.db.try_with_mut_snapshot(|snap| {
                    let cids: Vec<i64> = snap.contracts.keys().copied().collect();
                    for cid in cids { let _ = snap.resolve_expired_contract(cid, now_unix); }
                    let mut list: Vec<ContractRecord> = snap.contracts.values().filter(|c| {
                            let region_ok = filter_region.map(|reg| {
                                    ctx.sde.stations.get(&c.start_station_id)
                                        .and_then(|st| ctx.sde.solar_systems.get(&st.solar_system_id))
                                        .map(|sys| sys.region_id == reg).unwrap_or(true)
                                }).unwrap_or(true);
                            let item_type_ok = filter_item_type.map(|tid| {
                                    c.item_ids.iter().any(|iid| {
                                        snap.items.get(iid).map(|it| it.type_id == tid).unwrap_or(false)
                                    }) || c.requested_items.iter().any(|&(rtid, _)| rtid == tid)
                                }).unwrap_or(true);
                            c.status == "Outstanding"
                                && check_contract_visibility(c, char_id, caller_corp, caller_alliance)
                                && filter_type.map(|ft| c.contract_type == ft).unwrap_or(true)
                                && filter_station.map(|st| c.start_station_id == st).unwrap_or(true)
                                && min_price.map(|mp| c.price.0 >= mp.0).unwrap_or(true)
                                && max_price.map(|mp| c.price.0 <= mp.0).unwrap_or(true)
                                && region_ok
                                && item_type_ok
                        }).cloned().collect();
                    list.sort_by_key(|c| c.contract_id);
                    Ok::<_, std::convert::Infallible>(
                        list.iter().map(|c| contract_to_pyrep(c, snap)).collect::<Vec<_>>(),
                    )
                }).unwrap();
            Some(Ok(PyRep::List(rows)))
        }

        "GetMyCurrentContractList" | "GetContractList" | "GetContractListForOwner" => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let owner_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            let caller_corp = session.corporation_id.unwrap_or(0);
            if owner_id != char_id && owner_id != caller_corp {
                return Some(Err(rpc_err("ContractAccessDenied", format!("Cannot query contract list for foreign owner {owner_id}"))));
            }
            let now_unix = ctx.now_unix();
            let rows = ctx.db.try_with_mut_snapshot(|snap| {
                let cids: Vec<i64> = snap.contracts.keys().copied().collect();
                for cid in cids { let _ = snap.resolve_expired_contract(cid, now_unix); }
                let mut list: Vec<ContractRecord> = snap.contracts.values().filter(|c| {
                    matches!(c.status.as_str(), "Outstanding" | "InProgress")
                        && (c.issuer_id == owner_id || c.issuer_corp_id == owner_id || c.acceptor_id == Some(owner_id) || c.assignee_id == owner_id || c.current_bidder_id == Some(owner_id))
                }).cloned().collect();
                list.sort_by_key(|c| c.contract_id);
                Ok::<_, std::convert::Infallible>(list.iter().map(|c| contract_to_pyrep(c, snap)).collect::<Vec<_>>())
            }).unwrap();
            Some(Ok(PyRep::List(rows)))
        }

        "GetMyExpiredContractList" => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let now_unix = ctx.now_unix();
            let rows = ctx.db.try_with_mut_snapshot(|snap| {
                let cids: Vec<i64> = snap.contracts.keys().copied().collect();
                for cid in cids { let _ = snap.resolve_expired_contract(cid, now_unix); }
                let mut list: Vec<ContractRecord> = snap.contracts.values().filter(|c| {
                    !matches!(c.status.as_str(), "Outstanding" | "InProgress")
                        && (c.issuer_id == char_id || c.acceptor_id == Some(char_id) || c.assignee_id == char_id || c.current_bidder_id == Some(char_id))
                }).cloned().collect();
                list.sort_by_key(|c| c.contract_id);
                Ok::<_, std::convert::Infallible>(list.iter().map(|c| contract_to_pyrep(c, snap)).collect::<Vec<_>>())
            }).unwrap();
            Some(Ok(PyRep::List(rows)))
        }

        "GetMyBids" => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let parse_flag = |r: &PyRep| match r { PyRep::Bool(b) => Some(*b), PyRep::Integer(i) if *i == 0 || *i == 1 => Some(*i != 0), _ => None };
            let is_accepted = args.first().and_then(parse_flag).or_else(|| kwargs.get("isAccepted").and_then(parse_flag));
            let owner_id = args.get(1).and_then(|a| a.as_i64()).filter(|&v| v > 1)
                .or_else(|| args.first().and_then(|a| a.as_i64()).filter(|&v| v > 1))
                .or_else(|| kwargs.get("ownerID").and_then(|a| a.as_i64()))
                .or_else(|| kwargs.get("forCorp").and_then(parse_flag).filter(|&b| b).and(session.corporation_id))
                .unwrap_or(char_id);
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| -> Result<Vec<PyRep>, BridgeRpcError> {
                if owner_id != char_id {
                    let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
                    let allowed = owner_id == caller_corp && snap.corporations.get(&caller_corp).is_some_and(|corp| {
                        let r = corp.member_roles.get(&char_id).copied().unwrap_or(session.corp_role);
                        char_id == corp.ceo_id || (r & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_CONTRACT_MANAGER)) != 0
                    });
                    if !allowed {
                        return Err(rpc_err("CorpRoleDenied", "Viewing corporation auction bids requires Contract Manager or Director role"));
                    }
                }
                let cids: Vec<i64> = snap.contracts.keys().copied().collect();
                for cid in cids { let _ = snap.resolve_expired_contract(cid, now_unix); }
                let dismissed: std::collections::HashSet<String> = snap.quasar_state_blobs.get("contract_dismissed_notifs")
                    .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok()).unwrap_or_default().into_iter().collect();
                let mut list: Vec<ContractRecord> = snap.contracts.values().filter(|c| {
                    if c.contract_type != "Auction" { return false; }
                    if dismissed.contains(&format!("{owner_id}:{}", c.contract_id)) || dismissed.contains(&format!("{char_id}:{}", c.contract_id)) {
                        return false;
                    }
                    let has_bid = c.current_bidder_id == Some(owner_id) || c.bid_history.iter().any(|&(b, _, _)| b == owner_id)
                        || (owner_id != char_id && c.for_corp && (c.current_bidder_id == Some(char_id) || c.bid_history.iter().any(|&(b, _, _)| b == char_id)));
                    if !has_bid { return false; }
                    match is_accepted {
                        Some(false) => c.status == "Outstanding",
                        Some(true) => c.status != "Outstanding" && (c.acceptor_id == Some(owner_id) || c.current_bidder_id == Some(owner_id) || (owner_id != char_id && c.for_corp && (c.acceptor_id == Some(char_id) || c.current_bidder_id == Some(char_id)))),
                        None => true,
                    }
                }).cloned().collect();
                list.sort_by_key(|c| c.contract_id);
                Ok(list.iter().map(|c| contract_to_pyrep(c, snap)).collect())
            });
            Some(res.map(PyRep::List))
        }

        "GetLoginInfo" => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let caller_corp = session.corporation_id.unwrap_or(0);
            let now_unix = ctx.now_unix();
            let info = ctx.db.with_snapshot(|snap| {
                let dismissed: std::collections::HashSet<String> = snap.quasar_state_blobs.get("contract_dismissed_notifs")
                    .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok()).unwrap_or_default().into_iter().collect();
                let is_dismissed = |cid: i64| dismissed.contains(&format!("{char_id}:{cid}")) || (caller_corp != 0 && dismissed.contains(&format!("{caller_corp}:{cid}")));
                let (mut assigned, mut needs_attention, mut in_progress) = (Vec::new(), Vec::new(), Vec::new());
                for c in snap.contracts.values() {
                    if is_dismissed(c.contract_id) { continue; }
                    if c.status == "Outstanding" && c.issuer_id != char_id && (c.assignee_id == char_id || (caller_corp != 0 && c.assignee_id == caller_corp)) {
                        assigned.push(PyRep::keyval([("contractID", PyRep::Integer(c.contract_id)), ("issuerID", PyRep::Integer(c.issuer_id))]));
                    } else if c.status == "Outstanding" && (c.issuer_id == char_id || c.acceptor_id == Some(char_id)) && c.expiration_unix <= now_unix {
                        needs_attention.push(PyRep::keyval([("contractID", PyRep::Integer(c.contract_id))]));
                    } else if c.status == "InProgress" && (c.issuer_id == char_id || c.acceptor_id == Some(char_id)) {
                        let exp_ft = ((c.expiration_unix as i64) + 11_644_473_600) * 10_000_000;
                        in_progress.push(PyRep::keyval([("contractID", PyRep::Integer(c.contract_id)), ("startStationID", PyRep::Integer(c.start_station_id)), ("endStationID", PyRep::Integer(c.end_station_id)), ("expires", PyRep::Integer(exp_ft))]));
                    }
                }
                PyRep::keyval([("assignedToMe", PyRep::List(assigned)), ("needsAttention", PyRep::List(needs_attention)), ("inProgress", PyRep::List(in_progress))])
            });
            Some(Ok(info))
        }

        _ => None,
    }
}
