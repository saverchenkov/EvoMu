//! Remediation integration tests for 11 auditor-flagged rows across calendarMgr.

use std::collections::HashMap;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use crate::common;

const PILOT1: i64 = 90_000_001;
const PILOT2: i64 = 90_000_002;

async fn setup_pilot(
    ctx: &ServerContext,
    sess_id: u64,
    user_name: &str,
    secret: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, user_name, secret).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    sess
}

fn get_i64(rep: &PyRep, key: &str) -> Option<i64> {
    rep.dict_get(key).and_then(|v| v.as_i64())
}

fn get_bool(rep: &PyRep, key: &str) -> Option<bool> {
    rep.dict_get(key).and_then(|v| v.as_bool())
}

fn get_str<'a>(rep: &'a PyRep, key: &str) -> Option<&'a str> {
    rep.dict_get(key).and_then(|v| v.as_str())
}

#[tokio::test]
async fn test_calendar_manager_remediation_lifecycle() {
    // parity: rpc:calendarMgr.CreateAllianceEvent, rpc:calendarMgr.CreateCorporationEvent, rpc:calendarMgr.CreatePersonalEvent, rpc:calendarMgr.DeleteEvent, rpc:calendarMgr.EditAllianceEvent, rpc:calendarMgr.EditCorporationEvent, rpc:calendarMgr.EditPersonalEvent, rpc:calendarMgr.GetResponsesForCharacter, rpc:calendarMgr.GetResponsesToEvent, rpc:calendarMgr.SendEventResponse, rpc:calendarMgr.UpdateEventParticipants
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let corp_id = 98_000_101i64;
    let ally_id = 99_000_101i64;

    let uid3 = ctx.db.provision_player_account("pilot3", "p3-sec");
    let pilot3 = ctx
        .db
        .create_character(uid3, "Pilot Three", 1, 1, 11, 17, &ctx.sde)
        .expect("create pilot3")
        .character_id;

    let mut sess1 = setup_pilot(&ctx, 2001, "demo", "demo-secret", PILOT1).await;
    let mut sess2 = setup_pilot(&ctx, 2002, "other", "other-secret", PILOT2).await;
    let mut sess3 = setup_pilot(&ctx, 2003, "pilot3", "p3-sec", pilot3).await;

    sess1.corporation_id = Some(corp_id);
    sess1.alliance_id = Some(ally_id);
    sess2.corporation_id = Some(corp_id);
    sess2.alliance_id = Some(ally_id);
    sess3.corporation_id = Some(98_000_102);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&PILOT1) {
            c1.corporation_id = corp_id;
            c1.alliance_id = Some(ally_id);
        }
        if let Some(c2) = snap.characters.get_mut(&PILOT2) {
            c2.corporation_id = corp_id;
            c2.alliance_id = Some(ally_id);
        }
        if let Some(c3) = snap.characters.get_mut(&pilot3) {
            c3.corporation_id = 98_000_102;
            c3.alliance_id = None;
        }

        snap.corporations.insert(
            corp_id,
            eve_db::CorporationRecord {
                corporation_id: corp_id,
                name: "Remediation Corp".into(),
                ticker: "REMED".into(),
                ceo_id: PILOT1,
                alliance_id: Some(ally_id),
                tax_rate: 0.05,
                master_wallet: eve_db::IskCents(10_000_000_00),
                members: std::collections::HashSet::from([PILOT1, PILOT2]),
                member_roles: HashMap::from([(PILOT1, eve_db::corp_roles::ROLE_DIRECTOR)]),
                applications: HashMap::new(),
                wallet_divisions: eve_db::CorporationRecord::default_wallet_divisions(eve_db::IskCents(10_000_000_00)),
                description: "Remediation corp".into(),
                url: "".into(),
                hq_station_id: Some(60_003_760),
            },
        );

        snap.alliances.insert(
            ally_id,
            eve_db::AllianceRecord {
                alliance_id: ally_id,
                name: "Remediation Alliance".into(),
                short_name: "REM".into(),
                executor_corp_id: corp_id,
                member_corp_ids: std::collections::HashSet::from([corp_id]),
            },
        );
    });

    let kw = HashMap::new();
    let future_time = ctx.now_blueos_filetime_ticks() + 86_400 * 10_000_000;

    // 1. CreatePersonalEvent: rejects past dateTime and empty title
    let past_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreatePersonalEvent", &[PyRep::Integer(100_000_000), PyRep::Integer(60), PyRep::String("Past Event".into()), PyRep::String("Should fail".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(past_err.code, "UserError");

    let empty_title_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreatePersonalEvent", &[PyRep::Integer(future_time), PyRep::Integer(60), PyRep::String("   ".into()), PyRep::String("Should fail".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(empty_title_err.code, "UserError");

    let _ = ctx.drain_notifications_for_char(PILOT2);
    // CreatePersonalEvent: succeeds with future dateTime and returns KeyVal
    let ev1 = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreatePersonalEvent", &[PyRep::Integer(future_time), PyRep::Integer(60), PyRep::String("Ops Sync".into()), PyRep::String("Strategy meeting".into()), PyRep::Integer(1)], &kw, None).await.expect("CreatePersonalEvent");
    let ev1_id = get_i64(&ev1, "eventID").expect("eventID");
    assert_eq!(get_i64(&ev1, "ownerID"), Some(PILOT1));

    // 2. EditPersonalEvent (7-arg client arity: eventID, oldDateTime, dateTime, duration, title, description, importance)
    let unauth_edit = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "EditPersonalEvent", &[PyRep::Integer(ev1_id), PyRep::Integer(future_time), PyRep::Integer(future_time + 1000), PyRep::Integer(90), PyRep::String("Hacked Title".into()), PyRep::String("Hacked Desc".into()), PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(unauth_edit.code, "CalendarAccessDenied");

    // Reject empty title on edit
    let empty_edit_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditPersonalEvent", &[PyRep::Integer(ev1_id), PyRep::Integer(future_time), PyRep::Integer(future_time + 2000), PyRep::Integer(90), PyRep::String("".into()), PyRep::String("Updated Strategy".into()), PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(empty_edit_err.code, "UserError");

    // Authorized owner edits personal event with 7-arg client arity
    let edit_rep = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditPersonalEvent", &[PyRep::Integer(ev1_id), PyRep::Integer(future_time), PyRep::Integer(future_time + 2000), PyRep::Integer(90), PyRep::String("Updated Ops Sync".into()), PyRep::String("Updated Strategy".into()), PyRep::Integer(2)], &kw, None).await.expect("EditPersonalEvent");
    assert_eq!(get_i64(&edit_rep, "eventID"), Some(ev1_id));
    let edit_details = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(ev1_id)], &kw, None).await.expect("GetEventDetails");
    assert_eq!(get_str(&edit_details, "title"), Some("Updated Ops Sync"));
    assert_eq!(get_str(&edit_details, "description"), Some("Updated Strategy"));
    assert_eq!(get_i64(&edit_details, "dateTime"), Some(future_time + 2000));
    assert_eq!(get_i64(&edit_details, "duration"), Some(90));
    assert_eq!(get_i64(&edit_details, "importance"), Some(2));

    // 3. UpdateEventParticipants: unauthorized caller fails
    let unauth_parts = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "UpdateEventParticipants", &[PyRep::Integer(ev1_id), PyRep::List(vec![PyRep::Integer(PILOT2)]), PyRep::List(vec![])], &kw, None).await.unwrap_err();
    assert_eq!(unauth_parts.code, "CalendarAccessDenied");

    let _ = ctx.drain_notifications_for_char(PILOT2);
    // Owner adds Pilot 2 as participant -> emits OnNewCalendarEvent to Pilot 2
    let upd_parts = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "UpdateEventParticipants", &[PyRep::Integer(ev1_id), PyRep::List(vec![PyRep::Integer(PILOT2)]), PyRep::List(vec![])], &kw, None).await.expect("UpdateEventParticipants");
    assert_eq!(get_i64(&upd_parts, "eventID"), Some(ev1_id));
    let p2_invite_notifs = ctx.drain_notifications_for_char(PILOT2);
    assert!(
        p2_invite_notifs.iter().any(|n| n.event == "OnNewCalendarEvent" && n.args.first() == Some(&PyRep::Integer(ev1_id))),
        "Invited participant Pilot 2 must receive OnNewCalendarEvent"
    );

    // 4. SendEventResponse (3-arg client arity: eventID, ownerID, response) & RespondToEvent
    let unauth_resp = ctx.dispatch_rpc(&mut sess3, "calendarMgr", None, "SendEventResponse", &[PyRep::Integer(ev1_id), PyRep::Integer(PILOT1), PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(unauth_resp.code, "CalendarAccessDenied");

    let unauth_respond = ctx.dispatch_rpc(&mut sess3, "calendarMgr", None, "RespondToEvent", &[PyRep::Integer(ev1_id), PyRep::Integer(PILOT1), PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(unauth_respond.code, "CalendarAccessDenied");

    // Reject out-of-range RSVP status code
    let bad_status_err = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "SendEventResponse", &[PyRep::Integer(ev1_id), PyRep::Integer(PILOT1), PyRep::Integer(99)], &kw, None).await.unwrap_err();
    assert_eq!(bad_status_err.code, "UserError");

    // Invited Pilot 2 accepts event using 3-arg SendEventResponse(eventID, ownerID, response)
    let send_resp = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "SendEventResponse", &[PyRep::Integer(ev1_id), PyRep::Integer(PILOT1), PyRep::Integer(2)], &kw, None).await.expect("SendEventResponse");
    assert_eq!(get_i64(&send_resp, "status"), Some(2));

    // 5. GetResponsesToEvent: returns responses to authorized participant
    let resp_to_ev = ctx
        .dispatch_rpc(
            &mut sess2,
            "calendarMgr",
            None,
            "GetResponsesToEvent",
            &[PyRep::Integer(ev1_id)],
            &kw,
            None,
        )
        .await
        .expect("GetResponsesToEvent");
    let PyRep::Rowset { rows: resp_rows, .. } = resp_to_ev else {
        panic!("expected rowset");
    };
    assert!(resp_rows.iter().any(|r| r[0] == PyRep::Integer(PILOT2) && r[1] == PyRep::Integer(2)));

    // 6. GetResponsesForCharacter: retrieves active responses for Pilot 2
    let resp_for_char = ctx
        .dispatch_rpc(
            &mut sess2,
            "calendarMgr",
            None,
            "GetResponsesForCharacter",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetResponsesForCharacter");
    let PyRep::Rowset { rows: char_resp_rows, .. } = resp_for_char else {
        panic!("expected rowset");
    };
    assert!(char_resp_rows.iter().any(|r| r[0] == PyRep::Integer(ev1_id) && r[1] == PyRep::Integer(2)));

    // 7. CreateCorporationEvent & EditCorporationEvent
    let non_director_corp = ctx
        .dispatch_rpc(
            &mut sess2,
            "calendarMgr",
            None,
            "CreateCorporationEvent",
            &[
                PyRep::Integer(future_time),
                PyRep::Integer(60),
                PyRep::String("Unauthorized Corp Event".into()),
                PyRep::String("No perms".into()),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(non_director_corp.code, "CalendarAccessDenied");

    // CreateCorporationEvent: rejects past dateTime and empty title
    let corp_past_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreateCorporationEvent", &[PyRep::Integer(100_000_000), PyRep::Integer(60), PyRep::String("Past Corp Op".into()), PyRep::String("Desc".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(corp_past_err.code, "UserError");
    let corp_empty_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreateCorporationEvent", &[PyRep::Integer(future_time), PyRep::Integer(60), PyRep::String("".into()), PyRep::String("Desc".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(corp_empty_err.code, "UserError");

    let _ = ctx.drain_notifications_for_char(PILOT2);
    let corp_ev = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreateCorporationEvent", &[PyRep::Integer(future_time), PyRep::Integer(60), PyRep::String("Corp Mining Op".into()), PyRep::String("Moon mining".into()), PyRep::Integer(1)], &kw, None).await.expect("CreateCorporationEvent");
    let corp_ev_id = get_i64(&corp_ev, "eventID").expect("eventID");
    assert_eq!(get_i64(&corp_ev, "ownerID"), Some(corp_id));
    assert!(
        ctx.drain_notifications_for_char(PILOT2)
            .iter()
            .any(|n| n.event == "OnNewCalendarEvent" && n.args.first() == Some(&PyRep::Integer(corp_ev_id))),
        "Corp member Pilot 2 must receive OnNewCalendarEvent on CreateCorporationEvent"
    );

    let edit_corp = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditCorporationEvent", &[PyRep::Integer(corp_ev_id), PyRep::Integer(future_time), PyRep::Integer(future_time + 500), PyRep::Integer(120), PyRep::String("Updated Mining Op".into()), PyRep::String("Moon mining 2".into()), PyRep::Integer(2)], &kw, None).await.expect("EditCorporationEvent");
    assert_eq!(get_i64(&edit_corp, "eventID"), Some(corp_ev_id));
    assert!(
        ctx.drain_notifications_for_char(PILOT2)
            .iter()
            .any(|n| n.event == "OnEditCalendarEvent" && n.args.first() == Some(&PyRep::Integer(corp_ev_id))),
        "Corp member Pilot 2 must receive OnEditCalendarEvent on EditCorporationEvent"
    );
    let corp_details = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(corp_ev_id)], &kw, None).await.expect("GetEventDetails corp");
    assert_eq!(get_str(&corp_details, "title"), Some("Updated Mining Op"));
    assert_eq!(get_str(&corp_details, "description"), Some("Moon mining 2"));
    assert_eq!(get_i64(&corp_details, "dateTime"), Some(future_time + 500));

    // 8. CreateAllianceEvent & EditAllianceEvent
    let non_ally_ev = ctx.dispatch_rpc(&mut sess3, "calendarMgr", None, "CreateAllianceEvent", &[PyRep::Integer(future_time), PyRep::Integer(60), PyRep::String("Invalid Ally Event".into()), PyRep::String("No ally".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(non_ally_ev.code, "CalendarAccessDenied");

    let ally_past_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreateAllianceEvent", &[PyRep::Integer(100_000_000), PyRep::Integer(120), PyRep::String("Past CTA".into()), PyRep::String("Desc".into()), PyRep::Integer(3)], &kw, None).await.unwrap_err();
    assert_eq!(ally_past_err.code, "UserError");

    let ally_ev = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreateAllianceEvent", &[PyRep::Integer(future_time), PyRep::Integer(120), PyRep::String("Strategic CTA".into()), PyRep::String("Fleet fight".into()), PyRep::Integer(3)], &kw, None).await.expect("CreateAllianceEvent");
    let ally_ev_id = get_i64(&ally_ev, "eventID").expect("eventID");
    assert_eq!(get_i64(&ally_ev, "ownerID"), Some(ally_id));

    let edit_ally = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditAllianceEvent", &[PyRep::Integer(ally_ev_id), PyRep::Integer(future_time), PyRep::Integer(future_time + 1000), PyRep::Integer(180), PyRep::String("Strategic CTA Rescheduled".into()), PyRep::String("Stand by in staging".into()), PyRep::Integer(3)], &kw, None).await.expect("EditAllianceEvent");
    assert_eq!(get_i64(&edit_ally, "eventID"), Some(ally_ev_id));
    let ally_details = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(ally_ev_id)], &kw, None).await.expect("GetEventDetails ally");
    assert_eq!(get_str(&ally_details, "title"), Some("Strategic CTA Rescheduled"));
    assert_eq!(get_str(&ally_details, "description"), Some("Stand by in staging"));
    assert_eq!(get_i64(&ally_details, "dateTime"), Some(future_time + 1000));

    // 9. DeleteEvent: unauthorized fails, owner succeeds and emits OnRemoveCalendarEvent
    let unauth_del = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "DeleteEvent", &[PyRep::Integer(ev1_id)], &kw, None).await.unwrap_err();
    assert_eq!(unauth_del.code, "CalendarAccessDenied");

    let _ = ctx.drain_notifications_for_char(PILOT2);
    let del_rep = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "DeleteEvent", &[PyRep::Integer(ev1_id)], &kw, None).await.expect("DeleteEvent");
    assert_eq!(get_bool(&del_rep, "deleted"), Some(true));
    assert!(
        ctx.drain_notifications_for_char(PILOT2)
            .iter()
            .any(|n| n.event == "OnRemoveCalendarEvent" && n.args.first() == Some(&PyRep::Integer(ev1_id))),
        "Participant Pilot 2 must receive OnRemoveCalendarEvent on DeleteEvent"
    );

    let not_found_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "GetResponsesToEvent", &[PyRep::Integer(ev1_id)], &kw, None).await.unwrap_err();
    assert_eq!(not_found_err.code, "CalendarEventNotFound");

    // 10. SQLite persistence round-trip for calendar events, edits, and soft-deletes
    let ctx_reloaded = crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "calendar_remediation");
    common::pin_clock(&ctx_reloaded);
    let mut sess1_reloaded = setup_pilot(&ctx_reloaded, 2004, "demo", "demo-secret", PILOT1).await;
    sess1_reloaded.corporation_id = Some(corp_id);
    sess1_reloaded.alliance_id = Some(ally_id);
    let corp_after_reload = ctx_reloaded.dispatch_rpc(&mut sess1_reloaded, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(corp_ev_id)], &kw, None).await.expect("GetEventDetails corp after SQLite reload");
    assert_eq!(get_str(&corp_after_reload, "title"), Some("Updated Mining Op"));
    assert_eq!(get_i64(&corp_after_reload, "dateTime"), Some(future_time + 500));
    let del_after_reload = ctx_reloaded.dispatch_rpc(&mut sess1_reloaded, "calendarMgr", None, "GetResponsesToEvent", &[PyRep::Integer(ev1_id)], &kw, None).await.unwrap_err();
    assert_eq!(del_after_reload.code, "CalendarEventNotFound");
}
