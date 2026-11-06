#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::{auth_cache, client_login, macho_cluster_mgmt};
use eve_db::{IskCents, MarketOrderRecord, VirtualServerClock, WorldRepository};
use eve_machonet::service_roles::{ROLE_ADMIN, ROLE_GML, ROLE_GMS, ROLE_PROGRAMMER};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn test_ctx(now_unix: u64) -> ServerContext {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let ctx = ServerContext::with_sde_and_repository(shared_sde(), db);
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

fn make_session(user_id: Option<u64>, char_id: Option<i64>, role: u64, session_id: u64) -> SessionState {
    SessionState {
        session_id,
        user_id,
        character_id: char_id,
        role,
        solar_system_id: Some(30_002_187),
        station_id: Some(60_003_760),
        ..Default::default()
    }
}

fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries.iter().find(|(k, _)| k.as_str() == Some(key)).map(|(_, v)| v),
        PyRep::KeyVal(map) => map.get(key),
        _ => None,
    }
}

fn list_len(rep: &PyRep) -> usize {
    match rep {
        PyRep::List(v) | PyRep::Tuple(v) => v.len(),
        PyRep::Rowset { rows, .. } => rows.len(),
        PyRep::Dict(v) => v.len(),
        PyRep::KeyVal(m) => m.len(),
        _ => 0,
    }
}

async fn call_auth(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    kw: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    auth_cache::handle(ctx, sess, svc, method, args, kw)
        .await
        .expect("unhandled auth_cache route")
}

async fn call_login(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    kw: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    client_login::handle(ctx, sess, svc, method, args, kw, Some("unit_test"))
        .await
        .expect("unhandled client_login route")
}

fn call_cluster(
    ctx: &ServerContext,
    sess: &SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    kw: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    macho_cluster_mgmt::handle(ctx, sess, svc, method, args, kw)
        .expect("unhandled macho_cluster_mgmt route")
}

#[tokio::test]
async fn auth_cache_machonet_config_and_notifications() {
    let ctx = test_ctx(1_800_000_000);
    let mut sess = make_session(Some(1), Some(90_000_001), 0, 10);
    let kw = HashMap::new();

    assert!(list_len(&call_auth(&ctx, &mut sess, "machoNet", "GetServiceInfo", &[], &kw).await.unwrap()) > 0);
    assert!(call_auth(&ctx, &mut sess, "machoNet", "GetTime", &[], &kw).await.unwrap().as_i64().unwrap() > 0);
    assert!(call_auth(&ctx, &mut sess, "machoNet", "NoBlock", &[], &kw).await.is_ok());

    // GetCachedObject branches: invalid args + cache hit on registered key
    assert_eq!(call_auth(&ctx, &mut sess, "machoNet", "GetCachedObject", &[], &kw).await.unwrap_err().code, "InvalidArgument");
    assert!(call_auth(&ctx, &mut sess, "machoNet", "GetCachedObject", &[PyRep::Integer(1)], &kw).await.is_err());

    // SetGlobalConfigValue requires GM role
    assert_eq!(
        call_auth(&ctx, &mut sess, "machoNet", "SetGlobalConfigValue", &[PyRep::String("taxRate".into()), PyRep::Real(0.04)], &kw)
            .await
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    sess.role = ROLE_GML;
    assert_eq!(
        call_auth(&ctx, &mut sess, "machoNet", "SetGlobalConfigValue", &[PyRep::String("taxRate".into()), PyRep::Real(0.04)], &kw)
            .await
            .unwrap(),
        PyRep::Bool(true)
    );
    let mut cfg_kw = HashMap::new();
    cfg_kw.insert("key".into(), PyRep::String("brokerMin".into()));
    cfg_kw.insert("value".into(), PyRep::Integer(100));
    assert_eq!(
        call_auth(&ctx, &mut sess, "machoNet", "SetGlobalConfigValue", &[], &cfg_kw).await.unwrap(),
        PyRep::Bool(true)
    );

    // ForwardCharacterNotification guards
    sess.role = 0;
    assert_eq!(
        call_auth(&ctx, &mut sess, "machoNet", "ForwardCharacterNotification", &[PyRep::Integer(90_000_002)], &kw)
            .await
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    sess.role = ROLE_GML;
    assert_eq!(
        call_auth(&ctx, &mut sess, "machoNet", "ForwardCharacterNotification", &[], &kw).await.unwrap_err().code,
        "InvalidArgument"
    );
    assert_eq!(
        call_auth(
            &ctx,
            &mut sess,
            "machoNet",
            "ForwardCharacterNotification",
            &[PyRep::Integer(90_000_001), PyRep::String("OnNotificationReceived".into()), PyRep::Tuple(vec![PyRep::Integer(42)])],
            &kw,
        )
        .await
        .unwrap(),
        PyRep::Bool(true)
    );
}

#[tokio::test]
async fn auth_cache_login_shutdown_displacement_multilogin_and_slash() {
    let ctx = test_ctx(1_800_000_000);
    let kw = HashMap::new();
    ctx.db.provision_player_account("pilot1", "secret123");
    ctx.db.provision_player_account("pilot2", "secret123");

    let mut s1 = make_session(None, None, 0, 101);
    assert!(call_auth(&ctx, &mut s1, "authentication", "Login", &[PyRep::String("pilot1".into())], &kw).await.is_err());
    assert!(call_auth(&ctx, &mut s1, "authentication", "Login", &[PyRep::String("pilot1".into()), PyRep::String("wrongpw".into())], &kw).await.is_err());

    // Shutdown blocks non-GM logins but permits ROLE_GMS
    ctx.cluster_state.write().unwrap().shutdown_in_progress = true;
    assert_eq!(
        call_auth(&ctx, &mut s1, "authentication", "Login", &[PyRep::String("pilot1".into()), PyRep::String("secret123".into())], &kw)
            .await
            .unwrap_err()
            .code,
        "ClusterShuttingDown"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("pilot1") {
            acc.role = ROLE_GMS;
        }
    });
    assert!(call_auth(&ctx, &mut s1, "authentication", "Login", &[PyRep::String("pilot1".into()), PyRep::String("secret123".into())], &kw)
        .await
        .is_ok());
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("pilot1") {
            acc.role = 0;
        }
    });
    ctx.cluster_state.write().unwrap().shutdown_in_progress = false;

    // Banned account check
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("pilot2") {
            acc.banned_until_unix = Some(1_800_010_000);
            acc.ban_reason = Some("RMT".into());
        }
    });
    assert_eq!(
        call_auth(&ctx, &mut s1, "authentication", "Login", &[PyRep::String("pilot2".into()), PyRep::String("secret123".into())], &kw)
            .await
            .unwrap_err()
            .code,
        "UserBanned"
    );

    // Valid login + second session displacement + SetLanguageID
    s1.character_id = Some(999);
    let ok1 = call_auth(&ctx, &mut s1, "authentication", "Login", &[PyRep::String("pilot1".into()), PyRep::String("secret123".into())], &kw).await.unwrap();
    assert!(list_len(&ok1) >= 3 && s1.character_id.is_none() && s1.user_id.is_some());

    let mut s2 = make_session(None, None, 0, 102);
    assert!(call_auth(&ctx, &mut s2, "authentication", "Login", &[PyRep::String("pilot1".into()), PyRep::String("secret123".into())], &kw).await.is_ok());
    assert!(call_auth(&ctx, &mut s2, "authentication", "SetLanguageID", &[PyRep::String("   ".into())], &kw).await.is_err());
    assert_eq!(
        call_auth(&ctx, &mut s2, "authentication", "SetLanguageID", &[PyRep::String("DE".into())], &kw).await.unwrap(),
        PyRep::None
    );
    assert_eq!(s2.language_id, "DE");

    // MultiLoginBlocker + Alpha computer-hash conflict vs Omega
    assert!(call_auth(&ctx, &mut s2, "multiLoginBlocker", "Login", &[PyRep::String(" ".into())], &kw).await.is_err());
    assert_eq!(
        call_auth(&ctx, &mut s2, "multiLoginBlocker", "Login", &[PyRep::Bytes(b"pc-hash-xyz".to_vec())], &kw).await.unwrap(),
        PyRep::Bool(true)
    );
    s2.character_id = Some(90_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.clone_grade = 1;
        }
    });
    assert_eq!(call_auth(&ctx, &mut s2, "subscriptionMgr", "GetCloneGrade", &[], &kw).await.unwrap(), PyRep::Integer(0));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.clone_grade = 2;
        }
    });
    assert_eq!(call_auth(&ctx, &mut s2, "loginRewardFacilities", "get_my_clone_grade", &[], &kw).await.unwrap(), PyRep::Integer(1));

    // SlashCmd role gate + /tr + /createccpevent ROLE_GMH check
    assert!(call_auth(&ctx, &mut s2, "slash", "SlashCmd", &[PyRep::String("/heal".into())], &kw).await.is_err());
    s2.role = ROLE_GML;
    s2.character_id = Some(90_000_001);
    assert!(call_auth(&ctx, &mut s2, "slash", "SlashCmd", &[PyRep::String("/tr 30000142".into())], &kw).await.is_ok());
    assert_eq!(s2.solar_system_id, Some(30_000_142));
    assert_eq!(
        call_auth(&ctx, &mut s2, "slash", "SlashCmd", &[PyRep::String("/createccpevent 1".into())], &kw)
            .await
            .unwrap_err()
            .code,
        "InsufficientRole"
    );
    assert!(call_auth(&ctx, &mut s2, "slash", "SlashCmd", &[PyRep::String("/heal".into())], &kw).await.is_ok());
}

#[test]
fn cluster_mgmt_load_session_db2_debug_and_machonet_lifecycle() {
    let ctx = test_ctx(1_800_000_000);
    let mut user_sess = make_session(Some(1), Some(90_000_001), 0, 201);
    let mut gm_sess = make_session(Some(1), Some(90_000_001), ROLE_GML, 202);
    let kw = HashMap::new();

    assert_eq!(call_cluster(&ctx, &user_sess, "externalQueueMgr", "PublishProxyEvent", &[], &kw).unwrap(), PyRep::None);
    assert!(call_cluster(&ctx, &user_sess, "tcpRawProxyService", "GetESPTunnelingAddressByNodeID", &[PyRep::Integer(1)], &kw).is_ok());
    assert_eq!(call_cluster(&ctx, &user_sess, "machoNet", "GetEjabberdAddress", &[], &kw).unwrap(), PyRep::String("127.0.0.1".into()));
    assert!(dict_get(&call_cluster(&ctx, &user_sess, "machoNet", "GetGlobalConfig", &[], &kw).unwrap(), "clusterName").is_some());

    // Role gates
    assert!(call_cluster(&ctx, &user_sess, "loadService", "StartLoad", &[PyRep::Integer(50)], &kw).is_err());
    assert!(call_cluster(&ctx, &user_sess, "machoNet", "GetMetrics", &[], &kw).is_err());
    assert!(call_cluster(&ctx, &user_sess, "debug", "Exec", &[PyRep::String("status".into())], &kw).is_err());

    // GM loadService clamping + config
    assert!(call_cluster(&ctx, &gm_sess, "loadService", "StartLoad", &[PyRep::Real(150.0)], &kw).is_ok());
    assert!(call_cluster(&ctx, &gm_sess, "loadService", "StopLoad", &[], &kw).is_ok());
    assert!(call_cluster(&ctx, &gm_sess, "loadService", "SetConfig", &[PyRep::Dict(vec![(PyRep::String("tickHz".into()), PyRep::Integer(20))])], &kw).is_ok());
    assert!(call_cluster(&ctx, &gm_sess, "loadService", "GetTotalStats", &[], &kw).is_ok());

    // SetConnectionProperty + GracefulShutdown + TerminateAllClients + ReloadClientCodeHash
    assert!(call_cluster(&ctx, &gm_sess, "machoNet", "SetConnectionProperty", &[PyRep::String("badProp".into()), PyRep::Integer(1)], &kw).is_err());
    assert!(call_cluster(&ctx, &gm_sess, "machoNet", "SetConnectionProperty", &[PyRep::String("clusterName".into()), PyRep::String("SINGULARITY".into())], &kw).is_ok());
    assert_eq!(call_cluster(&ctx, &user_sess, "machoNet", "GetConnectedClusterName", &[], &kw).unwrap(), PyRep::String("SINGULARITY".into()));
    assert!(!call_cluster(&ctx, &user_sess, "machoNet", "GetConnectedClusterMode", &[], &kw).unwrap().as_str().unwrap().is_empty());
    assert!(dict_get(&call_cluster(&ctx, &gm_sess, "machoNet", "GetConnectionProperties", &[], &kw).unwrap(), "clusterName").is_some());

    assert!(call_cluster(&ctx, &gm_sess, "machoNet", "GracefulShutdown", &[PyRep::Integer(1), PyRep::String("Maint".into()), PyRep::Integer(30), PyRep::Bool(true)], &kw).is_ok());
    assert!(ctx.cluster_state.read().unwrap().shutdown_in_progress);
    assert!(call_cluster(&ctx, &gm_sess, "machoNet", "GracefulShutdown", &[PyRep::Integer(1), PyRep::String("Cancel".into()), PyRep::Integer(0)], &kw).is_ok());
    assert!(!ctx.cluster_state.read().unwrap().shutdown_in_progress);

    assert!(call_cluster(&ctx, &gm_sess, "machoNet", "GracefulShutdownStarted", &[], &kw).is_ok());
    assert!(call_cluster(&ctx, &gm_sess, "machoNet", "GracefulShutdownComplete", &[PyRep::Integer(7)], &kw).is_ok());
    assert!(call_cluster(&ctx, &gm_sess, "machoNet", "GracefulShutdownComplete", &[PyRep::Integer(7)], &kw).is_ok());
    assert_eq!(ctx.cluster_state.read().unwrap().shutdown_complete_nodes, vec![7]);
    assert_eq!(
        call_cluster(&ctx, &gm_sess, "machoNet", "ReloadClientCodeHash", &[PyRep::String("deadbeef1234".into())], &kw).unwrap(),
        PyRep::String("deadbeef1234".into())
    );
    assert!(call_cluster(&ctx, &gm_sess, "machoNet", "TerminateAllClients", &[PyRep::String("Reboot".into())], &kw).is_ok());
    assert!(ctx.cluster_state.read().unwrap().clients_terminated);

    // sessionMgr & debug.Exec & DB2.CallProc rate-limit
    assert!(call_cluster(&ctx, &gm_sess, "sessionMgr", "RemoveSessionsFromServer", &[PyRep::Integer(201)], &kw).is_ok());
    assert!(call_cluster(&ctx, &gm_sess, "sessionMgr", "EndAllGameSessions", &[], &kw).is_ok());
    assert!(call_cluster(&ctx, &gm_sess, "debug", "Exec", &[PyRep::String("status".into())], &kw).is_err());
    gm_sess.role |= ROLE_PROGRAMMER;
    assert_eq!(list_len(&call_cluster(&ctx, &gm_sess, "debug", "Exec", &[PyRep::String("ping()".into())], &kw).unwrap()), 1);

    assert!(call_cluster(&ctx, &user_sess, "DB2", "CallProc", &[PyRep::String("zcluster.Nodes_PolarisID".into())], &kw).is_err());
    assert_eq!(call_cluster(&ctx, &gm_sess, "DB2", "CallProc", &[PyRep::String("zcluster.Nodes_PolarisID".into())], &kw).unwrap(), PyRep::Integer(1));
    assert!(list_len(&call_cluster(&ctx, &gm_sess, "DB2", "CallProc", &[PyRep::String("zuser.Users_SelectByRole".into())], &kw).unwrap()) >= 1);
    user_sess.user_id = Some(777);
    for _ in 0..16 {
        assert_eq!(
            call_cluster(&ctx, &user_sess, "DB2", "CallProc", &[PyRep::String("zcluster.Attacks_Insert".into()), PyRep::String("flood".into())], &kw).unwrap(),
            PyRep::None
        );
    }
    assert_eq!(
        call_cluster(&ctx, &user_sess, "DB2", "CallProc", &[PyRep::String("zcluster.Attacks_Insert".into())], &kw).unwrap_err().message,
        "RateLimited"
    );
}

#[tokio::test]
async fn client_login_alerts_market_cohorts_and_load_slot() {
    let ctx = test_ctx(1_800_000_000);
    let mut sess = make_session(Some(1), Some(90_000_001), 0, 301);
    let kw = HashMap::new();

    assert!(call_login(&ctx, &mut sess, "alert", "BeanCount", &[PyRep::Integer(12345)], &kw).await.is_ok());
    assert!(call_login(
        &ctx,
        &mut sess,
        "alert",
        "BeanDelivery",
        &[PyRep::Dict(vec![(
            PyRep::Integer(12345),
            PyRep::Tuple(vec![PyRep::Integer(-3), PyRep::Integer(1_800_000_000)]),
        )])],
        &kw,
    )
    .await
    .is_err());
    assert!(call_login(
        &ctx,
        &mut sess,
        "alert",
        "BeanDelivery",
        &[PyRep::Dict(vec![(
            PyRep::Integer(12345),
            PyRep::Tuple(vec![PyRep::Integer(5), PyRep::Integer(1_800_000_000)]),
        )])],
        &kw,
    )
    .await
    .is_ok());

    assert!(call_login(&ctx, &mut sess, "alert", "LogModeChanged", &[PyRep::Integer(900), PyRep::String("I".into())], &kw).await.is_err());
    sess.role = ROLE_ADMIN;
    assert_eq!(call_login(&ctx, &mut sess, "alert", "LogModeChanged", &[PyRep::Integer(900), PyRep::String("I".into())], &kw).await.unwrap(), PyRep::None);
    assert!(list_len(&call_login(&ctx, &mut sess, "alert", "GetLogModeForError", &[PyRep::Dict(vec![(PyRep::Integer(900), PyRep::None)])], &kw).await.unwrap()) >= 1);
    assert!(call_login(&ctx, &mut sess, "alert", "SendProxyStackTraceAlert", &[PyRep::Integer(9911), PyRep::String("RuntimeError".into()), PyRep::String("B".into())], &kw).await.is_ok());

    ctx.db.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            700_001,
            MarketOrderRecord {
                order_id: 700_001,
                owner_id: 90_000_001,
                station_id: 60_003_760,
                region_id: 10_000_002,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(10),
                vol_entered: 100,
                vol_remaining: 50,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });
    assert!(list_len(&call_login(&ctx, &mut sess, "config", "GetAverageMarketPrices", &[], &kw).await.unwrap()) >= 1);
    assert!(call_login(&ctx, &mut sess, "config", "GetBlackListedPlanets", &[], &kw).await.is_ok());
    assert!(call_login(&ctx, &mut sess, "air_npe", "is_air_npe_enabled", &[], &kw).await.is_ok());
    assert!(call_login(&ctx, &mut sess, "invbroker", "GetItemDescriptor", &[], &kw).await.is_ok());
    assert_eq!(call_login(&ctx, &mut sess, "userSvc", "GetRedeemTokens", &[], &kw).await.unwrap(), PyRep::List(vec![]));
    assert_eq!(call_login(&ctx, &mut sess, "charUnboundMgr", "GetCharOmegaDowngradeStatus", &[], &kw).await.unwrap(), PyRep::Bool(false));
    assert!(call_login(&ctx, &mut sess, "map", "GetSecurityModifiedSystems", &[], &kw).await.is_ok());
    assert!(call_login(&ctx, &mut sess, "config", "GetOldStationData", &[], &kw).await.is_ok());

    // GetCohortsForUser: < 30 days new_player vs >= 30 days veteran
    ctx.db.with_mut_snapshot(|snap| {
        for ch in snap.characters.values_mut().filter(|c| c.user_id == 1) {
            ch.create_date_unix = 1_800_000_000 - (5 * 86_400);
        }
        snap.service_state_blobs.remove("user_cohorts:1");
    });
    assert!(format!("{:?}", call_login(&ctx, &mut sess, "charUnboundMgr", "GetCohortsForUser", &[], &kw).await.unwrap()).contains("cohort_new_player"));

    ctx.db.with_mut_snapshot(|snap| {
        for ch in snap.characters.values_mut().filter(|c| c.user_id == 1) {
            ch.create_date_unix = 1_800_000_000 - (31 * 86_400);
        }
    });
    assert!(format!("{:?}", call_login(&ctx, &mut sess, "charUnboundMgr", "GetCohortsForUser", &[], &kw).await.unwrap()).contains("cohort_veteran"));

    // populationCap GetCharacterLoadSlot & MoveCharacterToNewSystem + GetCharacterLockType
    assert!(call_login(&ctx, &mut sess, "populationCap", "GetCharacterLoadSlot", &[PyRep::Integer(999_999)], &kw).await.is_err());
    assert!(list_len(&call_login(&ctx, &mut sess, "populationCap", "GetCharacterLoadSlot", &[PyRep::Integer(90_000_001)], &kw).await.unwrap()) >= 1);
    assert!(call_login(&ctx, &mut sess, "populationCap", "MoveCharacterToNewSystem", &[PyRep::Integer(90_000_001), PyRep::Integer(30_000_142), PyRep::String("bad".into())], &kw).await.is_err());
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert("character_lock:90000001".into(), "transfer".into());
    });
    assert_eq!(call_login(&ctx, &mut sess, "charUnboundMgr", "GetCharacterLockType", &[PyRep::Integer(90_000_001)], &kw).await.unwrap(), PyRep::Integer(1));
}
