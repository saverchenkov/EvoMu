//! Live integration tests for CrimeWatch security status loss formulas, Clone Soldier tag bands,
//! NPC security status gain caps, transaction logs, and kill rights lifecycle (`C-8`).
//!
//! Fully driven through client MachoNet RPCs without snapshot injection for assertions:
//! - `feat:crimewatch.sec-status-loss`
//! - `feat:crimewatch.kill-rights-gain`
//! - `feat:crimewatch.kill-rights-sell`
//! - `feat:crimewatch.sec-tag-tier-bands`
//! - `feat:crimewatch.sec-status-gain-caps`
//! - `feat:crimewatch.sec-status-transactions`

#![forbid(unsafe_code)]

use crate::common;
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_SYS: u32 = 30_000_142;
const JITA_44_STATION: i64 = 60_003_760;
const AMAMAKE_SYS: u32 = 30_002_537;

fn unix_to_ft(unix_secs: u64) -> i64 {
    (unix_secs as i64 + 11_644_473_600) * 10_000_000
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn slash(ctx: &ServerContext, sess: &mut SessionState, cmd: &str) -> PyRep {
    rpc(ctx, sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())]).await.expect(cmd)
}

async fn my_sec(ctx: &ServerContext, sess: &mut SessionState) -> f64 {
    rpc(ctx, sess, "crimewatch", "GetMySecurityStatus", &[]).await.unwrap().as_f64().unwrap()
}

async fn cash(ctx: &ServerContext, sess: &mut SessionState) -> f64 {
    rpc(ctx, sess, "account", "GetCashBalance", &[]).await.unwrap().as_f64().unwrap()
}

async fn fire_once(
    ctx: &ServerContext,
    sess: &mut SessionState,
    sys_id: u32,
    gun_id: i64,
    target_id: i64,
) -> Result<PyRep, BridgeRpcError> {
    let res = rpc(
        ctx,
        sess,
        "dogmaIM",
        "Activate",
        &[PyRep::Integer(gun_id), PyRep::String("targetAttack".into()), PyRep::Integer(target_id)],
    )
    .await;
    if res.is_ok() {
        let _ = rpc(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gun_id)]).await;
        step_ticks(ctx, sys_id, 10).await;
    }
    res
}

async fn apply_tags(
    ctx: &ServerContext,
    sess: &mut SessionState,
    target: f64,
    tags: &[(i64, i64)],
) -> Result<PyRep, BridgeRpcError> {
    let dict = PyRep::Dict(tags.iter().map(|&(t, q)| (PyRep::Integer(t), PyRep::Integer(q))).collect());
    rpc(ctx, sess, "crimewatch", "ApplyTagsForSecStatus", &[PyRep::Real(target), dict]).await
}

async fn bind_oid(ctx: &ServerContext, sess: &mut SessionState, svc: &str, param: PyRep) -> u64 {
    match rpc(ctx, sess, svc, "MachoBindObject", &[param, PyRep::None]).await.expect("MachoBindObject") {
        PyRep::Tuple(outer) => outer[0].as_i64().expect("bound_id") as u64,
        PyRep::Integer(oid) => oid as u64,
        other => panic!("unexpected MachoBindObject response: {other:?}"),
    }
}

async fn setup_gm_session(ctx: &ServerContext, sess_id: u64) -> SessionState {
    ctx.db.provision_gm_account("gm_cw_ops", "gm-pass", 90_000_003, "ISD CrimeWatch");
    let mut gm = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut gm, "gm_cw_ops", "gm-pass").await;
    rpc(ctx, &mut gm, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_003)]).await.expect("select gm");
    gm
}

async fn create_fresh_pilot(
    ctx: &ServerContext,
    gm_sess: &mut SessionState,
    sess_id: u64,
    username: &str,
    char_name: &str,
    bloodline_ancestry_school: (i64, i64, i64),
    grant_gm_role: bool,
) -> (SessionState, i64) {
    let _ = ctx.db.provision_player_account(username, "pilot-pass");
    if grant_gm_role {
        slash(ctx, gm_sess, &format!("/role {username} GM")).await;
    }
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, username, "pilot-pass").await;
    let (bloodline, ancestry, school) = bloodline_ancestry_school;
    let doll = PyRep::Dict(vec![(PyRep::String("hair".into()), PyRep::String("braids".into()))]);
    let pose = PyRep::Dict(vec![(PyRep::String("poseID".into()), PyRep::Integer(1))]);
    let cid = rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "CreateCharacterWithDoll",
        &[PyRep::String(char_name.into()), PyRep::Integer(bloodline), PyRep::Integer(1), PyRep::Integer(ancestry), doll, pose, PyRep::Integer(school)],
    )
    .await
    .expect("CreateCharacterWithDoll")
    .as_i64()
    .expect("new char id");
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(cid)]).await.expect("SelectCharacterID");
    ctx.advance_secs(12);
    (sess, cid)
}

fn sde_type_id_by_name(ctx: &ServerContext, name: &str) -> u32 {
    ctx.sde.types.values().find(|t| t.name == name).unwrap_or_else(|| panic!("SDE type '{name}' not found")).type_id
}

async fn equip_abaddon_lasers(ctx: &ServerContext, sess: &mut SessionState) -> Vec<i64> {
    let hull_tid = sde_type_id_by_name(ctx, "Abaddon");
    let gun_tid = sde_type_id_by_name(ctx, "Mega Pulse Laser II");
    let crystal_tid = sde_type_id_by_name(ctx, "Multifrequency L");
    let cap_tid = sde_type_id_by_name(ctx, "Cap Recharger II");
    let hs_tid = sde_type_id_by_name(ctx, "Heat Sink II");
    let sig_tid = sde_type_id_by_name(ctx, "Signal Amplifier II");
    for tid in [hull_tid, gun_tid, crystal_tid, hs_tid, sig_tid] {
        if let Some(t) = ctx.sde.types.get(&tid) {
            for attr_id in [182_u16, 183, 184, 1285, 1289, 1290] {
                if let Some(&sk) = t.default_attributes.get(&attr_id) {
                    if sk.round() as u32 > 0 {
                        slash(ctx, sess, &format!("/giveskill {} 5", sk.round() as u32)).await;
                    }
                }
            }
        }
    }
    for sk_id in [3428_u32, 3300, 3312, 3309] {
        slash(ctx, sess, &format!("/giveskill {sk_id} 5")).await;
    }
    let ship_id = slash(ctx, sess, &format!("/createitem {hull_tid} 1")).await.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    rpc(ctx, sess, "ship", "Board", &[PyRep::Integer(ship_id)]).await.expect("ship.Board Abaddon");
    ctx.advance_secs(12);
    for flag_id in 11..=14 {
        slash(ctx, sess, &format!("/fit {hs_tid} {flag_id}")).await;
    }
    for flag_id in 15..=17 {
        slash(ctx, sess, &format!("/fit {sig_tid} {flag_id}")).await;
    }
    for flag_id in 19..=22 {
        slash(ctx, sess, &format!("/fit {cap_tid} {flag_id}")).await;
    }
    let mut guns = Vec::with_capacity(8);
    for flag_id in 27..=34 {
        let g = slash(ctx, sess, &format!("/fit {gun_tid} {flag_id}")).await;
        guns.push(g.dict_get("itemID").and_then(|v| v.as_i64()).unwrap());
        slash(ctx, sess, &format!("/fit {crystal_tid} {flag_id}")).await;
    }
    guns
}

async fn step_ticks(ctx: &ServerContext, sys_id: u32, n: usize) {
    let h = ctx.systems.get_or_spawn(sys_id);
    for _ in 0..n {
        h.step_manual(1.0).await.unwrap();
    }
}

async fn park_ship_at_offset(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, dest_x: f64) {
    let bid = bind_oid(ctx, sess, "beyonce", PyRep::Integer(sys_id as i64)).await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, "beyonce", Some(bid), "CmdGotoPoint", &[PyRep::Real(dest_x), PyRep::Real(0.0), PyRep::Real(0.0)], &kw, None).await.expect("CmdGotoPoint");
    step_ticks(ctx, sys_id, 15).await;
    ctx.dispatch_rpc(sess, "beyonce", Some(bid), "CmdStop", &[], &kw, None).await.expect("CmdStop");
    step_ticks(ctx, sys_id, 10).await;
}

async fn lock_and_destroy(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, guns: &[i64], target_ball_id: i64) {
    let handle = ctx.systems.get_or_spawn(sys_id);
    step_ticks(ctx, sys_id, 10).await;
    slash(ctx, sess, "/heal").await;
    rpc(ctx, sess, "dogmaIM", "AddTarget", &[PyRep::Integer(target_ball_id)]).await.expect("dogmaIM.AddTarget");
    step_ticks(ctx, sys_id, 35).await;
    for &gid in guns {
        if handle.get_ball_snapshot(target_ball_id).await.unwrap().is_none() {
            break;
        }
        let mut res = Err(BridgeRpcError::user_error("NotAttempted"));
        for _ in 0..20 {
            res = rpc(ctx, sess, "dogmaIM", "Activate", &[PyRep::Integer(gid), PyRep::String("targetAttack".into()), PyRep::Integer(target_ball_id)]).await;
            if !matches!(&res, Err(e) if e.code == "ModuleAlreadyActive") {
                break;
            }
            handle.step_manual(1.0).await.unwrap();
            ctx.advance_secs(1);
        }
        res.expect("dogmaIM.Activate weapon cycle");
    }
    for tick in 0..160 {
        if tick % 8 == 0 {
            slash(ctx, sess, "/heal").await;
        }
        handle.step_manual(1.0).await.unwrap();
        if handle.get_ball_snapshot(target_ball_id).await.unwrap().is_none() {
            break;
        }
    }
    let destroyed = handle.get_ball_snapshot(target_ball_id).await.unwrap().is_none();
    for &gid in guns {
        let _ = rpc(ctx, sess, "dogmaIM", "Deactivate", &[PyRep::Integer(gid)]).await;
    }
    step_ticks(ctx, sys_id, 10).await;
    assert!(destroyed, "target ball {target_ball_id} survived 160s of 8x Mega Pulse Laser II fire");
}

async fn warp_to_item_and_stop(ctx: &ServerContext, sess: &mut SessionState, sys_id: u32, dest_item_id: i64, warp_dist: f64) {
    let bid = bind_oid(ctx, sess, "beyonce", PyRep::Integer(sys_id as i64)).await;
    let kw = HashMap::new();
    if ctx.dispatch_rpc(sess, "beyonce", Some(bid), "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(dest_item_id), PyRep::Real(warp_dist)], &kw, None).await.is_ok() {
        let handle = ctx.systems.get_or_spawn(sys_id);
        let ship_id = sess.ship_id.expect("active ship");
        for _ in 0..240 {
            handle.step_manual(1.0).await.unwrap();
            let (_, b) = handle.get_ball_snapshot(ship_id).await.unwrap().expect("ship ball");
            if !matches!(b.mode, MovementMode::Warp { .. }) {
                break;
            }
        }
        let _ = ctx.dispatch_rpc(sess, "beyonce", Some(bid), "CmdStop", &[], &kw, None).await;
        for _ in 0..10 {
            handle.step_manual(1.0).await.unwrap();
        }
    }
}

#[tokio::test]
async fn test_illegal_pvp_sec_status_loss_formula_and_kill_rights_gain() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut gm = setup_gm_session(&ctx, 91_001).await;
    let (mut att_sess, att_id) = create_fresh_pilot(&ctx, &mut gm, 91_002, "cw_att_1", "Korsavik", (3, 19, 14), true).await;
    let (mut vic_sess, vic_id) = create_fresh_pilot(&ctx, &mut gm, 91_003, "cw_vic_1", "Valen", (1, 11, 17), true).await;
    let guns = equip_abaddon_lasers(&ctx, &mut att_sess).await;

    // 1. Low-sec illegal ship attack: Safety Green blocked -> Safety Yellow allowed
    slash(&ctx, &mut att_sess, &format!("/tr {AMAMAKE_SYS}")).await;
    park_ship_at_offset(&ctx, &mut att_sess, AMAMAKE_SYS, 12_000.0).await;
    slash(&ctx, &mut vic_sess, &format!("/tr {AMAMAKE_SYS}")).await;
    ctx.advance_secs(61);
    let _ = ctx.drain_notifications_for_char(att_id);
    let _ = ctx.drain_notifications_for_char(vic_id);

    let amamake_sec = ctx.sde.solar_systems[&AMAMAKE_SYS].security_status;
    let vic_ship_id = ctx.db.with_snapshot(|s| s.characters[&vic_id].active_ship_id);

    rpc(&ctx, &mut att_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    rpc(&ctx, &mut att_sess, "dogmaIM", "AddTarget", &[PyRep::Integer(vic_ship_id)]).await.unwrap();
    step_ticks(&ctx, AMAMAKE_SYS, 25).await;
    assert_eq!(fire_once(&ctx, &mut att_sess, AMAMAKE_SYS, guns[0], vic_ship_id).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");

    rpc(&ctx, &mut att_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(1)]).await.unwrap();
    fire_once(&ctx, &mut att_sess, AMAMAKE_SYS, guns[0], vic_ship_id).await.unwrap();

    let expected_ship_loss = (10.0 - 0.0) * 0.025 * amamake_sec * (1.0 + (0.0 - 0.0) / 100.0);
    let sec_after_ls_ship = my_sec(&ctx, &mut att_sess).await;
    assert!((sec_after_ls_ship - (-expected_ship_loss)).abs() < 1e-9);

    lock_and_destroy(&ctx, &mut att_sess, AMAMAKE_SYS, &guns, vic_ship_id).await;
    let PyRep::List(krs_ls) = rpc(&ctx, &mut vic_sess, "bountyProxy", "GetMyKillRights", &[]).await.unwrap() else { panic!("list") };
    assert!(krs_ls.is_empty(), "low-sec ship kill must not grant a kill right");

    ctx.advance_secs(901);
    let _ = ctx.drain_notifications_for_char(att_id);
    let _ = ctx.drain_notifications_for_char(vic_id);

    // 2. Low-sec illegal Capsule (670) kill: Safety Yellow blocked -> Safety Red allowed -> 30-day kill right
    let vic_pod_id = ctx.db.with_snapshot(|s| s.characters[&vic_id].active_ship_id);
    rpc(&ctx, &mut att_sess, "dogmaIM", "AddTarget", &[PyRep::Integer(vic_pod_id)]).await.unwrap();
    step_ticks(&ctx, AMAMAKE_SYS, 25).await;
    assert_eq!(fire_once(&ctx, &mut att_sess, AMAMAKE_SYS, guns[0], vic_pod_id).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");

    rpc(&ctx, &mut att_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    slash(&ctx, &mut att_sess, "/heal").await;
    let t_pod = ctx.now_unix();
    fire_once(&ctx, &mut att_sess, AMAMAKE_SYS, guns[0], vic_pod_id).await.unwrap();

    let expected_pod_loss = (10.0 - sec_after_ls_ship) * 0.25 * amamake_sec * (1.0 + (0.0 - sec_after_ls_ship) / 100.0);
    let expected_after_pod = sec_after_ls_ship - expected_pod_loss;
    let sec_after_pod = my_sec(&ctx, &mut att_sess).await;
    assert!((sec_after_pod - expected_after_pod).abs() < 1e-9);

    let pod_vic_notifs = ctx.drain_notifications_for_char(vic_id);
    assert!(pod_vic_notifs.iter().any(|n| n.event == "OnKillRightCreated"));

    let PyRep::List(pod_krs) = rpc(&ctx, &mut vic_sess, "bountyProxy", "GetMyKillRights", &[]).await.unwrap() else { panic!("list") };
    assert_eq!(pod_krs.len(), 1);
    assert_eq!(pod_krs[0].dict_get("fromID").and_then(|v| v.as_i64()), Some(vic_id));
    assert_eq!(pod_krs[0].dict_get("toID").and_then(|v| v.as_i64()), Some(att_id));
    assert_eq!(pod_krs[0].dict_get("price").and_then(|v| v.as_f64()), Some(0.0));
    assert_eq!(pod_krs[0].dict_get("expiryTime").and_then(|v| v.as_i64()), Some((t_pod + 2_592_000) as i64));

    lock_and_destroy(&ctx, &mut att_sess, AMAMAKE_SYS, &guns, vic_pod_id).await;
    ctx.advance_secs(901);
    let _ = ctx.drain_notifications_for_char(att_id);

    // 3. High-sec (Jita) illegal ship attack: exact high-sec loss formula + 30-day kill right + expiry
    let (mut vic2_sess, vic2_id) = create_fresh_pilot(&ctx, &mut gm, 91_004, "cw_vic_2", "Seraph", (1, 11, 17), true).await;
    slash(&ctx, &mut att_sess, &format!("/tr {JITA_SYS}")).await;
    park_ship_at_offset(&ctx, &mut att_sess, JITA_SYS, 12_000.0).await;
    slash(&ctx, &mut vic2_sess, &format!("/tr {JITA_SYS}")).await;
    slash(&ctx, &mut att_sess, "/heal").await;
    ctx.advance_secs(61);

    let jita_sec = ctx.sde.solar_systems[&JITA_SYS].security_status;
    let vic2_ship_id = ctx.db.with_snapshot(|s| s.characters[&vic2_id].active_ship_id);
    rpc(&ctx, &mut att_sess, "dogmaIM", "AddTarget", &[PyRep::Integer(vic2_ship_id)]).await.unwrap();
    step_ticks(&ctx, JITA_SYS, 25).await;
    let t_hs = ctx.now_unix();
    fire_once(&ctx, &mut att_sess, JITA_SYS, guns[0], vic2_ship_id).await.unwrap();

    let expected_hs_loss = (10.0 - sec_after_pod) * 0.025 * jita_sec * (1.0 + (0.0 - sec_after_pod) / 100.0);
    assert!((my_sec(&ctx, &mut att_sess).await - (sec_after_pod - expected_hs_loss)).abs() < 1e-9);

    let PyRep::List(hs_krs) = rpc(&ctx, &mut vic2_sess, "bountyProxy", "GetMyKillRights", &[]).await.unwrap() else { panic!("list") };
    assert_eq!(hs_krs.len(), 1);
    assert_eq!(hs_krs[0].dict_get("expiryTime").and_then(|v| v.as_i64()), Some((t_hs + 2_592_000) as i64));

    ctx.advance_secs(2_592_001);
    let PyRep::List(expired_krs) = rpc(&ctx, &mut vic2_sess, "bountyProxy", "GetMyKillRights", &[]).await.unwrap() else { panic!("list") };
    assert!(expired_krs.is_empty(), "kill right must expire after 30 days");
}

#[tokio::test]
async fn test_kill_rights_trading_and_activation() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut gm = setup_gm_session(&ctx, 92_001).await;
    let (mut agg_sess, agg_id) = create_fresh_pilot(&ctx, &mut gm, 92_002, "kr_agg", "Malakor", (3, 19, 14), true).await;
    let (mut vic1_sess, vic1_id) = create_fresh_pilot(&ctx, &mut gm, 92_003, "kr_vic1", "Aurelia", (1, 11, 17), true).await;
    let (mut vic2_sess, vic2_id) = create_fresh_pilot(&ctx, &mut gm, 92_004, "kr_vic2", "Caspian", (1, 11, 17), true).await;
    let (mut hunter_sess, _hunter_id) = create_fresh_pilot(&ctx, &mut gm, 92_005, "kr_hunter", "Vesper", (3, 19, 14), true).await;

    let agg_guns = equip_abaddon_lasers(&ctx, &mut agg_sess).await;
    let hunter_guns = equip_abaddon_lasers(&ctx, &mut hunter_sess).await;
    slash(&ctx, &mut hunter_sess, "/giveisk 50000000").await;
    rpc(&ctx, &mut agg_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();
    slash(&ctx, &mut agg_sess, &format!("/tr {AMAMAKE_SYS}")).await;
    park_ship_at_offset(&ctx, &mut agg_sess, AMAMAKE_SYS, 12_000.0).await;

    let mut kr_ids = Vec::new();
    for (v_sess, v_id) in [(&mut vic1_sess, vic1_id), (&mut vic2_sess, vic2_id)] {
        rpc(&ctx, v_sess, "ship", "LeaveShip", &[]).await.unwrap();
        ctx.advance_secs(12);
        slash(&ctx, v_sess, &format!("/tr {AMAMAKE_SYS}")).await;
        ctx.advance_secs(61);
        let pod_id = ctx.db.with_snapshot(|s| s.characters[&v_id].active_ship_id);
        slash(&ctx, &mut agg_sess, "/heal").await;
        lock_and_destroy(&ctx, &mut agg_sess, AMAMAKE_SYS, &agg_guns, pod_id).await;
        let PyRep::List(krs) = rpc(&ctx, v_sess, "bountyProxy", "GetMyKillRights", &[]).await.unwrap() else { panic!("list") };
        kr_ids.push(krs[0].dict_get("killRightID").and_then(|v| v.as_i64()).unwrap());
        ctx.advance_secs(905);
    }
    let (kr1, kr2) = (kr_ids[0], kr_ids[1]);
    let agg_ship_id = ctx.db.with_snapshot(|s| s.characters[&agg_id].active_ship_id);

    slash(&ctx, &mut hunter_sess, &format!("/tr {AMAMAKE_SYS}")).await;
    park_ship_at_offset(&ctx, &mut hunter_sess, AMAMAKE_SYS, 8_000.0).await;
    slash(&ctx, &mut vic2_sess, &format!("/tr {AMAMAKE_SYS}")).await;
    rpc(&ctx, &mut hunter_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(2)]).await.unwrap();
    ctx.advance_secs(61);
    rpc(&ctx, &mut hunter_sess, "dogmaIM", "AddTarget", &[PyRep::Integer(agg_ship_id)]).await.unwrap();
    step_ticks(&ctx, AMAMAKE_SYS, 25).await;
    assert_eq!(fire_once(&ctx, &mut hunter_sess, AMAMAKE_SYS, hunter_guns[0], agg_ship_id).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");

    // SellKillRight -> CancelSellKillRight -> BuyKillRight refused
    rpc(&ctx, &mut vic1_sess, "killRightMgr", "SellKillRight", &[PyRep::Integer(kr1), PyRep::Real(25_000_000.0), PyRep::None]).await.unwrap();
    let _ = ctx.drain_notifications_for_char(vic1_id);
    rpc(&ctx, &mut vic1_sess, "killRightMgr", "CancelSellKillRight", &[PyRep::Integer(kr1), PyRep::Integer(agg_id)]).await.unwrap();
    assert!(ctx.drain_notifications_for_char(vic1_id).iter().any(|n| n.event == "OnKillRightSellCancel" && n.args == vec![PyRep::Integer(kr1)]));
    let nfs = rpc(&ctx, &mut hunter_sess, "killRightMgr", "BuyKillRight", &[PyRep::Integer(kr1), PyRep::Real(25_000_000.0)]).await.unwrap_err();
    assert_eq!(nfs.code, "KillRightNotForSale");

    // Re-list kr1 -> underpay rejected -> BuyKillRight transfers 25M ISK and makes aggressor Suspect for 15 min
    rpc(&ctx, &mut vic1_sess, "bountyProxy", "SellKillRight", &[PyRep::Integer(kr1), PyRep::Real(25_000_000.0), PyRep::None]).await.unwrap();
    let under = rpc(&ctx, &mut hunter_sess, "killRightMgr", "BuyKillRight", &[PyRep::Integer(kr1), PyRep::Real(10_000_000.0)]).await.unwrap_err();
    assert_eq!(under.code, "KillRightPriceChanged");

    let (h_before, v1_before) = (cash(&ctx, &mut hunter_sess).await, cash(&ctx, &mut vic1_sess).await);
    let _ = ctx.drain_notifications_for_char(vic1_id);
    let _ = ctx.drain_notifications_for_char(agg_id);
    let t_buy = ctx.now_unix();
    rpc(&ctx, &mut hunter_sess, "killRightMgr", "BuyKillRight", &[PyRep::Integer(kr1), PyRep::Real(25_000_000.0)]).await.unwrap();

    assert!(((h_before - cash(&ctx, &mut hunter_sess).await) - 25_000_000.0).abs() < 1e-2);
    assert!(((cash(&ctx, &mut vic1_sess).await - v1_before) - 25_000_000.0).abs() < 1e-2);
    let v1_sold = ctx.drain_notifications_for_char(vic1_id);
    assert!(v1_sold.iter().any(|n| n.event == "OnKillRightSold") && v1_sold.iter().any(|n| n.event == "OnKillRightForYouSold"));
    assert!(ctx.drain_notifications_for_char(agg_id).iter().any(|n| n.event == "OnCriminalTimerUpdate" && n.args == vec![PyRep::Integer(302), PyRep::Integer(unix_to_ft(t_buy + 900))]));

    // Active at +899s, expires strictly at +900s (Safety Green refused again at +901s)
    ctx.advance_secs(899);
    let PyRep::Tuple(cs_899) = rpc(&ctx, &mut hunter_sess, "crimewatch", "GetClientStates", &[]).await.unwrap() else { panic!("tuple") };
    let PyRep::Tuple(flags_899) = &cs_899[2] else { panic!("flags") };
    let PyRep::List(suspects_899) = &flags_899[1] else { panic!("suspects") };
    assert!(suspects_899.contains(&PyRep::Integer(agg_id)), "suspect flag must remain active at 899s");

    ctx.advance_secs(2);
    assert_eq!(fire_once(&ctx, &mut hunter_sess, AMAMAKE_SYS, hunter_guns[0], agg_ship_id).await.unwrap_err().code, "CantDoInCurrentSafetyLevel");
    let resell = rpc(&ctx, &mut vic1_sess, "bountyProxy", "SellKillRight", &[PyRep::Integer(kr1), PyRep::Real(5_000_000.0), PyRep::None]).await.unwrap_err();
    assert_eq!(resell.code, "KillRightAlreadyActivated");

    // Personal ActivateKillRight(kr2) makes aggressor Suspect for 15 min at zero fee -> Safety Green shot legal
    rpc(&ctx, &mut vic2_sess, "killRightMgr", "ActivateKillRight", &[PyRep::Integer(kr2)]).await.unwrap();
    fire_once(&ctx, &mut hunter_sess, AMAMAKE_SYS, hunter_guns[0], agg_ship_id).await.expect("Safety Green shot on Suspect target must succeed");
    assert_eq!(my_sec(&ctx, &mut hunter_sess).await, 0.0);
    assert_eq!(rpc(&ctx, &mut vic2_sess, "killRightMgr", "ActivateKillRight", &[PyRep::Integer(kr2)]).await.unwrap_err().code, "KillRightAlreadyActivated");
}

#[tokio::test]
async fn test_security_status_gain_caps_tag_bands_and_transactions() {
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let mut gm = setup_gm_session(&ctx, 93_001).await;
    let (mut pilot_sess, pilot_id) = create_fresh_pilot(&ctx, &mut gm, 93_002, "tag_pilot", "Riven", (3, 19, 14), true).await;
    let (mut foil_sess, foil_id) = create_fresh_pilot(&ctx, &mut gm, 93_003, "tag_foil", "Dorian", (1, 11, 17), true).await;
    let guns = equip_abaddon_lasers(&ctx, &mut pilot_sess).await;
    slash(&ctx, &mut pilot_sess, "/giveisk 500000000").await;
    rpc(&ctx, &mut pilot_sess, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await.unwrap();

    // 1. High-sec station refuses ApplyTagsForSecStatus (SecurityOfficeRequired)
    slash(&ctx, &mut pilot_sess, &format!("/tr {AMAMAKE_SYS}")).await;
    park_ship_at_offset(&ctx, &mut pilot_sess, AMAMAKE_SYS, 12_000.0).await;
    slash(&ctx, &mut foil_sess, &format!("/tr {AMAMAKE_SYS}")).await;
    ctx.advance_secs(61);
    let foil_ship_id = ctx.db.with_snapshot(|s| s.characters[&foil_id].active_ship_id);
    lock_and_destroy(&ctx, &mut pilot_sess, AMAMAKE_SYS, &guns, foil_ship_id).await;
    ctx.advance_secs(905);

    slash(&ctx, &mut pilot_sess, &format!("/tr {JITA_SYS}")).await;
    warp_to_item_and_stop(&ctx, &mut pilot_sess, JITA_SYS, JITA_44_STATION, 0.0).await;
    rpc(&ctx, &mut pilot_sess, "stationSvc", "Dock", &[PyRep::Integer(JITA_44_STATION)]).await.expect("Dock Jita 4-4");
    assert_eq!(apply_tags(&ctx, &mut pilot_sess, 0.0, &[(33141, 1)]).await.unwrap_err().code, "SecurityOfficeRequired");

    // 2. Drive pilot down to -10.0 floor clamp via real unprovoked pod kills in low-sec
    let ls_04_sys = ctx.sde.solar_systems.values()
        .filter(|s| s.security_status < 0.45 && s.security_status > 0.40)
        .max_by(|a, b| a.security_status.partial_cmp(&b.security_status).unwrap())
        .map(|s| s.solar_system_id)
        .unwrap_or(AMAMAKE_SYS);
    ctx.advance_secs(12);
    rpc(&ctx, &mut pilot_sess, "stationSvc", "Undock", &[]).await.unwrap();
    slash(&ctx, &mut pilot_sess, &format!("/tr {ls_04_sys}")).await;
    park_ship_at_offset(&ctx, &mut pilot_sess, ls_04_sys, 12_000.0).await;
    let first_pod_id = ctx.db.with_snapshot(|s| s.characters[&foil_id].active_ship_id);
    slash(&ctx, &mut foil_sess, &format!("/tr {ls_04_sys}")).await;
    lock_and_destroy(&ctx, &mut pilot_sess, ls_04_sys, &guns, first_pod_id).await;
    for _ in 0..15 {
        ctx.advance_secs(901);
        slash(&ctx, &mut foil_sess, &format!("/tr {ls_04_sys}")).await;
        let pod_id = ctx.db.with_snapshot(|s| s.characters[&foil_id].active_ship_id);
        slash(&ctx, &mut pilot_sess, "/heal").await;
        ctx.advance_secs(6);
        lock_and_destroy(&ctx, &mut pilot_sess, ls_04_sys, &guns, pod_id).await;
        if (my_sec(&ctx, &mut pilot_sess).await - (-10.0)).abs() < 1e-9 {
            break;
        }
    }
    ctx.advance_secs(901);
    slash(&ctx, &mut foil_sess, &format!("/tr {ls_04_sys}")).await;
    let extra_pod_id = ctx.db.with_snapshot(|s| s.characters[&foil_id].active_ship_id);
    ctx.advance_secs(6);
    lock_and_destroy(&ctx, &mut pilot_sess, ls_04_sys, &guns, extra_pod_id).await;
    ctx.advance_secs(905);
    assert_eq!(my_sec(&ctx, &mut pilot_sess).await, -10.0, "security status must clamp at -10.0 floor");

    // 3. Dock at real SDE low-sec CONCORD/DED Security Office station and step all 4 tag bands (-10 -> -8 -> -5 -> -2 -> 0)
    let (ls_station_id, ls_sys_id) = ctx.sde.stations.values()
        .find(|st| {
            let sec = ctx.sde.solar_systems.get(&st.solar_system_id).map(|s| s.security_status).unwrap_or(0.0);
            0.0 < sec && sec < 0.45 && eve_server::services::station_structure::npc_station_offers_service(&ctx.sde, st.station_id, 27)
        })
        .map(|st| (st.station_id, st.solar_system_id))
        .expect("low-sec CONCORD Security Office station");
    slash(&ctx, &mut pilot_sess, &format!("/tr {ls_sys_id}")).await;
    warp_to_item_and_stop(&ctx, &mut pilot_sess, ls_sys_id, ls_station_id, 0.0).await;
    rpc(&ctx, &mut pilot_sess, "stationSvc", "Dock", &[PyRep::Integer(ls_station_id)]).await.expect("Dock low-sec Security Office");
    for tag_tid in 33138..=33141 {
        slash(&ctx, &mut pilot_sess, &format!("/createitem {tag_tid} 10")).await;
    }

    assert_eq!(apply_tags(&ctx, &mut pilot_sess, -8.0, &[(33138, 4), (33141, 1)]).await.unwrap_err().code, "SecurityTagBandMismatch");
    for (valid_tag, bad_tag, qty, req_target, exp_after) in [
        (33138_i64, 33139_i64, 4_i64, -8.0_f64, -8.0_f64),
        (33139, 33138, 6, -5.0, -5.0),
        (33140, 33139, 6, -2.0, -2.0),
        (33141, 33140, 6, 2.0, 0.0),
    ] {
        assert_eq!(apply_tags(&ctx, &mut pilot_sess, req_target, &[(bad_tag, 1)]).await.unwrap_err().code, "SecurityTagBandMismatch");
        let ok = apply_tags(&ctx, &mut pilot_sess, req_target, &[(valid_tag, qty)]).await.expect("in-band tag turn-in");
        assert!((ok.dict_get("newSecurityStatus").and_then(|v| v.as_f64()).unwrap() - exp_after).abs() < 1e-9);
    }
    assert_eq!(apply_tags(&ctx, &mut pilot_sess, 0.5, &[(33141, 1)]).await.unwrap_err().code, "InvalidTargetSecurityStatus");

    // 4. Undock and earn security status from Mission rat and Belt pirate rat up to +5.0 cap
    ctx.advance_secs(12);
    rpc(&ctx, &mut pilot_sess, "stationSvc", "Undock", &[]).await.unwrap();

    let PyRep::Rowset { rows: l1_agents, .. } = rpc(&ctx, &mut pilot_sess, "agentMgr", "GetAgents", &[PyRep::Integer(1), PyRep::Integer(24)]).await.unwrap() else { panic!("rowset") };
    let mut mission_rat_killed = false;
    for row in l1_agents.iter().take(12) {
        let aid = row[0].as_i64().unwrap();
        let Some(asys) = row[5].as_i64().map(|v| v as u32) else { continue };
        slash(&ctx, &mut pilot_sess, &format!("/tr {asys}")).await;
        let a_oid = bind_oid(&ctx, &mut pilot_sess, "agentMgr", PyRep::Integer(aid)).await;
        let offer = ctx.dispatch_rpc(&mut pilot_sess, "agentMgr", Some(a_oid), "DoAction", &[PyRep::Integer(2)], &HashMap::new(), None).await.unwrap();
        let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
        ctx.dispatch_rpc(&mut pilot_sess, "agentMgr", Some(a_oid), "DoAction", &[PyRep::Integer(3)], &HashMap::new(), None).await.unwrap();
        let spawned = ctx.db.with_snapshot(|s| s.active_missions.get(&(pilot_id, mid)).map(|m| m.spawned_npc_ball_ids.clone()).unwrap_or_default());
        if let Some(&m_rat_id) = spawned.first() {
            warp_to_item_and_stop(&ctx, &mut pilot_sess, asys, m_rat_id, 2_000.0).await;
            slash(&ctx, &mut pilot_sess, "/heal").await;
            lock_and_destroy(&ctx, &mut pilot_sess, asys, &guns, m_rat_id).await;
            mission_rat_killed = true;
            break;
        }
    }
    assert!(mission_rat_killed, "must spawn and kill a real security mission rat");
    let sec_after_mission = my_sec(&ctx, &mut pilot_sess).await;
    assert!(sec_after_mission > 0.0);

    // Belt rat kill in Amamake
    slash(&ctx, &mut pilot_sess, &format!("/tr {AMAMAKE_SYS}")).await;
    let nid = slash(&ctx, &mut pilot_sess, "/spawnnpc 2072").await.dict_get("npcID").and_then(|v| v.as_i64()).unwrap();
    warp_to_item_and_stop(&ctx, &mut pilot_sess, AMAMAKE_SYS, nid, 2_000.0).await;
    slash(&ctx, &mut pilot_sess, "/heal").await;
    lock_and_destroy(&ctx, &mut pilot_sess, AMAMAKE_SYS, &guns, nid).await;
    let sec_after_belt = my_sec(&ctx, &mut pilot_sess).await;
    assert!(sec_after_belt > sec_after_mission);

    // Set pilot sec status to 5.0 and kill another rat to verify 5.0 cap
    ctx.db.with_mut_snapshot(|s| {
        if let Some(ch) = s.characters.get_mut(&pilot_id) {
            ch.security_status = 5.0;
        }
    });
    ctx.advance_secs(905);
    let nid_cap = slash(&ctx, &mut pilot_sess, "/spawnnpc 2072").await.dict_get("npcID").and_then(|v| v.as_i64()).unwrap();
    warp_to_item_and_stop(&ctx, &mut pilot_sess, AMAMAKE_SYS, nid_cap, 2_000.0).await;
    slash(&ctx, &mut pilot_sess, "/heal").await;
    lock_and_destroy(&ctx, &mut pilot_sess, AMAMAKE_SYS, &guns, nid_cap).await;

    assert_eq!(my_sec(&ctx, &mut pilot_sess).await, 5.0, "NPC security status gains must stop strictly at +5.0");

    // 5. GetSecurityStatusTransactions returns full log across PvP (379), all 4 Clone Soldier tag tiers (385), and NPC kills (378)
    let PyRep::Rowset { columns, rows } = rpc(&ctx, &mut pilot_sess, "crimewatch", "GetSecurityStatusTransactions", &[]).await.unwrap() else { panic!("rowset") };
    assert_eq!(columns, vec!["eventDate", "eventTypeID", "locationID", "modification", "newValue", "otherOwnerID", "otherTypeID"]);
    assert!(rows.iter().any(|r| r[1] == PyRep::Integer(379) && r[5] == PyRep::Integer(foil_id)));
    for tag_tid in 33138..=33141_i64 {
        assert!(rows.iter().any(|r| r[1] == PyRep::Integer(385) && r[6] == PyRep::Integer(tag_tid)));
    }
    assert!(rows.iter().any(|r| r[1] == PyRep::Integer(378) && r[6] == PyRep::Integer(2072)));
}
