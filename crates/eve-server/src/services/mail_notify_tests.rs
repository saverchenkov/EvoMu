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
