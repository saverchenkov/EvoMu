//! Extra unit and mutation-killing tests for `fleet_social_fleet_ops.rs`.

use crate::services::fleet_social::fleet_social_fleet_ops::{
    compute_oriented_formation_offset,
    fleet_social_fleet_ops_tests::{seed_multi_member_fleet, test_ctx_with_clock},
    handle_fleet_warp,
};
use crate::services::fleet_social::handle;
use eve_db::clock::VirtualServerClock;
use eve_db::FleetRole;
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

fn member_session(char_id: i64, fleet_id: i64, sys_id: u32, ship_id: i64) -> SessionState {
    let mut s = SessionState::new(char_id as u64);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.fleet_id = Some(fleet_id);
    s.solar_system_id = Some(sys_id);
    s.ship_id = Some(ship_id);
    s
}

#[test]
fn compute_oriented_formation_offset_covers_all_formations_axes_and_edge_cases() {
    // Formation 5: relative delta clamped to 24,000m
    let rel = compute_oriented_formation_offset(
        5,
        1,
        5_000.0,
        10_000.0,
        DVec3::X,
        DVec3::new(30_000.0, 0.0, 0.0),
    );
    assert!((rel.length() - 24_000.0).abs() < 1e-6);

    // Slot 0, formation 0, or unknown formation -> ZERO
    assert_eq!(
        compute_oriented_formation_offset(1, 0, 5_000.0, 10_000.0, DVec3::X, DVec3::ZERO),
        DVec3::ZERO
    );
    assert_eq!(
        compute_oriented_formation_offset(0, 2, 5_000.0, 10_000.0, DVec3::X, DVec3::ZERO),
        DVec3::ZERO
    );
    assert_eq!(
        compute_oriented_formation_offset(99, 2, 5_000.0, 10_000.0, DVec3::X, DVec3::ZERO),
        DVec3::ZERO
    );

    // Zero warp_dir falls back to DVec3::X; Y-aligned warp_dir uses DVec3::Z as ref_up
    let wall_zero_dir =
        compute_oriented_formation_offset(1, 1, 5_000.0, 10_000.0, DVec3::ZERO, DVec3::ZERO);
    assert!((wall_zero_dir.length() - 5_000.0).abs() < 1e-6);

    let wall_y_dir =
        compute_oriented_formation_offset(1, 1, 5_000.0, 10_000.0, DVec3::Y, DVec3::ZERO);
    assert!((wall_y_dir.length() - 5_000.0).abs() < 1e-6);

    // Wall formation overflow slot_idx > 12 (uses grid arithmetic)
    let wall_slot_13 =
        compute_oriented_formation_offset(1, 13, 5_000.0, 10_000.0, DVec3::X, DVec3::ZERO);
    assert!(wall_slot_13.length() > 0.0 && wall_slot_13.length() <= 24_000.0);

    // Sphere formation (formation_id == 2)
    let sphere =
        compute_oriented_formation_offset(2, 3, 5_000.0, 12_000.0, DVec3::X, DVec3::ZERO);
    assert!((sphere.length() - 12_000.0).abs() < 1e-3);

    // Arrow formation (formation_id == 3): odd vs even slot_idx mirror across right axis
    let arrow_odd =
        compute_oriented_formation_offset(3, 1, 5_000.0, 10_000.0, DVec3::X, DVec3::ZERO);
    let arrow_even =
        compute_oriented_formation_offset(3, 2, 5_000.0, 10_000.0, DVec3::X, DVec3::ZERO);
    assert!((arrow_odd.x - arrow_even.x).abs() < 1e-6);
    assert!((arrow_odd.z + arrow_even.z).abs() < 1e-6);

    // Diamond formation (formation_id == 4): all 6 octahedron axes + second ring (slot 7)
    for slot in 1..=6 {
        let d =
            compute_oriented_formation_offset(4, slot, 4_000.0, 10_000.0, DVec3::X, DVec3::ZERO);
        assert!((d.length() - 4_000.0).abs() < 1e-6);
    }
    let d_ring2 =
        compute_oriented_formation_offset(4, 7, 4_000.0, 10_000.0, DVec3::X, DVec3::ZERO);
    assert!((d_ring2.length() - 8_000.0).abs() < 1e-6);
}

#[tokio::test]
async fn broadcast_permissions_unrestricted_names_and_bubble_boundary() {
    let ctx = test_ctx_with_clock(1_775_000_000);
    let (fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // SquadMember (90_000_004) can send all 9 unrestricted broadcast types
    let mut sq_member = member_session(90_000_004, fleet_id, 30_000_142, 100_000_004);
    let unrestricted = [
        "NeedArmor",
        "NeedShield",
        "NeedCapacitor",
        "NeedBackup",
        "InPosition",
        "HoldPosition",
        "NeedRepairs",
        "Location",
        "Travel",
    ];
    for (idx, b_name) in unrestricted.iter().enumerate() {
        ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(
            1_775_000_010 + (idx as u64) * 5,
        )));
        let res = handle(
            &ctx,
            &mut sq_member,
            "fleetObjectHandler",
            "SendBroadcast",
            &[PyRep::String((*b_name).into()), PyRep::Integer(0)],
            &kw,
        )
        .await
        .unwrap()
        .expect("unrestricted broadcast must succeed for SquadMember");
        assert_eq!(
            res.dict_get("broadcast").and_then(|v| v.as_str()),
            Some(*b_name)
        );
    }

    // Restricted broadcast ("Target") rejected for SquadMember
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_000_100)));
    let denied = handle(
        &ctx,
        &mut sq_member,
        "fleetObjectHandler",
        "SendBroadcast",
        &[PyRep::String("Target".into()), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("restricted broadcast must fail for SquadMember");
    assert_eq!(denied.code, "CannotBroadcastInFleet");

    // SquadCommander (90_000_003) and WingCommander (90_000_002) can send restricted broadcasts
    let mut sc_sess = member_session(90_000_003, fleet_id, 30_000_142, 100_000_003);
    handle(
        &ctx,
        &mut sc_sess,
        "fleetMgr",
        "SendBroadcast",
        &[PyRep::String("Target".into()), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .expect("SquadCommander can send Target");

    // BroadcastToBubble boundary: 250,000.0 m included, 250,001.0 m excluded
    let jita = ctx.systems.get_or_spawn(30_000_142);
    let _ = jita
        .spawn_ball(Ball {
            id: 100_000_002,
            type_id: 588,
            position: DVec3::new(250_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = jita
        .spawn_ball(Ball {
            id: 100_000_004,
            type_id: 590,
            position: DVec3::new(250_001.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_000_200)));
    let bubble_res = handle(
        &ctx,
        &mut fc_sess,
        "fleetMgr",
        "BroadcastToBubble",
        &[PyRep::String("Target".into()), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .expect("BroadcastToBubble ok");
    let recips: Vec<i64> = match bubble_res.dict_get("recipientIDs") {
        Some(PyRep::List(items)) => items.iter().filter_map(|i| i.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert!(recips.contains(&90_000_001));
    assert!(
        recips.contains(&90_000_002),
        "exact 250km boundary must be included"
    );
    assert!(
        !recips.contains(&90_000_004),
        "250,001m must be excluded from bubble"
    );

    // When sender has no ball in space, only sender receives BroadcastToBubble
    let _ = jita.remove_ball(100_000_001).await;
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(1_775_000_210)));
    let no_ball_res = handle(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "BroadcastToBubble",
        &[PyRep::String("Target".into()), PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .expect("BroadcastToBubble without sender ball");
    let no_ball_recips: Vec<i64> = match no_ball_res.dict_get("recipientIDs") {
        Some(PyRep::List(items)) => items.iter().filter_map(|i| i.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert_eq!(no_ball_recips, vec![90_000_001]);
}

#[tokio::test]
async fn warp_to_member_guards_and_abyssal_restriction() {
    let ctx = test_ctx_with_clock(1_775_000_000);
    let (fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // Missing target_char_id -> InvalidArgument
    let err = handle(&ctx, &mut fc_sess, "fleetMgr", "WarpToMember", &[], &kw)
        .await
        .unwrap()
        .expect_err("missing target");
    assert_eq!(err.code, "InvalidArgument");

    // Different solar system (90_000_005 is in Perimeter 30_000_144)
    let err = handle(
        &ctx,
        &mut fc_sess,
        "fleetMgr",
        "WarpToMember",
        &[PyRep::Integer(90_000_005)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("different solar system");
    assert_eq!(err.code, "DifferentSolarSystem");

    // TargetNotInSpace when target ball removed
    let jita = ctx.systems.get_or_spawn(30_000_142);
    let _ = jita.remove_ball(100_000_003).await;
    let err = handle(
        &ctx,
        &mut fc_sess,
        "fleetMgr",
        "WarpToMember",
        &[PyRep::Integer(90_000_003)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("target ball missing");
    assert_eq!(err.code, "TargetNotInSpace");

    // Re-spawn target ball at 400km, remove caller ball -> ShipNotInSpace
    let _ = jita
        .spawn_ball(Ball {
            id: 100_000_003,
            type_id: 589,
            position: DVec3::new(400_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = jita.remove_ball(100_000_001).await;
    let err = handle(
        &ctx,
        &mut fc_sess,
        "fleetMgr",
        "WarpToMember",
        &[PyRep::Integer(90_000_003)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("caller ball missing");
    assert_eq!(err.code, "ShipNotInSpace");

    // Spawn caller ball with max_velocity == 0.0 -> WarpScrambled
    let _ = jita
        .spawn_ball(Ball {
            id: 100_000_001,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let err = handle(
        &ctx,
        &mut fc_sess,
        "fleetMgr",
        "WarpToMember",
        &[PyRep::Integer(90_000_003)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("zero max_velocity");
    assert_eq!(err.code, "WarpScrambled");

    // Abyssal deadspace check (32_000_000..=32_999_999)
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 32_000_001;
        snap.characters.get_mut(&90_000_003).unwrap().solar_system_id = 32_000_001;
    });
    let err = handle(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "WarpToMember",
        &[PyRep::Integer(90_000_003)],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("abyssal space");
    assert_eq!(err.code, "CannotWarpInAbyssalSpace");
    let _ = fleet_id;
}

#[tokio::test]
async fn options_motd_max_size_composition_and_regroup_branches() {
    let ctx = test_ctx_with_clock(1_775_000_000);
    let (fleet_id, mut fc_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // SetOptions with integer positional args (1, 1, 1, 1)
    let opt_res = handle(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "SetOptions",
        &[
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(1),
            PyRep::Integer(1),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("SetOptions int args");
    assert_eq!(
        opt_res.dict_get("isFreeMove").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        opt_res.dict_get("isRegistered").and_then(|v| v.as_bool()),
        Some(true)
    );
    {
        let ex = ctx.fleet_extras.lock().unwrap();
        assert_eq!(ex.voice_enabled.get(&fleet_id), Some(&true));
        assert_eq!(ex.loot_logging.get(&fleet_id), Some(&true));
    }

    // SetFleetMaxSize clamping: 0 -> 1, 999 -> 256
    let min_res = handle(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "SetFleetMaxSize",
        &[PyRep::Integer(0)],
        &kw,
    )
    .await
    .unwrap()
    .expect("clamp min");
    assert_eq!(min_res.dict_get("maxSize").and_then(|v| v.as_i64()), Some(1));
    let max_res = handle(
        &ctx,
        &mut fc_sess,
        "fleetObjectHandler",
        "SetFleetMaxSize",
        &[PyRep::Integer(999)],
        &kw,
    )
    .await
    .unwrap()
    .expect("clamp max");
    assert_eq!(
        max_res.dict_get("maxSize").and_then(|v| v.as_i64()),
        Some(256)
    );

    // Non-boss denied for SetOptions, SetAutoJoinSquadID, SetFleetMaxSize, SetMotd (when WingCommander)
    let mut wc_sess = member_session(90_000_002, fleet_id, 30_000_142, 100_000_002);
    let fleet_obj_svc = "fleetObjectHandler";
    for (m, args) in [
        ("SetOptions", vec![PyRep::Bool(false)]),
        ("SetAutoJoinSquadID", vec![PyRep::Integer(720_001)]),
        ("SetFleetMaxSize", vec![PyRep::Integer(50)]),
        ("SetMotd", vec![PyRep::String("nope".into())]),
    ] {
        let err = handle(&ctx, &mut wc_sess, fleet_obj_svc, m, &args, &kw)
            .await
            .unwrap()
            .expect_err("non-boss denied");
        assert_eq!(err.code, "FleetPermissionDenied");
    }

    // GetFleetComposition: SquadMember denied with FleetRoleDenied; WingCommander allowed
    let mut sq_member = member_session(90_000_004, fleet_id, 30_000_142, 100_000_004);
    let comp_err = handle(
        &ctx,
        &mut sq_member,
        "fleetObjectHandler",
        "GetFleetComposition",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect_err("SquadMember cannot view composition");
    assert_eq!(comp_err.code, "FleetRoleDenied");

    let comp_ok = handle(
        &ctx,
        &mut wc_sess,
        "fleetObjectHandler",
        "GetFleetComposition",
        &[],
        &kw,
    )
    .await
    .unwrap()
    .expect("WingCommander can view composition");
    assert!(!matches!(comp_ok, PyRep::None));

    // GetMotd & GetFleetMaxSize clear session.fleet_id when fleet does not exist
    let mut stale_sess = member_session(90_000_004, 999_999, 30_000_142, 100_000_004);
    let _ = handle(
        &ctx,
        &mut stale_sess,
        "fleetObjectHandler",
        "GetMotd",
        &[],
        &kw,
    )
    .await;
    assert_eq!(stale_sess.fleet_id, None);

    stale_sess.fleet_id = Some(999_999);
    let _ = handle(
        &ctx,
        &mut stale_sess,
        "fleetObjectHandler",
        "GetFleetMaxSize",
        &[],
        &kw,
    )
    .await;
    assert_eq!(stale_sess.fleet_id, None);

    // Regroup by SquadMember (non-boss) denied; Regroup by FC skips >500km off-grid member
    let reg_err = handle(&ctx, &mut sq_member, "fleetMgr", "Regroup", &[], &kw)
        .await
        .unwrap()
        .expect_err("SquadMember cannot Regroup");
    assert_eq!(reg_err.code, "FleetPermissionDenied");

    // Move 90_000_003 (100_000_003) to 600,000m (> 500,000m grid limit)
    let jita = ctx.systems.get_or_spawn(30_000_142);
    let _ = jita
        .spawn_ball(Ball {
            id: 100_000_003,
            type_id: 589,
            position: DVec3::new(600_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let reg_ok = handle(&ctx, &mut fc_sess, "fleetMgr", "Regroup", &[], &kw)
        .await
        .unwrap()
        .expect("FC Regroup");
    let regrouped: Vec<i64> = match reg_ok.dict_get("regroupedMemberIDs") {
        Some(PyRep::List(v)) => v.iter().filter_map(|i| i.as_i64()).collect(),
        _ => Vec::new(),
    };
    assert!(regrouped.contains(&90_000_002));
    assert!(regrouped.contains(&90_000_004));
    assert!(
        !regrouped.contains(&90_000_003),
        "member at 600km (>500km) must not be regrouped"
    );

    // handle_fleet_warp direct call with explicit (x, y, z, minRange, formationID, spacing)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap
            .fleets
            .get_mut(&fleet_id)
            .and_then(|f| f.member_details.get_mut(&90_000_004))
        {
            m.role = FleetRole::SquadMember;
            m.takes_fleet_warp = false;
        }
    });
    let fw = handle_fleet_warp(
        &ctx,
        &fc_sess,
        &[
            PyRep::Real(5_000_000.0),
            PyRep::Real(0.0),
            PyRep::Real(0.0),
            PyRep::Real(10_000.0),
            PyRep::Integer(3),
            PyRep::Real(5_000.0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .expect("FleetWarp with explicit XYZ and Arrow formation");
    assert_eq!(fw.dict_get("formationID").and_then(|v| v.as_i64()), Some(3));
}
