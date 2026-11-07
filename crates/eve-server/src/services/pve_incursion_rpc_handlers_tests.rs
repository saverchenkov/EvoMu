//! Semantic unit tests split from `pve_incursion_sites_tests`.

use super::*;
use crate::services::pve_special_sites::with_pve_special_reg;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

#[tokio::test]
async fn test_incursion_rpc_handlers() {
    let ctx = build_test_ctx();
    let mut unauth = SessionState::new(1);
    let err = handle_incursion_rpc(&ctx, &mut unauth, "incursion", "GetIncursionLPLog", &[], &HashMap::new())
        .await
        .unwrap()
        .unwrap_err();
    assert!(!err.code.is_empty());

    let char_id = 92_000_100;
    ctx.db
        .provision_gm_account("inc_rpc", "pw", char_id, "IncRpc");
    let uid = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id).unwrap_or(1));
    let mut sess = SessionState::new(2);
    sess.user_id = Some(uid);
    sess.character_id = Some(char_id);

    // Empty LP log -> empty list for GetIncursionLPLog, PyRep::None for UpdateIncursionLPLog
    let empty_get = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "incursion",
        "GetIncursionLPLog",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(empty_get, PyRep::List(ref v) if v.is_empty()));

    let empty_upd = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "incursion",
        "UpdateIncursionLPLog",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(empty_upd, PyRep::None);

    // Insert 2 LP log entries and query with filters
    with_pve_special_reg(&ctx, |reg| {
        reg.incursion_lp_logs.insert(
            char_id,
            vec![
                IncursionLPLogEntry {
                    log_id: 1,
                    character_id: char_id,
                    site_id: "INC-20000001-5165".into(),
                    tier: "Vanguard".into(),
                    constellation_id: 20_000_001,
                    solar_system_id: 30_000_001,
                    isk_earned: 10_395_000,
                    concord_lp_earned: 1_400,
                    fleet_size: 10,
                    fleet_scale_factor: 1.0,
                    timestamp_filetime: 0,
                },
                IncursionLPLogEntry {
                    log_id: 2,
                    character_id: char_id,
                    site_id: "INC-20000002-5168".into(),
                    tier: "Assault".into(),
                    constellation_id: 20_000_002,
                    solar_system_id: 30_000_002,
                    isk_earned: 18_200_000,
                    concord_lp_earned: 3_500,
                    fleet_size: 15,
                    fleet_scale_factor: 1.0,
                    timestamp_filetime: 133_000_000_000_000_000,
                },
            ],
        );
    });

    let all_logs = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "incursion",
        "GetIncursionLPLog",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    match all_logs {
        PyRep::List(rows) => assert_eq!(rows.len(), 2),
        other => panic!("expected list, got {other:?}"),
    }

    let by_site = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "incursion",
        "UpdateIncursionLPLog",
        &[PyRep::String("INC-20000001-5165".into())],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    match by_site {
        PyRep::List(rows) => {
            assert_eq!(rows.len(), 1);
            assert!(rows[0].dict_get("timestamp").and_then(|v| v.as_i64()).unwrap() > 0);
        }
        other => panic!("expected list, got {other:?}"),
    }

    let by_cid = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "incursion",
        "UpdateIncursionLPLog",
        &[PyRep::Integer(20_000_002)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    match by_cid {
        PyRep::List(rows) => assert_eq!(rows.len(), 1),
        other => panic!("expected list, got {other:?}"),
    }

    // taleMgr RPCs
    let global_tales = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "taleMgr",
        "GetGlobalWorldEventTales",
        &[],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    match global_tales {
        PyRep::List(rows) => assert!(!rows.is_empty()),
        other => panic!("expected list, got {other:?}"),
    }

    let tpl_tales = handle_incursion_rpc(
        &ctx,
        &mut sess,
        "taleMgr",
        "get_active_tales_by_template",
        &[PyRep::Integer(INCURSION_TALE_TEMPLATE_ID as i64)],
        &HashMap::new(),
    )
    .await
    .unwrap()
    .unwrap();
    match tpl_tales {
        PyRep::List(rows) => assert_eq!(rows.len(), 3),
        other => panic!("expected 3 incursion tales, got {other:?}"),
    }

    assert!(
        handle_incursion_rpc(&ctx, &mut sess, "", "", &[], &HashMap::new())
            .await
            .is_none()
    );
}
