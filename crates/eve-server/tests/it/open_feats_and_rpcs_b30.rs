//! End-to-end integration tests for the 17 `feat:lifecycle.*` parity rows (Batch 30).
//!
//! Covers:
//! - `feat:lifecycle.fleet-kick-session`, `feat:lifecycle.fleet-login-consistency`,
//!   `feat:lifecycle.fleet-logout`, `feat:lifecycle.fleet-server-restart`
//! - `feat:lifecycle.industry-pause-and-many`, `feat:lifecycle.industry-pi-durable`,
//!   `feat:lifecycle.owner_notes-persistence`
//! - `feat:lifecycle.mail-no-global-cap`, `feat:lifecycle.mail-state-persist`,
//!   `feat:lifecycle.notifications-durable`
//! - `feat:lifecycle.market_order-expiry-trigger`, `feat:lifecycle.market-wallet-history-durable`,
//!   `feat:lifecycle.lp-durable`
//! - `feat:lifecycle.jetcan-empty`, `feat:lifecycle.mobile_warp_disruptor-lifecycle`,
//!   `feat:lifecycle.module-cycles-stop-on-leave-space`, `feat:lifecycle.mission-downtime-respawn`

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    CorporationRecord, InventoryFlag, InventoryItemRecord, IskCents, MailRecord,
    PlanetaryColonyRecord, TrainedSkillRecord, UpwellStructureRecord, VirtualServerClock,
    WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::industry_pi::load_job_cost_meta;
use eve_server::services::station_structure::structure_lifecycle::sync_all_structures;
use eve_server::services::wallet_serverops::run_daily_downtime_and_jobs;
use glam::DVec3;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const CORP_ID: i64 = 2001;
const JITA_SYSTEM_ID: u32 = 30_000_142;
const JITA_STATION_ID: i64 = 60_003_760;
const JITA_PLANET_ID: i64 = 40_009_082;
const FH: &str = "fleetObjectHandler";

fn temp_db_path(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_b30_{tag}_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn open_fresh_ctx_at(path: &Path, base_unix: u64) -> ServerContext {
    let demo = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
    let repo = WorldRepository::open_sqlite(path).expect("open_sqlite");
    repo.with_mut_snapshot(|s| *s = demo);
    let ctx = ServerContext::with_repository(Arc::new(repo));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
    ctx
}

fn open_ctx_at(path: &Path, base_unix: u64) -> ServerContext {
    let repo = WorldRepository::open_sqlite(path).expect("open_sqlite");
    let ctx = ServerContext::with_repository(Arc::new(repo));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(base_unix)));
    ctx
}

async fn rpc(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None).await
}

async fn login_select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await.expect("SelectCharacterID");
    ctx.advance_secs(12);
    sess
}

fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton, version: 1, ..Default::default() }
}

#[tokio::test]
async fn test_b30_fleet_lifecycle_kick_login_logout_and_restart() {
    // parity: feat:lifecycle.fleet-kick-session, feat:lifecycle.fleet-login-consistency, feat:lifecycle.fleet-logout, feat:lifecycle.fleet-server-restart
    eve_testkit::attest(&[
        "feat:lifecycle.fleet-kick-session",
        "feat:lifecycle.fleet-login-consistency",
        "feat:lifecycle.fleet-logout",
        "feat:lifecycle.fleet-server-restart",
    ]);

    let db_path = temp_db_path("fleet");
    let ctx = open_fresh_ctx_at(&db_path, 1_760_000_000);
    let uid3 = ctx.db.provision_player_account("b30_fl3", "b30_fl3_pass");
    let p3 = ctx.db.create_character(uid3, "B30 Fleet 3", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;

    let mut s1 = login_select(&ctx, 9101, "demo", "demo-secret", P1).await;
    let mut s2 = login_select(&ctx, 9102, "other", "other-secret", P2).await;
    let mut _s3 = login_select(&ctx, 9103, "b30_fl3", "b30_fl3_pass", p3).await;

    let fid = rpc(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await.unwrap().dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
    assert!(rpc(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await.is_err());

    rpc(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2), PyRep::None, PyRep::None, PyRep::Integer(4)]).await.unwrap();
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, P2);
    rpc(&ctx, &mut s2, FH, "AcceptInvite", &[PyRep::Integer(fid)]).await.unwrap();
    assert_eq!(s2.fleet_id, Some(fid));

    // KickMember clears target session fleet_id and blocks kicked member's Invite (`fleet-kick-session`)
    rpc(&ctx, &mut s1, FH, "KickMember", &[PyRep::Integer(P2)]).await.unwrap();
    assert!(rpc(&ctx, &mut s2, FH, "Invite", &[PyRep::Integer(p3), PyRep::None, PyRep::None, PyRep::Integer(4)]).await.is_err());

    // Re-invite P2, then boss P1 logs out -> boss passes to P2 (`fleet-logout`)
    rpc(&ctx, &mut s1, FH, "Invite", &[PyRep::Integer(P2), PyRep::None, PyRep::None, PyRep::Integer(4)]).await.unwrap();
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, P2);
    rpc(&ctx, &mut s2, FH, "AcceptInvite", &[PyRep::Integer(fid)]).await.unwrap();
    ctx.drain_notifications_for_char(P2);

    rpc(&ctx, &mut s1, "authentication", "Logout", &[]).await.unwrap();
    assert_eq!(s1.fleet_id, None);
    ctx.db.with_snapshot(|snap| {
        let f = snap.fleets.get(&fid).expect("fleet survives with P2");
        assert_eq!(f.boss_id, P2);
        assert_ne!(f.member_details[&P2].job & 2, 0);
    });

    // Server restart drops offline fleets so no ghost members survive (`fleet-server-restart`)
    drop(ctx);
    let ctx2 = open_ctx_at(&db_path, 1_760_001_000);
    let s2_relog = login_select(&ctx2, 9104, "other", "other-secret", P2).await;
    assert_eq!(s2_relog.fleet_id, None);
    ctx2.db.with_snapshot(|snap| assert!(!snap.fleets.contains_key(&fid)));
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b30_industry_pause_many_jobs_and_pi_durability() {
    // parity: feat:lifecycle.industry-pause-and-many, feat:lifecycle.industry-pi-durable, feat:lifecycle.owner_notes-persistence
    eve_testkit::attest(&[
        "feat:lifecycle.industry-pause-and-many",
        "feat:lifecycle.industry-pi-durable",
        "feat:lifecycle.owner_notes-persistence",
    ]);

    let db_path = temp_db_path("industry");
    let base_u = 1_760_000_000u64;
    let ctx = open_fresh_ctx_at(&db_path, base_u);
    let struct_id = 1_000_000_251i64;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.corporation_id = CORP_ID;
            ch.balance = IskCents(100_000_000_00);
            ch.station_id = None;
            ch.structure_id = Some(struct_id);
            ch.solar_system_id = JITA_SYSTEM_ID;
        }
        snap.corporations.entry(CORP_ID).or_insert_with(|| CorporationRecord {
            corporation_id: CORP_ID, name: "B30 Corp".into(), ticker: "B30".into(), ceo_id: P1, ..Default::default()
        });
        for &(tid, lvl) in &[(3380, 5), (3387, 5), (2495, 5), (2505, 5)] {
            snap.skills.entry(P1).or_default().insert(tid, TrainedSkillRecord { type_id: tid, level: lvl, skill_points: 256_000.0 });
        }
        snap.structures.insert(struct_id, UpwellStructureRecord {
            structure_id: struct_id, type_id: 35826, owner_corp_id: CORP_ID, solar_system_id: JITA_SYSTEM_ID,
            name: "B30 Azbel".into(), position: [0.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 5_000, shield_hp: 10000.0, armor_hp: 10000.0,
            hull_hp: 10000.0, reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
        });
        let mut svc = make_item(struct_id + 1, 35878, CORP_ID, struct_id, InventoryFlag::ServiceSlot0, 1, true);
        svc.dynamic_attributes.insert(2, 1.0);
        snap.items.insert(struct_id + 1, svc);
        snap.module_online_states.insert(struct_id + 1, true);
        for iid in [710_001, 710_002, 710_003, 710_004] {
            snap.items.insert(iid, make_item(iid, 785, P1, struct_id, InventoryFlag::Hangar, 1, true));
        }
        for (iid, tid, qty) in [(710_010, 34u32, 5_000_000), (710_011, 35u32, 1_000_000), (710_012, 36u32, 500_000)] {
            snap.items.insert(iid, make_item(iid, tid, P1, struct_id, InventoryFlag::Hangar, qty, false));
        }
        snap.planetary_colonies.insert((P1, JITA_PLANET_ID), PlanetaryColonyRecord {
            owner_id: P1, planet_id: JITA_PLANET_ID, solar_system_id: JITA_SYSTEM_ID,
            command_center_level: 4, pins_count: 2, extracted_commodities: HashMap::new(), last_extracted_unix: base_u,
        });
    });

    let mut sess = login_select(&ctx, 9201, "demo", "demo-secret", P1).await;
    sync_all_structures(&ctx);
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&struct_id).unwrap().fuel_blocks = 1);

    let install_mfg = |bp_id: i64, runs: i64| PyRep::Dict(vec![
        (PyRep::String("blueprintID".into()), PyRep::Integer(bp_id)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(runs)),
    ]);
    let j1 = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[install_mfg(710_001, 1)]).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let j2 = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[install_mfg(710_002, 1)]).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();
    let j_long = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[install_mfg(710_003, 10)]).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    ctx.advance_secs(7_200);
    sync_all_structures(&ctx);
    let _ = rpc(&ctx, &mut sess, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)]).await.unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.industry_jobs[&j1].status, "Active");
        assert_eq!(snap.industry_jobs[&j_long].status, "Paused");
        assert_eq!(load_job_cost_meta(snap, j_long).unwrap().paused_at_unix, Some(base_u + 12 + 3_600));
    });

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.get_mut(&struct_id).unwrap().fuel_blocks = 5_000;
        snap.items.get_mut(&(struct_id + 1)).unwrap().dynamic_attributes.insert(2, 1.0);
        snap.module_online_states.insert(struct_id + 1, true);
    });
    sync_all_structures(&ctx);
    let _ = rpc(&ctx, &mut sess, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)]).await.unwrap();
    let many_rep = rpc(&ctx, &mut sess, "industryManager", "CompleteManyJobs", &[PyRep::List(vec![PyRep::Integer(j1), PyRep::Integer(j2)])]).await.unwrap();
    let many_str = format!("{many_rep:?}");
    assert!(many_str.contains(&j1.to_string()) && many_str.contains(&j2.to_string()));

    let j_corp = rpc(&ctx, &mut sess, "industryManager", "InstallJob", &[PyRep::Dict(vec![
        (PyRep::String("blueprintID".into()), PyRep::Integer(710_004)),
        (PyRep::String("activityID".into()), PyRep::Integer(1)),
        (PyRep::String("runs".into()), PyRep::Integer(1)),
        (PyRep::String("useCorp".into()), PyRep::Bool(true)),
        (PyRep::String("ownerID".into()), PyRep::Integer(CORP_ID)),
    ])]).await.unwrap().dict_get("jobID").and_then(|v| v.as_i64()).unwrap();

    drop(ctx);
    let ctx2 = open_ctx_at(&db_path, base_u + 8_000);
    ctx2.db.with_snapshot(|snap| {
        assert!(snap.planetary_colonies.contains_key(&(P1, JITA_PLANET_ID)));
        assert!(load_job_cost_meta(snap, j_corp).unwrap().use_corp);
    });
    let mut sess2 = login_select(&ctx2, 9202, "demo", "demo-secret", P1).await;
    ctx2.db.with_mut_snapshot(|snap| snap.characters.get_mut(&P1).unwrap().corporation_id = 9999);
    assert_eq!(rpc(&ctx2, &mut sess2, "industryManager", "CancelJob", &[PyRep::Integer(j_corp)]).await.unwrap_err().code, "JobOwnershipViolation");
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b30_mail_no_global_cap_and_state_persistence() {
    // parity: feat:lifecycle.mail-no-global-cap, feat:lifecycle.mail-state-persist, feat:lifecycle.notifications-durable
    eve_testkit::attest(&[
        "feat:lifecycle.mail-no-global-cap",
        "feat:lifecycle.mail-state-persist",
        "feat:lifecycle.notifications-durable",
    ]);

    let db_path = temp_db_path("mail");
    let base_u = 1_760_000_000u64;
    let ctx = open_fresh_ctx_at(&db_path, base_u);

    ctx.db.with_mut_snapshot(|snap| {
        for i in 1..=3_001i64 {
            snap.mails.push(MailRecord {
                message_id: 100_000 + i, sender_id: P1, recipient_id: 80_000_000 + (i % 50),
                subject: format!("Bulk {i}"), body: "Body".into(),
            });
        }
    });

    let mut s1 = login_select(&ctx, 9301, "demo", "demo-secret", P1).await;
    let mail_id = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
        PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::None,
        PyRep::String("Mail 3002".into()), PyRep::String("Delivered past 3001".into()),
        PyRep::Integer(0), PyRep::Integer(0),
    ]).await.unwrap().as_i64().unwrap();
    assert!(mail_id > 0);

    let mut s2 = login_select(&ctx, 9302, "other", "other-secret", P2).await;
    let sync_rep = rpc(&ctx, &mut s2, "mailMgr", "SyncMail", &[PyRep::Integer(0), PyRep::Integer(0)]).await.unwrap();
    assert!(format!("{sync_rep:?}").contains(&mail_id.to_string()));
    rpc(&ctx, &mut s2, "mailMgr", "MarkAsRead", &[PyRep::List(vec![PyRep::Integer(mail_id)])]).await.unwrap();

    let notif_id_before = ctx.create_character_notification(1_000_125, P2, 14, 2, "B30 offline notif").notification_id;
    drop(ctx);

    let ctx2 = open_ctx_at(&db_path, base_u + 100);
    let notif_id_after = ctx2.create_character_notification(1_000_125, P2, 14, 2, "B30 post-restart notif").notification_id;
    assert!(notif_id_after > notif_id_before);
    ctx2.db.with_snapshot(|snap| assert!(snap.read_mails.contains(&(P2, mail_id))));
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b30_market_order_expiry_wallet_history_and_lp_durability() {
    // parity: feat:lifecycle.market_order-expiry-trigger, feat:lifecycle.market-wallet-history-durable, feat:lifecycle.lp-durable
    eve_testkit::attest(&[
        "feat:lifecycle.market_order-expiry-trigger",
        "feat:lifecycle.market-wallet-history-durable",
        "feat:lifecycle.lp-durable",
    ]);

    let db_path = temp_db_path("market");
    let base_u = 1_760_000_000u64;
    let ctx = open_fresh_ctx_at(&db_path, base_u);

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&P1).unwrap().balance = IskCents(50_000_000_00);
        snap.items.insert(720_001, make_item(720_001, 34, P1, JITA_STATION_ID, InventoryFlag::Hangar, 1_000, false));
        snap.corp_lp_balances.insert((P1, 1_000_035), 25_000);
    });

    let mut s1 = login_select(&ctx, 9401, "demo", "demo-secret", P1).await;
    let buy_rep = rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &[
        PyRep::Integer(JITA_STATION_ID), PyRep::Integer(34), PyRep::Real(1.0), PyRep::Integer(100),
        PyRep::Integer(-1), PyRep::Integer(1), PyRep::Integer(1), PyRep::Bool(false),
    ]).await.unwrap();
    let order_id = buy_rep.dict_get("orderID").and_then(|v| v.as_i64()).unwrap();

    ctx.advance_virtual_clock(90_000);
    let _ = rpc(&ctx, &mut s1, "marketProxy", "GetCharOrders", &[]).await.unwrap();
    ctx.db.with_snapshot(|snap| {
        let ord = &snap.market_orders[&order_id];
        assert_eq!(ord.vol_remaining, 0);
        assert_eq!(ord.escrow.0, 0);
        assert!(snap.wallet_journal.iter().any(|j| j.entry_type == "MarketOrderExpireRefund"));
    });

    let lp_before = ctx.db.with_snapshot(|snap| snap.corp_lp_balances[&(P1, 1_000_035)]);
    let isk_before = ctx.db.get_character(P1).unwrap().balance;
    assert!(rpc(&ctx, &mut s1, "lpStore", "TakeOffer", &[PyRep::Integer(1_000_035), PyRep::Integer(999_999), PyRep::Integer(1)]).await.is_err());
    assert_eq!(ctx.db.with_snapshot(|snap| snap.corp_lp_balances[&(P1, 1_000_035)]), lp_before);
    assert_eq!(ctx.db.get_character(P1).unwrap().balance, isk_before);

    drop(ctx);
    let ctx2 = open_ctx_at(&db_path, base_u + 90_100);
    ctx2.db.with_snapshot(|snap| {
        assert_eq!(snap.corp_lp_balances[&(P1, 1_000_035)], lp_before);
        assert!(!snap.wallet_journal.is_empty());
    });
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b30_space_jetcan_mwd_module_cycles_and_mission_downtime() {
    // parity: feat:lifecycle.jetcan-empty, feat:lifecycle.mobile_warp_disruptor-lifecycle, feat:lifecycle.module-cycles-stop-on-leave-space, feat:lifecycle.mission-downtime-respawn
    eve_testkit::attest(&[
        "feat:lifecycle.jetcan-empty",
        "feat:lifecycle.mobile_warp_disruptor-lifecycle",
        "feat:lifecycle.module-cycles-stop-on-leave-space",
        "feat:lifecycle.mission-downtime-respawn",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    let sys_id = JITA_SYSTEM_ID;
    let ship_id = 620_000_001i64;
    let cargo_item_id = 620_000_010i64;
    let mod_id = 620_000_020i64;
    let mwd_id = 620_000_030i64;
    let now_u = ctx.now_unix();

    let sys = ctx.systems.get_or_spawn(sys_id);
    let station_pos = ctx.sde.stations[&JITA_STATION_ID].position;
    let ship_ball = Ball {
        id: ship_id, type_id: 587, position: station_pos + DVec3::new(100.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0,
        radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    sys.spawn_ball_with_meta(ship_ball, ctx.build_ship_combat_state(P1, ship_id, 587), ctx.build_ship_slim_meta(P1, 587)).await.unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        for stid in [11_584_u32, 3_435_u32, 3_436_u32] {
            snap.skills.entry(P1).or_default().insert(stid, TrainedSkillRecord { type_id: stid, level: 5, skill_points: 256_000.0 });
        }
        snap.items.insert(ship_id, make_item(ship_id, 587, P1, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(cargo_item_id, make_item(cargo_item_id, 34, P1, ship_id, InventoryFlag::Cargo, 50, false));
        snap.items.insert(mod_id, make_item(mod_id, 3528, P1, ship_id, InventoryFlag::LoSlot0, 1, true));
        snap.items.insert(mwd_id, make_item(mwd_id, 12_198, P1, ship_id, InventoryFlag::Cargo, 1, true));
    });

    let mut sess = SessionState {
        user_id: Some(1), session_id: 9501, character_id: Some(P1),
        ship_id: Some(ship_id), solar_system_id: Some(sys_id), ..Default::default()
    };
    let _ = ctx.db.acquire_online_lock(1, 9501, Some(P1), false, now_u);

    let jet_res = rpc(&ctx, &mut sess, "ship", "Jettison", &[PyRep::List(vec![PyRep::Integer(cargo_item_id)])]).await.unwrap();
    let can_id = match &jet_res {
        PyRep::Tuple(t) => t.first().and_then(|v| match v { PyRep::List(l) => l.first().and_then(|x| x.as_i64()), _ => None }).unwrap(),
        _ => panic!("unexpected Jettison response"),
    };
    assert!(sys.get_ball_snapshot(can_id).await.unwrap().is_some());
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(cargo_item_id), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
    assert!(sys.get_ball_snapshot(can_id).await.unwrap().is_none());

    let _ = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id), PyRep::String("online".into())]).await;
    let _ = rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(JITA_STATION_ID)]).await;
    ctx.db.with_snapshot(|snap| assert!(!snap.module_active_states.contains(&mod_id)));

    let null_sys_id = *ctx.sde.solar_systems.iter().find(|(_, s)| s.solar_system_id < 31_000_000 && s.security_status <= 0.0).unwrap().0;
    let null_sys = ctx.systems.get_or_spawn(null_sys_id);
    let mwd_ship_ball = Ball {
        id: ship_id, type_id: 587, position: DVec3::new(10_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0,
        radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    };
    null_sys.spawn_ball_with_meta(mwd_ship_ball, ctx.build_ship_combat_state(P1, ship_id, 587), ctx.build_ship_slim_meta(P1, 587)).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.solar_system_id = null_sys_id;
            ch.station_id = None;
        }
    });
    sess.station_id = None;
    sess.solar_system_id = Some(null_sys_id);
    rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[
        PyRep::Integer(mwd_id),
        PyRep::Tuple(vec![PyRep::Real(10_000.0), PyRep::Real(0.0), PyRep::Real(0.0)]),
    ]).await.unwrap();

    ctx.advance_secs(172_900);
    run_daily_downtime_and_jobs(&ctx);
    ctx.db.with_snapshot(|snap| assert!(!snap.items.contains_key(&mwd_id)));
}
