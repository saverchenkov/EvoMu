//! End-to-end integration tests for `feat:lifecycle.*` parity rows (Batch 30, Part 2).
//!
//! Covers:
//! - `feat:lifecycle.mail-no-global-cap`, `feat:lifecycle.mail-state-persist`,
//!   `feat:lifecycle.notifications-durable`
//! - `feat:lifecycle.market_order-expiry-trigger`, `feat:lifecycle.lp-durable`
//! - `feat:lifecycle.jetcan-empty`, `feat:lifecycle.mobile_warp_disruptor-lifecycle`,
//!   `feat:lifecycle.module-cycles-stop-on-leave-space`, `feat:lifecycle.mission-downtime-respawn`

#![forbid(unsafe_code)]

use crate::common;
use eve_db::{
    corp_roles, ActiveMissionRecord, CorporationRecord, InventoryFlag, InventoryItemRecord,
    IskCents, MailRecord, MarketOrderRecord, OwnerNoteRecord, TrainedSkillRecord,
    VirtualServerClock, WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::with_combat_runtime;
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
const AMARR_SYSTEM_ID: u32 = 30_002_187;
const AMARR_REGION_ID: u32 = 10_000_043;
const AMARR_STATION_ID: i64 = 60_008_494;

fn temp_db_path(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_b30_p2_{tag}_{}.sqlite", std::process::id()));
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

fn insert_expired_order(snap: &mut eve_db::WorldStateSnapshot, oid: i64, owner: i64, tid: u32, is_bid: bool, price: IskCents, qty: i32, escrow: IskCents, now_u: u64) {
    snap.market_orders.insert(oid, MarketOrderRecord {
        order_id: oid, owner_id: owner, station_id: AMARR_STATION_ID, region_id: AMARR_REGION_ID,
        type_id: tid, is_bid, price, vol_entered: qty, vol_remaining: qty, min_volume: 1, is_npc: false, escrow,
    });
    let nid = -(4_000_000_000_000_i64.saturating_add(oid.abs()));
    snap.owner_notes.insert(nid, OwnerNoteRecord {
        note_id: nid, character_id: -4, item_id: oid, label: format!("MarketOrderMeta:{oid}"),
        note: format!("{{\"range\":32767,\"duration\":1,\"issue_unix\":{}}}", now_u.saturating_sub(86_500)), timestamp_unix: now_u,
    });
}

#[tokio::test]
async fn test_b30_mail_no_global_cap_state_and_notifications_durability() {
    // parity: feat:lifecycle.mail-no-global-cap, feat:lifecycle.mail-state-persist, feat:lifecycle.notifications-durable
    eve_testkit::attest(&[
        "feat:lifecycle.mail-no-global-cap",
        "feat:lifecycle.mail-state-persist",
        "feat:lifecycle.notifications-durable",
    ]);

    let db_path = temp_db_path("mail_notif");
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
    let ml_rep = rpc(&ctx, &mut s1, "mailingListsMgr", "Create", &[PyRep::String("B30 Dispatch".into()), PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(250)]).await.unwrap();
    let list_id = ml_rep.dict_get("listID").and_then(|v| v.as_i64()).or_else(|| ml_rep.as_i64()).unwrap_or(700_001);
    rpc(&ctx, &mut s1, "mailingListsMgr", "SaveWelcomeMail", &[PyRep::Integer(list_id), PyRep::String("Welcome".into()), PyRep::String("Body".into())]).await.unwrap();

    let mail_3002 = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
        PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::None,
        PyRep::String("Mail 3002".into()), PyRep::String("Delivered past 3001".into()),
        PyRep::Integer(0), PyRep::Integer(0),
    ]).await.unwrap().as_i64().unwrap();
    let mail_trash = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
        PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::None,
        PyRep::String("Trash Mail".into()), PyRep::String("Trash".into()),
        PyRep::Integer(0), PyRep::Integer(0),
    ]).await.unwrap().as_i64().unwrap();
    let mail_del = rpc(&ctx, &mut s1, "mailMgr", "SendMail", &[
        PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::None,
        PyRep::String("Del Mail".into()), PyRep::String("Del".into()),
        PyRep::Integer(0), PyRep::Integer(0),
    ]).await.unwrap().as_i64().unwrap();

    let notif_p1 = ctx.create_character_notification(1_000_125, P1, 10, 1, "B30 P1 processed notif").notification_id;
    rpc(&ctx, &mut s1, "notificationMgr", "MarkAsProcessed", &[PyRep::List(vec![PyRep::Integer(notif_p1)])]).await.unwrap();
    let notif_offline = ctx.create_character_notification(1_000_125, P2, 14, 2, "B30 offline notif").notification_id;

    let mut s2 = login_select(&ctx, 9302, "other", "other-secret", P2).await;
    let sync_rep = rpc(&ctx, &mut s2, "mailMgr", "SyncMail", &[PyRep::Integer(0), PyRep::Integer(0)]).await.unwrap();
    assert!(format!("{sync_rep:?}").contains(&mail_3002.to_string()));
    rpc(&ctx, &mut s2, "mailingListsMgr", "Join", &[PyRep::String("B30 Dispatch".into())]).await.unwrap();
    rpc(&ctx, &mut s1, "mailingListsMgr", "SetMembersOperator", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(P2)])]).await.unwrap();
    let lbl_id = rpc(&ctx, &mut s2, "mailMgr", "CreateLabel", &[PyRep::String("B30 Intel".into()), PyRep::Integer(4)]).await.unwrap().as_i64().unwrap_or(64);
    rpc(&ctx, &mut s2, "mailMgr", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(mail_3002)]), PyRep::List(vec![PyRep::Integer(4), PyRep::Integer(lbl_id)])]).await.unwrap();
    rpc(&ctx, &mut s2, "mailMgr", "MarkAsRead", &[PyRep::List(vec![PyRep::Integer(mail_3002)])]).await.unwrap();
    rpc(&ctx, &mut s2, "mailMgr", "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(mail_trash)])]).await.unwrap();
    rpc(&ctx, &mut s2, "mailMgr", "DeleteMail", &[PyRep::List(vec![PyRep::Integer(mail_del)])]).await.unwrap();
    rpc(&ctx, &mut s1, "calendarMgr", "CreatePersonalEvent", &[
        PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(60),
        PyRep::String("B30 Formup".into()), PyRep::String("Ops".into()), PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(P2)]),
    ]).await.unwrap();

    let (read_b, trash_b, del_b, lbl_b, ml_b, cal_b) = ctx.db.with_snapshot(|s| (
        s.read_mails.clone(), s.trashed_mails.clone(), s.deleted_mails.clone(),
        s.mail_assigned_labels.clone(), s.mailing_lists.clone(), s.calendar_events.clone(),
    ));
    drop(ctx);

    let ctx2 = open_ctx_at(&db_path, base_u + 100);
    let mut s1_re = login_select(&ctx2, 9303, "demo", "demo-secret", P1).await;
    let p1_notifs = rpc(&ctx2, &mut s1_re, "notificationMgr", "GetAllNotifications", &[]).await.unwrap();
    assert!(p1_notifs.crowset_rows().unwrap().iter().any(|r| r.dbrow_get("notificationID") == Some(&PyRep::Integer(notif_p1)) && r.dbrow_get("processed") == Some(&PyRep::Bool(true))));

    let mut s2_re = login_select(&ctx2, 9304, "other", "other-secret", P2).await;
    let p2_login_notifs = ctx2.drain_notifications_for_char(P2);
    assert!(p2_login_notifs.iter().any(|n| n.event == "OnNotificationReceived" && n.args.first() == Some(&PyRep::Integer(notif_offline))));
    let p2_unproc = rpc(&ctx2, &mut s2_re, "notificationMgr", "GetUnprocessed", &[]).await.unwrap();
    assert!(p2_unproc.crowset_rows().unwrap().iter().any(|r| r.dbrow_get("notificationID") == Some(&PyRep::Integer(notif_offline))));
    let notif_after = ctx2.create_character_notification(1_000_125, P2, 14, 2, "B30 post-restart").notification_id;
    assert!(notif_after > notif_offline);

    ctx2.db.with_snapshot(|s| {
        assert_eq!(s.read_mails, read_b);
        assert_eq!(s.trashed_mails, trash_b);
        assert_eq!(s.deleted_mails, del_b);
        assert_eq!(s.mail_assigned_labels, lbl_b);
        assert_eq!(s.mailing_lists.len(), ml_b.len());
        assert_eq!(s.calendar_events.len(), cal_b.len());
    });
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b30_market_order_expiry_and_lp_durability() {
    // parity: feat:lifecycle.market_order-expiry-trigger, feat:lifecycle.lp-durable
    eve_testkit::attest(&[
        "feat:lifecycle.market_order-expiry-trigger",
        "feat:lifecycle.lp-durable",
    ]);

    let db_path = temp_db_path("market_lp");
    let base_u = 1_760_000_000u64;
    let ctx = open_fresh_ctx_at(&db_path, base_u);
    let (buy_oid, sell_oid, p2_bid, p2_ask) = (910_001i64, 910_002i64, 910_004i64, 910_005i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.retain(|_, o| o.region_id != AMARR_REGION_ID);
        let roles = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_ACCOUNTANT | corp_roles::ROLE_TRADER;
        let mut corp = CorporationRecord {
            corporation_id: CORP_ID, name: "B30 Market Corp".into(), ticker: "B30M".into(), ceo_id: P1,
            hq_station_id: Some(AMARR_STATION_ID), members: [P1].into_iter().collect(),
            member_roles: HashMap::from([(P1, roles)]), ..Default::default()
        };
        corp.ensure_wallet_divisions();
        snap.corporations.insert(CORP_ID, corp);
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.corporation_id = CORP_ID;
            ch.balance = IskCents(10_000_000_00);
            ch.station_id = Some(AMARR_STATION_ID);
            ch.solar_system_id = AMARR_SYSTEM_ID;
            ch.region_id = AMARR_REGION_ID;
            ch.loyalty_points.insert(1_000_035, 15_000);
            ch.loyalty_points.insert(1_000_036, 500);
            ch.loyalty_points.insert(1_000_125, 5_000);
        }
        snap.items.insert(720_001, make_item(720_001, 36, P1, AMARR_STATION_ID, InventoryFlag::Hangar, 100, false));
        snap.items.insert(720_002, make_item(720_002, 34, P1, AMARR_STATION_ID, InventoryFlag::Hangar, 50, false));
        insert_expired_order(snap, buy_oid, P1, 35, true, IskCents(100_00), 100, IskCents(10_000_00), base_u);
        insert_expired_order(snap, sell_oid, P1, 35, false, IskCents(200_00), 50, IskCents(0), base_u);
        insert_expired_order(snap, p2_bid, P2, 36, true, IskCents(500_00), 40, IskCents(20_000_00), base_u);
        insert_expired_order(snap, p2_ask, P2, 36, false, IskCents(50_00), 40, IskCents(0), base_u);
    });

    let mut s1 = login_select(&ctx, 9401, "demo", "demo-secret", P1).await;
    // All 5 read RPCs exclude expired orders
    for m in ["GetRegionBest", "GetStationAsks", "GetCharOrders", "GetCorporationOrders"] {
        let rep = rpc(&ctx, &mut s1, "marketProxy", m, &[]).await.unwrap();
        assert!(!format!("{rep:?}").contains(&p2_ask.to_string()));
    }
    let ord_rep = rpc(&ctx, &mut s1, "marketProxy", "GetOrders", &[PyRep::Integer(36)]).await.unwrap();
    assert!(!format!("{ord_rep:?}").contains(&p2_bid.to_string()));

    // All 3 match RPCs never match expired orders
    let bal_before = ctx.db.get_character(P1).unwrap().balance.0;
    let buy_m = rpc(&ctx, &mut s1, "marketProxy", "PlaceBuyOrder", &[PyRep::Integer(AMARR_STATION_ID), PyRep::Integer(36), PyRep::Real(100.0), PyRep::Integer(10), PyRep::Integer(32767), PyRep::Integer(1), PyRep::Integer(0), PyRep::Bool(false)]).await;
    assert!(buy_m.is_err() || buy_m.as_ref().ok().and_then(|r| r.dict_get("filledImmediately")).and_then(|v| v.as_bool()).unwrap_or(false) == false);
    let sell_err = rpc(&ctx, &mut s1, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![PyRep::Dict(vec![
        (PyRep::String("itemID".into()), PyRep::Integer(720_001)), (PyRep::String("typeID".into()), PyRep::Integer(36)),
        (PyRep::String("price".into()), PyRep::Real(100.0)), (PyRep::String("quantity".into()), PyRep::Integer(10)),
        (PyRep::String("stationID".into()), PyRep::Integer(AMARR_STATION_ID)), (PyRep::String("duration".into()), PyRep::Integer(0)),
    ])]), PyRep::Bool(false)]).await.unwrap_err();
    assert_eq!(sell_err.code, "ImmediateOrderUnfilled");
    let _ = rpc(&ctx, &mut s1, "marketProxy", "BuyMultipleItems", &[PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(36), PyRep::Integer(10), PyRep::Real(100.0)])])]).await;
    assert_eq!(ctx.db.get_character(P1).unwrap().balance.0, bal_before);

    // Refunded exactly once + OnMarketItemReceived carries stationID
    assert_eq!(bal_before, 10_000_000_00 + 10_000_00);
    let _ = rpc(&ctx, &mut s1, "marketProxy", "GetCharOrders", &[]).await.unwrap();
    assert_eq!(ctx.db.get_character(P1).unwrap().balance.0, bal_before);
    let notifs = ctx.drain_notifications_for_char(P1);
    let item_recv = notifs.iter().find(|n| n.event == "OnMarketItemReceived").unwrap();
    assert_eq!(item_recv.args.last(), Some(&PyRep::Integer(AMARR_STATION_ID)));

    // LP durability + logs + atomic failed purchase
    rpc(&ctx, &mut s1, "LPSvc", "TransferLPFromMyWalletToOtherCorp", &[PyRep::Integer(CORP_ID), PyRep::Integer(1_000_035), PyRep::Integer(6_000)]).await.unwrap();
    rpc(&ctx, &mut s1, "LPSvc", "ExchangeConcordLP", &[PyRep::Integer(1_000_036), PyRep::Integer(1_000)]).await.unwrap();
    let corp_lp_before = rpc(&ctx, &mut s1, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[]).await.unwrap();
    let logs_before = rpc(&ctx, &mut s1, "rewardMgr", "GetRewardLPLogs", &[]).await.unwrap();
    drop(ctx);

    let ctx2 = open_ctx_at(&db_path, base_u + 100);
    common::seed_test_lp_store_offers(&ctx2);
    let mut s1_re = login_select(&ctx2, 9402, "demo", "demo-secret", P1).await;
    let cn_station = ctx2.sde.stations.values().find(|s| s.owner_corporation_id == 1_000_035 && eve_server::services::station_structure::npc_station_offers_service(&ctx2.sde, s.station_id, 25)).unwrap().station_id;
    ctx2.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&P1) { ch.station_id = Some(cn_station); } });
    s1_re.station_id = Some(cn_station);
    assert_eq!(rpc(&ctx2, &mut s1_re, "LPSvc", "GetAllMyCorporationWalletLPBalances", &[]).await.unwrap(), corp_lp_before);
    assert_eq!(rpc(&ctx2, &mut s1_re, "rewardMgr", "GetRewardLPLogs", &[]).await.unwrap(), logs_before);

    let snap_before = ctx2.db.with_snapshot(|s| (s.characters[&P1].loyalty_points.clone(), s.corp_lp_balances.clone(), s.characters[&P1].balance, s.items[&720_002].quantity));
    assert_eq!(rpc(&ctx2, &mut s1_re, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(1_000_035), PyRep::Integer(1004), PyRep::Integer(1)]).await.unwrap_err().code, "MissingRequiredItems");
    assert_eq!(rpc(&ctx2, &mut s1_re, "LPStoreMgr", "TakeOfferForCharacter", &[PyRep::Integer(1_000_035), PyRep::Integer(1003), PyRep::Integer(1)]).await.unwrap_err().code, "InsufficientLoyaltyPoints");
    let snap_after = ctx2.db.with_snapshot(|s| (s.characters[&P1].loyalty_points.clone(), s.corp_lp_balances.clone(), s.characters[&P1].balance, s.items[&720_002].quantity));
    assert_eq!(snap_after, snap_before);
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

    let db_path = temp_db_path("space");
    let day_start = 1_800_000_000u64 - (1_800_000_000u64 % 86_400);
    let base_u = day_start + 11 * 3_600 - 5;
    let ctx = open_fresh_ctx_at(&db_path, base_u);
    let sys_id = JITA_SYSTEM_ID;
    let (ship_id, cargo_a, cargo_b, mod_id, mwd_id, pre_npc_id) = (620_000_001i64, 620_000_010i64, 620_000_011i64, 620_000_020i64, 620_000_030i64, 888_111_001i64);

    let sys = ctx.systems.get_or_spawn(sys_id);
    let station_pos = ctx.sde.stations[&JITA_STATION_ID].position;
    let ship_ball = Ball { id: ship_id, type_id: 587, position: station_pos + DVec3::new(100.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop };
    sys.spawn_ball_with_meta(ship_ball.clone(), ctx.build_ship_combat_state(P1, ship_id, 587), ctx.build_ship_slim_meta(P1, 587)).await.unwrap();
    sys.spawn_ball_with_combat(Ball { id: pre_npc_id, type_id: 10_019, position: DVec3::new(30_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }, CombatState::default()).await.unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&P1) {
            ch.solar_system_id = sys_id; ch.station_id = None; ch.structure_id = None; ch.active_ship_id = ship_id;
        }
        for stid in [11_584_u32, 3_435_u32, 3_436_u32] {
            snap.skills.entry(P1).or_default().insert(stid, TrainedSkillRecord { type_id: stid, level: 5, skill_points: 256_000.0 });
        }
        snap.items.insert(ship_id, make_item(ship_id, 587, P1, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(cargo_a, make_item(cargo_a, 34, P1, ship_id, InventoryFlag::Cargo, 25, false));
        snap.items.insert(cargo_b, make_item(cargo_b, 34, P1, ship_id, InventoryFlag::Cargo, 25, false));
        snap.items.insert(mod_id, make_item(mod_id, 3528, P1, ship_id, InventoryFlag::LoSlot0, 1, true));
        snap.items.insert(mwd_id, make_item(mwd_id, 12_198, P1, ship_id, InventoryFlag::Cargo, 1, true));
        snap.active_missions.insert((P1, 3_019_001), ActiveMissionRecord {
            mission_id: 500_001, character_id: P1, agent_id: 3_019_001, solar_system_id: sys_id,
            state: "Accepted".into(), current_room_id: 1, npcs_remaining: 1, spawned_npc_ball_ids: vec![pre_npc_id], bookmark_id: None,
        });
    });

    let mut sess = SessionState { user_id: Some(1), session_id: 9501, character_id: Some(P1), ship_id: Some(ship_id), solar_system_id: Some(sys_id), ..Default::default() };
    let _ = ctx.db.acquire_online_lock(1, 9501, Some(P1), false, base_u);

    // 1. `jetcan-empty`: last item removed deletes container item, ball, and broadcasts RemoveBall
    let jet_res = rpc(&ctx, &mut sess, "ship", "Jettison", &[PyRep::List(vec![PyRep::Integer(cargo_a), PyRep::Integer(cargo_b)])]).await.unwrap();
    let can_id = match &jet_res { PyRep::Tuple(t) => t.first().and_then(|v| match v { PyRep::List(l) => l.first().and_then(|x| x.as_i64()), _ => None }).unwrap(), _ => panic!() };
    let _ = sys.step_manual(1.0).await;
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(cargo_a), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
    assert!(sys.get_ball_snapshot(can_id).await.unwrap().is_some());
    ctx.drain_notifications_for_char(P1);
    rpc(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(cargo_b), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.unwrap();
    ctx.db.with_snapshot(|s| assert!(!s.items.contains_key(&can_id)));
    assert!(sys.get_ball_snapshot(can_id).await.unwrap().is_none());
    assert!(ctx.drain_notifications_for_char(P1).iter().any(|n| n.event == "DoDestinyUpdate" && format!("{:?}", n.args).contains("RemoveBall")));

    // 2. `module-cycles-stop-on-leave-space`: Activate repeater, verify active cycle, Dock, verify cycle removed & deactivation emitted
    let act_rep = rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(mod_id), PyRep::String("repair".into())]).await.unwrap();
    assert_eq!(act_rep.dict_get("active"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|s| assert!(s.module_active_states.contains(&mod_id)));
    let _ = sys.step_manual(1.0).await.unwrap();
    assert_eq!(sys.update_module_overload(ship_id, mod_id, false).await, Ok(true));
    ctx.advance_secs(12);
    ctx.drain_notifications_for_char(P1);
    rpc(&ctx, &mut sess, "stationSvc", "Dock", &[PyRep::Integer(JITA_STATION_ID)]).await.unwrap();
    assert_eq!(sess.station_id, Some(JITA_STATION_ID));
    ctx.db.with_snapshot(|s| assert!(!s.module_active_states.contains(&mod_id)));
    assert_eq!(sys.update_module_overload(ship_id, mod_id, false).await, Ok(false));
    assert!(ctx.drain_notifications_for_char(P1).iter().any(|n| n.event == "OnGodmaShipEffect"));

    // 3. `mission-downtime-respawn`: Downtime resets uncompleted mission dungeon and respawns wave on warp-in
    ctx.advance_secs(10);
    run_daily_downtime_and_jobs(&ctx);
    assert!(sys.get_ball_snapshot(pre_npc_id).await.unwrap().is_none());
    ctx.db.with_snapshot(|s| assert_eq!(s.active_missions[&(P1, 3_019_001)].npcs_remaining, 4));
    ctx.db.with_mut_snapshot(|s| { if let Some(ch) = s.characters.get_mut(&P1) { ch.station_id = None; ch.solar_system_id = sys_id; } });
    sess.station_id = None;
    sess.solar_system_id = Some(sys_id);
    sys.spawn_ball_with_meta(ship_ball.clone(), ctx.build_ship_combat_state(P1, ship_id, 587), ctx.build_ship_slim_meta(P1, 587)).await.unwrap();
    let beyonce_oid = ctx.bind_object(&sess, "beyonce", sys_id as i64).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdWarpToStuff", &[PyRep::String("mission".into()), PyRep::Integer(500_001)], &HashMap::new(), None).await.unwrap();
    let respawned_ids = ctx.db.with_snapshot(|s| s.active_missions[&(P1, 3_019_001)].spawned_npc_ball_ids.clone());
    assert_eq!(respawned_ids.len(), 4);
    assert!(with_combat_runtime(&ctx, |rt| respawned_ids.iter().all(|id| rt.npc_entities.contains_key(id))));

    // 4. `mobile_warp_disruptor-lifecycle`: Anchor completion from anchor_start_unix, bubble re-created after restart, removed after 2 days
    let null_sys_id = 30_004_608u32;
    let null_sys = ctx.systems.get_or_spawn(null_sys_id);
    null_sys.spawn_ball_with_meta(Ball { id: ship_id, type_id: 587, position: DVec3::new(50_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 1.0, mass: 1_000_000.0, radius: 50.0, speed_fraction: 0.0, mode: MovementMode::Stop }, ctx.build_ship_combat_state(P1, ship_id, 587), ctx.build_ship_slim_meta(P1, 587)).await.unwrap();
    ctx.db.with_mut_snapshot(|s| { s.characters.get_mut(&P1).unwrap().solar_system_id = null_sys_id; });
    sess.solar_system_id = Some(null_sys_id);
    rpc(&ctx, &mut sess, "posMgr", "AnchorStructure", &[PyRep::Integer(mwd_id), PyRep::Tuple(vec![PyRep::Real(50_000.0), PyRep::Real(0.0), PyRep::Real(0.0)])]).await.unwrap();
    ctx.advance_secs(120);
    assert_eq!(null_sys.is_ball_scrambled(ship_id).await.unwrap(), true);
    let mwd_u = ctx.now_unix();
    drop(ctx);

    let ctx2 = open_ctx_at(&db_path, mwd_u + 60);
    let null_sys2 = ctx2.systems.get_or_spawn(null_sys_id);
    assert!(null_sys2.get_ball_snapshot(mwd_id).await.unwrap().is_some());
    ctx2.advance_secs(172_900);
    run_daily_downtime_and_jobs(&ctx2);
    ctx2.db.with_snapshot(|s| assert!(!s.items.contains_key(&mwd_id)));
    let _ = std::fs::remove_file(&db_path);
}
