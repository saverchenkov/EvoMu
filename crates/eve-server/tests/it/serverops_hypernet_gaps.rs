//! Integration tests for HyperNet Relay MechanicRng multi-buyer winner selection.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::{HashMap, HashSet};

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn login_pilot_char(
    ctx: &ServerContext,
    sess_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

fn get_balance(ctx: &ServerContext, char_id: i64) -> IskCents {
    ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).map(|c| c.balance).unwrap_or(IskCents(0))
    })
}

#[tokio::test]
// parity: feat:wallet.hypernet-relay, rpc:raffleProxy.BuyRandomTickets
async fn test_hypernet_mechanic_rng_multi_buyer_distribution() {
    eve_testkit::attest(&["feat:wallet.hypernet-relay", "rpc:raffleProxy.BuyRandomTickets"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = login_pilot_char(&ctx, 1101, "demo", "demo-secret", P1).await;
    let mut s2 = login_pilot_char(&ctx, 1102, "other", "other-secret", P2).await;

    // Seed test RNG for reproducible multi-raffle execution
    ctx.mechanic_rng.set_seed(Some(987654321));

    // Setup inventory: P1 has items to raffle and hypercores; both pilots have ISK.
    let item_ids = [998_001_i64, 998_002_i64, 998_003_i64, 998_004_i64];
    let hypercore_item = 998_010_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for &id in &item_ids {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: 587, // Rifter
                    owner_id: P1,
                    location_id: 60003760,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            hypercore_item,
            InventoryItemRecord {
                item_id: hypercore_item,
                type_id: 52568, // HyperCore
                owner_id: P1,
                location_id: 60003760,
                flag: InventoryFlag::Hangar,
                quantity: 200,
                singleton: false,
                ..Default::default()
            },
        );
        if let Some(c) = snap.characters.get_mut(&P1) {
            c.balance = IskCents::from_isk(10_000_000);
        }
        if let Some(c) = snap.characters.get_mut(&P2) {
            c.balance = IskCents::from_isk(10_000_000);
        }
    });

    let mut winners = HashSet::new();
    let mut selected_ticket_sets: HashSet<Vec<i64>> = HashSet::new();

    let ticket_price_cents = 10_000_00_i64; // 10,000 ISK per ticket
    let four_tickets_cost = IskCents(ticket_price_cents * 4); // 40,000 ISK
    let total_pot = IskCents(ticket_price_cents * 8); // 80,000 ISK
    let fee = IskCents((total_pot.0 / 100) * 5); // 4,000 ISK (5% fee)
    let creator_payout = total_pot.checked_sub(fee).unwrap(); // 76,000 ISK (95%)

    // Run multiple raffles where P1 and P2 each purchase 4 out of 8 tickets.
    for &raffle_item in &item_ids {
        let bal_p1_before_raffle = get_balance(&ctx, P1);
        let bal_p2_before_raffle = get_balance(&ctx, P2);

        let c_raf = rpc(
            &ctx,
            &mut s1,
            "raffleProxy",
            "CreateRaffle",
            &[PyRep::Dict(vec![
                (PyRep::String("itemID".into()), PyRep::Integer(raffle_item)),
                (PyRep::String("ticketCount".into()), PyRep::Integer(8)),
                (PyRep::String("ticketPrice".into()), PyRep::Real(10_000.0)),
            ])],
        )
        .await
        .expect("CreateRaffle");
        let PyRep::KeyVal(r_kv) = c_raf else { panic!("expected KeyVal"); };
        let raffle_id = r_kv.get("raffle_id").and_then(|v| v.as_i64()).unwrap();

        // Distinct buyer 1 (P1) buys 4 random tickets
        let buy1 = rpc(
            &ctx,
            &mut s1,
            "raffleProxy",
            "BuyRandomTickets",
            &[PyRep::Integer(raffle_id), PyRep::Integer(4)],
        )
        .await
        .expect("BuyRandomTickets P1");
        let PyRep::KeyVal(b1_kv) = &buy1 else { panic!("expected KeyVal"); };
        if let Some(PyRep::Dict(entries)) = b1_kv.get("sold_tickets") {
            let mut bought: Vec<i64> = entries.iter().filter_map(|(k, _)| k.as_i64()).collect();
            bought.sort_unstable();
            assert_eq!(bought.len(), 4);
            selected_ticket_sets.insert(bought);
        }

        let bal_p1_after_p1_buy = get_balance(&ctx, P1);
        // Assert buyer P1 is debited exactly 4 * ticket price
        assert_eq!(
            bal_p1_before_raffle.checked_sub(bal_p1_after_p1_buy),
            Some(four_tickets_cost),
            "Buyer P1 must be debited exactly 4 * ticket price"
        );

        // Distinct buyer 2 (P2) buys 4 tickets (completing the 8-ticket raffle)
        let buy2 = rpc(
            &ctx,
            &mut s2,
            "raffleProxy",
            "BuyRandomTickets",
            &[PyRep::Integer(raffle_id), PyRep::Integer(4)],
        )
        .await
        .expect("BuyRandomTickets P2");
        assert!(matches!(buy2, PyRep::KeyVal(_)));

        let bal_p2_after_p2_buy = get_balance(&ctx, P2);
        // Assert buyer P2 is debited exactly 4 * ticket price
        assert_eq!(
            bal_p2_before_raffle.checked_sub(bal_p2_after_p2_buy),
            Some(four_tickets_cost),
            "Buyer P2 must be debited exactly 4 * ticket price"
        );

        let bal_p1_after_completion = get_balance(&ctx, P1);
        // Assert creator P1 is credited exactly 95% of total pot (total_pot - fee)
        assert_eq!(
            bal_p1_after_completion.checked_sub(bal_p1_after_p1_buy),
            Some(creator_payout),
            "Creator P1 must be credited exactly 95% of total pot upon raffle completion"
        );

        // Assert 5% fee is deducted (total ISK across creator and buyers drops by exactly the 5% fee)
        let total_before = bal_p1_before_raffle.0 + bal_p2_before_raffle.0;
        let total_after = bal_p1_after_completion.0 + bal_p2_after_p2_buy.0;
        assert_eq!(
            total_before - total_after,
            fee.0,
            "Total ISK across creator and buyers must drop by exactly the 5% fee sink"
        );

        // Verify completed raffle state
        let raf_finished = rpc(&ctx, &mut s1, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)])
            .await
            .expect("GetRaffle finished");
        let PyRep::KeyVal(f_kv) = raf_finished else { panic!("expected KeyVal"); };
        assert_eq!(f_kv.get("tickets_remaining").and_then(|v| v.as_i64()), Some(0));

        let win_ticket = f_kv.get("winning_ticket").and_then(|v| v.as_i64()).unwrap();
        assert!((0..8).contains(&win_ticket));

        let winner_id = f_kv.get("winner_id").and_then(|v| v.as_i64()).unwrap();
        assert!(winner_id == P1 || winner_id == P2);
        winners.insert(winner_id);

        // Award the item to whoever won
        let mut winner_sess = if winner_id == P1 { s1.clone() } else { s2.clone() };
        let award = rpc(&ctx, &mut winner_sess, "raffleProxy", "AwardItem", &[PyRep::Integer(raffle_id)])
            .await
            .expect("AwardItem");
        assert_eq!(award, PyRep::Integer(raffle_item));
        ctx.db.with_snapshot(|snap| {
            let it = snap.items.get(&raffle_item).expect("raffle item in DB");
            assert_eq!(it.owner_id, winner_id);
        });
    }

    // Verify non-trivial random ticket distribution across distinct buyers:
    // Both buyers won at least one raffle across the sample, and BuyRandomTickets drew distinct ticket index sets.
    assert!(
        winners.contains(&P1) && winners.contains(&P2),
        "MechanicRng multi-buyer raffle selection must award wins to distinct buyers across multiple raffles; observed winners: {:?}",
        winners
    );
    assert!(
        selected_ticket_sets.len() > 1,
        "BuyRandomTickets must select randomized unsold ticket indices rather than fixed 0..count; observed sets: {:?}",
        selected_ticket_sets
    );
}

#[tokio::test]
// parity: feat:wallet.hypernet-hypercore-sizing-expiry, rpc:raffleProxy.CreateRaffle
async fn test_hypernet_create_raffle_validation_and_hypercore_expiry() {
    eve_testkit::attest(&[
        "feat:wallet.hypernet-hypercore-sizing-expiry",
        "rpc:raffleProxy.CreateRaffle",
    ]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = login_pilot_char(&ctx, 1103, "demo", "demo-secret", P1).await;
    let mut s2 = login_pilot_char(&ctx, 1104, "other", "other-secret", P2).await;

    let valid_item = 998_101_i64;
    let stacked_item = 998_102_i64;
    let hypercore_item = 998_110_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            valid_item,
            InventoryItemRecord {
                item_id: valid_item,
                type_id: 587,
                owner_id: P1,
                location_id: 60003760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.items.insert(
            stacked_item,
            InventoryItemRecord {
                item_id: stacked_item,
                type_id: 587,
                owner_id: P1,
                location_id: 60003760,
                flag: InventoryFlag::Hangar,
                quantity: 5,
                singleton: false,
                ..Default::default()
            },
        );
        snap.items.insert(
            hypercore_item,
            InventoryItemRecord {
                item_id: hypercore_item,
                type_id: 52568, // HyperCore
                owner_id: P1,
                location_id: 60003760,
                flag: InventoryFlag::Hangar,
                quantity: 20,
                singleton: false,
                ..Default::default()
            },
        );
        if let Some(c) = snap.characters.get_mut(&P2) {
            c.balance = IskCents::from_isk(50_000_000);
        }
    });

    // 1. Reject raffle creation when docked in a player structure
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P1) {
            c.station_id = None;
            c.structure_id = Some(1_000_000_000_001);
        }
    });
    let err_struct = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(valid_item)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(8)),
            (PyRep::String("ticket_price".into()), PyRep::Real(1_000_000.0)),
        ])],
    )
    .await
    .unwrap_err();
    assert_eq!(err_struct.code, "InvalidRaffleLocation");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&P1) {
            c.station_id = Some(60003760);
            c.structure_id = None;
        }
    });

    // 2. Reject stacked items (quantity > 1)
    let err_stacked = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(stacked_item)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(8)),
            (PyRep::String("ticket_price".into()), PyRep::Real(1_000_000.0)),
        ])],
    )
    .await
    .unwrap_err();
    assert_eq!(err_stacked.code, "InvalidRaffleItem");

    // 3. Reject invalid node counts and >3 day durations
    let err_nodes = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(valid_item)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(12)),
            (PyRep::String("ticket_price".into()), PyRep::Real(1_000_000.0)),
        ])],
    )
    .await
    .unwrap_err();
    assert_eq!(err_nodes.code, "InvalidRaffleTicketCount");

    let err_dur = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(valid_item)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(8)),
            (PyRep::String("ticket_price".into()), PyRep::Real(1_000_000.0)),
            (PyRep::String("duration".into()), PyRep::Integer(259_201)),
        ])],
    )
    .await
    .unwrap_err();
    assert_eq!(err_dur.code, "InvalidRaffleDuration");

    // 4. Reject when required HyperCores exceed hangar stock (48 nodes * 5M ISK = 240M ISK => >20 cores)
    let err_cores = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(valid_item)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(48)),
            (PyRep::String("ticket_price".into()), PyRep::Real(5_000_000.0)),
        ])],
    )
    .await
    .unwrap_err();
    assert_eq!(err_cores.code, "InsufficientHyperCores");

    // 5. Create valid 48-node raffle at 1.25M ISK/ticket (60M total => consumes >1 HyperCore)
    let created = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(valid_item)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(48)),
            (PyRep::String("ticket_price".into()), PyRep::Real(1_250_000.0)),
        ])],
    )
    .await
    .expect("CreateRaffle 48-node");
    let PyRep::KeyVal(r_kv) = created else { panic!("expected KeyVal"); };
    let raffle_id = r_kv.get("raffle_id").and_then(|v| v.as_i64()).unwrap();
    let cores_used = r_kv.get("hypercores_used").and_then(|v| v.as_i64()).unwrap();
    assert!(cores_used >= 2, "Expected >=2 HyperCores consumed for 60M 48-node raffle, got {cores_used}");

    let cores_after_create = ctx.db.with_snapshot(|snap| snap.items[&hypercore_item].quantity);
    assert_eq!(cores_after_create, 20 - (cores_used as i32));

    // 6. P2 buys 2 tickets, then raffle expires after 72h: buyer refunded, item returned, HyperCores NOT refunded
    let p2_before_buy = get_balance(&ctx, P2);
    rpc(
        &ctx,
        &mut s2,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(raffle_id), PyRep::Integer(2)],
    )
    .await
    .expect("BuyRandomTickets");
    assert_eq!(
        p2_before_buy.0 - get_balance(&ctx, P2).0,
        IskCents::from_isk(2_500_000).0
    );

    ctx.advance_virtual_clock(72 * 3_600 + 1);
    let expired = rpc(&ctx, &mut s1, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)])
        .await
        .expect("GetRaffle expiry sweep");
    let PyRep::KeyVal(e_kv) = expired else { panic!("expected KeyVal"); };
    assert_eq!(e_kv.get("is_expired"), Some(&PyRep::Bool(true)));
    assert_eq!(get_balance(&ctx, P2), p2_before_buy, "Expired raffle must refund ticket buyer");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items[&valid_item].owner_id, P1, "Expired raffle must return item to creator");
        assert_eq!(snap.items[&hypercore_item].quantity, 20 - (cores_used as i32), "HyperCores must remain consumed after expiry");
    });
}

#[tokio::test]
// parity: rpc:raffleProxy.GetActiveHistoricPrices, rpc:raffleProxy.BuyRandomTickets, rpc:raffleProxy.CreateRaffle
async fn test_hypernet_active_historic_prices_and_random_tickets() {
    eve_testkit::attest(&[
        "rpc:raffleProxy.GetActiveHistoricPrices",
        "rpc:raffleProxy.BuyRandomTickets",
        "rpc:raffleProxy.CreateRaffle",
    ]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.mechanic_rng.set_seed(Some(123456789));

    let mut s1 = login_pilot_char(&ctx, 1105, "demo", "demo-secret", P1).await;
    let mut s2 = login_pilot_char(&ctx, 1106, "other", "other-secret", P2).await;

    // Use Merlin (type_id = 603) so there are no pre-existing raffles for this type
    let merlin_type_id = 603_i64;
    let initial = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "GetActiveHistoricPrices",
        &[PyRep::Integer(merlin_type_id)],
    )
    .await
    .expect("initial GetActiveHistoricPrices");
    let PyRep::Tuple(init_tup) = initial else { panic!("expected Tuple"); };
    assert_eq!(init_tup.len(), 2);
    let PyRep::Real(base_active) = init_tup[0] else { panic!("expected Real"); };
    let PyRep::Real(base_historic) = init_tup[1] else { panic!("expected Real"); };
    assert!(base_active > 0.0 && (base_active - base_historic).abs() < 1e-6);

    let item_a = 998_201_i64;
    let item_b = 998_202_i64;
    let cores_id = 998_210_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for &id in &[item_a, item_b] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: merlin_type_id as u32,
                    owner_id: P1,
                    location_id: 60003760,
                    flag: InventoryFlag::Hangar,
                    quantity: 1,
                    singleton: true,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            cores_id,
            InventoryItemRecord {
                item_id: cores_id,
                type_id: 52568,
                owner_id: P1,
                location_id: 60003760,
                flag: InventoryFlag::Hangar,
                quantity: 50,
                singleton: false,
                ..Default::default()
            },
        );
        if let Some(c) = snap.characters.get_mut(&P2) {
            c.balance = IskCents::from_isk(50_000_000);
        }
    });

    // Create two active raffles: 400,000 ISK total and 1,200,000 ISK total (mean active = 800,000 ISK)
    let r_a = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(item_a)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(8)),
            (PyRep::String("ticket_price".into()), PyRep::Real(50_000.0)),
        ])],
    )
    .await
    .expect("CreateRaffle A");
    let PyRep::KeyVal(kv_a) = r_a else { panic!("expected KeyVal"); };
    let raf_a = kv_a.get("raffle_id").and_then(|v| v.as_i64()).unwrap();

    let r_b = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "CreateRaffle",
        &[PyRep::Dict(vec![
            (PyRep::String("item_id".into()), PyRep::Integer(item_b)),
            (PyRep::String("ticket_count".into()), PyRep::Integer(8)),
            (PyRep::String("ticket_price".into()), PyRep::Real(150_000.0)),
        ])],
    )
    .await
    .expect("CreateRaffle B");
    let PyRep::KeyVal(kv_b) = r_b else { panic!("expected KeyVal"); };
    let raf_b = kv_b.get("raffle_id").and_then(|v| v.as_i64()).unwrap();

    let mid = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "GetActiveHistoricPrices",
        &[PyRep::Integer(merlin_type_id)],
    )
    .await
    .expect("mid GetActiveHistoricPrices");
    let PyRep::Tuple(mid_tup) = mid else { panic!("expected Tuple"); };
    let (PyRep::Real(mid_active), PyRep::Real(mid_historic)) = (mid_tup[0].clone(), mid_tup[1].clone()) else {
        panic!("expected Reals");
    };
    assert!((mid_active - 800_000.0).abs() < 1e-3, "Active mean should be 800,000 ISK, got {mid_active}");
    assert!((mid_historic - base_historic).abs() < 1e-3, "Historic should still be SDE fallback before any raffle completes");

    // Complete Raffle A (400,000 ISK) via BuyRandomTickets
    rpc(
        &ctx,
        &mut s2,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(raf_a), PyRep::Integer(8)],
    )
    .await
    .expect("BuyRandomTickets complete A");

    let after_a = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "GetActiveHistoricPrices",
        &[PyRep::Integer(merlin_type_id)],
    )
    .await
    .expect("after A GetActiveHistoricPrices");
    let PyRep::Tuple(after_a_tup) = after_a else { panic!("expected Tuple"); };
    let (PyRep::Real(act_a), PyRep::Real(hist_a)) = (after_a_tup[0].clone(), after_a_tup[1].clone()) else {
        panic!("expected Reals");
    };
    assert!((act_a - 1_200_000.0).abs() < 1e-3, "Only Raffle B (1.2M) remains active, got {act_a}");
    assert!((hist_a - 400_000.0).abs() < 1e-3, "Completed Raffle A (400k) sets historic mean, got {hist_a}");

    // Complete Raffle B (1,200,000 ISK) -> historic mean becomes (400k + 1.2M) / 2 = 800,000 ISK
    rpc(
        &ctx,
        &mut s2,
        "raffleProxy",
        "BuyRandomTickets",
        &[PyRep::Integer(raf_b), PyRep::Integer(8)],
    )
    .await
    .expect("BuyRandomTickets complete B");

    let after_b = rpc(
        &ctx,
        &mut s1,
        "raffleProxy",
        "GetActiveHistoricPrices",
        &[PyRep::Integer(merlin_type_id)],
    )
    .await
    .expect("after B GetActiveHistoricPrices");
    let PyRep::Tuple(after_b_tup) = after_b else { panic!("expected Tuple"); };
    let (PyRep::Real(act_b), PyRep::Real(hist_b)) = (after_b_tup[0].clone(), after_b_tup[1].clone()) else {
        panic!("expected Reals");
    };
    assert!((act_b - base_active).abs() < 1e-3, "No active raffles left; active falls back to SDE base price, got {act_b}");
    assert!((hist_b - 800_000.0).abs() < 1e-3, "Historic mean of 400k and 1.2M completed raffles must be 800k, got {hist_b}");
}
