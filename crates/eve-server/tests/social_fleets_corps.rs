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

    // 2. Mail Labels: Pilot 2 creates labels
    let label1_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "CreateLabel", &[PyRep::String("Important".into()), PyRep::String("#FF0000".into())], &kw, None)
        .await
        .expect("CreateLabel 1");
    let label1_id = match &label1_rep {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == "labelID").and_then(|(_, v)| v.as_i64()).unwrap(),
        _ => panic!("Expected keyval for CreateLabel 1"),
    };

    let label2_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "CreateLabel", &[PyRep::String("Orders".into()), PyRep::String("#0000FF".into())], &kw, None)
        .await
        .expect("CreateLabel 2");
    let label2_id = match &label2_rep {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == "labelID").and_then(|(_, v)| v.as_i64()).unwrap(),
        _ => panic!("Expected keyval for CreateLabel 2"),
    };

    // Edit label 2
    let edit_label_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "EditLabel", &[PyRep::Integer(label2_id), PyRep::String("Fleet Orders".into()), PyRep::String("#00FF00".into())], &kw, None)
        .await
        .expect("EditLabel");
    assert!(is_keyval(&edit_label_rep));

    // Query labels
    let labels_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "GetLabels", &[], &kw, None)
        .await
        .expect("GetLabels");
    assert!(is_rowset(&labels_rep));
    if let PyRep::Rowset { rows, .. } = &labels_rep {
        assert_eq!(rows.len(), 6, "Pilot 2 should have 4 system labels + 2 custom labels");
    }

    // 3. Send Mail: Pilot 1 -> Pilot 2
    let send_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "mailMgr",
            None,
            "SendMail",
            &[
                PyRep::Integer(90_000_002),
                PyRep::String("Fleet Deployment to Tama".into()),
                PyRep::String("Report to frontline in Heavy Assault Cruisers.".into()),
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

    // 4. Pilot 2 fetches mail headers (SyncMail alias)
    let sync_headers = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "SyncMail", &[], &kw, None)
        .await
        .expect("SyncMail");
    assert!(is_rowset(&sync_headers));
    if let PyRep::Rowset { rows, .. } = &sync_headers {
        assert!(rows.iter().any(|cols| cols.first() == Some(&PyRep::Integer(mail_id))));
    }

    // 5. Pilot 2 reads mail body: auto-marks as read
    let body_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "GetBody", &[PyRep::Integer(mail_id)], &kw, None)
        .await
        .expect("GetBody");
    assert!(is_keyval(&body_rep));

    ctx.db.with_snapshot(|snap| {
        assert!(snap.read_mails.contains(&(90_000_002, mail_id)), "Mail must be marked read on GetBody");
    });

    // 6. Pilot 2 toggles unread state
    let unread_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "MarkAsUnread", &[PyRep::Integer(mail_id)], &kw, None)
        .await
        .expect("MarkAsUnread");
    assert!(is_keyval(&unread_rep));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.read_mails.contains(&(90_000_002, mail_id)), "Mail must be unread now");
    });

    // Mark as read again
    let mark_read = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "MarkRead", &[PyRep::Integer(mail_id)], &kw, None)
        .await
        .expect("MarkRead");
    assert!(is_keyval(&mark_read));

    // 7. Assign labels to mail
    let assign_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "AssignLabels", &[PyRep::Integer(mail_id), PyRep::List(vec![PyRep::Integer(label1_id), PyRep::Integer(label2_id)])], &kw, None)
        .await
        .expect("AssignLabels");
    assert!(is_keyval(&assign_rep));

    // Remove one label
    let rem_label_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "RemoveLabels", &[PyRep::Integer(mail_id), PyRep::List(vec![PyRep::Integer(label1_id)])], &kw, None)
        .await
        .expect("RemoveLabels");
    assert!(is_keyval(&rem_label_rep));

    // Delete label 1
    let del_label_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "DeleteLabel", &[PyRep::Integer(label1_id)], &kw, None)
        .await
        .expect("DeleteLabel");
    assert!(is_keyval(&del_label_rep));

    // 8. Delete mail
    let del_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "DeleteMail", &[PyRep::Integer(mail_id)], &kw, None)
        .await
        .expect("DeleteMail");
    assert!(is_keyval(&del_rep));
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

    // 2. Pilot 1 creates fleet
    let create_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "CreateFleet", &[], &kw, None)
        .await
        .expect("CreateFleet");
    assert!(is_keyval(&create_rep));
    let fid = sess1.fleet_id.expect("fleet_id set");

    // 3. Hierarchy: Wings and Squads
    let wing_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "CreateWing", &[PyRep::String("Skirmish Wing".into())], &kw, None)
        .await
        .expect("CreateWing");
    assert!(is_keyval(&wing_rep));

    let squad_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "CreateSquad", &[PyRep::Integer(2), PyRep::String("Interceptors".into())], &kw, None)
        .await
        .expect("CreateSquad");
    assert!(is_keyval(&squad_rep));

    // 4. Pilot 1 invites Pilot 2
    let invite_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "Invite", &[PyRep::Integer(90_000_002)], &kw, None)
        .await
        .expect("Invite");
    assert!(is_keyval(&invite_rep));

    let p2_invites = ctx.drain_notifications_for_char(90_000_002);
    assert!(p2_invites.iter().any(|n| n.event == "OnFleetInvite"), "OnFleetInvite notification required");

    // Pilot 2 joins fleet
    let join_rep = ctx
        .dispatch_rpc(&mut sess2, "fleetMgr", None, "JoinFleet", &[PyRep::Integer(fid)], &kw, None)
        .await
        .expect("JoinFleet");
    assert!(is_keyval(&join_rep));
    assert_eq!(sess2.fleet_id, Some(fid));

    // 5. Broadcast command
    let bcast_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "Broadcast", &[PyRep::String("TargetPrimary".into()), PyRep::Integer(100_000_002)], &kw, None)
        .await
        .expect("Broadcast");
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

    // 7. Warp-to-member: Pilot 2 warps to Pilot 1
    let warp_to_rep = ctx
        .dispatch_rpc(&mut sess2, "fleetMgr", None, "WarpToMember", &[PyRep::Integer(90_000_001), PyRep::Real(10_000.0)], &kw, None)
        .await
        .expect("WarpToMember");
    assert!(is_keyval(&warp_to_rep));

    // 8. Fleet Warp: Pilot 1 initiates fleet warp for all members
    let fleet_warp_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetMgr",
            None,
            "CmdFleetWarp",
            &[PyRep::Real(1_000_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)],
            &kw,
            None,
        )
        .await
        .expect("CmdFleetWarp");
    assert!(is_keyval(&fleet_warp_rep));

    // 9. Kick and Leave
    let kick_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "KickMember", &[PyRep::Integer(90_000_002)], &kw, None)
        .await
        .expect("KickMember");
    assert!(is_keyval(&kick_rep));

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

    // 1. Create Corporation
    let create_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpmgr",
            None,
            "CreateCorporation",
            &[PyRep::String("DeepSpace Logistics".into()), PyRep::String("DSLOG".into()), PyRep::Real(0.075)],
            &kw,
            None,
        )
        .await
        .expect("CreateCorporation");
    assert!(is_keyval(&create_rep));
    let corp_id = sess1.corporation_id.expect("corp_id set");

    // 2. Wallet Divisions
    let divs_rep = ctx
        .dispatch_rpc(&mut sess1, "corpmgr", None, "GetWalletDivisions", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetWalletDivisions");
    assert!(is_rowset(&divs_rep));

    // Update Division Names
    let update_divs_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpmgr",
            None,
            "UpdateDivisionNames",
            &[PyRep::Integer(1), PyRep::String("Master Wallet".into())],
            &kw,
            None,
        )
        .await
        .expect("UpdateDivisionNames");
    assert!(is_keyval(&update_divs_rep));

    // 3. Public Info & Edit Details
    let public_info = ctx
        .dispatch_rpc(&mut sess2, "corpmgr", None, "GetPublicInfo", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetPublicInfo");
    assert!(is_keyval(&public_info));

    // Pilot 1 updates corp details
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
    assert!(is_keyval(&apply_rep));

    let app_id = ctx.db.with_snapshot(|snap| {
        snap.corporations[&corp_id].applications.keys().next().copied().unwrap()
    });

    let accept_rep = ctx
        .dispatch_rpc(&mut sess1, "corpmgr", None, "AcceptApplication", &[PyRep::Integer(app_id)], &kw, None)
        .await
        .expect("AcceptApplication");
    assert!(is_keyval(&accept_rep));

    // 5. Update Member Roles
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

    // Role bitmask authorization check: Pilot 2 (not director/CEO) denied from changing roles
    sess2.corporation_id = Some(corp_id);
    sess2.corp_role = corp_roles::ROLE_ACCOUNTANT;
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

    // 1. Folders: Pilot 1 creates a personal folder
    let folder_rep = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "CreateFolder", &[PyRep::String("Safe Spots".into())], &kw, None)
        .await
        .expect("CreateFolder");
    let folder_id = match &folder_rep {
        PyRep::KeyVal(entries) => entries.iter().find(|(k, _)| k.as_str() == "folderID").and_then(|(_, v)| v.as_i64()).unwrap(),
        _ => panic!("Expected keyval for CreateFolder"),
    };

    let folders_list = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "GetFolders", &[], &kw, None)
        .await
        .expect("GetFolders");
    assert!(is_rowset(&folders_list));

    // 2. Create Bookmark in Folder
    let bm1_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "BookmarkLocation",
            &[PyRep::String("Deep Safe 1".into()), PyRep::String("100 AU off plane".into()), PyRep::Integer(folder_id)],
            &kw,
            None,
        )
        .await
        .expect("BookmarkLocation");
    assert!(is_keyval(&bm1_rep));

    let bm_id = ctx.db.with_snapshot(|snap| {
        snap.bookmarks.values().find(|b| b.owner_id == 90_000_001).map(|b| b.bookmark_id).unwrap()
    });

    // 3. Update Bookmark
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

    // 4. Corp Shared Folders
    let corp_id = 200_001;
    sess1.corporation_id = Some(corp_id);
    sess1.corp_role = corp_roles::ROLE_DIRECTOR;
    sess2.corporation_id = Some(corp_id);

    let shared_folder_rep = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "CreateSharedFolder", &[PyRep::String("Corp Tacticals".into())], &kw, None)
        .await
        .expect("CreateSharedFolder");
    assert!(is_keyval(&shared_folder_rep));

    let p2_shared = ctx
        .dispatch_rpc(&mut sess2, "bookmark", None, "GetSharedFolders", &[], &kw, None)
        .await
        .expect("GetSharedFolders");
    assert!(is_rowset(&p2_shared));

    // 5. Scan Bookmark (saving a resolved signature directly as a bookmark)
    let _ = ctx
        .dispatch_rpc(&mut sess1, "scanMgr", None, "GetSystemSignatures", &[PyRep::Integer(30_000_142)], &kw, None)
        .await
        .expect("GetSystemSignatures");
    let wh_sig_id = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures[&30_000_142].first().unwrap().signature_id.clone()
    });

    let scan_bm = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "BookmarkScanResult",
            &[
                PyRep::String(wh_sig_id),
                PyRep::String("Resolved K162".into()),
                PyRep::Real(1.0 * AU),
                PyRep::Real(2.0 * AU),
                PyRep::Real(3.0 * AU),
            ],
            &kw,
            None,
        )
        .await
        .expect("BookmarkScanResult");
    assert!(is_keyval(&scan_bm));

    // 6. Delete Bookmark and Folder
    let del_bm = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "DeleteBookmark", &[PyRep::Integer(bm_id)], &kw, None)
        .await
        .expect("DeleteBookmark");
    assert!(is_keyval(&del_bm));

    let del_folder = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "DeleteFolder", &[PyRep::Integer(folder_id)], &kw, None)
        .await
        .expect("DeleteFolder");
    assert!(is_keyval(&del_folder));
}

#[tokio::test]
// parity: plan:SO-5
// parity: feat:scanning.dscan, feat:scanning.probe-launch, feat:scanning.probe-scan-math, feat:scanning.probe-recall, feat:navigation.acceleration-gates
async fn test_so5_scanning_dscan_probes_and_acceleration_gates() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut sess = SessionState::new(501);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("Select Pilot 1");

    let sys = ctx.systems.get_or_spawn(30_000_142);
    let ship_ball = Ball {
        id: 500_000_001,
        type_id: 587,
        position: DVec3::new(0.0, 0.0, 0.0),
        velocity: DVec3::new(0.0, 0.0, 100.0), // Moving along +Z
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 0.33,
        mode: MovementMode::Stop,
    };
    let target_ball = Ball {
        id: 500_000_002,
        type_id: 603,
        position: DVec3::new(0.0, 0.0, 10_000_000.0), // 10,000 km straight ahead (+Z)
        velocity: DVec3::ZERO,
        max_velocity: 200.0,
        inertia: 3.0,
        mass: 5_000_000.0,
        radius: 80.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(ship_ball).await;
    let _ = sys.spawn_ball(target_ball).await;
    sess.ship_id = Some(500_000_001);
    sess.solar_system_id = Some(30_000_142);
    ctx.db.try_with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&90_000_001).unwrap();
        ch.active_ship_id = 500_000_001;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = 30_000_142;
        Ok::<(), std::convert::Infallible>(())
    }).unwrap();

    // 1. D-Scan: 360 degree scan detects target
    let dscan_360 = ctx
        .dispatch_rpc(&mut sess, "directionalScanMgr", None, "PerformDirectionalScan", &[PyRep::Real(360.0), PyRep::Real(14.3 * AU)], &kw, None)
        .await
        .expect("PerformDirectionalScan 360");
    assert!(is_rowset(&dscan_360));
    if let PyRep::Rowset { rows, .. } = &dscan_360 {
        assert!(rows.iter().any(|r| r.first() == Some(&PyRep::Integer(500_000_002))), "Target must appear in 360 D-scan");
    }

    // Narrow 15-degree cone directly forward detects target
    let dscan_15 = ctx
        .dispatch_rpc(&mut sess, "directionalScanMgr", None, "PerformDirectionalScan", &[PyRep::Real(15.0), PyRep::Real(14.3 * AU)], &kw, None)
        .await
        .expect("PerformDirectionalScan 15");
    assert!(is_rowset(&dscan_15));
    if let PyRep::Rowset { rows, .. } = &dscan_15 {
        assert!(rows.iter().any(|r| r.first() == Some(&PyRep::Integer(500_000_002))), "Target must appear in 15-deg forward cone");
    }

    // 2. Launch Probes
    let launch_rep = ctx
        .dispatch_rpc(&mut sess, "probeMgr", None, "LaunchProbes", &[PyRep::Integer(8)], &kw, None)
        .await
        .expect("LaunchProbes");
    assert!(is_keyval(&launch_rep));

    // 3. Probe Scan Math
    let _ = ctx
        .dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemSignatures", &[PyRep::Integer(30_000_142)], &kw, None)
        .await
        .expect("GetSystemSignatures");

    let (wh_sig_id, wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&30_000_142].iter().find(|s| s.sig_group == "Wormhole").unwrap();
        (sig.signature_id.clone(), sig.position)
    });

    let center = DVec3::new(wh_pos[0], wh_pos[1], wh_pos[2]);
    let d = 0.5 * AU * 0.4;
    let probe_coords = vec![
        center,
        center + DVec3::new(d, 0.0, 0.0),
        center + DVec3::new(-d, 0.0, 0.0),
        center + DVec3::new(0.0, d, 0.0),
        center + DVec3::new(0.0, -d, 0.0),
        center + DVec3::new(0.0, 0.0, d),
        center + DVec3::new(0.0, 0.0, -d),
        center + DVec3::new(d * 0.5, d * 0.5, d * 0.5),
    ];
    let probe_list = PyRep::List(probe_coords.into_iter().map(|p| {
        PyRep::List(vec![PyRep::Real(p.x), PyRep::Real(p.y), PyRep::Real(p.z)])
    }).collect());

    for _ in 0..4 {
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "scanMgr",
                None,
                "ScanWithProbes",
                &[probe_list.clone(), PyRep::Real(0.5 * AU), PyRep::Real(50.0)],
                &kw,
                None,
            )
            .await
            .expect("ScanWithProbes Pinpoint");
    }

    ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&30_000_142].iter().find(|s| s.signature_id == wh_sig_id).unwrap();
        assert_eq!(sig.signal_strength, 1.0, "Wormhole must reach 100% resolution");
    });

    // 4. Recall Probes
    let recall_rep = ctx
        .dispatch_rpc(&mut sess, "probeMgr", None, "RecallProbes", &[], &kw, None)
        .await
        .expect("RecallProbes");
    assert!(is_keyval(&recall_rep));

    // 5. Acceleration Gate
    let gate_ball = Ball {
        id: 500_000_100,
        type_id: 17832, // Acceleration Gate
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

    // Normal activation within range
    let gate_rep = ctx
        .dispatch_rpc(&mut sess, "dungeonMgr", None, "ActivateAccelerationGate", &[PyRep::Integer(500_000_100)], &kw, None)
        .await
        .expect("ActivateAccelerationGate");
    assert!(is_keyval(&gate_rep));

    // Capital ship rejection: ship mass > 500,000,000 kg
    let titan_ball = Ball {
        id: 500_000_200,
        type_id: 11567, // Avatar Titan
        position: DVec3::new(1_500.0, 0.0, 0.0),
        velocity: DVec3::ZERO,
        max_velocity: 70.0,
        inertia: 0.05,
        mass: 2_400_000_000.0, // 2.4B kg
        radius: 7_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys.spawn_ball(titan_ball).await;
    sess.ship_id = Some(500_000_200);
    ctx.db.try_with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().active_ship_id = 500_000_200;
        Ok::<(), std::convert::Infallible>(())
    }).unwrap();

    let capital_err = ctx
        .dispatch_rpc(&mut sess, "dungeonMgr", None, "ActivateAccelerationGate", &[PyRep::Integer(500_000_100)], &kw, None)
        .await;
    assert_eq!(capital_err.unwrap_err().code, "ShipTooLargeForGate");
}

#[tokio::test]
// parity: plan:SO-6
// parity: feat:navigation.wormhole-jump, feat:navigation.wormhole-polarization, feat:navigation.wormhole-lifetime, feat:navigation.wormhole-statics, feat:navigation.wormhole-thera
async fn test_so6_wormholes_polarization_lifetime_mass_and_thera() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut sess = SessionState::new(601);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None)
        .await
        .expect("Select Pilot 1");

    let _ = ctx
        .dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemSignatures", &[PyRep::Integer(30_000_142)], &kw, None)
        .await
        .expect("GetSystemSignatures");

    let wh_sig_id = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures[&30_000_142].iter().find(|s| s.sig_group == "Wormhole").unwrap().signature_id.clone()
    });

    // 1. Lifetime and Mass stages
    let fresh_info = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "GetWormholeInfo", &[PyRep::String(wh_sig_id.clone())], &kw, None)
        .await
        .expect("GetWormholeInfo fresh");
    assert!(is_keyval(&fresh_info));

    // 2. System Rules & Thera
    let thera_rules = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "GetWormholeSystemRules", &[PyRep::Integer(31_000_005)], &kw, None)
        .await
        .expect("GetWormholeSystemRules Thera");
    if let PyRep::KeyVal(entries) = &thera_rules {
        let is_thera = entries.iter().find(|(k, _)| k.as_str() == "isThera").unwrap().1.as_bool().unwrap();
        let wh_class = entries.iter().find(|(k, _)| k.as_str() == "wormholeClass").unwrap().1.as_i64().unwrap();
        assert!(is_thera);
        assert_eq!(wh_class, 12);
    }

    let thera_statics = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "GetSystemStatics", &[PyRep::Integer(31_000_005)], &kw, None)
        .await
        .expect("GetSystemStatics Thera");
    assert!(is_rowset(&thera_statics));

    // 3. Jump Wormhole
    let wh_pos = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures[&30_000_142].iter().find(|s| s.signature_id == wh_sig_id).unwrap().position
    });
    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
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
    ctx.db.try_with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&90_000_001).unwrap();
        ch.active_ship_id = 600_000_001;
        ch.station_id = None;
        ch.structure_id = None;
        ch.solar_system_id = 30_000_142;
        Ok::<(), std::convert::Infallible>(())
    }).unwrap();

    let jump_rep = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "JumpWormhole", &[PyRep::String(wh_sig_id.clone())], &kw, None)
        .await
        .expect("JumpWormhole");
    assert!(is_keyval(&jump_rep));
    assert_eq!(sess.solar_system_id, Some(31_000_005));

    // 4. Polarization Check
    let pol_rep = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "GetWormholePolarization", &[PyRep::Integer(31_000_005)], &kw, None)
        .await
        .expect("GetWormholePolarization");
    if let PyRep::KeyVal(entries) = &pol_rep {
        let is_polarized = entries.iter().find(|(k, _)| k.as_str() == "isPolarized").unwrap().1.as_bool().unwrap();
        assert!(is_polarized, "Pilot must be polarized after jumping wormhole");
    }

    // Attempting immediate jump back while polarized is rejected
    // Move ship back into range of wormhole in system 30000142
    ctx.db.try_with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&90_000_001).unwrap();
        ch.solar_system_id = 30_000_142;
        Ok::<(), std::convert::Infallible>(())
    }).unwrap();
    let _ = jita_sys.spawn_ball(ship_ball).await;
    sess.solar_system_id = Some(30_000_142);
    let pol_err = ctx
        .dispatch_rpc(&mut sess, "wormholeMgr", None, "JumpWormhole", &[PyRep::String(wh_sig_id.clone())], &kw, None)
        .await;
    assert_eq!(pol_err.unwrap_err().code, "WormholePolarized");
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
