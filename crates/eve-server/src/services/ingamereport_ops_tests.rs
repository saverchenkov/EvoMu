#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use std::collections::HashMap;

#[test]
fn ingamereport_ops_dispatch_and_persistence_mutations() {
    let ctx = test_ctx();
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    assert!(handle(&ctx, &sess, "unknown_report_method", &[], &kw).is_none());

    let unauth = SessionState::default();
    assert!(handle(&ctx, &unauth, "report_exploit_abuse", &[], &kw)
        .unwrap()
        .is_err());

    let methods = [
        ("report_exploit_abuse", "exploit_abuse"),
        ("report_gambling", "gambling"),
        ("report_inappropriate_character_name", "inappropriate_character_name"),
        ("report_inappropriate_language", "inappropriate_language"),
        ("report_macro_use", "macro_use"),
        ("report_offensive_mail", "offensive_mail"),
        ("report_other", "other"),
        ("report_questionable_transaction", "questionable_transaction"),
    ];

    for (i, (m, cat)) in methods.iter().enumerate() {
        let target_id = 90_000_100 + (i as i64);
        let res = handle(
            &ctx,
            &sess,
            m,
            &[
                PyRep::Integer(target_id),
                PyRep::String(format!("reason_{cat}")),
                PyRep::String(format!("details_{cat}")),
            ],
            &kw,
        )
        .unwrap()
        .unwrap();
        assert_eq!(res, PyRep::None);
    }

    // Kwargs extraction path
    let mut kw2 = HashMap::new();
    kw2.insert("targetID".into(), PyRep::Integer(90_000_999));
    kw2.insert("reason".into(), PyRep::String("kw_reason".into()));
    kw2.insert("comment".into(), PyRep::String("kw_comment".into()));
    assert_eq!(
        handle(&ctx, &sess, "report_other", &[], &kw2).unwrap().unwrap(),
        PyRep::None
    );

    let reports = get_persisted_reports(&ctx);
    assert!(reports.len() >= 9);
    assert!(reports.iter().any(|r| r.category == "exploit_abuse" && r.reason == "reason_exploit_abuse"));
    assert!(reports.iter().any(|r| r.reported_entity_id == 90_000_999 && r.details == "kw_comment"));

    // V24.01 5-arg `report_inappropriate_language(charID, comments, chat_time, chat_channel, chat_content)`
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "report_inappropriate_language",
            &[
                PyRep::Integer(90_000_777),
                PyRep::String("toxic local spam".into()),
                PyRep::Integer(1_700_000_123),
                PyRep::String("local_30000142".into()),
                PyRep::String("bad message line".into()),
            ],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    let reports_after = get_persisted_reports(&ctx);
    assert!(reports_after.iter().any(|r| {
        r.reported_entity_id == 90_000_777
            && r.category == "inappropriate_language"
            && r.reason == "toxic local spam"
            && r.details.contains("chat_channel=local_30000142")
            && r.details.contains("chat_content=bad message line")
    }));
}

#[test]
fn ingamereport_ops_kwargs_aliases_numeric_channel_and_legacy_fallback() {
    let ctx = test_ctx();

    // 1. Empty ingamereport_tickets triggers legacy owner_notes (-78) fallback in get_persisted_reports
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleet_and_social_ops.ingamereport_tickets.clear();
        let legacy = InGameReportRecord {
            report_id: 42,
            reporter_char_id: 90_000_001,
            reporter_corp_id: 0,
            reported_entity_id: 90_000_444,
            category: "macro_use".into(),
            reason: "legacy_reason".into(),
            details: "legacy_details".into(),
            created_at: 1_700_000_000,
        };
        snap.owner_notes.insert(
            780_000_042,
            eve_db::OwnerNoteRecord {
                note_id: 780_000_042,
                character_id: -78,
                item_id: 90_000_444,
                label: "report:macro_use:42".into(),
                note: serde_json::to_string(&legacy).unwrap(),
                timestamp_unix: 1_700_000_000,
            },
        );
        // Invalid JSON under -78 and unrelated character notes are ignored cleanly
        snap.owner_notes.insert(
            780_000_043,
            eve_db::OwnerNoteRecord {
                note_id: 780_000_043,
                character_id: -78,
                item_id: 0,
                label: "bad".into(),
                note: "{not-json".into(),
                timestamp_unix: 1_700_000_000,
            },
        );
        snap.owner_notes.insert(
            780_000_044,
            eve_db::OwnerNoteRecord {
                note_id: 780_000_044,
                character_id: 90_000_001,
                item_id: 0,
                label: "player-note".into(),
                note: serde_json::to_string(&legacy).unwrap(),
                timestamp_unix: 1_700_000_000,
            },
        );
    });
    let legacy_loaded = get_persisted_reports(&ctx);
    assert_eq!(legacy_loaded.len(), 1);
    assert_eq!(legacy_loaded[0].report_id, 42);
    assert_eq!(legacy_loaded[0].reported_entity_id, 90_000_444);

    // 2. Unauth character error when user_id is set but character_id is None
    let mut no_char = SessionState::new(9001);
    no_char.user_id = Some(1);
    assert!(handle(&ctx, &no_char, "report_other", &[], &HashMap::new())
        .unwrap()
        .is_err());

    // Session with corporation_id == None falls back to 0
    let mut sess = auth_session(1, 90_000_001, 0, 30_000_142);
    sess.corporation_id = None;

    // 3. reportedID + category + details aliases
    let mut kw_rep = HashMap::new();
    kw_rep.insert("reportedID".into(), PyRep::Integer(90_001_111));
    kw_rep.insert("category".into(), PyRep::String("cat_alias".into()));
    kw_rep.insert("details".into(), PyRep::String("det_alias".into()));
    assert_eq!(
        handle(&ctx, &sess, "report_gambling", &[], &kw_rep)
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // characterID / mailID / transactionID + comments / text aliases + omitted args fallback
    for (key, id, text_key) in [
        ("characterID", 90_001_222_i64, "text"),
        ("mailID", 90_001_333_i64, "details"),
        ("transactionID", 90_001_444_i64, "comment"),
    ] {
        let mut kw_i = HashMap::new();
        kw_i.insert(key.into(), PyRep::Integer(id));
        kw_i.insert("comments".into(), PyRep::String(format!("cmt_{id}")));
        kw_i.insert(text_key.into(), PyRep::String(format!("txt_{id}")));
        assert_eq!(
            handle(&ctx, &sess, "report_offensive_mail", &[], &kw_i)
                .unwrap()
                .unwrap(),
            PyRep::None
        );
    }
    assert_eq!(
        handle(&ctx, &sess, "report_other", &[], &HashMap::new())
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // 4. V24 5-arg chat with string chatTimestamp & numeric channel_id + kwargs-only chat path
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "report_inappropriate_language",
            &[
                PyRep::Integer(90_001_555),
                PyRep::String("spam".into()),
                PyRep::String("2026-11-09T12:00:00Z".into()),
                PyRep::Integer(30_000_142),
                PyRep::String("numeric chan msg".into()),
            ],
            &HashMap::new(),
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );

    // args.len() >= 4 with empty chat strings preserves kwargs["details"]
    let mut kw_empty_chat = HashMap::new();
    kw_empty_chat.insert("details".into(), PyRep::String("plain_details".into()));
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "report_inappropriate_character_name",
            &[
                PyRep::Integer(90_001_666),
                PyRep::String("bad name".into()),
                PyRep::None,
                PyRep::String(String::new()),
            ],
            &kw_empty_chat,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );

    // Kwargs-only chat (< 4 positional args) with numeric chat_channel
    let mut kw_chat = HashMap::new();
    kw_chat.insert("reportedID".into(), PyRep::Integer(90_001_777));
    kw_chat.insert("chat_time".into(), PyRep::String("1775000001".into()));
    kw_chat.insert("chat_channel".into(), PyRep::Integer(42));
    kw_chat.insert("chat_content".into(), PyRep::String("kw chat".into()));
    assert_eq!(
        handle(&ctx, &sess, "report_inappropriate_language", &[], &kw_chat)
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    let all = get_persisted_reports(&ctx);
    assert!(all.iter().any(|r| {
        r.reported_entity_id == 90_001_111
            && r.reporter_corp_id == 0
            && r.reason == "cat_alias"
            && r.details == "det_alias"
    }));
    assert!(all.iter().any(|r| r.reported_entity_id == 90_001_222 && r.details == "txt_90001222"));
    assert!(all.iter().any(|r| r.reported_entity_id == 90_001_333 && r.details == "txt_90001333"));
    assert!(all.iter().any(|r| r.reported_entity_id == 90_001_444 && r.details == "txt_90001444"));
    assert!(all.iter().any(|r| r.reported_entity_id == 0 && r.reason.is_empty() && r.details.is_empty()));
    assert!(all.iter().any(|r| {
        r.reported_entity_id == 90_001_555
            && r.details == "chat_time=2026-11-09T12:00:00Z; chat_channel=30000142; chat_content=numeric chan msg"
    }));
    assert!(all.iter().any(|r| r.reported_entity_id == 90_001_666 && r.details == "plain_details"));
    assert!(all.iter().any(|r| {
        r.reported_entity_id == 90_001_777
            && r.details == "chat_time=1775000001; chat_channel=42; chat_content=kw chat"
    }));
}

#[test]
fn mutant_kill_persisted_reports_sort_precedence_and_submit_roundtrip() {
    let ctx = test_ctx();
    let sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);

    // Non-empty ingamereport_tickets takes precedence over legacy -78 owner_notes and sorts ascending by report_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleet_and_social_ops.ingamereport_tickets = vec![
            InGameReportRecord {
                report_id: 200,
                reporter_char_id: 90_000_001,
                reporter_corp_id: 98_000_001,
                reported_entity_id: 222,
                category: "other".into(),
                reason: "second".into(),
                details: "d2".into(),
                created_at: 10,
            },
            InGameReportRecord {
                report_id: 100,
                reporter_char_id: 90_000_001,
                reporter_corp_id: 98_000_001,
                reported_entity_id: 111,
                category: "other".into(),
                reason: "first".into(),
                details: "d1".into(),
                created_at: 5,
            },
        ];
        let legacy = InGameReportRecord {
            report_id: 999,
            reporter_char_id: 90_000_001,
            reporter_corp_id: 0,
            reported_entity_id: 999,
            category: "macro_use".into(),
            reason: "ignored_when_tickets_present".into(),
            details: String::new(),
            created_at: 1,
        };
        snap.owner_notes.insert(
            780_999,
            eve_db::OwnerNoteRecord {
                note_id: 780_999,
                character_id: -78,
                item_id: 999,
                label: "report:macro_use:999".into(),
                note: serde_json::to_string(&legacy).unwrap(),
                timestamp_unix: 1,
            },
        );
    });
    let primary = get_persisted_reports(&ctx);
    assert_eq!(primary.iter().map(|r| r.report_id).collect::<Vec<_>>(), vec![100, 200]);

    // Empty ingamereport_tickets falls back to -78 owner_notes sorted ascending by report_id
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleet_and_social_ops.ingamereport_tickets.clear();
        let older = InGameReportRecord {
            report_id: 12,
            reporter_char_id: 90_000_001,
            reporter_corp_id: 0,
            reported_entity_id: 12,
            category: "gambling".into(),
            reason: "older".into(),
            details: String::new(),
            created_at: 1,
        };
        snap.owner_notes.insert(
            780_012,
            eve_db::OwnerNoteRecord {
                note_id: 780_012,
                character_id: -78,
                item_id: 12,
                label: "report:gambling:12".into(),
                note: serde_json::to_string(&older).unwrap(),
                timestamp_unix: 1,
            },
        );
    });
    let fallback = get_persisted_reports(&ctx);
    assert_eq!(fallback.iter().map(|r| r.report_id).collect::<Vec<_>>(), vec![12, 999]);

    // Submit via handle returns PyRep::None and appends report record
    let mut kw = HashMap::new();
    kw.insert("details".into(), PyRep::String("auto_miner".into()));
    assert_eq!(
        handle(
            &ctx,
            &sess,
            "report_macro_use",
            &[PyRep::Integer(90_009_111), PyRep::String("botting".into())],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    let after = get_persisted_reports(&ctx);
    assert!(after.iter().any(|r| {
        r.category == "macro_use"
            && r.reported_entity_id == 90_009_111
            && r.reason == "botting"
            && r.details == "auto_miner"
    }));
}
