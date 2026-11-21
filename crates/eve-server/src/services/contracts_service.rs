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

#[path = "contracts_service_ops.rs"]
pub(crate) mod contracts_service_ops;
pub use contracts_service_ops::notify_contract;
#[cfg(test)]
pub(crate) use contracts_service_ops::check_contract_visibility;
pub(crate) use contracts_service_ops::{
    active_corp_acct_div, check_contract_visibility_in_snap, contract_availability_code,
    contract_location_ids, contract_status_code, contract_type_code,
    handle_break_plastic_wrap_rpc, handle_get_login_info, handle_gm_expire_contract,
    handle_place_bid, handle_reject_delete_fail, has_corp_contract_role,
    has_corp_flag_take_role_at_location, has_corp_wallet_take_role, is_corp_contract_flag,
    is_item_in_asset_safety_wrap, is_valid_contract_location, lazy_resolve_expired_and_notify,
    parse_contract_availability, parse_item_ids, parse_requested_items, push_contract_party_chars,
    rpc_err, unix_to_contract_filetime, ContractNotifyDelta,
};

use crate::domain::market_acl::ContractAvailability;
use crate::router::{NotificationTarget, RpcArgs, ServerContext};
use eve_db::{ContractRecord, InventoryFlag, InventoryItemRecord, IskCents, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, ItemQty, PyRep};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CreatedContractsReply {
    pub contract_ids: Vec<i64>,
}

impl CreatedContractsReply {
    pub(crate) fn single(contract_id: i64) -> Self {
        Self {
            contract_ids: vec![contract_id],
        }
    }

    pub(crate) fn to_pyrep(&self) -> PyRep {
        PyRep::List(self.contract_ids.iter().copied().map(PyRep::Integer).collect())
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContractItemReply {
    pub item_id: i64,
    pub type_id: i64,
    pub quantity: i64,
    pub in_crate: bool,
    pub parent_id: i64,
    pub flag_id: i64,
    pub singleton: bool,
    pub copy: bool,
    pub material_efficiency: i64,
    pub time_efficiency: i64,
    pub runs_remaining: Option<i32>,
    pub damage: f64,
    pub source_mutaplasmid_id: Option<u32>,
    pub dynamic_attributes: Vec<(i64, f64)>,
}

impl ContractItemReply {
    pub(crate) fn offered_with_parent(
        it: &eve_db::InventoryItemRecord,
        parent_id: i64,
        flag_id: i64,
    ) -> Self {
        let me = it.dynamic_attributes.get(&1112).copied().unwrap_or(0.0) as i64;
        let te = it.dynamic_attributes.get(&1113).copied().unwrap_or(0.0) as i64;
        let dmg = it.dynamic_attributes.get(&3).copied().unwrap_or(0.0);
        let mut dyn_entries: Vec<(i64, f64)> = it
            .dynamic_attributes
            .iter()
            .map(|(&attr_id, &val)| (i64::from(attr_id), val))
            .collect();
        dyn_entries.sort_by_key(|(k, _)| *k);
        Self {
            item_id: it.item_id,
            type_id: i64::from(it.type_id),
            quantity: i64::from(it.quantity).max(1),
            in_crate: true,
            parent_id,
            flag_id,
            singleton: it.singleton,
            copy: it.is_blueprint_copy,
            material_efficiency: me,
            time_efficiency: te,
            runs_remaining: it.blueprint_runs_remaining,
            damage: dmg,
            source_mutaplasmid_id: it.source_mutaplasmid_id,
            dynamic_attributes: dyn_entries,
        }
    }

    pub(crate) fn offered(it: &eve_db::InventoryItemRecord) -> Self {
        Self::offered_with_parent(it, 0, 0)
    }

    pub(crate) fn offered_child(it: &eve_db::InventoryItemRecord) -> Self {
        Self::offered_with_parent(it, it.location_id, i64::from(it.flag as u16))
    }

    pub(crate) fn requested(type_id: u32, quantity: i32) -> Self {
        Self {
            item_id: 0,
            type_id: i64::from(type_id),
            quantity: i64::from(quantity).max(1),
            in_crate: false,
            parent_id: 0,
            flag_id: 0,
            singleton: false,
            copy: false,
            material_efficiency: 0,
            time_efficiency: 0,
            runs_remaining: None,
            damage: 0.0,
            source_mutaplasmid_id: None,
            dynamic_attributes: Vec::new(),
        }
    }

    pub(crate) fn to_pyrep(&self) -> PyRep {
        let runs_rep = self
            .runs_remaining
            .map(|r| PyRep::Integer(i64::from(r)))
            .unwrap_or(PyRep::Integer(-1));
        // V24 contract UI evaluates `item.licensedProductionRunsRemaining or 0`;
        // -1 would be truthy and display "-1 runs", so unlimited/BPO uses None.
        let licensed_runs_rep = self
            .runs_remaining
            .map(|r| PyRep::Integer(i64::from(r)))
            .unwrap_or(PyRep::None);
        let mut fields = vec![
            ("itemID", PyRep::Integer(self.item_id)),
            ("typeID", PyRep::Integer(self.type_id)),
            ("itemTypeID", PyRep::Integer(self.type_id)),
            ("quantity", PyRep::Integer(self.quantity)),
            ("inCrate", PyRep::Bool(self.in_crate)),
            ("parentID", PyRep::Integer(self.parent_id)),
            ("flagID", PyRep::Integer(self.flag_id)),
            ("singleton", PyRep::Bool(self.singleton)),
            ("copy", PyRep::Bool(self.copy)),
            ("isBlueprintCopy", PyRep::Bool(self.copy)),
            ("materialLevel", PyRep::Integer(self.material_efficiency)),
            ("productivityLevel", PyRep::Integer(self.time_efficiency)),
            ("licensedProductionRunsRemaining", licensed_runs_rep),
            ("materialEfficiency", PyRep::Integer(self.material_efficiency)),
            ("timeEfficiency", PyRep::Integer(self.time_efficiency)),
            ("runs", runs_rep.clone()),
            ("runsRemaining", runs_rep),
            ("damage", PyRep::Real(self.damage)),
        ];
        if self.in_crate {
            let dyn_entries = self
                .dynamic_attributes
                .iter()
                .map(|&(attr_id, val)| (PyRep::Integer(attr_id), PyRep::Real(val)))
                .collect();
            fields.push((
                "sourceMutaplasmidID",
                self.source_mutaplasmid_id
                    .map(|m| PyRep::Integer(i64::from(m)))
                    .unwrap_or(PyRep::None),
            ));
            fields.push(("dynamicAttributes", PyRep::Dict(dyn_entries)));
        }
        PyRep::keyval(fields)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContractBidReply {
    pub bidder_id: i64,
    pub amount_isk: f64,
    pub timestamp_unix: i64,
}

impl ContractBidReply {
    pub(crate) fn to_pyrep(&self) -> PyRep {
        PyRep::keyval([
            ("bidderID", PyRep::Integer(self.bidder_id)),
            ("amount", PyRep::Real(self.amount_isk)),
            ("amountISK", PyRep::Real(self.amount_isk)),
            ("timestampUnix", PyRep::Integer(self.timestamp_unix)),
        ])
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContractReply {
    pub contract_id: i64,
    pub issuer_id: i64,
    pub issuer_corp_id: i64,
    pub acceptor_id: i64,
    pub assignee_id: i64,
    pub availability_code: i64,
    pub type_code: i64,
    pub status_code: i64,
    pub start_station_id: i64,
    pub end_station_id: i64,
    pub start_solar_system_id: i64,
    pub end_solar_system_id: i64,
    pub start_region_id: i64,
    pub end_region_id: i64,
    pub price: f64,
    pub reward: f64,
    pub collateral: f64,
    pub buyout: f64,
    pub current_bid: f64,
    pub current_bidder_id: Option<i64>,
    pub volume: f64,
    pub crate_id: i64,
    pub item_ids: Vec<i64>,
    pub items: Vec<ContractItemReply>,
    pub requested_items: Vec<(u32, i32)>,
    pub bids: Vec<ContractBidReply>,
    pub date_issued_ft: i64,
    pub date_expired_ft: i64,
    pub date_accepted_ft: i64,
    pub date_completed_ft: i64,
    pub expiration_unix: i64,
    pub date_issued_unix: i64,
    pub num_days: i64,
    pub title: String,
    pub for_corp: bool,
    pub primary_copy: bool,
    pub primary_me: i64,
    pub primary_te: i64,
    pub primary_runs: Option<i32>,
    pub primary_dmg: f64,
    pub primary_mutaplasmid_id: Option<u32>,
}

impl ContractReply {
    pub(crate) fn from_record(
        c: &ContractRecord,
        snap: &eve_db::WorldStateSnapshot,
        sde: &eve_sde::SdeRegistry,
    ) -> Self {
        let mut seen_item_ids = std::collections::HashSet::new();
        let mut items = Vec::new();
        for &id in &c.item_ids {
            if seen_item_ids.insert(id) {
                if let Some(it) = snap.items.get(&id) {
                    items.push(ContractItemReply::offered(it));
                }
            }
        }
        for child_id in snap.collect_item_descendants_many(&c.item_ids) {
            if seen_item_ids.insert(child_id) {
                if let Some(it) = snap.items.get(&child_id) {
                    items.push(ContractItemReply::offered_child(it));
                }
            }
        }
        for &(tid, qty) in &c.requested_items {
            if tid > 0 {
                items.push(ContractItemReply::requested(tid, qty));
            }
        }
        let bids = c
            .bid_history
            .iter()
            .map(|&(bidder_id, amount, ts)| ContractBidReply {
                bidder_id,
                amount_isk: amount.to_wire_f64(),
                timestamp_unix: i64::try_from(ts).unwrap_or(i64::MAX),
            })
            .collect();
        let availability_str = if c.availability.is_empty() {
            "Public"
        } else {
            c.availability.as_str()
        };
        let primary = c.item_ids.first().and_then(|id| snap.items.get(id));
        let (start_sys_id, start_reg_id) = contract_location_ids(sde, snap, c.start_station_id);
        let (raw_end_sys_id, end_reg_id) = contract_location_ids(sde, snap, c.end_station_id);
        let end_sys_id = if raw_end_sys_id > 0 {
            raw_end_sys_id
        } else {
            start_sys_id
        };
        let raw_date_issued_unix = i64::try_from(c.date_issued_unix).unwrap_or(i64::MAX);
        let raw_expiration_unix = i64::try_from(c.expiration_unix).unwrap_or(i64::MAX);
        let date_issued_unix = if c.date_issued_unix > 0 {
            raw_date_issued_unix
        } else {
            1_700_000_000
        };
        let expiration_unix = if c.expiration_unix > 0 {
            raw_expiration_unix
        } else {
            1_700_086_400
        };
        let date_issued_ft = unix_to_contract_filetime(date_issued_unix);
        let date_expired_ft = unix_to_contract_filetime(expiration_unix);
        let date_accepted_ft = unix_to_contract_filetime(1_700_000_000);
        Self {
            contract_id: c.contract_id,
            issuer_id: c.issuer_id,
            issuer_corp_id: c.issuer_corp_id,
            acceptor_id: c.acceptor_id.unwrap_or(0),
            assignee_id: c.assignee_id,
            availability_code: contract_availability_code(availability_str),
            type_code: contract_type_code(&c.contract_type),
            status_code: contract_status_code(&c.status),
            start_station_id: c.start_station_id,
            end_station_id: c.end_station_id,
            start_solar_system_id: start_sys_id,
            end_solar_system_id: end_sys_id,
            start_region_id: start_reg_id,
            end_region_id: end_reg_id,
            price: c.price.to_wire_f64(),
            reward: c.reward.to_wire_f64(),
            collateral: c.collateral.to_wire_f64(),
            buyout: c.buyout.to_wire_f64(),
            current_bid: c.current_bid.to_wire_f64(),
            current_bidder_id: c.current_bidder_id,
            volume: 1.0,
            crate_id: 0,
            item_ids: c.item_ids.clone(),
            items,
            requested_items: c.requested_items.clone(),
            bids,
            date_issued_ft,
            date_expired_ft,
            date_accepted_ft,
            date_completed_ft: date_accepted_ft,
            expiration_unix: raw_expiration_unix,
            date_issued_unix: raw_date_issued_unix,
            num_days: i64::from(c.num_days.max(1)),
            title: c.title.clone(),
            for_corp: c.for_corp,
            primary_copy: primary.map(|it| it.is_blueprint_copy).unwrap_or(false),
            primary_me: primary
                .map(|it| it.dynamic_attributes.get(&1112).copied().unwrap_or(0.0) as i64)
                .unwrap_or(0),
            primary_te: primary
                .map(|it| it.dynamic_attributes.get(&1113).copied().unwrap_or(0.0) as i64)
                .unwrap_or(0),
            primary_runs: primary.and_then(|it| it.blueprint_runs_remaining),
            primary_dmg: primary
                .map(|it| it.dynamic_attributes.get(&3).copied().unwrap_or(0.0))
                .unwrap_or(0.0),
            primary_mutaplasmid_id: primary.and_then(|it| it.source_mutaplasmid_id),
        }
    }

    pub(crate) fn items_pyrep(&self) -> PyRep {
        PyRep::List(self.items.iter().map(ContractItemReply::to_pyrep).collect())
    }

    pub(crate) fn bids_pyrep(&self) -> PyRep {
        PyRep::List(self.bids.iter().map(ContractBidReply::to_pyrep).collect())
    }

    fn base_fields(&self) -> Vec<(&'static str, PyRep)> {
        let item_ids_rep = PyRep::List(self.item_ids.iter().copied().map(PyRep::Integer).collect());
        let req_rep = PyRep::List(
            self.requested_items
                .iter()
                .map(|&(tid, qty)| {
                    PyRep::keyval([
                        ("typeID", PyRep::Integer(i64::from(tid))),
                        ("quantity", PyRep::Integer(i64::from(qty))),
                    ])
                })
                .collect(),
        );
        let primary_runs = self
            .primary_runs
            .map(|r| PyRep::Integer(i64::from(r)))
            .unwrap_or(PyRep::Integer(-1));
        let primary_muta = self
            .primary_mutaplasmid_id
            .map(|m| PyRep::Integer(i64::from(m)))
            .unwrap_or(PyRep::None);
        vec![
            ("contractID", PyRep::Integer(self.contract_id)),
            ("issuerID", PyRep::Integer(self.issuer_id)),
            ("issuerCorpID", PyRep::Integer(self.issuer_corp_id)),
            ("acceptorID", PyRep::Integer(self.acceptor_id)),
            ("assigneeID", PyRep::Integer(self.assignee_id)),
            ("availability", PyRep::Integer(self.availability_code)),
            ("type", PyRep::Integer(self.type_code)),
            ("status", PyRep::Integer(self.status_code)),
            ("startStationID", PyRep::Integer(self.start_station_id)),
            ("endStationID", PyRep::Integer(self.end_station_id)),
            ("startSolarSystemID", PyRep::Integer(self.start_solar_system_id)),
            ("endSolarSystemID", PyRep::Integer(self.end_solar_system_id)),
            ("startRegionID", PyRep::Integer(self.start_region_id)),
            ("endRegionID", PyRep::Integer(self.end_region_id)),
            ("price", PyRep::Real(self.price)),
            ("reward", PyRep::Real(self.reward)),
            ("collateral", PyRep::Real(self.collateral)),
            ("buyout", PyRep::Real(self.buyout)),
            ("currentBid", PyRep::Real(self.current_bid)),
            (
                "currentBidderID",
                self.current_bidder_id.map(PyRep::Integer).unwrap_or(PyRep::None),
            ),
            ("volume", PyRep::Real(self.volume)),
            ("crateID", PyRep::Integer(self.crate_id)),
            ("itemIDs", item_ids_rep),
            ("items", self.items_pyrep()),
            ("requestedItems", req_rep),
            ("bids", self.bids_pyrep()),
            ("numBids", PyRep::Integer(i64::try_from(self.bids.len()).unwrap_or(i64::MAX))),
            ("dateIssued", PyRep::Integer(self.date_issued_ft)),
            ("dateExpired", PyRep::Integer(self.date_expired_ft)),
            ("dateAccepted", PyRep::Integer(self.date_accepted_ft)),
            ("dateCompleted", PyRep::Integer(self.date_completed_ft)),
            ("expirationUnix", PyRep::Integer(self.expiration_unix)),
            ("dateIssuedUnix", PyRep::Integer(self.date_issued_unix)),
            ("numDays", PyRep::Integer(self.num_days)),
            ("title", PyRep::String(self.title.clone())),
            ("forCorp", PyRep::Bool(self.for_corp)),
            ("copy", PyRep::Bool(self.primary_copy)),
            ("isBlueprintCopy", PyRep::Bool(self.primary_copy)),
            ("materialEfficiency", PyRep::Integer(self.primary_me)),
            ("timeEfficiency", PyRep::Integer(self.primary_te)),
            ("runs", primary_runs.clone()),
            ("runsRemaining", primary_runs),
            ("damage", PyRep::Real(self.primary_dmg)),
            ("sourceMutaplasmidID", primary_muta),
        ]
    }

    pub(crate) fn inner_contract_pyrep(&self) -> PyRep {
        PyRep::keyval(self.base_fields())
    }

    pub(crate) fn to_pyrep(&self) -> PyRep {
        let base = self.base_fields();
        let contract_kv = PyRep::keyval(base.clone());
        let mut outer = base;
        outer.push(("contract", contract_kv));
        PyRep::keyval(outer)
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContractListReply {
    pub contracts: Vec<ContractReply>,
}

impl ContractListReply {
    pub(crate) fn from_records(
        list: &[ContractRecord],
        snap: &eve_db::WorldStateSnapshot,
        sde: &eve_sde::SdeRegistry,
    ) -> Self {
        Self {
            contracts: list
                .iter()
                .map(|c| ContractReply::from_record(c, snap, sde))
                .collect(),
        }
    }

    pub(crate) fn to_pyrep(&self) -> PyRep {
        let mut contracts_rep = Vec::with_capacity(self.contracts.len());
        let mut items_by_cid = Vec::with_capacity(self.contracts.len());
        for c in &self.contracts {
            items_by_cid.push((PyRep::Integer(c.contract_id), c.items_pyrep()));
            contracts_rep.push(c.to_pyrep());
        }
        PyRep::keyval([
            ("contracts", PyRep::List(contracts_rep)),
            ("items", PyRep::Dict(items_by_cid)),
        ])
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContractSearchEntry {
    pub contract: ContractReply,
}

impl ContractSearchEntry {
    pub(crate) fn to_pyrep(&self) -> PyRep {
        PyRep::keyval([
            ("contract", self.contract.inner_contract_pyrep()),
            ("items", self.contract.items_pyrep()),
            ("bids", self.contract.bids_pyrep()),
            ("numBids", PyRep::Integer(i64::try_from(self.contract.bids.len()).unwrap_or(i64::MAX))),
        ])
    }
}

#[derive(Debug, Clone)]
pub(crate) struct ContractSearchReply {
    pub contracts: Vec<ContractSearchEntry>,
    pub max_results: i64,
    pub search_time: i64,
}

impl ContractSearchReply {
    pub(crate) fn from_records(
        list: &[ContractRecord],
        snap: &eve_db::WorldStateSnapshot,
        sde: &eve_sde::SdeRegistry,
    ) -> Self {
        Self {
            contracts: list
                .iter()
                .map(|c| ContractSearchEntry {
                    contract: ContractReply::from_record(c, snap, sde),
                })
                .collect(),
            max_results: 100,
            search_time: 1,
        }
    }

    pub(crate) fn to_pyrep(&self) -> PyRep {
        let entries = self
            .contracts
            .iter()
            .map(ContractSearchEntry::to_pyrep)
            .collect::<Vec<_>>();
        let n = i64::try_from(entries.len()).unwrap_or(i64::MAX);
        PyRep::keyval([
            ("contracts", PyRep::List(entries)),
            ("numFound", PyRep::Integer(n)),
            ("maxResults", PyRep::Integer(self.max_results)),
            ("searchTime", PyRep::Integer(self.search_time)),
        ])
    }
}

fn contract_legacy_blob_u64(
    snap: &mut eve_db::WorldStateSnapshot,
    key: &str,
    remove: bool,
) -> Option<u64> {
    let blobs = &mut snap.service_state_blobs;
    let raw = if remove { blobs.remove(key) } else { blobs.get(key).cloned() };
    raw.and_then(|s| s.parse::<u64>().ok())
}

pub(crate) fn pop_escrowed_item_orig_flag(
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
            contract_legacy_blob_u64(snap, &format!("contract_orig_flag:{cid}:{iid}"), true)
                .and_then(|v| u16::try_from(v).ok())
        })
        .map(InventoryFlag::from_u16)
        .filter(|f| *f != InventoryFlag::Locked)
        .unwrap_or(InventoryFlag::Hangar)
}

pub(crate) fn contract_legacy_bid_div(
    snap: &mut eve_db::WorldStateSnapshot,
    cid: i64,
    bidder_id: i64,
    remove: bool,
) -> Option<u8> {
    contract_legacy_blob_u64(snap, &format!("contract_bid_div:{cid}:{bidder_id}"), remove)
        .and_then(|v| u8::try_from(v).ok())
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
            let avail_raw = kwargs
                .get("availability")
                .filter(|v| !matches!(v, PyRep::None))
                .or_else(|| p_field_some("availability"))
                .or_else(|| p_field_some("isPrivate"))
                .or_else(|| kwargs.get("isPrivate").filter(|v| !matches!(v, PyRep::None)))
                .or_else(|| args.get(1));
            let parsed_avail = match parse_contract_availability(avail_raw) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
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
            let parsed_avail = if assignee_id > 0 && parsed_avail == ContractAvailability::Public {
                ContractAvailability::Personal
            } else {
                parsed_avail
            };
            let availability = parsed_avail.as_str().to_string();
            let num_days = p_field_some("numDays")
                .or_else(|| args.get(4))
                .and_then(|a| a.as_i64())
                .filter(|&d| d > 0)
                .and_then(|d| u32::try_from(d.clamp(1, 28)).ok())
                .unwrap_or(1);
            let duration_secs = if let Some(exp_min) = p_field_some("expireTime")
                .or_else(|| p_field_some("minutesExpire"))
                .or_else(|| kwargs.get("minutesExpire"))
                .or_else(|| args.get(3))
                .and_then(|a| a.as_i64())
                .filter(|&m| m > 0)
            {
                u64::try_from(exp_min)
                    .unwrap_or(u64::MAX)
                    .saturating_mul(60)
                    .clamp(60, 2_419_200)
            } else {
                86_400
            };
            let Some(start_station_id) = p_field_some("startStationID")
                .or_else(|| args.get(5))
                .and_then(|a| a.as_i64())
                .filter(|&id| id > 0)
                .or(session.station_id)
                .or(session.structure_id)
            else {
                return Some(Err(rpc_err(
                    "InvalidContractStation",
                    "Contract requires a valid startStationID when caller is not docked",
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

            if !item_ids_req.is_empty() {
                let mut seen_item_ids = std::collections::HashSet::with_capacity(item_ids_req.len());
                for &iid in &item_ids_req {
                    if !seen_item_ids.insert(iid) {
                        return Some(Err(rpc_err(
                            "InvalidContractParameters",
                            format!("Duplicate item {iid} in contract item list"),
                        )));
                    }
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

            let due_issuer_cids: Vec<i64> = ctx.db.with_snapshot(|snap| {
                snap.contracts
                    .values()
                    .filter(|c| {
                        c.issuer_id == char_id
                            && matches!(c.status.as_str(), "Outstanding" | "InProgress")
                            && c.expiration_unix > 0
                            && now_unix >= c.expiration_unix
                    })
                    .map(|c| c.contract_id)
                    .collect()
            });
            if !due_issuer_cids.is_empty() {
                let _ = lazy_resolve_expired_and_notify(ctx, Some(&due_issuer_cids), now_unix);
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                if !is_valid_contract_location(ctx, snap, start_station_id)
                    || !is_valid_contract_location(ctx, snap, end_station_id)
                {
                    return Err(rpc_err(
                        "InvalidContractStation",
                        format!("Invalid start {start_station_id} or destination {end_station_id}"),
                    ));
                }
                if item_ids_req.is_empty() && !snap.has_structure_acl_access(start_station_id, char_id) {
                    return Err(rpc_err(
                        "StructureAccessDenied",
                        format!("Character {char_id} lacks access to structure {start_station_id}"),
                    ));
                }

                let active_count = snap.contracts.values().filter(|c| {
                        c.issuer_id == char_id
                            && c.for_corp == for_corp
                            && matches!(c.status.as_str(), "Outstanding" | "InProgress")
                    }).count();
                let max_slots = if for_corp {
                    let lvl = snap.skills.get(&char_id).and_then(|m| m.get(&25233)).map(|s| usize::from(s.level))
                        .unwrap_or(1);
                    10 + 10 * lvl
                } else if let Some(skill) = snap.skills.get(&char_id).and_then(|m| m.get(&25235)) {
                    1 + 4 * usize::from(skill.level)
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

                let Some(issuer_corp_id) = snap.characters.get(&char_id).map(|c| c.corporation_id) else {
                    return Err(rpc_err("CharacterNotFound", format!("Character {char_id} not found")));
                };
                let mut resolved_assignee_id = assignee_id;
                if parsed_avail == ContractAvailability::Alliance && resolved_assignee_id == 0 {
                    let issuer_alliance = snap
                        .corporations
                        .get(&issuer_corp_id)
                        .and_then(|corp| corp.alliance_id)
                        .filter(|&a| a != 0)
                        .or_else(|| {
                            snap.characters
                                .get(&char_id)
                                .and_then(|ch| ch.alliance_id)
                                .filter(|&a| a != 0)
                        });
                    resolved_assignee_id = issuer_alliance.ok_or_else(|| {
                        rpc_err(
                            "CrpAccessDenied",
                            "Alliance contracts require membership in an alliance",
                        )
                    })?;
                }
                let is_public =
                    parsed_avail == ContractAvailability::Public && resolved_assignee_id == 0;
                let broker_fee_isk = if is_public { (price.to_wire_f64() * 0.01).max(10_000.0) } else { 10_000.0 };
                let deposit_isk = if is_public {
                    (price.to_wire_f64().max(collateral.to_wire_f64()) * 0.01).max(100_000.0)
                } else { 0.0
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
                let deposit_cents = IskCents::from_f64_isk(deposit_isk);
                let fee_cents = IskCents::from_f64_isk(broker_fee_isk + deposit_isk);
                let broker_fee_cents = IskCents(fee_cents.0.saturating_sub(deposit_cents.0));
                let total_upfront = reward
                    .checked_add(fee_cents)
                    .ok_or_else(|| rpc_err("InvalidContractParameters", "Contract reward and fee overflow"))?;
                let issuer_div_num = if for_corp { u8::try_from(acct_div_idx + 1).unwrap_or(1) } else { 1 };
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
                        corp.debit_division(issuer_div_num, total_upfront)
                            .ok_or_else(|| rpc_err("InsufficientFunds", format!("Not enough funds in corporation wallet division {issuer_div_num} to escrow contract reward")))?;
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
                    let descendants = snap.collect_item_descendants(iid);
                    if contract_type == "Courier"
                        && (!descendants.is_empty() || snap.is_container_non_empty(iid))
                    {
                        return Err(rpc_err(
                            "CannotContractNonEmptyContainer",
                            format!("Cannot escrow item {iid} while it contains items"),
                        ));
                    }
                    if snap.is_item_in_alsc_lock(iid) {
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
                    for &child_id in &descendants {
                        if snap.is_character_boarded_or_saved_ship(char_id, active_ship, child_id) {
                            return Err(rpc_err(
                                "CannotContractActiveShip",
                                "Cannot escrow the currently boarded active ship into a contract",
                            ));
                        }
                        if snap.is_item_in_alsc_lock(child_id) {
                            return Err(rpc_err(
                                "ContractItemLocked",
                                format!("Contained item {child_id} is locked inside an Audit Log Secure Container"),
                            ));
                        }
                        if snap
                            .item_custom_names
                            .get(&child_id)
                            .is_some_and(|name| name.starts_with("courier_cargo:"))
                        {
                            return Err(rpc_err(
                                "ItemCannotBeContracted",
                                "Courier mission items cannot be placed in contracts",
                            ));
                        }
                        let Some(child_snap) = snap.items.get(&child_id) else {
                            return Err(rpc_err(
                                "ContractItemNotFound",
                                format!("Contained contract item {child_id} not found"),
                            ));
                        };
                        if is_item_in_asset_safety_wrap(&ext, child_id, child_snap.location_id, child_snap.type_id) {
                            return Err(rpc_err(
                                "ItemCannotBeContracted",
                                format!("Contained item {child_id} is an Asset Safety wrap or sealed inside one"),
                            ));
                        }
                        let valid_child_owner = if for_corp {
                            issuer_corp_id != 0 && child_snap.owner_id == issuer_corp_id
                        } else {
                            child_snap.owner_id == char_id
                        };
                        if !valid_child_owner {
                            return Err(rpc_err(
                                "ContractItemOwnershipOrLocationInvalid",
                                format!("Contained item {child_id} must be owned by issuer"),
                            ));
                        }
                        if child_snap.flag == InventoryFlag::Locked
                            || snap.is_item_locked_in_contract_or_job(child_id)
                            || snap.is_item_or_ancestor_locked_or_impounded(child_id)
                            || snap.is_war_hq_structure(child_id)
                        {
                            return Err(rpc_err(
                                "ContractItemLocked",
                                format!("Contained item {child_id} is locked or ineligible for contracting"),
                            ));
                        }
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

                if deposit_cents.0 > 0 {
                    snap.fleet_and_social_ops
                        .contract_deposits
                        .insert(id, deposit_cents.0);
                }
                if for_corp && issuer_div_num > 1 {
                    snap.kv_mut(eve_db::Ns::ContractOrigFlag)
                        .set_with_unix(id, "issuer_div", &u16::from(issuer_div_num), now_unix)
                        .map_err(|e| rpc_err("ServiceStateLimitExceeded", e))?;
                }
                let issuer_entity_id = if for_corp { issuer_corp_id } else { char_id };
                snap.record_contract_journal(
                    issuer_entity_id, issuer_div_num, "ContractReward",
                    eve_db::ledger::ref_types::CONTRACT_REWARD, issuer_entity_id, id, id,
                    IskCents(-reward.0), now_unix, format!("Escrowed reward for contract {id}"),
                );
                snap.record_contract_journal(
                    issuer_entity_id, issuer_div_num, "ContractBrokersFee",
                    eve_db::ledger::ref_types::CONTRACT_BROKERS_FEE, issuer_entity_id, 1_000_023, id,
                    IskCents(-broker_fee_cents.0), now_unix, format!("Contract broker fee for contract {id}"),
                );
                snap.record_contract_journal(
                    issuer_entity_id, issuer_div_num, "ContractDeposit",
                    eve_db::ledger::ref_types::CONTRACT_BROKERS_FEE, issuer_entity_id, id, id,
                    IskCents(-deposit_cents.0), now_unix, format!("Escrowed deposit for contract {id}"),
                );

                let exp_unix = now_unix.saturating_add(duration_secs);
                snap.contracts.insert(id, ContractRecord {
                    contract_id: id, issuer_id: char_id, issuer_corp_id, acceptor_id: None,
                    assignee_id: resolved_assignee_id,
                    availability, contract_type: contract_type.to_string(), status: "Outstanding".into(),
                    start_station_id, end_station_id, price, reward, collateral, buyout,
                    current_bid: IskCents(0), current_bidder_id: None, bid_history: Vec::new(),
                    item_ids: escrowed_items, requested_items, expiration_unix: exp_unix,
                    date_issued_unix: now_unix, title, for_corp, num_days,
                });
                let notify_delta = delta.finish(snap);
                Ok((id, resolved_assignee_id, notify_delta))
            });

            match res {
                Ok((cid, resolved_assignee_id, notify_delta)) => {
                    notify_delta.emit(ctx);
                    let escrowed_ids = ctx.db.with_snapshot(|s| {
                        s.contracts
                            .get(&cid)
                            .map(|c| {
                                let mut ids = c.item_ids.clone();
                                ids.extend(s.collect_item_descendants_many(&c.item_ids));
                                ids
                            })
                            .unwrap_or_default()
                    });
                    for iid in escrowed_ids {
                        let _ = crate::services::combat_crimewatch::combat_wrecks_killmails::void_ship_insurance_contract(ctx, iid);
                    }
                    notify_contract(ctx, char_id, "OnContractCreated", cid);
                    if resolved_assignee_id > 0 { notify_contract(ctx, resolved_assignee_id, "OnContractAssigned", cid); }
                    Some(Ok(CreatedContractsReply::single(cid).to_pyrep()))
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

            if !lazy_resolve_expired_and_notify(ctx, Some(&[cid]), now_unix).is_empty() {
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
                let is_corp_avail = ContractAvailability::parse(&c.availability)
                    == Some(ContractAvailability::Corporation);
                let is_corp_accept = accept_for_corp
                    || (c.assignee_id != 0
                        && c.assignee_id == effective_caller_corp
                        && c.assignee_id != char_id
                        && !is_corp_avail);
                let is_self_corp_contract = c.for_corp
                    && c.issuer_corp_id != 0
                    && c.issuer_corp_id == effective_caller_corp
                    && (is_corp_accept || (c.assignee_id != char_id && c.assignee_id != effective_caller_corp && !is_corp_avail));
                if c.issuer_id == char_id || is_self_corp_contract {
                    return Err(rpc_err("CannotAcceptOwnContract", "Issuer cannot accept their own contract"));
                }
                if !check_contract_visibility_in_snap(snap, &c, char_id, effective_caller_corp, caller_alliance) {
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
                    let mut aggregated_reqs: Vec<(u32, ItemQty)> = Vec::new();
                    for &(req_tid, req_qty) in &c.requested_items {
                        let Ok(qty) = ItemQty::try_from_i32(req_qty) else {
                            continue;
                        };
                        if let Some(entry) = aggregated_reqs.iter_mut().find(|(t, _)| *t == req_tid) {
                            entry.1 = entry
                                .1
                                .checked_add(qty)
                                .map_err(|_| rpc_err("ContractOverflow", "Requested item quantity overflow"))?;
                        } else {
                            aggregated_reqs.push((req_tid, qty));
                        }
                    }
                    let active_ship = caller_ship.or_else(|| snap.characters.get(&char_id).map(|ch| ch.active_ship_id).filter(|&id| id > 0));
                    for (req_tid, req_qty) in aggregated_reqs {
                        let mut needed = Some(req_qty);
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
                                && !snap.is_item_in_alsc_lock(it.item_id)
                                && !is_item_in_asset_safety_wrap(&ext, it.item_id, it.location_id, it.type_id)
                                && !snap.is_item_locked_in_contract_or_job(it.item_id)
                                && !snap.items.values().any(|child| child.item_id != it.item_id && child.location_id == it.item_id && child.quantity > 0)
                                && !snap.item_custom_names.get(&it.item_id).is_some_and(|name| name.starts_with("courier_cargo:"))
                        }).map(|it| (it.item_id, it.quantity)).collect();
                        candidate_ids.sort_by_key(|(id, _)| *id);
                        for (cand_id, cand_qty) in candidate_ids {
                            let Some(rem) = needed else {
                                break;
                            };
                            let Ok(cand_item_qty) = ItemQty::try_from_i32(cand_qty) else {
                                continue;
                            };
                            let take_qty = rem.min(cand_item_qty);
                            let take_i32 = take_qty
                                .try_into_stack_i32()
                                .map_err(|e| e.into_rpc_error("ContractOverflow"))?;
                            requested_transfers.push((cand_id, take_i32));
                            needed = rem.checked_sub(take_qty);
                        }
                        if let Some(rem) = needed {
                            return Err(rpc_err("MissingRequestedContractItems", format!("Acceptor lacks {rem} units of requested type {req_tid} in station hangar {}", c.start_station_id)));
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
                let acceptor_div_num = if is_corp_accept { u8::try_from(acct_div_idx + 1).unwrap_or(1) } else { 1 };
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
                    let Some(corp) = snap.corporations.get_mut(&effective_caller_corp) else {
                        return Err(rpc_err("CorpNotFound", format!("Corporation {effective_caller_corp} not found")));
                    };
                    let cur = corp.division_balance(acceptor_div_num);
                    let mut new_bal = cur.checked_sub(required_isk).ok_or_else(|| rpc_err("InsufficientFunds", "Not enough corporation ISK to cover contract collateral and price"))?;
                    if c.contract_type == "ItemExchange" && c.reward.0 > 0 {
                        new_bal = new_bal.checked_add(c.reward).unwrap_or(new_bal);
                    }
                    corp.set_division_balance(acceptor_div_num, new_bal);
                    if c.contract_type == "Courier" && acceptor_div_num > 1 {
                        snap.kv_mut(eve_db::Ns::ContractBidDiv)
                            .set_with_unix(cid, effective_caller_corp.to_string(), &acceptor_div_num, now_unix)
                            .map_err(|e| rpc_err("ServiceStateLimitExceeded", e))?;
                    }
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
                snap.record_contract_journal(
                    acceptor_entity, acceptor_div_num, "ContractPrice",
                    eve_db::ledger::ref_types::CONTRACT_PRICE, acceptor_entity, issuer_entity, cid,
                    IskCents(-c.price.0), now_unix, format!("Paid price for contract {cid}"),
                );
                snap.record_contract_journal(
                    acceptor_entity, acceptor_div_num, "ContractCollateral",
                    eve_db::ledger::ref_types::CONTRACT_COLLATERAL, acceptor_entity, cid, cid,
                    IskCents(-c.collateral.0), now_unix, format!("Collateral escrowed for courier contract {cid}"),
                );
                if c.contract_type == "ItemExchange" {
                    snap.record_contract_journal(
                        acceptor_entity, acceptor_div_num, "ContractReward",
                        eve_db::ledger::ref_types::CONTRACT_REWARD, issuer_entity, acceptor_entity, cid,
                        c.reward, now_unix, format!("Reward received on contract {cid}"),
                    );
                }

                snap.credit_contract_issuer_with_journal(
                    &c, c.price, "ContractPrice", eve_db::ledger::ref_types::CONTRACT_PRICE,
                    acceptor_entity, now_unix, format!("Payment received on contract {cid}"),
                );

                if c.contract_type == "ItemExchange" {
                    snap.refund_contract_deposit(&c, now_unix, "accepted");
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

                    let transferred_ids = snap.settle_contract_escrow_items(
                        cid,
                        &c.item_ids,
                        acceptor_entity_id,
                        c.start_station_id,
                        Some(acceptor_flag),
                        false,
                    );
                    snap.clear_contract_divisions(cid);
                    if let Some(c_mut) = snap.contracts.get_mut(&cid) {
                        c_mut.acceptor_id = Some(acceptor_entity_id);
                        c_mut.status = "Completed".into();
                    }
                    let notify_delta = accept_delta.finish(snap);
                    return Ok(("Completed", c.expiration_unix, None, c.issuer_id, transferred_ids, notify_delta));
                }

                snap.refund_contract_deposit(&c, now_unix, "accepted courier");
                let (courier_owner_id, wrap_flag) = if is_corp_accept {
                    (effective_caller_corp, InventoryFlag::CorpDeliveries)
                } else {
                    (char_id, InventoryFlag::Hangar)
                };
                snap.next_entity_id += 1;
                let wrap_id = snap.next_entity_id;
                snap.items.insert(wrap_id, InventoryItemRecord {
                    item_id: wrap_id, type_id: 3468, owner_id: courier_owner_id, location_id: c.start_station_id,
                    flag: wrap_flag, quantity: 1, singleton: true, version: 1, ..Default::default()
                });
                for iid in &c.item_ids {
                    let _ = pop_escrowed_item_orig_flag(snap, cid, *iid);
                    if let Some(item) = snap.items.get_mut(iid) {
                        item.location_id = wrap_id;
                        item.flag = InventoryFlag::Locked;
                        item.version += 1;
                    }
                }
                let courier_days = u64::from(c.num_days.max(1)).min(28);
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
                        for &tid in &transferred_ids {
                            ctx.invalidate_ship_fitting_cache_only(tid);
                        }
                    }
                    notify_contract(ctx, issuer_id, "OnContractAccepted", cid);
                    if new_status == "Completed" {
                        notify_contract(ctx, issuer_id, "OnContractCompleted", cid);
                    }
                    let mut fields = vec![
                        ("contractID", PyRep::Integer(cid)),
                        ("status", PyRep::String(new_status.into())),
                        ("expirationUnix", PyRep::Integer(i64::try_from(exp_unix).unwrap_or(i64::MAX))),
                    ];
                    if let Some(wid) = wrap_id_opt { fields.push(("courierPackageID", PyRep::Integer(wid))); }
                    Some(Ok(PyRep::keyval(fields)))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "PlaceBid" | "Bid" => Some(handle_place_bid(ctx, session, args, kwargs)),

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
            if !lazy_resolve_expired_and_notify(ctx, Some(&[cid]), now_unix).is_empty() {
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
                let deliver_flag = if c.for_corp && c.issuer_corp_id != 0 {
                    InventoryFlag::CorpDeliveries
                } else {
                    InventoryFlag::Hangar
                };
                for iid in &c.item_ids {
                    if let Some(item) = snap.items.get_mut(iid) {
                        item.owner_id = issuer_entity_id;
                        item.location_id = c.end_station_id;
                        item.flag = deliver_flag;
                        item.version += 1;
                    }
                }
                for wid in wraps_to_remove { snap.remove_system_cleanup_item(wid); }
                if let Some(c_mut) = snap.contracts.get_mut(&cid) { c_mut.status = "Completed".into(); }
                snap.refund_contract_deposit(&c, now_unix, "completed courier");
                let total_return = c.collateral.checked_add(c.reward).unwrap_or(c.collateral);
                let (acceptor_entity_id, acceptor_div) = if is_corp_acceptor {
                    let div = snap
                        .kv(eve_db::Ns::ContractBidDiv)
                        .get::<u8>(cid, &effective_corp.to_string())
                        .filter(|&d| (1..=7).contains(&d))
                        .unwrap_or(1);
                    if let Some(corp) = snap.corporations.get_mut(&effective_corp) {
                        corp.credit_division(div, total_return);
                    }
                    (effective_corp, div)
                } else {
                    if let Some(ch) = snap.characters.get_mut(&char_id) {
                        ch.balance = ch.balance.checked_add(total_return).unwrap_or(ch.balance);
                    }
                    (char_id, 1)
                };
                snap.record_contract_journal(
                    acceptor_entity_id, acceptor_div, "ContractCollateral",
                    eve_db::ledger::ref_types::CONTRACT_COLLATERAL, cid, acceptor_entity_id, cid,
                    c.collateral, now_unix, format!("Collateral returned on completed courier contract {cid}"),
                );
                snap.record_contract_journal(
                    acceptor_entity_id, acceptor_div, "ContractReward",
                    eve_db::ledger::ref_types::CONTRACT_REWARD, issuer_entity_id, acceptor_entity_id, cid,
                    c.reward, now_unix, format!("Reward paid on completed courier contract {cid}"),
                );
                snap.clear_contract_divisions(cid);
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

        "GM_ExpireContract" => Some(handle_gm_expire_contract(ctx, session, args, kwargs)),

        "BreakPlasticWrap" => Some(handle_break_plastic_wrap_rpc(ctx, session, args, kwargs)),

        "DeleteContract" | "FailContract" | "RejectContract" => {
            Some(handle_reject_delete_fail(ctx, session, method, args, kwargs))
        }

        "GetContractInfo" | "GetContract" => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let rpc = RpcArgs::new(args, kwargs);
            let cid = match rpc.req_positive_i64(0, "contractID") { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let caller_corp = session.corporation_id.unwrap_or(0);
            let caller_alliance = session.alliance_id;
            let now_unix = ctx.now_unix();
            let _ = lazy_resolve_expired_and_notify(ctx, Some(&[cid]), now_unix);
            let res = ctx.db.with_snapshot(|snap| {
                let Some(c) = snap.contracts.get(&cid) else {
                    return Err(rpc_err("ContractNotFound", format!("Contract {cid} does not exist")));
                };
                let effective_caller_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|ch| ch.corporation_id)
                    .filter(|&id| id != 0)
                    .unwrap_or(caller_corp);
                if !check_contract_visibility_in_snap(snap, c, char_id, effective_caller_corp, caller_alliance)
                    && c.acceptor_id != Some(char_id)
                {
                    return Err(rpc_err("ContractAccessDenied", format!("Character {char_id} cannot view private contract {cid}")));
                }
                Ok(ContractReply::from_record(c, snap, &ctx.sde).to_pyrep())
            });
            if res.is_ok() && method == "GetContractInfo" { notify_contract(ctx, char_id, "OnShowContract", cid); }
            Some(res)
        }

        "SearchContracts" | "GetPublicContractList" => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let filter_type = args.first().and_then(|a| a.as_str()).or_else(|| kwargs.get("contractType").and_then(|v| v.as_str()));
            let filter_station = kwargs.get("stationID").and_then(|v| v.as_i64());
            let filter_region = kwargs.get("regionID").and_then(|v| v.as_i64());
            let filter_item_type = kwargs
                .get("itemTypeID")
                .or_else(|| kwargs.get("typeID"))
                .and_then(|v| v.as_i64());
            let min_price = kwargs.get("minPrice").and_then(|v| v.as_f64()).map(IskCents::from_f64_isk);
            let max_price = kwargs.get("maxPrice").and_then(|v| v.as_f64()).map(IskCents::from_f64_isk);
            let caller_corp = session.corporation_id.unwrap_or(0);
            let caller_alliance = session.alliance_id;
            let now_unix = ctx.now_unix();

            let _ = lazy_resolve_expired_and_notify(ctx, None, now_unix);
            let reply = ctx.db.with_snapshot(|snap| {
                    let effective_caller_corp = snap
                        .characters
                        .get(&char_id)
                        .map(|ch| ch.corporation_id)
                        .filter(|&id| id != 0)
                        .unwrap_or(caller_corp);
                    let mut list: Vec<ContractRecord> = snap.contracts.values().filter(|c| {
                            let region_ok = filter_region.map(|reg| {
                                let Ok(reg_id) = u32::try_from(reg) else {
                                    return false;
                                };
                                let loc_known = ctx.sde.stations.contains_key(&c.start_station_id)
                                    || snap.structures.contains_key(&c.start_station_id);
                                if loc_known {
                                    crate::domain::market_acl::contract_in_region(
                                        &ctx.sde,
                                        snap,
                                        c.start_station_id,
                                        reg_id,
                                    )
                                } else {
                                    true
                                }
                            }).unwrap_or(true);
                            let item_type_ok = filter_item_type.map(|tid| {
                                    TypeId::try_from(tid).ok().is_some_and(|tid_u32| {
                                        let descendants = snap.collect_item_descendants_many(&c.item_ids);
                                        c.item_ids.iter().chain(descendants.iter())
                                            .any(|iid| snap.items.get(iid).is_some_and(|it| it.type_id == tid_u32))
                                            || c.requested_items.iter().any(|&(rtid, _)| rtid == tid_u32)
                                    })
                                }).unwrap_or(true);
                            c.status == "Outstanding"
                                && check_contract_visibility_in_snap(snap, c, char_id, effective_caller_corp, caller_alliance)
                                && filter_type.map(|ft| c.contract_type == ft).unwrap_or(true)
                                && filter_station.map(|st| c.start_station_id == st).unwrap_or(true)
                                && min_price.map(|mp| c.price.0 >= mp.0).unwrap_or(true)
                                && max_price.map(|mp| c.price.0 <= mp.0).unwrap_or(true)
                                && region_ok
                                && item_type_ok
                        }).cloned().collect();
                    list.sort_by_key(|c| c.contract_id);
                    ContractSearchReply::from_records(&list, snap, &ctx.sde)
                });
            Some(Ok(reply.to_pyrep()))
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
            let _ = lazy_resolve_expired_and_notify(ctx, None, now_unix);
            let reply = ctx.db.with_snapshot(|snap| {
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
                ContractListReply::from_records(&list, snap, &ctx.sde)
            });
            Some(Ok(reply.to_pyrep()))
        }

        "GetMyExpiredContractList" => {
            let (_, char_id) = match ServerContext::require_character(session) { Ok(v) => v, Err(e) => return Some(Err(e)) };
            let caller_corp = session.corporation_id.unwrap_or(0);
            let for_corp = args.first().and_then(|a| match a { PyRep::Bool(b) => Some(*b), _ => None })
                .or_else(|| kwargs.get("forCorp").and_then(|v| match v { PyRep::Bool(b) => Some(*b), _ => None }))
                .unwrap_or(false);
            let now_unix = ctx.now_unix();
            let _ = lazy_resolve_expired_and_notify(ctx, None, now_unix);
            let reply = ctx.db.with_snapshot(|snap| {
                let mut list: Vec<ContractRecord> = snap.contracts.values().filter(|c| {
                    // Exclude active (Outstanding / InProgress) and terminal deleted/cancelled/reversed
                    // contracts from Requires Attention (#1328); keep Expired, Failed, Rejected,
                    // unclaimed FinishedIssuer / FinishedContractor, and settled Completed / Finished rows.
                    !matches!(
                        c.status.as_str(),
                        "Outstanding"
                            | "outstanding"
                            | "InProgress"
                            | "in_progress"
                            | "Deleted"
                            | "deleted"
                            | "Cancelled"
                            | "cancelled"
                            | "Reversed"
                            | "reversed"
                    )
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
                ContractListReply::from_records(&list, snap, &ctx.sde)
            });
            Some(Ok(reply.to_pyrep()))
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
            if owner_id != char_id {
                let auth_res = ctx.db.with_snapshot(|snap| {
                    let caller_corp = snap.characters.get(&char_id).map(|c| c.corporation_id).or(session.corporation_id).unwrap_or(0);
                    let allowed = owner_id == caller_corp
                        && has_corp_contract_role(snap, char_id, caller_corp, now_unix);
                    if !allowed {
                        return Err(rpc_err("CorpRoleDenied", "Viewing corporation auction bids requires Contract Manager or Director role"));
                    }
                    Ok(())
                });
                if let Err(e) = auth_res {
                    return Some(Err(e));
                }
            }
            let _ = lazy_resolve_expired_and_notify(ctx, None, now_unix);
            let res = ctx.db.with_snapshot(|snap| -> Result<ContractListReply, BridgeRpcError> {
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
                Ok(ContractListReply::from_records(&list, snap, &ctx.sde))
            });
            Some(res.map(|reply| reply.to_pyrep()))
        }

        "GetLoginInfo" => Some(handle_get_login_info(ctx, session)),

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
    } else if snap.corporations.contains_key(&acceptor_owner_id) {
        InventoryFlag::CorpDeliveries
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
        account_changes: Vec<(i64, String, i64, f64)>,
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
            for child_id in snap.collect_item_descendants_many(&tracked_item_ids) {
                if !tracked_item_ids.contains(&child_id) {
                    tracked_item_ids.push(child_id);
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
            let pre_corp_bals: Vec<(u8, i64)> = (c_before.for_corp && c_before.issuer_corp_id != 0)
                .then(|| {
                    snap.corporations.get(&c_before.issuer_corp_id).map(|c| {
                        (1..=7_u8).map(|d| (d, c.division_balance(d).0)).collect()
                    })
                })
                .flatten()
                .unwrap_or_default();
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
            let mut party_targets = Vec::new();
            if c_before.issuer_id > 0 {
                party_targets.push(c_before.issuer_id);
            }
            if let Some(aid) = c_before.acceptor_id {
                push_contract_party_chars(snap, aid, &mut party_targets);
            }
            if new_status == "Completed" {
                if let Some(bid) = c_before.current_bidder_id {
                    push_contract_party_chars(snap, bid, &mut party_targets);
                }
            }
            for target_id in party_targets {
                contract_events.push((target_id, event_name, cid));
            }

            if let Some(pre) = pre_issuer_bal {
                if let Some(ch) = snap.characters.get(&c_before.issuer_id) {
                    if ch.balance.0 != pre {
                        account_changes.push((c_before.issuer_id, "cash".to_string(), c_before.issuer_id, ch.balance.to_wire_f64()));
                    }
                }
            }
            if !pre_corp_bals.is_empty() {
                if let Some(corp) = snap.corporations.get(&c_before.issuer_corp_id) {
                    for (div_id, pre) in pre_corp_bals {
                        let post = corp.division_balance(div_id);
                        if post.0 != pre {
                            let key = if div_id <= 1 { "cash".to_string() } else { format!("cash{div_id}") };
                            account_changes.push((c_before.issuer_id, key, c_before.issuer_corp_id, post.to_wire_f64()));
                        }
                    }
                }
            }
            if let Some((aid, pre)) = pre_acceptor_bal {
                if let Some(ch) = snap.characters.get(&aid) {
                    if ch.balance.0 != pre {
                        account_changes.push((aid, "cash".to_string(), aid, ch.balance.to_wire_f64()));
                    }
                }
            }
            if let Some((bid, pre)) = pre_bidder_bal {
                if let Some(ch) = snap.characters.get(&bid) {
                    if ch.balance.0 != pre {
                        account_changes.push((bid, "cash".to_string(), bid, ch.balance.to_wire_f64()));
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
            ctx.emit_typed(
                NotificationTarget::Character(target_char),
                crate::router::notify_registry::OnContractCacheCleared,
            );
        } else {
            notify_contract(ctx, target_char, event_name, cid);
        }
    }
    for (target_char, key, owner_id, bal_wire) in batch.account_changes {
        let _ = ctx.notify_account_change(target_char, &key, owner_id, bal_wire);
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

#[cfg(test)]
mod qty_tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn requested_items_and_corp_div_reject_truncation_and_overflow() {
        let kw = HashMap::new();
        for (tid, qty) in [
            (34_i64, 0_i64),
            (34_i64, -1_i64),
            (34_i64, i64::from(i32::MAX) + 1),
            (i64::from(u32::MAX) + 35, 10_i64),
            (0_i64, 10_i64),
        ] {
            let rep = PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(tid),
                PyRep::Integer(qty),
            ])]);
            let err = parse_requested_items(Some(&rep), &kw).unwrap_err();
            assert_eq!(err.code, "InvalidContractParameters");
        }

        let valid = PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(34),
            PyRep::Integer(i64::from(i32::MAX)),
        ])]);
        assert_eq!(
            parse_requested_items(Some(&valid), &kw).unwrap(),
            vec![(34, i32::MAX)]
        );

        let mut sess = SessionState::default();
        sess.corp_account_key = i32::MIN;
        assert_eq!(active_corp_acct_div(&sess), 0);
        sess.corp_account_key = 1003;
        assert_eq!(active_corp_acct_div(&sess), 3);
        sess.corp_account_key = i32::MAX;
        assert_eq!(active_corp_acct_div(&sess), 6);
    }
}

