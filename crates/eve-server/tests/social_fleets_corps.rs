//! Track 4: Phase 8 Social Systems, Mail, Fleets, Corporations & Chat Integration Tests (`SO-1`..`SO-7`).
//!
// parity: plan:SO-1, plan:SO-2, plan:SO-3, plan:SO-4, plan:SO-5, plan:SO-6, plan:SO-7

#![forbid(unsafe_code)]

mod common;

use eve_db::corp_roles;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

const AU: f64 = 149_597_870_700.0;

fn is_keyval(rep: &PyRep) -> bool {
    matches!(rep, PyRep::KeyVal(_))
}

fn is_rowset(rep: &PyRep) -> bool {
    matches!(rep, PyRep::Rowset { .. })
}

#[tokio::test]
// parity: plan:SO-1
// parity: feat:social.mail-send, feat:social.mail-headers, feat:social.mail-body, feat:social.mail-read-state, feat:social.mail-labels
async fn test_so1_mail_lifecycle_labels_and_notifications() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // 1. Login Pilot 1 (Demo, 90000001) and Pilot 2 (Other, 90000002)
    let mut sess1 = SessionState::new(101);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("Select Pilot 1");

    let mut sess2 = SessionState::new(102);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None)
        .await
        .expect("Select Pilot 2");

    // 2. Mail Labels: Pilot 2 creates custom labels with hex color strings
    let label1_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "CreateLabel", &[PyRep::String("Important".into()), PyRep::String("#FF0000".into())], &kw, None)
        .await
        .expect("CreateLabel 1");
    let PyRep::KeyVal(l1_kv) = &label1_rep else { panic!("Expected KeyVal for CreateLabel 1") };
    let label1_id = l1_kv["labelID"].as_i64().unwrap();
    assert_eq!(l1_kv["name"], PyRep::String("Important".into()));
    assert_eq!(l1_kv["color"], PyRep::String("#FF0000".into()));

    let label2_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "CreateLabel", &[PyRep::String("Orders".into()), PyRep::String("#0000FF".into())], &kw, None)
        .await
        .expect("CreateLabel 2");
    let PyRep::KeyVal(l2_kv) = &label2_rep else { panic!("Expected KeyVal for CreateLabel 2") };
    let label2_id = l2_kv["labelID"].as_i64().unwrap();
    assert_eq!(l2_kv["name"], PyRep::String("Orders".into()));
    assert_eq!(l2_kv["color"], PyRep::String("#0000FF".into()));

    // Edit label 2 name and hex color
    let edit_label_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "EditLabel", &[PyRep::Integer(label2_id), PyRep::String("Fleet Orders".into()), PyRep::String("#00FF00".into())], &kw, None)
        .await
        .expect("EditLabel");
    let PyRep::KeyVal(edit_kv) = &edit_label_rep else { panic!("Expected KeyVal for EditLabel") };
    assert_eq!(edit_kv["labelID"], PyRep::Integer(label2_id));
    assert_eq!(edit_kv["name"], PyRep::String("Fleet Orders".into()));
    assert_eq!(edit_kv["color"], PyRep::String("#00FF00".into()));

    // Query labels and verify custom label names and hex colors
    let labels_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "GetLabels", &[], &kw, None)
        .await
        .expect("GetLabels");
    let PyRep::Rowset { rows: label_rows, .. } = &labels_rep else { panic!("Expected Rowset from GetLabels") };
    assert_eq!(label_rows.len(), 6, "Pilot 2 should have 4 system labels + 2 custom labels");
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

    // Notifications check: Pilot 2 gets OnMailReceived, Pilot 1 gets OnMailSent
    let p2_notifs = ctx.drain_notifications_for_char(90_000_002);
    assert!(p2_notifs.iter().any(|n| n.event == "OnMailReceived"), "Expected OnMailReceived for Pilot 2");
    let p1_notifs = ctx.drain_notifications_for_char(90_000_001);
    assert!(p1_notifs.iter().any(|n| n.event == "OnMailSent"), "Expected OnMailSent for Pilot 1");

    // 4. Pilot 2 fetches mail headers via SyncMail -> util.KeyVal { oldMail, newMail, mailStatus }
    let sync_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "SyncMail", &[], &kw, None)
        .await
        .expect("SyncMail");
    let PyRep::KeyVal(sync_kv) = &sync_rep else { panic!("SyncMail must return util.KeyVal") };
    assert!(is_rowset(&sync_kv["oldMail"]), "SyncMail must include oldMail Rowset");
    let PyRep::Rowset { rows: new_mail_rows, .. } = &sync_kv["newMail"] else {
        panic!("SyncMail must include newMail Rowset");
    };
    let mail_hdr = new_mail_rows
        .iter()
        .find(|cols| cols.first() == Some(&PyRep::Integer(mail_id)))
        .expect("newMail must contain sent message header");
    assert_eq!(mail_hdr[1], PyRep::Integer(90_000_001), "senderID must match Pilot 1");
    assert_eq!(mail_hdr[2], PyRep::List(vec![PyRep::Integer(90_000_002)]), "toCharacterIDs must match Pilot 2");
    assert_eq!(mail_hdr[3], PyRep::String(expected_title.into()), "title must match");
    assert_eq!(mail_hdr[5], PyRep::Bool(false), "unread mail must have read == false");

    let PyRep::Rowset { rows: status_rows, .. } = &sync_kv["mailStatus"] else {
        panic!("SyncMail must include mailStatus Rowset");
    };
    let status_row = status_rows
        .iter()
        .find(|cols| cols.first() == Some(&PyRep::Integer(mail_id)))
        .expect("mailStatus must contain message status row");
    assert_eq!(status_row[1], PyRep::Integer(0), "unread statusMask must be 0");
    assert_eq!(status_row[3], PyRep::Bool(false), "unread mailStatus read flag must be false");

    // 5. Pilot 2 reads mail body: verify body text and auto-mark as read
    let body_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "GetBody", &[PyRep::Integer(mail_id)], &kw, None)
        .await
        .expect("GetBody");
    let PyRep::KeyVal(body_kv) = &body_rep else { panic!("Expected KeyVal from GetBody") };
    assert_eq!(body_kv["messageID"], PyRep::Integer(mail_id));
    assert_eq!(body_kv["body"], PyRep::String(expected_body.into()), "GetBody must return exact mail body text");
    assert_eq!(body_kv["read"], PyRep::Bool(true));

    ctx.db.with_snapshot(|snap| {
        assert!(snap.read_mails.contains(&(90_000_002, mail_id)), "Mail must be marked read on GetBody");
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
    let PyRep::KeyVal(unread_kv) = &unread_rep else { panic!("Expected KeyVal from MarkAsUnread") };
    assert_eq!(unread_kv["read"], PyRep::Bool(false));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.read_mails.contains(&(90_000_002, mail_id)), "Mail must be unread after MarkAsUnread");
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
    let PyRep::KeyVal(mark_read_kv) = &mark_read_rep else { panic!("Expected KeyVal from MarkAsRead") };
    assert_eq!(mark_read_kv["read"], PyRep::Bool(true));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.read_mails.contains(&(90_000_002, mail_id)), "Mail must be marked read after MarkAsRead");
    });

    // 7. Assign labels to mail using real V24/Crucible signature: AssignLabels(messageIDs list, labelID)
    ctx.dispatch_rpc(
        &mut sess2,
        "mailMgr",
        None,
        "AssignLabels",
        &[PyRep::List(vec![PyRep::Integer(mail_id)]), PyRep::Integer(label1_id)],
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
        &[PyRep::List(vec![PyRep::Integer(mail_id)]), PyRep::Integer(label2_id)],
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
    let PyRep::KeyVal(sl_kv) = &sync_labeled else { panic!() };
    let PyRep::Rowset { rows: sl_rows, .. } = &sl_kv["newMail"] else { panic!() };
    let PyRep::List(assigned_labels) = &sl_rows[0][6] else { panic!("Expected labels list") };
    assert!(assigned_labels.contains(&PyRep::Integer(label1_id)), "Mail must have label 1 assigned");
    assert!(assigned_labels.contains(&PyRep::Integer(label2_id)), "Mail must have label 2 assigned");

    // Remove label 1 using real signature: RemoveLabels(messageIDs list, labelID)
    ctx.dispatch_rpc(
        &mut sess2,
        "mailMgr",
        None,
        "RemoveLabels",
        &[PyRep::List(vec![PyRep::Integer(mail_id)]), PyRep::Integer(label1_id)],
        &kw,
        None,
    )
    .await
    .expect("RemoveLabels");

    let sync_removed = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "SyncMail", &[], &kw, None)
        .await
        .expect("SyncMail after RemoveLabels");
    let PyRep::KeyVal(sr_kv) = &sync_removed else { panic!() };
    let PyRep::Rowset { rows: sr_rows, .. } = &sr_kv["newMail"] else { panic!() };
    let PyRep::List(remaining_labels) = &sr_rows[0][6] else { panic!("Expected labels list") };
    assert!(!remaining_labels.contains(&PyRep::Integer(label1_id)), "Label 1 must be removed");
    assert!(remaining_labels.contains(&PyRep::Integer(label2_id)), "Label 2 must remain assigned");

    // Delete label 1 and verify GetLabels now has 5 labels
    ctx.dispatch_rpc(&mut sess2, "mailMgr", None, "DeleteLabel", &[PyRep::Integer(label1_id)], &kw, None)
        .await
        .expect("DeleteLabel");
    let labels_after_del = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "GetLabels", &[], &kw, None)
        .await
        .expect("GetLabels after DeleteLabel");
    let PyRep::Rowset { rows: label_rows_after, .. } = &labels_after_del else { panic!() };
    assert_eq!(label_rows_after.len(), 5);
    assert!(!label_rows_after.iter().any(|r| r[0] == PyRep::Integer(label1_id)));

    // 8. Delete mail and verify removal from SyncMail
    ctx.dispatch_rpc(&mut sess2, "mailMgr", None, "DeleteMail", &[PyRep::Integer(mail_id)], &kw, None)
        .await
        .expect("DeleteMail");
    let sync_after_del = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "SyncMail", &[], &kw, None)
        .await
        .expect("SyncMail after DeleteMail");
    let PyRep::KeyVal(sad_kv) = &sync_after_del else { panic!() };
    let PyRep::Rowset { rows: sad_rows, .. } = &sad_kv["newMail"] else { panic!() };
    assert!(sad_rows.is_empty(), "Deleted mail must no longer appear in SyncMail");
}

#[tokio::test]
// parity: plan:SO-2
// parity: feat:fleet.create-fleet, feat:fleet.invite-accept, feat:fleet.leave-kick, feat:navigation.warp-to-member, feat:navigation.fleet-warp
async fn test_so2_fleet_manager_hierarchy_broadcast_and_fleet_warp() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // 1. Login Pilot 1 (FC) and Pilot 2 (Member)
    let mut sess1 = SessionState::new(201);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("Select Pilot 1");

    let mut sess2 = SessionState::new(202);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None)
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
    let PyRep::Tuple(bind_tuple) = &bind_rep else { panic!("Expected Tuple from MachoBindObject") };
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
    let PyRep::KeyVal(wing_kv) = &wing_rep else { panic!("Expected KeyVal from CreateWing") };
    let wing2_id = wing_kv["wingID"].as_i64().unwrap();
    assert_eq!(wing2_id, 2);

    let squad_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            Some(fleet_bound_id),
            "CreateSquad",
            &[PyRep::Integer(wing2_id), PyRep::String("Interceptors".into())],
            &kw,
            None,
        )
        .await
        .expect("CreateSquad");
    let PyRep::KeyVal(squad_kv) = &squad_rep else { panic!("Expected KeyVal from CreateSquad") };
    let squad2_id = squad_kv["squadID"].as_i64().unwrap();
    assert_eq!(squad2_id, 2);

    let init_state = ctx
        .dispatch_rpc(&mut sess1, "fleetObjectHandler", Some(fleet_bound_id), "GetInitState", &[], &kw, None)
        .await
        .expect("GetInitState");
    let PyRep::KeyVal(init_kv) = &init_state else { panic!("Expected KeyVal from GetInitState") };
    assert_eq!(init_kv["fleetID"], PyRep::Integer(fid));
    assert_eq!(init_kv["bossID"], PyRep::Integer(90_000_001));
    let PyRep::Rowset { rows: wing_rows, .. } = &init_kv["wings"] else { panic!("Expected wings Rowset") };
    assert_eq!(wing_rows.len(), 2, "Fleet must have 2 wings (Wing 1 + Skirmish Wing)");
    assert!(wing_rows.iter().any(|r| r[0] == PyRep::Integer(2) && r[1] == PyRep::String("Skirmish Wing".into())));
    let PyRep::Rowset { rows: squad_rows, .. } = &init_kv["squads"] else { panic!("Expected squads Rowset") };
    assert_eq!(squad_rows.len(), 2, "Fleet must have 2 squads (Squad 1 + Interceptors)");
    assert!(squad_rows.iter().any(|r| r[0] == PyRep::Integer(2) && r[1] == PyRep::Integer(2) && r[2] == PyRep::String("Interceptors".into())));

    // 4. Pilot 1 invites Pilot 2 via bound fleetObjectHandler, Pilot 2 accepts and is moved to Wing 2 / Squad 2
    let invite_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            Some(fleet_bound_id),
            "Invite",
            &[PyRep::Integer(90_000_002), PyRep::Integer(wing2_id), PyRep::Integer(squad2_id), PyRep::Integer(4)],
            &kw,
            None,
        )
        .await
        .expect("Invite");
    assert!(is_keyval(&invite_rep));

    let p2_invites = ctx.drain_notifications_for_char(90_000_002);
    assert!(p2_invites.iter().any(|n| n.event == "OnFleetInvite"), "OnFleetInvite notification required");

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
    let PyRep::Tuple(bind2_tuple) = &bind2_rep else { panic!() };
    let fleet_bound_id2 = bind2_tuple[0].as_i64().unwrap() as u64;

    let join_rep = ctx
        .dispatch_rpc(&mut sess2, "fleetObjectHandler", Some(fleet_bound_id2), "AcceptInvite", &[PyRep::Integer(fid)], &kw, None)
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
        &[PyRep::Integer(90_000_002), PyRep::Integer(4), PyRep::Integer(wing2_id), PyRep::Integer(squad2_id)],
        &kw,
        None,
    )
    .await
    .expect("MoveMember");

    // Verify member list and squad placement in fleet state
    let state_after_join = ctx
        .dispatch_rpc(&mut sess1, "fleetObjectHandler", Some(fleet_bound_id), "GetInitState", &[], &kw, None)
        .await
        .expect("GetInitState after join");
    let PyRep::KeyVal(saj_kv) = &state_after_join else { panic!() };
    let PyRep::Rowset { rows: member_rows, .. } = &saj_kv["members"] else { panic!("Expected members Rowset") };
    assert_eq!(member_rows.len(), 2, "Fleet must have 2 members");
    let p2_member_row = member_rows
        .iter()
        .find(|r| r[0] == PyRep::Integer(90_000_002))
        .expect("Pilot 2 must be in fleet members rowset");
    assert_eq!(p2_member_row[1], PyRep::Integer(4), "Pilot 2 role must be SquadMember (4)");
    assert_eq!(p2_member_row[2], PyRep::Integer(wing2_id), "Pilot 2 must be placed in Wing 2");
    assert_eq!(p2_member_row[3], PyRep::Integer(squad2_id), "Pilot 2 must be placed in Squad 2");

    // 5. Broadcast command via bound fleetObjectHandler
    let bcast_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            Some(fleet_bound_id),
            "SendBroadcast",
            &[PyRep::String("TargetPrimary".into()), PyRep::Integer(1), PyRep::Integer(100_000_002)],
            &kw,
            None,
        )
        .await
        .expect("SendBroadcast");
    assert!(is_keyval(&bcast_rep));

    let p2_bcasts = ctx.drain_notifications_for_char(90_000_002);
    assert!(p2_bcasts.iter().any(|n| n.event == "OnFleetBroadcast"), "OnFleetBroadcast notification required");

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

    ctx.db.try_with_mut_snapshot(|snap| {
        let ch1 = snap.characters.get_mut(&90_000_001).unwrap();
        ch1.active_ship_id = 200_000_001;
        ch1.solar_system_id = 30_000_142;
        ch1.station_id = None;
        let ch2 = snap.characters.get_mut(&90_000_002).unwrap();
        ch2.active_ship_id = 200_000_002;
        ch2.solar_system_id = 30_000_142;
        ch2.station_id = None;
        Ok::<(), std::convert::Infallible>(())
    }).unwrap();

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
    let (_, b2_warping) = sys.get_ball_snapshot(200_000_002).await.unwrap().expect("ball 2 exists");
    assert!(
        matches!(b2_warping.mode, MovementMode::Warp { .. }),
        "Pilot 2 ship must enter MovementMode::Warp on WarpToMember"
    );
    for _ in 0..30 {
        let _ = sys.step_manual(1.0).await;
    }
    let (_, b2_landed) = sys.get_ball_snapshot(200_000_002).await.unwrap().expect("ball 2 exists");
    assert!(
        b2_landed.position.distance(DVec3::new(20_000.0, 0.0, 0.0)) < 2_500.0,
        "Pilot 2 must land ~10km from Pilot 1 (at ~20,000m), got {:?}",
        b2_landed.position
    );

    // 8. Fleet Warp: Pilot 1 (FC) initiates fleet warp for all on-grid fleet members
    let fleet_warp_dest = DVec3::new(1_000_000_000.0, 0.0, 0.0);
    let fleet_warp_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            Some(fleet_bound_id),
            "CmdFleetWarp",
            &[PyRep::Real(fleet_warp_dest.x), PyRep::Real(fleet_warp_dest.y), PyRep::Real(fleet_warp_dest.z), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect("CmdFleetWarp");
    assert!(is_keyval(&fleet_warp_rep));

    // Verify both Pilot 1 and Pilot 2 entered Warp together and land at fleet_warp_dest
    let (_, b1_fw) = sys.get_ball_snapshot(200_000_001).await.unwrap().unwrap();
    let (_, b2_fw) = sys.get_ball_snapshot(200_000_002).await.unwrap().unwrap();
    assert!(matches!(b1_fw.mode, MovementMode::Warp { .. }), "FC ship must enter Warp on CmdFleetWarp");
    assert!(matches!(b2_fw.mode, MovementMode::Warp { .. }), "Squad member ship must enter Warp on CmdFleetWarp");
    for _ in 0..30 {
        let _ = sys.step_manual(1.0).await;
    }
    let (_, b1_fw_landed) = sys.get_ball_snapshot(200_000_001).await.unwrap().unwrap();
    let (_, b2_fw_landed) = sys.get_ball_snapshot(200_000_002).await.unwrap().unwrap();
    assert!(b1_fw_landed.position.distance(fleet_warp_dest) < 2_500.0, "FC must land at fleet warp destination");
    assert!(b2_fw_landed.position.distance(fleet_warp_dest) < 2_500.0, "Member must land at fleet warp destination");

    // 9. Kick and Leave: verify notifications, kicked member's session & fleet state
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
        .dispatch_rpc(&mut sess2, "fleetMgr", None, "GetFleet", &[], &kw, None)
        .await
        .expect_err("Kicked member must not be in fleet");
    assert_eq!(p2_fleet_err.code, "NotInFleet");
    assert_eq!(sess2.fleet_id, None, "Kicked member session.fleet_id must be cleared");

    // Verify fleet member list now contains only Pilot 1
    let state_after_kick = ctx
        .dispatch_rpc(&mut sess1, "fleetObjectHandler", Some(fleet_bound_id), "GetInitState", &[], &kw, None)
        .await
        .expect("GetInitState after kick");
    let PyRep::KeyVal(sak_kv) = &state_after_kick else { panic!() };
    let PyRep::Rowset { rows: rem_members, .. } = &sak_kv["members"] else { panic!() };
    assert_eq!(rem_members.len(), 1, "Only FC should remain in fleet after kicking Pilot 2");
    assert_eq!(rem_members[0][0], PyRep::Integer(90_000_001));

    let leave_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "LeaveFleet", &[], &kw, None)
        .await
        .expect("LeaveFleet");
    assert!(is_keyval(&leave_rep));
    assert_eq!(sess1.fleet_id, None);
}

#[tokio::test]
// parity: plan:SO-3
// parity: feat:corporation.create-corp, feat:corporation.corp-public-info, feat:corporation.corp-edit-details, feat:wallet.corp-divisions
async fn test_so3_corporation_roles_public_info_and_wallet_divisions() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(301);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("Select Pilot 1");

    let mut sess2 = SessionState::new(302);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None)
        .await
        .expect("Select Pilot 2");

    // 1. Create Corporation: verify Corporation Management (3363) skill check and 1,599,800 ISK registration cost
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(skills) = snap.skills.get_mut(&90_000_001) {
            skills.remove(&3363);
        }
    });
    let no_skill_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "AddCorporation",
            &[PyRep::String("DeepSpace Logistics".into()), PyRep::String("DSLOG".into()), PyRep::Real(0.075)],
            &kw,
            None,
        )
        .await
        .expect_err("AddCorporation without Corporation Management skill must fail");
    assert_eq!(no_skill_err.code, "MissingCorporationManagementSkill");

    // Train Corporation Management (3363) to level 1 and record balance before creation
    let bal_before = ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            3363,
            eve_db::TrainedSkillRecord {
                type_id: 3363,
                level: 1,
                skill_points: 250.0,
            },
        );
        snap.characters[&90_000_001].balance
    });

    let create_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "AddCorporation",
            &[PyRep::String("DeepSpace Logistics".into()), PyRep::String("DSLOG".into()), PyRep::Real(0.075)],
            &kw,
            None,
        )
        .await
        .expect("AddCorporation");
    assert!(is_keyval(&create_rep));
    let corp_id = sess1.corporation_id.expect("corp_id set");

    let bal_after = ctx.db.with_snapshot(|snap| snap.characters[&90_000_001].balance);
    assert_eq!(
        bal_before.0 - bal_after.0,
        159_980_000,
        "AddCorporation must deduct 1,599,800.00 ISK (159,980,000 cents)"
    );

    // 2. Wallet Divisions: verify 7 starting divisions, rename division 2, transfer funds, and re-read
    let divs_rep = ctx
        .dispatch_rpc(&mut sess1, "account", None, "GetWalletDivisionsInfo", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetWalletDivisionsInfo");
    let PyRep::Rowset { rows: init_divs, .. } = &divs_rep else { panic!("Expected Rowset from GetWalletDivisionsInfo") };
    assert_eq!(init_divs.len(), 7, "New corporation must have 7 wallet divisions");
    assert_eq!(init_divs[0], vec![PyRep::Integer(1), PyRep::String("Master Wallet".into()), PyRep::Real(0.0)]);
    assert_eq!(init_divs[1], vec![PyRep::Integer(2), PyRep::String("Division 2".into()), PyRep::Real(0.0)]);

    // Rename Division 2 and transfer ISK from Division 1 to Division 2
    ctx.dispatch_rpc(
        &mut sess1,
        "corpmgr",
        None,
        "UpdateDivisionNames",
        &[PyRep::Integer(2), PyRep::String("Freight Escrow".into())],
        &kw,
        None,
    )
    .await
    .expect("UpdateDivisionNames");

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().wallet_divisions[0].balance =
            eve_db::IskCents::from_isk(500_000);
    });
    ctx.dispatch_rpc(
        &mut sess1,
        "account",
        None,
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(200_000.0)],
        &kw,
        None,
    )
    .await
    .expect("GiveCashFromCorpAccount");

    let divs_after_rep = ctx
        .dispatch_rpc(&mut sess1, "account", None, "GetWalletDivisionsInfo", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetWalletDivisionsInfo after update");
    let PyRep::Rowset { rows: updated_divs, .. } = &divs_after_rep else { panic!() };
    assert_eq!(updated_divs[0], vec![PyRep::Integer(1), PyRep::String("Master Wallet".into()), PyRep::Real(300_000.0)]);
    assert_eq!(updated_divs[1], vec![PyRep::Integer(2), PyRep::String("Freight Escrow".into()), PyRep::Real(200_000.0)]);

    // 3. Public Info & Edit Details: verify all public info fields before and after UpdateCorporationDetails
    let public_info = ctx
        .dispatch_rpc(&mut sess2, "corpmgr", None, "GetPublicInfo", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetPublicInfo");
    let PyRep::KeyVal(pub_kv) = &public_info else { panic!("Expected KeyVal from GetPublicInfo") };
    assert_eq!(pub_kv["corporationID"], PyRep::Integer(corp_id));
    assert_eq!(pub_kv["corporationName"], PyRep::String("DeepSpace Logistics".into()));
    assert_eq!(pub_kv["ticker"], PyRep::String("DSLOG".into()));
    assert_eq!(pub_kv["ceoID"], PyRep::Integer(90_000_001));
    assert_eq!(pub_kv["memberCount"], PyRep::Integer(1));
    assert_eq!(pub_kv["taxRate"], PyRep::Real(0.075));

    // Pilot 1 updates corp details (description, url, taxRate)
    let update_corp = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpmgr",
            None,
            "UpdateCorporationDetails",
            &[
                PyRep::Integer(corp_id),
                PyRep::String("Premier nullsec freight services".into()),
                PyRep::String("https://dslogistics.eve".into()),
                PyRep::Real(0.08),
            ],
            &kw,
            None,
        )
        .await
        .expect("UpdateCorporationDetails");
    assert!(is_keyval(&update_corp));

    // Re-read GetPublicInfo to verify persisted description, URL, and taxRate
    let public_info_after = ctx
        .dispatch_rpc(&mut sess2, "corpmgr", None, "GetPublicInfo", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetPublicInfo after UpdateCorporationDetails");
    let PyRep::KeyVal(pub_after_kv) = &public_info_after else { panic!() };
    assert_eq!(pub_after_kv["description"], PyRep::String("Premier nullsec freight services".into()));
    assert_eq!(pub_after_kv["url"], PyRep::String("https://dslogistics.eve".into()));
    assert_eq!(pub_after_kv["taxRate"], PyRep::Real(0.08));

    // 4. Pilot 2 Application and Acceptance
    let apply_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "corpmgr",
            None,
            "ApplyForMembership",
            &[PyRep::Integer(corp_id), PyRep::String("Experienced hauler applying".into())],
            &kw,
            None,
        )
        .await
        .expect("ApplyForMembership");
    let PyRep::KeyVal(app_kv) = &apply_rep else { panic!() };
    let app_id = app_kv["applicationID"].as_i64().unwrap();

    let accept_rep = ctx
        .dispatch_rpc(&mut sess1, "corpmgr", None, "AcceptApplication", &[PyRep::Integer(app_id)], &kw, None)
        .await
        .expect("AcceptApplication");
    assert!(is_keyval(&accept_rep));

    // Verify memberCount incremented to 2 in GetPublicInfo
    let pub_members = ctx
        .dispatch_rpc(&mut sess2, "corpmgr", None, "GetPublicInfo", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetPublicInfo after AcceptApplication");
    let PyRep::KeyVal(pm_kv) = &pub_members else { panic!() };
    assert_eq!(pm_kv["memberCount"], PyRep::Integer(2));

    // 5. Update Member Roles & verify role bitmask authorization
    let update_roles = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpmgr",
            None,
            "UpdateMemberRoles",
            &[PyRep::Integer(90_000_002), PyRep::Integer(corp_roles::ROLE_ACCOUNTANT as i64)],
            &kw,
            None,
        )
        .await
        .expect("UpdateMemberRoles");
    assert!(is_keyval(&update_roles));

    // Role bitmask authorization check: Pilot 2 (Accountant, not Director/CEO) denied from changing roles
    let denied = ctx
        .dispatch_rpc(
            &mut sess2,
            "corpmgr",
            None,
            "UpdateMemberRoles",
            &[PyRep::Integer(90_000_001), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await;
    assert_eq!(denied.unwrap_err().code, "CorpRoleDenied");
}

#[tokio::test]
// parity: plan:SO-4
// parity: feat:bookmarks.create, feat:bookmarks.update-delete, feat:bookmarks.folders, feat:bookmarks.shared-folders, feat:scanning.scan-bookmark
async fn test_so4_bookmarks_folders_shared_folders_and_scan_bookmarks() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(401);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("Select Pilot 1");

    let mut sess2 = SessionState::new(402);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None)
        .await
        .expect("Select Pilot 2");

    // 1. Folders & Subfolders (`AddFolder`, `DeleteFolder`, `CreateSubfolder`, `DeleteSubfolder`, per-character limits)
    let folder_rep = ctx
        .dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String("Safe Spots".into())], &kw, None)
        .await
        .expect("AddFolder");
    let PyRep::KeyVal(f_kv) = &folder_rep else { panic!("Expected KeyVal for AddFolder") };
    let folder_id = f_kv["folderID"].as_i64().unwrap();
    assert_eq!(f_kv["folderName"], PyRep::String("Safe Spots".into()));

    // Create 5 subfolders under folder_id (max 5 subfolders per folder)
    let mut subfolder_ids = Vec::new();
    for idx in 1..=5 {
        let sub_rep = ctx
            .dispatch_rpc(
                &mut sess1,
                "accessGroupBookmarkMgr",
                None,
                "CreateSubfolder",
                &[PyRep::Integer(folder_id), PyRep::String(format!("Sub {idx}"))],
                &kw,
                None,
            )
            .await
            .expect("CreateSubfolder");
        let PyRep::KeyVal(sub_kv) = &sub_rep else { panic!() };
        subfolder_ids.push(sub_kv["subfolderID"].as_i64().unwrap());
    }
    // 6th subfolder must be rejected by per-folder subfolder limit
    let sub_limit_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "accessGroupBookmarkMgr",
            None,
            "CreateSubfolder",
            &[PyRep::Integer(folder_id), PyRep::String("Sub 6 Overflow".into())],
            &kw,
            None,
        )
        .await
        .expect_err("6th subfolder must fail with MaxSubfoldersReached");
    assert_eq!(sub_limit_err.code, "MaxSubfoldersReached");

    // Deletesubfolder and verify removal
    for &sf_id in &subfolder_ids[1..] {
        ctx.dispatch_rpc(
            &mut sess1,
            "accessGroupBookmarkMgr",
            None,
            "DeleteSubfolder",
            &[PyRep::Integer(sf_id)],
            &kw,
            None,
        )
        .await
        .expect("DeleteSubfolder");
    }

    // Create 4 more personal root folders (total 5) and verify 6th personal folder fails with MaxPersonalFoldersReached
    let mut extra_folders = Vec::new();
    for idx in 2..=5 {
        let ef_rep = ctx
            .dispatch_rpc(
                &mut sess1,
                "accessGroupBookmarkMgr",
                None,
                "AddFolder",
                &[PyRep::String(format!("Personal Folder {idx}"))],
                &kw,
                None,
            )
            .await
            .expect("AddFolder within limit");
        let PyRep::KeyVal(ef_kv) = &ef_rep else { panic!() };
        extra_folders.push(ef_kv["folderID"].as_i64().unwrap());
    }
    let root_limit_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "accessGroupBookmarkMgr",
            None,
            "AddFolder",
            &[PyRep::String("Personal Folder 6 Overflow".into())],
            &kw,
            None,
        )
        .await
        .expect_err("6th personal root folder must fail with MaxPersonalFoldersReached");
    assert_eq!(root_limit_err.code, "MaxPersonalFoldersReached");

    for ef_id in extra_folders {
        ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "DeleteFolder", &[PyRep::Integer(ef_id)], &kw, None)
            .await
            .expect("Delete extra folder");
    }

    // 2. Create Bookmark in Folder (`BookmarkLocation`) and verify stored coordinates, item, and folder
    let ship_id = sess1.ship_id.expect("Pilot 1 ship_id");
    ctx.dispatch_rpc(&mut sess1, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock Pilot 1");
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let expected_pos = DVec3::new(125_000.0, -45_000.0, 80_000.0);
    let _ = sys.set_ball_position(ship_id, expected_pos).await;

    let bm1_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "BookmarkLocation",
            &[
                PyRep::String("Deep Safe 1".into()),
                PyRep::Integer(ship_id),
                PyRep::String("100 AU off plane".into()),
                PyRep::Integer(folder_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("BookmarkLocation");
    let PyRep::KeyVal(bm1_kv) = &bm1_rep else { panic!("Expected KeyVal from BookmarkLocation") };
    let bm_id = bm1_kv["bookmarkID"].as_i64().unwrap();

    // Verify stored coordinates, itemID, memo, and folderID via GetBookmarks
    let bms_rep = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "GetBookmarks", &[], &kw, None)
        .await
        .expect("GetBookmarks");
    let PyRep::Rowset { rows: bm_rows, .. } = &bms_rep else { panic!("Expected Rowset from GetBookmarks") };
    let bm_row = bm_rows
        .iter()
        .find(|r| r[0] == PyRep::Integer(bm_id))
        .expect("Created bookmark must appear in GetBookmarks");
    assert_eq!(bm_row[1], PyRep::String("Deep Safe 1".into()));
    assert_eq!(bm_row[2], PyRep::Integer(30_000_142));
    assert_eq!(bm_row[3], PyRep::Real(expected_pos.x));
    assert_eq!(bm_row[4], PyRep::Real(expected_pos.y));
    assert_eq!(bm_row[5], PyRep::Real(expected_pos.z));
    assert_eq!(bm_row[6], PyRep::Integer(ship_id));
    assert_eq!(bm_row[7], PyRep::String("100 AU off plane".into()));
    assert_eq!(bm_row[8], PyRep::Integer(folder_id));

    // 3. Update Bookmark and verify persisted change via GetBookmarks
    let update_bm = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "UpdateBookmark",
            &[PyRep::Integer(bm_id), PyRep::String("Deep Safe 1 (Checked)".into()), PyRep::String("Updated memo".into())],
            &kw,
            None,
        )
        .await
        .expect("UpdateBookmark");
    assert!(is_keyval(&update_bm));

    let bms_after_update = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "GetBookmarks", &[], &kw, None)
        .await
        .expect("GetBookmarks after UpdateBookmark");
    let PyRep::Rowset { rows: bm_rows_upd, .. } = &bms_after_update else { panic!() };
    let bm_row_upd = bm_rows_upd.iter().find(|r| r[0] == PyRep::Integer(bm_id)).unwrap();
    assert_eq!(bm_row_upd[1], PyRep::String("Deep Safe 1 (Checked)".into()));
    assert_eq!(bm_row_upd[7], PyRep::String("Updated memo".into()));

    // 4. Shared Bookmark Folders: create corporation via real RPCs, verify access-group rights,
    // AddToKnownFolders / RemoveFromKnownFolders, and OnSharedBookmarksFolderUpdated notifications
    let create_corp_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "AddCorporation",
            &[PyRep::String("Cartography Syndicate".into()), PyRep::String("CARTO".into()), PyRep::Real(0.05)],
            &kw,
            None,
        )
        .await
        .expect("AddCorporation for shared bookmarks");
    let PyRep::KeyVal(cc_kv) = &create_corp_rep else { panic!() };
    let corp_id = cc_kv["corporationID"].as_i64().unwrap();

    let apply_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "corpRegistry",
            None,
            "InsertApplication",
            &[PyRep::Integer(corp_id), PyRep::String("Scout joining".into())],
            &kw,
            None,
        )
        .await
        .expect("Pilot 2 InsertApplication");
    let PyRep::KeyVal(app_kv) = &apply_rep else { panic!() };
    let app_id = app_kv["applicationID"].as_i64().unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateApplicationOffer", &[PyRep::Integer(app_id)], &kw, None)
        .await
        .expect("Pilot 1 UpdateApplicationOffer");

    // Pilot 2 (normal corp member without Director/CEO access-group rights) cannot create a shared corp folder
    let p2_denied_shared = ctx
        .dispatch_rpc(
            &mut sess2,
            "accessGroupBookmarkMgr",
            None,
            "AddFolder",
            &[PyRep::String("Unauthorized Shared".into()), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect_err("Regular member without access-group rights must be denied shared AddFolder");
    assert_eq!(p2_denied_shared.code, "BookmarkAccessDenied");

    // Pilot 1 (CEO) creates shared folder and emits OnSharedBookmarksFolderUpdated
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let shared_folder_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "accessGroupBookmarkMgr",
            None,
            "AddFolder",
            &[PyRep::String("Corp Tacticals".into()), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("AddFolder shared");
    let PyRep::KeyVal(sf_kv) = &shared_folder_rep else { panic!() };
    let shared_folder_id = sf_kv["folderID"].as_i64().unwrap();
    assert_eq!(sf_kv["isShared"], PyRep::Bool(true));

    // Pilot 2 subscribes to the shared folder via AddToKnownFolders and receives OnSharedBookmarksFolderUpdated
    let add_known_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "accessGroupBookmarkMgr",
            None,
            "AddToKnownFolders",
            &[PyRep::Integer(shared_folder_id)],
            &kw,
            None,
        )
        .await
        .expect("AddToKnownFolders");
    let PyRep::KeyVal(ak_kv) = &add_known_rep else { panic!() };
    assert_eq!(ak_kv["subscribed"], PyRep::Bool(true));
    let p2_shared_notifs = ctx.drain_notifications_for_char(90_000_002);
    assert!(
        p2_shared_notifs.iter().any(|n| n.event == "OnSharedBookmarksFolderUpdated"),
        "AddToKnownFolders must emit OnSharedBookmarksFolderUpdated"
    );

    let p2_shared = ctx
        .dispatch_rpc(&mut sess2, "bookmark", None, "GetFolders", &[], &kw, None)
        .await
        .expect("GetFolders");
    let PyRep::Rowset { rows: sf_rows, .. } = &p2_shared else { panic!() };
    assert!(
        sf_rows.iter().any(|r| r[0] == PyRep::Integer(shared_folder_id) && r[1] == PyRep::String("Corp Tacticals".into())),
        "GetFolders must return Corp Tacticals for Pilot 2"
    );

    // Pilot 2 unsubscribes via RemoveFromKnownFolders and receives OnSharedBookmarksFolderUpdated
    let rem_known_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "accessGroupBookmarkMgr",
            None,
            "RemoveFromKnownFolders",
            &[PyRep::Integer(shared_folder_id)],
            &kw,
            None,
        )
        .await
        .expect("RemoveFromKnownFolders");
    let PyRep::KeyVal(rk_kv) = &rem_known_rep else { panic!() };
    assert_eq!(rk_kv["subscribed"], PyRep::Bool(false));
    let p2_unsub_notifs = ctx.drain_notifications_for_char(90_000_002);
    assert!(
        p2_unsub_notifs.iter().any(|n| n.event == "OnSharedBookmarksFolderUpdated"),
        "RemoveFromKnownFolders must emit OnSharedBookmarksFolderUpdated"
    );

    // 5. Scan Bookmark: resolve a cosmic signature to 100% via V24 scanMgr.RequestScans and save via BookmarkScanResult
    let scan_mgr_rep = ctx
        .dispatch_rpc(&mut sess1, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None)
        .await
        .expect("GetSystemScanMgr");
    let PyRep::KeyVal(sm_kv) = &scan_mgr_rep else { panic!() };
    let scan_bound_id = sm_kv["boundID"].as_i64().unwrap() as u64;

    let (wh_sig_id, wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&30_000_142].first().unwrap();
        (sig.signature_id.clone(), sig.position)
    });

    // Attempting BookmarkScanResult before resolving signature to 100% must fail with SignatureNotResolved
    let unres_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "BookmarkScanResult",
            &[PyRep::String(wh_sig_id.clone()), PyRep::String("Unresolved Sig".into()), PyRep::Integer(folder_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Bookmarking unresolved signature must fail");
    assert_eq!(unres_err.code, "SignatureNotResolved");

    // Fit Core Probe Launcher I + 8 probes on Pilot 1's ship, launch probes, and resolve signature via RequestScans
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(90_000_001).or_default();
        for &sid in &[3402u32, 3412, 3413, 3426] {
            skills.insert(
                sid,
                eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        snap.items.insert(
            440_001,
            eve_db::InventoryItemRecord {
                item_id: 440_001,
                type_id: 17938,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            440_002,
            eve_db::InventoryItemRecord {
                item_id: 440_002,
                type_id: 30013,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 8,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    let launch_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "dogmaIM",
            None,
            "Activate",
            &[PyRep::Integer(440_001), PyRep::String("launchScanners".into()), PyRep::None, PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("Launch probes in SO-4");
    let PyRep::KeyVal(lkv) = &launch_rep else { panic!() };
    let PyRep::List(pids) = &lkv["probeIDs"] else { panic!() };
    let probe_ids: Vec<i64> = pids.iter().filter_map(|p| p.as_i64()).collect();
    let center = DVec3::new(wh_pos[0], wh_pos[1], wh_pos[2]);
    let scan_radius = 0.25 * AU;
    let probes_dict = PyRep::Dict(
        probe_ids
            .iter()
            .map(|&pid| {
                (
                    PyRep::Integer(pid),
                    PyRep::keyval([
                        ("pos", PyRep::Tuple(vec![PyRep::Real(center.x), PyRep::Real(center.y), PyRep::Real(center.z)])),
                        ("scanRange", PyRep::Real(scan_radius)),
                    ]),
                )
            })
            .collect(),
    );
    for _ in 0..4 {
        ctx.dispatch_rpc(&mut sess1, "scanMgr", Some(scan_bound_id), "RequestScans", &[probes_dict.clone()], &kw, None)
            .await
            .expect("RequestScans in SO-4");
    }

    // Bookmark the 100%-resolved signature and verify persisted coordinates in GetBookmarks
    let scan_bm = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "BookmarkScanResult",
            &[
                PyRep::String(wh_sig_id.clone()),
                PyRep::String("Resolved K162".into()),
                PyRep::Integer(folder_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("BookmarkScanResult");
    let PyRep::KeyVal(sbm_kv) = &scan_bm else { panic!() };
    let scan_bm_id = sbm_kv["bookmarkID"].as_i64().unwrap();

    let bms_with_scan = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "GetBookmarks", &[], &kw, None)
        .await
        .expect("GetBookmarks after BookmarkScanResult");
    let PyRep::Rowset { rows: bms_scan_rows, .. } = &bms_with_scan else { panic!() };
    let scan_row = bms_scan_rows
        .iter()
        .find(|r| r[0] == PyRep::Integer(scan_bm_id))
        .expect("Scan bookmark must be persisted in GetBookmarks");
    assert_eq!(scan_row[1], PyRep::String("Resolved K162".into()));
    assert_eq!(scan_row[3], PyRep::Real(wh_pos[0]));
    assert_eq!(scan_row[4], PyRep::Real(wh_pos[1]));
    assert_eq!(scan_row[5], PyRep::Real(wh_pos[2]));
    assert_eq!(scan_row[8], PyRep::Integer(folder_id));

    // 6. Delete Bookmark and Folder, then verify removal in GetBookmarks and GetFolders
    let del_bm = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "DeleteBookmarks", &[PyRep::List(vec![PyRep::Integer(bm_id)])], &kw, None)
        .await
        .expect("DeleteBookmarks");
    assert!(is_keyval(&del_bm));

    let bms_after_del = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "GetBookmarks", &[], &kw, None)
        .await
        .expect("GetBookmarks after DeleteBookmarks");
    let PyRep::Rowset { rows: bms_del_rows, .. } = &bms_after_del else { panic!() };
    assert!(!bms_del_rows.iter().any(|r| r[0] == PyRep::Integer(bm_id)), "Deleted bookmark must be removed");

    let del_folder = ctx
        .dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "DeleteFolder", &[PyRep::Integer(folder_id)], &kw, None)
        .await
        .expect("DeleteFolder");
    assert!(is_keyval(&del_folder));

    let folders_after_del = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "GetFolders", &[], &kw, None)
        .await
        .expect("GetFolders after DeleteFolder");
    let PyRep::Rowset { rows: f_del_rows, .. } = &folders_after_del else { panic!() };
    assert!(!f_del_rows.iter().any(|r| r[0] == PyRep::Integer(folder_id)), "Deleted folder must be removed");
}

#[tokio::test]
// parity: plan:SO-5
// parity: feat:scanning.dscan, feat:scanning.probe-launch, feat:scanning.probe-scan-math, feat:scanning.probe-recall, feat:navigation.acceleration-gates
async fn test_so5_scanning_dscan_probes_and_acceleration_gates() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut sess = SessionState::new(501);
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
    .expect("Select Pilot 1");

    let char_id = 90_000_001i64;
    let ship_id = sess.ship_id.expect("ship_id");

    // Grant Astrometrics (3412) and CPU Management (3426) for Core Probe Launcher I (17938)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, i| i.location_id != ship_id);
        let skills = snap.skills.entry(char_id).or_default();
        for &sid in &[3402u32, 3412, 3413, 3426] {
            skills.insert(
                sid,
                eve_db::TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
        // Fit Core Probe Launcher I (type 17938, group 481, capacity 0.8 m3) in HiSlot0
        snap.items.insert(
            550_001,
            eve_db::InventoryItemRecord {
                item_id: 550_001,
                type_id: 17938,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Place 16 Core Scanner Probe I (type 30013, group 479, volume 0.1 m3 each) in Cargo
        snap.items.insert(
            550_002,
            eve_db::InventoryItemRecord {
                item_id: 550_002,
                type_id: 30013,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 16,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    // Undock into Jita (30000142)
    ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Undock", &[], &kw, None)
        .await
        .expect("Undock");

    let sys = ctx.systems.get_or_spawn(30_000_142);
    let _ = sys.set_ball_position(ship_id, DVec3::ZERO).await;

    let target_ball = Ball {
        id: 500_000_002,
        type_id: 603, // Merlin
        position: DVec3::new(0.0, 0.0, 10_000_000.0), // 10,000 km straight ahead (+Z)
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 3.0,
        mass: 5_000_000.0,
        radius: 80.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let recon_ball = Ball {
        id: 500_000_003,
        type_id: 11961, // Huginn (Combat Recon Ship, group 906 -> immune to D-scan!)
        position: DVec3::new(0.0, 0.0, 5_000_000.0),
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 3.0,
        mass: 12_000_000.0,
        radius: 120.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let cloaked_ball = Ball {
        id: 500_000_004,
        type_id: 603,
        position: DVec3::new(0.0, 0.0, 4_000_000.0),
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 3.0,
        mass: 5_000_000.0,
        radius: 80.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(target_ball).await;
    let _ = sys.spawn_ball(recon_ball).await;
    let mut cloaked_cs = eve_destiny::CombatState::default();
    cloaked_cs.is_cloaked = true;
    let _ = sys.spawn_ball_with_combat(cloaked_ball, cloaked_cs).await;

    // Bind scanMgr via real V24 scanMgr.GetSystemScanMgr
    let scan_mgr_rep = ctx
        .dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None)
        .await
        .expect("GetSystemScanMgr");
    let PyRep::KeyVal(sm_kv) = &scan_mgr_rep else { panic!("Expected KeyVal from GetSystemScanMgr") };
    let scan_bound_id = sm_kv["boundID"].as_i64().unwrap() as u64;

    // 1. D-Scan via real V24 scanMgr.ConeScan(scanangle, scanRange, x, y, z)
    // 360 degree scan detects Merlin (500_000_002), but NOT Combat Recon Huginn (500_000_003) or Cloaked ship (500_000_004)
    let dscan_360 = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            Some(scan_bound_id),
            "ConeScan",
            &[
                PyRep::Real(360.0),
                PyRep::Real(14.3 * AU),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::Real(1.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("ConeScan 360");
    let PyRep::List(hits_360) = &dscan_360 else { panic!("Expected List from ConeScan") };
    let has_hit = |hits: &[PyRep], item_id: i64| -> bool {
        hits.iter().any(|h| {
            let PyRep::KeyVal(kv) = h else { return false };
            kv.iter().any(|(k, v)| k == "id" && v.as_i64() == Some(item_id))
        })
    };
    assert!(has_hit(hits_360, 500_000_002), "Merlin must appear in 360 ConeScan");
    assert!(!has_hit(hits_360, 500_000_003), "Combat Recon Huginn (group 906) must be immune to ConeScan");
    assert!(!has_hit(hits_360, 500_000_004), "Cloaked ship must be immune to ConeScan");

    // Narrow 15-degree cone directly forward (+Z) detects Merlin
    let dscan_15_fwd = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            Some(scan_bound_id),
            "ConeScan",
            &[
                PyRep::Real(15.0),
                PyRep::Real(14.3 * AU),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::Real(1.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("ConeScan 15 forward");
    let PyRep::List(hits_15_fwd) = &dscan_15_fwd else { panic!() };
    assert!(has_hit(hits_15_fwd, 500_000_002), "Merlin must appear in 15-deg forward ConeScan");

    // Narrow 15-degree cone backward (-Z) misses Merlin
    let dscan_15_back = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            Some(scan_bound_id),
            "ConeScan",
            &[
                PyRep::Real(15.0),
                PyRep::Real(14.3 * AU),
                PyRep::Real(0.0),
                PyRep::Real(0.0),
                PyRep::Real(-1.0),
            ],
            &kw,
            None,
        )
        .await
        .expect("ConeScan 15 backward");
    let PyRep::List(hits_15_back) = &dscan_15_back else { panic!() };
    assert!(!has_hit(hits_15_back, 500_000_002), "Merlin must NOT appear in 15-deg backward ConeScan");

    // 2. Load 8 Core Scanner Probe I (30013) into Core Probe Launcher I (17938) via dogmaIM.LoadAmmo
    let load_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "LoadAmmo",
            &[
                PyRep::Integer(ship_id),
                PyRep::List(vec![PyRep::Integer(550_001)]),
                PyRep::List(vec![PyRep::Integer(550_002)]),
            ],
            &kw,
            None,
        )
        .await
        .expect("LoadAmmo Core Scanner Probes");
    let PyRep::KeyVal(load_kv) = &load_rep else { panic!() };
    assert_eq!(
        load_kv["totalLoaded"],
        PyRep::Integer(8),
        "Core Probe Launcher I (0.8 m3) must load exactly 8 probes (0.1 m3 each)"
    );

    // Activate Core Probe Launcher I via dogmaIM.Activate to deploy an 8-probe Pinpoint formation
    let act_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(550_001),
                PyRep::String("launchScanners".into()),
                PyRep::None,
                PyRep::Integer(0),
            ],
            &kw,
            None,
        )
        .await
        .expect("Activate Core Probe Launcher I");
    let PyRep::KeyVal(akv) = &act_rep else { panic!("Expected KeyVal from probe launch") };
    let PyRep::List(probe_id_reps) = &akv["probeIDs"] else { panic!("Expected probeIDs list") };
    let launched_probe_ids: Vec<i64> = probe_id_reps.iter().filter_map(|p| p.as_i64()).collect();
    assert_eq!(launched_probe_ids.len(), 8);

    // 2nd launch attempt when launcher is empty must fail with NoChargesLoaded (all 8 loaded probes deployed)
    let empty_err = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            None,
            "Activate",
            &[
                PyRep::Integer(550_001),
                PyRep::String("launchScanners".into()),
                PyRep::None,
                PyRep::Integer(0),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Launch must fail when launcher is empty");
    assert_eq!(empty_err.code, "NoProbesLoaded");

    // 3. Probe Scan Math via real V24 scanMgr.RequestScans(probes_dict)
    let (wh_sig_id, wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&30_000_142]
            .iter()
            .find(|s| s.sig_group == "Wormhole")
            .unwrap();
        (sig.signature_id.clone(), sig.position)
    });

    let center = DVec3::new(wh_pos[0], wh_pos[1], wh_pos[2]);
    let scan_radius = 0.25 * AU;
    let d = scan_radius * 0.4;
    let offsets = [
        DVec3::ZERO,
        DVec3::new(d, 0.0, 0.0),
        DVec3::new(-d, 0.0, 0.0),
        DVec3::new(0.0, d, 0.0),
        DVec3::new(0.0, -d, 0.0),
        DVec3::new(0.0, 0.0, d),
        DVec3::new(0.0, 0.0, -d),
        DVec3::new(d * 0.5, d * 0.5, d * 0.5),
    ];
    let probes_dict = PyRep::Dict(
        launched_probe_ids
            .iter()
            .enumerate()
            .map(|(idx, &pid)| {
                let p = center + offsets[idx];
                (
                    PyRep::Integer(pid),
                    PyRep::keyval([
                        (
                            "pos",
                            PyRep::Tuple(vec![
                                PyRep::Real(p.x),
                                PyRep::Real(p.y),
                                PyRep::Real(p.z),
                            ]),
                        ),
                        ("scanRange", PyRep::Real(scan_radius)),
                    ]),
                )
            })
            .collect(),
    );

    for _ in 0..4 {
        ctx.dispatch_rpc(
            &mut sess,
            "scanMgr",
            Some(scan_bound_id),
            "RequestScans",
            &[probes_dict.clone()],
            &kw,
            None,
        )
        .await
        .expect("RequestScans Pinpoint");
    }

    ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&30_000_142]
            .iter()
            .find(|s| s.signature_id == wh_sig_id)
            .unwrap();
        assert_eq!(sig.signal_strength, 1.0, "Wormhole must reach 100% resolution");
    });

    let scan_notifs = ctx.drain_notifications_for_char(char_id);
    assert!(
        scan_notifs.iter().any(|n| n.event == "OnSystemScanStarted"),
        "RequestScans must emit OnSystemScanStarted"
    );
    assert!(
        scan_notifs.iter().any(|n| n.event == "OnSystemScanStopped"),
        "RequestScans must emit OnSystemScanStopped"
    );

    // 4. Probe Recall, Reconnect, and Destroy via real V24 scanMgr RPCs
    // Mark first probe inactive (state = 0) and test ReconnectToLostProbes
    {
        let mut pmap = ctx.active_scanner_probes.write().unwrap();
        if let Some(p) = pmap.get_mut(&launched_probe_ids[0]) {
            p.state = 0;
        }
    }
    let recon_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            Some(scan_bound_id),
            "ReconnectToLostProbes",
            &[],
            &kw,
            None,
        )
        .await
        .expect("ReconnectToLostProbes");
    let PyRep::KeyVal(recon_kv) = &recon_rep else { panic!() };
    let PyRep::List(recon_list) = &recon_kv["reconnectedProbeIDs"] else { panic!() };
    assert!(recon_list.contains(&PyRep::Integer(launched_probe_ids[0])));

    // Destroy one probe via DestroyProbe
    ctx.dispatch_rpc(
        &mut sess,
        "scanMgr",
        Some(scan_bound_id),
        "DestroyProbe",
        &[PyRep::Integer(launched_probe_ids[7])],
        &kw,
        None,
    )
    .await
    .expect("DestroyProbe");

    // Recover remaining 7 probes back to ship (launcher / cargo) via RecoverProbes
    let recover_ids: Vec<PyRep> = launched_probe_ids[0..7]
        .iter()
        .map(|&id| PyRep::Integer(id))
        .collect();
    let recover_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            Some(scan_bound_id),
            "RecoverProbes",
            &[PyRep::List(recover_ids)],
            &kw,
            None,
        )
        .await
        .expect("RecoverProbes");
    let PyRep::KeyVal(rec_kv) = &recover_rep else { panic!() };
    let PyRep::List(recovered_list) = &rec_kv["recoveredProbeIDs"] else { panic!() };
    assert_eq!(recovered_list.len(), 7, "Must recover all 7 remaining active probes");
    assert_eq!(rec_kv["recoveredCount"], PyRep::Integer(7));

    // Verify ship (launcher + cargo hold) now has 8 (initial remaining) + 7 (recovered) = 15 probes
    let ship_probes: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|i| i.location_id == ship_id && i.type_id == 30013)
            .map(|i| i.quantity)
            .sum()
    });
    assert_eq!(ship_probes, 15, "Ship must contain 15 Core Scanner Probes after recovering 7");

    // 5. Acceleration Gate via real V24 keeper.ActivateAccelerationGate with SDE dungeon ship restrictions and gate keys
    let gate_ball = Ball {
        id: 500_000_100,
        type_id: 17831, // SDE Acceleration Gate (group 366)
        position: DVec3::new(1_000.0, 0.0, 0.0), // 1,000m from ship (within 2,500m)
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 0.0,
        mass: 100_000_000.0,
        radius: 200.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(gate_ball).await;

    // Configure gate item record linked to SDE dungeon 111 ("Sansha Military Outpost", difficulty 1 -> Frigate/Destroyer only)
    // and requiring 1x SDE Gate Key (type_id 2076, group 474), consumed on activation
    ctx.db.with_mut_snapshot(|snap| {
        let mut gate_attrs = HashMap::new();
        gate_attrs.insert(1001, 111.0); // dungeonID = 111 (difficulty 1)
        gate_attrs.insert(1002, 2076.0); // requiredKeyTypeID = 2076 ("Gate Key")
        gate_attrs.insert(1003, 1.0); // consumeKey = true
        snap.items.insert(
            500_000_100,
            eve_db::InventoryItemRecord {
                item_id: 500_000_100,
                type_id: 17831,
                owner_id: 1,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: gate_attrs,
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    // Cruiser (Vexor, type 626, group 26) must be rejected by difficulty-1 dungeon gate (ShipRestrictedByDungeonGate)
    let cruiser_ball = Ball {
        id: 500_000_150,
        type_id: 626, // Vexor Cruiser (group 26)
        position: DVec3::new(1_200.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 210.0,
        inertia: 4.5,
        mass: 11_000_000.0,
        radius: 130.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(cruiser_ball).await;
    sess.ship_id = Some(500_000_150);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = 500_000_150;
    });
    let cruiser_err = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(500_000_100)],
            &kw,
            None,
        )
        .await
        .expect_err("Cruiser must be rejected by difficulty-1 Frigate/Destroyer dungeon gate");
    assert_eq!(cruiser_err.code, "ShipRestrictedByDungeonGate");

    // Capital ship (Avatar Titan, type 11567, group 30) must be rejected with ShipTooLargeForGate
    let titan_ball = Ball {
        id: 500_000_200,
        type_id: 11567, // Avatar Titan (group 30)
        position: DVec3::new(1_500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 70.0,
        inertia: 0.05,
        mass: 2_400_000_000.0,
        radius: 7_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(titan_ball).await;
    sess.ship_id = Some(500_000_200);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = 500_000_200;
    });
    let capital_err = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(500_000_100)],
            &kw,
            None,
        )
        .await
        .expect_err("Titan must be rejected by acceleration gate");
    assert_eq!(capital_err.code, "ShipTooLargeForGate");

    // Switch back to Rifter Frigate (ship_id, type 587, group 25): allowed by dungeon difficulty 1,
    // but rejected with MissingGateKey before placing Gate Key (2076) in cargo
    let _ = sys.remove_ball(500_000_150).await;
    let _ = sys.remove_ball(500_000_200).await;
    sess.ship_id = Some(ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().active_ship_id = ship_id;
    });
    let no_key_err = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(500_000_100)],
            &kw,
            None,
        )
        .await
        .expect_err("Activating locked acceleration gate without Gate Key in cargo must fail");
    assert_eq!(no_key_err.code, "MissingGateKey");

    // Place 1x SDE Gate Key (type 2076) in Rifter cargo and activate gate via keeper.ActivateAccelerationGate
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            550_099,
            eve_db::InventoryItemRecord {
                item_id: 550_099,
                type_id: 2076,
                owner_id: char_id,
                location_id: ship_id,
                flag: eve_db::InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    let gate_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "keeper",
            None,
            "ActivateAccelerationGate",
            &[PyRep::Integer(500_000_100)],
            &kw,
            None,
        )
        .await
        .expect("keeper.ActivateAccelerationGate with Frigate and Gate Key");
    let PyRep::KeyVal(gate_kv) = &gate_rep else { panic!("Expected KeyVal from ActivateAccelerationGate") };
    assert_eq!(gate_kv["dungeonID"], PyRep::Integer(111));
    assert_eq!(gate_kv["requiredKeyTypeID"], PyRep::Integer(2076));
    assert_eq!(gate_kv["keyConsumed"], PyRep::Bool(true));

    // Verify Gate Key was consumed from cargo and ship entered warp to next pocket
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&550_099), "Gate Key must be consumed from cargo upon activation");
    });
    let (_, ship_warping) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(matches!(ship_warping.mode, MovementMode::Warp { .. }), "Ship must enter Warp on acceleration gate activation");
    for _ in 0..30 {
        let _ = sys.step_manual(1.0).await;
    }
    let (_, ship_landed) = sys.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    assert!(
        ship_landed.position.distance(DVec3::new(500_000.0, 0.0, 0.0)) < 2_500.0,
        "Ship must land at acceleration gate pocket destination, got {:?}",
        ship_landed.position
    );
}

#[tokio::test]
// parity: plan:SO-6
// parity: feat:navigation.wormhole-jump, feat:navigation.wormhole-polarization, feat:navigation.wormhole-lifetime, feat:navigation.wormhole-statics, feat:navigation.wormhole-thera
async fn test_so6_wormholes_polarization_lifetime_mass_and_thera() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut sess = SessionState::new(601);
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
    .expect("Select Pilot 1");

    // Initialize system cosmic signatures and wormhole balls via real V24 scanMgr.GetSystemScanMgr
    let _ = ctx
        .dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None)
        .await
        .expect("GetSystemScanMgr");

    let (wh_sig_id, wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&30_000_142]
            .iter()
            .find(|s| s.sig_group == "Wormhole")
            .unwrap();
        (sig.signature_id.clone(), sig.position)
    });

    // Resolve wormhole item ID in Jita Ballpark (group_id = 988)
    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    let wh_item_id =
        eve_server::services::space_destiny::wormhole_item_id_for_signature(30_000_142, &wh_sig_id);
    let (_, wh_ball) = jita_sys
        .get_ball_snapshot(wh_item_id)
        .await
        .unwrap()
        .expect("Wormhole ball must be spawned in Jita Ballpark");
    assert_eq!(wh_ball.type_id, 26272);

    // Position pilot's Rifter (600_000_001) within 2,500m of the wormhole ball
    let ship_ball = Ball {
        id: 600_000_001,
        type_id: 587,
        position: DVec3::new(wh_pos[0] + 500.0, wh_pos[1], wh_pos[2]),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_200_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = jita_sys.spawn_ball(ship_ball.clone()).await;
    sess.ship_id = Some(600_000_001);
    sess.solar_system_id = Some(30_000_142);
    ctx.db
        .try_with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&90_000_001).unwrap();
            ch.active_ship_id = 600_000_001;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();

    // 1. Max jump mass enforcement: ship exceeding max_jump_mass_kg (375,000,000 kg) is rejected
    let heavy_ball = Ball {
        mass: 1_500_000_000.0,
        ..ship_ball.clone()
    };
    let _ = jita_sys.spawn_ball(heavy_ball).await;
    let heavy_err = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(wh_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Ship exceeding max_jump_mass_kg must be rejected");
    assert_eq!(heavy_err.code, "ShipTooHeavyForWormhole");

    // Restore normal Rifter mass (1,200,000 kg)
    let _ = jita_sys.spawn_ball(ship_ball.clone()).await;

    // 2. Jump Wormhole via real V24 wormholeMgr.WormholeJump(wormholeItemID) -> transits to Thera (31000005)
    let jump_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(wh_item_id)],
            &kw,
            None,
        )
        .await
        .expect("WormholeJump to Thera");
    let PyRep::KeyVal(jkv) = &jump_rep else { panic!("Expected KeyVal from WormholeJump") };
    assert_eq!(sess.solar_system_id, Some(31_000_005));
    assert_eq!(jkv["isThera"], PyRep::Bool(true));
    assert_eq!(jkv["wormholeClass"], PyRep::Integer(12));
    assert_eq!(jkv["wormholeSize"], PyRep::Real(1.0)); // > 50% mass remaining
    assert_eq!(jkv["wormholeAge"], PyRep::Integer(1));  // >= 4h remaining
    assert!(
        (jkv["remainingMassKg"].as_f64().unwrap() - (2_000_000_000.0 - 1_200_000.0)).abs() < 1.0,
        "WormholeJump must subtract jumping ship mass from remaining_mass_kg"
    );

    // 3. Thera rules: anchoring structures or POS towers in Thera (31000005) is disallowed
    let thera_anchor_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "Anchor",
            &[PyRep::Integer(35832), PyRep::String("Thera Astrahus".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Anchoring structures in Thera must be rejected");
    assert_eq!(thera_anchor_err.code, "AnchoringDisallowedInTheraOrShattered");

    // 4. Polarization Check via real V24 wormholeMgr.GetWormholePolarization
    let pol_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "GetWormholePolarization",
            &[PyRep::Integer(31_000_005)],
            &kw,
            None,
        )
        .await
        .expect("GetWormholePolarization");
    if let PyRep::KeyVal(entries) = &pol_rep {
        let is_polarized = entries
            .iter()
            .find(|(k, _)| k.as_str() == "isPolarized")
            .unwrap()
            .1
            .as_bool()
            .unwrap();
        assert!(is_polarized, "Pilot must be polarized after jumping wormhole");
    }

    // Attempting immediate jump on same wormhole while polarized is rejected with WormholePolarized
    ctx.db
        .try_with_mut_snapshot(|snap| {
            snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 30_000_142;
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();
    let _ = jita_sys.spawn_ball(ship_ball.clone()).await;
    sess.solar_system_id = Some(30_000_142);
    let pol_err = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(wh_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Polarized pilot cannot jump same wormhole");
    assert_eq!(pol_err.code, "WormholePolarized");

    // Advance virtual clock 301s past 5-minute polarization window
    ctx.advance_secs(301);

    // 5. Mass exhaustion collapse & automatic static wormhole respawn:
    // Set remaining_mass_kg to 800,000 kg (< 1,200,000 kg Rifter mass) and destination to Pulsar system (31000004)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sigs) = snap.cosmic_signatures.get_mut(&30_000_142) {
            if let Some(wh) = sigs.iter_mut().find(|s| s.signature_id == wh_sig_id) {
                wh.remaining_mass_kg = 800_000.0;
                wh.destination_system_id = Some(31_000_004);
            }
        }
    });

    let collapse_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(wh_item_id)],
            &kw,
            None,
        )
        .await
        .expect("WormholeJump collapsing mass");
    let PyRep::KeyVal(ckv) = &collapse_jump else { panic!() };
    assert_eq!(ckv["collapsed"], PyRep::Bool(true), "Wormhole must collapse when remaining_mass_kg <= 0");
    assert_eq!(ckv["environmentalEffect"], PyRep::String("Magnetar".into()));
    let replacement_sig_id = ckv["replacementSignatureID"]
        .as_str()
        .expect("Collapsed static wormhole must spawn a replacement static signature")
        .to_string();
    assert_ne!(replacement_sig_id, wh_sig_id);

    // Verify old wormhole ball was removed from Jita and replacement static signature exists
    assert!(
        jita_sys.get_ball_snapshot(wh_item_id).await.unwrap().is_none(),
        "Collapsed wormhole ball must be removed from Ballpark"
    );

    // 6. Lifetime expiration collapse & static respawn:
    // Expire the replacement wormhole's lifetime_seconds = 0 and verify WormholeJump returns WormholeCollapsed and spawns a new static
    let rep_wh_item_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(
        30_000_142,
        &replacement_sig_id,
    );
    let rep_pos = ctx.db.with_mut_snapshot(|snap| {
        let sig = snap
            .cosmic_signatures
            .get_mut(&30_000_142)
            .unwrap()
            .iter_mut()
            .find(|s| s.signature_id == replacement_sig_id)
            .unwrap();
        sig.lifetime_seconds = 0;
        sig.position
    });
    ctx.advance_secs(301);
    ctx.db
        .try_with_mut_snapshot(|snap| {
            snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 30_000_142;
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();
    sess.solar_system_id = Some(30_000_142);
    let _ = jita_sys
        .spawn_ball(Ball {
            position: DVec3::new(rep_pos[0] + 200.0, rep_pos[1], rep_pos[2]),
            ..ship_ball
        })
        .await;

    let expired_err = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(rep_wh_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Expired wormhole must collapse on jump attempt");
    assert_eq!(expired_err.code, "WormholeCollapsed");
}

#[tokio::test]
// parity: plan:SO-7
// parity: feat:social.chat-local, feat:social.chat-corp-alliance-fleet, feat:social.chat-player-channels, feat:social.chat-private
async fn test_so7_chat_channels_local_corp_fleet_player_and_private() {
    let ctx = ServerContext::new();
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
        assert!(wh_chan.is_delayed_local, "W-space local must be marked delayed local");
    });

    // 2. Corp, Alliance & Fleet chat channels
    let corp_chan = "corpid:1000001";
    let join_corp1 = ctx
        .dispatch_rpc(&mut sess1, "lscService", None, "JoinChannel", &[PyRep::String(corp_chan.into())], &kw, None)
        .await
        .expect("Join Corp channel P1");
    assert!(is_keyval(&join_corp1));

    let join_corp2 = ctx
        .dispatch_rpc(&mut sess2, "lscService", None, "JoinChannel", &[PyRep::String(corp_chan.into())], &kw, None)
        .await
        .expect("Join Corp channel P2");
    assert!(is_keyval(&join_corp2));

    let send_corp = ctx
        .dispatch_rpc(
            &mut sess1,
            "lscService",
            None,
            "SendMessage",
            &[PyRep::String(corp_chan.into()), PyRep::String("All hands deploy to POS".into())],
            &kw,
            None,
        )
        .await
        .expect("SendMessage Corp");
    assert!(is_keyval(&send_corp));

    let p2_chat = ctx.drain_notifications_for_char(90_000_002);
    assert!(p2_chat.iter().any(|n| n.event == "OnLSC"), "Pilot 2 should receive OnLSC for corp chat");

    // 3. Player Created Custom Channels
    let create_chan = ctx
        .dispatch_rpc(
            &mut sess1,
            "chatMgr",
            None,
            "CreateChannel",
            &[PyRep::String("WormholeHunters".into()), PyRep::String("Class 5 capital escalations".into())],
            &kw,
            None,
        )
        .await
        .expect("CreateChannel");
    assert!(is_keyval(&create_chan));
    let custom_chan_id = match &create_chan {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == "channelID").and_then(|(_, v)| v.as_str()).unwrap().to_string(),
        _ => "channel:wormholehunters".to_string(),
    };

    // Grant ownership / operator to Pilot 2
    let grant_op = ctx
        .dispatch_rpc(
            &mut sess1,
            "chatMgr",
            None,
            "GrantChannelOwnership",
            &[PyRep::String(custom_chan_id), PyRep::Integer(90_000_002)],
            &kw,
            None,
        )
        .await
        .expect("GrantChannelOwnership");
    assert!(is_keyval(&grant_op));

    // 4. Private 1-on-1 Conversation
    let priv_convo = ctx
        .dispatch_rpc(
            &mut sess1,
            "lscService",
            None,
            "StartConversation",
            &[PyRep::Integer(90_000_002), PyRep::String("Cynosaural field is up".into())],
            &kw,
            None,
        )
        .await
        .expect("StartConversation");
    assert!(is_keyval(&priv_convo));

    let p2_priv = ctx.drain_notifications_for_char(90_000_002);
    assert!(p2_priv.iter().any(|n| n.event == "OnLSC"), "Pilot 2 should receive OnLSC for private conversation");
}
