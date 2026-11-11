//! Edge-case behavioral tests for `fleet_social_fleet_ops`.

use super::fleet_social_fleet_ops_tests::{seed_multi_member_fleet, test_ctx_and_clock};
use super::*;
use eve_db::{BookmarkRecord, ServerClock};
use eve_machonet::SessionState;
use std::collections::HashMap;

fn make_member_session(char_id: i64, fleet_id: i64, sys_id: u32, ship_id: i64) -> SessionState {
    let mut s = SessionState::new(char_id as u64);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(98_000_001);
    s.solar_system_id = Some(sys_id);
    s.ship_id = Some(ship_id);
    s.fleet_id = Some(fleet_id);
    s
}

#[tokio::test]
async fn send_broadcast_role_cooldown_scope_and_filter_edges() {
    let (ctx, clock) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, fc_sess) = seed_multi_member_fleet(&ctx).await;

    // Missing fleet -> NotInFleet
    let mut no_fleet_sess = fc_sess.clone();
    no_fleet_sess.fleet_id = None;
    let err_nf = handle_send_broadcast(&ctx, &mut no_fleet_sess, "SendBroadcast", &[PyRep::String("NeedShield".into())])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err_nf.code, "NotInFleet");

    // SquadMember (90_000_004) denied "Target" broadcast -> CannotBroadcastInFleet,
    // but allowed unrestricted "NeedShield"
    let mut mem_sess = make_member_session(90_000_004, fleet_id, 30_000_142, 100_000_004);
    let err_role = handle_send_broadcast(&ctx, &mut mem_sess, "SendBroadcast", &[PyRep::String("Target".into())])
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err_role.code, "CannotBroadcastInFleet");

    // Mute "NeedShield" filter on FC (90_000_001)
    handle_set_broadcast_filter(
        &ctx,
        &fc_sess,
        &[PyRep::String("NeedShield".into()), PyRep::Bool(false)],
    )
    .unwrap();

    let bcast_res = handle_send_broadcast(
        &ctx,
        &mut mem_sess,
        "BroadcastToSystem",
        &[PyRep::String("NeedShield".into()), PyRep::Integer(0)],
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::List(recips) = bcast_res.dict_get("recipientIDs").unwrap() else { panic!() };
    assert!(!recips.contains(&PyRep::Integer(90_000_001)));
    assert!(recips.contains(&PyRep::Integer(90_000_002)));
    assert!(!recips.contains(&PyRep::Integer(90_000_005)));

    // Immediate identical repeat triggers BroadcastSpamCooldown
    let err_spam = handle_send_broadcast(
        &ctx,
        &mut mem_sess,
        "BroadcastToSystem",
        &[PyRep::String("NeedShield".into()), PyRep::Integer(0)],
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_spam.code, "BroadcastSpamCooldown");

    // Different name bypasses cooldown immediately
    handle_send_broadcast(
        &ctx,
        &mut mem_sess,
        "BroadcastToSystem",
        &[PyRep::String("NeedArmor".into()), PyRep::Integer(0)],
    )
    .await
    .unwrap()
    .unwrap();

    // Advance 3s and send identical NeedShield -> succeeds
    clock.advance_secs(3);
    handle_send_broadcast(
        &ctx,
        &mut mem_sess,
        "BroadcastToSystem",
        &[PyRep::String("NeedShield".into()), PyRep::Integer(0)],
    )
    .await
    .unwrap()
    .unwrap();

    // GetBroadcastHistory for non-member fails NotInFleet
    assert_eq!(
        handle_get_broadcast_history(&ctx, &no_fleet_sess).unwrap_err().code,
        "NotInFleet"
    );
}

#[tokio::test]
async fn fleet_ops_warp_to_member_stale_fleet_roles_clamping_and_regroup_edges() {
    let (ctx, _) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let mut wc_sess = make_member_session(90_000_002, fleet_id, 30_000_142, 100_000_002);
    let mut mem_sess = make_member_session(90_000_004, fleet_id, 30_000_142, 100_000_004);
    let kw = HashMap::new();

    // Stale fleet_id clears session.fleet_id = None and returns FleetNotFound
    let mut stale_sess = make_member_session(90_000_001, 999_999, 30_000_142, 100_000_001);
    let err_stale = handle_fleet_ops(&ctx, &mut stale_sess, "fleetObjectHandler", "GetMotd", &[], &kw)
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(err_stale.code, "FleetNotFound");
    assert_eq!(stale_sess.fleet_id, None);

    // Role denials for SquadMember
    assert_eq!(
        handle_fleet_ops(&ctx, &mut mem_sess, "fleetObjectHandler", "SetMotd", &[PyRep::String("x".into())], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetPermissionDenied"
    );
    assert_eq!(
        handle_fleet_ops(&ctx, &mut mem_sess, "fleetObjectHandler", "SetAutoJoinSquadID", &[PyRep::Integer(720_001)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetPermissionDenied"
    );
    assert_eq!(
        handle_fleet_ops(&ctx, &mut mem_sess, "fleetObjectHandler", "GetFleetComposition", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetRoleDenied"
    );
    assert_eq!(
        handle_fleet_ops(&ctx, &mut mem_sess, "fleetObjectHandler", "Regroup", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetPermissionDenied"
    );

    // SetFleetMaxSize clamped to 1..=256
    handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "SetFleetMaxSize", &[PyRep::Integer(999)], &kw)
        .await
        .unwrap()
        .unwrap();
    let max_sz = handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "GetFleetMaxSize", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(max_sz.as_i64(), Some(256));

    // WarpToMember error cases
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(99_999_999)], &kw)
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
    let _ = ctx.systems.get_or_spawn(30_000_142).remove_ball(100_000_003).await;
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "WarpToMember", &[PyRep::Integer(90_000_003)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "TargetNotInSpace"
    );

    // WingCommander Regroup only regroups wing1 members accepting regroup within 500km
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_003).unwrap().station_id = None;
        snap.fleets
            .get_mut(&fleet_id)
            .unwrap()
            .member_details
            .get_mut(&90_000_001)
            .unwrap()
            .accepts_regroup = false;
    });
    let regrouped = handle_fleet_ops(&ctx, &mut wc_sess, "fleetObjectHandler", "Regroup", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        regrouped.dict_get("regroupedMemberIDs"),
        Some(&PyRep::List(vec![PyRep::Integer(90_000_003)]))
    );
}

#[test]
fn compute_oriented_formation_offset_degenerate_axes_and_wall_diamond_arrow_branches() {
    // Formation 0 -> zero
    assert_eq!(
        compute_oriented_formation_offset(0, 1, 1_000.0, 5.0, DVec3::X, DVec3::ZERO),
        DVec3::ZERO
    );
    // Formation 5 clamps relative_delta length to 24_000.0
    let rel = compute_oriented_formation_offset(5, 1, 1_000.0, 5.0, DVec3::ZERO, DVec3::new(50_000.0, 0.0, 0.0));
    assert!((rel.length() - 24_000.0).abs() < 1e-3);

    // forward == +Y uses +Z ref_up without NaN
    let up_y = compute_oriented_formation_offset(1, 1, 1_000.0, 5.0, DVec3::Y, DVec3::ZERO);
    assert!(!up_y.x.is_nan() && !up_y.y.is_nan() && !up_y.z.is_nan());

    // Wall lies in the plane perpendicular to forward (+X -> right=+Z, up=+Y) and slot > 12 steps up in rank
    let wall_slot1 = compute_oriented_formation_offset(1, 1, 1_000.0, 15.0, DVec3::X, DVec3::ZERO);
    let wall_slot13 = compute_oriented_formation_offset(1, 13, 1_000.0, 15.0, DVec3::X, DVec3::ZERO);
    assert!(wall_slot13.x.abs() < 1e-6 && wall_slot13.y > 0.0 && wall_slot1.z > 0.0);

    // Diamond ring 2 (slot_idx = 7) has 2x radius relative to ring 1 (slot_idx = 1)
    let dia1 = compute_oriented_formation_offset(4, 1, 1_000.0, 15.0, DVec3::X, DVec3::ZERO);
    let dia7 = compute_oriented_formation_offset(4, 7, 1_000.0, 15.0, DVec3::X, DVec3::ZERO);
    assert!((dia7.length() - 2.0 * dia1.length()).abs() < 1e-3);

    // Arrow odd vs even rank
    let arr1 = compute_oriented_formation_offset(3, 1, 1_000.0, 15.0, DVec3::X, DVec3::ZERO);
    let arr2 = compute_oriented_formation_offset(3, 2, 1_000.0, 15.0, DVec3::X, DVec3::ZERO);
    assert!((arr1.z + arr2.z).abs() < 1e-6);
}

#[tokio::test]
async fn fleet_warp_bookmark_restrictions_opt_out_and_distance_skips() {
    let (ctx, _) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, fc_sess) = seed_multi_member_fleet(&ctx).await;
    let mem_sess = make_member_session(90_000_004, fleet_id, 30_000_142, 100_000_004);
    let kw = HashMap::new();

    // SquadMember denied FleetWarp
    assert_eq!(
        handle_fleet_warp(&ctx, &mem_sess, &[PyRep::Integer(30_000_142)], &kw).await.unwrap().unwrap_err().code,
        "FleetPermissionDenied"
    );

    // Missing destination -> InvalidArgument
    assert_eq!(
        handle_fleet_warp(&ctx, &fc_sess, &[], &kw).await.unwrap().unwrap_err().code,
        "InvalidArgument"
    );

    // Zarzakh (30100000) bookmark rejected with DunBlacklistCannotWarp
    let zarzakh_bm = 900_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            zarzakh_bm,
            BookmarkRecord {
                bookmark_id: zarzakh_bm,
                owner_id: 90_000_001,
                label: "Zarzakh".into(),
                solar_system_id: 30_100_000,
                x: 1_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: "Zarzakh".into(),
                folder_id: None,
                is_shared: false,
            },
        );
        // Set 90_000_002 takes_fleet_warp = false -> opt-out
        snap.fleets
            .get_mut(&fleet_id)
            .unwrap()
            .member_details
            .get_mut(&90_000_002)
            .unwrap()
            .takes_fleet_warp = false;
    });
    assert_eq!(
        handle_fleet_warp(&ctx, &fc_sess, &[PyRep::Integer(zarzakh_bm)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "DunBlacklistCannotWarp"
    );

    // Foreign inaccessible bookmark -> BookmarkNotFound
    ctx.db.with_mut_snapshot(|snap| {
        snap.bookmarks.insert(
            999_999,
            BookmarkRecord {
                bookmark_id: 999_999,
                owner_id: 99_999_888,
                label: "Private".into(),
                solar_system_id: 30_000_142,
                x: 1_000_000.0,
                y: 0.0,
                z: 0.0,
                item_id: None,
                memo: "Private".into(),
                folder_id: None,
                is_shared: false,
            },
        );
    });
    assert_eq!(
        handle_fleet_warp(&ctx, &fc_sess, &[PyRep::Integer(999_999)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "BookmarkNotFound"
    );

    // Explicit coordinates at 1_000_000m along +X: 90_000_002 opted out (OnFleetMoveFailed emitted)
    let fc_uid = ctx.db.with_mut_snapshot(|snap| {
        let uid = snap.characters[&90_000_001].user_id;
        snap.accounts.entry("fc_test_user".into()).or_insert_with(|| eve_db::AccountRecord {
            user_id: uid,
            username: "fc_test_user".into(),
            password_token: String::new(),
            role: 0,
            banned_until_unix: None,
            ban_reason: None,
        });
        uid
    });
    ctx.db
        .acquire_online_lock(fc_uid, fc_sess.session_id, Some(90_000_001), true, ctx.now_unix())
        .unwrap();
    let _ = ctx.drain_notifications_for_char(90_000_001);
    let warp_res = handle_fleet_warp(
        &ctx,
        &fc_sess,
        &[
            PyRep::Real(1_000_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let skipped_opt_out = warp_res.dict_get("skippedOptOutMemberIDs").unwrap();
    assert_eq!(skipped_opt_out, &PyRep::List(vec![PyRep::Integer(90_000_002)]));
    assert!(ctx
        .drain_notifications_for_char(90_000_001)
        .iter()
        .any(|n| n.event == "OnFleetMoveFailed"));
}
