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
    let boss_wing = ctx
        .db
        .with_snapshot(|s| s.fleets.get(&fid).unwrap().wings[0]);
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
