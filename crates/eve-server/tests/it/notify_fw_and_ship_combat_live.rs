//! Live test suite for authentic notifications:
//! - notify:OnShipDeath
//! - notify:OnKillNotification
//! - notify:OnVictoryPointStateUpdated
//! - notify:OnFacWarBunkerVulnerable
//! - notify:OnSystemFullyCorruptedForFaction
//! - notify:OnSolarSystemLPChange

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::fw_services::{add_victory_points, award_fw_kill_lp};
use eve_server::services::insurgency_services::{add_system_corruption, init_insurgency_campaign, FORECASTING_DURATION_SECS};
use glam::DVec3;

use crate::common;

const PILOT_A: i64 = 90_000_001;
const PILOT_B: i64 = 90_000_002;
const SYS_AMO: u32 = 30_002_055;
const SYS_ZARZAKH: u32 = 30_100_000;

fn new_ctx() -> ServerContext {
    let ctx = ServerContext::new();
    ctx.set_clock_paused(true);
    ctx
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn slash(ctx: &ServerContext, sess: &mut SessionState, cmd: &str) -> PyRep {
    rpc(ctx, sess, "slash", "SlashCmd", &[PyRep::String(cmd.into())])
        .await
        .unwrap_or_else(|e| panic!("slash `{cmd}` failed: {} ({})", e.code, e.message))
}

fn drain_all(ctx: &ServerContext, char_id: i64) -> Vec<(String, Vec<PyRep>)> {
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .map(|n| (n.event, n.args))
        .collect()
}

async fn login_pilot_undocked(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let _ = ctx.db.set_account_role(user, eve_db::service_roles::ROLE_GM_DEFAULT);
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .unwrap();
    sess.role |= eve_db::service_roles::ROLE_GM_DEFAULT;
    rpc(ctx, &mut sess, "stationSvc", "Undock", &[]).await.unwrap();
    sess
}

// parity: notify:OnShipDeath
// parity: notify:OnKillNotification
#[tokio::test]
async fn test_live_notify_ship_death_and_kill_notification() {
    eve_testkit::attest(&["notify:OnShipDeath", "notify:OnKillNotification"]);

    let ctx = new_ctx();
    let mut sess_a = login_pilot_undocked(&ctx, 94_001, "demo", "demo-secret", PILOT_A).await;
    let mut sess_b = login_pilot_undocked(&ctx, 94_002, "other", "other-secret", PILOT_B).await;

    // Teleport both ships to Zarzakh (lawless space)
    slash(&ctx, &mut sess_a, "/tr 30100000").await;
    slash(&ctx, &mut sess_b, "/tr 30100000").await;

    let _ship_a = sess_a.ship_id.expect("active ship a");
    let ship_b = sess_b.ship_id.expect("active ship b");

    let sys_h = ctx.systems.get_or_spawn(SYS_ZARZAKH);
    sys_h.set_ball_position(ship_b, DVec3::new(11_000.0, 0.0, 0.0)).await;

    // Fit gun and give ammo to pilot A
    slash(&ctx, &mut sess_a, "/setskill 3300 5").await; // Gunnery 5
    slash(&ctx, &mut sess_a, "/setskill 3301 5").await; // Small Projectile Turret 5
    let fit_rep = slash(&ctx, &mut sess_a, "/fit me 484").await; // Small Autocannon
    let gun_id = fit_rep.dict_get("itemID").expect("fitted gun itemID").as_i64().unwrap();
    slash(&ctx, &mut sess_a, "/giveitem 218 100").await; // EMP S ammo

    drain_all(&ctx, PILOT_A);
    drain_all(&ctx, PILOT_B);

    rpc(
        &ctx,
        &mut sess_a,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(ship_b)],
    )
    .await
    .unwrap();

    sys_h.step_manual(10.0).await.unwrap();
    drain_all(&ctx, PILOT_A);
    drain_all(&ctx, PILOT_B);

    for _ in 0..40 {
        if sys_h.get_ball_snapshot(ship_b).await.ok().flatten().is_none() {
            break;
        }
        let _ = rpc(
            &ctx,
            &mut sess_a,
            "dogmaIM",
            "Activate",
            &[
                PyRep::Integer(gun_id),
                PyRep::String("projectileFired".into()),
                PyRep::Integer(ship_b),
            ],
        )
        .await;
        ctx.advance_secs(5);
        let _ = sys_h.step_manual(5.0).await;
    }

    assert!(
        sys_h.get_ball_snapshot(ship_b).await.ok().flatten().is_none(),
        "ship_b must be destroyed by weapon fire"
    );

    let kill_a = drain_all(&ctx, PILOT_A);
    let kill_b = drain_all(&ctx, PILOT_B);

    assert!(
        kill_a.iter().any(|(e, _)| e == "OnKillNotification"),
        "attacker must receive OnKillNotification"
    );
    assert!(
        kill_b.iter().any(|(e, _)| e == "OnKillNotification"),
        "victim must receive OnKillNotification"
    );
    assert!(
        kill_b.iter().any(|(e, _)| e == "OnShipDeath"),
        "victim must receive OnShipDeath"
    );
}

// parity: notify:OnVictoryPointStateUpdated
// parity: notify:OnFacWarBunkerVulnerable
#[tokio::test]
async fn test_live_notify_fw_victory_points_and_bunker_vulnerable() {
    eve_testkit::attest(&["notify:OnVictoryPointStateUpdated", "notify:OnFacWarBunkerVulnerable"]);

    let ctx = new_ctx();
    let (fw_sys, occupier_fac, threshold) = ctx.db.with_snapshot(|snap| {
        let occ = snap.fw_system_occupations.get(&SYS_AMO).expect("Amo warzone system");
        (occ.solar_system_id, occ.occupier_faction_id, occ.victory_points_threshold)
    });

    let mut sess = login_pilot_undocked(&ctx, 94_003, "demo", "demo-secret", PILOT_A).await;
    slash(&ctx, &mut sess, &format!("/tr {fw_sys}")).await;
    drain_all(&ctx, PILOT_A);

    // 1. Partial VP increase triggers OnVictoryPointStateUpdated
    let delta = 200;
    add_victory_points(&ctx, fw_sys, delta);
    let notifs_vp = drain_all(&ctx, PILOT_A);
    let vp_notif = notifs_vp.iter().find(|(e, _)| e == "OnVictoryPointStateUpdated");
    assert!(
        vp_notif.is_some(),
        "pilot in warzone system must receive OnVictoryPointStateUpdated"
    );
    let args = &vp_notif.unwrap().1;
    assert_eq!(args.first(), Some(&PyRep::Integer(i64::from(fw_sys))));
    let dict = args.get(1).expect("victory point state dict");
    assert_eq!(dict.dict_get("solarSystemID"), Some(&PyRep::Integer(i64::from(fw_sys))));
    assert_eq!(dict.dict_get("threshold"), Some(&PyRep::Integer(threshold)));

    // 2. VP reaching 100% threshold makes bunker vulnerable and triggers OnFacWarBunkerVulnerable
    add_victory_points(&ctx, fw_sys, threshold);
    let notifs_vun = drain_all(&ctx, PILOT_A);
    let bunker_notif = notifs_vun.iter().find(|(e, _)| e == "OnFacWarBunkerVulnerable");
    assert!(
        bunker_notif.is_some(),
        "pilot in warzone system must receive OnFacWarBunkerVulnerable when system becomes 100% contested"
    );
    assert_eq!(bunker_notif.unwrap().1, vec![PyRep::Integer(occupier_fac)]);
}

// parity: notify:OnSystemFullyCorruptedForFaction
#[tokio::test]
async fn test_live_notify_insurgency_system_fully_corrupted() {
    eve_testkit::attest(&["notify:OnSystemFullyCorruptedForFaction"]);

    let ctx = new_ctx();
    let sys_id = SYS_AMO;
    let mut sess = login_pilot_undocked(&ctx, 94_004, "demo", "demo-secret", PILOT_A).await;
    slash(&ctx, &mut sess, &format!("/tr {sys_id}")).await;
    drain_all(&ctx, PILOT_A);

    let camp_id = 9002_i64;
    let pirate_fac = 500010_i64; // Guristas Pirates
    init_insurgency_campaign(&ctx, camp_id, pirate_fac, 500001, sys_id, vec![sys_id]);
    ctx.advance_secs(FORECASTING_DURATION_SECS + 1);

    add_system_corruption(&ctx, camp_id, sys_id, 1000);
    let notifs = drain_all(&ctx, PILOT_A);
    let corr_notif = notifs.iter().find(|(e, _)| e == "OnSystemFullyCorruptedForFaction");
    assert!(
        corr_notif.is_some(),
        "pilot in system must receive OnSystemFullyCorruptedForFaction"
    );
    assert_eq!(
        corr_notif.unwrap().1,
        vec![
            PyRep::Integer(i64::from(sys_id)),
            PyRep::Integer(pirate_fac),
            PyRep::Integer(1000),
            PyRep::Integer(1000),
        ]
    );
}

// parity: notify:OnSolarSystemLPChange
#[tokio::test]
async fn test_live_notify_fw_solarsystem_lp_change() {
    eve_testkit::attest(&["notify:OnSolarSystemLPChange"]);

    let ctx = new_ctx();
    let sys_id = SYS_AMO;
    let occ_fac = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations
            .get(&sys_id)
            .map(|o| o.occupier_faction_id)
            .expect("Amo occupier faction")
    });

    let mut sess = login_pilot_undocked(&ctx, 94_005, "demo", "demo-secret", PILOT_A).await;
    slash(&ctx, &mut sess, &format!("/tr {sys_id}")).await;

    rpc(
        &ctx,
        &mut sess,
        "facWarMgr",
        "JoinFactionAsCharacter",
        &[PyRep::Integer(occ_fac)],
    )
    .await
    .unwrap();

    let earned_lp = award_fw_kill_lp(&ctx, PILOT_A, sys_id, 100_000_000.0);
    assert!(earned_lp > 0, "earned_lp must be positive: {earned_lp}");
    drain_all(&ctx, PILOT_A);

    let res = rpc(
        &ctx,
        &mut sess,
        "facWarMgr",
        "DonateLPsToSolarSystem",
        &[PyRep::Integer(i64::from(sys_id)), PyRep::Integer(earned_lp)],
    )
    .await
    .unwrap();
    assert_eq!(res.dict_get("totalLP"), Some(&PyRep::Integer(earned_lp)));

    let notifs = drain_all(&ctx, PILOT_A);
    let lp_notif = notifs.iter().find(|(e, _)| e == "OnSolarSystemLPChange");
    assert!(
        lp_notif.is_some(),
        "pilot in system must receive OnSolarSystemLPChange"
    );
    assert_eq!(
        lp_notif.unwrap().1,
        vec![
            PyRep::Integer(0),
            PyRep::Integer(earned_lp),
        ]
    );
}
