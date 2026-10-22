//! EVE mail notification emission: `OnMailSent` from the game-client `mailMgr.SendMail` path and
//! from ESI `POST /characters/{id}/mail`, plus `OnMailUpdatedByExternal` / `OnMailDeleted` from the
//! ESI organise / delete routes. Layouts follow the V24 client's
//! `eve/client/script/ui/services/mail/mailSvc.pyj` (`mailSvc.OnMailSent`, `OnMailUpdatedByExternal`,
//! `OnMailDeleted`); every assertion checks exact values on the recipient and absence elsewhere.

use crate::common;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::esi::{EsiHttpRequest, EsiRouter};
use eve_server::router::ServerContext;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const MINUTE_TICKS: i64 = 600_000_000;
const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

async fn select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(&mut s, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &HashMap::new(), None)
        .await
        .expect("SelectCharacterID");
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    ctx.drain_notifications_for_char(char_id).into_iter().filter(|n| n.event == event).map(|n| n.args).collect()
}

/// ESI token for P1 (account `demo`, which already owns P1).
fn p1_token(ctx: &ServerContext) -> String {
    let now = ctx.now_unix();
    let scopes = [
        "esi-mail.read_mail.v1",
        "esi-mail.send_mail.v1",
        "esi-mail.organize_mail.v1",
        "esi-calendar.respond_calendar_events.v1",
    ];
    let code = ctx
        .db
        .authorize_pkce("demo", "demo-secret", P1, "evomu-notify-mail-test", "https://localhost/callback",
            scopes.iter().map(|s| s.to_string()).collect(), PKCE_CHALLENGE, "S256", now)
        .expect("authorize_pkce");
    ctx.db.exchange_pkce_code(&code, "evomu-notify-mail-test", PKCE_VERIFIER, now).expect("exchange").access_token
}

/// `sentDate` must be the send time truncated to the minute (as `mailSvc.SendMail` computes it).
fn assert_sent_date(args: &[PyRep], before: i64, after: i64) {
    let PyRep::Integer(d) = args[2] else { panic!("sentDate must be an integer FILETIME: {:?}", args[2]) };
    assert_eq!(d % MINUTE_TICKS, 0, "sentDate is minute-aligned");
    assert!(d >= before / MINUTE_TICKS * MINUTE_TICKS && d <= after, "sentDate {d} outside [{before}, {after}]");
}

#[tokio::test]
async fn test_mail_sent_updated_deleted_notifications() {
    // parity: notify:OnMailSent, notify:OnMailUpdatedByExternal, notify:OnMailDeleted
    eve_testkit::attest(&["notify:OnMailSent", "notify:OnMailUpdatedByExternal", "notify:OnMailDeleted"]);
    let ctx = Arc::new(ServerContext::new());
    let mut s1 = select(&ctx, 701, "demo", "demo-secret", P1).await;
    let _s2 = select(&ctx, 702, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("mailp3", "mailp3-secret");
    let p3 = ctx.db.create_character(uid3, "Mail Pilot Three", 1, 1, 11, 17, &ctx.sde).expect("create p3").character_id;
    let _s3 = select(&ctx, 703, "mailp3", "mailp3-secret", p3).await;
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // Game client: mailMgr.SendMail(toCharacterIDs, toListID, toCorpOrAllianceID, title, body,
    // isReplyTo, isForwardedFrom). The recipient gets OnMailSent(messageID, senderID, sentDate,
    // toCharacterIDs, toListID, toCorpOrAllianceID, title, statusMask); the sender's own client
    // already ran OnMailSent locally, and the uninvolved P3 gets nothing.
    let before = ctx.now_blueos_filetime_ticks();
    let mid = ctx
        .dispatch_rpc(&mut s1, "mailMgr", None, "SendMail",
            &[PyRep::List(vec![PyRep::Integer(P2)]), PyRep::None, PyRep::None, PyRep::String("Op tonight".into()),
              PyRep::String("Form up at 20:00".into()), PyRep::None, PyRep::None],
            &HashMap::new(), None)
        .await
        .expect("SendMail")
        .as_i64()
        .expect("messageID");
    let after = ctx.now_blueos_filetime_ticks();
    let got = events(&ctx, P2, "OnMailSent");
    assert_eq!(got.len(), 1);
    assert_sent_date(&got[0], before, after);
    let mut expected = vec![
        PyRep::Integer(mid),
        PyRep::Integer(P1),
        got[0][2].clone(),
        PyRep::List(vec![PyRep::Integer(P2)]),
        PyRep::None,
        PyRep::None,
        PyRep::String("Op tonight".into()),
        PyRep::Integer(0),
    ];
    assert_eq!(got[0], expected);
    assert!(events(&ctx, P1, "OnMailSent").is_empty(), "game-client sender is not re-notified");
    assert!(events(&ctx, p3, "OnMailSent").is_empty());
    assert!(events(&ctx, P2, "OnNewMailReceived").is_empty(), "OnNewMailReceived is client-local");

    // ESI POST /characters/{P1}/mail: sent outside the game client, so sender and recipient both get it.
    let router = EsiRouter::new(Arc::clone(&ctx));
    let token = p1_token(&ctx);
    let body = json!({"approved_cost": 0, "subject": "ESI op", "body": "x",
        "recipients": [{"recipient_id": P2, "recipient_type": "character"}]});
    let before = ctx.now_blueos_filetime_ticks();
    let resp = router.handle_request(
        &EsiHttpRequest::new("POST", format!("/v1/characters/{P1}/mail/")).with_bearer_token(&token).with_body(serde_json::to_vec(&body).unwrap()),
    );
    assert_eq!(resp.status_code, 201, "{}", String::from_utf8_lossy(&resp.body));
    let esi_mid: i64 = serde_json::from_slice(&resp.body).expect("mail id");
    let after = ctx.now_blueos_filetime_ticks();
    for c in [P1, P2] {
        let got = events(&ctx, c, "OnMailSent");
        assert_eq!(got.len(), 1, "char {c}");
        assert_sent_date(&got[0], before, after);
        expected = vec![
            PyRep::Integer(esi_mid),
            PyRep::Integer(P1),
            got[0][2].clone(),
            PyRep::List(vec![PyRep::Integer(P2)]),
            PyRep::None,
            PyRep::None,
            PyRep::String("ESI op".into()),
            PyRep::Integer(0),
        ];
        assert_eq!(got[0], expected);
    }
    assert!(events(&ctx, p3, "OnMailSent").is_empty());

    // ESI PUT /characters/{P1}/mail/{id}: OnMailUpdatedByExternal(messageIDs, is_read, new_label);
    // the label mask is the OR of the label IDs, and an omitted field is None.
    let put = |payload: serde_json::Value| {
        router.handle_request(
            &EsiHttpRequest::new("PUT", format!("/v1/characters/{P1}/mail/{esi_mid}/")).with_bearer_token(&token).with_body(serde_json::to_vec(&payload).unwrap()),
        )
    };
    assert_eq!(put(json!({"read": false, "labels": [2, 16]})).status_code, 204);
    assert_eq!(
        events(&ctx, P1, "OnMailUpdatedByExternal"),
        vec![vec![PyRep::List(vec![PyRep::Integer(esi_mid)]), PyRep::Bool(false), PyRep::Integer(18)]]
    );
    assert_eq!(put(json!({"read": true})).status_code, 204);
    assert_eq!(
        events(&ctx, P1, "OnMailUpdatedByExternal"),
        vec![vec![PyRep::List(vec![PyRep::Integer(esi_mid)]), PyRep::Bool(true), PyRep::None]]
    );
    assert!(events(&ctx, P2, "OnMailUpdatedByExternal").is_empty(), "only the owner of the mailbox");
    // A rejected update (empty body) changes nothing and notifies nobody.
    assert_eq!(put(json!({})).status_code, 400);
    assert!(events(&ctx, P1, "OnMailUpdatedByExternal").is_empty());

    // ESI DELETE: OnMailDeleted(messageIDs) to the mailbox owner only; deleting again is a 404, no event.
    let del = || router.handle_request(&EsiHttpRequest::new("DELETE", format!("/v1/characters/{P1}/mail/{esi_mid}/")).with_bearer_token(&token));
    assert_eq!(del().status_code, 204);
    assert_eq!(events(&ctx, P1, "OnMailDeleted"), vec![vec![PyRep::List(vec![PyRep::Integer(esi_mid)])]]);
    assert!(events(&ctx, P2, "OnMailDeleted").is_empty());
    assert_eq!(del().status_code, 404);
    assert!(events(&ctx, P1, "OnMailDeleted").is_empty());
}

#[tokio::test]
async fn test_mailing_list_and_external_label_notifications() {
    // parity: notify:OnMailingListSetOperator, notify:OnMailingListSetMuted, notify:OnMailingListSetClear, notify:OnMailingListLeave, notify:OnMailingListDeleted, notify:OnLabelsCreatedByExternal
    eve_testkit::attest(&[
        "notify:OnMailingListSetOperator",
        "notify:OnMailingListSetMuted",
        "notify:OnMailingListSetClear",
        "notify:OnMailingListLeave",
        "notify:OnMailingListDeleted",
        "notify:OnLabelsCreatedByExternal",
    ]);
    let ctx = Arc::new(ServerContext::new());
    let mut s1 = select(&ctx, 711, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 712, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("ml_p3", "ml_p3-secret");
    let p3 = ctx.db.create_character(uid3, "Mailing List Pilot Three", 1, 1, 11, 17, &ctx.sde).expect("create p3").character_id;
    let mut s3 = select(&ctx, 713, "ml_p3", "ml_p3-secret", p3).await;

    let ml = ctx
        .dispatch_rpc(&mut s1, "mailingListsMgr", None, "Create", &[PyRep::String("OpsList".into())], &HashMap::new(), None)
        .await
        .expect("Create");
    let list_id = ml.dict_get("listID").and_then(PyRep::as_i64).expect("listID");
    ctx.dispatch_rpc(&mut s2, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &HashMap::new(), None).await.expect("P2 Join");
    ctx.dispatch_rpc(&mut s3, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &HashMap::new(), None).await.expect("P3 Join");
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // Non-operator P2 cannot change roles and emits no notifications.
    let err = ctx
        .dispatch_rpc(&mut s2, "mailingListsMgr", None, "SetMembersOperator", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(p3)])], &HashMap::new(), None)
        .await
        .expect_err("non-operator denied");
    assert_eq!(err.code, "MailingListAccessDenied");
    assert!(events(&ctx, p3, "OnMailingListSetOperator").is_empty());

    // Owner P1 promotes P2 to operator -> OnMailingListSetOperator(listID) to P2 only.
    ctx.dispatch_rpc(&mut s1, "mailingListsMgr", None, "SetMembersOperator", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(P2)])], &HashMap::new(), None)
        .await
        .expect("SetMembersOperator");
    assert_eq!(events(&ctx, P2, "OnMailingListSetOperator"), vec![vec![PyRep::Integer(list_id)]]);
    assert!(events(&ctx, P1, "OnMailingListSetOperator").is_empty());
    assert!(events(&ctx, p3, "OnMailingListSetOperator").is_empty());

    // Operator P2 mutes P3 -> OnMailingListSetMuted(listID) to P3 only, and P3 is rejected from sending to the list.
    ctx.dispatch_rpc(&mut s2, "mailingListsMgr", None, "SetMembersMuted", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(p3)])], &HashMap::new(), None)
        .await
        .expect("SetMembersMuted");
    assert_eq!(events(&ctx, p3, "OnMailingListSetMuted"), vec![vec![PyRep::Integer(list_id)]]);
    assert!(events(&ctx, P1, "OnMailingListSetMuted").is_empty());
    assert!(events(&ctx, P2, "OnMailingListSetMuted").is_empty());
    let mute_err = ctx
        .dispatch_rpc(
            &mut s3,
            "mailMgr",
            None,
            "SendMail",
            &[PyRep::List(vec![]), PyRep::Integer(list_id), PyRep::None, PyRep::String("Muted attempt".into()), PyRep::String("x".into()), PyRep::None, PyRep::None],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("muted member must not post to mailing list");
    assert_eq!(mute_err.code, "MutedInMailingList");

    // Owner P1 clears P3's role -> OnMailingListSetClear(listID) to P3 only, and P3 can post again.
    ctx.dispatch_rpc(&mut s1, "mailingListsMgr", None, "SetMembersClear", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(p3)])], &HashMap::new(), None)
        .await
        .expect("SetMembersClear");
    assert_eq!(events(&ctx, p3, "OnMailingListSetClear"), vec![vec![PyRep::Integer(list_id)]]);
    assert!(events(&ctx, P1, "OnMailingListSetClear").is_empty());
    assert!(events(&ctx, P2, "OnMailingListSetClear").is_empty());
    ctx.dispatch_rpc(
        &mut s3,
        "mailMgr",
        None,
        "SendMail",
        &[PyRep::List(vec![]), PyRep::Integer(list_id), PyRep::None, PyRep::String("Unmuted post".into()), PyRep::String("x".into()), PyRep::None, PyRep::None],
        &HashMap::new(),
        None,
    )
    .await
    .expect("cleared member can post to mailing list");
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // Owner P1 kicks P3 -> OnMailingListLeave(listID, p3) to P3 only; kicking again is a no-op.
    ctx.dispatch_rpc(&mut s1, "mailingListsMgr", None, "KickMembers", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(p3)])], &HashMap::new(), None)
        .await
        .expect("KickMembers");
    assert_eq!(events(&ctx, p3, "OnMailingListLeave"), vec![vec![PyRep::Integer(list_id), PyRep::Integer(p3)]]);
    assert!(events(&ctx, P1, "OnMailingListLeave").is_empty());
    assert!(events(&ctx, P2, "OnMailingListLeave").is_empty());
    ctx.dispatch_rpc(&mut s1, "mailingListsMgr", None, "KickMembers", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(p3)])], &HashMap::new(), None)
        .await
        .expect("KickMembers non-member");
    assert!(events(&ctx, p3, "OnMailingListLeave").is_empty());

    // Non-owner P2 cannot delete the list; owner P1 deleting it sends OnMailingListDeleted(listID)
    // to remaining member P2 (not to the deleting owner P1 and not to already-kicked P3).
    assert_eq!(
        ctx.dispatch_rpc(&mut s2, "mailingListsMgr", None, "Delete", &[PyRep::Integer(list_id)], &HashMap::new(), None).await.expect_err("non-owner").code,
        "MailingListAccessDenied"
    );
    assert!(events(&ctx, P2, "OnMailingListDeleted").is_empty());
    ctx.dispatch_rpc(&mut s1, "mailingListsMgr", None, "Delete", &[PyRep::Integer(list_id)], &HashMap::new(), None)
        .await
        .expect("Delete");
    assert_eq!(events(&ctx, P2, "OnMailingListDeleted"), vec![vec![PyRep::Integer(list_id)]]);
    assert!(events(&ctx, P1, "OnMailingListDeleted").is_empty(), "owner deletes locally");
    assert!(events(&ctx, p3, "OnMailingListDeleted").is_empty(), "kicked member not notified");

    // ESI POST /characters/{P1}/mail/labels: emits OnLabelsCreatedByExternal(KeyVal(labelID, name, color))
    // to P1 only; game-client mailMgr.CreateLabel updates locally without emitting it.
    ctx.dispatch_rpc(&mut s1, "mailMgr", None, "CreateLabel", &[PyRep::String("LocalLabel".into()), PyRep::String("#ffffff".into())], &HashMap::new(), None)
        .await
        .expect("CreateLabel");
    assert!(events(&ctx, P1, "OnLabelsCreatedByExternal").is_empty());

    let router = EsiRouter::new(Arc::clone(&ctx));
    let token = p1_token(&ctx);
    let resp = router.handle_request(
        &EsiHttpRequest::new("POST", format!("/v3/characters/{P1}/mail/labels/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!({"name": " OpsTag ", "color": "#fe0000"})).unwrap()),
    );
    assert_eq!(resp.status_code, 201, "{}", String::from_utf8_lossy(&resp.body));
    let label_id: i64 = serde_json::from_slice(&resp.body).expect("label_id");
    assert_eq!(
        events(&ctx, P1, "OnLabelsCreatedByExternal"),
        vec![vec![PyRep::keyval([
            ("labelID", PyRep::Integer(label_id)),
            ("name", PyRep::String("OpsTag".into())),
            ("color", PyRep::String("#fe0000".into())),
        ])]]
    );
    assert!(events(&ctx, P2, "OnLabelsCreatedByExternal").is_empty());
    let bad = router.handle_request(
        &EsiHttpRequest::new("POST", format!("/v3/characters/{P1}/mail/labels/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!({"name": "BadColor", "color": "#123456"})).unwrap()),
    );
    assert_eq!(bad.status_code, 400);
    assert!(events(&ctx, P1, "OnLabelsCreatedByExternal").is_empty());
}

#[tokio::test]
async fn test_calendar_event_and_external_response_notifications() {
    // parity: notify:OnNewCalendarEvent, notify:OnEditCalendarEvent, notify:OnRemoveCalendarEvent, notify:OnEventResponseByExternal
    eve_testkit::attest(&[
        "notify:OnNewCalendarEvent",
        "notify:OnEditCalendarEvent",
        "notify:OnRemoveCalendarEvent",
        "notify:OnEventResponseByExternal",
    ]);
    let ctx = Arc::new(ServerContext::new());
    let mut s1 = select(&ctx, 721, "demo", "demo-secret", P1).await;
    let _s2 = select(&ctx, 722, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("cal_p3", "cal_p3-secret");
    let p3 = ctx.db.create_character(uid3, "Calendar Pilot Three", 1, 1, 11, 17, &ctx.sde).expect("create p3").character_id;
    let mut s3 = select(&ctx, 723, "cal_p3", "cal_p3-secret", p3).await;
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // CreatePersonalEvent with P2 as invitee -> OnNewCalendarEvent(eventID, ownerID, eventDateTime,
    // eventDuration, eventTitle, importance, None, true) to P2 only (creator P1 runs it locally).
    let dt1 = 133_600_000_000_000_000i64;
    let ev = ctx
        .dispatch_rpc(
            &mut s1,
            "calendarMgr",
            None,
            "CreatePersonalEvent",
            &[
                PyRep::Integer(dt1),
                PyRep::Integer(60),
                PyRep::String("Roam".into()),
                PyRep::String("Bring nano".into()),
                PyRep::Integer(1),
                PyRep::List(vec![PyRep::Integer(P2)]),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CreatePersonalEvent");
    let eid = ev.dict_get("eventID").and_then(PyRep::as_i64).expect("eventID");
    let expected_new = vec![
        PyRep::Integer(eid),
        PyRep::Integer(P1),
        PyRep::Integer(dt1),
        PyRep::Integer(60),
        PyRep::String("Roam".into()),
        PyRep::Integer(1),
        PyRep::None,
        PyRep::Bool(true),
    ];
    assert_eq!(events(&ctx, P2, "OnNewCalendarEvent"), vec![expected_new.clone()]);
    assert!(events(&ctx, P1, "OnNewCalendarEvent").is_empty(), "creator adds personal event locally");
    assert!(events(&ctx, p3, "OnNewCalendarEvent").is_empty(), "uninvited character gets nothing");

    // UpdateEventParticipants(eid, charsToAdd=[p3], charsToRemove=[P2]):
    // P3 gets OnNewCalendarEvent and P2 gets OnRemoveCalendarEvent(eid, dt1, isDeleted=false).
    ctx.dispatch_rpc(
        &mut s1,
        "calendarMgr",
        None,
        "UpdateEventParticipants",
        &[
            PyRep::Integer(eid),
            PyRep::List(vec![PyRep::Integer(p3)]),
            PyRep::List(vec![PyRep::Integer(P2)]),
        ],
        &HashMap::new(),
        None,
    )
    .await
    .expect("UpdateEventParticipants");
    assert_eq!(events(&ctx, p3, "OnNewCalendarEvent"), vec![expected_new]);
    assert_eq!(
        events(&ctx, P2, "OnRemoveCalendarEvent"),
        vec![vec![PyRep::Integer(eid), PyRep::Integer(dt1), PyRep::Bool(false)]]
    );
    assert!(events(&ctx, P1, "OnRemoveCalendarEvent").is_empty());

    // EditPersonalEvent(eid, oldDateTime, dateTime, duration, title, description, important):
    // owner P1 and current invitee P3 get OnEditCalendarEvent; removed P2 gets nothing.
    let dt2 = dt1 + 36_000_000_000;
    ctx.dispatch_rpc(
        &mut s1,
        "calendarMgr",
        None,
        "EditPersonalEvent",
        &[
            PyRep::Integer(eid),
            PyRep::Integer(dt1),
            PyRep::Integer(dt2),
            PyRep::Integer(90),
            PyRep::String("Roam v2".into()),
            PyRep::String("Updated".into()),
            PyRep::Integer(0),
        ],
        &HashMap::new(),
        None,
    )
    .await
    .expect("EditPersonalEvent");
    let expected_edit = vec![
        PyRep::Integer(eid),
        PyRep::Integer(P1),
        PyRep::Integer(dt1),
        PyRep::Integer(dt2),
        PyRep::Integer(90),
        PyRep::String("Roam v2".into()),
        PyRep::Integer(0),
        PyRep::None,
    ];
    assert_eq!(events(&ctx, P1, "OnEditCalendarEvent"), vec![expected_edit.clone()]);
    assert_eq!(events(&ctx, p3, "OnEditCalendarEvent"), vec![expected_edit]);
    assert!(events(&ctx, P2, "OnEditCalendarEvent").is_empty(), "removed invitee not notified");

    // ESI PUT /characters/{P1}/calendar/{eid}: emits OnEventResponseByExternal(eid, eventKV, 4) to P1 only.
    let router = EsiRouter::new(Arc::clone(&ctx));
    let token = p1_token(&ctx);
    let resp = router.handle_request(
        &EsiHttpRequest::new("PUT", format!("/v3/characters/{P1}/calendar/{eid}/"))
            .with_bearer_token(&token)
            .with_body(serde_json::to_vec(&json!({"response": "accepted"})).unwrap()),
    );
    assert_eq!(resp.status_code, 204, "{}", String::from_utf8_lossy(&resp.body));
    assert_eq!(
        events(&ctx, P1, "OnEventResponseByExternal"),
        vec![vec![
            PyRep::Integer(eid),
            PyRep::keyval([
                ("eventID", PyRep::Integer(eid)),
                ("ownerID", PyRep::Integer(P1)),
                ("eventDateTime", PyRep::Integer(dt2)),
                ("eventDuration", PyRep::Integer(90)),
                ("eventTitle", PyRep::String("Roam v2".into())),
                ("importance", PyRep::Integer(0)),
                ("flag", PyRep::Integer(P1)),
                ("isDeleted", PyRep::Bool(false)),
            ]),
            PyRep::Integer(4),
        ]]
    );
    assert!(events(&ctx, P2, "OnEventResponseByExternal").is_empty());
    assert!(events(&ctx, p3, "OnEventResponseByExternal").is_empty());

    // DeleteEvent: non-owner P3 is denied; owner P1 deletes and notifies P1 and P3 with isDeleted=true.
    assert_eq!(
        ctx.dispatch_rpc(&mut s3, "calendarMgr", None, "DeleteEvent", &[PyRep::Integer(eid)], &HashMap::new(), None).await.expect_err("non-owner").code,
        "CalendarAccessDenied"
    );
    assert!(events(&ctx, P1, "OnRemoveCalendarEvent").is_empty());
    ctx.dispatch_rpc(&mut s1, "calendarMgr", None, "DeleteEvent", &[PyRep::Integer(eid)], &HashMap::new(), None)
        .await
        .expect("DeleteEvent");
    let expected_del = vec![PyRep::Integer(eid), PyRep::Integer(dt2), PyRep::Bool(true)];
    assert_eq!(events(&ctx, P1, "OnRemoveCalendarEvent"), vec![expected_del.clone()]);
    assert_eq!(events(&ctx, p3, "OnRemoveCalendarEvent"), vec![expected_del]);
    assert!(events(&ctx, P2, "OnRemoveCalendarEvent").is_empty());
}
