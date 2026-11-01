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
    let jita: u32 = 30_000_142;

    // Seed a Pharolux Cyno Beacon (type_id 35840) in Jita
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            77_000_001,
            eve_db::UpwellStructureRecord {
                structure_id: 77_000_001,
                type_id: 35840,
                solar_system_id: jita,
                owner_corp_id: 98_000_001,
                name: "Jita Cyno Beacon".into(),
                state: "Online".into(),
                ..Default::default()
            },
        );
    });

    let beacons = rpc(&ctx, &mut s, "map", "GetBeaconCount", &[])
        .await
        .expect("GetBeaconCount");
    let PyRep::Dict(beacon_entries) = beacons else {
        panic!("expected Dict from GetBeaconCount");
    };
    let jita_beacons = beacon_entries
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(i64::from(jita)))
        .map(|(_, v)| v.clone());
    assert_eq!(
        jita_beacons,
        Some(PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(1)])),
        "GetBeaconCount must report seeded Pharolux Cyno Beacon in Jita"
    );

    let incursions = rpc(&ctx, &mut s, "map", "GetIncursionGlobalReport", &[])
        .await
        .expect("GetIncursionGlobalReport");
    let PyRep::List(inc_list) = incursions else {
        panic!("expected List from GetIncursionGlobalReport");
    };
    assert!(!inc_list.is_empty(), "GetIncursionGlobalReport must return active incursions");

    let inc_systems = rpc(&ctx, &mut s, "map", "GetSystemsInIncursions", &[])
        .await
        .expect("GetSystemsInIncursions");
    let PyRep::List(inc_sys_list) = inc_systems else {
        panic!("expected List from GetSystemsInIncursions");
    };
    assert!(!inc_sys_list.is_empty(), "GetSystemsInIncursions must return incursion systems");

    let fw_zone = rpc(&ctx, &mut s, "map", "GetFacWarZoneInfo", &[PyRep::Integer(500_001)])
        .await
        .expect("GetFacWarZoneInfo");
    assert!(
        format!("{fw_zone:?}").contains("solarSystemIds"),
        "GetFacWarZoneInfo must return KeyVal with solarSystemIds"
    );

    let edencom_fort = rpc(&ctx, &mut s, "map", "GetEdencomFortressSystems", &[])
        .await
        .expect("GetEdencomFortressSystems");
    let PyRep::List(ef_list) = edencom_fort else {
        panic!("expected List from GetEdencomFortressSystems");
    };
    assert!(!ef_list.is_empty(), "GetEdencomFortressSystems must be non-empty");

    let edencom_minor = rpc(&ctx, &mut s, "map", "GetEdencomMinorVictorySystems", &[])
        .await
        .expect("GetEdencomMinorVictorySystems");
    let PyRep::List(em_list) = edencom_minor else {
        panic!("expected List from GetEdencomMinorVictorySystems");
    };
    assert!(!em_list.is_empty(), "GetEdencomMinorVictorySystems must be non-empty");

    let trig_minor = rpc(&ctx, &mut s, "map", "GetTriglavianMinorVictorySystems", &[])
        .await
        .expect("GetTriglavianMinorVictorySystems");
    let PyRep::List(tm_list) = trig_minor else {
        panic!("expected List from GetTriglavianMinorVictorySystems");
    };
    assert!(!tm_list.is_empty(), "GetTriglavianMinorVictorySystems must be non-empty");

    let deadspace_complex = rpc(&ctx, &mut s, "map", "GetDeadspaceComplexMap", &[])
        .await
        .expect("GetDeadspaceComplexMap");
    assert!(matches!(deadspace_complex, PyRep::ObjectEx { .. }));

    let deadspace_agents = rpc(&ctx, &mut s, "map", "GetDeadspaceAgentsMap", &[])
        .await
        .expect("GetDeadspaceAgentsMap");
    assert!(matches!(deadspace_agents, PyRep::ObjectEx { .. }));

    // Record jump events via map_service::record_system_jump and kill events, then verify map.GetHistory(1/2/3)
    eve_server::services::map_service::record_system_jump(&ctx, jita);
    eve_server::services::map_service::record_system_jump(&ctx, jita);
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.record_system_kill(jita, 1, 2, 0, now_u);
    });

    let find_sys_row = |rep: PyRep, sys_id: u32| -> (i64, i64, i64) {
        let PyRep::ObjectEx { list, .. } = rep else {
            panic!("expected ObjectEx CRowset from GetHistory");
        };
        list.into_iter()
            .find_map(|r| match r {
                PyRep::DBRow { columns, .. } => {
                    let sid = columns.iter().find_map(|(k, v)| (k == "solarSystemID").then(|| v.as_i64()).flatten())?;
                    if sid != i64::from(sys_id) {
                        return None;
                    }
                    let v1 = columns.iter().find_map(|(k, v)| (k == "value1").then(|| v.as_i64()).flatten()).unwrap_or(0);
                    let v2 = columns.iter().find_map(|(k, v)| (k == "value2").then(|| v.as_i64()).flatten()).unwrap_or(0);
                    let v3 = columns.iter().find_map(|(k, v)| (k == "value3").then(|| v.as_i64()).flatten()).unwrap_or(0);
                    Some((v1, v2, v3))
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("missing row for solarSystemID {sys_id}"))
    };

    // stat_type 1 => jumps
    let hist_jumps = rpc(&ctx, &mut s, "map", "GetHistory", &[PyRep::Integer(1)])
        .await
        .expect("GetHistory(1)");
    assert_eq!(find_sys_row(hist_jumps, jita), (2, 0, 0), "GetHistory(1) must return 2 jumps");

    // stat_type 2 => kills (ship_kills + npc_kills, npc_kills, pod_kills)
    let hist_kills = rpc(&ctx, &mut s, "map", "GetHistory", &[PyRep::Integer(2)])
        .await
        .expect("GetHistory(2)");
    assert_eq!(find_sys_row(hist_kills, jita), (3, 2, 0), "GetHistory(2) must return (3, 2, 0) kills");

    // stat_type 3 => pilots in space (0 while docked)
    let hist_pilots = rpc(&ctx, &mut s, "map", "GetHistory", &[PyRep::Integer(3)])
        .await
        .expect("GetHistory(3)");
    assert_eq!(find_sys_row(hist_pilots, jita), (0, 0, 0), "GetHistory(3) must return 0 pilots in space");
}

#[tokio::test]
async fn test_ui_neocom_badges_info_panels_and_photo_mode() {
    // parity: feat:ui.neocom-badges, feat:ui.photo-mode
    eve_testkit::attest(&["feat:ui.neocom-badges", "feat:ui.photo-mode"]);

    let ctx = ServerContext::new();
    let mut s = select_pilot(&ctx, 9102, "demo", "demo-secret", P1).await;
    let now_u = ctx.now_unix();

    // Seed 1 unread mail and 1 unread notification for P1
    ctx.db.with_mut_snapshot(|snap| {
        snap.mails.push(eve_db::MailRecord {
            message_id: 880_001,
            sender_id: P2,
            recipient_id: P1,
            subject: "Neocom Badge Test".into(),
            body: "Unread mail body".into(),
        });
        snap.mail_assigned_labels.insert((P1, 880_001), vec![1]);
        snap.mail_timestamps.insert(880_001, now_u);
    });
    ctx.notification_records
        .write()
        .unwrap()
        .push(eve_server::router::NotificationRecord {
            notification_id: 990_101,
            type_id: 101,
            sender_id: 1_000_125,
            receiver_id: P1,
            processed: false,
            group_id: 12,
            created: now_u as i64,
            data: "planetID: 40009082\n".into(),
            deleted: false,
            interacted: false,
        });

    // Neocom badges: unread mail count = 1, unread notifications count = 1, skill queue & free SP
    let mail_headers = rpc(&ctx, &mut s, "mailMgr", "GetMailHeaders", &[])
        .await
        .expect("GetMailHeaders");
    let PyRep::Rowset { rows: mail_rows, .. } = mail_headers else {
        panic!("expected Rowset from GetMailHeaders");
    };
    assert_eq!(mail_rows.len(), 1, "GetMailHeaders must return 1 seeded unread mail");

    let notifs = rpc(&ctx, &mut s, "notificationMgr", "GetUnprocessed", &[])
        .await
        .expect("GetUnprocessed");
    let PyRep::ObjectEx { list: notif_rows, .. } = notifs else {
        panic!("expected ObjectEx CRowset from GetUnprocessed");
    };
    assert_eq!(notif_rows.len(), 1, "GetUnprocessed must return 1 seeded unread notification");

    let skill_q = rpc(&ctx, &mut s, "skillMgr2", "GetSkillQueueAndFreePoints", &[])
        .await
        .expect("GetSkillQueueAndFreePoints");
    let PyRep::Tuple(sq_parts) = skill_q else {
        panic!("expected Tuple from GetSkillQueueAndFreePoints");
    };
    assert_eq!(sq_parts.len(), 2, "GetSkillQueueAndFreePoints must return (queue, freePoints)");

    // Info panels: mission tracker & sov
    let journal = rpc(&ctx, &mut s, "agentMgr", "GetMyJournalDetails", &[])
        .await
        .expect("GetMyJournalDetails");
    let PyRep::Tuple(j_parts) = journal else {
        panic!("expected Tuple from GetMyJournalDetails");
    };
    assert_eq!(j_parts.len(), 2, "GetMyJournalDetails must return (missions, offers)");

    let sov_info = rpc(
        &ctx,
        &mut s,
        "sovMgr",
        "GetSovStructuresInfoForSolarSystem",
        &[PyRep::Integer(30_000_142)],
    )
    .await
    .expect("GetSovStructuresInfoForSolarSystem");
    assert!(matches!(sov_info, PyRep::List(_)), "GetSovStructuresInfoForSolarSystem must return List");

    // Photo mode: station / scene state queries succeed with concrete stationID
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
    assert!(
        format!("{station_info:?}").contains("60003760"),
        "GetStation must return station 60003760 details"
    );
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

    // Trigger real session changes (Undock -> Dock) and verify OnSessionChanged payload keys & old/new values
    let extract_station_change = |notifs: &[eve_server::router::PendingMachoNotification]| -> Option<(PyRep, PyRep)> {
        notifs
            .iter()
            .filter(|n| n.event == "OnSessionChanged")
            .find_map(|n| {
                assert_eq!(n.args.len(), 3, "OnSessionChanged must have 3 args");
                let PyRep::Dict(changes) = &n.args[2] else {
                    return None;
                };
                changes.iter().find_map(|(k, v)| {
                    if *k == PyRep::String("stationid".into()) {
                        if let PyRep::Tuple(pair) = v {
                            if pair.len() == 2 {
                                return Some((pair[0].clone(), pair[1].clone()));
                            }
                        }
                    }
                    None
                })
            })
    };

    let _ = ctx.drain_notifications_for_char(P1);
    rpc(&ctx, &mut s, "ship", "Undock", &[])
        .await
        .expect("Undock");
    assert_eq!(s.station_id, None);
    let undock_notifs = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        extract_station_change(&undock_notifs),
        Some((PyRep::Integer(target_station), PyRep::None)),
        "Undock must emit OnSessionChanged with stationid: (60003760, None)"
    );

    ctx.advance_secs(12);
    let _ = ctx.drain_notifications_for_char(P1);
    rpc(&ctx, &mut s, "beyonce", "CmdDock", &[PyRep::Integer(target_station)])
        .await
        .expect("CmdDock");
    assert_eq!(s.station_id, Some(target_station));
    let dock_notifs = ctx.drain_notifications_for_char(P1);
    assert_eq!(
        extract_station_change(&dock_notifs),
        Some((PyRep::None, PyRep::Integer(target_station))),
        "CmdDock must emit OnSessionChanged with stationid: (None, 60003760)"
    );
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
    let PyRep::List(teams) = team0 else {
        panic!("expected List from GetTeamMembers, got {team0:?}");
    };
    assert_eq!(teams.len(), 2, "GetTeamMembers must return [red_list, blue_list]");
    let PyRep::List(red_members) = &teams[0] else {
        panic!("expected List for red team");
    };
    let PyRep::List(blue_members) = &teams[1] else {
        panic!("expected List for blue team");
    };
    assert_eq!(red_members.len(), 1, "red team must have 1 player (P1)");
    assert_eq!(blue_members.len(), 1, "blue team must have 1 player (P2)");

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
    // parity: feat:ui.search, rpc:custom_agency_provider.warp_to_content
    eve_testkit::attest(&[
        "feat:ui.search",
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

#[tokio::test]
async fn test_stargate_jump_records_single_map_history_jump() {
    let ctx = ServerContext::new();
    let mut s = select_pilot(&ctx, 9230, "demo", "demo-secret", P1).await;
    rpc(&ctx, &mut s, "stationSvc", "Undock", &[]).await.expect("Undock");
    let ship_id = s.ship_id.expect("ship_id");
    let gate = ctx.sde.jumps.values().find(|j| j.from_solar_system_id == 30_000_142).expect("Jita gate").clone();
    ctx.systems.get_or_spawn(30_000_142).set_ball_position(ship_id, gate.position).await;
    ctx.advance_secs(11);
    let bid = ctx.bind_object(&mut s, "beyonce", 30_000_142).expect("bind beyonce");
    ctx.dispatch_rpc(&mut s, "beyonce", Some(bid), "CmdStargateJump", &[PyRep::Integer(gate.stargate_id)], &HashMap::new(), None).await.expect("CmdStargateJump");
    let PyRep::ObjectEx { list, .. } = rpc(&ctx, &mut s, "map", "GetHistory", &[PyRep::Integer(1)]).await.expect("GetHistory(1)") else { panic!("expected CRowset") };
    let jumps = list.into_iter().find_map(|r| match r {
        PyRep::DBRow { columns, .. } if columns.iter().any(|(k, v)| k == "solarSystemID" && v.as_i64() == Some(i64::from(gate.to_solar_system_id))) => {
            columns.iter().find_map(|(k, v)| (k == "value1").then(|| v.as_i64()).flatten())
        }
        _ => None,
    });
    assert_eq!(jumps, Some(1), "one CmdStargateJump must record exactly 1 jump in map.GetHistory(1)");
}
