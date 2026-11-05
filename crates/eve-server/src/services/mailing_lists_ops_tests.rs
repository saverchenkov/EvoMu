use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::MailRecord;
use std::collections::HashMap;

#[test]
fn test_mail_mgr_list_mark_and_trash_operations() {
    let (ctx, session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let list_id = 501_i64;

    // Seed mails and mailing list in snapshot
    ctx.db.with_mut_snapshot(|snap| {
        for (mid, label) in [(101, 1), (102, 4), (103, 1), (104, 1)] {
            snap.mails.push(MailRecord {
                message_id: mid,
                sender_id: 90_000_002,
                recipient_id: char_id,
                subject: format!("Mail {mid}"),
                body: "Body".into(),
            });
            snap.mail_assigned_labels.insert((char_id, mid), vec![label]);
        }
    });
    {
        let mut s = ctx.social_extras.lock().unwrap();
        s.mailing_lists.insert(
            list_id,
            MailingListState {
                list_id,
                name: "Ops List".into(),
                owner_id: char_id,
                default_access: 1,
                default_role: 1,
                cost: 0,
                members: [(char_id, 3)].into_iter().collect(),
                entity_access: Default::default(),
                welcome_mail: None,
                mail_ids: [101, 103].into_iter().collect(),
            },
        );
    }
    sync_mailing_lists_to_db(&ctx);

    // MarkAsReadByList & MarkAsUnreadByList
    let read_res = handle_mail_and_mailing_lists_ops(
        &ctx,
        &session,
        "mailMgr",
        "MarkAsReadByList",
        &[PyRep::Integer(list_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(read_res.dict_get("read").and_then(|v| v.as_bool()), Some(true));

    let unread_res = handle_mail_and_mailing_lists_ops(
        &ctx,
        &session,
        "mailMgr",
        "MarkAsUnreadByList",
        &[PyRep::Integer(list_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(unread_res.dict_get("read").and_then(|v| v.as_bool()), Some(false));

    // MoveToTrash, MoveToTrashByLabel, MoveToTrashByList, MoveAllToTrash
    for (method, args) in [
        ("MoveToTrash", vec![PyRep::List(vec![PyRep::Integer(101)])]),
        ("MoveToTrashByLabel", vec![PyRep::Integer(4)]),
        ("MoveToTrashByList", vec![PyRep::Integer(list_id)]),
        ("MoveAllToTrash", vec![]),
    ] {
        let res = handle_mail_and_mailing_lists_ops(&ctx, &session, "mailMgr", method, &args, &kw)
            .unwrap()
            .unwrap();
        assert_eq!(res.dict_get("inTrash").and_then(|v| v.as_bool()), Some(true));
    }
}

#[test]
fn test_mailing_lists_mgr_full_lifecycle() {
    let (ctx, session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let kw = HashMap::new();
    let member_id = 90_000_003;

    // Create mailing list
    let list_id_rep = handle_mail_and_mailing_lists_ops(
        &ctx,
        &session,
        "mailingListsMgr",
        "Create",
        &[
            PyRep::String("Incursion Runners".into()),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let list_id = list_id_rep.as_i64().unwrap();

    // Add member so we can test roles, welcome mail, and kick
    {
        let mut s = ctx.social_extras.lock().unwrap();
        if let Some(ml) = s.mailing_lists.get_mut(&list_id) {
            ml.members.insert(member_id, 1);
        }
    }

    // SaveWelcomeMail, GetWelcomeMail, SendWelcomeMail, ClearWelcomeMail
    handle_mail_and_mailing_lists_ops(
        &ctx,
        &session,
        "mailingListsMgr",
        "SaveWelcomeMail",
        &[
            PyRep::Integer(list_id),
            PyRep::String("Welcome aboard".into()),
            PyRep::String("Read MOTD".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();

    let wm = handle_mail_and_mailing_lists_ops(
        &ctx,
        &session,
        "mailingListsMgr",
        "GetWelcomeMail",
        &[PyRep::Integer(list_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(wm, PyRep::List(_)));

    handle_mail_and_mailing_lists_ops(
        &ctx,
        &session,
        "mailingListsMgr",
        "SendWelcomeMail",
        &[
            PyRep::Integer(list_id),
            PyRep::String("Welcome aboard".into()),
            PyRep::String("Read MOTD".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();

    handle_mail_and_mailing_lists_ops(
        &ctx,
        &session,
        "mailingListsMgr",
        "ClearWelcomeMail",
        &[PyRep::Integer(list_id)],
        &kw,
    )
    .unwrap()
    .unwrap();

    // SetDefaultAccess, SetEntityAccess, ClearEntityAccess, GetSettings, GetMembers
    handle_mail_and_mailing_lists_ops(
        &ctx,
        &session,
        "mailingListsMgr",
        "SetDefaultAccess",
        &[
            PyRep::Integer(list_id),
            PyRep::Integer(2),
            PyRep::Integer(1),
            PyRep::Integer(100),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();

    handle_mail_and_mailing_lists_ops(
        &ctx,
        &session,
        "mailingListsMgr",
        "SetEntityAccess",
        &[
            PyRep::Integer(list_id),
            PyRep::Integer(member_id),
            PyRep::Integer(2),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();

    handle_mail_and_mailing_lists_ops(
        &ctx,
        &session,
        "mailingListsMgr",
        "ClearEntityAccess",
        &[PyRep::Integer(list_id), PyRep::Integer(member_id)],
        &kw,
    )
    .unwrap()
    .unwrap();

    let settings = handle_mail_and_mailing_lists_ops(
        &ctx,
        &session,
        "mailingListsMgr",
        "GetSettings",
        &[PyRep::Integer(list_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(settings.dict_get("listID").and_then(|v| v.as_i64()), Some(list_id));

    // SetMembersOperator, SetMembersMuted, SetMembersClear, KickMembers, Delete
    for method in ["SetMembersOperator", "SetMembersMuted", "SetMembersClear", "KickMembers"] {
        handle_mail_and_mailing_lists_ops(
            &ctx,
            &session,
            "mailingListsMgr",
            method,
            &[
                PyRep::Integer(list_id),
                PyRep::List(vec![PyRep::Integer(member_id)]),
            ],
            &kw,
        )
        .unwrap()
        .unwrap();
    }

    let _ = char_id;
    let del = handle_mail_and_mailing_lists_ops(
        &ctx,
        &session,
        "mailingListsMgr",
        "Delete",
        &[PyRep::Integer(list_id)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(del, PyRep::Bool(true));
}
