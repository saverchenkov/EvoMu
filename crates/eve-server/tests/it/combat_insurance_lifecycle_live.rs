//! Live insurance lifecycle (`feat:combat.insurance-void-and-expiry`), player RPCs only:
//! `insuranceSvc.InsureShip` on market-bought, assembled hulls; voiding by repackaging
//! (`repackagingSvc.RepackageItems`) and by escrowing in an item-exchange contract
//! (`contractProxy.CreateContract`); the 12-week (84-day) term; and payout on a real loss
//! (`ship.SelfDestruct`) with the Secure Commerce Commission insurance EVE-mail.
//! Expired/voided hulls fall back to the default 40% payout.

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{buy, dock_station, fund_pilots, login, rpc, travel, JITA_STATION, PILOT_1};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::world_seeder::seed_full_market_catalog;
use std::collections::HashMap;

const RIFTER: u32 = 587;
const FILETIME_TICKS_PER_SEC: i64 = 10_000_000;

async fn new_hull(ctx: &ServerContext, sess: &mut eve_machonet::SessionState) -> i64 {
    let ship = buy(ctx, sess, RIFTER, 1).await[0];
    rpc(ctx, sess, "ship", "AssembleShip", &[PyRep::Integer(ship)]).await.expect("AssembleShip");
    ship
}

async fn contracts(ctx: &ServerContext, sess: &mut eve_machonet::SessionState) -> Vec<PyRep> {
    match rpc(ctx, sess, "insuranceSvc", "GetContracts", &[]).await.expect("GetContracts") {
        PyRep::List(v) => v,
        other => panic!("GetContracts list, got {other:?}"),
    }
}

fn contract_for(list: &[PyRep], ship: i64) -> Option<&PyRep> {
    list.iter().find(|c| c.dict_get("shipID").and_then(|v| v.as_i64()) == Some(ship))
}

fn balance(ctx: &ServerContext) -> f64 {
    ctx.db.with_snapshot(|s| s.characters[&PILOT_1].balance.to_wire_f64())
}

/// Boards `ship` in Jita, undocks and self-destructs it; returns the wallet delta of the loss.
async fn lose_ship(ctx: &ServerContext, sess: &mut eve_machonet::SessionState, ship: i64) -> f64 {
    rpc(ctx, sess, "ship", "Board", &[PyRep::Integer(ship)]).await.expect("Board");
    ctx.advance_secs(15);
    let before = balance(ctx);
    travel(ctx, sess, 30_000_142).await;
    let start = rpc(ctx, sess, "ship", "SelfDestruct", &[]).await.expect("start self-destruct");
    assert_eq!(start.dict_get("countdownStarted").and_then(|v| v.as_bool()), Some(true));
    // The 2-minute countdown detonates on the server lifecycle tick.
    ctx.advance_secs(125);
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&ship)), "self-destructed hull is gone");
    let delta = balance(ctx) - before;
    dock_station(ctx, sess, JITA_STATION).await;
    delta
}

#[tokio::test]
async fn test_live_insurance_void_triggers_expiry_and_loss_payout_mail() {
    // parity: feat:combat.insurance-void-and-expiry
    eve_testkit::attest(&["feat:combat.insurance-void-and-expiry"]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 20_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 8911, "demo", PILOT_1).await;

    // 1. Platinum contract with a 12-week term; repackaging voids it.
    let ship_a = new_hull(&ctx, &mut s1).await;
    let ins = rpc(&ctx, &mut s1, "insuranceSvc", "InsureShip", &[PyRep::Integer(ship_a), PyRep::Real(1.0)]).await.expect("InsureShip A");
    let list = contracts(&ctx, &mut s1).await;
    let c_a = contract_for(&list, ship_a).unwrap_or_else(|| panic!("contract for A after {ins:?}"));
    let term = c_a.dict_get("endDate").and_then(|v| v.as_i64()).unwrap() - c_a.dict_get("startDate").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(term, 84 * 86_400 * FILETIME_TICKS_PER_SEC, "insurance lasts 12 weeks");
    assert_eq!(c_a.dict_get("fraction").and_then(|v| v.as_f64()), Some(1.0));
    let platinum_payout = c_a.dict_get("payoutISK").and_then(|v| v.as_f64()).unwrap();
    rpc(&ctx, &mut s1, "repackagingSvc", "RepackageItems", &[PyRep::List(vec![PyRep::Integer(ship_a)])]).await.expect("RepackageItems");
    assert!(contract_for(&contracts(&ctx, &mut s1).await, ship_a).is_none(), "repackaging voids insurance");

    // 2. Putting an insured hull into an item-exchange contract voids it.
    let ship_b = new_hull(&ctx, &mut s1).await;
    rpc(&ctx, &mut s1, "insuranceSvc", "InsureShip", &[PyRep::Integer(ship_b), PyRep::Real(1.0)]).await.expect("InsureShip B");
    assert!(contract_for(&contracts(&ctx, &mut s1).await, ship_b).is_some());
    let params = PyRep::keyval([
        ("contractType", PyRep::String("ItemExchange".into())),
        ("availability", PyRep::String("Public".into())),
        ("assigneeID", PyRep::Integer(0)),
        ("expireTime", PyRep::Integer(1440)),
        ("numDays", PyRep::Integer(3)),
        ("startStationID", PyRep::Integer(JITA_STATION)),
        ("endStationID", PyRep::Integer(JITA_STATION)),
        ("price", PyRep::Real(1_000_000.0)),
        ("reward", PyRep::Real(0.0)),
        ("collateral", PyRep::Real(0.0)),
        ("title", PyRep::String("Insured Rifter".into())),
        ("itemList", PyRep::List(vec![PyRep::Integer(ship_b)])),
    ]);
    let kw = HashMap::from([("confirm".to_string(), PyRep::Bool(true))]);
    ctx.dispatch_rpc(&mut s1, "contractProxy", None, "CreateContract", &[params], &kw, None).await.expect("CreateContract");
    assert!(contract_for(&contracts(&ctx, &mut s1).await, ship_b).is_none(), "contracting voids insurance");

    // 3. A contract older than 12 weeks has lapsed: the loss pays only the default 40%.
    let ship_c = new_hull(&ctx, &mut s1).await;
    rpc(&ctx, &mut s1, "insuranceSvc", "InsureShip", &[PyRep::Integer(ship_c), PyRep::Real(1.0)]).await.expect("InsureShip C");
    ctx.advance_virtual_clock(85 * 86_400);
    assert!(contract_for(&contracts(&ctx, &mut s1).await, ship_c).is_none(), "12-week term expired");
    let lapsed_payout = lose_ship(&ctx, &mut s1, ship_c).await;
    assert!(lapsed_payout > 0.0 && lapsed_payout < platinum_payout * 0.75, "lapsed hull gets the default 40% payout ({lapsed_payout} vs {platinum_payout})");

    // 4. A live Platinum contract pays immediately on loss and sends the insurance EVE-mail.
    let ship_d = new_hull(&ctx, &mut s1).await;
    rpc(&ctx, &mut s1, "insuranceSvc", "InsureShip", &[PyRep::Integer(ship_d), PyRep::Real(1.0)]).await.expect("InsureShip D");
    let d_payout = contract_for(&contracts(&ctx, &mut s1).await, ship_d).and_then(|c| c.dict_get("payoutISK")).and_then(|v| v.as_f64()).unwrap();
    let paid = lose_ship(&ctx, &mut s1, ship_d).await;
    assert!((paid - d_payout).abs() < 0.02, "loss pays the contracted Platinum payout ({paid} vs {d_payout})");
    assert!(contract_for(&contracts(&ctx, &mut s1).await, ship_d).is_none(), "contract consumed by the payout");
    let mails = rpc(&ctx, &mut s1, "mailMgr", "SyncMail", &[]).await.expect("SyncMail");
    assert!(format!("{mails:?}").contains(&format!("Insurance Payout for Ship {ship_d}")), "insurance EVE-mail: {mails:?}");
}
