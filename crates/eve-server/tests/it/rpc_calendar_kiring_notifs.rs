//! Parity tests for Calendar (`calendarMgr`), Notifications (`notificationMgr`), and Online Status (`onlineStatus`).

#![forbid(unsafe_code)]

use crate::common;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn is_keyval(rep: &PyRep) -> bool {
    matches!(rep, PyRep::KeyVal(_))
}

fn is_rowset(rep: &PyRep) -> bool {
    matches!(rep, PyRep::Rowset { .. })
}

#[tokio::test]
async fn test_calendar_alliance_events_and_participant_management() {
    // parity: rpc:calendarMgr.CreateAllianceEvent, rpc:calendarMgr.EditAllianceEvent, rpc:calendarMgr.UpdateEventParticipants
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(901);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess1");

    let mut sess2 = SessionState::new(902);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess2");

    let p3_uid = ctx.db.provision_player_account("cal_pilot3", "p3-secret");
    let p3_char = ctx
        .db
        .create_character(p3_uid, "Calendar Pilot Three", 1, 1, 11, 17, &ctx.sde)
        .unwrap()
        .character_id;
    let mut sess3 = SessionState::new(903);
    common::login_pilot(&ctx, &mut sess3, "cal_pilot3", "p3-secret").await;
    ctx.dispatch_rpc(
        &mut sess3,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(p3_char)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess3");

    // Establish corporation and alliance membership
    let corp_id = 98_000_001i64;
    let ally_id = 99_000_001i64;
    sess1.corporation_id = Some(corp_id);
    sess1.alliance_id = Some(ally_id);
    sess2.corporation_id = Some(corp_id);
    sess2.alliance_id = Some(ally_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&90_000_001) {
            c1.corporation_id = corp_id;
            c1.alliance_id = Some(ally_id);
        }
        if let Some(c2) = snap.characters.get_mut(&90_000_002) {
            c2.corporation_id = corp_id;
            c2.alliance_id = Some(ally_id);
        }
        snap.corporations.entry(corp_id).or_insert_with(|| eve_db::CorporationRecord {
            corporation_id: corp_id,
            name: "Calendar Test Corp".into(),
            ticker: "CTST".into(),
            ceo_id: 90_000_001,
            alliance_id: Some(ally_id),
            tax_rate: 0.05,
            master_wallet: eve_db::IskCents(10_000_000_00),
            members: std::collections::HashSet::from([90_000_001, 90_000_002]),
            member_roles: HashMap::from([(90_000_001, eve_db::corp_roles::ROLE_DIRECTOR)]),
            applications: HashMap::new(),
            wallet_divisions: eve_db::CorporationRecord::default_wallet_divisions(eve_db::IskCents(10_000_000_00)),
            description: "Test corp".into(),
            url: "".into(),
            hq_station_id: Some(60_003_760),
        });
        snap.alliances.entry(ally_id).or_insert_with(|| eve_db::AllianceRecord {
            alliance_id: ally_id,
            name: "Calendar Test Alliance".into(),
            short_name: "CTAL".into(),
            executor_corp_id: corp_id,
            member_corp_ids: std::collections::HashSet::from([corp_id]),
        });
    });

    // 1. Unaffiliated Pilot 3 (no alliance) fails to CreateAllianceEvent
    let unauth_err = ctx
        .dispatch_rpc(
            &mut sess3,
            "calendarMgr",
            None,
            "CreateAllianceEvent",
            &[
                PyRep::Integer(135_500_000_000_000_000),
                PyRep::Integer(120),
                PyRep::String("Unaffiliated Op".into()),
                PyRep::String("No alliance".into()),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(unauth_err.code, "CalendarAccessDenied");

    // 2. Non-director member Pilot 2 fails to CreateAllianceEvent
    let role_err = ctx
        .dispatch_rpc(
            &mut sess2,
            "calendarMgr",
            None,
            "CreateAllianceEvent",
            &[
                PyRep::Integer(135_500_000_000_000_000),
                PyRep::Integer(120),
                PyRep::String("Unauthorized Op".into()),
                PyRep::String("No role".into()),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(role_err.code, "CalendarAccessDenied");

    // 3. CEO/Director Pilot 1 creates Alliance Event (`rpc:calendarMgr.CreateAllianceEvent`)
    let create_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "calendarMgr",
            None,
            "CreateAllianceEvent",
            &[
                PyRep::Integer(135_600_000_000_000_000),
                PyRep::Integer(180),
                PyRep::String("Alliance Strategic Fleet".into()),
                PyRep::String("Form up in staging system".into()),
                PyRep::Integer(2),
                PyRep::List(vec![]),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateAllianceEvent");
    assert!(is_keyval(&create_rep));
    let ally_ev_id = create_rep
        .dict_get("eventID")
        .and_then(|v| v.as_i64())
        .expect("eventID integer");
    assert_eq!(
        create_rep.dict_get("ownerID"),
        Some(&PyRep::Integer(ally_id)),
        "Alliance event ownerID must match alliance ID"
    );
    assert_eq!(
        create_rep.dict_get("title"),
        Some(&PyRep::String("Alliance Strategic Fleet".into()))
    );

    // 4. Pilot 1 edits Alliance Event (`rpc:calendarMgr.EditAllianceEvent`)
    let edit_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "calendarMgr",
            None,
            "EditAllianceEvent",
            &[
                PyRep::Integer(ally_ev_id),
                PyRep::Integer(135_600_000_000_000_000),
                PyRep::Integer(240),
                PyRep::String("Updated Alliance Strategic Fleet".into()),
                PyRep::String("Updated staging instructions".into()),
                PyRep::Integer(2),
            ],
            &kw,
            None,
        )
        .await
        .expect("EditAllianceEvent");
    assert!(is_keyval(&edit_rep));
    assert_eq!(
        edit_rep.dict_get("eventID"),
        Some(&PyRep::Integer(ally_ev_id))
    );
    assert_eq!(edit_rep.dict_get("status"), Some(&PyRep::Integer(1)));

    // Non-creator Pilot 2 cannot edit Alliance Event
    let edit_err = ctx
        .dispatch_rpc(
            &mut sess2,
            "calendarMgr",
            None,
            "EditAllianceEvent",
            &[
                PyRep::Integer(ally_ev_id),
                PyRep::Integer(135_600_000_000_000_000),
                PyRep::Integer(240),
                PyRep::String("Hacked Op".into()),
                PyRep::String("Unauthorized edit".into()),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(edit_err.code, "CalendarAccessDenied");

    // Non-existent event returns CalendarEventNotFound on EditAllianceEvent
    let missing_edit_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "calendarMgr",
            None,
            "EditAllianceEvent",
            &[
                PyRep::Integer(999_999_999),
                PyRep::Integer(135_600_000_000_000_000),
                PyRep::Integer(240),
                PyRep::String("Missing Op".into()),
                PyRep::String("Does not exist".into()),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(missing_edit_err.code, "CalendarEventNotFound");

    // Side-effect verification: GetEventDetails shows updated fields
    let details = ctx
        .dispatch_rpc(
            &mut sess1,
            "calendarMgr",
            None,
            "GetEventDetails",
            &[PyRep::Integer(ally_ev_id)],
            &kw,
            None,
        )
        .await
        .expect("GetEventDetails");
    assert_eq!(
        details.dict_get("title"),
        Some(&PyRep::String("Updated Alliance Strategic Fleet".into()))
    );
    assert_eq!(details.dict_get("duration"), Some(&PyRep::Integer(240)));

    // 5. UpdateEventParticipants (`rpc:calendarMgr.UpdateEventParticipants`)
    // Pilot 1 creates a personal event inviting Pilot 2
    let pers_ev = ctx
        .dispatch_rpc(
            &mut sess1,
            "calendarMgr",
            None,
            "CreatePersonalEvent",
            &[
                PyRep::Integer(135_700_000_000_000_000),
                PyRep::Integer(60),
                PyRep::String("Escort Op".into()),
                PyRep::String("Need haulers escorted".into()),
                PyRep::Integer(0),
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreatePersonalEvent");
    let pers_ev_id = pers_ev.dict_get("eventID").and_then(|v| v.as_i64()).unwrap();

    // Verify initial participants: creator (4) and invited Pilot 2 (3)
    let init_resps = ctx
        .dispatch_rpc(
            &mut sess1,
            "calendarMgr",
            None,
            "GetResponsesToEvent",
            &[PyRep::Integer(pers_ev_id)],
            &kw,
            None,
        )
        .await
        .expect("GetResponsesToEvent");
    assert!(is_rowset(&init_resps));
    let PyRep::Rowset { rows: init_rows, .. } = &init_resps else { panic!() };
    assert_eq!(init_rows.len(), 2);

    // Update participants: remove Pilot 2 and add Pilot 3
    let upd_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "calendarMgr",
            None,
            "UpdateEventParticipants",
            &[
                PyRep::Integer(pers_ev_id),
                PyRep::List(vec![PyRep::Integer(p3_char)]),
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
            ],
            &kw,
            None,
        )
        .await
        .expect("UpdateEventParticipants");
    assert!(is_keyval(&upd_rep));
    assert_eq!(
        upd_rep.dict_get("eventID"),
        Some(&PyRep::Integer(pers_ev_id))
    );
    assert_eq!(upd_rep.dict_get("status"), Some(&PyRep::Integer(1)));

    // Non-owner Pilot 2 cannot update participants
    let upd_err = ctx
        .dispatch_rpc(
            &mut sess2,
            "calendarMgr",
            None,
            "UpdateEventParticipants",
            &[
                PyRep::Integer(pers_ev_id),
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
                PyRep::List(vec![]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(upd_err.code, "CalendarAccessDenied");

    // Non-existent event returns CalendarEventNotFound on UpdateEventParticipants
    let missing_upd_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "calendarMgr",
            None,
            "UpdateEventParticipants",
            &[
                PyRep::Integer(999_999_999),
                PyRep::List(vec![PyRep::Integer(p3_char)]),
                PyRep::List(vec![]),
            ],
            &kw,
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(missing_upd_err.code, "CalendarEventNotFound");

    // Verification: responses now contain creator (90000001) and Pilot 3 (p3_char), not Pilot 2
    let after_resps = ctx
        .dispatch_rpc(
            &mut sess1,
            "calendarMgr",
            None,
            "GetResponsesToEvent",
            &[PyRep::Integer(pers_ev_id)],
            &kw,
            None,
        )
        .await
        .expect("GetResponsesToEvent after update");
    let PyRep::Rowset { rows: after_rows, .. } = after_resps else { panic!() };
    let participant_ids: Vec<i64> = after_rows.iter().filter_map(|r| r[0].as_i64()).collect();
    assert!(participant_ids.contains(&p3_char));
    assert!(!participant_ids.contains(&90_000_002));
}

#[tokio::test]
async fn test_notification_manager_group_lifecycle_and_emission() {
    // parity: rpc:notificationMgr.DeleteAllNotifications, rpc:notificationMgr.DeleteGroupNotifications, rpc:notificationMgr.LogNotificationInteraction, rpc:notificationMgr.MarkAllAsProcessed, rpc:notificationMgr.MarkGroupAsProcessed, notify:OnNotificationReceived, notify:OnNotificationDeleted
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess = SessionState::new(910);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");

    // Drain initial login notifications
    let _ = ctx.drain_notifications_for_char(90_000_001);

    // 1. Create notifications in two separate groups
    // Group 9: Bounties
    let n1 = ctx.create_character_notification(
        1_000_035,
        90_000_001,
        14,
        9,
        "bounty: 100000",
    );
    let n2 = ctx.create_character_notification(
        1_000_035,
        90_000_001,
        15,
        9,
        "bounty: 200000",
    );
    // Group 5: War declarations
    let n3 = ctx.create_character_notification(
        1_000_035,
        90_000_001,
        2,
        5,
        "war: declared",
    );

    // 2. Verify notify:OnNotificationReceived emission
    let drained = ctx.drain_notifications_for_char(90_000_001);
    let recv_events: Vec<_> = drained
        .into_iter()
        .filter(|n| n.event == "OnNotificationReceived")
        .collect();
    assert_eq!(recv_events.len(), 3, "Three OnNotificationReceived notifications emitted");
    assert_eq!(recv_events[0].id_type, "charid");
    assert_eq!(recv_events[0].args[0], PyRep::Integer(n1.notification_id));
    assert_eq!(recv_events[0].args[1], PyRep::Integer(14));
    assert_eq!(recv_events[1].args[0], PyRep::Integer(n2.notification_id));
    assert_eq!(recv_events[2].args[0], PyRep::Integer(n3.notification_id));

    // 3. LogNotificationInteraction (`rpc:notificationMgr.LogNotificationInteraction`)
    let log_res = ctx
        .dispatch_rpc(
            &mut sess,
            "notificationMgr",
            None,
            "LogNotificationInteraction",
            &[PyRep::Integer(n1.notification_id), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("LogNotificationInteraction");
    assert_eq!(log_res, PyRep::Bool(true));
    {
        let recs = ctx.notification_records.read().unwrap();
        let updated_n1 = recs
            .iter()
            .find(|r| r.notification_id == n1.notification_id)
            .expect("n1 in records");
        assert!(updated_n1.interacted, "n1 marked interacted by LogNotificationInteraction");
    }

    // 4. MarkGroupAsProcessed (`rpc:notificationMgr.MarkGroupAsProcessed`)
    let grp_proc = ctx
        .dispatch_rpc(
            &mut sess,
            "notificationMgr",
            None,
            "MarkGroupAsProcessed",
            &[PyRep::Integer(9)],
            &kw,
            None,
        )
        .await
        .expect("MarkGroupAsProcessed");
    assert_eq!(grp_proc, PyRep::Integer(2), "2 notifications in group 9 marked processed");

    // Verify side-effect: GetUnprocessed only returns group 5 notification
    let unproc = ctx
        .dispatch_rpc(&mut sess, "notificationMgr", None, "GetUnprocessed", &[], &kw, None)
        .await
        .expect("GetUnprocessed");
    let unproc_rows = unproc.crowset_rows().expect("CRowset");
    assert_eq!(unproc_rows.len(), 1);
    assert_eq!(
        unproc_rows[0].dbrow_get("notificationID"),
        Some(&PyRep::Integer(n3.notification_id))
    );

    // 5. MarkAllAsProcessed (`rpc:notificationMgr.MarkAllAsProcessed`)
    let all_proc = ctx
        .dispatch_rpc(&mut sess, "notificationMgr", None, "MarkAllAsProcessed", &[], &kw, None)
        .await
        .expect("MarkAllAsProcessed");
    assert_eq!(all_proc, PyRep::Integer(1), "Remaining notification marked processed");

    // Verify 0 unprocessed remain
    let unproc2 = ctx
        .dispatch_rpc(&mut sess, "notificationMgr", None, "GetUnprocessed", &[], &kw, None)
        .await
        .expect("GetUnprocessed after MarkAllAsProcessed");
    assert_eq!(unproc2.crowset_rows().unwrap().len(), 0);

    // 6. DeleteGroupNotifications (`rpc:notificationMgr.DeleteGroupNotifications`)
    let _ = ctx.drain_notifications_for_char(90_000_001);
    let del_grp = ctx
        .dispatch_rpc(
            &mut sess,
            "notificationMgr",
            None,
            "DeleteGroupNotifications",
            &[PyRep::Integer(9)],
            &kw,
            None,
        )
        .await
        .expect("DeleteGroupNotifications");
    assert_eq!(del_grp, PyRep::Integer(2), "2 group 9 notifications deleted");

    // Verify notify:OnNotificationDeleted emission
    let del_drained = ctx.drain_notifications_for_char(90_000_001);
    assert!(
        del_drained.iter().any(|n| n.event == "OnNotificationDeleted"),
        "OnNotificationDeleted emitted on DeleteGroupNotifications"
    );

    // Verify side-effect: GetAllNotifications only returns group 5 notification
    let all_after_grp_del = ctx
        .dispatch_rpc(&mut sess, "notificationMgr", None, "GetAllNotifications", &[], &kw, None)
        .await
        .expect("GetAllNotifications");
    assert_eq!(all_after_grp_del.crowset_rows().unwrap().len(), 1);

    // 7. DeleteAllNotifications (`rpc:notificationMgr.DeleteAllNotifications`)
    let del_all = ctx
        .dispatch_rpc(&mut sess, "notificationMgr", None, "DeleteAllNotifications", &[], &kw, None)
        .await
        .expect("DeleteAllNotifications");
    assert_eq!(del_all, PyRep::Integer(1), "Last notification deleted");

    let del_all_drained = ctx.drain_notifications_for_char(90_000_001);
    assert!(
        del_all_drained.iter().any(|n| n.event == "OnNotificationDeleted"),
        "OnNotificationDeleted emitted on DeleteAllNotifications"
    );

    // Verify 0 notifications remain
    let all_final = ctx
        .dispatch_rpc(&mut sess, "notificationMgr", None, "GetAllNotifications", &[], &kw, None)
        .await
        .expect("GetAllNotifications empty");
    assert_eq!(all_final.crowset_rows().unwrap().len(), 0);
}

#[tokio::test]
async fn test_online_status_service_queries() {
    // parity: rpc:onlineStatus.GetInitialState, rpc:onlineStatus.GetOnlineStatus
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(921);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess1");

    let mut sess2 = SessionState::new(922);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID sess2");

    // 1. GetOnlineStatus (`rpc:onlineStatus.GetOnlineStatus`)
    // Pilot 2 is online
    let p2_status = ctx
        .dispatch_rpc(
            &mut sess1,
            "onlineStatus",
            None,
            "GetOnlineStatus",
            &[PyRep::Integer(90_000_002)],
            &kw,
            None,
        )
        .await
        .expect("GetOnlineStatus for online pilot");
    assert_eq!(p2_status, PyRep::Bool(true));

    // Offline pilot
    let offline_status = ctx
        .dispatch_rpc(
            &mut sess1,
            "onlineStatus",
            None,
            "GetOnlineStatus",
            &[PyRep::Integer(99_999_999)],
            &kw,
            None,
        )
        .await
        .expect("GetOnlineStatus for offline pilot");
    assert_eq!(offline_status, PyRep::Bool(false));

    // 2. GetInitialState (`rpc:onlineStatus.GetInitialState`)
    // Pilot 1 adds Pilot 2 as a watched contact
    ctx.esi_state
        .char_contacts
        .write()
        .unwrap()
        .entry(90_000_001)
        .or_default()
        .push(eve_server::esi::runtime_state::EsiContactEntry {
            contact_id: 90_000_002,
            contact_type: "character".into(),
            standing: 5.0,
            is_watched: true,
            is_blocked: false,
            label_ids: vec![],
        });

    let init_state = ctx
        .dispatch_rpc(&mut sess1, "onlineStatus", None, "GetInitialState", &[], &kw, None)
        .await
        .expect("GetInitialState");
    assert!(is_rowset(&init_state));
    let PyRep::Rowset { rows, columns, .. } = init_state else { panic!() };
    assert_eq!(columns, vec!["contactID", "online"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], PyRep::Integer(90_000_002));
    assert_eq!(rows[0][1], PyRep::Bool(true));
}
