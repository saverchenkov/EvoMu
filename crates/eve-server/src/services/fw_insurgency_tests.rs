#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};
use std::collections::HashMap;

#[tokio::test]
async fn fw_insurgency_hub_and_scan_mutations() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let kw = HashMap::new();

    let unknown_svc = "other";
    let unknown_method = "UnknownMethod";
    assert!(handle_hub_and_scan(&ctx, &mut sess, unknown_svc, "ConeScan", &[], &kw)
        .await
        .is_none());
    assert!(handle_hub_and_scan(&ctx, &mut sess, "scanSvc", unknown_method, &[], &kw)
        .await
        .is_none());

    let mut unauth = SessionState::default();
    assert!(handle_hub_and_scan(&ctx, &mut unauth, "scanSvc", "ConeScan", &[], &kw)
        .await
        .unwrap()
        .is_err());

    // Without solar_system_id, requires NoSolarSystem error
    sess.solar_system_id = None;
    assert!(handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "SaveUpgradeConfiguration", &[], &kw)
        .await
        .unwrap()
        .is_err());
    assert!(handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "GetInstalledUpgradesLocal", &[], &kw)
        .await
        .unwrap()
        .is_err());
    assert!(handle_hub_and_scan(&ctx, &mut sess, "scanSvc", "GetScanResults", &[], &kw)
        .await
        .unwrap()
        .is_err());
    assert!(handle_hub_and_scan(&ctx, &mut sess, "scanSvc", "ConeScan", &[], &kw)
        .await
        .unwrap()
        .is_err());

    // High-sec system (30000142 Jita) rejects SaveUpgradeConfiguration with NotNullSecSystem
    sess.solar_system_id = Some(30000142);
    let err = handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "SaveUpgradeConfiguration", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err.code, "NotNullSecSystem");

    // GetInstalledUpgradesLocal in 30000142
    let installed = handle_hub_and_scan(&ctx, &mut sess, "sovHubSvc", "GetInstalledUpgradesLocal", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(installed.dict_get("solarSystemID"), Some(&PyRep::Integer(30000142)));
    assert!(matches!(installed.dict_get("upgrades"), Some(PyRep::List(_))));

    // GetScanResults in 30000142
    let scan_res = handle_hub_and_scan(&ctx, &mut sess, "scanSvc", "GetScanResults", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scan_res.dict_get("solarSystemID"), Some(&PyRep::Integer(30000142)));
    assert!(matches!(scan_res.dict_get("anomalies"), Some(PyRep::List(_))));
    assert!(matches!(scan_res.dict_get("signatures"), Some(PyRep::List(_))));

    // ConeScan in abyssal / zarzakh is forbidden
    sess.solar_system_id = Some(30_100_000);
    assert!(handle_hub_and_scan(&ctx, &mut sess, "scanSvc", "ConeScan", &[], &kw)
        .await
        .unwrap()
        .is_err());
    sess.solar_system_id = Some(32_000_001);
    assert!(handle_hub_and_scan(&ctx, &mut sess, "scanSvc", "ConeScan", &[], &kw)
        .await
        .unwrap()
        .is_err());

    // ConeScan in normal space succeeds
    sess.solar_system_id = Some(30000142);
    let cone = handle_hub_and_scan(
        &ctx,
        &mut sess,
        "scanSvc",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(1.0e12)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(cone, PyRep::List(_)));
}
