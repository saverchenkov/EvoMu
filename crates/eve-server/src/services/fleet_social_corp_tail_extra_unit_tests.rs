//! Extra unit and mutation-killing tests for `services::fleet_social::fleet_social_corp_tail`
//! covering `bloodline_default_npc_corp` fallback, every role/title/cooldown branch of
//! `member_kick_blocker` (including exact 86_400s boundary), all row/Rowset/Dict payload
//! shapes and non-CEO `ROLE_DIRECTOR` protections in `handle_update_members`,
//! `emit_corp_role_session_changed`, `handle_kick_out_members` session-change & role guards,
//! downtime stale auto-kick cleanup, and `InsertVoteCase` via `voteManager.InsertVoteCase`.

use crate::services::fleet_social::fleet_social_corp_tail::*;
use crate::services::fleet_social::fleet_social_corps::fleet_social_corps_tests::seed_player_corps;
use crate::services::fleet_social::fleet_social_corps::with_corp_gov;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_with_clock;
use eve_db::corp_roles;
use eve_db::snapshot_ext::{CorpMemberRoleAssignment, CorpTitleEntry};
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn bloodline_default_npc_corp_and_member_kick_blocker_all_branches_and_cooldown_boundary() {
    let ctx = test_ctx_with_clock(1_800_050_000);
    let _ = seed_player_corps(&ctx);
    let corp_id = 98_000_001i64;
    let target_id = 90_000_002i64;

    // 1. bloodline_default_npc_corp: unknown character falls back to 1_000_006
    ctx.db.with_snapshot(|snap| {
        assert_eq!(bloodline_default_npc_corp(&ctx, snap, 999_999_999), 1_000_006);
        assert!(bloodline_default_npc_corp(&ctx, snap, target_id) >= 1_000_000);
        // Unknown corporation returns None in member_kick_blocker
        assert_eq!(member_kick_blocker(snap, 999_999_999, target_id, 1_800_050_000), None);
    });

    // Clear all roles/titles on target_id so baseline is None
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(target_id, 0);
        snap.corp_role_assignments.remove(&(corp_id, target_id));
        if let Some(g) = snap.corp_governance.get_mut(&corp_id) {
            g.member_titles.remove(&target_id);
            g.role_drop_timestamps.remove(&target_id);
        }
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(member_kick_blocker(snap, corp_id, target_id, 1_800_050_000), None);
    });

    // 2. Test each of the 8 role fields in CorpMemberRoleAssignment individually
    let setters: &[fn(&mut CorpMemberRoleAssignment)] = &[
        |ra| ra.roles = 1,
        |ra| ra.grantable_roles = 1,
        |ra| ra.roles_at_hq = 1,
        |ra| ra.grantable_roles_at_hq = 1,
        |ra| ra.roles_at_base = 1,
        |ra| ra.grantable_roles_at_base = 1,
        |ra| ra.roles_at_other = 1,
        |ra| ra.grantable_roles_at_other = 1,
    ];
    for setter in setters {
        ctx.db.with_mut_snapshot(|snap| {
            let mut ra = CorpMemberRoleAssignment {
                character_id: target_id,
                ..Default::default()
            };
            setter(&mut ra);
            snap.corp_role_assignments.insert((corp_id, target_id), ra);
        });
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                member_kick_blocker(snap, corp_id, target_id, 1_800_050_000),
                Some("MemberHasRoles")
            );
        });
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_role_assignments.remove(&(corp_id, target_id));
    });

    // 3. Pending role grants (pending_role_grants, pending_title_role_grants, pending_scoped_role_grants)
    with_corp_gov(&ctx, |g| {
        g.entry(corp_id).or_default().pending_role_grants.insert(target_id, (1, 1_900_000_000));
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(member_kick_blocker(snap, corp_id, target_id, 1_800_050_000), Some("MemberHasRoles"));
    });
    with_corp_gov(&ctx, |g| {
        let st = g.entry(corp_id).or_default();
        st.pending_role_grants.clear();
        st.pending_title_role_grants.insert(target_id, (1, 1_900_000_000));
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(member_kick_blocker(snap, corp_id, target_id, 1_800_050_000), Some("MemberHasRoles"));
    });
    with_corp_gov(&ctx, |g| {
        let st = g.entry(corp_id).or_default();
        st.pending_title_role_grants.clear();
        st.pending_scoped_role_grants.insert(target_id, (1, 0, 0, 1_900_000_000));
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(member_kick_blocker(snap, corp_id, target_id, 1_800_050_000), Some("MemberHasRoles"));
    });
    with_corp_gov(&ctx, |g| {
        g.entry(corp_id).or_default().pending_scoped_role_grants.clear();
    });

    // 4. snap.corp_titles entries (roles, grantable_roles, roles_at_hq, roles_at_base, roles_at_other)
    with_corp_gov(&ctx, |g| {
        let st = g.entry(corp_id).or_default();
        st.titles.clear();
        st.member_titles.insert(target_id, 2); // bit 1 -> title_id = 2
    });
    let title_setters: &[fn(&mut CorpTitleEntry)] = &[
        |t| t.roles = vec!["Accountant".into()],
        |t| t.grantable_roles = vec!["Accountant".into()],
        |t| t.roles_at_hq = vec!["Hangar_Take_1".into()],
        |t| t.roles_at_base = vec!["Hangar_Take_1".into()],
        |t| t.roles_at_other = vec!["Hangar_Take_1".into()],
    ];
    for t_set in title_setters {
        ctx.db.with_mut_snapshot(|snap| {
            let mut te = CorpTitleEntry {
                title_id: 2,
                name: "Officer".into(),
                ..Default::default()
            };
            t_set(&mut te);
            snap.corp_titles.insert(corp_id, vec![te]);
        });
        ctx.db.with_snapshot(|snap| {
            assert_eq!(
                member_kick_blocker(snap, corp_id, target_id, 1_800_050_000),
                Some("MemberHasRoleGrantingTitle")
            );
        });
    }
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_titles.remove(&corp_id);
        snap.corp_governance.get_mut(&corp_id).unwrap().member_titles.clear();
    });

    // 5. Role drop 24h (86_400s) cooldown exact boundary:
    //    drop_ts = 1_000_000 -> at 1_086_399 returns Some("RoleDropCooldownActive"), at 1_086_400 returns None
    with_corp_gov(&ctx, |g| {
        g.entry(corp_id).or_default().role_drop_timestamps.insert(target_id, 1_000_000);
    });
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            member_kick_blocker(snap, corp_id, target_id, 1_086_399),
            Some("RoleDropCooldownActive")
        );
        assert_eq!(member_kick_blocker(snap, corp_id, target_id, 1_086_400), None);
    });
}

#[test]
fn update_members_covers_row_shapes_director_toggles_base_removal_and_session_notify() {
    let ctx = test_ctx_with_clock(1_800_060_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let corp_id = 98_000_001i64;
    let target_id = 90_000_002i64;

    // 1. TargetNotInCorp when target is not a member of corp_id
    assert_eq!(
        handle_update_members(
            &ctx,
            &mut ceo_sess,
            "UpdateMember",
            &[PyRep::keyval([
                ("characterID", PyRep::Integer(90_000_999)),
                ("roles", PyRep::Integer(0)),
            ])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "TargetNotInCorp"
    );

    // 2. Non-CEO Director cannot toggle ROLE_DIRECTOR in any of the 8 role/grantable/scoped masks
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .member_roles
            .insert(target_id, corp_roles::ROLE_DIRECTOR);
    });
    for field in [
        "grantableRoles",
        "rolesAtHQ",
        "grantableRolesAtHQ",
        "rolesAtBase",
        "grantableRolesAtBase",
        "rolesAtOther",
        "grantableRolesAtOther",
    ] {
        assert_eq!(
            handle_update_members(
                &ctx,
                &mut mem_sess,
                "UpdateMember",
                &[PyRep::keyval([
                    ("characterID", PyRep::Integer(90_000_001)),
                    (field, PyRep::Integer(corp_roles::ROLE_DIRECTOR as i64)),
                ])],
            )
            .unwrap()
            .unwrap_err()
            .code,
            "CrpAccessDenied",
            "non-CEO director should not be able to toggle ROLE_DIRECTOR in {field}"
        );
    }

    // 3. 14-element named row format, numeric row with bool offset at index 3, and Rowset format
    let delayed_bit = 1u64 << corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK.trailing_zeros();
    with_corp_gov(&ctx, |g| {
        g.entry(corp_id)
            .or_default()
            .titles
            .insert(2, ("HangarOfficer".into(), delayed_bit));
    });
    let row_14 = vec![
        PyRep::Integer(target_id),
        PyRep::String("Pilot".into()),
        PyRep::Integer(4),                     // divisionID
        PyRep::Integer(5),                     // squadronID
        PyRep::Integer(corp_roles::ROLE_AUDITOR as i64),
        PyRep::Integer(corp_roles::ROLE_DIPLOMAT as i64),
        PyRep::Integer(delayed_bit as i64),    // rolesAtHQ (delayed hangar)
        PyRep::Integer(1),                     // grantableRolesAtHQ
        PyRep::Integer(delayed_bit as i64),    // rolesAtBase (delayed hangar)
        PyRep::Integer(1),                     // grantableRolesAtBase
        PyRep::Integer(delayed_bit as i64),    // rolesAtOther (delayed hangar)
        PyRep::Integer(1),                     // grantableRolesAtOther
        PyRep::Integer(60_003_760),            // baseID > 0 -> inserts member_bases
        PyRep::Integer(2),                     // titleMask = 2 (title #2 has delayed hangar role)
    ];
    let r14 = handle_update_members(&ctx, &mut ceo_sess, "UpdateMember", &[PyRep::List(row_14)])
        .unwrap()
        .unwrap();
    let PyRep::KeyVal(kv14) = r14 else { panic!() };
    assert_eq!(kv14.get("delayed24h"), Some(&PyRep::Bool(true)));
    ctx.db.with_snapshot(|snap| {
        let g = snap.corp_governance.get(&corp_id).unwrap();
        assert_eq!(g.member_bases.get(&target_id), Some(&60_003_760));
        assert_eq!(g.member_divisions.get(&target_id), Some(&(4, 5)));
        assert!(g.pending_title_role_grants.contains_key(&target_id));
        assert!(g.pending_scoped_role_grants.contains_key(&target_id));
    });

    // Numeric row with Bool at index 3 and baseID = 0 -> removes member_bases entry
    let row_numeric_bool = vec![
        PyRep::Integer(target_id),
        PyRep::Integer(0),         // new_roles
        PyRep::Integer(0),         // title_mask
        PyRep::Bool(false),        // bool offset!
        PyRep::Integer(0),         // grantable_roles
        PyRep::Integer(0),         // roles_at_hq
        PyRep::Integer(0),         // grantable_roles_at_hq
        PyRep::Integer(0),         // roles_at_base
        PyRep::Integer(0),         // grantable_roles_at_base
        PyRep::Integer(0),         // roles_at_other
        PyRep::Integer(0),         // grantable_roles_at_other
        PyRep::Integer(0),         // base_id == 0 -> removes from member_bases
    ];
    let r_rowset = handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMembers",
        &[PyRep::rowset(vec!["charID"], vec![row_numeric_bool])],
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(kv_rs) = r_rowset else { panic!() };
    assert_eq!(kv_rs.get("updatedCount").and_then(|v| v.as_i64()), Some(1));
    ctx.db.with_snapshot(|snap| {
        let g = snap.corp_governance.get(&corp_id).unwrap();
        assert!(!g.member_bases.contains_key(&target_id));
        assert!(!g.pending_title_role_grants.contains_key(&target_id));
        assert!(!g.pending_scoped_role_grants.contains_key(&target_id));
    });

    // Dict with "memberID" and "role" keys updating CEO's own session
    let r_self = handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[PyRep::keyval([
            ("memberID", PyRep::Integer(90_000_001)),
            ("role", PyRep::Integer(corp_roles::ROLE_AUDITOR as i64)),
        ])],
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(kv_self) = r_self else { panic!() };
    assert_eq!(kv_self.get("characterID").and_then(|v| v.as_i64()), Some(90_000_001));
    assert_eq!(ceo_sess.corp_role, corp_roles::ROLE_AUDITOR);

    // 4. Direct emit_corp_role_session_changed no-op when nothing changed vs emits OnSessionChanged when changed
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&target_id).unwrap().user_id = 1);
    ctx.db.acquire_online_lock(1, 2, Some(target_id), true, ctx.now_unix()).unwrap();
    let _ = ctx.drain_notifications_for_char(target_id);
    emit_corp_role_session_changed(&ctx, corp_id, target_id, 1, 1, 2, 2, 3, 3, 4, 4);
    assert!(ctx.drain_notifications_for_char(target_id).is_empty());
    emit_corp_role_session_changed(&ctx, corp_id, target_id, 1, 2, 2, 4, 3, 6, 4, 8);
    let notifs = ctx.drain_notifications_for_char(target_id);
    assert!(notifs.iter().any(|n| n.event == "OnSessionChanged"));
}

#[tokio::test]
async fn kick_out_guards_downtime_stale_cleanup_and_insert_vote_case_edge_cases() {
    let ctx = test_ctx_with_clock(1_800_070_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let corp_id = 98_000_001i64;
    let ceo_id = 90_000_001i64;
    let mem_id = 90_000_002i64;

    // 1. SessionChangeInProgress on caller and on target
    let now = ctx.now_unix();
    crate::services::space_destiny::record_session_change_timer(&ctx, ceo_id, now, 30_000_142, 0, "jump");
    assert_eq!(
        handle_kick_out_members(&ctx, &mut ceo_sess, &[PyRep::Integer(mem_id)])
            .unwrap()
            .unwrap_err()
            .code,
        "SessionChangeInProgress"
    );
    ctx.advance_secs(15);

    let now2 = ctx.now_unix();
    crate::services::space_destiny::record_session_change_timer(&ctx, mem_id, now2, 30_000_142, 0, "dock");
    assert_eq!(
        handle_kick_out_members(&ctx, &mut ceo_sess, &[PyRep::Integer(mem_id)])
            .unwrap()
            .unwrap_err()
            .code,
        "SessionChangeInProgress"
    );
    ctx.advance_secs(15);

    // 2. Non-director / non-personnel-manager cannot kick another member -> CorpRoleDenied
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(mem_id, 0);
    });
    assert_eq!(
        handle_kick_out_members(&ctx, &mut mem_sess, &[PyRep::Integer(ceo_id)])
            .unwrap()
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );

    // 3. NotCorpMember and CeoCannotLeave while other members remain
    assert_eq!(
        handle_kick_out_members(&ctx, &mut ceo_sess, &[PyRep::Integer(90_000_999)])
            .unwrap()
            .unwrap_err()
            .code,
        "NotCorpMember"
    );
    assert_eq!(
        handle_kick_out_members(&ctx, &mut ceo_sess, &[PyRep::Integer(ceo_id)])
            .unwrap()
            .unwrap_err()
            .code,
        "CeoCannotLeave"
    );

    // 4. execute_pending_auto_kicks_on_downtime cleans up stale queued member who already left
    with_corp_gov(&ctx, |g| {
        g.entry(corp_id).or_default().pending_auto_kicks.insert(90_000_999, ctx.now_unix());
    });
    assert_eq!(execute_pending_auto_kicks_on_downtime(&ctx), 0);
    ctx.db.with_snapshot(|snap| {
        assert!(
            !snap
                .corp_governance
                .get(&corp_id)
                .unwrap()
                .pending_auto_kicks
                .contains_key(&90_000_999)
        );
    });

    // 5. InsertVoteCase via `voteManager.InsertVoteCase`:
    //    - Leading corp_id argument with non-existent corp -> CorporationNotFound
    //    - CandidateNotMember for integer option, tuple option, and default option
    //    - Share creation (vote_type == 5) bounds: <= 0 and > 10_000_000_000 -> InvalidArgument
    //    - Rowset / String options
    let kw = HashMap::new();
    let err_no_corp = crate::services::fleet_social::handle(
        &ctx,
        &mut ceo_sess,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::Integer(999_999_999),
            PyRep::String("Election".into()),
            PyRep::String("Desc".into()),
            PyRep::Integer(0),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_no_corp.code, "CorporationNotFound");

    // CandidateNotMember (integer option)
    let err_cand_int = crate::services::fleet_social::handle(
        &ctx,
        &mut ceo_sess,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::String("Election".into()),
            PyRep::String("Desc".into()),
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Integer(90_000_999)]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_cand_int.code, "CandidateNotMember");

    // CandidateNotMember (tuple option `[1, "Outlander", 90_000_999]`)
    let err_cand_tup = crate::services::fleet_social::handle(
        &ctx,
        &mut ceo_sess,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::String("Election".into()),
            PyRep::String("Desc".into()),
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::String("Outlander".into()),
                PyRep::Integer(90_000_999),
            ])]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(err_cand_tup.code, "CandidateNotMember");

    // Share creation (vote_type == 5) bounds: 0 and 10_000_000_001 -> InvalidArgument
    for bad_shares in [0i64, 10_000_000_001i64] {
        let err_sh = crate::services::fleet_social::handle(
            &ctx,
            &mut ceo_sess,
            "voteManager",
            "InsertVoteCase",
            &[
                PyRep::String("Shares".into()),
                PyRep::String("Bounds".into()),
                PyRep::Integer(5),
                PyRep::Integer(bad_shares),
            ],
            &kw,
        )
        .await
        .unwrap()
        .unwrap_err();
        assert_eq!(err_sh.code, "InvalidArgument");
    }

    // Valid general vote (vote_type == 4) with String options and leading corp_id
    let ok_vote = crate::services::fleet_social::handle(
        &ctx,
        &mut ceo_sess,
        "voteManager",
        "InsertVoteCase",
        &[
            PyRep::Integer(corp_id),
            PyRep::String("General Poll".into()),
            PyRep::String("Pick one".into()),
            PyRep::Integer(4),
            PyRep::List(vec![PyRep::String("Option Alpha".into()), PyRep::String("Option Beta".into())]),
        ],
        &kw,
    )
    .await
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(v_kv) = ok_vote else { panic!() };
    assert_eq!(v_kv.get("voteType").and_then(|v| v.as_i64()), Some(4));
}
