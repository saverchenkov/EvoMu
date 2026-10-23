//! `D-6` Social Durability SQLite restart integration tests:
//! - `feat:lifecycle.contacts-persist`
//! - `feat:lifecycle.mail-state-persist`
//! - `feat:lifecycle.bookmarks-persist`
//! - `feat:lifecycle.chat-channels-persist`
//! - `feat:lifecycle.social-durable`

#![forbid(unsafe_code)]

use crate::common;
use eve_db::WorldRepository;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::char_social_mail_routes::dispatch_char_social_mail_route;
use eve_server::esi::char_social_routes::dispatch_char_social_route;
use eve_server::esi::character_mail_routes::dispatch_character_mail_routes;
use eve_server::router::ServerContext;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn temp_sqlite_path(tag: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("evomu_d6_social_{}_{}.sqlite", tag, std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn seed_repo_with_corp(repo: &WorldRepository) {
    repo.with_mut_snapshot(|snap| {
        *snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        snap.corporations.insert(
            98_000_001,
            eve_db::CorporationRecord {
                corporation_id: 98_000_001,
                name: "D6 Social Corp".into(),
                ticker: "D6SOC".into(),
                ceo_id: 90_000_001,
                alliance_id: Some(99_000_001),
                tax_rate: 0.05,
                master_wallet: eve_db::IskCents(0),
                members: HashSet::from([90_000_001, 90_000_002]),
                member_roles: HashMap::from([(90_000_001, u64::MAX)]),
                wallet_divisions: Vec::new(),
                applications: HashMap::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(60_003_760),
            },
        );
        snap.alliances.insert(
            99_000_001,
            eve_db::AllianceRecord {
                alliance_id: 99_000_001,
                name: "D6 Social Alliance".into(),
                short_name: "D6ALY".into(),
                executor_corp_id: 98_000_001,
                member_corp_ids: HashSet::from([98_000_001]),
            },
        );
        for cid in [90_000_001, 90_000_002] {
            if let Some(ch) = snap.characters.get_mut(&cid) {
                ch.corporation_id = 98_000_001;
                ch.alliance_id = Some(99_000_001);
            }
        }
    });
}

async fn call(ctx: &ServerContext, sess: &mut SessionState, svc: &str, method: &str, args: &[PyRep]) -> PyRep {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, svc, None, method, args, &kw, None)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {e:?}"))
}

async fn login_and_select(ctx: &ServerContext, sess_id: u64, username: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, username, "demo-secret").await;
    call(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)]).await;
    sess.alliance_id = Some(99_000_001);
    sess
}

#[tokio::test]
async fn test_d6_contacts_labels_blocklist_cspa_persist_across_sqlite_restart() {
    // parity: feat:lifecycle.contacts-persist, feat:lifecycle.social-durable
    let db_path = temp_sqlite_path("contacts");
    let kw = HashMap::new();

    let (char_label_id, corp_label_id, ally_label_id, corp_id) = {
        let repo1 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
        seed_repo_with_corp(&repo1);
        let ctx1 = ServerContext::with_repository(repo1);
        common::pin_clock(&ctx1);

        let mut sess1 = login_and_select(&ctx1, 7101, "demo", 90_000_001).await;
        let corp_id = sess1.corporation_id.unwrap_or(98_000_001);

        // 1. Personal contact label + contact + watchlist + blocklist + CSPA charge + notes
        let lbl_rep = call(&ctx1, &mut sess1, "charMgr", "CreateLabel", &[PyRep::String("Trusted Friends".into()), PyRep::Integer(0x00FF00)]).await;
        let char_label_id = lbl_rep.dict_get("labelID").and_then(|v| v.as_i64()).unwrap_or(1);

        call(&ctx1, &mut sess1, "charMgr", "AddContact", &[PyRep::Integer(90_000_002), PyRep::Real(10.0), PyRep::Bool(true), PyRep::Integer(char_label_id)]).await;
        call(&ctx1, &mut sess1, "charMgr", "BlockOwners", &[PyRep::List(vec![PyRep::Integer(90_000_003)])]).await;
        call(&ctx1, &mut sess1, "charMgr", "LogSettings", &[PyRep::Real(5_000.0)]).await;
        call(&ctx1, &mut sess1, "charMgr", "SetNote", &[PyRep::Integer(90_000_002), PyRep::String("Reliable fleet scout".into())]).await;
        call(&ctx1, &mut sess1, "charMgr", "AddOwnerNote", &[PyRep::Integer(90_000_001), PyRep::String("S:Hangar Checklist".into()), PyRep::String("Bring nanite paste".into())]).await;

        // 2. Corporate contact + label
        let corp_lbl = call(&ctx1, &mut sess1, "corpRegistry", "CreateLabel", &[PyRep::String("Blue Coalition".into()), PyRep::Integer(0x0055FF)]).await;
        let corp_label_id = corp_lbl.dict_get("labelID").and_then(|v| v.as_i64()).unwrap_or(1);
        call(&ctx1, &mut sess1, "corpRegistry", "AddCorporateContact", &[PyRep::Integer(90_000_002), PyRep::Real(8.5), PyRep::Integer(corp_label_id)]).await;

        // 3. Alliance contact + label
        let ally_lbl = call(&ctx1, &mut sess1, "allianceRegistry", "CreateLabel", &[PyRep::String("Pact Partners".into()), PyRep::Integer(0xFFAA00)]).await;
        let ally_label_id = ally_lbl.dict_get("labelID").and_then(|v| v.as_i64()).unwrap_or(1);
        call(&ctx1, &mut sess1, "allianceRegistry", "AddAllianceContact", &[PyRep::Integer(90_000_002), PyRep::Real(7.5), PyRep::Integer(ally_label_id)]).await;

        (char_label_id, corp_label_id, ally_label_id, corp_id)
    };

    // Restart server from the SQLite file on disk
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
    let ctx2 = ServerContext::with_repository(repo2);
    common::pin_clock(&ctx2);

    let mut sess1 = login_and_select(&ctx2, 7102, "demo", 90_000_001).await;
    let contacts_kv = call(&ctx2, &mut sess1, "charMgr", "GetContacts", &[]).await;
    let PyRep::Rowset { rows: addr_rows, .. } = contacts_kv.dict_get("addresses").expect("addresses") else {
        panic!("expected addresses rowset");
    };
    assert_eq!(
        addr_rows,
        &vec![vec![
            PyRep::Integer(90_000_002),
            PyRep::Bool(true),
            PyRep::Real(10.0),
            PyRep::Integer(char_label_id),
        ]],
        "GetContacts addresses must preserve exact contactID, watchlist, standing, and labelMask and exclude blocked-only non-contacts"
    );
    let PyRep::Rowset { rows: blocked_rows, .. } = contacts_kv.dict_get("blocked").expect("blocked") else {
        panic!("expected blocked rowset");
    };
    assert_eq!(
        blocked_rows,
        &vec![vec![PyRep::Integer(90_000_003)]],
        "GetContacts blocked must contain blocked owner 90_000_003 after restart"
    );

    let labels_rep = call(&ctx2, &mut sess1, "charMgr", "GetLabels", &[]).await;
    assert!(
        format!("{labels_rep:?}").contains("Trusted Friends")
            && format!("{labels_rep:?}").contains(&char_label_id.to_string()),
        "Personal contact labels must survive SQLite restart"
    );

    let note_rep = call(&ctx2, &mut sess1, "charMgr", "GetNote", &[PyRep::Integer(90_000_002)]).await;
    assert_eq!(note_rep.as_str(), Some("Reliable fleet scout"));

    let owner_notes = call(&ctx2, &mut sess1, "charMgr", "GetOwnerNoteLabels", &[]).await;
    assert!(
        format!("{owner_notes:?}").contains("S:Hangar Checklist"),
        "Owner notes must survive SQLite restart"
    );

    let corp_contacts = call(&ctx2, &mut sess1, "corpRegistry", "GetCorporateContacts", &[]).await;
    assert!(
        format!("{corp_contacts:?}").contains("90000002")
            && format!("{corp_contacts:?}").contains("8.5")
            && format!("{corp_contacts:?}").contains(&corp_label_id.to_string()),
        "Corporate contacts and labels must survive SQLite restart"
    );

    let ally_contacts = call(&ctx2, &mut sess1, "allianceRegistry", "GetAllianceContacts", &[]).await;
    assert!(
        format!("{ally_contacts:?}").contains("90000002")
            && format!("{ally_contacts:?}").contains("7.5")
            && format!("{ally_contacts:?}").contains(&ally_label_id.to_string()),
        "Alliance contacts must survive SQLite restart"
    );
    let ally_labels = call(&ctx2, &mut sess1, "allianceRegistry", "GetLabels", &[]).await;
    assert!(
        format!("{ally_labels:?}").contains("Pact Partners")
            && format!("{ally_labels:?}").contains(&ally_label_id.to_string()),
        "Alliance contact labels must survive SQLite restart"
    );

    // Verify ESI also sees the restored character, corporation & alliance contacts and labels
    let params = HashMap::from([("character_id".to_string(), "90000001".to_string())]);
    let esi_contacts = dispatch_char_social_mail_route(&ctx2, "GET", "/characters/{character_id}/contacts", &params, &HashMap::new(), b"")
        .expect("ESI GET /characters/{character_id}/contacts");
    assert_eq!(esi_contacts.status_code, 200);
    let esi_char_body = esi_contacts.body.as_ref().unwrap().to_string();
    assert!(esi_char_body.contains("90000002") && esi_char_body.contains("10.0"));

    let corp_params = HashMap::from([("corporation_id".to_string(), corp_id.to_string())]);
    let esi_corp_contacts = dispatch_char_social_mail_route(&ctx2, "GET", "/corporations/{corporation_id}/contacts", &corp_params, &HashMap::new(), b"")
        .expect("ESI GET /corporations/{corporation_id}/contacts");
    assert_eq!(esi_corp_contacts.status_code, 200);
    assert!(esi_corp_contacts.body.as_ref().unwrap().to_string().contains("90000002"));

    let ally_params = HashMap::from([("alliance_id".to_string(), "99000001".to_string())]);
    let esi_ally_contacts = dispatch_char_social_mail_route(&ctx2, "GET", "/alliances/{alliance_id}/contacts", &ally_params, &HashMap::new(), b"")
        .expect("ESI GET /alliances/{alliance_id}/contacts");
    assert_eq!(esi_ally_contacts.status_code, 200);
    assert!(esi_ally_contacts.body.as_ref().unwrap().to_string().contains("90000002"));

    let esi_ally_labels = dispatch_char_social_mail_route(&ctx2, "GET", "/alliances/{alliance_id}/contacts/labels", &ally_params, &HashMap::new(), b"")
        .expect("ESI GET /alliances/{alliance_id}/contacts/labels");
    assert_eq!(esi_ally_labels.status_code, 200);
    assert!(esi_ally_labels.body.as_ref().unwrap().to_string().contains("Pact Partners"));

    // Verify blocked sender (90_000_003) cannot send mail to 90_000_001 after restart
    let mut sess3 = SessionState::new(7103);
    sess3.user_id = Some(3);
    sess3.character_id = Some(90_000_003);
    sess3.corporation_id = Some(1_000_009);
    let blocked_mail = ctx2
        .dispatch_rpc(
            &mut sess3,
            "mailMgr",
            None,
            "SendMail",
            &[PyRep::List(vec![PyRep::Integer(90_000_001)]), PyRep::None, PyRep::Integer(0), PyRep::String("Spam".into()), PyRep::String("Blocked body".into())],
            &kw,
            None,
        )
        .await;
    assert!(blocked_mail.is_err(), "Blocked sender must still be refused by mailMgr.SendMail after SQLite restart");

    // Verify friendly contact (90_000_002) gets CSPA exemption when contacting 90_000_001 after restart
    let mut sess2 = SessionState::new(7104);
    sess2.user_id = Some(2);
    sess2.character_id = Some(90_000_002);
    sess2.corporation_id = Some(1_000_009);
    let cspa_res = call(&ctx2, &mut sess2, "charMgr", "CSPAChargedAction", &[PyRep::Integer(90_000_001)]).await;
    assert_eq!(cspa_res, PyRep::Real(0.0), "Friendly contact must remain exempt from CSPA charge after SQLite restart");

    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_d6_mail_state_labels_trash_mailing_lists_and_calendar_persist_across_sqlite_restart() {
    // parity: feat:lifecycle.mail-state-persist, feat:lifecycle.social-durable
    let db_path = temp_sqlite_path("mail_calendar");

    let (mail_read_id, mail_trash_id, mail_del_id, mail_unread_id, custom_lbl_id, list_id, event_id) = {
        let repo1 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
        seed_repo_with_corp(&repo1);
        let ctx1 = ServerContext::with_repository(repo1);
        common::pin_clock(&ctx1);

        let mut sess1 = login_and_select(&ctx1, 7201, "demo", 90_000_001).await;
        let mut sess2 = SessionState::new(7202);
        sess2.user_id = Some(2);
        sess2.character_id = Some(90_000_002);
        sess2.corporation_id = Some(98_000_001);

        let ml_rep = call(
            &ctx1,
            &mut sess1,
            "mailingListsMgr",
            "Create",
            &[PyRep::String("Vanguard Dispatch".into()), PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(250)],
        )
        .await;
        let list_id = ml_rep.dict_get("listID").and_then(|v| v.as_i64()).or_else(|| ml_rep.as_i64()).unwrap_or(700_001);

        call(&ctx1, &mut sess1, "mailingListsMgr", "SaveWelcomeMail", &[PyRep::Integer(list_id), PyRep::String("Welcome to Vanguard".into()), PyRep::String("Stay on comms".into())]).await;
        call(&ctx1, &mut sess2, "mailingListsMgr", "Join", &[PyRep::String("Vanguard Dispatch".into())]).await;
        call(&ctx1, &mut sess2, "mailMgr", "MarkAllAsRead", &[]).await;
        call(&ctx1, &mut sess1, "mailingListsMgr", "SetMembersOperator", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(90_000_002)])]).await;

        let mut sent_ids = Vec::new();
        for idx in 1..=4 {
            let mid = call(
                &ctx1,
                &mut sess1,
                "mailMgr",
                "SendMail",
                &[PyRep::List(vec![PyRep::Integer(90_000_002)]), PyRep::None, PyRep::Integer(0), PyRep::String(format!("Subject {idx}")), PyRep::String(format!("Body {idx}"))],
            )
            .await
            .as_i64()
            .expect("mail_id");
            sent_ids.push(mid);
        }
        let (m1, m2, m3, m4) = (sent_ids[0], sent_ids[1], sent_ids[2], sent_ids[3]);

        let lbl_rep = call(&ctx1, &mut sess2, "mailMgr", "CreateLabel", &[PyRep::String("Fleet Intel".into()), PyRep::Integer(4)]).await;
        let custom_lbl_id = lbl_rep.dict_get("labelID").and_then(|v| v.as_i64()).or_else(|| lbl_rep.as_i64()).unwrap_or(16);

        // Assign Sent (2), Corp (4), Alliance (8), and custom label to m1, then mark m1 read
        call(&ctx1, &mut sess2, "mailMgr", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(m1)]), PyRep::List(vec![PyRep::Integer(2), PyRep::Integer(4), PyRep::Integer(8), PyRep::Integer(custom_lbl_id)])]).await;
        call(&ctx1, &mut sess2, "mailMgr", "MarkAsRead", &[PyRep::List(vec![PyRep::Integer(m1)])]).await;

        // Assign Corp (4), Alliance (8), and custom label to m4, and leave m4 UNREAD
        call(&ctx1, &mut sess2, "mailMgr", "AssignLabels", &[PyRep::List(vec![PyRep::Integer(m4)]), PyRep::List(vec![PyRep::Integer(4), PyRep::Integer(8), PyRep::Integer(custom_lbl_id)])]).await;

        call(&ctx1, &mut sess2, "mailMgr", "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(m2)])]).await;
        call(&ctx1, &mut sess2, "mailMgr", "DeleteMail", &[PyRep::List(vec![PyRep::Integer(m3)])]).await;

        let ev_rep = call(
            &ctx1,
            &mut sess1,
            "calendarMgr",
            "CreatePersonalEvent",
            &[PyRep::Integer(133_500_000_000_000_000), PyRep::Integer(90), PyRep::String("Stratop Formup".into()), PyRep::String("Bring doctrines".into()), PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(90_000_002)])],
        )
        .await;
        let event_id = ev_rep.dict_get("eventID").and_then(|v| v.as_i64()).unwrap_or(700_002);
        call(&ctx1, &mut sess2, "calendarMgr", "SendEventResponse", &[PyRep::Integer(event_id), PyRep::Integer(4)]).await;

        (m1, m2, m3, m4, custom_lbl_id, list_id, event_id)
    };

    // Restart server from disk
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
    let ctx2 = ServerContext::with_repository(repo2);
    common::pin_clock(&ctx2);

    let mut sess2 = SessionState::new(7203);
    sess2.user_id = Some(2);
    sess2.character_id = Some(90_000_002);
    sess2.corporation_id = Some(98_000_001);

    let headers = call(&ctx2, &mut sess2, "mailMgr", "GetMailHeaders", &[]).await;
    let PyRep::Rowset { rows: hdr_rows, .. } = &headers else { panic!("expected headers rowset") };
    let m1_row = hdr_rows.iter().find(|r| r[0] == PyRep::Integer(mail_read_id)).expect("m1 in headers");
    assert_eq!(m1_row[5], PyRep::Bool(true), "m1 read status must be true after restart");
    assert_eq!(
        m1_row[6],
        PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(4), PyRep::Integer(8), PyRep::Integer(custom_lbl_id)]),
        "m1 assigned labels (Inbox, Sent, Corp, Alliance, custom) must survive restart"
    );
    let m4_row = hdr_rows.iter().find(|r| r[0] == PyRep::Integer(mail_unread_id)).expect("m4 in headers");
    assert_eq!(m4_row[5], PyRep::Bool(false), "m4 read status must be false after restart");
    assert!(
        !hdr_rows.iter().any(|r| r[0] == PyRep::Integer(mail_trash_id) || r[0] == PyRep::Integer(mail_del_id)),
        "Trashed and deleted mails must not appear in active inbox headers after restart"
    );

    let labels = call(&ctx2, &mut sess2, "mailMgr", "GetLabels", &[]).await;
    let PyRep::Rowset { rows: lbl_rows, .. } = &labels else { panic!("expected labels rowset") };
    let unread_for = |lid: i64| -> i64 {
        lbl_rows.iter().find(|r| r[0] == PyRep::Integer(lid)).and_then(|r| r[3].as_i64()).unwrap_or(-1)
    };
    assert_eq!(unread_for(1), 1, "Inbox unreadCount must be 1 after restart");
    assert_eq!(unread_for(2), 0, "Sent unreadCount must be 0 after restart");
    assert_eq!(unread_for(4), 1, "Corp unreadCount must be 1 after restart");
    assert_eq!(unread_for(8), 1, "Alliance unreadCount must be 1 after restart");
    assert_eq!(unread_for(custom_lbl_id), 1, "Custom label unreadCount must be 1 after restart");

    // Verify ESI mail headers, mail labels/unread counts, and mailing lists after restart
    let char2_params = HashMap::from([("character_id".to_string(), "90000002".to_string())]);
    let esi_mail_hdrs = dispatch_character_mail_routes(&ctx2, "GET", "/characters/{character_id}/mail", &char2_params, &HashMap::new(), None, 1, b"")
        .expect("ESI GET /characters/{character_id}/mail");
    assert_eq!(esi_mail_hdrs.status_code, 200);
    let esi_hdrs_arr = esi_mail_hdrs.body.as_ref().unwrap().as_array().expect("mail array");
    let esi_m1 = esi_hdrs_arr.iter().find(|m| m["mail_id"] == mail_read_id).expect("m1 in ESI mail");
    assert_eq!(esi_m1["is_read"], serde_json::json!(true));
    assert_eq!(esi_m1["labels"], serde_json::json!([1, 2, 4, 8, custom_lbl_id]));
    let esi_m4 = esi_hdrs_arr.iter().find(|m| m["mail_id"] == mail_unread_id).expect("m4 in ESI mail");
    assert_eq!(esi_m4["is_read"], serde_json::json!(false));
    assert!(
        !esi_hdrs_arr.iter().any(|m| m["mail_id"] == mail_trash_id || m["mail_id"] == mail_del_id),
        "Trashed (m2) and deleted (m3) mails must not appear in ESI mail headers"
    );

    let esi_mail_lbls = dispatch_char_social_mail_route(&ctx2, "GET", "/characters/{character_id}/mail/labels", &char2_params, &HashMap::new(), b"")
        .expect("ESI GET /characters/{character_id}/mail/labels");
    assert_eq!(esi_mail_lbls.status_code, 200);
    let esi_lbls_json = esi_mail_lbls.body.as_ref().unwrap();
    assert_eq!(esi_lbls_json["total_unread_count"], serde_json::json!(1));
    let esi_lbl_unread = |lid: i64| -> i64 {
        esi_lbls_json["labels"].as_array().unwrap().iter().find(|l| l["label_id"] == lid).and_then(|l| l["unread_count"].as_i64()).unwrap_or(-1)
    };
    assert_eq!((esi_lbl_unread(1), esi_lbl_unread(2), esi_lbl_unread(4), esi_lbl_unread(8), esi_lbl_unread(custom_lbl_id)), (1, 0, 1, 1, 1));

    // Verify post-restart mailMgr.MarkAsUnread and mailMgr.EditLabel propagate immediately to ESI without split-brain
    call(&ctx2, &mut sess2, "mailMgr", "MarkAsUnread", &[PyRep::List(vec![PyRep::Integer(mail_read_id)])]).await;
    call(&ctx2, &mut sess2, "mailMgr", "EditLabel", &[PyRep::Integer(custom_lbl_id), PyRep::String("Fleet Intel Updated".into()), PyRep::String("#00ff33".into())]).await;
    let esi_hdrs_after_unread = dispatch_character_mail_routes(&ctx2, "GET", "/characters/{character_id}/mail", &char2_params, &HashMap::new(), None, 1, b"")
        .expect("ESI GET /characters/{character_id}/mail after MarkAsUnread");
    let esi_m1_unread = esi_hdrs_after_unread.body.as_ref().unwrap().as_array().unwrap().iter().find(|m| m["mail_id"] == mail_read_id).unwrap();
    assert_eq!(esi_m1_unread["is_read"], serde_json::json!(false), "MarkAsUnread after restart must update ESI is_read to false");
    let esi_lbls_after_edit = dispatch_char_social_mail_route(&ctx2, "GET", "/characters/{character_id}/mail/labels", &char2_params, &HashMap::new(), b"")
        .expect("ESI GET /characters/{character_id}/mail/labels after EditLabel");
    let esi_lbls_after_json = esi_lbls_after_edit.body.as_ref().unwrap();
    assert_eq!(esi_lbls_after_json["total_unread_count"], serde_json::json!(2));
    assert!(esi_lbls_after_json.to_string().contains("Fleet Intel Updated"), "EditLabel after restart must reflect in ESI mail labels");

    let esi_mail_lists = dispatch_char_social_mail_route(&ctx2, "GET", "/characters/{character_id}/mail/lists", &char2_params, &HashMap::new(), b"")
        .expect("ESI GET /characters/{character_id}/mail/lists");
    assert_eq!(esi_mail_lists.status_code, 200);
    let esi_lists_str = esi_mail_lists.body.as_ref().unwrap().to_string();
    assert!(esi_lists_str.contains("Vanguard Dispatch") && esi_lists_str.contains(&list_id.to_string()));

    let restored = call(&ctx2, &mut sess2, "mailMgr", "MoveFromTrash", &[PyRep::List(vec![PyRep::Integer(mail_trash_id)])]).await;
    assert!(format!("{restored:?}").contains("true"));
    let headers_after_restore = call(&ctx2, &mut sess2, "mailMgr", "GetMailHeaders", &[]).await;
    assert!(format!("{headers_after_restore:?}").contains(&mail_trash_id.to_string()));

    let joined_lists = call(&ctx2, &mut sess2, "mailingListsMgr", "GetJoinedLists", &[]).await;
    assert!(format!("{joined_lists:?}").contains("Vanguard Dispatch"));

    let members = call(&ctx2, &mut sess2, "mailingListsMgr", "GetMembers", &[PyRep::Integer(list_id)]).await;
    let PyRep::Rowset { rows: mem_rows, .. } = &members else { panic!("expected members rowset") };
    assert!(mem_rows.contains(&vec![PyRep::Integer(90_000_002), PyRep::Integer(2)]), "Member 90_000_002 must retain operator role (2) after restart: {mem_rows:?}");

    let settings = call(&ctx2, &mut sess2, "mailingListsMgr", "GetSettings", &[PyRep::Integer(list_id)]).await;
    assert_eq!(settings.dict_get("defaultAccess"), Some(&PyRep::Integer(1)));
    assert_eq!(settings.dict_get("defaultRole"), Some(&PyRep::Integer(0)));
    assert_eq!(settings.dict_get("cost"), Some(&PyRep::Integer(250)));

    let welcome = call(&ctx2, &mut sess2, "mailingListsMgr", "GetWelcomeMail", &[PyRep::Integer(list_id)]).await;
    assert!(format!("{welcome:?}").contains("Welcome to Vanguard"));

    let ev_details = call(&ctx2, &mut sess2, "calendarMgr", "GetEventDetails", &[PyRep::Integer(event_id)]).await;
    assert!(format!("{ev_details:?}").contains("Stratop Formup") && format!("{ev_details:?}").contains("Bring doctrines"));

    let esi_params = HashMap::from([("character_id".to_string(), "90000002".to_string()), ("event_id".to_string(), event_id.to_string())]);
    let esi_att = dispatch_char_social_route(&ctx2, "GET", "/characters/{character_id}/calendar/{event_id}/attendees", &esi_params, &HashMap::new(), b"")
        .expect("ESI calendar attendees after restart");
    assert_eq!(esi_att.status_code, 200);
    assert!(esi_att.body.as_ref().unwrap().to_string().contains("accepted"));

    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_d6_bookmarks_folders_subfolders_acls_and_expiry_persist_across_sqlite_restart() {
    // parity: feat:lifecycle.bookmarks-persist, feat:lifecycle.social-durable
    let db_path = temp_sqlite_path("bookmarks");

    let (folder_id, subfolder_id, empty_subfolder_id, perm_bm_id, temp_bm_id) = {
        let repo1 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
        seed_repo_with_corp(&repo1);
        let ctx1 = ServerContext::with_repository(repo1);
        common::pin_clock(&ctx1);

        let mut sess1 = login_and_select(&ctx1, 7301, "demo", 90_000_001).await;
        let mut sess2 = SessionState::new(7302);
        sess2.user_id = Some(2);
        sess2.character_id = Some(90_000_002);
        sess2.corporation_id = Some(98_000_001);

        let f_rep = call(
            &ctx1,
            &mut sess1,
            "accessGroupBookmarkMgr",
            "AddFolder",
            &[PyRep::String("Wormhole Chain".into()), PyRep::Bool(true), PyRep::List(vec![PyRep::Integer(90_000_002), PyRep::Integer(98_000_001)])],
        )
        .await;
        let folder_id = f_rep.dict_get("folderID").and_then(|v| v.as_i64()).unwrap_or(1);

        call(
            &ctx1,
            &mut sess1,
            "accessGroupBookmarkMgr",
            "UpdateFolder",
            &[PyRep::Integer(folder_id), PyRep::String("Wormhole Chain".into()), PyRep::List(vec![PyRep::Integer(90_000_002), PyRep::Integer(98_000_001), PyRep::Integer(99_000_001)])],
        )
        .await;

        let sf_rep = call(&ctx1, &mut sess1, "bookmark", "CreateSubfolder", &[PyRep::Integer(folder_id), PyRep::String("C5 Statics".into())]).await;
        let subfolder_id = sf_rep.dict_get("subfolderID").and_then(|v| v.as_i64()).unwrap_or(1);
        let sf2_rep = call(&ctx1, &mut sess1, "bookmark", "CreateSubfolder", &[PyRep::Integer(folder_id), PyRep::String("Empty Reconnaissance".into())]).await;
        let empty_subfolder_id = sf2_rep.dict_get("subfolderID").and_then(|v| v.as_i64()).unwrap_or(2);

        let bm1 = call(
            &ctx1,
            &mut sess1,
            "bookmark",
            "BookmarkLocation",
            &[PyRep::String("Safe Spot Alpha".into()), PyRep::Integer(60_003_760), PyRep::String("Deep safe".into()), PyRep::Integer(folder_id), PyRep::Integer(subfolder_id), PyRep::Integer(0)],
        )
        .await;
        let perm_bm_id = bm1.dict_get("bookmarkID").and_then(|v| v.as_i64()).unwrap_or(1);

        call(&ctx1, &mut sess1, "accessGroupBookmarkMgr", "MoveBookmarksToFolderAndSubfolder", &[PyRep::List(vec![PyRep::Integer(perm_bm_id)]), PyRep::Integer(folder_id), PyRep::Integer(subfolder_id)]).await;

        let bm2 = call(
            &ctx1,
            &mut sess1,
            "bookmark",
            "BookmarkLocation",
            &[PyRep::String("Temp Wormhole Exit".into()), PyRep::Integer(60_003_760), PyRep::String("Expires in 3h".into()), PyRep::Integer(folder_id), PyRep::Integer(subfolder_id), PyRep::Integer(3)],
        )
        .await;
        let temp_bm_id = bm2.dict_get("bookmarkID").and_then(|v| v.as_i64()).unwrap_or(2);

        call(&ctx1, &mut sess2, "accessGroupBookmarkMgr", "AddToKnownFolders", &[PyRep::Integer(folder_id), PyRep::Bool(true)]).await;

        (folder_id, subfolder_id, empty_subfolder_id, perm_bm_id, temp_bm_id)
    };

    // Restart server at T + 1h (before the 3h expiry)
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
    let ctx2 = ServerContext::with_repository(repo2);
    common::pin_clock(&ctx2);
    ctx2.advance_virtual_clock(3_600);

    let mut sess1 = login_and_select(&ctx2, 7303, "demo", 90_000_001).await;
    let active_bms = call(&ctx2, &mut sess1, "accessGroupBookmarkMgr", "GetMyActiveBookmarks", &[]).await;
    let PyRep::Tuple(active_parts) = &active_bms else { panic!("expected (folders, bookmarks, subfolders) tuple") };
    let PyRep::List(folders_list) = &active_parts[0] else { panic!("expected folders list") };
    let PyRep::List(bms_list) = &active_parts[1] else { panic!("expected bookmarks list") };
    let PyRep::List(subfolders_list) = &active_parts[2] else { panic!("expected subfolders list") };
    assert!(
        folders_list.iter().any(|f| f.dict_get("folderID") == Some(&PyRep::Integer(folder_id)) && f.dict_get("isShared") == Some(&PyRep::Bool(true)))
            && !folders_list.iter().any(|f| f.dict_get("folderID") == Some(&PyRep::Integer(subfolder_id)) || f.dict_get("folderID") == Some(&PyRep::Integer(empty_subfolder_id))),
        "Top-level folders must include shared root folder and exclude subfolders: {folders_list:?}"
    );
    let perm_kv = bms_list.iter().find(|b| b.dict_get("bookmarkID") == Some(&PyRep::Integer(perm_bm_id))).expect("perm_bm_id in active bookmarks");
    assert_eq!(perm_kv.dict_get("itemID"), Some(&PyRep::Integer(60_003_760)), "Bookmark itemID must persist across SQLite restart");
    assert_eq!(perm_kv.dict_get("isShared"), Some(&PyRep::Bool(true)), "Bookmark isShared must persist across SQLite restart");
    assert_eq!(perm_kv.dict_get("subfolderID"), Some(&PyRep::Integer(subfolder_id)), "Bookmark subfolderID must persist across SQLite restart");
    assert_eq!(perm_kv.dict_get("memo"), Some(&PyRep::String("Deep safe".into())), "Bookmark memo must persist across SQLite restart");
    assert!(
        subfolders_list.iter().any(|s| s.dict_get("subfolderID") == Some(&PyRep::Integer(subfolder_id)) && s.dict_get("subfolderName") == Some(&PyRep::String("C5 Statics".into())) && s.dict_get("folderID") == Some(&PyRep::Integer(folder_id)))
            && subfolders_list.iter().any(|s| s.dict_get("subfolderID") == Some(&PyRep::Integer(empty_subfolder_id)) && s.dict_get("subfolderName") == Some(&PyRep::String("Empty Reconnaissance".into())) && s.dict_get("folderID") == Some(&PyRep::Integer(folder_id))),
        "Both populated and empty subfolders must survive SQLite restart in subfolders list with subfolderName and folderID: {subfolders_list:?}"
    );

    let folder_info = call(&ctx2, &mut sess1, "accessGroupBookmarkMgr", "GetFolderInfo", &[PyRep::Integer(folder_id)]).await;
    let PyRep::Rowset { rows: fi_rows, .. } = &folder_info else { panic!("expected folder_info rowset") };
    let root_row = fi_rows.iter().find(|r| r[0] == PyRep::Integer(folder_id)).expect("root folder row");
    assert_eq!(
        root_row[5],
        PyRep::List(vec![PyRep::Integer(90_000_002), PyRep::Integer(98_000_001), PyRep::Integer(99_000_001)]),
        "Folder ACL accessGroupIDs must persist across SQLite restart"
    );

    // Verify ESI character & corporation bookmarks and folders at T + 1h after restart
    let char1_params = HashMap::from([("character_id".to_string(), "90000001".to_string())]);
    let esi_char_bms = dispatch_char_social_route(&ctx2, "GET", "/characters/{character_id}/bookmarks", &char1_params, &HashMap::new(), b"")
        .expect("ESI GET /characters/{character_id}/bookmarks");
    assert_eq!(esi_char_bms.status_code, 200);
    let esi_char_bms_str = esi_char_bms.body.as_ref().unwrap().to_string();
    assert!(esi_char_bms_str.contains("Safe Spot Alpha") && esi_char_bms_str.contains("Temp Wormhole Exit") && esi_char_bms_str.contains("Deep safe") && esi_char_bms_str.contains("60003760"));

    let esi_char_folders = dispatch_char_social_route(&ctx2, "GET", "/characters/{character_id}/bookmarks/folders", &char1_params, &HashMap::new(), b"")
        .expect("ESI GET /characters/{character_id}/bookmarks/folders");
    assert_eq!(esi_char_folders.status_code, 200);
    let esi_char_folders_str = esi_char_folders.body.as_ref().unwrap().to_string();
    assert!(esi_char_folders_str.contains("Wormhole Chain") && !esi_char_folders_str.contains("C5 Statics") && !esi_char_folders_str.contains("Empty Reconnaissance"));

    let corp_params = HashMap::from([("corporation_id".to_string(), "98000001".to_string())]);
    let esi_corp_bms = dispatch_char_social_route(&ctx2, "GET", "/corporations/{corporation_id}/bookmarks", &corp_params, &HashMap::new(), b"")
        .expect("ESI GET /corporations/{corporation_id}/bookmarks");
    assert_eq!(esi_corp_bms.status_code, 200);
    let esi_corp_bms_str = esi_corp_bms.body.as_ref().unwrap().to_string();
    assert!(esi_corp_bms_str.contains("Safe Spot Alpha") && esi_corp_bms_str.contains("Temp Wormhole Exit"));

    let esi_corp_folders = dispatch_char_social_route(&ctx2, "GET", "/corporations/{corporation_id}/bookmarks/folders", &corp_params, &HashMap::new(), b"")
        .expect("ESI GET /corporations/{corporation_id}/bookmarks/folders");
    assert_eq!(esi_corp_folders.status_code, 200);
    let esi_corp_folders_str = esi_corp_folders.body.as_ref().unwrap().to_string();
    assert!(esi_corp_folders_str.contains("Wormhole Chain") && !esi_corp_folders_str.contains("C5 Statics") && !esi_corp_folders_str.contains("Empty Reconnaissance"));

    // Advance clock past 3h total (another 2h + 60s) and verify the 3h bookmark is deleted at expiry in both RPC and ESI
    ctx2.advance_virtual_clock(7_260);
    let bms_after_expiry = call(&ctx2, &mut sess1, "bookmark", "GetBookmarks", &[]).await;
    let bms_after_dbg = format!("{bms_after_expiry:?}");
    assert!(
        bms_after_dbg.contains(&perm_bm_id.to_string()) && !bms_after_dbg.contains(&temp_bm_id.to_string()),
        "3h expiry bookmark must be deleted when expiry is reached after restart: {bms_after_dbg}"
    );
    let esi_char_bms_after = dispatch_char_social_route(&ctx2, "GET", "/characters/{character_id}/bookmarks", &char1_params, &HashMap::new(), b"")
        .expect("ESI GET /characters/{character_id}/bookmarks after expiry");
    let esi_after_str = esi_char_bms_after.body.as_ref().unwrap().to_string();
    assert!(esi_after_str.contains("Safe Spot Alpha") && !esi_after_str.contains("Temp Wormhole Exit"));

    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_d6_chat_channels_motd_operators_password_and_bans_mutes_persist_across_sqlite_restart() {
    // parity: feat:lifecycle.chat-channels-persist, feat:lifecycle.social-durable
    let db_path = temp_sqlite_path("chat_channels");
    let kw = HashMap::new();

    let chan_id = {
        let repo1 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 1"));
        seed_repo_with_corp(&repo1);
        let ctx1 = ServerContext::with_repository(repo1);
        common::pin_clock(&ctx1);

        let mut sess1 = login_and_select(&ctx1, 7401, "demo", 90_000_001).await;
        let created = call(
            &ctx1,
            &mut sess1,
            "XmppChatMgr",
            "CreatePlayerOwnedChannel",
            &[PyRep::String("Intel_North".into()), PyRep::String("northpass".into()), PyRep::String("Report hostiles +1".into())],
        )
        .await;
        let chan_id = created.dict_get("channelID").and_then(|v| v.as_str()).unwrap_or("channel:intel_north").to_string();

        call(&ctx1, &mut sess1, "LSC", "AccessControl", &[PyRep::String(chan_id.clone()), PyRep::Integer(90_000_002), PyRep::Integer(2)]).await;
        call(&ctx1, &mut sess1, "LSC", "AccessControl", &[PyRep::String(chan_id.clone()), PyRep::Integer(90_000_005), PyRep::Integer(1)]).await;
        call(&ctx1, &mut sess1, "LSC", "AccessControl", &[PyRep::String(chan_id.clone()), PyRep::Integer(90_000_003), PyRep::Integer(-2)]).await;
        call(&ctx1, &mut sess1, "XmppChatMgr", "GMMute", &[PyRep::Integer(90_000_002), PyRep::String(chan_id.clone()), PyRep::Integer(3_600), PyRep::String("Gagged for 1h".into())]).await;

        chan_id
    };

    // Restart server from SQLite file
    let repo2 = Arc::new(WorldRepository::open_sqlite(&db_path).expect("open_sqlite 2"));
    let ctx2 = ServerContext::with_repository(repo2);
    common::pin_clock(&ctx2);

    let mut sess1 = login_and_select(&ctx2, 7402, "demo", 90_000_001).await;
    let info = call(&ctx2, &mut sess1, "LSC", "GetMembers", &[PyRep::String(chan_id.clone())]).await;
    let info_dbg = format!("{info:?}");
    assert!(
        info_dbg.contains("Intel_North")
            && info_dbg.contains("Report hostiles +1")
            && info_dbg.contains("90000002")
            && info_dbg.contains("90000005")
            && info_dbg.contains("90000003"),
        "Player-owned channel name, MOTD, operators, allow list, deny list, and mutes must survive restart: {info_dbg}"
    );

    // Non-operator, non-allow-listed character 90_000_004 must be rejected with wrong password and accepted with persisted password
    let mut sess4 = SessionState::new(7405);
    sess4.user_id = Some(4);
    sess4.character_id = Some(90_000_004);
    let wrong_pw = ctx2
        .dispatch_rpc(
            &mut sess4,
            "LSC",
            None,
            "JoinChannel",
            &[PyRep::String(chan_id.clone()), PyRep::String("wrongpass".into())],
            &kw,
            None,
        )
        .await
        .expect_err("wrong password must fail after restart");
    assert_eq!(wrong_pw.code, "InvalidChannelPassword");
    call(&ctx2, &mut sess4, "LSC", "JoinChannel", &[PyRep::String(chan_id.clone()), PyRep::String("northpass".into())]).await;

    let mut sess2 = SessionState::new(7403);
    sess2.user_id = Some(2);
    sess2.character_id = Some(90_000_002);
    call(&ctx2, &mut sess2, "LSC", "JoinChannel", &[PyRep::String(chan_id.clone()), PyRep::String("northpass".into())]).await;

    let muted_send = ctx2
        .dispatch_rpc(
            &mut sess2,
            "LSC",
            None,
            "SendMessage",
            &[PyRep::String(chan_id.clone()), PyRep::String("Testing mute".into())],
            &kw,
            None,
        )
        .await;
    assert!(muted_send.is_err());

    let mut sess3 = SessionState::new(7404);
    sess3.user_id = Some(3);
    sess3.character_id = Some(90_000_003);
    let denied_join = ctx2
        .dispatch_rpc(
            &mut sess3,
            "LSC",
            None,
            "JoinChannel",
            &[PyRep::String(chan_id), PyRep::String("northpass".into())],
            &kw,
            None,
        )
        .await;
    assert!(denied_join.is_err());

    let _ = std::fs::remove_file(&db_path);
}
