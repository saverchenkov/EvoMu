//! Semantic unit tests split from `fleet_social_fleet_ops_tests`.

use super::*;
use eve_db::ServerClock;
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
    assert_eq!(p_kv.get("acceptsFleetWarp").unwrap(), &PyRep::Bool(false));
    assert_eq!(p_kv.get("acceptsRegroup").unwrap(), &PyRep::Bool(false));
    assert_eq!(p_kv.get("acceptsFleetRegroups").unwrap(), &PyRep::Bool(false));
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
async fn wave2_fleet_settings_motd_options_max_size_auto_join_and_composition() {
    let (ctx, _clock) = test_ctx_and_clock(1_800_040_000);
    let (fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();
    for (idx, cid) in [90_000_001_i64, 90_000_002, 90_000_003, 90_000_004, 90_000_005]
        .into_iter()
        .enumerate()
    {
        mark_online(&ctx, cid, 5_000 + idx as u64, 1_800_040_000);
    }

    let mut wc_sess = SessionState::new(502);
    wc_sess.user_id = Some(90_000_002);
    wc_sess.character_id = Some(90_000_002);
    wc_sess.fleet_id = Some(fleet_id);
    wc_sess.solar_system_id = Some(30_000_142);

    let mut member_sess = SessionState::new(504);
    member_sess.user_id = Some(90_000_004);
    member_sess.character_id = Some(90_000_004);
    member_sess.fleet_id = Some(fleet_id);
    member_sess.solar_system_id = Some(30_000_142);

    // SquadMember and WingCommander (non-FC/non-boss) denied on SetMotd (Wave 2 domain/fleets::update_fleet_settings)
    assert!(
        handle_fleet_ops(
            &ctx,
            &mut member_sess,
            "fleetObjectHandler",
            "SetMotd",
            &[PyRep::String("Denied".into())],
            &kw,
        )
        .await
        .unwrap()
        .is_err()
    );
    assert!(
        handle_fleet_ops(
            &ctx,
            &mut wc_sess,
            "fleetObjectHandler",
            "SetMotd",
            &[PyRep::String("DeniedWC".into())],
            &kw,
        )
        .await
        .unwrap()
        .is_err()
    );

    // FleetCommander/Boss updates MOTD via SetMotdEx, emits OnFleetMotdChanged, readable via GetMotd
    let _ = ctx.drain_notifications_for_char(90_000_004);
    let PyRep::KeyVal(motd_kv) = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "SetMotdEx",
        &[PyRep::String("Align D-scan".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!()
    };
    assert_eq!(
        motd_kv.get("motd").and_then(|s| s.as_str()),
        Some("Align D-scan")
    );
    assert_eq!(
        handle_fleet_ops(&ctx, &mut member_sess, "fleetObjectHandler", "GetMotd", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::String("Align D-scan".into())
    );
    assert!(ctx
        .drain_notifications_for_char(90_000_004)
        .iter()
        .any(|n| n.event == "OnFleetMotdChanged"));

    // Non-member calling GetMotd or GetFleetMaxSize fails and clears session.fleet_id
    let mut outsider = SessionState::new(599);
    outsider.user_id = Some(90_000_099);
    outsider.character_id = Some(90_000_099);
    outsider.fleet_id = Some(fleet_id);
    assert!(handle_fleet_ops(&ctx, &mut outsider, "fleetObjectHandler", "GetMotd", &[], &kw)
        .await
        .unwrap()
        .is_err());
    assert_eq!(outsider.fleet_id, None);
    outsider.fleet_id = Some(fleet_id);
    assert_eq!(
        handle_fleet_ops(&ctx, &mut outsider, "fleetObjectHandler", "GetFleetMaxSize", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInFleet"
    );
    assert_eq!(outsider.fleet_id, None);

    // Wave 2 SetOptions: non-boss (WC) rejected even though commander, Boss succeeds via bools/ints/KeyVal
    assert!(handle_fleet_ops(
        &ctx,
        &mut wc_sess,
        "fleetObjectHandler",
        "SetOptions",
        &[PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap()
    .is_err());

    let _ = ctx.drain_notifications_for_char(90_000_004);
    let opt1 = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "SetOptions",
        &[PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(1)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(opt1_kv) = opt1 else { panic!() };
    assert_eq!(opt1_kv.get("isFreeMove").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(opt1_kv.get("isRegistered").and_then(|v| v.as_bool()), Some(true));
    assert!(ctx
        .drain_notifications_for_char(90_000_004)
        .iter()
        .any(|n| n.event == "OnFleetOptionsChanged"));

    let opt2 = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "SetOptions",
        &[PyRep::keyval([
            ("isFreeMove", PyRep::Bool(false)),
            ("isRegistered", PyRep::Bool(false)),
            ("isVoiceEnabled", PyRep::Bool(false)),
            ("isLootLogging", PyRep::Bool(false)),
        ])],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(opt2_kv) = opt2 else { panic!() };
    assert_eq!(opt2_kv.get("isFreeMove").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(opt2_kv.get("isRegistered").and_then(|v| v.as_bool()), Some(false));

    // SetAutoJoinSquadID & SetFleetMaxSize boss-only + [1, 256] clamping
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut wc_sess,
            "fleetObjectHandler",
            "SetAutoJoinSquadID",
            &[PyRep::Integer(720_001)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );
    let PyRep::KeyVal(aj_set_kv) = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "SetAutoJoinSquadID",
        &[PyRep::Integer(720_001)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!()
    };
    assert_eq!(
        aj_set_kv.get("autoJoinSquadID").and_then(|v| v.as_i64()),
        Some(720_001)
    );
    let PyRep::KeyVal(aj_clr_kv) = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "SetAutoJoinSquadID",
        &[PyRep::None],
        &kw,
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!()
    };
    assert_eq!(
        aj_clr_kv.get("autoJoinSquadID"),
        Some(&PyRep::None)
    );

    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut wc_sess,
            "fleetObjectHandler",
            "SetFleetMaxSize",
            &[PyRep::Integer(50)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );
    assert!(handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "SetFleetMaxSize",
        &[PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "GetFleetMaxSize", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(1)
    );
    assert!(handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "SetFleetMaxSize",
        &[PyRep::Integer(999)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());
    assert_eq!(
        handle_fleet_ops(&ctx, &mut fc_sess, "fleetObjectHandler", "GetFleetMaxSize", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::Integer(256)
    );

    // GetFleetComposition: SquadMember denied, Commander gets rowset with 606 fallback on missing ship item
    assert_eq!(
        handle_fleet_ops(&ctx, &mut member_sess, "fleetObjectHandler", "GetFleetComposition", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetRoleDenied"
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get(&90_000_004) {
            snap.items.remove(&ch.active_ship_id);
        }
    });
    let comp = handle_fleet_ops(&ctx, &mut wc_sess, "fleetObjectHandler", "GetFleetComposition", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    if let PyRep::Rowset { columns, rows } = comp {
        assert_eq!(columns[0], "characterID");
        assert_eq!(rows.len(), 5);
        let row_4 = rows.iter().find(|r| r[0] == PyRep::Integer(90_000_004)).unwrap();
        assert_eq!(row_4[1], PyRep::Integer(606)); // fallback ship_type_id
    } else {
        panic!("expected PyRep::Rowset for GetFleetComposition");
    }
}

#[tokio::test]
async fn wave2_broadcast_scopes_up_down_bubble_250km_and_cooldown_key_tuple() {
    let (ctx, clock) = test_ctx_and_clock(1_800_050_000);
    let (fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();
    for (idx, cid) in [90_000_001_i64, 90_000_002, 90_000_003, 90_000_004, 90_000_005]
        .into_iter()
        .enumerate()
    {
        mark_online(&ctx, cid, 6_000 + idx as u64, 1_800_050_000);
    }

    let mut wc_sess = SessionState::new(602);
    wc_sess.user_id = Some(90_000_002);
    wc_sess.character_id = Some(90_000_002);
    wc_sess.fleet_id = Some(fleet_id);
    wc_sess.solar_system_id = Some(30_000_142);

    let mut mem_sess = SessionState::new(604);
    mem_sess.user_id = Some(90_000_004);
    mem_sess.character_id = Some(90_000_004);
    mem_sess.fleet_id = Some(fleet_id);
    mem_sess.solar_system_id = Some(30_000_142);

    // No solar system -> NotInSolarSystem
    let mut no_sys = SessionState::new(699);
    no_sys.user_id = Some(90_000_001);
    no_sys.character_id = Some(90_000_001);
    no_sys.fleet_id = Some(fleet_id);
    assert_eq!(
        handle_fleet_ops(&ctx, &mut no_sys, "fleetObjectHandler", "SendBroadcast", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInSolarSystem"
    );

    // SquadMember denied on restricted "Target", allowed on unrestricted "NeedShield"
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut mem_sess,
            "fleetObjectHandler",
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

    // BROADCAST_DOWN (scope 1) from WingCommander (wing1): reaches sender (90_000_002) and subordinate
    // SquadCommander in wing1 (90_000_003), NOT superior FC (90_000_001) and NOT wing2 (90_000_004/005)
    for cid in [90_000_001_i64, 90_000_002, 90_000_003, 90_000_004, 90_000_005] {
        let _ = ctx.drain_notifications_for_char(cid);
    }
    let PyRep::KeyVal(down_kv) = handle_fleet_ops(
        &ctx,
        &mut wc_sess,
        "fleetObjectHandler",
        "SendBroadcast",
        &[PyRep::String("AlignTo".into()), PyRep::Integer(1), PyRep::Integer(500_001)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!()
    };
    let PyRep::List(down_recips) = down_kv.get("recipientIDs").unwrap() else { panic!() };
    assert_eq!(down_recips.len(), 2);
    assert!(down_recips.contains(&PyRep::Integer(90_000_002)));
    assert!(down_recips.contains(&PyRep::Integer(90_000_003)));
    assert!(ctx.drain_notifications_for_char(90_000_001).is_empty());
    assert_eq!(ctx.drain_notifications_for_char(90_000_002).len(), 1);
    assert_eq!(ctx.drain_notifications_for_char(90_000_003).len(), 1);
    assert!(ctx.drain_notifications_for_char(90_000_004).is_empty());
    assert!(ctx.drain_notifications_for_char(90_000_005).is_empty());

    // BROADCAST_UP (scope 2) from SquadMember 90_000_004 (wing2): reaches FC (90_000_001) + self (90_000_004)
    let PyRep::KeyVal(up_kv) = handle_fleet_ops(
        &ctx,
        &mut mem_sess,
        "fleetObjectHandler",
        "SendBroadcast",
        &[PyRep::String("NeedShield".into()), PyRep::Integer(2)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!()
    };
    let PyRep::List(up_recips) = up_kv.get("recipientIDs").unwrap() else { panic!() };
    assert_eq!(up_recips.len(), 2);
    assert!(up_recips.contains(&PyRep::Integer(90_000_001)));
    assert!(up_recips.contains(&PyRep::Integer(90_000_004)));
    assert_eq!(ctx.drain_notifications_for_char(90_000_001).len(), 1);
    assert!(ctx.drain_notifications_for_char(90_000_002).is_empty());
    assert_eq!(ctx.drain_notifications_for_char(90_000_004).len(), 1);

    // Cooldown key includes item_id, location, scope, and name: identical triggers BroadcastSpamCooldown,
    // changing item_id succeeds immediately, and advancing clock by >=2s clears cooldown
    assert_eq!(
        handle_fleet_ops(
            &ctx,
            &mut mem_sess,
            "fleetObjectHandler",
            "SendBroadcast",
            &[PyRep::String("NeedShield".into()), PyRep::Integer(2)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "BroadcastSpamCooldown"
    );
    assert!(handle_fleet_ops(
        &ctx,
        &mut mem_sess,
        "fleetObjectHandler",
        "SendBroadcast",
        &[PyRep::String("NeedShield".into()), PyRep::Integer(2), PyRep::Integer(777)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());
    clock.advance_secs(2);
    assert!(handle_fleet_ops(
        &ctx,
        &mut mem_sess,
        "fleetObjectHandler",
        "SendBroadcast",
        &[PyRep::String("NeedShield".into()), PyRep::Integer(2), PyRep::Integer(777)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());

    // BroadcastToBubble from FC at (0,0,0):
    // 90_000_002 (15k) & 90_000_004 (25k) are <= 250km -> delivered;
    // 90_000_003 (400k) is > 250km and 90_000_005 is in Perim -> skipped!
    for cid in [90_000_001_i64, 90_000_002, 90_000_003, 90_000_004, 90_000_005] {
        let _ = ctx.drain_notifications_for_char(cid);
    }
    let PyRep::KeyVal(bubble_kv) = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetMgr",
        "BroadcastToBubble",
        &[PyRep::String("Target".into()), PyRep::Integer(0), PyRep::Integer(800_001), PyRep::Integer(587)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!()
    };
    let PyRep::List(bubble_recips) = bubble_kv.get("recipientIDs").unwrap() else { panic!() };
    assert_eq!(bubble_recips.len(), 3);
    assert!(bubble_recips.contains(&PyRep::Integer(90_000_001)));
    assert!(bubble_recips.contains(&PyRep::Integer(90_000_002)));
    assert!(bubble_recips.contains(&PyRep::Integer(90_000_004)));
    assert_eq!(ctx.drain_notifications_for_char(90_000_001).len(), 1);
    assert_eq!(ctx.drain_notifications_for_char(90_000_002).len(), 1);
    assert!(ctx.drain_notifications_for_char(90_000_003).is_empty());
    assert_eq!(ctx.drain_notifications_for_char(90_000_004).len(), 1);
    assert!(ctx.drain_notifications_for_char(90_000_005).is_empty());

    // Without a sender ball, BroadcastToBubble delivers ONLY to the sender
    clock.advance_secs(3);
    fc_sess.ship_id = Some(9_999_999);
    let PyRep::KeyVal(nb_kv) = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetMgr",
        "BroadcastToBubble",
        &[PyRep::String("Target".into()), PyRep::Integer(0), PyRep::Integer(800_001), PyRep::Integer(587)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap() else {
        panic!()
    };
    assert_eq!(nb_kv.get("recipientIDs"), Some(&PyRep::List(vec![PyRep::Integer(90_000_001)])));
    assert_eq!(ctx.drain_notifications_for_char(90_000_001).len(), 1);
    assert!(ctx.drain_notifications_for_char(90_000_002).is_empty());

    // Broadcast filter mute/unmute & broadcast history
    assert!(handle_fleet_ops(
        &ctx,
        &mut wc_sess,
        "fleetObjectHandler",
        "SetBroadcastFilter", // rpc-lint: allow test fixture
        &[PyRep::String("Target".into()), PyRep::Bool(false)],
        &kw,
    )
    .await
    .unwrap()
    .is_ok());
    clock.advance_secs(3);
    let _ = handle_fleet_ops(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "SendBroadcast",
        &[PyRep::String("Target".into()), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert!(ctx.drain_notifications_for_char(90_000_002).is_empty(), "muted member must not receive Target broadcast");
}

