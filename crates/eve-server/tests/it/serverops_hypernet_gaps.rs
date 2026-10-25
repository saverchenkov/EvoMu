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
// parity: feat:wallet.hypernet-relay
async fn test_hypernet_mechanic_rng_multi_buyer_distribution() {
    eve_testkit::attest(&["feat:wallet.hypernet-relay"]);
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

        // Distinct buyer 1 (P1) buys 4 tickets
        let buy1 = rpc(
            &ctx,
            &mut s1,
            "raffleProxy",
            "BuyRandomTickets",
            &[PyRep::Integer(raffle_id), PyRep::Integer(4)],
        )
        .await
        .expect("BuyRandomTickets P1");
        assert!(matches!(buy1, PyRep::KeyVal(_)));

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
    // Both buyers won at least one raffle across the sample
    assert!(
        winners.contains(&P1) && winners.contains(&P2),
        "MechanicRng multi-buyer raffle selection must award wins to distinct buyers across multiple raffles; observed winners: {:?}",
        winners
    );
}
