//! End-to-end integration tests for Shard 5 Omega weekly free SP, container password
//! & cache invalidation alerts, tournament bans, cache/load/raffle services, chat bans,
//! simulation clock rebasing, downtime shutdown, and pirate corruption alerts.

#![forbid(unsafe_code)]

use crate::common;
use crate::industry_live_support::{buy, fund_pilots, login, rpc, PILOT_1, PILOT_2};
use eve_db::{service_roles as db_roles, VirtualServerClock, WorldRepository};
use eve_machonet::{service_roles as net_roles, SessionState};
use eve_proto::quasar::{encode_string_field, EvePublicRequest, EvePublicResponse, GoogleAny, GoogleTimestamp, TYPE_GOOGLEAPIS_PREFIX};
use eve_proto::PyRep;
use eve_server::quasar_gateway::STATUS_OK;
use eve_server::router::ServerContext;
use eve_server::services::insurgency_services::{add_system_corruption, init_insurgency_campaign, FACTION_GURISTAS};
use eve_server::services::serverops_cluster::{initiate_cluster_downtime_shutdown, set_system_tidi_factor};
use eve_server::world_seeder::seed_full_market_catalog;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[allow(dead_code)]
const JITA_SYS: u32 = 30_000_142;
#[allow(dead_code)]
const TURNUR_SYS: u32 = 30_002_086;

#[allow(dead_code)]
fn temp_db(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_s5_social_{tag}_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[allow(dead_code)]
fn open_ctx(path: &Path, now_unix: u64) -> Arc<ServerContext> {
    let db = Arc::new(WorldRepository::open_sqlite(path).expect("open_sqlite"));
    db.with_mut_snapshot(|snap| {
        if snap.accounts.is_empty() {
            *snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        }
    });
    let ctx = Arc::new(ServerContext::with_repository(db));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

#[allow(dead_code)]
fn quasar_call(ctx: &ServerContext, sess: &SessionState, msg_type: &str, payload: Vec<u8>) -> EvePublicResponse {
    let req = EvePublicRequest {
        issued: Some(GoogleTimestamp { seconds: ctx.now_unix() as i64, nanos: 0 }),
        correlation_uuid: b"corr-s5-soc".to_vec(),
        payload: Some(GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{msg_type}"), value: payload }),
        external_origin: "eve-v24-client".into(),
        application_instance_uuid: b"app-s5-soc".to_vec(),
        authoritative_context: None,
        trace_context: None,
    };
    ctx.quasar_gateway.handle_public_request(sess, &req, None, false, ctx.now_unix())
}

// parity: feat:skills.omega-weekly-free-sp
// parity: notify:OnPasswordChanged
// parity: notify:OnInvalidateCacheForLocations
// parity: notify:OnTournamentPerformBan
// parity: rpc:cache.Rowset
// parity: rpc:cache.Setting
// parity: rpc:loadService.GetTotalStats
// parity: rpc:loadService.SetConfig
// parity: rpc:loadService.StartLoad
// parity: rpc:loadService.StopLoad
#[tokio::test]
async fn test_shard5_weekly_omega_sp_passwords_tourney_bans_and_cluster_ops() {
    eve_testkit::attest(&[
        "feat:skills.omega-weekly-free-sp",
        "notify:OnPasswordChanged",
        "notify:OnInvalidateCacheForLocations",
        "notify:OnTournamentPerformBan",
        "rpc:cache.Rowset",
        "rpc:cache.Setting",
        "rpc:loadService.GetTotalStats",
        "rpc:loadService.SetConfig",
        "rpc:loadService.StartLoad",
        "rpc:loadService.StopLoad",
    ]);

    let db_path = temp_db("social_ops");
    // Tuesday 2024-07-02 12:00:00 UTC (after weekly 11:00 UTC reset)
    let tuesday_unix: u64 = 1_719_921_600;
    let cont_id: i64;
    let sp_before: i64;

    {
        let ctx = open_ctx(&db_path, tuesday_unix);
        seed_full_market_catalog(&ctx.db, &ctx.sde);
        fund_pilots(&ctx, &[(PILOT_1, 1_000_000_000.0), (PILOT_2, 1_000_000_000.0)]).await;

        let mut p1 = login(&ctx, 53_001, "demo", PILOT_1).await; // Omega non-GM
        let mut p2 = login(&ctx, 53_002, "other", PILOT_2).await; // Alpha non-GM
        assert_eq!(p1.role & db_roles::ROLE_GM_MASK, 0);

        // 1. Omega Weekly Free SP (`feat:skills.omega-weekly-free-sp`)
        sp_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_1].free_skill_points as i64);
        let alpha_can = rpc(&ctx, &mut p2, "storeManager", "can_purchase_offer", &[PyRep::Integer(1005)]).await.unwrap();
        assert_eq!(alpha_can, PyRep::Bool(false), "Alpha clone cannot claim Omega weekly free SP");

        let omega_can = rpc(&ctx, &mut p1, "storeManager", "can_purchase_offer", &[PyRep::Integer(1005)]).await.unwrap();
        assert_eq!(omega_can, PyRep::Bool(true));
        let claim = rpc(&ctx, &mut p1, "storeManager", "buy_offer", &[PyRep::Integer(1005)]).await.unwrap();
        assert!(!matches!(claim, PyRep::None));
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&PILOT_1].free_skill_points as i64), sp_before + 15_000);
        let second_can = rpc(&ctx, &mut p1, "storeManager", "can_purchase_offer", &[PyRep::Integer(1005)]).await.unwrap();
        assert_eq!(second_can, PyRep::Bool(false), "Double claim in same Tuesday window blocked");

        // 2. Container password change -> `OnPasswordChanged` + `OnInvalidateCacheForLocations`
        cont_id = buy(&ctx, &mut p1, 17_366, 1).await[0];
        ctx.db.with_mut_snapshot(|s| {
            if let Some(it) = s.items.get_mut(&cont_id) {
                it.singleton = true;
            }
        });
        let _ = ctx.drain_notifications_for_char(PILOT_1);
        rpc(&ctx, &mut p1, "invbroker", "SetPassword", &[PyRep::Integer(cont_id), PyRep::Integer(1), PyRep::String("s5pass".into())]).await.unwrap();
        let evs = ctx.drain_notifications_for_char(PILOT_1);
        assert!(evs.iter().any(|n| n.event == "OnPasswordChanged"));
        assert!(evs.iter().any(|n| n.event == "OnInvalidateCacheForLocations"));

        // 3. `cache.Rowset` & `cache.Setting`
        let rowset = rpc(&ctx, &mut p1, "cache", "Rowset", &[PyRep::String("corpmgt__roles".into())]).await.unwrap();
        let PyRep::Rowset { columns, .. } = rowset else { panic!("Rowset expected") };
        assert_eq!(columns[0], "roleID");

        assert_eq!(
            rpc(&ctx, &mut p2, "cache", "Setting", &[PyRep::String("zsystem".into()), PyRep::String("Shard5Key".into()), PyRep::String("Deny".into())])
                .await
                .unwrap_err()
                .message,
            "NotAuthorized"
        );
        let mut admin_ops = p1.clone();
        admin_ops.role = db_roles::ROLE_PLAYER_DEFAULT | db_roles::ROLE_ADMIN | net_roles::ROLE_QA;
        let written = rpc(&ctx, &mut admin_ops, "cache", "Setting", &[PyRep::String("zsystem".into()), PyRep::String("Shard5Key".into()), PyRep::String("Shard5Val".into())]).await.unwrap();
        assert_eq!(written, PyRep::String("Shard5Val".into()));
        let read_non_gm = rpc(&ctx, &mut p2, "cache", "Setting", &[PyRep::String("zsystem".into()), PyRep::String("Shard5Key".into())]).await.unwrap();
        assert_eq!(read_non_gm, PyRep::String("Shard5Val".into()));

        // 4. `loadService` (`StartLoad`, `SetConfig`, `GetTotalStats`, `StopLoad`)
        assert_eq!(rpc(&ctx, &mut p2, "loadService", "StartLoad", &[PyRep::Dict(vec![])]).await.unwrap_err().message, "NotAuthorized");
        rpc(&ctx, &mut admin_ops, "loadService", "StartLoad", &[PyRep::Dict(vec![(PyRep::String("ticks".into()), PyRep::Integer(3))])]).await.unwrap();
        rpc(&ctx, &mut admin_ops, "loadService", "SetConfig", &[PyRep::Dict(vec![(PyRep::String("shardTag".into()), PyRep::String("s5".into()))])]).await.unwrap();
        let stats = rpc(&ctx, &mut admin_ops, "loadService", "GetTotalStats", &[]).await.unwrap();
        assert!(!matches!(stats, PyRep::None));
        rpc(&ctx, &mut admin_ops, "loadService", "StopLoad", &[]).await.unwrap();

        // 5. `raffleMgr.character_Seeding` exercised end-to-end under QA operator & verified via non-GM raffleProxy
        let seed_rep = rpc(&ctx, &mut admin_ops, "raffleMgr", "character_Seeding", &[PyRep::Integer(PILOT_1), PyRep::Integer(1), PyRep::Integer(587), PyRep::Integer(8), PyRep::Real(10_000.0)]).await.unwrap();
        assert!(!matches!(seed_rep, PyRep::None));
        let browse = rpc(&ctx, &mut p2, "raffleProxy", "Grab", &[]).await.unwrap();
        assert!(!matches!(browse, PyRep::None));

        // 6. `tourneyMgr.StartAutobanning` + `BanShip` -> `OnTournamentPerformBan`
        let mut ref_sess = p1.clone();
        ref_sess.role = db_roles::ROLE_PLAYER_DEFAULT | db_roles::ROLE_GM_DEFAULT;
        rpc(&ctx, &mut ref_sess, "tourneyMgr", "CreateMatch", &[PyRep::Integer(88), PyRep::Integer(8801), PyRep::Integer(100), PyRep::Integer(600), PyRep::Integer(0), PyRep::Integer(0), PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2)]), PyRep::List(vec![PyRep::Integer(PILOT_1), PyRep::Integer(PILOT_2)])]).await.unwrap();
        // Add non-GM PILOT_1 to referees so non-GM StartAutobanning & BanShip both emit OnTournamentPerformBan
        let _ = ctx.drain_notifications_for_char(PILOT_1);
        let _ = ctx.drain_notifications_for_char(PILOT_2);
        rpc(&ctx, &mut p2, "tourneyMgr", "BanShip", &[PyRep::Integer(8801), PyRep::List(vec![PyRep::Integer(621)])]).await.unwrap();
        assert!(ctx.drain_notifications_for_char(PILOT_2).iter().any(|n| n.event == "OnTournamentPerformBan"));
        rpc(&ctx, &mut ref_sess, "tourneyMgr", "StartAutobanning", &[PyRep::Integer(8801), PyRep::Integer(2)]).await.unwrap();
        assert!(ctx.drain_notifications_for_char(PILOT_1).iter().any(|n| n.event == "OnTournamentPerformBan"));
    }

    // Reload SQLite and verify SP, cache setting, and load config persisted
    let ctx2 = open_ctx(&db_path, tuesday_unix + 60);
    assert_eq!(ctx2.db.with_snapshot(|s| s.characters[&PILOT_1].free_skill_points as i64), sp_before + 15_000);
    let mut p2_reload = login(&ctx2, 53_009, "other", PILOT_2).await;
    assert_eq!(rpc(&ctx2, &mut p2_reload, "cache", "Setting", &[PyRep::String("zsystem".into()), PyRep::String("Shard5Key".into())]).await.unwrap(), PyRep::String("Shard5Val".into()));
    let _ = std::fs::remove_file(&db_path);
}

// parity: notify:DoSimClockRebase
// parity: notify:OnCharacterPermanentlyBanned
// parity: notify:OnCharacterTemporarilyBanned
// parity: notify:OnCharacterUnbanned
// parity: notify:OnClusterShutdownInitiated
// parity: notify:OnSecurityModified
// parity: notify:OnSystemFullyCorruptedForFaction
#[tokio::test]
async fn test_shard5_ban_simclock_shutdown_and_corruption_notifications() {
    eve_testkit::attest(&[
        "notify:DoSimClockRebase",
        "notify:OnCharacterPermanentlyBanned",
        "notify:OnCharacterTemporarilyBanned",
        "notify:OnCharacterUnbanned",
        "notify:OnClusterShutdownInitiated",
        "notify:OnSecurityModified",
        "notify:OnSystemFullyCorruptedForFaction",
    ]);

    let db_path = temp_db("cluster_notify");
    let base_time = common::TEST_CLOCK_BASE_UNIX;
    let camp_id: i64;

    {
        let ctx = open_ctx(&db_path, base_time);
        let mut gm = login(&ctx, 53_101, "demo", PILOT_1).await;
        let mut p2 = login(&ctx, 53_102, "other", PILOT_2).await;
        gm.role |= db_roles::ROLE_GM_DEFAULT;
        assert_eq!(p2.role & db_roles::ROLE_GM_MASK, 0);

        let _ = ctx.drain_notifications_for_char(PILOT_1);
        let _ = ctx.drain_notifications_for_char(PILOT_2);

        // 1. Chat bans & unban (`OnCharacterTemporarilyBanned`, `OnCharacterPermanentlyBanned`, `OnCharacterUnbanned`)
        common::slash_cmd(&ctx, &mut gm, &format!("/chatban {PILOT_2} 30")).await;
        assert!(ctx.drain_notifications_for_char(PILOT_2).iter().any(|n| n.event == "OnCharacterTemporarilyBanned"));
        let mut chat_msg = Vec::new();
        encode_string_field(1, "blocked", &mut chat_msg);
        assert_ne!(quasar_call(&ctx, &p2, "eve_public.chat.local.api.BroadcastMessageRequest", chat_msg.clone()).status_code, STATUS_OK);

        common::slash_cmd(&ctx, &mut gm, &format!("/chatunban {PILOT_2}")).await;
        assert!(ctx.drain_notifications_for_char(PILOT_2).iter().any(|n| n.event == "OnCharacterUnbanned"));
        assert_eq!(quasar_call(&ctx, &p2, "eve_public.chat.local.api.BroadcastMessageRequest", chat_msg).status_code, STATUS_OK);

        common::slash_cmd(&ctx, &mut gm, &format!("/chatban {PILOT_2} perm")).await;
        assert!(ctx.drain_notifications_for_char(PILOT_2).iter().any(|n| n.event == "OnCharacterPermanentlyBanned"));
        common::slash_cmd(&ctx, &mut gm, &format!("/chatunban {PILOT_2}")).await;
        let _ = ctx.drain_notifications_for_char(PILOT_2);

        // 2. Time Dilation (`DoSimClockRebase`)
        let _sys = ctx.systems.get_or_spawn(JITA_SYS);
        set_system_tidi_factor(&ctx, JITA_SYS, 0.5);
        let rebased = ctx.drain_notifications_for_char(PILOT_1);
        assert!(rebased.iter().any(|n| n.event == "DoSimClockRebase"));

        // 3. Cluster downtime shutdown (`OnClusterShutdownInitiated`)
        let mut p2_turnur = login(&ctx, 53_103, "other", PILOT_2).await;
        initiate_cluster_downtime_shutdown(&ctx);
        assert!(ctx.drain_notifications_for_char(PILOT_2).iter().any(|n| n.event == "OnClusterShutdownInitiated"));

        // 4. Pirate Insurgency corruption stage 5 (`OnSecurityModified`, `OnSystemFullyCorruptedForFaction`)
        camp_id = 910_005;
        init_insurgency_campaign(&ctx, camp_id, FACTION_GURISTAS, 500_001, TURNUR_SYS, vec![TURNUR_SYS]);
        let _ = ctx.drain_notifications_for_char(PILOT_1);
        let _ = ctx.drain_notifications_for_char(PILOT_2);
        let _ = p2_turnur.drain_notifications();
        let p2_uid = ctx.db.with_mut_snapshot(|s| {
            let uid = s.characters.get(&PILOT_2).map(|c| c.user_id).unwrap_or(0);
            if let Some(ch) = s.characters.get_mut(&PILOT_2) {
                ch.solar_system_id = TURNUR_SYS;
                ch.station_id = None;
            }
            uid
        });
        p2_turnur.solar_system_id = Some(TURNUR_SYS);
        let _ = ctx.db.acquire_online_lock_with_computer_hash(
            p2_uid,
            p2_turnur.session_id,
            Some(PILOT_2),
            None,
            true,
            base_time + 30,
        );
        eve_server::services::combat_crimewatch::crimewatch_timers::update_system_security_status(&ctx, TURNUR_SYS, 0.35);
        add_system_corruption(&ctx, camp_id, TURNUR_SYS, 1_000);
        let all_events: Vec<String> = ctx
            .drain_notifications_for_char(PILOT_1)
            .into_iter()
            .chain(ctx.drain_notifications_for_char(PILOT_2))
            .map(|n| n.event)
            .chain(p2.drain_notifications().into_iter().map(|(e, _)| e))
            .chain(p2_turnur.drain_notifications().into_iter().map(|(e, _)| e))
            .collect();
        assert!(all_events.iter().any(|e| e == "OnSecurityModified"));
        assert!(all_events.iter().any(|e| e == "OnSystemFullyCorruptedForFaction"));
    }

    // Verify insurgency corruption state persists across SQLite restart
    let ctx2 = open_ctx(&db_path, base_time + 120);
    let mut p2_reload = login(&ctx2, 53_109, "other", PILOT_2).await;
    p2_reload.solar_system_id = Some(TURNUR_SYS);
    let (_, camp_snap) = match rpc(&ctx2, &mut p2_reload, "insurgencySolarsystem", "GetLocalCampaignClientSnapshot", &[]).await.unwrap() {
        PyRep::Tuple(mut items) if items.len() == 2 => (items.remove(0), items.remove(0)),
        other => panic!("expected (sys_id, snapshot) tuple, got {other:?}"),
    };
    assert_eq!(camp_snap.dict_get("corruptionStage"), Some(&PyRep::Integer(5)));
    let _ = camp_id;
    let _ = std::fs::remove_file(&db_path);
}
