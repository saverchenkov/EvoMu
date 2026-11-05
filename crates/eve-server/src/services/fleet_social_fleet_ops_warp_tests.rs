//! Unit tests for `fleet_social_fleet_ops` regroup, formation geometry, and fleet warp.

use super::super::*;
use super::{seed_multi_member_fleet, test_ctx_with_clock};
use eve_db::BookmarkRecord;
use eve_machonet::SessionState;
use std::collections::{HashMap, HashSet};

#[test]
fn oriented_formation_offsets_cover_all_shapes_and_degenerate_heading() {
    // Point formation (0) or slot 0 -> origin
    assert_eq!(
        compute_oriented_formation_offset(0, 2, 5_000.0, 10_000.0, DVec3::X, DVec3::Y),
        DVec3::ZERO
    );
    assert_eq!(
        compute_oriented_formation_offset(1, 0, 5_000.0, 10_000.0, DVec3::X, DVec3::Y),
        DVec3::ZERO
    );

    // Wall formation (1) with slot <= 12 and slot > 12 + degenerate ZERO heading fallback
    let wall1 = compute_oriented_formation_offset(1, 1, 5_000.0, 10_000.0, DVec3::ZERO, DVec3::ZERO);
    assert!((wall1.length() - 5_000.0).abs() < 1e-6);
    assert!(wall1.x.abs() < 1e-6);
    let wall_far = compute_oriented_formation_offset(1, 15, 25_000.0, 25_000.0, DVec3::Y, DVec3::ZERO);
    assert!(wall_far.length() <= 24_000.0 + 1e-6);

    // Sphere formation (2)
    let sph1 = compute_oriented_formation_offset(2, 1, 8_000.0, 10_000.0, DVec3::Z, DVec3::ZERO);
    let sph2 = compute_oriented_formation_offset(2, 2, 8_000.0, 10_000.0, DVec3::Z, DVec3::ZERO);
    assert!((sph1.length() - 10_000.0).abs() < 1e-3);
    assert!((sph2.length() - 10_000.0).abs() < 1e-3);
    assert!(sph1.distance(sph2) > 5_000.0);

    // Arrow V-wedge formation (3) trailing behind forward heading
    let arr1 = compute_oriented_formation_offset(3, 1, 5_000.0, 10_000.0, DVec3::X, DVec3::ZERO);
    let arr2 = compute_oriented_formation_offset(3, 2, 5_000.0, 10_000.0, DVec3::X, DVec3::ZERO);
    assert!((arr1.x - -5_000.0).abs() < 1e-6);
    assert!((arr2.x - -5_000.0).abs() < 1e-6);
    assert!((arr1.z + arr2.z).abs() < 1e-6);

    // Diamond formation (4) cycles all 6 octahedron axes
    let axes: Vec<DVec3> = (1..=6)
        .map(|s| compute_oriented_formation_offset(4, s, 5_000.0, 10_000.0, DVec3::X, DVec3::ZERO))
        .collect();
    assert!((axes[0] + axes[1]).length() < 1e-6);
    assert!((axes[2] + axes[3]).length() < 1e-6);
    assert!((axes[4] + axes[5]).length() < 1e-6);

    // Relative formation (5) preserves relative delta clamped at 24km
    let rel = compute_oriented_formation_offset(
        5,
        1,
        5_000.0,
        10_000.0,
        DVec3::X,
        DVec3::new(30_000.0, 0.0, 40_000.0),
    );
    assert!((rel.length() - 24_000.0).abs() < 1e-6);
}

#[tokio::test]
async fn fleet_ops_regroup_filters_scope_grid_and_opt_outs() {
    let ctx = test_ctx_with_clock(1_800_000_300);
    let (fleet_id, mut session) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // Mark 90_000_004 as opt-out from regroup
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap
            .fleets
            .get_mut(&fleet_id)
            .and_then(|f| f.member_details.get_mut(&90_000_004))
        {
            m.accepts_regroup = false;
        }
    });

    // Unauthorized SquadMember denied Regroup (temporarily set to Wing2 SquadMember)
    let mut mem_sess = SessionState::new(20);
    mem_sess.user_id = Some(90_000_004);
    mem_sess.character_id = Some(90_000_004);
    mem_sess.solar_system_id = Some(30_000_142);
    mem_sess.ship_id = Some(100_000_004);
    mem_sess.fleet_id = Some(fleet_id);
    assert_eq!(
        handle_fleet_ops(&ctx, &mut mem_sess, "fleetObjectHandler", "Regroup", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetPermissionDenied"
    );

    // FC issues Regroup in Jita:
    // - 90_000_002 is 15km away -> regrouped
    // - 90_000_003 is 400km away (<= 500km grid) -> regrouped
    // - 90_000_004 opted out -> in skippedOptOutMemberIDs
    // - 90_000_005 is in Perimeter -> ignored
    let r_res = handle_fleet_ops(&ctx, &mut session, "fleetObjectHandler", "Regroup", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::KeyVal(r_kv) = r_res else { panic!() };
    let PyRep::List(regrouped) = r_kv.get("regroupedMemberIDs").unwrap() else { panic!() };
    let reg_ids: HashSet<i64> = regrouped.iter().filter_map(|v| v.as_i64()).collect();
    assert_eq!(reg_ids, HashSet::from([90_000_002, 90_000_003]));
    assert_eq!(
        r_kv.get("skippedOptOutMemberIDs").unwrap(),
        &PyRep::List(vec![PyRep::Integer(90_000_004)])
    );
}

#[tokio::test]
async fn fleet_warp_handles_destinations_bookmarks_opt_outs_and_scopes() {
    let ctx = test_ctx_with_clock(1_800_000_400);
    let (fleet_id, mut session) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // Seed personal bookmark in Jita (30_000_142) and one in EVE system (30_100_000) and one owned by another char
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            910_001,
            BookmarkRecord {
                bookmark_id: 910_001,
                owner_id: 90_000_001,
                label: "Jita Ping".into(),
                solar_system_id: 30_000_142,
                x: 20_000_000.0,
                y: 5_000_000.0,
                z: -10_000_000.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            910_002,
            BookmarkRecord {
                bookmark_id: 910_002,
                owner_id: 90_000_001,
                label: "EVE Blacklist".into(),
                solar_system_id: 30_100_000,
                x: 1_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmarks.insert(
            910_003,
            BookmarkRecord {
                bookmark_id: 910_003,
                owner_id: 90_000_999,
                label: "Private Foreign".into(),
                solar_system_id: 30_000_142,
                x: 5_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
        if let Some(m) = snap
            .fleets
            .get_mut(&fleet_id)
            .and_then(|f| f.member_details.get_mut(&90_000_004))
        {
            m.takes_fleet_warp = false;
        }
    });

    // SquadMember denied FleetWarp
    let mut mem_sess = SessionState::new(30);
    mem_sess.user_id = Some(90_000_004);
    mem_sess.character_id = Some(90_000_004);
    mem_sess.solar_system_id = Some(30_000_142);
    mem_sess.ship_id = Some(100_000_004);
    mem_sess.fleet_id = Some(fleet_id);
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut mem_sess,
            "fleetObjectHandler",
            "WarpFleet",
            &[PyRep::Integer(60_003_760)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );

    // Blacklisted bookmark -> DunBlacklistCannotWarp; inaccessible bookmark -> BookmarkNotFound
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut session,
            "fleetObjectHandler",
            "FleetWarp",
            &[PyRep::Integer(910_002)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "DunBlacklistCannotWarp"
    );
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut session,
            "fleetObjectHandler",
            "FleetWarp",
            &[PyRep::Integer(910_003)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "BookmarkNotFound"
    );

    // Valid bookmark warp with Arrow formation (formationID=3, spacing=5_000m, minRange=10_000m)
    let fw_res = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "WarpFleet",
        &[
            PyRep::Integer(910_001),
            PyRep::Real(10_000.0),
            PyRep::Integer(3),
            PyRep::Real(5_000.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(fw_kv) = fw_res else { panic!() };
    assert_eq!(fw_kv.get("status").unwrap().as_str().unwrap(), "FleetWarpInitiated");
    let PyRep::List(warped) = fw_kv.get("warpedMemberIDs").unwrap() else { panic!() };
    let w_ids: HashSet<i64> = warped.iter().filter_map(|v| v.as_i64()).collect();
    assert_eq!(w_ids, HashSet::from([90_000_001, 90_000_002, 90_000_003]));
    assert_eq!(
        fw_kv.get("skippedOptOutMemberIDs").unwrap(),
        &PyRep::List(vec![PyRep::Integer(90_000_004)])
    );

    // Explicit coordinates warp [x, y, z, minRange, formationID, spacing]
    let coord_res = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetMgr",
        "FleetWarp",
        &[
            PyRep::Real(50_000_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::Real(25_000.0),
            PyRep::Integer(1),
            PyRep::Real(6_000.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(c_kv) = coord_res else { panic!() };
    assert_eq!(c_kv.get("formationID").unwrap(), &PyRep::Integer(1));
    assert!((c_kv.get("destinationX").unwrap().as_f64().unwrap() - 50_000_000.0).abs() < 1.0);

    // Abyssal deadspace warp rejected
    let mut abyss_sess = session.clone();
    abyss_sess.solar_system_id = Some(32_000_001);
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut abyss_sess,
            "fleetObjectHandler",
            "WarpFleet",
            &[PyRep::Integer(60_003_760)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotWarpInAbyssalSpace"
    );
}
