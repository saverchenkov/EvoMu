//! `market_trading` — Regional Market Order Book, Matching Engine, Limit Orders, Escrow, Fees, & Price History (`E-1`, `E-2`).
//!
//! Handles `marketProxy` RPCs for:
//! - Limit orders (`PlaceBuyOrder`, `PlaceMultiSellOrder` with `vol_entered`, `vol_remaining`, `escrow`)
//! - Immediate order matching against opposite resting orders (Price-Time Priority)
//! - Escrow ISK deduction for buy orders and escrow refund on cancellation
//! - Broker fee calculation (`broker_fee_rate = 0.03 - 0.003 * broker_relations`, plus standing modifiers)
//! - Sales tax calculation (`compute_sales_tax_rate` with Accounting skill discount)
//! - Order cancellation with escrow and item refund (`CancelCharOrder`)
//! - Regional order book queries (`GetOrders`, `GetStationAsks`, `GetRegionBest`, `GetCharOrders`)
//! - Price history queries (`GetOldPriceHistory`, `GetNewPriceHistory`)
//! - Atomic multi-item buy order execution (`BuyMultipleItems`)

#![forbid(unsafe_code)]

#[path = "market_orders_escrow.rs"]
pub mod market_orders_escrow;
pub use market_orders_escrow::*;

use super::market_contracts::PLEX_TYPE_ID;
use crate::formulas::{compute_broker_fee_rate, compute_sales_tax_rate};
use crate::router::{NotificationTarget, ServerContext};
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, DbColumnType, PyRep};
use eve_sde::TypeId;
use std::collections::{HashMap, HashSet, VecDeque};

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub(crate) struct MarketOrderMeta {
    pub(crate) range: i64,
    pub(crate) duration: i64,
    pub(crate) issue_unix: u64,
    #[serde(default)]
    pub(crate) order_state: Option<u8>,
    #[serde(default)]
    pub(crate) account_key: Option<u16>,
    #[serde(default)]
    pub(crate) final_vol_remaining: Option<i32>,
    #[serde(default)]
    pub(crate) closed_unix: Option<u64>,
}

fn market_order_meta_note_id(order_id: i64) -> i64 {
    -(4_000_000_000_000_i64.saturating_add(order_id.abs()))
}

pub(crate) fn save_market_order_meta(
    snap: &mut eve_db::WorldStateSnapshot,
    order_id: i64,
    meta: MarketOrderMeta,
) {
    let nid = market_order_meta_note_id(order_id);
    if let Ok(js) = serde_json::to_string(&meta) {
        snap.owner_notes.insert(
            nid,
            eve_db::OwnerNoteRecord {
                note_id: nid,
                character_id: -4,
                item_id: order_id,
                label: format!("mkt_order:{order_id}"),
                note: js,
                timestamp_unix: meta.issue_unix,
            },
        );
    }
}

pub(crate) fn load_market_order_meta(
    snap: &eve_db::WorldStateSnapshot,
    o: &MarketOrderRecord,
    default_unix: u64,
) -> MarketOrderMeta {
    let nid = market_order_meta_note_id(o.order_id);
    if let Some(note) = snap.owner_notes.get(&nid) {
        if let Ok(meta) = serde_json::from_str::<MarketOrderMeta>(&note.note) {
            return meta;
        }
    }
    MarketOrderMeta {
        range: if o.is_bid { -1 } else { 32767 },
        duration: if o.is_npc { 365 } else { 90 },
        issue_unix: default_unix,
        order_state: None,
        account_key: None,
        final_vol_remaining: None,
        closed_unix: None,
    }
}

pub(crate) fn unix_to_filetime(unix_secs: u64) -> i64 {
    (unix_secs as i64 + 11_644_473_600).saturating_mul(10_000_000)
}

fn market_order_descriptor() -> DBRowDescriptor {
    DBRowDescriptor::new([
        ("price", DbColumnType::R8),
        ("volRemaining", DbColumnType::R8),
        ("typeID", DbColumnType::I4),
        ("orderID", DbColumnType::I8),
        ("volEntered", DbColumnType::I4),
        ("minVolume", DbColumnType::I4),
        ("bid", DbColumnType::Bool),
        ("stationID", DbColumnType::I8),
        ("regionID", DbColumnType::I4),
        ("range", DbColumnType::I2),
        ("issueDate", DbColumnType::FileTime),
        ("duration", DbColumnType::I2),
    ])
}

pub(crate) fn resolve_station_solar_system(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    station_id: i64,
) -> Option<u32> {
    if let Some(st) = snap.structures.get(&station_id) {
        return Some(st.solar_system_id);
    }
    if let Some(st) = ctx.sde.stations.get(&station_id) {
        return Some(st.solar_system_id);
    }
    for sys in ctx.sde.solar_systems.values() {
        if sys.stations.iter().any(|s| s.station_id == station_id) {
            return Some(sys.solar_system_id);
        }
    }
    None
}

pub(crate) fn solar_system_jump_distance(
    ctx: &ServerContext,
    from_sys: u32,
    to_sys: u32,
) -> Option<u32> {
    if from_sys == to_sys {
        return Some(0);
    }
    let mut visited = HashSet::from([from_sys]);
    let mut queue = VecDeque::from([(from_sys, 0_u32)]);
    while let Some((curr, dist)) = queue.pop_front() {
        if dist >= 40 {
            continue;
        }
        for jump in ctx.sde.jumps.values() {
            let neighbor_opt = if jump.from_solar_system_id == curr {
                Some(jump.to_solar_system_id)
            } else if jump.to_solar_system_id == curr {
                Some(jump.from_solar_system_id)
            } else {
                None
            };
            if let Some(neighbor) = neighbor_opt {
                if neighbor == to_sys {
                    return Some(dist + 1);
                }
                if visited.insert(neighbor) {
                    queue.push_back((neighbor, dist + 1));
                }
            }
        }
    }
    None
}

pub(crate) fn can_bid_match_seller_station(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    bid_order: &MarketOrderRecord,
    seller_station_id: i64,
    default_unix: u64,
) -> bool {
    if bid_order.station_id == seller_station_id {
        return true;
    }
    let meta = load_market_order_meta(snap, bid_order, default_unix);
    if meta.range < 0 {
        return false;
    }
    if meta.range >= 32767 {
        return true;
    }
    let Some(bid_sys) = resolve_station_solar_system(ctx, snap, bid_order.station_id) else {
        return false;
    };
    let Some(sell_sys) = resolve_station_solar_system(ctx, snap, seller_station_id) else {
        return false;
    };
    if bid_sys == sell_sys {
        return true;
    }
    if meta.range == 0 {
        return false;
    }
    let Some(jumps) = solar_system_jump_distance(ctx, bid_sys, sell_sys) else {
        return false;
    };
    (jumps as i64) <= meta.range
}

pub(crate) fn is_structure_market_online(
    snap: &eve_db::WorldStateSnapshot,
    structure_id: i64,
) -> bool {
    let Some(st) = snap.structures.get(&structure_id) else {
        return true;
    };
    if st.state == "Destroyed" {
        return false;
    }
    let mut has_market_hub = false;
    for item in snap.items.values() {
        if item.location_id == structure_id && item.type_id == 35_892 {
            has_market_hub = true;
            if snap.module_online_states.get(&item.item_id).copied().unwrap_or(true) {
                return true;
            }
        }
    }
    !has_market_hub
}

pub(crate) fn resolve_structure_broker_rate(
    snap: &eve_db::WorldStateSnapshot,
    structure_id: i64,
) -> f64 {
    let nid = -(1_000_000_000_000_i64.saturating_add(structure_id.abs()));
    if let Some(note) = snap.owner_notes.get(&nid) {
        if let Ok(map) = serde_json::from_str::<HashMap<u8, f64>>(&note.note) {
            if let Some(&owner_rate) = map.get(&0) {
                if owner_rate.is_finite() && owner_rate >= 0.0 {
                    return 0.005 + owner_rate.clamp(0.0, 1.0);
                }
            }
            if let Some(&rate) = map.get(&1) {
                if rate.is_finite() && rate >= 0.0 {
                    if rate < 0.01 {
                        return 0.005 + rate;
                    }
                    return rate.clamp(0.01, 1.0);
                }
            }
        }
    }
    0.015
}

pub(crate) fn structure_owner_broker_share(
    snap: &eve_db::WorldStateSnapshot,
    structure_id: i64,
    broker_fee: IskCents,
) -> IskCents {
    if broker_fee.0 <= 0 {
        return IskCents(0);
    }
    let nid = -(1_000_000_000_000_i64.saturating_add(structure_id.abs()));
    if let Some(note) = snap.owner_notes.get(&nid) {
        if let Ok(map) = serde_json::from_str::<HashMap<u8, f64>>(&note.note) {
            if let Some(&owner_rate) = map.get(&0) {
                if owner_rate <= 0.0 {
                    return IskCents(0);
                }
                let total_rate = 0.005 + owner_rate.clamp(0.0, 1.0);
                return IskCents(((broker_fee.0 as f64) * (owner_rate / total_rate)).round() as i64);
            }
            if let Some(&rate) = map.get(&1) {
                if rate <= 0.0 {
                    return IskCents(0);
                }
            }
        }
    }
    IskCents(broker_fee.0 / 2)
}

/// Computes exact integer sales tax in ISK cents (`i128` parts-per-million rounded to nearest cent)
/// from the seller's own Accounting (`16622`) skill level (`R21-P1-4`).
pub fn compute_sales_tax_cents(fill_gross: IskCents, seller_acct_lvl: u8) -> IskCents {
    let tax_rate = compute_sales_tax_rate(seller_acct_lvl);
    let tax_ppm = (tax_rate * 1_000_000.0).round() as i128;
    IskCents((((fill_gross.0 as i128) * tax_ppm + 500_000) / 1_000_000) as i64)
}

fn order_to_packed_cells(
    snap: &eve_db::WorldStateSnapshot,
    o: &MarketOrderRecord,
    default_unix: u64,
) -> Vec<PyRep> {
    let meta = load_market_order_meta(snap, o, default_unix);
    vec![
        PyRep::Real(o.price.to_wire_f64()),
        PyRep::Real(o.vol_remaining as f64),
        PyRep::Integer(o.type_id as i64),
        PyRep::Integer(o.order_id),
        PyRep::Integer(o.vol_entered as i64),
        PyRep::Integer(o.min_volume as i64),
        PyRep::Bool(o.is_bid),
        PyRep::Integer(o.station_id),
        PyRep::Integer(o.region_id as i64),
        PyRep::Integer(meta.range),
        PyRep::Integer(unix_to_filetime(meta.issue_unix)),
        PyRep::Integer(meta.duration),
    ]
}

fn best_asks_to_summary_rows<'a>(
    orders: impl Iterator<Item = &'a MarketOrderRecord>,
) -> Vec<Vec<PyRep>> {
    let mut best_by_type: std::collections::BTreeMap<TypeId, &MarketOrderRecord> =
        std::collections::BTreeMap::new();
    for o in orders {
        match best_by_type.get(&o.type_id) {
            Some(prev) if o.price.0 >= prev.price.0 => {}
            _ => {
                best_by_type.insert(o.type_id, o);
            }
        }
    }
    best_by_type
        .into_values()
        .map(|o| {
            vec![
                PyRep::Integer(o.type_id as i64),
                PyRep::Real(o.price.to_wire_f64()),
                PyRep::Real(o.vol_remaining as f64),
                PyRep::Integer(o.station_id),
            ]
        })
        .collect()
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service != "marketProxy" {
        return None;
    }

    match method {
        "GetMarketGroups" => {
            let mut mgs: Vec<&eve_sde::MarketGroupInfo> = ctx.sde.market_groups.values().collect();
            mgs.sort_by_key(|mg| mg.market_group_id);
            let rows = mgs
                .into_iter()
                .map(|mg| {
                    vec![
                        PyRep::Integer(mg.market_group_id as i64),
                        mg.parent_group_id
                            .map(|pid| PyRep::Integer(pid as i64))
                            .unwrap_or(PyRep::None),
                        PyRep::String(mg.name.clone()),
                        PyRep::String(mg.name.clone()),
                        mg.icon_id
                            .map(|iid| PyRep::Integer(iid as i64))
                            .unwrap_or(PyRep::None),
                        PyRep::Integer(i64::from(mg.has_types)),
                    ]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec![
                    "marketGroupID",
                    "parentGroupID",
                    "marketGroupName",
                    "description",
                    "iconID",
                    "hasTypes",
                ],
                rows,
            )))
        }

        "GetMarketGroupTypes" | "GetTypesInMarketGroup" => {
            let Some(mg_id) = args.first().and_then(|a| a.as_i64()).map(|v| v as u32) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "GetMarketGroupTypes requires marketGroupID integer".into(),
                }));
            };
            let type_ids = ctx.sde.types_in_market_group(mg_id);
            let rows = type_ids
                .into_iter()
                .filter_map(|tid| {
                    let t = ctx.sde.types.get(&tid)?;
                    Some(vec![
                        PyRep::Integer(tid as i64),
                        PyRep::String(t.name.clone()),
                        PyRep::Integer(t.group_id as i64),
                        PyRep::Integer(t.category_id as i64),
                        PyRep::Real(t.volume),
                        PyRep::Real(t.base_price),
                    ])
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec![
                    "typeID",
                    "typeName",
                    "groupID",
                    "categoryID",
                    "volume",
                    "basePrice",
                ],
                rows,
            )))
        }

        "GetOrders" => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoRegion".into(),
                    message: "Session has no active region".into(),
                }));
            };
            let Some(raw_type_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketOrder".into(),
                    message: "GetOrders requires typeID".into(),
                }));
            };
            let type_id = raw_type_id as TypeId;
            let now_unix = ctx.now_unix();
            let (sell_rows, buy_rows) = ctx.db.with_snapshot(|snap| {
                let mut sells = Vec::new();
                let mut buys = Vec::new();
                let mut matching: Vec<&MarketOrderRecord> = snap
                    .market_orders
                    .values()
                    .filter(|o| {
                        o.type_id == type_id
                            && o.vol_remaining > 0
                            && o.region_id == region_id
                            && is_structure_market_online(snap, o.station_id)
                    })
                    .collect();
                matching.sort_by_key(|o| o.order_id);
                for o in matching {
                    let cells = order_to_packed_cells(snap, o, now_unix);
                    if o.is_bid {
                        buys.push(cells);
                    } else {
                        sells.push(cells);
                    }
                }
                (sells, buys)
            });
            let hdr = market_order_descriptor();
            Some(Ok(PyRep::Tuple(vec![
                hdr.build_crowset(sell_rows),
                hdr.build_crowset(buy_rows),
            ])))
        }

        "GetStationAsks" => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let Some(station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDockedAtStation".into(),
                    message: "GetStationAsks requires being docked at a station or structure"
                        .into(),
                }));
            };
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoRegion".into(),
                    message: "Session has no active region".into(),
                }));
            };
            let rows = ctx.db.with_snapshot(|snap| {
                best_asks_to_summary_rows(snap.market_orders.values().filter(|o| {
                    !o.is_bid
                        && o.vol_remaining > 0
                        && o.region_id == region_id
                        && o.station_id == station_id
                }))
            });
            Some(Ok(DBRowDescriptor::market_summary_header()
                .build_cindexed_rowset("typeID", rows)))
        }

        "GetRegionBest" => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoRegion".into(),
                    message: "Session has no active region".into(),
                }));
            };
            let rows = ctx.db.with_snapshot(|snap| {
                best_asks_to_summary_rows(
                    snap.market_orders
                        .values()
                        .filter(|o| !o.is_bid && o.vol_remaining > 0 && o.region_id == region_id),
                )
            });
            Some(Ok(DBRowDescriptor::market_summary_header()
                .build_cindexed_rowset("typeID", rows)))
        }

        "GetCharOrders" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let filter_type_id = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|&tid| tid > 0)
                .map(|tid| tid as TypeId);
            let now_unix = ctx.now_unix();
            let rows = ctx.db.with_snapshot(|snap| {
                let mut orders: Vec<&MarketOrderRecord> = snap
                    .market_orders
                    .values()
                    .filter(|o| {
                        o.owner_id == char_id
                            && !o.is_npc
                            && o.vol_remaining > 0
                            && filter_type_id.map(|tid| o.type_id == tid).unwrap_or(true)
                    })
                    .collect();
                orders.sort_by_key(|o| o.order_id);
                orders
                    .into_iter()
                    .map(|o| {
                        let meta = load_market_order_meta(snap, o, now_unix);
                        vec![
                            PyRep::Integer(o.order_id),
                            PyRep::Integer(o.type_id as i64),
                            PyRep::Bool(o.is_bid),
                            PyRep::Real(o.price.to_wire_f64()),
                            PyRep::Integer(o.vol_remaining as i64),
                            PyRep::Integer(o.station_id),
                            PyRep::Integer(meta.range),
                            PyRep::Integer(unix_to_filetime(meta.issue_unix)),
                            PyRep::Integer(meta.duration),
                            PyRep::Integer(o.min_volume as i64),
                            PyRep::Real(o.escrow.to_wire_f64()),
                        ]
                    })
                    .collect()
            });
            Some(Ok(PyRep::rowset(
                vec![
                    "orderID",
                    "typeID",
                    "bid",
                    "price",
                    "volRemaining",
                    "stationID",
                    "range",
                    "issueDate",
                    "duration",
                    "minVolume",
                    "escrow",
                ],
                rows,
            )))
        }

        "GetCharEscrow" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let (total_escrow, total_remaining_cost, rows) = ctx.db.with_snapshot(|snap| {
                let mut esc_sum = IskCents(0);
                let mut full_sum = IskCents(0);
                let mut order_rows = Vec::new();
                let mut orders: Vec<&MarketOrderRecord> = snap
                    .market_orders
                    .values()
                    .filter(|o| {
                        o.owner_id == char_id && !o.is_npc && o.is_bid && o.vol_remaining > 0
                    })
                    .collect();
                orders.sort_by_key(|o| o.order_id);
                for o in orders {
                    let full_cost = o
                        .price
                        .checked_mul_qty(o.vol_remaining as i64)
                        .unwrap_or(IskCents(0));
                    esc_sum = esc_sum.checked_add(o.escrow).unwrap_or(esc_sum);
                    full_sum = full_sum.checked_add(full_cost).unwrap_or(full_sum);
                    order_rows.push(PyRep::keyval([
                        ("orderID", PyRep::Integer(o.order_id)),
                        ("typeID", PyRep::Integer(o.type_id as i64)),
                        ("stationID", PyRep::Integer(o.station_id)),
                        ("escrow", PyRep::Real(o.escrow.to_wire_f64())),
                        ("escrowISK", PyRep::Real(o.escrow.to_wire_f64())),
                        ("remainingCostISK", PyRep::Real(full_cost.to_wire_f64())),
                    ]));
                }
                (esc_sum, full_sum, order_rows)
            });
            let uncovered = total_remaining_cost
                .checked_sub(total_escrow)
                .unwrap_or(IskCents(0));
            Some(Ok(PyRep::keyval([
                ("escrow", PyRep::Real(total_escrow.to_wire_f64())),
                ("totalEscrow", PyRep::Real(total_escrow.to_wire_f64())),
                ("totalEscrowISK", PyRep::Real(total_escrow.to_wire_f64())),
                (
                    "totalRemainingCostISK",
                    PyRep::Real(total_remaining_cost.to_wire_f64()),
                ),
                ("additionalToCover", PyRep::Real(uncovered.to_wire_f64())),
                ("additionalToCoverISK", PyRep::Real(uncovered.to_wire_f64())),
                ("orders", PyRep::List(rows)),
            ])))
        }

        "PlaceBuyOrder" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(docked_station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDockedAtStation".into(),
                    message: "Character must be docked at a station or structure to place a market order"
                        .into(),
                }));
            };
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoRegion".into(),
                    message: "Session has no active region".into(),
                }));
            };
            let station_id = args
                .first()
                .and_then(|a| a.as_i64())
                .unwrap_or(docked_station_id);
            let Some(raw_type_id) = args.get(1).and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketOrder".into(),
                    message: "Market order placement requires typeID".into(),
                }));
            };
            let type_id = raw_type_id as TypeId;
            let price_isk = args.get(2).and_then(|a| a.as_f64()).unwrap_or(0.0);
            let quantity = args.get(3).and_then(|a| a.as_i64()).unwrap_or(0);
            let is_bid = true;
            let is_v24_buy_sig = matches!(args.get(6), Some(PyRep::Integer(_)));
            let order_range = if is_v24_buy_sig {
                args.get(4).and_then(|a| a.as_i64()).unwrap_or(-1)
            } else {
                _kwargs
                    .get("orderRange")
                    .or_else(|| _kwargs.get("range"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(-1)
            };
            let min_volume_i32 = args
                .get(5)
                .and_then(|a| a.as_i64())
                .unwrap_or(1)
                .max(1) as i32;
            let duration_days = if is_v24_buy_sig {
                args.get(6).and_then(|a| a.as_i64()).unwrap_or(90)
            } else {
                args.get(4).and_then(|a| a.as_i64()).unwrap_or(90)
            };

            let use_corp = args
                .get(7)
                .and_then(|a| a.as_bool())
                .or_else(|| _kwargs.get("useCorp").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            let account_key = args
                .get(8)
                .and_then(|a| a.as_i64())
                .or_else(|| _kwargs.get("accountKey").and_then(|v| v.as_i64()))
                .unwrap_or(1000)
                .clamp(1000, 1006) as u16;

            if !price_isk.is_finite()
                || price_isk <= 0.0
                || quantity <= 0
                || quantity > (i32::MAX as i64)
                || ![0, 1, 3, 7, 14, 30, 90].contains(&duration_days)
                || !matches!(order_range, -1 | 0 | 1..=40 | 32767)
            {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketOrder".into(),
                    message: "Market order requires finite price > 0, positive quantity, valid range, and duration in [0, 1, 3, 7, 14, 30, 90]"
                        .into(),
                }));
            }
            if !super::market_contracts::is_valid_4_sig_fig_price(price_isk) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidPriceTickSize".into(),
                    message: format!(
                        "Order price {price_isk} violates EVE 4-significant-figure tick size rule"
                    ),
                }));
            }
            let quantity_i32 = quantity as i32;
            if station_id != docked_station_id {
                return Some(Err(BridgeRpcError {
                    code: "StationMismatch".into(),
                    message: format!(
                        "Cannot place market order at station {station_id} while docked at {docked_station_id}"
                    ),
                }));
            }
            let station_info = ctx.sde.solar_systems.values().find_map(|sys| {
                if sys.region_id == region_id {
                    sys.stations.iter().find(|st| st.station_id == station_id)
                } else {
                    None
                }
            });
            let (structure_info, market_hub_online) = ctx.db.with_snapshot(|snap| {
                let info = snap
                    .structures
                    .get(&station_id)
                    .filter(|st| st.state != "Destroyed")
                    .map(|st| (st.owner_corp_id, resolve_structure_broker_rate(snap, station_id)));
                (info, is_structure_market_online(snap, station_id))
            });
            if structure_info.is_some() && !market_hub_online {
                return Some(Err(BridgeRpcError {
                    code: "MarketHubOffline".into(),
                    message: format!("Standup Market Hub I service module at structure {station_id} is offline"),
                }));
            }
            let (owner_corp_id, owner_faction_id, structure_broker_opt) =
                if let Some(station_def) = station_info {
                    (
                        station_def.owner_corporation_id,
                        station_def.owner_faction_id,
                        None,
                    )
                } else if let Some((st_corp_id, st_rate)) = structure_info {
                    (st_corp_id, None, Some((st_corp_id, st_rate)))
                } else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidMarketStation".into(),
                        message: format!("Station {station_id} is not in region {region_id}"),
                    }));
                };

            let (broker_lvl, _acct_lvl, mt_lvl, vis_lvl, faction_standing, corp_standing, max_open_orders) =
                ctx.db.with_snapshot(|snap| {
                    let sk = snap.skills.get(&char_id);
                    let b_lvl = sk.and_then(|m| m.get(&3446)).map(|s| s.level).unwrap_or(0);
                    let a_lvl = sk.and_then(|m| m.get(&16622)).map(|s| s.level).unwrap_or(0);
                    let m_lvl = sk.and_then(|m| m.get(&16597)).map(|s| s.level).unwrap_or(0);
                    let v_lvl = sk
                        .and_then(|m| m.get(&3447))
                        .map(|s| s.level)
                        .unwrap_or_else(|| if sk.is_some_and(|m| !m.is_empty()) { 5 } else { 0 });
                    let trade_lvl = sk.and_then(|m| m.get(&3443)).map(|s| s.level).unwrap_or(0) as usize;
                    let retail_lvl = sk.and_then(|m| m.get(&3444)).map(|s| s.level).unwrap_or(0) as usize;
                    let wholesale_lvl = sk.and_then(|m| m.get(&16596)).map(|s| s.level).unwrap_or(0) as usize;
                    let tycoon_lvl = sk.and_then(|m| m.get(&18580)).map(|s| s.level).unwrap_or(0) as usize;
                    let order_limit = 5 + 4 * trade_lvl + 8 * retail_lvl + 16 * wholesale_lvl + 32 * tycoon_lvl;
                    let ch = snap.characters.get(&char_id);
                    let f_st = ch
                        .and_then(|c| owner_faction_id.and_then(|fid| c.standings.get(&fid).copied()))
                        .unwrap_or(0.0);
                    let c_st = ch
                        .and_then(|c| c.standings.get(&owner_corp_id).copied())
                        .unwrap_or(0.0);
                    (b_lvl, a_lvl, m_lvl, v_lvl, f_st, c_st, order_limit)
                });
            if type_id != PLEX_TYPE_ID
                && duration_days > 0
                && order_range > super::market_contracts::max_remote_jumps_for_skill_level(vis_lvl)
            {
                return Some(Err(BridgeRpcError {
                    code: "VisibilitySkillRequired".into(),
                    message: format!(
                        "Buy order range {order_range} exceeds maximum range {} for Visibility skill level {vis_lvl}",
                        super::market_contracts::max_remote_jumps_for_skill_level(vis_lvl)
                    ),
                }));
            }
            let broker_rate = if let Some((_, st_rate)) = structure_broker_opt {
                st_rate
            } else {
                compute_broker_fee_rate(broker_lvl, faction_standing, corp_standing)
            };
            let order_price = IskCents::from_f64_isk(price_isk);
            let Some(max_order_cost) = order_price.checked_mul_qty(quantity) else {
                return Some(Err(BridgeRpcError {
                    code: "MarketOrderOverflow".into(),
                    message: "Order total cost overflows 64-bit ISK cents".into(),
                }));
            };
            let broker_fee = if duration_days > 0 {
                let broker_ppm = (broker_rate * 1_000_000.0).round() as i128;
                IskCents((((max_order_cost.0 as i128) * broker_ppm + 500_000) / 1_000_000) as i64)
                    .max(IskCents(10_000))
            } else {
                IskCents(0)
            };
            let now_unix = ctx.now_unix();
            let tax_collector = crate::services::wallet_serverops::tax_collector_id(ctx);

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let caller_corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                let div_id = (account_key.saturating_sub(999)).clamp(1, 7) as u8;
                if use_corp {
                    let Some(corp) = snap.corporations.get_mut(&caller_corp_id) else {
                        return Err(BridgeRpcError {
                            code: "CorporationNotFound".into(),
                            message: format!("Corporation {caller_corp_id} not found for corporate buy order"),
                        });
                    };
                    let role_mask = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                    if corp.ceo_id != char_id
                        && !eve_db::corp_roles::has_role(role_mask, eve_db::corp_roles::ROLE_ACCOUNTANT)
                        && !eve_db::corp_roles::has_role(role_mask, eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT)
                        && !eve_db::corp_roles::has_role(role_mask, 1u64 << 20) // corpRoleTrader
                    {
                        return Err(BridgeRpcError {
                            code: "CorpRoleRequired".into(),
                            message: "Character lacks corporation trader/accountant role to place corporate buy order".into(),
                        });
                    }
                    corp.ensure_wallet_divisions();
                }
                let order_owner_id = if use_corp { caller_corp_id } else { char_id };

                let escrow_fraction = if mt_lvl > 0 {
                    0.75_f64.powi(mt_lvl.min(5) as i32)
                } else {
                    1.0
                };
                let min_escrow_upfront = IskCents(((max_order_cost.0 as f64) * escrow_fraction).round() as i64);
                let required_upfront = if is_bid {
                    min_escrow_upfront.checked_add(broker_fee).ok_or_else(|| {
                        BridgeRpcError {
                            code: "MarketOrderOverflow".into(),
                            message: "Order cost + broker fee overflow".into(),
                        }
                    })?
                } else {
                    broker_fee
                };

                let caller_balance = if use_corp {
                    snap.corporations
                        .get(&caller_corp_id)
                        .and_then(|c| c.wallet_divisions.iter().find(|d| d.division_id == div_id).map(|d| d.balance.min(c.master_wallet)))
                        .unwrap_or(IskCents(0))
                } else {
                    snap.characters
                        .get(&char_id)
                        .map(|c| c.balance)
                        .unwrap_or(IskCents(0))
                };
                if caller_balance.checked_sub(required_upfront).is_none() {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Not enough ISK for market order and broker fee".into(),
                    });
                }

                // Collect matching opposite orders sorted by best price-time priority
                let mut candidate_ids: Vec<(i64, i64)> = snap
                    .market_orders
                    .values()
                    .filter(|o| {
                        o.type_id == type_id
                            // PLEX trades on a single global market (`feat:market.plex-market`).
                            && (type_id == PLEX_TYPE_ID || o.station_id == station_id)
                            && o.is_bid != is_bid
                            && o.vol_remaining > 0
                            && ((is_bid && o.price.0 <= order_price.0)
                                || (!is_bid && o.price.0 >= order_price.0))
                    })
                    .map(|o| (o.order_id, o.price.0))
                    .collect();

                if is_bid {
                    candidate_ids.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
                } else {
                    candidate_ids.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
                }

                // Debit broker fee upfront
                if use_corp {
                    if let Some(corp) = snap.corporations.get_mut(&caller_corp_id) {
                        corp.master_wallet = corp.master_wallet.checked_sub(broker_fee).unwrap_or(IskCents(0));
                        if let Some(d) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == div_id) {
                            d.balance = d.balance.checked_sub(broker_fee).unwrap_or(IskCents(0));
                        }
                        let bal_after = corp.master_wallet;
                        snap.append_journal(
                            caller_corp_id,
                            "MarketBrokerFee",
                            broker_fee,
                            bal_after,
                            format!("Broker fee for corporate order on type {type_id}"),
                        );
                    }
                } else if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = ch.balance.checked_sub(broker_fee).unwrap_or(IskCents(0));
                    let bal_after = ch.balance;
                    snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                        owner_id: char_id,
                        entry_type: "MarketBrokerFee",
                        amount: IskCents(-broker_fee.0),
                        balance_after: bal_after,
                        reason: format!("Broker fee for order on type {type_id}"),
                        meta: eve_db::ledger::JournalMeta::new(
                            eve_db::ledger::ref_types::BROKER_FEE,
                            char_id,
                            owner_corp_id,
                            now_unix,
                        ),
                    });
                }
                if let Some((st_corp_id, _)) = structure_broker_opt {
                    if broker_fee.0 > 0 {
                        let owner_share = structure_owner_broker_share(snap, station_id, broker_fee);
                        if owner_share.0 > 0 {
                            if let Some(corp_mut) = snap.corporations.get_mut(&st_corp_id) {
                                corp_mut.master_wallet = corp_mut
                                    .master_wallet
                                    .checked_add(owner_share)
                                    .unwrap_or(corp_mut.master_wallet);
                                let c_bal = corp_mut.master_wallet;
                                snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                                    owner_id: st_corp_id,
                                    entry_type: "MarketBrokerFee",
                                    amount: owner_share,
                                    balance_after: c_bal,
                                    reason: format!("Structure market broker fee for order on type {type_id}"),
                                    meta: eve_db::ledger::JournalMeta::new(
                                        eve_db::ledger::ref_types::BROKER_FEE,
                                        order_owner_id,
                                        st_corp_id,
                                        now_unix,
                                    ),
                                });
                            }
                        }
                    }
                }

                let mut remaining_qty = quantity_i32;
                let mut first_matched_oid = None;
                let mut actual_sales_tax_charged = IskCents(0);
                let mut matched_sellers: Vec<(i64, i64)> = Vec::new();
                let mut delivered_items: Vec<(i64, TypeId, i64, i64)> = Vec::new();

                for (oid, _) in candidate_ids {
                    if remaining_qty <= 0 {
                        break;
                    }
                    let Some(resting) = snap.market_orders.get(&oid).cloned() else {
                        continue;
                    };
                    let fill_qty = remaining_qty.min(resting.vol_remaining);
                    if fill_qty <= 0 {
                        continue;
                    }
                    let Some(fill_gross) = resting.price.checked_mul_qty(fill_qty as i64) else {
                        continue;
                    };

                    if is_bid {
                        let cur_bal = if use_corp {
                            snap.corporations
                                .get(&caller_corp_id)
                                .and_then(|c| c.wallet_divisions.iter().find(|d| d.division_id == div_id).map(|d| d.balance.min(c.master_wallet)))
                                .unwrap_or(IskCents(0))
                        } else {
                            snap.characters
                                .get(&char_id)
                                .map(|c| c.balance)
                                .unwrap_or(IskCents(0))
                        };
                        if cur_bal.checked_sub(fill_gross).is_none() {
                            return Err(BridgeRpcError {
                                code: "InsufficientFunds".into(),
                                message: "Not enough ISK to complete immediate buy fill".into(),
                            });
                        }
                        let buyer_bal = if use_corp {
                            snap.corporations.get_mut(&caller_corp_id).map(|corp| {
                                corp.master_wallet = corp.master_wallet.checked_sub(fill_gross).unwrap_or(IskCents(0));
                                if let Some(d) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == div_id) {
                                    d.balance = d.balance.checked_sub(fill_gross).unwrap_or(IskCents(0));
                                }
                                corp.master_wallet
                            })
                        } else {
                            snap.characters.get_mut(&char_id).map(|buyer| {
                                buyer.balance = buyer.balance.checked_sub(fill_gross).unwrap_or(IskCents(0));
                                buyer.balance
                            })
                        };
                        let (mut seller_net, mut seller_bal, mut fill_tax) = (IskCents(0), None, IskCents(0));
                        if !resting.is_npc {
                            matched_sellers.push((oid, resting.owner_id));
                            let seller_meta = load_market_order_meta(snap, &resting, now_unix);
                            let seller_acct_lvl = snap
                                .skills
                                .get(&resting.owner_id)
                                .and_then(|m| m.get(&16622))
                                .map(|s| s.level)
                                .unwrap_or(0);
                            fill_tax = compute_sales_tax_cents(fill_gross, seller_acct_lvl);
                            actual_sales_tax_charged = actual_sales_tax_charged
                                .checked_add(fill_tax)
                                .unwrap_or(actual_sales_tax_charged);
                            seller_net = fill_gross.checked_sub(fill_tax).unwrap_or(IskCents(0));
                            if let Some(seller) = snap.characters.get_mut(&resting.owner_id) {
                                seller.balance = seller.balance.checked_add(seller_net).unwrap_or(seller.balance);
                                seller_bal = Some(seller.balance);
                            } else if let Some(seller_corp) = snap.corporations.get_mut(&resting.owner_id) {
                                seller_corp.master_wallet = seller_corp.master_wallet.checked_add(seller_net).unwrap_or(seller_corp.master_wallet);
                                let s_div = (seller_meta.account_key.unwrap_or(1000).saturating_sub(999)).clamp(1, 7) as u8;
                                seller_corp.ensure_wallet_divisions();
                                if let Some(d) = seller_corp.wallet_divisions.iter_mut().find(|d| d.division_id == s_div) {
                                    d.balance = d.balance.checked_add(seller_net).unwrap_or(d.balance);
                                }
                                seller_bal = Some(seller_corp.master_wallet);
                            }
                        }
                        snap.journal_market_fill(eve_db::ledger::MarketFill {
                            buyer_id: order_owner_id,
                            seller_id: resting.owner_id,
                            type_id,
                            quantity: fill_qty as i64,
                            price: resting.price,
                            location_id: station_id,
                            date_unix: now_unix,
                            gross: fill_gross,
                            buyer_balance_after: buyer_bal,
                            seller_net,
                            seller_balance_after: seller_bal,
                            sales_tax: fill_tax,
                            tax_collector_id: tax_collector,
                        });
                        // PLEX trades settle into the account-wide PLEX Vault instead of the hangar.
                        if type_id == PLEX_TYPE_ID {
                            let _ = snap.adjust_plex_vault(char_id, fill_qty as i64);
                        }
                        if type_id != PLEX_TYPE_ID {
                            snap.next_entity_id += 1;
                            let new_item_id = snap.next_entity_id;
                            let deliv_flag = if use_corp {
                                InventoryFlag::Deliveries
                            } else {
                                InventoryFlag::Hangar
                            };
                            snap.items.insert(
                                new_item_id,
                                InventoryItemRecord {
                                    item_id: new_item_id,
                                    type_id,
                                    owner_id: order_owner_id,
                                    location_id: station_id,
                                    flag: deliv_flag,
                                    quantity: fill_qty,
                                    singleton: false,
                                    version: 1,
                                    ..Default::default()
                                },
                            );
                            delivered_items.push((new_item_id, type_id, deliv_flag as i64, station_id));
                        }
                    }

                    let mut resting_completed = false;
                    if let Some(o_mut) = snap.market_orders.get_mut(&oid) {
                        // NPC-seeded skillbook/blueprint sell orders (SDE categoryID 16 = Skill,
                        // 9 = Blueprint; same rule as `world_seeder`) are infinite NPC stock.
                        let is_npc_skill_or_bp = o_mut.is_npc
                            && !o_mut.is_bid
                            && ctx
                                .sde
                                .types
                                .get(&o_mut.type_id)
                                .map(|t| t.category_id == 16 || t.category_id == 9)
                                .unwrap_or(false);
                        if is_npc_skill_or_bp {
                            o_mut.vol_remaining = o_mut.vol_entered;
                        } else {
                            o_mut.vol_remaining -= fill_qty;
                            if o_mut.vol_remaining == 0 && !o_mut.is_npc {
                                resting_completed = true;
                            }
                        }
                        if o_mut.is_bid {
                            o_mut.escrow =
                                o_mut.escrow.checked_sub(fill_gross).unwrap_or(IskCents(0));
                        }
                    }
                    if resting_completed {
                        let mut r_meta = load_market_order_meta(snap, &resting, now_unix);
                        r_meta.order_state = Some(2);
                        r_meta.final_vol_remaining = Some(0);
                        r_meta.closed_unix = Some(now_unix);
                        save_market_order_meta(snap, oid, r_meta);
                    }
                    remaining_qty -= fill_qty;
                    if first_matched_oid.is_none() {
                        first_matched_oid = Some(oid);
                    }
                }

                if remaining_qty == 0 {
                    return Ok((
                        first_matched_oid.unwrap_or(0),
                        true,
                        actual_sales_tax_charged,
                        IskCents(0),
                        matched_sellers,
                        delivered_items,
                    ));
                }

                if duration_days == 0 {
                    return Err(BridgeRpcError {
                        code: "ImmediateOrderUnfilled".into(),
                        message: "Immediate order (duration=0) could not be completely filled against existing orders".into(),
                    });
                }

                let active_open_orders = snap
                    .market_orders
                    .values()
                    .filter(|o| o.owner_id == order_owner_id && !o.is_npc && o.vol_remaining > 0)
                    .count();
                if active_open_orders >= max_open_orders {
                    return Err(BridgeRpcError {
                        code: "MaxOpenOrdersReached".into(),
                        message: format!(
                            "Character has reached maximum open market orders ({active_open_orders}/{max_open_orders})"
                        ),
                    });
                }

                let full_esc = order_price
                    .checked_mul_qty(remaining_qty as i64)
                    .unwrap_or(IskCents(0));
                let escrow = if mt_lvl > 0 {
                    IskCents(((full_esc.0 as f64) * escrow_fraction).round() as i64)
                } else {
                    full_esc
                };
                let cur_bal = if use_corp {
                    snap.corporations
                        .get(&caller_corp_id)
                        .and_then(|c| c.wallet_divisions.iter().find(|d| d.division_id == div_id).map(|d| d.balance.min(c.master_wallet)))
                        .unwrap_or(IskCents(0))
                } else {
                    snap.characters
                        .get(&char_id)
                        .map(|c| c.balance)
                        .unwrap_or(IskCents(0))
                };
                if cur_bal.checked_sub(escrow).is_none() {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Not enough ISK to cover buy order escrow".into(),
                    });
                }
                let escrow_bal = if use_corp {
                    snap.corporations.get_mut(&caller_corp_id).map(|corp| {
                        corp.master_wallet = corp.master_wallet.checked_sub(escrow).unwrap_or(IskCents(0));
                        if let Some(d) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == div_id) {
                            d.balance = d.balance.checked_sub(escrow).unwrap_or(IskCents(0));
                        }
                        corp.master_wallet
                    })
                } else {
                    snap.characters.get_mut(&char_id).map(|buyer| {
                        buyer.balance = buyer.balance.checked_sub(escrow).unwrap_or(IskCents(0));
                        buyer.balance
                    })
                };

                snap.next_entity_id += 1;
                let new_oid = snap.next_entity_id;
                let eff_min_vol = min_volume_i32.min(remaining_qty).max(1);
                snap.market_orders.insert(
                    new_oid,
                    MarketOrderRecord {
                        order_id: new_oid,
                        owner_id: order_owner_id,
                        station_id,
                        region_id,
                        type_id,
                        is_bid,
                        price: order_price,
                        vol_entered: quantity_i32,
                        vol_remaining: remaining_qty,
                        min_volume: eff_min_vol,
                        is_npc: false,
                        escrow,
                    },
                );
                save_market_order_meta(
                    snap,
                    new_oid,
                    MarketOrderMeta {
                        range: order_range,
                        duration: duration_days,
                        issue_unix: now_unix,
                        order_state: None,
                        account_key: if use_corp { Some(account_key) } else { None },
                        final_vol_remaining: None,
                        closed_unix: None,
                    },
                );
                if let (Some(bal), true) = (escrow_bal, escrow.0 > 0) {
                    snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                        owner_id: order_owner_id,
                        entry_type: "MarketEscrow",
                        amount: IskCents(-escrow.0),
                        balance_after: bal,
                        reason: format!("Escrow for buy order {new_oid}"),
                        meta: eve_db::ledger::JournalMeta::new(eve_db::ledger::ref_types::MARKET_ESCROW, order_owner_id, 0, now_unix)
                            .with_context(eve_db::ledger::JournalContext::MarketOrder, new_oid),
                    });
                }
                Ok((new_oid, false, actual_sales_tax_charged, escrow, matched_sellers, delivered_items))
            });

            match res {
                Ok((order_id, filled_immediately, actual_sales_tax_charged, escrow_cents, matched_sellers, delivered_items)) => {
                    for (matched_oid, seller_id) in matched_sellers {
                        ctx.notify(
                            NotificationTarget::Character(seller_id),
                            "OnOwnOrdersChanged",
                            "charid",
                            vec![
                                PyRep::Integer(matched_oid),
                                PyRep::String("Change".into()),
                                PyRep::Bool(false),
                            ],
                        );
                    }
                    for (iid, tid, flag_id, loc_id) in delivered_items {
                        ctx.notify(
                            NotificationTarget::Character(char_id),
                            "OnMarketItemReceived",
                            "charid",
                            vec![
                                PyRep::Integer(iid),
                                PyRep::Integer(tid as i64),
                                PyRep::Integer(flag_id),
                                PyRep::Integer(loc_id),
                            ],
                        );
                    }
                    ctx.notify(
                        NotificationTarget::Character(char_id),
                        "OnOwnOrdersChanged",
                        "charid",
                        vec![
                            PyRep::Integer(order_id),
                            PyRep::String(if filled_immediately { "Change" } else { "Add" }.into()),
                            PyRep::Bool(false),
                        ],
                    );
                    Some(Ok(PyRep::keyval([
                        ("orderID", PyRep::Integer(order_id)),
                        ("filledImmediately", PyRep::Bool(filled_immediately)),
                        ("brokerFeeISK", PyRep::Real(broker_fee.to_wire_f64())),
                        (
                            "salesTaxISK",
                            PyRep::Real(actual_sales_tax_charged.to_wire_f64()),
                        ),
                        ("escrowISK", PyRep::Real(escrow_cents.to_wire_f64())),
                        ("range", PyRep::Integer(order_range)),
                        ("orderRange", PyRep::Integer(order_range)),
                        ("duration", PyRep::Integer(duration_days)),
                        ("minVolume", PyRep::Integer(min_volume_i32 as i64)),
                        ("issueDate", PyRep::Integer(unix_to_filetime(now_unix))),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        _ => market_orders_escrow::handle_orders_escrow(ctx, session, service, method, args, _kwargs),
    }
}
