//! Extra unit and mutation-killing tests for `fleet_social_fleets.rs`.

use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, seed_multi_member_fleet, test_ctx_and_clock,
};
use crate::services::fleet_social::handle;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use std::collections::HashMap;

fn make_session(char_id: i64, fleet_id: Option<i64>) -> SessionState {
    let mut s = SessionState::new(char_id as u64);
    s.user_id = Some(char_id as u64);
    s.character_id = Some(char_id);
    s.corporation_id = Some(1_000_009);
    s.solar_system_id = Some(30_000_142);
    s.fleet_id = fleet_id;
    s
}

#[tokio::test]
async fn init_ad_registration_zero_arg_create_and_wing_squad_management_guards() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let boss_id = 90_000_001;
    let kw = HashMap::new();

    // Init when not in fleet -> NotInFleet
    let mut sess = make_session(boss_id, None);
    assert_eq!(
        handle(&ctx, &mut sess, "fleetObjectHandler", "Init", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInFleet"
    );

    // Zero-arg fleetObjectHandler.CreateFleet returns integer fleet_id
    let fid_rep = handle(&ctx, &mut sess, "fleetObjectHandler", "CreateFleet", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let fid = fid_rep.as_i64().expect("integer fleet_id");
    assert_eq!(sess.fleet_id, Some(fid));

    // AlreadyInFleet when creating a second fleet
    assert_eq!(
        handle(&ctx, &mut sess, "fleetMgr", "CreateFleet", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AlreadyInFleet"
    );

    // Init with adInfoData kwarg registers fleet advert and emits OnFleetOptionsChanged
    let mut init_kw = HashMap::new();
    init_kw.insert(
        "adInfoData".into(),
        PyRep::keyval([
            ("fleetName", PyRep::String("Vanguard Fleet".into())),
            ("description", PyRep::String("Armor Doctrines".into())),
            ("inviteScope", PyRep::Integer(2)),
            ("useAdvanceOptions", PyRep::Bool(false)),
            ("minStanding", PyRep::Real(5.0)),
            ("minSecurity", PyRep::Real(-2.0)),
        ]),
    );
    let _ = handle(&ctx, &mut sess, "fleetObjectHandler", "Init", &[], &init_kw)
        .await
        .unwrap()
        .unwrap();
    ctx.db.with_snapshot(|snap| {
        assert!(snap.fleets.get(&fid).unwrap().is_registered);
    });
    assert_eq!(
        ctx.fleet_extras
            .lock()
            .unwrap()
            .adverts
            .get(&fid)
            .map(|a| a.fleet_name.as_str()),
        Some("Vanguard Fleet")
    );

    // Create 4 more wings to reach max 5 wings, then verify MaxWingsReached on 6th
    let mut created_wings = Vec::new();
    for i in 2..=5 {
        let w = handle(
            &ctx,
            &mut sess,
            "fleetObjectHandler",
            "CreateWing",
            &[PyRep::String(format!("Wing {i}"))],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        created_wings.push(w.dict_get("wingID").and_then(|v| v.as_i64()).unwrap());
    }
    assert_eq!(
        handle(&ctx, &mut sess, "fleetObjectHandler", "CreateWing", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "MaxWingsReached"
    );

    let extra_wing = created_wings[0];
    // Rename wing, then create 5 squads in extra_wing and verify MaxSquadsReached on 6th
    let renamed_w = handle(
        &ctx,
        &mut sess,
        "fleetObjectHandler",
        "ChangeWingName",
        &[PyRep::Integer(extra_wing), PyRep::String("Logistics Wing".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        renamed_w.dict_get("name").and_then(|v| v.as_str()),
        Some("Logistics Wing")
    );

    let mut created_squads = Vec::new();
    for i in 1..=5 {
        let s = handle(
            &ctx,
            &mut sess,
            "fleetObjectHandler",
            "CreateSquad",
            &[PyRep::Integer(extra_wing), PyRep::String(format!("Squad {i}"))],
            &kw,
        )
        .await
        .unwrap()
        .unwrap();
        created_squads.push(s.dict_get("squadID").and_then(|v| v.as_i64()).unwrap());
    }
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fleetObjectHandler",
            "CreateSquad",
            &[PyRep::Integer(extra_wing), PyRep::String("Overflow".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "MaxSquadsReached"
    );

    // Rename squad, delete squad, and delete empty wing
    let sq0 = created_squads[0];
    let renamed_s = handle(
        &ctx,
        &mut sess,
        "fleetObjectHandler",
        "ChangeSquadName",
        &[PyRep::Integer(sq0), PyRep::String("Logi Alpha".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        renamed_s.dict_get("name").and_then(|v| v.as_str()),
        Some("Logi Alpha")
    );

    let del_s = handle(
        &ctx,
        &mut sess,
        "fleetObjectHandler",
        "DeleteSquad",
        &[PyRep::Integer(sq0)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(del_s.dict_get("deleted").and_then(|v| v.as_bool()), Some(true));

    // Deleting wing that still contains boss -> WingNotEmpty
    let boss_wing = ctx.db.with_mut_snapshot(|s| {
        let fl = s.fleets.get_mut(&fid).unwrap();
        let bw = fl.wings[0];
        fl.member_details
            .entry(90_000_001)
            .and_modify(|m| m.wing_id = Some(bw))
            .or_insert_with(|| eve_db::FleetMemberRecord {
                character_id: 90_000_001,
                role: eve_db::FleetRole::FleetCommander,
                wing_id: Some(bw),
                squad_id: None,
                takes_fleet_warp: true,
                accepts_regroup: true,
                accepts_conduit_jumps: true,
                job: 0,
                role_name: String::new(),
                join_time_unix: 0,
            });
        bw
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sess,
            "fleetObjectHandler",
            "DeleteWing",
            &[PyRep::Integer(boss_wing)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "WingNotEmpty"
    );

    // Deleting empty extra_wing -> succeeds and cleans up remaining squads in that wing
    let del_w = handle(
        &ctx,
        &mut sess,
        "fleetObjectHandler",
        "DeleteWing",
        &[PyRep::Integer(extra_wing)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(del_w.dict_get("deleted").and_then(|v| v.as_bool()), Some(true));
}

#[tokio::test]
async fn invite_accept_reject_error_paths_and_role_occupation() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, mut boss_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    // Seed two additional characters: 90_000_006 (online, user_id = 1) and 90_000_007 (offline)
    let (c5, c6) = (90_000_006_i64, 90_000_007_i64);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch5 = clone_char_with(snap, c5, "Candidate Six", 1_000_009, 30_000_142, 100_000_006);
        ch5.user_id = 1;
        snap.characters.insert(c5, ch5);
        let mut ch6 = clone_char_with(snap, c6, "Offline Seven", 1_000_009, 30_000_142, 100_000_007);
        ch6.user_id = 2;
        snap.characters.insert(c6, ch6);
    });
    ctx.db
        .acquire_online_lock(1, 57, Some(c5), true, 1_700_000_000)
        .unwrap();

    // 1. Offline character -> CharacterOffline
    assert_eq!(
        handle(
            &ctx,
            &mut boss_sess,
            "fleetMgr",
            "Invite",
            &[PyRep::Integer(c6), PyRep::Integer(710_001), PyRep::Integer(720_001), PyRep::Integer(4)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterOffline"
    );

    // 2. Already in fleet (90_000_002) -> AlreadyInFleet
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_002).unwrap().user_id = 1;
    });
    ctx.db
        .acquire_online_lock(1, 56, Some(90_000_002), true, 1_700_000_000)
        .unwrap();
    assert_eq!(
        handle(
            &ctx,
            &mut boss_sess,
            "fleetMgr",
            "Invite",
            &[PyRep::Integer(90_000_002), PyRep::Integer(710_001), PyRep::Integer(720_001), PyRep::Integer(4)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "AlreadyInFleet"
    );
    ctx.db
        .acquire_online_lock(1, 58, Some(c5), true, 1_700_000_000)
        .unwrap();

    // 3. PositionOccupied when inviting as FleetCommander (1) or WingCommander (2) of Wing 710_001
    for occupied_role in [1_i64, 2_i64] {
        assert_eq!(
            handle(
                &ctx,
                &mut boss_sess,
                "fleetMgr",
                "Invite",
                &[PyRep::Integer(c5), PyRep::Integer(710_001), PyRep::Integer(720_001), PyRep::Integer(occupied_role)],
                &kw,
            )
            .await
            .unwrap()
            .unwrap_err()
            .code,
            "PositionOccupied"
        );
    }

    // 4. FleetFull when max_size == current member count (5)
    ctx.fleet_extras.lock().unwrap().max_sizes.insert(fleet_id, 5);
    assert_eq!(
        handle(
            &ctx,
            &mut boss_sess,
            "fleetMgr",
            "Invite",
            &[PyRep::Integer(c5), PyRep::Integer(710_001), PyRep::Integer(720_001), PyRep::Integer(4)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetFull"
    );
    ctx.fleet_extras.lock().unwrap().max_sizes.insert(fleet_id, 256);

    // 5. Invite c5, verify InviteAlreadyPending on second invite, then RejectInvite(true)
    let _ = handle(
        &ctx,
        &mut boss_sess,
        "fleetMgr",
        "Invite",
        &[PyRep::Integer(c5), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        handle(
            &ctx,
            &mut boss_sess,
            "fleetMgr",
            "Invite",
            &[PyRep::Integer(c5), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InviteAlreadyPending"
    );

    let mut c5_sess = make_session(c5, None);
    let rej = handle(
        &ctx,
        &mut c5_sess,
        "fleetObjectHandler",
        "RejectInvite",
        &[PyRep::Bool(true)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rej.dict_get("rejected").and_then(|v| v.as_bool()), Some(true));

    // Accepting after reject -> InviteExpired
    assert_eq!(
        handle(
            &ctx,
            &mut c5_sess,
            "fleetObjectHandler",
            "AcceptInvite",
            &[PyRep::Integer(fleet_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InviteExpired"
    );
}

#[tokio::test]
async fn kick_member_hierarchy_free_move_and_disband_fleet() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let (fleet_id, mut boss_sess) = seed_multi_member_fleet(&ctx).await;
    let kw = HashMap::new();

    let mut wc_sess = make_session(90_000_002, Some(fleet_id));
    let mut sc_sess = make_session(90_000_003, Some(fleet_id));
    let mut sm_sess = make_session(90_000_004, Some(fleet_id));

    // 1. SquadMember (90_000_004) cannot kick anyone else -> FleetPermissionDenied
    assert_eq!(
        handle(
            &ctx,
            &mut sm_sess,
            "fleetMgr",
            "KickMember",
            &[PyRep::Integer(90_000_003)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );

    // 2. WingCommander (90_000_002) cannot kick Fleet Boss (90_000_001) -> CannotKickBoss
    assert_eq!(
        handle(
            &ctx,
            &mut wc_sess,
            "fleetMgr",
            "KickMember",
            &[PyRep::Integer(90_000_001)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotKickBoss"
    );

    // 3. SquadCommander (90_000_003) cannot kick WingCommander (90_000_002) -> FleetPermissionDenied
    assert_eq!(
        handle(
            &ctx,
            &mut sc_sess,
            "fleetMgr",
            "KickMember",
            &[PyRep::Integer(90_000_002)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );

    // 4. MoveMember: when free_move == false, SquadMember self-move fails with FleetPermissionDenied
    assert_eq!(
        handle(
            &ctx,
            &mut sm_sess,
            "fleetMgr",
            "MoveMember",
            &[
                PyRep::Integer(90_000_004),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(4),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );

    // Enable free_move: SquadMember can now self-move to SquadMember (role 4), but NOT to SquadCommander (role 3)
    ctx.db.with_mut_snapshot(|snap| {
        snap.fleets.get_mut(&fleet_id).unwrap().free_move = true;
    });
    assert_eq!(
        handle(
            &ctx,
            &mut sm_sess,
            "fleetMgr",
            "MoveMember",
            &[
                PyRep::Integer(90_000_004),
                PyRep::Integer(1),
                PyRep::Integer(1),
                PyRep::Integer(3),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );
    let self_moved = handle(
        &ctx,
        &mut sm_sess,
        "fleetMgr",
        "MoveMember",
        &[
            PyRep::Integer(90_000_004),
            PyRep::Integer(710_001),
            PyRep::Integer(720_001),
            PyRep::Integer(4),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(self_moved.dict_get("role").and_then(|v| v.as_i64()), Some(4));

    // 5. SquadCommander kicks SquadMember (90_000_004) in same squad -> succeeds
    let kicked = handle(
        &ctx,
        &mut sc_sess,
        "fleetMgr",
        "KickMember",
        &[PyRep::Integer(90_000_004)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        kicked.dict_get("kickedCharacterID").and_then(|v| v.as_i64()),
        Some(90_000_004)
    );

    // 6. DisbandFleet: non-boss (WingCommander) denied -> FleetPermissionDenied; Boss succeeds!
    assert_eq!(
        handle(&ctx, &mut wc_sess, "fleetObjectHandler", "DisbandFleet", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetPermissionDenied"
    );
    let disbanded = handle(&ctx, &mut boss_sess, "fleetObjectHandler", "DisbandFleet", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(disbanded.dict_get("disbanded").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(boss_sess.fleet_id, None);
}

#[tokio::test]
async fn coverage_sweep_fleets_error_guards_hierarchy_invite_accept_and_move() {
    use eve_db::ServerClock;
    let (ctx, clock) = test_ctx_and_clock(1_700_000_000);
    let kw = HashMap::new();
    let boss_id = 90_000_001_i64;
    let wc_id = 90_000_002_i64;
    let sc_id = 90_000_003_i64;
    let sm_id = 90_000_004_i64;
    let sm2_id = 90_000_005_i64;
    let outsider_id = 90_000_006_i64;
    ctx.db.with_mut_snapshot(|snap| {
        for cid in [wc_id, sc_id, sm_id, sm2_id, outsider_id] {
            let mut rec = clone_char_with(snap, cid, &format!("Pilot{cid}"), 1_000_009, 30_000_142, 700_000 + cid);
            rec.user_id = 1;
            snap.characters.insert(cid, rec);
        }
    });

    // 1. NotInFleet guards across all fleet methods when session.fleet_id == None
    let mut no_fleet_sess = make_session(boss_id, None);
    for (svc, method, args) in [
        ("fleetObjectHandler", "CreateWing", vec![]),
        ("fleetObjectHandler", "DeleteWing", vec![PyRep::Integer(1)]),
        ("fleetObjectHandler", "ChangeWingName", vec![PyRep::Integer(1), PyRep::String("W".into())]),
        ("fleetObjectHandler", "CreateSquad", vec![PyRep::Integer(1)]),
        ("fleetObjectHandler", "DeleteSquad", vec![PyRep::Integer(1)]),
        ("fleetObjectHandler", "ChangeSquadName", vec![PyRep::Integer(1), PyRep::String("S".into())]),
        ("fleetObjectHandler", "Invite", vec![PyRep::Integer(wc_id)]),
        ("fleetObjectHandler", "KickMember", vec![PyRep::Integer(wc_id)]),
        ("fleetObjectHandler", "LeaveFleet", vec![]),
        ("fleetObjectHandler", "DisbandFleet", vec![]),
        ("fleetObjectHandler", "FinishMove", vec![]),
        ("fleetObjectHandler", "MoveMember", vec![PyRep::Integer(wc_id), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)]),
    ] {
        assert_eq!(
            handle(&ctx, &mut no_fleet_sess, svc, method, &args, &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            "NotInFleet",
            "{svc}.{method}"
        );
    }

    // 2. Seed multi-member fleet (boss FC + WC + SC + SM + SM2)
    let mut boss_sess = make_session(boss_id, None);
    let fid = handle(&ctx, &mut boss_sess, "fleetObjectHandler", "CreateFleet", &[], &kw)
        .await
        .unwrap()
        .unwrap()
        .as_i64()
        .unwrap();
    let w1 = ctx.db.with_snapshot(|s| s.fleets[&fid].wings[0]);
    let s1 = ctx.db.with_snapshot(|s| s.fleets[&fid].squads[0]);

    // InvalidArgument & NotFound errors on wing/squad/invite/kick
    assert_eq!(handle(&ctx, &mut boss_sess, "fleetObjectHandler", "DeleteWing", &[], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(handle(&ctx, &mut boss_sess, "fleetObjectHandler", "DeleteWing", &[PyRep::Integer(999_999)], &kw).await.unwrap().unwrap_err().code, "WingNotFound");
    assert_eq!(handle(&ctx, &mut boss_sess, "fleetObjectHandler", "ChangeWingName", &[], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(handle(&ctx, &mut boss_sess, "fleetObjectHandler", "ChangeWingName", &[PyRep::Integer(999_999), PyRep::String("W".into())], &kw).await.unwrap().unwrap_err().code, "WingNotFound");
    assert_eq!(handle(&ctx, &mut boss_sess, "fleetObjectHandler", "CreateSquad", &[PyRep::Integer(999_999)], &kw).await.unwrap().unwrap_err().code, "WingNotFound");
    assert_eq!(handle(&ctx, &mut boss_sess, "fleetObjectHandler", "DeleteSquad", &[], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(handle(&ctx, &mut boss_sess, "fleetObjectHandler", "DeleteSquad", &[PyRep::Integer(999_999)], &kw).await.unwrap().unwrap_err().code, "SquadNotFound");
    assert_eq!(handle(&ctx, &mut boss_sess, "fleetObjectHandler", "ChangeSquadName", &[], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(handle(&ctx, &mut boss_sess, "fleetObjectHandler", "ChangeSquadName", &[PyRep::Integer(999_999), PyRep::String("S".into())], &kw).await.unwrap().unwrap_err().code, "SquadNotFound");
    assert_eq!(handle(&ctx, &mut boss_sess, "fleetObjectHandler", "Invite", &[], &kw).await.unwrap().unwrap_err().code, "InvalidArgument");
    assert_eq!(handle(&ctx, &mut boss_sess, "fleetObjectHandler", "KickMember", &[PyRep::Integer(999_999)], &kw).await.unwrap().unwrap_err().code, "MemberNotFound");

    // Create second wing + squad for hierarchy tests
    handle(&ctx, &mut boss_sess, "fleetObjectHandler", "CreateWing", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let w2 = ctx.db.with_snapshot(|s| *s.fleets[&fid].wings.last().unwrap());
    handle(&ctx, &mut boss_sess, "fleetObjectHandler", "CreateSquad", &[PyRep::Integer(w2)], &kw)
        .await
        .unwrap()
        .unwrap();
    let s2 = ctx.db.with_snapshot(|s| *s.fleets[&fid].squads.last().unwrap());

    // Invite with default wing/squad (None) and specific roles (WC=2 in w2, SC=3 in s2, SM=4), then AcceptInvite
    ctx.db.acquire_online_lock(1, 501, Some(wc_id), true, 1_700_000_000).unwrap();
    handle(&ctx, &mut boss_sess, "fleetObjectHandler", "Invite", &[PyRep::Integer(wc_id), PyRep::Integer(w2), PyRep::None, PyRep::Integer(2)], &kw).await.unwrap().unwrap();
    ctx.db.acquire_online_lock(1, 502, Some(sc_id), true, 1_700_000_000).unwrap();
    handle(&ctx, &mut boss_sess, "fleetObjectHandler", "Invite", &[PyRep::Integer(sc_id), PyRep::Integer(w2), PyRep::Integer(s2), PyRep::Integer(3)], &kw).await.unwrap().unwrap();
    ctx.db.acquire_online_lock(1, 503, Some(sm_id), true, 1_700_000_000).unwrap();
    handle(&ctx, &mut boss_sess, "fleetObjectHandler", "Invite", &[PyRep::Integer(sm_id)], &kw).await.unwrap().unwrap();
    ctx.db.acquire_online_lock(1, 504, Some(sm2_id), true, 1_700_000_000).unwrap();
    handle(&ctx, &mut boss_sess, "fleetObjectHandler", "Invite", &[PyRep::Integer(sm2_id), PyRep::Integer(w1), PyRep::Integer(s1), PyRep::Integer(4)], &kw).await.unwrap().unwrap();

    let mut wc_sess = make_session(wc_id, None);
    let mut sc_sess = make_session(sc_id, None);
    let mut sm_sess = make_session(sm_id, None);
    let mut sm2_sess = make_session(sm2_id, None);
    let mut outsider_sess = make_session(outsider_id, None);

    // RejectInvite with false & AcceptInvite expired
    handle(&ctx, &mut outsider_sess, "fleetObjectHandler", "RejectInvite", &[PyRep::Integer(fid), PyRep::Integer(0)], &kw).await.unwrap().unwrap();
    assert_eq!(
        handle(&ctx, &mut outsider_sess, "fleetMgr", "AcceptInvite", &[PyRep::Integer(fid)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InviteExpired"
    );

    handle(&ctx, &mut wc_sess, "fleetMgr", "AcceptInvite", &[PyRep::Integer(fid)], &kw).await.unwrap().unwrap();
    handle(&ctx, &mut sc_sess, "fleetMgr", "AcceptInvite", &[PyRep::Integer(fid)], &kw).await.unwrap().unwrap();
    handle(&ctx, &mut sm_sess, "fleetMgr", "AcceptInvite", &[PyRep::Integer(fid)], &kw).await.unwrap().unwrap();
    handle(&ctx, &mut sm2_sess, "fleetMgr", "AcceptInvite", &[PyRep::Integer(fid)], &kw).await.unwrap().unwrap();

    // AcceptInvite when already in fleet (L606)
    assert_eq!(
        handle(&ctx, &mut wc_sess, "fleetMgr", "AcceptInvite", &[PyRep::Integer(fid)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "AlreadyInFleet"
    );

    // DeleteSquad with members -> SquadNotEmpty (L405)
    assert_eq!(
        handle(&ctx, &mut boss_sess, "fleetObjectHandler", "DeleteSquad", &[PyRep::Integer(s1)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "SquadNotEmpty"
    );

    // Permission checks for SM / SC / WC on CreateWing, DeleteWing, ChangeWingName, CreateSquad, DeleteSquad, ChangeSquadName, Invite, KickMember, MoveMember
    assert_eq!(handle(&ctx, &mut sm_sess, "fleetObjectHandler", "CreateWing", &[], &kw).await.unwrap().unwrap_err().code, "FleetPermissionDenied");
    assert_eq!(handle(&ctx, &mut sm_sess, "fleetObjectHandler", "DeleteWing", &[PyRep::Integer(w2)], &kw).await.unwrap().unwrap_err().code, "FleetPermissionDenied");
    assert_eq!(handle(&ctx, &mut sm_sess, "fleetObjectHandler", "ChangeWingName", &[PyRep::Integer(w1), PyRep::String("X".into())], &kw).await.unwrap().unwrap_err().code, "FleetPermissionDenied");
    assert_eq!(handle(&ctx, &mut sc_sess, "fleetObjectHandler", "CreateSquad", &[PyRep::Integer(w1)], &kw).await.unwrap().unwrap_err().code, "FleetPermissionDenied");
    assert_eq!(handle(&ctx, &mut sc_sess, "fleetObjectHandler", "DeleteSquad", &[PyRep::Integer(s2)], &kw).await.unwrap().unwrap_err().code, "SquadNotEmpty");
    assert_eq!(handle(&ctx, &mut sm_sess, "fleetObjectHandler", "ChangeSquadName", &[PyRep::Integer(s1), PyRep::String("X".into())], &kw).await.unwrap().unwrap_err().code, "FleetPermissionDenied");
    assert_eq!(handle(&ctx, &mut sm_sess, "fleetObjectHandler", "Invite", &[PyRep::Integer(outsider_id)], &kw).await.unwrap().unwrap_err().code, "FleetPermissionDenied");
    assert_eq!(handle(&ctx, &mut sm_sess, "fleetObjectHandler", "KickMember", &[PyRep::Integer(sm2_id)], &kw).await.unwrap().unwrap_err().code, "FleetPermissionDenied");
    assert_eq!(handle(&ctx, &mut sm_sess, "fleetObjectHandler", "MoveMember", &[PyRep::Integer(sm2_id), PyRep::Integer(w1), PyRep::Integer(s1), PyRep::Integer(4)], &kw).await.unwrap().unwrap_err().code, "FleetPermissionDenied");

    // PositionOccupied checks on MoveMember (FC=1, WC=2, SC=3)
    assert_eq!(
        handle(&ctx, &mut boss_sess, "fleetObjectHandler", "MoveMember", &[PyRep::Integer(sm_id), PyRep::Integer(w1), PyRep::Integer(s1), PyRep::Integer(1)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "PositionOccupied"
    );
    assert_eq!(
        handle(&ctx, &mut boss_sess, "fleetObjectHandler", "MoveMember", &[PyRep::Integer(sm_id), PyRep::Integer(w2), PyRep::Integer(s2), PyRep::Integer(2)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "PositionOccupied"
    );
    assert_eq!(
        handle(&ctx, &mut boss_sess, "fleetObjectHandler", "MoveMember", &[PyRep::Integer(sm_id), PyRep::Integer(w2), PyRep::Integer(s2), PyRep::Integer(3)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "PositionOccupied"
    );

    // Move WC to w1 as WC -> clears w2 commander_id & sets w1 commander_id (L1196, L1208); then FinishMove 0-arg (L779)
    clock.advance_secs(20);
    handle(&ctx, &mut boss_sess, "fleetObjectHandler", "MoveMember", &[PyRep::Integer(wc_id), PyRep::Integer(w1), PyRep::Integer(s1), PyRep::Integer(2)], &kw)
        .await
        .unwrap()
        .unwrap();
    // SessionChangeTimerActive right after move on self-move (target_id == char_id -> L1177)
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&fid).unwrap().free_move = true;
    });
    ctx.session_change_timers.lock().unwrap().insert(
        wc_id,
        crate::services::space_destiny::SessionChangeTimerState {
            expires_unix: clock.now_unix_secs() + 60,
            system_id: 30_000_142,
            started_tick: 0,
            reason: "fleet".into(),
            location_change_invul_until: 0,
        },
    );
    assert_eq!(
        handle(&ctx, &mut wc_sess, "fleetObjectHandler", "MoveMember", &[PyRep::Integer(wc_id), PyRep::Integer(w2), PyRep::Integer(s2), PyRep::Integer(4)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "SessionChangeTimerActive"
    );
    assert_eq!(handle(&ctx, &mut boss_sess, "fleetObjectHandler", "FinishMove", &[], &kw).await.unwrap().unwrap(), PyRep::None);

    // Kick SC by boss (FC hierarchy L867)
    handle(&ctx, &mut boss_sess, "fleetObjectHandler", "KickMember", &[PyRep::Integer(sc_id)], &kw).await.unwrap().unwrap();

    // Session stale fleet_id -> FleetNotFound across methods
    let mut stale_sess = make_session(outsider_id, Some(999_888_777));
    for (svc, method, args) in [
        ("fleetObjectHandler", "CreateWing", vec![]),
        ("fleetObjectHandler", "DeleteWing", vec![PyRep::Integer(1)]),
        ("fleetObjectHandler", "ChangeWingName", vec![PyRep::Integer(1), PyRep::String("W".into())]),
        ("fleetObjectHandler", "CreateSquad", vec![PyRep::Integer(1)]),
        ("fleetObjectHandler", "DeleteSquad", vec![PyRep::Integer(1)]),
        ("fleetObjectHandler", "ChangeSquadName", vec![PyRep::Integer(1), PyRep::String("S".into())]),
        ("fleetObjectHandler", "Invite", vec![PyRep::Integer(outsider_id)]),
        ("fleetObjectHandler", "KickMember", vec![PyRep::Integer(outsider_id)]),
        ("fleetObjectHandler", "DisbandFleet", vec![]),
        ("fleetObjectHandler", "MoveMember", vec![PyRep::Integer(outsider_id), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)]),
    ] {
        stale_sess.fleet_id = Some(999_888_777);
        assert_eq!(
            handle(&ctx, &mut stale_sess, svc, method, &args, &kw)
                .await
                .unwrap()
                .unwrap_err()
                .code,
            "FleetNotFound",
            "{svc}.{method}"
        );
    }

    // SessionChangeTimerActive & FleetFull on AcceptInvite (L622, L645)
    ctx.db.acquire_online_lock(1, 505, Some(outsider_id), true, 1_700_000_000).unwrap();
    handle(&ctx, &mut boss_sess, "fleetObjectHandler", "Invite", &[PyRep::Integer(outsider_id)], &kw).await.unwrap().unwrap();
    ctx.fleet_extras.lock().unwrap().max_sizes.insert(fid, 2);
    assert_eq!(
        handle(&ctx, &mut outsider_sess, "fleetMgr", "AcceptInvite", &[PyRep::Integer(fid)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetFull"
    );
    ctx.fleet_extras.lock().unwrap().max_sizes.insert(fid, 256);
    // Record recent session change for outsider to hit SessionChangeTimerActive on AcceptInvite (L622)
    ctx.session_change_timers.lock().unwrap().insert(
        outsider_id,
        crate::services::space_destiny::SessionChangeTimerState {
            expires_unix: clock.now_unix_secs() + 60,
            system_id: 30_000_142,
            started_tick: 0,
            reason: "fleet".into(),
            location_change_invul_until: 0,
        },
    );
    assert_eq!(
        handle(&ctx, &mut outsider_sess, "fleetMgr", "AcceptInvite", &[PyRep::Integer(fid)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "SessionChangeTimerActive"
    );

    // Active RejectInvite(true) cleans invite & emits OnFleetJoinReject (L770-797)
    handle(&ctx, &mut outsider_sess, "fleetObjectHandler", "RejectInvite", &[PyRep::Bool(true)], &kw)
        .await
        .unwrap()
        .unwrap();

    // Non-member with spoofed session.fleet_id -> NotInFleet on caller_m checks (L252, L287, L328, L360, L398, L444, L501, L859, L962)
    let mut spoof_sess = make_session(outsider_id, Some(fid));
    for (svc, method, args) in [
        ("fleetObjectHandler", "CreateWing", vec![]),
        ("fleetObjectHandler", "DeleteWing", vec![PyRep::Integer(w1)]),
        ("fleetObjectHandler", "ChangeWingName", vec![PyRep::Integer(w1), PyRep::String("X".into())]),
        ("fleetObjectHandler", "CreateSquad", vec![PyRep::Integer(w1)]),
        ("fleetObjectHandler", "DeleteSquad", vec![PyRep::Integer(s1)]),
        ("fleetObjectHandler", "ChangeSquadName", vec![PyRep::Integer(s1), PyRep::String("X".into())]),
        ("fleetObjectHandler", "Invite", vec![PyRep::Integer(outsider_id)]),
        ("fleetObjectHandler", "KickMember", vec![PyRep::Integer(sm_id)]),
        ("fleetObjectHandler", "DisbandFleet", vec![]),
        ("fleetObjectHandler", "MoveMember", vec![PyRep::Integer(sm_id), PyRep::Integer(w1), PyRep::Integer(s1), PyRep::Integer(4)]),
    ] {
        assert_eq!(handle(&ctx, &mut spoof_sess, svc, method, &args, &kw).await.unwrap().unwrap_err().code, "NotInFleet");
    }

    // SquadMember lacks permission on commander/boss actions -> FleetPermissionDenied (L255, L290, L331, L363, L401, L447, L506, L864, L965, L1097)
    ctx.db.with_mut_snapshot(|s| {
        s.fleets.get_mut(&fid).unwrap().free_move = false;
    });
    for (svc, method, args) in [
        ("fleetObjectHandler", "CreateWing", vec![]),
        ("fleetObjectHandler", "DeleteWing", vec![PyRep::Integer(w1)]),
        ("fleetObjectHandler", "ChangeWingName", vec![PyRep::Integer(w1), PyRep::String("X".into())]),
        ("fleetObjectHandler", "CreateSquad", vec![PyRep::Integer(w1)]),
        ("fleetObjectHandler", "DeleteSquad", vec![PyRep::Integer(s1)]),
        ("fleetObjectHandler", "ChangeSquadName", vec![PyRep::Integer(s1), PyRep::String("X".into())]),
        ("fleetObjectHandler", "Invite", vec![PyRep::Integer(outsider_id)]),
        ("fleetObjectHandler", "KickMember", vec![PyRep::Integer(sm2_id)]),
        ("fleetObjectHandler", "DisbandFleet", vec![]),
        ("fleetObjectHandler", "MoveMember", vec![PyRep::Integer(sm2_id), PyRep::Integer(w1), PyRep::Integer(s1), PyRep::Integer(4)]),
    ] {
        assert_eq!(handle(&ctx, &mut sm_sess, svc, method, &args, &kw).await.unwrap().unwrap_err().code, "FleetPermissionDenied");
    }

    // MoveMember MemberNotFound (L1104) & WC trying to move Boss -> FleetPermissionDenied (L1128)
    assert_eq!(
        handle(&ctx, &mut boss_sess, "fleetObjectHandler", "MoveMember", &[PyRep::Integer(outsider_id), PyRep::Integer(w1), PyRep::Integer(s1), PyRep::Integer(4)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "MemberNotFound"
    );
    assert_eq!(
        handle(&ctx, &mut wc_sess, "fleetObjectHandler", "MoveMember", &[PyRep::Integer(boss_id), PyRep::Integer(w1), PyRep::Integer(s1), PyRep::Integer(4)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetPermissionDenied"
    );

    // Fallback to handle_fleet_ops (L1255)
    assert!(handle(&ctx, &mut boss_sess, "fleetObjectHandler", "GetFleetComposition", &[], &kw).await.is_some());
}
