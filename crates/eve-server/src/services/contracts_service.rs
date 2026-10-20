//! `contracts_service` — Contract Lifecycle, Escrow, Courier, ItemExchange & Auctions (`E-3`).
//!
//! Handles `contractProxy` and `contractMgr` RPCs for:
//! - `ItemExchange` (one-way, want-to-buy, and two-way `requested_items`, preserving BPC/mutated/damage attributes)
//! - `Courier` (plastic wrap `24445`, collateral + reward escrow, delivery, failure/expiry settlement)
//! - `Auction` (starting bid, minimum bid increments, outbid escrow refund, immediate buyout, expiry settlement)
//! - Contract visibility (`Public`, `Personal`, `Corporation`, `Alliance`), slot limits (`Contracting` `25235`),
//!   and search/query endpoints (`GetContractInfo`, `SearchContracts`, `GetMyCurrentContractList`,
//!   `GetMyExpiredContractList`, `GetContractListForOwner`, `GetMyBids`, `RejectContract`).

// parity: plan:E-3
// parity: feat:contracts.item-exchange, feat:contracts.item-exchange-request, feat:contracts.auction
// parity: feat:contracts.courier, feat:contracts.courier-failure, feat:contracts.visibility
// parity: feat:contracts.search, feat:contracts.my-contracts, feat:contracts.fees-limits
// parity: feat:contracts.expiry, feat:contracts.reject, feat:removed.loan-contracts

use crate::router::ServerContext;
use eve_db::{ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

fn parse_requested_items(
    raw: Option<&PyRep>,
    kwargs: &HashMap<String, PyRep>,
) -> Result<Vec<(TypeId, i32)>, BridgeRpcError> {
    let source = raw
        .or_else(|| kwargs.get("requestedItems"))
        .or_else(|| kwargs.get("requestItemTypeList"));
    let Some(rep) = source else {
        return Ok(Vec::new());
    };
    let list = match rep {
        PyRep::None => return Ok(Vec::new()),
        PyRep::List(v) | PyRep::Tuple(v) => v,
        _ => {
            return Err(BridgeRpcError {
                code: "InvalidContractParameters".into(),
                message: "requestedItems must be a list or tuple of (typeID, quantity) pairs"
                    .into(),
            })
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
            return Err(BridgeRpcError {
                code: "InvalidContractParameters".into(),
                message: "Each requested item entry must provide (typeID, quantity)".into(),
            });
        };
        if tid <= 0 || qty <= 0 || qty > i32::MAX as i64 {
            return Err(BridgeRpcError {
                code: "InvalidContractParameters".into(),
                message: format!("Invalid requested item (typeID={tid}, quantity={qty})"),
            });
        }
        out.push((tid as TypeId, qty as i32));
    }
    Ok(out)
}

fn parse_item_ids(raw: Option<&PyRep>) -> Vec<i64> {
    match raw {
        Some(PyRep::Integer(id)) if *id > 0 => vec![*id],
        Some(PyRep::List(items) | PyRep::Tuple(items)) => items
            .iter()
            .filter_map(|v| match v {
                PyRep::Integer(id) if *id > 0 => Some(*id),
                PyRep::Tuple(pair) | PyRep::List(pair) => {
                    pair.first().and_then(|x| x.as_i64()).filter(|id| *id > 0)
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn contract_to_pyrep(c: &ContractRecord) -> PyRep {
    let items_rep = PyRep::List(c.item_ids.iter().map(|&id| PyRep::Integer(id)).collect());
    let req_rep = PyRep::List(
        c.requested_items
            .iter()
            .map(|&(tid, qty)| {
                PyRep::keyval([
                    ("typeID", PyRep::Integer(tid as i64)),
                    ("quantity", PyRep::Integer(qty as i64)),
                ])
            })
            .collect(),
    );
    let bids_rep = PyRep::List(
        c.bid_history
            .iter()
            .map(|&(bidder_id, amount, ts)| {
                PyRep::keyval([
                    ("bidderID", PyRep::Integer(bidder_id)),
                    ("amountISK", PyRep::Real(amount.to_wire_f64())),
                    ("timestampUnix", PyRep::Integer(ts as i64)),
                ])
            })
            .collect(),
    );
    let availability = if c.availability.is_empty() {
        "Public".to_string()
    } else {
        c.availability.clone()
    };
    PyRep::keyval([
        ("contractID", PyRep::Integer(c.contract_id)),
        ("issuerID", PyRep::Integer(c.issuer_id)),
        ("issuerCorpID", PyRep::Integer(c.issuer_corp_id)),
        (
            "acceptorID",
            c.acceptor_id.map(PyRep::Integer).unwrap_or(PyRep::None),
        ),
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
        (
            "currentBidderID",
            c.current_bidder_id
                .map(PyRep::Integer)
                .unwrap_or(PyRep::None),
        ),
        ("itemIDs", items_rep),
        ("requestedItems", req_rep),
        ("bids", bids_rep),
        ("expirationUnix", PyRep::Integer(c.expiration_unix as i64)),
        ("dateIssuedUnix", PyRep::Integer(c.date_issued_unix as i64)),
        ("numDays", PyRep::Integer(c.num_days.max(1) as i64)),
        ("title", PyRep::String(c.title.clone())),
        ("forCorp", PyRep::Bool(c.for_corp)),
    ])
}

fn check_contract_visibility(
    c: &ContractRecord,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
) -> bool {
    if c.issuer_id == char_id {
        return true;
    }
    let avail = if c.availability.is_empty() {
        "Public"
    } else {
        c.availability.as_str()
    };
    if avail.eq_ignore_ascii_case("Public") && c.assignee_id == 0 {
        return true;
    }
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
    if service != "contractProxy" {
        return None;
    }

    match method {
        // V24.01 `code.ccp` `eve/client/script/ui/shared/neocom/contracts/createContractWnd.pyj`:
        // `contractProxy.CreateContract(type, avail, assigneeID, expireTime, duration, startStationID, endStationID, price, reward, collateral, title, description, itemList=..., flag=..., requestItemTypeList=..., forCorp=...)`
        // where `type` is `conTypeItemExchange = 1`, `conTypeAuction = 2`, `conTypeCourier = 3`.
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
                    return Some(Err(BridgeRpcError {
                        code: "InvalidContractType".into(),
                        message: format!(
                            "Unsupported contract type code {other}; must be 1 (ItemExchange), 2 (Auction), or 3 (Courier)"
                        ),
                    }));
                }
                Some(PyRep::String(s)) if matches!(s.as_str(), "Courier" | "ItemExchange" | "Auction") => {
                    s.as_str()
                }
                other => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidContractType".into(),
                        message: format!("Unsupported contract type {other:?}"),
                    }));
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
            let assignee_id = args
                .get(2)
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("assigneeID").and_then(|v| v.as_i64()))
                .unwrap_or(0);
            let num_days = args
                .get(4)
                .and_then(|a| a.as_i64())
                .filter(|&d| d > 0)
                .map(|d| d.clamp(1, 28) as u32)
                .unwrap_or(1);
            let duration_secs = if let Some(exp_min) =
                args.get(3).and_then(|a| a.as_i64()).filter(|&m| m > 0)
            {
                ((exp_min as u64).saturating_mul(60)).clamp(60, 2_419_200)
            } else {
                86_400
            };
            let Some(start_station_id) = args
                .get(5)
                .and_then(|a| a.as_i64())
                .filter(|&id| id > 0)
                .or(session.station_id)
                .or(session.structure_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDockedAtStartStation".into(),
                    message: "Caller must be docked at start_station_id to create a contract"
                        .into(),
                }));
            };
            let end_station_id = match args.get(6).and_then(|a| a.as_i64()).filter(|&id| id > 0) {
                Some(eid) => eid,
                None if matches!(contract_type, "ItemExchange" | "Auction") => start_station_id,
                None => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidContractDestination".into(),
                        message: "Courier contracts require an explicit destination stationID"
                            .into(),
                    }));
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
            let raw_title = args
                .get(10)
                .and_then(|a| a.as_str())
                .or_else(|| kwargs.get("title").and_then(|v| v.as_str()))
                .unwrap_or("");
            let title = raw_title.chars().take(128).collect::<String>();
            let _description = args.get(11).and_then(|a| a.as_str()).unwrap_or("");
            let items_source = args
                .get(12)
                .or_else(|| kwargs.get("itemList"))
                .or_else(|| kwargs.get("items"));
            let item_ids_req = parse_item_ids(items_source);
            let requested_items = match parse_requested_items(args.get(14), kwargs) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let for_corp = args
                .get(15)
                .and_then(|a| a.as_bool())
                .or_else(|| kwargs.get("forCorp").and_then(|v| v.as_bool()))
                .unwrap_or(false);

            if session.station_id.or(session.structure_id) != Some(start_station_id) {
                return Some(Err(BridgeRpcError {
                    code: "MustBeDockedAtStartStation".into(),
                    message: format!(
                        "Caller must be docked at start_station_id {start_station_id} to create a contract"
                    ),
                }));
            }

            if matches!(contract_type, "ItemExchange" | "Auction") {
                if end_station_id != start_station_id {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidContractDestination".into(),
                        message: format!(
                            "{contract_type} contracts cannot relocate items to a different destination station ({start_station_id} != {end_station_id})"
                        ),
                    }));
                }
                let allow_want_to_buy =
                    contract_type == "ItemExchange" && !requested_items.is_empty();
                if item_ids_req.is_empty() && !allow_want_to_buy {
                    return Some(Err(BridgeRpcError {
                        code: "ContractRequiresItem".into(),
                        message: format!(
                            "{contract_type} contracts require an explicit item_id in station hangar"
                        ),
                    }));
                }
            }

            if !primary_isk.is_finite()
                || primary_isk < 0.0
                || !secondary_isk.is_finite()
                || secondary_isk < 0.0
            {
                return Some(Err(BridgeRpcError {
                    code: "InvalidContractParameters".into(),
                    message: "Contract price/reward and collateral/buyout must be non-negative finite numbers"
                        .into(),
                }));
            }

            if contract_type == "Auction" && secondary_isk > 0.0 && secondary_isk < primary_isk {
                return Some(Err(BridgeRpcError {
                    code: "InvalidContractParameters".into(),
                    message: "Auction buyout price cannot be lower than starting bid".into(),
                }));
            }

            let start_sde_valid = ctx
                .sde
                .solar_systems
                .values()
                .any(|s| s.stations.iter().any(|st| st.station_id == start_station_id));
            let end_sde_valid = ctx
                .sde
                .solar_systems
                .values()
                .any(|s| s.stations.iter().any(|st| st.station_id == end_station_id));
            let now_unix = ctx.now_unix();
            let active_ship = session.ship_id;

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let start_valid = start_sde_valid
                    || snap
                        .structures
                        .get(&start_station_id)
                        .map(|st| st.state != "Destroyed")
                        .unwrap_or(false);
                let end_valid = end_sde_valid
                    || snap
                        .structures
                        .get(&end_station_id)
                        .map(|st| st.state != "Destroyed")
                        .unwrap_or(false);
                if !start_valid || !end_valid {
                    return Err(BridgeRpcError {
                        code: "InvalidContractStation".into(),
                        message: format!(
                            "Start station {start_station_id} or destination station {end_station_id} is not a valid station or active structure"
                        ),
                    });
                }

                // Settle any already-expired contracts before counting active contract slots
                let expired_ids: Vec<i64> = snap
                    .contracts
                    .values()
                    .filter(|c| {
                        c.issuer_id == char_id
                            && matches!(c.status.as_str(), "Outstanding" | "InProgress")
                            && now_unix >= c.expiration_unix
                    })
                    .map(|c| c.contract_id)
                    .collect();
                for eid in expired_ids {
                    let _ = snap.resolve_expired_contract(eid, now_unix);
                }

                // Enforce Contracting (`25235`) / Corporation Contracting (`25233`) slot limits (`feat:contracts.fees-limits`)
                let active_count = snap
                    .contracts
                    .values()
                    .filter(|c| {
                        c.issuer_id == char_id
                            && c.for_corp == for_corp
                            && matches!(c.status.as_str(), "Outstanding" | "InProgress")
                    })
                    .count();
                let max_slots = if for_corp {
                    let lvl = snap
                        .skills
                        .get(&char_id)
                        .and_then(|m| m.get(&25233))
                        .map(|s| s.level as usize)
                        .unwrap_or(1);
                    10 + 10 * lvl
                } else if let Some(skill) = snap.skills.get(&char_id).and_then(|m| m.get(&25235)) {
                    1 + 4 * (skill.level as usize)
                } else {
                    // Default fixture characters without explicit Contracting skill entry get 1 + 4*1 = 5 slots
                    5
                };
                if active_count >= max_slots {
                    return Err(BridgeRpcError {
                        code: "ContractSlotLimitReached".into(),
                        message: format!(
                            "Character {char_id} has {active_count} active contracts (max {max_slots})"
                        ),
                    });
                }

                let primary_cents = IskCents::from_f64_isk(primary_isk);
                let secondary_cents = IskCents::from_f64_isk(secondary_isk);

                // Determine price, reward, collateral, buyout by contract type
                let (price, reward, collateral, buyout) = match contract_type {
                    "Courier" => (IskCents(0), primary_cents, secondary_cents, IskCents(0)),
                    "Auction" => (primary_cents, IskCents(0), IskCents(0), secondary_cents),
                    _ => {
                        // ItemExchange: primary_cents is price paid by acceptor; secondary_cents can be reward offered by issuer (want-to-buy)
                        let wtb_reward = kwargs
                            .get("rewardISK")
                            .and_then(|v| v.as_f64())
                            .filter(|v| v.is_finite() && *v >= 0.0)
                            .map(IskCents::from_f64_isk)
                            .unwrap_or_else(|| {
                                if item_ids_req.is_empty() && !requested_items.is_empty() {
                                    secondary_cents
                                } else {
                                    IskCents(0)
                                }
                            });
                        (primary_cents, wtb_reward, IskCents(0), IskCents(0))
                    }
                };

                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let issuer_corp_id = ch.corporation_id;
                if reward.0 > 0 {
                    let new_bal = ch
                        .balance
                        .checked_sub(reward)
                        .ok_or_else(|| BridgeRpcError {
                            code: "InsufficientFunds".into(),
                            message: "Not enough ISK to escrow contract reward".into(),
                        })?;
                    ch.balance = new_bal;
                }

                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                let mut escrowed_items = Vec::new();
                for iid in item_ids_req {
                    if Some(iid) == active_ship
                        || snap
                            .characters
                            .get(&char_id)
                            .map(|c| c.active_ship_id == iid)
                            .unwrap_or(false)
                    {
                        return Err(BridgeRpcError {
                            code: "CannotContractActiveShip".into(),
                            message:
                                "Cannot escrow the currently boarded active ship into a contract"
                                    .into(),
                        });
                    }
                    let has_contained = snap.items.values().any(|other| {
                        other.item_id != iid && other.location_id == iid && other.quantity > 0
                    });
                    if has_contained {
                        return Err(BridgeRpcError {
                            code: "CannotContractNonEmptyContainer".into(),
                            message: format!(
                                "Cannot escrow item {iid} into a contract while it contains fitted modules, rigs, or cargo"
                            ),
                        });
                    }
                    let already_locked = snap.is_item_locked_in_contract_or_job(iid);
                    let Some(item) = snap.items.get_mut(&iid) else {
                        return Err(BridgeRpcError {
                            code: "ContractItemNotFound".into(),
                            message: format!("Contract item {iid} not found"),
                        });
                    };
                    if item.owner_id != char_id || item.location_id != start_station_id {
                        return Err(BridgeRpcError {
                            code: "ContractItemOwnershipOrLocationInvalid".into(),
                            message: format!(
                                "Item {iid} must be owned by issuer at station {start_station_id}"
                            ),
                        });
                    }
                    if item.flag == InventoryFlag::Locked || already_locked {
                        return Err(BridgeRpcError {
                            code: "ContractItemLocked".into(),
                            message: format!(
                                "Item {iid} is currently locked in an active industry job or contract"
                            ),
                        });
                    }
                    item.location_id = id;
                    item.flag = InventoryFlag::Locked;
                    item.version += 1;
                    escrowed_items.push(iid);
                }

                if escrowed_items.is_empty() && contract_type == "Courier" {
                    return Err(BridgeRpcError {
                        code: "CourierContractRequiresItems".into(),
                        message: "Courier contracts must contain at least one item to wrap".into(),
                    });
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
                Ok((id, exp_unix))
            });

            match res {
                Ok((cid, exp_unix)) => Some(Ok(PyRep::keyval([
                    ("contractID", PyRep::Integer(cid)),
                    ("status", PyRep::String("Outstanding".into())),
                    ("expirationUnix", PyRep::Integer(exp_unix as i64)),
                    ("numDays", PyRep::Integer(num_days as i64)),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        "AcceptContract" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let cid = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let caller_station = session.station_id.or(session.structure_id);
            let caller_ship = session.ship_id;
            let caller_corp = session.corporation_id.unwrap_or(0);
            let caller_alliance = session.alliance_id;
            let now_unix = ctx.now_unix();

            let expired_resolved = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    Ok::<_, std::convert::Infallible>(snap.resolve_expired_contract(cid, now_unix))
                })
                .unwrap();
            if expired_resolved.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ContractExpired".into(),
                    message: format!(
                        "Contract {cid} has expired; escrowed items and reward returned to issuer"
                    ),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ContractNotFound".into(),
                        message: format!("Contract {cid} does not exist"),
                    });
                };
                if c.status != "Outstanding" {
                    return Err(BridgeRpcError {
                        code: "InvalidContractState".into(),
                        message: format!(
                            "Contract {cid} is in state '{}', not Outstanding",
                            c.status
                        ),
                    });
                }
                if !matches!(c.contract_type.as_str(), "Courier" | "ItemExchange") {
                    return Err(BridgeRpcError {
                        code: "InvalidContractType".into(),
                        message: format!(
                            "Contract type '{}' cannot be directly accepted via AcceptContract; use PlaceBid for Auctions",
                            c.contract_type
                        ),
                    });
                }
                if c.issuer_id == char_id {
                    return Err(BridgeRpcError {
                        code: "CannotAcceptOwnContract".into(),
                        message: "Issuer cannot accept their own contract".into(),
                    });
                }
                if !check_contract_visibility(&c, char_id, caller_corp, caller_alliance) {
                    return Err(BridgeRpcError {
                        code: "ContractAccessDenied".into(),
                        message: format!(
                            "Character {char_id} is not authorized to accept private contract {cid}"
                        ),
                    });
                }
                if caller_station != Some(c.start_station_id) {
                    return Err(BridgeRpcError {
                        code: "MustBeDockedAtStartStation".into(),
                        message: format!(
                            "Accepting contract {cid} requires being docked at start station {}",
                            c.start_station_id
                        ),
                    });
                }
                for iid in &c.item_ids {
                    let valid_escrow = snap
                        .items
                        .get(iid)
                        .map(|item| item.location_id == cid && item.flag == InventoryFlag::Locked)
                        .unwrap_or(false);
                    if !valid_escrow {
                        return Err(BridgeRpcError {
                            code: "ContractEscrowItemMissing".into(),
                            message: format!(
                                "Escrowed item {iid} is missing or no longer locked in contract {cid}"
                            ),
                        });
                    }
                }
                if c.contract_type == "Courier" && caller_ship.is_none() {
                    return Err(BridgeRpcError {
                        code: "NoActiveShip".into(),
                        message: "Accepting a Courier contract requires an active ship to load the courier package".into(),
                    });
                }

                // Verify acceptor possesses all `requested_items` in station Hangar before any mutation (`feat:contracts.item-exchange-request`)
                let mut requested_transfers: Vec<(i64, i32)> = Vec::new();
                if c.contract_type == "ItemExchange" && !c.requested_items.is_empty() {
                    for &(req_tid, req_qty) in &c.requested_items {
                        let mut needed = req_qty;
                        let mut candidate_ids: Vec<(i64, i32)> = snap
                            .items
                            .values()
                            .filter(|it| {
                                it.owner_id == char_id
                                    && it.location_id == c.start_station_id
                                    && it.flag == InventoryFlag::Hangar
                                    && it.type_id == req_tid
                                    && it.quantity > 0
                                    && Some(it.item_id) != caller_ship
                                    && !snap.is_item_locked_in_contract_or_job(it.item_id)
                            })
                            .map(|it| (it.item_id, it.quantity))
                            .collect();
                        candidate_ids.sort_by_key(|(id, _)| *id);
                        for (cand_id, cand_qty) in candidate_ids {
                            if needed <= 0 {
                                break;
                            }
                            let take = needed.min(cand_qty);
                            requested_transfers.push((cand_id, take));
                            needed -= take;
                        }
                        if needed > 0 {
                            return Err(BridgeRpcError {
                                code: "MissingRequestedContractItems".into(),
                                message: format!(
                                    "Acceptor lacks {needed} units of requested type {req_tid} in station hangar {}",
                                    c.start_station_id
                                ),
                            });
                        }
                    }
                }

                let required_isk =
                    c.collateral.checked_add(c.price).ok_or_else(|| BridgeRpcError {
                        code: "ContractOverflow".into(),
                        message: "Collateral + price overflow".into(),
                    })?;
                let Some(ch) = snap.characters.get_mut(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    });
                };
                let new_bal = ch
                    .balance
                    .checked_sub(required_isk)
                    .ok_or_else(|| BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Not enough ISK to cover contract collateral and price".into(),
                    })?;
                ch.balance = new_bal;
                if c.contract_type == "ItemExchange" && c.reward.0 > 0 {
                    ch.balance = ch.balance.checked_add(c.reward).unwrap_or(ch.balance);
                }

                if c.price.0 > 0 {
                    if let Some(issuer) = snap.characters.get_mut(&c.issuer_id) {
                        issuer.balance = issuer
                            .balance
                            .checked_add(c.price)
                            .unwrap_or(issuer.balance);
                    }
                }

                if c.contract_type == "ItemExchange" {
                    // Transfer requested items from acceptor to issuer
                    for (cand_id, take_qty) in requested_transfers {
                        let cur_qty = snap.items.get(&cand_id).map(|i| i.quantity).unwrap_or(0);
                        if take_qty >= cur_qty {
                            if let Some(it) = snap.items.get_mut(&cand_id) {
                                it.owner_id = c.issuer_id;
                                it.location_id = c.start_station_id;
                                it.flag = InventoryFlag::Hangar;
                                it.version += 1;
                            }
                        } else if let Some(it) = snap.items.get_mut(&cand_id) {
                            let tid = it.type_id;
                            it.quantity -= take_qty;
                            it.version += 1;
                            snap.next_entity_id += 1;
                            let split_id = snap.next_entity_id;
                            snap.items.insert(
                                split_id,
                                InventoryItemRecord {
                                    item_id: split_id,
                                    type_id: tid,
                                    owner_id: c.issuer_id,
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

                    // Transfer escrowed items from issuer to acceptor (preserving singleton/BPC/mutated attributes)
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
                    return Ok(("Completed", c.expiration_unix, None));
                }

                let ship_id = caller_ship.unwrap();
                snap.next_entity_id += 1;
                let wrap_id = snap.next_entity_id;
                snap.items.insert(
                    wrap_id,
                    InventoryItemRecord {
                        item_id: wrap_id,
                        type_id: 3468, // SDE Plastic Wrap (group 649)
                        owner_id: char_id,
                        location_id: ship_id,
                        flag: InventoryFlag::Cargo,
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
                Ok(("InProgress", delivery_deadline, Some(wrap_id)))
            });
            match res {
                Ok((new_status, exp_unix, wrap_id_opt)) => {
                    let mut fields = vec![
                        ("contractID", PyRep::Integer(cid)),
                        ("status", PyRep::String(new_status.into())),
                        ("expirationUnix", PyRep::Integer(exp_unix as i64)),
                    ];
                    if let Some(wid) = wrap_id_opt {
                        fields.push(("courierPackageID", PyRep::Integer(wid)));
                    }
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
                return Some(Err(BridgeRpcError {
                    code: "InvalidBidAmount".into(),
                    message: "Auction bid must be a positive finite ISK amount".into(),
                }));
            }
            let caller_corp = session.corporation_id.unwrap_or(0);
            let caller_alliance = session.alliance_id;
            let now_unix = ctx.now_unix();

            let expired_resolved = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    Ok::<_, std::convert::Infallible>(snap.resolve_expired_contract(cid, now_unix))
                })
                .unwrap();
            if expired_resolved.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ContractExpired".into(),
                    message: format!("Auction contract {cid} has expired"),
                }));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ContractNotFound".into(),
                        message: format!("Contract {cid} does not exist"),
                    });
                };
                if c.contract_type != "Auction" {
                    return Err(BridgeRpcError {
                        code: "InvalidContractType".into(),
                        message: format!("Contract {cid} is not an Auction"),
                    });
                };
                if c.status != "Outstanding" {
                    return Err(BridgeRpcError {
                        code: "InvalidContractState".into(),
                        message: format!("Auction {cid} is no longer Outstanding"),
                    });
                }
                if c.issuer_id == char_id {
                    return Err(BridgeRpcError {
                        code: "CannotBidOnOwnContract".into(),
                        message: "Issuer cannot bid on their own auction".into(),
                    });
                }
                if !check_contract_visibility(&c, char_id, caller_corp, caller_alliance) {
                    return Err(BridgeRpcError {
                        code: "ContractAccessDenied".into(),
                        message: format!("Character {char_id} cannot bid on private auction {cid}"),
                    });
                }

                let raw_bid = IskCents::from_f64_isk(bid_isk);
                let is_buyout = c.buyout.0 > 0 && raw_bid.0 >= c.buyout.0;
                let effective_bid = if is_buyout { c.buyout } else { raw_bid };

                if !is_buyout {
                    if c.current_bidder_id.is_none() {
                        if effective_bid.0 < c.price.0 {
                            return Err(BridgeRpcError {
                                code: "BidTooLow".into(),
                                message: format!(
                                    "Opening bid {:.2} ISK is below starting price {:.2} ISK",
                                    effective_bid.to_wire_f64(),
                                    c.price.to_wire_f64()
                                ),
                            });
                        }
                    } else {
                        let min_inc = (c.current_bid.0 / 20).max(10_000); // 5% or 100.00 ISK minimum increment
                        let min_required = c.current_bid.0.saturating_add(min_inc);
                        if effective_bid.0 < min_required {
                            return Err(BridgeRpcError {
                                code: "BidTooLow".into(),
                                message: format!(
                                    "Bid {:.2} ISK does not meet minimum increment above current bid {:.2} ISK",
                                    effective_bid.to_wire_f64(),
                                    c.current_bid.to_wire_f64()
                                ),
                            });
                        }
                    }
                }

                // Check bidder's available balance (accounting for self-outbid refund if already highest bidder)
                let prev_self_refund = if c.current_bidder_id == Some(char_id) {
                    c.current_bid
                } else {
                    IskCents(0)
                };
                let bidder_avail = snap
                    .characters
                    .get(&char_id)
                    .map(|ch| ch.balance.checked_add(prev_self_refund).unwrap_or(ch.balance))
                    .ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    })?;
                if bidder_avail.0 < effective_bid.0 {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Insufficient ISK to place auction bid".into(),
                    });
                }

                // Refund previous highest bidder first
                if let Some(prev_bidder) = c.current_bidder_id {
                    if c.current_bid.0 > 0 {
                        if let Some(prev_ch) = snap.characters.get_mut(&prev_bidder) {
                            prev_ch.balance = prev_ch
                                .balance
                                .checked_add(c.current_bid)
                                .unwrap_or(prev_ch.balance);
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

                // Deduct effective_bid from new bidder
                if let Some(bidder_ch) = snap.characters.get_mut(&char_id) {
                    bidder_ch.balance = bidder_ch
                        .balance
                        .checked_sub(effective_bid)
                        .unwrap_or(IskCents(0));
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
                    if let Some(issuer_ch) = snap.characters.get_mut(&c.issuer_id) {
                        issuer_ch.balance = issuer_ch
                            .balance
                            .checked_add(effective_bid)
                            .unwrap_or(issuer_ch.balance);
                        let bal_after = issuer_ch.balance;
                        snap.append_journal(
                            c.issuer_id,
                            "ContractAuctionSold",
                            effective_bid,
                            bal_after,
                            format!("Auction contract {cid} bought out by {char_id}"),
                        );
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
                    if is_buyout {
                        c_mut.acceptor_id = Some(char_id);
                        c_mut.status = "Completed".into();
                    }
                }
                Ok((new_status, effective_bid, is_buyout))
            });

            match res {
                Ok((new_status, effective_bid, is_buyout)) => Some(Ok(PyRep::keyval([
                    ("contractID", PyRep::Integer(cid)),
                    ("status", PyRep::String(new_status.into())),
                    ("currentBidISK", PyRep::Real(effective_bid.to_wire_f64())),
                    ("highestBidderID", PyRep::Integer(char_id)),
                    ("boughtOut", PyRep::Bool(is_buyout)),
                ]))),
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
            let expired_in_progress = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    Ok::<_, std::convert::Infallible>(snap.resolve_expired_contract(cid, now_unix))
                })
                .unwrap();
            if expired_in_progress.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ContractExpired".into(),
                    message: format!(
                        "Courier contract {cid} exceeded its delivery deadline; collateral and reward forfeited to issuer"
                    ),
                }));
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(BridgeRpcError {
                        code: "ContractNotFound".into(),
                        message: format!("Contract {cid} does not exist"),
                    });
                };
                if c.status != "InProgress" || c.acceptor_id != Some(char_id) {
                    return Err(BridgeRpcError {
                        code: "InvalidContractState".into(),
                        message: format!(
                            "Contract {cid} is not InProgress for character {char_id}"
                        ),
                    });
                }
                if c.contract_type != "Courier" {
                    return Err(BridgeRpcError {
                        code: "InvalidContractType".into(),
                        message: format!("Contract {cid} is not a Courier contract"),
                    });
                }
                if caller_station != Some(c.end_station_id) {
                    return Err(BridgeRpcError {
                        code: "CourierNotAtDestinationStation".into(),
                        message: format!(
                            "Completing Courier contract {cid} requires being docked at destination station {}",
                            c.end_station_id
                        ),
                    });
                }
                let Some(ship_id) = caller_ship else {
                    return Err(BridgeRpcError {
                        code: "NoActiveShip".into(),
                        message:
                            "Completing a Courier contract requires the active delivery ship"
                                .into(),
                    });
                };
                let ship_at_dest = snap
                    .items
                    .get(&ship_id)
                    .map(|sh| sh.owner_id == char_id && sh.location_id == c.end_station_id)
                    .unwrap_or(false);
                if !ship_at_dest {
                    return Err(BridgeRpcError {
                        code: "DeliveryShipNotAtDestination".into(),
                        message: format!(
                            "Active ship {ship_id} is not docked at destination station {}",
                            c.end_station_id
                        ),
                    });
                }
                let mut wraps_to_remove = Vec::new();
                for iid in &c.item_ids {
                    let present = snap
                        .items
                        .get(iid)
                        .map(|item| {
                            if item.location_id == ship_id || item.location_id == c.end_station_id {
                                return true;
                            }
                            if let Some(wrap_item) = snap
                                .items
                                .get(&item.location_id)
                                .filter(|w| w.type_id == 3468 && w.owner_id == char_id)
                            {
                                if wrap_item.location_id == ship_id
                                    || wrap_item.location_id == c.end_station_id
                                {
                                    wraps_to_remove.push(wrap_item.item_id);
                                    return true;
                                }
                            }
                            false
                        })
                        .unwrap_or(false);
                    if !present {
                        return Err(BridgeRpcError {
                            code: "CourierPackageMissing".into(),
                            message: format!(
                                "Courier Plastic Wrap package for item {iid} is not present in active ship {ship_id} or destination station {}",
                                c.end_station_id
                            ),
                        });
                    }
                }
                for iid in &c.item_ids {
                    if let Some(item) = snap.items.get_mut(iid) {
                        item.owner_id = c.issuer_id;
                        item.location_id = c.end_station_id;
                        item.flag = InventoryFlag::Hangar;
                        item.version += 1;
                    }
                }
                for wid in wraps_to_remove {
                    snap.items.remove(&wid);
                }
                if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                    c_mut.status = "Completed".into();
                }
                let total_return = c
                    .collateral
                    .checked_add(c.reward)
                    .unwrap_or(c.collateral);
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = ch.balance.checked_add(total_return).unwrap_or(ch.balance);
                }
                Ok(total_return.to_wire_f64())
            });
            match res {
                Ok(payout) => Some(Ok(PyRep::keyval([
                    ("contractID", PyRep::Integer(cid)),
                    ("status", PyRep::String("Completed".into())),
                    ("totalPayoutISK", PyRep::Real(payout)),
                ]))),
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
                    return Err(BridgeRpcError {
                        code: "ContractNotFound".into(),
                        message: format!("Contract {cid} does not exist"),
                    });
                };
                if method == "RejectContract" {
                    let is_assignee = c.assignee_id != 0
                        && (c.assignee_id == char_id || c.assignee_id == caller_corp);
                    if !is_assignee || c.status != "Outstanding" {
                        return Err(BridgeRpcError {
                            code: "ContractOwnershipViolation".into(),
                            message: format!(
                                "Character {char_id} cannot reject contract {cid} in state '{}'",
                                c.status
                            ),
                        });
                    }
                    if c.reward.0 > 0 {
                        if let Some(issuer) = snap.characters.get_mut(&c.issuer_id) {
                            issuer.balance = issuer
                                .balance
                                .checked_add(c.reward)
                                .unwrap_or(issuer.balance);
                        }
                    }
                    for iid in &c.item_ids {
                        if let Some(item) = snap.items.get_mut(iid) {
                            item.owner_id = c.issuer_id;
                            item.location_id = c.start_station_id;
                            item.flag = InventoryFlag::Hangar;
                            item.version += 1;
                        }
                    }
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                        c_mut.status = "Rejected".into();
                    }
                    return Ok("Rejected");
                }

                if c.issuer_id != char_id && c.acceptor_id != Some(char_id) {
                    return Err(BridgeRpcError {
                        code: "ContractOwnershipViolation".into(),
                        message: format!(
                            "Character {char_id} is neither issuer nor acceptor of contract {cid}"
                        ),
                    });
                }
                if let Some(new_state) = snap.resolve_expired_contract(cid, now_unix) {
                    return Ok(new_state);
                }
                if method == "DeleteContract" && c.status == "Outstanding" && c.issuer_id == char_id
                {
                    if c.contract_type == "Auction" && c.current_bidder_id.is_some() {
                        return Err(BridgeRpcError {
                            code: "CannotDeleteAuctionWithBids".into(),
                            message: format!(
                                "Auction contract {cid} already has active bids and cannot be deleted"
                            ),
                        });
                    }
                    if c.reward.0 > 0 {
                        if let Some(issuer) = snap.characters.get_mut(&c.issuer_id) {
                            issuer.balance = issuer
                                .balance
                                .checked_add(c.reward)
                                .unwrap_or(issuer.balance);
                        }
                    }
                    for iid in &c.item_ids {
                        if let Some(item) = snap.items.get_mut(iid) {
                            item.owner_id = c.issuer_id;
                            item.location_id = c.start_station_id;
                            item.flag = InventoryFlag::Hangar;
                            item.version += 1;
                        }
                    }
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                        c_mut.status = "Deleted".into();
                    }
                    return Ok("Deleted");
                }
                if method == "FailContract"
                    && c.status == "InProgress"
                    && c.contract_type == "Courier"
                    && c.acceptor_id == Some(char_id)
                {
                    snap.fail_courier_contract(cid, now_unix);
                    return Ok("Failed");
                }
                Err(BridgeRpcError {
                    code: "InvalidContractState".into(),
                    message: format!(
                        "Contract {cid} in state '{}' cannot be resolved via {method} before expiration",
                        c.status
                    ),
                })
            });
            match res {
                Ok(new_status) => Some(Ok(PyRep::keyval([
                    ("contractID", PyRep::Integer(cid)),
                    ("status", PyRep::String(new_status.into())),
                ]))),
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
                    return Err(BridgeRpcError {
                        code: "ContractNotFound".into(),
                        message: format!("Contract {cid} does not exist"),
                    });
                };
                if !check_contract_visibility(c, char_id, caller_corp, caller_alliance)
                    && c.acceptor_id != Some(char_id)
                {
                    return Err(BridgeRpcError {
                        code: "ContractAccessDenied".into(),
                        message: format!(
                            "Character {char_id} cannot view private contract {cid}"
                        ),
                    });
                }
                Ok(contract_to_pyrep(c))
            });
            Some(res)
        }

        "SearchContracts" | "GetPublicContractList" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let filter_type = args
                .first()
                .and_then(|a| a.as_str())
                .or_else(|| kwargs.get("contractType").and_then(|v| v.as_str()));
            let filter_station = kwargs.get("stationID").and_then(|v| v.as_i64());
            let min_price = kwargs
                .get("minPrice")
                .and_then(|v| v.as_f64())
                .map(IskCents::from_f64_isk);
            let max_price = kwargs
                .get("maxPrice")
                .and_then(|v| v.as_f64())
                .map(IskCents::from_f64_isk);
            let caller_corp = session.corporation_id.unwrap_or(0);
            let caller_alliance = session.alliance_id;
            let now_unix = ctx.now_unix();

            let rows = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    let cids: Vec<i64> = snap.contracts.keys().copied().collect();
                    for cid in cids {
                        let _ = snap.resolve_expired_contract(cid, now_unix);
                    }
                    let mut list: Vec<ContractRecord> = snap
                        .contracts
                        .values()
                        .filter(|c| {
                            c.status == "Outstanding"
                                && check_contract_visibility(
                                    c,
                                    char_id,
                                    caller_corp,
                                    caller_alliance,
                                )
                                && filter_type.map(|ft| c.contract_type == ft).unwrap_or(true)
                                && filter_station
                                    .map(|st| c.start_station_id == st)
                                    .unwrap_or(true)
                                && min_price.map(|mp| c.price.0 >= mp.0).unwrap_or(true)
                                && max_price.map(|mp| c.price.0 <= mp.0).unwrap_or(true)
                        })
                        .cloned()
                        .collect();
                    list.sort_by_key(|c| c.contract_id);
                    Ok::<_, std::convert::Infallible>(
                        list.iter().map(contract_to_pyrep).collect::<Vec<_>>(),
                    )
                })
                .unwrap();
            Some(Ok(PyRep::List(rows)))
        }

        "GetMyCurrentContractList" | "GetContractList" | "GetContractListForOwner" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let owner_id = args.first().and_then(|a| a.as_i64()).unwrap_or(char_id);
            let caller_corp = session.corporation_id.unwrap_or(0);
            if owner_id != char_id && owner_id != caller_corp {
                return Some(Err(BridgeRpcError {
                    code: "ContractAccessDenied".into(),
                    message: format!("Cannot query contract list for foreign owner {owner_id}"),
                }));
            }
            let now_unix = ctx.now_unix();
            let rows = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    let cids: Vec<i64> = snap.contracts.keys().copied().collect();
                    for cid in cids {
                        let _ = snap.resolve_expired_contract(cid, now_unix);
                    }
                    let mut list: Vec<ContractRecord> = snap
                        .contracts
                        .values()
                        .filter(|c| {
                            matches!(c.status.as_str(), "Outstanding" | "InProgress")
                                && (c.issuer_id == owner_id
                                    || c.issuer_corp_id == owner_id
                                    || c.acceptor_id == Some(owner_id)
                                    || c.assignee_id == owner_id
                                    || c.current_bidder_id == Some(owner_id))
                        })
                        .cloned()
                        .collect();
                    list.sort_by_key(|c| c.contract_id);
                    Ok::<_, std::convert::Infallible>(
                        list.iter().map(contract_to_pyrep).collect::<Vec<_>>(),
                    )
                })
                .unwrap();
            Some(Ok(PyRep::List(rows)))
        }

        "GetMyExpiredContractList" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let rows = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    let cids: Vec<i64> = snap.contracts.keys().copied().collect();
                    for cid in cids {
                        let _ = snap.resolve_expired_contract(cid, now_unix);
                    }
                    let mut list: Vec<ContractRecord> = snap
                        .contracts
                        .values()
                        .filter(|c| {
                            !matches!(c.status.as_str(), "Outstanding" | "InProgress")
                                && (c.issuer_id == char_id
                                    || c.acceptor_id == Some(char_id)
                                    || c.assignee_id == char_id
                                    || c.current_bidder_id == Some(char_id))
                        })
                        .cloned()
                        .collect();
                    list.sort_by_key(|c| c.contract_id);
                    Ok::<_, std::convert::Infallible>(
                        list.iter().map(contract_to_pyrep).collect::<Vec<_>>(),
                    )
                })
                .unwrap();
            Some(Ok(PyRep::List(rows)))
        }

        "GetMyBids" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let rows = ctx
                .db
                .try_with_mut_snapshot(|snap| {
                    let cids: Vec<i64> = snap.contracts.keys().copied().collect();
                    for cid in cids {
                        let _ = snap.resolve_expired_contract(cid, now_unix);
                    }
                    let mut list: Vec<ContractRecord> = snap
                        .contracts
                        .values()
                        .filter(|c| {
                            c.contract_type == "Auction"
                                && (c.current_bidder_id == Some(char_id)
                                    || c.bid_history.iter().any(|&(b, _, _)| b == char_id))
                        })
                        .cloned()
                        .collect();
                    list.sort_by_key(|c| c.contract_id);
                    Ok::<_, std::convert::Infallible>(
                        list.iter().map(contract_to_pyrep).collect::<Vec<_>>(),
                    )
                })
                .unwrap();
            Some(Ok(PyRep::List(rows)))
        }

        _ => None,
    }
}
