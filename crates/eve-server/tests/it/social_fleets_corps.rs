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
    // parity: feat:social.mail-send, feat:social.mail-headers, feat:social.mail-body, feat:social.mail-read-state, feat:social.mail-labels, feat:social.mail-trash, rpc:charMgr.LogSettings, rpc:charUnboundMgr.SelectCharacterID, rpc:mailMgr.AssignLabels, rpc:mailMgr.CreateLabel, rpc:mailMgr.DeleteLabel, rpc:mailMgr.DeleteMail, rpc:mailMgr.EditLabel, rpc:mailMgr.EmptyTrash, rpc:mailMgr.GetBody, rpc:mailMgr.GetLabels, rpc:mailMgr.MarkAsRead, rpc:mailMgr.MarkAsUnread, rpc:mailMgr.MoveFromTrash, rpc:mailMgr.MoveToTrash, rpc:mailMgr.RemoveLabels, rpc:mailMgr.SyncMail, rpc:mailingListsMgr.Create, rpc:mailingListsMgr.Join
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
    let label1_id = label1_rep.as_i64().expect("label1_id");
    assert!(label1_id >= 16);

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
    let label2_id = label2_rep.as_i64().expect("label2_id");
    assert!(label2_id >= 16);

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
    let PyRep::Dict(lbl_dict) = &labels_rep else {
        panic!("Expected Dict from GetLabels")
    };
    assert_eq!(lbl_dict.len(), 6, "Pilot 2 should have 4 system labels + 2 custom labels");
    let l1_entry = lbl_dict.iter().find(|(k, _)| k == &PyRep::Integer(label1_id)).map(|(_, v)| v).expect("label1 in GetLabels");
    assert_eq!(l1_entry.dict_get("name"), Some(&PyRep::String("Important".into())));
    assert_eq!(l1_entry.dict_get("color"), Some(&PyRep::String("#FF0000".into())));
    let l2_entry = lbl_dict.iter().find(|(k, _)| k == &PyRep::Integer(label2_id)).map(|(_, v)| v).expect("label2 in GetLabels");
    assert_eq!(l2_entry.dict_get("name"), Some(&PyRep::String("Fleet Orders".into())));
    assert_eq!(l2_entry.dict_get("color"), Some(&PyRep::String("#00FF00".into())));

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

    // Notifications check: Pilot 2 gets OnMailSent (its client raises OnNewMailReceived itself);
    // Pilot 1's own client ran OnMailSent locally, so the server does not echo it back.
    let p2_notifs = ctx.drain_notifications_for_char(90_000_002);
    assert!(
        p2_notifs.iter().any(|n| n.event == "OnMailSent" && n.args[0] == PyRep::Integer(mail_id)),
        "Expected OnMailSent for Pilot 2"
    );
    let p1_notifs = ctx.drain_notifications_for_char(90_000_001);
    assert!(
        !p1_notifs.iter().any(|n| n.event == "OnMailSent"),
        "Sender must not be re-notified"
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
        PyRep::String("90000002".into()),
        "toCharacterIDs must match Pilot 2"
    );
    assert_eq!(
        mail_hdr[5],
        PyRep::String(expected_title.into()),
        "title must match"
    );
    assert_eq!(
        mail_hdr[7],
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
    let PyRep::Buffer(comp_bytes) = &body_rep else {
        panic!("Expected Buffer from GetBody")
    };
    let decomp = miniz_oxide::inflate::decompress_to_vec_zlib(comp_bytes).expect("decompress zlib");
    assert_eq!(
        String::from_utf8(decomp).unwrap(),
        expected_body,
        "GetBody must return exact mail body text"
    );

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
    let PyRep::List(assigned_labels) = &sl_rows[0][8] else {
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
    let PyRep::List(remaining_labels) = &sr_rows[0][8] else {
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
    let PyRep::Dict(label_dict_after) = &labels_after_del else {
        panic!("expected Dict")
    };
    assert_eq!(label_dict_after.len(), 5);
    assert!(!label_dict_after
        .iter()
        .any(|(k, _)| k == &PyRep::Integer(label1_id)));

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
            "CreatePersonalEvent",
            &[
                PyRep::Integer(ctx.now_blueos_filetime_ticks() + 86_400 * 10_000_000),
                PyRep::Integer(60),
                PyRep::String("CTA Op".into()),
                PyRep::String("Formup in Jita".into()),
                PyRep::Integer(1),
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
            ],
            &kw,
            None,
        )
        .await
        .expect("calendarMgr.CreatePersonalEvent");
    let PyRep::KeyVal(ev_kv) = &ev_rep else {
        panic!()
    };
    let event_id = ev_kv["eventID"].as_i64().unwrap();
    let rsvp = ctx
        .dispatch_rpc(
            &mut sess2,
            "calendarMgr",
            None,
            "SendEventResponse",
            &[PyRep::Integer(event_id), PyRep::Integer(90_000_001), PyRep::Integer(4)],
            &kw,
            None,
        )
        .await
        .expect("calendarMgr.SendEventResponse");
    assert!(is_keyval(&rsvp));
}

#[tokio::test]
async fn test_so2_fleet_manager_hierarchy_broadcast_and_fleet_warp() {
    // parity: feat:fleet.create-fleet, feat:fleet.invite-accept, feat:fleet.leave-kick, feat:navigation.warp-to-member, feat:navigation.fleet-warp, rpc:charUnboundMgr.SelectCharacterID, rpc:fleetMgr.AddToWatchlist, rpc:fleetObjectHandler.AcceptInvite, rpc:fleetObjectHandler.CreateSquad, rpc:fleetObjectHandler.CreateWing, rpc:fleetObjectHandler.GetInitState, rpc:fleetObjectHandler.KickMember, rpc:fleetObjectHandler.MoveMember, rpc:fleetObjectHandler.SendBroadcast, rpc:fleetProxy.AddFleetFinderAdvert, rpc:fleetProxy.GetAvailableFleetAds
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
    let PyRep::Dict(wing_dict) = &init_kv["wings"] else {
        panic!("Expected wings Dict")
    };
    assert_eq!(
        wing_dict.len(),
        2,
        "Fleet must have 2 wings (Wing 1 + Skirmish Wing)"
    );
    assert!(wing_dict
        .iter()
        .any(|(k, v)| k == &PyRep::Integer(2) && v.dict_get("name") == Some(&PyRep::String("Skirmish Wing".into()))));
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

    ctx.advance_secs(12);
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
    let PyRep::Dict(member_dict) = &saj_kv["members"] else {
        panic!("Expected members Dict")
    };
    assert_eq!(member_dict.len(), 2, "Fleet must have 2 members");
    let p2_member = member_dict
        .iter()
        .find_map(|(k, v)| (k == &PyRep::Integer(90_000_002)).then_some(v))
        .expect("Pilot 2 must be in fleet members dict");
    assert_eq!(
        p2_member.dict_get("role"),
        Some(&PyRep::Integer(4)),
        "Pilot 2 role must be SquadMember (4)"
    );
    assert_eq!(
        p2_member.dict_get("wingID"),
        Some(&PyRep::Integer(wing2_id)),
        "Pilot 2 must be placed in Wing 2"
    );
    assert_eq!(
        p2_member.dict_get("squadID"),
        Some(&PyRep::Integer(squad2_id)),
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
    let PyRep::Dict(rem_members) = &sak_kv["members"] else {
        panic!()
    };
    assert_eq!(
        rem_members.len(),
        1,
        "Only FC should remain in fleet after kicking Pilot 2"
    );
    assert_eq!(rem_members[0].0, PyRep::Integer(90_000_001));

    let leave_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "LeaveFleet", &[], &kw, None)
        .await
        .expect("LeaveFleet");
    assert!(is_keyval(&leave_rep));
    assert_eq!(sess1.fleet_id, None);
}

#[tokio::test]
async fn test_so7_chat_channels_local_corp_fleet_player_and_private() {
    // parity: feat:social.chat-xmpp-auth, feat:social.chat-local, feat:social.chat-corp-alliance-fleet, feat:social.chat-player-channels, feat:social.chat-private, rpc:XmppChatMgr.CreatePlayerOwnedChannel, rpc:XmppChatMgr.CreatePrivateChannel, rpc:charUnboundMgr.SelectCharacterID, rpc:chatAuthenticationService.GetAuthenticationToken, rpc:fleetObjectHandler.Init, rpc:fleetObjectHandler.LeaveFleet
    eve_testkit::attest(&[
        "feat:social.chat-xmpp-auth",
        "feat:social.chat-local",
        "feat:social.chat-corp-alliance-fleet",
        "feat:social.chat-player-channels",
        "feat:social.chat-private",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(701);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("Select Pilot 1");

    let mut sess2 = SessionState::new(702);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None)
        .await
        .expect("Select Pilot 2");

    // 0. XMPP Authentication Token
    let auth_rep = ctx
        .dispatch_rpc(&mut sess1, "chatAuthenticationService", None, "GetAuthenticationToken", &[], &kw, None)
        .await
        .expect("GetAuthenticationToken");
    assert!(is_keyval(&auth_rep));

    // 1. Local Chat (`feat:social.chat-local`): K-space immediate membership vs Wormhole/Pochven delayed local (unseen until speaking)
    let resync_jita = ctx
        .dispatch_rpc(&mut sess1, "XmppChatMgr", None, "ResyncSystemChannelAccess", &[], &kw, None)
        .await
        .expect("ResyncSystemChannelAccess Jita");
    let PyRep::KeyVal(rj_kv) = &resync_jita else { panic!() };
    assert_eq!(rj_kv["channelID"], PyRep::String("solarsystemid2:30000142".into()));
    assert_eq!(rj_kv["isDelayedLocal"], PyRep::Bool(false));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.chat_channels["solarsystemid2:30000142"].members.contains(&90_000_001));
    });

    // Move to wormhole system 31000005 (delayed local: unseen until speaking) and resync via XmppChatMgr.ResyncSystemChannelAccess
    sess1.solar_system_id = Some(31_000_005);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 31_000_005;
    });
    let resync_wh = ctx
        .dispatch_rpc(&mut sess1, "XmppChatMgr", None, "ResyncSystemChannelAccess", &[], &kw, None)
        .await
        .expect("ResyncSystemChannelAccess Wormhole");
    let PyRep::KeyVal(rw_kv) = &resync_wh else { panic!() };
    assert_eq!(rw_kv["channelID"], PyRep::String("solarsystemid2:31000005".into()));
    assert_eq!(rw_kv["isDelayedLocal"], PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.chat_channels["solarsystemid2:30000142"].members.contains(&90_000_001), "Old system local membership must be removed");
        assert!(snap.chat_channels["solarsystemid2:31000005"].is_delayed_local);
    });

    let wh_before_speak = ctx
        .dispatch_rpc(&mut sess1, "XmppChatMgr", None, "AddCharacterToChannel", &[PyRep::String("solarsystemid2:31000005".into())], &kw, None)
        .await
        .expect("AddCharacterToChannel wormhole before speaking");
    let PyRep::KeyVal(wbs_kv) = &wh_before_speak else { panic!() };
    assert_eq!(wbs_kv["memberCount"], PyRep::Integer(0), "Pilot in wormhole delayed local must be unseen before speaking");

    ctx.dispatch_rpc(&mut sess1, "LSC", None, "SendMessage", &[PyRep::String("solarsystemid2:31000005".into()), PyRep::String("o7 in J-space".into())], &kw, None)
        .await
        .expect("SendMessage in wormhole local");
    let wh_after_speak = ctx
        .dispatch_rpc(&mut sess1, "XmppChatMgr", None, "AddCharacterToChannel", &[PyRep::String("solarsystemid2:31000005".into())], &kw, None)
        .await
        .expect("AddCharacterToChannel wormhole after speaking");
    let PyRep::KeyVal(was_kv) = &wh_after_speak else { panic!() };
    assert_eq!(was_kv["memberCount"], PyRep::Integer(1), "Pilot in wormhole delayed local must become visible after speaking");

    // Verify Pochven (30002086) also uses delayed local
    sess1.solar_system_id = Some(30_002_086);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 30_002_086;
    });
    let resync_poch = ctx
        .dispatch_rpc(&mut sess1, "XmppChatMgr", None, "ResyncSystemChannelAccess", &[], &kw, None)
        .await
        .expect("ResyncSystemChannelAccess Pochven");
    let PyRep::KeyVal(rp_kv) = &resync_poch else { panic!() };
    assert_eq!(rp_kv["isDelayedLocal"], PyRep::Bool(true));
    sess1.solar_system_id = Some(30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 30_000_142;
    });

    // 2. Corp, Alliance & Fleet chat channels (`feat:social.chat-corp-alliance-fleet`): automatic membership & revocation on change
    ctx.advance_secs(12);
    let corp_rep = ctx
        .dispatch_rpc(&mut sess1, "corpRegistry", None, "AddCorporation", &[PyRep::String("Comms Relay Corp".into()), PyRep::String("CRELY".into()), PyRep::Real(0.05)], &kw, None)
        .await
        .expect("AddCorporation for chat test");
    let PyRep::KeyVal(cc_kv) = &corp_rep else { panic!() };
    let new_corp_id = cc_kv["corporationID"].as_i64().unwrap();
    sess1.alliance_id = Some(99_000_777);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().alliance_id = Some(99_000_777);
    });

    let fleet_id = ctx
        .dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateFleet", &[], &kw, None)
        .await
        .expect("fleetObjectHandler.CreateFleet")
        .as_i64()
        .unwrap();
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "Init", &[PyRep::Integer(587)], &kw, None)
        .await
        .expect("fleetObjectHandler.Init");

    ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "ResyncSystemChannelAccess", &[], &kw, None)
        .await
        .expect("ResyncSystemChannelAccess with new corp, alliance, and fleet");
    let corp_chan_name = format!("corpid:{new_corp_id}");
    let alliance_chan_name = "allianceid:99000777".to_string();
    let fleet_chan_name = format!("fleetid:{fleet_id}");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.chat_channels.get("corpid:1000009").is_some_and(|c| c.members.contains(&90_000_001)), "Old NPC corp chat access must be revoked");
        assert!(snap.chat_channels[&corp_chan_name].members.contains(&90_000_001), "New corp chat must be auto-joined");
        assert!(snap.chat_channels[&alliance_chan_name].members.contains(&90_000_001), "Alliance chat must be auto-joined");
        assert!(snap.chat_channels[&fleet_chan_name].members.contains(&90_000_001), "Fleet chat must be auto-joined");
    });

    // Leave fleet and resync -> fleet chat access must be revoked
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "LeaveFleet", &[], &kw, None)
        .await
        .expect("LeaveFleet");
    ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "ResyncSystemChannelAccess", &[], &kw, None)
        .await
        .expect("Resync after LeaveFleet");
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.chat_channels.get(&fleet_chan_name).is_some_and(|c| c.members.contains(&90_000_001)), "Fleet chat access must be revoked on LeaveFleet");
    });

    // 3. Player Created Custom Channels (`feat:social.chat-player-channels`): password, operators, mute/ban, GrantChannelOwnership
    let create_chan = ctx
        .dispatch_rpc(
            &mut sess1,
            "XmppChatMgr",
            None,
            "CreatePlayerOwnedChannel",
            &[PyRep::String("WormholeHunters".into()), PyRep::String("wh-pass".into()), PyRep::String("Class 5 capital escalations".into())],
            &kw,
            None,
        )
        .await
        .expect("CreatePlayerOwnedChannel");
    let PyRep::KeyVal(ch_kv) = &create_chan else { panic!() };
    let custom_chan_id = ch_kv["channelID"].as_str().unwrap().to_string();

    // Wrong password rejected; right password accepted
    let bad_pw = ctx
        .dispatch_rpc(&mut sess2, "XmppChatMgr", None, "AddCharacterToChannel", &[PyRep::String(custom_chan_id.clone()), PyRep::String("wrong-pw".into())], &kw, None)
        .await
        .expect_err("Joining password-protected player channel with wrong password must fail");
    assert_eq!(bad_pw.code, "InvalidChannelPassword");

    ctx.dispatch_rpc(&mut sess2, "XmppChatMgr", None, "AddCharacterToChannel", &[PyRep::String(custom_chan_id.clone()), PyRep::String("wh-pass".into())], &kw, None)
        .await
        .expect("Join player channel with valid password");

    // Mute and unmute Pilot 2 via XmppChatMgr.GMMute / GMUnmute, grant operator, and transfer ownership via GrantChannelOwnership
    ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GMMute", &[PyRep::Integer(90_000_002), PyRep::String(custom_chan_id.clone()), PyRep::Integer(300)], &kw, None)
        .await
        .expect("GMMute Pilot 2");
    let muted_err = ctx
        .dispatch_rpc(&mut sess2, "LSC", None, "SendMessage", &[PyRep::String(custom_chan_id.clone()), PyRep::String("Testing mute".into())], &kw, None)
        .await
        .expect_err("Muted member must not send messages");
    assert_eq!(muted_err.code, "MutedInChannel");
    ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GMUnmute", &[PyRep::Integer(90_000_002), PyRep::String(custom_chan_id.clone())], &kw, None)
        .await
        .expect("GMUnmute Pilot 2");

    ctx.dispatch_rpc(&mut sess1, "LSC", None, "AccessControl", &[PyRep::String(custom_chan_id.clone()), PyRep::Integer(90_000_002), PyRep::Integer(2)], &kw, None)
        .await
        .expect("AccessControl grant operator");
    ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GrantChannelOwnership", &[PyRep::String(custom_chan_id.clone()), PyRep::Integer(90_000_002)], &kw, None)
        .await
        .expect("GrantChannelOwnership");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.chat_channels[&custom_chan_id].owner_id, Some(90_000_002), "Channel ownership must transfer to Pilot 2");
    });

    // 4. Private 1-on-1 Conversation (`feat:social.chat-private`): block list & CSPA fee enforcement
    ctx.dispatch_rpc(&mut sess2, "charMgr", None, "BlockOwners", &[PyRep::List(vec![PyRep::Integer(90_000_001)])], &kw, None)
        .await
        .expect("Pilot 2 BlockOwners Pilot 1");
    let blocked_err = ctx
        .dispatch_rpc(&mut sess1, "XmppChatMgr", None, "CreatePrivateChannel", &[PyRep::Integer(90_000_002)], &kw, None)
        .await
        .expect_err("Blocked sender must be denied CreatePrivateChannel");
    assert_eq!(blocked_err.code, "SenderBlockedByRecipient");

    ctx.dispatch_rpc(&mut sess2, "charMgr", None, "UnblockOwners", &[PyRep::List(vec![PyRep::Integer(90_000_001)])], &kw, None)
        .await
        .expect("Pilot 2 UnblockOwners Pilot 1");
    ctx.dispatch_rpc(&mut sess2, "charMgr", None, "LogSettings", &[PyRep::Real(50_000.0)], &kw, None)
        .await
        .expect("Pilot 2 sets 50,000 ISK CSPA fee");

    let bal_before_cspa = ctx.db.with_snapshot(|snap| snap.characters[&90_000_001].balance);
    let priv_convo = ctx
        .dispatch_rpc(&mut sess1, "XmppChatMgr", None, "CreatePrivateChannel", &[PyRep::Integer(90_000_002)], &kw, None)
        .await
        .expect("CreatePrivateChannel with CSPA charge");
    let PyRep::KeyVal(pc_kv) = &priv_convo else { panic!() };
    let priv_chan_id = pc_kv["channelID"].as_str().unwrap().to_string();
    let bal_after_cspa = ctx.db.with_snapshot(|snap| snap.characters[&90_000_001].balance);
    assert_eq!(bal_before_cspa.0 - bal_after_cspa.0, 5_000_000, "CreatePrivateChannel must debit recipient's 50,000 ISK CSPA fee");

    let _ = ctx.drain_notifications_for_char(90_000_002);
    let send_priv = ctx
        .dispatch_rpc(&mut sess1, "LSC", None, "SendMessage", &[PyRep::String(priv_chan_id), PyRep::String("Cynosaural field is up".into())], &kw, None)
        .await
        .expect("SendMessage Private");
    assert!(is_keyval(&send_priv));
    let p2_priv = ctx.drain_notifications_for_char(90_000_002);
    assert!(!p2_priv.is_empty(), "Pilot 2 should receive notification for private conversation");
}
