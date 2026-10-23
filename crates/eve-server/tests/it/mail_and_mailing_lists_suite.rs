//! Integration test suite for EVE Mail & Mailing Lists RPC methods (`SO-1`).
//!
//! Validates all 18 Mail & Mailing Lists operations:
//! - 6 `mailMgr` methods:
//!   - `MarkAsReadByList`
//!   - `MarkAsUnreadByList`
//!   - `MoveAllToTrash`
//!   - `MoveToTrash`
//!   - `MoveToTrashByLabel`
//!   - `MoveToTrashByList`
//! - 12 `mailingListsMgr` methods:
//!   - `ClearEntityAccess`
//!   - `ClearWelcomeMail`
//!   - `Delete`
//!   - `GetSettings`
//!   - `KickMembers`
//!   - `SaveWelcomeMail`
//!   - `SendWelcomeMail`
//!   - `SetDefaultAccess`
//!   - `SetEntitiesAccess`
//!   - `SetMembersClear`
//!   - `SetMembersMuted`
//!   - `SetMembersOperator`

use crate::common;
use eve_db::MailRecord;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn select_pilot(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    let kw = HashMap::new();
    ctx.dispatch_rpc(
        &mut s,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    s
}

fn drain_events(ctx: &ServerContext, char_id: i64, event_name: &str) -> Vec<Vec<PyRep>> {
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == event_name)
        .map(|n| n.args)
        .collect()
}

#[tokio::test]
async fn test_mail_mgr_trash_and_read_state_operations() {
    // parity: rpc:mailMgr.MarkAsReadByList, rpc:mailMgr.MarkAsUnreadByList, rpc:mailMgr.MoveAllToTrash, rpc:mailMgr.MoveToTrash, rpc:mailMgr.MoveToTrashByLabel, rpc:mailMgr.MoveToTrashByList
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let mut s1 = select_pilot(&ctx, 801, "demo", "demo-secret", P1).await;
    let mut s2 = select_pilot(&ctx, 802, "other", "other-secret", P2).await;
    let kw = HashMap::new();

    let mid_read = 710_001i64;
    let mid_label = 710_002i64;
    let mid_all1 = 710_004i64;
    let mid_all2 = 710_005i64;
    let custom_label = 7i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.mails.push(MailRecord {
            message_id: mid_read,
            sender_id: P2,
            recipient_id: P1,
            subject: "Read Test".into(),
            body: "Read Body".into(),
        });
        snap.mail_assigned_labels.insert((P1, mid_read), vec![1]);

        snap.mails.push(MailRecord {
            message_id: mid_label,
            sender_id: P2,
            recipient_id: P1,
            subject: "Label Test".into(),
            body: "Label Body".into(),
        });
        snap.mail_assigned_labels.insert((P1, mid_label), vec![custom_label]);

        snap.mails.push(MailRecord {
            message_id: mid_all1,
            sender_id: P2,
            recipient_id: P1,
            subject: "All Test 1".into(),
            body: "Body 1".into(),
        });
        snap.mail_assigned_labels.insert((P1, mid_all1), vec![1]);

        snap.mails.push(MailRecord {
            message_id: mid_all2,
            sender_id: P2,
            recipient_id: P1,
            subject: "All Test 2".into(),
            body: "Body 2".into(),
        });
        snap.mail_assigned_labels.insert((P1, mid_all2), vec![1]);
    });

    // P1 creates mailing list and P2 joins
    let ml_res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "Create",
            &[PyRep::String("List For Trash".into())],
            &kw,
            None,
        )
        .await
        .expect("Create");
    let list_id = ml_res.dict_get("listID").and_then(PyRep::as_i64).unwrap();

    ctx.dispatch_rpc(&mut s2, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("P2 join");

    // P2 sends authentic list mail to P1 via SendMail with toListID
    let mid_list = ctx
        .dispatch_rpc(
            &mut s2,
            "mailMgr",
            None,
            "SendMail",
            &[
                PyRep::List(vec![]),
                PyRep::Integer(list_id),
                PyRep::None,
                PyRep::String("List Mail".into()),
                PyRep::String("List Body".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("SendMail to list")
        .as_i64()
        .unwrap();

    ctx.drain_notifications_for_char(P1);

    // 1. MarkAsReadByList: mark list mail as read via listID
    let res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailMgr",
            None,
            "MarkAsReadByList",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await
        .expect("MarkAsReadByList");

    assert_eq!(res.dict_get("read"), Some(&PyRep::Bool(true)));
    assert_eq!(
        res.dict_get("messageIDs"),
        Some(&PyRep::List(vec![PyRep::Integer(mid_list)]))
    );
    assert!(ctx.db.with_snapshot(|s| s.read_mails.contains(&(P1, mid_list))));
    let notifs_read = ctx.drain_notifications_for_char(P1);
    assert!(notifs_read.iter().any(|n| n.event == "OnMailCountersUpdate"));
    let ext_updates: Vec<_> = notifs_read.iter().filter(|n| n.event == "OnMailUpdatedByExternal").collect();
    assert!(!ext_updates.is_empty(), "OnMailUpdatedByExternal must be emitted");
    assert_eq!(ext_updates[0].args[1], PyRep::Bool(true));

    // 2. MarkAsUnreadByList: mark list mail as unread via listID
    let res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailMgr",
            None,
            "MarkAsUnreadByList",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await
        .expect("MarkAsUnreadByList");

    assert_eq!(res.dict_get("read"), Some(&PyRep::Bool(false)));
    assert_eq!(
        res.dict_get("messageIDs"),
        Some(&PyRep::List(vec![PyRep::Integer(mid_list)]))
    );
    assert!(!ctx.db.with_snapshot(|s| s.read_mails.contains(&(P1, mid_list))));
    let notifs_unread = ctx.drain_notifications_for_char(P1);
    assert!(notifs_unread.iter().any(|n| n.event == "OnMailCountersUpdate"));
    let ext_unread: Vec<_> = notifs_unread.iter().filter(|n| n.event == "OnMailUpdatedByExternal").collect();
    assert!(!ext_unread.is_empty());
    assert_eq!(ext_unread[0].args[1], PyRep::Bool(false));

    // 3. MoveToTrash: move `mid_read` to trash
    let res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailMgr",
            None,
            "MoveToTrash",
            &[PyRep::List(vec![PyRep::Integer(mid_read)])],
            &kw,
            None,
        )
        .await
        .expect("MoveToTrash");

    assert_eq!(res.dict_get("inTrash"), Some(&PyRep::Bool(true)));
    assert_eq!(res.dict_get("trashed"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|s| s.trashed_mails.contains(&(P1, mid_read))));
    let trash_events = drain_events(&ctx, P1, "OnMailTrashed");
    assert_eq!(trash_events.len(), 1);
    assert_eq!(
        trash_events[0][0],
        PyRep::List(vec![PyRep::Integer(mid_read)])
    );

    // 4. MoveToTrashByLabel: trash mails carrying custom label
    let res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailMgr",
            None,
            "MoveToTrashByLabel",
            &[PyRep::Integer(custom_label)],
            &kw,
            None,
        )
        .await
        .expect("MoveToTrashByLabel");

    assert_eq!(res.dict_get("inTrash"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|s| s.trashed_mails.contains(&(P1, mid_label))));
    let label_trash_events = drain_events(&ctx, P1, "OnMailTrashed");
    assert_eq!(label_trash_events.len(), 1);
    assert_eq!(
        label_trash_events[0][0],
        PyRep::List(vec![PyRep::Integer(mid_label)])
    );

    // 5. MoveToTrashByList: trash mails matching list ID
    let res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailMgr",
            None,
            "MoveToTrashByList",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await
        .expect("MoveToTrashByList");

    assert_eq!(res.dict_get("inTrash"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|s| s.trashed_mails.contains(&(P1, mid_list))));
    let list_trash_events = drain_events(&ctx, P1, "OnMailTrashed");
    assert_eq!(list_trash_events.len(), 1);
    assert_eq!(
        list_trash_events[0][0],
        PyRep::List(vec![PyRep::Integer(mid_list)])
    );

    // 6. MoveAllToTrash: move remaining active mails to trash
    let res = ctx
        .dispatch_rpc(&mut s1, "mailMgr", None, "MoveAllToTrash", &[], &kw, None)
        .await
        .expect("MoveAllToTrash");

    assert_eq!(res.dict_get("inTrash"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|s| s.trashed_mails.contains(&(P1, mid_all1))));
    assert!(ctx.db.with_snapshot(|s| s.trashed_mails.contains(&(P1, mid_all2))));

    // 7. Restart durability: verify list mail association survives server hydration/restart (R-ML-03)
    let snap_data = ctx.db.with_snapshot(|s| s.clone());
    let db2 = std::sync::Arc::new(eve_db::WorldRepository::new_empty());
    db2.with_mut_snapshot(|s| *s = snap_data);
    let ctx2 = ServerContext::with_repository(db2);
    let mut s1_re = select_pilot(&ctx2, 803, "demo", "demo-secret", P1).await;
    let res_restart = ctx2
        .dispatch_rpc(
            &mut s1_re,
            "mailMgr",
            None,
            "MarkAsReadByList",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await
        .expect("MarkAsReadByList after restart");
    assert_eq!(res_restart.dict_get("read"), Some(&PyRep::Bool(true)));
}

#[tokio::test]
async fn test_mailing_lists_mgr_lifecycle_access_and_welcome_operations() {
    // parity: rpc:mailingListsMgr.ClearEntityAccess, rpc:mailingListsMgr.ClearWelcomeMail, rpc:mailingListsMgr.Delete, rpc:mailingListsMgr.GetSettings, rpc:mailingListsMgr.KickMembers, rpc:mailingListsMgr.SaveWelcomeMail, rpc:mailingListsMgr.SendWelcomeMail, rpc:mailingListsMgr.SetDefaultAccess, rpc:mailingListsMgr.SetEntitiesAccess, rpc:mailingListsMgr.SetMembersClear, rpc:mailingListsMgr.SetMembersMuted, rpc:mailingListsMgr.SetMembersOperator
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();

    let mut s1 = select_pilot(&ctx, 811, "demo", "demo-secret", P1).await;
    let mut s2 = select_pilot(&ctx, 812, "other", "other-secret", P2).await;

    let uid3 = ctx.db.provision_player_account("pilot3", "pilot3-secret");
    let p3 = ctx
        .db
        .create_character(uid3, "Pilot Three", 1, 1, 11, 17, &ctx.sde)
        .expect("create p3")
        .character_id;
    let mut s3 = select_pilot(&ctx, 813, "pilot3", "pilot3-secret", p3).await;
    let kw = HashMap::new();

    // Setup: P1 creates mailing list
    let create_res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "Create",
            &[PyRep::String("Vanguard Vanguard".into())],
            &kw,
            None,
        )
        .await
        .expect("Create");
    let list_id = create_res
        .dict_get("listID")
        .and_then(PyRep::as_i64)
        .expect("listID");

    // 1. GetSettings: inspect initial configuration
    let settings = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "GetSettings",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await
        .expect("GetSettings");
    assert_eq!(settings.dict_get("listID"), Some(&PyRep::Integer(list_id)));
    assert_eq!(settings.dict_get("defaultAccess"), Some(&PyRep::Integer(1)));
    assert_eq!(settings.dict_get("welcomeMail"), Some(&PyRep::None));

    // 2. SetDefaultAccess: configure restricted access (defaultAccess = 0/closed)
    let res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "SetDefaultAccess",
            &[PyRep::Integer(list_id), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("SetDefaultAccess");
    assert_eq!(res.dict_get("defaultAccess"), Some(&PyRep::Integer(0)));

    // Non-member P3 without access fails GetSettings
    let denied = ctx
        .dispatch_rpc(
            &mut s3,
            "mailingListsMgr",
            None,
            "GetSettings",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await;
    let denied_err = denied.unwrap_err();
    assert_eq!(denied_err.code, "UserError");
    assert!(denied_err.message.contains("MailingListAccessDenied"));

    // 3. SetEntitiesAccess: set explicit access override for P3
    let entity_access_dict = vec![(PyRep::Integer(p3), PyRep::Integer(2))];
    let res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "SetEntitiesAccess",
            &[PyRep::Integer(list_id), PyRep::Dict(entity_access_dict)],
            &kw,
            None,
        )
        .await
        .expect("SetEntitiesAccess");
    assert_eq!(res.dict_get("listID"), Some(&PyRep::Integer(list_id)));

    // P3 now has access override and can read settings
    let p3_settings = ctx
        .dispatch_rpc(
            &mut s3,
            "mailingListsMgr",
            None,
            "GetSettings",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await
        .expect("P3 GetSettings after access override");
    assert_eq!(p3_settings.dict_get("listID"), Some(&PyRep::Integer(list_id)));

    // 4. ClearEntityAccess: clear access override for P3
    let res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "ClearEntityAccess",
            &[PyRep::Integer(list_id), PyRep::Integer(p3)],
            &kw,
            None,
        )
        .await
        .expect("ClearEntityAccess");
    assert_eq!(res.as_bool(), Some(true));

    // Reset default access to standard public (1) and defaultMemberAccess (1) so members can join (R-ML-02)
    ctx.dispatch_rpc(&mut s1, "mailingListsMgr", None, "SetDefaultAccess", &[PyRep::Integer(list_id), PyRep::Integer(1), PyRep::Integer(1)], &kw, None).await.expect("SetDefaultAccess public");

    // Corp-level entity override blocks P3 joining even when default_access is public
    let p3_corp = ctx.db.with_snapshot(|s| s.characters[&p3].corporation_id);
    let block_dict = vec![(PyRep::Integer(p3_corp), PyRep::Integer(0))];
    ctx.dispatch_rpc(&mut s1, "mailingListsMgr", None, "SetEntitiesAccess", &[PyRep::Integer(list_id), PyRep::Dict(block_dict)], &kw, None).await.expect("SetEntitiesAccess block corp");
    let join_blocked = ctx.dispatch_rpc(&mut s3, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &kw, None).await;
    let err = join_blocked.unwrap_err();
    assert_eq!(err.code, "UserError");
    assert_eq!(err.message, "Access to this mailing list is blocked");
    ctx.dispatch_rpc(&mut s1, "mailingListsMgr", None, "ClearEntityAccess", &[PyRep::Integer(list_id), PyRep::Integer(p3_corp)], &kw, None).await.expect("ClearEntityAccess unblock corp");

    // P2 and P3 join the list with default role (1)
    ctx.dispatch_rpc(&mut s2, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &kw, None).await.expect("P2 Join");
    ctx.dispatch_rpc(&mut s3, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &kw, None).await.expect("P3 Join");

    ctx.drain_notifications_for_char(P2);
    ctx.drain_notifications_for_char(p3);

    // 5. SetMembersOperator: promote P2 to operator role (2)
    let res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "SetMembersOperator",
            &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(P2)])],
            &kw,
            None,
        )
        .await
        .expect("SetMembersOperator");
    assert_eq!(res.as_bool(), Some(true));
    let notifs = drain_events(&ctx, P2, "OnMailingListSetOperator");
    assert_eq!(notifs.len(), 1);
    assert_eq!(notifs[0][0], PyRep::Integer(list_id));
    let members = ctx.dispatch_rpc(&mut s1, "mailingListsMgr", None, "GetMembers", &[PyRep::Integer(list_id)], &kw, None).await.expect("GetMembers");
    let PyRep::Rowset { rows: mem_rows, .. } = &members else { panic!("rowset") };
    assert!(mem_rows.contains(&vec![PyRep::Integer(P2), PyRep::Integer(2)]), "P2 must have operator role 2");
    assert!(mem_rows.contains(&vec![PyRep::Integer(P1), PyRep::Integer(3)]), "P1 must retain owner role 3");

    // 6. SetMembersMuted: P2 (as newly promoted operator) mutes P3 to role 0 (R-ML-01)
    let res = ctx
        .dispatch_rpc(
            &mut s2,
            "mailingListsMgr",
            None,
            "SetMembersMuted",
            &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(p3)])],
            &kw,
            None,
        )
        .await
        .expect("SetMembersMuted by operator");
    assert_eq!(res.as_bool(), Some(true));
    let mute_notifs = drain_events(&ctx, p3, "OnMailingListSetMuted");
    assert_eq!(mute_notifs.len(), 1);
    let members = ctx.dispatch_rpc(&mut s1, "mailingListsMgr", None, "GetMembers", &[PyRep::Integer(list_id)], &kw, None).await.expect("GetMembers");
    let PyRep::Rowset { rows: mem_rows, .. } = &members else { panic!("rowset") };
    assert!(mem_rows.contains(&vec![PyRep::Integer(p3), PyRep::Integer(0)]), "P3 must have muted role 0");
    // Verify muted P3 cannot send mail to list
    let mute_err = ctx.dispatch_rpc(&mut s3, "mailMgr", None, "SendMail", &[PyRep::List(vec![]), PyRep::Integer(list_id), PyRep::None, PyRep::String("M".into()), PyRep::String("B".into())], &kw, None).await.unwrap_err();
    assert_eq!(mute_err.code, "UserError");
    assert_eq!(mute_err.message, "MutedInMailingList");

    // 7. SetMembersClear: P1 clears P3 role back to default role 1 (R-ML-01)
    let res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "SetMembersClear",
            &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(p3)])],
            &kw,
            None,
        )
        .await
        .expect("SetMembersClear");
    assert_eq!(res.as_bool(), Some(true));
    let clear_notifs = drain_events(&ctx, p3, "OnMailingListSetClear");
    assert_eq!(clear_notifs.len(), 1);
    let members = ctx.dispatch_rpc(&mut s1, "mailingListsMgr", None, "GetMembers", &[PyRep::Integer(list_id)], &kw, None).await.expect("GetMembers");
    let PyRep::Rowset { rows: mem_rows, .. } = &members else { panic!("rowset") };
    assert!(mem_rows.contains(&vec![PyRep::Integer(p3), PyRep::Integer(1)]), "P3 must have default role 1");
    // Verify cleared P3 can now send mail to list (R-ML-02)
    let send_ok = ctx.dispatch_rpc(&mut s3, "mailMgr", None, "SendMail", &[PyRep::List(vec![]), PyRep::Integer(list_id), PyRep::None, PyRep::String("OK".into()), PyRep::String("Body".into())], &kw, None).await.expect("SendMail cleared");
    assert!(send_ok.as_i64().is_some());

    // 8. KickMembers: operator P2 kicks P3
    let res = ctx
        .dispatch_rpc(
            &mut s2,
            "mailingListsMgr",
            None,
            "KickMembers",
            &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(p3)])],
            &kw,
            None,
        )
        .await
        .expect("KickMembers");
    assert_eq!(res.as_bool(), Some(true));
    let kick_notifs = drain_events(&ctx, p3, "OnMailingListLeave");
    assert_eq!(kick_notifs.len(), 1);
    assert_eq!(kick_notifs[0][0], PyRep::Integer(list_id));
    assert_eq!(kick_notifs[0][1], PyRep::Integer(p3));

    // R-ML-05: Verify non-member P3 cannot send mail to the list
    let mail_ids_before = ctx.db.with_snapshot(|s| s.mailing_lists.get(&list_id).unwrap().mail_ids.clone());
    let non_member_send_err = ctx.dispatch_rpc(&mut s3, "mailMgr", None, "SendMail", &[PyRep::List(vec![]), PyRep::Integer(list_id), PyRep::None, PyRep::String("X".into()), PyRep::String("Y".into())], &kw, None).await.unwrap_err();
    assert_eq!(non_member_send_err.code, "UserError");
    assert_eq!(non_member_send_err.message, "MailingListAccessDenied");
    let mail_ids_after = ctx.db.with_snapshot(|s| s.mailing_lists.get(&list_id).unwrap().mail_ids.clone());
    assert_eq!(mail_ids_before.len(), mail_ids_after.len(), "Mail IDs should not increase");

    // 9. SaveWelcomeMail & GetWelcomeMail (R-ML-04): authentic [0].title / [0].body shape and access check
    let res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "SaveWelcomeMail",
            &[
                PyRep::Integer(list_id),
                PyRep::String("Welcome Vanguard".into()),
                PyRep::String("Rules of engagement inside".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("SaveWelcomeMail");
    assert_eq!(res.dict_get("title"), Some(&PyRep::String("Welcome Vanguard".into())));

    let wm = ctx.dispatch_rpc(&mut s1, "mailingListsMgr", None, "GetWelcomeMail", &[PyRep::Integer(list_id)], &kw, None).await.expect("GetWelcomeMail");
    let PyRep::List(ref items) = wm else { panic!("expected List") };
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].dict_get("title"), Some(&PyRep::String("Welcome Vanguard".into())));
    assert_eq!(items[0].dict_get("body"), Some(&PyRep::String("Rules of engagement inside".into())));

    // 10. SendWelcomeMail: broadcast welcome mail to active member P2
    ctx.drain_notifications_for_char(P2);
    let res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "SendWelcomeMail",
            &[
                PyRep::Integer(list_id),
                PyRep::String("Welcome Vanguard".into()),
                PyRep::String("Rules of engagement inside".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("SendWelcomeMail");
    assert_eq!(res.as_bool(), Some(true));
    let sent_notifs = drain_events(&ctx, P2, "OnMailSent");
    assert!(!sent_notifs.is_empty(), "P2 must receive welcome mail via OnMailSent");

    // 11. ClearWelcomeMail: clears welcome mail and verify empty list from GetWelcomeMail
    let res = ctx
        .dispatch_rpc(&mut s1, "mailingListsMgr", None, "ClearWelcomeMail", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("ClearWelcomeMail");
    assert_eq!(res.as_bool(), Some(true));
    let cleared_settings = ctx
        .dispatch_rpc(&mut s1, "mailingListsMgr", None, "GetSettings", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("GetSettings after clear");
    assert_eq!(cleared_settings.dict_get("welcomeMail"), Some(&PyRep::None));
    let wm_cleared = ctx.dispatch_rpc(&mut s1, "mailingListsMgr", None, "GetWelcomeMail", &[PyRep::Integer(list_id)], &kw, None).await.expect("GetWelcomeMail cleared");
    let PyRep::List(ref empty_items) = wm_cleared else { panic!("expected List") };
    assert!(empty_items.is_empty());

    // 12. Delete: non-owner P2 cannot delete list; owner P1 can delete list
    let non_owner_del = ctx
        .dispatch_rpc(
            &mut s2,
            "mailingListsMgr",
            None,
            "Delete",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await;
    let del_err = non_owner_del.unwrap_err();
    assert_eq!(del_err.code, "UserError");
    assert!(del_err.message.contains("MailingListAccessDenied"));

    ctx.drain_notifications_for_char(P2);
    let res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "Delete",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await
        .expect("Owner Delete");
    assert_eq!(res.as_bool(), Some(true));

    let del_notifs = drain_events(&ctx, P2, "OnMailingListDeleted");
    assert_eq!(del_notifs.len(), 1);
    assert_eq!(del_notifs[0][0], PyRep::Integer(list_id));

    // Deleted list cannot be fetched
    let get_deleted = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "GetSettings",
            &[PyRep::Integer(list_id)],
            &kw,
            None,
        )
        .await;
    assert!(get_deleted.is_err(), "Deleted mailing list settings cannot be queried");
}
