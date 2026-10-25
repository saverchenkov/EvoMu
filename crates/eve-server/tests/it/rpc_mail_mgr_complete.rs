//! Complete integration tests for `mailMgr` RPC methods (`SO-1`).
//!
//! Validates all 30 open `mailMgr` methods:
//! - Lifecycle, Headers, Body, Sync & Labels:
//!   `AssignLabels`, `CreateLabel`, `DeleteLabel`, `DeleteMail`, `EditLabel`, `GetBody`,
//!   `GetLabels`, `GetMailHeaders`, `PokePlayerAboutChatMsgGm`, `RemoveLabels`,
//!   `ReplaceLabels`, `SyncMail`, `iteritems`, `itervalues`, `keys`
//! - Read states, Lists & Trash:
//!   `EmptyTrash`, `MarkAllAsRead`, `MarkAllAsUnread`, `MarkAsRead`, `MarkAsReadByLabel`,
//!   `MarkAsReadByList`, `MarkAsUnread`, `MarkAsUnreadByLabel`, `MarkAsUnreadByList`,
//!   `MoveAllFromTrash`, `MoveAllToTrash`, `MoveFromTrash`, `MoveToTrash`,
//!   `MoveToTrashByLabel`, `MoveToTrashByList`

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

#[tokio::test]
async fn test_mail_mgr_lifecycle_headers_body_and_sync() {
    // parity: rpc:mailMgr.AssignLabels, rpc:mailMgr.CreateLabel, rpc:mailMgr.DeleteLabel, rpc:mailMgr.DeleteMail, rpc:mailMgr.EditLabel, rpc:mailMgr.GetBody, rpc:mailMgr.GetLabels, rpc:mailMgr.GetMailHeaders, rpc:mailMgr.PokePlayerAboutChatMsgGm, rpc:mailMgr.RemoveLabels, rpc:mailMgr.ReplaceLabels, rpc:mailMgr.SyncMail, rpc:mailMgr.iteritems, rpc:mailMgr.itervalues, rpc:mailMgr.keys
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let mut s1 = select_pilot(&ctx, 811, "demo", "demo-secret", P1).await;
    let mut s2 = select_pilot(&ctx, 812, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("mail_p3", "mail_p3-secret");
    let p3 = ctx.db.create_character(uid3, "Mail Pilot Three", 1, 1, 11, 17, &ctx.sde).expect("create p3").character_id;
    let mut s3 = select_pilot(&ctx, 813, "mail_p3", "mail_p3-secret", p3).await;
    let kw = HashMap::new();

    let m1 = 720_001i64;
    let m2 = 720_002i64;
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.mails.push(MailRecord {
            message_id: m1,
            sender_id: P1,
            recipient_id: P2,
            subject: "Fleet Mobilization".into(),
            body: "Report to staging station immediately.".into(),
        });
        snap.mail_assigned_labels.insert((P2, m1), vec![1]);
        snap.mail_timestamps.insert(m1, now);

        snap.mails.push(MailRecord {
            message_id: m2,
            sender_id: P1,
            recipient_id: P2,
            subject: "Doctrine Update".into(),
            body: "Armor fleet fits are now mandatory.".into(),
        });
        snap.mail_assigned_labels.insert((P2, m2), vec![1]);
        snap.mail_timestamps.insert(m2, now + 1);
    });

    // 1. GetMailHeaders
    let hdrs_rep = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "GetMailHeaders", &[], &kw, None)
        .await
        .expect("GetMailHeaders");
    let PyRep::Rowset { rows: hdr_rows, .. } = &hdrs_rep else {
        panic!("GetMailHeaders must return rowset");
    };
    assert_eq!(hdr_rows.len(), 2, "P2 should have 2 mail headers");
    let m1_hdr = hdr_rows.iter().find(|r| r[0] == PyRep::Integer(m1)).expect("m1 header");
    assert_eq!(m1_hdr[1], PyRep::Integer(P1), "senderID must be P1");
    assert_eq!(m1_hdr[2], PyRep::String(P2.to_string()), "toCharacterIDs must be String");
    assert_eq!(m1_hdr[3], PyRep::None, "toCorpOrAllianceID must be None");
    assert_eq!(m1_hdr[4], PyRep::None, "toListID must be None");
    assert_eq!(m1_hdr[5], PyRep::String("Fleet Mobilization".into()), "title match");
    assert_eq!(m1_hdr[7], PyRep::Bool(false), "initially unread");

    // GetMailHeaders(toFetch) filtering
    let fetch_rep = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "GetMailHeaders", &[PyRep::List(vec![PyRep::Integer(m1)])], &kw, None)
        .await
        .expect("GetMailHeaders with toFetch");
    let PyRep::Rowset { rows: fetch_rows, .. } = &fetch_rep else {
        panic!("GetMailHeaders toFetch must return rowset");
    };
    assert_eq!(fetch_rows.len(), 1, "toFetch must filter to only requested messageID");
    assert_eq!(fetch_rows[0][0], PyRep::Integer(m1));

    // 2. GetBody - P2 reads m1 (returns zlib-compressed Buffer)
    let body_rep = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "GetBody", &[PyRep::Integer(m1)], &kw, None)
        .await
        .expect("GetBody");
    let PyRep::Buffer(comp_bytes) = &body_rep else { panic!("GetBody must return Buffer"); };
    let decomp = miniz_oxide::inflate::decompress_to_vec_zlib(comp_bytes).expect("decompress zlib");
    assert_eq!(
        String::from_utf8(decomp).unwrap(),
        "Report to staging station immediately."
    );

    // Multi-recipient GetBody: P1 sends to both P2 and P3
    let send_multi = ctx
        .dispatch_rpc(
            &mut s1,
            "mailMgr",
            None,
            "SendMail",
            &[
                PyRep::List(vec![PyRep::Integer(P2), PyRep::Integer(p3)]),
                PyRep::None,
                PyRep::None,
                PyRep::String("Multi Recipient Test".into()),
                PyRep::String("Multi recipient payload content".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("SendMail multi");
    let multi_mid = send_multi.as_i64().expect("multi mid");
    let p2_read = ctx.dispatch_rpc(&mut s2, "mailMgr", None, "GetBody", &[PyRep::Integer(multi_mid)], &kw, None).await.expect("P2 read multi");
    let PyRep::Buffer(p2_bytes) = &p2_read else { panic!("p2 buffer"); };
    assert_eq!(String::from_utf8(miniz_oxide::inflate::decompress_to_vec_zlib(p2_bytes).unwrap()).unwrap(), "Multi recipient payload content");

    let p3_read = ctx.dispatch_rpc(&mut s3, "mailMgr", None, "GetBody", &[PyRep::Integer(multi_mid)], &kw, None).await.expect("P3 read multi");
    let PyRep::Buffer(p3_bytes) = &p3_read else { panic!("p3 buffer"); };
    assert_eq!(String::from_utf8(miniz_oxide::inflate::decompress_to_vec_zlib(p3_bytes).unwrap()).unwrap(), "Multi recipient payload content");

    let p1_read = ctx.dispatch_rpc(&mut s1, "mailMgr", None, "GetBody", &[PyRep::Integer(multi_mid)], &kw, None).await.expect("P1 read multi");
    let PyRep::Buffer(p1_bytes) = &p1_read else { panic!("p1 buffer"); };
    assert_eq!(String::from_utf8(miniz_oxide::inflate::decompress_to_vec_zlib(p1_bytes).unwrap()).unwrap(), "Multi recipient payload content");

    // Unauthorized GetBody: P3 cannot view mail between P1 and P2
    let auth_err = ctx
        .dispatch_rpc(&mut s3, "mailMgr", None, "GetBody", &[PyRep::Integer(m1)], &kw, None)
        .await
        .expect_err("P3 viewing P2 mail must fail");
    assert_eq!(auth_err.code, "AccessDenied");

    // 3. SyncMail
    let sync_rep = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "SyncMail", &[], &kw, None)
        .await
        .expect("SyncMail");
    let PyRep::KeyVal(sync_kv) = &sync_rep else { panic!("SyncMail must return KeyVal"); };
    assert!(sync_kv.get("oldMail").is_some(), "SyncMail must include oldMail");
    let Some(PyRep::Rowset { rows: new_rows, .. }) = sync_kv.get("newMail") else {
        panic!("SyncMail must include newMail Rowset");
    };
    assert!(new_rows.len() >= 2, "newMail rowset must list headers");
    let Some(PyRep::Rowset { rows: status_rows, .. }) = sync_kv.get("mailStatus") else {
        panic!("SyncMail must include mailStatus Rowset");
    };
    let m1_status = status_rows.iter().find(|r| r[0] == PyRep::Integer(m1)).expect("m1 status");
    assert_eq!(m1_status[1], PyRep::Integer(1), "statusMask: read=1");
    assert_eq!(m1_status[3], PyRep::Bool(true), "read boolean is true");

    // 4. keys, iteritems, itervalues
    let keys_rep = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "keys", &[], &kw, None)
        .await
        .expect("keys");
    let PyRep::List(keys_list) = &keys_rep else { panic!("keys must return List"); };
    assert!(keys_list.contains(&PyRep::Integer(m1)));
    assert!(keys_list.contains(&PyRep::Integer(m2)));
    assert!(keys_list.contains(&PyRep::Integer(multi_mid)));

    let items_rep = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "iteritems", &[], &kw, None)
        .await
        .expect("iteritems");
    let PyRep::List(items_list) = &items_rep else { panic!("iteritems must return List"); };
    assert_eq!(items_list.len(), 3);
    let first_item = &items_list[0];
    let PyRep::Tuple(pair) = first_item else { panic!("item must be a tuple"); };
    assert_eq!(pair.len(), 2);

    let vals_rep = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "itervalues", &[], &kw, None)
        .await
        .expect("itervalues");
    let PyRep::List(vals_list) = &vals_rep else { panic!("itervalues must return List"); };
    assert_eq!(vals_list.len(), 3);

    // 5. Labels: CreateLabel, GetLabels, EditLabel
    let create_lbl = ctx
        .dispatch_rpc(
            &mut s2,
            "mailMgr",
            None,
            "CreateLabel",
            &[PyRep::String("Deployments".into()), PyRep::String("#ff5500".into())],
            &kw,
            None,
        )
        .await
        .expect("CreateLabel");
    let custom_lbl_id = create_lbl.as_i64().expect("CreateLabel must return integer label_id");
    assert!(custom_lbl_id >= 16, "Custom label ID must be bitmask >= 16");

    let labels_rep = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "GetLabels", &[], &kw, None)
        .await
        .expect("GetLabels");
    let PyRep::Dict(lbl_dict) = &labels_rep else { panic!("GetLabels must return Dict"); };
    let custom_entry = lbl_dict.iter().find(|(k, _)| k == &PyRep::Integer(custom_lbl_id)).map(|(_, v)| v).expect("Custom label present in GetLabels");
    assert_eq!(custom_entry.dict_get("name"), Some(&PyRep::String("Deployments".into())));

    let edit_lbl = ctx
        .dispatch_rpc(
            &mut s2,
            "mailMgr",
            None,
            "EditLabel",
            &[PyRep::Integer(custom_lbl_id), PyRep::String("Combat Ops".into()), PyRep::String("#00ff99".into())],
            &kw,
            None,
        )
        .await
        .expect("EditLabel");
    let PyRep::KeyVal(el_kv) = &edit_lbl else { panic!("EditLabel KeyVal"); };
    assert_eq!(el_kv.get("name"), Some(&PyRep::String("Combat Ops".into())));

    // 6. AssignLabels, RemoveLabels, ReplaceLabels
    // Unauthorized mutation: P3 cannot assign labels to m1
    let unauth_assign = ctx
        .dispatch_rpc(
            &mut s3,
            "mailMgr",
            None,
            "AssignLabels",
            &[PyRep::List(vec![PyRep::Integer(m1)]), PyRep::List(vec![PyRep::Integer(custom_lbl_id)])],
            &kw,
            None,
        )
        .await
        .expect_err("Unauth AssignLabels must fail");
    assert_eq!(unauth_assign.code, "AccessDenied");

    let assign_rep = ctx
        .dispatch_rpc(
            &mut s2,
            "mailMgr",
            None,
            "AssignLabels",
            &[PyRep::List(vec![PyRep::Integer(m1)]), PyRep::List(vec![PyRep::Integer(custom_lbl_id)])],
            &kw,
            None,
        )
        .await
        .expect("AssignLabels");
    assert_eq!(assign_rep.dict_get("assigned"), Some(&PyRep::Bool(true)));

    let remove_rep = ctx
        .dispatch_rpc(
            &mut s2,
            "mailMgr",
            None,
            "RemoveLabels",
            &[PyRep::List(vec![PyRep::Integer(m1)]), PyRep::List(vec![PyRep::Integer(custom_lbl_id)])],
            &kw,
            None,
        )
        .await
        .expect("RemoveLabels");
    assert_eq!(remove_rep.dict_get("removed"), Some(&PyRep::Bool(true)));

    let replace_rep = ctx
        .dispatch_rpc(
            &mut s2,
            "mailMgr",
            None,
            "ReplaceLabels",
            &[PyRep::Integer(m1), PyRep::List(vec![PyRep::Integer(2), PyRep::Integer(custom_lbl_id)])],
            &kw,
            None,
        )
        .await
        .expect("ReplaceLabels");
    assert_eq!(replace_rep.dict_get("assigned"), Some(&PyRep::Bool(true)));

    // 7. DeleteLabel
    let sys_del_err = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "DeleteLabel", &[PyRep::Integer(1)], &kw, None)
        .await
        .expect_err("System label 1 cannot be deleted");
    assert_eq!(sys_del_err.code, "CannotDeleteSystemLabel");

    let del_lbl = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "DeleteLabel", &[PyRep::Integer(custom_lbl_id)], &kw, None)
        .await
        .expect("DeleteLabel");
    assert_eq!(del_lbl.dict_get("deleted"), Some(&PyRep::Bool(true)));

    // 8. PokePlayerAboutChatMsgGm
    let non_gm_err = ctx
        .dispatch_rpc(
            &mut s1,
            "mailMgr",
            None,
            "PokePlayerAboutChatMsgGm",
            &[PyRep::Integer(P2), PyRep::String("Urgent GM notification".into())],
            &kw,
            None,
        )
        .await
        .expect_err("Non-GM poke must fail");
    assert_eq!(non_gm_err.code, "UserError");

    let mut gm_sess = s1.clone();
    gm_sess.role = eve_machonet::service_roles::ROLE_GMH;
    ctx.drain_notifications_for_char(P2);
    let gm_poke = ctx
        .dispatch_rpc(
            &mut gm_sess,
            "mailMgr",
            None,
            "PokePlayerAboutChatMsgGm",
            &[PyRep::Integer(P2), PyRep::String("Official GM notice".into())],
            &kw,
            None,
        )
        .await
        .expect("GM poke");
    assert!(gm_poke.as_i64().unwrap_or(0) > 0);
    let p2_events = ctx.drain_notifications_for_char(P2);
    assert!(p2_events.iter().any(|n| n.event == "OnCharacterPokedByGM"));
    assert!(p2_events.iter().any(|n| n.event == "OnMailSent"));

    // 9. DeleteMail
    let unauth_del = ctx
        .dispatch_rpc(&mut s3, "mailMgr", None, "DeleteMail", &[PyRep::Integer(m2)], &kw, None)
        .await
        .expect_err("Unauth DeleteMail must fail");
    assert_eq!(unauth_del.code, "AccessDenied");

    let del_mail = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "DeleteMail", &[PyRep::Integer(m2)], &kw, None)
        .await
        .expect("DeleteMail");
    assert_eq!(del_mail.dict_get("deleted"), Some(&PyRep::Bool(true)));

    let keys_after = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "keys", &[], &kw, None)
        .await
        .expect("keys after delete");
    let PyRep::List(keys_after_list) = &keys_after else { panic!("keys must be List"); };
    assert!(!keys_after_list.contains(&PyRep::Integer(m2)), "Deleted mail m2 must not appear in keys");
}

#[tokio::test]
async fn test_mail_mgr_read_states_and_trash_operations() {
    // parity: rpc:mailMgr.EmptyTrash, rpc:mailMgr.MarkAllAsRead, rpc:mailMgr.MarkAllAsUnread, rpc:mailMgr.MarkAsRead, rpc:mailMgr.MarkAsReadByLabel, rpc:mailMgr.MarkAsReadByList, rpc:mailMgr.MarkAsUnread, rpc:mailMgr.MarkAsUnreadByLabel, rpc:mailMgr.MarkAsUnreadByList, rpc:mailMgr.MoveAllFromTrash, rpc:mailMgr.MoveAllToTrash, rpc:mailMgr.MoveFromTrash, rpc:mailMgr.MoveToTrash, rpc:mailMgr.MoveToTrashByLabel, rpc:mailMgr.MoveToTrashByList
    let ctx = ServerContext::new();
    ctx.seed_demo_world_fixtures();
    let mut s1 = select_pilot(&ctx, 821, "demo", "demo-secret", P1).await;
    let mut s2 = select_pilot(&ctx, 822, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("mail_p3_1", "mail_p3_1-secret");
    let p3 = ctx.db.create_character(uid3, "Mail Pilot Three", 1, 1, 11, 17, &ctx.sde).expect("create p3").character_id;
    let mut s3 = select_pilot(&ctx, 823, "mail_p3_1", "mail_p3_1-secret", p3).await;
    let kw = HashMap::new();

    let m1 = 721_001i64;
    let m2 = 721_002i64;
    let custom_label = 32i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.mails.push(MailRecord {
            message_id: m1,
            sender_id: P1,
            recipient_id: P2,
            subject: "Standard Mail".into(),
            body: "Standard Body".into(),
        });
        snap.mail_assigned_labels.insert((P2, m1), vec![1]);

        snap.mails.push(MailRecord {
            message_id: m2,
            sender_id: P1,
            recipient_id: P2,
            subject: "Label Mail".into(),
            body: "Label Body".into(),
        });
        snap.mail_assigned_labels.insert((P2, m2), vec![custom_label]);
    });

    // P1 creates mailing list and P2 joins
    let ml_res = ctx
        .dispatch_rpc(
            &mut s1,
            "mailingListsMgr",
            None,
            "Create",
            &[PyRep::String("Vanguard Ops".into())],
            &kw,
            None,
        )
        .await
        .expect("Create");
    let list_id = ml_res.dict_get("listID").and_then(PyRep::as_i64).unwrap();

    ctx.dispatch_rpc(&mut s2, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("P2 join");

    // P1 sends list mail to the mailing list
    let send_list_mail = ctx
        .dispatch_rpc(
            &mut s1,
            "mailMgr",
            None,
            "SendMail",
            &[
                PyRep::List(vec![]),
                PyRep::Integer(list_id),
                PyRep::Integer(0),
                PyRep::String("List Mail".into()),
                PyRep::String("List Body".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("SendMail to list");
    let m3 = send_list_mail.as_i64().unwrap();


    // 1. MarkAsRead & MarkAsUnread
    let unauth_read = ctx
        .dispatch_rpc(&mut s3, "mailMgr", None, "MarkAsRead", &[PyRep::List(vec![PyRep::Integer(m1)])], &kw, None)
        .await
        .expect_err("Unauth MarkAsRead must fail");
    assert_eq!(unauth_read.code, "AccessDenied");

    let read_res = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "MarkAsRead", &[PyRep::List(vec![PyRep::Integer(m1)])], &kw, None)
        .await
        .expect("MarkAsRead");
    assert_eq!(read_res.dict_get("read"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|snap| snap.read_mails.contains(&(P2, m1))));

    let unread_res = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "MarkAsUnread", &[PyRep::List(vec![PyRep::Integer(m1)])], &kw, None)
        .await
        .expect("MarkAsUnread");
    assert_eq!(unread_res.dict_get("read"), Some(&PyRep::Bool(false)));
    assert!(!ctx.db.with_snapshot(|snap| snap.read_mails.contains(&(P2, m1))));

    // 2. MarkAsReadByLabel & MarkAsUnreadByLabel
    let r_lbl = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "MarkAsReadByLabel", &[PyRep::Integer(custom_label)], &kw, None)
        .await
        .expect("MarkAsReadByLabel");
    assert_eq!(r_lbl.dict_get("read"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|snap| snap.read_mails.contains(&(P2, m2))));

    let u_lbl = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "MarkAsUnreadByLabel", &[PyRep::Integer(custom_label)], &kw, None)
        .await
        .expect("MarkAsUnreadByLabel");
    assert_eq!(u_lbl.dict_get("read"), Some(&PyRep::Bool(false)));
    assert!(!ctx.db.with_snapshot(|snap| snap.read_mails.contains(&(P2, m2))));

    // 3. MarkAsReadByList & MarkAsUnreadByList
    let r_list = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "MarkAsReadByList", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("MarkAsReadByList");
    assert_eq!(r_list.dict_get("read"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|snap| snap.read_mails.contains(&(P2, m3))));

    let u_list = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "MarkAsUnreadByList", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("MarkAsUnreadByList");
    assert_eq!(u_list.dict_get("read"), Some(&PyRep::Bool(false)));
    assert!(!ctx.db.with_snapshot(|snap| snap.read_mails.contains(&(P2, m3))));

    // 4. MarkAllAsRead & MarkAllAsUnread
    let all_read = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "MarkAllAsRead", &[], &kw, None)
        .await
        .expect("MarkAllAsRead");
    let PyRep::KeyVal(ar_kv) = &all_read else { panic!("MarkAllAsRead KeyVal"); };
    assert_eq!(ar_kv.get("markedReadCount"), Some(&PyRep::Integer(3)));

    let all_unread = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "MarkAllAsUnread", &[], &kw, None)
        .await
        .expect("MarkAllAsUnread");
    let PyRep::KeyVal(au_kv) = &all_unread else { panic!("MarkAllAsUnread KeyVal"); };
    assert_eq!(au_kv.get("markedReadCount"), Some(&PyRep::Integer(3)));

    // 5. MoveToTrash & MoveFromTrash
    let unauth_trash = ctx
        .dispatch_rpc(&mut s3, "mailMgr", None, "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(m1)])], &kw, None)
        .await
        .expect_err("Unauth MoveToTrash must fail");
    assert_eq!(unauth_trash.code, "AccessDenied");

    let trash_m1 = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "MoveToTrash", &[PyRep::List(vec![PyRep::Integer(m1)])], &kw, None)
        .await
        .expect("MoveToTrash");
    assert_eq!(trash_m1.dict_get("inTrash"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|snap| snap.trashed_mails.contains(&(P2, m1))));

    let unauth_from_trash = ctx
        .dispatch_rpc(&mut s3, "mailMgr", None, "MoveFromTrash", &[PyRep::List(vec![PyRep::Integer(m1)])], &kw, None)
        .await
        .expect_err("Unauth MoveFromTrash must fail");
    assert_eq!(unauth_from_trash.code, "AccessDenied");

    let restore_m1 = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "MoveFromTrash", &[PyRep::List(vec![PyRep::Integer(m1)])], &kw, None)
        .await
        .expect("MoveFromTrash");
    assert_eq!(restore_m1.dict_get("inTrash"), Some(&PyRep::Bool(false)));
    assert!(!ctx.db.with_snapshot(|snap| snap.trashed_mails.contains(&(P2, m1))));

    // 6. MoveToTrashByLabel
    let trash_by_lbl = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "MoveToTrashByLabel", &[PyRep::Integer(custom_label)], &kw, None)
        .await
        .expect("MoveToTrashByLabel");
    assert_eq!(trash_by_lbl.dict_get("inTrash"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|snap| snap.trashed_mails.contains(&(P2, m2))));

    // 7. MoveToTrashByList
    let trash_by_lst = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "MoveToTrashByList", &[PyRep::Integer(list_id)], &kw, None)
        .await
        .expect("MoveToTrashByList");
    assert_eq!(trash_by_lst.dict_get("inTrash"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|snap| snap.trashed_mails.contains(&(P2, m3))));

    // 8. MoveAllFromTrash
    let restore_all = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "MoveAllFromTrash", &[], &kw, None)
        .await
        .expect("MoveAllFromTrash");
    assert_eq!(restore_all.dict_get("inTrash"), Some(&PyRep::Bool(false)));
    assert!(!ctx.db.with_snapshot(|snap| snap.trashed_mails.contains(&(P2, m2))));
    assert!(!ctx.db.with_snapshot(|snap| snap.trashed_mails.contains(&(P2, m3))));

    // 9. MoveAllToTrash
    let trash_all = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "MoveAllToTrash", &[], &kw, None)
        .await
        .expect("MoveAllToTrash");
    assert_eq!(trash_all.dict_get("inTrash"), Some(&PyRep::Bool(true)));
    assert!(ctx.db.with_snapshot(|snap| snap.trashed_mails.contains(&(P2, m1))));
    assert!(ctx.db.with_snapshot(|snap| snap.trashed_mails.contains(&(P2, m2))));
    assert!(ctx.db.with_snapshot(|snap| snap.trashed_mails.contains(&(P2, m3))));

    // 10. EmptyTrash
    let empty_trash = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "EmptyTrash", &[], &kw, None)
        .await
        .expect("EmptyTrash");
    assert_eq!(empty_trash.dict_get("deleted"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.trashed_mails.contains(&(P2, m1)));
        assert!(snap.deleted_mails.contains(&(P2, m1)));
        assert!(snap.deleted_mails.contains(&(P2, m2)));
        assert!(snap.deleted_mails.contains(&(P2, m3)));
    });

    let empty_again = ctx
        .dispatch_rpc(&mut s2, "mailMgr", None, "EmptyTrash", &[], &kw, None)
        .await
        .expect("EmptyTrash when already empty");
    assert_eq!(empty_again.dict_get("emptied"), Some(&PyRep::Bool(true)));
}
