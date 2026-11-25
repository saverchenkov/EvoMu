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

#[tokio::test]
async fn fleet_warp_tracks_members_in_active_warps_preserves_mid_tunnel_update_state_and_prunes_on_land() {
    let ctx = test_ctx_with_clock(1_800_000_900);
    let (fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();
    let _ = fleet_id;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap
            .fleets
            .get_mut(&fleet_id)
            .and_then(|f| f.member_details.get_mut(&90_000_004))
        {
            m.takes_fleet_warp = false;
        }
    });

    let dest = DVec3::new(1_000_000_000.0, 0.0, 0.0);
    let fw_res = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "FleetWarp",
        &[
            PyRep::Real(dest.x),
            PyRep::Real(dest.y),
            PyRep::Real(dest.z),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(fw_kv) = fw_res else { panic!() };
    assert_eq!(fw_kv.get("status").and_then(|s| s.as_str()), Some("FleetWarpInitiated"));

    let sys = ctx.systems.get_or_spawn(30_000_142);
    {
        let view = sys.view();
        let warps = &view.active_warps;
        assert_eq!(warps.get(&100_000_001), Some(&90_000_001));
        assert_eq!(warps.get(&100_000_002), Some(&90_000_002));
        assert_eq!(warps.get(&100_000_003), Some(&90_000_003));
        assert!(!warps.contains_key(&100_000_004));
        assert!(!warps.contains_key(&100_000_005));
    }
    assert_eq!(
        sys.warp_provenance(100_000_002),
        Some(crate::system_actor::WarpProvenance::PlayerOrFleetWarp)
    );

    let mut wingmate_sess = SessionState::new(30_002);
    wingmate_sess.user_id = Some(90_000_002);
    wingmate_sess.character_id = Some(90_000_002);
    wingmate_sess.solar_system_id = Some(30_000_142);
    wingmate_sess.ship_id = Some(100_000_002);
    wingmate_sess.fleet_id = Some(fleet_id);
    mark_online(&ctx, 90_000_002, wingmate_sess.session_id, 1_800_000_900);

    assert_eq!(
        crate::services::space_destiny::handle(
            &ctx,
            &mut wingmate_sess,
            None,
            "beyonce",
            "UpdateStateRequest",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    let (_, b2_align) = sys.get_ball_snapshot(100_000_002).await.unwrap().unwrap();
    assert!(matches!(b2_align.mode, eve_destiny::MovementMode::Warp { .. }));

    for _ in 0..5 {
        let _ = sys.step_manual(1.0).await;
    }
    let (_, b2_cruise) = sys.get_ball_snapshot(100_000_002).await.unwrap().unwrap();
    assert!(matches!(b2_cruise.mode, eve_destiny::MovementMode::Warp { .. }));

    assert_eq!(
        crate::services::space_destiny::handle(
            &ctx,
            &mut wingmate_sess,
            None,
            "beyonce",
            "UpdateStateRequest",
            &[],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );
    let (_, b2_after_mid) = sys.get_ball_snapshot(100_000_002).await.unwrap().unwrap();
    assert!(
        matches!(b2_after_mid.mode, eve_destiny::MovementMode::Warp { .. }),
        "Mid-tunnel UpdateStateRequest must not force-stop fleet-warped wingmate"
    );

    let mut landed = false;
    for _ in 0..240 {
        let _ = sys.step_manual(1.0).await;
        let (_, b1) = sys.get_ball_snapshot(100_000_001).await.unwrap().unwrap();
        let (_, b2) = sys.get_ball_snapshot(100_000_002).await.unwrap().unwrap();
        let (_, b3) = sys.get_ball_snapshot(100_000_003).await.unwrap().unwrap();
        if matches!(b1.mode, eve_destiny::MovementMode::Stop)
            && matches!(b2.mode, eve_destiny::MovementMode::Stop)
            && matches!(b3.mode, eve_destiny::MovementMode::Stop)
        {
            landed = true;
            break;
        }
    }
    assert!(landed, "Fleet-warped ships must complete warp and reach MovementMode::Stop");

    let (_, b2_landed) = sys.get_ball_snapshot(100_000_002).await.unwrap().unwrap();
    assert!(
        b2_landed.position.distance(dest) < 25_000.0,
        "Wingmate must land at destination grid, dist={}",
        b2_landed.position.distance(dest)
    );
    {
        let view = sys.view();
        let warps = &view.active_warps;
        assert!(!warps.contains_key(&100_000_001));
        assert!(!warps.contains_key(&100_000_002));
        assert!(!warps.contains_key(&100_000_003));
    }
    assert_eq!(sys.warp_provenance(100_000_002), None);
    assert!(sys.is_pending_warp_finished(100_000_002));

    let _ = ctx.drain_notifications_for_char(90_000_002);
    crate::services::space_destiny::handle(
        &ctx,
        &mut wingmate_sess,
        None,
        "beyonce",
        "UpdateStateRequest",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let notes = ctx.drain_notifications_for_char(90_000_002);
    assert!(notes.iter().any(|n| n.event == "OnWarpFinished"));
    assert!(!sys.is_pending_warp_finished(100_000_002));
}

#[tokio::test]
async fn fleet_warp_enforces_gated_dungeon_mjd_crimewatch_cloak_and_tourney_restrictions() {
    let ctx = test_ctx_with_clock(1_800_001_000);
    let (_fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);

    // 1. Deadspace gated room target blocks FleetWarp with NaturalPhenomenaDisruptingWarp, and
    //    ungated interior room (`r_idx > 0`, `r.gated == false`) blocks with WarpBlockedByDeadspacePocket.
    let dungeon_base = [2_000_000_000.0_f64, 0.0, 0.0];
    let rooms_py = PyRep::List(vec![
        PyRep::keyval([("roomName", PyRep::String("Entry".into()))]),
        PyRep::keyval([("roomName", PyRep::String("Deep Chamber".into()))]),
    ]);
    let mut d_kw = HashMap::new();
    d_kw.insert("solarSystemID".into(), PyRep::Integer(i64::from(sys_id)));
    d_kw.insert(
        "dungeonID".into(),
        PyRep::Integer(i64::from(ctx.sde.dungeons.keys().next().copied().unwrap_or(2001))),
    );
    d_kw.insert("dungeonName".into(), PyRep::String("Fleet Gate Test".into()));
    d_kw.insert("hasEntryGate".into(), PyRep::Bool(true));
    d_kw.insert(
        "position".into(),
        PyRep::List(vec![
            PyRep::Real(dungeon_base[0]),
            PyRep::Real(dungeon_base[1]),
            PyRep::Real(dungeon_base[2]),
        ]),
    );
    d_kw.insert("rooms".into(), rooms_py);
    let _ = crate::services::pve_dungeon_runtime_rpc::spawn_dungeon_instance(
        &ctx,
        90_000_001,
        Some(sys_id),
        &[],
        &d_kw,
    )
    .await
    .expect("spawn gated dungeon");

    let ds_err = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "FleetWarp",
        &[
            PyRep::Real(dungeon_base[0] + 500_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(ds_err.code, "NaturalPhenomenaDisruptingWarp");

    crate::services::pve_dungeon_runtime::with_dungeon_runtime_store(&ctx, |store| {
        for inst in store.instances.values_mut() {
            for r in &mut inst.rooms {
                r.gated = false;
            }
        }
    });

    let gated_err = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "FleetWarp",
        &[
            PyRep::Real(dungeon_base[0] + 500_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(gated_err.code, "WarpBlockedByDeadspacePocket");

    // 2. Per-member MJD spool, high-sec criminal timer, and DB non-covert cloak skip only those members.
    sys.modify_combat_state(100_000_002, |cs| {
        cs.mjfg_spool_remaining_s = 5.0;
    })
    .await
    .unwrap();
    {
        let now = ctx.now_unix();
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(90_000_003).criminal_flag_until_unix = now + 900;
    }
    let cloak_mod_id = 911_370_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            eve_db::InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: 11370,
                owner_id: 90_000_004,
                location_id: 100_000_004,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(cloak_mod_id);
    });

    let open_dest = DVec3::new(10_000_000_000.0, 0.0, 0.0);
    let partial_res = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "FleetWarp",
        &[
            PyRep::Real(open_dest.x),
            PyRep::Real(open_dest.y),
            PyRep::Real(open_dest.z),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(partial_kv) = partial_res else {
        panic!("expected KeyVal");
    };
    assert_eq!(partial_kv.get("warpCount").and_then(|v| v.as_i64()), Some(1));
    assert_eq!(
        partial_kv.get("warpedMemberIDs"),
        Some(&PyRep::List(vec![PyRep::Integer(90_000_001)]))
    );

    // 3. Covert Ops cloak (even with stale CombatState.is_covert_cloak = false) and gate cloak still warp.
    sys.modify_combat_state(100_000_002, |cs| {
        cs.mjfg_spool_remaining_s = 0.0;
    })
    .await
    .unwrap();
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(90_000_003).criminal_flag_until_unix = 0;
    }
    sys.modify_combat_state(100_000_003, |cs| {
        cs.gate_cloak_remaining_ticks = 30;
        cs.is_cloaked = true;
    })
    .await
    .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&cloak_mod_id).unwrap().type_id = 11578;
    });
    sys.modify_combat_state(100_000_004, |cs| {
        cs.is_cloaked = true;
        cs.is_covert_cloak = false;
    })
    .await
    .unwrap();

    let open_dest_2 = DVec3::new(12_000_000_000.0, 0.0, 0.0);
    let covert_res = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "FleetWarp",
        &[
            PyRep::Real(open_dest_2.x),
            PyRep::Real(open_dest_2.y),
            PyRep::Real(open_dest_2.z),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(covert_kv) = covert_res else {
        panic!("expected KeyVal");
    };
    assert_eq!(covert_kv.get("warpCount").and_then(|v| v.as_i64()), Some(4));
    let cs4 = sys.get_combat_state(100_000_004).await.unwrap().unwrap();
    assert!(cs4.is_covert_cloak);
    let cs3 = sys.get_combat_state(100_000_003).await.unwrap().unwrap();
    assert_eq!(cs3.gate_cloak_remaining_ticks, 0);

    // 4. Tourney warp restriction rejects FleetWarp when FC is restricted, and skips restricted wingmates.
    let ref_cid = 90_000_099_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters[&90_000_001].clone();
        let mut gm_ch = base;
        gm_ch.character_id = ref_cid;
        gm_ch.name = "TourneyRef".into();
        gm_ch.solar_system_id = sys_id;
        snap.characters.insert(ref_cid, gm_ch);
    });
    let mut gm = SessionState::new(99);
    gm.user_id = Some(1);
    gm.character_id = Some(ref_cid);
    gm.solar_system_id = Some(sys_id);
    gm.role = eve_db::service_roles::ROLE_GMH | eve_db::service_roles::ROLE_ADMIN;

    let match_id = 7_001_i64;
    crate::services::tourney_service::handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "CreateMatch",
        &[
            PyRep::Integer(1),
            PyRep::Integer(match_id),
            PyRep::Integer(50),
            PyRep::Integer(600),
            PyRep::Integer(120),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("CreateMatch");
    crate::services::tourney_service::handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "AddPlayer",
        &[PyRep::Integer(match_id), PyRep::Integer(0), PyRep::Integer(90_000_002)],
        &kw,
    )
    .await
    .unwrap()
    .expect("AddPlayer red wingmate");
    crate::services::tourney_service::handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "AddPlayer",
        &[PyRep::Integer(match_id), PyRep::Integer(1), PyRep::Integer(90_000_005)],
        &kw,
    )
    .await
    .unwrap()
    .expect("AddPlayer blue");
    crate::services::tourney_service::handle(
        &ctx,
        &mut gm,
        "tourneyMgr",
        "StartCountdown",
        &[PyRep::Integer(match_id), PyRep::Integer(60)],
        &kw,
    )
    .await
    .unwrap()
    .expect("StartCountdown");

    let wing_tourney_res = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "FleetWarp",
        &[
            PyRep::Real(14_000_000_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(wt_kv) = wing_tourney_res else {
        panic!("expected KeyVal");
    };
    assert_eq!(wt_kv.get("warpCount").and_then(|v| v.as_i64()), Some(3));
    if let Some(PyRep::List(ids)) = wt_kv.get("warpedMemberIDs") {
        assert!(!ids.contains(&PyRep::Integer(90_000_002)));
    } else {
        panic!("expected warpedMemberIDs list");
    }

    // FC restricted inside a tourney arena rejects FleetWarp directly.
    let mut wing_fc_sess = SessionState::new(98);
    wing_fc_sess.user_id = Some(90_000_002);
    wing_fc_sess.character_id = Some(90_000_002);
    wing_fc_sess.solar_system_id = Some(sys_id);
    wing_fc_sess.ship_id = Some(100_000_002);
    wing_fc_sess.fleet_id = Some(_fleet_id);
    let fc_tourney_err = handle_fleet_ops(
        &ctx,
        &mut wing_fc_sess,
        "fleetObjectHandler",
        "FleetWarp",
        &[
            PyRep::Real(15_000_000_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(fc_tourney_err.code, "WarpRestrictedInTourney");
}

#[tokio::test]
async fn fleet_warp_enforces_capacitor_short_warp_clamp_and_pilot_warp_speed() {
    let ctx = test_ctx_with_clock(1_800_001_100);
    let (_fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let kw = HashMap::new();

    // Wingmate 90_000_003 (100_000_003) has 0 GJ capacitor -> skipped.
    sys.set_capacitor(100_000_003, 0.0).await.unwrap();

    // Wingmate 90_000_004 (100_000_004, type 590, mass 1_000_000) has cap for 500 km only -> short warp clamp.
    let wcn_590 = ctx
        .sde
        .types
        .get(&590)
        .and_then(|t| t.default_attributes.get(&153).copied())
        .unwrap_or(1.24e-6);
    let cap_per_m_590 = (wcn_590 * 1_000_000.0) / crate::services::space_destiny::AU_IN_METERS;
    sys.set_capacitor(100_000_004, cap_per_m_590 * 500_000.0)
        .await
        .unwrap();

    let cs1_before = sys.get_combat_state(100_000_001).await.unwrap().unwrap();
    let dest = DVec3::new(100_000_000_000.0, 0.0, 0.0);
    let res = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "FleetWarp",
        &[
            PyRep::Real(dest.x),
            PyRep::Real(dest.y),
            PyRep::Real(dest.z),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(kv) = res else {
        panic!("expected KeyVal");
    };
    assert_eq!(kv.get("warpCount").and_then(|v| v.as_i64()), Some(3));
    if let Some(PyRep::List(ids)) = kv.get("warpedMemberIDs") {
        assert!(!ids.contains(&PyRep::Integer(90_000_003)));
        assert!(ids.contains(&PyRep::Integer(90_000_001)));
        assert!(ids.contains(&PyRep::Integer(90_000_002)));
        assert!(ids.contains(&PyRep::Integer(90_000_004)));
    } else {
        panic!("expected warpedMemberIDs");
    }

    // 90_000_001 flies Rifter (587) -> uses pilot warp speed != hardcoded 3.0 AU/s and deducts capacitor.
    let expected_speed = crate::services::space_destiny::compute_pilot_warp_speed_au_s(
        &ctx,
        90_000_001,
        sys_id,
        587,
    );
    assert!(expected_speed > 3.0, "Rifter warp speed should exceed 3.0 AU/s");
    let (_, b1) = sys.get_ball_snapshot(100_000_001).await.unwrap().unwrap();
    match b1.mode {
        eve_destiny::MovementMode::Warp {
            warp_speed_au_s, ..
        } => {
            assert!((warp_speed_au_s - expected_speed).abs() < 1e-6);
        }
        other => panic!("expected Warp mode for Rifter, got {other:?}"),
    }
    let cs1_after = sys.get_combat_state(100_000_001).await.unwrap().unwrap();
    assert!(
        cs1_after.capacitor < cs1_after.max_capacitor
            && (cs1_after.capacitor < cs1_before.capacitor
                || cs1_after.max_capacitor > cs1_before.max_capacitor),
        "capacitor must be deducted (and SDE max_cap repaired if synthetic default)"
    );

    // 90_000_004 clamped short warp >= 150 km and drained remaining cap to 0.0.
    let (_, b4) = sys.get_ball_snapshot(100_000_004).await.unwrap().unwrap();
    match b4.mode {
        eve_destiny::MovementMode::Warp { destination, .. } => {
            let clamped_dist = b4.position.distance(destination);
            assert!(
                (150_000.0..=1_000_000.0).contains(&clamped_dist),
                "expected ~500 km clamped warp, got {clamped_dist}"
            );
        }
        other => panic!("expected clamped Warp mode, got {other:?}"),
    }
    let cs4_after = sys.get_combat_state(100_000_004).await.unwrap().unwrap();
    assert!(cs4_after.capacitor.abs() < 1e-9);
}

#[tokio::test]
async fn fleet_warp_skips_active_warp_preserves_cloak_invuln_and_deduplicates_invuln_cancel() {
    let ctx = test_ctx_with_clock(1_800_001_200);
    let (_fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let now = ctx.now_unix();
    mark_online(&ctx, 90_000_001, fc_sess.session_id, now);
    mark_online(&ctx, 90_000_002, 81_002, now);
    mark_online(&ctx, 90_000_003, 81_003, now);

    // 1. Put 90_000_002 (100_000_002) already into active warp (Cruising), with gate cloak
    //    and undock invulnerability; verify skip preserves ALL of them.
    let mut active_mode = MovementMode::new_warp(DVec3::ZERO, DVec3::new(10_000_000_000.0, 0.0, 0.0), 3.0);
    if let MovementMode::Warp { ref mut phase, .. } = active_mode {
        *phase = eve_destiny::WarpPhase::Cruising;
    }
    sys.set_warp_movement_mode(
        100_000_002,
        active_mode.clone(),
        crate::system_actor::WarpProvenance::PlayerOrFleetWarp,
    )
    .await
    .unwrap();
    sys.modify_combat_state(100_000_002, |cs| {
        cs.gate_cloak_remaining_ticks = 25;
        cs.is_cloaked = true;
        cs.undock_invulnerable_remaining_ticks = 25;
    })
    .await
    .unwrap();

    // 2. Give 90_000_003 (100_000_003) BOTH gate cloak and undock invulnerability.
    sys.modify_combat_state(100_000_003, |cs| {
        cs.gate_cloak_remaining_ticks = 20;
        cs.is_cloaked = true;
        cs.undock_invulnerable_remaining_ticks = 20;
    })
    .await
    .unwrap();

    // Give FC undock invulnerability to verify beyonce.CmdWarpToStuff(..., fleet=True) deduplication too.
    sys.modify_combat_state(100_000_001, |cs| {
        cs.undock_invulnerable_remaining_ticks = 15;
    })
    .await
    .unwrap();
    let _ = ctx.drain_notifications_for_char(90_000_001);
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let _ = ctx.drain_notifications_for_char(90_000_003);

    let dest = DVec3::new(5_000_000_000.0, 0.0, 0.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            991_101,
            BookmarkRecord {
                bookmark_id: 991_101,
                owner_id: 90_000_001,
                label: "FleetParitySpot".into(),
                solar_system_id: sys_id,
                x: dest.x,
                y: dest.y,
                z: dest.z,
                item_id: None,
                memo: String::new(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    let mut kw_fleet = HashMap::new();
    kw_fleet.insert("fleet".to_string(), PyRep::Bool(true));
    let res = crate::services::space_destiny::handle(
        &ctx,
        &mut fc_sess,
        None,
        "beyonce",
        "CmdWarpToStuff",
        &[PyRep::String("bookmark".into()), PyRep::Integer(991_101)],
        &kw_fleet,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(kv) = res else { panic!("expected KeyVal") };
    if let Some(PyRep::List(ids)) = kv.get("warpedMemberIDs") {
        assert!(!ids.contains(&PyRep::Integer(90_000_002)), "active warp member must be skipped");
        assert!(ids.contains(&PyRep::Integer(90_000_001)));
        assert!(ids.contains(&PyRep::Integer(90_000_003)));
    } else {
        panic!("expected warpedMemberIDs");
    }

    // Active-warp 90_000_002 kept its original Cruising trajectory, cloak, and undock invuln.
    let (_, b2) = sys.get_ball_snapshot(100_000_002).await.unwrap().unwrap();
    match b2.mode {
        MovementMode::Warp { destination, phase, .. } => {
            assert_eq!(phase, eve_destiny::WarpPhase::Cruising);
            assert!((destination.x - 10_000_000_000.0).abs() < 1.0);
        }
        other => panic!("expected untouched Cruising warp, got {other:?}"),
    }
    let cs2 = sys.get_combat_state(100_000_002).await.unwrap().unwrap();
    assert_eq!(cs2.gate_cloak_remaining_ticks, 25);
    assert_eq!(cs2.undock_invulnerable_remaining_ticks, 25);
    assert!(ctx.drain_notifications_for_char(90_000_002).is_empty());

    // Warped 90_000_003 & FC 90_000_001 each received OnInvulnCancelled strictly ONCE (and 90_000_003 once for OnJumpCloakCancelled).
    let n1 = ctx.drain_notifications_for_char(90_000_001);
    assert_eq!(n1.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);
    let cs1 = sys.get_combat_state(100_000_001).await.unwrap().unwrap();
    assert_eq!(cs1.undock_invulnerable_remaining_ticks, 0);
    let n3 = ctx.drain_notifications_for_char(90_000_003);
    assert_eq!(n3.iter().filter(|n| n.event == "OnJumpCloakCancelled").count(), 1);
    assert_eq!(n3.iter().filter(|n| n.event == "OnInvulnCancelled").count(), 1);
    let cs3 = sys.get_combat_state(100_000_003).await.unwrap().unwrap();
    assert_eq!(cs3.gate_cloak_remaining_ticks, 0);
    assert_eq!(cs3.undock_invulnerable_remaining_ticks, 0);
}

#[tokio::test]
async fn fleet_warp_enforces_post_range_and_formation_minimum_distance() {
    let ctx = test_ctx_with_clock(1_800_001_300);
    let (_fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let kw = HashMap::new();

    // Place FC at 0 and wingmate 90_000_002 at (-10_000, 0, 0). Target at (160_000, 0, 0) with
    // min_range = 100_000 -> requested_dist_m <= 70_000 < 150_000 -> all near members must be skipped.
    sys.set_ball_position(100_000_001, DVec3::ZERO).await;
    sys.set_ball_position(100_000_002, DVec3::new(-10_000.0, 0.0, 0.0)).await;
    sys.set_ball_position(100_000_003, DVec3::new(0.0, 0.0, 0.0)).await;
    sys.set_ball_position(100_000_004, DVec3::new(0.0, 0.0, 0.0)).await;

    let res_short = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "FleetWarp",
        &[
            PyRep::Real(160_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::Real(100_000.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(kv_short) = res_short else { panic!("expected KeyVal") };
    assert_eq!(kv_short.get("warpCount").and_then(|v| v.as_i64()), Some(0));
    for sid in [100_000_001, 100_000_002, 100_000_003, 100_000_004] {
        let (_, b) = sys.get_ball_snapshot(sid).await.unwrap().unwrap();
        assert!(matches!(b.mode, MovementMode::Stop), "ship {sid} must stay stopped under min_range short warp");
    }

    // With min_range = 0.0 and Arrow formation (id=3, spacing=20_000): FC (slot 0) warps 160 km,
    // while slot 1 is shifted -20 km behind anchor (< 150 km) and must stay stopped.
    sys.set_ball_position(100_000_002, DVec3::ZERO).await;
    let res_form = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "FleetWarp",
        &[
            PyRep::Real(160_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::Integer(3),
            PyRep::Real(20_000.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(kv_form) = res_form else { panic!("expected KeyVal") };
    assert_eq!(kv_form.get("warpCount").and_then(|v| v.as_i64()), Some(1));
    let (_, b1) = sys.get_ball_snapshot(100_000_001).await.unwrap().unwrap();
    let (_, b2) = sys.get_ball_snapshot(100_000_002).await.unwrap().unwrap();
    assert!(matches!(b1.mode, MovementMode::Warp { .. }));
    assert!(matches!(b2.mode, MovementMode::Stop));
}

#[tokio::test]
async fn fleet_warp_runs_hooks_dungeon_exit_and_warp_bubble_interdiction() {
    let ctx = test_ctx_with_clock(1_800_001_400);
    let (_fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let kw = HashMap::new();
    let now = ctx.now_unix();
    mark_online(&ctx, 90_000_001, fc_sess.session_id, now);
    mark_online(&ctx, 90_000_002, 82_002, now);
    mark_online(&ctx, 90_000_003, 82_003, now);
    mark_online(&ctx, 90_000_004, 82_004, now);

    // Seed active dungeon at origin so all warped members emit OnExitingDungeon on direct FleetWarp.
    let origin_pos = [0.0, 0.0, 0.0];
    crate::services::pve_dungeon_runtime::with_dungeon_runtime_store(&ctx, |store| {
        store.instances.insert(
            959_101,
            eve_db::DungeonRuntimeInstance {
                instance_id: 959_101,
                dungeon_id: 9_901,
                dungeon_name: "Direct Fleet Exit Pocket".into(),
                category: "Deadspace".into(),
                archetype: "Deadspace".into(),
                solar_system_id: sys_id,
                owner_char_id: 90_000_001,
                base_position: origin_pos,
                disallow_mwd: false,
                entry_gate_ball_id: None,
                entry_gate_pos: None,
                entry_gate_locked: false,
                current_room_index: 0,
                rooms: vec![eve_db::DungeonRuntimeRoom {
                    room_index: 0,
                    room_id: 9_901_00,
                    room_name: "Entry".into(),
                    position: origin_pos,
                    gate_destination_pos: origin_pos,
                    gated: false,
                    disallow_mwd: None,
                    gate_ball_id: None,
                    gate_locked: false,
                    objects: Vec::new(),
                    npc_groups: Vec::new(),
                    triggers: Vec::new(),
                    spawned: false,
                    cleared: false,
                    ..Default::default()
                }],
                completed: false,
                despawned: false,
                active_objective_id: 0,
                last_completed_task_id: None,
                completed_task_ids: Vec::new(),
                created_unix: now,
                ..Default::default()
            },
        );
    });

    // Seed wingmate 90_000_002 with active non-warp module cycle, deployed drone, safe logoff,
    // and make 90_000_003 interdiction-nullified, while placing 90_000_004 inside an origin bubble.
    sys.add_module_cycle(crate::system_actor::ActiveModuleCycle {
        ship_id: 100_000_002,
        module_id: 77_002,
        module_type_id: 519,
        owner_char_id: Some(90_000_002),
        effect_name: "moduleBonusMicrowarpdrive".into(),
        is_mwd_or_mjd: true,
        repeat: 1,
        duration_s: 10.0,
        ..Default::default()
    })
    .await
    .unwrap();
    crate::services::drone_test_helpers::seed_active_drone(
        &ctx,
        crate::services::combat_crimewatch::DroneSpaceState {
            ball_id: 88_002,
            drone_item_id: 88_002,
            type_id: 2454,
            group_id: 100,
            owner_char_id: 90_000_002,
            original_owner_char_id: 90_000_002,
            parent_ship_id: 100_000_002,
            solar_system_id: sys_id,
            target_id: None,
            activity_state: 0,
            assist_char_id: None,
            guard_char_id: None,
            is_abandoned: false,
            is_sentry: false,
            bandwidth_used: 5.0,
            max_velocity: 1000.0,
            optimal_range: 5000.0,
            damage_vector: eve_destiny::DamageVector::default(),
            shield_repair_amount: 0.0,
            armor_repair_amount: 0.0,
            hull_repair_amount: 0.0,
            web_speed_factor: 0.0,
            neut_amount: 0.0,
            ecm_strength: 0.0,
            tp_sig_bonus_pct: 0.0,
        },
    );
    crate::sp6_lifecycle::with_mut_sp6_state(&ctx.db, |st| {
        st.safe_logoffs.insert(
            90_000_002,
            eve_db::SafeLogoffEntry {
                char_id: 90_000_002,
                ship_id: 100_000_002,
                solar_system_id: sys_id,
                started_unix: now,
                complete_at_unix: now + 30,
                completed: false,
            },
        );
    });

    // Place 90_000_004 far enough inside an origin bubble at (5_000_000, 0, 0), and put FC/WC/SC
    // within 50 km of (0,0,0) inside the dungeon pocket.
    sys.set_ball_position(100_000_003, DVec3::new(10_000.0, 0.0, 0.0)).await;
    sys.set_ball_position(100_000_004, DVec3::new(5_000_000.0, 0.0, 0.0)).await;
    sys.set_interdiction_nullified(100_000_003, true).await.unwrap();
    sys.add_warp_bubble(771_001, DVec3::new(5_000_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();

    let dest = DVec3::new(100_000_000.0, 0.0, 0.0);
    // Catch bubble 100 km in front of destination with 20 km radius -> intercepts non-nullified at x ~ 99_880_000.
    sys.add_warp_bubble(771_002, DVec3::new(99_900_000.0, 0.0, 0.0), 20_000.0)
        .await
        .unwrap();

    let _ = ctx.drain_notifications_for_char(90_000_001);
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let _ = ctx.drain_notifications_for_char(90_000_003);

    let res = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "FleetWarp",
        &[
            PyRep::Real(dest.x),
            PyRep::Real(dest.y),
            PyRep::Real(dest.z),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(kv) = res else { panic!("expected KeyVal") };
    assert_eq!(kv.get("warpCount").and_then(|v| v.as_i64()), Some(3));

    // Origin bubble member 90_000_004 was skipped.
    let (_, b4) = sys.get_ball_snapshot(100_000_004).await.unwrap().unwrap();
    assert!(matches!(b4.mode, MovementMode::Stop));

    // Non-nullified FC & WC were intercepted by destination catch bubble; nullified SC bypassed to dest.
    let (_, b1) = sys.get_ball_snapshot(100_000_001).await.unwrap().unwrap();
    let (_, b2) = sys.get_ball_snapshot(100_000_002).await.unwrap().unwrap();
    let (_, b3) = sys.get_ball_snapshot(100_000_003).await.unwrap().unwrap();
    match (b1.mode, b2.mode, b3.mode) {
        (
            MovementMode::Warp { destination: d1, .. },
            MovementMode::Warp { destination: d2, .. },
            MovementMode::Warp { destination: d3, .. },
        ) => {
            assert!(d1.x < 99_950_000.0, "FC must be intercepted by catch bubble, got {d1:?}");
            assert!(d2.x < 99_950_000.0, "WC must be intercepted by catch bubble, got {d2:?}");
            assert!((d3.x - dest.x).abs() < 1.0, "nullified SC must bypass catch bubble, got {d3:?}");
        }
        other => panic!("expected Warp modes for warped members, got {other:?}"),
    }

    // Per-member hooks ran for wingmate 90_000_002: module stopped, drone abandoned, safe logoff aborted, dungeon exited.
    assert!(!sys.has_module_cycle(100_000_002, 77_002).await);
    assert!(
        crate::services::drone_test_helpers::active_drone(&ctx, 88_002)
            .unwrap()
            .is_abandoned
    );
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sp6_lifecycle.safe_logoffs.contains_key(&90_000_002));
    });
    assert!(ctx
        .drain_notifications_for_char(90_000_001)
        .iter()
        .any(|n| n.event == "OnExitingDungeon"));
    assert!(ctx
        .drain_notifications_for_char(90_000_002)
        .iter()
        .any(|n| n.event == "OnExitingDungeon"));
}

#[tokio::test]
async fn warp_to_member_enforces_movement_guard_and_pre_warp_common_for_cloak_scram_docked_and_too_close() {
    let ctx = test_ctx_with_clock(100_000);
    let (_fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);

    // 1. Too close (< 150 km raw distance, and >= 150 km target pulled < 150 km by minRange) -> WarpDistanceTooClose.
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(90_000_002)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "WarpDistanceTooClose"
    );
    sys.set_ball_position(100_000_003, glam::DVec3::new(200_000.0, 0.0, 0.0)).await;
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut fc_sess,
            "fleetObjectHandler",
            "WarpToMember",
            &[PyRep::Integer(90_000_003), PyRep::Real(100_000.0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "WarpDistanceTooClose"
    );
    sys.set_ball_position(100_000_003, glam::DVec3::new(400_000.0, 0.0, 0.0)).await;

    // 2. Cloaked caller (CombatState non-covert cloak and active DB non-covert cloak module) -> CloakedCannotWarp.
    sys.modify_combat_state(100_000_001, |cs| {
        cs.is_cloaked = true;
        cs.is_covert_cloak = false;
        cs.gate_cloak_remaining_ticks = 0;
    })
    .await
    .unwrap();
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(90_000_003)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CloakedCannotWarp"
    );
    sys.modify_combat_state(100_000_001, |cs| {
        cs.is_cloaked = false;
    })
    .await
    .unwrap();

    let cloak_mod_id = 911_371_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            cloak_mod_id,
            eve_db::InventoryItemRecord {
                item_id: cloak_mod_id,
                type_id: 11370,
                owner_id: 90_000_001,
                location_id: 100_000_001,
                flag: eve_db::InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_active_states.insert(cloak_mod_id);
    });
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(90_000_003)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CloakedCannotWarp"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_active_states.remove(&cloak_mod_id);
    });

    // 3. Warp-scrambled, inside warp disruption bubble, and MJD-spooling (`validate_pre_warp_common`).
    sys.set_warp_scramble_strength(100_000_001, 2).await.unwrap();
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(90_000_003)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    sys.set_warp_scramble_strength(100_000_001, 0).await.unwrap();

    let bubble_id = 771_901_i64;
    sys.add_warp_bubble(bubble_id, glam::DVec3::ZERO, 20_000.0).await.unwrap();
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(90_000_003)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "WarpScrambled"
    );
    sys.remove_warp_bubble(bubble_id).await.unwrap();

    sys.add_module_cycle(crate::system_actor::ActiveModuleCycle {
        ship_id: 100_000_001,
        module_id: 77_901,
        module_type_id: 4383,
        owner_char_id: Some(90_000_001),
        effect_name: "microJumpDrive".into(),
        is_mwd_or_mjd: true,
        mjd_jump_distance_m: Some(100_000.0),
        repeat: 0,
        duration_s: 12.0,
        ..Default::default()
    })
    .await
    .unwrap();
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(90_000_003)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "CantMoveWhileModuleActive"
    );
    sys.remove_module_cycle(100_000_001, 77_901).await.unwrap();

    // 4. Docked caller -> ShipNotInSpace; docked target -> TargetNotInSpace.
    fc_sess.station_id = Some(60_003_760);
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(90_000_003)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "ShipNotInSpace"
    );
    fc_sess.station_id = None;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_003).unwrap().station_id = Some(60_003_760);
    });
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(90_000_003)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetNotInSpace"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_003).unwrap().station_id = None;
    });

    // 5. Covert-cloaked caller is allowed through `validate_ship_movement` and keeps cloak active.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&cloak_mod_id).unwrap().type_id = 11578;
        snap.module_active_states.insert(cloak_mod_id);
    });
    sys.modify_combat_state(100_000_001, |cs| {
        cs.is_cloaked = true;
        cs.is_covert_cloak = true;
    })
    .await
    .unwrap();
    let (_, before_ball) = sys.get_ball_snapshot(100_000_001).await.unwrap().unwrap();
    assert!(matches!(before_ball.mode, MovementMode::Stop));

    let ok_res = handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(90_000_003)], &kw)
        .await
        .unwrap()
        .expect("covert-cloaked WarpToMember must succeed");
    assert!(matches!(ok_res, PyRep::KeyVal(_)));
    let (_, after_ball) = sys.get_ball_snapshot(100_000_001).await.unwrap().unwrap();
    assert!(matches!(after_ball.mode, MovementMode::Warp { .. }));
    let cs = sys.get_combat_state(100_000_001).await.unwrap().unwrap();
    assert!(cs.is_cloaked && cs.is_covert_cloak);
}

#[tokio::test]
async fn squad_wing_and_fleet_warp_reject_cloaked_scrambled_docked_and_too_close_members() {
    let kw = HashMap::new();
    let sys_id = 30_000_142_u32;
    let dest = glam::DVec3::new(50_000_000.0, 0.0, 0.0);

    // Scenario A: Squad warp (SquadCommander 90_000_003) and Wing warp (WingCommander 90_000_002).
    {
        let ctx = test_ctx_with_clock(100_000);
        let (fleet_id, fc_sess) = seed_multi_member_fleet(&ctx).await;
        let sys = ctx.systems.get_or_spawn(sys_id);

        // Put 90_000_004 into Wing1 / Squad1 alongside SC 90_000_003, and add docked 90_000_006 into Wing1 / Squad2.
        let extra_cid = 90_000_006_i64;
        ctx.db.with_mut_snapshot(|snap| {
            let mut ch = snap.characters[&90_000_004].clone();
            ch.character_id = extra_cid;
            ch.user_id = 90_000_006;
            ch.active_ship_id = 100_000_006;
            ch.solar_system_id = sys_id;
            ch.station_id = Some(60_003_760);
            ch.structure_id = None;
            snap.characters.insert(extra_cid, ch);

            let fleet = snap.fleets.get_mut(&fleet_id).unwrap();
            fleet.members.push(extra_cid);
            if let Some(m1) = fleet.member_details.get_mut(&90_000_001) {
                m1.wing_id = None;
                m1.squad_id = None;
            }
            if let Some(m2) = fleet.member_details.get_mut(&90_000_002) {
                m2.squad_id = None;
            }
            if let Some(m4) = fleet.member_details.get_mut(&90_000_004) {
                m4.wing_id = Some(710_001);
                m4.squad_id = Some(720_001);
            }
            fleet.member_details.insert(
                extra_cid,
                eve_db::FleetMemberRecord {
                    character_id: extra_cid,
                    role: eve_db::FleetRole::SquadMember,
                    wing_id: Some(710_001),
                    squad_id: Some(720_002),
                    takes_fleet_warp: true,
                    ..Default::default()
                },
            );
        });

        // Position everyone in range (< 8,000 km) of SC/WC at 49,500 km, except 90_000_004 eligible at 49,400 km.
        for (sid, x) in [
            (100_000_001, 49_500_000.0),
            (100_000_002, 49_550_000.0),
            (100_000_003, 49_500_000.0),
            (100_000_004, 49_400_000.0),
        ] {
            sys.set_ball_position(sid, glam::DVec3::new(x, 0.0, 0.0)).await;
        }

        // Docked SC rejected directly with ShipNotInSpace.
        let mut sc_sess = fc_sess.clone();
        sc_sess.character_id = Some(90_000_003);
        sc_sess.ship_id = Some(100_000_003);
        sc_sess.station_id = Some(60_003_760);
        assert_eq!(
            handle_fleet_ops(
                &ctx,
                &mut sc_sess,
                "fleetObjectHandler",
                "WarpFleet",
                &[PyRep::Real(dest.x), PyRep::Real(dest.y), PyRep::Real(dest.z)],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "ShipNotInSpace"
        );
        sc_sess.station_id = None;

        // Cloak SC 90_000_003 with CombatState non-covert cloak: Squad Warp skips SC while warping squadmate 90_000_004.
        sys.modify_combat_state(100_000_003, |cs| {
            cs.is_cloaked = true;
            cs.is_covert_cloak = false;
        })
        .await
        .unwrap();
        let sq_res = handle_fleet_ops(
            &ctx,
            &mut sc_sess,
            "fleetObjectHandler",
            "WarpFleet",
            &[PyRep::Real(dest.x), PyRep::Real(dest.y), PyRep::Real(dest.z)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        let PyRep::KeyVal(sq_kv) = sq_res else { panic!("expected KeyVal") };
        assert_eq!(sq_kv.get("warpCount").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(
            sq_kv.get("warpedMemberIDs"),
            Some(&PyRep::List(vec![PyRep::Integer(90_000_004)]))
        );
        assert!(matches!(sys.get_ball_snapshot(100_000_003).await.unwrap().unwrap().1.mode, MovementMode::Stop));
        assert!(sys.get_combat_state(100_000_003).await.unwrap().unwrap().is_cloaked);
        assert!(matches!(sys.get_ball_snapshot(100_000_004).await.unwrap().unwrap().1.mode, MovementMode::Warp { .. }));
        assert!(matches!(sys.get_ball_snapshot(100_000_001).await.unwrap().unwrap().1.mode, MovementMode::Stop));
        assert!(matches!(sys.get_ball_snapshot(100_000_002).await.unwrap().unwrap().1.mode, MovementMode::Stop));

        // Now Wing warp by WC 90_000_002: cloak WC via DB module, scramble SC 90_000_003, leave 90_000_006 docked,
        // and place an ineligible too-close Wing1 member? Actually uncloak WC? No, test all Wing1 blocked -> warpCount == 0!
        let cloak_wc = 911_372_i64;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                cloak_wc,
                eve_db::InventoryItemRecord {
                    item_id: cloak_wc,
                    type_id: 11370,
                    owner_id: 90_000_002,
                    location_id: 100_000_002,
                    flag: eve_db::InventoryFlag::HiSlot0,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
            snap.module_active_states.insert(cloak_wc);
            // Move 90_000_004 outside Wing1 into Wing2 so Wing1 only contains WC, SC, and docked 90_000_006.
            snap.fleets.get_mut(&fleet_id).unwrap().member_details.get_mut(&90_000_004).unwrap().wing_id = Some(710_002);
        });
        sys.modify_combat_state(100_000_003, |cs| {
            cs.is_cloaked = false;
        })
        .await
        .unwrap();
        sys.set_warp_scramble_strength(100_000_003, 1).await.unwrap();
        let mut wc_sess = fc_sess.clone();
        wc_sess.character_id = Some(90_000_002);
        wc_sess.ship_id = Some(100_000_002);
        let wing_res = handle_fleet_ops(
            &ctx,
            &mut wc_sess,
            "fleetObjectHandler",
            "FleetWarp",
            &[PyRep::Real(dest.x), PyRep::Real(dest.y), PyRep::Real(dest.z)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        let PyRep::KeyVal(wing_kv) = wing_res else { panic!("expected KeyVal") };
        assert_eq!(wing_kv.get("warpCount").and_then(|v| v.as_i64()), Some(0));
        assert!(matches!(sys.get_ball_snapshot(100_000_002).await.unwrap().unwrap().1.mode, MovementMode::Stop));
        assert!(matches!(sys.get_ball_snapshot(100_000_003).await.unwrap().unwrap().1.mode, MovementMode::Stop));
    }

    // Scenario B: Fleet warp (FleetCommander 90_000_001) rejects DB-docked caller, then skips cloaked/scrambled/too-close/docked members.
    {
        let ctx = test_ctx_with_clock(100_000);
        let (_fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
        let sys = ctx.systems.get_or_spawn(sys_id);

        for (sid, x) in [
            (100_000_001, 49_500_000.0),
            (100_000_002, 49_520_000.0),
            (100_000_003, 49_540_000.0),
            (100_000_004, 49_900_000.0), // 100 km from dest -> < 150 km too close
        ] {
            sys.set_ball_position(sid, glam::DVec3::new(x, 0.0, 0.0)).await;
        }

        // DB-docked FC rejected even if session.station_id == None.
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&90_000_001).unwrap().station_id = Some(60_003_760);
        });
        assert_eq!(
            handle_fleet_ops(
                &ctx,
                &mut fc_sess,
                "fleetObjectHandler",
                "FleetWarp",
                &[PyRep::Real(dest.x), PyRep::Real(dest.y), PyRep::Real(dest.z)],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "ShipNotInSpace"
        );
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&90_000_001).unwrap().station_id = None;
            snap.characters.get_mut(&90_000_005).unwrap().solar_system_id = sys_id;
            snap.characters.get_mut(&90_000_005).unwrap().station_id = Some(60_003_760);
        });

        // WC cloaked, SC warp-scrambled, SM4 too close (< 150km), SM5 docked in same system.
        sys.modify_combat_state(100_000_002, |cs| {
            cs.is_cloaked = true;
            cs.is_covert_cloak = false;
        })
        .await
        .unwrap();
        sys.modify_combat_state(100_000_003, |cs| {
            cs.is_cloaked = false;
        })
        .await
        .unwrap();
        sys.set_warp_scramble_strength(100_000_003, 1).await.unwrap();

        let fl_res = handle_fleet_ops(
            &ctx,
            &mut fc_sess,
            "fleetObjectHandler",
            "FleetWarp",
            &[PyRep::Real(dest.x), PyRep::Real(dest.y), PyRep::Real(dest.z)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        let PyRep::KeyVal(fl_kv) = fl_res else { panic!("expected KeyVal") };
        assert_eq!(fl_kv.get("warpCount").and_then(|v| v.as_i64()), Some(1));
        assert_eq!(
            fl_kv.get("warpedMemberIDs"),
            Some(&PyRep::List(vec![PyRep::Integer(90_000_001)]))
        );
        assert!(matches!(sys.get_ball_snapshot(100_000_001).await.unwrap().unwrap().1.mode, MovementMode::Warp { .. }));
        for blocked_sid in [100_000_002_i64, 100_000_003, 100_000_004] {
            assert!(
                matches!(sys.get_ball_snapshot(blocked_sid).await.unwrap().unwrap().1.mode, MovementMode::Stop),
                "blocked fleet member {blocked_sid} must stay in Stop mode"
            );
        }
        assert!(sys.get_combat_state(100_000_002).await.unwrap().unwrap().is_cloaked);
    }
}

