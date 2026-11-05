#![forbid(unsafe_code)]

use super::*;
use crate::services::account_services::account_services_tests::{auth_session, test_ctx};

#[tokio::test]
async fn nav_warp_interdiction_routes_waypoints_and_session_timer_mutations() {
    let ctx = test_ctx();
    let mut sess = auth_session(1, 90_000_001, 98_000_001, 30_000_142);

    assert!(!is_valid_waypoint_destination(&ctx, 0));
    assert!(!is_valid_waypoint_destination(&ctx, -5));
    assert!(is_valid_waypoint_destination(&ctx, 30000142));
    assert!(is_valid_waypoint_destination(&ctx, 60003760));
    assert_eq!(resolve_waypoint_solar_system(&ctx, 60003760), Some(30000142));

    // Sync waypoints from settings
    assert!(sync_waypoints_from_char_settings(
        &ctx,
        90_000_001,
        &PyRep::List(vec![PyRep::Integer(999_999_999)])
    )
    .is_err());

    sync_waypoints_from_char_settings(
        &ctx,
        90_000_001,
        &PyRep::List(vec![PyRep::Integer(30000142), PyRep::Integer(30000144)]),
    )
    .unwrap();
    sync_waypoints_on_login(&ctx, 90_000_001);
    advance_waypoints_on_arrival(&ctx, 90_000_001, 30000142);
    let rem_wps = ctx
        .db
        .with_snapshot(|s| s.characters.get(&90_000_001).unwrap().autopilot_waypoints.clone());
    assert_eq!(rem_wps, vec![30000144]);
    sync_waypoints_to_char_settings(&ctx, 90_000_001, &[]);

    // Route calculation
    assert!(calculate_route_with_waypoints(&ctx, 999_999, &[], &[], "shortest").is_err());
    assert!(calculate_route_with_waypoints(&ctx, 30000142, &[999_999_999], &[], "shortest").is_err());
    assert_eq!(
        calculate_route_with_waypoints(&ctx, 30000142, &[], &[30000142], "shortest")
            .unwrap_err()
            .code,
        "OriginSystemInAvoidList"
    );
    assert_eq!(
        calculate_route_with_waypoints(&ctx, 30000142, &[], &[], "shortest").unwrap(),
        vec![30000142]
    );
    let route = calculate_route_with_waypoints(&ctx, 30000142, &[30000144], &[], "secure").unwrap();
    assert_eq!(route.first().copied(), Some(30000142));
    assert_eq!(route.last().copied(), Some(30000144));

    // Warp landing point resolution
    let ship_pos = DVec3::ZERO;
    let anchor_pos = DVec3::new(1_000_000.0, 0.0, 0.0);
    let station_landing = resolve_warp_landing_point(&ctx, 30000142, 60003760, ship_pos, anchor_pos);
    assert!(station_landing.x <= anchor_pos.x);

    // Ensure not in active warp
    assert!(ensure_not_in_active_warp(&MovementMode::Stop).is_ok());

    // handle_use_warp_vector while docked fails ShipDocked
    sess.station_id = Some(60_003_760);
    sess.ship_id = Some(100_000_101);
    assert_eq!(
        handle_use_warp_vector(&ctx, &mut sess, &[PyRep::Integer(1)])
            .await
            .unwrap_err()
            .code,
        "ShipDocked"
    );

    // handle_set_session_check
    let timer_init = handle_set_session_check(&ctx, &mut sess, &[]).await.unwrap();
    assert_eq!(timer_init.dict_get("active"), Some(&PyRep::Bool(false)));

    assert!(handle_set_session_check(&ctx, &mut sess, &[PyRep::String("invalid_trigger".into())])
        .await
        .is_err());

    let set_timer = handle_set_session_check(&ctx, &mut sess, &[PyRep::String("dock".into())])
        .await
        .unwrap();
    assert_eq!(set_timer.dict_get("sessionChangeTimerSeconds"), Some(&PyRep::Integer(10)));
}
