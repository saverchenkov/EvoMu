//! Integration acceptance tests for `#1185`:
//! Ensures 1-unit and cross-region same-UTC-day high-price mineral self-trades
//! cannot manipulate `collect_live_market_adjusted_prices` or `insuranceSvc`
//! (`GetInsurancePrice` / `GetInsurancePrices` / `InsureShip`) into a `+0.70x` Platinum ISK loop.

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, TypeId};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::formulas::{
    compute_ship_insurance_reference_value, ADJUSTED_PRICE_DAILY_BAND, INSURANCE_REF_MAX_MULT,
};
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::industry_rigs::collect_live_market_adjusted_prices_with_sde;
use eve_server::services::station_structure::npc_station_offers_service;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

const PILOT_SELLER: i64 = 90_000_001;
const PILOT_BUYER: i64 = 90_000_002;
const RIFTER_TYPE_ID: TypeId = 587;
const TRITANIUM_TYPE_ID: TypeId = 34;
const ISOGEN_TYPE_ID: TypeId = 37;
const ZYDRINE_TYPE_ID: TypeId = 39;
const MANIPULATION_PRICE_ISK: f64 = 1_000_000_000.0;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn login_pilot_session(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    sess.role = eve_machonet::service_roles::ROLE_GM_DEFAULT;
    ctx.advance_secs(12);
    sess
}

fn dock_session(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    region_id: u32,
    solar_system_id: u32,
    station_id: i64,
) {
    let constellation_id = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.constellation_id)
        .unwrap_or(0);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.region_id = region_id;
            ch.constellation_id = constellation_id;
            ch.solar_system_id = solar_system_id;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
        }
    });
    sess.structure_id = None;
    sess.region_id = Some(region_id);
    if constellation_id != 0 {
        sess.constellation_id = Some(constellation_id);
    }
    sess.solar_system_id = Some(solar_system_id);
    sess.station_id = Some(station_id);
}

fn collect_ten_distinct_region_stations(ctx: &ServerContext) -> Vec<(u32, u32, i64)> {
    let mut by_region: BTreeMap<u32, (u32, i64)> = BTreeMap::new();
    for sys in ctx.sde.solar_systems.values() {
        if sys.region_id >= 11_000_000 || sys.stations.is_empty() {
            continue;
        }
        for st in &sys.stations {
            if st.station_id == 60_003_760 && npc_station_offers_service(&ctx.sde, st.station_id, 21)
            {
                by_region.insert(sys.region_id, (sys.solar_system_id, st.station_id));
                break;
            }
            by_region
                .entry(sys.region_id)
                .or_insert((sys.solar_system_id, st.station_id));
        }
    }
    let mut stations: Vec<(u32, u32, i64)> = by_region
        .into_iter()
        .map(|(reg, (sys, st))| (reg, sys, st))
        .collect();
    // Ensure index 0 offers insurance service (21) so GetInsurancePrice / InsureShip can run there.
    if let Some(ins_idx) = stations
        .iter()
        .position(|&(_, _, st_id)| st_id == 60_003_760)
        .or_else(|| {
            stations
                .iter()
                .position(|&(_, _, st_id)| npc_station_offers_service(&ctx.sde, st_id, 21))
        })
    {
        stations.swap(0, ins_idx);
    }
    assert!(
        stations.len() >= 10,
        "expected at least 10 distinct K-space regions with NPC stations, got {}",
        stations.len()
    );
    assert!(
        npc_station_offers_service(&ctx.sde, stations[0].2, 21),
        "expected station[0] ({}) to offer insurance service 21",
        stations[0].2
    );
    stations.truncate(10);
    stations
}

async fn execute_one_unit_self_trade(
    ctx: &ServerContext,
    s_seller: &mut SessionState,
    s_buyer: &mut SessionState,
    region_id: u32,
    solar_system_id: u32,
    station_id: i64,
    type_id: TypeId,
) {
    dock_session(ctx, s_seller, PILOT_SELLER, region_id, solar_system_id, station_id);
    dock_session(ctx, s_buyer, PILOT_BUYER, region_id, solar_system_id, station_id);

    let item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id,
                owner_id: PILOT_SELLER,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: false,
                ..Default::default()
            },
        );
        id
    });

    rpc(
        ctx,
        s_seller,
        "marketProxy",
        "PlaceMultiSellOrder",
        &[
            PyRep::List(vec![PyRep::keyval([
                ("stationID", PyRep::Integer(station_id)),
                ("typeID", PyRep::Integer(i64::from(type_id))),
                ("itemID", PyRep::Integer(item_id)),
                ("price", PyRep::Real(MANIPULATION_PRICE_ISK)),
                ("quantity", PyRep::Integer(1)),
            ])]),
            PyRep::Bool(false),
            PyRep::Integer(14),
        ],
    )
    .await
    .expect("PlaceMultiSellOrder 1 unit");

    rpc(
        ctx,
        s_buyer,
        "marketProxy",
        "PlaceBuyOrder",
        &[
            PyRep::Integer(station_id),
            PyRep::Integer(i64::from(type_id)),
            PyRep::Real(MANIPULATION_PRICE_ISK),
            PyRep::Integer(1),
            PyRep::Integer(-1),
            PyRep::Integer(1),
            PyRep::Integer(14),
            PyRep::Bool(false),
        ],
    )
    .await
    .expect("PlaceBuyOrder 1 unit");
}

#[tokio::test]
async fn test_insurance_rejects_one_unit_and_ten_region_same_day_mineral_self_trades() {
    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);

    ctx.db
        .provision_gm_account("demo", "demo-secret", PILOT_SELLER, "Seller Pilot");
    ctx.db
        .provision_gm_account("other", "other-secret", PILOT_BUYER, "Buyer Pilot");

    let mut s_seller = login_pilot_session(&ctx, 9801, "demo", "demo-secret", PILOT_SELLER).await;
    let mut s_buyer = login_pilot_session(&ctx, 9802, "other", "other-secret", PILOT_BUYER).await;

    let stations = collect_ten_distinct_region_stations(&ctx);
    let (ins_region_id, ins_sys_id, ins_station_id) = stations[0];

    // Clear pre-seeded mineral sell orders so Buyer matches Seller's 1,000,000,000 ISK orders directly
    // and records real trades in `snap.market_history`, and fund both pilots generously.
    let rifter_item_id = ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| {
            ![TRITANIUM_TYPE_ID, ISOGEN_TYPE_ID, ZYDRINE_TYPE_ID].contains(&o.type_id)
        });
        snap.market_history.clear();
        for cid in [PILOT_SELLER, PILOT_BUYER] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.balance = IskCents::from_isk(1_000_000_000_000);
            }
        }
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: RIFTER_TYPE_ID,
                owner_id: PILOT_SELLER,
                location_id: ins_station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        id
    });

    dock_session(
        &ctx,
        &mut s_seller,
        PILOT_SELLER,
        ins_region_id,
        ins_sys_id,
        ins_station_id,
    );
    let base_ref = compute_ship_insurance_reference_value(&ctx.sde, RIFTER_TYPE_ID);
    assert!(base_ref > 0.0, "Rifter must have positive SDE base_price");

    // 1. 1-region attack: Character A sells 1 unit of Tritanium, Isogen, and Zydrine to Character B
    //    at 1,000,000,000 ISK in stations[0].
    let initial_seller_buyer_sum = ctx.db.with_snapshot(|snap| {
        snap.characters[&PILOT_SELLER].balance.0 + snap.characters[&PILOT_BUYER].balance.0
    });
    for tid in [TRITANIUM_TYPE_ID, ISOGEN_TYPE_ID, ZYDRINE_TYPE_ID] {
        execute_one_unit_self_trade(
            &ctx,
            &mut s_seller,
            &mut s_buyer,
            ins_region_id,
            ins_sys_id,
            ins_station_id,
            tid,
        )
        .await;
    }

    let today_utc = eve_db::ledger::unix_secs_to_date_str(ctx.now_unix());
    ctx.db.with_snapshot(|snap| {
        for tid in [TRITANIUM_TYPE_ID, ISOGEN_TYPE_ID, ZYDRINE_TYPE_ID] {
            let days = snap
                .market_history
                .get(&(ins_region_id, tid))
                .expect("trade must record market_history in region 0");
            assert!(
                days.iter().any(|d| {
                    d.date == today_utc
                        && (d.average - MANIPULATION_PRICE_ISK).abs() < 1e-3
                        && d.volume == 1
                }),
                "expected 1-unit 1B ISK trade in market_history for ({ins_region_id}, {tid})"
            );
        }
        let adj = collect_live_market_adjusted_prices_with_sde(&ctx.sde, snap);
        for tid in [TRITANIUM_TYPE_ID, ISOGEN_TYPE_ID, ZYDRINE_TYPE_ID] {
            let sde_base = ctx.sde.types[&tid].base_price;
            assert!(sde_base > 0.0, "mineral {tid} must have positive SDE base_price");
            assert!(
                (adj[&tid] - sde_base).abs() < 1e-9,
                "type {tid} adjusted price must stay anchored at SDE base_price {sde_base} after 1-unit trade, got {}",
                adj[&tid]
            );
        }
    });

    dock_session(
        &ctx,
        &mut s_seller,
        PILOT_SELLER,
        ins_region_id,
        ins_sys_id,
        ins_station_id,
    );
    let quote_1_reg = rpc(
        &ctx,
        &mut s_seller,
        "insuranceSvc",
        "GetInsurancePrice",
        &[PyRep::Integer(i64::from(RIFTER_TYPE_ID))],
    )
    .await
    .expect("GetInsurancePrice after 1-region self-trade")
    .as_f64()
    .expect("float insurance quote");
    let quote_1_ship = rpc(
        &ctx,
        &mut s_seller,
        "insuranceSvc",
        "GetInsurancePrice",
        &[PyRep::Integer(rifter_item_id)],
    )
    .await
    .expect("GetInsurancePrice by itemID")
    .as_f64()
    .expect("float insurance quote");
    let single_day_upper = base_ref * (1.0 + ADJUSTED_PRICE_DAILY_BAND);
    assert!(
        quote_1_reg <= single_day_upper + 1e-6 && quote_1_reg >= base_ref * 0.50 - 1e-6,
        "1-region 1-unit trade quote {quote_1_reg} exceeded bounded tolerance [0.5x, {single_day_upper}]"
    );
    assert!(
        quote_1_ship <= single_day_upper + 1e-6,
        "ship quote {quote_1_ship} exceeded bounded tolerance"
    );

    // 2. 10-region same-UTC-day attack: repeat the 1-unit 1,000,000,000 ISK self-trade across
    //    9 additional distinct regions (10 total regions on the same UTC calendar date).
    for &(reg_id, sys_id, st_id) in &stations[1..10] {
        for tid in [TRITANIUM_TYPE_ID, ISOGEN_TYPE_ID, ZYDRINE_TYPE_ID] {
            execute_one_unit_self_trade(
                &ctx,
                &mut s_seller,
                &mut s_buyer,
                reg_id,
                sys_id,
                st_id,
                tid,
            )
            .await;
        }
    }

    ctx.db.with_snapshot(|snap| {
        for &(reg_id, _, _) in &stations {
            for tid in [TRITANIUM_TYPE_ID, ISOGEN_TYPE_ID, ZYDRINE_TYPE_ID] {
                let days = snap
                    .market_history
                    .get(&(reg_id, tid))
                    .expect("trade must record market_history in all 10 regions");
                assert!(days.iter().any(|d| {
                    d.date == today_utc
                        && (d.average - MANIPULATION_PRICE_ISK).abs() < 1e-3
                        && d.volume == 1
                }));
            }
        }
        let adj = collect_live_market_adjusted_prices_with_sde(&ctx.sde, snap);
        for tid in [TRITANIUM_TYPE_ID, ISOGEN_TYPE_ID, ZYDRINE_TYPE_ID] {
            let sde_base = ctx.sde.types[&tid].base_price;
            assert!(
                (adj[&tid] - sde_base).abs() < 1e-9,
                "10-region same-day 1-unit trades on type {tid} must stay anchored at SDE base_price {sde_base}, got {}",
                adj[&tid]
            );
        }
    });

    dock_session(
        &ctx,
        &mut s_seller,
        PILOT_SELLER,
        ins_region_id,
        ins_sys_id,
        ins_station_id,
    );
    let quote_10_reg = rpc(
        &ctx,
        &mut s_seller,
        "insuranceSvc",
        "GetInsurancePrice",
        &[PyRep::Integer(i64::from(RIFTER_TYPE_ID))],
    )
    .await
    .expect("GetInsurancePrice after 10-region same-day self-trade")
    .as_f64()
    .expect("float insurance quote");
    assert!(
        quote_10_reg <= base_ref * INSURANCE_REF_MAX_MULT + 1e-6,
        "10-region same-day 1-unit trades must not compound insurance ref_val: {quote_10_reg}"
    );

    let prices_dict = rpc(
        &ctx,
        &mut s_seller,
        "insuranceSvc",
        "GetInsurancePrices",
        &[PyRep::Integer(i64::from(RIFTER_TYPE_ID))],
    )
    .await
    .expect("GetInsurancePrices");
    let PyRep::Dict(entries) = prices_dict else {
        panic!("expected Dict from GetInsurancePrices, got {prices_dict:?}");
    };
    let rifter_dict_val = entries
        .into_iter()
        .find_map(|(k, v)| (k.as_i64() == Some(i64::from(RIFTER_TYPE_ID))).then(|| v.as_f64()))
        .flatten()
        .expect("Rifter entry in GetInsurancePrices");
    assert!(rifter_dict_val <= base_ref * INSURANCE_REF_MAX_MULT + 1e-6);

    // 3. Purchase Platinum insurance (1.00x) and assert self-trade + Platinum loop is strictly non-profitable.
    let insure_res = rpc(
        &ctx,
        &mut s_seller,
        "insuranceSvc",
        "InsureShip",
        &[PyRep::Integer(rifter_item_id), PyRep::Real(1.0)],
    )
    .await
    .expect("InsureShip Platinum");

    let cost_isk = insure_res
        .dict_get("costISK")
        .and_then(|v| v.as_f64())
        .expect("costISK");
    let payout_isk = insure_res
        .dict_get("payoutISK")
        .and_then(|v| v.as_f64())
        .expect("payoutISK");

    assert!(
        payout_isk <= base_ref * INSURANCE_REF_MAX_MULT + 1e-6,
        "Platinum payout {payout_isk} exceeded capped baseline multiple"
    );
    assert!(
        (cost_isk - payout_isk * 0.30).abs() < 1.0,
        "Platinum premium {cost_isk} should equal 0.30 * payout {payout_isk}"
    );
    let net_insurance_return = payout_isk - cost_isk;
    let combined_balance_after = ctx.db.with_snapshot(|snap| {
        snap.characters[&PILOT_SELLER].balance.0 + snap.characters[&PILOT_BUYER].balance.0
    });
    let market_tax_and_fee_sink_isk =
        ((initial_seller_buyer_sum - combined_balance_after) as f64) / 100.0;
    assert!(
        market_tax_and_fee_sink_isk > 0.0,
        "self-trades must pay positive broker/sales tax ISK sink"
    );
    assert!(
        net_insurance_return < base_ref + market_tax_and_fee_sink_isk,
        "Platinum insurance net payout ({net_insurance_return}) must be strictly below hull replacement cost ({base_ref}) + fees ({market_tax_and_fee_sink_isk})"
    );
    assert!(
        net_insurance_return < base_ref,
        "Platinum insurance net payout ({net_insurance_return}) must remain strictly below hull base_price ({base_ref})"
    );
}
