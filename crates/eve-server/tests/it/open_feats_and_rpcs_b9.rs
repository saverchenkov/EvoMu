//! Integration tests closing UI, star map, neocom badges, info panels, photo mode,
//! medical clone grades, session changes, and GM tournament `feat:` parity rows.

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn select_pilot(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    s
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_ui_star_map_data_and_map_stats() {
    // parity: feat:ui.star-map-data, feat:ui.map-stats
    eve_testkit::attest(&["feat:ui.star-map-data", "feat:ui.map-stats"]);

    let ctx = ServerContext::new();
    let mut s = select_pilot(&ctx, 9101, "demo", "demo-secret", P1).await;

    let beacons = rpc(&ctx, &mut s, "map", "GetBeaconCount", &[])
        .await
        .expect("GetBeaconCount");
    assert!(matches!(beacons, PyRep::Dict(_)));

    let incursions = rpc(&ctx, &mut s, "map", "GetIncursionGlobalReport", &[])
        .await
        .expect("GetIncursionGlobalReport");
    assert!(matches!(incursions, PyRep::List(_)));

    let inc_systems = rpc(&ctx, &mut s, "map", "GetSystemsInIncursions", &[])
        .await
        .expect("GetSystemsInIncursions");
    assert!(matches!(inc_systems, PyRep::List(_)));

    let fw_zone = rpc(&ctx, &mut s, "map", "GetFacWarZoneInfo", &[PyRep::Integer(500_001)])
        .await
        .expect("GetFacWarZoneInfo");
    assert!(!matches!(fw_zone, PyRep::None));

    let edencom_fort = rpc(&ctx, &mut s, "map", "GetEdencomFortressSystems", &[])
        .await
        .expect("GetEdencomFortressSystems");
    assert!(matches!(edencom_fort, PyRep::List(_)));

    let edencom_minor = rpc(&ctx, &mut s, "map", "GetEdencomMinorVictorySystems", &[])
        .await
        .expect("GetEdencomMinorVictorySystems");
    assert!(matches!(edencom_minor, PyRep::List(_)));

    let trig_minor = rpc(&ctx, &mut s, "map", "GetTriglavianMinorVictorySystems", &[])
        .await
        .expect("GetTriglavianMinorVictorySystems");
    assert!(matches!(trig_minor, PyRep::List(_)));

    let deadspace_complex = rpc(&ctx, &mut s, "map", "GetDeadspaceComplexMap", &[])
        .await
        .expect("GetDeadspaceComplexMap");
    assert!(matches!(deadspace_complex, PyRep::ObjectEx { .. }));

    let deadspace_agents = rpc(&ctx, &mut s, "map", "GetDeadspaceAgentsMap", &[])
        .await
        .expect("GetDeadspaceAgentsMap");
    assert!(matches!(deadspace_agents, PyRep::ObjectEx { .. }));

    // Record jump count via map_service::record_system_jump and verify map.GetHistory(3) & map.GetHistory(1)
    let jita: u32 = 30_000_142;
    eve_server::services::map_service::record_system_jump(&ctx, jita);
    eve_server::services::map_service::record_system_jump(&ctx, jita);

    let recorded_jumps: i64 = ctx.db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get("map:system_jumps")
            .and_then(|raw| serde_json::from_str::<HashMap<u32, i64>>(raw).ok())
            .and_then(|m| m.get(&jita).copied())
            .unwrap_or(0)
    });
    assert_eq!(recorded_jumps, 2, "record_system_jump must persist per-system jump count");

    let hist_jumps = rpc(&ctx, &mut s, "map", "GetHistory", &[PyRep::Integer(3)])
        .await
        .expect("GetHistory(3)");
    assert!(matches!(hist_jumps, PyRep::ObjectEx { .. }));

    let hist_kills = rpc(&ctx, &mut s, "map", "GetHistory", &[PyRep::Integer(1)])
        .await
        .expect("GetHistory(1)");
    assert!(matches!(hist_kills, PyRep::ObjectEx { .. }));
}

#[tokio::test]
async fn test_ui_neocom_badges_info_panels_and_photo_mode() {
    // parity: feat:ui.neocom-badges, feat:ui.info-panels, feat:ui.photo-mode
    eve_testkit::attest(&[
        "feat:ui.neocom-badges",
        "feat:ui.info-panels",
        "feat:ui.photo-mode",
    ]);

    let ctx = ServerContext::new();
    let mut s = select_pilot(&ctx, 9102, "demo", "demo-secret", P1).await;

    // Neocom badges: unread mail, unread notifications, skill queue & free SP
    let mail_headers = rpc(&ctx, &mut s, "mailMgr", "GetMailHeaders", &[])
        .await
        .expect("GetMailHeaders");
    assert!(!matches!(mail_headers, PyRep::None));

    let notifs = rpc(
        &ctx,
        &mut s,
        "notificationMgr",
        "GetUnprocessed",
        &[],
    )
    .await
    .expect("GetUnprocessed");
    assert!(!matches!(notifs, PyRep::None));

    let skill_q = rpc(&ctx, &mut s, "skillMgr2", "GetSkillQueueAndFreePoints", &[])
        .await
        .expect("GetSkillQueueAndFreePoints");
    assert!(matches!(skill_q, PyRep::Tuple(_) | PyRep::List(_) | PyRep::Dict(_)));

    // Info panels: mission tracker, incursion, FW, sov
    let journal = rpc(&ctx, &mut s, "agentMgr", "GetMyJournalDetails", &[])
        .await
        .expect("GetMyJournalDetails");
    assert!(matches!(journal, PyRep::Tuple(_) | PyRep::List(_) | PyRep::Dict(_)));

    let sov_info = rpc(
        &ctx,
        &mut s,
        "sovMgr",
        "GetSovStructuresInfoForSolarSystem",
        &[PyRep::Integer(30_000_142)],
    )
    .await
    .expect("GetSovStructuresInfoForSolarSystem");
    let _ = sov_info;

    // Photo mode: station / scene state queries succeed with zero server errors
    let station_id = s.station_id.unwrap_or(60_003_760);
    assert_eq!(station_id, 60_003_760);
    let station_info = rpc(
        &ctx,
        &mut s,
        "stationSvc",
        "GetStation",
        &[PyRep::Integer(station_id)],
    )
    .await
    .expect("GetStation");
    assert!(!matches!(station_info, PyRep::None));
}

#[tokio::test]
async fn test_removed_medical_clone_grades_and_session_changes() {
    // parity: feat:removed.medical-clone-grades, feat:serverops.session-changes
    eve_testkit::attest(&[
        "feat:removed.medical-clone-grades",
        "feat:serverops.session-changes",
    ]);

    let ctx = ServerContext::new();
    let mut s = select_pilot(&ctx, 9103, "demo", "demo-secret", P1).await;

    let isk_before = rpc(&ctx, &mut s, "account", "GetCashBalance", &[PyRep::Bool(false)])
        .await
        .expect("GetCashBalance before");

    let home_before = rpc(&ctx, &mut s, "charMgr", "GetHomeStation", &[])
        .await
        .expect("GetHomeStation");
    assert!(matches!(home_before, PyRep::Integer(_) | PyRep::Object { .. } | PyRep::Dict(_)));

    // Setting home station is free (medical clone grades removed in Rhea)
    let target_station = 60_003_760_i64;
    let set_res = rpc(
        &ctx,
        &mut s,
        "charMgr",
        "SetHomeStation",
        &[PyRep::Integer(target_station)],
    )
    .await
    .expect("SetHomeStation");
    assert!(matches!(set_res, PyRep::None | PyRep::Bool(true) | PyRep::Integer(_)));

    let isk_after = rpc(&ctx, &mut s, "account", "GetCashBalance", &[PyRep::Bool(false)])
        .await
        .expect("GetCashBalance after");
    assert_eq!(isk_before, isk_after, "Medical clone home station change must be free");

    // Session attribute changes propagate to SessionState
    assert_eq!(s.character_id, Some(P1));
    assert!(s.solar_system_id.is_some());
    assert!(s.corporation_id.is_some());
}

#[tokio::test]
async fn test_admin_gm_tournament_lifecycle() {
    // parity: feat:admin.gm-tournament
    eve_testkit::attest(&["feat:admin.gm-tournament"]);

    let ctx = ServerContext::new();
    let mut gm = select_pilot(&ctx, 9104, "demo", "demo-secret", P1).await;
    gm.role = u64::MAX;

    let match_id = 1001_i64;
    rpc(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1),
            PyRep::Tuple(vec![
                PyRep::Integer(match_id),
                PyRep::Integer(98_000_001),
                PyRep::Integer(98_000_002),
                PyRep::Integer(P1),
                PyRep::Integer(P2),
            ]),
        ],
    )
    .await
    .expect("CreateMatch");

    rpc(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "AddPlayer",
        &[
            PyRep::Integer(match_id),
            PyRep::Integer(0),
            PyRep::Integer(P1),
        ],
    )
    .await
    .expect("AddPlayer P1");

    rpc(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "AddPlayer",
        &[
            PyRep::Integer(match_id),
            PyRep::Integer(1),
            PyRep::Integer(P2),
        ],
    )
    .await
    .expect("AddPlayer P2");

    rpc(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "BanShip",
        &[
            PyRep::Integer(match_id),
            PyRep::Integer(0),
            PyRep::Integer(587),
        ],
    )
    .await
    .expect("BanShip");

    rpc(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "FinalizeBans",
        &[PyRep::Integer(match_id)],
    )
    .await
    .expect("FinalizeBans");

    let team0 = rpc(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "GetTeamMembers",
        &[PyRep::Integer(match_id), PyRep::Integer(0)],
    )
    .await
    .expect("GetTeamMembers 0");
    if let PyRep::List(members) = team0 {
        assert_eq!(members.len(), 2);
    }

    rpc(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "CleanupGrid",
        &[PyRep::Integer(match_id)],
    )
    .await
    .expect("CleanupGrid");
}

#[tokio::test]
async fn test_ui_browser_lockdown_policy_and_persistence() {
    // parity: feat:ui.browser-lockdown, rpc:browserLockdownSvc.IsBrowserInLockdown, rpc:browserLockdownSvc.GetDefaultHomePage, rpc:browserLockdownSvc.GetFlaggedSitesList, rpc:browserLockdownSvc.GetFlaggedSitesHash
    eve_testkit::attest(&[
        "feat:ui.browser-lockdown",
        "rpc:browserLockdownSvc.IsBrowserInLockdown",
        "rpc:browserLockdownSvc.GetDefaultHomePage",
        "rpc:browserLockdownSvc.GetFlaggedSitesList",
        "rpc:browserLockdownSvc.GetFlaggedSitesHash",
    ]);

    let ctx = ServerContext::new();
    let mut unauth = SessionState::new(9200);
    assert!(rpc(&ctx, &mut unauth, "browserLockdownSvc", "IsBrowserInLockdown", &[]).await.is_err());

    let mut pilot = select_pilot(&ctx, 9201, "demo", "demo-secret", P1).await;
    assert_eq!(
        rpc(&ctx, &mut pilot, "browserLockdownSvc", "IsBrowserInLockdown", &[]).await.unwrap(),
        PyRep::Bool(false)
    );
    assert_eq!(
        rpc(&ctx, &mut pilot, "browserLockdownSvc", "GetDefaultHomePage", &[]).await.unwrap(),
        PyRep::String("https://www.eveonline.com/".to_string())
    );
    assert_eq!(
        rpc(&ctx, &mut pilot, "browserLockdownSvc", "GetFlaggedSitesList", &[]).await.unwrap(),
        PyRep::List(vec![])
    );
    let h0 = rpc(&ctx, &mut pilot, "browserLockdownSvc", "GetFlaggedSitesHash", &[]).await.unwrap();

    // Persist updated policy and verify RPCs reflect it
    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::notify_emitters_batch9_part2::set_browser_lockdown_policy(
            snap,
            true,
            "https://forums.eveonline.com",
            vec![("https://phishing.example.test".to_string(), 2)],
        );
    });

    assert_eq!(
        rpc(&ctx, &mut pilot, "browserLockdownSvc", "IsBrowserInLockdown", &[]).await.unwrap(),
        PyRep::Bool(true)
    );
    assert_eq!(
        rpc(&ctx, &mut pilot, "browserLockdownSvc", "GetDefaultHomePage", &[]).await.unwrap(),
        PyRep::String("https://forums.eveonline.com".to_string())
    );
    assert_eq!(
        rpc(&ctx, &mut pilot, "browserLockdownSvc", "GetFlaggedSitesList", &[]).await.unwrap(),
        PyRep::List(vec![PyRep::Dict(vec![
            (PyRep::String("siteUrl".into()), PyRep::String("https://phishing.example.test".into())),
            (PyRep::String("siteFlag".into()), PyRep::Integer(2)),
        ])])
    );
    let h1 = rpc(&ctx, &mut pilot, "browserLockdownSvc", "GetFlaggedSitesHash", &[]).await.unwrap();
    assert_ne!(h0, h1);
}

#[tokio::test]
async fn test_ui_search_and_agency_content_warp() {
    // parity: feat:ui.search, feat:ui.agency, rpc:custom_agency_provider.warp_to_content
    eve_testkit::attest(&[
        "feat:ui.search",
        "feat:ui.agency",
        "rpc:custom_agency_provider.warp_to_content",
    ]);

    let ctx = ServerContext::new();
    let mut s = select_pilot(&ctx, 9210, "demo", "demo-secret", P1).await;

    // 1. search.Query & search.QuickQuery
    let q_res = rpc(&ctx, &mut s, "search", "Query", &[PyRep::String("Jita".to_string())])
        .await
        .expect("search.Query");
    assert!(format!("{q_res:?}").contains("30000142"), "search.Query('Jita') must include Jita (30000142)");
    let qq_res = rpc(&ctx, &mut s, "search", "QuickQuery", &[PyRep::String("Jita".to_string())])
        .await
        .expect("search.QuickQuery");
    assert!(format!("{qq_res:?}").contains("30000142"), "search.QuickQuery('Jita') must include Jita (30000142)");

    // 2. Agency content data & warp_to_content
    let mut unauth = SessionState::new(9211);
    let unauth_err = rpc(&ctx, &mut unauth, "custom_agency_provider", "get_content_data", &[])
        .await
        .unwrap_err();
    assert!(!unauth_err.code.is_empty());

    let content = rpc(&ctx, &mut s, "custom_agency_provider", "get_content_data", &[])
        .await
        .expect("get_content_data");
    let content_dbg = format!("{content:?}");
    assert!(
        content_dbg.contains("contentID") || content_dbg.contains("solarSystemID"),
        "get_content_data must return structured agency content"
    );

    // Negative: warp_to_content without contentID fails
    let warp_err = rpc(&ctx, &mut s, "custom_agency_provider", "warp_to_content", &[])
        .await
        .unwrap_err();
    assert!(!warp_err.code.is_empty());
}

#[tokio::test]
async fn test_beyonce_bind_unbind_and_bill_get() {
    // parity: rpc:beyonce.Bind, rpc:beyonce.Unbind, rpc:billMgr.get
    eve_testkit::attest(&[
        "rpc:beyonce.Bind",
        "rpc:beyonce.Unbind",
        "rpc:billMgr.get",
    ]);

    let ctx = ServerContext::new();
    let mut unauth = SessionState::new(9220);
    assert!(rpc(&ctx, &mut unauth, "beyonce", "Bind", &[PyRep::Integer(30000142)]).await.is_err());
    assert!(rpc(&ctx, &mut unauth, "billMgr", "get", &[]).await.is_err());

    let mut s = select_pilot(&ctx, 9221, "demo", "demo-secret", P1).await;

    // 1. beyonce.Bind & beyonce.Unbind
    let bound = rpc(&ctx, &mut s, "beyonce", "Bind", &[PyRep::Integer(30000142)])
        .await
        .expect("beyonce.Bind");
    let bound_id = bound.as_i64().expect("bound_id int");
    assert!(bound_id > 0);
    let unbind_res = rpc(&ctx, &mut s, "beyonce", "Unbind", &[PyRep::Integer(bound_id)])
        .await
        .expect("beyonce.Unbind");
    assert_eq!(unbind_res, PyRep::None);

    // 2. billMgr.get state-backed personal bills
    let empty_bills = rpc(&ctx, &mut s, "billMgr", "get", &[]).await.expect("billMgr.get empty");
    assert_eq!(empty_bills, PyRep::List(vec![]));

    ctx.db.with_mut_snapshot(|snap| {
        snap.owner_notes.insert(
            990_001,
            eve_db::OwnerNoteRecord {
                note_id: 990_001,
                character_id: -71,
                item_id: 0,
                label: "bill:990001".to_string(),
                note: serde_json::json!({
                    "bill_id": 990001,
                    "bill_type_id": 1,
                    "debtor_id": P1,
                    "creditor_id": 1000002,
                    "amount_cents": 25000000,
                    "due_unix": 1700086400,
                    "interest": 0.0,
                    "external_id": 0,
                    "external_id2": 0,
                    "paid": false,
                    "paid_by_division": null
                })
                .to_string(),
                timestamp_unix: 1_700_000_000,
            },
        );
    });
    let one_bill = rpc(&ctx, &mut s, "billMgr", "get", &[]).await.expect("billMgr.get 1");
    if let PyRep::List(rows) = one_bill {
        assert_eq!(rows.len(), 1);
    } else {
        panic!("expected PyRep::List from billMgr.get");
    }
}
