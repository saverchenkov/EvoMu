//! Authentic state-backed integration tests for In-Game Incident Reporting (`ingamereport`):
//! - `report_exploit_abuse`
//! - `report_gambling`
//! - `report_inappropriate_character_name`
//! - `report_inappropriate_language`
//! - `report_macro_use`
//! - `report_offensive_mail`
//! - `report_other`
//! - `report_questionable_transaction`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::ingamereport_ops;

use crate::common;

const PILOT_1: i64 = 90_000_001;
const PILOT_TARGET: i64 = 90_000_002;
const CORP_1: i64 = 98_000_001;

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

#[tokio::test]
async fn test_ingame_reports_submission_and_persistence() {
    // parity: rpc:ingamereport.report_exploit_abuse
    // parity: rpc:ingamereport.report_gambling
    // parity: rpc:ingamereport.report_inappropriate_character_name
    // parity: rpc:ingamereport.report_inappropriate_language
    // parity: rpc:ingamereport.report_macro_use
    // parity: rpc:ingamereport.report_offensive_mail
    // parity: rpc:ingamereport.report_other
    // parity: rpc:ingamereport.report_questionable_transaction

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ingamereport_ops::clear_reports();

    // 0. Verify unauthenticated session rejection
    let mut unauth_sess = SessionState::new(35_001);
    let unauth_err = rpc(
        &ctx,
        &mut unauth_sess,
        "ingamereport",
        "report_macro_use",
        &[PyRep::Integer(PILOT_TARGET)],
    )
    .await
    .expect_err("Must require character session");
    assert_eq!(unauth_err.code, "Unauthorized");

    // Authenticate PILOT_1
    let mut sess = SessionState::new(35_002);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(PILOT_1)]).await.expect("SelectCharacterID");
    sess.corporation_id = Some(CORP_1);

    // 1. report_macro_use
    let r1 = rpc(
        &ctx,
        &mut sess,
        "ingamereport",
        "report_macro_use",
        &[
            PyRep::Integer(PILOT_TARGET),
            PyRep::String("Suspected mining bot".into()),
            PyRep::String("Automated warping and unanchoring loop in belt 2".into()),
        ],
    )
    .await
    .expect("report_macro_use");
    assert_eq!(r1, PyRep::None);

    // 2. report_exploit_abuse
    let r2 = rpc(
        &ctx,
        &mut sess,
        "ingamereport",
        "report_exploit_abuse",
        &[
            PyRep::Integer(PILOT_TARGET),
            PyRep::String("Geometry clipping exploit".into()),
            PyRep::String("Pilot clipping inside station model to avoid lock".into()),
        ],
    )
    .await
    .expect("report_exploit_abuse");
    assert_eq!(r2, PyRep::None);

    // 3. report_gambling
    let r3 = rpc(
        &ctx,
        &mut sess,
        "ingamereport",
        "report_gambling",
        &[
            PyRep::Integer(PILOT_TARGET),
            PyRep::String("Unauthorized casino promotion".into()),
            PyRep::String("Advertising external gambling link in local chat".into()),
        ],
    )
    .await
    .expect("report_gambling");
    assert_eq!(r3, PyRep::None);

    // 4. report_inappropriate_character_name
    let r4 = rpc(
        &ctx,
        &mut sess,
        "ingamereport",
        "report_inappropriate_character_name",
        &[
            PyRep::Integer(PILOT_TARGET),
            PyRep::String("Hate speech pilot name".into()),
            PyRep::String("Violates naming policy guidelines".into()),
        ],
    )
    .await
    .expect("report_inappropriate_character_name");
    assert_eq!(r4, PyRep::None);

    // 5. report_inappropriate_language
    let r5 = rpc(
        &ctx,
        &mut sess,
        "ingamereport",
        "report_inappropriate_language",
        &[
            PyRep::Integer(PILOT_TARGET),
            PyRep::String("Severe harassment in Jita local".into()),
            PyRep::String("Repeated toxic abuse directed at newer players".into()),
        ],
    )
    .await
    .expect("report_inappropriate_language");
    assert_eq!(r5, PyRep::None);

    // 6. report_offensive_mail
    let mail_id = 450_001;
    let r6 = rpc(
        &ctx,
        &mut sess,
        "ingamereport",
        "report_offensive_mail",
        &[
            PyRep::Integer(mail_id),
            PyRep::String("Abusive EVE mail".into()),
            PyRep::String("Extortion message containing prohibited real-life threats".into()),
        ],
    )
    .await
    .expect("report_offensive_mail");
    assert_eq!(r6, PyRep::None);

    // 7. report_other
    let r7 = rpc(
        &ctx,
        &mut sess,
        "ingamereport",
        "report_other",
        &[
            PyRep::Integer(PILOT_TARGET),
            PyRep::String("General griefing".into()),
            PyRep::String("Repeatedly blocking undock with unpiloted cans".into()),
        ],
    )
    .await
    .expect("report_other");
    assert_eq!(r7, PyRep::None);

    // 8. report_questionable_transaction
    let txn_id = 999_888;
    let r8 = rpc(
        &ctx,
        &mut sess,
        "ingamereport",
        "report_questionable_transaction",
        &[
            PyRep::Integer(txn_id),
            PyRep::String("Suspected RMT transaction".into()),
            PyRep::String("Market order purchased at 1000x regular pricing".into()),
        ],
    )
    .await
    .expect("report_questionable_transaction");
    assert_eq!(r8, PyRep::None);

    // State Assertions: verify all 8 reports were persisted with authentic metadata
    let all_reports = ingamereport_ops::get_all_reports();
    assert_eq!(all_reports.len(), 8, "All 8 reports must be stored in state");

    let categories: Vec<&str> = all_reports.iter().map(|r| r.category.as_str()).collect();
    assert!(categories.contains(&"macro_use"));
    assert!(categories.contains(&"exploit_abuse"));
    assert!(categories.contains(&"gambling"));
    assert!(categories.contains(&"inappropriate_character_name"));
    assert!(categories.contains(&"inappropriate_language"));
    assert!(categories.contains(&"offensive_mail"));
    assert!(categories.contains(&"other"));
    assert!(categories.contains(&"questionable_transaction"));

    for report in &all_reports {
        assert_eq!(report.reporter_char_id, PILOT_1);
        assert_eq!(report.reporter_corp_id, CORP_1);
        assert!(report.reported_entity_id > 0);
        assert!(!report.reason.is_empty());
        assert!(!report.details.is_empty());
        assert!(report.created_at > 0);
    }

    // Verify DB owner notes persistence under character -78
    ctx.db.with_snapshot(|snap| {
        let db_reports: Vec<_> = snap
            .owner_notes
            .values()
            .filter(|n| n.character_id == -78)
            .collect();
        assert_eq!(db_reports.len(), 8, "8 reports must be persisted in owner_notes");
    });
}
