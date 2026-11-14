//! Live insurance lifecycle (`feat:combat.insurance-void-and-expiry`), player RPCs only:
//! `insuranceSvc.InsureShip` on market-bought, assembled hulls; voiding by repackaging
//! (`repackagingSvc.RepackageItems`) and by escrowing in an item-exchange contract
//! (`contractProxy.CreateContract`); the 12-week (84-day) term; and payout on a real PvP loss
//! (limited-engagement duel + live turret fire) with the Secure Commerce Commission insurance EVE-mail.
//! Expired/voided hulls fall back to the default 40% payout.

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{buy, dock_station, fund_pilots, login, move_item, rpc, travel, JITA_STATION, PILOT_1, PILOT_2};
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::world_seeder::seed_full_market_catalog;
use eve_testkit::client_harness::extract_bound_id_from_wire_rep;
use std::collections::HashMap;

const RIFTER: u32 = 587;
const EMP_S: u32 = 185;
const FILETIME_TICKS_PER_SEC: i64 = 10_000_000;
const PILOT_2_GUN: i64 = 100_000_006;

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

/// Boards `ship` in Jita, undocks, and loses it in a live duel against `PILOT_2`; returns the wallet delta of the loss.
async fn lose_ship(
    ctx: &ServerContext,
    sess: &mut eve_machonet::SessionState,
    attacker: &mut eve_machonet::SessionState,
    ship: i64,
) -> f64 {
    rpc(ctx, sess, "ship", "Board", &[PyRep::Integer(ship)]).await.expect("Board");
    ctx.advance_secs(15);
    let before = balance(ctx);
    travel(ctx, sess, 30_000_142).await;
    if attacker.station_id.is_some() {
        rpc(ctx, attacker, "stationSvc", "Undock", &[]).await.expect("attacker Undock");
        ctx.advance_secs(15);
    }
    let jita_h = ctx.systems.get_or_spawn(30_000_142);
    let att_ship = attacker.ship_id.expect("attacker ship");
    let beyonce_vic_bind = common::borrow_wire_harness(ctx, sess)
        .call_ok("beyonce", None, "MachoBindObject", &[PyRep::Integer(30_000_142), PyRep::None], &HashMap::new())
        .await;
    let beyonce_vic = extract_bound_id_from_wire_rep(&beyonce_vic_bind).expect("bind beyonce victim");
    let _ = common::borrow_wire_harness(ctx, sess)
        .call("beyonce", Some(beyonce_vic), "CmdStop", &[], &HashMap::new())
        .await;
    let beyonce_att_bind = common::borrow_wire_harness(ctx, attacker)
        .call_ok("beyonce", None, "MachoBindObject", &[PyRep::Integer(30_000_142), PyRep::None], &HashMap::new())
        .await;
    let beyonce_att = extract_bound_id_from_wire_rep(&beyonce_att_bind).expect("bind beyonce attacker");
    common::borrow_wire_harness(ctx, attacker)
        .call_ok(
            "beyonce",
            Some(beyonce_att),
            "CmdFollowBall",
            &[PyRep::Integer(ship), PyRep::Real(1_000.0)],
            &HashMap::new(),
        )
        .await;
    let mut last_d = f64::MAX;
    for _ in 0..1_200 {
        last_d = match (jita_h.get_ball_snapshot(att_ship).await, jita_h.get_ball_snapshot(ship).await) {
            (Ok(Some((_, a))), Ok(Some((_, b)))) => a.position.distance(b.position),
            _ => 0.0,
        };
        if last_d <= 2_500.0 {
            break;
        }
        jita_h.step_manual(1.0).await.expect("step approach");
    }
    assert!(last_d <= 2_500.0, "attacker failed to close range to victim: {last_d}m");
    let _ = common::borrow_wire_harness(ctx, attacker)
        .call("beyonce", Some(beyonce_att), "CmdStop", &[], &HashMap::new())
        .await;
    ctx.advance_secs(35);
    rpc(ctx, attacker, "crimewatch", "StartDuelChallenge", &[PyRep::Integer(PILOT_1)]).await.expect("StartDuelChallenge");
    rpc(ctx, sess, "crimewatch", "RespondToDuelChallenge", &[PyRep::Integer(PILOT_2), PyRep::Bool(true)]).await.expect("RespondToDuelChallenge");
    rpc(ctx, attacker, "dogmaIM", "AddTarget", &[PyRep::Integer(ship)]).await.expect("AddTarget");
    ctx.advance_secs(10);
    jita_h.step_manual(10.0).await.expect("step lock");
    rpc(
        ctx,
        attacker,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(PILOT_2_GUN), PyRep::String("projectileFired".into()), PyRep::Integer(ship)],
    )
    .await
    .expect("Activate projectileFired");
    for _ in 0..120 {
        if jita_h.get_ball_snapshot(ship).await.ok().flatten().is_none() {
            break;
        }
        ctx.advance_secs(5);
        jita_h.step_manual(5.0).await.expect("step combat");
    }
    let _ = rpc(ctx, attacker, "dogmaIM", "Deactivate", &[PyRep::Integer(PILOT_2_GUN), PyRep::String("projectileFired".into())]).await;
    let rem_cs = jita_h.get_combat_state(ship).await.ok().flatten();
    let rem_d = match (jita_h.get_ball_snapshot(att_ship).await, jita_h.get_ball_snapshot(ship).await) {
        (Ok(Some((_, a))), Ok(Some((_, b)))) => Some(a.position.distance(b.position)),
        _ => None,
    };
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&ship)), "destroyed hull is gone (dist={rem_d:?}, combat={rem_cs:?})");
    let delta = balance(ctx) - before;
    dock_station(ctx, sess, JITA_STATION).await;
    delta
}

#[tokio::test]
async fn test_live_insurance_void_triggers_expiry_and_loss_payout_mail() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    seed_full_market_catalog(&ctx.db, &ctx.sde);
    fund_pilots(&ctx, &[(PILOT_1, 20_000_000_000.0), (PILOT_2, 1_000_000_000.0)]).await;
    let mut s1 = login(&ctx, 8911, "demo", PILOT_1).await;
    let mut s2 = login(&ctx, 8912, "other", PILOT_2).await;
    let att_ship = s2.ship_id.expect("PILOT_2 ship");
    let ammo = buy(&ctx, &mut s2, EMP_S, 1_000).await[0];
    move_item(&ctx, &mut s2, ammo, att_ship, 5).await;
    rpc(&ctx, &mut s2, "dogmaIM", "LoadAmmo", &[PyRep::Integer(att_ship), PyRep::List(vec![PyRep::Integer(PILOT_2_GUN)]), PyRep::List(vec![PyRep::Integer(ammo)])])
        .await
        .expect("LoadAmmo");

    // 1. Platinum contract with a 12-week term; repackaging voids it.
    let ship_a = new_hull(&ctx, &mut s1).await;
    let ins = rpc(&ctx, &mut s1, "insuranceSvc", "InsureShip", &[PyRep::Integer(ship_a), PyRep::Real(1.0)]).await.expect("InsureShip A");
    let list = contracts(&ctx, &mut s1).await;
    let c_a = contract_for(&list, ship_a).unwrap_or_else(|| panic!("contract for A after {ins:?}"));
    let term = c_a.dict_get("endDate").and_then(|v| v.as_i64()).expect("endDate") - c_a.dict_get("startDate").and_then(|v| v.as_i64()).expect("startDate");
    assert_eq!(term, 84 * 86_400 * FILETIME_TICKS_PER_SEC, "insurance lasts 12 weeks");
    assert_eq!(c_a.dict_get("fraction").and_then(|v| v.as_f64()), Some(1.0));
    let platinum_payout = c_a.dict_get("payoutISK").and_then(|v| v.as_f64()).expect("payoutISK");
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
    common::borrow_wire_harness(&ctx, &mut s1)
        .call_ok("contractProxy", None, "CreateContract", &[params], &kw)
        .await;
    assert!(contract_for(&contracts(&ctx, &mut s1).await, ship_b).is_none(), "contracting voids insurance");

    // 3. A contract older than 12 weeks has lapsed: the loss pays only the default 40%.
    let ship_c = new_hull(&ctx, &mut s1).await;
    rpc(&ctx, &mut s1, "insuranceSvc", "InsureShip", &[PyRep::Integer(ship_c), PyRep::Real(1.0)]).await.expect("InsureShip C");
    ctx.advance_virtual_clock(85 * 86_400);
    assert!(contract_for(&contracts(&ctx, &mut s1).await, ship_c).is_none(), "12-week term expired");
    let lapsed_payout = lose_ship(&ctx, &mut s1, &mut s2, ship_c).await;
    assert!(lapsed_payout > 0.0 && lapsed_payout < platinum_payout * 0.75, "lapsed hull gets the default 40% payout ({lapsed_payout} vs {platinum_payout})");

    // 4. A live Platinum contract pays immediately on loss and sends the insurance EVE-mail.
    let ship_d = new_hull(&ctx, &mut s1).await;
    rpc(&ctx, &mut s1, "insuranceSvc", "InsureShip", &[PyRep::Integer(ship_d), PyRep::Real(1.0)]).await.expect("InsureShip D");
    let d_payout = contract_for(&contracts(&ctx, &mut s1).await, ship_d).and_then(|c| c.dict_get("payoutISK")).and_then(|v| v.as_f64()).expect("payoutISK");
    let paid = lose_ship(&ctx, &mut s1, &mut s2, ship_d).await;
    assert!((paid - d_payout).abs() < 0.02, "loss pays the contracted Platinum payout ({paid} vs {d_payout})");
    assert!(contract_for(&contracts(&ctx, &mut s1).await, ship_d).is_none(), "contract consumed by the payout");
    let mails = rpc(&ctx, &mut s1, "mailMgr", "SyncMail", &[]).await.expect("SyncMail");
    assert!(format!("{mails:?}").contains(&format!("Insurance Payout for Ship {ship_d}")), "insurance EVE-mail: {mails:?}");
}
