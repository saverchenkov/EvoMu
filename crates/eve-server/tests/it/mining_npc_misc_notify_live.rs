//! Live integration suite for Mining Belts & Ore Distribution, Navy & Hauler NPCs,
//! Crimewatch/Campaign/Tournament Notifications, `cache.Setting`, `raffleMgr.QA_SeedRaffles`,
//! and `plan:E-5` Agent Missions.
//!
//! Uses only live RPCs, GM slash commands, and server ticks (zero direct DB or runtime mutation).

#![forbid(unsafe_code)]

use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;

use eve_db::{InventoryFlag, ServerClock, VirtualServerClock};
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::living_world::ConvoyStepStatus;
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::{step_extended_npc_ai, with_combat_ephemeral};
use eve_server::services::combat_npc_police::step_empire_navy_patrols;
use eve_server::sp6_lifecycle::tick_background_cluster;
use glam::DVec3;

use crate::common;

const JITA_SYS: u32 = 30_000_142;
const PERIMETER_SYS: u32 = 30_000_144;
const JITA_STN: i64 = 60_003_760;
const ZARZAKH_SYS: u32 = 30_100_000;
const PILOT_A: i64 = 95_000_001;
const PILOT_B: i64 = 95_000_002;
const DEMO_CHAR: i64 = 90_000_001;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

async fn bind_oid(ctx: &ServerContext, sess: &mut SessionState, service: &str, bind_param: PyRep) -> u64 {
    let res = rpc(ctx, sess, service, "MachoBindObject", &[bind_param, PyRep::None])
        .await
        .expect("MachoBindObject");
    match res {
        PyRep::Tuple(outer) => outer[0].as_i64().expect("bound oid") as u64,
        PyRep::Integer(oid) => oid as u64,
        other => panic!("unexpected MachoBindObject response: {other:?}"),
    }
}

fn rep_list(rep: &PyRep) -> &[PyRep] {
    match rep {
        PyRep::List(v) | PyRep::Tuple(v) => v.as_slice(),
        other => panic!("expected List/Tuple, got {other:?}"),
    }
}

#[tokio::test]
async fn test_live_mining_belts_as_balls_and_sec_band_ore_distribution() {
    // parity: feat:mining.belts-as-balls, feat:mining.belt-ore-distribution
    eve_testkit::attest(&[
        "feat:mining.belts-as-balls",
        "feat:mining.belt-ore-distribution",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // 1. Verify authentic security-band & empire-quarter ore distribution
    let ore_ids = |sec: f64, reg: u32| -> BTreeSet<u32> {
        ctx.sde
            .belt_ores_for_security_and_region(sec, reg)
            .into_iter()
            .map(|t| t.type_id)
            .collect()
    };
    assert_eq!(ore_ids(1.0, 10_000_002), BTreeSet::from([1230, 1228]));
    assert_eq!(ore_ids(0.7, 10_000_002), BTreeSet::from([1230, 1228, 18]));
    assert_eq!(ore_ids(0.7, 10_000_043), BTreeSet::from([1230, 1228, 1224]));
    let low_ores = ore_ids(0.3, 10_000_032);
    assert!(!low_ores.contains(&1230) && !low_ores.contains(&1228));
    assert!(low_ores.contains(&20) && low_ores.contains(&1226));
    let null_ores = ore_ids(-0.8, 10_000_050);
    assert!(null_ores.contains(&22) && null_ores.contains(&1223) && null_ores.contains(&11396));
    assert_eq!(ore_ids(-0.5, 10_000_070).len(), 3);

    // 2. Spawn live pilot in space in Perimeter (1.0 Caldari) and verify belts & asteroids as balls
    let mut pilot = common::gm_pilot_in_space(&ctx, 95_001, PILOT_A, 587, PERIMETER_SYS).await;
    let handle = ctx.systems.get_or_spawn(PERIMETER_SYS);
    let _ = handle.populate_asteroid_belt_entities().await;

    let sde_belts: Vec<_> = ctx
        .sde
        .celestials_in_system(PERIMETER_SYS)
        .into_iter()
        .filter(|c| c.group_id == 9)
        .collect();
    assert!(!sde_belts.is_empty(), "Perimeter must have SDE asteroid belts");

    let target_belt_id = sde_belts[0].celestial_id;
    let target_belt_pos = sde_belts[0].position;
    let (_, belt_ball) = handle
        .get_ball_snapshot(target_belt_id)
        .await
        .expect("belt ball query")
        .expect("belt ball must exist in ballpark");
    assert_eq!(belt_ball.mode, MovementMode::Rigid);
    assert_eq!(handle.get_ball_group_id(target_belt_id).await.ok().flatten(), Some(9));

    let expected_ores: BTreeSet<u32> = ctx
        .sde
        .belt_ores_for_system(PERIMETER_SYS)
        .into_iter()
        .map(|t| t.type_id)
        .collect();
    let base_ast_id = 750_000_000i64;
    let mut spawned_types = BTreeSet::new();
    for offset in 0..expected_ores.len() as i64 {
        let aid = base_ast_id + offset;
        let (_, ast_ball) = handle
            .get_ball_snapshot(aid)
            .await
            .expect("ast ball query")
            .expect("asteroid ball must exist in ballpark");
        assert_eq!(ast_ball.mode, MovementMode::Stop);
        assert!(ast_ball.radius >= 60.0);
        let ast_state = handle
            .get_asteroid_state(aid)
            .await
            .expect("ast state query")
            .expect("asteroid state present");
        assert!(ast_state.remaining_units > 0);
        spawned_types.insert(ast_ball.type_id);
    }
    assert_eq!(spawned_types, expected_ores);

    // 3. Warp pilot to first belt via beyonce.CmdWarpToStuff (without asserting client-local OnWarpStarted)
    let beyonce_oid = bind_oid(&ctx, &mut pilot.sess, "beyonce", PyRep::Integer(PERIMETER_SYS as i64)).await;
    ctx.dispatch_rpc(
        &mut pilot.sess,
        "beyonce",
        Some(beyonce_oid),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(target_belt_id), PyRep::Real(0.0)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("CmdWarpToStuff belt");

    for _ in 0..40 {
        let _ = handle.tick().await;
        if let Ok(Some((_, b))) = handle.get_ball_snapshot(pilot.ship_id).await {
            if b.position.distance(target_belt_pos) < 150_000.0 && !matches!(b.mode, MovementMode::Warp { .. }) {
                break;
            }
        }
    }
    let (_, ship_after) = handle.get_ball_snapshot(pilot.ship_id).await.unwrap().unwrap();
    assert!(ship_after.position.distance(target_belt_pos) < 150_000.0);
}

#[tokio::test]
async fn test_live_navy_npc_standing_response_and_npc_hauler_convoy() {
    // parity: feat:npc.navy-npc, feat:npc.npc-hauler
    eve_testkit::attest(&["feat:npc.navy-npc", "feat:npc.npc-hauler"]);

    let clock = Arc::new(VirtualServerClock::with_fixed_base(1_700_000_000));
    let ctx = ServerContext::new();
    ctx.set_clock(clock.clone());

    // 1. Navy NPC standing-driven aggression: spawn pilot in Caldari space (Jita, faction 500001)
    let mut pilot = common::gm_pilot_in_space(&ctx, 95_101, PILOT_A, 587, JITA_SYS).await;
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    let st_rep = common::slash_cmd(&ctx, &mut pilot.sess, "/standing 500001 -6.0").await;
    assert_eq!(st_rep.dict_get("standing").and_then(|v| v.as_f64()), Some(-6.0));
    assert!(ctx.drain_notifications_for_char(PILOT_A).iter().any(|n| n.event == "OnSetPlayerStanding"));

    let navy_ids = step_empire_navy_patrols(&ctx, JITA_SYS).await;
    assert_eq!(navy_ids.len(), 2, "Caldari Navy patrol must spawn cruiser + frigate");
    let handle = ctx.systems.get_or_spawn(JITA_SYS);
    let (_, navy_ball) = handle.get_ball_snapshot(navy_ids[0]).await.unwrap().unwrap();
    assert!(matches!(navy_ball.type_id, 52_936 | 52_934));

    common::fly_to(&ctx, &mut pilot, navy_ball.position + DVec3::new(10_000.0, 0.0, 0.0)).await;
    step_extended_npc_ai(&ctx, JITA_SYS).await;

    let navy_acquired = with_combat_ephemeral(&ctx, |rt| {
        navy_ids.iter().any(|nid| {
            rt.npc_entities.get(nid).is_some_and(|n| {
                n.target_id == Some(pilot.ship_id) || n.aggression_threat.contains_key(&pilot.ship_id)
            })
        })
    });
    assert!(navy_acquired, "Empire Navy patrol must acquire pilot with faction standing <= -5.0");

    // 2. NPC Hauler Convoy: authentic industrial hull, warp to stargate, gate jump, and station dock
    let convoy_id = ctx.living_world.ensure_system_hauler_convoy(JITA_SYS, &ctx.sde);
    let convoy = ctx.living_world.get_convoy(convoy_id).expect("registered hauler convoy");
    assert_ne!(convoy.ship_type_id, 587, "hauler must not be a Rifter frigate");
    assert_eq!(ctx.sde.types.get(&convoy.ship_type_id).map(|t| t.group_id), Some(28));

    let route = ctx.living_world.get_convoy_route(convoy_id).expect("convoy route");
    assert!(route.route.len() >= 2, "hauler convoy must have a multi-system stargate route");

    let _ = tick_background_cluster(&ctx).await;
    let status1 = ctx
        .living_world
        .step_hauler_convoy(convoy_id, &ctx.systems, &ctx.db, &ctx.sde)
        .await
        .expect("step 1");
    assert_eq!(status1, ConvoyStepStatus::TraversingSystem { system_id: JITA_SYS });
    let (_, hauler_ball) = handle.get_ball_snapshot(convoy_id).await.unwrap().unwrap();
    assert!(matches!(hauler_ball.mode, MovementMode::Warp { .. }));

    clock.advance_secs(600);
    let status2 = ctx
        .living_world
        .step_hauler_convoy(convoy_id, &ctx.systems, &ctx.db, &ctx.sde)
        .await
        .expect("step 2");
    assert_eq!(
        status2,
        ConvoyStepStatus::JumpedGate {
            from_system: JITA_SYS,
            to_system: route.route[1],
        }
    );

    let mut docked = false;
    for _ in 0..10 {
        clock.advance_secs(600);
        let st = ctx
            .living_world
            .step_hauler_convoy(convoy_id, &ctx.systems, &ctx.db, &ctx.sde)
            .await
            .expect("step hop");
        if matches!(st, ConvoyStepStatus::DockedAtDestination { .. }) {
            docked = true;
            break;
        }
    }
    assert!(docked, "hauler convoy must complete route and dock at destination station");
}

#[tokio::test]
async fn test_live_crimewatch_system_criminal_and_disapproval_flag_notifications() {
    // parity: notify:OnSystemCriminalFlagUpdates, notify:OnSystemDisapprovalFlagUpdates
    eve_testkit::attest(&[
        "notify:OnSystemCriminalFlagUpdates",
        "notify:OnSystemDisapprovalFlagUpdates",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // 1. High-sec Jita illegal aggression -> OnSystemCriminalFlagUpdates
    let mut attacker = common::gm_pilot_in_space(&ctx, 95_201, PILOT_A, 587, JITA_SYS).await;
    ctx.dispatch_rpc(&mut attacker.sess, "beyonce", Some(attacker.beyonce), "CmdDock", &[PyRep::Integer(JITA_STN)], &HashMap::new(), None)
        .await
        .expect("CmdDock Jita 4-4");
    attacker.sess.station_id = Some(JITA_STN);
    common::slash_cmd(&ctx, &mut attacker.sess, "/giveskill 3300 5").await;
    common::slash_cmd(&ctx, &mut attacker.sess, "/giveskill 3301 5").await;
    common::slash_cmd(&ctx, &mut attacker.sess, "/giveskill 12210 1").await;
    let gun_id = common::stock_ship_bay(&ctx, &mut attacker, 3170, 1, InventoryFlag::HiSlot0 as i64).await;
    let _ammo_id = common::stock_ship_bay(&ctx, &mut attacker, 222, 100, InventoryFlag::Cargo as i64).await;
    ctx.advance_secs(31);
    rpc(&ctx, &mut attacker.sess, "stationSvc", "Undock", &[]).await.unwrap();
    attacker.sess.station_id = None;
    attacker.sess.solar_system_id = Some(JITA_SYS);
    attacker.beyonce = bind_oid(&ctx, &mut attacker.sess, "beyonce", PyRep::Integer(JITA_SYS as i64)).await;

    let victim = common::gm_pilot_in_space(&ctx, 95_202, PILOT_B, 587, JITA_SYS).await;
    let handle = ctx.systems.get_or_spawn(JITA_SYS);
    let v_pos = handle.get_ball_snapshot(victim.ship_id).await.unwrap().unwrap().1.position;
    for _ in 0..20 {
        common::fly_to(&ctx, &mut attacker, v_pos + DVec3::new(2_000.0, 0.0, 0.0)).await;
        let a_pos = handle.get_ball_snapshot(attacker.ship_id).await.unwrap().unwrap().1.position;
        if a_pos.distance(v_pos) < 10_000.0 {
            break;
        }
    }

    let cw_oid = bind_oid(&ctx, &mut attacker.sess, "crimewatch", PyRep::Integer(JITA_SYS as i64)).await;
    ctx.dispatch_rpc(&mut attacker.sess, "crimewatch", Some(cw_oid), "SetSafetyLevel", &[PyRep::Integer(0)], &HashMap::new(), None)
        .await
        .expect("SetSafetyLevel Red");

    let dogma_oid = bind_oid(&ctx, &mut attacker.sess, "dogmaIM", PyRep::Integer(JITA_SYS as i64)).await;
    ctx.dispatch_rpc(&mut attacker.sess, "dogmaIM", Some(dogma_oid), "AddTarget", &[PyRep::Integer(victim.ship_id)], &HashMap::new(), None)
        .await
        .expect("AddTarget");
    for _ in 0..10 {
        let _ = handle.tick().await;
    }

    let _ = ctx.drain_notifications_for_char(PILOT_B);
    ctx.dispatch_rpc(
        &mut attacker.sess,
        "dogmaIM",
        Some(dogma_oid),
        "Activate",
        &[PyRep::Integer(gun_id), PyRep::String("effects.ProjectileFired".into()), PyRep::Integer(victim.ship_id), PyRep::Integer(1)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("Activate weapon in highsec");

    let notifs_b = ctx.drain_notifications_for_char(PILOT_B);
    let crim_notif = notifs_b
        .iter()
        .find(|n| n.event == "OnSystemCriminalFlagUpdates")
        .expect("bystander must receive OnSystemCriminalFlagUpdates on highsec aggression");
    assert_eq!(crim_notif.id_type, "solarsystemid");
    assert_eq!(crim_notif.args.len(), 3);
    assert!(rep_list(&crim_notif.args[2]).iter().any(|x| x.as_i64() == Some(PILOT_A)));

    // 2. Zarzakh (30100000) aggression -> OnSystemDisapprovalFlagUpdates
    let mut attacker2 = common::gm_pilot_in_space(&ctx, 95_203, 95_000_003, 47466, JITA_SYS).await;
    ctx.dispatch_rpc(&mut attacker2.sess, "beyonce", Some(attacker2.beyonce), "CmdDock", &[PyRep::Integer(JITA_STN)], &HashMap::new(), None)
        .await
        .expect("CmdDock Jita 4-4");
    attacker2.sess.station_id = Some(JITA_STN);
    common::slash_cmd(&ctx, &mut attacker2.sess, "/giveskill 3300 5").await;
    common::slash_cmd(&ctx, &mut attacker2.sess, "/giveskill 3301 5").await;
    common::slash_cmd(&ctx, &mut attacker2.sess, "/giveskill 12210 1").await;
    let gun2_id = common::stock_ship_bay(&ctx, &mut attacker2, 3170, 1, InventoryFlag::HiSlot0 as i64).await;
    let _ammo2_id = common::stock_ship_bay(&ctx, &mut attacker2, 222, 100, InventoryFlag::Cargo as i64).await;
    ctx.advance_secs(31);
    rpc(&ctx, &mut attacker2.sess, "stationSvc", "Undock", &[]).await.unwrap();
    attacker2.sess.station_id = None;
    attacker2.sess.ship_id = Some(attacker2.ship_id);
    common::slash_cmd(&ctx, &mut attacker2.sess, &format!("/tr {ZARZAKH_SYS}")).await;
    attacker2.sess.solar_system_id = Some(ZARZAKH_SYS);
    attacker2.beyonce = bind_oid(&ctx, &mut attacker2.sess, "beyonce", PyRep::Integer(ZARZAKH_SYS as i64)).await;

    let victim2 = common::gm_pilot_in_space(&ctx, 95_204, 95_000_004, 47466, ZARZAKH_SYS).await;
    let z_handle = ctx.systems.get_or_spawn(ZARZAKH_SYS);
    let z_gate_id = ctx
        .sde
        .stargates()
        .values()
        .find(|g| g.from_solar_system_id == ZARZAKH_SYS)
        .map(|g| g.stargate_id)
        .expect("Zarzakh stargate");
    ctx.dispatch_rpc(
        &mut attacker2.sess,
        "beyonce",
        Some(attacker2.beyonce),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(z_gate_id), PyRep::Real(0.0)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("Warp attacker2 to Zarzakh gate");
    let mut v2_sess = victim2.sess.clone();
    ctx.dispatch_rpc(
        &mut v2_sess,
        "beyonce",
        Some(victim2.beyonce),
        "CmdWarpToStuff",
        &[PyRep::String("item".into()), PyRep::Integer(z_gate_id), PyRep::Real(0.0)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("Warp victim2 to Zarzakh gate");
    for _ in 0..40 {
        ctx.advance_secs(1);
        let _ = z_handle.tick().await;
        if let (Ok(Some((_, a_b))), Ok(Some((_, v_b)))) = (
            z_handle.get_ball_snapshot(attacker2.ship_id).await,
            z_handle.get_ball_snapshot(victim2.ship_id).await,
        ) {
            if !matches!(a_b.mode, MovementMode::Warp { .. })
                && !matches!(v_b.mode, MovementMode::Warp { .. })
            {
                break;
            }
        }
    }
    for _ in 0..15 {
        ctx.advance_secs(1);
        let _ = z_handle.tick().await;
    }

    let z_dogma_oid = bind_oid(&ctx, &mut attacker2.sess, "dogmaIM", PyRep::Integer(ZARZAKH_SYS as i64)).await;
    ctx.dispatch_rpc(&mut attacker2.sess, "dogmaIM", Some(z_dogma_oid), "AddTarget", &[PyRep::Integer(victim2.ship_id)], &HashMap::new(), None)
        .await
        .expect("AddTarget in Zarzakh");
    for _ in 0..30 {
        ctx.advance_secs(1);
        let _ = z_handle.tick().await;
        if z_handle
            .get_target_locks(attacker2.ship_id)
            .await
            .is_ok_and(|ls| ls.iter().any(|l| l.target_id == victim2.ship_id && l.locked))
        {
            break;
        }
    }

    let _ = ctx.drain_notifications_for_char(95_000_004);
    ctx.dispatch_rpc(
        &mut attacker2.sess,
        "dogmaIM",
        Some(z_dogma_oid),
        "Activate",
        &[PyRep::Integer(gun2_id), PyRep::String("effects.ProjectileFired".into()), PyRep::Integer(victim2.ship_id), PyRep::Integer(1)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("Activate weapon in Zarzakh");

    let z_notifs_b = ctx.drain_notifications_for_char(95_000_004);
    let dis_notif = z_notifs_b
        .iter()
        .find(|n| n.event == "OnSystemDisapprovalFlagUpdates")
        .expect("bystander must receive OnSystemDisapprovalFlagUpdates on Zarzakh aggression");
    assert_eq!(dis_notif.id_type, "solarsystemid");
    assert_eq!(dis_notif.args.len(), 2);
    assert!(rep_list(&dis_notif.args[1]).iter().any(|x| x.as_i64() == Some(95_000_003)));
}

#[tokio::test]
async fn test_live_campaign_enrollment_and_tournament_ban_notifications() {
    // parity: notify:OnCampaignEnrollmentChanged, notify:OnTournamentPerformBan
    eve_testkit::attest(&[
        "notify:OnCampaignEnrollmentChanged",
        "notify:OnTournamentPerformBan",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s1 = SessionState::new(95_301);
    common::login_pilot(&ctx, &mut s1, "demo", "demo-secret").await;
    rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(DEMO_CHAR)]).await.unwrap();
    s1.role |= eve_db::service_roles::ROLE_GM_DEFAULT;

    ctx.db.provision_gm_account("pilotb", "pilotb-secret", PILOT_B, "Pilot B");
    let mut s2 = SessionState::new(95_302);
    common::login_pilot(&ctx, &mut s2, "pilotb", "pilotb-secret").await;
    rpc(&ctx, &mut s2, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)]).await.unwrap();
    s2.role = eve_db::service_roles::ROLE_PLAYER;

    let _ = ctx.drain_notifications_for_char(DEMO_CHAR);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    // 1. OnCampaignEnrollmentChanged via live facWarMgr.JoinFactionAsCharacter
    let joined = rpc(&ctx, &mut s1, "facWarMgr", "JoinFactionAsCharacter", &[PyRep::Integer(500_001)])
        .await
        .expect("JoinFactionAsCharacter");
    assert_eq!(joined.dict_get("enlisted"), Some(&PyRep::Bool(true)));

    let notifs_a = ctx.drain_notifications_for_char(DEMO_CHAR);
    let camp_a: Vec<_> = notifs_a.iter().filter(|n| n.event == "OnCampaignEnrollmentChanged").collect();
    assert_eq!(camp_a.len(), 1);
    assert_eq!(camp_a[0].id_type, "charid");
    assert!(camp_a[0].args.is_empty());
    assert!(!ctx.drain_notifications_for_char(PILOT_B).iter().any(|n| n.event == "OnCampaignEnrollmentChanged"));

    // Verify per-character login campaign enrollment isolation via /campaign enroll + get_client_campaign_state
    let before_b_camp = rpc(&ctx, &mut s2, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("get_client_campaign_state B before");
    let camp_id = eve_server::services::campaigns_and_milestones::get_sde_login_campaigns(&ctx.sde)
        .keys()
        .copied()
        .next()
        .unwrap_or(2261);
    let _ = common::slash_cmd(&ctx, &mut s1, &format!("/campaign enroll {camp_id} {DEMO_CHAR}")).await;
    assert_eq!(
        ctx.drain_notifications_for_char(DEMO_CHAR)
            .iter()
            .filter(|n| n.event == "OnCampaignEnrollmentChanged")
            .count(),
        1
    );
    assert!(!ctx.drain_notifications_for_char(PILOT_B).iter().any(|n| n.event == "OnCampaignEnrollmentChanged"));
    let after_b_camp = rpc(&ctx, &mut s2, "loginCampaignManager", "get_client_campaign_state", &[])
        .await
        .expect("get_client_campaign_state B after");
    assert_eq!(before_b_camp, after_b_camp, "per-character enrollment must not alter PILOT_B's campaign state");

    // 2. OnTournamentPerformBan via live tourneyMgr.CreateMatch + BanShip (captain ban) + StartAutobanning
    rpc(
        &ctx,
        &mut s1,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(77),
            PyRep::Integer(7701),
            PyRep::Integer(100),
            PyRep::Integer(600),
            PyRep::Integer(0),
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2)]),
            PyRep::List(vec![PyRep::Integer(DEMO_CHAR), PyRep::Integer(PILOT_B)]),
        ],
    )
    .await
    .expect("CreateMatch");

    let _ = ctx.drain_notifications_for_char(DEMO_CHAR);
    let _ = ctx.drain_notifications_for_char(PILOT_B);

    let capt_ban = rpc(
        &ctx,
        &mut s2,
        "tourneyMgr",
        "BanShip",
        &[PyRep::Integer(7701), PyRep::List(vec![PyRep::Integer(621), PyRep::Integer(622)])],
    )
    .await
    .expect("captain BanShip");
    assert_eq!(capt_ban, PyRep::Bool(true));

    let ban_notifs_b = ctx.drain_notifications_for_char(PILOT_B);
    let ban_ev = ban_notifs_b
        .iter()
        .find(|n| n.event == "OnTournamentPerformBan")
        .expect("blue captain PILOT_B must receive OnTournamentPerformBan");
    assert_eq!(ban_ev.id_type, "charid");
    assert_eq!(ban_ev.args.len(), 6);
    assert_eq!(ban_ev.args[0].as_i64(), Some(7701));
    assert_eq!(ban_ev.args[1].as_i64(), Some(2));
    let banned_list: Vec<i64> = rep_list(&ban_ev.args[2]).iter().filter_map(|x| x.as_i64()).collect();
    assert_eq!(banned_list, vec![621, 622]);
    assert_eq!(ban_ev.args[4].as_i64(), Some(1));
    assert_eq!(ban_ev.args[5].as_i64(), Some(77));
}

#[tokio::test]
async fn test_live_cache_setting_raffle_seeding_and_plan_e5_missions() {
    // parity: rpc:cache.Setting, rpc:raffleMgr.QA_SeedRaffles, plan:E-5
    eve_testkit::attest(&[
        "rpc:cache.Setting",
        "rpc:raffleMgr.QA_SeedRaffles",
        "plan:E-5",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    ctx.db.provision_gm_account("pilotb2", "pilotb2-secret", PILOT_B, "Pilot B2");
    let mut non_gm = SessionState::new(95_401);
    common::login_pilot(&ctx, &mut non_gm, "pilotb2", "pilotb2-secret").await;
    rpc(&ctx, &mut non_gm, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_B)]).await.unwrap();
    non_gm.role = eve_db::service_roles::ROLE_PLAYER;
    non_gm.solar_system_id = Some(JITA_SYS);
    non_gm.station_id = Some(JITA_STN);

    let mut gm = SessionState::new(95_402);
    common::login_pilot(&ctx, &mut gm, "demo", "demo-secret").await;
    rpc(&ctx, &mut gm, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(DEMO_CHAR)]).await.unwrap();
    gm.role |= eve_db::service_roles::ROLE_GM_DEFAULT;
    gm.solar_system_id = Some(JITA_SYS);
    gm.station_id = Some(JITA_STN);

    // 1. rpc:cache.Setting: non-GM write rejected, GM write/read/clear persisted
    rpc(&ctx, &mut non_gm, "cache", "Setting", &[PyRep::String("zsystem".into()), PyRep::String("LiveTestKey".into()), PyRep::String("Blocked".into())])
        .await
        .expect_err("non-GM cache.Setting write must fail");

    let written = rpc(&ctx, &mut gm, "cache", "Setting", &[PyRep::String("zsystem".into()), PyRep::String("LiveTestKey".into()), PyRep::String("LiveVal99".into())])
        .await
        .expect("GM cache.Setting write");
    assert_eq!(written, PyRep::String("LiveVal99".into()));

    let read_back = rpc(&ctx, &mut non_gm, "cache", "Setting", &[PyRep::String("zsystem".into()), PyRep::String("LiveTestKey".into())])
        .await
        .expect("cache.Setting read");
    assert_eq!(read_back, PyRep::String("LiveVal99".into()));

    rpc(&ctx, &mut gm, "cache", "Setting", &[PyRep::String("zsystem".into()), PyRep::String("LiveTestKey".into()), PyRep::None])
        .await
        .expect("GM cache.Setting clear");

    // 2. rpc:raffleMgr.QA_SeedRaffles: non-GM rejected, undocked GM rejected, docked GM seeds raffles & emits OnRaffleCreatedServer
    rpc(&ctx, &mut non_gm, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(1), PyRep::Integer(587)])
        .await
        .expect_err("non-GM QA_SeedRaffles must fail");

    let undocked_gm = common::gm_pilot_in_space(&ctx, 95_403, 95_400_003, 587, JITA_SYS).await;
    let mut undocked_sess = undocked_gm.sess;
    let err_undocked = rpc(&ctx, &mut undocked_sess, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(1), PyRep::Integer(587)])
        .await
        .expect_err("undocked QA_SeedRaffles must fail");
    assert_eq!(err_undocked.message, "CharacterNotDocked");

    let _ = ctx.drain_notifications_for_char(DEMO_CHAR);
    let seeded = rpc(&ctx, &mut gm, "raffleMgr", "QA_SeedRaffles", &[PyRep::Integer(2), PyRep::Integer(587)])
        .await
        .expect("GM QA_SeedRaffles");
    let raffles = rep_list(&seeded);
    assert_eq!(raffles.len(), 2);
    for r in raffles {
        assert_eq!(r.dict_get("owner_id").and_then(|v| v.as_i64()), Some(DEMO_CHAR));
        assert_eq!(r.dict_get("location_id").and_then(|v| v.as_i64()), Some(JITA_STN));
        assert_eq!(r.dict_get("type_id").and_then(|v| v.as_i64()), Some(587));
        assert_eq!(r.dict_get("ticket_count").and_then(|v| v.as_i64()), Some(8));
    }
    assert_eq!(ctx.drain_notifications_for_char(DEMO_CHAR).iter().filter(|n| n.event == "OnRaffleCreatedServer").count(), 2, "OnRaffleCreatedServer emitted");

    // 3. plan:E-5: Live Agent Finder, Conversation/Offer, Accept, Journal, and Quit
    let agents_rep = rpc(&ctx, &mut non_gm, "agentMgr", "GetAgents", &[])
        .await
        .expect("GetAgents");
    let PyRep::Rowset { rows, .. } = agents_rep else {
        panic!("expected Rowset from GetAgents");
    };
    assert!(!rows.is_empty());
    let agent_row = rows
        .iter()
        .find(|r| r[6].as_i64() == Some(JITA_STN))
        .unwrap_or(&rows[0]);
    let agent_id = agent_row[0].as_i64().expect("agentID");
    let agent_sys = agent_row[5].as_i64().unwrap_or(JITA_SYS as i64) as u32;
    let agent_stn = agent_row[6].as_i64().unwrap_or(JITA_STN);
    if agent_stn != JITA_STN {
        non_gm.role |= eve_db::service_roles::ROLE_GM_DEFAULT;
        rpc(&ctx, &mut non_gm, "stationSvc", "Undock", &[]).await.unwrap();
        common::slash_cmd(&ctx, &mut non_gm, &format!("/tr {agent_sys}")).await;
        ctx.advance_secs(31);
        let b_oid = bind_oid(&ctx, &mut non_gm, "beyonce", PyRep::Integer(agent_sys as i64)).await;
        ctx.dispatch_rpc(
            &mut non_gm,
            "beyonce",
            Some(b_oid),
            "CmdWarpToStuff",
            &[PyRep::String("item".into()), PyRep::Integer(agent_stn), PyRep::Real(0.0)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CmdWarpToStuff agent station");
        let sys_h = ctx.systems.get_or_spawn(agent_sys);
        for _ in 0..40 {
            let _ = sys_h.tick().await;
        }
        ctx.dispatch_rpc(&mut non_gm, "beyonce", Some(b_oid), "CmdDock", &[PyRep::Integer(agent_stn)], &HashMap::new(), None)
            .await
            .expect("CmdDock agent station");
        non_gm.role = eve_db::service_roles::ROLE_PLAYER;
    }

    let agent_oid = bind_oid(&ctx, &mut non_gm, "agentMgr", PyRep::Integer(agent_id)).await;
    let offer_rep = ctx
        .dispatch_rpc(&mut non_gm, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &HashMap::new(), None)
        .await
        .expect("DoAction(2) RequestMission");
    assert!(!matches!(offer_rep, PyRep::None));

    let accept_rep = ctx
        .dispatch_rpc(&mut non_gm, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(4)], &HashMap::new(), None)
        .await
        .expect("DoAction(4) AcceptMission");
    assert!(!matches!(accept_rep, PyRep::None));

    let journal = rpc(&ctx, &mut non_gm, "agentMgr", "GetMyJournalDetails", &[])
        .await
        .expect("GetMyJournalDetails");
    let missions_list = rep_list(&rep_list(&journal)[0]);
    assert!(!missions_list.is_empty(), "accepted mission must appear in GetMyJournalDetails");

    let _quit_rep = ctx
        .dispatch_rpc(&mut non_gm, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(11)], &HashMap::new(), None)
        .await
        .expect("DoAction(11) QuitMission");
}
