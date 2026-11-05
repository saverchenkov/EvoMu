#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};

#[tokio::test]
async fn mobile_deployables_ext_restrictions_and_helpers_mutations() {
    assert!(is_single_use_deployable(GROUP_MOBILE_MICRO_JUMP_UNIT));
    assert!(is_single_use_deployable(GROUP_MOBILE_CYNO_BEACON));
    assert!(is_single_use_deployable(GROUP_MOBILE_SCAN_INHIBITOR));
    assert!(is_single_use_deployable(GROUP_MOBILE_OBSERVATORY));
    assert!(is_single_use_deployable(GROUP_MOBILE_CYNO_INHIBITOR));
    assert!(!is_single_use_deployable(GROUP_MOBILE_DEPOT));

    assert_eq!(total_lifetime_secs(0, GROUP_MOBILE_DEPOT, 60), DEPOT_LIFETIME_SECS);
    assert_eq!(total_lifetime_secs(0, GROUP_MOBILE_TRACTOR, 10), MTU_LIFETIME_SECS);
    assert_eq!(
        total_lifetime_secs(0, GROUP_MOBILE_MICRO_JUMP_UNIT, 60),
        60 + MMJU_LIFETIME_SECS
    );
    assert_eq!(
        total_lifetime_secs(TYPE_COVERT_MOBILE_CYNO_BEACON, GROUP_MOBILE_CYNO_BEACON, 60),
        60 + COVERT_CYNO_BEACON_DURATION_SECS
    );
    assert_eq!(
        total_lifetime_secs(0, GROUP_MOBILE_CYNO_BEACON, 60),
        60 + CYNO_BEACON_DURATION_SECS
    );
    assert_eq!(
        total_lifetime_secs(TYPE_MOBILE_OBSERVATORY, GROUP_MOBILE_OBSERVATORY, 600),
        600 + OBSERVATORY_ACTIVE_SECS
    );
    assert_eq!(
        total_lifetime_secs(TYPE_MOBILE_SCAN_INHIBITOR, GROUP_MOBILE_SCAN_INHIBITOR, 60),
        60 + SCAN_INHIBITOR_ACTIVE_SECS
    );
    assert_eq!(total_lifetime_secs(0, GROUP_MOBILE_CYNO_INHIBITOR, 60), 3_600);
    assert_eq!(total_lifetime_secs(0, GROUP_MOBILE_SIPHON, 60), SIPHON_LIFETIME_SECS);

    let ctx = test_ctx();
    assert!(default_deploy_delay_secs(&ctx.sde, TYPE_MOBILE_OBSERVATORY, GROUP_MOBILE_OBSERVATORY) > 0.0);
    assert_eq!(default_deploy_delay_secs(&ctx.sde, 999_999_999, GROUP_MOBILE_TRACTOR), 10.0);

    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);
    let pos = DVec3::new(1.0e10, 1.0e10, 1.0e10);

    // Cyno beacon without fleet fails MustBeInFleet
    let err = validate_ext_deployment_restrictions(
        &ctx,
        &sess,
        1,
        TYPE_COVERT_MOBILE_CYNO_BEACON,
        GROUP_MOBILE_CYNO_BEACON,
        30000142,
        pos,
    )
    .unwrap_err();
    assert_eq!(err.code, "MustBeInFleet");

    // In fleet, cyno beacon in highsec fails CannotLightCynoInSystem
    sess.fleet_id = Some(5001);
    let err_hs = validate_ext_deployment_restrictions(
        &ctx,
        &sess,
        1,
        TYPE_COVERT_MOBILE_CYNO_BEACON,
        GROUP_MOBILE_CYNO_BEACON,
        30000142,
        pos,
    )
    .unwrap_err();
    assert_eq!(err_hs.code, "CannotLightCynoInSystem");

    // Observatory restrictions
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            1,
            TYPE_MOBILE_OBSERVATORY,
            GROUP_MOBILE_OBSERVATORY,
            30_100_000,
            pos
        )
        .unwrap_err()
        .code,
        "CantAnchorInZarzakh"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            1,
            TYPE_MOBILE_OBSERVATORY,
            GROUP_MOBILE_OBSERVATORY,
            31_000_001,
            pos
        )
        .unwrap_err()
        .code,
        "CannotDeployInWormhole"
    );
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            1,
            TYPE_MOBILE_OBSERVATORY,
            GROUP_MOBILE_OBSERVATORY,
            30000142,
            pos
        )
        .unwrap_err()
        .code,
        "CannotDeployInHighSec"
    );

    // Scan inhibitor far from gates/stations in Jita succeeds
    assert!(validate_ext_deployment_restrictions(
        &ctx,
        &sess,
        1,
        TYPE_MOBILE_SCAN_INHIBITOR,
        GROUP_MOBILE_SCAN_INHIBITOR,
        30000142,
        pos,
    )
    .is_ok());

    // Siphon in highsec fails
    assert_eq!(
        validate_ext_deployment_restrictions(
            &ctx,
            &sess,
            1,
            33475,
            GROUP_MOBILE_SIPHON,
            30000142,
            pos
        )
        .unwrap_err()
        .code,
        "CantAnchorInHighSec"
    );

    // Cloak tracking & scan inhibitor position check
    record_ship_cloak_activated(&ctx, 555_001);
    assert!(read_deployable_state(&ctx).cloak_activated_unix.contains_key(&555_001));
    clear_ship_cloak_activated(&ctx, 555_001);
    assert!(!read_deployable_state(&ctx).cloak_activated_unix.contains_key(&555_001));

    let inhibs = vec![(100, DVec3::ZERO)];
    assert!(is_pos_scan_inhibited(&inhibs, DVec3::new(10_000.0, 0.0, 0.0)));
    assert!(!is_pos_scan_inhibited(&inhibs, DVec3::new(50_000.0, 0.0, 0.0)));

    assert!(active_scan_inhibitor_positions(&ctx, 30000142).is_empty());

    let mmju_err = handle_mmju_activate(&ctx, &sess, 90_000_001, 1, 30000142, 999_999)
        .await
        .unwrap_err();
    assert_eq!(mmju_err.code, "ItemNotFound");

    step_system_deployables_sync(&ctx, 30000142);
    sync_system_deployables(&ctx, 30000142).await;
}
