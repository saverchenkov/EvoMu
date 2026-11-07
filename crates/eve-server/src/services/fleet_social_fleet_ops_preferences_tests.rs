//! Semantic unit tests split from `fleet_social_fleet_ops_tests`.

use super::*;
use eve_machonet::SessionState;
use std::collections::{HashMap, HashSet};

#[tokio::test]
async fn fleet_ops_broadcasts_filters_and_cooldown_cover_scopes() {
    let ctx = test_ctx_with_clock(1_800_000_100);
    let (fleet_id, mut session) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // Unauthenticated & not-in-fleet errors
    let mut no_auth = SessionState::new(2);
    assert_eq!(
        handle_fleet_ops(&ctx, &mut no_auth, "fleetMgr", "SendBroadcast", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "Unauthorized"
    );
    let mut no_fleet = SessionState::new(3);
    no_fleet.user_id = Some(90_000_001);
    no_fleet.character_id = Some(90_000_001);
    assert_eq!(
        handle_fleet_ops(&ctx, &mut no_fleet, "fleetMgr", "SendBroadcast", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInFleet"
    );

    // No solar system check
    let saved_sys = session.solar_system_id.take();
    assert_eq!(
        handle_fleet_ops(&ctx, &mut session, "fleetObjectHandler", "SendBroadcast", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInSolarSystem"
    );
    session.solar_system_id = saved_sys;

    // SquadMember denied restricted broadcast ("Target"), allowed distress ("NeedShield")
    let mut mem_sess = SessionState::new(4);
    mem_sess.user_id = Some(90_000_004);
    mem_sess.character_id = Some(90_000_004);
    mem_sess.solar_system_id = Some(30_000_142);
    mem_sess.ship_id = Some(100_000_004);
    mem_sess.fleet_id = Some(fleet_id);
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut mem_sess,
            "fleetMgr",
            "SendBroadcast",
            &[PyRep::String("Target".into()), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotBroadcastInFleet"
    );
    let need_res = handle_fleet_ops(
        &ctx,
        &mut mem_sess,
        "fleetObjectHandler",
        "SendBroadcast",
        &[PyRep::String("NeedShield".into()), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(need_kv) = need_res else {
        panic!("expected KeyVal");
    };
    assert_eq!(need_kv.get("broadcast").unwrap().as_str().unwrap(), "NeedShield");

    // Filter out "EnemySpotted" on member 90_000_004
    ctx.fleet_extras
        .lock()
        .unwrap()
        .broadcast_filters
        .entry(90_000_004)
        .or_default()
        .insert("EnemySpotted".into());

    // FC global broadcast across all 5 members (excluding muted 90_000_004)
    let g_res = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "SendBroadcast",
        &[PyRep::String("EnemySpotted".into()), PyRep::Integer(0), PyRep::Integer(800_001), PyRep::Integer(587)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(g_kv) = g_res else { panic!() };
    let PyRep::List(recips) = g_kv.get("recipientIDs").unwrap() else { panic!() };
    let r_ids: HashSet<i64> = recips.iter().filter_map(|r| r.as_i64()).collect();
    assert!(r_ids.contains(&90_000_001));
    assert!(r_ids.contains(&90_000_005));
    assert!(!r_ids.contains(&90_000_004));

    // Spam cooldown within 2 seconds on identical broadcast
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut session,
            "fleetObjectHandler",
            "SendBroadcast",
            &[PyRep::String("EnemySpotted".into()), PyRep::Integer(0), PyRep::Integer(800_001), PyRep::Integer(587)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "BroadcastSpamCooldown"
    );

    ctx.advance_secs(3);

    // Bubble broadcast (location=2) includes on-grid 90_000_002 (15km) and 90_000_004 (25km), excludes 90_000_003 (400km) & 90_000_005 (Perimeter)
    let b_res = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "BroadcastToBubble",
        &[PyRep::String("Target".into()), PyRep::Integer(0), PyRep::Integer(800_002)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(b_kv) = b_res else { panic!() };
    let PyRep::List(b_recips) = b_kv.get("recipientIDs").unwrap() else { panic!() };
    let b_ids: HashSet<i64> = b_recips.iter().filter_map(|r| r.as_i64()).collect();
    assert!(b_ids.contains(&90_000_002));
    assert!(!b_ids.contains(&90_000_003));
    assert!(!b_ids.contains(&90_000_005));

    // System broadcast includes 90_000_003 (400km same system) and excludes 90_000_005 (Perimeter)
    let s_res = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetMgr",
        "BroadcastToSystem",
        &[PyRep::String("AlignTo".into()), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(s_kv) = s_res else { panic!() };
    let PyRep::List(s_recips) = s_kv.get("recipientIDs").unwrap() else { panic!() };
    let s_ids: HashSet<i64> = s_recips.iter().filter_map(|r| r.as_i64()).collect();
    assert!(s_ids.contains(&90_000_003));
    assert!(!s_ids.contains(&90_000_005));

    // Broadcast history & unmuting
    let hist = handle_fleet_ops(&ctx, &mut session, "fleetObjectHandler", "GetBroadcastHistory", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::List(h_list) = hist else { panic!() };
    assert_eq!(h_list.len(), 4);
}

#[tokio::test]
async fn fleet_ops_warp_to_member_motd_options_and_composition() {
    let ctx = test_ctx_with_clock(1_800_000_200);
    let (_fleet_id, mut session) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // WarpToMember error cases: close target (15km < 150km), different system (90_000_005), non-member (99_999)
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut session,
            "fleetObjectHandler",
            "WarpToMember",
            &[PyRep::Integer(90_000_002)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "WarpDistanceTooClose"
    );
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut session,
            "fleetMgr",
            "WarpToMember",
            &[PyRep::Integer(90_000_005)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "DifferentSolarSystem"
    );
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut session,
            "fleetMgr",
            "WarpToMember",
            &[PyRep::Integer(99_999)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "MemberNotFound"
    );

    // Valid WarpToMember -> 90_000_003 at 400_000m with minRange 10_000m -> lands at x = 390_000
    let w_res = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "WarpToMember",
        &[PyRep::Integer(90_000_003), PyRep::Real(10_000.0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(w_kv) = w_res else { panic!() };
    assert_eq!(w_kv.get("status").unwrap().as_str().unwrap(), "Warping");
    assert!((w_kv.get("destinationX").unwrap().as_f64().unwrap() - 390_000.0).abs() < 1.0);

    // MOTD set / permission denied / readback
    let mut mem_sess = SessionState::new(9);
    mem_sess.user_id = Some(90_000_002);
    mem_sess.character_id = Some(90_000_002);
    mem_sess.fleet_id = session.fleet_id;
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut mem_sess,
            "fleetObjectHandler",
            "SetMotd",
            &[PyRep::String("Denied".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );
    let m_set = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "SetMotdEx",
        &[PyRep::String("Form at Jita 4-4".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(m_kv) = m_set else { panic!() };
    assert_eq!(m_kv.get("motd").unwrap().as_str().unwrap(), "Form at Jita 4-4");
    assert_eq!(
        handle_fleet_ops(&ctx, &mut mem_sess, "fleetObjectHandler", "GetMotd", &[], &kw)
            .await
            .unwrap()
            .unwrap()
            .as_str()
            .unwrap(),
        "Form at Jita 4-4"
    );

    // SetOptions KeyVal + positional, SetAutoJoinSquadID, SetFleetMaxSize / GetFleetMaxSize
    let opts = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "SetOptions",
        &[PyRep::keyval([
            ("isFreeMove", PyRep::Bool(true)),
            ("isRegistered", PyRep::Bool(true)),
            ("isVoiceEnabled", PyRep::Bool(false)),
            ("isLootLogging", PyRep::Bool(true)),
        ])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(o_kv) = opts else { panic!() };
    assert_eq!(o_kv.get("isFreeMove").unwrap(), &PyRep::Bool(true));
    assert_eq!(o_kv.get("isRegistered").unwrap(), &PyRep::Bool(true));

    let aj = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "SetAutoJoinSquadID",
        &[PyRep::Integer(720_002)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(aj_kv) = aj else { panic!() };
    assert_eq!(aj_kv.get("autoJoinSquadID").unwrap(), &PyRep::Integer(720_002));

    let ms = handle_fleet_ops(
        &ctx,
        &mut session,
        "fleetObjectHandler",
        "SetFleetMaxSize",
        &[PyRep::Integer(50)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(ms_kv) = ms else { panic!() };
    assert_eq!(ms_kv.get("maxSize").unwrap(), &PyRep::Integer(50));
    assert_eq!(
        handle_fleet_ops(&ctx, &mut mem_sess, "fleetObjectHandler", "GetFleetMaxSize", &[], &kw)
            .await
            .unwrap()
            .unwrap()
            .as_i64()
            .unwrap(),
        50
    );

    // Member preferences: SetTakesFleetWarp, SetAcceptsRegroupValue, SetAcceptsConduitJumpsValue
    handle_fleet_ops(
        &ctx,
        &mut mem_sess,
        "fleetObjectHandler",
        "SetTakesFleetWarp",
        &[PyRep::Bool(false)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    handle_fleet_ops(
        &ctx,
        &mut mem_sess,
        "fleetMgr",
        "SetAcceptsRegroupValue",
        &[PyRep::Bool(false)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let p_res = handle_fleet_ops(
        &ctx,
        &mut mem_sess,
        "fleetObjectHandler",
        "SetAcceptsConduitJumpsValue",
        &[PyRep::Bool(false)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(p_kv) = p_res else { panic!() };
    assert_eq!(p_kv.get("takesFleetWarp").unwrap(), &PyRep::Bool(false));
    assert_eq!(p_kv.get("acceptsRegroup").unwrap(), &PyRep::Bool(false));
    assert_eq!(p_kv.get("acceptsConduitJumps").unwrap(), &PyRep::Bool(false));

    // GetFleetComposition (fleet members allowed, non-fleet denied)
    let mut outsider = SessionState::default();
    outsider.user_id = Some(99_999_888);
    outsider.character_id = Some(99_999_888);
    assert_eq!(
        handle_fleet_ops(&ctx, &mut outsider, "fleetObjectHandler", "GetFleetComposition", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInFleet"
    );
    let comp_mem = handle_fleet_ops(&ctx, &mut mem_sess, "fleetObjectHandler", "GetFleetComposition", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::Rowset { rows: mem_rows, .. } = comp_mem else { panic!() };
    assert_eq!(mem_rows.len(), 5);
    let comp = handle_fleet_ops(&ctx, &mut session, "fleetObjectHandler", "GetFleetComposition", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let PyRep::Rowset { rows: fc_rows, .. } = comp else { panic!() };
    assert_eq!(fc_rows.len(), 5);

    // Init state / wings delegation + unmatched method
    assert!(handle_fleet_ops(&ctx, &mut session, "fleetObjectHandler", "GetInitState", &[], &kw)
        .await
        .unwrap()
        .is_ok());
    assert!(handle_fleet_ops(&ctx, &mut session, "fleetObjectHandler", "GetWings", &[], &kw)
        .await
        .unwrap()
        .is_ok());
    assert!(handle_fleet_ops(&ctx, &mut session, "unmatched", "UnknownMethod", &[], &kw)
        .await
        .is_none());
}
