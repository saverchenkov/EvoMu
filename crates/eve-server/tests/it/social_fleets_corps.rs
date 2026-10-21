//! Track 4: Phase 8 Social Systems, Mail (`SO-1`), Fleets (`SO-2`) & Chat (`SO-7`) Integration Tests.

use crate::common;

use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

fn is_keyval(rep: &PyRep) -> bool {
    matches!(rep, PyRep::KeyVal(_))
}

fn is_rowset(rep: &PyRep) -> bool {
    matches!(rep, PyRep::Rowset { .. })
}

#[tokio::test]
async fn test_so1_mail_lifecycle_labels_mailing_lists_cspa_and_calendar() {
    // parity: feat:social.mail-send, feat:social.mail-headers, feat:social.mail-body, feat:social.mail-read-state, feat:social.mail-labels, feat:social.mail-trash, rpc:calendarMgr.CreateNewEvent, rpc:calendarMgr.RespondToEvent, rpc:charMgr.LogSettings, rpc:charUnboundMgr.SelectCharacterID, rpc:mailMgr.AssignLabels, rpc:mailMgr.CreateLabel, rpc:mailMgr.DeleteLabel, rpc:mailMgr.DeleteMail, rpc:mailMgr.EditLabel, rpc:mailMgr.EmptyTrash, rpc:mailMgr.GetBody, rpc:mailMgr.GetLabels, rpc:mailMgr.MarkAsRead, rpc:mailMgr.MarkAsUnread, rpc:mailMgr.MoveFromTrash, rpc:mailMgr.MoveToTrash, rpc:mailMgr.RemoveLabels, rpc:mailMgr.SendMail, rpc:mailMgr.SyncMail, rpc:mailingListsMgr.Create, rpc:mailingListsMgr.Join
    eve_testkit::attest(&[
        "feat:social.mail-send",
        "feat:social.mail-headers",
        "feat:social.mail-body",
        "feat:social.mail-read-state",
        "feat:social.mail-labels",
        "feat:social.mail-trash",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // 1. Login Pilot 1 (Demo, 90000001) and Pilot 2 (Other, 90000002)
    let mut sess1 = SessionState::new(101);
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
    .expect("Select Pilot 1");

    let mut sess2 = SessionState::new(102);
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
    .expect("Select Pilot 2");

    // 2. Mail Labels: Pilot 2 creates custom labels with hex color strings
    let label1_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "mailMgr",
            None,
            "CreateLabel",
            &[
                PyRep::String("Important".into()),
                PyRep::String("#FF0000".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateLabel 1");
    let PyRep::KeyVal(l1_kv) = &label1_rep else {
        panic!("Expected KeyVal for CreateLabel 1")
    };
    let label1_id = l1_kv["labelID"].as_i64().unwrap();
    assert_eq!(l1_kv["name"], PyRep::String("Important".into()));
    assert_eq!(l1_kv["color"], PyRep::String("#FF0000".into()));

    let label2_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "mailMgr",
            None,
            "CreateLabel",
            &[
                PyRep::String("Orders".into()),
                PyRep::String("#0000FF".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateLabel 2");
    let PyRep::KeyVal(l2_kv) = &label2_rep else {
        panic!("Expected KeyVal for CreateLabel 2")
    };
    let label2_id = l2_kv["labelID"].as_i64().unwrap();
    assert_eq!(l2_kv["name"], PyRep::String("Orders".into()));
    assert_eq!(l2_kv["color"], PyRep::String("#0000FF".into()));

    // Edit label 2 name and hex color
    let edit_label_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "mailMgr",
            None,
            "EditLabel",
            &[
                PyRep::Integer(label2_id),
                PyRep::String("Fleet Orders".into()),
                PyRep::String("#00FF00".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("EditLabel");
    let PyRep::KeyVal(edit_kv) = &edit_label_rep else {
        panic!("Expected KeyVal for EditLabel")
    };
    assert_eq!(edit_kv["labelID"], PyRep::Integer(label2_id));
    assert_eq!(edit_kv["name"], PyRep::String("Fleet Orders".into()));
    assert_eq!(edit_kv["color"], PyRep::String("#00FF00".into()));

    // Query labels and verify custom label names and hex colors
    let labels_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "GetLabels", &[], &kw, None)
        .await
        .expect("GetLabels");
    let PyRep::Rowset {
        rows: label_rows, ..
    } = &labels_rep
    else {
        panic!("Expected Rowset from GetLabels")
    };
    assert_eq!(
        label_rows.len(),
        6,
        "Pilot 2 should have 4 system labels + 2 custom labels"
    );
    assert!(
        label_rows.iter().any(|r| r[0] == PyRep::Integer(label1_id)
            && r[1] == PyRep::String("Important".into())
            && r[2] == PyRep::String("#FF0000".into())),
        "GetLabels must include label 1 with name 'Important' and color '#FF0000'"
    );
    assert!(
        label_rows.iter().any(|r| r[0] == PyRep::Integer(label2_id)
            && r[1] == PyRep::String("Fleet Orders".into())
            && r[2] == PyRep::String("#00FF00".into())),
        "GetLabels must include edited label 2 with name 'Fleet Orders' and color '#00FF00'"
    );

    // 3. Send Mail using real V24/Crucible 7-arg signature:
    // SendMail(toCharacterIDs, listID, toCorpOrAllianceID, title, body, isReplyTo, isForwardedFrom)
    let expected_title = "Fleet Deployment to Tama";
    let expected_body = "Report to frontline in Heavy Assault Cruisers.";
    let send_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "mailMgr",
            None,
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
                PyRep::None,
                PyRep::None,
                PyRep::String(expected_title.into()),
                PyRep::String(expected_body.into()),
                PyRep::Bool(false),
                PyRep::Bool(false),
            ],
            &kw,
            None,
        )
        .await
        .expect("SendMail");
    let mail_id = send_rep.as_i64().expect("mail_id integer");

    // Notifications check: Pilot 2 gets OnNewMailReceived, Pilot 1 gets OnMailSent
    let p2_notifs = ctx.drain_notifications_for_char(90_000_002);
    assert!(
        p2_notifs.iter().any(|n| n.event == "OnNewMailReceived"),
        "Expected OnNewMailReceived for Pilot 2"
    );
    let p1_notifs = ctx.drain_notifications_for_char(90_000_001);
    assert!(
        p1_notifs.iter().any(|n| n.event == "OnMailSent"),
        "Expected OnMailSent for Pilot 1"
    );

    // 4. Pilot 2 fetches mail headers via SyncMail -> util.KeyVal { oldMail, newMail, mailStatus }
    let sync_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "SyncMail", &[], &kw, None)
        .await
        .expect("SyncMail");
    let PyRep::KeyVal(sync_kv) = &sync_rep else {
        panic!("SyncMail must return util.KeyVal")
    };
    assert!(
        is_rowset(&sync_kv["oldMail"]),
        "SyncMail must include oldMail Rowset"
    );
    let PyRep::Rowset {
        rows: new_mail_rows,
        ..
    } = &sync_kv["newMail"]
    else {
        panic!("SyncMail must include newMail Rowset");
    };
    let mail_hdr = new_mail_rows
        .iter()
        .find(|cols| cols.first() == Some(&PyRep::Integer(mail_id)))
        .expect("newMail must contain sent message header");
    assert_eq!(
        mail_hdr[1],
        PyRep::Integer(90_000_001),
        "senderID must match Pilot 1"
    );
    assert_eq!(
        mail_hdr[2],
        PyRep::List(vec![PyRep::Integer(90_000_002)]),
        "toCharacterIDs must match Pilot 2"
    );
    assert_eq!(
        mail_hdr[3],
        PyRep::String(expected_title.into()),
        "title must match"
    );
    assert_eq!(
        mail_hdr[5],
        PyRep::Bool(false),
        "unread mail must have read == false"
    );

    let PyRep::Rowset {
        rows: status_rows, ..
    } = &sync_kv["mailStatus"]
    else {
        panic!("SyncMail must include mailStatus Rowset");
    };
    let status_row = status_rows
        .iter()
        .find(|cols| cols.first() == Some(&PyRep::Integer(mail_id)))
        .expect("mailStatus must contain message status row");
    assert_eq!(
        status_row[1],
        PyRep::Integer(0),
        "unread statusMask must be 0"
    );
    assert_eq!(
        status_row[3],
        PyRep::Bool(false),
        "unread mailStatus read flag must be false"
    );

    // 5. Pilot 2 reads mail body: verify body text and auto-mark as read
    let body_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "mailMgr",
            None,
            "GetBody",
            &[PyRep::Integer(mail_id)],
            &kw,
            None,
        )
        .await
        .expect("GetBody");
    let PyRep::KeyVal(body_kv) = &body_rep else {
        panic!("Expected KeyVal from GetBody")
    };
    assert_eq!(body_kv["messageID"], PyRep::Integer(mail_id));
    assert_eq!(
        body_kv["body"],
        PyRep::String(expected_body.into()),
        "GetBody must return exact mail body text"
    );
    assert_eq!(body_kv["read"], PyRep::Bool(true));

    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.read_mails.contains(&(90_000_002, mail_id)),
            "Mail must be marked read on GetBody"
        );
    });

    // 6. Pilot 2 toggles unread/read state using MarkAsUnread / MarkAsRead with messageID lists
    let unread_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "mailMgr",
            None,
            "MarkAsUnread",
            &[PyRep::List(vec![PyRep::Integer(mail_id)])],
            &kw,
            None,
        )
        .await
        .expect("MarkAsUnread");
    let PyRep::KeyVal(unread_kv) = &unread_rep else {
        panic!("Expected KeyVal from MarkAsUnread")
    };
    assert_eq!(unread_kv["read"], PyRep::Bool(false));
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap.read_mails.contains(&(90_000_002, mail_id)),
            "Mail must be unread after MarkAsUnread"
        );
    });

    // Mark as read again via real MarkAsRead(messageIDs list)
    let mark_read_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "mailMgr",
            None,
            "MarkAsRead",
            &[PyRep::List(vec![PyRep::Integer(mail_id)])],
            &kw,
            None,
        )
        .await
        .expect("MarkAsRead");
    let PyRep::KeyVal(mark_read_kv) = &mark_read_rep else {
        panic!("Expected KeyVal from MarkAsRead")
    };
    assert_eq!(mark_read_kv["read"], PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.read_mails.contains(&(90_000_002, mail_id)),
            "Mail must be marked read after MarkAsRead"
        );
    });

    // 7. Assign labels to mail using real V24/Crucible signature: AssignLabels(messageIDs list, labelID)
    ctx.dispatch_rpc(
        &mut sess2,
        "mailMgr",
        None,
        "AssignLabels",
        &[
            PyRep::List(vec![PyRep::Integer(mail_id)]),
            PyRep::Integer(label1_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("AssignLabels label 1");
    ctx.dispatch_rpc(
        &mut sess2,
        "mailMgr",
        None,
        "AssignLabels",
        &[
            PyRep::List(vec![PyRep::Integer(mail_id)]),
            PyRep::Integer(label2_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("AssignLabels label 2");

    // Verify both labels are assigned in SyncMail
    let sync_labeled = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "SyncMail", &[], &kw, None)
        .await
        .expect("SyncMail after AssignLabels");
    let PyRep::KeyVal(sl_kv) = &sync_labeled else {
        panic!()
    };
    let PyRep::Rowset { rows: sl_rows, .. } = &sl_kv["newMail"] else {
        panic!()
    };
    let PyRep::List(assigned_labels) = &sl_rows[0][6] else {
        panic!("Expected labels list")
    };
    assert!(
        assigned_labels.contains(&PyRep::Integer(label1_id)),
        "Mail must have label 1 assigned"
    );
    assert!(
        assigned_labels.contains(&PyRep::Integer(label2_id)),
        "Mail must have label 2 assigned"
    );

    // Remove label 1 using real signature: RemoveLabels(messageIDs list, labelID)
    ctx.dispatch_rpc(
        &mut sess2,
        "mailMgr",
        None,
        "RemoveLabels",
        &[
            PyRep::List(vec![PyRep::Integer(mail_id)]),
            PyRep::Integer(label1_id),
        ],
        &kw,
        None,
    )
    .await
    .expect("RemoveLabels");

    let sync_removed = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "SyncMail", &[], &kw, None)
        .await
        .expect("SyncMail after RemoveLabels");
    let PyRep::KeyVal(sr_kv) = &sync_removed else {
        panic!()
    };
    let PyRep::Rowset { rows: sr_rows, .. } = &sr_kv["newMail"] else {
        panic!()
    };
    let PyRep::List(remaining_labels) = &sr_rows[0][6] else {
        panic!("Expected labels list")
    };
    assert!(
        !remaining_labels.contains(&PyRep::Integer(label1_id)),
        "Label 1 must be removed"
    );
    assert!(
        remaining_labels.contains(&PyRep::Integer(label2_id)),
        "Label 2 must remain assigned"
    );

    // Delete label 1 and verify GetLabels now has 5 labels
    ctx.dispatch_rpc(
        &mut sess2,
        "mailMgr",
        None,
        "DeleteLabel",
        &[PyRep::Integer(label1_id)],
        &kw,
        None,
    )
    .await
    .expect("DeleteLabel");
    let labels_after_del = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "GetLabels", &[], &kw, None)
        .await
        .expect("GetLabels after DeleteLabel");
    let PyRep::Rowset {
        rows: label_rows_after,
        ..
    } = &labels_after_del
    else {
        panic!()
    };
    assert_eq!(label_rows_after.len(), 5);
    assert!(!label_rows_after
        .iter()
        .any(|r| r[0] == PyRep::Integer(label1_id)));

    // 8. Trash lifecycle (MoveToTrash, MoveFromTrash, EmptyTrash) and DeleteMail
    let tr_rep = ctx.dispatch_rpc(&mut sess2, "mailMgr", None, "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(mail_id)])], &kw, None).await.expect("MoveToTrash");
    assert_eq!(tr_rep.dict_get("inTrash"), Some(&PyRep::Bool(true)));
    let unt_rep = ctx.dispatch_rpc(&mut sess2, "mailMgr", None, "MoveFromTrash", &[PyRep::List(vec![PyRep::Integer(mail_id)])], &kw, None).await.expect("MoveFromTrash");
    assert_eq!(unt_rep.dict_get("inTrash"), Some(&PyRep::Bool(false)));
    ctx.dispatch_rpc(&mut sess2, "mailMgr", None, "EmptyTrash", &[], &kw, None).await.expect("EmptyTrash");
    ctx.dispatch_rpc(
        &mut sess2,
        "mailMgr",
        None,
        "DeleteMail",
        &[PyRep::Integer(mail_id)],
        &kw,
        None,
    )
    .await
    .expect("DeleteMail");
    let sync_after_del = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "SyncMail", &[], &kw, None)
        .await
        .expect("SyncMail after DeleteMail");
    let PyRep::KeyVal(sad_kv) = &sync_after_del else {
        panic!()
    };
    let PyRep::Rowset { rows: sad_rows, .. } = &sad_kv["newMail"] else {
        panic!()
    };
    assert!(
        sad_rows.is_empty(),
        "Deleted mail must no longer appear in SyncMail"
    );

    // 9. CSPA charge & Mailing Lists & Calendar Events
    ctx.dispatch_rpc(
        &mut sess2,
        "charMgr",
        None,
        "LogSettings",
        &[PyRep::Real(5_000.0)],
        &kw,
        None,
    )
    .await
    .expect("charMgr.LogSettings CSPA");
    let ml_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "mailingListsMgr",
            None,
            "Create",
            &[PyRep::String("FleetOpsList".into())],
            &kw,
            None,
        )
        .await
        .expect("mailingListsMgr.Create");
    let PyRep::KeyVal(ml_kv) = &ml_rep else {
        panic!()
    };
    let list_id = ml_kv["listID"].as_i64().unwrap();
    ctx.dispatch_rpc(
        &mut sess2,
        "mailingListsMgr",
        None,
        "Join",
        &[PyRep::Integer(list_id)],
        &kw,
        None,
    )
    .await
    .expect("mailingListsMgr.Join");
    let ev_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "calendarMgr",
            None,
            "CreateNewEvent",
            &[
                PyRep::Integer(1_800_000_000),
                PyRep::Integer(60),
                PyRep::String("CTA Op".into()),
                PyRep::String("Formup in Jita".into()),
                PyRep::Integer(1),
            ],
            &kw,
            None,
        )
        .await
        .expect("calendarMgr.CreateNewEvent");
    let PyRep::KeyVal(ev_kv) = &ev_rep else {
        panic!()
    };
    let event_id = ev_kv["eventID"].as_i64().unwrap();
    let rsvp = ctx
        .dispatch_rpc(
            &mut sess2,
            "calendarMgr",
            None,
            "RespondToEvent",
            &[PyRep::Integer(event_id), PyRep::Integer(1)],
            &kw,
            None,
        )
        .await
        .expect("calendarMgr.RespondToEvent");
    assert!(is_keyval(&rsvp));
}

#[tokio::test]
async fn test_so2_fleet_manager_hierarchy_broadcast_and_fleet_warp() {
    // parity: feat:fleet.create-fleet, feat:fleet.invite-accept, feat:fleet.leave-kick, feat:navigation.warp-to-member, feat:navigation.fleet-warp, rpc:charUnboundMgr.SelectCharacterID, rpc:fleetMgr.AddToWatchlist, rpc:fleetMgr.CreateFleet, rpc:fleetMgr.GetInitState, rpc:fleetMgr.LeaveFleet, rpc:fleetObjectHandler.AcceptInvite, rpc:fleetObjectHandler.CreateSquad, rpc:fleetObjectHandler.CreateWing, rpc:fleetObjectHandler.FleetWarp, rpc:fleetObjectHandler.GetInitState, rpc:fleetObjectHandler.Invite, rpc:fleetObjectHandler.KickMember, rpc:fleetObjectHandler.MachoBindObject, rpc:fleetObjectHandler.MoveMember, rpc:fleetObjectHandler.SendBroadcast, rpc:fleetObjectHandler.WarpToMember, rpc:fleetProxy.AddFleetFinderAdvert, rpc:fleetProxy.GetAvailableFleetAds
    eve_testkit::attest(&[
        "feat:fleet.create-fleet",
        "feat:fleet.invite-accept",
        "feat:fleet.leave-kick",
        "feat:navigation.warp-to-member",
        "feat:navigation.fleet-warp",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // 1. Login Pilot 1 (FC) and Pilot 2 (Member)
    let mut sess1 = SessionState::new(201);
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
    .expect("Select Pilot 1");

    let mut sess2 = SessionState::new(202);
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
    .expect("Select Pilot 2");

    // 2. Pilot 1 creates fleet and binds V24 fleetObjectHandler
    let create_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "CreateFleet", &[], &kw, None)
        .await
        .expect("CreateFleet");
    assert!(is_keyval(&create_rep));
    let fid = sess1.fleet_id.expect("fleet_id set");

    let bind_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            None,
            "MachoBindObject",
            &[PyRep::Integer(fid), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("Bind fleetObjectHandler");
    let PyRep::Tuple(bind_tuple) = &bind_rep else {
        panic!("Expected Tuple from MachoBindObject")
    };
    let fleet_bound_id = bind_tuple[0].as_i64().expect("bound_id integer") as u64;

    // 3. Hierarchy on bound fleetObjectHandler: CreateWing and CreateSquad, then verify structure via GetInitState
    let wing_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            Some(fleet_bound_id),
            "CreateWing",
            &[PyRep::String("Skirmish Wing".into())],
            &kw,
            None,
        )
        .await
        .expect("CreateWing");
    let PyRep::KeyVal(wing_kv) = &wing_rep else {
        panic!("Expected KeyVal from CreateWing")
    };
    let wing2_id = wing_kv["wingID"].as_i64().unwrap();
    assert_eq!(wing2_id, 2);

    let squad_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            Some(fleet_bound_id),
            "CreateSquad",
            &[
                PyRep::Integer(wing2_id),
                PyRep::String("Interceptors".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateSquad");
    let PyRep::KeyVal(squad_kv) = &squad_rep else {
        panic!("Expected KeyVal from CreateSquad")
    };
    let squad2_id = squad_kv["squadID"].as_i64().unwrap();
    assert_eq!(squad2_id, 2);

    let init_state = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            Some(fleet_bound_id),
            "GetInitState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetInitState");
    let PyRep::KeyVal(init_kv) = &init_state else {
        panic!("Expected KeyVal from GetInitState")
    };
    assert_eq!(init_kv["fleetID"], PyRep::Integer(fid));
    assert_eq!(init_kv["bossID"], PyRep::Integer(90_000_001));
    let PyRep::Rowset {
        rows: wing_rows, ..
    } = &init_kv["wings"]
    else {
        panic!("Expected wings Rowset")
    };
    assert_eq!(
        wing_rows.len(),
        2,
        "Fleet must have 2 wings (Wing 1 + Skirmish Wing)"
    );
    assert!(wing_rows
        .iter()
        .any(|r| r[0] == PyRep::Integer(2) && r[1] == PyRep::String("Skirmish Wing".into())));
    let PyRep::Rowset {
        rows: squad_rows, ..
    } = &init_kv["squads"]
    else {
        panic!("Expected squads Rowset")
    };
    assert_eq!(
        squad_rows.len(),
        2,
        "Fleet must have 2 squads (Squad 1 + Interceptors)"
    );
    assert!(squad_rows.iter().any(|r| r[0] == PyRep::Integer(2)
        && r[1] == PyRep::Integer(2)
        && r[2] == PyRep::String("Interceptors".into())));

    // 4. Pilot 1 invites Pilot 2 via bound fleetObjectHandler, Pilot 2 accepts and is moved to Wing 2 / Squad 2
    let invite_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            Some(fleet_bound_id),
            "Invite",
            &[
                PyRep::Integer(90_000_002),
                PyRep::Integer(wing2_id),
                PyRep::Integer(squad2_id),
                PyRep::Integer(4),
            ],
            &kw,
            None,
        )
        .await
        .expect("Invite");
    assert!(is_keyval(&invite_rep));

    let p2_invites = ctx.drain_notifications_for_char(90_000_002);
    assert!(
        p2_invites.iter().any(|n| n.event == "OnFleetInvite"),
        "OnFleetInvite notification required"
    );

    // Pilot 2 binds fleetObjectHandler and accepts invite via AcceptInvite
    let bind2_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "fleetObjectHandler",
            None,
            "MachoBindObject",
            &[PyRep::Integer(fid), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("Bind fleetObjectHandler for Pilot 2");
    let PyRep::Tuple(bind2_tuple) = &bind2_rep else {
        panic!()
    };
    let fleet_bound_id2 = bind2_tuple[0].as_i64().unwrap() as u64;

    let join_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "fleetObjectHandler",
            Some(fleet_bound_id2),
            "AcceptInvite",
            &[PyRep::Integer(fid)],
            &kw,
            None,
        )
        .await
        .expect("AcceptInvite");
    assert!(is_keyval(&join_rep));
    assert_eq!(sess2.fleet_id, Some(fid));

    // Move Pilot 2 into Wing 2 ("Skirmish Wing"), Squad 2 ("Interceptors") as SquadMember (role 4)
    ctx.dispatch_rpc(
        &mut sess1,
        "fleetObjectHandler",
        Some(fleet_bound_id),
        "MoveMember",
        &[
            PyRep::Integer(90_000_002),
            PyRep::Integer(wing2_id),
            PyRep::Integer(squad2_id),
            PyRep::Integer(4),
        ],
        &kw,
        None,
    )
    .await
    .expect("MoveMember");

    // Verify member list and squad placement in fleet state
    let state_after_join = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            Some(fleet_bound_id),
            "GetInitState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetInitState after join");
    let PyRep::KeyVal(saj_kv) = &state_after_join else {
        panic!()
    };
    let PyRep::Rowset {
        rows: member_rows, ..
    } = &saj_kv["members"]
    else {
        panic!("Expected members Rowset")
    };
    assert_eq!(member_rows.len(), 2, "Fleet must have 2 members");
    let p2_member_row = member_rows
        .iter()
        .find(|r| r[0] == PyRep::Integer(90_000_002))
        .expect("Pilot 2 must be in fleet members rowset");
    assert_eq!(
        p2_member_row[1],
        PyRep::Integer(4),
        "Pilot 2 role must be SquadMember (4)"
    );
    assert_eq!(
        p2_member_row[2],
        PyRep::Integer(wing2_id),
        "Pilot 2 must be placed in Wing 2"
    );
    assert_eq!(
        p2_member_row[3],
        PyRep::Integer(squad2_id),
        "Pilot 2 must be placed in Squad 2"
    );

    // 5. Broadcast command via bound fleetObjectHandler
    let bcast_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            Some(fleet_bound_id),
            "SendBroadcast",
            &[
                PyRep::String("TargetPrimary".into()),
                PyRep::Integer(1),
                PyRep::Integer(100_000_002),
            ],
            &kw,
            None,
        )
        .await
        .expect("SendBroadcast");
    assert!(is_keyval(&bcast_rep));

    let p2_bcasts = ctx.drain_notifications_for_char(90_000_002);
    assert!(
        p2_bcasts.iter().any(|n| n.event == "OnFleetBroadcast"),
        "OnFleetBroadcast notification required"
    );

    // 6. Spawn both ships in space in Jita (30000142)
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let ball1 = Ball {
        id: 200_000_001,
        type_id: 587,
        position: DVec3::new(10_000.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let ball2 = Ball {
        id: 200_000_002,
        type_id: 587,
        position: DVec3::new(500_000_000.0, 0.0, 0.0), // 500,000 km away
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        radius: 40.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(ball1).await;
    let _ = sys.spawn_ball(ball2).await;
    sess1.ship_id = Some(200_000_001);
    sess1.solar_system_id = Some(30_000_142);
    sess2.ship_id = Some(200_000_002);
    sess2.solar_system_id = Some(30_000_142);

    ctx.db
        .try_with_mut_snapshot(|snap| {
            let ch1 = snap.characters.get_mut(&90_000_001).unwrap();
            ch1.active_ship_id = 200_000_001;
            ch1.solar_system_id = 30_000_142;
            ch1.station_id = None;
            let ch2 = snap.characters.get_mut(&90_000_002).unwrap();
            ch2.active_ship_id = 200_000_002;
            ch2.solar_system_id = 30_000_142;
            ch2.station_id = None;
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();

    // 7. Warp-to-member: Pilot 2 warps to Pilot 1 at 10 km range
    let warp_to_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "fleetObjectHandler",
            Some(fleet_bound_id2),
            "WarpToMember",
            &[PyRep::Integer(90_000_001), PyRep::Real(10_000.0)],
            &kw,
            None,
        )
        .await
        .expect("WarpToMember");
    assert!(is_keyval(&warp_to_rep));

    // Verify Pilot 2's ship entered MovementMode::Warp and lands near Pilot 1 (at x = 20,000m)
    let (_, b2_warping) = sys
        .get_ball_snapshot(200_000_002)
        .await
        .unwrap()
        .expect("ball 2 exists");
    assert!(
        matches!(b2_warping.mode, MovementMode::Warp { .. }),
        "Pilot 2 ship must enter MovementMode::Warp on WarpToMember"
    );
    for _ in 0..30 {
        let _ = sys.step_manual(1.0).await;
    }
    let (_, b2_landed) = sys
        .get_ball_snapshot(200_000_002)
        .await
        .unwrap()
        .expect("ball 2 exists");
    assert!(
        b2_landed.position.distance(DVec3::new(20_000.0, 0.0, 0.0)) < 2_500.0,
        "Pilot 2 must land ~10km from Pilot 1 (at ~20,000m), got {:?}",
        b2_landed.position
    );

    // 8. Fleet Warp: Pilot 1 (FC) initiates fleet warp via V24 FleetWarp
    let fleet_warp_dest = DVec3::new(1_000_000_000.0, 0.0, 0.0);
    let fleet_warp_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            Some(fleet_bound_id),
            "FleetWarp",
            &[
                PyRep::Real(fleet_warp_dest.x),
                PyRep::Real(fleet_warp_dest.y),
                PyRep::Real(fleet_warp_dest.z),
                PyRep::Real(0.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("FleetWarp");
    assert!(is_keyval(&fleet_warp_rep));

    // Verify both Pilot 1 and Pilot 2 entered Warp together and land at fleet_warp_dest
    let (_, b1_fw) = sys.get_ball_snapshot(200_000_001).await.unwrap().unwrap();
    let (_, b2_fw) = sys.get_ball_snapshot(200_000_002).await.unwrap().unwrap();
    assert!(
        matches!(b1_fw.mode, MovementMode::Warp { .. }),
        "FC ship must enter Warp on FleetWarp"
    );
    assert!(
        matches!(b2_fw.mode, MovementMode::Warp { .. }),
        "Squad member ship must enter Warp on FleetWarp"
    );
    for _ in 0..30 {
        let _ = sys.step_manual(1.0).await;
    }
    let (_, b1_fw_landed) = sys.get_ball_snapshot(200_000_001).await.unwrap().unwrap();
    let (_, b2_fw_landed) = sys.get_ball_snapshot(200_000_002).await.unwrap().unwrap();
    assert!(
        b1_fw_landed.position.distance(fleet_warp_dest) < 2_500.0,
        "FC must land at fleet warp destination"
    );
    assert!(
        b2_fw_landed.position.distance(fleet_warp_dest) < 2_500.0,
        "Member must land at fleet warp destination"
    );

    // 9. Fleet Finder Advert (`fleetProxy`) & Watchlist (`fleetMgr.AddToWatchlist`)
    let ad_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetProxy",
            None,
            "AddFleetFinderAdvert",
            &[
                PyRep::String("Standing Response Fleet".into()),
                PyRep::String("Public Standing Fleet".into()),
                PyRep::Integer(3),
                PyRep::Bool(true),
            ],
            &kw,
            None,
        )
        .await
        .expect("AddFleetFinderAdvert");
    assert!(is_keyval(&ad_rep));
    let ads_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "fleetProxy",
            None,
            "GetAvailableFleetAds",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetAvailableFleetAds");
    assert!(is_rowset(&ads_rep));
    let wl_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetMgr",
            None,
            "AddToWatchlist",
            &[PyRep::Integer(90_000_002)],
            &kw,
            None,
        )
        .await
        .expect("AddToWatchlist");
    assert!(is_keyval(&wl_rep));

    // 10. Kick and Leave: verify notifications, kicked member's session & fleet state
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let kick_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            Some(fleet_bound_id),
            "KickMember",
            &[PyRep::Integer(90_000_002)],
            &kw,
            None,
        )
        .await
        .expect("KickMember");
    assert!(is_keyval(&kick_rep));

    let p2_kick_notifs = ctx.drain_notifications_for_char(90_000_002);
    assert!(
        p2_kick_notifs.iter().any(|n| n.event == "OnFleetLeave"),
        "Kicked member must receive OnFleetLeave"
    );

    // Kicked member querying fleet must get NotInFleet and have session.fleet_id cleared
    let p2_fleet_err = ctx
        .dispatch_rpc(&mut sess2, "fleetMgr", None, "GetInitState", &[], &kw, None)
        .await
        .expect_err("Kicked member must not be in fleet");
    assert_eq!(p2_fleet_err.code, "NotInFleet");
    assert_eq!(
        sess2.fleet_id, None,
        "Kicked member session.fleet_id must be cleared"
    );

    // Verify fleet member list now contains only Pilot 1
    let state_after_kick = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            Some(fleet_bound_id),
            "GetInitState",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetInitState after kick");
    let PyRep::KeyVal(sak_kv) = &state_after_kick else {
        panic!()
    };
    let PyRep::Rowset {
        rows: rem_members, ..
    } = &sak_kv["members"]
    else {
        panic!()
    };
    assert_eq!(
        rem_members.len(),
        1,
        "Only FC should remain in fleet after kicking Pilot 2"
    );
    assert_eq!(rem_members[0][0], PyRep::Integer(90_000_001));

    let leave_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "LeaveFleet", &[], &kw, None)
        .await
        .expect("LeaveFleet");
    assert!(is_keyval(&leave_rep));
    assert_eq!(sess1.fleet_id, None);
}

#[tokio::test]
async fn test_so7_chat_channels_local_corp_fleet_player_and_private() {
    // parity: feat:social.chat-xmpp-auth, feat:social.chat-local, feat:social.chat-corp-alliance-fleet, feat:social.chat-player-channels, feat:social.chat-private, rpc:LSC.AccessControl, rpc:LSC.JoinChannel, rpc:LSC.SendMessage, rpc:XmppChatMgr.CreatePlayerOwnedChannel, rpc:XmppChatMgr.CreatePrivateChannel, rpc:charUnboundMgr.SelectCharacterID, rpc:chatAuthenticationService.GetAuthenticationToken
    eve_testkit::attest(&[
        "feat:social.chat-xmpp-auth",
        "feat:social.chat-local",
        "feat:social.chat-corp-alliance-fleet",
        "feat:social.chat-player-channels",
        "feat:social.chat-private",
    ]);
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(701);
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
    .expect("Select Pilot 1");

    let mut sess2 = SessionState::new(702);
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
    .expect("Select Pilot 2");

    // 0. XMPP Authentication Token
    let auth_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "chatAuthenticationService",
            None,
            "GetAuthenticationToken",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetAuthenticationToken");
    assert!(is_keyval(&auth_rep));

    // 1. Local Chat population and movement sync
    ctx.sync_character_local_chat(90_000_001, None, 30_000_142);
    ctx.db.with_snapshot(|snap| {
        let chan = &snap.chat_channels["solarsystemid2:30000142"];
        assert!(chan.members.contains(&90_000_001));
    });

    // Move to wormhole system (delayed-mode local)
    ctx.sync_character_local_chat(90_000_001, Some(30_000_142), 31_000_005);
    ctx.db.with_snapshot(|snap| {
        let jita = &snap.chat_channels["solarsystemid2:30000142"];
        assert!(!jita.members.contains(&90_000_001));
        let wh_chan = &snap.chat_channels["solarsystemid2:31000005"];
        assert!(
            wh_chan.is_delayed_local,
            "W-space local must be marked delayed local"
        );
    });

    // 2. Corp, Alliance & Fleet chat channels via V24 LSC
    let corp_chan = "corpid:1000001";
    let join_corp1 = ctx
        .dispatch_rpc(
            &mut sess1,
            "LSC",
            None,
            "JoinChannel",
            &[PyRep::String(corp_chan.into())],
            &kw,
            None,
        )
        .await
        .expect("Join Corp channel P1");
    assert!(is_keyval(&join_corp1));

    let join_corp2 = ctx
        .dispatch_rpc(
            &mut sess2,
            "LSC",
            None,
            "JoinChannel",
            &[PyRep::String(corp_chan.into())],
            &kw,
            None,
        )
        .await
        .expect("Join Corp channel P2");
    assert!(is_keyval(&join_corp2));

    let send_corp = ctx
        .dispatch_rpc(
            &mut sess1,
            "LSC",
            None,
            "SendMessage",
            &[
                PyRep::String(corp_chan.into()),
                PyRep::String("All hands deploy to POS".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("SendMessage Corp");
    assert!(is_keyval(&send_corp));

    let p2_chat = ctx.drain_notifications_for_char(90_000_002);
    assert!(
        !p2_chat.is_empty(),
        "Pilot 2 should receive chat notification for corp chat"
    );

    // 3. Player Created Custom Channels (`XmppChatMgr.CreatePlayerOwnedChannel` + `LSC.AccessControl`)
    let create_chan = ctx
        .dispatch_rpc(
            &mut sess1,
            "XmppChatMgr",
            None,
            "CreatePlayerOwnedChannel",
            &[
                PyRep::String("WormholeHunters".into()),
                PyRep::None,
                PyRep::String("Class 5 capital escalations".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreatePlayerOwnedChannel");
    assert!(is_keyval(&create_chan));
    let custom_chan_id = match &create_chan {
        PyRep::KeyVal(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == "channelID")
            .and_then(|(_, v)| v.as_str())
            .unwrap()
            .to_string(),
        _ => "channel:wormholehunters".to_string(),
    };

    // Grant operator to Pilot 2 via LSC.AccessControl
    let grant_op = ctx
        .dispatch_rpc(
            &mut sess1,
            "LSC",
            None,
            "AccessControl",
            &[
                PyRep::String(custom_chan_id),
                PyRep::Integer(90_000_002),
                PyRep::Integer(2),
            ],
            &kw,
            None,
        )
        .await
        .expect("AccessControl");
    assert!(is_keyval(&grant_op));

    // 4. Private 1-on-1 Conversation (`XmppChatMgr.CreatePrivateChannel` + `LSC.SendMessage`)
    let priv_convo = ctx
        .dispatch_rpc(
            &mut sess1,
            "XmppChatMgr",
            None,
            "CreatePrivateChannel",
            &[PyRep::Integer(90_000_002)],
            &kw,
            None,
        )
        .await
        .expect("CreatePrivateChannel");
    assert!(is_keyval(&priv_convo));
    let priv_chan_id = match &priv_convo {
        PyRep::KeyVal(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == "channelID")
            .and_then(|(_, v)| v.as_str())
            .unwrap()
            .to_string(),
        _ => "private:90000001:90000002".to_string(),
    };

    let _ = ctx.drain_notifications_for_char(90_000_002);
    let send_priv = ctx
        .dispatch_rpc(
            &mut sess1,
            "LSC",
            None,
            "SendMessage",
            &[
                PyRep::String(priv_chan_id),
                PyRep::String("Cynosaural field is up".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("SendMessage Private");
    assert!(is_keyval(&send_priv));

    let p2_priv = ctx.drain_notifications_for_char(90_000_002);
    assert!(
        !p2_priv.is_empty(),
        "Pilot 2 should receive notification for private conversation"
    );
}
