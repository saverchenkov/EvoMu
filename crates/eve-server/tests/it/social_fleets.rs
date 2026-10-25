//! Phase 8 Social, Fleets, Corporations & Chat Integration Tests (`SO-1`..`SO-7`).

use crate::common;

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
async fn test_so1_fleet_creation_hierarchy_broadcast_and_position_sharing() {
    // parity: rpc:charUnboundMgr.SelectCharacterID, rpc:fleetObjectHandler.GetFleetComposition
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    // 1. Login Pilot 1 (Demo, char 90000001)
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
    .expect("SelectCharacterID Pilot 1");

    // 2. Login Pilot 2 (Rival/Other, char 90000002)
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
    .expect("SelectCharacterID Pilot 2");

    // 3. Pilot 1 creates fleet
    let create_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "CreateFleet", &[], &kw, None)
        .await
        .expect("CreateFleet");
    assert!(is_keyval(&create_rep));
    let fid = sess1.fleet_id.expect("sess1 fleet_id set");

    // 4. Pilot 1 creates Wing and Squad (Hierarchy)
    let wing_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetMgr",
            None,
            "CreateWing",
            &[PyRep::String("Alpha Wing".into())],
            &kw,
            None,
        )
        .await
        .expect("CreateWing");
    assert!(is_keyval(&wing_rep));

    let squad_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetMgr",
            None,
            "CreateSquad",
            &[PyRep::Integer(2), PyRep::String("Tackle Squad".into())],
            &kw,
            None,
        )
        .await
        .expect("CreateSquad");
    assert!(is_keyval(&squad_rep));

    // 5. Pilot 1 invites Pilot 2
    let invite_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetMgr",
            None,
            "Invite",
            &[PyRep::Integer(90_000_002)],
            &kw,
            None,
        )
        .await
        .expect("Invite");
    assert!(is_keyval(&invite_rep));

    // Verify Pilot 2 received OnFleetInvite notification
    let p2_notifs = ctx.drain_notifications_for_char(90_000_002);
    assert!(
        p2_notifs.iter().any(|n| n.event == "OnFleetInvite"),
        "Expected OnFleetInvite notification"
    );

    // 6. Pilot 2 accepts fleet invite
    ctx.advance_secs(12);
    let join_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "fleetMgr",
            None,
            "AcceptInvite",
            &[PyRep::Integer(fid)],
            &kw,
            None,
        )
        .await
        .expect("AcceptInvite");
    assert!(is_keyval(&join_rep));
    assert_eq!(sess2.fleet_id, Some(fid));

    // 7. Pilot 1 assigns Pilot 2 to Wing Commander role (role 2) in Wing 2
    let setrole_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetMgr",
            None,
            "MoveMember",
            &[
                PyRep::Integer(90_000_002),
                PyRep::Integer(2),
                PyRep::Integer(2),
                PyRep::Integer(2),
            ],
            &kw,
            None,
        )
        .await
        .expect("MoveMember");
    assert!(is_keyval(&setrole_rep));

    // 8. Query fleet state hierarchy
    let getfleet_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "GetInitState", &[], &kw, None)
        .await
        .expect("GetInitState");
    assert!(is_keyval(&getfleet_rep));

    // 9. Fleet position & composition sharing
    let pos_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetObjectHandler",
            None,
            "GetFleetComposition",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetFleetComposition");
    assert!(is_rowset(&pos_rep));

    // 10. Fleet broadcast event: Pilot 1 broadcasts "WarpTo"
    let bcast_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetMgr",
            None,
            "SendBroadcast",
            &[PyRep::String("WarpTo".into()), PyRep::Integer(60_003_760)],
            &kw,
            None,
        )
        .await
        .expect("SendBroadcast");
    assert!(is_keyval(&bcast_rep));

    // Verify broadcast event queued for fleet member Pilot 2
    let p2_bcast_notifs = ctx.drain_notifications_for_char(90_000_002);
    assert!(
        p2_bcast_notifs.iter().any(|n| n.event == "OnFleetBroadcast"),
        "Expected OnFleetBroadcast notification"
    );

    // 11. Kick member: Pilot 1 kicks Pilot 2
    let kick_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "fleetMgr",
            None,
            "KickMember",
            &[PyRep::Integer(90_000_002)],
            &kw,
            None,
        )
        .await
        .expect("KickMember");
    assert!(is_keyval(&kick_rep));

    // 12. Pilot 1 leaves fleet
    let leave_rep = ctx
        .dispatch_rpc(&mut sess1, "fleetMgr", None, "LeaveFleet", &[], &kw, None)
        .await
        .expect("LeaveFleet");
    assert!(is_keyval(&leave_rep));
    assert_eq!(sess1.fleet_id, None);
}

#[tokio::test]
async fn test_so2_corporation_roles_applications_and_wallet_divisions() {
    // parity: feat:corporation.create-corp, feat:corporation.applications, feat:corporation.roles, feat:wallet.corp-divisions, rpc:account.GetWalletDivisionsInfo, rpc:account.GiveCashFromCorpAccount, rpc:charUnboundMgr.SelectCharacterID, rpc:corpRegistry.AddCorporation, rpc:corpRegistry.GetApplications, rpc:corpRegistry.InsertApplication, rpc:corpRegistry.UpdateApplicationOffer, rpc:corpRegistry.UpdateMember, rpc:corpRegistry.UpdateDivisionNames
    eve_testkit::attest(&[
        "feat:corporation.create-corp",
        "feat:corporation.applications",
        "feat:corporation.roles",
        "feat:wallet.corp-divisions",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    // 1. Setup Pilot 1 (CEO) and Pilot 2 (Applicant -> Member)
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
    ctx.advance_secs(12);

    // 2. Pilot 1 creates corporation: verify Corporation Management (3363) skill check, 1,599,800 ISK fee,
    // duplicate name/ticker rejection, CEO assignment, and removal from old NPC corp (1000009)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(skills) = snap.skills.get_mut(&90_000_001) {
            skills.remove(&3363);
        }
    });
    let no_skill = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "AddCorporation",
            &[PyRep::String("Vanguard Industries".into()), PyRep::String("VANG".into()), PyRep::Real(0.05)],
            &kw,
            None,
        )
        .await
        .expect_err("AddCorporation without Corporation Management skill must fail");
    assert_eq!(no_skill.code, "MissingCorporationManagementSkill");

    let bal_before = ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            3363,
            eve_db::TrainedSkillRecord { type_id: 3363, level: 1, skill_points: 250.0 },
        );
        snap.characters[&90_000_001].balance
    });

    let create_corp = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "AddCorporation",
            &[PyRep::String("Vanguard Industries".into()), PyRep::String("VANG".into()), PyRep::Real(0.05)],
            &kw,
            None,
        )
        .await
        .expect("AddCorporation");
    let PyRep::KeyVal(cc_kv) = &create_corp else { panic!() };
    let corp_id = sess1.corporation_id.expect("sess1 corp_id set");
    assert_eq!(cc_kv["corporationID"], PyRep::Integer(corp_id));
    assert_eq!(cc_kv["ticker"], PyRep::String("VANG".into()));

    ctx.db.with_snapshot(|snap| {
        let ch = &snap.characters[&90_000_001];
        assert_eq!(bal_before.0 - ch.balance.0, 159_980_000, "AddCorporation must deduct 1,599,800 ISK");
        assert_eq!(ch.corporation_id, corp_id, "Creator must leave old NPC corp and join new corp");
        assert_ne!(ch.corporation_id, 1_000_009);
        assert_eq!(snap.corporations[&corp_id].ceo_id, 90_000_001);
    });

    // Duplicate name or ticker must be rejected
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_002).or_default().insert(
            3363,
            eve_db::TrainedSkillRecord { type_id: 3363, level: 1, skill_points: 250.0 },
        );
    });
    let dup_err = ctx
        .dispatch_rpc(
            &mut sess2,
            "corpRegistry",
            None,
            "AddCorporation",
            &[PyRep::String("Vanguard Industries".into()), PyRep::String("VANG".into()), PyRep::Real(0.05)],
            &kw,
            None,
        )
        .await
        .expect_err("Duplicate corporation name/ticker must be rejected");
    assert_eq!(dup_err.code, "CorpNameOrTickerTaken");

    // Verify 7 wallet divisions initialized and rename division 2 via corpRegistry.UpdateDivisionNames
    let divs_rep = ctx
        .dispatch_rpc(&mut sess1, "account", None, "GetWalletDivisionsInfo", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetWalletDivisionsInfo");
    let PyRep::Rowset { rows: init_divs, .. } = &divs_rep else { panic!() };
    assert_eq!(init_divs.len(), 7, "Corporation must have 7 wallet divisions (1000..1006 / 1..7)");
    assert_eq!(init_divs[0], vec![PyRep::Integer(1), PyRep::String("Master Wallet".into()), PyRep::Real(0.0)]);

    ctx.dispatch_rpc(
        &mut sess1,
        "corpRegistry",
        None,
        "UpdateDivisionNames",
        &[PyRep::Integer(2), PyRep::String("Combat SRP".into())],
        &kw,
        None,
    )
    .await
    .expect("UpdateDivisionNames");

    // 3. Pilot 2 applies for membership
    let apply_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "corpRegistry",
            None,
            "InsertApplication",
            &[PyRep::Integer(corp_id), PyRep::String("Combat pilot requesting invitation".into())],
            &kw,
            None,
        )
        .await
        .expect("InsertApplication");
    assert!(is_keyval(&apply_rep));

    // 4. Pilot 1 checks applications
    let apps_rep = ctx
        .dispatch_rpc(&mut sess1, "corpRegistry", None, "GetApplications", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetApplications");
    let PyRep::Rowset { rows: app_rows, .. } = &apps_rep else { panic!() };
    assert_eq!(app_rows.len(), 1);

    let app_id = ctx.db.with_snapshot(|snap| {
        snap.corporations[&corp_id].applications.keys().next().copied().unwrap()
    });

    // 5. Pilot 1 accepts Pilot 2's application
    let accept_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "UpdateApplicationOffer",
            &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
            &kw,
            None,
        )
        .await
        .expect("UpdateApplicationOffer");
    assert!(is_keyval(&accept_rep));

    // Verify Pilot 2 is now in corporation, and cannot transfer division funds before Accountant role is granted
    let members_rep = ctx
        .dispatch_rpc(&mut sess1, "corpRegistry", None, "GetMembers", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetMembers");
    let PyRep::Rowset { rows: mem_rows, .. } = &members_rep else { panic!() };
    assert_eq!(mem_rows.len(), 2);

    ctx.db
        .try_with_mut_snapshot(|snap| {
            let corp = snap.corporations.get_mut(&corp_id).unwrap();
            corp.wallet_divisions[0].balance = eve_db::IskCents::from_isk(50_000_000);
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();

    sess2.corporation_id = Some(corp_id);
    sess2.corp_role = 0;
    let unauth_xfer = ctx
        .dispatch_rpc(
            &mut sess2,
            "account",
            None,
            "GiveCashFromCorpAccount",
            &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(10_000_000.0)],
            &kw,
            None,
        )
        .await
        .expect_err("Member without Accountant/Director role must be denied GiveCashFromCorpAccount");
    assert_eq!(unauth_xfer.code, "CorpRoleDenied");

    // 6. Pilot 1 updates Pilot 2's roles to Accountant (ROLE_ACCOUNTANT = 1 << 8)
    let update_roles = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "UpdateMember",
            &[PyRep::Integer(90_000_002), PyRep::Integer(corp_roles::ROLE_ACCOUNTANT as i64)],
            &kw,
            None,
        )
        .await
        .expect("UpdateMember");
    assert!(is_keyval(&update_roles));

    // 7. Pilot 2 transfers funds between division 1 and division 2 and verifies via GetWalletDivisionsInfo
    sess2.corp_role = corp_roles::ROLE_ACCOUNTANT;
    let transfer_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "account",
            None,
            "GiveCashFromCorpAccount",
            &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(10_000_000.0)],
            &kw,
            None,
        )
        .await
        .expect("GiveCashFromCorpAccount");
    assert!(is_keyval(&transfer_rep));

    let divs_after = ctx
        .dispatch_rpc(&mut sess1, "account", None, "GetWalletDivisionsInfo", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetWalletDivisionsInfo after transfer");
    let PyRep::Rowset { rows: divs_after_rows, .. } = &divs_after else { panic!() };
    assert_eq!(divs_after_rows[0], vec![PyRep::Integer(1), PyRep::String("Master Wallet".into()), PyRep::Real(40_000_000.0)]);
    assert_eq!(divs_after_rows[1], vec![PyRep::Integer(2), PyRep::String("Combat SRP".into()), PyRep::Real(10_000_000.0)]);

    // 8. Verify role bitmask enforcement: Pilot 2 (not director/CEO) cannot UpdateMember
    let denied = ctx
        .dispatch_rpc(
            &mut sess2,
            "corpRegistry",
            None,
            "UpdateMember",
            &[PyRep::Integer(90_000_001), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await;
    assert_eq!(denied.unwrap_err().code, "CorpRoleDenied");
}

#[tokio::test]
async fn test_so3_chat_channel_join_leave_local_population_and_broadcast() {
    // parity: rpc:XmppChatMgr.CreatePrivateChannel, rpc:charUnboundMgr.SelectCharacterID
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(301);
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

    let mut sess2 = SessionState::new(302);
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
    ctx.advance_secs(12);

    // 1. Join custom channel
    let join1 = ctx
        .dispatch_rpc(
            &mut sess1,
            "LSC",
            None,
            "JoinChannel",
            &[PyRep::String("channel:wormhole_intel".into())],
            &kw,
            None,
        )
        .await
        .expect("Pilot 1 JoinChannel");
    assert!(is_keyval(&join1));

    let join2 = ctx
        .dispatch_rpc(
            &mut sess2,
            "LSC",
            None,
            "JoinChannel",
            &[PyRep::String("channel:wormhole_intel".into())],
            &kw,
            None,
        )
        .await
        .expect("Pilot 2 JoinChannel");
    assert!(is_keyval(&join2));

    // 2. Broadcast chat message
    let send_msg = ctx
        .dispatch_rpc(
            &mut sess1,
            "LSC",
            None,
            "SendMessage",
            &[
                PyRep::String("channel:wormhole_intel".into()),
                PyRep::String("Hostile Sabre on Jita gate".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("SendMessage");
    assert!(is_keyval(&send_msg));

    // Verify Pilot 2 received chat notification
    let p2_chat = ctx.drain_notifications_for_char(90_000_002);
    assert!(
        !p2_chat.is_empty(),
        "Expected chat notification for Pilot 2"
    );

    // 3. Leave channel
    let leave = ctx
        .dispatch_rpc(
            &mut sess2,
            "LSC",
            None,
            "LeaveChannel",
            &[PyRep::String("channel:wormhole_intel".into())],
            &kw,
            None,
        )
        .await
        .expect("LeaveChannel");
    assert!(is_keyval(&leave));

    // 4. Private 1-on-1 conversation
    let priv_convo = ctx
        .dispatch_rpc(
            &mut sess1,
            "XmppChatMgr",
            None,
            "CreatePrivateChannel",
            &[
                PyRep::Integer(90_000_002),
                PyRep::String("Need warp assist at relic site".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreatePrivateChannel");
    assert!(is_keyval(&priv_convo));

    // 5. Automatic local channel population on system change
    ctx.sync_character_local_chat(90_000_001, None, 30_000_142);
    ctx.db.with_snapshot(|snap| {
        let jita_chan = &snap.chat_channels["solarsystemid2:30000142"];
        assert!(
            jita_chan.members.contains(&90_000_001),
            "Pilot 1 must be in Jita local"
        );
    });

    // Move Pilot 1 to Tama (30002813)
    ctx.sync_character_local_chat(90_000_001, Some(30_000_142), 30_002_813);
    ctx.db.with_snapshot(|snap| {
        let jita_chan = &snap.chat_channels["solarsystemid2:30000142"];
        assert!(
            !jita_chan.members.contains(&90_000_001),
            "Pilot 1 must be removed from Jita local"
        );
        let tama_chan = &snap.chat_channels["solarsystemid2:30002813"];
        assert!(
            tama_chan.members.contains(&90_000_001),
            "Pilot 1 must be added to Tama local"
        );
    });
}

#[tokio::test]
async fn test_so4_in_game_mail_lifecycle() {
    // parity: rpc:charUnboundMgr.SelectCharacterID, rpc:mailMgr.DeleteMail, rpc:mailMgr.GetBody, rpc:mailMgr.GetMailHeaders, rpc:mailMgr.MarkAsRead
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(401);
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

    let mut sess2 = SessionState::new(402);
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
    ctx.advance_secs(12);

    // 1. Send mail: Pilot 1 -> Pilot 2
    let send_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "mailMgr",
            None,
            "SendMail",
            &[
                PyRep::Integer(90_000_002),
                PyRep::String("Fleet Deployment".into()),
                PyRep::String("Bring logistics cruisers to Jita 4-4".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("SendMail");
    let mail_id = send_rep.as_i64().expect("mail_id integer");

    // Verify OnMailSent notification emitted to the recipient
    let p2_notifs = ctx.drain_notifications_for_char(90_000_002);
    assert!(
        p2_notifs.iter().any(|n| n.event == "OnMailSent"),
        "Expected OnMailSent notification"
    );

    // 2. Pilot 2 fetches mail headers
    let headers_rep = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "GetMailHeaders", &[], &kw, None)
        .await
        .expect("GetMailHeaders");
    assert!(is_rowset(&headers_rep));

    // 3. Pilot 2 reads mail body
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
    assert!(matches!(body_rep, PyRep::Buffer(_)));

    // 4. Pilot 2 marks mail as read
    let read_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "mailMgr",
            None,
            "MarkAsRead",
            &[PyRep::Integer(mail_id)],
            &kw,
            None,
        )
        .await
        .expect("MarkAsRead");
    assert!(is_keyval(&read_rep));

    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.read_mails.contains(&(90_000_002, mail_id)),
            "Mail must be marked read for recipient"
        );
    });

    // 5. Pilot 2 deletes mail
    let delete_rep = ctx
        .dispatch_rpc(
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
    assert!(is_keyval(&delete_rep));

    // Verify mail no longer appears in Pilot 2's headers
    let headers_after = ctx
        .dispatch_rpc(&mut sess2, "mailMgr", None, "GetMailHeaders", &[], &kw, None)
        .await
        .expect("GetMailHeaders after delete");
    if let PyRep::Rowset { rows, .. } = headers_after {
        assert!(
            !rows
                .iter()
                .any(|cols| cols.first() == Some(&PyRep::Integer(mail_id))),
            "Deleted mail must not appear in headers"
        );
    }
}

#[tokio::test]
async fn test_so5_bookmarks_creation_entity_and_deletion() {
    // parity: rpc:charUnboundMgr.SelectCharacterID
    let ctx = ServerContext::new();
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(501);
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

    let mut sess2 = SessionState::new(502);
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
    ctx.advance_secs(12);

    // 1. Create static/docked bookmark
    let bm1_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "BookmarkLocation",
            &[PyRep::String("Safe Spot Alpha".into())],
            &kw,
            None,
        )
        .await
        .expect("BookmarkLocation");
    assert!(is_keyval(&bm1_rep));

    // 2. Create entity bookmark referencing station 60003760
    let bm2_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "BookmarkLocation",
            &[
                PyRep::Integer(60_003_760),
                PyRep::Integer(90_000_001),
                PyRep::String("Jita 4-4 Trade Hub".into()),
                PyRep::String("Main Market".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("BookmarkLocation entity");
    assert!(is_keyval(&bm2_rep));

    // 3. Get bookmarks
    let bms_rep = ctx
        .dispatch_rpc(&mut sess1, "bookmark", None, "GetBookmarks", &[], &kw, None)
        .await
        .expect("GetBookmarks");
    assert!(is_rowset(&bms_rep));

    let bm_id = ctx.db.with_snapshot(|snap| {
        snap.bookmarks
            .values()
            .find(|b| b.owner_id == 90_000_001)
            .map(|b| b.bookmark_id)
            .unwrap()
    });

    // 4. Pilot 2 cannot delete Pilot 1's bookmark
    let access_err = ctx
        .dispatch_rpc(
            &mut sess2,
            "bookmark",
            None,
            "DeleteBookmarks",
            &[PyRep::Integer(bm_id)],
            &kw,
            None,
        )
        .await;
    assert_eq!(access_err.unwrap_err().code, "AccessDenied");

    // 5. Pilot 1 deletes bookmark
    let del_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "bookmark",
            None,
            "DeleteBookmarks",
            &[PyRep::Integer(bm_id)],
            &kw,
            None,
        )
        .await
        .expect("DeleteBookmarks");
    assert!(is_keyval(&del_rep));
}

#[tokio::test]
async fn test_so6_cosmic_signatures_generation_and_probing_math() {
    // parity: rpc:charUnboundMgr.SelectCharacterID
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
    ctx.advance_secs(12);

    // 1. Initial query generates dynamic cosmic signatures for system
    let sigs_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "GetSignatures",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await
        .expect("GetSignatures");
    assert!(is_rowset(&sigs_rep));

    let (wh_sig_id, wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&30_000_142]
            .iter()
            .find(|s| s.sig_group == "Wormhole")
            .unwrap();
        (sig.signature_id.clone(), sig.position)
    });

    // 2. Wide probe scan (16 AU): probes detect signal and group
    let wide_scan = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "RequestScans",
            &[PyRep::Real(16.0 * AU), PyRep::Real(40.0)],
            &kw,
            None,
        )
        .await
        .expect("RequestScans Wide");
    assert!(is_rowset(&wide_scan));

    // 3. Pinpoint scan: 8-probe tetrahedral formation centered on the wormhole at 0.5 AU
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
    let probe_list = PyRep::List(
        probe_coords
            .into_iter()
            .map(|p| PyRep::List(vec![PyRep::Real(p.x), PyRep::Real(p.y), PyRep::Real(p.z)]))
            .collect(),
    );

    // Multiple pinpoint passes accumulate signal to 100%
    for _ in 0..4 {
        let _ = ctx
            .dispatch_rpc(
                &mut sess,
                "scanMgr",
                None,
                "RequestScans",
                &[PyRep::Real(0.5 * AU), PyRep::Real(50.0), probe_list.clone()],
                &kw,
                None,
            )
            .await
            .expect("RequestScans Pinpoint");
    }

    // Verify signature is 100% resolved in DB snapshot
    ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&30_000_142]
            .iter()
            .find(|s| s.signature_id == wh_sig_id)
            .unwrap();
        assert_eq!(
            sig.signal_strength, 1.0,
            "Wormhole signature must reach 100% signal strength"
        );
        assert_eq!(sig.type_name, "Unstable Wormhole (K162)");
    });
}

#[tokio::test]
async fn test_so7_wormhole_lifetime_mass_exhaustion_and_jump() {
    // parity: rpc:charUnboundMgr.SelectCharacterID, rpc:wormholeMgr.GetWormholePolarization, rpc:wormholeMgr.WormholeJump
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess = SessionState::new(701);
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
    ctx.advance_secs(12);

    // Ensure signatures exist in system 30000142
    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "GetSignatures",
            &[PyRep::Integer(30_000_142)],
            &kw,
            None,
        )
        .await
        .expect("GetSignatures");

    let wh_sig_id = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures[&30_000_142]
            .iter()
            .find(|s| s.sig_group == "Wormhole")
            .unwrap()
            .signature_id
            .clone()
    });

    // 1. Initial wormhole status: Healthy, Stable
    let info1 = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "GetWormholePolarization",
            &[PyRep::String(wh_sig_id.clone())],
            &kw,
            None,
        )
        .await
        .expect("GetWormholePolarization fresh");
    assert!(is_keyval(&info1));

    // 2. Mass stage tracking: Destabilized (40%) and Critical (5%)
    ctx.db
        .try_with_mut_snapshot(|snap| {
            let wh = snap
                .cosmic_signatures
                .get_mut(&30_000_142)
                .unwrap()
                .iter_mut()
                .find(|s| s.signature_id == wh_sig_id)
                .unwrap();
            wh.remaining_mass_kg = 800_000_000.0; // 40% -> Destabilized
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();

    let info_destab = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "GetWormholePolarization",
            &[PyRep::String(wh_sig_id.clone())],
            &kw,
            None,
        )
        .await
        .expect("GetWormholePolarization destab");
    if let PyRep::KeyVal(entries) = &info_destab {
        let mass_stage = entries
            .iter()
            .find(|(k, _)| k.as_str() == "massStage")
            .unwrap()
            .1
            .as_str()
            .unwrap();
        assert_eq!(mass_stage, "Destabilized");
    }

    ctx.db
        .try_with_mut_snapshot(|snap| {
            let wh = snap
                .cosmic_signatures
                .get_mut(&30_000_142)
                .unwrap()
                .iter_mut()
                .find(|s| s.signature_id == wh_sig_id)
                .unwrap();
            wh.remaining_mass_kg = 100_000_000.0; // 5% -> Critical
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();

    let info_crit = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "GetWormholePolarization",
            &[PyRep::String(wh_sig_id.clone())],
            &kw,
            None,
        )
        .await
        .expect("GetWormholePolarization crit");
    if let PyRep::KeyVal(entries) = &info_crit {
        let mass_stage = entries
            .iter()
            .find(|(k, _)| k.as_str() == "massStage")
            .unwrap()
            .1
            .as_str()
            .unwrap();
        assert_eq!(mass_stage, "Critical");
    }

    // 3. Lifetime stage tracking: EOL (remaining < 25%) and Expired
    ctx.db
        .try_with_mut_snapshot(|snap| {
            let wh = snap
                .cosmic_signatures
                .get_mut(&30_000_142)
                .unwrap()
                .iter_mut()
                .find(|s| s.signature_id == wh_sig_id)
                .unwrap();
            wh.created_unix = ctx.now_unix().saturating_sub(70_000); // 70k / 86.4k elapsed -> EOL
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();

    let info_eol = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "GetWormholePolarization",
            &[PyRep::String(wh_sig_id.clone())],
            &kw,
            None,
        )
        .await
        .expect("GetWormholePolarization EOL");
    if let PyRep::KeyVal(entries) = &info_eol {
        let life_stage = entries
            .iter()
            .find(|(k, _)| k.as_str() == "lifetimeStage")
            .unwrap()
            .1
            .as_str()
            .unwrap();
        assert_eq!(life_stage, "ReachingEndOfLifetime");
    }

    // 4. Wormhole Jump Mechanics
    let wh_pos = ctx.db.with_snapshot(|snap| {
        let wh = snap.cosmic_signatures[&30_000_142]
            .iter()
            .find(|s| s.signature_id == wh_sig_id)
            .unwrap();
        wh.position
    });
    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    let ship_ball = Ball {
        id: 100_000_001,
        type_id: 587,
        position: DVec3::new(wh_pos[0] + 500.0, wh_pos[1], wh_pos[2]),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_200_000.0, // 1,200 metric tons
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = jita_sys.spawn_ball(ship_ball).await;
    sess.ship_id = Some(100_000_001);
    sess.station_id = None;
    sess.structure_id = None;

    // Reset wormhole mass to 2,000,000,000 kg and fresh lifetime
    ctx.db
        .try_with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) {
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = 100_000_001;
            }
            let wh = snap
                .cosmic_signatures
                .get_mut(&30_000_142)
                .unwrap()
                .iter_mut()
                .find(|s| s.signature_id == wh_sig_id)
                .unwrap();
            wh.remaining_mass_kg = 2_000_000_000.0;
            wh.created_unix = ctx.now_unix();
            wh.collapsed = false;
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();

    // Execute Jump
    let jump_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::String(wh_sig_id.clone())],
            &kw,
            None,
        )
        .await
        .expect("WormholeJump");
    assert!(is_keyval(&jump_rep));

    // Verify session system changed to destination wormhole system (31000005)
    assert_eq!(sess.solar_system_id, Some(31_000_005));

    // Verify mass deducted by ship mass (1,200,000 kg)
    ctx.db.with_snapshot(|snap| {
        let wh = snap.cosmic_signatures[&30_000_142]
            .iter()
            .find(|s| s.signature_id == wh_sig_id)
            .unwrap();
        assert_eq!(wh.remaining_mass_kg, 2_000_000_000.0 - 1_200_000.0);
    });
}
