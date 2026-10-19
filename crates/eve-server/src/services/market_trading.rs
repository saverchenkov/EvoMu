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

use crate::formulas::{compute_broker_fee_rate, compute_sales_tax_rate};
use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, DbColumnType, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
struct MarketOrderMeta {
    range: i64,
    duration: i64,
    issue_unix: u64,
}

fn market_order_meta_note_id(order_id: i64) -> i64 {
    -(4_000_000_000_000_i64.saturating_add(order_id.abs()))
}

fn save_market_order_meta(
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

fn load_market_order_meta(
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
        duration: 90,
        issue_unix: default_unix,
    }
}

fn unix_to_filetime(unix_secs: u64) -> i64 {
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

fn resolve_station_solar_system(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    station_id: i64,
) -> Option<u32> {
    if let Some(st) = snap.structures.get(&station_id) {
        return Some(st.solar_system_id);
    }
    for sys in ctx.sde.solar_systems.values() {
        if sys.stations.iter().any(|s| s.station_id == station_id) {
            return Some(sys.solar_system_id);
        }
    }
    None
}

fn can_bid_match_seller_station(
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
    let bid_const = ctx
        .sde
        .solar_systems
        .get(&bid_sys)
        .map(|s| s.constellation_id);
    let sell_const = ctx
        .sde
        .solar_systems
        .get(&sell_sys)
        .map(|s| s.constellation_id);
    let approx_jumps = if bid_const.is_some() && bid_const == sell_const {
        1
    } else {
        2
    };
    approx_jumps <= meta.range
}

fn resolve_structure_broker_rate(snap: &eve_db::WorldStateSnapshot, structure_id: i64) -> f64 {
    let nid = -(1_000_000_000_000_i64.saturating_add(structure_id.abs()));
    if let Some(note) = snap.owner_notes.get(&nid) {
        if let Ok(map) = serde_json::from_str::<HashMap<u8, f64>>(&note.note) {
            if let Some(&rate) = map.get(&1) {
                if rate.is_finite() && rate >= 0.0 {
                    return rate.clamp(0.01, 1.0);
                }
            }
        }
    }
    0.015
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
                        o.type_id == type_id && o.vol_remaining > 0 && o.region_id == region_id
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
            let structure_info = ctx.db.with_snapshot(|snap| {
                snap.structures
                    .get(&station_id)
                    .filter(|st| st.state != "Destroyed")
                    .map(|st| (st.owner_corp_id, resolve_structure_broker_rate(snap, station_id)))
            });
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

            let (broker_lvl, _acct_lvl, mt_lvl, faction_standing, corp_standing, max_open_orders) =
                ctx.db.with_snapshot(|snap| {
                    let sk = snap.skills.get(&char_id);
                    let b_lvl = sk.and_then(|m| m.get(&3446)).map(|s| s.level).unwrap_or(0);
                    let a_lvl = sk.and_then(|m| m.get(&16622)).map(|s| s.level).unwrap_or(0);
                    let m_lvl = sk.and_then(|m| m.get(&16597)).map(|s| s.level).unwrap_or(0);
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
                    // NPC station broker fee uses raw unmodified standings (never Connections/Diplomacy)
                    (b_lvl, a_lvl, m_lvl, f_st, c_st, order_limit)
                });
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
            } else {
                IskCents(0)
            };
            let now_unix = ctx.now_unix();

            let res = ctx.db.try_with_mut_snapshot(|snap| {
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

                let caller_balance = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.balance)
                    .unwrap_or(IskCents(0));
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
                            && o.station_id == station_id
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
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.balance = ch.balance.checked_sub(broker_fee).unwrap_or(IskCents(0));
                    let bal_after = ch.balance;
                    snap.append_journal(
                        char_id,
                        "MarketBrokerFee",
                        broker_fee,
                        bal_after,
                        format!("Broker fee for order on type {type_id}"),
                    );
                }
                if let Some((st_corp_id, _)) = structure_broker_opt {
                    if broker_fee.0 > 0 {
                        let owner_share = IskCents(broker_fee.0 / 2);
                        if owner_share.0 > 0 {
                            if let Some(corp_mut) = snap.corporations.get_mut(&st_corp_id) {
                                corp_mut.master_wallet = corp_mut
                                    .master_wallet
                                    .checked_add(owner_share)
                                    .unwrap_or(corp_mut.master_wallet);
                                let c_bal = corp_mut.master_wallet;
                                snap.append_journal(
                                    st_corp_id,
                                    "MarketBrokerFee",
                                    owner_share,
                                    c_bal,
                                    format!("Structure market broker fee for order on type {type_id}"),
                                );
                            }
                        }
                    }
                }

                let mut remaining_qty = quantity_i32;
                let mut first_matched_oid = None;
                let mut actual_sales_tax_charged = IskCents(0);

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
                        let cur_bal = snap
                            .characters
                            .get(&char_id)
                            .map(|c| c.balance)
                            .unwrap_or(IskCents(0));
                        if cur_bal.checked_sub(fill_gross).is_none() {
                            return Err(BridgeRpcError {
                                code: "InsufficientFunds".into(),
                                message: "Not enough ISK to complete immediate buy fill".into(),
                            });
                        }
                        if let Some(buyer) = snap.characters.get_mut(&char_id) {
                            buyer.balance = buyer
                                .balance
                                .checked_sub(fill_gross)
                                .unwrap_or(IskCents(0));
                            let bal_after = buyer.balance;
                            snap.append_journal(
                                char_id,
                                "MarketTransaction",
                                fill_gross,
                                bal_after,
                                format!("Bought {fill_qty}x type {type_id}"),
                            );
                        }
                        if !resting.is_npc {
                            let seller_acct_lvl = snap
                                .skills
                                .get(&resting.owner_id)
                                .and_then(|m| m.get(&16622))
                                .map(|s| s.level)
                                .unwrap_or(0);
                            let fill_tax = compute_sales_tax_cents(fill_gross, seller_acct_lvl);
                            actual_sales_tax_charged = actual_sales_tax_charged
                                .checked_add(fill_tax)
                                .unwrap_or(actual_sales_tax_charged);
                            if let Some(seller) = snap.characters.get_mut(&resting.owner_id) {
                                let seller_net = fill_gross
                                    .checked_sub(fill_tax)
                                    .unwrap_or(IskCents(0));
                                seller.balance = seller
                                    .balance
                                    .checked_add(seller_net)
                                    .unwrap_or(seller.balance);
                                let s_bal_after = seller.balance;
                                snap.append_journal(
                                    resting.owner_id,
                                    "MarketSell",
                                    seller_net,
                                    s_bal_after,
                                    format!("Sold {fill_qty}x type {type_id}"),
                                );
                                if fill_tax.0 > 0 {
                                    snap.append_journal(
                                        resting.owner_id,
                                        "TransactionTax",
                                        fill_tax,
                                        s_bal_after,
                                        format!("Sales tax for selling {fill_qty}x type {type_id}"),
                                    );
                                }
                            }
                        }
                        snap.next_entity_id += 1;
                        let new_item_id = snap.next_entity_id;
                        snap.items.insert(
                            new_item_id,
                            InventoryItemRecord {
                                item_id: new_item_id,
                                type_id,
                                owner_id: char_id,
                                location_id: station_id,
                                flag: InventoryFlag::Hangar,
                                quantity: fill_qty,
                                singleton: false,
                                version: 1,
                                ..Default::default()
                            },
                        );
                    }

                    if let Some(o_mut) = snap.market_orders.get_mut(&oid) {
                        o_mut.vol_remaining -= fill_qty;
                        if o_mut.is_bid {
                            o_mut.escrow =
                                o_mut.escrow.checked_sub(fill_gross).unwrap_or(IskCents(0));
                        }
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
                    .filter(|o| o.owner_id == char_id && !o.is_npc && o.vol_remaining > 0)
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
                let cur_bal = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.balance)
                    .unwrap_or(IskCents(0));
                if cur_bal.checked_sub(escrow).is_none() {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: "Not enough ISK to cover buy order escrow".into(),
                    });
                }
                if let Some(buyer) = snap.characters.get_mut(&char_id) {
                    buyer.balance = buyer.balance.checked_sub(escrow).unwrap_or(IskCents(0));
                }

                snap.next_entity_id += 1;
                let new_oid = snap.next_entity_id;
                let eff_min_vol = min_volume_i32.min(remaining_qty).max(1);
                snap.market_orders.insert(
                    new_oid,
                    MarketOrderRecord {
                        order_id: new_oid,
                        owner_id: char_id,
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
                    },
                );
                Ok((new_oid, false, actual_sales_tax_charged, escrow))
            });

            match res {
                Ok((order_id, filled_immediately, actual_sales_tax_charged, escrow_cents)) => {
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

        // V24.01 `code.ccp` `eve/client/script/ui/services/marketsvc.pyj` (`SellMulti`) and
        // `eve/client/script/ui/shared/market/sellMulti.pyj` (`GetValidatedItem`):
        // `marketProxy.PlaceMultiSellOrder(itemList, useCorp, duration, expectedBrokersFee)`
        // where each entry in `itemList` is a KeyVal/dict with `(stationID, typeID, itemID, price, quantity[, officeID, delta, rawBrokerFeePercentage])`.
        "PlaceMultiSellOrder" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(docked_station_id) = session.station_id.or(session.structure_id) else {
                return Some(Err(BridgeRpcError {
                    code: "NotDockedAtStation".into(),
                    message: "Character must be docked at a station or structure to PlaceMultiSellOrder".into(),
                }));
            };
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoRegion".into(),
                    message: "Session has no active region".into(),
                }));
            };
            let Some(PyRep::List(entries) | PyRep::Tuple(entries)) = args.first() else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMultiSellOrder".into(),
                    message: "PlaceMultiSellOrder requires itemList as first argument".into(),
                }));
            };
            if entries.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMultiSellOrder".into(),
                    message: "PlaceMultiSellOrder requires at least one item entry".into(),
                }));
            }
            let use_corp = args.get(1).and_then(|a| a.as_bool()).unwrap_or(false);
            let duration_days = args.get(2).and_then(|a| a.as_i64()).unwrap_or(90);
            if ![0, 1, 3, 7, 14, 30, 90].contains(&duration_days) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketOrder".into(),
                    message: format!(
                        "Invalid market order duration {duration_days}; expected one of [0, 1, 3, 7, 14, 30, 90]"
                    ),
                }));
            }
            let expected_brokers_fee = match args.get(3) {
                Some(PyRep::None) | None => None,
                Some(v) => match v.as_f64() {
                    Some(f) if f.is_finite() && f > 0.0 => Some(IskCents::from_f64_isk(f)),
                    Some(f) if f == 0.0 => None,
                    _ => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidMultiSellOrder".into(),
                            message: "PlaceMultiSellOrder expectedBrokersFee must be non-negative and finite".into(),
                        }));
                    }
                },
            };

            let station_info = ctx.sde.solar_systems.values().find_map(|sys| {
                if sys.region_id == region_id {
                    sys.stations
                        .iter()
                        .find(|st| st.station_id == docked_station_id)
                } else {
                    None
                }
            });
            let (owner_corp_id, owner_faction_id, is_structure) = if let Some(station_def) =
                station_info
            {
                (
                    station_def.owner_corporation_id,
                    station_def.owner_faction_id,
                    false,
                )
            } else if let Some((str_owner_corp, str_region)) =
                ctx.db.with_snapshot(|snap| {
                    snap.structures.get(&docked_station_id).and_then(|st| {
                        ctx.sde
                            .solar_systems
                            .get(&st.solar_system_id)
                            .map(|sys| (st.owner_corp_id, sys.region_id))
                    })
                })
            {
                if str_region != region_id {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidMarketStation".into(),
                        message: format!(
                            "Structure {docked_station_id} is not in region {region_id}"
                        ),
                    }));
                }
                (str_owner_corp, None, true)
            } else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketStation".into(),
                    message: format!("Station {docked_station_id} is not in region {region_id}"),
                }));
            };

            let mut parsed_items: Vec<(i64, TypeId, i64, IskCents, i32)> =
                Vec::with_capacity(entries.len());
            for entry in entries {
                let (st_id, raw_tid, item_id, price_isk, qty) = match entry {
                    PyRep::KeyVal(kv) => (
                        kv.get("stationID")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(docked_station_id),
                        kv.get("typeID").and_then(|v| v.as_i64()).unwrap_or(0),
                        kv.get("itemID").and_then(|v| v.as_i64()).unwrap_or(0),
                        kv.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        kv.get("quantity").and_then(|v| v.as_i64()).unwrap_or(0),
                    ),
                    PyRep::Dict(kv) => {
                        let find_key = |k: &str| {
                            kv.iter()
                                .find(|(ek, _)| ek.as_str() == Some(k))
                                .map(|(_, ev)| ev)
                        };
                        (
                            find_key("stationID")
                                .and_then(|e| e.as_i64())
                                .unwrap_or(docked_station_id),
                            find_key("typeID").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("itemID").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("price").and_then(|e| e.as_f64()).unwrap_or(0.0),
                            find_key("quantity").and_then(|e| e.as_i64()).unwrap_or(0),
                        )
                    }
                    _ => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidMultiSellOrder".into(),
                            message: "Each PlaceMultiSellOrder entry must be a KeyVal/dict with stationID, typeID, itemID, price, quantity".into(),
                        }));
                    }
                };
                if st_id != docked_station_id {
                    return Some(Err(BridgeRpcError {
                        code: "StationMismatch".into(),
                        message: format!(
                            "Cannot place sell order at station {st_id} while docked at {docked_station_id}"
                        ),
                    }));
                }
                if raw_tid <= 0
                    || !price_isk.is_finite()
                    || price_isk <= 0.0
                    || qty <= 0
                    || qty > (i32::MAX as i64)
                {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidMultiSellOrder".into(),
                        message: "PlaceMultiSellOrder entry requires valid positive typeID, price, and quantity".into(),
                    }));
                }
                parsed_items.push((
                    st_id,
                    raw_tid as TypeId,
                    item_id,
                    IskCents::from_f64_isk(price_isk),
                    qty as i32,
                ));
            }

            let now_unix = ctx.now_unix();
            // Execute the entire multi-sell batch atomically inside a single snapshot transaction
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let caller_corp_id = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                if use_corp {
                    let Some(corp) = snap.corporations.get(&caller_corp_id) else {
                        return Err(BridgeRpcError {
                            code: "CorporationNotFound".into(),
                            message: format!(
                                "Corporation {caller_corp_id} not found for corporate sell order"
                            ),
                        });
                    };
                    let role_mask = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                    if corp.ceo_id != char_id
                        && !eve_db::corp_roles::has_role(
                            role_mask,
                            eve_db::corp_roles::ROLE_ACCOUNTANT,
                        )
                        && !eve_db::corp_roles::has_role(
                            role_mask,
                            eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT,
                        )
                    {
                        return Err(BridgeRpcError {
                            code: "CorpRoleRequired".into(),
                            message: "Character lacks corporation trader/accountant role to place corporate sell order".into(),
                        });
                    }
                }
                let order_owner_id = if use_corp { caller_corp_id } else { char_id };

                let sk = snap.skills.get(&char_id);
                let broker_lvl = sk.and_then(|m| m.get(&3446)).map(|s| s.level).unwrap_or(0);
                let acct_lvl = sk.and_then(|m| m.get(&16622)).map(|s| s.level).unwrap_or(0);
                let trade_lvl =
                    sk.and_then(|m| m.get(&3443)).map(|s| s.level).unwrap_or(0) as usize;
                let retail_lvl =
                    sk.and_then(|m| m.get(&3444)).map(|s| s.level).unwrap_or(0) as usize;
                let wholesale_lvl =
                    sk.and_then(|m| m.get(&16596)).map(|s| s.level).unwrap_or(0) as usize;
                let tycoon_lvl =
                    sk.and_then(|m| m.get(&18580)).map(|s| s.level).unwrap_or(0) as usize;
                let max_open_orders =
                    5 + 4 * trade_lvl + 8 * retail_lvl + 16 * wholesale_lvl + 32 * tycoon_lvl;
                let ch = snap.characters.get(&char_id);
                let faction_standing = ch
                    .and_then(|c| owner_faction_id.and_then(|fid| c.standings.get(&fid).copied()))
                    .unwrap_or(0.0);
                let corp_standing = ch
                    .and_then(|c| c.standings.get(&owner_corp_id).copied())
                    .unwrap_or(0.0);
                let broker_rate = if is_structure {
                    resolve_structure_broker_rate(snap, docked_station_id)
                } else {
                    compute_broker_fee_rate(broker_lvl, faction_standing, corp_standing)
                };
                let broker_ppm = (broker_rate * 1_000_000.0).round() as i128;

                let mut order_ids = Vec::with_capacity(parsed_items.len());
                let mut total_broker_fee = IskCents(0);
                let mut total_sales_tax = IskCents(0);
                let mut all_filled_immediately = true;

                for (station_id, type_id, raw_item_id, order_price, quantity_i32) in parsed_items {
                    let _ = order_price
                        .checked_mul_qty(quantity_i32 as i64)
                        .ok_or_else(|| BridgeRpcError {
                            code: "MarketOrderOverflow".into(),
                            message: "Order total cost overflows 64-bit ISK cents".into(),
                        })?;

                    // Resolve itemID (explicit or auto-discovered from hangar stack)
                    let item_id = if raw_item_id > 0 {
                        raw_item_id
                    } else {
                        let found_id = snap
                            .items
                            .values()
                            .find(|i| {
                                i.type_id == type_id
                                    && i.quantity >= quantity_i32
                                    && !i.singleton
                                    && snap.is_item_consumable_by(i, char_id, Some(station_id), None)
                                    && !snap.items.values().any(|other| {
                                        other.item_id != i.item_id
                                            && other.location_id == i.item_id
                                            && other.quantity > 0
                                    })
                            })
                            .map(|i| i.item_id);
                        let Some(fid) = found_id else {
                            let has_non_empty_candidate = snap.items.values().any(|i| {
                                i.type_id == type_id
                                    && snap.is_item_consumable_by(i, char_id, Some(station_id), None)
                                    && snap.items.values().any(|other| {
                                        other.item_id != i.item_id
                                            && other.location_id == i.item_id
                                            && other.quantity > 0
                                    })
                            });
                            if has_non_empty_candidate {
                                return Err(BridgeRpcError {
                                    code: "CannotSellItemWithContents".into(),
                                    message: format!(
                                        "Cannot sell ship or container of type {type_id} while it contains fitted modules, rigs, or cargo"
                                    ),
                                });
                            }
                            let has_assembled_candidate = snap.items.values().any(|i| {
                                i.type_id == type_id
                                    && i.singleton
                                    && snap.is_item_consumable_by(i, char_id, Some(station_id), None)
                            });
                            if has_assembled_candidate {
                                return Err(BridgeRpcError {
                                    code: "CannotSellAssembledItem".into(),
                                    message: format!(
                                        "Item of type {type_id} in station {station_id} hangar must be repackaged before selling on the market"
                                    ),
                                });
                            }
                            return Err(BridgeRpcError {
                                code: "InsufficientInventoryForSellOrder".into(),
                                message: format!(
                                    "Seller does not have {quantity_i32}x repackaged consumable type {type_id} in station {station_id} hangar"
                                ),
                            });
                        };
                        fid
                    };

                    // Validate and consume exact itemID from seller's hangar at station_id
                    let Some(item) = snap.items.get(&item_id).cloned() else {
                        return Err(BridgeRpcError {
                            code: "InsufficientInventoryForSellOrder".into(),
                            message: format!("Item {item_id} not found in seller inventory"),
                        });
                    };
                    if item.type_id != type_id
                        || !snap.is_item_consumable_by(&item, char_id, Some(station_id), None)
                    {
                        return Err(BridgeRpcError {
                            code: "InsufficientInventoryForSellOrder".into(),
                            message: format!(
                                "Item {item_id} (type {type_id}) is not reachable in station {station_id} hangar"
                            ),
                        });
                    }
                    if item.singleton {
                        return Err(BridgeRpcError {
                            code: "CannotSellAssembledItem".into(),
                            message: format!(
                                "Item {item_id} of type {type_id} must be repackaged before selling"
                            ),
                        });
                    }
                    if snap.items.values().any(|other| {
                        other.item_id != item_id
                            && other.location_id == item_id
                            && other.quantity > 0
                    }) {
                        return Err(BridgeRpcError {
                            code: "CannotSellItemWithContents".into(),
                            message: format!(
                                "Cannot sell container/ship {item_id} while it contains items"
                            ),
                        });
                    }
                    if item.quantity < quantity_i32 {
                        return Err(BridgeRpcError {
                            code: "InsufficientInventoryForSellOrder".into(),
                            message: format!(
                                "Item {item_id} has quantity {}, less than requested {quantity_i32}",
                                item.quantity
                            ),
                        });
                    }
                    if let Some(item_mut) = snap.items.get_mut(&item_id) {
                        if item_mut.quantity > quantity_i32 {
                            item_mut.quantity -= quantity_i32;
                            item_mut.version += 1;
                        } else {
                            snap.items.remove(&item_id);
                        }
                    }

                    // Match against resting bid orders in region whose range reaches station_id
                    let mut candidate_ids: Vec<(i64, i64)> = snap
                        .market_orders
                        .values()
                        .filter(|o| {
                            o.type_id == type_id
                                && o.region_id == region_id
                                && o.is_bid
                                && o.vol_remaining > 0
                                && o.price.0 >= order_price.0
                                && can_bid_match_seller_station(ctx, snap, o, station_id, now_unix)
                        })
                        .map(|o| (o.order_id, o.price.0))
                        .collect();
                    candidate_ids.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

                    let mut remaining_qty = quantity_i32;
                    let mut first_matched_oid = None;

                    for (oid, _) in candidate_ids {
                        if remaining_qty <= 0 {
                            break;
                        }
                        let Some(resting) = snap.market_orders.get(&oid).cloned() else {
                            continue;
                        };
                        let required_min = resting.min_volume.max(1).min(resting.vol_remaining);
                        if remaining_qty < required_min {
                            continue;
                        }
                        let fill_qty = remaining_qty.min(resting.vol_remaining);
                        if fill_qty <= 0 {
                            continue;
                        }
                        let Some(fill_gross) = resting.price.checked_mul_qty(fill_qty as i64)
                        else {
                            continue;
                        };
                        let from_escrow = resting.escrow.min(fill_gross);
                        let uncovered = fill_gross.checked_sub(from_escrow).unwrap_or(IskCents(0));
                        if !resting.is_npc && uncovered.0 > 0 {
                            // Margin Trading order: cover shortfall from buyer's wallet or cancel order if insolvent
                            let covered = if let Some(buyer_ch) =
                                snap.characters.get_mut(&resting.owner_id)
                            {
                                if let Some(new_bal) = buyer_ch.balance.checked_sub(uncovered) {
                                    buyer_ch.balance = new_bal;
                                    snap.append_journal(
                                        resting.owner_id,
                                        "MarketEscrowCover",
                                        uncovered,
                                        new_bal,
                                        format!("Covered margin escrow shortfall for buy order {oid}"),
                                    );
                                    true
                                } else {
                                    false
                                }
                            } else if let Some(buyer_corp) =
                                snap.corporations.get_mut(&resting.owner_id)
                            {
                                if let Some(new_bal) =
                                    buyer_corp.master_wallet.checked_sub(uncovered)
                                {
                                    buyer_corp.master_wallet = new_bal;
                                    snap.append_journal(
                                        resting.owner_id,
                                        "MarketEscrowCover",
                                        uncovered,
                                        new_bal,
                                        format!("Covered margin escrow shortfall for buy order {oid}"),
                                    );
                                    true
                                } else {
                                    false
                                }
                            } else {
                                false
                            };
                            if !covered {
                                if resting.escrow.0 > 0 {
                                    if let Some(buyer_ch) =
                                        snap.characters.get_mut(&resting.owner_id)
                                    {
                                        buyer_ch.balance = buyer_ch
                                            .balance
                                            .checked_add(resting.escrow)
                                            .unwrap_or(buyer_ch.balance);
                                    }
                                }
                                if let Some(o_mut) = snap.market_orders.get_mut(&oid) {
                                    o_mut.vol_remaining = 0;
                                    o_mut.escrow = IskCents(0);
                                }
                                continue;
                            }
                        }

                        let fill_tax = compute_sales_tax_cents(fill_gross, acct_lvl);
                        total_sales_tax = total_sales_tax
                            .checked_add(fill_tax)
                            .unwrap_or(total_sales_tax);
                        let seller_net =
                            fill_gross.checked_sub(fill_tax).unwrap_or(IskCents(0));
                        if use_corp {
                            if let Some(corp_mut) = snap.corporations.get_mut(&caller_corp_id) {
                                corp_mut.master_wallet = corp_mut
                                    .master_wallet
                                    .checked_add(seller_net)
                                    .unwrap_or(corp_mut.master_wallet);
                                let bal_after = corp_mut.master_wallet;
                                snap.append_journal(
                                    caller_corp_id,
                                    "MarketSell",
                                    seller_net,
                                    bal_after,
                                    format!("Sold {fill_qty}x type {type_id}"),
                                );
                                if fill_tax.0 > 0 {
                                    snap.append_journal(
                                        caller_corp_id,
                                        "TransactionTax",
                                        fill_tax,
                                        bal_after,
                                        format!("Sales tax for selling {fill_qty}x type {type_id}"),
                                    );
                                }
                            }
                        } else if let Some(seller) = snap.characters.get_mut(&char_id) {
                            seller.balance = seller
                                .balance
                                .checked_add(seller_net)
                                .unwrap_or(seller.balance);
                            let bal_after = seller.balance;
                            snap.append_journal(
                                char_id,
                                "MarketSell",
                                seller_net,
                                bal_after,
                                format!("Sold {fill_qty}x type {type_id}"),
                            );
                            if fill_tax.0 > 0 {
                                snap.append_journal(
                                    char_id,
                                    "TransactionTax",
                                    fill_tax,
                                    bal_after,
                                    format!("Sales tax for selling {fill_qty}x type {type_id}"),
                                );
                            }
                        }
                        if !resting.is_npc {
                            snap.next_entity_id += 1;
                            let new_item_id = snap.next_entity_id;
                            snap.items.insert(
                                new_item_id,
                                InventoryItemRecord {
                                    item_id: new_item_id,
                                    type_id,
                                    owner_id: resting.owner_id,
                                    location_id: station_id,
                                    flag: InventoryFlag::Hangar,
                                    quantity: fill_qty,
                                    singleton: false,
                                    version: 1,
                                    ..Default::default()
                                },
                            );
                        }
                        if let Some(o_mut) = snap.market_orders.get_mut(&oid) {
                            o_mut.vol_remaining -= fill_qty;
                            o_mut.escrow =
                                o_mut.escrow.checked_sub(from_escrow).unwrap_or(IskCents(0));
                        }
                        remaining_qty -= fill_qty;
                        if first_matched_oid.is_none() {
                            first_matched_oid = Some(oid);
                        }
                    }

                    if remaining_qty == 0 {
                        // Immediate fill against resting bid orders: no resting order is placed and no broker fee is charged
                        order_ids.push(PyRep::Integer(first_matched_oid.unwrap_or(0)));
                        continue;
                    }
                    all_filled_immediately = false;

                    if duration_days == 0 {
                        return Err(BridgeRpcError {
                            code: "ImmediateOrderUnfilled".into(),
                            message: "Immediate sell order (duration=0) could not be completely filled".into(),
                        });
                    }

                    // Broker fee applies only to the resting portion (`remaining_qty`) with a 100.00 ISK minimum floor
                    let resting_cost = order_price
                        .checked_mul_qty(remaining_qty as i64)
                        .ok_or_else(|| BridgeRpcError {
                            code: "MarketOrderOverflow".into(),
                            message: "Order resting cost overflows 64-bit ISK cents".into(),
                        })?;
                    let raw_broker_fee = IskCents(
                        (((resting_cost.0 as i128) * broker_ppm + 500_000) / 1_000_000) as i64,
                    );
                    let broker_fee = raw_broker_fee.max(IskCents(10_000));

                    if use_corp {
                        let corp_bal = snap
                            .corporations
                            .get(&caller_corp_id)
                            .map(|c| c.master_wallet)
                            .unwrap_or(IskCents(0));
                        if corp_bal.checked_sub(broker_fee).is_none() {
                            return Err(BridgeRpcError {
                                code: "InsufficientFunds".into(),
                                message: "Not enough corporation ISK for market broker fee".into(),
                            });
                        }
                        if let Some(corp_mut) = snap.corporations.get_mut(&caller_corp_id) {
                            corp_mut.master_wallet = corp_mut
                                .master_wallet
                                .checked_sub(broker_fee)
                                .unwrap_or(IskCents(0));
                            let bal_after = corp_mut.master_wallet;
                            snap.append_journal(
                                caller_corp_id,
                                "MarketBrokerFee",
                                broker_fee,
                                bal_after,
                                format!("Broker fee for sell order on type {type_id}"),
                            );
                        }
                    } else {
                        let caller_balance = snap
                            .characters
                            .get(&char_id)
                            .map(|c| c.balance)
                            .unwrap_or(IskCents(0));
                        if caller_balance.checked_sub(broker_fee).is_none() {
                            return Err(BridgeRpcError {
                                code: "InsufficientFunds".into(),
                                message: "Not enough ISK for market broker fee".into(),
                            });
                        }
                        if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                            ch_mut.balance = ch_mut
                                .balance
                                .checked_sub(broker_fee)
                                .unwrap_or(IskCents(0));
                            let bal_after = ch_mut.balance;
                            snap.append_journal(
                                char_id,
                                "MarketBrokerFee",
                                broker_fee,
                                bal_after,
                                format!("Broker fee for sell order on type {type_id}"),
                            );
                        }
                    }
                    if is_structure && broker_fee.0 > 0 {
                        let owner_share = IskCents(broker_fee.0 / 2);
                        if owner_share.0 > 0 {
                            if let Some(str_corp) = snap.corporations.get_mut(&owner_corp_id) {
                                str_corp.master_wallet = str_corp
                                    .master_wallet
                                    .checked_add(owner_share)
                                    .unwrap_or(str_corp.master_wallet);
                            }
                        }
                    }
                    total_broker_fee = total_broker_fee
                        .checked_add(broker_fee)
                        .unwrap_or(total_broker_fee);

                    let active_open_orders = snap
                        .market_orders
                        .values()
                        .filter(|o| o.owner_id == char_id && !o.is_npc && o.vol_remaining > 0)
                        .count();
                    if active_open_orders >= max_open_orders {
                        return Err(BridgeRpcError {
                            code: "MaxOpenOrdersReached".into(),
                            message: format!(
                                "Character has reached maximum open market orders ({active_open_orders}/{max_open_orders})"
                            ),
                        });
                    }

                    snap.next_entity_id += 1;
                    let new_oid = snap.next_entity_id;
                    snap.market_orders.insert(
                        new_oid,
                        MarketOrderRecord {
                            order_id: new_oid,
                            owner_id: order_owner_id,
                            station_id,
                            region_id,
                            type_id,
                            is_bid: false,
                            price: order_price,
                            vol_entered: quantity_i32,
                            vol_remaining: remaining_qty,
                            min_volume: 1,
                            is_npc: false,
                            escrow: IskCents(0),
                        },
                    );
                    save_market_order_meta(
                        snap,
                        new_oid,
                        MarketOrderMeta {
                            range: 32767,
                            duration: duration_days,
                            issue_unix: now_unix,
                        },
                    );
                    order_ids.push(PyRep::Integer(new_oid));
                }

                if let Some(expected_fee) = expected_brokers_fee {
                    if total_broker_fee.0 > expected_fee.0.saturating_add(1) {
                        return Err(BridgeRpcError {
                            code: "BrokerFeeExceeded".into(),
                            message: format!(
                                "Actual broker fee {:.2} ISK exceeds expectedBrokersFee {:.2} ISK",
                                total_broker_fee.to_wire_f64(),
                                expected_fee.to_wire_f64()
                            ),
                        });
                    }
                }

                Ok((order_ids, total_broker_fee, total_sales_tax, all_filled_immediately))
            });

            match res {
                Ok((order_ids, total_broker, total_tax, all_filled_immediately)) => {
                    let first_oid = order_ids
                        .first()
                        .and_then(|o| o.as_i64())
                        .unwrap_or(0);
                    Some(Ok(PyRep::keyval([
                        ("orderID", PyRep::Integer(first_oid)),
                        ("filledImmediately", PyRep::Bool(all_filled_immediately)),
                        ("brokerFeeISK", PyRep::Real(total_broker.to_wire_f64())),
                        ("salesTaxISK", PyRep::Real(total_tax.to_wire_f64())),
                        ("placedCount", PyRep::Integer(order_ids.len() as i64)),
                        ("orderIDs", PyRep::List(order_ids)),
                        ("totalBrokerFeeISK", PyRep::Real(total_broker.to_wire_f64())),
                        ("totalSalesTaxISK", PyRep::Real(total_tax.to_wire_f64())),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        "CancelCharOrder" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(order_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketOrder".into(),
                    message: "CancelCharOrder requires an explicit orderID".into(),
                }));
            };
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let Some(order) = snap.market_orders.get(&order_id).cloned() else {
                    return Err(BridgeRpcError {
                        code: "OrderNotFound".into(),
                        message: format!("Market order {order_id} not found"),
                    });
                };
                if order.owner_id != char_id {
                    return Err(BridgeRpcError {
                        code: "OrderOwnershipViolation".into(),
                        message: format!("Order {order_id} does not belong to character {char_id}"),
                    });
                }
                if order.vol_remaining <= 0 {
                    return Err(BridgeRpcError {
                        code: "OrderAlreadyFilled".into(),
                        message: format!("Order {order_id} is already filled or cancelled"),
                    });
                }
                if order.is_bid {
                    // Buy order: refund remaining escrow to character balance
                    if order.escrow.0 > 0 {
                        if let Some(buyer) = snap.characters.get_mut(&char_id) {
                            buyer.balance = buyer
                                .balance
                                .checked_add(order.escrow)
                                .unwrap_or(buyer.balance);
                            let bal_after = buyer.balance;
                            snap.append_journal(
                                char_id,
                                "MarketOrderCancelRefund",
                                order.escrow,
                                bal_after,
                                format!("Refunded escrow for cancelled buy order {order_id}"),
                            );
                        }
                    }
                } else {
                    // Sell order: return remaining items to seller's station hangar
                    let remaining_qty = order.vol_remaining;
                    let existing_item = snap
                        .items
                        .values()
                        .find(|i| {
                            i.owner_id == char_id
                                && i.location_id == order.station_id
                                && i.flag == InventoryFlag::Hangar
                                && i.type_id == order.type_id
                                && !i.singleton
                        })
                        .map(|i| i.item_id);

                    if let Some(eid) = existing_item {
                        if let Some(item) = snap.items.get_mut(&eid) {
                            item.quantity = item.quantity.saturating_add(remaining_qty);
                            item.version += 1;
                        }
                    } else {
                        snap.next_entity_id += 1;
                        let nid = snap.next_entity_id;
                        snap.items.insert(
                            nid,
                            InventoryItemRecord {
                                item_id: nid,
                                type_id: order.type_id,
                                owner_id: char_id,
                                location_id: order.station_id,
                                flag: InventoryFlag::Hangar,
                                quantity: remaining_qty,
                                singleton: false,
                                version: 1,
                                ..Default::default()
                            },
                        );
                    }
                }

                if let Some(o_mut) = snap.market_orders.get_mut(&order_id) {
                    o_mut.vol_remaining = 0;
                    o_mut.escrow = IskCents(0);
                }
                Ok(())
            });
            match res {
                Ok(()) => Some(Ok(PyRep::Bool(true))),
                Err(e) => Some(Err(e)),
            }
        }

        "BuyMultipleItems" => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(station_id) = session.station_id else {
                return Some(Err(BridgeRpcError {
                    code: "NotDockedAtStation".into(),
                    message: "Character must be docked at an NPC station to BuyMultipleItems".into(),
                }));
            };
            let raw_entries = match (args.first(), args.get(1)) {
                (Some(PyRep::List(entries) | PyRep::Tuple(entries)), _) => entries.as_slice(),
                (Some(PyRep::Integer(sid)), Some(PyRep::List(entries) | PyRep::Tuple(entries))) => {
                    if *sid != station_id {
                        return Some(Err(BridgeRpcError {
                            code: "StationMismatch".into(),
                            message: format!(
                                "BuyMultipleItems station {sid} does not match docked station {station_id}"
                            ),
                        }));
                    }
                    entries.as_slice()
                }
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidBuyMultipleItems".into(),
                        message: "BuyMultipleItems requires a non-empty list of (type_id, quantity[, max_price_isk]) entries".into(),
                    }));
                }
            };
            if raw_entries.is_empty() {
                return Some(Err(BridgeRpcError {
                    code: "InvalidBuyMultipleItems".into(),
                    message: "BuyMultipleItems requires a non-empty list of items to purchase"
                        .into(),
                }));
            }

            let mut parsed_requests: Vec<(TypeId, i32, Option<IskCents>)> =
                Vec::with_capacity(raw_entries.len());
            for entry in raw_entries {
                let (tid_i64, qty_i64, max_price_f64) = match entry {
                    PyRep::Tuple(els) | PyRep::List(els) => (
                        els.first().and_then(|e| e.as_i64()).unwrap_or(0),
                        els.get(1).and_then(|e| e.as_i64()).unwrap_or(0),
                        els.get(2).and_then(|e| e.as_f64()),
                    ),
                    PyRep::KeyVal(kv) => (
                        kv.get("typeID").and_then(|e| e.as_i64()).unwrap_or(0),
                        kv.get("quantity").and_then(|e| e.as_i64()).unwrap_or(0),
                        kv.get("maxPrice")
                            .or_else(|| kv.get("price"))
                            .and_then(|e| e.as_f64()),
                    ),
                    PyRep::Dict(kv) => {
                        let find_key = |k: &str| {
                            kv.iter()
                                .find(|(ek, _)| ek.as_str() == Some(k))
                                .map(|(_, ev)| ev)
                        };
                        (
                            find_key("typeID").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("quantity").and_then(|e| e.as_i64()).unwrap_or(0),
                            find_key("maxPrice")
                                .or_else(|| find_key("price"))
                                .and_then(|e| e.as_f64()),
                        )
                    }
                    _ => (0, 0, None),
                };
                let Ok(tid_u32) = u32::try_from(tid_i64) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidBuyMultipleItems".into(),
                        message: format!("Invalid type_id {tid_i64} in BuyMultipleItems"),
                    }));
                };
                let Ok(qty_i32) = i32::try_from(qty_i64) else {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidBuyMultipleItems".into(),
                        message: format!("Invalid quantity {qty_i64} in BuyMultipleItems"),
                    }));
                };
                if tid_u32 == 0
                    || qty_i32 <= 0
                    || !ctx
                        .sde
                        .types
                        .get(&tid_u32)
                        .map(|t| t.published)
                        .unwrap_or(false)
                {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidBuyMultipleItems".into(),
                        message: format!(
                            "BuyMultipleItems entry ({tid_u32}, {qty_i32}) must reference a published type and positive quantity"
                        ),
                    }));
                }
                let max_price_cents = match max_price_f64 {
                    Some(p) if !p.is_finite() || p <= 0.0 => {
                        return Some(Err(BridgeRpcError {
                            code: "InvalidBuyMultipleItems".into(),
                            message: "BuyMultipleItems max_price must be finite and positive".into(),
                        }));
                    }
                    Some(p) => Some(IskCents::from_f64_isk(p)),
                    None => None,
                };
                parsed_requests.push((tid_u32, qty_i32, max_price_cents));
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let buyer_bal = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.balance)
                    .ok_or_else(|| BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: format!("Character {char_id} not found"),
                    })?;

                let mut virtual_rem: HashMap<i64, i32> = snap
                    .market_orders
                    .values()
                    .filter(|o| !o.is_bid && o.station_id == station_id && o.vol_remaining > 0)
                    .map(|o| (o.order_id, o.vol_remaining))
                    .collect();

                // (order_id, type_id, fill_qty, fill_gross, resting_owner_id, resting_is_npc, ask_price)
                let mut planned_fills: Vec<(i64, TypeId, i32, IskCents, i64, bool, IskCents)> = Vec::new();
                let mut total_cost = IskCents(0);
                let mut total_qty_bought: i64 = 0;

                for (req_tid, req_qty, max_price_opt) in &parsed_requests {
                    let mut candidate_orders: Vec<(i64, IskCents, i64, bool)> = snap
                        .market_orders
                        .values()
                        .filter(|o| {
                            !o.is_bid
                                && o.station_id == station_id
                                && o.type_id == *req_tid
                                && virtual_rem.get(&o.order_id).copied().unwrap_or(0) > 0
                                && max_price_opt.map(|mp| o.price.0 <= mp.0).unwrap_or(true)
                        })
                        .map(|o| (o.order_id, o.price, o.owner_id, o.is_npc))
                        .collect();
                    candidate_orders
                        .sort_by(|a, b| a.1.0.cmp(&b.1.0).then_with(|| a.0.cmp(&b.0)));

                    let mut rem = *req_qty;
                    for (oid, ask_price, seller_id, is_npc) in candidate_orders {
                        if rem <= 0 {
                            break;
                        }
                        let avail = virtual_rem.get(&oid).copied().unwrap_or(0);
                        let fill_qty = rem.min(avail);
                        if fill_qty <= 0 {
                            continue;
                        }
                        let fill_gross = ask_price
                            .checked_mul_qty(fill_qty as i64)
                            .ok_or_else(|| BridgeRpcError {
                                code: "MarketOrderOverflow".into(),
                                message: "Order fill cost overflowed ISK bounds".into(),
                            })?;
                        total_cost =
                            total_cost
                                .checked_add(fill_gross)
                                .ok_or_else(|| BridgeRpcError {
                                    code: "MarketOrderOverflow".into(),
                                    message: "Total BuyMultipleItems cost overflowed ISK bounds"
                                        .into(),
                                })?;
                        if let Some(vr) = virtual_rem.get_mut(&oid) {
                            *vr -= fill_qty;
                        }
                        rem -= fill_qty;
                        total_qty_bought += fill_qty as i64;
                        planned_fills.push((oid, *req_tid, fill_qty, fill_gross, seller_id, is_npc, ask_price));
                    }

                    if rem > 0 {
                        return Err(BridgeRpcError {
                            code: "InsufficientMarketLiquidity".into(),
                            message: format!(
                                "Station {station_id} lacks sufficient ask orders for type {req_tid} (short by {rem} units)"
                            ),
                        });
                    }
                }

                if buyer_bal.0 < total_cost.0 {
                    return Err(BridgeRpcError {
                        code: "InsufficientFunds".into(),
                        message: format!(
                            "Insufficient ISK for BuyMultipleItems: need {:.2}, have {:.2}",
                            total_cost.to_wire_f64(),
                            buyer_bal.to_wire_f64()
                        ),
                    });
                }

                let mut total_sales_tax = IskCents(0);
                let mut orders_created = Vec::with_capacity(planned_fills.len());

                for (oid, fill_tid, fill_qty, fill_gross, seller_id, is_npc, ask_price) in planned_fills {
                    if let Some(buyer) = snap.characters.get_mut(&char_id) {
                        buyer.balance = buyer
                            .balance
                            .checked_sub(fill_gross)
                            .unwrap_or(IskCents(0));
                        let bal_after = buyer.balance;
                        snap.append_journal(
                            char_id,
                            "MarketTransaction",
                            fill_gross,
                            bal_after,
                            format!("Bought {fill_qty}x type {fill_tid}"),
                        );
                    }
                    if !is_npc {
                        let seller_acct_lvl = snap
                            .skills
                            .get(&seller_id)
                            .and_then(|m| m.get(&16622))
                            .map(|s| s.level)
                            .unwrap_or(0);
                        let fill_tax = compute_sales_tax_cents(fill_gross, seller_acct_lvl);
                        total_sales_tax = total_sales_tax
                            .checked_add(fill_tax)
                            .unwrap_or(total_sales_tax);
                        let seller_net =
                            fill_gross.checked_sub(fill_tax).unwrap_or(IskCents(0));
                        if let Some(seller) = snap.characters.get_mut(&seller_id) {
                            seller.balance = seller
                                .balance
                                .checked_add(seller_net)
                                .unwrap_or(seller.balance);
                            let bal_after = seller.balance;
                            snap.append_journal(
                                seller_id,
                                "MarketSell",
                                seller_net,
                                bal_after,
                                format!("Sold {fill_qty}x type {fill_tid}"),
                            );
                            if fill_tax.0 > 0 {
                                snap.append_journal(
                                    seller_id,
                                    "TransactionTax",
                                    fill_tax,
                                    bal_after,
                                    format!("Sales tax for selling {fill_qty}x type {fill_tid}"),
                                );
                            }
                        }
                    }
                    if let Some(o_mut) = snap.market_orders.get_mut(&oid) {
                        o_mut.vol_remaining -= fill_qty;
                    }
                    snap.next_entity_id += 1;
                    let new_item_id = snap.next_entity_id;
                    snap.items.insert(
                        new_item_id,
                        InventoryItemRecord {
                            item_id: new_item_id,
                            type_id: fill_tid,
                            owner_id: char_id,
                            location_id: station_id,
                            flag: InventoryFlag::Hangar,
                            quantity: fill_qty,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                    orders_created.push(PyRep::keyval([
                        ("orderID", PyRep::Integer(oid)),
                        ("typeID", PyRep::Integer(fill_tid as i64)),
                        ("quantity", PyRep::Integer(fill_qty as i64)),
                        ("price", PyRep::Real(ask_price.to_wire_f64())),
                        ("itemID", PyRep::Integer(new_item_id)),
                    ]));
                }

                Ok((parsed_requests.len() as i64, total_qty_bought, total_cost, total_sales_tax, orders_created))
            });

            match res {
                Ok((line_count, total_qty, total_cost, total_sales_tax, orders_created)) => Some(Ok(PyRep::keyval([
                    ("ordersCreated", PyRep::List(orders_created)),
                    ("purchasedCount", PyRep::Integer(line_count)),
                    ("totalQuantity", PyRep::Integer(total_qty)),
                    ("totalCostISK", PyRep::Real(total_cost.to_wire_f64())),
                    ("salesTaxISK", PyRep::Real(total_sales_tax.to_wire_f64())),
                ]))),
                Err(e) => Some(Err(e)),
            }
        }

        "GetOldPriceHistory" | "GetNewPriceHistory" => {
            let Some(region_id) = session.region_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoRegion".into(),
                    message: "Session has no active region".into(),
                }));
            };
            let Some(raw_type_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidMarketOrder".into(),
                    message: "GetPriceHistory requires typeID".into(),
                }));
            };
            let type_id = raw_type_id as TypeId;
            let now_unix = ctx.now_unix();

            let rows = ctx.db.with_snapshot(|snap| {
                let orders: Vec<&MarketOrderRecord> = snap
                    .market_orders
                    .values()
                    .filter(|o| o.type_id == type_id && o.region_id == region_id)
                    .collect();
                let ref_price = if let Some(first) = orders.first() {
                    first.price.to_wire_f64()
                } else if let Some(t) = ctx.sde.types.get(&type_id) {
                    t.base_price
                } else {
                    100.0
                };
                let mut history_rows = Vec::new();
                for day_offset in (1..=30).rev() {
                    let day_sec = now_unix.saturating_sub(day_offset * 86_400);
                    let file_time = (day_sec as i64 + 11_644_473_600) * 10_000_000;
                    let low = ref_price * 0.95;
                    let high = ref_price * 1.05;
                    let avg = ref_price;
                    let volume = 100_i64;
                    let order_count = 5_i64;
                    history_rows.push(vec![
                        PyRep::Integer(file_time),
                        PyRep::Real(low),
                        PyRep::Real(high),
                        PyRep::Real(avg),
                        PyRep::Integer(volume),
                        PyRep::Integer(order_count),
                    ]);
                }
                history_rows
            });
            let hdr = DBRowDescriptor::market_history_header();
            Some(Ok(hdr.build_crowset(rows)))
        }

        _ => None,
    }
}
