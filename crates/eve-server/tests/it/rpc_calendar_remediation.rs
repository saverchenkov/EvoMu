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
    let member_corp_id = 98_000_103i64;
    let ally_id = 99_000_101i64;

    let uid3 = ctx.db.provision_player_account("pilot3", "p3-sec");
    let pilot3 = ctx
        .db
        .create_character(uid3, "Pilot Three", 1, 1, 11, 17, &ctx.sde)
        .expect("create pilot3")
        .character_id;
    let uid4 = ctx.db.provision_player_account("pilot4", "p4-sec");
    let pilot4 = ctx
        .db
        .create_character(uid4, "Pilot Four", 1, 1, 11, 17, &ctx.sde)
        .expect("create pilot4")
        .character_id;

    let mut sess1 = setup_pilot(&ctx, 2001, "demo", "demo-secret", PILOT1).await;
    let mut sess2 = setup_pilot(&ctx, 2002, "other", "other-secret", PILOT2).await;
    let mut sess3 = setup_pilot(&ctx, 2003, "pilot3", "p3-sec", pilot3).await;
    let mut sess4 = setup_pilot(&ctx, 2005, "pilot4", "p4-sec", pilot4).await;

    sess1.corporation_id = Some(corp_id);
    sess1.alliance_id = Some(ally_id);
    sess2.corporation_id = Some(corp_id);
    sess2.alliance_id = Some(ally_id);
    sess3.corporation_id = Some(98_000_102);
    sess4.corporation_id = Some(member_corp_id);
    sess4.alliance_id = Some(ally_id);

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
        if let Some(c4) = snap.characters.get_mut(&pilot4) {
            c4.corporation_id = member_corp_id;
            c4.alliance_id = Some(ally_id);
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
        snap.corporations.insert(
            member_corp_id,
            eve_db::CorporationRecord {
                corporation_id: member_corp_id,
                name: "NonExec Member Corp".into(),
                ticker: "NEXEC".into(),
                ceo_id: pilot4,
                alliance_id: Some(ally_id),
                tax_rate: 0.05,
                master_wallet: eve_db::IskCents(10_000_000_00),
                members: std::collections::HashSet::from([pilot4]),
                member_roles: HashMap::from([(pilot4, eve_db::corp_roles::ROLE_DIRECTOR)]),
                applications: HashMap::new(),
                wallet_divisions: eve_db::CorporationRecord::default_wallet_divisions(eve_db::IskCents(10_000_000_00)),
                description: "Non-executor alliance member corp".into(),
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
                member_corp_ids: std::collections::HashSet::from([corp_id, member_corp_id]),
            },
        );
    });

    let kw = HashMap::new();
    let future_time = ctx.now_blueos_filetime_ticks() + 86_400 * 10_000_000;

    // 1. CreatePersonalEvent: rejects past dateTime, empty title, negative duration, and over-long duration
    let past_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreatePersonalEvent", &[PyRep::Integer(100_000_000), PyRep::Integer(60), PyRep::String("Past Event".into()), PyRep::String("Should fail".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(past_err.code, "UserError");

    let empty_title_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreatePersonalEvent", &[PyRep::Integer(future_time), PyRep::Integer(60), PyRep::String("   ".into()), PyRep::String("Should fail".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(empty_title_err.code, "UserError");

    let neg_dur_create_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreatePersonalEvent", &[PyRep::Integer(future_time), PyRep::Integer(-30), PyRep::String("Neg Dur".into()), PyRep::String("Should fail".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(neg_dur_create_err.code, "UserError");

    let long_dur_create_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreatePersonalEvent", &[PyRep::Integer(future_time), PyRep::Integer(20_000), PyRep::String("Long Dur".into()), PyRep::String("Should fail".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(long_dur_create_err.code, "UserError");

    let _ = ctx.drain_notifications_for_char(PILOT2);
    // CreatePersonalEvent: succeeds with future dateTime and returns KeyVal
    let ev1 = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreatePersonalEvent", &[PyRep::Integer(future_time), PyRep::Integer(60), PyRep::String("Ops Sync".into()), PyRep::String("Strategy meeting".into()), PyRep::Integer(1)], &kw, None).await.expect("CreatePersonalEvent");
    let ev1_id = get_i64(&ev1, "eventID").expect("eventID");
    assert_eq!(get_i64(&ev1, "ownerID"), Some(PILOT1));

    // 2. EditPersonalEvent (7-arg client arity: eventID, oldDateTime, dateTime, duration, title, description, importance)
    let unauth_edit = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "EditPersonalEvent", &[PyRep::Integer(ev1_id), PyRep::Integer(future_time), PyRep::Integer(future_time + 1000), PyRep::Integer(90), PyRep::String("Hacked Title".into()), PyRep::String("Hacked Desc".into()), PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(unauth_edit.code, "CalendarAccessDenied");

    // Reject empty title, past dateTime, and negative duration on EditPersonalEvent
    let empty_edit_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditPersonalEvent", &[PyRep::Integer(ev1_id), PyRep::Integer(future_time), PyRep::Integer(future_time + 2000), PyRep::Integer(90), PyRep::String("".into()), PyRep::String("Updated Strategy".into()), PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(empty_edit_err.code, "UserError");
    let past_edit_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditPersonalEvent", &[PyRep::Integer(ev1_id), PyRep::Integer(future_time), PyRep::Integer(100_000_000), PyRep::Integer(90), PyRep::String("Past Edit".into()), PyRep::String("Should not apply".into()), PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(past_edit_err.code, "UserError");
    let past_edit_5arg_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditPersonalEvent", &[PyRep::Integer(ev1_id), PyRep::Integer(100_000_000), PyRep::Integer(90), PyRep::String("Past 5-arg Edit".into()), PyRep::String("Should not apply".into())], &kw, None).await.unwrap_err();
    assert_eq!(past_edit_5arg_err.code, "UserError");
    let neg_dur_edit_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditPersonalEvent", &[PyRep::Integer(ev1_id), PyRep::Integer(future_time), PyRep::Integer(future_time + 2000), PyRep::Integer(-30), PyRep::String("Neg Dur Edit".into()), PyRep::String("Should not apply".into()), PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(neg_dur_edit_err.code, "UserError");
    let pre_edit_details = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(ev1_id)], &kw, None).await.expect("GetEventDetails pre-edit");
    assert_eq!(get_str(&pre_edit_details, "title"), Some("Ops Sync"));
    assert_eq!(get_i64(&pre_edit_details, "dateTime"), Some(future_time));
    assert_eq!(get_i64(&pre_edit_details, "duration"), Some(60));

    // Authorized owner edits personal event with 7-arg client arity -> emits OnEditCalendarEvent
    let _ = ctx.drain_notifications_for_char(PILOT1);
    let edit_rep = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditPersonalEvent", &[PyRep::Integer(ev1_id), PyRep::Integer(future_time), PyRep::Integer(future_time + 2000), PyRep::Integer(90), PyRep::String("Updated Ops Sync".into()), PyRep::String("Updated Strategy".into()), PyRep::Integer(2)], &kw, None).await.expect("EditPersonalEvent");
    assert_eq!(get_i64(&edit_rep, "eventID"), Some(ev1_id));
    assert!(
        ctx.drain_notifications_for_char(PILOT1)
            .iter()
            .any(|n| n.event == "OnEditCalendarEvent" && n.args.first() == Some(&PyRep::Integer(ev1_id))),
        "EditPersonalEvent must emit OnEditCalendarEvent"
    );
    let edit_details = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(ev1_id)], &kw, None).await.expect("GetEventDetails");
    assert_eq!(get_str(&edit_details, "title"), Some("Updated Ops Sync"));
    assert_eq!(get_str(&edit_details, "description"), Some("Updated Strategy"));
    assert_eq!(get_i64(&edit_details, "dateTime"), Some(future_time + 2000));
    assert_eq!(get_i64(&edit_details, "duration"), Some(90));
    assert_eq!(get_i64(&edit_details, "importance"), Some(2));

    // Uninvited outsider Pilot 3 cannot view personal event details via calendarMgr or calendarProxy
    let unauth_details = ctx.dispatch_rpc(&mut sess3, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(ev1_id)], &kw, None).await.unwrap_err();
    assert_eq!(unauth_details.code, "CalendarAccessDenied");
    let unauth_proxy_details = ctx.dispatch_rpc(&mut sess3, "calendarProxy", None, "GetEventDetails", &[PyRep::Integer(ev1_id), PyRep::Integer(PILOT1)], &kw, None).await.unwrap_err();
    assert_eq!(unauth_proxy_details.code, "CalendarAccessDenied");
    let proxy_details = ctx.dispatch_rpc(&mut sess1, "calendarProxy", None, "GetEventDetails", &[PyRep::Integer(ev1_id), PyRep::Integer(PILOT1)], &kw, None).await.expect("calendarProxy.GetEventDetails");
    assert_eq!(get_str(&proxy_details, "title"), Some("Updated Ops Sync"));
    assert_eq!(get_str(&proxy_details, "eventTitle"), Some("Updated Ops Sync"));
    assert_eq!(get_str(&proxy_details, "eventText"), Some("Updated Strategy"));
    assert_eq!(get_i64(&proxy_details, "eventDuration"), Some(90));
    assert_eq!(get_i64(&proxy_details, "flag"), Some(1));
    assert_eq!(get_i64(&proxy_details, "eventTag"), Some(1));
    assert_eq!(get_bool(&proxy_details, "isDeleted"), Some(false));
    assert!(get_i64(&proxy_details, "dateModified").unwrap_or(0) > 0);

    // Clearing duration by passing None in EditPersonalEvent sets duration to 0
    ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditPersonalEvent", &[PyRep::Integer(ev1_id), PyRep::Integer(future_time + 2000), PyRep::Integer(future_time + 2000), PyRep::None, PyRep::String("Updated Ops Sync".into()), PyRep::String("Updated Strategy".into()), PyRep::Integer(2)], &kw, None).await.expect("EditPersonalEvent clear duration");
    let cleared_dur_details = ctx.dispatch_rpc(&mut sess1, "calendarProxy", None, "GetEventDetails", &[PyRep::Integer(ev1_id), PyRep::Integer(PILOT1)], &kw, None).await.expect("GetEventDetails after clearing duration");
    assert_eq!(get_i64(&cleared_dur_details, "eventDuration"), Some(0));

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

    // 4. SendEventResponse (3-arg client arity: eventID, ownerID, response; valid client RSVP codes are {2, 4, 5})
    let unauth_resp = ctx.dispatch_rpc(&mut sess3, "calendarMgr", None, "SendEventResponse", &[PyRep::Integer(ev1_id), PyRep::Integer(PILOT1), PyRep::Integer(4)], &kw, None).await.unwrap_err();
    assert_eq!(unauth_resp.code, "CalendarAccessDenied");

    // Reject invalid/internal RSVP status codes (1=Deleted, 3=NotResponded, 99=Out-of-range)
    for invalid_code in [1, 3, 99] {
        let bad_status_err = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "SendEventResponse", &[PyRep::Integer(ev1_id), PyRep::Integer(PILOT1), PyRep::Integer(invalid_code)], &kw, None).await.unwrap_err();
        assert_eq!(bad_status_err.code, "UserError");
    }

    // Invited Pilot 2 responds with 2 (Declined), 5 (Tentative), and 4 (Accepted) using 3-arg SendEventResponse(eventID, ownerID, response)
    for valid_code in [2, 5, 4] {
        let send_resp = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "SendEventResponse", &[PyRep::Integer(ev1_id), PyRep::Integer(PILOT1), PyRep::Integer(valid_code)], &kw, None).await.expect("SendEventResponse");
        assert_eq!(get_i64(&send_resp, "status"), Some(valid_code));
    }

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
    assert!(resp_rows.iter().any(|r| r[0] == PyRep::Integer(PILOT2) && r[1] == PyRep::Integer(4)));

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
    assert!(char_resp_rows.iter().any(|r| r[0] == PyRep::Integer(ev1_id) && r[1] == PyRep::Integer(4)));

    // 7. CreateCorporationEvent & EditCorporationEvent (including corpRoleChatManager = 1 << 55 without Director)
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

    // Grant Pilot 2 ONLY corpRoleChatManager (Communications Officer = 1 << 55) without ROLE_DIRECTOR
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(PILOT2, 1u64 << 55);
    });

    // CreateCorporationEvent: rejects past dateTime and empty title
    let corp_past_err = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "CreateCorporationEvent", &[PyRep::Integer(100_000_000), PyRep::Integer(60), PyRep::String("Past Corp Op".into()), PyRep::String("Desc".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(corp_past_err.code, "UserError");
    let corp_empty_err = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "CreateCorporationEvent", &[PyRep::Integer(future_time), PyRep::Integer(60), PyRep::String("".into()), PyRep::String("Desc".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(corp_empty_err.code, "UserError");

    let _ = ctx.drain_notifications_for_char(PILOT1);
    let corp_ev = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "CreateCorporationEvent", &[PyRep::Integer(future_time), PyRep::Integer(60), PyRep::String("Corp Mining Op".into()), PyRep::String("Moon mining".into()), PyRep::Integer(1)], &kw, None).await.expect("CreateCorporationEvent by Comms Officer");
    let corp_ev_id = get_i64(&corp_ev, "eventID").expect("eventID");
    assert_eq!(get_i64(&corp_ev, "ownerID"), Some(corp_id));
    let p1_corp_create_notifs = ctx.drain_notifications_for_char(PILOT1);
    let corp_new_notif = p1_corp_create_notifs.iter().find(|n| n.event == "OnNewCalendarEvent" && n.args.first() == Some(&PyRep::Integer(corp_ev_id))).expect("OnNewCalendarEvent on CreateCorporationEvent");
    assert_eq!(
        corp_new_notif.args,
        vec![
            PyRep::Integer(corp_ev_id),
            PyRep::Integer(corp_id),
            PyRep::Integer(future_time),
            PyRep::Integer(60),
            PyRep::String("Corp Mining Op".into()),
            PyRep::Integer(1),
            PyRep::None,
            PyRep::Bool(true),
        ],
        "OnNewCalendarEvent must carry 8 positional arguments"
    );

    // EditPersonalEvent cannot edit a Corporation event
    let wrong_scope_edit = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditPersonalEvent", &[PyRep::Integer(corp_ev_id), PyRep::Integer(future_time), PyRep::Integer(future_time + 500), PyRep::Integer(120), PyRep::String("Wrong Scope".into()), PyRep::String("Should fail".into()), PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(wrong_scope_edit.code, "CalendarAccessDenied");

    let past_corp_edit = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "EditCorporationEvent", &[PyRep::Integer(corp_ev_id), PyRep::Integer(future_time), PyRep::Integer(100_000_000), PyRep::Integer(120), PyRep::String("Past Corp Edit".into()), PyRep::String("Should fail".into()), PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(past_corp_edit.code, "UserError");
    let neg_dur_corp_edit = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "EditCorporationEvent", &[PyRep::Integer(corp_ev_id), PyRep::Integer(future_time), PyRep::Integer(future_time + 500), PyRep::Integer(-15), PyRep::String("Neg Dur Corp Edit".into()), PyRep::String("Should fail".into()), PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(neg_dur_corp_edit.code, "UserError");
    let corp_pre_edit = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(corp_ev_id)], &kw, None).await.expect("GetEventDetails corp pre-edit");
    assert_eq!(get_str(&corp_pre_edit, "title"), Some("Corp Mining Op"));
    assert_eq!(get_i64(&corp_pre_edit, "dateTime"), Some(future_time));
    assert_eq!(get_i64(&corp_pre_edit, "duration"), Some(60));

    let _ = ctx.drain_notifications_for_char(PILOT1);
    let edit_corp = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "EditCorporationEvent", &[PyRep::Integer(corp_ev_id), PyRep::Integer(future_time), PyRep::Integer(future_time + 500), PyRep::Integer(120), PyRep::String("Updated Mining Op".into()), PyRep::String("Moon mining 2".into()), PyRep::Integer(2)], &kw, None).await.expect("EditCorporationEvent by Comms Officer");
    assert_eq!(get_i64(&edit_corp, "eventID"), Some(corp_ev_id));
    let p1_corp_edit_notifs = ctx.drain_notifications_for_char(PILOT1);
    let corp_edit_notif = p1_corp_edit_notifs.iter().find(|n| n.event == "OnEditCalendarEvent" && n.args.first() == Some(&PyRep::Integer(corp_ev_id))).expect("OnEditCalendarEvent on EditCorporationEvent");
    assert_eq!(
        corp_edit_notif.args,
        vec![
            PyRep::Integer(corp_ev_id),
            PyRep::Integer(corp_id),
            PyRep::Integer(future_time),
            PyRep::Integer(future_time + 500),
            PyRep::Integer(120),
            PyRep::String("Updated Mining Op".into()),
            PyRep::Integer(2),
            PyRep::None,
        ],
        "OnEditCalendarEvent must carry 8 positional arguments"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.remove(&PILOT2);
    });
    // Creator Pilot 2 whose Communications Officer role was revoked can no longer edit or delete the corporation event
    let revoked_edit = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "EditCorporationEvent", &[PyRep::Integer(corp_ev_id), PyRep::Integer(future_time + 500), PyRep::Integer(future_time + 600), PyRep::Integer(120), PyRep::String("Revoked Edit".into()), PyRep::String("Should fail".into()), PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(revoked_edit.code, "CalendarAccessDenied");
    let revoked_del = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "DeleteEvent", &[PyRep::Integer(corp_ev_id)], &kw, None).await.unwrap_err();
    assert_eq!(revoked_del.code, "CalendarAccessDenied");

    let corp_details = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(corp_ev_id)], &kw, None).await.expect("GetEventDetails corp");
    assert_eq!(get_str(&corp_details, "title"), Some("Updated Mining Op"));
    assert_eq!(get_str(&corp_details, "description"), Some("Moon mining 2"));
    assert_eq!(get_i64(&corp_details, "dateTime"), Some(future_time + 500));

    // Automated moon extraction event + /createccpevent ROLE_GMH check + split-brain sync & mutation auth checks
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(990_000_777, eve_db::UpwellStructureRecord {
            structure_id: 990_000_777, type_id: 35835, name: "Remediation Refinery".into(), owner_corp_id: corp_id,
            solar_system_id: 30_000_142, position: [100_000.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
            quantum_core_installed: true, fuel_blocks: 1_000, shield_hp: 5_000.0, armor_hp: 5_000.0, hull_hp: 5_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0,
        });
    });
    let _ = ctx.drain_notifications_for_char(PILOT2);
    let ext_rep = ctx.dispatch_rpc(&mut sess1, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(990_000_777), PyRep::Integer(518_400)], &kw, None).await.expect("StartNewExtraction");
    let auto_ev_id = get_i64(&ext_rep, "eventID").expect("auto_ev_id");
    let arr_unix = get_i64(&ext_rep, "chunkArrivalTime").expect("chunkArrivalTime");
    let arr_ft = (arr_unix + 11_644_473_600) * 10_000_000;
    let p2_auto_notifs = ctx.drain_notifications_for_char(PILOT2);
    let auto_new_notif = p2_auto_notifs.iter().find(|n| n.event == "OnNewCalendarEvent" && n.args.first() == Some(&PyRep::Integer(auto_ev_id))).expect("OnNewCalendarEvent on StartNewExtraction");
    assert_eq!(auto_new_notif.args.len(), 8);
    assert_eq!(auto_new_notif.args[6], PyRep::Integer(1), "Automated moon extraction event must set autoEventType=1");
    assert_eq!(auto_new_notif.args[7], PyRep::Bool(true));

    // /createccpevent rejects ROLE_GML (non-GMH) and succeeds with ROLE_GMH without overwriting automated event in social_extras/snap
    let prev_role = sess1.role;
    sess1.role = (sess1.role & !eve_machonet::service_roles::ROLE_GM_MASK) | eve_machonet::service_roles::ROLE_GML;
    let gml_ccp_err = ctx.dispatch_rpc(&mut sess1, "slash", None, "SlashCmd", &[PyRep::String(format!("/createccpevent {arr_ft} 1440 2 GML Attempt | Denied"))], &kw, None).await.unwrap_err();
    assert_eq!(gml_ccp_err.code, "InsufficientRole");
    sess1.role |= eve_machonet::service_roles::ROLE_GMH;
    let ccp_ev = ctx.dispatch_rpc(&mut sess1, "slash", None, "SlashCmd", &[PyRep::String(format!("/createccpevent {arr_ft} 1440 2 CCP Gala | Global"))], &kw, None).await.expect("/createccpevent with ROLE_GMH");
    sess1.role = prev_role;
    let ccp_ev_id = get_i64(&ccp_ev, "eventID").expect("ccp_ev_id");
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "DeleteEvent", &[PyRep::Integer(ccp_ev_id)], &kw, None).await.unwrap_err().code, "CalendarAccessDenied");

    // GetEventDetails(auto_ev_id) succeeds even after /createccpevent triggered persist_social_state
    let auto_details = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(auto_ev_id)], &kw, None).await.expect("GetEventDetails on automated event after /createccpevent");
    assert_eq!(get_i64(&auto_details, "autoEventType"), Some(1));
    assert_eq!(get_i64(&auto_details, "flag"), Some(16));

    // Manual player DeleteEvent / UpdateEventParticipants / EditCorporationEvent on Automated event are rejected
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "DeleteEvent", &[PyRep::Integer(auto_ev_id)], &kw, None).await.unwrap_err().code, "CalendarAccessDenied");
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "UpdateEventParticipants", &[PyRep::Integer(auto_ev_id), PyRep::List(vec![PyRep::Integer(PILOT2)]), PyRep::List(vec![])], &kw, None).await.unwrap_err().code, "CalendarAccessDenied");

    // CancelExtraction emits 3-arg OnRemoveCalendarEvent(auto_ev_id, arr_ft, true) and soft-deletes in both social_extras and snap
    let _ = ctx.drain_notifications_for_char(PILOT2);
    ctx.dispatch_rpc(&mut sess1, "moonExtractions", None, "CancelExtraction", &[PyRep::Integer(990_000_777)], &kw, None).await.expect("CancelExtraction");
    let p2_cancel_notifs = ctx.drain_notifications_for_char(PILOT2);
    let cancel_notif = p2_cancel_notifs.iter().find(|n| n.event == "OnRemoveCalendarEvent" && n.args.first() == Some(&PyRep::Integer(auto_ev_id))).expect("OnRemoveCalendarEvent on CancelExtraction");
    assert_eq!(cancel_notif.args, vec![PyRep::Integer(auto_ev_id), PyRep::Integer(arr_ft), PyRep::Bool(true)]);
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(auto_ev_id)], &kw, None).await.unwrap_err().code, "CalendarEventNotFound");

    // Pilot 2 RSVPs 4 (Accepted) to corp_ev_id so we can verify RSVP persistence across SQLite reload
    ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "SendEventResponse", &[PyRep::Integer(corp_ev_id), PyRep::Integer(corp_id), PyRep::Integer(4)], &kw, None).await.expect("SendEventResponse on corp event");

    // 8. CreateAllianceEvent & EditAllianceEvent
    let non_ally_ev = ctx.dispatch_rpc(&mut sess3, "calendarMgr", None, "CreateAllianceEvent", &[PyRep::Integer(future_time), PyRep::Integer(60), PyRep::String("Invalid Ally Event".into()), PyRep::String("No ally".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(non_ally_ev.code, "CalendarAccessDenied");

    // Director of non-executor member corp in the same alliance cannot create alliance events
    let non_exec_ally_ev = ctx.dispatch_rpc(&mut sess4, "calendarMgr", None, "CreateAllianceEvent", &[PyRep::Integer(future_time), PyRep::Integer(60), PyRep::String("NonExec Ally Event".into()), PyRep::String("Not executor".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err();
    assert_eq!(non_exec_ally_ev.code, "CalendarAccessDenied");

    let ally_past_err = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreateAllianceEvent", &[PyRep::Integer(100_000_000), PyRep::Integer(120), PyRep::String("Past CTA".into()), PyRep::String("Desc".into()), PyRep::Integer(3)], &kw, None).await.unwrap_err();
    assert_eq!(ally_past_err.code, "UserError");

    let _ = ctx.drain_notifications_for_char(pilot4);
    let ally_ev = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreateAllianceEvent", &[PyRep::Integer(future_time), PyRep::Integer(120), PyRep::String("Strategic CTA".into()), PyRep::String("Fleet fight".into()), PyRep::Integer(3)], &kw, None).await.expect("CreateAllianceEvent");
    let ally_ev_id = get_i64(&ally_ev, "eventID").expect("eventID");
    assert_eq!(get_i64(&ally_ev, "ownerID"), Some(ally_id));
    assert!(
        ctx.drain_notifications_for_char(pilot4)
            .iter()
            .any(|n| n.event == "OnNewCalendarEvent" && n.args.first() == Some(&PyRep::Integer(ally_ev_id))),
        "Alliance member Pilot 4 in non-executor corp must receive OnNewCalendarEvent"
    );

    // Alliance member in non-executor corp sees the alliance event in calendarProxy.GetEventList(month, year) (ownerType=16159)
    let p4_month_evs = ctx.dispatch_rpc(&mut sess4, "calendarProxy", None, "GetEventList", &[PyRep::Integer(1), PyRep::Integer(2027)], &kw, None).await.expect("calendarProxy.GetEventList for alliance member");
    let PyRep::List(p4_list) = p4_month_evs else { panic!("expected outer list from calendarProxy.GetEventList") };
    let Some(PyRep::Rowset { rows: p4_rows, .. }) = p4_list.first() else { panic!("expected rowset in outer list") };
    assert!(
        p4_rows.iter().any(|r| r[0] == PyRep::Integer(ally_ev_id) && r[1] == PyRep::Integer(ally_id) && r[6] == PyRep::Integer(16159)),
        "Alliance member must see alliance event in calendarProxy.GetEventList(1, 2027)"
    );
    let p4_other_month = ctx.dispatch_rpc(&mut sess4, "calendarProxy", None, "GetEventList", &[PyRep::Integer(2), PyRep::Integer(2027)], &kw, None).await.expect("calendarProxy.GetEventList for empty month");
    let PyRep::List(p4_other_list) = p4_other_month else { panic!("expected outer list") };
    let Some(PyRep::Rowset { rows: p4_other_rows, .. }) = p4_other_list.first() else { panic!("expected rowset") };
    assert!(p4_other_rows.is_empty(), "calendarProxy.GetEventList must filter by month and year");

    let past_ally_edit = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditAllianceEvent", &[PyRep::Integer(ally_ev_id), PyRep::Integer(future_time), PyRep::Integer(100_000_000), PyRep::Integer(180), PyRep::String("Past Ally Edit".into()), PyRep::String("Should fail".into()), PyRep::Integer(3)], &kw, None).await.unwrap_err();
    assert_eq!(past_ally_edit.code, "UserError");
    let neg_dur_ally_edit = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditAllianceEvent", &[PyRep::Integer(ally_ev_id), PyRep::Integer(future_time), PyRep::Integer(future_time + 1000), PyRep::Integer(-10), PyRep::String("Neg Dur Ally Edit".into()), PyRep::String("Should fail".into()), PyRep::Integer(3)], &kw, None).await.unwrap_err();
    assert_eq!(neg_dur_ally_edit.code, "UserError");
    let ally_pre_edit = ctx.dispatch_rpc(&mut sess4, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(ally_ev_id)], &kw, None).await.expect("GetEventDetails ally pre-edit");
    assert_eq!(get_str(&ally_pre_edit, "title"), Some("Strategic CTA"));
    assert_eq!(get_i64(&ally_pre_edit, "dateTime"), Some(future_time));
    assert_eq!(get_i64(&ally_pre_edit, "duration"), Some(120));

    let _ = ctx.drain_notifications_for_char(pilot4);
    let edit_ally = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditAllianceEvent", &[PyRep::Integer(ally_ev_id), PyRep::Integer(future_time), PyRep::Integer(future_time + 1000), PyRep::Integer(180), PyRep::String("Strategic CTA Rescheduled".into()), PyRep::String("Stand by in staging".into()), PyRep::Integer(3)], &kw, None).await.expect("EditAllianceEvent");
    assert_eq!(get_i64(&edit_ally, "eventID"), Some(ally_ev_id));
    assert!(
        ctx.drain_notifications_for_char(pilot4)
            .iter()
            .any(|n| n.event == "OnEditCalendarEvent" && n.args.first() == Some(&PyRep::Integer(ally_ev_id))),
        "Alliance member Pilot 4 must receive OnEditCalendarEvent on EditAllianceEvent"
    );
    let ally_details = ctx.dispatch_rpc(&mut sess4, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(ally_ev_id)], &kw, None).await.expect("GetEventDetails ally");
    assert_eq!(get_str(&ally_details, "title"), Some("Strategic CTA Rescheduled"));
    assert_eq!(get_str(&ally_details, "description"), Some("Stand by in staging"));
    assert_eq!(get_i64(&ally_details, "dateTime"), Some(future_time + 1000));

    // Create a second personal event ev2 with Pilot 2 invited, then UpdateEventParticipants(ev2, [pilot3], [PILOT2])
    let ev2 = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreatePersonalEvent", &[PyRep::Integer(future_time), PyRep::Integer(45), PyRep::String("Roster Swap".into()), PyRep::String("Swap invitees".into()), PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(PILOT2)])], &kw, None).await.expect("CreatePersonalEvent ev2");
    let ev2_id = get_i64(&ev2, "eventID").expect("ev2_id");
    let _ = ctx.drain_notifications_for_char(pilot3);
    let _ = ctx.drain_notifications_for_char(PILOT2);
    ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "UpdateEventParticipants", &[PyRep::Integer(ev2_id), PyRep::List(vec![PyRep::Integer(pilot3)]), PyRep::List(vec![PyRep::Integer(PILOT2)])], &kw, None).await.expect("UpdateEventParticipants add pilot3 remove PILOT2");
    assert!(
        ctx.drain_notifications_for_char(pilot3).iter().any(|n| n.event == "OnNewCalendarEvent" && n.args.first() == Some(&PyRep::Integer(ev2_id))),
        "Added participant Pilot 3 must receive OnNewCalendarEvent"
    );
    assert!(
        ctx.drain_notifications_for_char(PILOT2).iter().any(|n| n.event == "OnRemoveCalendarEvent" && n.args.first() == Some(&PyRep::Integer(ev2_id))),
        "Removed participant Pilot 2 must receive OnRemoveCalendarEvent"
    );

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

    // 10. SQLite persistence round-trip for calendar events, alliance events, RSVP responses, and soft-deletes
    let ctx_reloaded = crate::sp6_logoff_and_server_tick::persist_and_reopen_sqlite(&ctx, "calendar_remediation");
    common::pin_clock(&ctx_reloaded);
    let mut sess1_reloaded = setup_pilot(&ctx_reloaded, 2004, "demo", "demo-secret", PILOT1).await;
    sess1_reloaded.corporation_id = Some(corp_id);
    sess1_reloaded.alliance_id = Some(ally_id);
    let mut sess2_reloaded = setup_pilot(&ctx_reloaded, 2006, "other", "other-secret", PILOT2).await;
    sess2_reloaded.corporation_id = Some(corp_id);
    sess2_reloaded.alliance_id = Some(ally_id);

    let corp_after_reload = ctx_reloaded.dispatch_rpc(&mut sess1_reloaded, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(corp_ev_id)], &kw, None).await.expect("GetEventDetails corp after SQLite reload");
    assert_eq!(get_str(&corp_after_reload, "title"), Some("Updated Mining Op"));
    assert_eq!(get_i64(&corp_after_reload, "dateTime"), Some(future_time + 500));

    let ally_after_reload = ctx_reloaded.dispatch_rpc(&mut sess1_reloaded, "calendarMgr", None, "GetEventDetails", &[PyRep::Integer(ally_ev_id)], &kw, None).await.expect("GetEventDetails ally after SQLite reload");
    assert_eq!(get_str(&ally_after_reload, "title"), Some("Strategic CTA Rescheduled"));
    assert_eq!(get_i64(&ally_after_reload, "ownerID"), Some(ally_id));

    let rsvp_after_reload = ctx_reloaded.dispatch_rpc(&mut sess2_reloaded, "calendarMgr", None, "GetResponsesToEvent", &[PyRep::Integer(corp_ev_id)], &kw, None).await.expect("GetResponsesToEvent after SQLite reload");
    let PyRep::Rowset { rows: rsvp_reload_rows, .. } = rsvp_after_reload else { panic!("expected rowset") };
    assert!(
        rsvp_reload_rows.iter().any(|r| r[0] == PyRep::Integer(PILOT2) && r[1] == PyRep::Integer(4)),
        "RSVP response (4=Accepted) must survive SQLite reload"
    );

    let del_after_reload = ctx_reloaded.dispatch_rpc(&mut sess1_reloaded, "calendarMgr", None, "GetResponsesToEvent", &[PyRep::Integer(ev1_id)], &kw, None).await.unwrap_err();
    assert_eq!(del_after_reload.code, "CalendarEventNotFound");

    let ev2_parts_after_reload = ctx_reloaded.dispatch_rpc(&mut sess1_reloaded, "calendarMgr", None, "GetResponsesToEvent", &[PyRep::Integer(ev2_id)], &kw, None).await.expect("GetResponsesToEvent ev2 after SQLite reload");
    let PyRep::Rowset { rows: ev2_reload_rows, .. } = ev2_parts_after_reload else { panic!("expected rowset") };
    assert!(ev2_reload_rows.iter().any(|r| r[0] == PyRep::Integer(pilot3) && r[1] == PyRep::Integer(3)), "Added participant Pilot 3 must persist across SQLite reload");
    assert!(!ev2_reload_rows.iter().any(|r| r[0] == PyRep::Integer(PILOT2)), "Removed participant Pilot 2 must remain absent after SQLite reload");
}
