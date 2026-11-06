//! Deep branch-coverage and mutation-killing unit tests for `hypernet_raffle_ops`.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::account_services::account_services_tests::{
    auth_session, test_ctx, PyRepTestExt,
};
use crate::services::hypernet_raffle_ops::*;
use crate::services::wallet_services_ext::save_json_note;
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, MarketHistoryDayRecord, MarketOrderRecord,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::{BTreeMap, HashMap};

fn call_raffle(
    ctx: &ServerContext,
    session: &mut SessionState,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    handle(ctx, session, "raffleProxy", method, args, kwargs).expect("raffleProxy method matched")
}

#[test]
fn hypercore_sizing_expired_raffle_sweep_and_active_historic_price_fallbacks() {
    for &sz in &ALLOWED_RAFFLE_SIZES {
        assert!(compute_required_hypercores(10_000_000.0, sz).unwrap() >= 1);
    }
    assert_eq!(
        compute_required_hypercores(10_000.0, 32).unwrap_err().code,
        "InvalidRaffleTicketCount"
    );
    assert_eq!(
        compute_required_hypercores(f64::INFINITY, 8)
            .unwrap_err()
            .code,
        "InvalidRafflePrice"
    );

    let ctx = test_ctx();
    let creator_id = 960_001_i64;
    let buyer_id = 960_002_i64;
    let station_id = 60_003_760_i64;
    let item_id = 960_100_i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().unwrap();
        for (cid, bal_isk) in [(creator_id, 10_000_000.0), (buyer_id, 5_000_000.0)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.balance = IskCents::from_f64_isk(bal_isk);
            snap.characters.insert(cid, ch);
        }
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 587,
                owner_id: -52_568,
                location_id: -52_568,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Seed an expired raffle (raffle_id = 701) with 2 sold tickets to buyer_id
        let expired_rec = RaffleStateRecord {
            raffle_id: 701,
            creator_id,
            station_id,
            item_id,
            type_id: 587,
            ticket_count: 8,
            ticket_price_cents: 100_000_000, // 1,000,000 ISK per ticket
            hypercores_used: 1,
            created_unix: now.saturating_sub(300_000),
            expires_unix: now.saturating_sub(1),
            sold_tickets: BTreeMap::from([(0, buyer_id), (1, buyer_id)]),
            winning_ticket: None,
            winner_id: None,
            item_awarded: false,
            expired: false,
        };
        save_json_note(
            snap,
            raffle_note_id(701),
            -74,
            701,
            "raffle:701".into(),
            &expired_rec,
            now,
        );
    });

    // sweep_expired_raffles refunds 2,000,000 ISK to buyer_id and returns item_id to creator_id's Hangar
    assert_eq!(sweep_expired_raffles(&ctx), 1);
    assert_eq!(sweep_expired_raffles(&ctx), 0);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters[&buyer_id].balance,
            IskCents::from_f64_isk(7_000_000.0)
        );
        let it = &snap.items[&item_id];
        assert_eq!(it.owner_id, creator_id);
        assert_eq!(it.location_id, station_id);
        assert_eq!(it.flag, InventoryFlag::Hangar);
    });

    // Test GetActiveHistoricPrices fallback chains:
    // 1. Sell market_orders for active_avg, volume-weighted market_history for historic_avg
    let test_tid = 588_u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            960_501,
            MarketOrderRecord {
                order_id: 960_501,
                owner_id: creator_id,
                station_id,
                region_id: 10_000_002,
                type_id: test_tid,
                is_bid: false,
                price: IskCents::from_f64_isk(2_500_000.0),
                vol_entered: 5,
                vol_remaining: 5,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.market_history.insert(
            (10_000_002, test_tid),
            vec![
                MarketHistoryDayRecord {
                    date: "2026-11-01".into(),
                    lowest: 1_000_000.0,
                    highest: 2_000_000.0,
                    average: 1_500_000.0,
                    volume: 2,
                    order_count: 1,
                    open_price: Some(1_500_000.0),
                    close_price: Some(1_500_000.0),
                },
                MarketHistoryDayRecord {
                    date: "2026-11-02".into(),
                    lowest: 3_000_000.0,
                    highest: 3_000_000.0,
                    average: 3_000_000.0,
                    volume: 1,
                    order_count: 1,
                    open_price: Some(3_000_000.0),
                    close_price: Some(3_000_000.0),
                },
            ],
        );
    });

    let mut sess = auth_session(1, creator_id, 98_000_001, 30_000_142);
    let mut kw_tid = HashMap::new();
    kw_tid.insert("typeID".into(), PyRep::Integer(test_tid as i64));
    let PyRep::Tuple(tup) =
        call_raffle(&ctx, &mut sess, "GetActiveHistoricPrices", &[], &kw_tid).unwrap()
    else {
        panic!("expected tuple");
    };
    let act_p = tup[0].as_f64().unwrap();
    let hist_p = tup[1].as_f64().unwrap();
    assert!((act_p - 2_500_000.0).abs() < 1e-3);
    // Weighted average: (1.5M * 2 + 3.0M * 1) / 3 == 2.0M
    assert!((hist_p - 2_000_000.0).abs() < 1e-3);

    // 2. When active and finished raffles exist for test_tid, they override market_orders & market_history
    ctx.db.with_mut_snapshot(|snap| {
        let active_r = RaffleStateRecord {
            raffle_id: 702,
            creator_id,
            station_id,
            item_id: 960_101,
            type_id: test_tid,
            ticket_count: 8,
            ticket_price_cents: 50_000_000, // 8 * 500k = 4.0M
            hypercores_used: 1,
            created_unix: now,
            expires_unix: now + 3_600,
            sold_tickets: BTreeMap::new(),
            winning_ticket: None,
            winner_id: None,
            item_awarded: false,
            expired: false,
        };
        let won_r = RaffleStateRecord {
            raffle_id: 703,
            creator_id,
            station_id,
            item_id: 960_102,
            type_id: test_tid,
            ticket_count: 8,
            ticket_price_cents: 100_000_000, // 8 * 1M = 8.0M
            hypercores_used: 2,
            created_unix: now,
            expires_unix: now + 3_600,
            sold_tickets: BTreeMap::new(),
            winning_ticket: Some(0),
            winner_id: Some(buyer_id),
            item_awarded: true,
            expired: false,
        };
        save_json_note(snap, raffle_note_id(702), -74, 702, "raffle:702".into(), &active_r, now);
        save_json_note(snap, raffle_note_id(703), -74, 703, "raffle:703".into(), &won_r, now);
    });
    let PyRep::Tuple(tup2) =
        call_raffle(&ctx, &mut sess, "GetActiveHistoricPrices", &[], &kw_tid).unwrap()
    else {
        panic!("expected tuple");
    };
    assert!((tup2[0].as_f64().unwrap() - 4_000_000.0).abs() < 1e-3);
    assert!((tup2[1].as_f64().unwrap() - 8_000_000.0).abs() < 1e-3);
}

#[test]
fn create_raffle_validation_branches_dict_keyval_buy_errors_and_filtered_grab_matrix() {
    let ctx = test_ctx();
    let creator_id = 961_001_i64;
    let buyer_id = 961_002_i64;
    let station_id = 60_003_760_i64;
    let ship_id = 961_099_i64;
    let item_bpo = 961_101_i64;
    let item_bpc = 961_102_i64;
    let child_id = 961_103_i64;
    let core_id = 961_200_i64;

    let mut creator_sess = auth_session(1, creator_id, 98_000_001, 30_000_142);
    creator_sess.station_id = Some(station_id);
    let mut buyer_sess = auth_session(2, buyer_id, 98_000_001, 30_000_142);
    buyer_sess.station_id = Some(station_id);
    let kw = HashMap::new();

    ctx.db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.values().next().cloned().unwrap();
        for (cid, bal_isk) in [(creator_id, 100_000_000.0), (buyer_id, 100_000_000.0)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid;
            ch.station_id = Some(station_id);
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
            ch.balance = IskCents::from_f64_isk(bal_isk);
            snap.characters.insert(cid, ch);
        }
        for (iid, is_bpc) in [(item_bpo, false), (item_bpc, true)] {
            snap.items.insert(
                iid,
                InventoryItemRecord {
                    item_id: iid,
                    type_id: 587,
                    owner_id: creator_id,
                    location_id: station_id,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    is_blueprint_copy: is_bpc,
                    ..Default::default()
                },
            );
        }
    });

    // 1. Docked in Upwell structure or invalid station_id -> InvalidRaffleLocation
    creator_sess.structure_id = Some(1_000_000_001);
    assert_eq!(
        call_raffle(
            &ctx,
            &mut creator_sess,
            "CreateRaffle",
            &[PyRep::Integer(item_bpo), PyRep::Integer(8), PyRep::Real(100_000.0)],
            &kw,
        )
        .unwrap_err()
        .code,
        "InvalidRaffleLocation"
    );
    creator_sess.structure_id = None;
    creator_sess.station_id = Some(999_999_999);
    assert_eq!(
        call_raffle(
            &ctx,
            &mut creator_sess,
            "CreateRaffle",
            &[PyRep::Integer(item_bpo), PyRep::Integer(8), PyRep::Real(100_000.0)],
            &kw,
        )
        .unwrap_err()
        .code,
        "InvalidRaffleLocation"
    );
    creator_sess.station_id = Some(station_id);

    // 2. Missing item -> ItemNotFound; non-empty container -> InvalidRaffleItem; missing HyperCores -> InsufficientHyperCores
    assert_eq!(
        call_raffle(
            &ctx,
            &mut creator_sess,
            "CreateRaffle",
            &[PyRep::Integer(999_999_999), PyRep::Integer(8), PyRep::Real(100_000.0)],
            &kw,
        )
        .unwrap_err()
        .code,
        "ItemNotFound"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            child_id,
            InventoryItemRecord {
                item_id: child_id,
                type_id: 34,
                owner_id: creator_id,
                location_id: item_bpo,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            core_id,
            InventoryItemRecord {
                item_id: core_id,
                type_id: HYPERCORE_TYPE_ID,
                owner_id: creator_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 1, // exact 1 core for first raffle
                ..Default::default()
            },
        );
    });
    assert_eq!(
        call_raffle(
            &ctx,
            &mut creator_sess,
            "CreateRaffle",
            &[PyRep::Integer(item_bpo), PyRep::Integer(8), PyRep::Real(100_000.0)],
            &kw,
        )
        .unwrap_err()
        .code,
        "InvalidRaffleItem"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&child_id);
    });

    // 3. CreateRaffle via PyRep::KeyVal (consumes exact 1 HyperCore and removes core_id stack)
    let created_bpo = call_raffle(
        &ctx,
        &mut creator_sess,
        "CreateRaffle",
        &[PyRep::keyval([
            ("item_id", PyRep::Integer(item_bpo)),
            ("ticket_count", PyRep::Integer(8)),
            ("ticket_price", PyRep::Real(100_000.0)),
            ("duration_secs", PyRep::Integer(3_600)),
        ])],
        &kw,
    )
    .expect("create bpo raffle");
    let rid_bpo = created_bpo.dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&core_id));
    });

    // Without HyperCores, second CreateRaffle fails with InsufficientHyperCores
    assert_eq!(
        call_raffle(
            &ctx,
            &mut creator_sess,
            "CreateRaffle",
            &[PyRep::Dict(vec![
                (PyRep::String("itemID".into()), PyRep::Integer(item_bpc)),
                (PyRep::String("ticketCount".into()), PyRep::Integer(16)),
                (PyRep::String("ticketPrice".into()), PyRep::Real(200_000.0)),
            ])],
            &kw,
        )
        .unwrap_err()
        .code,
        "InsufficientHyperCores"
    );

    // Seed more HyperCores and create BPC raffle via PyRep::Dict
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            core_id,
            InventoryItemRecord {
                item_id: core_id,
                type_id: HYPERCORE_TYPE_ID,
                owner_id: creator_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                ..Default::default()
            },
        );
    });
    let created_bpc = call_raffle(
        &ctx,
        &mut creator_sess,
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("itemID".into()), PyRep::Integer(item_bpc)),
            (PyRep::String("ticketCount".into()), PyRep::Integer(16)),
            (PyRep::String("ticketPrice".into()), PyRep::Real(200_000.0)),
        ])],
        &kw,
    )
    .expect("create bpc raffle");
    let rid_bpc = created_bpc.dict_get("raffle_id").and_then(|v| v.as_i64()).unwrap();

    // 4. FilteredGrab with blueprint_type (1 = BPO only, 2 = BPC only), ticket_count, min/max price, solar_system_id, size
    let grab_bpo = call_raffle(
        &ctx,
        &mut buyer_sess,
        "FilteredGrab",
        &[PyRep::keyval([
            ("blueprint_type", PyRep::Integer(1)),
            ("ticket_count", PyRep::Integer(8)),
            ("min_price", PyRep::Real(50_000.0)),
            ("max_price", PyRep::Real(150_000.0)),
            ("solar_system_id", PyRep::Integer(30_000_142)),
        ])],
        &kw,
    )
    .unwrap();
    let bpo_list = grab_bpo.as_list().unwrap();
    assert_eq!(bpo_list.len(), 1);
    assert_eq!(bpo_list[0].dict_get("raffle_id"), Some(&PyRep::Integer(rid_bpo)));

    let grab_bpc = call_raffle(
        &ctx,
        &mut buyer_sess,
        "FilteredGrab",
        &[PyRep::keyval([("blueprintType", PyRep::Integer(2))])],
        &kw,
    )
    .unwrap();
    let bpc_list = grab_bpc.as_list().unwrap();
    assert_eq!(bpc_list.len(), 1);
    assert_eq!(bpc_list[0].dict_get("raffle_id"), Some(&PyRep::Integer(rid_bpc)));

    // 5. BuyTicket / BuyRandomTickets / AwardItem error branches
    assert_eq!(
        call_raffle(
            &ctx,
            &mut buyer_sess,
            "BuyTicket",
            &[PyRep::Integer(rid_bpo), PyRep::Integer(-1)],
            &kw,
        )
        .unwrap_err()
        .code,
        "TicketUnavailable"
    );
    assert_eq!(
        call_raffle(
            &ctx,
            &mut buyer_sess,
            "BuyRandomTickets",
            &[PyRep::Integer(rid_bpo), PyRep::Integer(99)],
            &kw,
        )
        .unwrap_err()
        .code,
        "NotEnoughTickets"
    );
    // Buy 1 ticket on rid_bpo, verify GetMyActiveTickets & GetCreatedParticipated
    call_raffle(
        &ctx,
        &mut buyer_sess,
        "BuyTicket",
        &[PyRep::Integer(rid_bpo), PyRep::Integer(0)],
        &kw,
    )
    .unwrap();
    assert_eq!(
        call_raffle(&ctx, &mut buyer_sess, "GetMyActiveTickets", &[], &kw)
            .unwrap()
            .as_list()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        call_raffle(&ctx, &mut buyer_sess, "GetCreatedParticipated", &[], &kw)
            .unwrap()
            .as_list()
            .unwrap()
            .len(),
        1
    );

    // Non-winner cannot AwardItem
    assert_eq!(
        call_raffle(
            &ctx,
            &mut creator_sess,
            "AwardItem",
            &[PyRep::Integer(rid_bpo)],
            &kw,
        )
        .unwrap_err()
        .code,
        "NotRaffleWinner"
    );
}
