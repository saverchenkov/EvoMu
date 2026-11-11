use super::*;
use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use eve_db::MailRecord;

#[test]
fn test_mail_notify_meta_emitters_headers_and_auth() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    session.corporation_id = Some(98_000_001);
    session.alliance_id = Some(99_000_001);
    session.role = u64::MAX;

    // Seed mail record and record_mail_meta
    let mid = 55_001_i64;
    let recipients = [char_id];
    let sent = SentMail {
        message_id: mid,
        sender_id: 90_000_002,
        to_character_ids: &recipients,
        to_list_id: Some(77),
        to_corp_or_alliance_id: Some(98_000_001),
        title: "Fleet CTA",
        status_mask: MAIL_STATUS_AUTOMATED,
    };
    record_mail_meta(&ctx, mid, &recipients, Some(77), Some(98_000_001));

    ctx.db.with_mut_snapshot(|snap| {
        snap.mails.push(MailRecord {
            message_id: mid,
            sender_id: 90_000_002,
            recipient_id: char_id,
            subject: "Fleet CTA".into(),
            body: "Form up in Jita".into(),
        });
        snap.mail_timestamps.insert(mid, 1_700_000_000);
        snap.mail_assigned_labels.insert((char_id, mid), vec![1, 4]);
    });

    // Test authorization checks
    let auth_ok = ctx
        .db
        .with_snapshot(|snap| check_mails_authorized(&ctx, snap, &[mid], char_id));
    assert!(auth_ok);
    let auth_empty = ctx
        .db
        .with_snapshot(|snap| check_mails_authorized(&ctx, snap, &[], 99_999_999));
    assert!(auth_empty);
    let auth_denied = ctx
        .db
        .with_snapshot(|snap| check_mails_authorized(&ctx, snap, &[mid], 90_000_999));
    assert!(!auth_denied);

    // Test resolve_mail_targets and active_mailbox_window_ids
    let targets = resolve_mail_targets(&ctx, char_id, "MarkAsReadByLabel", Some(&PyRep::Integer(1)));
    assert!(targets.contains(&mid));
    let all_targets = resolve_mail_targets(&ctx, char_id, "MoveAllToTrash", None);
    assert!(all_targets.contains(&mid));
    let win_ids = ctx.db.with_snapshot(|snap| active_mailbox_window_ids(snap, char_id));
    assert!(win_ids.contains(&mid));

    // Test emitters
    emit_mail_sent(&ctx, &sent, &recipients);
    emit_mail_deleted(&ctx, char_id, &[mid]);
    emit_mail_updated_by_external(&ctx, char_id, &[mid], Some(true), Some(5));
    emit_labels_created_by_external(&ctx, char_id, 8, "Urgent", "ffffff");
    emit_mailing_list_list_id(&ctx, "OnMailingListSetOperator", 77, &recipients);
    emit_mailing_list_leave(&ctx, 77, &recipients);
    emit_event_response_by_external(&ctx, char_id, 10, char_id, 1000, 60, "Title", 1, 1);
    emit_character_poked_by_gm(&ctx, char_id, char_id, "local");

    // Test GetMailHeaders and SyncMail
    let headers = handle_mail_headers(
        &ctx,
        &mut session,
        "GetMailHeaders",
        &[PyRep::List(vec![PyRep::Integer(mid)])],
    )
    .unwrap()
    .unwrap();
    assert!(!matches!(headers, PyRep::None));

    let synced = handle_mail_headers(
        &ctx,
        &mut session,
        "SyncMail",
        &[PyRep::Integer(0), PyRep::Integer(mid)],
    )
    .unwrap()
    .unwrap();
    assert!(synced.dict_get("newMail").is_some());

    // Test handle_poke_player_about_chat_msg_gm
    let poke = handle_poke_player_about_chat_msg_gm(
        &ctx,
        &mut session,
        &[PyRep::Integer(char_id), PyRep::String("Watch your language".into())],
    )
    .unwrap()
    .unwrap();
    assert!(poke.as_i64().unwrap_or(0) > 0);
}

#[test]
fn test_mail_notify_edge_auth_window_targets_headers_and_gm_poke_errors() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let other_char = 90_000_002_i64;
    let third_char = 90_000_003_i64;

    // 1. Emitters with duplicate recipients and None options
    let sent_none = SentMail {
        message_id: 60_001,
        sender_id: char_id,
        to_character_ids: &[other_char],
        to_list_id: None,
        to_corp_or_alliance_id: None,
        title: "Ping",
        status_mask: 0,
    };
    emit_mail_sent(&ctx, &sent_none, &[other_char, other_char, third_char]);
    emit_mail_updated_by_external(&ctx, char_id, &[60_001], None, None);

    // 2. is_mail_authorized & check_mails_authorized edge cases:
    // - missing mail_id -> false
    // - authorized via sender, meta_recipient, and mailing list member
    let list_id = 77_700_i64;
    record_mail_meta(&ctx, 60_002, &[third_char], None, None);
    ctx.db.with_mut_snapshot(|snap| {
        snap.mails.clear();
        snap.mails.push(MailRecord {
            message_id: 60_001,
            sender_id: char_id,
            recipient_id: other_char,
            subject: "Sent".into(),
            body: "B1".into(),
        });
        snap.mails.push(MailRecord {
            message_id: 60_002,
            sender_id: other_char,
            recipient_id: 88_888, // third_char only in meta_recipient!
            subject: "MetaRec".into(),
            body: "B2".into(),
        });
        snap.mails.push(MailRecord {
            message_id: 60_003,
            sender_id: other_char,
            recipient_id: list_id, // mailing list mail
            subject: "ListMail".into(),
            body: "B3".into(),
        });
        snap.mails.push(MailRecord {
            message_id: 60_004,
            sender_id: other_char,
            recipient_id: char_id,
            subject: "TrashedInSnap".into(),
            body: "B4".into(),
        });
        snap.mails.push(MailRecord {
            message_id: 60_005,
            sender_id: other_char,
            recipient_id: char_id,
            subject: "DeletedInSnap".into(),
            body: "B5".into(),
        });
        snap.mails.push(MailRecord {
            message_id: 60_006,
            sender_id: other_char,
            recipient_id: char_id,
            subject: "TrashedInExtras".into(),
            body: "B6".into(),
        });
        snap.mails.push(MailRecord {
            message_id: 60_007,
            sender_id: other_char,
            recipient_id: char_id,
            subject: "FiletimeRead".into(),
            body: "B7".into(),
        });

        let mut ml = eve_db::MailingListRecord {
            list_id,
            name: "Alliance List".into(),
            owner_id: other_char,
            default_access: 1,
            default_role: 0,
            cost: 0,
            members: std::collections::HashMap::from([(third_char, 0)]),
            entity_access: std::collections::HashMap::new(),
            welcome_mail: None,
            mail_ids: std::collections::HashSet::from([60_003]),
        };
        ml.members.insert(char_id, 0);
        snap.mailing_lists.insert(list_id, ml);

        snap.trashed_mails.insert((char_id, 60_004));
        snap.deleted_mails.insert((char_id, 60_005));
        snap.read_mails.insert((char_id, 60_007));
        // Timestamp already in Windows FILETIME (> 10^17) vs unix seconds vs absent
        snap.mail_timestamps.insert(60_007, 133_500_000_000_000_000);
        snap.mail_timestamps.insert(60_001, 1_700_000_000);
    });
    ctx.social_extras.lock().unwrap().trashed_mails.insert((char_id, 60_006));

    ctx.db.with_snapshot(|snap| {
        assert!(!is_mail_authorized(&ctx, snap, 999_999, char_id));
        assert!(is_mail_authorized(&ctx, snap, 60_001, char_id)); // sender
        assert!(is_mail_authorized(&ctx, snap, 60_002, third_char)); // meta recipient
        assert!(!is_mail_authorized(&ctx, snap, 60_002, char_id)); // unrelated
        assert!(is_mail_authorized(&ctx, snap, 60_003, char_id)); // list member
        assert!(!check_mails_authorized(&ctx, snap, &[60_001, 60_002], char_id));
    });

    // 3. resolve_mail_targets across label, MoveAllToTrash, and extract_id_list variants
    // Default label for sender=2 (60_001), recipient=1 (60_007); trashed/deleted excluded
    let inbox_targets =
        resolve_mail_targets(&ctx, char_id, "MarkAsUnreadByLabel", Some(&PyRep::Integer(1)));
    assert!(inbox_targets.contains(&60_007));
    assert!(!inbox_targets.contains(&60_001));
    assert!(!inbox_targets.contains(&60_004));
    assert!(!inbox_targets.contains(&60_005));
    assert!(!inbox_targets.contains(&60_006));

    let sent_targets =
        resolve_mail_targets(&ctx, char_id, "MoveToTrashByLabel", Some(&PyRep::Integer(2)));
    assert_eq!(sent_targets, vec![60_001]);

    let all_trash = resolve_mail_targets(&ctx, char_id, "MoveAllToTrash", None);
    assert!(all_trash.contains(&60_001));
    assert!(all_trash.contains(&60_004)); // MoveAllToTrash only skips deleted_mails
    assert!(!all_trash.contains(&60_005));

    assert_eq!(
        resolve_mail_targets(
            &ctx,
            char_id,
            "DeleteMails",
            Some(&PyRep::Tuple(vec![PyRep::Integer(11), PyRep::Integer(22)]))
        ),
        vec![11, 22]
    );
    assert_eq!(
        resolve_mail_targets(&ctx, char_id, "MarkAsRead", Some(&PyRep::Integer(33))),
        vec![33]
    );
    assert!(resolve_mail_targets(&ctx, char_id, "MarkAsRead", None).is_empty());

    // 4. handle_mail_headers: unauth, Tuple, Integer, empty List (returns all eligible)
    let mut anon = SessionState::default();
    assert!(handle_mail_headers(&ctx, &mut anon, "GetMailHeaders", &[]).unwrap().is_err());

    let hdr_tuple = handle_mail_headers(
        &ctx,
        &mut session,
        "GetMailHeaders",
        &[PyRep::Tuple(vec![PyRep::Integer(60_001), PyRep::Integer(60_002), PyRep::Integer(60_007)])],
    )
    .unwrap()
    .unwrap();
    let hdr_int = handle_mail_headers(
        &ctx,
        &mut session,
        "GetMailHeaders",
        &[PyRep::Integer(60_003)],
    )
    .unwrap()
    .unwrap();
    let hdr_empty_list = handle_mail_headers(
        &ctx,
        &mut session,
        "GetMailHeaders",
        &[PyRep::List(vec![])],
    )
    .unwrap()
    .unwrap();
    // Should include 60_001, 60_003, 60_007 (not 60_002, 60_004, 60_005, 60_006)
    let row_count = |rep: &PyRep| -> usize {
        if let PyRep::Rowset { rows, .. } = rep {
            rows.len()
        } else {
            0
        }
    };
    assert_eq!(row_count(&hdr_tuple), 2); // 60_001 + 60_007 (60_002 skipped as unauthorized)
    assert_eq!(row_count(&hdr_int), 1); // 60_003 via list membership
    assert_eq!(row_count(&hdr_empty_list), 3); // 60_001, 60_003, 60_007

    // 5. active_mailbox_window_ids 3,000 limit enforcement
    ctx.db.with_mut_snapshot(|snap| {
        for idx in 1..=3_010_i64 {
            let mid = 70_000 + idx;
            snap.mails.push(MailRecord {
                message_id: mid,
                sender_id: other_char,
                recipient_id: char_id,
                subject: format!("Bulk {idx}"),
                body: "x".into(),
            });
            snap.mail_timestamps.insert(mid, idx as u64);
        }
        let win = active_mailbox_window_ids(snap, char_id);
        assert_eq!(win.len(), 3_000);
        assert!(win.contains(&(70_000 + 3_010))); // newest retained
        assert!(!win.contains(&(70_000 + 1))); // oldest dropped
    });

    // 6. handle_poke_player_about_chat_msg_gm error branches
    session.role = 0;
    assert_eq!(
        handle_poke_player_about_chat_msg_gm(&ctx, &mut session, &[PyRep::Integer(char_id), PyRep::String("help".into())])
            .unwrap()
            .unwrap_err()
            .code,
        "UserError"
    );
    session.role = u64::MAX;
    assert!(handle_poke_player_about_chat_msg_gm(&ctx, &mut anon, &[]).unwrap().is_err());
    assert_eq!(
        handle_poke_player_about_chat_msg_gm(&ctx, &mut session, &[PyRep::Integer(0), PyRep::String("help".into())])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidTargetCharacter"
    );
    assert_eq!(
        handle_poke_player_about_chat_msg_gm(&ctx, &mut session, &[PyRep::Integer(char_id), PyRep::String("   ".into())])
            .unwrap()
            .unwrap_err()
            .code,
        "PokeMessageRequired"
    );
    let too_long = "x".repeat(4001);
    assert_eq!(
        handle_poke_player_about_chat_msg_gm(&ctx, &mut session, &[PyRep::Integer(char_id), PyRep::String(too_long)])
            .unwrap()
            .unwrap_err()
            .code,
        "CustomInfo"
    );
    assert_eq!(
        handle_poke_player_about_chat_msg_gm(&ctx, &mut session, &[PyRep::Integer(99_888_777), PyRep::String("corp".into())])
            .unwrap()
            .unwrap_err()
            .code,
        "TargetCharacterNotFound"
    );
}

#[test]
fn handle_mail_headers_direct_syncmail_and_get_headers_mutants() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let mid = 88_001_i64;
    record_mail_meta(&ctx, mid, &[char_id], Some(55), None);
    ctx.db.with_mut_snapshot(|snap| {
        snap.mails.push(MailRecord {
            message_id: mid,
            sender_id: 90_000_002,
            recipient_id: char_id,
            subject: "Direct Header Check".into(),
            body: "Body".into(),
        });
        snap.mail_timestamps.insert(mid, 1_700_000_100);
    });

    let sync_rep = handle_mail_headers(&ctx, &mut session, "SyncMail", &[])
        .expect("SyncMail returns Some")
        .expect("SyncMail returns Ok");
    assert!(sync_rep.dict_get("oldMail").is_some());
    assert!(sync_rep.dict_get("mailStatus").is_some());
    let Some(PyRep::Rowset { rows: sync_rows, .. }) = sync_rep.dict_get("newMail") else {
        panic!("expected newMail Rowset");
    };
    assert_eq!(sync_rows.len(), 1);
    assert_eq!(sync_rows[0][0], PyRep::Integer(mid));

    let hdr_rep = handle_mail_headers(&ctx, &mut session, "GetMailHeaders", &[PyRep::Integer(mid)])
        .expect("GetMailHeaders returns Some")
        .expect("GetMailHeaders returns Ok");
    let PyRep::Rowset { rows, .. } = hdr_rep else {
        panic!("expected Rowset");
    };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][0], PyRep::Integer(mid));
}
