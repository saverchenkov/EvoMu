//! Unit tests for `fleet_social_corp_tail` role updates, delayed hangar grants, blockers, and vote creation.

#[cfg(test)]
#[path = "fleet_social_corp_tail_lifecycle_tests.rs"]
mod fleet_social_corp_tail_lifecycle_tests;

use super::*;
use crate::services::fleet_social::fleet_social_corps::fleet_social_corps_tests::seed_player_corps;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_with_clock;
use eve_db::corp_roles;

#[test]
fn corp_role_updates_cover_all_payload_shapes_delayed_hangars_and_title_blockers() {
    let ctx = test_ctx_with_clock(1_800_020_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);

    // Seed a role-granting corp title #1 and an office in corp_governance
    super::super::fleet_social_corps::with_corp_gov(&ctx, |g| {
        let st = g.entry(98_000_001).or_default();
        st.titles.insert(
            1,
            ("Quartermaster".into(), corp_roles::ROLE_ACCOUNTANT),
        );
    });

    // Empty args -> InvalidArgument; negative mask -> CrpAccessDenied; non-director caller -> CorpRoleDenied
    assert_eq!(
        handle_update_members(&ctx, &mut ceo_sess, "UpdateMembers", &[])
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_update_members(
            &ctx,
            &mut ceo_sess,
            "UpdateMember",
            &[PyRep::keyval([
                ("characterID", PyRep::Integer(90_000_002)),
                ("roles", PyRep::Integer(-1)),
            ])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );
    assert_eq!(
        handle_update_members(
            &ctx,
            &mut mem_sess,
            "UpdateMember",
            &[PyRep::keyval([
                ("characterID", PyRep::Integer(90_000_002)),
                ("roles", PyRep::Integer(0)),
            ])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Non-CEO director cannot toggle ROLE_DIRECTOR on another member
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&98_000_001)
            .unwrap()
            .member_roles
            .insert(90_000_002, corp_roles::ROLE_DIRECTOR);
    });
    assert_eq!(
        handle_update_members(
            &ctx,
            &mut mem_sess,
            "UpdateMember",
            &[PyRep::keyval([
                ("characterID", PyRep::Integer(90_000_002)),
                ("roles", PyRep::Integer(0)),
            ])],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CrpAccessDenied"
    );

    // Reset 90_000_002 to 0 roles, then grant immediate role + delayed hangar role + scoped roles + title mask via Dict/KeyVal
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&98_000_001)
            .unwrap()
            .member_roles
            .insert(90_000_002, 0);
    });
    let delayed_bit = corp_roles::CORP_DELAYED_DIVISION_ROLE_MASK.trailing_zeros();
    let combined_roles = corp_roles::ROLE_PERSONNEL_MANAGER | (1u64 << delayed_bit);
    let res = handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMembers",
        &[PyRep::List(vec![PyRep::keyval([
            ("characterID", PyRep::Integer(90_000_002)),
            ("roles", PyRep::Integer(combined_roles as i64)),
            ("grantableRoles", PyRep::Integer(corp_roles::ROLE_DIPLOMAT as i64)),
            ("rolesAtHQ", PyRep::Integer(combined_roles as i64)),
            ("grantableRolesAtHQ", PyRep::Integer(0)),
            ("rolesAtBase", PyRep::Integer(corp_roles::ROLE_AUDITOR as i64)),
            ("grantableRolesAtBase", PyRep::Integer(0)),
            ("rolesAtOther", PyRep::Integer(0)),
            ("grantableRolesAtOther", PyRep::Integer(0)),
            ("divisionID", PyRep::Integer(2)),
            ("squadronID", PyRep::Integer(3)),
            ("baseID", PyRep::Integer(60_003_760)),
            ("titleMask", PyRep::Integer(1)),
        ])])],
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(u_kv) = res else { panic!() };
    assert_eq!(u_kv.get("characterID").unwrap().as_i64().unwrap(), 90_000_002);
    assert_eq!(u_kv.get("delayed24h").unwrap().as_bool(), Some(true));
    assert_eq!(u_kv.get("updatedCount").unwrap().as_i64(), Some(1));

    // member_kick_blocker catches active roles first
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            member_kick_blocker(snap, 98_000_001, 90_000_002, 1_800_020_010),
            Some("MemberHasRoles")
        );
    });

    // Advance clock past 24h so pending hangar roles mature on next read/promote
    ctx.advance_secs(86_405);
    super::super::fleet_social_corps::promote_due_pending_corp_grants(&ctx, 98_000_001);
    ctx.db.with_snapshot(|snap| {
        let eff = snap.corporations.get(&98_000_001).unwrap().member_roles.get(&90_000_002).copied().unwrap_or(0);
        assert_eq!(eff & (1u64 << delayed_bit), 1u64 << delayed_bit);
    });

    // Positional tuple/row formats (4-elem short row, 14-elem named row, numeric row) also work
    assert!(handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[
            PyRep::Integer(90_000_002),
            PyRep::String("Pilot".into()),
            PyRep::Integer(0),
            PyRep::Integer(1),
        ],
    )
    .unwrap()
    .is_ok());
    // Clear scoped roles too -> now only title #1 (ROLE_ACCOUNTANT) remains -> MemberHasRoleGrantingTitle
    assert!(handle_update_members(
        &ctx,
        &mut ceo_sess,
        "UpdateMember",
        &[PyRep::keyval([
            ("characterID", PyRep::Integer(90_000_002)),
            ("roles", PyRep::Integer(0)),
            ("grantableRoles", PyRep::Integer(0)),
            ("rolesAtHQ", PyRep::Integer(0)),
            ("rolesAtBase", PyRep::Integer(0)),
            ("rolesAtOther", PyRep::Integer(0)),
            ("titleMask", PyRep::Integer(1)),
        ])],
    )
    .unwrap()
    .is_ok());
    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            member_kick_blocker(snap, 98_000_001, 90_000_002, ctx.now_unix()),
            Some("MemberHasRoleGrantingTitle")
        );
    });
}

#[test]
fn insert_vote_case_covers_ceo_election_and_share_creation_branches() {
    let ctx = test_ctx_with_clock(1_800_021_000);
    let _ = seed_player_corps(&ctx);

    // Seed shares for 90_000_002 so member can propose CEO election (vote_type=0)
    super::super::fleet_social_corps::with_corp_gov(&ctx, |g| {
        let st = g.entry(98_000_001).or_default();
        st.shares.insert(90_000_002, 500);
        st.company_shares = 1_000;
    });

    // Non-member rejected
    assert_eq!(
        handle_insert_vote_case(
            &ctx,
            98_000_001,
            90_000_999,
            1_800_021_000,
            &[PyRep::String("Vote".into())],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Shareholder 90_000_002 proposes CEO election with candidate 90_000_001
    let ceo_vote = handle_insert_vote_case(
        &ctx,
        98_000_001,
        90_000_002,
        1_800_021_000,
        &[
            PyRep::String("New CEO".into()),
            PyRep::String("Elect Prime".into()),
            PyRep::Integer(0),
            PyRep::List(vec![PyRep::Integer(90_000_001)]),
        ],
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(cv_kv) = ceo_vote else { panic!() };
    assert_eq!(cv_kv.get("voteType").unwrap().as_i64().unwrap(), 0);
    assert_eq!(cv_kv.get("parameter").unwrap().as_i64().unwrap(), 90_000_001);

    // Non-director shareholder cannot create share issuance vote (vote_type=5); CEO can!
    assert_eq!(
        handle_insert_vote_case(
            &ctx,
            98_000_001,
            90_000_002,
            1_800_021_000,
            &[
                PyRep::String("Mint".into()),
                PyRep::String("Issue 100 shares".into()),
                PyRep::Integer(5),
                PyRep::Integer(100),
            ],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    let share_vote = handle_insert_vote_case(
        &ctx,
        98_000_001,
        90_000_001,
        1_800_021_000,
        &[
            PyRep::String("Mint".into()),
            PyRep::String("Issue 250 shares".into()),
            PyRep::Integer(5),
            PyRep::List(vec![PyRep::Tuple(vec![PyRep::String("Issue 250".into()), PyRep::Integer(250)])]),
        ],
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(sv_kv) = share_vote else { panic!() };
    assert_eq!(sv_kv.get("voteType").unwrap().as_i64().unwrap(), 5);
    assert_eq!(sv_kv.get("parameter").unwrap().as_i64().unwrap(), 250);
}
