//! Cohesive helper functions, ACL/role checks, lazy-expiry notification dispatch,
//! and extracted handlers for [`super::contracts_service`].

#![forbid(unsafe_code)]

use super::{contract_legacy_bid_div, rpc_err};
use crate::domain::market_acl::ContractAvailability;
use crate::router::{NotificationTarget, RpcArgs, ServerContext};
use eve_db::{ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, TypeId, WorldStateSnapshot};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{HashMap, HashSet};

pub fn notify_contract(ctx: &ServerContext, char_id: i64, event: &str, cid: i64) {
    match event {
        "OnContractCreated" => {
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnContractCreated,
            );
        }
        "OnContractAssigned" => {
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnContractAssigned { contract_id: cid },
            );
        }
        "OnContractAccepted" => {
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnContractAccepted { contract_id: cid },
            );
        }
        "OnContractCompleted" => {
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnContractCompleted { contract_id: cid },
            );
        }
        "OnContractOutbid" => {
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnContractOutbid { contract_id: cid },
            );
        }
        "OnContractBuyout" => {
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnContractBuyout { contract_id: cid },
            );
        }
        "OnContractFailed" => {
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnContractFailed { contract_id: cid },
            );
        }
        "OnContractRejected" => {
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnContractRejected { contract_id: cid },
            );
        }
        "OnShowContract" => {
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnShowContract { contract_id: cid },
            );
        }
        "OnContractCacheCleared" => {
            ctx.emit_typed(
                NotificationTarget::Character(char_id),
                crate::router::notify_registry::OnContractCacheCleared,
            );
        }
        other => {
            debug_assert!(false, "unhandled contract notification event: {other}");
        }
    }
}

pub(crate) fn push_contract_party_chars(
    snap: &WorldStateSnapshot,
    entity_id: i64,
    out: &mut Vec<i64>,
) {
    if entity_id <= 0 {
        return;
    }
    if snap.characters.contains_key(&entity_id) {
        if !out.contains(&entity_id) {
            out.push(entity_id);
        }
    } else if let Some(corp) = snap.corporations.get(&entity_id) {
        for &cid in &corp.members {
            if snap.characters.contains_key(&cid) && !out.contains(&cid) {
                out.push(cid);
            }
        }
        for ch in snap.characters.values() {
            if ch.corporation_id == entity_id && !out.contains(&ch.character_id) {
                out.push(ch.character_id);
            }
        }
    }
}

pub(crate) fn collect_contract_notify_chars(
    snap: &WorldStateSnapshot,
    c: &ContractRecord,
) -> Vec<i64> {
    let mut out = Vec::new();
    if c.issuer_id > 0 {
        out.push(c.issuer_id);
    }
    for maybe_id in [c.acceptor_id, c.current_bidder_id].into_iter().flatten() {
        push_contract_party_chars(snap, maybe_id, &mut out);
    }
    out
}

pub(crate) fn terminal_contract_event_name(status: &str) -> &'static str {
    match status {
        "Failed" => "OnContractFailed",
        "Completed" => "OnContractCompleted",
        "Rejected" => "OnContractRejected",
        _ => "OnContractCacheCleared",
    }
}

pub(crate) fn emit_contract_terminal_notifications(
    ctx: &ServerContext,
    party_chars: &[i64],
    status: &str,
    cid: i64,
) {
    let event = terminal_contract_event_name(status);
    for &char_id in party_chars {
        if char_id > 0 {
            notify_contract(ctx, char_id, event, cid);
        }
    }
}

pub(crate) struct ContractResolvedNotifications {
    fallback_char: i64,
    item_diffs: Vec<(Option<InventoryItemRecord>, Option<InventoryItemRecord>)>,
}

impl ContractResolvedNotifications {
    pub(crate) fn emit(self, ctx: &ServerContext) {
        for (pre, post) in self.item_diffs {
            ctx.notify_item_mutation(self.fallback_char, pre.as_ref(), post.as_ref());
        }
    }
}

pub(crate) struct ContractNotifyDelta {
    fallback_char: i64,
    pre_next_entity_id: i64,
    items_before: HashMap<i64, InventoryItemRecord>,
}

impl ContractNotifyDelta {
    pub(crate) fn capture(
        snap: &WorldStateSnapshot,
        fallback_char: i64,
        contract_id: i64,
        _owner_ids: &[i64],
        station_ids: &[i64],
        extra_item_ids: &[i64],
    ) -> Self {
        let mut items_before = HashMap::new();
        let mut root_ids: Vec<i64> = extra_item_ids.to_vec();
        for &iid in extra_item_ids {
            if let Some(it) = snap.items.get(&iid) {
                items_before.insert(iid, it.clone());
            }
        }
        for (&iid, it) in &snap.items {
            if (contract_id > 0 && it.location_id == contract_id)
                || station_ids.contains(&it.location_id)
            {
                items_before.insert(iid, it.clone());
                if contract_id > 0 && it.location_id == contract_id {
                    root_ids.push(iid);
                }
            }
        }
        for child_id in snap.collect_item_descendants_many(&root_ids) {
            if let Some(it) = snap.items.get(&child_id) {
                items_before.insert(child_id, it.clone());
            }
        }
        Self {
            fallback_char,
            pre_next_entity_id: snap.next_entity_id,
            items_before,
        }
    }

    pub(crate) fn item_changed(a: &InventoryItemRecord, b: &InventoryItemRecord) -> bool {
        a.owner_id != b.owner_id
            || a.location_id != b.location_id
            || a.flag != b.flag
            || a.quantity != b.quantity
            || a.singleton != b.singleton
            || a.type_id != b.type_id
    }

    pub(crate) fn finish(self, snap: &WorldStateSnapshot) -> ContractResolvedNotifications {
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
            if iid > self.pre_next_entity_id
                && iid <= snap.next_entity_id
                && !self.items_before.contains_key(&iid)
            {
                item_diffs.push((None, Some(post.clone())));
            }
        }
        ContractResolvedNotifications {
            fallback_char: self.fallback_char,
            item_diffs,
        }
    }
}

fn is_contract_due(c: &ContractRecord, now_unix: u64) -> bool {
    matches!(c.status.as_str(), "Outstanding" | "InProgress")
        && c.expiration_unix > 0
        && now_unix >= c.expiration_unix
}

fn collect_contract_wrap_extra(snap: &WorldStateSnapshot, c: &ContractRecord) -> Vec<i64> {
    let mut extra = c.item_ids.clone();
    let mut wrap_ids: Vec<i64> = snap
        .items
        .iter()
        .filter(|(_, it)| {
            c.item_ids
                .iter()
                .any(|iid| snap.items.get(iid).is_some_and(|ci| ci.location_id == it.item_id))
        })
        .map(|(id, _)| *id)
        .collect();
    extra.append(&mut wrap_ids);
    extra
}

/// Lazily resolves any due contracts in `cids` (or all due contracts if `cids` is `None`)
/// and emits typed contract, wallet, and item notifications to all affected parties.
pub(crate) fn lazy_resolve_expired_and_notify(
    ctx: &ServerContext,
    cids: Option<&[i64]>,
    now_unix: u64,
) -> Vec<(i64, &'static str)> {
    let any_due = ctx.db.with_snapshot(|snap| match cids {
        Some(ids) => ids
            .iter()
            .any(|id| snap.contracts.get(id).is_some_and(|c| is_contract_due(c, now_unix))),
        None => snap.contracts.values().any(|c| is_contract_due(c, now_unix)),
    });
    if !any_due {
        return Vec::new();
    }

    let resolved = ctx.db.with_mut_snapshot(|snap| {
        let mut due_ids: Vec<i64> = match cids {
            Some(ids) => ids
                .iter()
                .copied()
                .filter(|id| snap.contracts.get(id).is_some_and(|c| is_contract_due(c, now_unix)))
                .collect(),
            None => snap
                .contracts
                .values()
                .filter(|c| is_contract_due(c, now_unix))
                .map(|c| c.contract_id)
                .collect(),
        };
        due_ids.sort_unstable();
        due_ids.dedup();

        let mut events = Vec::new();
        for cid in due_ids {
            let Some(c_snap) = snap.contracts.get(&cid).cloned() else {
                continue;
            };
            let extra = collect_contract_wrap_extra(snap, &c_snap);
            let delta = ContractNotifyDelta::capture(
                snap,
                c_snap.issuer_id,
                cid,
                &[
                    c_snap.issuer_id,
                    c_snap.issuer_corp_id,
                    c_snap.acceptor_id.unwrap_or(0),
                    c_snap.current_bidder_id.unwrap_or(0),
                ],
                &[c_snap.start_station_id, c_snap.end_station_id],
                &extra,
            );
            let party_chars = collect_contract_notify_chars(snap, &c_snap);
            if let Some(new_state) = snap.resolve_expired_contract(cid, now_unix) {
                events.push((cid, new_state, party_chars, delta.finish(snap)));
            }
        }
        events
    });

    let mut summary = Vec::with_capacity(resolved.len());
    for (cid, new_state, party_chars, notify_delta) in resolved {
        notify_delta.emit(ctx);
        emit_contract_terminal_notifications(ctx, &party_chars, new_state, cid);
        summary.push((cid, new_state));
    }
    summary
}

pub(crate) fn parse_requested_items(
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
        let type_id = TypeId::try_from(tid)
            .ok()
            .filter(|&t| t > 0)
            .ok_or_else(|| {
                rpc_err(
                    "InvalidContractParameters",
                    format!("Invalid requested item (typeID={tid}, quantity={qty})"),
                )
            })?;
        let item_qty = eve_proto::ItemQty::try_from_i64(qty)
            .and_then(eve_proto::ItemQty::try_into_stack_i32)
            .map_err(|_| {
                rpc_err(
                    "InvalidContractParameters",
                    format!("Invalid requested item (typeID={tid}, quantity={qty})"),
                )
            })?;
        out.push((type_id, item_qty));
    }
    Ok(out)
}

pub(crate) fn parse_item_ids(raw: Option<&PyRep>) -> Vec<i64> {
    match raw {
        Some(PyRep::Integer(id)) if *id > 0 => vec![*id],
        Some(PyRep::List(items) | PyRep::Tuple(items)) => items
            .iter()
            .filter_map(|v| match v {
                PyRep::Integer(id) if *id > 0 => Some(*id),
                PyRep::Tuple(pair) | PyRep::List(pair) => {
                    pair.first().and_then(|x| x.as_i64()).filter(|id| *id > 0)
                }
                other => other
                    .dict_get("itemID")
                    .and_then(|x| x.as_i64())
                    .filter(|id| *id > 0),
            })
            .collect(),
        _ => Vec::new(),
    }
}

pub(crate) fn parse_contract_availability(
    raw: Option<&PyRep>,
) -> Result<ContractAvailability, BridgeRpcError> {
    match raw {
        Some(PyRep::Bool(false)) | Some(PyRep::Integer(0)) | None | Some(PyRep::None) => {
            Ok(ContractAvailability::Public)
        }
        Some(PyRep::Bool(true)) | Some(PyRep::Integer(1)) => Ok(ContractAvailability::Personal),
        Some(PyRep::Integer(2)) => Ok(ContractAvailability::Corporation),
        Some(PyRep::Integer(3)) => Ok(ContractAvailability::Alliance),
        Some(PyRep::Integer(other)) => Err(rpc_err(
            "InvalidContractParameters",
            format!("Unsupported contract availability code {other}"),
        )),
        Some(PyRep::String(s)) => ContractAvailability::parse(s).ok_or_else(|| {
            rpc_err(
                "InvalidContractParameters",
                format!("Unsupported contract availability '{s}'"),
            )
        }),
        Some(other) => Err(rpc_err(
            "InvalidContractParameters",
            format!("Unsupported contract availability {other:?}"),
        )),
    }
}

pub(crate) fn contract_location_ids(
    sde: &eve_sde::SdeRegistry,
    snap: &WorldStateSnapshot,
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

/// Returns `true` when `location_id` resolves to an SDE NPC station or a non-destroyed Upwell structure.
pub(crate) fn is_valid_contract_location(
    ctx: &ServerContext,
    snap: &WorldStateSnapshot,
    location_id: i64,
) -> bool {
    ctx.sde.stations.contains_key(&location_id)
        || ctx
            .sde
            .solar_systems
            .values()
            .any(|s| s.stations.iter().any(|st| st.station_id == location_id))
        || snap
            .structures
            .get(&location_id)
            .is_some_and(|st| st.state != "Destroyed")
}

pub(crate) fn contract_type_code(contract_type: &str) -> i64 {
    match contract_type {
        "ItemExchange" | "item_exchange" => 1,
        "Auction" | "auction" => 2,
        "Courier" | "courier" => 3,
        "Loan" | "loan" => 4,
        _ => 1,
    }
}

pub(crate) fn contract_status_code(status: &str) -> i64 {
    match status {
        "Outstanding" | "outstanding" => 0,
        "InProgress" | "in_progress" => 1,
        "FinishedIssuer" | "finished_issuer" => 2,
        "FinishedContractor" | "finished_contractor" => 3,
        "Finished" | "finished" | "Completed" | "completed" => 4,
        "Cancelled" | "cancelled" => 5,
        "Rejected" | "rejected" => 6,
        "Failed" | "failed" => 7,
        "Deleted" | "deleted" => 8,
        "Reversed" | "reversed" => 9,
        _ => 0,
    }
}

pub(crate) fn contract_availability_code(availability: &str) -> i64 {
    ContractAvailability::parse(availability)
        .map(ContractAvailability::wire_code)
        .unwrap_or(1)
}

pub(crate) fn unix_to_contract_filetime(unix_secs: i64) -> i64 {
    if unix_secs > 100_000_000_000_000 {
        unix_secs
    } else if unix_secs > 0 {
        unix_secs
            .saturating_add(11_644_473_600)
            .saturating_mul(10_000_000)
    } else {
        133_500_000_000_000_000
    }
}

#[cfg(test)]
pub(crate) fn check_contract_visibility(
    c: &ContractRecord,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
) -> bool {
    crate::domain::market_acl::is_contract_visible_to_character(c, char_id, corp_id, alliance_id)
}

pub(crate) fn check_contract_visibility_in_snap(
    snap: &WorldStateSnapshot,
    c: &ContractRecord,
    char_id: i64,
    corp_id: i64,
    alliance_id: Option<i64>,
) -> bool {
    crate::domain::market_acl::is_contract_visible_to_character_in_snap(
        snap,
        c,
        char_id,
        corp_id,
        alliance_id,
    )
}

pub(crate) fn active_corp_acct_div(session: &SessionState) -> usize {
    usize::try_from(session.corp_account_key.saturating_sub(1000).clamp(0, 6)).unwrap_or(0)
}

pub(crate) fn is_corp_contract_flag(flag: InventoryFlag) -> bool {
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

pub(crate) fn effective_corp_roles_at_location(
    snap: &WorldStateSnapshot,
    corp_id: i64,
    char_id: i64,
    location_id: Option<i64>,
    now_unix: u64,
) -> (bool, u64) {
    snap.effective_corp_roles_at(corp_id, char_id, location_id, 0, now_unix)
}

pub(crate) fn has_corp_contract_role(
    snap: &WorldStateSnapshot,
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

pub(crate) fn has_corp_wallet_take_role(
    snap: &WorldStateSnapshot,
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

pub(crate) fn has_corp_flag_take_role_at_location(
    snap: &WorldStateSnapshot,
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

pub(crate) fn is_item_in_asset_safety_wrap(
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

pub(crate) fn handle_place_bid(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let rpc = RpcArgs::new(args, kwargs);
    let cid = rpc.req_positive_i64(0, "contractID")?;
    let bid_isk = rpc.req_finite_f64(1, "bidAmount")?;
    let bid_for_corp = rpc.opt_bool(2, "forCorp")?.unwrap_or(false);
    if bid_isk <= 0.0 {
        return Err(rpc_err(
            "InvalidBidAmount",
            "Auction bid must be a positive finite ISK amount",
        ));
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
            crate::services::fleet_social::fleet_social_corps::promote_due_pending_corp_grants(
                ctx, corp_id,
            );
        }
    }

    if !lazy_resolve_expired_and_notify(ctx, Some(&[cid]), now_unix).is_empty() {
        return Err(rpc_err(
            "ContractExpired",
            format!("Auction contract {cid} has expired"),
        ));
    }

    let (new_status, effective_bid, is_buyout, prev_bidder_opt, issuer_id, transferred_ids, notify_delta) =
        ctx.db.try_with_mut_snapshot(|snap| {
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
                return Err(eve_proto::BridgeRpcError::invalid_contract_state(
                    cid,
                    "Outstanding",
                    &c.status,
                    format!("Auction {cid} is no longer Outstanding"),
                ));
            }
            let effective_caller_corp = snap
                .characters
                .get(&char_id)
                .map(|ch| ch.corporation_id)
                .filter(|&id| id != 0)
                .unwrap_or(caller_corp);
            if c.issuer_id == char_id
                || (bid_for_corp && c.for_corp && c.issuer_corp_id == effective_caller_corp)
            {
                return Err(rpc_err("CannotBidOnOwnContract", "Issuer cannot bid on their own auction"));
            }
            if !check_contract_visibility_in_snap(
                snap,
                &c,
                char_id,
                effective_caller_corp,
                caller_alliance,
            ) {
                return Err(rpc_err(
                    "ContractAccessDenied",
                    format!("Character {char_id} cannot bid on private auction {cid}"),
                ));
            }
            if bid_for_corp {
                if effective_caller_corp == 0 || !snap.corporations.contains_key(&effective_caller_corp) {
                    return Err(rpc_err(
                        "CrpAccessDenied",
                        "Corporate auction bids require a player corporation",
                    ));
                }
                if !has_corp_contract_role(snap, char_id, effective_caller_corp, now_unix) {
                    return Err(rpc_err(
                        "CrpAccessDenied",
                        "Bidding for a corporation requires ROLE_CONTRACT_MANAGER or ROLE_DIRECTOR",
                    ));
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
                } else {
                    let min_inc = (c.current_bid.0 / 20).max(10_000);
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
            let bidder_div_num = u8::try_from(acct_div_idx + 1).unwrap_or(1);
            let prev_div_opt = c.current_bidder_id.map(|prev_bidder| {
                let kv_div = snap.kv(eve_db::Ns::ContractBidDiv).get::<u8>(cid, &prev_bidder.to_string());
                kv_div
                    .or_else(|| contract_legacy_bid_div(snap, cid, prev_bidder, false))
                    .filter(|&d| (1..=7).contains(&d))
                    .unwrap_or(1)
            });
            // Only treat the existing escrow as immediately reusable if it belongs to the exact
            // same entity AND corporate wallet division (different corp divisions must fund the full bid).
            let same_wallet_bucket = c.current_bidder_id == Some(bidder_entity_id)
                && (!bid_for_corp || prev_div_opt == Some(bidder_div_num));
            let prev_self_refund = if same_wallet_bucket { c.current_bid } else { IskCents(0) };

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
                    return Err(rpc_err(
                        "InsufficientFunds",
                        "Insufficient corporation ISK to place auction bid",
                    ));
                }
            } else {
                let bidder_avail = snap
                    .characters
                    .get(&char_id)
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
                &[
                    char_id,
                    effective_caller_corp,
                    c.issuer_id,
                    c.issuer_corp_id,
                    c.current_bidder_id.unwrap_or(0),
                ],
                &[c.start_station_id],
                &c.item_ids,
            );
            let prev_bidder_opt = c.current_bidder_id;
            if let Some(prev_bidder) = prev_bidder_opt {
                if c.current_bid.0 > 0 {
                    let prev_sub = prev_bidder.to_string();
                    let kv_div = snap.kv(eve_db::Ns::ContractBidDiv).get::<u8>(cid, &prev_sub);
                    snap.kv_mut(eve_db::Ns::ContractBidDiv).remove(cid, &prev_sub);
                    let legacy_div = contract_legacy_bid_div(snap, cid, prev_bidder, true);
                    let prev_div = kv_div
                        .or(legacy_div)
                        .filter(|&d| (1..=7).contains(&d))
                        .unwrap_or(1);
                    if let Some(prev_corp) = snap.corporations.get_mut(&prev_bidder) {
                        let refunded = prev_corp
                            .division_balance(prev_div)
                            .checked_add(c.current_bid)
                            .unwrap_or(prev_corp.division_balance(prev_div));
                        prev_corp.set_division_balance(prev_div, refunded);
                        snap.record_contract_journal(
                            prev_bidder, prev_div, "ContractAuctionBidRefund",
                            eve_db::ledger::ref_types::CONTRACT_PRICE, cid, prev_bidder, cid,
                            c.current_bid, now_unix, format!("Outbid refund on auction contract {cid}"),
                        );
                    } else if let Some(prev_ch) = snap.characters.get_mut(&prev_bidder) {
                        prev_ch.balance = prev_ch.balance.checked_add(c.current_bid).unwrap_or(prev_ch.balance);
                        snap.record_contract_journal(
                            prev_bidder, 1, "ContractAuctionBidRefund",
                            eve_db::ledger::ref_types::CONTRACT_PRICE, cid, prev_bidder, cid,
                            c.current_bid, now_unix, format!("Outbid refund on auction contract {cid}"),
                        );
                    }
                }
            }

            if bid_for_corp {
                let corp = snap
                    .corporations
                    .get_mut(&effective_caller_corp)
                    .ok_or_else(|| rpc_err("CorpNotFound", format!("Corporation {effective_caller_corp} not found")))?;
                let new_bal = corp
                    .division_balance(bidder_div_num)
                    .checked_sub(effective_bid)
                    .ok_or_else(|| {
                        rpc_err("InsufficientFunds", "Insufficient corporation ISK to place auction bid")
                    })?;
                corp.set_division_balance(bidder_div_num, new_bal);
                snap.kv_mut(eve_db::Ns::ContractBidDiv)
                    .set_with_unix(cid, effective_caller_corp.to_string(), &bidder_div_num, now_unix)
                    .map_err(|e| rpc_err("ServiceStateLimitExceeded", e))?;
                snap.record_contract_journal(
                    effective_caller_corp, bidder_div_num, "ContractAuctionBid",
                    eve_db::ledger::ref_types::CONTRACT_PRICE, effective_caller_corp, cid, cid,
                    IskCents(-effective_bid.0), now_unix,
                    format!("Corporate bid placed on auction contract {cid}"),
                );
            } else {
                let bidder_ch = snap
                    .characters
                    .get_mut(&char_id)
                    .ok_or_else(|| rpc_err("CharacterNotFound", format!("Character {char_id} not found")))?;
                bidder_ch.balance = bidder_ch
                    .balance
                    .checked_sub(effective_bid)
                    .ok_or_else(|| rpc_err("InsufficientFunds", "Insufficient ISK to place auction bid"))?;
                snap.record_contract_journal(
                    char_id, 1, "ContractAuctionBid",
                    eve_db::ledger::ref_types::CONTRACT_PRICE, char_id, cid, cid,
                    IskCents(-effective_bid.0), now_unix,
                    format!("Bid placed on auction contract {cid}"),
                );
            }

            let mut new_status = "Outstanding";
            if is_buyout {
                snap.credit_contract_issuer_with_journal(
                    &c, effective_bid, "ContractAuctionSold",
                    eve_db::ledger::ref_types::CONTRACT_PRICE, bidder_entity_id, now_unix,
                    format!("Auction contract {cid} bought out by {bidder_entity_id}"),
                );
                snap.refund_contract_deposit(&c, now_unix, "bought-out auction");
                let win_flag = if bid_for_corp { InventoryFlag::CorpDeliveries } else { InventoryFlag::Hangar };
                let transferred_ids = snap.settle_contract_escrow_items(
                    cid, &c.item_ids, bidder_entity_id, c.start_station_id, Some(win_flag), false,
                );
                snap.clear_contract_divisions(cid);
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
            }
            let notify_delta = bid_delta.finish(snap);
            Ok((new_status, effective_bid, is_buyout, prev_bidder_opt, c.issuer_id, Vec::new(), notify_delta))
        })?;

    notify_delta.emit(ctx);
    if !transferred_ids.is_empty() {
        crate::services::quasar_cosmetics_live::strip_ship_cosmetics_for_ships(&ctx.db, &transferred_ids);
        for &tid in &transferred_ids {
            ctx.invalidate_ship_fitting_cache_only(tid);
        }
    }
    if let Some(prev_bidder) = prev_bidder_opt {
        if prev_bidder != char_id {
            notify_contract(ctx, prev_bidder, "OnContractOutbid", cid);
        }
    }
    if is_buyout {
        notify_contract(ctx, issuer_id, "OnContractBuyout", cid);
        notify_contract(ctx, issuer_id, "OnContractCompleted", cid);
    }
    Ok(PyRep::keyval([
        ("contractID", PyRep::Integer(cid)),
        ("status", PyRep::String(new_status.into())),
        ("currentBidISK", PyRep::Real(effective_bid.to_wire_f64())),
        ("highestBidderID", PyRep::Integer(char_id)),
        ("boughtOut", PyRep::Bool(is_buyout)),
    ]))
}

pub(crate) fn handle_gm_expire_contract(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (user_id, _) = ServerContext::require_character(session)?;
    let rpc = RpcArgs::new(args, kwargs);
    let cid = rpc.req_positive_i64(0, "contractID")?;
    let now_unix = ctx.now_unix();
    let (new_status, party_chars, notify_delta) = ctx.db.try_with_mut_snapshot(|snap| {
        let effective_role = snap
            .accounts
            .values()
            .find(|a| a.user_id == user_id)
            .map(|a| a.role)
            .unwrap_or_else(|| {
                if user_id != 0 && !snap.accounts.is_empty() {
                    0
                } else {
                    session.role
                }
            });
        let has_gm_role = (effective_role & eve_machonet::service_roles::ROLE_GM_MASK) != 0;
        if !has_gm_role {
            return Err(BridgeRpcError::insufficient_role_with_code(
                "GMRoleRequired",
                eve_machonet::service_roles::ROLE_GM_MASK,
                effective_role,
                "GM_ExpireContract",
                "GM_ExpireContract requires GM role",
            ));
        }
        let c_snap = snap
            .contracts
            .get(&cid)
            .cloned()
            .ok_or_else(|| rpc_err("ContractNotFound", format!("Contract {cid} does not exist")))?;
        let expire_extra = collect_contract_wrap_extra(snap, &c_snap);
        let expire_delta = ContractNotifyDelta::capture(
            snap,
            c_snap.issuer_id,
            cid,
            &[
                c_snap.issuer_id,
                c_snap.issuer_corp_id,
                c_snap.acceptor_id.unwrap_or(0),
                c_snap.current_bidder_id.unwrap_or(0),
            ],
            &[c_snap.start_station_id, c_snap.end_station_id],
            &expire_extra,
        );
        let party_chars = collect_contract_notify_chars(snap, &c_snap);
        let Some(c_mut) = snap.contracts.get_mut(&cid) else {
            return Err(rpc_err("ContractNotFound", format!("Contract {cid} does not exist")));
        };
        c_mut.expiration_unix = 1;
        let resolved = snap.resolve_expired_contract(cid, now_unix).unwrap_or("Expired");
        let notify_delta = expire_delta.finish(snap);
        Ok((resolved, party_chars, notify_delta))
    })?;

    notify_delta.emit(ctx);
    emit_contract_terminal_notifications(ctx, &party_chars, new_status, cid);
    Ok(PyRep::keyval([
        ("contractID", PyRep::Integer(cid)),
        ("status", PyRep::String(new_status.into())),
    ]))
}

pub(crate) fn handle_break_plastic_wrap_rpc(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let rpc = RpcArgs::new(args, kwargs);
    let wrap_id = match rpc.req_i64_any(0, &["itemID", "plasticWrapID"])? {
        v if v > 0 => v,
        _ => {
            return Err(rpc_err(
                "InvalidArgument",
                "BreakPlasticWrap requires positive wrap itemID",
            ))
        }
    };
    let now_unix = ctx.now_unix();
    let (cid, issuer_id, party_chars, extracted, transferred_ids, notify_delta) =
        ctx.db.try_with_mut_snapshot(|snap| {
            let wrap_item_ids: Vec<(i64, i64)> = snap
                .items
                .values()
                .filter(|it| it.location_id == wrap_id)
                .map(|it| (it.item_id, it.owner_id))
                .collect();
            let mut break_extra = vec![wrap_id];
            break_extra.extend(wrap_item_ids.iter().map(|(iid, _)| *iid));
            let mut break_owners: Vec<i64> = snap
                .items
                .get(&wrap_id)
                .map(|w| vec![char_id, w.owner_id])
                .unwrap_or_else(|| vec![char_id]);
            let matching_c = snap
                .contracts
                .values()
                .find(|c| {
                    c.contract_type == "Courier"
                        && c.status == "InProgress"
                        && c.item_ids.iter().any(|iid| snap.items.get(iid).is_some_and(|it| it.location_id == wrap_id))
                })
                .cloned();
            let party_chars = matching_c
                .as_ref()
                .map(|mc| collect_contract_notify_chars(snap, mc))
                .unwrap_or_default();
            if let Some(mc) = &matching_c {
                break_owners.push(mc.issuer_id);
                break_owners.push(mc.issuer_corp_id);
                if let Some(aid) = mc.acceptor_id {
                    break_owners.push(aid);
                }
            }
            let break_delta = ContractNotifyDelta::capture(snap, char_id, 0, &break_owners, &[], &break_extra);
            let (cid, issuer_id, extracted) =
                super::break_courier_plastic_wrap(snap, char_id, wrap_id, now_unix)?;
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
            Ok((cid, issuer_id, party_chars, extracted, transferred_ids, notify_delta))
        })?;

    notify_delta.emit(ctx);
    if !transferred_ids.is_empty() {
        crate::services::quasar_cosmetics_live::strip_ship_cosmetics_for_ships(&ctx.db, &transferred_ids);
    }
    if party_chars.is_empty() {
        notify_contract(ctx, issuer_id, "OnContractFailed", cid);
    } else {
        emit_contract_terminal_notifications(ctx, &party_chars, "Failed", cid);
    }
    Ok(PyRep::keyval([
        ("contractID", PyRep::Integer(cid)),
        ("status", PyRep::String("Failed".into())),
        ("extractedItemIDs", PyRep::List(extracted.into_iter().map(PyRep::Integer).collect())),
    ]))
}

fn validate_reject_contract_preconditions(
    snap: &WorldStateSnapshot, c: &ContractRecord, cid: i64, char_id: i64, effective_corp: i64, now_unix: u64,
) -> Result<(), BridgeRpcError> {
    let is_assignee = c.assignee_id != 0 && (c.assignee_id == char_id || c.assignee_id == effective_corp);
    if !is_assignee {
        return Err(eve_proto::BridgeRpcError::contract_ownership_violation(
            cid, char_id, (c.assignee_id != 0).then_some(c.assignee_id),
            format!("Character {char_id} is not the assignee of contract {cid}"),
        ));
    }
    if c.status != "Outstanding" {
        return Err(eve_proto::BridgeRpcError::invalid_contract_state(
            cid, "Outstanding", &c.status, format!("Contract {cid} in state '{}' cannot be rejected", c.status),
        ));
    }
    if c.assignee_id == effective_corp && c.assignee_id != char_id && !has_corp_contract_role(snap, char_id, effective_corp, now_unix) {
        return Err(rpc_err("CrpAccessDenied", "Corporate Contract Manager or Director role required to reject a corporate contract"));
    }
    Ok(())
}

pub(crate) fn handle_reject_delete_fail(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let rpc = RpcArgs::new(args, kwargs);
    let cid = rpc.req_positive_i64(0, "contractID")?;
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

    if method == "RejectContract" {
        let should_lazy_expire = ctx.db.with_snapshot(|snap| {
            let Some(c) = snap.contracts.get(&cid) else {
                return Ok(false);
            };
            let effective_corp = snap
                .characters
                .get(&char_id)
                .map(|ch| ch.corporation_id)
                .filter(|&id| id != 0)
                .unwrap_or(caller_corp);
            validate_reject_contract_preconditions(snap, c, cid, char_id, effective_corp, now_unix)?;
            Ok(is_contract_due(c, now_unix))
        })?;
        if should_lazy_expire && !lazy_resolve_expired_and_notify(ctx, Some(&[cid]), now_unix).is_empty() {
            return Err(rpc_err(
                "ContractExpired",
                format!("Contract {cid} has expired and was settled"),
            ));
        }
    }

    let (new_status, party_chars, notify_delta) = ctx.db.try_with_mut_snapshot(|snap| {
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
        let resolve_extra = collect_contract_wrap_extra(snap, &c);
        let resolve_delta = ContractNotifyDelta::capture(
            snap,
            char_id,
            cid,
            &[
                char_id,
                effective_corp,
                c.issuer_id,
                c.issuer_corp_id,
                c.acceptor_id.unwrap_or(0),
                c.current_bidder_id.unwrap_or(0),
            ],
            &[c.start_station_id, c.end_station_id],
            &resolve_extra,
        );
        let party_chars = collect_contract_notify_chars(snap, &c);
        let has_caller_corp_contract_role = has_corp_contract_role(snap, char_id, effective_corp, now_unix);

        if method == "RejectContract" {
            validate_reject_contract_preconditions(snap, &c, cid, char_id, effective_corp, now_unix)?;
            if c.contract_type == "Auction" && c.current_bidder_id.is_some() {
                return Err(eve_proto::BridgeRpcError::invalid_contract_state(
                    cid,
                    "NoBids",
                    "HasBids",
                    "Cannot reject an auction contract that has active bids",
                ));
            }
            snap.credit_contract_issuer_with_journal(
                &c, c.reward, "ContractRewardRefund", eve_db::ledger::ref_types::CONTRACT_REWARD,
                cid, now_unix, format!("Reward refunded on rejected contract {cid}"),
            );
            snap.refund_contract_deposit(&c, now_unix, "rejected");
            let _ = snap.settle_contract_escrow_items(
                cid, &c.item_ids, issuer_entity_id, c.start_station_id, None, false,
            );
            snap.clear_contract_divisions(cid);
            if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                c_mut.status = "Rejected".into();
            }
            return Ok(("Rejected", vec![c.issuer_id], resolve_delta.finish(snap)));
        }

        let can_manage_as_issuer = c.issuer_id == char_id
            || (c.for_corp
                && c.issuer_corp_id != 0
                && c.issuer_corp_id == effective_corp
                && has_caller_corp_contract_role);
        let is_corp_acceptor =
            effective_corp != 0 && c.acceptor_id == Some(effective_corp) && has_caller_corp_contract_role;
        if !can_manage_as_issuer && c.acceptor_id != Some(char_id) && !is_corp_acceptor {
            let required_owner_id = if method == "DeleteContract" {
                Some(issuer_entity_id)
            } else {
                c.acceptor_id
            };
            return Err(eve_proto::BridgeRpcError::contract_ownership_violation(
                cid,
                char_id,
                required_owner_id,
                format!("Character {char_id} is neither issuer nor acceptor of contract {cid}"),
            ));
        }
        if let Some(new_state) = snap.resolve_expired_contract(cid, now_unix) {
            return Ok((new_state, party_chars, resolve_delta.finish(snap)));
        }
        if method == "DeleteContract" {
            if !can_manage_as_issuer {
                return Err(eve_proto::BridgeRpcError::contract_ownership_violation(
                    cid, char_id, Some(issuer_entity_id),
                    format!("Character {char_id} cannot delete contract {cid}; only the issuer may delete it"),
                ));
            }
            if c.status != "Outstanding" && c.status != "Expired" {
                return Err(eve_proto::BridgeRpcError::invalid_contract_state(
                    cid, "Outstanding", &c.status,
                    format!("Contract {cid} in state '{}' cannot be deleted", c.status),
                ));
            }
            if c.status == "Outstanding" && c.contract_type == "Auction" && c.current_bidder_id.is_some() {
                return Err(rpc_err("CannotDeleteAuctionWithBids", format!("Auction contract {cid} already has active bids and cannot be deleted")));
            }
            let root_ids: &[i64] = if c.status == "Outstanding" {
                snap.credit_contract_issuer_with_journal(
                    &c, c.reward, "ContractRewardRefund", eve_db::ledger::ref_types::CONTRACT_REWARD,
                    cid, now_unix, format!("Reward refunded on deleted contract {cid}"),
                );
                snap.refund_contract_deposit(&c, now_unix, "deleted");
                &c.item_ids
            } else {
                &[]
            };
            let _ = snap.settle_contract_escrow_items(cid, root_ids, issuer_entity_id, c.start_station_id, None, true);
            snap.clear_contract_divisions(cid);
            if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                c_mut.status = "Deleted".into();
            }
            return Ok(("Deleted", Vec::new(), resolve_delta.finish(snap)));
        }
        if c.status == "Failed" && c.contract_type == "Courier" && c.expiration_unix > 0 && c.expiration_unix <= now_unix {
            return Ok(("Failed", Vec::new(), resolve_delta.finish(snap)));
        }
        if c.status != "InProgress" {
            return Err(eve_proto::BridgeRpcError::invalid_contract_state(
                cid, "InProgress", &c.status,
                format!("Contract {cid} in state '{}' cannot be resolved via {method}", c.status),
            ));
        }
        if c.contract_type != "Courier" {
            return Err(rpc_err("InvalidContractType", format!("Contract {cid} is not a Courier contract")));
        }
        if c.acceptor_id != Some(char_id) && !is_corp_acceptor {
            return Err(eve_proto::BridgeRpcError::contract_ownership_violation(
                cid, char_id, c.acceptor_id,
                format!("Character {char_id} is not the acceptor of contract {cid}"),
            ));
        }
        snap.fail_courier_contract(cid, now_unix);
        Ok(("Failed", party_chars, resolve_delta.finish(snap)))
    })?;

    notify_delta.emit(ctx);
    if new_status != "Deleted" && !party_chars.is_empty() {
        emit_contract_terminal_notifications(ctx, &party_chars, new_status, cid);
    }
    Ok(PyRep::keyval([
        ("contractID", PyRep::Integer(cid)),
        ("status", PyRep::String(new_status.into())),
    ]))
}

pub(crate) fn handle_get_login_info(
    ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let caller_corp = session.corporation_id.unwrap_or(0);
    let now_unix = ctx.now_unix();
    Ok(ctx.db.with_snapshot(|snap| {
        let dismissed: HashSet<String> = snap
            .quasar_state_blobs
            .get("contract_dismissed_notifs")
            .and_then(|s| serde_json::from_str::<Vec<String>>(s).ok())
            .unwrap_or_default()
            .into_iter()
            .collect();
        let is_dismissed = |cid: i64| {
            dismissed.contains(&format!("{char_id}:{cid}"))
                || (caller_corp != 0 && dismissed.contains(&format!("{caller_corp}:{cid}")))
        };
        let (mut assigned, mut needs_attention, mut in_progress) = (Vec::new(), Vec::new(), Vec::new());
        for c in snap.contracts.values() {
            if is_dismissed(c.contract_id) {
                continue;
            }
            if c.status == "Outstanding"
                && c.issuer_id != char_id
                && (c.assignee_id == char_id || (caller_corp != 0 && c.assignee_id == caller_corp))
            {
                assigned.push(PyRep::keyval([
                    ("contractID", PyRep::Integer(c.contract_id)),
                    ("issuerID", PyRep::Integer(c.issuer_id)),
                ]));
            } else if c.status == "Outstanding"
                && (c.issuer_id == char_id || c.acceptor_id == Some(char_id))
                && c.expiration_unix <= now_unix
            {
                needs_attention.push(PyRep::keyval([("contractID", PyRep::Integer(c.contract_id))]));
            } else if c.status == "InProgress"
                && (c.issuer_id == char_id || c.acceptor_id == Some(char_id))
            {
                let exp_ft = i64::try_from(c.expiration_unix)
                    .unwrap_or(i64::MAX)
                    .saturating_add(11_644_473_600)
                    .saturating_mul(10_000_000);
                in_progress.push(PyRep::keyval([
                    ("contractID", PyRep::Integer(c.contract_id)),
                    ("startStationID", PyRep::Integer(c.start_station_id)),
                    ("endStationID", PyRep::Integer(c.end_station_id)),
                    ("expires", PyRep::Integer(exp_ft)),
                ]));
            }
        }
        PyRep::keyval([
            ("assignedToMe", PyRep::List(assigned)),
            ("needsAttention", PyRep::List(needs_attention)),
            ("inProgress", PyRep::List(in_progress)),
        ])
    }))
}
