//! Additional edge-case and branch coverage tests for `services::fleet_social_fleets`.

use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_and_clock,
};
use eve_db::{AccountRecord, FleetRole, ServerClock};

fn make_session(char_id: i64) -> SessionState {
    SessionState {
        user_id: Some(char_id as u64),
        character_id: Some(char_id),
        corporation_id: Some(1_000_009),
        solar_system_id: Some(30_000_142),
        ..Default::default()
    }
}

#[tokio::test]
async fn fleet_wings_squads_limits_and_rename_permissions_edges() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let boss_id = 90_000_001_i64;
    let wc_id = 90_000_002_i64;
    let sc_id = 90_000_003_i64;

    let (u2, u3) = ctx.db.with_mut_snapshot(|snap| {
        let base_uid = snap.characters.get(&boss_id).unwrap().user_id;
        let u2 = base_uid + 1;
        let u3 = base_uid + 2;
        for uid in [u2, u3] {
            let username = format!("fleet_u_{uid}");
            snap.accounts.insert(
                username.clone(),
                AccountRecord {
                    user_id: uid,
                    username,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
        }
        let mut c2 = clone_char_with(snap, wc_id, "Wing Cmdr", 1_000_009, 30_000_142, 100_000_002);
        c2.user_id = u2;
        let mut c3 = clone_char_with(snap, sc_id, "Squad Cmdr", 1_000_009, 30_000_142, 100_000_003);
        c3.user_id = u3;
        snap.characters.insert(wc_id, c2);
        snap.characters.insert(sc_id, c3);
        (u2, u3)
    });
    ctx.db
        .acquire_online_lock(u2, 2, Some(wc_id), false, 1_700_000_000)
        .unwrap();
    ctx.db
        .acquire_online_lock(u3, 3, Some(sc_id), false, 1_700_000_000)
        .unwrap();

    let mut boss_sess = make_session(boss_id);
    let mut wc_sess = make_session(wc_id);
    let mut sc_sess = make_session(sc_id);
    let kw = HashMap::new();
    let svc = "fleetObjectHandler";

    // CreateWing outside fleet -> NotInFleet
    assert_eq!(
        handle_fleets(&ctx, &mut boss_sess, svc, "CreateWing", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "NotInFleet"
    );

    let created = handle_fleets(
        &ctx,
        &mut boss_sess,
        "fleetMgr",
        "CreateFleet",
        &[PyRep::String("Ops".into()), PyRep::String("MOTD".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let fleet_id = created.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();

    // Init clears DB fallback and ignores non-boss adInfoData
    let mut boss_sess_no_fleet = make_session(boss_id);
    boss_sess_no_fleet.fleet_id = None;
    assert_eq!(
        handle_fleets(&ctx, &mut boss_sess_no_fleet, svc, "Init", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::None
    );

    // Get initial wing & squad IDs
    let (wing_id, squad_id) = ctx.db.with_snapshot(|snap| {
        let f = snap.fleets.get(&fleet_id).unwrap();
        let wid = f.wings[0];
        let sid = f.squads[0];
        (wid, sid)
    });

    // Invite wc_id as WingCommander into wing_id, sc_id as SquadCommander into squad_id
    let _ = handle_fleets(
        &ctx,
        &mut boss_sess,
        svc,
        "Invite",
        &[
            PyRep::Integer(wc_id),
            PyRep::Integer(wing_id),
            PyRep::Integer(-1),
            PyRep::Integer(role_to_i64(FleetRole::WingCommander)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let _ = handle_fleets(&ctx, &mut wc_sess, svc, "AcceptInvite", &[PyRep::Integer(fleet_id)], &kw)
        .await
        .unwrap()
        .unwrap();

    let _ = handle_fleets(
        &ctx,
        &mut boss_sess,
        svc,
        "Invite",
        &[
            PyRep::Integer(sc_id),
            PyRep::Integer(wing_id),
            PyRep::Integer(squad_id),
            PyRep::Integer(role_to_i64(FleetRole::SquadCommander)),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let _ = handle_fleets(&ctx, &mut sc_sess, svc, "AcceptInvite", &[PyRep::Integer(fleet_id)], &kw)
        .await
        .unwrap()
        .unwrap();

    // WC can rename own wing, SC cannot rename wing
    assert_eq!(
        handle_fleets(
            &ctx,
            &mut wc_sess,
            svc,
            "ChangeWingName",
            &[PyRep::Integer(wing_id), PyRep::String("Alpha".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::keyval([("wingID", PyRep::Integer(wing_id)), ("name", PyRep::String("Alpha".into()))])
    );
    assert_eq!(
        handle_fleets(
            &ctx,
            &mut sc_sess,
            svc,
            "ChangeWingName",
            &[PyRep::Integer(wing_id), PyRep::String("Nope".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );

    // SC can rename own squad; WingNotFound & SquadNotFound & InvalidArgument
    let sq_rename = handle_fleets(
        &ctx,
        &mut sc_sess,
        svc,
        "ChangeSquadName",
        &[PyRep::Integer(squad_id), PyRep::String("Alpha-1".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(sq_rename.dict_get("squadID").and_then(|v| v.as_i64()), Some(squad_id));
    assert_eq!(sq_rename.dict_get("name").and_then(|v| v.as_str()), Some("Alpha-1"));
    assert_eq!(
        handle_fleets(
            &ctx,
            &mut boss_sess,
            svc,
            "ChangeWingName",
            &[PyRep::Integer(999_999), PyRep::String("X".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "WingNotFound"
    );
    assert_eq!(
        handle_fleets(
            &ctx,
            &mut boss_sess,
            svc,
            "ChangeSquadName",
            &[PyRep::Integer(999_999), PyRep::String("X".into())],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "SquadNotFound"
    );
    assert_eq!(
        handle_fleets(&ctx, &mut boss_sess, svc, "DeleteSquad", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );

    // DeleteWing fails with WingNotEmpty while wc/sc are inside wing
    assert_eq!(
        handle_fleets(
            &ctx,
            &mut boss_sess,
            svc,
            "DeleteWing",
            &[PyRep::Integer(wing_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "WingNotEmpty"
    );
    assert_eq!(
        handle_fleets(
            &ctx,
            &mut boss_sess,
            svc,
            "DeleteSquad",
            &[PyRep::Integer(squad_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "SquadNotEmpty"
    );

    // Create empty wing -> create empty squad -> DeleteWing cleans child squads
    let wing2 = handle_fleets(
        &ctx,
        &mut boss_sess,
        svc,
        "CreateWing",
        &[PyRep::String("Empty Wing".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap()
    .dict_get("wingID")
    .and_then(|v| v.as_i64())
    .unwrap();
    let squad2 = handle_fleets(
        &ctx,
        &mut boss_sess,
        svc,
        "CreateSquad",
        &[PyRep::Integer(wing2), PyRep::String("Empty Sq".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap()
    .dict_get("squadID")
    .and_then(|v| v.as_i64())
    .unwrap();
    assert_eq!(
        handle_fleets(&ctx, &mut boss_sess, svc, "DeleteWing", &[PyRep::Integer(wing2)], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::keyval(vec![
            ("wingID", PyRep::Integer(wing2)),
            ("deleted", PyRep::Bool(true)),
        ])
    );
    ctx.db.with_snapshot(|snap| {
        let f = snap.fleets.get(&fleet_id).unwrap();
        assert!(!f.wings.contains(&wing2));
        assert!(!f.squads.contains(&squad2));
    });

    // DisbandFleet non-boss -> FleetPermissionDenied, boss -> succeeds & cleans fleet
    assert_eq!(
        handle_fleets(&ctx, &mut wc_sess, svc, "DisbandFleet", &[], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "FleetPermissionDenied"
    );
    assert_eq!(
        handle_fleets(&ctx, &mut boss_sess, svc, "DisbandFleet", &[], &kw)
            .await
            .unwrap()
            .unwrap(),
        PyRep::keyval(vec![
            ("fleetID", PyRep::Integer(fleet_id)),
            ("disbanded", PyRep::Bool(true)),
        ])
    );
    assert!(boss_sess.fleet_id.is_none());
}

#[tokio::test]
async fn fleet_invite_reject_kick_leave_and_move_member_edges() {
    let (ctx, clock) = test_ctx_and_clock(1_700_000_000);
    let boss_id = 90_000_001_i64;
    let m1_id = 90_000_002_i64;
    let m2_id = 90_000_003_i64;
    let offline_id = 90_000_004_i64;

    let (u2, u3) = ctx.db.with_mut_snapshot(|snap| {
        let base_uid = snap.characters.get(&boss_id).unwrap().user_id;
        for (idx, (cid, name)) in [
            (m1_id, "Fleet M1"),
            (m2_id, "Fleet M2"),
            (offline_id, "Offline M"),
        ]
        .into_iter()
        .enumerate()
        {
            let uid = base_uid + 10 + idx as u64;
            let username = format!("fleet_m_u_{uid}");
            snap.accounts.insert(
                username.clone(),
                AccountRecord {
                    user_id: uid,
                    username,
                    password_token: String::new(),
                    role: 0,
                    banned_until_unix: None,
                    ban_reason: None,
                },
            );
            let mut c = clone_char_with(snap, cid, name, 1_000_009, 30_000_142, 100_000_010 + idx as i64);
            c.user_id = uid;
            snap.characters.insert(cid, c);
        }
        (base_uid + 10, base_uid + 11)
    });
    ctx.db
        .acquire_online_lock(u2, 10, Some(m1_id), false, 1_700_000_000)
        .unwrap();
    ctx.db
        .acquire_online_lock(u3, 11, Some(m2_id), false, 1_700_000_000)
        .unwrap();

    let mut boss_sess = make_session(boss_id);
    let mut m1_sess = make_session(m1_id);
    let mut m2_sess = make_session(m2_id);
    let kw = HashMap::new();
    let svc = "fleetObjectHandler";

    let created = handle_fleets(&ctx, &mut boss_sess, "fleetMgr", "CreateFleet", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    let fleet_id = created.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
    let (wing_id, squad_id) = ctx.db.with_snapshot(|snap| {
        let f = snap.fleets.get(&fleet_id).unwrap();
        (f.wings[0], f.squads[0])
    });

    // Offline character invite -> CharacterOffline
    assert_eq!(
        handle_fleets(
            &ctx,
            &mut boss_sess,
            svc,
            "Invite",
            &[
                PyRep::Integer(offline_id),
                PyRep::Integer(wing_id),
                PyRep::Integer(squad_id),
                PyRep::Integer(4),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterOffline"
    );

    // Occupied FC position (boss is already FC) -> PositionOccupied
    assert_eq!(
        handle_fleets(
            &ctx,
            &mut boss_sess,
            svc,
            "Invite",
            &[
                PyRep::Integer(m1_id),
                PyRep::Integer(-1),
                PyRep::Integer(-1),
                PyRep::Integer(1),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "PositionOccupied"
    );

    // Invite m1, then duplicate invite -> InviteAlreadyPending
    let _ = handle_fleets(
        &ctx,
        &mut boss_sess,
        svc,
        "Invite",
        &[
            PyRep::Integer(m1_id),
            PyRep::Integer(wing_id),
            PyRep::Integer(squad_id),
            PyRep::Integer(3), // SC
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        handle_fleets(
            &ctx,
            &mut boss_sess,
            svc,
            "Invite",
            &[
                PyRep::Integer(m1_id),
                PyRep::Integer(wing_id),
                PyRep::Integer(squad_id),
                PyRep::Integer(4),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "InviteAlreadyPending"
    );

    // RejectInvite(false) & Invite -> expire via clock -> AcceptInvite returns InviteExpired
    let rej_res = handle_fleets(
        &ctx,
        &mut m1_sess,
        svc,
        "RejectInvite",
        &[PyRep::Integer(fleet_id), PyRep::Bool(false)],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(rej_res.dict_get("rejected"), Some(&PyRep::Bool(true)));
    let _ = handle_fleets(
        &ctx,
        &mut boss_sess,
        svc,
        "Invite",
        &[
            PyRep::Integer(m1_id),
            PyRep::Integer(wing_id),
            PyRep::Integer(squad_id),
            PyRep::Integer(3),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    clock.advance_secs(121);
    assert_eq!(
        handle_fleets(&ctx, &mut m1_sess, svc, "AcceptInvite", &[PyRep::Integer(fleet_id)], &kw)
            .await
            .unwrap()
            .unwrap_err()
            .code,
        "InviteExpired"
    );

    // Fresh invite + AcceptInvite as SC, then invite m2 as SquadMember
    let _ = handle_fleets(
        &ctx,
        &mut boss_sess,
        svc,
        "Invite",
        &[
            PyRep::Integer(m1_id),
            PyRep::Integer(wing_id),
            PyRep::Integer(squad_id),
            PyRep::Integer(3),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let _ = handle_fleets(&ctx, &mut m1_sess, svc, "AcceptInvite", &[PyRep::Integer(fleet_id)], &kw)
        .await
        .unwrap()
        .unwrap();

    // SC cannot invite as WingCommander -> FleetPermissionDenied
    assert_eq!(
        handle_fleets(
            &ctx,
            &mut m1_sess,
            svc,
            "Invite",
            &[
                PyRep::Integer(m2_id),
                PyRep::Integer(wing_id),
                PyRep::Integer(-1),
                PyRep::Integer(2),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );

    // Boss invites m2, m2 joins; FinishMove <2 args, MoveMember free_move self-move
    let _ = handle_fleets(
        &ctx,
        &mut boss_sess,
        svc,
        "Invite",
        &[
            PyRep::Integer(m2_id),
            PyRep::Integer(wing_id),
            PyRep::Integer(squad_id),
            PyRep::Integer(4),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let _ = handle_fleets(&ctx, &mut m2_sess, svc, "AcceptInvite", &[PyRep::Integer(fleet_id)], &kw)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        handle_fleets(
            &ctx,
            &mut boss_sess,
            svc,
            "FinishMove",
            &[PyRep::Integer(m2_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap(),
        PyRep::None
    );

    // CannotKickBoss
    assert_eq!(
        handle_fleets(
            &ctx,
            &mut m1_sess,
            svc,
            "KickMember",
            &[PyRep::Integer(boss_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "CannotKickBoss"
    );

    // Move m2 into wing2 via FinishMove as boss (while session change timer inactive)
    let wing2 = handle_fleets(
        &ctx,
        &mut boss_sess,
        svc,
        "CreateWing",
        &[PyRep::String("Beta".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap()
    .dict_get("wingID")
    .and_then(|v| v.as_i64())
    .unwrap();
    let squad2 = handle_fleets(
        &ctx,
        &mut boss_sess,
        svc,
        "CreateSquad",
        &[PyRep::Integer(wing2), PyRep::String("Beta-1".into())],
        &kw,
    )
    .await
    .unwrap()
    .unwrap()
    .dict_get("squadID")
    .and_then(|v| v.as_i64())
    .unwrap();
    // Advance clock past session change timer from AcceptInvite
    clock.advance_secs(15);
    let _ = handle_fleets(
        &ctx,
        &mut boss_sess,
        svc,
        "MoveMember",
        &[
            PyRep::Integer(m2_id),
            PyRep::Integer(wing2),
            PyRep::Integer(squad2),
            PyRep::Integer(3), // SC
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();

    // SC m1 cannot kick cross-squad m2
    assert_eq!(
        handle_fleets(
            &ctx,
            &mut m1_sess,
            svc,
            "KickMember",
            &[PyRep::Integer(m2_id)],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err()
        .code,
        "FleetPermissionDenied"
    );

    // Boss LeaveFleet transfers boss role to remaining member (m1 or m2)
    let leave_res = handle_fleets(&ctx, &mut boss_sess, svc, "LeaveFleet", &[], &kw)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(leave_res.dict_get("fleetID").and_then(|v| v.as_i64()), Some(fleet_id));
    assert_eq!(leave_res.dict_get("leftFleetID").and_then(|v| v.as_i64()), Some(fleet_id));
    assert_eq!(leave_res.dict_get("left").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(leave_res.dict_get("disbanded").and_then(|v| v.as_bool()), Some(false));
    assert!(leave_res
        .dict_get("newBossID")
        .and_then(|v| v.as_i64())
        .is_some_and(|nb| nb != boss_id));
    assert!(boss_sess.fleet_id.is_none());
    ctx.db.with_snapshot(|snap| {
        let f = snap.fleets.get(&fleet_id).expect("fleet stays alive");
        assert_ne!(f.boss_id, boss_id);
        assert_eq!(f.members.len(), 2);
    });
}
