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
}
