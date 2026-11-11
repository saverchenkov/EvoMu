//! Edge-case behavioral tests for `fleet_social_corps`.

use super::*;
use crate::services::fleet_social::fleet_social_corps::fleet_social_corps_tests::seed_player_corps;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::{corp_roles, CorpApplicationRecord, IskCents};
use eve_machonet::SessionState;
use std::collections::HashMap;

#[test]
fn eval_member_query_clause_operators_and_props() {
    let ctx = test_ctx_with_clock(1_700_000_000);
    let _ = seed_player_corps(&ctx);
    with_corp_gov(&ctx, |govs| {
        let gov = govs.entry(98_000_001).or_default();
        gov.member_titles.insert(90_000_001, 3);
        gov.member_bases.insert(90_000_001, 60_003_760);
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.employment_history
            .entry(90_000_001)
            .or_default()
            .push((98_000_001, 1_700_000_000, None));
        snap.corp_role_assignments.insert(
            (98_000_001, 90_000_001),
            eve_db::snapshot_ext::CorpMemberRoleAssignment {
                character_id: 90_000_001,
                roles: corp_roles::ROLE_DIRECTOR,
                grantable_roles: corp_roles::ROLE_DIRECTOR,
                roles_at_hq: 16,
                grantable_roles_at_hq: 32,
                roles_at_base: 64,
                grantable_roles_at_base: 128,
                roles_at_other: 256,
                grantable_roles_at_other: 512,
                ..Default::default()
            },
        );
    });

    let m = CorpMemberRowSnapshot {
        character_id: 90_000_001,
        name: "Alpha Pilot".into(),
        role_mask: 0b1010,
        title: "Fleet Boss".into(),
        corporation_id: 98_000_001,
        location_id: 60_003_760,
        ship_type_id: 606,
        logon_date_time: 0,
        grantable_roles: corp_roles::ROLE_DIRECTOR,
        roles_at_hq: 16,
        grantable_roles_at_hq: 32,
        roles_at_base: 64,
        grantable_roles_at_base: 128,
        roles_at_other: 256,
        grantable_roles_at_other: 512,
        title_mask: 3,
        base_id: Some(60_003_760),
        start_date_time: 1_700_000_000,
        account_key: 1000,
        division_id: 1,
        squadron_id: 2,
    };

    // Name case-insensitive substring vs exact equality/inequality and code 6
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "characterName", &PyRep::Integer(6), &PyRep::String("alpha".into())));
    assert!(!eval_member_query_clause(&ctx, 98_000_001, &m, "characterName", &PyRep::String("=".into()), &PyRep::String("alpha".into())));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "characterName", &PyRep::String("==".into()), &PyRep::String("ALPHA PILOT".into())));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "characterName", &PyRep::String("!=".into()), &PyRep::String("Beta".into())));

    // Integer comparison operators 1..=5 and "!="
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "characterID", &PyRep::Integer(1), &PyRep::Integer(90_000_001)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "characterID", &PyRep::Integer(2), &PyRep::Integer(90_000_000)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "characterID", &PyRep::Integer(3), &PyRep::Integer(90_000_001)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "characterID", &PyRep::Integer(4), &PyRep::Integer(90_000_002)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "characterID", &PyRep::Integer(5), &PyRep::Integer(90_000_001)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "characterID", &PyRep::String("!=".into()), &PyRep::Integer(90_000_002)));

    // Bitmask operators 7 / "&" and 8 / "!&"
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "roles", &PyRep::Integer(7), &PyRep::Integer(0b1000)));
    assert!(!eval_member_query_clause(&ctx, 98_000_001, &m, "roles", &PyRep::String("&".into()), &PyRep::Integer(0b0100)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "roles", &PyRep::Integer(8), &PyRep::Integer(0b0100)));
    assert!(!eval_member_query_clause(&ctx, 98_000_001, &m, "roles", &PyRep::String("!&".into()), &PyRep::Integer(0b0010)));

    // Alternate property aliases
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "baseID", &PyRep::Integer(1), &PyRep::Integer(60_003_760)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "titleMask", &PyRep::Integer(7), &PyRep::Integer(1)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "rolesAtHQ", &PyRep::Integer(1), &PyRep::Integer(16)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "grantableRolesAtHQ", &PyRep::Integer(1), &PyRep::Integer(32)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "rolesAtBase", &PyRep::Integer(1), &PyRep::Integer(64)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "grantableRolesAtBase", &PyRep::Integer(1), &PyRep::Integer(128)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "rolesAtOther", &PyRep::Integer(1), &PyRep::Integer(256)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "grantableRolesAtOther", &PyRep::Integer(1), &PyRep::Integer(512)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "grantableRoles", &PyRep::Integer(7), &PyRep::Integer(corp_roles::ROLE_DIRECTOR as i64)));

    // startDateTime compares against employment start timestamp in Unix seconds
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "startDateTime", &PyRep::String("==".into()), &PyRep::Integer(1_700_000_000)));
    assert!(eval_member_query_clause(&ctx, 98_000_001, &m, "unknownProp", &PyRep::Integer(1), &PyRep::Integer(0)));
}

#[test]
fn get_member_ids_by_query_supports_and_or_tuples_and_string_fallback() {
    let ctx = test_ctx_with_clock(1_700_000_000);
    let (mut ceo_sess, _) = seed_player_corps(&ctx);
    let kw = HashMap::new();

    // 4-tuple OR join: first clause uninitialized (ignored), OR char 90_000_002
    let query = PyRep::List(vec![
        PyRep::Tuple(vec![
            PyRep::None,
            PyRep::String("characterName".into()),
            PyRep::String("contains".into()),
            PyRep::String("NonExistent".into()),
        ]),
        PyRep::Tuple(vec![
            PyRep::Integer(2),
            PyRep::String("characterID".into()),
            PyRep::Integer(1),
            PyRep::Integer(90_000_002),
        ]),
    ]);
    let res = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMemberIDsByQuery",
        &[query],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::List(rows) = res else { panic!("expected list") };
    assert_eq!(rows, vec![PyRep::Integer(90_000_002)]);

    // 3-tuple AND clauses filtering to CEO only by role bitmask + name substring
    let and_query = PyRep::List(vec![
        PyRep::Tuple(vec![
            PyRep::String("roles".into()),
            PyRep::Integer(7),
            PyRep::Integer(corp_roles::ROLE_DIRECTOR as i64),
        ]),
        PyRep::Tuple(vec![
            PyRep::String("name".into()),
            PyRep::String("contains".into()),
            PyRep::String("prime".into()),
        ]),
    ]);
    let res_and = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMemberIDsByQuery",
        &[and_query],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::List(ids_and) = res_and else { panic!() };
    assert_eq!(ids_and, vec![PyRep::Integer(90_000_001)]);

    // String fallback matching partial char_id string
    let res_str = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMemberIDsByQuery",
        &[PyRep::String("90000002".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::List(ids_str) = res_str else { panic!() };
    assert_eq!(ids_str, vec![PyRep::Integer(90_000_002)]);
}

#[test]
fn audit_timestamp_normalization_and_audit_member_filters() {
    assert_eq!(normalize_audit_timestamp_to_unix(0), None);
    assert_eq!(normalize_audit_timestamp_to_unix(-10), None);
    assert_eq!(normalize_audit_timestamp_to_unix(1_700_000_500), Some(1_700_000_500));
    let ft = (1_700_000_500i64 + 11_644_473_600) * 10_000_000;
    assert_eq!(normalize_audit_timestamp_to_unix(ft), Some(1_700_000_500));

    let ctx = test_ctx_with_clock(1_700_001_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let kw = HashMap::new();

    // Non-director cannot audit
    let denied = handle_corps(
        &ctx,
        &mut mem_sess,
        "corpmgr",
        "AuditMember",
        &[PyRep::Integer(90_000_002)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(denied.code, "CorpRoleDenied");

    // Seed two role_audit_log tuples: (target_char_id, issuer_id, old_roles, new_roles, ts_unix)
    with_corp_gov(&ctx, |govs| {
        let gov = govs.entry(98_000_001).or_default();
        gov.role_audit_log.push((90_000_002, 90_000_001, 0, 1, 1_700_000_100));
        gov.role_audit_log.push((90_000_002, 90_000_001, 1, 3, 1_700_000_800));
    });

    // Filter with fromDate + rowsPerPage=1
    let mut filter_kw = HashMap::new();
    filter_kw.insert("fromDate".into(), PyRep::Integer(1_700_000_500));
    filter_kw.insert("rowsPerPage".into(), PyRep::Integer(1));
    let audit = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpmgr",
        "AuditMember",
        &[PyRep::Integer(90_000_002)],
        &filter_kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::Rowset { rows, .. } = audit else { panic!("expected rowset") };
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0][4].as_i64(), Some(1_700_000_800));
}

#[test]
fn corpmgr_attributes_update_corp_clamping_and_logo_errors() {
    let ctx = test_ctx_with_clock(1_700_000_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let kw = HashMap::new();

    // corpmgr attribute reads
    assert_eq!(
        handle_corps(&ctx, &mut ceo_sess, "corpmgr", "allianceID", &[], &kw)
            .unwrap()
            .unwrap()
            .as_i64(),
        Some(99_000_001)
    );
    assert_eq!(
        handle_corps(&ctx, &mut ceo_sess, "corpmgr", "ceoID", &[], &kw)
            .unwrap()
            .unwrap()
            .as_i64(),
        Some(90_000_001)
    );
    assert_eq!(
        handle_corps(&ctx, &mut ceo_sess, "corpmgr", "corporationID", &[], &kw)
            .unwrap()
            .unwrap()
            .as_i64(),
        Some(98_000_001)
    );
    // NPC corp GetPublicInfo fallback
    let npc_info = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpmgr",
        "GetPublicInfo",
        &[PyRep::Integer(1_000_009)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(npc_info, PyRep::KeyVal(_)));

    // Missing corp GetPublicInfo returns CorporationNotFound
    let missing_info = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpmgr",
        "GetPublicInfo",
        &[PyRep::Integer(98_999_999)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(missing_info.code, "CorporationNotFound");

    // UpdateCorporation with leading corp_id arg + out-of-range tax_rate clamped to 1.0 + allowWar toggle
    let updated = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::Integer(98_000_001),
            PyRep::String("Desc".into()),
            PyRep::String("https://ex.org".into()),
            PyRep::Real(2.5),
            PyRep::Bool(false),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(kv) = updated else { panic!() };
    assert!((kv.get("taxRate").unwrap().as_f64().unwrap() - 1.0).abs() < 1e-6);

    // UpdateLogo rejected for non-director
    let logo_err = handle_corps(
        &ctx,
        &mut mem_sess,
        "corpRegistry",
        "UpdateLogo",
        &[PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(logo_err.code, "CorpRoleDenied");

    // GetMembersPaged and GetMembersByIds edge behavior
    let paged = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMembersPaged",
        &[PyRep::Integer(1)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert!(matches!(paged, PyRep::ObjectEx { .. } | PyRep::Rowset { .. }));

    let by_ids = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMembersByIds",
        &[PyRep::Tuple(vec![PyRep::Integer(90_000_002), PyRep::Integer(99_999_999)])],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::Rowset { rows: by_id_rows, .. } = by_ids else { panic!() };
    assert_eq!(by_id_rows.len(), 1);
}

#[test]
fn applications_invitations_and_cash_transfer_error_edges() {
    let ctx = test_ctx_with_clock(1_700_000_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let kw = HashMap::new();

    // InsertApplication to NPC corp rejected
    let npc_app_err = handle_corps(
        &ctx,
        &mut mem_sess,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(1_000_009), PyRep::String("hi".into())],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(npc_app_err.code, "CannotApplyToNpcCorporation");

    // InsertApplication to own corp rejected
    let own_app_err = handle_corps(
        &ctx,
        &mut mem_sess,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(98_000_001), PyRep::String("hi".into())],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(own_app_err.code, "AlreadyInCorporation");

    // InsertInvitation with accept_now=true (arg 1) rejected + unknown char rejected
    let accept_now_err = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "InsertInvitation",
        &[
            PyRep::Integer(90_000_003),
            PyRep::Bool(true),
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(accept_now_err.code, "AccessDenied");

    let not_found_err = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "InsertInvitation",
        &[PyRep::Integer(99_999_999)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(not_found_err.code, "CharacterNotFound");

    // Seed an application from 90_000_003 to 98_000_001 on CorporationRecord.applications
    let app_id = 555_001;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&98_000_001)
            .unwrap()
            .applications
            .insert(
                app_id,
                CorpApplicationRecord {
                    application_id: app_id,
                    character_id: 90_000_003,
                    corporation_id: 98_000_001,
                    message: "pls".into(),
                    created_unix: 1_700_000_000,
                    status: "Applied".into(),
                },
            );
    });

    // Applicant session
    let mut app_sess = SessionState::new(90_000_003);
    app_sess.user_id = Some(90_000_003);
    app_sess.character_id = Some(90_000_003);
    app_sess.corporation_id = Some(98_000_002);
    app_sess.station_id = Some(60_003_760);

    // Invalid action code
    let bad_act = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[
            PyRep::Integer(app_id),
            PyRep::String("bad".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_act.code, "InvalidApplicationAction");

    // Applicant undocked cannot accept invitation -> MustBeDockedToJoinCorp
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&98_000_001)
            .unwrap()
            .applications
            .get_mut(&app_id)
            .unwrap()
            .status = "Invited".into();
        snap.characters.get_mut(&90_000_003).unwrap().station_id = None;
    });
    app_sess.station_id = None;
    let undocked_err = handle_corps(
        &ctx,
        &mut app_sess,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[
            PyRep::Integer(app_id),
            PyRep::String("Accepted".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(undocked_err.code, "MustBeDockedToJoinCorp");

    // CEO of another corp cannot accept invitation/offer to leave own corp -> CeoCannotLeaveCorp
    app_sess.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_003).unwrap().station_id = Some(60_003_760);
        snap.corporations.get_mut(&98_000_002).unwrap().ceo_id = 90_000_003;
    });
    let ceo_leave_err = handle_corps(
        &ctx,
        &mut app_sess,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[
            PyRep::Integer(app_id),
            PyRep::String("Accepted".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(ceo_leave_err.code, "CeoCannotLeaveCorp");

    // Restore non-CEO and Offered status, then close app via rejection -> ApplicationAlreadyClosed on retry
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&98_000_002).unwrap().ceo_id = 90_000_004;
        snap.corporations
            .get_mut(&98_000_001)
            .unwrap()
            .applications
            .get_mut(&app_id)
            .unwrap()
            .status = "Offered".into();
    });
    let _ = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[
            PyRep::Integer(app_id),
            PyRep::String("Rejected".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let closed_err = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[
            PyRep::Integer(app_id),
            PyRep::String("Rejected".into()),
        ],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(closed_err.code, "ApplicationAlreadyClosed");

    // GiveCashFromCorpAccount invalid amount / division / insufficient balance
    let bad_amt = handle_corps(
        &ctx,
        &mut ceo_sess,
        "account",
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(-5.0)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_amt.code, "InvalidArgument");

    let bad_div = handle_corps(
        &ctx,
        &mut ceo_sess,
        "account",
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(1), PyRep::Integer(1), PyRep::Real(100.0)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(bad_div.code, "InvalidTransferDivision");

    let low_funds = handle_corps(
        &ctx,
        &mut ceo_sess,
        "account",
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(999_999_999_999.0)],
        &kw,
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(low_funds.code, "InsufficientDivisionFunds");

    // CanBeKickedOut false for CEO
    let kick_ceo = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "CanBeKickedOut",
        &[PyRep::Integer(90_000_001)],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(kick_ceo, PyRep::Bool(false));

    // Add extra char in corp2 and confirm CanLeaveCurrentCorporation true for non-CEO
    ctx.db.with_mut_snapshot(|snap| {
        let ch4 = clone_char_with(snap, 90_000_004, "Other CEO", 98_000_002, 30_000_142, 100_000_004);
        snap.characters.insert(90_000_004, ch4);
        snap.corporations.get_mut(&98_000_002).unwrap().set_division_balance(1, IskCents::from_isk(100));
    });
    let can_leave = handle_corps(
        &ctx,
        &mut app_sess,
        "corpRegistry",
        "CanLeaveCurrentCorporation",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(can_leave.dict_get("canLeave"), Some(&PyRep::Bool(true)));

    // Direct unit checks for `promote_all_due_corp_grants` and `GetMembersPaged` (`handle_get_members_paged`)
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let gov = snap.corp_governance.entry(98_000_001).or_default();
        gov.pending_friendly_fire = Some((true, now_u.saturating_sub(10)));
    });
    super::promote_all_due_corp_grants(&ctx);
    ctx.db.with_snapshot(|snap| {
        assert!(snap.corp_governance[&98_000_001].friendly_fire_legal);
        assert!(snap.corp_governance[&98_000_001].pending_friendly_fire.is_none());
    });

    let paged = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMembersPaged",
        &[PyRep::Integer(1), PyRep::Integer(10)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::ObjectEx { header, .. } = paged else {
        panic!("expected PagedResultSet ObjectEx, got {paged:?}");
    };
    let PyRep::Tuple(h) = header.as_ref() else {
        panic!("expected Tuple header in PagedResultSet");
    };
    assert_eq!(h.len(), 2);
    assert!(matches!(h[0], PyRep::Token(ref t) if t == "eve.common.script.util.pagedCollection.PagedResultSet"));
    assert!(matches!(h[1], PyRep::Tuple(ref inner) if inner.len() == 4));

    let agg_obj = crate::services::fleet_social::fleet_social_corp_tail_dispatch::build_aggression_settings_object(
        98_000_001,
        true,
        PyRep::Integer(100),
        PyRep::None,
        Some(false),
        Some(200),
    );
    let PyRep::Object { type_name, args } = agg_obj else {
        panic!("expected AggressionSettings PyRep::Object");
    };
    assert_eq!(
        type_name,
        "crimewatch.corp_aggression.settings.AggressionSettings"
    );
    assert_eq!(args.dict_get("corporationID"), Some(&PyRep::Integer(98_000_001)));
    assert_eq!(args.dict_get("isFriendlyFireLegal"), Some(&PyRep::Bool(true)));
    assert_eq!(args.dict_get("pendingFriendlyFireLegal"), Some(&PyRep::Bool(false)));
    assert_eq!(args.dict_get("changeAtTime"), Some(&PyRep::Integer(200)));
}
