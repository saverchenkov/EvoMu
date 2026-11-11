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

fn mark_online(ctx: &ServerContext, char_id: i64, session_id: u64, now_unix: u64) {
    let uid = char_id as u64;
    let uname = format!("u{uid}");
    ctx.db.with_mut_snapshot(|snap| {
        if !snap.accounts.values().any(|a| a.user_id == uid) {
            snap.accounts.insert(
                uname.clone(),
                eve_db::AccountRecord {
                    user_id: uid,
                    username: uname,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
    });
    ctx.db
        .acquire_online_lock(uid, session_id, Some(char_id), true, now_unix)
        .unwrap();
}

#[tokio::test]
async fn wave2_fleet_warp_rpc_validation_command_scoping_opt_out_and_safety_filters() {
    let (ctx, _clock) = super::test_ctx_and_clock(1_800_060_000);
    let (fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();
    for (idx, cid) in [90_000_001_i64, 90_000_002, 90_000_003, 90_000_004, 90_000_005]
        .into_iter()
        .enumerate()
    {
        mark_online(&ctx, cid, 7_000 + idx as u64, 1_800_060_000);
    }

    // Wave 2 req_finite_vec3 / opt_finite_f64 validation
    assert!(
        handle_fleet_ops(
            &ctx,
            &mut fc_sess,
            "fleetObjectHandler",
            "FleetWarp",
            &[PyRep::Real(f64::NAN), PyRep::Real(0.0), PyRep::Real(0.0)],
            &kw,
        )
        .await
        .unwrap()
        .is_err()
    );
    assert!(
        handle_fleet_ops(
            &ctx,
            &mut fc_sess,
            "fleetObjectHandler",
            "FleetWarp",
            &[PyRep::Real(1_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(f64::INFINITY)],
            &kw,
        )
        .await
        .unwrap()
        .is_err()
    );
    assert!(
        handle_fleet_ops(
            &ctx,
            &mut fc_sess,
            "fleetObjectHandler",
            "FleetWarp",
            &[PyRep::Integer(60_003_760), PyRep::Real(f64::NAN)],
            &kw,
        )
        .await
        .unwrap()
        .is_err()
    );

    // NoSolarSystem error
    let mut no_sys = fc_sess.clone();
    no_sys.solar_system_id = None;
    assert_eq!(
        handle_fleet_ops(&ctx, &mut no_sys, "fleetObjectHandler", "FleetWarp", &[PyRep::Integer(60_003_760)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NoSolarSystem"
    );

    // SquadMember (non-boss) denied FleetPermissionDenied
    let mut mem4 = SessionState::new(704);
    mem4.user_id = Some(90_000_004);
    mem4.character_id = Some(90_000_004);
    mem4.fleet_id = Some(fleet_id);
    mem4.solar_system_id = Some(30_000_142);
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut mem4,
            "fleetObjectHandler",
            "FleetWarp",
            &[PyRep::Real(2_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );

    // SquadCommander (90_000_003, wing1/squad1) scopes warp to squad1 [90_000_001, 90_000_002, 90_000_003],
    // NEVER warping wing2/squad2 member 90_000_004
    let mut sc3 = SessionState::new(703);
    sc3.user_id = Some(90_000_003);
    sc3.character_id = Some(90_000_003);
    sc3.fleet_id = Some(fleet_id);
    sc3.solar_system_id = Some(30_000_142);
    sc3.ship_id = Some(100_000_003);
    let sc_res = handle_fleet_ops(
        &ctx,
        &mut sc3,
        "fleetObjectHandler",
        "FleetWarp",
        &[PyRep::Real(3_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(10_000.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(sc_kv) = sc_res else { panic!() };
    let PyRep::List(sc_warped) = sc_kv.get("warpedMemberIDs").unwrap() else { panic!() };
    assert!(sc_warped.contains(&PyRep::Integer(90_000_001)));
    assert!(sc_warped.contains(&PyRep::Integer(90_000_002)));
    assert!(sc_warped.contains(&PyRep::Integer(90_000_003)));
    assert!(!sc_warped.contains(&PyRep::Integer(90_000_004)));

    // SquadMember who is Fleet Boss CAN initiate fleet warp fleet-wide
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.get_mut(&fleet_id).unwrap().boss_id = 90_000_004;
    });
    mem4.ship_id = Some(100_000_004);
    let boss_mem_res = handle_fleet_ops(
        &ctx,
        &mut mem4,
        "fleetObjectHandler",
        "FleetWarp",
        &[PyRep::Real(4_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(bm_kv) = boss_mem_res else { panic!() };
    assert_eq!(bm_kv.get("warpCount").and_then(|v| v.as_i64()), Some(4));

    // Restore FC as boss and test opt-out + OnFleetMoveFailed, scramble, >8000km, <150km, and docked
    ctx.db.with_mut_snapshot(|snap| {
        let fl = snap.fleets.get_mut(&fleet_id).unwrap();
        fl.boss_id = 90_000_001;
        fl.member_details.get_mut(&90_000_001).unwrap().takes_fleet_warp = false; // caller exempted
        fl.member_details.get_mut(&90_000_002).unwrap().takes_fleet_warp = false; // skipped + OnFleetMoveFailed
    });
    let sys = ctx.systems.get_or_spawn(30_000_142);
    // Teleport ships cleanly:
    // FC 90_000_001 -> (0, 0, 0), dest = (5_000_000, 0, 0)
    // 90_000_003 -> (8_000_001, 0, 0) (> 8,000km from FC -> skipped)
    // 90_000_004 -> scramble strength 1 (scrambled -> skipped)
    let _ = sys.set_ball_position(100_000_001, DVec3::ZERO).await;
    let _ = sys.set_ball_position(100_000_003, DVec3::new(8_000_001.0, 0.0, 0.0)).await;
    let _ = sys.set_ball_position(100_000_004, DVec3::new(10_000.0, 0.0, 0.0)).await;
    let _ = sys.set_warp_scramble_strength(100_000_004, 1).await;
    let _ = ctx.drain_notifications_for_char(90_000_001);

    let filt_res = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "FleetWarp",
        &[PyRep::Real(5_000_000.0), PyRep::Real(0.0), PyRep::Real(0.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(f_kv) = filt_res else { panic!() };
    let PyRep::List(warped_ids) = f_kv.get("warpedMemberIDs").unwrap() else { panic!() };
    assert_eq!(warped_ids, &vec![PyRep::Integer(90_000_001)]);
    let PyRep::List(skipped_ids) = f_kv.get("skippedOptOutMemberIDs").unwrap() else { panic!() };
    assert_eq!(skipped_ids, &vec![PyRep::Integer(90_000_002)]);
    let fc_notes = ctx.drain_notifications_for_char(90_000_001);
    assert!(
        fc_notes.iter().any(|n| n.event == "OnFleetMoveFailed" && n.args == vec![PyRep::Integer(90_000_002)]),
        "expected OnFleetMoveFailed for opt-out 90_000_002, got {fc_notes:?}"
    );
}

#[tokio::test]
async fn wave2_fleet_warp_bookmarks_expiry_station_fallback_warp_to_member_and_vertical_formation() {
    let (ctx, _clock) = super::test_ctx_and_clock(1_800_070_000);
    let (fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // Expired bookmark -> BookmarkNotFound; Jove/30_100_000 bookmark -> DunBlacklistCannotWarp
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            990_001,
            BookmarkRecord {
                bookmark_id: 990_001,
                owner_id: 90_000_001,
                label: "Expired BM".into(),
                solar_system_id: 30_000_142,
                x: 2_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
        snap.bookmark_expiries.insert(990_001, 1_800_070_000); // now >= expiry -> expired

        snap.bookmarks.insert(
            990_002,
            BookmarkRecord {
                bookmark_id: 990_002,
                owner_id: 90_000_001,
                label: "Jove Blacklist".into(),
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
            990_003,
            BookmarkRecord {
                bookmark_id: 990_003,
                owner_id: 999_888, // private bookmark owned by stranger
                label: "Private BM".into(),
                solar_system_id: 30_000_142,
                x: 2_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "FleetWarp", &[PyRep::Integer(990_001)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "BookmarkNotFound"
    );
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "FleetWarp", &[PyRep::Integer(990_002)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DunBlacklistCannotWarp"
    );
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "FleetWarp", &[PyRep::Integer(990_003)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "BookmarkNotFound"
    );

    // SDE station fallback in 30_000_142
    if let Some(st) = ctx.sde.solar_systems.get(&30_000_142).and_then(|s| s.stations.first()) {
        let st_id = st.station_id;
        let st_pos = st.position;
        let st_res = handle_fleet_ops(
            &ctx,
            &mut fc_sess,
            "fleetObjectHandler",
            "FleetWarp",
            &[PyRep::Integer(st_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        let PyRep::KeyVal(st_kv) = st_res else { panic!() };
        assert!((st_kv.get("destinationX").and_then(|v| v.as_f64()).unwrap() - st_pos.x).abs() < 1e-3);
    }

    // WarpToMember checks: MemberNotFound, DifferentSolarSystem, WarpDistanceTooClose (<150km),
    // WarpScrambled, and valid WarpToMember landing at target - dir * min_range
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(90_999_999)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "MemberNotFound"
    );
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(90_000_005)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DifferentSolarSystem"
    );
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(90_000_002)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "WarpDistanceTooClose"
    );

    // Target 90_000_003 at 400km: first verify caller scrambled fails, then unscrambled succeeds with minRange=20km
    let sys = ctx.systems.get_or_spawn(30_000_142);
    let _ = sys.set_ball_position(100_000_001, DVec3::ZERO).await;
    let _ = sys.set_ball_position(100_000_003, DVec3::new(400_000.0, 0.0, 0.0)).await;
    let _ = sys.set_warp_scramble_strength(100_000_001, 1).await;
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut fc_sess,
            "fleetObjectHandler",
            "WarpToMember",
            &[PyRep::Integer(90_000_003), PyRep::Real(20_000.0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "WarpScrambled"
    );
    let _ = sys.set_warp_scramble_strength(100_000_001, 0).await;
    let wtm_res = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "WarpToMember",
        &[PyRep::Integer(90_000_003), PyRep::Real(20_000.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(wtm_kv) = wtm_res else { panic!() };
    assert_eq!(wtm_kv.get("status").and_then(|s| s.as_str()), Some("Warping"));
    assert!((wtm_kv.get("destX").and_then(|v| v.as_f64()).unwrap() - 380_000.0).abs() < 1e-6);

    // Vertical heading exercises ref_up = DVec3::Z fallback in compute_oriented_formation_offset
    let vert_wall = compute_oriented_formation_offset(1, 1, 5_000.0, 10_000.0, DVec3::Y, DVec3::ZERO);
    assert!(vert_wall.y.abs() < 1e-6);
    assert!((vert_wall.length() - 5_000.0).abs() < 1e-6);
    let _ = fleet_id;
}

