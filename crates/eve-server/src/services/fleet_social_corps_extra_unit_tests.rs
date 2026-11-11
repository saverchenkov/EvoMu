//! Extra unit and mutation-killing tests for `services::fleet_social::fleet_social_corps`
//! covering `promote_due_pending_corp_grants` (title & scoped role grants, friendly fire),
//! `get_character_medals_and_decorations` (status <= 1 vs > 1), `record_corp_join_in_snap_pub`
//! governance cleanup and employment history, `require_corp_caller` auto-sync and external corp denial,
//! `UpdateCorporation` (leading corp_id arg & `allowWar` toggle), `UpdateDivisionNames` Dict payload
//! and `115..=121` / out-of-range division IDs, non-director `GetMembers` privacy masking,
//! `UpdateApplicationOffer` error branches, `CanLeaveCurrentCorporation` (NPC & active war),
//! `CanBeKickedOut` guards, and `GiveCashFromCorpAccount` division take-bit / Accountant auth.

#![forbid(unsafe_code)]

use crate::services::fleet_social::fleet_social_corps::fleet_social_corps_tests::seed_player_corps;
use crate::services::fleet_social::fleet_social_corps::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_with_clock;
use crate::services::fleet_social::handle;
use crate::services::space_destiny::{clear_session_change_timer, record_session_change_timer};
use eve_db::{corp_roles, WarRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

fn call_corps(
    ctx: &crate::router::ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    tokio::runtime::Runtime::new()
        .unwrap()
        .block_on(handle(ctx, sess, svc, method, args, kwargs))
        .unwrap_or_else(|| panic!("unhandled {svc}.{method}"))
}

#[test]
fn pending_grants_medals_employment_history_and_require_corp_caller_guards() {
    let now = 1_800_020_000_u64;
    let ctx = test_ctx_with_clock(now);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);

    // 1. promote_due_pending_corp_grants: corp_id <= 0 no-op; title & scoped role grants + friendly_fire mature
    promote_due_pending_corp_grants(&ctx, 0);
    with_corp_gov(&ctx, |g| {
        let st = g.entry(98_000_001).or_default();
        st.pending_title_role_grants
            .insert(90_000_002, (corp_roles::ROLE_ACCOUNTANT, now - 1));
        st.pending_scoped_role_grants
            .insert(90_000_002, (1, 2, 4, now - 1));
        st.pending_friendly_fire = Some((true, now - 1));
        // Recipient with status 1 (decoration only) vs status 2 (both medal and decoration)
        st.medal_recipients
            .push((99, 90_000_002, 90_000_001, "Private Ribbon".into(), 1));
        st.medal_recipients
            .push((100, 90_000_002, 90_000_001, "Public Star".into(), 2));
    });
    promote_due_pending_corp_grants(&ctx, 98_000_001);

    ctx.db.with_snapshot(|snap| {
        let ra = &snap.corp_role_assignments[&(98_000_001, 90_000_002)];
        assert!(corp_roles::has_role(ra.roles, corp_roles::ROLE_ACCOUNTANT));
        assert_eq!((ra.roles_at_hq, ra.roles_at_base, ra.roles_at_other), (1, 2, 4));
        assert!(snap.corp_governance[&98_000_001].friendly_fire_legal);

        let (medals, decors) = get_character_medals_and_decorations(snap, 90_000_002);
        assert_eq!(medals.len(), 1);
        assert_eq!(decors.len(), 2);
    });

    // 2. record_corp_join_in_snap_pub cleans old corp governance state and updates employment history
    ctx.db.with_mut_snapshot(|snap| {
        let g = snap.corp_governance.entry(98_000_001).or_default();
        g.member_titles.insert(90_000_002, 1);
        g.member_divisions.insert(90_000_002, (2, 3));
        g.member_bases.insert(90_000_002, 60_003_760);
        g.member_account_keys.insert(90_000_002, 1001);

        let _ = record_corp_join_in_snap_pub(snap, 90_000_002, 98_000_001, 98_000_002, now);
        let g_after = &snap.corp_governance[&98_000_001];
        assert!(!g_after.member_titles.contains_key(&90_000_002));
        assert!(!g_after.member_divisions.contains_key(&90_000_002));
        assert!(!g_after.member_bases.contains_key(&90_000_002));
        assert!(!g_after.member_account_keys.contains_key(&90_000_002));
        let hist = &snap.employment_history[&90_000_002];
        assert_eq!(hist.len(), 2);
        assert_eq!(hist[0].0, 98_000_001);
        assert_eq!(hist[0].2, Some(now));
        assert_eq!(hist[1], (98_000_002, now, None));
    });

    // 3. require_corp_caller auto-syncs corporation_id from snapshot and rejects external target_corp_arg
    mem_sess.corporation_id = None;
    let (synced_corp, caller_cid, has_dir) =
        require_corp_caller(&ctx, &mut mem_sess, None).unwrap();
    assert_eq!((synced_corp, caller_cid, has_dir), (98_000_001, 90_000_002, false));
    assert_eq!(
        require_corp_caller(&ctx, &mut ceo_sess, Some(98_000_002))
            .unwrap_err()
            .code,
        "CorpAccessDenied"
    );
}

#[test]
fn update_corp_divisions_dict_non_director_members_and_audit_member() {
    let ctx = test_ctx_with_clock(1_800_025_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let kw = HashMap::new();

    // 1. UpdateCorporation with leading corp_id integer argument and allow_war toggle
    let upd = call_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::Integer(98_000_001),
            PyRep::String("Updated Desc".into()),
            PyRep::String("https://alpha.eve".into()),
            PyRep::Real(0.12),
            PyRep::Bool(false),
        ],
        &kw,
    )
    .unwrap();
    assert_eq!(
        upd.dict_get("description").and_then(PyRep::as_str),
        Some("Updated Desc")
    );

    // 2. UpdateDivisionNames via Dict payload, 115..=121 hangar flag ID, and DivisionNotFound (99)
    let dict_arg = PyRep::Dict(vec![
        (
            PyRep::String("division1".into()),
            PyRep::String("Dict Hangar 1".into()),
        ),
        (
            PyRep::String("walletDivision1".into()),
            PyRep::String("Dict Wallet 1".into()),
        ),
    ]);
    let div_dict = call_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateDivisionNames",
        &[dict_arg],
        &kw,
    )
    .unwrap();
    assert_eq!(
        div_dict.dict_get("name").and_then(PyRep::as_str),
        Some("Dict Hangar 1")
    );

    let div_116 = call_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateDivisionNames",
        &[
            PyRep::Integer(116), // 116 - 114 = division 2
            PyRep::String("Hangar Flag Div 2".into()),
        ],
        &kw,
    )
    .unwrap();
    assert_eq!(
        div_116.dict_get("divisionID").and_then(PyRep::as_i64),
        Some(2)
    );

    assert_eq!(
        call_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "UpdateDivisionNames",
            &[PyRep::Integer(99), PyRep::String("NoSuchDiv".into())],
            &kw,
        )
        .unwrap_err()
        .code,
        "DivisionNotFound"
    );

    // 3. Non-director GetMembers masks other members' locationID (0) while keeping own locationID
    let PyRep::Rowset { rows, .. } =
        call_corps(&ctx, &mut mem_sess, "corpRegistry", "GetMembers", &[], &kw).unwrap()
    else {
        panic!("expected Rowset")
    };
    let ceo_row = rows.iter().find(|r| r[0].as_i64() == Some(90_000_001)).unwrap();
    let self_row = rows.iter().find(|r| r[0].as_i64() == Some(90_000_002)).unwrap();
    assert_eq!(ceo_row[5].as_i64(), Some(0));
    assert_eq!(self_row[5].as_i64(), Some(30_000_142));

    // 4. AuditMember denied for line member; returns filtered audit log rows for CEO
    assert_eq!(
        call_corps(&ctx, &mut mem_sess, "corpmgr", "AuditMember", &[], &kw)
            .unwrap_err()
            .code,
        "CorpRoleDenied"
    );
    with_corp_gov(&ctx, |g| {
        g.entry(98_000_001).or_default().role_audit_log.push((
            90_000_002,
            90_000_001,
            0,
            corp_roles::ROLE_DIRECTOR,
            1_800_025_000,
        ));
    });
    let PyRep::Rowset { rows: audit_rows, .. } = call_corps(
        &ctx,
        &mut ceo_sess,
        "corpmgr",
        "AuditMember",
        &[PyRep::Integer(90_000_002)],
        &kw,
    )
    .unwrap()
    else {
        panic!("expected Rowset")
    };
    assert_eq!(audit_rows.len(), 1);
}

#[test]
fn applications_leave_kick_war_blockers_and_wallet_division_take_bits() {
    let now = 1_800_030_000_u64;
    let ctx = test_ctx_with_clock(now);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let kw = HashMap::new();

    // 1. InsertApplication error branches: InvalidArgument, AlreadyInCorporation, CorporationNotFound
    assert_eq!(
        call_corps(&ctx, &mut mem_sess, "corpRegistry", "InsertApplication", &[], &kw)
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        call_corps(
            &ctx,
            &mut mem_sess,
            "corpRegistry",
            "InsertApplication",
            &[PyRep::Integer(98_000_001)],
            &kw,
        )
        .unwrap_err()
        .code,
        "AlreadyInCorporation"
    );
    assert_eq!(
        call_corps(
            &ctx,
            &mut mem_sess,
            "corpRegistry",
            "InsertApplication",
            &[PyRep::Integer(98_999_999)],
            &kw,
        )
        .unwrap_err()
        .code,
        "CorporationNotFound"
    );

    // 2. Applicant 90_000_003 applies to 98_000_001; test UpdateApplicationOffer guards:
    //    InvalidApplicationAction, SessionChangeInProgress, MustBeDockedToJoinCorp, CorporationFull
    let mut app_sess = SessionState::new(30);
    app_sess.user_id = Some(90_000_003);
    app_sess.character_id = Some(90_000_003);
    app_sess.corporation_id = Some(98_000_002);
    app_sess.station_id = Some(60_003_760);

    let app_rep = call_corps(
        &ctx,
        &mut app_sess,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(98_000_001), PyRep::String("Join".into())],
        &kw,
    )
    .unwrap();
    let app_id = app_rep.dict_get("applicationID").and_then(PyRep::as_i64).unwrap();

    // Non-applicant cannot withdraw; non-director cannot reject; unknown action rejected
    assert_eq!(
        call_corps(
            &ctx,
            &mut mem_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(app_id), PyRep::String("Withdrawn".into())],
            &kw,
        )
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    assert_eq!(
        call_corps(
            &ctx,
            &mut mem_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(app_id), PyRep::String("Rejected".into())],
            &kw,
        )
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(app_id), PyRep::String("BogusAction".into())],
            &kw,
        )
        .unwrap_err()
        .code,
        "InvalidApplicationAction"
    );

    // Session-change timer active on applicant blocks acceptance
    record_session_change_timer(&ctx, 90_000_003, now, 30_000_142, 1, "jump");
    assert_eq!(
        call_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
            &kw,
        )
        .unwrap_err()
        .code,
        "SessionChangeInProgress"
    );
    clear_session_change_timer(&ctx, 90_000_003);

    // Applicant undocked in space blocks acceptance
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&90_000_003).unwrap().station_id = None;
    });
    assert_eq!(
        call_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
            &kw,
        )
        .unwrap_err()
        .code,
        "MustBeDockedToJoinCorp"
    );
    ctx.db.with_mut_snapshot(|s| {
        s.characters.get_mut(&90_000_003).unwrap().station_id = Some(60_003_760);
    });

    // Member limit == 2 blocks acceptance and InsertInvitation -> CorporationFull
    with_corp_gov(&ctx, |g| {
        g.entry(98_000_001).or_default().member_limit = Some(2);
    });
    assert_eq!(
        call_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
            &kw,
        )
        .unwrap_err()
        .code,
        "CorporationFull"
    );
    assert_eq!(
        call_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "InsertInvitation",
            &[PyRep::Integer(90_000_003)],
            &kw,
        )
        .unwrap_err()
        .code,
        "CorporationFull"
    );

    // 3. CanLeaveCurrentCorporation: sole member in corp with active war -> false ("Corporation has active wars")
    ctx.db.with_mut_snapshot(|s| {
        s.corporations.get_mut(&98_000_001).unwrap().members.remove(&90_000_002);
        s.wars.insert(
            1,
            WarRecord {
                war_id: 1,
                aggressor_corp_id: 98_000_001,
                defender_corp_id: 98_000_002,
                mutual: false,
                started_unix: now - 100,
                active: true,
            },
        );
    });
    let war_leave = call_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "CanLeaveCurrentCorporation",
        &[],
        &kw,
    )
    .unwrap();
    assert_eq!(
        war_leave.dict_get("canLeave").and_then(PyRep::as_bool),
        Some(false)
    );
    assert_eq!(
        war_leave.dict_get("reason").and_then(PyRep::as_str),
        Some("Corporation has active wars")
    );

    // 4. GiveCashFromCorpAccount authorized via specific division 1 take bit (1 << 20) and InsufficientDivisionFunds
    ctx.db.with_mut_snapshot(|s| {
        let c = s.corporations.get_mut(&98_000_001).unwrap();
        c.members.insert(90_000_002);
        c.member_roles.insert(90_000_002, eve_db::corp_roles::ROLE_ACCOUNT_CAN_TAKE[0]); // div 1 take bit only
    });
    let div_xfer = call_corps(
        &ctx,
        &mut mem_sess,
        "account",
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(1_000.0)],
        &kw,
    )
    .unwrap();
    assert_eq!(
        div_xfer.dict_get("transferredISK").and_then(PyRep::as_f64),
        Some(1_000.0)
    );
    assert_eq!(
        call_corps(
            &ctx,
            &mut mem_sess,
            "account",
            "GiveCashFromCorpAccount",
            &[PyRep::Integer(2), PyRep::Integer(1), PyRep::Real(500.0)], // lacks div 2 take bit
            &kw,
        )
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        call_corps(
            &ctx,
            &mut ceo_sess,
            "account",
            "GiveCashFromCorpAccount",
            &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(999_999_999_999.0)],
            &kw,
        )
        .unwrap_err()
        .code,
        "InsufficientDivisionFunds"
    );
}
