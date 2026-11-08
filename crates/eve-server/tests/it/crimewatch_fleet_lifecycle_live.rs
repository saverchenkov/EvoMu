//! Live integration tests for Crimewatch and Fleet parity batches.

use crate::common::{gm_pilot_in_space, pin_clock, slash_cmd, LivePilot};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::combat::crimewatch::compute_sec_status_loss;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const ASABONA_SYS: u32 = 30_000_012;
const JITA_SYS: u32 = 30_000_142;
const JITA_STATION: i64 = 60_003_760;
const TYPE_CATALYST: u32 = 16_240;
const TYPE_VELATOR: u32 = 606;
const TYPE_CAPSULE: u32 = 670;
const TYPE_PIRATE_RAT: u32 = 2072;

async fn rpc(
    ctx: &ServerContext,
    p: &mut LivePilot,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(&mut p.sess, service, None, method, args, &HashMap::new(), None)
        .await
}

async fn bind_oid(ctx: &ServerContext, p: &mut LivePilot, service: &str, arg: PyRep) -> u64 {
    let rep = rpc(ctx, p, service, "MachoBindObject", &[arg, PyRep::None])
        .await
        .expect("MachoBindObject");
    let PyRep::Tuple(parts) = rep else { panic!("expected bound tuple") };
    parts[0].as_i64().expect("oid") as u64
}

fn drain(ctx: &ServerContext, p: &mut LivePilot) -> Vec<(String, Vec<PyRep>)> {
    let mut out = p.sess.drain_notifications();
    for n in ctx.drain_notifications_for_char(p.char_id) {
        if !out.iter().any(|(e, a)| e == &n.event && a == &n.args) {
            out.push((n.event, n.args));
        }
    }
    out
}

async fn get_sec(ctx: &ServerContext, p: &mut LivePilot) -> f64 {
    rpc(ctx, p, "crimewatch", "GetCharacterSecurityStatus", &[PyRep::Integer(p.char_id)])
        .await
        .expect("GetCharacterSecurityStatus")
        .as_f64()
        .expect("f64 sec")
}

async fn get_isk(ctx: &ServerContext, p: &mut LivePilot) -> f64 {
    rpc(ctx, p, "account", "GetCashBalance", &[PyRep::Bool(false)])
        .await
        .expect("GetCashBalance")
        .as_f64()
        .expect("f64 isk")
}

async fn dock_at(ctx: &ServerContext, p: &mut LivePilot, sys_id: u32, station_id: i64) {
    ctx.advance_secs(12);
    slash_cmd(ctx, &mut p.sess, &format!("/tr {sys_id}")).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    sys.set_ball_position(p.ship_id, ctx.sde.stations[&station_id].position + DVec3::new(300.0, 0.0, 0.0)).await;
    sys.step_manual(0.1).await.unwrap();
    rpc(ctx, p, "stationSvc", "Dock", &[PyRep::Integer(station_id)]).await.expect("Dock");
}

async fn fit_guns(ctx: &ServerContext, p: &mut LivePilot, count: usize) -> Vec<i64> {
    let orig_sys = p.sess.solar_system_id.unwrap_or(ASABONA_SYS);
    let ship_id = p.ship_id;
    dock_at(ctx, p, JITA_SYS, JITA_STATION).await;
    for sk in [3300_u32, 3301, 3302, 3303, 3304, 3305, 3306, 3310, 3392, 3394, 3413, 3416, 3418, 3419, 3425, 3426, 11084] {
        slash_cmd(ctx, &mut p.sess, &format!("/giveskill {sk} 5")).await;
    }
    let mse = slash_cmd(ctx, &mut p.sess, "/createitem 3831 1").await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    rpc(ctx, p, "invbroker", "Add", &[PyRep::Integer(mse), PyRep::Integer(ship_id), PyRep::Integer(19)]).await.unwrap();
    let dcu = slash_cmd(ctx, &mut p.sess, "/createitem 2048 1").await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    rpc(ctx, p, "invbroker", "Add", &[PyRep::Integer(dcu), PyRep::Integer(ship_id), PyRep::Integer(11)]).await.unwrap();
    let hi_cap = ctx.db.with_snapshot(|s| s.items.get(&ship_id).and_then(|i| ctx.sde.types.get(&i.type_id)).and_then(|t| t.default_attributes.get(&14)).map(|v| *v as usize)).unwrap_or(count);
    let mut ids = Vec::with_capacity(count.min(hi_cap));
    for idx in 0..count.min(hi_cap) {
        let gid = slash_cmd(ctx, &mut p.sess, "/createitem 2881 1").await.dict_get("itemID").and_then(|v| v.as_i64()).expect("gun id");
        rpc(ctx, p, "invbroker", "Add", &[PyRep::Integer(gid), PyRep::Integer(ship_id), PyRep::Integer(27 + idx as i64)]).await.expect("fit gun");
        ids.push(gid);
    }
    let ammo_id = slash_cmd(ctx, &mut p.sess, "/createitem 185 2000").await.dict_get("itemID").and_then(|v| v.as_i64()).expect("ammo id");
    rpc(ctx, p, "invbroker", "Add", &[PyRep::Integer(ammo_id), PyRep::Integer(ship_id), PyRep::Integer(5)]).await.expect("move ammo");
    let gun_reps: Vec<PyRep> = ids.iter().map(|&g| PyRep::Integer(g)).collect();
    rpc(ctx, p, "dogmaIM", "LoadAmmo", &[PyRep::Integer(ship_id), PyRep::List(gun_reps), PyRep::List(vec![PyRep::Integer(ammo_id)])]).await.expect("LoadAmmo");
    ctx.advance_secs(31);
    rpc(ctx, p, "stationSvc", "Undock", &[]).await.expect("Undock");
    slash_cmd(ctx, &mut p.sess, &format!("/tr {orig_sys}")).await;
    p.beyonce = bind_oid(ctx, p, "beyonce", PyRep::Integer(i64::from(orig_sys))).await;
    ctx.advance_secs(31);
    ids
}

async fn move_within_range(ctx: &ServerContext, attacker: &mut LivePilot, sys_id: u32, target_id: i64, range_m: f64) {
    let sys = ctx.systems.get_or_spawn(sys_id);
    if let (Some((_, a)), Some((_, b))) = (
        sys.get_ball_snapshot(attacker.ship_id).await.unwrap(),
        sys.get_ball_snapshot(target_id).await.unwrap(),
    ) {
        if a.position.distance(b.position) > range_m {
            sys.set_ball_position(attacker.ship_id, b.position + DVec3::new(range_m, 0.0, 0.0)).await;
            sys.step_manual(0.1).await.unwrap();
        }
    }
}

async fn lock_target(ctx: &ServerContext, attacker: &mut LivePilot, sys_id: u32, target_id: i64) {
    move_within_range(ctx, attacker, sys_id, target_id, 2_500.0).await;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let d_oid = bind_oid(ctx, attacker, "dogmaIM", PyRep::Integer(attacker.ship_id)).await;
    ctx.dispatch_rpc(&mut attacker.sess, "dogmaIM", Some(d_oid), "AddTarget", &[PyRep::Integer(target_id)], &HashMap::new(), None)
        .await
        .expect("AddTarget");
    for _ in 0..8 {
        ctx.advance_secs(1);
        sys.step_manual(1.0).await.unwrap();
        slash_cmd(ctx, &mut attacker.sess, "/heal").await;
    }
}

async fn shoot_and_destroy(ctx: &ServerContext, attacker: &mut LivePilot, sys_id: u32, guns: &[i64], target_id: i64) {
    ctx.advance_secs(65);
    let sys = ctx.systems.get_or_spawn(sys_id);
    lock_target(ctx, attacker, sys_id, target_id).await;
    for _ in 0..30 {
        if sys.get_ball_snapshot(target_id).await.unwrap().is_none() {
            return;
        }
        for &gid in guns {
            if sys.get_ball_snapshot(target_id).await.unwrap().is_none() {
                return;
            }
            let _ = rpc(ctx, attacker, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String("targetAttack".into()), PyRep::Integer(target_id)]).await;
        }
        for _ in 0..5 {
            sys.step_manual(1.0).await.unwrap();
        }
        ctx.advance_secs(5);
    }
    assert!(sys.get_ball_snapshot(target_id).await.unwrap().is_none(), "target {target_id} must be destroyed");
}

async fn destroy_and_find_pod(ctx: &ServerContext, attacker: &mut LivePilot, sys_id: u32, guns: &[i64], ship_id: i64) -> i64 {
    let sys = ctx.systems.get_or_spawn(sys_id);
    if sys.get_ball_snapshot(ship_id).await.unwrap().is_none() {
        return sys.list_balls().await.unwrap().into_iter().rfind(|b| b.type_id == TYPE_CAPSULE).map(|b| b.id).expect("existing capsule");
    }
    let before: std::collections::HashSet<i64> = sys.list_balls().await.unwrap().into_iter().filter(|b| b.type_id == TYPE_CAPSULE).map(|b| b.id).collect();
    shoot_and_destroy(ctx, attacker, sys_id, guns, ship_id).await;
    sys.list_balls().await.unwrap().into_iter().find(|b| b.type_id == TYPE_CAPSULE && !before.contains(&b.id)).map(|b| b.id).expect("spawned capsule")
}

#[tokio::test]
async fn test_live_crimewatch_sec_status_loss_and_transactions() {
    // parity: feat:crimewatch.sec-status-loss, feat:crimewatch.sec-status-transactions
    eve_testkit::attest(&[
        "feat:crimewatch.sec-status-loss",
        "feat:crimewatch.sec-status-transactions",
    ]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut aggr = gm_pilot_in_space(&ctx, 11, 93_100_001, TYPE_CATALYST, ASABONA_SYS).await;
    let mut vict = gm_pilot_in_space(&ctx, 12, 93_100_002, TYPE_VELATOR, ASABONA_SYS).await;
    let guns = fit_guns(&ctx, &mut aggr, 7).await;

    rpc(&ctx, &mut aggr, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    let target_sec = 3.2_f64;
    slash_cmd(&ctx, &mut aggr.sess, &format!("/secstatus {} {target_sec}", vict.char_id)).await;
    assert!((get_sec(&ctx, &mut vict).await - target_sec).abs() < 1e-6);

    // 1. Low-sec ship attack + kill (non-zero target sec, starting aggressor sec 0.0 -> new sec)
    let low_sec_sys_val = ctx.sde.solar_systems[&ASABONA_SYS].security_status;
    let _sec0 = get_sec(&ctx, &mut aggr).await;
    let vict_ship1 = vict.ship_id;
    ctx.advance_secs(65);
    lock_target(&ctx, &mut aggr, ASABONA_SYS, vict_ship1).await;
    let aggr_ship = aggr.ship_id;
    let d_oid = bind_oid(&ctx, &mut aggr, "dogmaIM", PyRep::Integer(aggr_ship)).await;
    let sys_low = ctx.systems.get_or_spawn(ASABONA_SYS);
    let mut saw_ship_loss = false;
    while sys_low.get_ball_snapshot(vict_ship1).await.unwrap().is_some() {
        for &gid in &guns {
            let before = get_sec(&ctx, &mut aggr).await;
            if ctx
                .dispatch_rpc(
                    &mut aggr.sess,
                    "dogmaIM",
                    Some(d_oid),
                    "Activate",
                    &[
                        PyRep::Integer(gid),
                        PyRep::String("targetAttack".into()),
                        PyRep::Integer(vict_ship1),
                    ],
                    &HashMap::new(),
                    None,
                )
                .await
                .is_ok()
            {
                let after = get_sec(&ctx, &mut aggr).await;
                if (before - after).abs() > 1e-12 {
                    let expected_drop = compute_sec_status_loss(before, target_sec, false, low_sec_sys_val);
                    assert!((before - after - expected_drop).abs() < 1e-9);
                    saw_ship_loss = true;
                }
            }
            if sys_low.get_ball_snapshot(vict_ship1).await.unwrap().is_none() {
                break;
            }
        }
        ctx.advance_secs(4);
        sys_low.step_manual(4.0).await.unwrap();
    }
    assert!(saw_ship_loss, "low-sec ship aggression must apply exact sec loss formula");

    // 2. Low-sec pod kill with non-zero aggressor & target sec
    let pod_id = destroy_and_find_pod(&ctx, &mut aggr, ASABONA_SYS, &guns, vict_ship1).await;
    let before_pod2 = get_sec(&ctx, &mut aggr).await;
    shoot_and_destroy(&ctx, &mut aggr, ASABONA_SYS, &guns, pod_id).await;
    let after_pod = get_sec(&ctx, &mut aggr).await;
    assert!((before_pod2 - after_pod - compute_sec_status_loss(before_pod2, target_sec, true, low_sec_sys_val)).abs() < 1e-6);
    let _ = vict.sess.drain_notifications();

    // 3. High-sec ship aggression (10x higher system sec factor) & GetSecurityStatusTransactions log
    let mut vict2 = gm_pilot_in_space(&ctx, 13, 93_100_003, TYPE_VELATOR, JITA_SYS).await;
    slash_cmd(&ctx, &mut aggr.sess, &format!("/tr {JITA_SYS}")).await;
    ctx.advance_secs(31);
    let high_sec_sys_val = ctx.sde.solar_systems[&JITA_SYS].security_status;
    let before_hs = get_sec(&ctx, &mut aggr).await;
    shoot_and_destroy(&ctx, &mut aggr, JITA_SYS, &guns, vict2.ship_id).await;
    let after_hs = get_sec(&ctx, &mut aggr).await;
    assert!(before_hs - after_hs >= compute_sec_status_loss(before_hs, 0.0, false, high_sec_sys_val) - 1e-9);
    let _ = vict2.sess.drain_notifications();

    let PyRep::Rowset { columns, rows } = rpc(&ctx, &mut aggr, "crimewatch", "GetSecurityStatusTransactions", &[]).await.expect("GetSecurityStatusTransactions") else {
        panic!("expected Rowset");
    };
    assert_eq!(
        columns,
        vec![
            "eventDate",
            "eventTypeID",
            "locationID",
            "modification",
            "newValue",
            "otherOwnerID",
            "otherTypeID",
        ]
    );
    assert!(rows.iter().any(|r| {
        r[1] == PyRep::Integer(379)
            && r[3].as_f64().unwrap_or(0.0) < 0.0
            && r[5] == PyRep::Integer(vict.char_id)
    }));
}

#[tokio::test]
async fn test_live_crimewatch_kill_rights_gain_sell_and_activate() {
    // parity: feat:crimewatch.kill-rights-gain, feat:crimewatch.kill-rights-sell
    eve_testkit::attest(&[
        "feat:crimewatch.kill-rights-gain",
        "feat:crimewatch.kill-rights-sell",
    ]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut killer = gm_pilot_in_space(&ctx, 21, 93_200_001, TYPE_CATALYST, ASABONA_SYS).await;
    let mut victim = gm_pilot_in_space(&ctx, 22, 93_200_002, TYPE_VELATOR, ASABONA_SYS).await;
    let mut buyer = gm_pilot_in_space(&ctx, 23, 93_200_003, TYPE_VELATOR, ASABONA_SYS).await;
    let guns = fit_guns(&ctx, &mut killer, 7).await;
    rpc(&ctx, &mut killer, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    let _ = drain(&ctx, &mut victim);

    // 2. Low-sec ship destruction grants NO kill right; low-sec pod kill grants 30-day kill right + OnKillRightCreated
    let v_ship1 = victim.ship_id;
    let pod1 = destroy_and_find_pod(&ctx, &mut killer, ASABONA_SYS, &guns, v_ship1).await;
    let PyRep::List(krs_after_ship) = rpc(&ctx, &mut victim, "bountyProxy", "GetMyKillRights", &[]).await.unwrap() else {
        panic!("list");
    };
    assert!(krs_after_ship.is_empty(), "low-sec ship kill must not grant kill right");

    let t_before = ctx.now_unix() as i64;
    shoot_and_destroy(&ctx, &mut killer, ASABONA_SYS, &guns, pod1).await;
    let t_after = ctx.now_unix() as i64;
    let PyRep::List(krs_after_pod) = rpc(&ctx, &mut victim, "bountyProxy", "GetMyKillRights", &[]).await.unwrap() else {
        panic!("list");
    };
    assert_eq!(krs_after_pod.len(), 1);
    let kr1_id = krs_after_pod[0].dict_get("killRightID").and_then(|v| v.as_i64()).unwrap();
    let exp1 = krs_after_pod[0].dict_get("expiryTime").and_then(|v| v.as_i64()).unwrap();
    assert!(exp1 >= t_before + 2_592_000 && exp1 <= t_after + 2_592_000, "kill right expires after 30 days");
    assert!(drain(&ctx, &mut victim).iter().any(|(e, _)| e == "OnKillRightCreated"));

    // 3. High-sec ship kill grants kill right immediately upon ship destruction
    let mut victim2 = gm_pilot_in_space(&ctx, 24, 93_200_004, TYPE_VELATOR, JITA_SYS).await;
    slash_cmd(&ctx, &mut killer.sess, &format!("/tr {JITA_SYS}")).await;
    ctx.advance_secs(31);
    let _ = drain(&ctx, &mut victim2);
    shoot_and_destroy(&ctx, &mut killer, JITA_SYS, &guns, victim2.ship_id).await;
    assert!(drain(&ctx, &mut victim2).iter().any(|(e, _)| e == "OnKillRightCreated"));
    ctx.advance_secs(905);

    // 4. Sell -> Cancel -> OnKillRightSellCancel -> Re-list -> Buy (ISK + 15m Suspect) -> Expiry
    rpc(&ctx, &mut victim, "bountyProxy", "SellKillRight", &[PyRep::Integer(kr1_id), PyRep::Real(2_500_000.0), PyRep::None])
        .await
        .expect("SellKillRight");
    rpc(&ctx, &mut victim, "bountyProxy", "CancelSellKillRight", &[PyRep::Integer(kr1_id)])
        .await
        .expect("CancelSellKillRight");
    assert!(drain(&ctx, &mut victim).iter().any(|(e, _)| e == "OnKillRightSellCancel"));

    // Re-list for buyer, buy, check ISK transfer + OnKillRightSold / OnKillRightForYouSold + 900s Suspect
    ctx.advance_secs(31);
    let _ = rpc(&ctx, &mut killer, "stationSvc", "Undock", &[]).await;
    killer.ship_id = killer.sess.ship_id.unwrap_or(killer.ship_id);
    slash_cmd(&ctx, &mut killer.sess, &format!("/tr {ASABONA_SYS}")).await;
    slash_cmd(&ctx, &mut buyer.sess, &format!("/tr {ASABONA_SYS}")).await;
    slash_cmd(&ctx, &mut buyer.sess, "/giveisk 10000000").await;
    rpc(&ctx, &mut victim, "bountyProxy", "SellKillRight", &[PyRep::Integer(kr1_id), PyRep::Real(2_500_000.0), PyRep::Integer(buyer.char_id)]).await.expect("re-list SellKillRight");
    let v_isk_before = get_isk(&ctx, &mut victim).await;
    let b_isk_before = get_isk(&ctx, &mut buyer).await;
    let _ = drain(&ctx, &mut killer);
    rpc(&ctx, &mut buyer, "killRightMgr", "BuyKillRight", &[PyRep::Integer(kr1_id), PyRep::Real(2_500_000.0)]).await.expect("BuyKillRight");
    assert!((get_isk(&ctx, &mut victim).await - v_isk_before - 2_500_000.0).abs() < 1.0);
    assert!((b_isk_before - get_isk(&ctx, &mut buyer).await - 2_500_000.0).abs() < 1.0);
    assert!(drain(&ctx, &mut victim).iter().any(|(e, _)| e == "OnKillRightForYouSold"));
    assert!(drain(&ctx, &mut killer).iter().any(|(e, _)| e == "OnKillRightUsed"));
    ctx.advance_secs(901);

    // 5. Personal 0-fee activation on kr2 + 30-day expiry on kr3
    dock_at(&ctx, &mut killer, JITA_SYS, JITA_STATION).await;
    let req_skills: Vec<u32> = ctx.sde.types.get(&TYPE_VELATOR).map(|t| [182_u16, 183, 184, 1285, 1289, 1290].iter().filter_map(|a| t.default_attributes.get(a)).map(|v| v.round() as u32).filter(|&s| s > 0).collect()).unwrap_or_default();
    for sk in req_skills { slash_cmd(&ctx, &mut killer.sess, &format!("/giveskill {sk} 5")).await; }
    let new_vel_id = slash_cmd(&ctx, &mut killer.sess, &format!("/createitem {TYPE_VELATOR} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    ctx.advance_secs(31);
    rpc(&ctx, &mut killer, "ship", "Board", &[PyRep::Integer(new_vel_id)]).await.expect("Board");
    let mut guns2 = Vec::new();
    for idx in 0..2 {
        let gid = slash_cmd(&ctx, &mut killer.sess, "/createitem 2881 1").await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
        rpc(&ctx, &mut killer, "invbroker", "Add", &[PyRep::Integer(gid), PyRep::Integer(new_vel_id), PyRep::Integer(27 + idx)]).await.unwrap();
        guns2.push(gid);
    }
    let ammo2 = slash_cmd(&ctx, &mut killer.sess, "/createitem 185 2000").await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut killer, "invbroker", "Add", &[PyRep::Integer(ammo2), PyRep::Integer(new_vel_id), PyRep::Integer(5)]).await.unwrap();
    rpc(&ctx, &mut killer, "dogmaIM", "LoadAmmo", &[PyRep::Integer(new_vel_id), PyRep::List(guns2.iter().map(|&g| PyRep::Integer(g)).collect()), PyRep::List(vec![PyRep::Integer(ammo2)])]).await.unwrap();
    ctx.advance_secs(31);
    rpc(&ctx, &mut killer, "stationSvc", "Undock", &[]).await.expect("Undock");
    killer.ship_id = new_vel_id;
    killer.sess.ship_id = Some(new_vel_id);
    slash_cmd(&ctx, &mut killer.sess, &format!("/tr {ASABONA_SYS}")).await;
    slash_cmd(&ctx, &mut victim2.sess, &format!("/tr {ASABONA_SYS}")).await;
    let PyRep::List(krs2) = rpc(&ctx, &mut victim2, "bountyProxy", "GetMyKillRights", &[]).await.unwrap() else {
        panic!("list");
    };
    let kr2_id = krs2[0].dict_get("killRightID").and_then(|v| v.as_i64()).unwrap();
    rpc(&ctx, &mut victim2, "killRightMgr", "ActivateKillRight", &[PyRep::Integer(kr2_id)]).await.expect("ActivateKillRight");

    let mut victim3 = gm_pilot_in_space(&ctx, 25, 93_200_005, TYPE_VELATOR, JITA_SYS).await;
    slash_cmd(&ctx, &mut killer.sess, &format!("/tr {JITA_SYS}")).await;
    shoot_and_destroy(&ctx, &mut killer, JITA_SYS, &guns2, victim3.ship_id).await;
    let PyRep::List(krs3) = rpc(&ctx, &mut victim3, "bountyProxy", "GetMyKillRights", &[]).await.unwrap() else {
        panic!("list");
    };
    assert_eq!(krs3.len(), 1);
    ctx.advance_secs(2_592_001);
    let PyRep::List(krs3_expired) = rpc(&ctx, &mut victim3, "bountyProxy", "GetMyKillRights", &[]).await.unwrap() else {
        panic!("list");
    };
    assert!(krs3_expired.is_empty(), "kill right must expire at +30d");
}

#[tokio::test]
async fn test_live_crimewatch_sec_gain_caps_and_tag_tier_bands() {
    // parity: feat:crimewatch.sec-status-gain-caps, feat:crimewatch.sec-tag-tier-bands, feat:crimewatch.sec-status-transactions
    eve_testkit::attest(&[
        "feat:crimewatch.sec-status-gain-caps",
        "feat:crimewatch.sec-tag-tier-bands",
        "feat:crimewatch.sec-status-transactions",
    ]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut pilot = gm_pilot_in_space(&ctx, 31, 93_300_001, 24_700, ASABONA_SYS).await;

    // 1. Floor -10.0 and ceiling +10.0 clamp & high-sec station rejects tags with SecurityOfficeRequired
    let clamped_hi = slash_cmd(&ctx, &mut pilot.sess, "/secstatus 12.5").await.as_f64().unwrap();
    assert_eq!(clamped_hi, 10.0);
    slash_cmd(&ctx, &mut pilot.sess, "/secstatus 0.0").await;
    dock_at(&ctx, &mut pilot, JITA_SYS, JITA_STATION).await;
    let clamped_lo = slash_cmd(&ctx, &mut pilot.sess, "/secstatus -12.5").await.as_f64().unwrap();
    assert_eq!(clamped_lo, -10.0);
    let hs_err = rpc(
        &ctx,
        &mut pilot,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(-9.5), PyRep::Dict(vec![(PyRep::Integer(33138), PyRep::Integer(1))])],
    )
    .await
    .unwrap_err();
    assert_eq!(hs_err.code, "SecurityOfficeRequired");
    ctx.advance_secs(12);
    rpc(&ctx, &mut pilot, "stationSvc", "Undock", &[]).await.expect("Undock Jita");

    // 3. Dock at low-sec CONCORD/DED Security Office (service 27, 0 < sec < 0.45) & turn in all 4 tiers (+0.5/tag up to 0.0)
    let (sec_station_id, sec_sys_id) = ctx
        .sde
        .stations
        .values()
        .filter(|s| {
            ctx.sde
                .solar_systems
                .get(&s.solar_system_id)
                .is_some_and(|sys| 0.0 < sys.security_status && sys.security_status < 0.45)
                && eve_server::services::station_structure::npc_station_offers_service(&ctx.sde, s.station_id, 27)
        })
        .map(|s| (s.station_id, s.solar_system_id))
        .next()
        .expect("lowsec CONCORD Security Office station");
    dock_at(&ctx, &mut pilot, sec_sys_id, sec_station_id).await;
    slash_cmd(&ctx, &mut pilot.sess, "/giveisk 500000000").await;

    for (tag_tid, bad_tag, qty, expected_sec, expected_fee) in [
        (33138_i64, 33139_i64, 4_i64, -8.0_f64, 4_000_000.0_f64),
        (33139, 33138, 6, -5.0, 12_000_000.0),
        (33140, 33141, 6, -2.0, 30_000_000.0),
        (33141, 33140, 4, 0.0, 40_000_000.0),
    ] {
        slash_cmd(&ctx, &mut pilot.sess, &format!("/createitem {bad_tag} 1")).await;
        slash_cmd(&ctx, &mut pilot.sess, &format!("/createitem {tag_tid} {qty}")).await;
        let bad_err = rpc(
            &ctx,
            &mut pilot,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(expected_sec), PyRep::Dict(vec![(PyRep::Integer(bad_tag), PyRep::Integer(1))])],
        )
        .await
        .unwrap_err();
        assert_eq!(bad_err.code, "SecurityTagBandMismatch");

        let isk_pre = get_isk(&ctx, &mut pilot).await;
        let ok = rpc(
            &ctx,
            &mut pilot,
            "crimewatch",
            "ApplyTagsForSecStatus",
            &[PyRep::Real(expected_sec), PyRep::Dict(vec![(PyRep::Integer(tag_tid), PyRep::Integer(qty))])],
        )
        .await
        .expect("in-band tag turn-in");
        assert!((ok.dict_get("newSecurityStatus").and_then(|v| v.as_f64()).unwrap() - expected_sec).abs() < 1e-9);
        assert!((isk_pre - get_isk(&ctx, &mut pilot).await - expected_fee).abs() < 1.0);
    }
    let over_cap = rpc(
        &ctx,
        &mut pilot,
        "crimewatch",
        "ApplyTagsForSecStatus",
        &[PyRep::Real(0.5), PyRep::Dict(vec![(PyRep::Integer(33141), PyRep::Integer(1))])],
    )
    .await
    .unwrap_err();
    assert_eq!(over_cap.code, "InvalidTargetSecurityStatus");

    // 4. Undock, fit guns, and earn security status from Mission rat, Belt pirate rat, and Incursion rat up to +5.0 cap
    ctx.advance_secs(12);
    rpc(&ctx, &mut pilot, "stationSvc", "Undock", &[]).await.unwrap();
    pilot.ship_id = pilot.sess.ship_id.unwrap_or(pilot.ship_id);
    let guns = fit_guns(&ctx, &mut pilot, 7).await;

    let PyRep::Rowset { rows: l1_agents, .. } = rpc(&ctx, &mut pilot, "agentMgr", "GetAgents", &[PyRep::Integer(1), PyRep::Integer(24)]).await.unwrap() else { panic!("agents rowset") };
    let mut mission_ok = false;
    for row in l1_agents.iter() {
        if row[1].as_i64() == Some(9) {
            continue;
        }
        let aid = row[0].as_i64().unwrap();
        let Some(asys) = row[5].as_i64().map(|v| v as u32) else { continue };
        if ctx.sde.solar_systems.get(&asys).is_none_or(|s| s.security_status < 0.7) {
            continue;
        }
        let Some(astation) = ctx.sde.agents.get(&aid).and_then(|a| a.station_id) else { continue };
        ctx.db.with_mut_snapshot(|s| {
            if let Some(ch) = s.characters.get_mut(&pilot.char_id) {
                ch.solar_system_id = asys;
                ch.station_id = Some(astation);
                ch.structure_id = None;
            }
            if let Some(ship) = s.items.get_mut(&pilot.ship_id) {
                ship.location_id = astation;
                ship.flag = eve_db::InventoryFlag::Hangar;
            }
        });
        pilot.sess.solar_system_id = Some(asys);
        pilot.sess.station_id = Some(astation);
        pilot.sess.structure_id = None;
        let a_oid = bind_oid(&ctx, &mut pilot, "agentMgr", PyRep::Integer(aid)).await;
        let offer = ctx.dispatch_rpc(&mut pilot.sess, "agentMgr", Some(a_oid), "DoAction", &[PyRep::Integer(2)], &HashMap::new(), None).await.unwrap();
        let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
        ctx.dispatch_rpc(&mut pilot.sess, "agentMgr", Some(a_oid), "DoAction", &[PyRep::Integer(3)], &HashMap::new(), None).await.unwrap();
        ctx.advance_secs(12);
        rpc(&ctx, &mut pilot, "stationSvc", "Undock", &[]).await.unwrap();
        pilot.ship_id = pilot.sess.ship_id.unwrap_or(pilot.ship_id);
        let spawned = ctx.db.with_snapshot(|s| s.active_missions.get(&(pilot.char_id, mid)).map(|m| (m.solar_system_id, m.spawned_npc_ball_ids.clone())));
        if let Some((mission_sys, rats)) = spawned {
            if let Some(&m_rat_id) = rats.first() {
                slash_cmd(&ctx, &mut pilot.sess, &format!("/tr {mission_sys}")).await;
                slash_cmd(&ctx, &mut pilot.sess, "/heal").await;
                shoot_and_destroy(&ctx, &mut pilot, mission_sys, &guns, m_rat_id).await;
                mission_ok = true;
                break;
            }
        }
    }
    assert!(mission_ok, "mission rat kill must increase sec status");
    let sec_mission = get_sec(&ctx, &mut pilot).await;
    assert!(sec_mission > 0.0);

    // Belt rat kill in Asabona
    slash_cmd(&ctx, &mut pilot.sess, &format!("/tr {ASABONA_SYS}")).await;
    let nid = slash_cmd(&ctx, &mut pilot.sess, &format!("/spawnnpc {TYPE_PIRATE_RAT}")).await.dict_get("npcID").and_then(|v| v.as_i64()).unwrap();
    slash_cmd(&ctx, &mut pilot.sess, "/heal").await;
    shoot_and_destroy(&ctx, &mut pilot, ASABONA_SYS, &guns, nid).await;
    let sec_belt = get_sec(&ctx, &mut pilot).await;
    assert!(sec_belt > sec_mission);

    // Incursion rat kill via taleMgr + live combat
    let PyRep::List(tales) = rpc(&ctx, &mut pilot, "taleMgr", "GetGlobalWorldEventTales", &[]).await.unwrap() else {
        panic!("tales list");
    };
    let vg_sys = tales[0]
        .dict_get("vanguardSolarSystemIDs")
        .or_else(|| tales[0].dict_get("solarSystemIDs"))
        .and_then(|v| match v {
            PyRep::List(l) | PyRep::Tuple(l) => l.first().and_then(|x| x.as_i64()).map(|x| x as u32),
            _ => None,
        })
        .unwrap_or(ASABONA_SYS);
    let inc_rat_tid = ctx.sde.types.values().find(|t| t.name.contains("Centii") && t.default_attributes.get(&481).is_some_and(|b| *b > 0.0)).map(|t| t.type_id).unwrap_or(TYPE_PIRATE_RAT);
    slash_cmd(&ctx, &mut pilot.sess, &format!("/tr {vg_sys}")).await;
    ctx.advance_secs(901);
    let inc_rat_id = slash_cmd(&ctx, &mut pilot.sess, &format!("/spawnnpc {inc_rat_tid}")).await.dict_get("npcID").and_then(|v| v.as_i64()).unwrap();
    slash_cmd(&ctx, &mut pilot.sess, "/heal").await;
    shoot_and_destroy(&ctx, &mut pilot, vg_sys, &guns, inc_rat_id).await;
    assert!(get_sec(&ctx, &mut pilot).await > sec_belt);

    // Verify +5.0 cap on NPC kills (using /secstatus GM command, zero snapshot mutation)
    slash_cmd(&ctx, &mut pilot.sess, "/secstatus 5.0").await;
    ctx.advance_secs(905);
    let nid_cap = slash_cmd(&ctx, &mut pilot.sess, &format!("/spawnnpc {TYPE_PIRATE_RAT}")).await.dict_get("npcID").and_then(|v| v.as_i64()).unwrap();
    shoot_and_destroy(&ctx, &mut pilot, vg_sys, &guns, nid_cap).await;
    assert_eq!(get_sec(&ctx, &mut pilot).await, 5.0);
}

#[tokio::test]
async fn test_live_fleet_boss_handover_formations_and_tagging() {
    // parity: feat:fleet.boss-handover, feat:fleet.stored-formations-and-warp-shapes
    eve_testkit::attest(&[
        "feat:fleet.boss-handover",
        "feat:fleet.stored-formations-and-warp-shapes",
    ]);

    let ctx = ServerContext::new();
    pin_clock(&ctx);
    let mut fc = gm_pilot_in_space(&ctx, 41, 93_400_001, TYPE_CATALYST, ASABONA_SYS).await;
    let mut sc = gm_pilot_in_space(&ctx, 42, 93_400_002, TYPE_VELATOR, ASABONA_SYS).await;
    let mut m1 = gm_pilot_in_space(&ctx, 43, 93_400_003, TYPE_VELATOR, ASABONA_SYS).await;
    let mut m2 = gm_pilot_in_space(&ctx, 44, 93_400_004, TYPE_VELATOR, ASABONA_SYS).await;
    let mut foe = gm_pilot_in_space(&ctx, 45, 93_400_005, TYPE_VELATOR, ASABONA_SYS).await;
    let fc_guns = fit_guns(&ctx, &mut fc, 7).await;

    let fid = rpc(&ctx, &mut fc, "fleetObjectHandler", "CreateFleet", &[]).await.unwrap().as_i64().unwrap();
    fc.sess.fleet_id = Some(fid);
    for member in [&mut sc, &mut m1, &mut m2] {
        rpc(
            &ctx,
            &mut fc,
            "fleetObjectHandler",
            "Invite",
            &[PyRep::Integer(member.char_id), PyRep::None, PyRep::None, PyRep::Integer(4)],
        )
        .await
        .expect("Invite");
        rpc(&ctx, member, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fid)])
            .await
            .expect("AcceptInvite");
        member.sess.fleet_id = Some(fid);
    }

    // 1. Stored formations & live FleetWarp shapes (Point, Arrow, Sphere, Wall, Diamond)
    let dest_pos = ctx
        .sde
        .celestials_in_system(ASABONA_SYS)
        .into_iter()
        .filter(|c| c.position.length() > 1_000_000.0)
        .min_by_key(|c| c.position.length() as u64)
        .map(|c| c.position)
        .unwrap_or(DVec3::new(250_000_000.0, 10_000_000.0, 50_000_000.0));
    let sys = ctx.systems.get_or_spawn(ASABONA_SYS);
    for (i, sh) in [fc.ship_id, sc.ship_id, m1.ship_id, m2.ship_id].iter().enumerate() {
        sys.set_ball_position(*sh, DVec3::new(10_000.0 + (i as f64) * 150.0, 0.0, 0.0)).await;
    }
    sys.step_manual(0.1).await.unwrap();

    for (shape, spacing, size) in [
        ("Point", 5_000.0_f64, 10_000.0_f64),
        ("Arrow", 5_000.0, 10_000.0),
        ("Sphere", 12_000.0, 12_000.0),
        ("Plane", 5_000.0, 10_000.0),
        ("Diamond", 5_000.0, 10_000.0),
    ] {
        let fleet_id = fc.sess.fleet_id.unwrap();
        let f_oid = bind_oid(&ctx, &mut fc, "fleetObjectHandler", PyRep::Integer(fleet_id)).await;
        let set_res = ctx.dispatch_rpc(&mut fc.sess, "fleetObjectHandler", Some(f_oid), "SetFleetFormationSettings", &[PyRep::String(shape.into()), PyRep::Real(spacing), PyRep::Real(size)], &HashMap::new(), None).await.expect("SetFleetFormationSettings");
        assert_eq!(set_res.dict_get("formationName").and_then(|v| v.as_str()), Some(shape));
        let got = ctx.dispatch_rpc(&mut fc.sess, "fleetObjectHandler", Some(f_oid), "GetFormationSettings", &[], &HashMap::new(), None).await.unwrap();
        assert_eq!(got.dict_get("formationName").and_then(|v| v.as_str()), Some(shape));

        ctx.dispatch_rpc(&mut fc.sess, "fleetObjectHandler", Some(f_oid), "FleetWarp", &[PyRep::Real(dest_pos.x), PyRep::Real(dest_pos.y), PyRep::Real(dest_pos.z), PyRep::Real(10_000.0)], &HashMap::new(), None).await.expect("FleetWarp");
        for _ in 0..90 {
            sys.step_manual(1.0).await.unwrap();
            ctx.advance_secs(1);
            if !matches!(sys.get_ball_snapshot(fc.ship_id).await.unwrap().unwrap().1.mode, eve_destiny::MovementMode::Warp { .. }) {
                break;
            }
        }

        let p_fc = sys.get_ball_snapshot(fc.ship_id).await.unwrap().unwrap().1.position;
        let p_sc = sys.get_ball_snapshot(sc.ship_id).await.unwrap().unwrap().1.position;
        let p_m1 = sys.get_ball_snapshot(m1.ship_id).await.unwrap().unwrap().1.position;
        assert!(p_fc.distance(p_sc) <= 25_000.0 && p_fc.distance(p_m1) <= 25_000.0, "{shape} within command burst radius");
        if shape != "Point" {
            assert!(p_sc.distance(p_m1) >= 2_500.0, "{shape} separates fleet ships to avoid collision");
        }
        for (i, sh) in [fc.ship_id, sc.ship_id, m1.ship_id, m2.ship_id].iter().enumerate() {
            sys.set_ball_position(*sh, DVec3::new(10_000.0 + (i as f64) * 150.0, 0.0, 0.0)).await;
        }
        sys.step_manual(0.1).await.unwrap();
    }

    // 2. Fleet target tagging (CmdFleetTagTarget + OnFleetStateChange + GetInitState + live combat destruction clear)
    let fleet_id = fc.sess.fleet_id.unwrap();
    let f_oid = bind_oid(&ctx, &mut fc, "fleetObjectHandler", PyRep::Integer(fleet_id)).await;
    assert!(rpc(&ctx, &mut m1, "beyonce", "CmdFleetTagTarget", &[PyRep::Integer(foe.ship_id), PyRep::String("A".into())]).await.is_err());
    assert!(rpc(&ctx, &mut fc, "beyonce", "CmdFleetTagTarget", &[PyRep::Integer(foe.ship_id), PyRep::String("A".into())]).await.is_err());
    lock_target(&ctx, &mut fc, ASABONA_SYS, foe.ship_id).await;
    assert!(rpc(&ctx, &mut fc, "beyonce", "CmdFleetTagTarget", &[PyRep::Integer(foe.ship_id), PyRep::String("INVALID".into())]).await.is_err());
    let _ = drain(&ctx, &mut sc);
    rpc(&ctx, &mut fc, "beyonce", "CmdFleetTagTarget", &[PyRep::Integer(foe.ship_id), PyRep::String("A".into())]).await.expect("tag A");
    assert!(drain(&ctx, &mut sc).iter().any(|(e, _)| e == "OnFleetStateChange"));
    let init_tagged = ctx.dispatch_rpc(&mut fc.sess, "fleetObjectHandler", Some(f_oid), "GetInitState", &[], &HashMap::new(), None).await.unwrap();
    assert!(init_tagged.dict_get("targetTags").is_some_and(|d| d.dict_get(&foe.ship_id.to_string()).and_then(|v| v.as_str()) == Some("A") || !matches!(d, PyRep::None)));
    rpc(&ctx, &mut fc, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    shoot_and_destroy(&ctx, &mut fc, ASABONA_SYS, &fc_guns, foe.ship_id).await;
    let _ = foe.sess.drain_notifications();
    // 3. Boss handover: explicit MakeLeader + V24 rank priority on real /kick disconnect & LeaveFleet
    ctx.db.provision_player_account("live93400009", "live93400009-secret");
    let mut p_sess = eve_machonet::SessionState::new(49);
    crate::common::login_pilot(&ctx, &mut p_sess, "live93400009", "live93400009-secret").await;
    let doll = PyRep::keyval([("gender", PyRep::Integer(1))]);
    let anc = ctx.sde.ancestries.values().find(|a| a.bloodline_id == 1).map(|a| i64::from(a.ancestry_id)).unwrap_or(1);
    let sch = ctx.sde.schools.values().find(|s| s.race_id == 1).map(|s| i64::from(s.school_id)).unwrap_or(11);
    let p_cid = ctx.dispatch_rpc(&mut p_sess, "charUnboundMgr", None, "CreateCharacterWithDoll", &[PyRep::String("Fleet Member 9".into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(anc), doll.clone(), doll, PyRep::Integer(sch)], &HashMap::new(), None).await.unwrap().as_i64().unwrap();
    ctx.dispatch_rpc(&mut p_sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(p_cid)], &HashMap::new(), None).await.unwrap();
    ctx.advance_secs(31);
    let mut p1 = LivePilot { sess: p_sess, char_id: p_cid, ship_id: 0, beyonce: 0 };
    rpc(&ctx, &mut fc, "fleetObjectHandler", "Invite", &[PyRep::Integer(p1.char_id), PyRep::None, PyRep::None, PyRep::Integer(4)]).await.expect("Invite p1");
    rpc(&ctx, &mut p1, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fleet_id)]).await.expect("AcceptInvite p1");
    p1.sess.fleet_id = Some(fleet_id);
    ctx.advance_secs(31);
    ctx.dispatch_rpc(&mut fc.sess, "fleetObjectHandler", Some(f_oid), "MoveMember", &[PyRep::Integer(fc.char_id), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)], &HashMap::new(), None).await.expect("step down fc");
    ctx.advance_secs(31);
    ctx.dispatch_rpc(&mut fc.sess, "fleetObjectHandler", Some(f_oid), "MoveMember", &[PyRep::Integer(sc.char_id), PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(2)], &HashMap::new(), None).await.expect("promote sc");
    ctx.advance_secs(31);
    ctx.dispatch_rpc(&mut fc.sess, "fleetObjectHandler", Some(f_oid), "MakeLeader", &[PyRep::Integer(p1.char_id)], &HashMap::new(), None).await.expect("MakeLeader p1");
    let p1_oid = bind_oid(&ctx, &mut p1, "fleetObjectHandler", PyRep::Integer(fleet_id)).await;
    let st_p1_boss = ctx.dispatch_rpc(&mut p1.sess, "fleetObjectHandler", Some(p1_oid), "GetInitState", &[], &HashMap::new(), None).await.unwrap();
    assert_eq!(st_p1_boss.dict_get("bossID").and_then(|v| v.as_i64()), Some(p1.char_id));
    for rem in [&mut fc, &mut sc, &mut m1, &mut m2] { let _ = drain(&ctx, rem); }
    slash_cmd(&ctx, &mut fc.sess, "/kick live93400009 disconnect").await;
    for rem in [&mut fc, &mut sc, &mut m1, &mut m2] {
        assert!(drain(&ctx, rem).iter().any(|(e, _)| e == "OnChangeFleetBoss" || e == "OnFleetMemberChanged"), "all remaining members notified");
    }
    let sc_oid = bind_oid(&ctx, &mut sc, "fleetObjectHandler", PyRep::Integer(fleet_id)).await;
    let st_sc_boss = ctx.dispatch_rpc(&mut sc.sess, "fleetObjectHandler", Some(sc_oid), "GetInitState", &[], &HashMap::new(), None).await.unwrap();
    assert_eq!(st_sc_boss.dict_get("bossID").and_then(|v| v.as_i64()), Some(sc.char_id));
    ctx.advance_secs(31);
    rpc(&ctx, &mut sc, "fleetMgr", "LeaveFleet", &[]).await.expect("LeaveFleet");
}
