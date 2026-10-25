//! Wallet, market-order lifecycle and server-ops integration tests.
//!
//! Covers personal wallet transfers (`account.GiveCash`), wallet balance + `OnAccountChange`,
//! CSPA charge configuration, partial fills with minimum volume, order modification cooldown,
//! cancellation refunds, skill-driven open-order limits, cross-region `GetCharOrders`, and the
//! server-ops endpoints (`machoNet.GetTime`, `pingService.Ping`,
//! `machoNet.GetClusterGameStatisticsForClient`, client telemetry sinks, `localizationServer`)
//! plus the disabled real-money `FastCheckoutService`.



use crate::common;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, MarketOrderRecord,
    TrainedSkillRecord, TypeId,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use eve_server::services::wallet_serverops::{
    assign_or_reinforce_system_node, compute_required_hypercores, create_bill,
    get_system_tidi_factor, process_due_automatic_bills, run_daily_downtime_and_jobs,
    set_system_tidi_factor, CorpBillRecord, HYPERCORE_TYPE_ID,
};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SYSTEM_ID: i64 = 30_000_142;
const THE_FORGE_REGION_ID: u32 = 10_000_002;
const AMARR_STATION_ID: i64 = 60_008_494;
const DOMAIN_REGION_ID: u32 = 10_000_043;
const TRITANIUM: TypeId = 34;
const PYERITE: TypeId = 35;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn setup_pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn balance(ctx: &ServerContext, char_id: i64) -> IskCents {
    ctx.db.get_character(char_id).unwrap().balance
}

fn kv_f64(rep: &PyRep, key: &str) -> f64 {
    let PyRep::KeyVal(kv) = rep else { panic!("expected KeyVal, got {rep:?}") };
    kv.get(key).and_then(|v| v.as_f64()).unwrap_or_else(|| panic!("missing {key}"))
}

fn kv_i64(rep: &PyRep, key: &str) -> i64 {
    let PyRep::KeyVal(kv) = rep else { panic!("expected KeyVal, got {rep:?}") };
    kv.get(key).and_then(|v| v.as_i64()).unwrap_or_else(|| panic!("missing {key}"))
}

fn set_skill(ctx: &ServerContext, char_id: i64, type_id: TypeId, level: u8) {
    ctx.db.with_mut_snapshot(|snap| {
        let sk = snap.skills.entry(char_id).or_default();
        if level == 0 {
            sk.remove(&type_id);
        } else {
            sk.insert(type_id, TrainedSkillRecord { type_id, level, skill_points: 256_000.0 });
        }
    });
}

fn vol_remaining(ctx: &ServerContext, oid: i64) -> Option<i32> {
    ctx.db.with_snapshot(|snap| snap.market_orders.get(&oid).map(|o| o.vol_remaining))
}

fn sell_args(type_id: TypeId, price: f64, qty: i64) -> Vec<PyRep> {
    vec![
        PyRep::List(vec![PyRep::keyval([
            ("stationID", PyRep::Integer(JITA_STATION_ID)),
            ("typeID", PyRep::Integer(type_id as i64)),
            ("price", PyRep::Real(price)),
            ("quantity", PyRep::Integer(qty)),
        ])]),
        PyRep::Bool(false),
        PyRep::Integer(90),
        PyRep::Real(0.0),
    ]
}

/// V24 `PlaceBuyOrder(stationID, typeID, price, quantity, orderRange, minVolume, duration, useCorp)`.
fn buy_args(type_id: TypeId, price: f64, qty: i64, min_volume: i64) -> Vec<PyRep> {
    vec![
        PyRep::Integer(JITA_STATION_ID),
        PyRep::Integer(type_id as i64),
        PyRep::Real(price),
        PyRep::Integer(qty),
        PyRep::Integer(-1),
        PyRep::Integer(min_volume),
        PyRep::Integer(90),
        PyRep::Bool(false),
    ]
}

fn own_orders_changed(ctx: &ServerContext, char_id: i64, order_id: i64) -> bool {
    ctx.drain_notifications_for_char(char_id)
        .iter()
        .any(|n| n.event == "OnOwnOrdersChanged" && n.args.first().and_then(|a| a.as_i64()) == Some(order_id))
}

#[tokio::test]
async fn test_partial_fill_min_volume_notifications_and_cancel_refund() {
    // parity: feat:market.partial-fill, feat:market.cancel-order
    eve_testkit::attest(&["feat:market.partial-fill", "feat:market.cancel-order"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 701, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 702, "other", "other-secret", P2).await;
    // Full escrow (no Margin Trading) so refunds are exact.
    set_skill(&ctx, P2, 16_597, 0);
    let foreign_bids = ctx.db.with_snapshot(|snap| {
        snap.market_orders.values().any(|o| {
            o.is_bid && o.type_id == TRITANIUM && o.station_id == JITA_STATION_ID && o.price.0 >= 200
        })
    });
    assert!(!foreign_bids, "fixture: no pre-existing Tritanium bids at >= 2.00 ISK in Jita");

    // Buyer posts 100 Tritanium @ 2.00 with minimum volume 50.
    let p2_before_place = balance(&ctx, P2);
    let buy = rpc(&ctx, &mut s2, "marketProxy", "PlaceBuyOrder", &buy_args(TRITANIUM, 2.0, 100, 50))
        .await
        .expect("PlaceBuyOrder with minVolume 50");
    let buy_oid = kv_i64(&buy, "orderID");
    let broker_fee = IskCents::from_f64_isk(kv_f64(&buy, "brokerFeeISK"));
    assert!(broker_fee.0 > 0);
    assert_eq!(kv_i64(&buy, "minVolume"), 50);
    assert_eq!(ctx.db.with_snapshot(|s| s.market_orders[&buy_oid].min_volume), 50);
    let _ = ctx.drain_notifications_for_char(P2);

    // A 10-unit sale is below the order's minimum volume: it must not fill the bid.
    let small = rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &sell_args(TRITANIUM, 2.0, 10))
        .await
        .expect("PlaceMultiSellOrder 10 units");
    let PyRep::KeyVal(small_kv) = &small else { panic!("KeyVal") };
    assert_eq!(small_kv.get("filledImmediately"), Some(&PyRep::Bool(false)));
    assert_eq!(vol_remaining(&ctx, buy_oid), Some(100), "min volume enforced: bid untouched");
    let small_oid = kv_i64(&small, "orderID");
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(small_oid)])
        .await
        .expect("cancel resting 10-unit sell");
    assert!(!own_orders_changed(&ctx, P2, buy_oid), "no fill -> no buyer order change");

    // A 60-unit sale fills partially: 40 remain, seller paid, buyer notified.
    let p1_before_fill = balance(&ctx, P1);
    let fill = rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &sell_args(TRITANIUM, 2.0, 60))
        .await
        .expect("PlaceMultiSellOrder 60 units");
    let PyRep::KeyVal(fill_kv) = &fill else { panic!("KeyVal") };
    assert_eq!(fill_kv.get("filledImmediately"), Some(&PyRep::Bool(true)));
    assert_eq!(vol_remaining(&ctx, buy_oid), Some(40), "volume remaining decremented by fill");
    assert!(balance(&ctx, P1).0 > p1_before_fill.0, "seller credited for the partial fill");
    assert!(own_orders_changed(&ctx, P2, buy_oid), "OnOwnOrdersChanged pushed to the buyer");
    let escrow_after_fill = ctx.db.with_snapshot(|s| s.market_orders[&buy_oid].escrow);
    assert_eq!(escrow_after_fill, IskCents::from_f64_isk(80.0), "escrow drawn down by 60 x 2.00");

    // Cancel: the remaining 40 x 2.00 escrow is refunded; broker fee is not.
    let p2_before_cancel = balance(&ctx, P2);
    rpc(&ctx, &mut s2, "marketProxy", "CancelCharOrder", &[PyRep::Integer(buy_oid)])
        .await
        .expect("CancelCharOrder partially filled bid");
    let p2_after_cancel = balance(&ctx, P2);
    assert_eq!(p2_after_cancel.0 - p2_before_cancel.0, IskCents::from_f64_isk(80.0).0);
    let spent = p2_before_place.0 - p2_after_cancel.0;
    assert_eq!(spent, IskCents::from_f64_isk(120.0).0 + broker_fee.0, "only fills + broker fee kept");
    assert!(ctx.db.with_snapshot(|s| s.market_orders.get(&buy_oid).map_or(true, |o| o.vol_remaining == 0)));

    // Cancelling a resting sell returns the unsold items to the hangar.
    let hangar = |ctx: &ServerContext| {
        ctx.db.with_snapshot(|s| {
            s.items
                .values()
                .filter(|i| i.owner_id == P1 && i.location_id == JITA_STATION_ID && i.type_id == TRITANIUM)
                .filter(|i| i.flag == eve_db::InventoryFlag::Hangar)
                .map(|i| i.quantity)
                .sum::<i32>()
        })
    };
    let before_list = hangar(&ctx);
    let ask = rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &sell_args(TRITANIUM, 90.0, 25))
        .await
        .expect("resting sell");
    assert_eq!(hangar(&ctx), before_list - 25);
    rpc(&ctx, &mut s1, "marketProxy", "CancelCharOrder", &[PyRep::Integer(kv_i64(&ask, "orderID"))])
        .await
        .expect("cancel resting sell");
    assert_eq!(hangar(&ctx), before_list, "unsold items returned to the hangar");
}

#[tokio::test]
async fn test_modify_order_relist_fee_and_five_minute_cooldown() {
    // parity: feat:market.modify-order
    eve_testkit::attest(&["feat:market.modify-order"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 711, "demo", "demo-secret", P1).await;
    let ask = rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &sell_args(TRITANIUM, 50.0, 100))
        .await
        .expect("resting sell @ 50");
    let oid = kv_i64(&ask, "orderID");

    let early = rpc(&ctx, &mut s1, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(oid), PyRep::Real(45.0)])
        .await
        .unwrap_err();
    assert_eq!(early.code, "OrderModificationCooldown");
    ctx.advance_virtual_clock(250);
    let still_early =
        rpc(&ctx, &mut s1, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(oid), PyRep::Real(45.0)])
            .await
            .unwrap_err();
    assert_eq!(still_early.code, "OrderModificationCooldown");

    ctx.advance_virtual_clock(55);
    let before = balance(&ctx, P1);
    let journal_before = ctx.db.with_snapshot(|s| s.wallet_journal.len());
    let ok = rpc(&ctx, &mut s1, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(oid), PyRep::Real(45.0)])
        .await
        .expect("ModifyCharOrder after 301 s");
    let fee = IskCents::from_f64_isk(kv_f64(&ok, "relistFeeISK"));
    assert!(fee.0 > 0, "relist fee charged");
    assert_eq!(kv_f64(&ok, "newPrice"), 45.0);
    assert_eq!(before.0 - balance(&ctx, P1).0, fee.0, "wallet debited by exactly the relist fee");
    assert_eq!(ctx.db.with_snapshot(|s| s.market_orders[&oid].price), IskCents::from_f64_isk(45.0));
    let relist_journaled = ctx.db.with_snapshot(|s| {
        s.wallet_journal[journal_before..]
            .iter()
            .any(|j| j.character_id == P1 && j.entry_type == "MarketBrokerFee" && j.amount == IskCents(-fee.0))
    });
    assert!(relist_journaled, "relist fee journaled");

    // The modification restarts the 5-minute timer.
    let again = rpc(&ctx, &mut s1, "marketProxy", "ModifyCharOrder", &[PyRep::Integer(oid), PyRep::Real(44.0)])
        .await
        .unwrap_err();
    assert_eq!(again.code, "OrderModificationCooldown");
}

#[tokio::test]
async fn test_order_limits_from_trade_retail_wholesale_tycoon() {
    // parity: feat:market.order-limits
    eve_testkit::attest(&["feat:market.order-limits"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 721, "demo", "demo-secret", P1).await;
    let names: Vec<(TypeId, &str)> =
        vec![(3443, "Trade"), (3444, "Retail"), (16_596, "Wholesale"), (18_580, "Tycoon")];
    for (tid, name) in &names {
        assert_eq!(ctx.sde.types.get(tid).map(|t| t.name.as_str()), Some(*name), "SDE skill {tid}");
    }
    // (Trade, Retail, Wholesale, Tycoon) -> max open orders
    let cases: [([u8; 4], usize); 6] = [
        ([0, 0, 0, 0], 5),
        ([1, 0, 0, 0], 9),
        ([0, 1, 0, 0], 13),
        ([0, 0, 1, 0], 21),
        ([0, 0, 0, 1], 37),
        ([5, 5, 5, 5], 305),
    ];
    for (levels, limit) in cases {
        for (i, (tid, _)) in names.iter().enumerate() {
            set_skill(&ctx, P1, *tid, levels[i]);
        }
        ctx.db.with_mut_snapshot(|snap| {
            snap.market_orders.retain(|_, o| o.owner_id != P1);
            for _ in 0..(limit - 1) {
                snap.next_entity_id += 1;
                let oid = snap.next_entity_id;
                snap.market_orders.insert(
                    oid,
                    MarketOrderRecord {
                        order_id: oid,
                        owner_id: P1,
                        station_id: JITA_STATION_ID,
                        region_id: THE_FORGE_REGION_ID,
                        type_id: PYERITE,
                        is_bid: false,
                        price: IskCents::from_isk(1_000),
                        vol_entered: 1,
                        vol_remaining: 1,
                        min_volume: 1,
                        is_npc: false,
                        escrow: IskCents(0),
                    },
                );
            }
        });
        rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &buy_args(TRITANIUM, 1.0, 1, 1))
            .await
            .unwrap_or_else(|e| panic!("order #{limit} must fit limit {limit} for {levels:?}: {e:?}"));
        let over = rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &buy_args(TRITANIUM, 1.0, 1, 1))
            .await
            .unwrap_err();
        assert_eq!(over.code, "MaxOpenOrdersReached", "order #{} exceeds {limit} for {levels:?}", limit + 1);
    }
}

#[tokio::test]
async fn test_get_char_orders_lists_active_orders_across_regions() {
    // parity: feat:market.char-orders
    eve_testkit::attest(&["feat:market.char-orders"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 731, "demo", "demo-secret", P1).await;
    let jita = rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &sell_args(TRITANIUM, 77.0, 5))
        .await
        .expect("Jita sell");
    let jita_oid = kv_i64(&jita, "orderID");
    let (amarr_oid, filled_oid, other_oid) = ctx.db.with_mut_snapshot(|snap| {
        let mut mk = |owner: i64, station: i64, region: u32, vol: i32| {
            snap.next_entity_id += 1;
            let oid = snap.next_entity_id;
            snap.market_orders.insert(
                oid,
                MarketOrderRecord {
                    order_id: oid,
                    owner_id: owner,
                    station_id: station,
                    region_id: region,
                    type_id: PYERITE,
                    is_bid: false,
                    price: IskCents::from_isk(12),
                    vol_entered: 10,
                    vol_remaining: vol,
                    min_volume: 1,
                    is_npc: false,
                    escrow: IskCents(0),
                },
            );
            oid
        };
        (
            mk(P1, AMARR_STATION_ID, DOMAIN_REGION_ID, 10),
            mk(P1, AMARR_STATION_ID, DOMAIN_REGION_ID, 0),
            mk(P2, AMARR_STATION_ID, DOMAIN_REGION_ID, 10),
        )
    });
    let rep = rpc(&ctx, &mut s1, "marketProxy", "GetCharOrders", &[]).await.expect("GetCharOrders");
    let PyRep::Rowset { columns, rows, .. } = rep else { panic!("expected Rowset") };
    let col = |name: &str| columns.iter().position(|c| c == name).unwrap_or_else(|| panic!("column {name}"));
    let (c_oid, c_station, c_vol) = (col("orderID"), col("stationID"), col("volRemaining"));
    let find = |oid: i64| rows.iter().find(|r| r[c_oid].as_i64() == Some(oid));
    let jita_row = find(jita_oid).expect("Jita (The Forge) order listed");
    assert_eq!(jita_row[c_station].as_i64(), Some(JITA_STATION_ID));
    assert_eq!(jita_row[c_vol].as_i64(), Some(5));
    let amarr_row = find(amarr_oid).expect("Amarr (Domain) order listed while docked in Jita");
    assert_eq!(amarr_row[c_station].as_i64(), Some(AMARR_STATION_ID));
    assert!(find(filled_oid).is_none(), "filled orders are not active");
    assert!(find(other_oid).is_none(), "other characters' orders are not listed");
}

#[tokio::test]
async fn test_wallet_balance_give_cash_journal_and_account_change() {
    // parity: feat:wallet.balance, feat:wallet.give-cash
    eve_testkit::attest(&["feat:wallet.balance", "feat:wallet.give-cash"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 741, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 742, "other", "other-secret", P2).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&P1).unwrap().balance = IskCents::from_isk(10_000);
        snap.characters.get_mut(&P2).unwrap().balance = IskCents::from_isk(500);
    });
    let bal = rpc(&ctx, &mut s1, "account", "GetCashBalance", &[]).await.expect("GetCashBalance");
    assert_eq!(bal, PyRep::Real(10_000.0));
    let _ = ctx.drain_notifications_for_char(P1);
    let _ = ctx.drain_notifications_for_char(P2);

    rpc(
        &ctx,
        &mut s1,
        "account",
        "GiveCash",
        &[PyRep::Integer(P2), PyRep::Real(2_500.5), PyRep::String("fuel money".into())],
    )
    .await
    .expect("GiveCash");
    assert_eq!(balance(&ctx, P1), IskCents::from_f64_isk(7_499.5));
    assert_eq!(balance(&ctx, P2), IskCents::from_f64_isk(3_000.5));
    assert_eq!(rpc(&ctx, &mut s1, "account", "GetCashBalance", &[]).await.unwrap(), PyRep::Real(7_499.5));
    assert_eq!(rpc(&ctx, &mut s2, "account", "GetCashBalance", &[]).await.unwrap(), PyRep::Real(3_000.5));

    for (cid, expected) in [(P1, 7_499.5), (P2, 3_000.5)] {
        let notes = ctx.drain_notifications_for_char(cid);
        let change = notes.iter().find(|n| n.event == "OnAccountChange").expect("OnAccountChange pushed");
        assert_eq!(change.args.get(1).and_then(|a| a.as_i64()), Some(cid));
        assert_eq!(change.args.get(2).and_then(|a| a.as_f64()), Some(expected));
    }
    ctx.db.with_snapshot(|snap| {
        let sent = snap
            .wallet_journal
            .iter()
            .find(|j| j.character_id == P1 && j.entry_type == "PlayerDonation")
            .expect("sender journal");
        assert_eq!(sent.amount, IskCents::from_f64_isk(-2_500.5));
        assert_eq!(sent.balance_after, IskCents::from_f64_isk(7_499.5));
        assert!(sent.reason.contains("fuel money") && sent.reason.contains(&P2.to_string()));
        let recv = snap
            .wallet_journal
            .iter()
            .find(|j| j.character_id == P2 && j.entry_type == "PlayerDonation")
            .expect("recipient journal");
        assert_eq!(recv.amount, IskCents::from_f64_isk(2_500.5));
        assert_eq!(recv.balance_after, IskCents::from_f64_isk(3_000.5));
        assert!(recv.reason.contains("fuel money") && recv.reason.contains(&P1.to_string()));
    });

    // Insufficient funds are rejected without moving ISK or journaling.
    let journal_len = ctx.db.with_snapshot(|s| s.wallet_journal.len());
    let err = rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(P2), PyRep::Real(1_000_000.0)])
        .await
        .unwrap_err();
    assert_eq!(err.code, "NotEnoughMoney");
    assert_eq!(balance(&ctx, P1), IskCents::from_f64_isk(7_499.5));
    assert_eq!(balance(&ctx, P2), IskCents::from_f64_isk(3_000.5));
    assert_eq!(ctx.db.with_snapshot(|s| s.wallet_journal.len()), journal_len);
    let neg = rpc(&ctx, &mut s1, "account", "GiveCash", &[PyRep::Integer(P2), PyRep::Real(-5.0)])
        .await
        .unwrap_err();
    assert_eq!(neg.code, "InvalidArgument");
}

#[tokio::test]
async fn test_cspa_contact_cost_set_get_and_charged_to_strangers() {
    // parity: feat:wallet.cspa-cost
    eve_testkit::attest(&["feat:wallet.cspa-cost"]);
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);
    let mut s2 = setup_pilot(&ctx, 752, "other", "other-secret", P2).await;
    assert_eq!(rpc(&ctx, &mut s2, "account", "GetDefaultContactCost", &[]).await.unwrap(), PyRep::None);
    rpc(&ctx, &mut s2, "account", "SetContactCost", &[PyRep::Integer(2_500)]).await.expect("SetContactCost");
    assert_eq!(rpc(&ctx, &mut s2, "account", "GetDefaultContactCost", &[]).await.unwrap(), PyRep::Real(2_500.0));
    let bad = rpc(&ctx, &mut s2, "account", "SetContactCost", &[PyRep::Integer(-1)]).await.unwrap_err();
    assert_eq!(bad.code, "InvalidArgument");

    // A stranger (different corporation, not a contact) mailing P2 pays the configured charge.
    ctx.db.with_mut_snapshot(|snap| {
        let p2_corp = snap.characters[&P2].corporation_id;
        let p1 = snap.characters.get_mut(&P1).unwrap();
        p1.corporation_id = p2_corp + 1;
        p1.user_id = 1;
        p1.balance = IskCents::from_isk(10_000);
    });
    let token = issue_mail_token(&ctx);
    let router = EsiRouter::new(Arc::clone(&ctx));
    let mail = |approved: i64| {
        EsiHttpRequest::new("POST", "/v1/characters/90000001/mail/").with_bearer_token(&token).with_body(
            serde_json::to_vec(&json!({
                "approved_cost": approved,
                "subject": "hello",
                "body": "cold contact",
                "recipients": [{ "recipient_id": P2, "recipient_type": "character" }]
            }))
            .unwrap(),
        )
    };
    let refused = router.handle_request(&mail(1_000));
    assert_eq!(refused.status_code, 400, "approved_cost below the CSPA charge is refused");
    assert_eq!(balance(&ctx, P1), IskCents::from_isk(10_000));
    let sent = router.handle_request(&mail(2_500));
    assert_eq!(sent.status_code, 201, "{}", String::from_utf8_lossy(&sent.body));
    assert_eq!(balance(&ctx, P1), IskCents::from_isk(7_500), "CSPA charge debited");

    // Clearing the cost (None) removes the charge.
    rpc(&ctx, &mut s2, "account", "SetContactCost", &[PyRep::None]).await.expect("clear CSPA");
    assert_eq!(rpc(&ctx, &mut s2, "account", "GetDefaultContactCost", &[]).await.unwrap(), PyRep::None);
    let free = router.handle_request(&mail(0));
    assert_eq!(free.status_code, 201);
    assert_eq!(balance(&ctx, P1), IskCents::from_isk(7_500));
}

fn issue_mail_token(ctx: &ServerContext) -> String {
    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    let now = ctx.now_unix();
    let code = ctx
        .db
        .authorize_pkce(
            "demo",
            "demo-secret",
            P1,
            "evomu-wallet-test",
            "https://localhost/callback",
            vec!["esi-mail.send_mail.v1".to_string()],
            CHALLENGE,
            "S256",
            now,
        )
        .expect("authorize_pkce");
    ctx.db
        .exchange_pkce_code(&code, "evomu-wallet-test", VERIFIER, now)
        .expect("exchange_pkce_code")
        .access_token
}

#[tokio::test]
async fn test_machonet_time_and_ping_are_authoritative_blue_time() {
    // parity: feat:serverops.machonet-time, feat:serverops.ping
    eve_testkit::attest(&["feat:serverops.machonet-time", "feat:serverops.ping"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 761, "demo", "demo-secret", P1).await;
    // Blue time = 100 ns ticks since 1601-01-01 UTC.
    let blue = |unix: u64| (unix as i64 + 11_644_473_600) * 10_000_000;
    let t0 = rpc(&ctx, &mut s1, "machoNet", "GetTime", &[]).await.expect("GetTime").as_i64().unwrap();
    assert_eq!(t0 / 10_000_000, blue(ctx.now_unix()) / 10_000_000);
    ctx.advance_virtual_clock(3_600);
    let t1 = rpc(&ctx, &mut s1, "machoNet", "GetTime", &[]).await.unwrap().as_i64().unwrap();
    assert_eq!((t1 - t0) / 10_000_000, 3_600, "GetTime follows the authoritative server clock");
    let ping = rpc(&ctx, &mut s1, "pingService", "Ping", &[]).await.expect("Ping").as_i64().unwrap();
    assert_eq!(ping / 10_000_000, blue(ctx.now_unix()) / 10_000_000, "Ping answers with server time");
}

#[tokio::test]
async fn test_cluster_statistics_count_online_pilots() {
    // parity: feat:serverops.cluster-stats
    eve_testkit::attest(&["feat:serverops.cluster-stats"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 771, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 772, "other", "other-secret", P2).await;
    ctx.db.with_mut_snapshot(|snap| {
        for cid in [P1, P2] {
            let c = snap.characters.get_mut(&cid).unwrap();
            c.solar_system_id = JITA_SYSTEM_ID as u32;
            c.station_id = Some(JITA_STATION_ID);
        }
    });
    let stats = |rep: PyRep| -> (i64, i64) {
        let PyRep::Tuple(items) = rep else { panic!("expected tuple") };
        assert_eq!(items.len(), 3);
        let get = |d: &PyRep| match d {
            PyRep::Dict(e) => e
                .iter()
                .find(|(k, _)| k.as_i64() == Some(JITA_SYSTEM_ID))
                .and_then(|(_, v)| v.as_i64())
                .unwrap_or(0),
            other => panic!("expected dict, got {other:?}"),
        };
        (get(&items[0]), get(&items[1]))
    };
    let r = rpc(&ctx, &mut s1, "machoNet", "GetClusterGameStatisticsForClient", &[]).await.unwrap();
    assert_eq!(stats(r), (2, 2), "two online pilots, both docked in Jita");
    ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&P2).unwrap().station_id = None);
    let r = rpc(&ctx, &mut s1, "machoNet", "GetClusterGameStatisticsForClient", &[]).await.unwrap();
    assert_eq!(stats(r), (2, 1), "undocked pilot still counted online");
    rpc(&ctx, &mut s2, "authentication", "Logout", &[]).await.expect("Logout");
    let r = rpc(&ctx, &mut s1, "machoNet", "GetClusterGameStatisticsForClient", &[]).await.unwrap();
    assert_eq!(stats(r), (1, 1), "logged-out pilot no longer counted");
}

#[tokio::test]
async fn test_client_telemetry_sinks_and_localization_server() {
    // parity: feat:serverops.client-telemetry, feat:serverops.localization-server
    eve_testkit::attest(&["feat:serverops.client-telemetry", "feat:serverops.localization-server"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 781, "demo", "demo-secret", P1).await;
    let s = |v: &str| PyRep::String(v.into());
    let calls: Vec<(&str, &str, Vec<PyRep>)> = vec![
        ("eventLog", "LogClientEvent", vec![s("autopilot"), PyRep::List(vec![s("jumps")]), s("start")]),
        ("eventLog", "LogClientStats", vec![PyRep::Dict(vec![(s("fps"), PyRep::Real(60.0))])]),
        ("eventLog", "LogPlayerRequestedDisconnect", vec![]),
        ("alert", "BeanDelivery", vec![PyRep::Dict(vec![])]),
        ("alert", "GroupBeanDelivery", vec![PyRep::Buffer(vec![0x78, 0x9c])]),
        ("eveguard_report", "heartbeat_started", vec![]),
        ("eveguard_report", "heartbeat_stopped", vec![]),
    ];
    for (svc, method, args) in calls {
        let r = rpc(&ctx, &mut s1, svc, method, &args).await;
        assert_eq!(r, Ok(PyRep::None), "{svc}.{method} accepted");
    }
    let trace_res = rpc(
        &ctx,
        &mut s1,
        "alert",
        "SendClientStackTraceAlert",
        &[PyRep::Tuple(vec![PyRep::Integer(1), s("x")]), s("Traceback ..."), s("error"), PyRep::None],
    )
    .await
    .expect("SendClientStackTraceAlert");
    assert!(matches!(trace_res, PyRep::Tuple(_)));
    let bean = rpc(&ctx, &mut s1, "alert", "BeanCount", &[PyRep::Integer(1)]).await.expect("BeanCount");
    assert!(matches!(bean, PyRep::Tuple(_)));
    let cfg_err = rpc(&ctx, &mut s1, "infoGatheringMgr", "GetStateAndConfig", &[]).await.unwrap_err();
    assert_eq!(cfg_err.code, "UnimplementedRPC");

    let text = rpc(&ctx, &mut s1, "localizationServer", "GetAllTextChanges", &[PyRep::Dict(vec![])])
        .await
        .expect("GetAllTextChanges");
    assert_eq!(text, PyRep::None, "no server-side text overrides by default");
}

#[tokio::test]
async fn test_fast_checkout_served_as_disabled() {
    // parity: feat:wallet.fast-checkout
    eve_testkit::attest(&["feat:wallet.fast-checkout"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 791, "demo", "demo-secret", P1).await;
    let enabled = rpc(&ctx, &mut s1, "FastCheckoutService", "IsFastCheckoutEnabledForUser", &[]).await;
    assert_eq!(enabled, Ok(PyRep::Bool(false)));
    let offers = rpc(&ctx, &mut s1, "FastCheckoutService", "GetOffersForUser", &[]).await;
    assert_eq!(offers, Ok(PyRep::List(Vec::new())));
    let before = balance(&ctx, P1);
    let plex_before = ctx.db.with_snapshot(|s| s.plex_vault_balance(P1));
    let buy = rpc(&ctx, &mut s1, "FastCheckoutService", "BuyOffer", &[PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(buy.code, "FastCheckoutDisabled");
    assert_eq!(balance(&ctx, P1), before);
    assert_eq!(ctx.db.with_snapshot(|s| s.plex_vault_balance(P1)), plex_before);
}

#[tokio::test]
async fn test_corporation_and_personal_bills_and_autopay() {
    // parity: feat:wallet.bills
    eve_testkit::attest(&["feat:wallet.bills"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 881, "demo", "demo-secret", P1).await;

    let corp_id = ctx.db.with_snapshot(|s| s.characters[&P1].corporation_id);
    let landlord_corp = 1_000_035_i64; // Caldari Navy
    let now = ctx.now_unix();

    // Setup corp wallet divisions with funds
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.entry(corp_id).or_insert_with(|| CorporationRecord {
            corporation_id: corp_id,
            name: "Trader Corp".into(),
            ticker: "TRDR".into(),
            ceo_id: P1,
            master_wallet: IskCents::from_isk(10_000_000),
            ..Default::default()
        });
        corp.ceo_id = P1;
        corp.master_wallet = IskCents::from_isk(10_000_000);
        corp.ensure_wallet_divisions();
        for d in &mut corp.wallet_divisions {
            d.balance = IskCents::from_isk(1_000_000);
        }
    });

    // 1. Create personal bill (e.g. personal fee 2,500 ISK)
    let p_bill = create_bill(
        &ctx,
        1,
        P1,
        landlord_corp,
        2_500.0,
        now + 3600,
        0.0,
        0,
        0,
    );

    // 2. Create corporation office rent bill (bill_type_id = 2, 10,000 ISK)
    let c_bill = create_bill(
        &ctx,
        2,
        corp_id,
        landlord_corp,
        10_000.0,
        now + 3600,
        0.0,
        JITA_STATION_ID,
        0,
    );

    // GetCorporationBills lists unpaid bills for the corporation and character
    let bills_rep = rpc(&ctx, &mut s1, "billMgr", "GetCorporationBills", &[]).await.expect("GetCorporationBills");
    let PyRep::Rowset { rows, .. } = bills_rep else { panic!("expected Rowset"); };
    assert!(rows.iter().any(|r| r.first() == Some(&PyRep::Integer(p_bill))));
    assert!(rows.iter().any(|r| r.first() == Some(&PyRep::Integer(c_bill))));

    // 3. CharPayBill pays personal bill
    let p1_before = balance(&ctx, P1);
    let pay_p = rpc(&ctx, &mut s1, "billMgr", "CharPayBill", &[PyRep::Integer(p_bill)]).await.expect("CharPayBill");
    assert_eq!(pay_p, PyRep::Bool(true));
    assert_eq!(p1_before.0 - balance(&ctx, P1).0, 250_000);

    // 4. PayCorporationBill pays corp office rent from division 3 (fromAccountKey = 1002)
    let div3_before = ctx.db.with_snapshot(|s| s.corporations[&corp_id].wallet_divisions.iter().find(|d| d.division_id == 3).unwrap().balance);
    let pay_c = rpc(&ctx, &mut s1, "billMgr", "PayCorporationBill", &[PyRep::Integer(c_bill), PyRep::Integer(1002)]).await.expect("PayCorporationBill");
    assert_eq!(pay_c, PyRep::Bool(true));
    let div3_after = ctx.db.with_snapshot(|s| s.corporations[&corp_id].wallet_divisions.iter().find(|d| d.division_id == 3).unwrap().balance);
    assert_eq!(div3_before.0 - div3_after.0, 1_000_000);

    // 5. Automatic payment settings (GetAutomaticPaySettings & SendAutomaticPaySettings)
    let auto_dict = PyRep::Dict(vec![
        (PyRep::Integer(2), PyRep::Bool(true)),
        (PyRep::String("divisionID".into()), PyRep::Integer(1004)),
    ]);
    let set_ap = rpc(&ctx, &mut s1, "billMgr", "SendAutomaticPaySettings", &[PyRep::Dict(vec![(PyRep::Integer(corp_id), auto_dict)])]).await.expect("SendAutomaticPaySettings");
    assert_eq!(set_ap, PyRep::Bool(true));

    let get_ap = rpc(&ctx, &mut s1, "billMgr", "GetAutomaticPaySettings", &[]).await.expect("GetAutomaticPaySettings");
    let PyRep::Dict(ap_entries) = get_ap else { panic!("expected Dict"); };
    assert!(!ap_entries.is_empty());

    // Create a due bill and verify process_due_automatic_bills honours settings
    let due_bill = create_bill(
        &ctx,
        2,
        corp_id,
        landlord_corp,
        5_000.0,
        now - 10,
        0.0,
        0,
        0,
    );
    let paid_auto = process_due_automatic_bills(&ctx);
    assert!(paid_auto >= 1);
    let is_due_paid = ctx.db.with_snapshot(|snap| {
        snap.owner_notes
            .values()
            .filter(|n| n.character_id == -71)
            .filter_map(|n| serde_json::from_str::<CorpBillRecord>(&n.note).ok())
            .find(|b| b.bill_id == due_bill)
            .map(|b| b.paid)
    });
    assert_eq!(is_due_paid, Some(true));
}

#[tokio::test]
async fn test_new_eden_store_and_aurum_removal() {
    // parity: feat:wallet.nes-store, feat:removed.aurum-currency
    eve_testkit::attest(&["feat:wallet.nes-store", "feat:removed.aurum-currency"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 882, "demo", "demo-secret", P1).await;

    // 1. get_categories
    let cats = rpc(&ctx, &mut s1, "storeManager", "get_categories", &[]).await.expect("get_categories");
    let PyRep::List(cat_list) = cats else { panic!("expected List"); };
    assert!(!cat_list.is_empty());

    // 2. get_offers (shows offers priced in PLEX)
    let offers = rpc(&ctx, &mut s1, "storeManager", "get_offers", &[]).await.expect("get_offers");
    let PyRep::List(offer_list) = offers else { panic!("expected List"); };
    assert!(!offer_list.is_empty());

    // 3. can_purchase_offer validates
    let can_p = rpc(&ctx, &mut s1, "storeManager", "can_purchase_offer", &[PyRep::Integer(1002)]).await.expect("can_purchase_offer");
    assert_eq!(can_p, PyRep::Bool(true));

    // 4. Legacy Aurum currency rejection (`feat:removed.aurum-currency`)
    let aur_err = rpc(&ctx, &mut s1, "storeManager", "buy_offer", &[PyRep::Integer(1002), PyRep::String("AUR".into()), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(aur_err.code, "AurumCurrencyRemoved");

    // 5. buy_offer debits PLEX and delivers SKIN item (`feat:wallet.nes-store`)
    ctx.db.with_mut_snapshot(|s| {
        s.adjust_plex_vault(P1, 500).unwrap();
    });
    let vault_before = ctx.db.with_snapshot(|s| s.plex_vault_balance(P1));
    let buy_res = rpc(&ctx, &mut s1, "storeManager", "buy_offer", &[PyRep::Integer(1002), PyRep::String("PLX".into()), PyRep::Integer(1)]).await.expect("buy_offer");
    let PyRep::KeyVal(buy_kv) = buy_res else { panic!("expected KeyVal"); };
    assert_eq!(buy_kv.get("success"), Some(&PyRep::Bool(true)));
    assert_eq!(buy_kv.get("offerID"), Some(&PyRep::Integer(1002)));
    let vault_after = ctx.db.with_snapshot(|s| s.plex_vault_balance(P1));
    assert_eq!(vault_before - vault_after, 110, "Offer 1002 costs 110 PLEX");

    // Item delivered to station hangar
    let has_skin = ctx.db.with_snapshot(|s| {
        s.items.values().any(|i| i.owner_id == P1 && i.location_id == JITA_STATION_ID && i.flag == InventoryFlag::Hangar)
    });
    assert!(has_skin, "Purchased SKIN delivered to station hangar");
}

#[tokio::test]
async fn test_station_direct_trade_window() {
    // parity: feat:wallet.trade-window
    eve_testkit::attest(&["feat:wallet.trade-window"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 883, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 884, "other", "other-secret", P2).await;

    // Both pilots docked at Jita
    s1.station_id = Some(JITA_STATION_ID);
    s2.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) { ch.station_id = Some(JITA_STATION_ID); }
        if let Some(ch) = snap.characters.get_mut(&P2) { ch.station_id = Some(JITA_STATION_ID); }
    });

    // Give P1 100 Tritanium in Jita hangar
    let p1_item = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(id, InventoryItemRecord {
            item_id: id,
            type_id: TRITANIUM,
            owner_id: P1,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 100,
            singleton: false,
            version: 1,
            ..Default::default()
        });
        id
    });

    let p1_bal_before = balance(&ctx, P1);
    let p2_bal_before = balance(&ctx, P2);

    // 1. InitiateTrade
    let init_rep = rpc(&ctx, &mut s1, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await.expect("InitiateTrade");
    let trade_id = kv_i64(&init_rep, "tradeContainerID");

    // 2. P1 offers 100 Tritanium
    let off_item = rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id), PyRep::Integer(p1_item), PyRep::Integer(100)]).await.expect("AddItem");
    assert_eq!(kv_i64(&off_item, "tradeContainerID"), trade_id);

    // 3. P2 offers 50,000 ISK
    let off_money = rpc(&ctx, &mut s2, "trademgr", "OfferMoney", &[PyRep::Integer(trade_id), PyRep::Real(50_000.0)]).await.expect("OfferMoney");
    assert_eq!(kv_i64(&off_money, "tradeContainerID"), trade_id);

    // 4. P1 accepts trade
    let acc1 = rpc(&ctx, &mut s1, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)]).await.expect("MakeOffer s1");
    let PyRep::KeyVal(kv1) = acc1 else { panic!("expected KeyVal"); };
    assert_eq!(kv1.get("completed"), Some(&PyRep::Bool(false)));

    // 5. P2 accepts trade -> completes atomically
    let acc2 = rpc(&ctx, &mut s2, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id), PyRep::Bool(true)]).await.expect("MakeOffer s2");
    let PyRep::KeyVal(kv2) = acc2 else { panic!("expected KeyVal"); };
    assert_eq!(kv2.get("completed"), Some(&PyRep::Bool(true)));

    // Verify atomic results:
    // P2 received the 100 Tritanium
    let p2_has_trit = ctx.db.with_snapshot(|s| {
        s.items.get(&p1_item).map(|i| i.owner_id == P2 && i.quantity == 100 && i.flag == InventoryFlag::Hangar).unwrap_or(false)
    });
    assert!(p2_has_trit, "Item ownership transferred to P2 in hangar");

    // ISK debited from P2 and credited to P1
    assert_eq!(balance(&ctx, P1).0 - p1_bal_before.0, IskCents::from_isk(50_000).0);
    assert_eq!(p2_bal_before.0 - balance(&ctx, P2).0, IskCents::from_isk(50_000).0);

    // Partial stack trade step: Seller A (P1) offers 30 units out of 100 to Buyer B (P2)
    let p1_partial_item = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let id = s.next_entity_id;
        s.items.insert(id, InventoryItemRecord {
            item_id: id,
            type_id: TRITANIUM,
            owner_id: P1,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 100,
            singleton: false,
            version: 1,
            ..Default::default()
        });
        id
    });

    let p1_bal_before_partial = balance(&ctx, P1);
    let p2_bal_before_partial = balance(&ctx, P2);
    let total_isk_before = p1_bal_before_partial.0 + p2_bal_before_partial.0;

    let init_rep2 = rpc(&ctx, &mut s1, "trademgr", "InitiateTrade", &[PyRep::Integer(P2)]).await.expect("InitiateTrade 2");
    let trade_id2 = kv_i64(&init_rep2, "tradeContainerID");

    rpc(&ctx, &mut s1, "trademgr", "AddItem", &[PyRep::Integer(trade_id2), PyRep::Integer(p1_partial_item), PyRep::Integer(30)]).await.expect("AddItem partial");
    rpc(&ctx, &mut s2, "trademgr", "OfferMoney", &[PyRep::Integer(trade_id2), PyRep::Real(15_000.0)]).await.expect("OfferMoney partial");
    rpc(&ctx, &mut s1, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id2), PyRep::Bool(true)]).await.expect("MakeOffer 2 s1");
    let acc2_2 = rpc(&ctx, &mut s2, "trademgr", "MakeOffer", &[PyRep::Integer(trade_id2), PyRep::Bool(true)]).await.expect("MakeOffer 2 s2");
    let PyRep::KeyVal(kv2_2) = acc2_2 else { panic!("expected KeyVal"); };
    assert_eq!(kv2_2.get("completed"), Some(&PyRep::Bool(true)));

    // Assert that seller A retains exactly 70 units of the item in hangar
    let p1_retains_70 = ctx.db.with_snapshot(|s| {
        s.items.get(&p1_partial_item).map(|i| i.owner_id == P1 && i.quantity == 70 && i.location_id == JITA_STATION_ID && i.flag == InventoryFlag::Hangar).unwrap_or(false)
    });
    assert!(p1_retains_70, "Seller A retains exactly 70 units of the item in hangar");

    // Assert that buyer B receives 30 units of the item in hangar
    let p2_received_30 = ctx.db.with_snapshot(|s| {
        s.items.values().any(|i| i.owner_id == P2 && i.type_id == TRITANIUM && i.quantity == 30 && i.location_id == JITA_STATION_ID && i.flag == InventoryFlag::Hangar)
    });
    assert!(p2_received_30, "Buyer B receives 30 units of the item in hangar");

    // Assert ISK and item conservation
    let total_isk_after = balance(&ctx, P1).0 + balance(&ctx, P2).0;
    assert_eq!(total_isk_before, total_isk_after, "Total ISK is strictly conserved");
    assert_eq!(balance(&ctx, P1).0 - p1_bal_before_partial.0, IskCents::from_isk(15_000).0);
    assert_eq!(p2_bal_before_partial.0 - balance(&ctx, P2).0, IskCents::from_isk(15_000).0);

    let total_item_units: i32 = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| (i.item_id == p1_partial_item || (i.owner_id == P2 && i.quantity == 30)) && i.type_id == TRITANIUM).map(|i| i.quantity).sum()
    });
    assert_eq!(total_item_units, 100, "Item units strictly conserved (70 retained + 30 transferred = 100)");
}

#[tokio::test]
async fn test_hypernet_relay_and_hypercore_scaling_and_expiry() {
    // parity: feat:wallet.hypernet-relay, feat:wallet.hypernet-hypercore-sizing-expiry
    eve_testkit::attest(&["feat:wallet.hypernet-relay", "feat:wallet.hypernet-hypercore-sizing-expiry"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut s1 = setup_pilot(&ctx, 885, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 886, "other", "other-secret", P2).await;
    s1.station_id = Some(JITA_STATION_ID);
    s2.station_id = Some(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) { ch.station_id = Some(JITA_STATION_ID); }
        if let Some(ch) = snap.characters.get_mut(&P2) { ch.station_id = Some(JITA_STATION_ID); }
    });

    // 1. HyperCore sizing formula & validation
    assert!(compute_required_hypercores(10_000_000.0, 10).is_err());
    let cores_8 = compute_required_hypercores(50_000_000.0, 8).unwrap();
    let cores_512 = compute_required_hypercores(50_000_000.0, 512).unwrap();
    assert!(cores_512 >= cores_8, "More nodes require more HyperCores");

    // Put item and HyperCores in P1's hangar
    let (raffle_item, _) = ctx.db.with_mut_snapshot(|s| {
        s.next_entity_id += 1;
        let it_id = s.next_entity_id;
        s.items.insert(it_id, InventoryItemRecord {
            item_id: it_id,
            type_id: 587,
            owner_id: P1,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        });
        s.next_entity_id += 1;
        let hc_id = s.next_entity_id;
        s.items.insert(hc_id, InventoryItemRecord {
            item_id: hc_id,
            type_id: HYPERCORE_TYPE_ID,
            owner_id: P1,
            location_id: JITA_STATION_ID,
            flag: InventoryFlag::Hangar,
            quantity: 50,
            singleton: false,
            version: 1,
            ..Default::default()
        });
        (it_id, hc_id)
    });

    // 2. CreateRaffle (16 nodes, 10,000 ISK/ticket)
    let c_raf = rpc(&ctx, &mut s1, "raffleProxy", "CreateRaffle", &[PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(raffle_item)),
        (PyRep::String("ticketCount".into()), PyRep::Integer(16)),
        (PyRep::String("ticketPrice".into()), PyRep::Real(10_000.0)),
    ])]).await.expect("CreateRaffle");
    let raffle_id = kv_i64(&c_raf, "raffle_id");

    // Item escrowed from hangar
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&raffle_item].owner_id), -52_568);

    // 3. P2 buys 1 ticket
    let p2_before = balance(&ctx, P2);
    let buy_t = rpc(&ctx, &mut s2, "raffleProxy", "BuyTicket", &[PyRep::Integer(raffle_id), PyRep::Integer(0)]).await.expect("BuyTicket");
    assert_eq!(kv_i64(&buy_t, "raffle_id"), raffle_id);
    assert_eq!(kv_i64(&buy_t, "tickets_remaining"), 15);
    assert_eq!(p2_before.0 - balance(&ctx, P2).0, IskCents::from_isk(10_000).0);

    // 4. 72-hour expiry sweep
    ctx.advance_virtual_clock(72 * 3_600 + 1);
    let raf_info = rpc(&ctx, &mut s1, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)]).await.expect("GetRaffle sweeps expired");
    let PyRep::KeyVal(r_kv) = raf_info else { panic!("expected KeyVal"); };
    assert_eq!(r_kv.get("is_expired"), Some(&PyRep::Bool(true)));

    // Ticket buyer refunded
    assert_eq!(balance(&ctx, P2), p2_before, "Buyer refunded upon raffle expiry");
    // Escrowed item returned to creator
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&raffle_item].flag), InventoryFlag::Hangar);
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&raffle_item].owner_id), P1);
}

#[tokio::test]
async fn test_pos_towers_and_starbases_removed() {
    // parity: feat:removed.pos-towers
    eve_testkit::attest(&["feat:removed.pos-towers"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 887, "demo", "demo-secret", P1).await;

    let towers = rpc(&ctx, &mut s1, "posMgr", "GetControlTowers", &[]).await.expect("GetControlTowers");
    assert_eq!(towers, PyRep::List(Vec::new()));

    let moon = rpc(&ctx, &mut s1, "posMgr", "GetMoonForTower", &[PyRep::Integer(1)]).await.expect("GetMoonForTower");
    assert_eq!(moon, PyRep::None);

    let anchor_err = rpc(&ctx, &mut s1, "posMgr", "AnchorOrbital", &[PyRep::Integer(1), PyRep::Integer(1)]).await.unwrap_err();
    assert_eq!(anchor_err.code, "StarbaseRemoved");
}

#[tokio::test]
async fn test_time_dilation_and_node_balancing() {
    // parity: feat:serverops.time-dilation, feat:serverops.node-balancing
    eve_testkit::attest(&["feat:serverops.time-dilation", "feat:serverops.node-balancing"]);
    let ctx = ServerContext::new();

    // 1. Time dilation setting & notification
    let factor = set_system_tidi_factor(&ctx, JITA_SYSTEM_ID as u32, 0.45);
    assert!((factor - 0.45).abs() < 1e-6);
    let read_factor = get_system_tidi_factor(&ctx, JITA_SYSTEM_ID as u32);
    assert!((read_factor - 0.45).abs() < 1e-6);

    // TiDi factor clamped between 0.10 and 1.0
    let clamped_min = set_system_tidi_factor(&ctx, JITA_SYSTEM_ID as u32, 0.01);
    assert!((clamped_min - 0.10).abs() < 1e-6);

    // 2. Node distribution and reinforced dedicated node
    let default_node = assign_or_reinforce_system_node(&ctx, JITA_SYSTEM_ID as u32, None);
    assert!(default_node >= 1);
    let dedicated = assign_or_reinforce_system_node(&ctx, JITA_SYSTEM_ID as u32, Some(42));
    assert_eq!(dedicated, 42);
    let retrieved = assign_or_reinforce_system_node(&ctx, JITA_SYSTEM_ID as u32, None);
    assert_eq!(retrieved, 42);
}

#[tokio::test]
async fn test_daily_downtime_and_maintenance_jobs() {
    // parity: feat:serverops.downtime, feat:serverops.daily-jobs
    eve_testkit::attest(&["feat:serverops.downtime", "feat:serverops.daily-jobs"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);

    // Advance clock to 11:05 UTC (window 11:00-11:15 UTC)
    let current = ctx.now_unix();
    let current_sec = current % 86_400;
    let target_sec = 11 * 3_600 + 300;
    if current_sec <= target_sec {
        ctx.advance_virtual_clock(target_sec - current_sec);
    } else {
        ctx.advance_virtual_clock(86_400 - current_sec + target_sec);
    }

    let report = run_daily_downtime_and_jobs(&ctx);
    assert_eq!(report.get("is1100UtcWindow"), Some(&1));
    assert!(report.contains_key("replenishedAsteroids"));
    assert!(report.contains_key("expiredMarketOrders"));
    assert!(report.contains_key("autoPaidBills"));
    assert!(report.contains_key("expiredRaffles"));
}

