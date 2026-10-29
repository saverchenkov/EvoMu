//! Wallet, market-order lifecycle and server-ops telemetry/time integration tests (part 1).

use crate::common;
use eve_db::{IskCents, MarketOrderRecord, TrainedSkillRecord, TypeId};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
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
        .any(|n| {
            n.event == "OnOwnOrdersChanged"
                && match n.args.first() {
                    Some(PyRep::Integer(oid)) => *oid == order_id,
                    Some(PyRep::List(orders)) => orders.iter().any(|o| {
                        o.dict_get("orderID").and_then(|v| v.as_i64()) == Some(order_id)
                    }),
                    _ => false,
                }
        })
}

#[tokio::test]
async fn test_partial_fill_min_volume_notifications_and_cancel_refund() {
    // parity: feat:market.partial-fill, feat:market.cancel-order
    eve_testkit::attest(&["feat:market.partial-fill", "feat:market.cancel-order"]);
    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 701, "demo", "demo-secret", P1).await;
    let mut s2 = setup_pilot(&ctx, 702, "other", "other-secret", P2).await;
    set_skill(&ctx, P2, 16_597, 0);
    let foreign_bids = ctx.db.with_snapshot(|snap| {
        snap.market_orders.values().any(|o| {
            o.is_bid && o.type_id == TRITANIUM && o.station_id == JITA_STATION_ID && o.price.0 >= 200
        })
    });
    assert!(!foreign_bids, "fixture: no pre-existing Tritanium bids at >= 2.00 ISK in Jita");

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

    let p2_before_cancel = balance(&ctx, P2);
    rpc(&ctx, &mut s2, "marketProxy", "CancelCharOrder", &[PyRep::Integer(buy_oid)])
        .await
        .expect("CancelCharOrder partially filled bid");
    let p2_after_cancel = balance(&ctx, P2);
    assert_eq!(p2_after_cancel.0 - p2_before_cancel.0, IskCents::from_f64_isk(80.0).0);
    let spent = p2_before_place.0 - p2_after_cancel.0;
    assert_eq!(spent, IskCents::from_f64_isk(120.0).0 + broker_fee.0, "only fills + broker fee kept");
    assert!(ctx.db.with_snapshot(|s| s.market_orders.get(&buy_oid).map_or(true, |o| o.vol_remaining == 0)));

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
    let cfg_res = rpc(&ctx, &mut s1, "infoGatheringMgr", "GetStateAndConfig", &[]).await.expect("GetStateAndConfig");
    assert_eq!(cfg_res.dict_get("isEnabled"), Some(&PyRep::Bool(true)));

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
