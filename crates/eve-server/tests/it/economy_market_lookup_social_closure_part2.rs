//! Live RPC integration tests for logon milestones, browser lockdown policy,
//! ship SKIN application, paperdoll apparel, Neocom badges, and star map stats.
#![forbid(unsafe_code)]
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;
fn dict_get<'a>(rep: &'a PyRep, key: &str) -> Option<&'a PyRep> {
    match rep {
        PyRep::Dict(entries) => entries.iter().find_map(|(k, v)| match k {
            PyRep::String(s) if s == key => Some(v),
            _ => None,
        }),
        PyRep::KeyVal(entries) => entries.iter().find_map(|(k, v)| (k == key).then_some(v)),
        PyRep::Object { args, .. } => dict_get(args, key),
        _ => None,
    }
}
fn extract_rowset_lines(rep: &PyRep) -> Vec<Vec<PyRep>> {
    match rep {
        PyRep::Rowset { rows, .. } => rows.clone(),
        other => panic!("expected PyRep::Rowset, got {other:?}"),
    }
}
#[tokio::test]
async fn test_milestone_logon_and_browser_lockdown_policy() {
    // parity: rpc:milestoneMgr.ProcessCharacterLogon, feat:ui.milestone-logon, feat:ui.browser-lockdown
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();
    let mut unauth = SessionState::new(4106);
    assert!(
        ctx.dispatch_rpc(
            &mut unauth,
            "milestoneMgr",
            None,
            "ProcessCharacterLogon",
            &[],
            &empty_kw,
            None,
        )
        .await
        .is_err()
    );
    assert!(
        ctx.dispatch_rpc(
            &mut unauth,
            "browserLockdownSvc",
            None,
            "IsBrowserInLockdown",
            &[],
            &empty_kw,
            None,
        )
        .await
        .is_err()
    );
    let mut sess = SessionState::new(4107);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    ctx.dispatch_rpc(
        &mut sess,
        "milestoneMgr",
        None,
        "ProcessCharacterLogon",
        &[],
        &empty_kw,
        None,
    )
    .await
    .expect("ProcessCharacterLogon");
    let first_logon_ts = ctx
        .db
        .with_snapshot(|snap| {
            eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap)
                .milestone_logons
                .get(&90_000_001)
                .copied()
        })
        .expect("milestone_logons must record character logon");
    assert!(first_logon_ts > 0);
    // Second session logon persists across sessions
    let mut sess_next = SessionState::new(4108);
    sess_next.user_id = Some(1);
    sess_next.character_id = Some(90_000_001);
    ctx.dispatch_rpc(&mut sess_next, "milestoneMgr", None, "ProcessCharacterLogon", &[], &empty_kw, None)
        .await
        .expect("ProcessCharacterLogon second session");
    let second_logon_ts = ctx
        .db
        .with_snapshot(|snap| {
            eve_server::services::cosmetics_ui_ops::load_cosmetics_ui_state(snap)
                .milestone_logons
                .get(&90_000_001)
                .copied()
        })
        .expect("milestone_logons persisted across sessions");
    assert!(second_logon_ts >= first_logon_ts);
    // Default policy
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "browserLockdownSvc", None, "IsBrowserInLockdown", &[], &empty_kw, None)
            .await
            .expect("IsBrowserInLockdown"),
        PyRep::Bool(false)
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "browserLockdownSvc", None, "GetDefaultHomePage", &[], &empty_kw, None)
            .await
            .expect("GetDefaultHomePage"),
        PyRep::String("https://www.eveonline.com/".into())
    );
    // Update persisted policy and verify regular user sees updated state
    ctx.db.with_mut_snapshot(|snap| {
        eve_server::services::notify_emitters_batch9_part2::set_browser_lockdown_policy(
            snap,
            true,
            "https://support.eveonline.com/",
            vec!["https://blocked.example.com".into()],
        );
    });
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "browserLockdownSvc", None, "IsBrowserInLockdown", &[], &empty_kw, None)
            .await
            .expect("IsBrowserInLockdown after"),
        PyRep::Bool(true)
    );
    assert_eq!(
        ctx.dispatch_rpc(&mut sess, "browserLockdownSvc", None, "GetFlaggedSitesList", &[], &empty_kw, None)
            .await
            .expect("GetFlaggedSitesList after"),
        PyRep::List(vec![PyRep::String("https://blocked.example.com".into())])
    );
}
#[tokio::test]
async fn test_cosmetics_skin_apply_apparel_and_neocom_badges() {
    // parity: feat:cosmetics.skin-apply, feat:cosmetics.apparel, feat:ui.neocom-badges
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();
    let mut sess = SessionState::new(4109);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.corporation_id = Some(98_000_001);
    sess.station_id = Some(60_003_760);
    // 1. Apply SKIN to active ship
    let (ship_id, ship_type_id) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&90_000_001).expect("char");
        let ship = snap.items.get(&ch.active_ship_id).expect("active ship");
        (ship.item_id, ship.type_id)
    });
    let skin_id = ctx
        .sde
        .skins
        .values()
        .find(|s| s.types.contains(&ship_type_id))
        .map(|s| s.skin_id)
        .expect("SDE skin for active ship");
    // Negative: unlicensed SKIN rejected
    let unlic_err = ctx
        .dispatch_rpc(
            &mut sess,
            "shipCosmeticsMgr",
            None,
            "ApplySkinToShip",
            &[PyRep::Integer(ship_id), PyRep::Integer(skin_id as i64)],
            &empty_kw,
            None,
        )
        .await
        .expect_err("unlicensed SKIN must fail");
    assert_eq!(unlic_err.code, "SkinNotLicensed");
    // Negative: wrong ship type SKIN rejected
    if let Some(wrong_skin) = ctx.sde.skins.values().find(|s| !s.types.contains(&ship_type_id)) {
        let wrong_err = ctx
            .dispatch_rpc(
                &mut sess,
                "shipCosmeticsMgr",
                None,
                "ApplySkinToShip",
                &[PyRep::Integer(ship_id), PyRep::Integer(wrong_skin.skin_id as i64)],
                &empty_kw,
                None,
            )
            .await
            .expect_err("incompatible ship type SKIN must fail");
        assert_eq!(wrong_err.code, "InvalidShipTypeForSkin");
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.licensed_skins
            .entry(90_000_001)
            .or_default()
            .push(skin_id);
    });
    let _ = sess.drain_notifications();
    ctx.dispatch_rpc(
        &mut sess,
        "shipCosmeticsMgr",
        None,
        "ApplySkinToShip",
        &[PyRep::Integer(ship_id), PyRep::Integer(skin_id as i64)],
        &empty_kw,
        None,
    )
    .await
    .expect("ApplySkinToShip");
    let applied = ctx.db.with_snapshot(|snap| snap.applied_ship_skins.get(&ship_id).copied());
    assert_eq!(applied, Some(skin_id), "applied_ship_skins must record exact active ship SKIN");
    let skin_notifs = sess.drain_notifications();
    assert!(
        skin_notifs.iter().any(|n| n.0 == "OnCurrentShipSkinChange"),
        "ApplySkinToShip must push OnCurrentShipSkinChange notification"
    );
    // 2. Paperdoll apparel update & persistence
    let bad_doll = PyRep::keyval([("apparel", PyRep::List(vec![PyRep::Integer(999_999_999)]))]);
    let bad_apparel_err = ctx
        .dispatch_rpc(
            &mut sess,
            "paperDollServer",
            None,
            "UpdateExistingCharacterLimited",
            &[PyRep::Integer(90_000_001), bad_doll, PyRep::None],
            &empty_kw,
            None,
        )
        .await
        .expect_err("unowned apparel must fail");
    assert_eq!(bad_apparel_err.code, "InvalidApparelItem");
    let apparel_type_id = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 30)
        .map(|t| t.type_id)
        .expect("SDE apparel type (category 30)");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            990_000_777,
            eve_db::InventoryItemRecord {
                item_id: 990_000_777,
                type_id: apparel_type_id,
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: eve_db::InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    let doll_payload = PyRep::keyval([
        ("apparel", PyRep::List(vec![PyRep::Integer(i64::from(apparel_type_id))])),
        ("colors", PyRep::keyval([("skin", PyRep::Integer(7))])),
    ]);
    ctx.dispatch_rpc(
        &mut sess,
        "paperDollServer",
        None,
        "UpdateExistingCharacterLimited",
        &[PyRep::Integer(90_000_001), doll_payload, PyRep::None],
        &empty_kw,
        None,
    )
    .await
    .expect("UpdateExistingCharacterLimited");
    let doll_data = ctx
        .dispatch_rpc(
            &mut sess,
            "paperDollServer",
            None,
            "GetMyPaperDollData",
            &[],
            &empty_kw,
            None,
        )
        .await
        .expect("GetMyPaperDollData");
    let doll_str = dict_get(&doll_data, "dollInfo")
        .and_then(|v| v.as_str())
        .expect("dollInfo string");
    assert!(!doll_str.starts_with("bloodline:"), "dollInfo must reflect updated payload, not default: {doll_str}");
    assert!(doll_str.contains("colors"), "dollInfo must round-trip updated payload: {doll_str}");
    let Some(PyRep::List(worn)) = dict_get(&doll_data, "wornApparel") else {
        panic!("expected wornApparel list");
    };
    assert!(worn.contains(&PyRep::Integer(i64::from(apparel_type_id))));
    // 3. Neocom badges: unread mail & unprocessed notifications
    let mut sess2 = SessionState::new(4110);
    sess2.user_id = Some(2);
    sess2.character_id = Some(90_000_002);
    let mail_id = ctx
        .dispatch_rpc(
            &mut sess2,
            "mailMgr",
            None,
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(90_000_001)]),
                PyRep::String("Neocom Badge Test".into()),
                PyRep::String("Body".into()),
                PyRep::Integer(0),
                PyRep::Integer(0),
            ],
            &empty_kw,
            None,
        )
        .await
        .expect("SendMail")
        .as_i64()
        .expect("mail_id");
    let headers_before = ctx
        .dispatch_rpc(&mut sess, "mailMgr", None, "GetMailHeaders", &[], &empty_kw, None)
        .await
        .expect("GetMailHeaders");
    let hdr_rows_before = extract_rowset_lines(&headers_before);
    assert!(
        hdr_rows_before
            .iter()
            .any(|r| r.first() == Some(&PyRep::Integer(mail_id)) && r.get(7) == Some(&PyRep::Bool(false))),
        "new mail must be unread for Neocom badge"
    );
    ctx.dispatch_rpc(
        &mut sess,
        "mailMgr",
        None,
        "MarkAsRead",
        &[PyRep::List(vec![PyRep::Integer(mail_id)])],
        &empty_kw,
        None,
    )
    .await
    .expect("MarkAsRead");
    let headers_after = ctx
        .dispatch_rpc(&mut sess, "mailMgr", None, "GetMailHeaders", &[], &empty_kw, None)
        .await
        .expect("GetMailHeaders after");
    let hdr_rows_after = extract_rowset_lines(&headers_after);
    assert!(
        hdr_rows_after
            .iter()
            .any(|r| r.first() == Some(&PyRep::Integer(mail_id)) && r.get(7) == Some(&PyRep::Bool(true))),
        "mail must be marked read"
    );
}
#[tokio::test]
async fn test_map_stats_star_map_data_and_info_panels() {
    // parity: feat:ui.star-map-data, feat:ui.map-stats, feat:ui.info-panels
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let empty_kw = HashMap::new();
    let mut unauth = SessionState::new(4110);
    assert!(
        ctx.dispatch_rpc(&mut unauth, "map", None, "GetSolarSystemVisits", &[], &empty_kw, None)
            .await
            .is_err()
    );
    let mut sess = SessionState::new(4111);
    sess.user_id = Some(1);
    sess.character_id = Some(90_000_001);
    sess.solar_system_id = Some(30_000_142);
    let expected_jita_stations = ctx
        .sde
        .stations
        .values()
        .filter(|s| s.solar_system_id == 30_000_142)
        .count() as i64;
    assert!(expected_jita_stations > 0);
    let station_counts = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetStationCount", &[], &empty_kw, None)
        .await
        .expect("GetStationCount");
    let PyRep::Dict(entries) = &station_counts else {
        panic!("unexpected GetStationCount shape: {station_counts:?}");
    };
    assert!(
        entries.iter().any(|(k, v)| k == &PyRep::Integer(30_000_142) && v == &PyRep::Integer(expected_jita_stations)),
        "GetStationCount must match SDE station count for Jita"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs.insert(
            "char_system_visits:90000001".to_string(),
            r#"{"30000142":{"visits":7,"last_visited_ticks":133000000000000000}}"#.to_string(),
        );
        snap.killmails.insert(
            990_001,
            eve_db::KillmailRecord {
                kill_id: 990_001,
                victim_character_id: 90_000_002,
                attacker_character_id: 90_000_001,
                solar_system_id: 30_000_142,
                ship_type_id: 587,
                total_value_isk: 1_500_000.0,
                killed_unix: 1_700_000_000,
                ..Default::default()
            },
        );
    });
    let PyRep::List(visits) = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetSolarSystemVisits", &[], &empty_kw, None)
        .await
        .expect("GetSolarSystemVisits")
    else {
        panic!("expected GetSolarSystemVisits list");
    };
    assert!(
        visits.iter().any(|v| {
            dict_get(v, "solarSystemID") == Some(&PyRep::Integer(30_000_142))
                && dict_get(v, "visits") == Some(&PyRep::Integer(7))
        }),
        "GetSolarSystemVisits must return persisted visit count for 30000142"
    );
    let history = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetHistory", &[PyRep::Integer(1), PyRep::Integer(24)], &empty_kw, None)
        .await
        .expect("GetHistory");
    let PyRep::ObjectEx { list, .. } = &history else {
        panic!("unexpected GetHistory shape: {history:?}");
    };
    assert!(
        list.iter().any(|row| match row {
            PyRep::DBRow { columns, .. } => {
                columns.iter().any(|(k, v)| k == "solarSystemID" && v == &PyRep::Integer(30_000_142))
                    && columns.iter().any(|(k, v)| k == "value1" && v.as_i64().unwrap_or(0) >= 1)
            }
            _ => false,
        }),
        "GetHistory must report >= 1 ship kill in 30000142"
    );
    let fw_info = ctx
        .dispatch_rpc(&mut sess, "map", None, "GetFacWarZoneInfo", &[PyRep::Integer(500_001)], &empty_kw, None)
        .await
        .expect("GetFacWarZoneInfo");
    assert_eq!(dict_get(&fw_info, "maxWarZonePoints"), Some(&PyRep::Integer(100_000)));
    let Some(PyRep::List(fw_systems)) = dict_get(&fw_info, "systems") else {
        panic!("expected systems list in GetFacWarZoneInfo");
    };
    assert!(!fw_systems.is_empty(), "GetFacWarZoneInfo must return warzone systems");
    assert!(dict_get(&fw_systems[0], "ownerFactionID").is_some());
}
