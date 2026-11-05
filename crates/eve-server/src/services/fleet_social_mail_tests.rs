use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_and_clock,
};

fn make_session(char_id: i64, corp_id: i64) -> SessionState {
    SessionState {
        user_id: Some(1),
        character_id: Some(char_id),
        corporation_id: Some(corp_id),
        solar_system_id: Some(30_000_142),
        corp_role: u64::MAX,
        ..Default::default()
    }
}

#[test]
fn extract_label_bits_and_mail_lifecycle() {
    assert_eq!(extract_label_bits(Some(&PyRep::Integer(5))), vec![1, 4]);
    assert_eq!(
        extract_label_bits(Some(&PyRep::List(vec![PyRep::Integer(2), PyRep::Integer(8)]))),
        vec![2, 8]
    );
    assert!(extract_label_bits(None).is_empty());

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let sender_id = 90_000_001;
    let recip_id = 90_000_002;
    ctx.db.with_mut_snapshot(|snap| {
        let r = clone_char_with(snap, recip_id, "Mail Recipient", 1_000_009, 30_000_142, 100_000_002);
        snap.characters.insert(recip_id, r);
    });
    let mut sender_sess = make_session(sender_id, 1_000_009);
    let mut recip_sess = make_session(recip_id, 1_000_009);
    let kw = HashMap::new();
    let svc = "mailMgr";

    assert!(handle_mail_and_calendar(&ctx, &mut sender_sess, "other_svc", "SendMail", &[], &kw).is_none());

    let sent = handle_mail_and_calendar(
        &ctx,
        &mut sender_sess,
        svc,
        "SendMail",
        &[
            PyRep::List(vec![PyRep::Integer(recip_id)]),
            PyRep::String("hello pilot".into()),
            PyRep::String("fly safe out there".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let mail_id = sent
        .as_i64()
        .or_else(|| sent.dict_get("messageID").and_then(|v| v.as_i64()))
        .unwrap();

    let mark_read = handle_mark_as_read(&ctx, &recip_sess, &[PyRep::List(vec![PyRep::Integer(mail_id)])])
        .unwrap()
        .unwrap();
    assert_eq!(mark_read.dict_get("read").and_then(|v| v.as_bool()), Some(true));

    let mark_unread = handle_mark_as_unread(&ctx, &recip_sess, &[PyRep::List(vec![PyRep::Integer(mail_id)])])
        .unwrap()
        .unwrap();
    assert_eq!(mark_unread.dict_get("read").and_then(|v| v.as_bool()), Some(false));

    let by_lbl_read = handle_mark_as_read_by_label(&ctx, &recip_sess, &[PyRep::Integer(1)])
        .unwrap()
        .unwrap();
    assert_eq!(by_lbl_read.dict_get("read").and_then(|v| v.as_bool()), Some(true));

    let by_lbl_unread = handle_mark_as_unread_by_label(&ctx, &recip_sess, &[PyRep::Integer(1)])
        .unwrap()
        .unwrap();
    assert_eq!(by_lbl_unread.dict_get("read").and_then(|v| v.as_bool()), Some(false));

    let assign = handle_mail_and_calendar(
        &ctx,
        &mut recip_sess,
        svc,
        "AssignLabels",
        &[PyRep::List(vec![PyRep::Integer(mail_id)]), PyRep::Integer(2)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(assign.dict_get("assigned").and_then(|v| v.as_bool()), Some(true));

    let rem = handle_mail_and_calendar(
        &ctx,
        &mut recip_sess,
        svc,
        "RemoveLabels",
        &[PyRep::List(vec![PyRep::Integer(mail_id)]), PyRep::Integer(2)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(rem.dict_get("removed").and_then(|v| v.as_bool()), Some(true));
}
