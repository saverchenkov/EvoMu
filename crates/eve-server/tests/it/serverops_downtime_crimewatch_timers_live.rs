//! Live Serverops Downtime, Daily Jobs, and Crimewatch Timers test suite.
//!
//! Covers:
//! - `feat:serverops.downtime`
//! - `feat:serverops.daily-jobs`
//! - `notify:OnWeaponsTimerUpdate`
//! - `notify:OnPvpTimerUpdate`
//! - `notify:OnCriminalTimerUpdate`
//! - `notify:OnNpcTimerUpdate`
//! - `notify:OnDisapprovalTimerUpdate`

#![forbid(unsafe_code)]

use crate::esi_fw_sov_incursions_live::{rpc, select_gm_pilot, slash};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::BTreeSet;
use std::sync::Arc;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const PERIMETER_SYSTEM: u32 = 30_000_144;
const JITA_SYSTEM: u32 = 30_000_142;
const ZARZAKH_SYSTEM: u32 = 30_100_000;

async fn select_player_pilot(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    crate::common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(
        ctx,
        &mut sess,
        "charUnboundMgr",
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
    )
    .await
    .expect("SelectCharacterID");
    ctx.advance_secs(5);
    sess
}

fn find_timer_notification(
    notifs: &[eve_server::router::PendingMachoNotification],
    event_name: &str,
) -> Option<(i64, Option<i64>)> {
    notifs
        .iter()
        .rfind(|n| n.event == event_name)
        .and_then(|n| {
            let code = n.args.first().and_then(|v| v.as_i64())?;
            let expiry = n.args.get(1).and_then(|v| v.as_i64());
            Some((code, expiry))
        })
}

#[tokio::test]
async fn test_live_serverops_daily_downtime_and_jobs() {
    // parity: feat:serverops.downtime, feat:serverops.daily-jobs
    eve_testkit::attest(&[
        "feat:serverops.downtime",
        "feat:serverops.daily-jobs",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    let perim_handle = ctx.systems.get_or_spawn(PERIMETER_SYSTEM);
    let belt_asts = perim_handle
        .populate_asteroid_belt_entities()
        .await
        .expect("populate asteroid belts in Perimeter");
    assert!(!belt_asts.is_empty(), "Perimeter has asteroid belts");

    let belt_ids: BTreeSet<i64> = belt_asts.iter().map(|a| a.belt_id).collect();
    let target_belt_id = *belt_ids.iter().next().expect("first belt");
    let belt_ast_ids: Vec<i64> = belt_asts
        .iter()
        .filter(|a| a.belt_id == target_belt_id)
        .map(|a| a.asteroid_id)
        .collect();
    assert!(!belt_ast_ids.is_empty(), "Target belt has asteroids");

    // Mine out / deplete all asteroids in the target belt
    for &aid in &belt_ast_ids {
        let _ = perim_handle.mine_asteroid(aid, 100_000_000, 0.0).await;
        let ast_state = perim_handle
            .get_asteroid_state(aid)
            .await
            .expect("reply")
            .expect("get_asteroid");
        assert_eq!(ast_state.remaining_units, 0);
        assert!(ast_state.depleted);
    }

    // Connect Pilot 1 as a normal player and Pilot 2 as a GM
    let _sess_player = select_player_pilot(&ctx, 60_001, "demo", "demo-secret", PILOT_1).await;
    let mut sess_gm = select_gm_pilot(&ctx, 60_002, "other", "other-secret", PILOT_2).await;

    assert!(ctx.db.online_session_count() >= 2);

    // Pilot 2 (GM) triggers live cluster downtime & daily jobs via GM /downtime slash command
    let rep = slash(&ctx, &mut sess_gm, "/downtime").await;

    assert!(
        rep.dict_get("clusterRestartEpoch")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            >= 1,
        "Cluster restart epoch must advance"
    );
    assert!(
        rep.dict_get("disconnectedSessions")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            >= 1,
        "Non-GM player session must be disconnected"
    );
    assert!(
        rep.dict_get("replenishedAsteroids")
            .and_then(|v| v.as_i64())
            .unwrap_or(0)
            >= belt_ast_ids.len() as i64,
        "Depleted asteroid belt must be respawned"
    );

    // Verify OnClusterShutdownInitiated was delivered to the player
    let p1_notifs = ctx.drain_notifications_for_char(PILOT_1);
    let shutdown_ev = p1_notifs
        .iter()
        .find(|n| n.event == "OnClusterShutdownInitiated")
        .expect("OnClusterShutdownInitiated notification");
    assert_eq!(shutdown_ev.args.len(), 3);
    assert_eq!(
        shutdown_ev.args[0],
        PyRep::String("UI/Shared/ShutdownNotificationTitle".into())
    );
    assert_eq!(shutdown_ev.args[2], PyRep::Integer(15));

    // Verify all belt asteroids in the active solar system are restored
    for &aid in &belt_ast_ids {
        let ast_state = perim_handle
            .get_asteroid_state(aid)
            .await
            .expect("reply")
            .expect("get_asteroid after downtime");
        assert!(
            ast_state.remaining_units > 0,
            "Asteroid units respawned at downtime"
        );
        assert!(
            !ast_state.depleted,
            "Asteroid not depleted after downtime"
        );
    }

    // Verify cluster is running and ready for player connections
    assert!(!ctx.is_shutting_down());
    let _sess_reconnected = select_player_pilot(&ctx, 60_003, "demo", "demo-secret", PILOT_1).await;
    assert!(ctx.db.online_session_count() >= 2);
}

#[tokio::test]
async fn test_live_crimewatch_weapons_pvp_and_criminal_timers() {
    // parity: notify:OnWeaponsTimerUpdate, notify:OnPvpTimerUpdate, notify:OnCriminalTimerUpdate
    eve_testkit::attest(&[
        "notify:OnWeaponsTimerUpdate",
        "notify:OnPvpTimerUpdate",
        "notify:OnCriminalTimerUpdate",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    let mut sess_atk = select_gm_pilot(&ctx, 61_001, "demo", "demo-secret", PILOT_1).await;
    let mut sess_vic = select_gm_pilot(&ctx, 61_002, "other", "other-secret", PILOT_2).await;

    // Both pilots undock into space in Jita
    rpc(&ctx, &mut sess_atk, "stationSvc", "Undock", &[]).await.expect("Undock atk");
    rpc(&ctx, &mut sess_vic, "stationSvc", "Undock", &[]).await.expect("Undock vic");

    let ship_atk = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    let ship_vic = ctx.db.with_snapshot(|s| s.characters[&PILOT_2].active_ship_id);

    let jita_handle = ctx.systems.get_or_spawn(JITA_SYSTEM);
    let pos_atk = DVec3::new(10_000.0, 0.0, 0.0);
    let pos_vic = DVec3::new(12_000.0, 0.0, 0.0);
    let _ = jita_handle.set_ball_position(ship_atk, pos_atk).await;
    let _ = jita_handle.set_ball_position(ship_vic, pos_vic).await;

    // Train required skills and fit gun with ammunition
    slash(&ctx, &mut sess_atk, "/setskill 3300 5").await; // Gunnery
    slash(&ctx, &mut sess_atk, "/setskill 3301 5").await; // Small Hybrid Turret
    let gun_rep = slash(&ctx, &mut sess_atk, "/fit me 484").await; // 125mm Railgun I
    let gun_id = gun_rep
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("gun itemID");
    slash(&ctx, &mut sess_atk, "/giveitem 178 500").await; // Iron Charge S

    // Set Safety Level to Red to authorize illegal high-sec aggression
    rpc(
        &ctx,
        &mut sess_atk,
        "crimewatch",
        "SetSafetyLevel",
        &[PyRep::Integer(0)],
    )
    .await
    .expect("SetSafetyLevel Red");

    let _ = ctx.drain_notifications_for_char(PILOT_1);
    let _ = ctx.drain_notifications_for_char(PILOT_2);

    // Target lock victim ship
    rpc(
        &ctx,
        &mut sess_atk,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(ship_vic)],
    )
    .await
    .expect("AddTarget victim");
    jita_handle.step_manual(5.0).await.expect("step_manual");

    // Fire weapon at victim ship
    rpc(
        &ctx,
        &mut sess_atk,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(ship_vic),
        ],
    )
    .await
    .expect("Activate weapon on victim");

    // Assert immediate active timer notification states (101, 201, 301)
    let notifs = ctx.drain_notifications_for_char(PILOT_1);

    let (weap_code, weap_exp) = find_timer_notification(&notifs, "OnWeaponsTimerUpdate")
        .expect("OnWeaponsTimerUpdate active");
    assert_eq!(weap_code, 101, "Weapons timer active (101)");
    assert!(weap_exp.is_some());

    let (pvp_code, pvp_exp) = find_timer_notification(&notifs, "OnPvpTimerUpdate")
        .expect("OnPvpTimerUpdate active");
    assert_eq!(pvp_code, 201, "PvP timer active (201)");
    assert!(pvp_exp.is_some());

    let (crim_code, crim_exp) = find_timer_notification(&notifs, "OnCriminalTimerUpdate")
        .expect("OnCriminalTimerUpdate active");
    assert_eq!(crim_code, 301, "Criminal timer active (301)");
    assert!(crim_exp.is_some());

    // Advance clock past 1s trigger: timers transition into countdown states (102, 202, 303)
    ctx.advance_secs(3);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let tick_notifs = ctx.drain_notifications_for_char(PILOT_1);

    let (weap_cd, _) = find_timer_notification(&tick_notifs, "OnWeaponsTimerUpdate")
        .expect("OnWeaponsTimerUpdate countdown");
    assert_eq!(weap_cd, 102, "Weapons timer countdown (102)");

    let (pvp_cd, _) = find_timer_notification(&tick_notifs, "OnPvpTimerUpdate")
        .expect("OnPvpTimerUpdate countdown");
    assert_eq!(pvp_cd, 202, "PvP timer countdown (202)");

    let (crim_cd, _) = find_timer_notification(&tick_notifs, "OnCriminalTimerUpdate")
        .expect("OnCriminalTimerUpdate countdown");
    assert_eq!(crim_cd, 303, "Criminal timer countdown (303)");
}

#[tokio::test]
async fn test_live_crimewatch_npc_and_disapproval_timers() {
    // parity: notify:OnNpcTimerUpdate, notify:OnDisapprovalTimerUpdate
    eve_testkit::attest(&[
        "notify:OnNpcTimerUpdate",
        "notify:OnDisapprovalTimerUpdate",
    ]);

    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    let mut sess_atk = select_gm_pilot(&ctx, 62_001, "demo", "demo-secret", PILOT_1).await;
    let mut sess_vic = select_gm_pilot(&ctx, 62_002, "other", "other-secret", PILOT_2).await;

    // Both pilots undock into space in Jita
    rpc(&ctx, &mut sess_atk, "stationSvc", "Undock", &[]).await.expect("Undock atk");
    rpc(&ctx, &mut sess_vic, "stationSvc", "Undock", &[]).await.expect("Undock vic");

    let ship_atk = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].active_ship_id);
    let ship_vic = ctx.db.with_snapshot(|s| s.characters[&PILOT_2].active_ship_id);

    let jita_handle = ctx.systems.get_or_spawn(JITA_SYSTEM);
    let _ = jita_handle
        .set_ball_position(ship_atk, DVec3::new(15_000.0, 0.0, 0.0))
        .await;

    // Train skills, fit weapon, and load ammo
    slash(&ctx, &mut sess_atk, "/setskill 3300 5").await;
    slash(&ctx, &mut sess_atk, "/setskill 3301 5").await;
    let gun_rep = slash(&ctx, &mut sess_atk, "/fit me 484").await;
    let gun_id = gun_rep
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("gun itemID");
    slash(&ctx, &mut sess_atk, "/giveitem 178 500").await;

    // 1. NPC Timer: spawn NPC via live /spawnnpc command
    let spawn_rep = slash(&ctx, &mut sess_atk, "/spawnnpc 3744").await;
    let npc_id = spawn_rep
        .dict_get("npcID")
        .and_then(|v| v.as_i64())
        .expect("npcID");

    rpc(
        &ctx,
        &mut sess_atk,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(npc_id)],
    )
    .await
    .expect("AddTarget NPC");
    jita_handle.step_manual(5.0).await.expect("step_manual");

    let _ = ctx.drain_notifications_for_char(PILOT_1);

    // Fire weapon at NPC
    rpc(
        &ctx,
        &mut sess_atk,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(npc_id),
        ],
    )
    .await
    .expect("Activate weapon on NPC");

    let npc_notifs = ctx.drain_notifications_for_char(PILOT_1);
    let (npc_code, npc_exp) = find_timer_notification(&npc_notifs, "OnNpcTimerUpdate")
        .expect("OnNpcTimerUpdate active");
    assert_eq!(npc_code, 401, "NPC timer active (401)");
    assert!(npc_exp.is_some());

    // Deactivate weapon and unspawn NPC so it does not retaliate and re-trigger timer
    let _ = rpc(
        &ctx,
        &mut sess_atk,
        "dogmaIM",
        "Deactivate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("projectileFired".into()),
        ],
    )
    .await;
    slash(&ctx, &mut sess_atk, &format!("/unspawn {npc_id}")).await;

    // Advance clock: transitions to countdown state (402)
    ctx.advance_secs(3);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let npc_tick_notifs = ctx.drain_notifications_for_char(PILOT_1);
    let (npc_cd, _) = find_timer_notification(&npc_tick_notifs, "OnNpcTimerUpdate")
        .expect("OnNpcTimerUpdate countdown");
    assert_eq!(npc_cd, 402, "NPC timer countdown (402)");

    ctx.advance_secs(10);

    // 2. Disapproval Timer: travel to Deathless system Zarzakh and attack player
    slash(&ctx, &mut sess_atk, &format!("/tr {ZARZAKH_SYSTEM}")).await;
    slash(&ctx, &mut sess_vic, &format!("/tr {ZARZAKH_SYSTEM}")).await;

    let zarzakh_handle = ctx.systems.get_or_spawn(ZARZAKH_SYSTEM);
    let z_pos_atk = DVec3::new(5_000.0, 0.0, 0.0);
    let z_pos_vic = DVec3::new(7_000.0, 0.0, 0.0);
    let _ = zarzakh_handle.set_ball_position(ship_atk, z_pos_atk).await;
    let _ = zarzakh_handle.set_ball_position(ship_vic, z_pos_vic).await;

    rpc(
        &ctx,
        &mut sess_atk,
        "dogmaIM",
        "AddTarget",
        &[PyRep::Integer(ship_vic)],
    )
    .await
    .expect("AddTarget in Zarzakh");
    zarzakh_handle.step_manual(5.0).await.expect("step_manual");

    let _ = ctx.drain_notifications_for_char(PILOT_1);

    // Fire weapon in Zarzakh
    rpc(
        &ctx,
        &mut sess_atk,
        "dogmaIM",
        "Activate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("projectileFired".into()),
            PyRep::Integer(ship_vic),
        ],
    )
    .await
    .expect("Activate weapon in Zarzakh");

    let dis_notifs = ctx.drain_notifications_for_char(PILOT_1);
    let (dis_code, dis_exp) = find_timer_notification(&dis_notifs, "OnDisapprovalTimerUpdate")
        .expect("OnDisapprovalTimerUpdate active");
    assert_eq!(dis_code, 501, "Disapproval timer active (501)");
    assert!(dis_exp.is_some());

    // Deactivate weapon before advancing clock
    let _ = rpc(
        &ctx,
        &mut sess_atk,
        "dogmaIM",
        "Deactivate",
        &[
            PyRep::Integer(gun_id),
            PyRep::String("projectileFired".into()),
        ],
    )
    .await;

    // Advance clock: transitions to countdown state (502)
    ctx.advance_secs(3);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;

    let dis_tick_notifs = ctx.drain_notifications_for_char(PILOT_1);
    let (dis_cd, _) = find_timer_notification(&dis_tick_notifs, "OnDisapprovalTimerUpdate")
        .expect("OnDisapprovalTimerUpdate countdown");
    assert_eq!(dis_cd, 502, "Disapproval timer countdown (502)");
}
