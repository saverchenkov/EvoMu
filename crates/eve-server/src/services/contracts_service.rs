//! `contracts_service` — Contract Lifecycle, Escrow, Courier, ItemExchange & Auctions (`E-3`).
//!
//! Handles `contractProxy` RPCs for:
//! - `ItemExchange` (one-way, want-to-buy, and two-way `requested_items`, preserving BPC/mutated/damage attributes)
//! - `Courier` (plastic wrap `3468`, collateral + reward escrow, delivery, failure/expiry settlement, `BreakPlasticWrap`)
//! - `Auction` (starting bid, minimum bid increments, outbid escrow refund, immediate buyout, expiry settlement)
//! - Contract visibility (`Public`, `Personal`, `Corporation`, `Alliance`), slot limits (`Contracting` `25235`),
//!   and search/query endpoints (`GetContract`, `SearchContracts`, `GetMyCurrentContractList`,
//!   `GetMyExpiredContractList`, `GetContractListForOwner`, `GetMyBids`, `RejectContract`, `GM_ExpireContract`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, RpcArgs, ServerContext};
use eve_db::{ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

pub fn notify_contract(ctx: &ServerContext, char_id: i64, event: &str, cid: i64) {
    ctx.notify(NotificationTarget::Character(char_id), event, "charid", vec![PyRep::Integer(cid)]);
}

struct ContractResolvedNotifications {
    fallback_char: i64,
    item_diffs: Vec<(Option<InventoryItemRecord>, Option<InventoryItemRecord>)>,
    wallet_diffs: Vec<(bool, i64, u8, IskCents)>,
}

impl ContractResolvedNotifications {
    fn emit(self, ctx: &ServerContext) {
        for (pre, post) in self.item_diffs {
            ctx.notify_item_mutation(self.fallback_char, pre.as_ref(), post.as_ref());
        }
        for (is_corp, owner_id, div_id, balance) in self.wallet_diffs {
            ctx.notify_wallet_balance(is_corp, owner_id, div_id, balance);
        }
    }
}

struct ContractNotifyDelta {
    fallback_char: i64,
    pre_next_entity_id: i64,
    items_before: HashMap<i64, InventoryItemRecord>,
    wallets_before: Vec<(bool, i64, u8, IskCents)>,
}

impl ContractNotifyDelta {
    fn capture(
        snap: &eve_db::WorldStateSnapshot,
        fallback_char: i64,
        contract_id: i64,
        owner_ids: &[i64],
        station_ids: &[i64],
        extra_item_ids: &[i64],
    ) -> Self {
        let mut items_before = HashMap::new();
        for &iid in extra_item_ids {
            if let Some(it) = snap.items.get(&iid) {
                items_before.insert(iid, it.clone());
            }
        }
        for (&iid, it) in &snap.items {
            if (contract_id > 0 && it.location_id == contract_id) || station_ids.contains(&it.location_id) {
                items_before.insert(iid, it.clone());
            }
        }
        let mut wallets_before = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for &oid in owner_ids {
            if oid <= 0 {
                continue;
            }
            if let Some(ch) = snap.characters.get(&oid) {
                if seen.insert((false, oid, 1_u8)) {
                    wallets_before.push((false, oid, 1_u8, ch.balance));
                }
            }
            if let Some(corp) = snap.corporations.get(&oid) {
                for div_id in 1..=7_u8 {
                    if seen.insert((true, oid, div_id)) {
                        wallets_before.push((true, oid, div_id, corp.division_balance(div_id)));
                    }
                }
            }
        }
        Self {
            fallback_char,
            pre_next_entity_id: snap.next_entity_id,
            items_before,
            wallets_before,
        }
    }

    fn item_changed(a: &InventoryItemRecord, b: &InventoryItemRecord) -> bool {
        a.owner_id != b.owner_id
            || a.location_id != b.location_id
            || a.flag != b.flag
            || a.quantity != b.quantity
            || a.singleton != b.singleton
            || a.type_id != b.type_id
    }

    fn finish(self, snap: &eve_db::WorldStateSnapshot) -> ContractResolvedNotifications {
        let mut item_diffs = Vec::new();
        for (iid, pre) in &self.items_before {
            match snap.items.get(iid) {
                None => item_diffs.push((Some(pre.clone()), None)),
                Some(post) if Self::item_changed(pre, post) => {
                    item_diffs.push((Some(pre.clone()), Some(post.clone())));
                }
                _ => {}
            }
        }
        for (&iid, post) in &snap.items {
            if iid > self.pre_next_entity_id && !self.items_before.contains_key(&iid) {
                item_diffs.push((None, Some(post.clone())));
            }
        }
        let mut wallet_diffs = Vec::new();
        for (is_corp, oid, div_id, before_bal) in self.wallets_before {
            let after_bal = if is_corp {
                snap.corporations.get(&oid).map(|c| c.division_balance(div_id))
            } else {
                snap.characters.get(&oid).map(|ch| ch.balance)
            };
            if let Some(after_bal) = after_bal {
                if after_bal != before_bal {
                    wallet_diffs.push((is_corp, oid, div_id, after_bal));
                }
            }
        }
        ContractResolvedNotifications {
            fallback_char: self.fallback_char,
            item_diffs,
            wallet_diffs,
        }
    }
}

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into(), }
}

fn parse_requested_items(raw: Option<&PyRep>, kwargs: &HashMap<String, PyRep>) -> Result<Vec<(TypeId, i32)>, BridgeRpcError> {
    let source = raw.or_else(|| kwargs.get("requestedItems")).or_else(|| kwargs.get("requestItemTypeList"));
    let Some(rep) = source else { return Ok(Vec::new()); };
    let list = match rep {
        PyRep::None => return Ok(Vec::new()),
        PyRep::List(v) | PyRep::Tuple(v) => v,
        _ => return Err(rpc_err("InvalidContractParameters", "requestedItems must be a list or tuple of (typeID, quantity) pairs")),
    };
    let mut out = Vec::with_capacity(list.len());
    for entry in list {
        let (tid_opt, qty_opt) = match entry {
            PyRep::Tuple(pair) | PyRep::List(pair) => (pair.first().and_then(|v| v.as_i64()), pair.get(1).and_then(|v| v.as_i64())),
            PyRep::KeyVal(kv) => (kv.get("typeID").and_then(|v| v.as_i64()), kv.get("quantity").and_then(|v| v.as_i64())),
            _ => (None, None),
        };
        let (Some(tid), Some(qty)) = (tid_opt, qty_opt) else {
            return Err(rpc_err("InvalidContractParameters", "Each requested item entry must provide (typeID, quantity)"));
        };
        if tid <= 0 || qty <= 0 || qty > i32::MAX as i64 {
            return Err(rpc_err("InvalidContractParameters", format!("Invalid requested item (typeID={tid}, quantity={qty})")));
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
            PyRep::Tuple(pair) | PyRep::List(pair) => pair.first().and_then(|x| x.as_i64()).filter(|id| *id > 0),
            other => other.dict_get("itemID").and_then(|x| x.as_i64()).filter(|id| *id > 0),
        }).collect(),
        _ => Vec::new(),
    }
}

fn contract_location_ids(
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    location_id: i64,
) -> (i64, i64) {
    let sys_id = sde
        .stations
        .get(&location_id)
        .map(|st| st.solar_system_id)
        .or_else(|| snap.structures.get(&location_id).map(|st| st.solar_system_id))
        .unwrap_or(0);
    let region_id = if sys_id > 0 {
        sde.solar_systems
            .get(&sys_id)
            .map(|sys| i64::from(sys.region_id))
            .unwrap_or(0)
    } else {
        0
    };
    (i64::from(sys_id), region_id)
}

fn contract_to_pyrep(
    c: &ContractRecord,
    snap: &eve_db::WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
) -> PyRep {
    let items_rep = PyRep::List(c.item_ids.iter().map(|&id| PyRep::Integer(id)).collect());
    let detailed_items_rep = PyRep::List(c.item_ids.iter().filter_map(|&id| {
        let it = snap.items.get(&id)?;
        let me = it.dynamic_attributes.get(&1112).copied().unwrap_or(0.0) as i64;
        let te = it.dynamic_attributes.get(&1113).copied().unwrap_or(0.0) as i64;
        let dmg = it.dynamic_attributes.get(&3).copied().unwrap_or(0.0);
        let runs_rep = it.blueprint_runs_remaining.map(|r| PyRep::Integer(r as i64)).unwrap_or(PyRep::Integer(-1));
        let mut dyn_entries: Vec<(PyRep, PyRep)> = it.dynamic_attributes.iter().map(|(&attr_id, &val)| (PyRep::Integer(attr_id as i64), PyRep::Real(val))).collect();
        dyn_entries.sort_by_key(|(k, _)| k.as_i64().unwrap_or(0));
        Some(PyRep::keyval([
            ("itemID", PyRep::Integer(it.item_id)), ("typeID", PyRep::Integer(it.type_id as i64)),
            ("quantity", PyRep::Integer(it.quantity as i64)), ("singleton", PyRep::Bool(it.singleton)),
            ("copy", PyRep::Bool(it.is_blueprint_copy)), ("isBlueprintCopy", PyRep::Bool(it.is_blueprint_copy)),
            ("materialEfficiency", PyRep::Integer(me)), ("timeEfficiency", PyRep::Integer(te)),
            ("runs", runs_rep.clone()), ("runsRemaining", runs_rep), ("damage", PyRep::Real(dmg)),
            ("sourceMutaplasmidID", it.source_mutaplasmid_id.map(|m| PyRep::Integer(m as i64)).unwrap_or(PyRep::None)),
            ("dynamicAttributes", PyRep::Dict(dyn_entries)),
        ]))
    }).collect());
    let req_rep = PyRep::List(c.requested_items.iter().map(|&(tid, qty)| PyRep::keyval([("typeID", PyRep::Integer(tid as i64)), ("quantity", PyRep::Integer(qty as i64))])).collect());
    let bids_rep = PyRep::List(c.bid_history.iter().map(|&(bidder_id, amount, ts)| PyRep::keyval([("bidderID", PyRep::Integer(bidder_id)), ("amountISK", PyRep::Real(amount.to_wire_f64())), ("timestampUnix", PyRep::Integer(ts as i64))])).collect());
    let availability = if c.availability.is_empty() { "Public".to_string() } else { c.availability.clone() };
    let primary = c.item_ids.first().and_then(|id| snap.items.get(id));
    let primary_me = primary.map(|it| it.dynamic_attributes.get(&1112).copied().unwrap_or(0.0) as i64).unwrap_or(0);
    let primary_te = primary.map(|it| it.dynamic_attributes.get(&1113).copied().unwrap_or(0.0) as i64).unwrap_or(0);
    let primary_copy = primary.map(|it| it.is_blueprint_copy).unwrap_or(false);
    let primary_runs = primary.and_then(|it| it.blueprint_runs_remaining).map(|r| PyRep::Integer(r as i64)).unwrap_or(PyRep::Integer(-1));
    let primary_dmg = primary.map(|it| it.dynamic_attributes.get(&3).copied().unwrap_or(0.0)).unwrap_or(0.0);
    let primary_muta = primary.and_then(|it| it.source_mutaplasmid_id).map(|m| PyRep::Integer(m as i64)).unwrap_or(PyRep::None);
    let (start_sys_id, start_reg_id) = contract_location_ids(sde, snap, c.start_station_id);
    let (end_sys_id, end_reg_id) = contract_location_ids(sde, snap, c.end_station_id);
    let base_fields = [
        ("contractID", PyRep::Integer(c.contract_id)), ("issuerID", PyRep::Integer(c.issuer_id)),
        ("issuerCorpID", PyRep::Integer(c.issuer_corp_id)), ("acceptorID", c.acceptor_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("assigneeID", PyRep::Integer(c.assignee_id)), ("availability", PyRep::String(availability)),
        ("type", PyRep::String(c.contract_type.clone())), ("status", PyRep::String(c.status.clone())),
        ("startStationID", PyRep::Integer(c.start_station_id)), ("endStationID", PyRep::Integer(c.end_station_id)),
        ("startSolarSystemID", PyRep::Integer(start_sys_id)), ("endSolarSystemID", PyRep::Integer(end_sys_id)),
        ("startRegionID", PyRep::Integer(start_reg_id)), ("endRegionID", PyRep::Integer(end_reg_id)),
        ("price", PyRep::Real(c.price.to_wire_f64())), ("reward", PyRep::Real(c.reward.to_wire_f64())),
        ("collateral", PyRep::Real(c.collateral.to_wire_f64())), ("buyout", PyRep::Real(c.buyout.to_wire_f64())),
        ("currentBid", PyRep::Real(c.current_bid.to_wire_f64())), ("currentBidderID", c.current_bidder_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        ("itemIDs", items_rep), ("items", detailed_items_rep), ("requestedItems", req_rep), ("bids", bids_rep),
        ("expirationUnix", PyRep::Integer(c.expiration_unix as i64)), ("dateIssuedUnix", PyRep::Integer(c.date_issued_unix as i64)),
        ("numDays", PyRep::Integer(c.num_days.max(1) as i64)), ("title", PyRep::String(c.title.clone())), ("forCorp", PyRep::Bool(c.for_corp)),
        ("copy", PyRep::Bool(primary_copy)), ("isBlueprintCopy", PyRep::Bool(primary_copy)),
        ("materialEfficiency", PyRep::Integer(primary_me)), ("timeEfficiency", PyRep::Integer(primary_te)),
        ("runs", primary_runs.clone()), ("runsRemaining", primary_runs), ("damage", PyRep::Real(primary_dmg)),
        ("sourceMutaplasmidID", primary_muta),
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
    crate::domain::market_acl::is_contract_visible_to_character(c, char_id, corp_id, alliance_id)
}

fn active_corp_acct_div(session: &SessionState) -> usize {
    (session.corp_account_key - 1000).clamp(0, 6) as usize
}

fn is_corp_contract_flag(flag: InventoryFlag) -> bool {
    matches!(
        flag,
        InventoryFlag::CorpDeliveries
            | InventoryFlag::CorpSAG1
            | InventoryFlag::CorpSAG2
            | InventoryFlag::CorpSAG3
            | InventoryFlag::CorpSAG4
            | InventoryFlag::CorpSAG5
            | InventoryFlag::CorpSAG6
            | InventoryFlag::CorpSAG7
    )
}

fn effective_corp_roles_at_location(
    snap: &eve_db::WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    location_id: Option<i64>,
    now_unix: u64,
) -> (bool, u64) {
    snap.effective_corp_roles_at(corp_id, char_id, location_id, 0, now_unix)
}

fn has_corp_contract_role(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    corp_id: i64,
    now_unix: u64,
) -> bool {
    const CONTRACT_ROLES: u64 =
        eve_db::corp_roles::ROLE_CONTRACT_MANAGER | eve_db::corp_roles::ROLE_DIRECTOR;
    if corp_id == 0 || !snap.corporations.contains_key(&corp_id) {
        return false;
    }
    let (is_ceo, roles) = effective_corp_roles_at_location(snap, corp_id, char_id, None, now_unix);
    is_ceo || (roles & CONTRACT_ROLES) != 0
}

fn has_corp_wallet_take_role(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    corp_id: i64,
    acct_div_idx: usize,
    now_unix: u64,
) -> bool {
    if corp_id == 0 || !snap.corporations.contains_key(&corp_id) {
        return false;
    }
    let (is_ceo, roles) = effective_corp_roles_at_location(snap, corp_id, char_id, None, now_unix);
    let take_role = eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[acct_div_idx.min(6)];
    is_ceo
        || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR)
        || eve_db::corp_roles::has_role(roles, take_role)
}

fn has_corp_flag_take_role_at_location(
    snap: &eve_db::WorldStateSnapshot,
    char_id: i64,
    corp_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    now_unix: u64,
) -> bool {
    if corp_id == 0 || !snap.corporations.contains_key(&corp_id) {
        return false;
    }
    let (is_ceo, roles) =
        effective_corp_roles_at_location(snap, corp_id, char_id, Some(location_id), now_unix);
    if is_ceo || eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DIRECTOR) {
        return true;
    }
    match flag {
        InventoryFlag::CorpDeliveries => {
            eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_DELIVERIES_HANGAR_TAKE)
        }
        InventoryFlag::CorpSAG1 => {
            eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[0])
        }
        InventoryFlag::CorpSAG2 => {
            eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[1])
        }
        InventoryFlag::CorpSAG3 => {
            eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[2])
        }
        InventoryFlag::CorpSAG4 => {
            eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[3])
        }
        InventoryFlag::CorpSAG5 => {
            eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[4])
        }
        InventoryFlag::CorpSAG6 => {
            eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[5])
        }
        InventoryFlag::CorpSAG7 => {
            eve_db::corp_roles::has_role(roles, eve_db::corp_roles::ROLE_HANGAR_CAN_TAKE[6])
        }
        _ => false,
    }
}

fn is_item_in_asset_safety_wrap(
    ext: &crate::services::station_structure::structure_upwell_ops::StructureExtendedState,
    item_id: i64,
    item_location_id: i64,
    item_type_id: u32,
) -> bool {
    item_type_id == 60
        || ext.asset_safety_wraps.values().any(|w| {
            !w.retrieval_fee_paid
                && (w.wrap_id == item_id
                    || w.wrap_id == item_location_id
                    || w.item_ids.contains(&item_id))
        })
}

fn pop_escrowed_item_orig_flag(
    snap: &mut eve_db::WorldStateSnapshot,
    cid: i64,
    iid: i64,
) -> InventoryFlag {
    let iid_sub = iid.to_string();
    let kv_flag = snap
        .kv(eve_db::Ns::ContractOrigFlag)
        .get::<u16>(cid, &iid_sub);
    snap.kv_mut(eve_db::Ns::ContractOrigFlag)
        .remove(cid, &iid_sub);
    kv_flag
        .or_else(|| {
            snap.service_state_blobs
                .remove(&format!("contract_orig_flag:{cid}:{iid}"))
                .and_then(|s| s.parse::<u16>().ok())
        })
        .map(InventoryFlag::from_u16)
        .filter(|f| *f != InventoryFlag::Locked)
        .unwrap_or(InventoryFlag::Hangar)
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "contractProxy" { return None; }

    match method {
        "CreateContract" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let payload_opt = match args.first() {
                Some(
                    p @ (PyRep::KeyVal(_)
                    | PyRep::Dict(_)
                    | PyRep::ObjectEx { .. }
                    | PyRep::Object { .. }),
                ) => Some(p),
                _ => None,
            };
            let p_field = |key: &str| payload_opt.and_then(|p| p.dict_get(key));
            let p_field_some = |key: &str| p_field(key).filter(|v| !matches!(v, PyRep::None));
            let contract_type = match p_field_some("contractType").or_else(|| p_field_some("type")).or_else(|| args.first()) {
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
            let availability = if let Some(av_kw) = kwargs.get("availability").or_else(|| p_field_some("availability")).and_then(|v| v.as_str()) {
                av_kw.to_string()
            } else {
                match p_field_some("availability")
                    .or_else(|| p_field_some("isPrivate"))
                    .or_else(|| kwargs.get("isPrivate"))
                    .or_else(|| args.get(1))
                {
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
            let rpc = RpcArgs::new(args, kwargs);
            let assignee_id = match p_field_some("assigneeID").or_else(|| p_field_some("assignedToID")) {
                Some(PyRep::Integer(v)) => *v,
                Some(PyRep::None) => 0,
                Some(_) => return Some(Err(rpc_err("InvalidContractParameters", "assigneeID must be an integer"))),
                None => match rpc.opt_i64(2, "assigneeID") {
                    Ok(Some(v)) => v,
                    Ok(None) => kwargs.get("assignedToID").and_then(|v| v.as_i64()).unwrap_or(0),
                    Err(e) => return Some(Err(e)),
                },
            };
            let availability = if assignee_id > 0 && availability.eq_ignore_ascii_case("Public") {
                "Personal".to_string()
            } else {
                availability
            };
            let num_days = p_field_some("numDays").or_else(|| args.get(4)).and_then(|a| a.as_i64()).filter(|&d| d > 0).map(|d| d.clamp(1, 28) as u32)
                .unwrap_or(1);
            let duration_secs = if let Some(exp_min) = p_field_some("expireTime")
                .or_else(|| p_field_some("minutesExpire"))
                .or_else(|| kwargs.get("minutesExpire"))
                .or_else(|| args.get(3))
                .and_then(|a| a.as_i64())
                .filter(|&m| m > 0)
            { ((exp_min as u64).saturating_mul(60)).clamp(60, 2_419_200) } else { 86_400 };
            let Some(start_station_id) = p_field_some("startStationID").or_else(|| args.get(5)).and_then(|a| a.as_i64()).filter(|&id| id > 0)
                .or(session.station_id).or(session.structure_id)
            else {
                return Some(Err(rpc_err(
                    "MustBeDockedAtStartStation",
                    "Caller must be docked at start_station_id to create a contract",
                )));
            };
            let end_station_id = match p_field_some("endStationID")
                .or_else(|| p_field_some("destinationID"))
                .or_else(|| kwargs.get("destinationID"))
                .or_else(|| args.get(6))
                .and_then(|a| a.as_i64())
                .filter(|&id| id > 0)
            {
                Some(eid) => eid,
                None if matches!(contract_type, "ItemExchange" | "Auction") => start_station_id,
                None => {
                    return Some(Err(rpc_err(
                        "InvalidContractDestination",
                        "Courier contracts require an explicit destination stationID",
                    )));
                }
            };
            let parse_opt_isk = |field: &str, idx: usize| -> Result<f64, BridgeRpcError> {
                if let Some(v) = p_field(field) {
                    return match v {
                        PyRep::None => Ok(0.0),
                        PyRep::Real(f) if f.is_finite() => Ok(*f),
                        PyRep::Integer(i) => Ok(*i as f64),
                        _ => Err(rpc_err("InvalidContractParameters", format!("{field} must be a finite number"))),
                    };
                }
                Ok(rpc.opt_finite_f64(idx, field)?.unwrap_or(0.0))
            };
            let price_arg = match parse_opt_isk("price", 7) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let reward_arg = match parse_opt_isk("reward", 8) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let collateral_arg = match parse_opt_isk("collateral", 9) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let (primary_isk, secondary_isk) = match contract_type {
                "Courier" => (reward_arg, collateral_arg),
                "Auction" => (price_arg, collateral_arg),
                _ => (price_arg, reward_arg),
            };
            let raw_title = if let Some(t) = p_field_some("title")
                .filter(|v| v.as_str() != Some(""))
                .or_else(|| p_field_some("description"))
            {
                match t {
                    PyRep::None => "",
                    other => match other.as_str() {
                        Some(s) => s,
                        None => return Some(Err(rpc_err("InvalidContractParameters", "title must be a string"))),
                    },
                }
            } else {
                match rpc.opt_str(10, "title") {
                    Ok(Some(s)) if !s.is_empty() => s,
                    Ok(_) => kwargs.get("description").and_then(|v| v.as_str()).unwrap_or(""),
                    Err(e) => return Some(Err(e)),
                }
            };
            let title = raw_title.chars().take(128).collect::<String>();
            let items_source = p_field_some("itemList").or_else(|| p_field_some("items")).or_else(|| args.get(12)).or_else(|| kwargs.get("itemList")).or_else(|| kwargs.get("items"));
            let item_ids_req = parse_item_ids(items_source);
            let requested_items = match parse_requested_items(
                p_field_some("requestedItems")
                    .or_else(|| p_field_some("requestItemTypeList"))
                    .or_else(|| args.get(14)),
                kwargs,
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let for_corp = if let Some(fc) = p_field("forCorp") {
                match fc {
                    PyRep::None => false,
                    PyRep::Bool(b) => *b,
                    PyRep::Integer(i) => *i != 0,
                    _ => return Some(Err(rpc_err("InvalidContractParameters", "forCorp must be a boolean"))),
                }
            } else {
                match rpc.opt_bool(15, "forCorp") { Ok(v) => v.unwrap_or(false), Err(e) => return Some(Err(e)) }
            };

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
            let acct_div_idx = active_corp_acct_div(session);
            if for_corp {
                if let Some(corp_id) = ctx
                    .db
                    .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id))
                    .or(session.corporation_id)
                    .filter(|&id| id > 0)
                {
                    crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
                }
            }
            let cs_state = crate::services::inventory_fitting::fitting_containers_ships::read_container_ship_state(ctx);

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
                    if !snap.corporations.contains_key(&issuer_corp_id) {
                        return Err(rpc_err("CrpAccessDenied", "Corporation contracts require a player corporation"));
                    }
                    if !has_corp_contract_role(snap, char_id, issuer_corp_id, now_unix) {
                        return Err(rpc_err("CrpAccessDenied", "Creating corporation contracts requires ROLE_CONTRACT_MANAGER or ROLE_DIRECTOR"));
                    }
                }
                let delta = ContractNotifyDelta::capture(
                    snap,
                    char_id,
                    0,
                    &[char_id, issuer_corp_id],
                    &[start_station_id, end_station_id],
                    &item_ids_req,
                );
                let fee_cents = IskCents::from_f64_isk(broker_fee_isk + deposit_isk);
                let total_upfront = reward
                    .checked_add(fee_cents)
                    .ok_or_else(|| rpc_err("InvalidContractParameters", "Contract reward and fee overflow"))?;
                if total_upfront.0 > 0 {
                    if for_corp {
                        if !has_corp_wallet_take_role(snap, char_id, issuer_corp_id, acct_div_idx, now_unix) {
                            return Err(rpc_err(
                                "CrpAccessDenied",
                                format!(
                                    "Funded corporation contracts require ROLE_DIRECTOR or ROLE_ACCOUNT_CAN_TAKE for division {}",
                                    acct_div_idx + 1
                                ),
                            ));
                        }
                        let corp = snap.corporations.get_mut(&issuer_corp_id).unwrap();
                        let div_num = (acct_div_idx + 1) as u8;
                        corp.debit_division(div_num, total_upfront)
                            .ok_or_else(|| rpc_err("InsufficientFunds", format!("Not enough funds in corporation wallet division {div_num} to escrow contract reward")))?;
                    } else if let Some(ch) = snap.characters.get_mut(&char_id) {
                        ch.balance = ch.balance.checked_sub(total_upfront)
                            .ok_or_else(|| rpc_err("InsufficientFunds", "Not enough ISK to escrow contract reward"))?;
                    }
                }

                let ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
                snap.next_entity_id += 1;
                let id = snap.next_entity_id;
                let mut escrowed_items = Vec::new();
                for iid in item_ids_req {
                    if snap.is_character_boarded_or_saved_ship(char_id, active_ship, iid) {
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
                    if cs_state.alsc_locked_items.contains(&iid) {
                        return Err(rpc_err(
                            "ContractItemLocked",
                            format!("Item {iid} is locked inside an Audit Log Secure Container"),
                        ));
                    }
                    let already_locked = snap.is_item_locked_in_contract_or_job(iid);
                    if snap.item_custom_names.get(&iid).is_some_and(|name| name.starts_with("courier_cargo:")) {
                        return Err(rpc_err(
                            "ItemCannotBeContracted",
                            "Courier mission items cannot be placed in contracts",
                        ));
                    }
                    let Some(item_snap) = snap.items.get(&iid).cloned() else {
                        return Err(rpc_err("ContractItemNotFound", format!("Contract item {iid} not found")));
                    };
                    if snap.items.get(&item_snap.location_id).is_some_and(|p| p.type_id == 60 || crate::services::inventory_fitting::fitting_containers_ships::is_container_group(ctx.sde.types.get(&p.type_id).map(|t| t.group_id).unwrap_or(0), p.type_id)) {
                        return Err(rpc_err("ItemInsideContainer", format!("Item {iid} is inside a container and must be moved to item hangar before adding to contract")));
                    }
                    if is_item_in_asset_safety_wrap(&ext, iid, item_snap.location_id, item_snap.type_id) {
                        return Err(rpc_err(
                            "ItemCannotBeContracted",
                            format!("Item {iid} is an Asset Safety wrap or sealed inside one"),
                        ));
                    }
                    let valid_item_owner = if for_corp {
                        issuer_corp_id != 0 && item_snap.owner_id == issuer_corp_id
                    } else {
                        item_snap.owner_id == char_id
                    };
                    if !valid_item_owner || item_snap.location_id != start_station_id {
                        return Err(rpc_err(
                            "ContractItemOwnershipOrLocationInvalid",
                            format!("Item {iid} must be owned by issuer at station {start_station_id}"),
                        ));
                    }
                    let item_flag = item_snap.flag;
                    if item_flag == InventoryFlag::Locked
                        || already_locked
                        || snap.is_item_or_ancestor_locked_or_impounded(iid)
                        || snap.is_war_hq_structure(iid)
                        || snap.is_container_non_empty(iid)
                        || !snap.has_structure_acl_access(start_station_id, char_id)
                    {
                        return Err(rpc_err(
                            "ContractItemLocked",
                            format!("Item {iid} is locked, non-empty, or ineligible for contracting"),
                        ));
                    }
                    if for_corp {
                        if !is_corp_contract_flag(item_snap.flag) {
                            return Err(rpc_err(
                                "ContractItemFlagInvalid",
                                format!("Corporate contract item {iid} must be in a corporate hangar division or CorpDeliveries (flag {:?})", item_snap.flag),
                            ));
                        }
                        if !has_corp_flag_take_role_at_location(
                            snap,
                            char_id,
                            issuer_corp_id,
                            start_station_id,
                            item_snap.flag,
                            now_unix,
                        ) {
                            return Err(rpc_err(
                                "CrpAccessDenied",
                                format!("Lacks corporation hangar take role for {:?} at location {start_station_id}", item_snap.flag),
                            ));
                        }
                    } else if item_snap.flag != InventoryFlag::Hangar {
                        return Err(rpc_err(
                            "ContractItemFlagInvalid",
                            format!("Contract item {iid} must be in personal hangar (flag {:?} is not contractable)", item_snap.flag),
                        ));
                    }
                    let orig_flag = item_snap.flag;
                    if let Some(item) = snap.items.get_mut(&iid) {
                        item.location_id = id;
                        item.flag = InventoryFlag::Locked;
                        item.version += 1;
                    }
                    if for_corp {
                        snap.kv_mut(eve_db::Ns::ContractOrigFlag)
                            .set_with_unix(id, iid.to_string(), &(orig_flag as u16), now_unix)
                            .map_err(|e| rpc_err("ServiceStateLimitExceeded", e))?;
                    }
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
                    snap.fleet_and_social_ops
                        .contract_deposits
                        .insert(id, deposit_cents.0);
                }

                let exp_unix = now_unix.saturating_add(duration_secs);
                snap.contracts.insert(id, ContractRecord {
                    contract_id: id, issuer_id: char_id, issuer_corp_id, acceptor_id: None, assignee_id,
                    availability, contract_type: contract_type.to_string(), status: "Outstanding".into(),
                    start_station_id, end_station_id, price, reward, collateral, buyout,
                    current_bid: IskCents(0), current_bidder_id: None, bid_history: Vec::new(),
                    item_ids: escrowed_items, requested_items, expiration_unix: exp_unix,
                    date_issued_unix: now_unix, title, for_corp, num_days,
                });
                let notify_delta = delta.finish(snap);
                Ok((id, exp_unix, broker_fee_isk, deposit_isk, max_slots, notify_delta))
            });

            match res {
                Ok((cid, exp_unix, broker_fee_isk, deposit_isk, max_slots, notify_delta)) => {
                    notify_delta.emit(ctx);
                    let escrowed_ids = ctx.db.with_snapshot(|s| s.contracts.get(&cid).map(|c| c.item_ids.clone()).unwrap_or_default());
                    for iid in escrowed_ids {
                        let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::void_ship_insurance_contract(ctx, iid);
                    }
                    notify_contract(ctx, char_id, "OnContractCreated", cid);
                    if assignee_id > 0 { notify_contract(ctx, assignee_id, "OnContractAssigned", cid); }
                    Some(Ok(PyRep::keyval([
                        ("contractID", PyRep::Integer(cid)), ("status", PyRep::String("Outstanding".into())),
                        ("expirationUnix", PyRep::Integer(exp_unix as i64)), ("numDays", PyRep::Integer(num_days as i64)),
                        ("brokerFeeISK", PyRep::Real(broker_fee_isk)), ("depositISK", PyRep::Real(deposit_isk)),
                        ("maxContractsLimit", PyRep::Integer(max_slots as i64)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "AcceptContract" => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let rpc = RpcArgs::new(args, kwargs);
            let cid = match rpc.req_positive_i64(0, "contractID") { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let accept_for_corp = match rpc.opt_bool(1, "forCorp") { Ok(v) => v.unwrap_or(false), Err(e) => return Some(Err(e)) };
            let caller_station = session.station_id.or(session.structure_id);
            let caller_ship = session.ship_id;
            let caller_corp = session.corporation_id.unwrap_or(0);
            let caller_alliance = session.alliance_id;
            let now_unix = ctx.now_unix();
            let acct_div_idx = active_corp_acct_div(session);
            if let Some(corp_id) = ctx
                .db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id))
                .or(session.corporation_id)
                .filter(|&id| id > 0)
            {
                crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
            }
            let cs_state = crate::services::inventory_fitting::fitting_containers_ships::read_container_ship_state(ctx);

            let expired_resolved = ctx.db.try_with_mut_snapshot(|snap| {
                Ok::<_, std::convert::Infallible>(snap.resolve_expired_contract(cid, now_unix))
            }).unwrap();
            if expired_resolved.is_some() {
                return Some(Err(rpc_err("ContractExpired", format!("Contract {cid} has expired; escrowed items and reward returned to issuer"))));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(rpc_err("ContractNotFound", format!("Contract {cid} does not exist")));
                };
                if c.status == "Expired" {
                    return Err(rpc_err("ContractExpired", format!("Contract {cid} has expired; escrowed items and reward returned to issuer")));
                }
                if c.status != "Outstanding" {
                    return Err(rpc_err("InvalidContractState", format!("Contract {cid} is in state '{}', not Outstanding", c.status)));
                }
                if !matches!(c.contract_type.as_str(), "Courier" | "ItemExchange") {
                    return Err(rpc_err("InvalidContractType", format!("Contract type '{}' cannot be directly accepted via AcceptContract", c.contract_type)));
                }
                let effective_caller_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|ch| ch.corporation_id)
                    .filter(|&id| id != 0)
                    .unwrap_or(caller_corp);
                let is_corp_accept = accept_for_corp
                    || (c.assignee_id != 0
                        && c.assignee_id == effective_caller_corp
                        && c.assignee_id != char_id
                        && !c.availability.eq_ignore_ascii_case("Corporation"));
                let is_self_corp_contract = c.for_corp
                    && c.issuer_corp_id != 0
                    && c.issuer_corp_id == effective_caller_corp
                    && (is_corp_accept || (c.assignee_id != char_id && c.assignee_id != effective_caller_corp && !c.availability.eq_ignore_ascii_case("Corporation")));
                if c.issuer_id == char_id || is_self_corp_contract {
                    return Err(rpc_err("CannotAcceptOwnContract", "Issuer cannot accept their own contract"));
                }
                if !check_contract_visibility(&c, char_id, effective_caller_corp, caller_alliance) {
                    return Err(rpc_err("ContractAccessDenied", format!("Character {char_id} is not authorized to accept private contract {cid}")));
                }
                if is_corp_accept && !has_corp_contract_role(snap, char_id, effective_caller_corp, now_unix) {
                    return Err(rpc_err("CrpAccessDenied", "Accepting a contract on behalf of a corporation requires ROLE_CONTRACT_MANAGER or ROLE_DIRECTOR"));
                }
                if caller_station != Some(c.start_station_id) {
                    return Err(rpc_err("MustBeDockedAtStartStation", format!("Accepting contract {cid} requires being docked at {}", c.start_station_id)));
                }
                for iid in &c.item_ids {
                    let valid_escrow = snap.items.get(iid).map(|item| item.location_id == cid && item.flag == InventoryFlag::Locked).unwrap_or(false);
                    if !valid_escrow {
                        return Err(rpc_err("ContractEscrowItemMissing", format!("Escrowed item {iid} is missing or no longer locked in contract {cid}")));
                    }
                }
                if c.contract_type == "Courier" && caller_ship.is_none() {
                    return Err(rpc_err("NoActiveShip", "Accepting a Courier contract requires an active ship to load the courier package"));
                }

                let ext = crate::services::station_structure::structure_upwell_ops::load_ext_state(snap);
                let mut requested_transfers: Vec<(i64, i32)> = Vec::new();
                if c.contract_type == "ItemExchange" && !c.requested_items.is_empty() {
                    let mut aggregated_reqs: Vec<(u32, i32)> = Vec::new();
                    for &(req_tid, req_qty) in &c.requested_items {
                        if req_qty <= 0 { continue; }
                        if let Some(entry) = aggregated_reqs.iter_mut().find(|(t, _)| *t == req_tid) {
                            entry.1 = entry.1.checked_add(req_qty).ok_or_else(|| rpc_err("ContractOverflow", "Requested item quantity overflow"))?;
                        } else {
                            aggregated_reqs.push((req_tid, req_qty));
                        }
                    }
                    let active_ship = caller_ship.or_else(|| snap.characters.get(&char_id).map(|ch| ch.active_ship_id).filter(|&id| id > 0));
                    for (req_tid, req_qty) in aggregated_reqs {
                        let mut needed = req_qty;
                        let mut candidate_ids: Vec<(i64, i32)> = snap.items.values().filter(|it| {
                            let owner_flag_ok = if is_corp_accept {
                                it.owner_id == effective_caller_corp
                                    && is_corp_contract_flag(it.flag)
                                    && has_corp_flag_take_role_at_location(
                                        snap,
                                        char_id,
                                        effective_caller_corp,
                                        c.start_station_id,
                                        it.flag,
                                        now_unix,
                                    )
                            } else {
                                it.owner_id == char_id && it.flag == InventoryFlag::Hangar
                            };
                            owner_flag_ok
                                && it.location_id == c.start_station_id
                                && it.type_id == req_tid
                                && it.quantity > 0
                                && !it.is_blueprint_copy
                                && it.dynamic_attributes.get(&3).copied().unwrap_or(0.0) <= 0.0
                                && it.source_mutaplasmid_id.is_none()
                                && Some(it.item_id) != active_ship
                                && !cs_state.alsc_locked_items.contains(&it.item_id)
                                && !is_item_in_asset_safety_wrap(&ext, it.item_id, it.location_id, it.type_id)
                                && !snap.is_item_locked_in_contract_or_job(it.item_id)
                                && !snap.items.values().any(|child| child.item_id != it.item_id && child.location_id == it.item_id && child.quantity > 0)
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
                            return Err(rpc_err("MissingRequestedContractItems", format!("Acceptor lacks {needed} units of requested type {req_tid} in station hangar {}", c.start_station_id)));
                        }
                    }
                }

                let acceptor_entity = if is_corp_accept { effective_caller_corp } else { char_id };
                let issuer_entity = if c.for_corp && c.issuer_corp_id != 0 { c.issuer_corp_id } else { c.issuer_id };
                let accept_delta = ContractNotifyDelta::capture(
                    snap,
                    char_id,
                    cid,
                    &[char_id, effective_caller_corp, c.issuer_id, c.issuer_corp_id, issuer_entity, acceptor_entity],
                    &[c.start_station_id, c.end_station_id],
                    &c.item_ids,
                );
                let required_isk = c.collateral.checked_add(c.price).ok_or_else(|| rpc_err("ContractOverflow", "Collateral + price overflow"))?;
                if is_corp_accept {
                    if required_isk.0 > 0
                        && !has_corp_wallet_take_role(snap, char_id, effective_caller_corp, acct_div_idx, now_unix)
                    {
                        return Err(rpc_err(
                            "CrpAccessDenied",
                            format!(
                                "Accepting funded corporate contracts requires ROLE_DIRECTOR or ROLE_ACCOUNT_CAN_TAKE for division {}",
                                acct_div_idx + 1
                            ),
                        ));
                    }
                    let div_num = (acct_div_idx + 1) as u8;
                    let Some(corp) = snap.corporations.get_mut(&effective_caller_corp) else {
                        return Err(rpc_err("CorpNotFound", format!("Corporation {effective_caller_corp} not found")));
                    };
                    let cur = corp.division_balance(div_num);
                    let mut new_bal = cur.checked_sub(required_isk).ok_or_else(|| rpc_err("InsufficientFunds", "Not enough corporation ISK to cover contract collateral and price"))?;
                    if c.contract_type == "ItemExchange" && c.reward.0 > 0 {
                        new_bal = new_bal.checked_add(c.reward).unwrap_or(new_bal);
                    }
                    corp.set_division_balance(div_num, new_bal);
                } else {
                    let Some(ch) = snap.characters.get_mut(&char_id) else {
                        return Err(rpc_err("CharacterNotFound", format!("Character {char_id} not found")));
                    };
                    let new_bal = ch.balance.checked_sub(required_isk).ok_or_else(|| rpc_err("InsufficientFunds", "Not enough ISK to cover contract collateral and price"))?;
                    ch.balance = new_bal;
                    if c.contract_type == "ItemExchange" && c.reward.0 > 0 {
                        ch.balance = ch.balance.checked_add(c.reward).unwrap_or(ch.balance);
                    }
                }

                snap.credit_contract_issuer(&c, c.price);

                if c.contract_type == "ItemExchange" {
                    if let Some(dep_raw) = snap
                        .fleet_and_social_ops
                        .contract_deposits
                        .remove(&cid)
                        .filter(|&d| d > 0)
                    {
                        snap.credit_contract_issuer(&c, IskCents(dep_raw));
                    }
                    let issuer_entity_id = if c.for_corp && c.issuer_corp_id != 0 { c.issuer_corp_id } else { c.issuer_id };
                    let req_dest_flag = if c.for_corp && c.issuer_corp_id != 0 {
                        InventoryFlag::CorpDeliveries
                    } else {
                        InventoryFlag::Hangar
                    };
                    let (acceptor_entity_id, acceptor_flag) = if is_corp_accept {
                        (effective_caller_corp, InventoryFlag::CorpDeliveries)
                    } else {
                        (char_id, InventoryFlag::Hangar)
                    };
                    let acceptor_active_ship = snap.characters.get(&char_id).map(|ch| ch.active_ship_id);
                    for (cand_id, take_qty) in requested_transfers {
                        let (cur_owner, cur_qty) = snap
                            .items
                            .get(&cand_id)
                            .map(|i| (i.owner_id, i.quantity))
                            .unwrap_or((0, 0));
                        let actor = if cur_owner == char_id { Some(char_id) } else { None };
                        let mut xfer = eve_db::TransferItemSpec::move_whole(
                            actor,
                            None,
                            Some(issuer_entity_id),
                            c.start_station_id,
                            req_dest_flag,
                        );
                        xfer.quantity = if take_qty >= cur_qty { None } else { Some(take_qty) };
                        xfer.reject_non_empty_container = true;
                        xfer.active_ship_id = acceptor_active_ship;
                        snap.transfer_inventory_item(cand_id, xfer)
                            .map_err(|e| rpc_err("ContractTransferFailed", e.to_string()))?;
                    }

                    let mut transferred_ids = Vec::new();
                    for iid in &c.item_ids {
                        let _ = pop_escrowed_item_orig_flag(snap, cid, *iid);
                        if let Some(item) = snap.items.get_mut(iid) {
                            if item.owner_id != acceptor_entity_id {
                                transferred_ids.push(*iid);
                            }
                            item.owner_id = acceptor_entity_id;
                            item.location_id = c.start_station_id;
                            item.flag = acceptor_flag;
                            item.version += 1;
                        }
                        snap.sp6_lifecycle.self_destructs.remove(iid);
                    }
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                        c_mut.acceptor_id = Some(acceptor_entity_id);
                        c_mut.status = "Completed".into();
                    }
                    let notify_delta = accept_delta.finish(snap);
                    return Ok(("Completed", c.expiration_unix, None, c.issuer_id, transferred_ids, notify_delta));
                }

                if let Some(dep_raw) = snap.fleet_and_social_ops.contract_deposits.remove(&cid) {
                    snap.credit_contract_issuer(&c, IskCents(dep_raw));
                }
                let courier_owner_id = if is_corp_accept { effective_caller_corp } else { char_id };
                snap.next_entity_id += 1;
                let wrap_id = snap.next_entity_id;
                snap.items.insert(wrap_id, InventoryItemRecord {
                    item_id: wrap_id, type_id: 3468, owner_id: courier_owner_id, location_id: c.start_station_id,
                    flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default()
                });
                for iid in &c.item_ids {
                    let _ = pop_escrowed_item_orig_flag(snap, cid, *iid);
                    if let Some(item) = snap.items.get_mut(iid) {
                        item.location_id = wrap_id;
                        item.flag = InventoryFlag::Locked;
                        item.version += 1;
                    }
                }
                let courier_days = (c.num_days.max(1) as u64).min(28);
                let delivery_deadline = now_unix.saturating_add(courier_days.saturating_mul(86_400));
                if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                    c_mut.acceptor_id = Some(if is_corp_accept { effective_caller_corp } else { char_id });
                    c_mut.status = "InProgress".into();
                    c_mut.expiration_unix = delivery_deadline;
                }
                let notify_delta = accept_delta.finish(snap);
                Ok(("InProgress", delivery_deadline, Some(wrap_id), c.issuer_id, Vec::new(), notify_delta))
            });
            match res {
                Ok((new_status, exp_unix, wrap_id_opt, issuer_id, transferred_ids, notify_delta)) => {
                    notify_delta.emit(ctx);
                    if !transferred_ids.is_empty() {
                        crate::services::quasar_cosmetics_live::strip_ship_cosmetics_for_ships(&ctx.db, &transferred_ids);
                    }
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
            let rpc = RpcArgs::new(args, kwargs);
            let cid = match rpc.req_positive_i64(0, "contractID") { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let bid_isk = match rpc.req_finite_f64(1, "bidAmount") { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let bid_for_corp = match rpc.opt_bool(2, "forCorp") { Ok(v) => v.unwrap_or(false), Err(e) => return Some(Err(e)) };
            if bid_isk <= 0.0 {
                return Some(Err(rpc_err("InvalidBidAmount", "Auction bid must be a positive finite ISK amount",)));
            }
            let caller_corp = session.corporation_id.unwrap_or(0);
            let caller_alliance = session.alliance_id;
            let now_unix = ctx.now_unix();
            let acct_div_idx = active_corp_acct_div(session);
            if bid_for_corp {
                if let Some(corp_id) = ctx
                    .db
                    .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id))
                    .or(session.corporation_id)
                    .filter(|&id| id > 0)
                {
                    crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
                }
            }

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
                if c.status == "Expired" {
                    return Err(rpc_err("ContractExpired", format!("Auction contract {cid} has expired")));
                }
                if c.status != "Outstanding" {
                    return Err(rpc_err("InvalidContractState", format!("Auction {cid} is no longer Outstanding")));
                }
                let effective_caller_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|ch| ch.corporation_id)
                    .filter(|&id| id != 0)
                    .unwrap_or(caller_corp);
                if c.issuer_id == char_id || (bid_for_corp && c.for_corp && c.issuer_corp_id == effective_caller_corp) {
                    return Err(rpc_err("CannotBidOnOwnContract", "Issuer cannot bid on their own auction"));
                }
                if !check_contract_visibility(&c, char_id, effective_caller_corp, caller_alliance) {
                    return Err(rpc_err(
                        "ContractAccessDenied",
                        format!("Character {char_id} cannot bid on private auction {cid}"),
                    ));
                }
                if bid_for_corp {
                    if effective_caller_corp == 0 || !snap.corporations.contains_key(&effective_caller_corp) {
                        return Err(rpc_err("CrpAccessDenied", "Corporate auction bids require a player corporation"));
                    }
                    if !has_corp_contract_role(snap, char_id, effective_caller_corp, now_unix) {
                        return Err(rpc_err("CrpAccessDenied", "Bidding for a corporation requires ROLE_CONTRACT_MANAGER or ROLE_DIRECTOR"));
                    }
                    if !has_corp_wallet_take_role(snap, char_id, effective_caller_corp, acct_div_idx, now_unix) {
                        return Err(rpc_err(
                            "CrpAccessDenied",
                            format!(
                                "Corporate auction bids require ROLE_DIRECTOR or ROLE_ACCOUNT_CAN_TAKE for division {}",
                                acct_div_idx + 1
                            ),
                        ));
                    }
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

                let bidder_entity_id = if bid_for_corp { effective_caller_corp } else { char_id };
                let bidder_div_num = (acct_div_idx + 1) as u8;
                let prev_self_refund = if c.current_bidder_id == Some(bidder_entity_id) { c.current_bid } else { IskCents(0) };
                if bid_for_corp {
                    let corp = snap
                        .corporations
                        .get(&effective_caller_corp)
                        .ok_or_else(|| rpc_err("CorpNotFound", format!("Corporation {effective_caller_corp} not found")))?;
                    let bidder_avail = corp
                        .division_balance(bidder_div_num)
                        .checked_add(prev_self_refund)
                        .unwrap_or_else(|| corp.division_balance(bidder_div_num));
                    if bidder_avail.0 < effective_bid.0 {
                        return Err(rpc_err("InsufficientFunds", "Insufficient corporation ISK to place auction bid"));
                    }
                } else {
                    let bidder_avail = snap.characters.get(&char_id)
                        .map(|ch| ch.balance.checked_add(prev_self_refund).unwrap_or(ch.balance))
                        .ok_or_else(|| rpc_err("CharacterNotFound", format!("Character {char_id} not found")))?;
                    if bidder_avail.0 < effective_bid.0 {
                        return Err(rpc_err("InsufficientFunds", "Insufficient ISK to place auction bid"));
                    }
                }

                let bid_delta = ContractNotifyDelta::capture(
                    snap,
                    char_id,
                    cid,
                    &[char_id, effective_caller_corp, c.issuer_id, c.issuer_corp_id, c.current_bidder_id.unwrap_or(0)],
                    &[c.start_station_id],
                    &c.item_ids,
                );
                let prev_bidder_opt = c.current_bidder_id;
                if let Some(prev_bidder) = prev_bidder_opt {
                    if c.current_bid.0 > 0 {
                        let prev_sub = prev_bidder.to_string();
                        let kv_div = snap
                            .kv(eve_db::Ns::ContractBidDiv)
                            .get::<u8>(cid, &prev_sub);
                        snap.kv_mut(eve_db::Ns::ContractBidDiv)
                            .remove(cid, &prev_sub);
                        let legacy_div = snap
                            .service_state_blobs
                            .remove(&format!("contract_bid_div:{cid}:{prev_bidder}"))
                            .and_then(|s| s.parse::<u8>().ok());
                        if let Some(prev_corp) = snap.corporations.get_mut(&prev_bidder) {
                            let prev_div = kv_div
                                .or(legacy_div)
                                .filter(|&d| (1..=7).contains(&d))
                                .unwrap_or(1);
                            let refunded = prev_corp.division_balance(prev_div).checked_add(c.current_bid).unwrap_or(prev_corp.division_balance(prev_div));
                            prev_corp.set_division_balance(prev_div, refunded);
                            snap.append_journal(prev_bidder, "ContractAuctionBidRefund", c.current_bid, refunded, format!("Outbid refund on auction contract {cid}"));
                        } else if let Some(prev_ch) = snap.characters.get_mut(&prev_bidder) {
                            prev_ch.balance = prev_ch.balance.checked_add(c.current_bid).unwrap_or(prev_ch.balance);
                            let bal_after = prev_ch.balance;
                            snap.append_journal(prev_bidder, "ContractAuctionBidRefund", c.current_bid, bal_after, format!("Outbid refund on auction contract {cid}"));
                        }
                    }
                }

                if bid_for_corp {
                    if snap.corporations.contains_key(&effective_caller_corp) {
                        snap.kv_mut(eve_db::Ns::ContractBidDiv)
                            .set_with_unix(
                                cid,
                                effective_caller_corp.to_string(),
                                &bidder_div_num,
                                now_unix,
                            )
                            .map_err(|e| rpc_err("ServiceStateLimitExceeded", e))?;
                        let corp = snap.corporations.get_mut(&effective_caller_corp).unwrap();
                        let new_bal = corp.division_balance(bidder_div_num).checked_sub(effective_bid).unwrap_or(IskCents(0));
                        corp.set_division_balance(bidder_div_num, new_bal);
                        snap.append_journal(effective_caller_corp, "ContractAuctionBid", effective_bid, new_bal, format!("Corporate bid placed on auction contract {cid}"));
                    }
                } else if let Some(bidder_ch) = snap.characters.get_mut(&char_id) {
                    bidder_ch.balance = bidder_ch.balance.checked_sub(effective_bid).unwrap_or(IskCents(0));
                    let bal_after = bidder_ch.balance;
                    snap.append_journal(char_id, "ContractAuctionBid", effective_bid, bal_after, format!("Bid placed on auction contract {cid}"));
                }

                let mut new_status = "Outstanding";
                if is_buyout {
                    snap.credit_contract_issuer(&c, effective_bid);
                    if let Some(dep_raw) = snap.fleet_and_social_ops.contract_deposits.remove(&cid) {
                        snap.credit_contract_issuer(&c, IskCents(dep_raw));
                    }
                    if c.for_corp {
                        let bal_after = snap.corporations.get(&c.issuer_corp_id).map(|corp| corp.division_balance(1)).unwrap_or(IskCents(0));
                        snap.append_journal(c.issuer_corp_id, "ContractAuctionSold", effective_bid, bal_after, format!("Auction contract {cid} bought out by {bidder_entity_id}"));
                    } else if let Some(issuer_ch) = snap.characters.get(&c.issuer_id) {
                        let bal_after = issuer_ch.balance;
                        snap.append_journal(c.issuer_id, "ContractAuctionSold", effective_bid, bal_after, format!("Auction contract {cid} bought out by {bidder_entity_id}"));
                    }
                    let win_flag = if bid_for_corp { InventoryFlag::CorpDeliveries } else { InventoryFlag::Hangar };
                    let mut transferred_ids = Vec::new();
                    for iid in &c.item_ids {
                        let _ = pop_escrowed_item_orig_flag(snap, cid, *iid);
                        if let Some(item) = snap.items.get_mut(iid) {
                            if item.owner_id != bidder_entity_id {
                                transferred_ids.push(*iid);
                            }
                            item.owner_id = bidder_entity_id;
                            item.location_id = c.start_station_id;
                            item.flag = win_flag;
                            item.version += 1;
                        }
                        snap.sp6_lifecycle.self_destructs.remove(iid);
                    }
                    new_status = "Completed";
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                        c_mut.current_bid = effective_bid;
                        c_mut.current_bidder_id = Some(bidder_entity_id);
                        c_mut.bid_history.push((bidder_entity_id, effective_bid, now_unix));
                        c_mut.acceptor_id = Some(bidder_entity_id);
                        c_mut.status = "Completed".into();
                    }
                    let notify_delta = bid_delta.finish(snap);
                    return Ok((new_status, effective_bid, is_buyout, prev_bidder_opt, c.issuer_id, transferred_ids, notify_delta));
                }

                if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                    c_mut.current_bid = effective_bid;
                    c_mut.current_bidder_id = Some(bidder_entity_id);
                    c_mut.bid_history.push((bidder_entity_id, effective_bid, now_unix));
                    if is_buyout { c_mut.acceptor_id = Some(bidder_entity_id); c_mut.status = "Completed".into(); }
                }
                let notify_delta = bid_delta.finish(snap);
                Ok((new_status, effective_bid, is_buyout, prev_bidder_opt, c.issuer_id, Vec::new(), notify_delta))
            });

            match res {
                Ok((new_status, effective_bid, is_buyout, prev_bidder_opt, issuer_id, transferred_ids, notify_delta)) => {
                    notify_delta.emit(ctx);
                    if !transferred_ids.is_empty() {
                        crate::services::quasar_cosmetics_live::strip_ship_cosmetics_for_ships(&ctx.db, &transferred_ids);
                    }
                    if let Some(prev_bidder) = prev_bidder_opt {
                        if prev_bidder != char_id { notify_contract(ctx, prev_bidder, "OnContractOutbid", cid); }
                    }
                    if is_buyout {
                        notify_contract(ctx, issuer_id, "OnContractBuyout", cid);
                        notify_contract(ctx, issuer_id, "OnContractCompleted", cid);
                    }
                    Some(Ok(PyRep::keyval([
                        ("contractID", PyRep::Integer(cid)), ("status", PyRep::String(new_status.into())),
                        ("currentBidISK", PyRep::Real(effective_bid.to_wire_f64())),
                        ("highestBidderID", PyRep::Integer(char_id)), ("boughtOut", PyRep::Bool(is_buyout)),
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
            let rpc = RpcArgs::new(args, kwargs);
            let cid = match rpc.req_positive_i64(0, "contractID") { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let caller_station = session.station_id.or(session.structure_id);
            let caller_ship = session.ship_id;
            let caller_corp = session.corporation_id.unwrap_or(0);
            let now_unix = ctx.now_unix();
            if let Some(corp_id) = ctx
                .db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id))
                .or(session.corporation_id)
                .filter(|&id| id > 0)
            {
                crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
            }
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
                let effective_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|ch| ch.corporation_id)
                    .filter(|&id| id != 0)
                    .unwrap_or(caller_corp);
                let is_corp_acceptor = effective_corp != 0 && c.acceptor_id == Some(effective_corp) && has_corp_contract_role(snap, char_id, effective_corp, now_unix);
                if (c.acceptor_id == Some(char_id) || is_corp_acceptor)
                    && (c.status == "Expired"
                        || (c.status == "Failed"
                            && c.contract_type == "Courier"
                            && c.expiration_unix > 0
                            && c.expiration_unix <= now_unix))
                {
                    return Err(rpc_err(
                        "ContractExpired",
                        format!("Courier contract {cid} exceeded its delivery deadline; collateral and reward forfeited"),
                    ));
                }
                if c.status != "InProgress" || (c.acceptor_id != Some(char_id) && !is_corp_acceptor) {
                    return Err(rpc_err("InvalidContractState", format!("Contract {cid} is not InProgress for character {char_id}")));
                }
                if c.contract_type != "Courier" {
                    return Err(rpc_err("InvalidContractType", format!("Contract {cid} is not a Courier contract")));
                }
                if caller_station != Some(c.end_station_id) {
                    return Err(rpc_err("CourierNotAtDestinationStation", format!("Completing Courier contract {cid} requires being docked at {}", c.end_station_id)));
                }
                let Some(ship_id) = caller_ship else {
                    return Err(rpc_err("NoActiveShip", "Completing a Courier contract requires the active delivery ship"));
                };
                let ship_at_dest = snap.items.get(&ship_id).is_some_and(|sh| {
                    (sh.owner_id == char_id || (is_corp_acceptor && sh.owner_id == effective_corp)) && sh.location_id == c.end_station_id
                });
                if !ship_at_dest {
                    return Err(rpc_err("DeliveryShipNotAtDestination", format!("Active ship {ship_id} is not docked at destination station {}", c.end_station_id)));
                }
                let mut wraps_to_remove = Vec::new();
                for iid in &c.item_ids {
                    let present = snap.items.get(iid).is_some_and(|item| {
                        if let Some(wrap_item) = snap.items.get(&item.location_id).filter(|w| {
                            w.type_id == 3468
                                && (w.owner_id == char_id || (is_corp_acceptor && w.owner_id == effective_corp))
                                && (w.location_id == ship_id || w.location_id == c.end_station_id)
                        }) {
                            wraps_to_remove.push(wrap_item.item_id);
                            return true;
                        }
                        false
                    });
                    if !present {
                        return Err(rpc_err("CourierPackageMissing", format!("Courier Plastic Wrap package for item {iid} is not present at destination")));
                    }
                }
                let mut complete_extra_ids = c.item_ids.clone();
                complete_extra_ids.extend_from_slice(&wraps_to_remove);
                let complete_delta = ContractNotifyDelta::capture(
                    snap,
                    char_id,
                    cid,
                    &[char_id, effective_corp, c.issuer_id, c.issuer_corp_id, c.acceptor_id.unwrap_or(0)],
                    &[c.start_station_id, c.end_station_id],
                    &complete_extra_ids,
                );
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
                for wid in wraps_to_remove { snap.remove_system_cleanup_item(wid); }
                if let Some(c_mut) = snap.contracts.get_mut(&cid) { c_mut.status = "Completed".into(); }
                if let Some(dep_raw) = snap.fleet_and_social_ops.contract_deposits.remove(&cid) {
                    snap.credit_contract_issuer(&c, IskCents(dep_raw));
                }
                let total_return = c.collateral.checked_add(c.reward).unwrap_or(c.collateral);
                if is_corp_acceptor {
                    if let Some(corp) = snap.corporations.get_mut(&effective_corp) {
                        corp.credit_division(1, total_return);
                    }
                } else if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = ch.balance.checked_add(total_return).unwrap_or(ch.balance);
                }
                let notify_delta = complete_delta.finish(snap);
                Ok((total_return.to_wire_f64(), c.issuer_id, notify_delta))
            });
            match res {
                Ok((payout, issuer_id, notify_delta)) => {
                    notify_delta.emit(ctx);
                    notify_contract(ctx, issuer_id, "OnContractCompleted", cid);
                    Some(Ok(PyRep::keyval([("contractID", PyRep::Integer(cid)), ("status", PyRep::String("Completed".into())), ("totalPayoutISK", PyRep::Real(payout))])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "GM_ExpireContract" => {
            let (user_id, _char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rpc = RpcArgs::new(args, kwargs);
            let cid = match rpc.req_positive_i64(0, "contractID") { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let has_gm_role = snap.accounts.values().any(|a| a.user_id == user_id && a.role > 0);
                if !has_gm_role { return Err(rpc_err("GMRoleRequired", "GM_ExpireContract requires GM role")); }
                let c_snap = snap
                    .contracts
                    .get(&cid)
                    .cloned()
                    .ok_or_else(|| rpc_err("ContractNotFound", format!("Contract {cid} does not exist")))?;
                let mut expire_extra = c_snap.item_ids.clone();
                let mut wrap_ids: Vec<i64> = snap
                    .items
                    .iter()
                    .filter(|(_, it)| c_snap.item_ids.iter().any(|iid| snap.items.get(iid).is_some_and(|ci| ci.location_id == it.item_id)))
                    .map(|(id, _)| *id)
                    .collect();
                expire_extra.append(&mut wrap_ids);
                let expire_delta = ContractNotifyDelta::capture(
                    snap,
                    c_snap.issuer_id,
                    cid,
                    &[c_snap.issuer_id, c_snap.issuer_corp_id, c_snap.acceptor_id.unwrap_or(0), c_snap.current_bidder_id.unwrap_or(0)],
                    &[c_snap.start_station_id, c_snap.end_station_id],
                    &expire_extra,
                );
                let Some(c_mut) = snap.contracts.get_mut(&cid) else {
                    return Err(rpc_err("ContractNotFound", format!("Contract {cid} does not exist")));
                };
                c_mut.expiration_unix = 1;
                let resolved = snap.resolve_expired_contract(cid, now_unix).unwrap_or("Expired");
                let notify_delta = expire_delta.finish(snap);
                Ok((resolved, notify_delta))
            });
            match res {
                Ok((new_status, notify_delta)) => {
                    notify_delta.emit(ctx);
                    Some(Ok(PyRep::keyval([
                        ("contractID", PyRep::Integer(cid)),
                        ("status", PyRep::String(new_status.into())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "BreakPlasticWrap" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rpc = RpcArgs::new(args, kwargs);
            let wrap_id = match rpc.req_i64_any(0, &["itemID", "plasticWrapID"]) {
                Ok(v) if v > 0 => v,
                Ok(_) => return Some(Err(rpc_err("InvalidArgument", "BreakPlasticWrap requires positive wrap itemID"))),
                Err(e) => return Some(Err(e)),
            };
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let wrap_item_ids: Vec<(i64, i64)> = snap
                    .items
                    .values()
                    .filter(|it| it.location_id == wrap_id)
                    .map(|it| (it.item_id, it.owner_id))
                    .collect();
                let mut break_extra = vec![wrap_id];
                break_extra.extend(wrap_item_ids.iter().map(|(iid, _)| *iid));
                let break_owners: Vec<i64> = snap
                    .items
                    .get(&wrap_id)
                    .map(|w| vec![char_id, w.owner_id])
                    .unwrap_or_else(|| vec![char_id]);
                let break_delta = ContractNotifyDelta::capture(snap, char_id, 0, &break_owners, &[], &break_extra);
                let (cid, issuer_id, extracted) = break_courier_plastic_wrap(snap, char_id, wrap_id, now_unix)?;
                let transferred_ids: Vec<i64> = wrap_item_ids
                    .into_iter()
                    .filter_map(|(iid, old_owner)| {
                        snap.items
                            .get(&iid)
                            .is_some_and(|it| it.owner_id != old_owner)
                            .then_some(iid)
                    })
                    .collect();
                let notify_delta = break_delta.finish(snap);
                Ok((cid, issuer_id, extracted, transferred_ids, notify_delta))
            });
            match res {
                Ok((cid, issuer_id, extracted, transferred_ids, notify_delta)) => {
                    notify_delta.emit(ctx);
                    if !transferred_ids.is_empty() {
                        crate::services::quasar_cosmetics_live::strip_ship_cosmetics_for_ships(&ctx.db, &transferred_ids);
                    }
                    notify_contract(ctx, issuer_id, "OnContractFailed", cid);
                    Some(Ok(PyRep::keyval([
                        ("contractID", PyRep::Integer(cid)),
                        ("status", PyRep::String("Failed".into())),
                        ("extractedItemIDs", PyRep::List(extracted.into_iter().map(PyRep::Integer).collect())),
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
            let rpc = RpcArgs::new(args, kwargs);
            let cid = match rpc.req_positive_i64(0, "contractID") { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let caller_corp = session.corporation_id.unwrap_or(0);
            let now_unix = ctx.now_unix();
            if let Some(corp_id) = ctx
                .db
                .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.corporation_id))
                .or(session.corporation_id)
                .filter(|&id| id > 0)
            {
                crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(ctx, corp_id);
            }
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid).cloned() else {
                    return Err(rpc_err("ContractNotFound", format!("Contract {cid} does not exist")));
                };
                let issuer_entity_id = if c.for_corp && c.issuer_corp_id != 0 {
                    c.issuer_corp_id
                } else {
                    c.issuer_id
                };
                let effective_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|ch| ch.corporation_id)
                    .filter(|&id| id != 0)
                    .unwrap_or(caller_corp);
                let mut resolve_extra = c.item_ids.clone();
                let mut wrap_ids: Vec<i64> = snap
                    .items
                    .iter()
                    .filter(|(_, it)| c.item_ids.iter().any(|iid| snap.items.get(iid).is_some_and(|ci| ci.location_id == it.item_id)))
                    .map(|(id, _)| *id)
                    .collect();
                resolve_extra.append(&mut wrap_ids);
                let resolve_delta = ContractNotifyDelta::capture(
                    snap,
                    char_id,
                    cid,
                    &[char_id, effective_corp, c.issuer_id, c.issuer_corp_id, c.acceptor_id.unwrap_or(0), c.current_bidder_id.unwrap_or(0)],
                    &[c.start_station_id, c.end_station_id],
                    &resolve_extra,
                );
                let has_caller_corp_contract_role =
                    has_corp_contract_role(snap, char_id, effective_corp, now_unix);
                if method == "RejectContract" {
                    let is_assignee = c.assignee_id != 0
                        && (c.assignee_id == char_id || c.assignee_id == effective_corp);
                    if !is_assignee || c.status != "Outstanding" {
                        return Err(rpc_err("ContractOwnershipViolation", format!("Character {char_id} cannot reject contract {cid} in state '{}'", c.status)));
                    }
                    if c.assignee_id == effective_corp && c.assignee_id != char_id && !has_caller_corp_contract_role {
                        return Err(rpc_err("CrpAccessDenied", "Corporate Contract Manager or Director role required to reject a corporate contract"));
                    }
                    snap.credit_contract_issuer(&c, c.reward);
                    if let Some(dep_raw) = snap.fleet_and_social_ops.contract_deposits.remove(&cid) {
                        snap.credit_contract_issuer(&c, IskCents(dep_raw));
                    }
                    for iid in &c.item_ids {
                        let orig_flag = pop_escrowed_item_orig_flag(snap, cid, *iid);
                        if let Some(item) = snap.items.get_mut(iid) {
                            item.owner_id = issuer_entity_id;
                            item.location_id = c.start_station_id;
                            item.flag = orig_flag;
                            item.version += 1;
                        }
                    }
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) { c_mut.status = "Rejected".into(); }
                    return Ok(("Rejected", c.issuer_id, resolve_delta.finish(snap)));
                }

                let can_manage_as_issuer = c.issuer_id == char_id
                    || (c.for_corp && c.issuer_corp_id != 0 && c.issuer_corp_id == effective_corp && has_caller_corp_contract_role);
                let is_corp_acceptor = effective_corp != 0 && c.acceptor_id == Some(effective_corp) && has_caller_corp_contract_role;
                if !can_manage_as_issuer && c.acceptor_id != Some(char_id) && !is_corp_acceptor {
                    return Err(rpc_err("ContractOwnershipViolation", format!("Character {char_id} is neither issuer nor acceptor of contract {cid}")));
                }
                if let Some(new_state) = snap.resolve_expired_contract(cid, now_unix) {
                    return Ok((new_state, c.issuer_id, resolve_delta.finish(snap)));
                }
                if method == "DeleteContract" && (c.status == "Outstanding" || c.status == "Expired") && can_manage_as_issuer {
                    if c.status == "Outstanding" && c.contract_type == "Auction" && c.current_bidder_id.is_some() {
                        return Err(rpc_err("CannotDeleteAuctionWithBids", format!("Auction contract {cid} already has active bids and cannot be deleted")));
                    }
                    if c.status == "Outstanding" {
                        snap.credit_contract_issuer(&c, c.reward);
                        if let Some(dep_raw) = snap.fleet_and_social_ops.contract_deposits.remove(&cid) {
                            snap.credit_contract_issuer(&c, IskCents(dep_raw));
                        }
                        for iid in &c.item_ids {
                            let orig_flag = pop_escrowed_item_orig_flag(snap, cid, *iid);
                            if let Some(item) = snap.items.get_mut(iid) {
                                item.owner_id = issuer_entity_id;
                                item.location_id = c.start_station_id;
                                item.flag = orig_flag;
                                item.version += 1;
                            }
                        }
                    }
                    let extra_ids: Vec<i64> = snap
                        .items
                        .values()
                        .filter(|item| item.location_id == cid)
                        .map(|item| item.item_id)
                        .collect();
                    for iid in extra_ids {
                        let orig_flag = pop_escrowed_item_orig_flag(snap, cid, iid);
                        if let Some(item) = snap.items.get_mut(&iid) {
                            item.owner_id = issuer_entity_id;
                            item.location_id = c.start_station_id;
                            item.flag = orig_flag;
                            item.version += 1;
                        }
                    }
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) { c_mut.status = "Deleted".into(); }
                    return Ok(("Deleted", c.issuer_id, resolve_delta.finish(snap)));
                }
                if method == "FailContract" && c.status == "InProgress" && c.contract_type == "Courier" && (c.acceptor_id == Some(char_id) || is_corp_acceptor) {
                    snap.fail_courier_contract(cid, now_unix);
                    return Ok(("Failed", c.issuer_id, resolve_delta.finish(snap)));
                }
                if method == "FailContract"
                    && c.status == "Failed"
                    && c.contract_type == "Courier"
                    && c.expiration_unix > 0
                    && c.expiration_unix <= now_unix
                {
                    return Ok(("Failed", c.issuer_id, resolve_delta.finish(snap)));
                }
                Err(rpc_err("InvalidContractState", format!("Contract {cid} in state '{}' cannot be resolved via {method}", c.status)))
            });
            match res {
                Ok((new_status, issuer_id, notify_delta)) => {
                    notify_delta.emit(ctx);
                    match new_status {
                        "Rejected" => notify_contract(ctx, issuer_id, "OnContractRejected", cid),
                        "Deleted" => {}
                        _ => notify_contract(ctx, issuer_id, "OnContractFailed", cid),
                    }
                    Some(Ok(PyRep::keyval([("contractID", PyRep::Integer(cid)), ("status", PyRep::String(new_status.into()))])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "GetContractInfo" | "GetContract" => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let rpc = RpcArgs::new(args, kwargs);
            let cid = match rpc.req_positive_i64(0, "contractID") { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let caller_corp = session.corporation_id.unwrap_or(0);
            let caller_alliance = session.alliance_id;
            let now_unix = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let _ = snap.resolve_expired_contract(cid, now_unix);
                let Some(c) = snap.contracts.get(&cid) else {
                    return Err(rpc_err("ContractNotFound", format!("Contract {cid} does not exist")));
                };
                if !check_contract_visibility(c, char_id, caller_corp, caller_alliance) && c.acceptor_id != Some(char_id) {
                    return Err(rpc_err("ContractAccessDenied", format!("Character {char_id} cannot view private contract {cid}")));
                }
                Ok(contract_to_pyrep(c, snap, &ctx.sde))
            });
            if res.is_ok() && method == "GetContractInfo" { notify_contract(ctx, char_id, "OnShowContract", cid); }
            Some(res)
        }

        "SearchContracts" | "GetPublicContractList" => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let filter_type = args.first().and_then(|a| a.as_str()).or_else(|| kwargs.get("contractType").and_then(|v| v.as_str()));
            let filter_station = kwargs.get("stationID").and_then(|v| v.as_i64());
            let filter_region = kwargs.get("regionID").and_then(|v| v.as_i64()).map(|r| r as u32);
            let filter_item_type = kwargs.get("itemTypeID").or_else(|| kwargs.get("typeID")).and_then(|v| v.as_i64()).map(|t| t as TypeId);
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
                                let loc_known = ctx.sde.stations.contains_key(&c.start_station_id)
                                    || snap.structures.contains_key(&c.start_station_id);
                                if loc_known {
                                    crate::domain::market_acl::contract_in_region(
                                        &ctx.sde,
                                        snap,
                                        c.start_station_id,
                                        reg,
                                    )
                                } else {
                                    true
                                }
                            }).unwrap_or(true);
                            let item_type_ok = filter_item_type.map(|tid| {
                                    c.item_ids.iter().any(|iid| snap.items.get(iid).map(|it| it.type_id == tid).unwrap_or(false))
                                        || c.requested_items.iter().any(|&(rtid, _)| rtid == tid)
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
                    Ok::<_, std::convert::Infallible>(list.iter().map(|c| contract_to_pyrep(c, snap, &ctx.sde)).collect::<Vec<_>>())
                }).unwrap();
            Some(Ok(PyRep::List(rows)))
        }

        "GetMyCurrentContractList" | "GetContractList" | "GetContractListForOwner" => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let caller_corp = session.corporation_id.unwrap_or(0);
            let is_corp_query = (method == "GetMyCurrentContractList")
                .then(|| args.get(1).and_then(|a| match a { PyRep::Bool(b) => Some(*b), _ => None }))
                .flatten()
                .or_else(|| args.first().and_then(|a| match a { PyRep::Bool(b) => Some(*b), _ => None }))
                .or_else(|| kwargs.get("forCorp").and_then(|v| match v { PyRep::Bool(b) => Some(*b), _ => None }))
                .unwrap_or(false);
            let owner_id = if is_corp_query && caller_corp != 0 {
                caller_corp
            } else {
                args.first().and_then(|a| a.as_i64()).filter(|&v| v > 1).unwrap_or(char_id)
            };
            if owner_id != char_id && owner_id != caller_corp {
                return Some(Err(rpc_err("ContractAccessDenied", format!("Cannot query contract list for foreign owner {owner_id}"))));
            }
            // Fix #969: respect V24 GetContractListForOwner(ownerID, filtStatus, contractType, issuedBy) filter args.
            let status_filter = (method == "GetContractListForOwner")
                .then(|| args.get(1).and_then(|v| v.as_i64()))
                .flatten();
            let type_filter = (method == "GetContractListForOwner")
                .then(|| args.get(2).and_then(|v| match v { PyRep::Integer(t) => Some(*t), _ => None }))
                .flatten();
            let issued_to_filter = (method == "GetContractListForOwner")
                .then(|| args.get(3).and_then(|v| match v {
                    PyRep::Bool(b) => Some(*b),
                    PyRep::Integer(i) => Some(*i != 0),
                    _ => None,
                }))
                .flatten();
            let now_unix = ctx.now_unix();
            let rows = ctx.db.try_with_mut_snapshot(|snap| {
                let cids: Vec<i64> = snap.contracts.keys().copied().collect();
                for cid in cids { let _ = snap.resolve_expired_contract(cid, now_unix); }
                let owner_is_corp = (owner_id == caller_corp && caller_corp != 0) || snap.corporations.contains_key(&owner_id);
                let mut list: Vec<ContractRecord> = snap.contracts.values().filter(|c| {
                    let status_ok = match status_filter {
                        Some(0) => c.status == "Outstanding",
                        Some(1) => c.status == "InProgress",
                        Some(2) => c.status == "FinishedIssuer",
                        Some(3) => c.status == "FinishedContractor",
                        Some(4) => matches!(
                            c.status.as_str(),
                            "Completed"
                                | "Finished"
                                | "FinishedIssuer"
                                | "FinishedContractor"
                                | "Cancelled"
                                | "Rejected"
                                | "Failed"
                                | "Expired"
                                | "Reversed"
                        ),
                        Some(5) => c.status == "Cancelled",
                        Some(6) => c.status == "Rejected",
                        Some(7) => c.status == "Failed",
                        _ => matches!(c.status.as_str(), "Outstanding" | "InProgress"),
                    };
                    let type_ok = match type_filter {
                        Some(1) => c.contract_type == "ItemExchange",
                        Some(2) => c.contract_type == "Auction",
                        Some(3) => c.contract_type == "Courier",
                        Some(4) => c.contract_type == "Loan",
                        _ => true,
                    };
                    let issued_by_owner = if owner_is_corp {
                        c.for_corp && (c.issuer_corp_id == owner_id || c.issuer_id == owner_id)
                    } else {
                        !c.for_corp && c.issuer_id == owner_id
                    };
                    let issued_to_owner = c.acceptor_id == Some(owner_id) || c.assignee_id == owner_id;
                    let owner_ok = match issued_to_filter {
                        Some(false) => issued_by_owner,
                        Some(true) => issued_to_owner,
                        None => issued_by_owner || issued_to_owner || c.current_bidder_id == Some(owner_id),
                    };
                    status_ok && type_ok && owner_ok
                }).cloned().collect();
                list.sort_by_key(|c| c.contract_id);
                Ok::<_, std::convert::Infallible>(list.iter().map(|c| contract_to_pyrep(c, snap, &ctx.sde)).collect::<Vec<_>>())
            }).unwrap();
            Some(Ok(PyRep::List(rows)))
        }

        "GetMyExpiredContractList" => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let caller_corp = session.corporation_id.unwrap_or(0);
            let for_corp = args.first().and_then(|a| match a { PyRep::Bool(b) => Some(*b), _ => None })
                .or_else(|| kwargs.get("forCorp").and_then(|v| match v { PyRep::Bool(b) => Some(*b), _ => None }))
                .unwrap_or(false);
            let now_unix = ctx.now_unix();
            let rows = ctx.db.try_with_mut_snapshot(|snap| {
                let cids: Vec<i64> = snap.contracts.keys().copied().collect();
                for cid in cids { let _ = snap.resolve_expired_contract(cid, now_unix); }
                let mut list: Vec<ContractRecord> = snap.contracts.values().filter(|c| {
                    !matches!(c.status.as_str(), "Outstanding" | "InProgress")
                        && if for_corp {
                            caller_corp != 0
                                && ((c.for_corp && (c.issuer_corp_id == caller_corp || c.issuer_id == caller_corp))
                                    || c.acceptor_id == Some(caller_corp)
                                    || c.assignee_id == caller_corp
                                    || c.current_bidder_id == Some(caller_corp))
                        } else {
                            c.issuer_id == char_id || c.acceptor_id == Some(char_id) || c.assignee_id == char_id || c.current_bidder_id == Some(char_id)
                        }
                }).cloned().collect();
                list.sort_by_key(|c| c.contract_id);
                Ok::<_, std::convert::Infallible>(list.iter().map(|c| contract_to_pyrep(c, snap, &ctx.sde)).collect::<Vec<_>>())
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
                    let allowed = owner_id == caller_corp
                        && has_corp_contract_role(snap, char_id, caller_corp, now_unix);
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
                Ok(list.iter().map(|c| contract_to_pyrep(c, snap, &ctx.sde)).collect())
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

/// Breaks a Courier Plastic Wrap item in the hauler's hangar (`feat:contracts.courier-plastic-wrap-break`).
/// Extracts wrapped items into hauler's hangar, marks the courier contract as Failed,
/// and transfers escrowed collateral to contract issuer via `fail_courier_contract`.
pub fn break_courier_plastic_wrap(
    snap: &mut eve_db::WorldStateSnapshot,
    char_id: i64,
    wrap_id: i64,
    now_unix: u64,
) -> Result<(i64, i64, Vec<i64>), BridgeRpcError> {
    let Some(wrap_item) = snap.items.get(&wrap_id).cloned() else {
        return Err(rpc_err("ItemNotFound", format!("Plastic Wrap {wrap_id} not found")));
    };
    let can_break = wrap_item.type_id == 3468
        && (wrap_item.owner_id == char_id
            || has_corp_contract_role(snap, char_id, wrap_item.owner_id, now_unix));
    if !can_break {
        return Err(rpc_err("InvalidPlasticWrap", format!("Item {wrap_id} is not a Courier Plastic Wrap accessible to {char_id}")));
    }
    let ch_opt = snap.characters.get(&char_id);
    let char_loc = ch_opt.and_then(|c| c.station_id.or(c.structure_id));
    let char_ship = ch_opt.map(|c| c.active_ship_id).filter(|&id| id > 0);
    if char_loc != Some(wrap_item.location_id) && char_ship != Some(wrap_item.location_id) {
        return Err(rpc_err("CustomInfo", format!("Must be docked at location or in ship {} where Plastic Wrap {wrap_id} is located", wrap_item.location_id)));
    }
    let cid_opt = snap.contracts.values().find(|c| {
        c.contract_type == "Courier"
            && c.status == "InProgress"
            && (c.acceptor_id == Some(char_id)
                || c.acceptor_id.is_some_and(|aid| {
                    aid == wrap_item.owner_id && has_corp_contract_role(snap, char_id, aid, now_unix)
                }))
            && c.item_ids.iter().any(|iid| snap.items.get(iid).is_some_and(|it| it.location_id == wrap_id))
    }).map(|c| (c.contract_id, c.issuer_id, c.acceptor_id.unwrap_or(char_id), c.item_ids.clone()));
    let Some((cid, issuer_id, acceptor_owner_id, item_ids)) = cid_opt else {
        return Err(rpc_err("ContractNotFound", format!("No active InProgress courier contract found for Plastic Wrap {wrap_id}")));
    };
    snap.fail_courier_contract(cid, now_unix);
    let target_flag = if snap.items.contains_key(&wrap_item.location_id) {
        InventoryFlag::Cargo
    } else {
        InventoryFlag::Hangar
    };
    let mut transferred_ids = Vec::new();
    for iid in &item_ids {
        if let Some(it) = snap.items.get_mut(iid) {
            if it.owner_id != acceptor_owner_id {
                transferred_ids.push(*iid);
            }
            it.owner_id = acceptor_owner_id;
            it.location_id = wrap_item.location_id;
            it.flag = target_flag;
            it.version += 1;
        }
    }
    snap.strip_ship_cosmetics(&transferred_ids);
    snap.remove_system_cleanup_item(wrap_id);
    Ok((cid, issuer_id, item_ids))
}

/// Resolves all due expired contracts (`Outstanding` / `InProgress` with `expiration_unix <= now_unix`)
/// and emits `OnContractExpired`, `OnContractFailed`, `OnContractCompleted`, `OnAccountChange`, and
/// `OnItemChange` notifications to all affected parties (`#736`).
pub fn sweep_expired_contracts_and_notify(ctx: &ServerContext, now_unix: u64) -> usize {
    let has_due = ctx.db.with_snapshot(|snap| {
        snap.contracts.values().any(|c| {
            matches!(c.status.as_str(), "Outstanding" | "InProgress")
                && c.expiration_unix > 0
                && c.expiration_unix <= now_unix
        })
    });
    if !has_due {
        return 0;
    }

    struct ContractNotifyBatch {
        contract_events: Vec<(i64, &'static str, i64)>,
        account_changes: Vec<(i64, i64, f64)>,
        item_changes: Vec<(i64, InventoryItemRecord, Vec<(i32, PyRep)>)>,
        resolved_count: usize,
    }

    let batch = ctx.db.with_mut_snapshot(|snap| {
        let mut due_ids: Vec<i64> = snap
            .contracts
            .values()
            .filter(|c| {
                matches!(c.status.as_str(), "Outstanding" | "InProgress")
                    && c.expiration_unix > 0
                    && c.expiration_unix <= now_unix
            })
            .map(|c| c.contract_id)
            .collect();
        due_ids.sort_unstable();

        let mut contract_events = Vec::new();
        let mut account_changes = Vec::new();
        let mut item_changes = Vec::new();
        let mut resolved_count = 0_usize;

        for cid in due_ids {
            let Some(c_before) = snap.contracts.get(&cid).cloned() else {
                continue;
            };
            let mut tracked_item_ids: Vec<i64> = c_before.item_ids.clone();
            for item in snap.items.values().filter(|it| it.location_id == cid) {
                if !tracked_item_ids.contains(&item.item_id) {
                    tracked_item_ids.push(item.item_id);
                }
            }
            let pre_items: Vec<(i64, i64, i64, i64)> = tracked_item_ids
                .iter()
                .filter_map(|iid| {
                    snap.items.get(iid).map(|it| {
                        (*iid, it.owner_id, it.location_id, i64::from(it.flag as u16))
                    })
                })
                .collect();

            let pre_issuer_bal = snap.characters.get(&c_before.issuer_id).map(|c| c.balance.0);
            let pre_corp_bal = (c_before.for_corp && c_before.issuer_corp_id != 0)
                .then(|| snap.corporations.get(&c_before.issuer_corp_id).map(|c| c.division_balance(1).0))
                .flatten();
            let pre_acceptor_bal = c_before
                .acceptor_id
                .and_then(|aid| snap.characters.get(&aid).map(|c| (aid, c.balance.0)));
            let pre_bidder_bal = c_before
                .current_bidder_id
                .and_then(|bid| snap.characters.get(&bid).map(|c| (bid, c.balance.0)));

            let Some(new_status) = snap.resolve_expired_contract(cid, now_unix) else {
                continue;
            };
            resolved_count += 1;

            let event_name = match new_status {
                "Failed" => "OnContractFailed",
                "Completed" => "OnContractCompleted",
                _ => "OnContractCacheCleared",
            };
            contract_events.push((c_before.issuer_id, event_name, cid));
            if let Some(aid) = c_before.acceptor_id.filter(|&id| id > 0 && id != c_before.issuer_id) {
                if snap.characters.contains_key(&aid) {
                    contract_events.push((aid, event_name, cid));
                }
            }
            if new_status == "Completed" {
                if let Some(bid) = c_before
                    .current_bidder_id
                    .filter(|&id| id > 0 && id != c_before.issuer_id && Some(id) != c_before.acceptor_id)
                {
                    if snap.characters.contains_key(&bid) {
                        contract_events.push((bid, event_name, cid));
                    }
                }
            }

            if let Some(pre) = pre_issuer_bal {
                if let Some(ch) = snap.characters.get(&c_before.issuer_id) {
                    if ch.balance.0 != pre {
                        account_changes.push((c_before.issuer_id, c_before.issuer_id, ch.balance.to_wire_f64()));
                    }
                }
            }
            if let Some(pre_corp) = pre_corp_bal {
                if let Some(corp) = snap.corporations.get(&c_before.issuer_corp_id) {
                    let post = corp.division_balance(1);
                    if post.0 != pre_corp {
                        account_changes.push((c_before.issuer_id, c_before.issuer_corp_id, post.to_wire_f64()));
                    }
                }
            }
            if let Some((aid, pre)) = pre_acceptor_bal {
                if let Some(ch) = snap.characters.get(&aid) {
                    if ch.balance.0 != pre {
                        account_changes.push((aid, aid, ch.balance.to_wire_f64()));
                    }
                }
            }
            if let Some((bid, pre)) = pre_bidder_bal {
                if let Some(ch) = snap.characters.get(&bid) {
                    if ch.balance.0 != pre {
                        account_changes.push((bid, bid, ch.balance.to_wire_f64()));
                    }
                }
            }

            for (iid, old_owner, old_loc, old_flag) in pre_items {
                if let Some(item) = snap.items.get(&iid).cloned() {
                    let mut diffs = Vec::new();
                    if item.owner_id != old_owner {
                        diffs.push((2, PyRep::Integer(old_owner)));
                    }
                    if item.location_id != old_loc {
                        diffs.push((3, PyRep::Integer(old_loc)));
                    }
                    let new_flag = i64::from(item.flag as u16);
                    if new_flag != old_flag {
                        diffs.push((4, PyRep::Integer(old_flag)));
                    }
                    if !diffs.is_empty() {
                        let notify_char = if snap.characters.contains_key(&item.owner_id) {
                            item.owner_id
                        } else {
                            c_before.issuer_id
                        };
                        if notify_char > 0 {
                            item_changes.push((notify_char, item, diffs));
                        }
                    }
                }
            }
        }

        ContractNotifyBatch {
            contract_events,
            account_changes,
            item_changes,
            resolved_count,
        }
    });

    for (target_char, event_name, cid) in batch.contract_events {
        if event_name == "OnContractCacheCleared" {
            ctx.notify(
                NotificationTarget::Character(target_char),
                event_name,
                "charid",
                vec![],
            );
        } else {
            notify_contract(ctx, target_char, event_name, cid);
        }
    }
    for (target_char, owner_id, bal_wire) in batch.account_changes {
        let _ = ctx.notify_account_change(target_char, "cash", owner_id, bal_wire);
    }
    for (target_char, item, diffs) in batch.item_changes {
        let _ = ctx.notify_item_record_change(target_char, &item, diffs);
    }

    batch.resolved_count
}

#[cfg(test)]
#[path = "contracts_service_tests.rs"]
mod contracts_service_tests;

#[cfg(test)]
#[path = "contracts_service_edges_tests.rs"]
mod contracts_service_edges_tests;

