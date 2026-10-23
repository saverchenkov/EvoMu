//! `market_contracts` — Domain 7: Regional Market Order Book Extensions, Order Modification,
//! Corporate Orders, Order Expiry & History, Sleeper NPC Buy Orders, Global PLEX Market, & Wallet (`§3.7`).

#![forbid(unsafe_code)]

use crate::formulas::{compute_broker_fee_rate, compute_sales_tax_rate};
use crate::router::{NotificationTarget, ServerContext};

fn notify_char(ctx: &ServerContext, char_id: i64, event: &str, args: Vec<PyRep>) {
    ctx.notify(NotificationTarget::Character(char_id), event, "charid", args);
}

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError { code: code.into(), message: message.into() }
}
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, DbColumnType, PyRep};
use eve_sde::TypeId;
use std::collections::HashMap;

pub const PLEX_TYPE_ID: TypeId = 44_992;
pub const JITA_STATION_ID: i64 = 60_003_760;
pub const THE_FORGE_REGION_ID: u32 = 10_000_002;

/// Sleeper blue loot NPC buy order table (`feat:market.npc-buy-orders-sleeper-loot`).
pub const SLEEPER_BLUE_LOOT_ORDERS: &[(TypeId, i64)] = &[
    (30_744, 200_000),   // Neural Network Analyzer
    (30_745, 500_000),   // Sleeper Data Library
    (30_746, 1_500_000), // Ancient Coordinates Database
    (30_747, 5_000_000), // Sleeper Drone AI Nexus
];

use super::market_trading::{
    load_market_order_meta, resolve_station_solar_system, resolve_structure_broker_rate,
    save_market_order_meta, unix_to_filetime, MarketOrderMeta,
};

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

/// Validates EVE's 4-significant-figure price tick rule (`feat:market.tick-size-four-significant-digits`).
/// Any positive price in ISK cents must have at most 4 significant decimal digits after
/// stripping trailing decimal zeros (`12340.00` -> `1234` valid; `12345.00` -> `12345` invalid).
pub fn is_valid_4_sig_fig_price(price_isk: f64) -> bool {
    if !price_isk.is_finite() || price_isk <= 0.0 { return false; }
    let scaled = price_isk * 100.0;
    let cents = scaled.round() as i64;
    if cents <= 0 || (scaled - (cents as f64)).abs() > 1e-4 { return false; }
    let mut rem = cents;
    while rem > 0 && rem % 10 == 0 { rem /= 10; }
    rem <= 9_999
}

/// Max jump distance allowed by remote market skills (`Procurement` `16594`, `Marketing` `16598`,
/// `Daytrading` `16595`, `Visibility` `3447`):
/// Level 0 = station only (`-1`), Level 1 = solar system (`0`), Level 2 = 5 jumps,
/// Level 3 = 10 jumps, Level 4 = 20 jumps, Level 5 = entire region (`32767`).
pub fn max_remote_jumps_for_skill_level(level: u8) -> i64 {
    match level { 0 => -1, 1 => 0, 2 => 5, 3 => 10, 4 => 20, _ => 32_767, }
}

/// Computes exact integer sales tax in ISK cents (`i128` parts-per-million rounded to nearest cent)
/// from the seller's own Accounting (`16622`) skill level (`R21-P1-4`).
pub fn compute_sales_tax_cents(fill_gross: IskCents, seller_acct_lvl: u8) -> IskCents {
    let tax_rate = compute_sales_tax_rate(seller_acct_lvl);
    let tax_ppm = (tax_rate * 1_000_000.0).round() as i128;
    IskCents((((fill_gross.0 as i128) * tax_ppm + 500_000) / 1_000_000) as i64)
}

/// Computes V24 order modification relist fee in ISK cents (`feat:market.order-modify-relist-fee`):
/// `max(100 ISK, (0.50 - 0.06 * adv_broker_lvl) * broker_rate * new_total + max(0, new_total - old_total) * broker_rate)`.
pub fn compute_relist_fee_cents(
    old_price: IskCents,
    new_price: IskCents,
    vol_remaining: i32,
    broker_rate: f64,
    adv_broker_lvl: u8,
) -> IskCents {
    let qty = (vol_remaining.max(0)) as i64;
    let old_total = old_price.checked_mul_qty(qty).unwrap_or(IskCents(0));
    let new_total = new_price.checked_mul_qty(qty).unwrap_or(IskCents(0));
    let discount_factor = 0.50 - 0.06 * (adv_broker_lvl.min(5) as f64);
    let base_part = (new_total.0 as f64) * broker_rate * discount_factor;
    let inc_cents = (new_total.0 - old_total.0).max(0);
    let delta_part = (inc_cents as f64) * broker_rate;
    let raw = IskCents((base_part + delta_part).round() as i64);
    raw.max(IskCents(10_000)) // 100.00 ISK minimum fee floor
}

fn build_price_history_rows(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    type_id: TypeId,
    region_id: Option<u32>,
    now_unix: u64,
) -> Vec<Vec<PyRep>> {
    let mut orders: Vec<&MarketOrderRecord> = snap.market_orders.values()
        .filter(|o| o.type_id == type_id && region_id.map(|r| o.region_id == r).unwrap_or(true)).collect();
    orders.sort_by_key(|o| o.order_id);
    let ref_price = if let Some(first) = orders.first() {
        first.price.to_wire_f64()
    } else if type_id == PLEX_TYPE_ID { 5_000_000.0
    } else if let Some(t) = ctx.sde.types.get(&type_id) { t.base_price.max(1.0)
    } else { 100.0
    };
    let mut history_rows = Vec::with_capacity(30);
    let today_sec = (now_unix / 86_400) * 86_400;
    for day_offset in (1..=30).rev() {
        let day_sec = today_sec.saturating_sub(day_offset * 86_400);
        let file_time = unix_to_filetime(day_sec);
        history_rows.push(vec![
            PyRep::Integer(file_time),
            PyRep::Real(ref_price * 0.95),
            PyRep::Real(ref_price * 1.05),
            PyRep::Real(ref_price),
            PyRep::Integer(100),
            PyRep::Integer(5),
        ]);
    }
    history_rows
}

/// Sweeps expired non-NPC orders (`issue_unix + duration * 86_400 <= now_unix`), refunding buy
/// order escrow or returning sell order items to hangar/deliveries (`feat:market.order-expiry-history`).
pub(crate) fn sweep_expired_orders(
    snap: &mut eve_db::WorldStateSnapshot,
    now_unix: u64,
) -> Vec<(i64, i64, bool, TypeId)> {
    let candidate_ids: Vec<i64> = snap.market_orders.values().filter(|o| !o.is_npc && o.vol_remaining > 0)
        .map(|o| o.order_id).collect();
    let mut expired = Vec::new();
    for oid in candidate_ids {
        let Some(order) = snap.market_orders.get(&oid).cloned() else { continue; };
        let mut meta = load_market_order_meta(snap, &order, now_unix);
        let dur_secs = (meta.duration.max(1) as u64).saturating_mul(86_400);
        if meta.issue_unix.saturating_add(dur_secs) > now_unix { continue; }
        if order.is_bid {
            if order.escrow.0 > 0 {
                if let Some(ch) = snap.characters.get_mut(&order.owner_id) {
                    ch.balance = ch.balance.checked_add(order.escrow).unwrap_or(ch.balance);
                    let bal = ch.balance;
                    snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                        owner_id: order.owner_id,
                        entry_type: "MarketOrderExpireRefund",
                        amount: order.escrow,
                        balance_after: bal,
                        reason: format!("Refunded escrow for expired buy order {oid}"),
                        meta: eve_db::ledger::JournalMeta::new(eve_db::ledger::ref_types::MARKET_ESCROW, 0, order.owner_id, now_unix)
                            .with_context(eve_db::ledger::JournalContext::MarketOrder, oid),
                    });
                } else if let Some(corp) = snap.corporations.get_mut(&order.owner_id) {
                    corp.master_wallet = corp.master_wallet.checked_add(order.escrow).unwrap_or(corp.master_wallet);
                    if let Some(ak) = meta.account_key {
                        let div_id = (ak.saturating_sub(999)).clamp(1, 7) as u8;
                        corp.ensure_wallet_divisions();
                        if let Some(div) = corp.wallet_divisions.iter_mut().find(|d| d.division_id == div_id)
                        { div.balance = div.balance.checked_add(order.escrow).unwrap_or(div.balance); }
                    }
                    let bal = corp.master_wallet;
                    snap.append_journal(
                        order.owner_id,
                        "MarketOrderExpireRefund",
                        order.escrow,
                        bal,
                        format!("Refunded escrow for expired corporate buy order {oid}"),
                    );
                }
            }
        } else { let is_corp_owner = snap.corporations.contains_key(&order.owner_id);
            let target_flag = if is_corp_owner { InventoryFlag::Deliveries } else { InventoryFlag::Hangar };
            let existing = snap.items.values().find(|i| {
                    i.owner_id == order.owner_id
                        && i.location_id == order.station_id
                        && i.flag == target_flag
                        && i.type_id == order.type_id
                        && !i.singleton
                }).map(|i| i.item_id);
            if let Some(eid) = existing {
                if let Some(it) = snap.items.get_mut(&eid) {
                    it.quantity = it.quantity.saturating_add(order.vol_remaining);
                    it.version += 1;
                }
            } else { snap.next_entity_id += 1;
                let nid = snap.next_entity_id;
                snap.items.insert(
                    nid,
                    InventoryItemRecord {
                        item_id: nid,
                        type_id: order.type_id,
                        owner_id: order.owner_id,
                        location_id: order.station_id,
                        flag: target_flag,
                        quantity: order.vol_remaining,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );
            }
        }
        if let Some(o_mut) = snap.market_orders.get_mut(&oid) { o_mut.vol_remaining = 0; o_mut.escrow = IskCents(0); }
        meta.order_state = Some(4); // 4 = Expired
        meta.final_vol_remaining = Some(order.vol_remaining);
        meta.closed_unix = Some(now_unix);
        save_market_order_meta(snap, oid, meta);
        expired.push((oid, order.owner_id, order.is_bid, order.type_id));
    }
    expired
}

pub(crate) fn emit_expired_order_notifications(ctx: &ServerContext, expired: &[(i64, i64, bool, TypeId)]) {
    for (oid, owner_id, is_bid, type_id) in expired {
        notify_char(
            ctx,
            *owner_id,
            "OnOwnOrdersChanged",
            vec![
                PyRep::Integer(*oid),
                PyRep::String("Expiry".into()),
                PyRep::Bool(false),
            ],
        );
        if !*is_bid {
            notify_char(
                ctx,
                *owner_id,
                "OnMarketItemReceived",
                vec![
                    PyRep::Integer(*oid),
                    PyRep::Integer(*type_id as i64),
                    PyRep::Integer(4),
                    PyRep::Integer(60_003_760),
                ],
            );
        }
    }
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if service == "contractProxy" {
        return super::contracts_service::handle(ctx, session, service, method, args, _kwargs);
    }

    if service == "marketProxy" {
        match method {
            "StartupCheck" => {
                let now_unix = ctx.now_unix();
                let station_id = session.station_id.unwrap_or(JITA_STATION_ID);
                let region_id = session.region_id.unwrap_or(THE_FORGE_REGION_ID);
                let ( npc_count, swept_list ) = ctx.db.with_mut_snapshot(|snap| {
                    // 1. Replenish depleted NPC seeded orders (`feat:market.npc-seeded-orders`)
                    let npc_ids: Vec<i64> = snap.market_orders.values().filter(|o| o.is_npc).map(|o| o.order_id)
                        .collect();
                    for oid in npc_ids {
                        if let Some(o_mut) = snap.market_orders.get_mut(&oid) {
                            if o_mut.vol_remaining < o_mut.vol_entered {
                                o_mut.vol_remaining = o_mut.vol_entered.max(10_000);
                            }
                        }
                        save_market_order_meta(
                            snap,
                            oid,
                            MarketOrderMeta {
                                range: 32767,
                                duration: 365,
                                issue_unix: now_unix,
                                order_state: None,
                                account_key: None,
                                final_vol_remaining: None,
                                closed_unix: None,
                            },
                        );
                    }
                    // 2. Seed permanent NPC buy orders for Sleeper blue loot (`feat:market.npc-buy-orders-sleeper-loot`)
                    for &(loot_tid, price_isk) in SLEEPER_BLUE_LOOT_ORDERS {
                        for &(st_id, reg_id) in &[
                            (JITA_STATION_ID, THE_FORGE_REGION_ID),
                            (station_id, region_id),
                        ] {
                            let exists = snap.market_orders.values().any(|o| {
                                o.is_npc
                                    && o.is_bid
                                    && o.type_id == loot_tid
                                    && o.station_id == st_id
                                    && o.vol_remaining > 0
                            });
                            if !exists {
                                snap.next_entity_id += 1;
                                let new_oid = snap.next_entity_id;
                                snap.market_orders.insert(
                                    new_oid,
                                    MarketOrderRecord {
                                        order_id: new_oid,
                                        owner_id: 1_000_125, // CONCORD / Sisters of EVE NPC buyer
                                        station_id: st_id,
                                        region_id: reg_id,
                                        type_id: loot_tid,
                                        is_bid: true,
                                        price: IskCents::from_isk(price_isk),
                                        vol_entered: 1_000_000,
                                        vol_remaining: 1_000_000,
                                        min_volume: 1,
                                        is_npc: true,
                                        escrow: IskCents(0),
                                    },
                                );
                                save_market_order_meta(
                                    snap,
                                    new_oid,
                                    MarketOrderMeta {
                                        range: 32767,
                                        duration: 365,
                                        issue_unix: now_unix,
                                        order_state: None,
                                        account_key: None,
                                        final_vol_remaining: None,
                                        closed_unix: None,
                                    },
                                );
                            }
                        }
                    }
                    let swept = sweep_expired_orders(snap, now_unix);
                    let total_npc = snap.market_orders.values().filter(|o| o.is_npc).count() as i64;
                    (total_npc, swept)
                });
                let swept_count = swept_list.len() as i64;
                emit_expired_order_notifications(ctx, &swept_list);
                return Some(Ok(PyRep::keyval([
                    ("status", PyRep::String("ok".into())),
                    ("npcOrdersActive", PyRep::Integer(npc_count)),
                    ("sleeperBuyOrdersSeeded", PyRep::Bool(true)),
                    ("expiredOrdersSwept", PyRep::Integer(swept_count)),
                ])));
            }

            "GetSystemAsks" => {
                if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
                let Some(solar_system_id) = args.first().and_then(|a| a.as_i64()).map(|v| v as u32)
                    .or(session.solar_system_id)
                else {
                    return Some(Err(rpc_err("NoSolarSystem", "GetSystemAsks requires an active solarSystemID")));
                };
                let rows = ctx.db.with_snapshot(|snap| {
                    let mut best_by_type: std::collections::BTreeMap<TypeId, &MarketOrderRecord> =
                        std::collections::BTreeMap::new();
                    for o in snap.market_orders.values().filter(|o| {
                        !o.is_bid
                            && o.vol_remaining > 0
                            && resolve_station_solar_system(ctx, snap, o.station_id)
                                == Some(solar_system_id)
                    }) {
                        match best_by_type.get(&o.type_id) {
                            Some(prev) if o.price.0 >= prev.price.0 => {}
                            _ => { best_by_type.insert(o.type_id, o); }
                        }
                    }
                    best_by_type.into_values().map(|o| {
                            vec![
                                PyRep::Integer(o.type_id as i64),
                                PyRep::Real(o.price.to_wire_f64()),
                                PyRep::Real(o.vol_remaining as f64),
                                PyRep::Integer(o.station_id),
                            ]
                        }).collect()
                });
                return Some(Ok(DBRowDescriptor::market_summary_header().build_cindexed_rowset("typeID", rows)));
            }

            "GetHistoryForManyTypeIDs" => {
                let region_id = session.region_id.unwrap_or(THE_FORGE_REGION_ID);
                let Some(PyRep::List(items) | PyRep::Tuple(items)) = args.first() else {
                    return Some(Err(rpc_err("InvalidArgument", "GetHistoryForManyTypeIDs requires a list of typeIDs")));
                };
                let now_unix = ctx.now_unix();
                let entries = ctx.db.with_snapshot(|snap| {
                    let hdr = DBRowDescriptor::market_history_header();
                    let mut out = Vec::with_capacity(items.len());
                    for item in items {
                        if let Some(tid) = item.as_i64().filter(|&v| v > 0) {
                            let rows = build_price_history_rows(ctx, snap, tid as TypeId, Some(region_id), now_unix,);
                            out.push((PyRep::Integer(tid), hdr.build_crowset(rows)));
                        }
                    }
                    out
                });
                return Some(Ok(PyRep::Dict(entries)));
            }

            "GetCorporationOrders" => {
                let (_, char_id) = match ServerContext::require_character(session) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                let now_unix = ctx.now_unix();
                let rows = ctx.db.with_snapshot(|snap| {
                    let corp_id = args.first().and_then(|a| a.as_i64()).filter(|&v| v > 0)
                        .or_else(|| snap.characters.get(&char_id).map(|c| c.corporation_id)).unwrap_or(0);
                    let mut orders: Vec<&MarketOrderRecord> = snap.market_orders.values()
                        .filter(|o| o.owner_id == corp_id && !o.is_npc && o.vol_remaining > 0).collect();
                    orders.sort_by_key(|o| o.order_id);
                    orders.into_iter().map(|o| {
                            let meta = load_market_order_meta(snap, o, now_unix);
                            let acct_key = meta.account_key.unwrap_or(1000) as i64;
                            vec![
                                PyRep::Integer(o.order_id),
                                PyRep::Integer(o.type_id as i64),
                                PyRep::Integer(char_id),
                                PyRep::Integer(o.region_id as i64),
                                PyRep::Integer(o.station_id),
                                PyRep::Integer(meta.range),
                                PyRep::Bool(o.is_bid),
                                PyRep::Real(o.price.to_wire_f64()),
                                PyRep::Integer(o.vol_entered as i64),
                                PyRep::Integer(o.vol_remaining as i64),
                                PyRep::Integer(unix_to_filetime(meta.issue_unix)),
                                PyRep::Integer(1), // orderState 1 = open
                                PyRep::Integer(o.min_volume as i64),
                                PyRep::Bool(false),
                                PyRep::Integer(acct_key),
                                PyRep::Integer(meta.duration),
                                PyRep::Bool(true), // isCorp = true
                                PyRep::Integer(
                                    resolve_station_solar_system(ctx, snap, o.station_id)
                                        .unwrap_or(30_000_142) as i64,
                                ),
                                PyRep::Real(o.escrow.to_wire_f64()),
                            ]
                        }).collect()
                });
                return Some(Ok(PyRep::rowset(
                    vec![
                        "orderID",
                        "typeID",
                        "charID",
                        "regionID",
                        "stationID",
                        "range",
                        "bid",
                        "price",
                        "volEntered",
                        "volRemaining",
                        "issueDate",
                        "orderState",
                        "minVolume",
                        "contraband",
                        "accountID",
                        "duration",
                        "isCorp",
                        "solarSystemID",
                        "escrow",
                    ],
                    rows,
                )));
            }

            "GetMarketOrderHistory" => {
                let (_, char_id) = match ServerContext::require_character(session) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                let include_corp = args.first().and_then(|a| a.as_bool()).unwrap_or(false)
                    || _kwargs.get("isCorp").and_then(|v| v.as_bool()).unwrap_or(false);
                let now_unix = ctx.now_unix();
                let (swept_list, rows) = ctx.db.with_mut_snapshot(|snap| {
                    let swept = sweep_expired_orders(snap, now_unix);
                    let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                    let mut orders: Vec<MarketOrderRecord> = snap.market_orders.values().filter(|o| {
                            !o.is_npc
                                && o.vol_remaining == 0
                                && (o.owner_id == char_id
                                    || (include_corp && o.owner_id == corp_id))
                        }).cloned().collect();
                    orders.sort_by_key(|o| o.order_id);
                    let out = orders.into_iter().filter_map(|o| {
                            let meta = load_market_order_meta(snap, &o, now_unix);
                            if meta.closed_unix.is_some_and(|c| now_unix.saturating_sub(c) > 90 * 86_400) {
                                return None;
                            }
                            let state_code = if let Some(st) = meta.order_state {
                                st as i64
                            } else { let oid_tag = format!("order {}", o.order_id);
                                let was_cancelled = snap.wallet_journal.iter().any(|j| {
                                    j.character_id == o.owner_id
                                        && j.entry_type == "MarketOrderCancelRefund"
                                        && j.reason.contains(&oid_tag)
                                });
                                let was_expired = snap.wallet_journal.iter().any(|j| {
                                    j.character_id == o.owner_id
                                        && j.entry_type == "MarketOrderExpireRefund"
                                        && j.reason.contains(&oid_tag)
                                });
                                if was_expired { 4 } else if was_cancelled { 3 } else { 2 }
                            };
                            let is_corp = o.owner_id != char_id;
                            Some(vec![
                                PyRep::Integer(o.order_id),
                                PyRep::Integer(o.type_id as i64),
                                PyRep::Integer(char_id),
                                PyRep::Integer(o.region_id as i64),
                                PyRep::Integer(o.station_id),
                                PyRep::Integer(meta.range),
                                PyRep::Bool(o.is_bid),
                                PyRep::Real(o.price.to_wire_f64()),
                                PyRep::Integer(o.vol_entered as i64),
                                PyRep::Integer(meta.final_vol_remaining.unwrap_or(0) as i64),
                                PyRep::Integer(unix_to_filetime(meta.issue_unix)),
                                PyRep::Integer(state_code),
                                PyRep::Integer(o.min_volume as i64),
                                PyRep::Bool(false),
                                PyRep::Integer(meta.account_key.unwrap_or(1000) as i64),
                                PyRep::Integer(meta.duration),
                                PyRep::Bool(is_corp),
                                PyRep::Integer(
                                    resolve_station_solar_system(ctx, snap, o.station_id)
                                        .unwrap_or(30_000_142) as i64,
                                ),
                                PyRep::Real(0.0),
                            ])
                        }).collect();
                    (swept, out)
                });
                emit_expired_order_notifications(ctx, &swept_list);
                return Some(Ok(PyRep::rowset(
                    vec![
                        "orderID",
                        "typeID",
                        "charID",
                        "regionID",
                        "stationID",
                        "range",
                        "bid",
                        "price",
                        "volEntered",
                        "volRemaining",
                        "issueDate",
                        "orderState",
                        "minVolume",
                        "contraband",
                        "accountID",
                        "duration",
                        "isCorp",
                        "solarSystemID",
                        "escrow",
                    ],
                    rows,
                )));
            }

            "ModifyCharOrder" | "ModifyPlexCharOrder" => {
                let (_, char_id) = match ServerContext::require_character(session) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                let Some(order_id) = args.first().and_then(|a| a.as_i64()) else {
                    return Some(Err(rpc_err("InvalidMarketOrder", format!("{method} requires orderID as first argument"))));
                };
                let Some(new_price_isk) = args.get(1).and_then(|a| a.as_f64()) else {
                    return Some(Err(rpc_err("InvalidMarketOrder", format!("{method} requires newPrice as second argument"))));
                };
                if !is_valid_4_sig_fig_price(new_price_isk) {
                    return Some(Err(rpc_err("InvalidPriceTickSize", format!(
                            "Price {new_price_isk} violates EVE 4-significant-figure tick size rule"
                        ))));
                }
                let new_range_opt = _kwargs.get("newRange").or_else(|| _kwargs.get("range")).and_then(|v| v.as_i64())
                    .or_else(|| args.get(6).and_then(|a| a.as_i64()));
                let account_key_opt = _kwargs.get("accountKey").and_then(|v| v.as_i64())
                    .or_else(|| args.get(2).and_then(|a| a.as_i64()).filter(|&v| (1000..=1006).contains(&v)))
                    .map(|v| v as u16);

                let now_unix = ctx.now_unix();
                let docked_station_opt = session.station_id.or(session.structure_id);
                let caller_sys_opt = session.solar_system_id;
                let caller_reg_opt = session.region_id;

                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let Some(order) = snap.market_orders.get(&order_id).cloned() else {
                        return Err(rpc_err("OrderNotFound", format!("Market order {order_id} not found")));
                    };
                    if method == "ModifyPlexCharOrder" && order.type_id != PLEX_TYPE_ID {
                        return Err(rpc_err("InvalidPlexOrder", format!("Order {order_id} is not a PLEX order")));
                    }
                    if order.vol_remaining <= 0 || order.is_npc {
                        return Err(rpc_err("OrderAlreadyFilled", format!("Order {order_id} is no longer active")));
                    }
                    let caller_corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                    let is_corp_order = order.owner_id == caller_corp_id && order.owner_id != char_id;
                    if order.owner_id != char_id && !is_corp_order {
                        return Err(rpc_err("OrderOwnershipViolation", format!("Order {order_id} does not belong to character {char_id}")));
                    }
                    if is_corp_order {
                        let Some(corp) = snap.corporations.get(&caller_corp_id) else {
                            return Err(rpc_err("CorporationNotFound", "Corporation not found"));
                        };
                        let role_mask = corp.member_roles.get(&char_id).copied().unwrap_or(0);
                        if corp.ceo_id != char_id
                            && !eve_db::corp_roles::has_role(role_mask, eve_db::corp_roles::ROLE_ACCOUNTANT,)
                            && !eve_db::corp_roles::has_role(role_mask, eve_db::corp_roles::ROLE_JUNIOR_ACCOUNTANT,)
                        {
                            return Err(rpc_err("CorpRoleRequired", "Character lacks corporation accountant/trader role to modify corporate order"));
                        }
                    }

                    let mut meta = load_market_order_meta(snap, &order, 0);
                    if now_unix < meta.issue_unix.saturating_add(300) {
                        return Err(rpc_err("OrderModificationCooldown", "Orders cannot be modified within 5 minutes (300 seconds) of creation or last modification"));
                    }

                    let sk = snap.skills.get(&char_id);
                    let broker_lvl = sk.and_then(|m| m.get(&3446)).map(|s| s.level).unwrap_or(0);
                    let adv_broker_lvl = sk.and_then(|m| m.get(&16597)).map(|s| s.level).unwrap_or(0);
                    let daytrading_lvl = sk.and_then(|m| m.get(&16595)).map(|s| s.level).unwrap_or(0);
                    let procurement_lvl = sk.and_then(|m| m.get(&16594)).map(|s| s.level).unwrap_or(0);
                    let marketing_lvl = sk.and_then(|m| m.get(&16598)).map(|s| s.level).unwrap_or(0);
                    let visibility_lvl = sk.and_then(|m| m.get(&3447)).map(|s| s.level).unwrap_or(0);

                    // Remote order modification range check (`Daytrading` `16595`)
                    if docked_station_opt != Some(order.station_id) {
                        let max_jumps = max_remote_jumps_for_skill_level(daytrading_lvl);
                        if max_jumps < 0 {
                            return Err(rpc_err("DaytradingSkillRequired", "Daytrading skill is required to modify market orders remotely"));
                        }
                        if caller_reg_opt != Some(order.region_id) && order.type_id != PLEX_TYPE_ID {
                            return Err(rpc_err("DaytradingSkillRequired", "Cannot modify market orders outside current region"));
                        }
                        let order_sys = resolve_station_solar_system(ctx, snap, order.station_id);
                        let jumps = match (caller_sys_opt, order_sys) {
                            (Some(cs), Some(os)) if cs == os => 0,
                            (Some(cs), Some(os)) => {
                                let c_const = ctx.sde.solar_systems.get(&cs).map(|s| s.constellation_id);
                                let o_const = ctx.sde.solar_systems.get(&os).map(|s| s.constellation_id);
                                if c_const.is_some() && c_const == o_const { 3 } else { 10 }
                            }
                            _ => 0,
                        };
                        if jumps > max_jumps {
                            return Err(rpc_err("DaytradingSkillRequired", format!(
                                    "Daytrading level {daytrading_lvl} allows max {max_jumps} jumps, order station is {jumps} jumps away"
                                )));
                        }
                    }

                    // Remote buy order `Visibility` (`3447`) check if updating buy range remotely
                    if let Some(new_r) = new_range_opt {
                        if order.is_bid && docked_station_opt != Some(order.station_id) && new_r >= 0 {
                            let max_vis_range = max_remote_jumps_for_skill_level(visibility_lvl);
                            if new_r > max_vis_range {
                                return Err(rpc_err("VisibilitySkillRequired", format!(
                                        "Visibility level {visibility_lvl} allows max buy range {max_vis_range}, requested {new_r}"
                                    )));
                            }
                        }
                        meta.range = new_r;
                    }
                    if let Some(ak) = account_key_opt { meta.account_key = Some(ak); }

                    // Resolve station vs Upwell structure broker rate & 0.5% SCC surcharge
                    let structure_info = snap.structures.get(&order.station_id).filter(|st| st.state != "Destroyed")
                        .map(|st| (st.owner_corp_id, resolve_structure_broker_rate(snap, order.station_id)));
                    let (broker_rate, structure_owner_opt) =
                        if let Some((st_corp_id, st_rate)) = structure_info {
                            (st_rate, Some(st_corp_id))
                        } else { let st_def = ctx.sde.stations.get(&order.station_id);
                            let owner_corp = st_def.map(|s| s.owner_corporation_id).unwrap_or(0);
                            let owner_fac = st_def.and_then(|s| s.owner_faction_id);
                            let ch = snap.characters.get(&char_id);
                            let f_st = ch.and_then(|c| owner_fac.and_then(|fid| c.standings.get(&fid).copied()))
                                .unwrap_or(0.0);
                            let c_st = ch.and_then(|c| c.standings.get(&owner_corp).copied()).unwrap_or(0.0);
                            (compute_broker_fee_rate(broker_lvl, f_st, c_st), None)
                        };

                    let new_price = IskCents::from_f64_isk(new_price_isk);
                    let old_price = order.price;
                    let relist_fee = compute_relist_fee_cents(
                        old_price,
                        new_price,
                        order.vol_remaining,
                        broker_rate,
                        adv_broker_lvl,
                    );
                    let scc_surcharge = if structure_owner_opt.is_some() {
                        IskCents(relist_fee.0 / 2)
                    } else { IskCents(0)
                    };

                    // Calculate buy order escrow delta
                    let escrow_fraction = if adv_broker_lvl > 0 {
                        0.75_f64.powi(adv_broker_lvl.min(5) as i32)
                    } else { 1.0
                    };
                    let new_escrow = if order.is_bid {
                        let full = new_price.checked_mul_qty(order.vol_remaining as i64)
                            .ok_or_else(|| rpc_err("MarketOrderOverflow", "Modified order cost overflows 64-bit ISK cents"))?;
                        IskCents(((full.0 as f64) * escrow_fraction).round() as i64)
                    } else { IskCents(0)
                    };
                    let escrow_delta = new_escrow.0 - order.escrow.0;
                    let net_wallet_debit_cents = relist_fee.0.saturating_add(escrow_delta);

                    if is_corp_order {
                        let div_id = meta.account_key.map(|ak| (ak.saturating_sub(999)).clamp(1, 7) as u8)
                            .unwrap_or(1);
                        let Some(corp_mut) = snap.corporations.get_mut(&caller_corp_id) else {
                            return Err(rpc_err("CorporationNotFound", "Corporation not found"));
                        };
                        corp_mut.ensure_wallet_divisions();
                        if net_wallet_debit_cents > 0 {
                            let req = IskCents(net_wallet_debit_cents);
                            let div_bal = corp_mut.wallet_divisions.iter().find(|d| d.division_id == div_id)
                                .map(|d| d.balance).unwrap_or(corp_mut.master_wallet);
                            if div_bal.0 < req.0 && corp_mut.master_wallet.0 < req.0 {
                                return Err(rpc_err("InsufficientFunds", "Not enough corporation ISK for order modification relist fee and escrow"));
                            }
                            if let Some(div) = corp_mut.wallet_divisions.iter_mut().find(|d| d.division_id == div_id)
                            { div.balance = IskCents((div.balance.0 - req.0).max(0)); }
                            corp_mut.master_wallet = IskCents((corp_mut.master_wallet.0 - req.0).max(0));
                        } else if net_wallet_debit_cents < 0 { let refund = IskCents(-net_wallet_debit_cents);
                            if let Some(div) = corp_mut.wallet_divisions.iter_mut().find(|d| d.division_id == div_id)
                            { div.balance = div.balance.checked_add(refund).unwrap_or(div.balance); }
                            corp_mut.master_wallet = corp_mut.master_wallet.checked_add(refund)
                                .unwrap_or(corp_mut.master_wallet);
                        }
                        let bal_after = corp_mut.master_wallet;
                        snap.append_journal(
                            caller_corp_id,
                            "MarketBrokerFee",
                            relist_fee,
                            bal_after,
                            format!("Relist fee for modifying corporate order {order_id}"),
                        );
                    } else { let cur_bal = snap.characters.get(&char_id).map(|c| c.balance).unwrap_or(IskCents(0));
                        if net_wallet_debit_cents > 0 && cur_bal.0 < net_wallet_debit_cents {
                            return Err(rpc_err("InsufficientFunds", "Not enough ISK for order modification relist fee and escrow delta"));
                        }
                        if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                            ch_mut.balance = IskCents(ch_mut.balance.0 - net_wallet_debit_cents);
                            let bal_after = ch_mut.balance;
                            snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                                owner_id: char_id,
                                entry_type: "MarketBrokerFee",
                                amount: IskCents(-relist_fee.0),
                                balance_after: bal_after,
                                reason: format!("Relist fee for modifying order {order_id}"),
                                meta: eve_db::ledger::JournalMeta::new(
                                    eve_db::ledger::ref_types::BROKER_FEE,
                                    char_id,
                                    structure_owner_opt.unwrap_or_else(|| {
                                        ctx.sde.stations.get(&order.station_id).map(|s| s.owner_corporation_id).unwrap_or(0)
                                    }),
                                    now_unix,
                                )
                                .with_context(eve_db::ledger::JournalContext::MarketOrder, order_id),
                            });
                        }
                    }

                    if let Some(st_corp_id) = structure_owner_opt {
                        let owner_share = relist_fee.checked_sub(scc_surcharge).unwrap_or(IskCents(0));
                        if owner_share.0 > 0 {
                            if let Some(st_corp) = snap.corporations.get_mut(&st_corp_id) {
                                st_corp.master_wallet = st_corp.master_wallet.checked_add(owner_share)
                                    .unwrap_or(st_corp.master_wallet);
                            }
                        }
                    }

                    if let Some(o_mut) = snap.market_orders.get_mut(&order_id) {
                        o_mut.price = new_price;
                        if o_mut.is_bid { o_mut.escrow = new_escrow; }
                    }
                    meta.issue_unix = now_unix;
                    save_market_order_meta(snap, order_id, meta);

                    // If a corporate buy order crosses a resting ask order at the same station, deliver matched items to corp Deliveries (`InventoryFlag::Deliveries`)
                    let mut delivered_to_corp_deliveries = 0_i32;
                    if is_corp_order && order.is_bid {
                        let ask_ids: Vec<i64> = snap.market_orders.values().filter(|o| {
                                !o.is_bid
                                    && o.station_id == order.station_id
                                    && o.type_id == order.type_id
                                    && o.vol_remaining > 0
                                    && o.price.0 <= new_price.0
                            }).map(|o| o.order_id).collect();
                        for ask_id in ask_ids {
                            let cur_rem = snap.market_orders.get(&order_id).map(|o| o.vol_remaining).unwrap_or(0);
                            if cur_rem <= 0 { break; }
                            let Some(ask) = snap.market_orders.get(&ask_id).cloned() else { continue; };
                            let fill_qty = cur_rem.min(ask.vol_remaining);
                            if fill_qty <= 0 { continue; }
                            let fill_cost = ask.price.checked_mul_qty(fill_qty as i64).unwrap_or(IskCents(0));
                            if let Some(a_mut) = snap.market_orders.get_mut(&ask_id) {
                                a_mut.vol_remaining -= fill_qty;
                            }
                            if let Some(b_mut) = snap.market_orders.get_mut(&order_id) {
                                b_mut.vol_remaining -= fill_qty;
                                b_mut.escrow = b_mut.escrow.checked_sub(fill_cost).unwrap_or(IskCents(0));
                            }
                            snap.next_entity_id += 1;
                            let delivery_item_id = snap.next_entity_id;
                            snap.items.insert(
                                delivery_item_id,
                                InventoryItemRecord {
                                    item_id: delivery_item_id,
                                    type_id: order.type_id,
                                    owner_id: caller_corp_id,
                                    location_id: order.station_id,
                                    flag: InventoryFlag::Deliveries,
                                    quantity: fill_qty,
                                    singleton: false,
                                    version: 1,
                                    ..Default::default()
                                },
                            );
                            delivered_to_corp_deliveries += fill_qty;
                        }
                    }

                    Ok((
                        old_price,
                        new_price,
                        relist_fee,
                        scc_surcharge,
                        new_escrow,
                        escrow_delta,
                        meta,
                        procurement_lvl,
                        marketing_lvl,
                        daytrading_lvl,
                        visibility_lvl,
                        delivered_to_corp_deliveries,
                    ))
                });

                return match res {
                    Ok((
                        old_price,
                        new_price,
                        relist_fee,
                        scc_surcharge,
                        new_escrow,
                        escrow_delta,
                        meta,
                        proc_lvl,
                        mkt_lvl,
                        day_lvl,
                        vis_lvl,
                        corp_delivered_qty,
                    )) => {
                        notify_char(ctx, 
                            char_id,
                            "OnOwnOrdersChanged",
                            vec![PyRep::Integer(order_id), PyRep::String("Modify".into()), PyRep::Bool(false),],
                        );
                        Some(Ok(PyRep::keyval([
                            ("orderID", PyRep::Integer(order_id)),
                            ("oldPrice", PyRep::Real(old_price.to_wire_f64())),
                            ("newPrice", PyRep::Real(new_price.to_wire_f64())),
                            ("relistFeeISK", PyRep::Real(relist_fee.to_wire_f64())),
                            ("brokerFeeISK", PyRep::Real(relist_fee.to_wire_f64())),
                            ("sccSurchargeISK", PyRep::Real(scc_surcharge.to_wire_f64())),
                            ("escrowISK", PyRep::Real(new_escrow.to_wire_f64())),
                            ("escrowDeltaISK", PyRep::Real(IskCents(escrow_delta).to_wire_f64()),),
                            ("range", PyRep::Integer(meta.range)),
                            ("issueDate", PyRep::Integer(unix_to_filetime(meta.issue_unix))),
                            ("accountKey", PyRep::Integer(meta.account_key.unwrap_or(1000) as i64),),
                            ("corpDeliveredQty", PyRep::Integer(corp_delivered_qty as i64),),
                            ("maxProcurementJumps", PyRep::Integer(max_remote_jumps_for_skill_level(proc_lvl)),),
                            ("maxMarketingJumps", PyRep::Integer(max_remote_jumps_for_skill_level(mkt_lvl)),),
                            ("maxDaytradingJumps", PyRep::Integer(max_remote_jumps_for_skill_level(day_lvl)),),
                            ("maxVisibilityJumps", PyRep::Integer(max_remote_jumps_for_skill_level(vis_lvl)),),
                        ])))
                    }
                    Err(e) => Some(Err(e)),
                };
            }

            "GetPlexBest" => {
                if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
                let (best_ask, best_bid, ask_vol, bid_vol, summary_rows) =
                    ctx.db.with_snapshot(|snap| {
                        let mut min_ask: Option<&MarketOrderRecord> = None;
                        let mut max_bid: Option<&MarketOrderRecord> = None;
                        let mut total_ask_vol = 0_i64;
                        let mut total_bid_vol = 0_i64;
                        for o in snap.market_orders.values()
                            .filter(|o| o.type_id == PLEX_TYPE_ID && o.vol_remaining > 0)
                        {
                            if o.is_bid {
                                total_bid_vol += o.vol_remaining as i64;
                                if max_bid.map(|b| o.price.0 > b.price.0).unwrap_or(true) { max_bid = Some(o); }
                            } else { total_ask_vol += o.vol_remaining as i64;
                                if min_ask.map(|a| o.price.0 < a.price.0).unwrap_or(true) { min_ask = Some(o); }
                            }
                        }
                        let rows = if let Some(a) = min_ask {
                            vec![vec![
                                PyRep::Integer(PLEX_TYPE_ID as i64),
                                PyRep::Real(a.price.to_wire_f64()),
                                PyRep::Real(a.vol_remaining as f64),
                                PyRep::Integer(a.station_id),
                            ]]
                        } else { Vec::new()
                        };
                        (
                            min_ask.map(|a| a.price.to_wire_f64()).unwrap_or(0.0),
                            max_bid.map(|b| b.price.to_wire_f64()).unwrap_or(0.0),
                            total_ask_vol,
                            total_bid_vol,
                            rows,
                        )
                    });
                let summary = DBRowDescriptor::market_summary_header().build_cindexed_rowset("typeID", summary_rows);
                return Some(Ok(PyRep::keyval([
                    ("typeID", PyRep::Integer(PLEX_TYPE_ID as i64)),
                    ("bestAskPrice", PyRep::Real(best_ask)),
                    ("bestBidPrice", PyRep::Real(best_bid)),
                    ("totalAskVolume", PyRep::Integer(ask_vol)),
                    ("totalBidVolume", PyRep::Integer(bid_vol)),
                    ("summary", summary),
                ])));
            }

            "GetPlexOrders" => {
                if let Err(e) = ServerContext::require_character(session) { return Some(Err(e)); }
                let now_unix = ctx.now_unix();
                let (sell_rows, buy_rows) = ctx.db.with_snapshot(|snap| {
                    let mut sells = Vec::new();
                    let mut buys = Vec::new();
                    let mut matching: Vec<&MarketOrderRecord> = snap.market_orders.values()
                        .filter(|o| o.type_id == PLEX_TYPE_ID && o.vol_remaining > 0).collect();
                    matching.sort_by_key(|o| o.order_id);
                    for o in matching {
                        let cells = order_to_packed_cells(snap, o, now_unix);
                        if o.is_bid { buys.push(cells); } else { sells.push(cells); }
                    }
                    (sells, buys)
                });
                let hdr = market_order_descriptor();
                return Some(Ok(PyRep::Tuple(vec![hdr.build_crowset(sell_rows), hdr.build_crowset(buy_rows),])));
            }

            "GetPlexHistory" | "GetPlexNewPriceHistory" | "GetPlexOldPriceHistory" => {
                let now_unix = ctx.now_unix();
                let rows = ctx.db
                    .with_snapshot(|snap| build_price_history_rows(ctx, snap, PLEX_TYPE_ID, None, now_unix));
                return Some(Ok(DBRowDescriptor::market_history_header().build_crowset(rows)));
            }

            "PlacePlexSellOrder" => {
                let (_, char_id) = match ServerContext::require_character(session) {
                    Ok(v) => v,
                    Err(e) => return Some(Err(e)),
                };
                let station_id = session.station_id.or(session.structure_id).unwrap_or(JITA_STATION_ID);
                let region_id = session.region_id.unwrap_or(THE_FORGE_REGION_ID);
                let (price_isk, quantity, duration_days) = match args.first() {
                    Some(PyRep::KeyVal(kv)) => (
                        kv.get("price").and_then(|v| v.as_f64()).unwrap_or(0.0),
                        kv.get("quantity").and_then(|v| v.as_i64()).unwrap_or(0),
                        kv.get("duration").and_then(|v| v.as_i64()).unwrap_or(90),
                    ),
                    _ => (
                        args.first().and_then(|a| a.as_f64()).unwrap_or(0.0),
                        args.get(1).and_then(|a| a.as_i64()).unwrap_or(0),
                        args.get(2).and_then(|a| a.as_i64()).unwrap_or(90),
                    ),
                };
                if !is_valid_4_sig_fig_price(price_isk) {
                    return Some(Err(rpc_err("InvalidPriceTickSize", format!(
                            "PLEX order price {price_isk} violates 4-significant-figure tick size rule"
                        ))));
                }
                if quantity <= 0
                    || quantity > (i32::MAX as i64)
                    || ![0, 1, 3, 7, 14, 30, 90].contains(&duration_days)
                {
                    return Some(Err(rpc_err("InvalidMarketOrder", "PlacePlexSellOrder requires positive quantity and valid duration")));
                }
                let qty_i32 = quantity as i32;
                let order_price = IskCents::from_f64_isk(price_isk);
                let now_unix = ctx.now_unix();
                let tax_collector = super::wallet_serverops::tax_collector_id(ctx);

                let res = ctx.db.try_with_mut_snapshot(|snap| {
                    let sk = snap.skills.get(&char_id);
                    let broker_lvl = sk.and_then(|m| m.get(&3446)).map(|s| s.level).unwrap_or(0);
                    let acct_lvl = sk.and_then(|m| m.get(&16622)).map(|s| s.level).unwrap_or(0);
                    let broker_rate = compute_broker_fee_rate(broker_lvl, 0.0, 0.0);
                    let broker_ppm = (broker_rate * 1_000_000.0).round() as i128;

                    // Consume PLEX from character's PLEX Vault (`plex_balance`) or station hangar item (`44992`)
                    let vault_plex = snap.plex_vault_balance(char_id);
                    if vault_plex >= quantity {
                        let _ = snap.adjust_plex_vault(char_id, -quantity);
                    } else { let hangar_item_id = snap.items.values().find(|i| {
                                i.owner_id == char_id
                                    && i.type_id == PLEX_TYPE_ID
                                    && i.quantity >= qty_i32
                                    && !i.singleton
                            }).map(|i| i.item_id);
                        let Some(hid) = hangar_item_id else {
                            return Err(BridgeRpcError {
                                code: "InsufficientPlexBalance".into(),
                                message: format!(
                                    "Insufficient PLEX in vault ({vault_plex}) or hangar to sell {quantity} PLEX"
                                ),
                            });
                        };
                        if let Some(it) = snap.items.get_mut(&hid) {
                            if it.quantity > qty_i32 {
                                it.quantity -= qty_i32;
                                it.version += 1;
                            } else { snap.items.remove(&hid);
                            }
                        }
                    }

                    // Match globally against any resting PLEX buy orders across all regions
                    let mut candidate_bids: Vec<(i64, i64)> = snap.market_orders.values().filter(|o| {
                            o.type_id == PLEX_TYPE_ID
                                && o.is_bid
                                && o.vol_remaining > 0
                                && o.price.0 >= order_price.0
                        }).map(|o| (o.order_id, o.price.0)).collect();
                    candidate_bids.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

                    let mut rem_qty = qty_i32;
                    let mut first_matched_oid = None;
                    let mut total_tax = IskCents(0);

                    for (bid_oid, _) in candidate_bids {
                        if rem_qty <= 0 { break; }
                        let Some(resting) = snap.market_orders.get(&bid_oid).cloned() else { continue; };
                        let fill_qty = rem_qty.min(resting.vol_remaining);
                        if fill_qty <= 0 { continue; }
                        let Some(fill_gross) = resting.price.checked_mul_qty(fill_qty as i64) else { continue; };
                        let fill_tax = compute_sales_tax_cents(fill_gross, acct_lvl);
                        total_tax = total_tax.checked_add(fill_tax).unwrap_or(total_tax);
                        let seller_net = fill_gross.checked_sub(fill_tax).unwrap_or(IskCents(0));
                        let seller_bal = snap.characters.get_mut(&char_id).map(|seller| {
                            seller.balance = seller.balance.checked_add(seller_net).unwrap_or(seller.balance);
                            seller.balance
                        });
                        snap.journal_market_fill(eve_db::ledger::MarketFill {
                            buyer_id: resting.owner_id,
                            seller_id: char_id,
                            type_id: PLEX_TYPE_ID,
                            quantity: fill_qty as i64,
                            price: resting.price,
                            location_id: resting.station_id,
                            date_unix: now_unix,
                            gross: fill_gross,
                            buyer_balance_after: None,
                            seller_net,
                            seller_balance_after: seller_bal,
                            sales_tax: fill_tax,
                            tax_collector_id: tax_collector,
                        });
                        if !resting.is_npc {
                            let _ = snap.adjust_plex_vault(resting.owner_id, fill_qty as i64);
                        }
                        if let Some(o_mut) = snap.market_orders.get_mut(&bid_oid) {
                            o_mut.vol_remaining -= fill_qty;
                            o_mut.escrow = o_mut.escrow.checked_sub(fill_gross).unwrap_or(IskCents(0));
                        }
                        rem_qty -= fill_qty;
                        if first_matched_oid.is_none() { first_matched_oid = Some(bid_oid); }
                    }

                    let rem_vault = snap.plex_vault_balance(char_id);

                    if rem_qty == 0 {
                        return Ok((first_matched_oid.unwrap_or(0), true, IskCents(0), total_tax, rem_vault,));
                    }
                    if duration_days == 0 {
                        return Err(rpc_err("ImmediateOrderUnfilled", "Immediate PLEX sell order could not be completely filled"));
                    }
                    let resting_cost = order_price.checked_mul_qty(rem_qty as i64)
                        .ok_or_else(|| rpc_err("MarketOrderOverflow", "PLEX order cost overflow"))?;
                    let broker_fee = IskCents(
                        (((resting_cost.0 as i128) * broker_ppm + 500_000) / 1_000_000) as i64,
                    ).max(IskCents(10_000));
                    let caller_bal = snap.characters.get(&char_id).map(|c| c.balance).unwrap_or(IskCents(0));
                    if caller_bal.0 < broker_fee.0 {
                        return Err(rpc_err("InsufficientFunds", "Not enough ISK for PLEX sell order broker fee"));
                    }
                    if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                        ch_mut.balance = ch_mut.balance.checked_sub(broker_fee).unwrap_or(IskCents(0));
                        let bal = ch_mut.balance;
                        snap.append_journal_entry(eve_db::ledger::JournalEntryInput {
                            owner_id: char_id,
                            entry_type: "MarketBrokerFee",
                            amount: IskCents(-broker_fee.0),
                            balance_after: bal,
                            reason: "Broker fee for PLEX sell order".into(),
                            meta: eve_db::ledger::JournalMeta::new(eve_db::ledger::ref_types::BROKER_FEE, char_id, 0, now_unix),
                        });
                    }
                    snap.next_entity_id += 1;
                    let new_oid = snap.next_entity_id;
                    snap.market_orders.insert(
                        new_oid,
                        MarketOrderRecord {
                            order_id: new_oid,
                            owner_id: char_id,
                            station_id,
                            region_id,
                            type_id: PLEX_TYPE_ID,
                            is_bid: false,
                            price: order_price,
                            vol_entered: qty_i32,
                            vol_remaining: rem_qty,
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
                            order_state: None,
                            account_key: None,
                            final_vol_remaining: None,
                            closed_unix: None,
                        },
                    );
                    Ok((new_oid, false, broker_fee, total_tax, rem_vault))
                });

                return match res {
                    Ok((oid, filled_imm, broker_fee, sales_tax, rem_vault)) => {
                        notify_char(ctx, 
                            char_id,
                            "OnOwnOrdersChanged",
                            vec![PyRep::Integer(oid), PyRep::String("Add".into()), PyRep::Bool(false),],
                        );
                        Some(Ok(PyRep::keyval([
                            ("orderID", PyRep::Integer(oid)),
                            ("typeID", PyRep::Integer(PLEX_TYPE_ID as i64)),
                            ("filledImmediately", PyRep::Bool(filled_imm)),
                            ("brokerFeeISK", PyRep::Real(broker_fee.to_wire_f64())),
                            ("salesTaxISK", PyRep::Real(sales_tax.to_wire_f64())),
                            ("plexRemainingVault", PyRep::Integer(rem_vault)),
                        ])))
                    }
                    Err(e) => Some(Err(e)),
                };
            }

            _ => return super::market_trading::handle(ctx, session, service, method, args, _kwargs),
        }
    }

    match (service, method) {
        ("account", "GetCashBalance") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let bal = ctx.db.get_character(char_id).map(|c| c.balance.to_wire_f64()).unwrap_or(0.0);
            Some(Ok(PyRep::Real(bal)))
        }

        ("vaultManager", "get_account_balance") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            // Account-wide PLEX Vault (`vaultManager.get_account_balance(PLX_CURRENCY)`).
            let plex_bal = ctx.db.with_snapshot(|snap| snap.plex_vault_balance(char_id));
            Some(Ok(PyRep::Integer(plex_bal)))
        }

        _ => None,
    }
}
