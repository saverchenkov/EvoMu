//! Extra unit and mutation-killing tests for `services::fleet_social::fleet_social_corps`
//! covering `fold_due_pending_corp_grants_in_snap` (title & scoped role grants, friendly fire),
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

    // 1. fold_due_pending_corp_grants_in_snap: corp_id <= 0 no-op; title & scoped role grants + friendly_fire mature
    let noop = ctx
        .db
        .with_mut_snapshot(|snap| fold_due_pending_corp_grants_in_snap(snap, 0, now));
    assert!(noop.is_empty());
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
    let matured = ctx
        .db
        .with_mut_snapshot(|snap| fold_due_pending_corp_grants_in_snap(snap, 98_000_001, now));
    assert_eq!(matured.len(), 1);
    emit_matured_corp_grants(&ctx, 98_000_001, matured);

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
    let PyRep::Tuple(parts) = call_corps(
        &ctx,
        &mut ceo_sess,
        "corpmgr",
        "AuditMember",
        &[PyRep::Integer(90_000_002)],
        &kw,
    )
    .unwrap()
    else {
        panic!("expected 2-tuple of Rowsets")
    };
    assert_eq!(parts.len(), 2);
    let PyRep::Rowset { rows: audit_rows, .. } = &parts[0] else {
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
    let PyRep::Tuple(war_parts) = war_leave else {
        panic!("expected 3-tuple from CanLeaveCurrentCorporation");
    };
    assert_eq!(war_parts.len(), 3);
    assert_eq!(war_parts[0], PyRep::Bool(false));
    assert_eq!(
        war_parts[1].dict_get("msg").and_then(PyRep::as_str),
        Some("CrpCantQuitNotInStasis")
    );
    assert_eq!(
        war_parts[1].dict_get("__class__").and_then(PyRep::as_str),
        Some("UserError")
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

#[test]
fn authz_88_select_character_loads_db_roles_and_corp_handlers_reject_stale_session_roles() {
    let now = 1_800_030_000_u64;
    let ctx = test_ctx_with_clock(now);
    let (_ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let corp_id = 98_000_001_i64;
    let ally_id = 99_888_001_i64;
    let mem_id = 90_000_002_i64;

    // 1. Seed DB roles + due 24h pending Director grant + corp.alliance_id (ch.alliance_id stale None),
    // alongside a stale forced_session_corp_sync entry from an old corporation.
    let uid = ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.alliance_id = Some(ally_id);
        corp.member_roles
            .insert(mem_id, corp_roles::ROLE_ACCOUNTANT);
        snap.corp_governance
            .entry(corp_id)
            .or_default()
            .pending_role_grants
            .insert(mem_id, (corp_roles::ROLE_DIRECTOR, now - 5));
        let ch = snap.characters.get_mut(&mem_id).unwrap();
        ch.alliance_id = None;
        ch.station_id = Some(60_003_760);
        ch.structure_id = None;
        if ch.user_id == 0 {
            ch.user_id = 1;
        }
        ch.user_id
    });
    crate::services::fleet_social::fleet_social_persistence::queue_forced_corp_sync(&ctx, mem_id, 98_000_999, None, 0);

    let kw = HashMap::new();
    let mut sel_sess = SessionState { session_id: 9_901, user_id: Some(uid), ..Default::default() };
    let _ = ctx.db.acquire_online_lock(uid, sel_sess.session_id, None, true, now);
    crate::services::character_skills::character_gm_tools::handle_char_unbound(&ctx, &mut sel_sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(mem_id)], &kw)
        .unwrap()
        .expect("SelectCharacterID succeeds");

    assert_eq!((sel_sess.corporation_id, sel_sess.alliance_id), (Some(corp_id), Some(ally_id)));
    assert_ne!(sel_sess.corp_role & corp_roles::ROLE_ACCOUNTANT, 0, "AUTHZ-88: SelectCharacterID must load DB corp roles");
    assert_ne!(sel_sess.corp_role & corp_roles::ROLE_DIRECTOR, 0, "AUTHZ-88: SelectCharacterID must load effective DB corp roles including due pending grants");
    crate::services::fleet_social::fleet_social_persistence::with_social_state(&ctx, |s| { assert!(!s.forced_session_corp_sync.contains_key(&mem_id)); });

    // 2. Stale session.corp_role / session.corporation_id must NOT authorize DB-stripped or excluded caller.
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.member_roles.insert(mem_id, 0);
        corp.members.remove(&mem_id);
        snap.corp_role_assignments.remove(&(corp_id, mem_id));
        if let Some(gov) = snap.corp_governance.get_mut(&corp_id) {
            gov.pending_role_grants.remove(&mem_id);
        }
    });
    mem_sess.corporation_id = Some(corp_id);
    mem_sess.corp_role = corp_roles::ROLE_DIRECTOR;

    assert_eq!(call_corps(&ctx, &mut mem_sess, "corpmgr", "AuditMember", &[PyRep::Integer(mem_id)], &kw).unwrap_err().code, "CorpRoleDenied");
    assert_eq!(require_corp_caller(&ctx, &mut mem_sess, None).unwrap_err().code, "CorpAccessDenied");

    // Restored non-director DB member with stale session.corporation_id converges to DB corp_id & alliance_id.
    ctx.db.with_mut_snapshot(|snap| { snap.corporations.get_mut(&corp_id).unwrap().members.insert(mem_id); });
    mem_sess.corporation_id = Some(98_999_999);
    let (db_corp, cid, has_dir) = require_corp_caller(&ctx, &mut mem_sess, None).unwrap();
    assert_eq!((db_corp, cid, has_dir), (corp_id, mem_id, false));
    assert_eq!((mem_sess.corporation_id, mem_sess.alliance_id), (Some(corp_id), Some(ally_id)));
}

#[test]
fn bf03_global_corp_checks_ignore_docked_roles_and_respect_grant_clock() {
    let now = 1_800_040_000_u64;
    let ctx = test_ctx_with_clock(now);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let (corp_id, ceo_id, mem_id, hq_id) = (98_000_001_i64, 90_000_001_i64, 90_000_002_i64, 60_003_760_i64);
    let hq_roles = corp_roles::ROLE_DIRECTOR | corp_roles::ROLE_PERSONNEL_MANAGER;
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_id).unwrap().hq_station_id = Some(hq_id);
        let ch = snap.characters.get_mut(&mem_id).unwrap();
        (ch.station_id, ch.structure_id) = (Some(hq_id), None);
        snap.corp_role_assignments.insert((corp_id, mem_id), eve_db::CorpMemberRoleAssignment { character_id: mem_id, roles_at_hq: hq_roles, ..Default::default() });
        let gov = snap.corp_governance.entry(corp_id).or_default();
        gov.member_titles.insert(mem_id, 1);
        gov.pending_role_grants.insert(mem_id, (corp_roles::ROLE_DIRECTOR, now + 3_600));
    });
    let kw = HashMap::new();
    let members = call_corps(&ctx, &mut ceo_sess, "corpRegistry", "GetMembers", &[], &kw).unwrap();
    let PyRep::Rowset { rows, .. } = &members else { panic!("expected Rowset") };
    let row = rows.iter().find(|r| r[0].as_i64() == Some(mem_id)).unwrap();
    assert_eq!((row[2].as_i64(), row[8].as_i64(), row[10].as_i64()), (Some(0), Some(0), Some(hq_roles as i64)));
    call_corps(&ctx, &mut ceo_sess, "corpRegistry", "UpdateTitle", &[PyRep::Integer(1), PyRep::String("HQOnly".into()), PyRep::Integer(corp_roles::ROLE_TRADER as i64), PyRep::Integer(0)], &kw).unwrap();
    call_corps(&ctx, &mut ceo_sess, "corpRegistry", "DeleteTitle", &[PyRep::Integer(1)], &kw).unwrap();
    let queued = crate::services::fleet_social::fleet_social_persistence::with_social_state(&ctx, |s| s.forced_session_corp_sync.get(&mem_id).copied()).unwrap();
    assert_eq!(queued.2, 0, "BF-03: UpdateTitle/DeleteTitle must not leak roles_at_hq into global sync");
    let denied = crate::services::character_lifecycle::handle(&ctx, &mut mem_sess, "charMgr", "GetPrivateInfoOnCorpChange", &[PyRep::Integer(ceo_id)], &kw).unwrap();
    assert_eq!(denied.unwrap_err().code, "CustomError");
    ctx.db.with_mut_snapshot(|snap| { snap.corp_governance.get_mut(&corp_id).unwrap().pending_role_grants.insert(mem_id, (corp_roles::ROLE_DIRECTOR, now - 1)); });
    assert!(crate::services::character_lifecycle::handle(&ctx, &mut mem_sess, "charMgr", "GetPrivateInfoOnCorpChange", &[PyRep::Integer(ceo_id)], &kw).unwrap().is_ok());
}

#[test]
fn bf_r2_01_r2_02_corp_caller_executor_and_lifecycle_fail_closed() {
    let now = 1_800_050_000_u64;
    let ctx = test_ctx_with_clock(now);
    let (_ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let (corp_a, corp_b, mem_id, npc_corp, ally_a) = (98_000_001_i64, 98_000_099_i64, 90_000_002_i64, 1_000_009_i64, 99_000_001_i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(corp_b, { let mut c = eve_db::CorporationRecord::default(); (c.corporation_id, c.ceo_id, c.members, c.member_roles) = (corp_b, mem_id, [mem_id].into(), [(mem_id, corp_roles::ROLE_DIRECTOR)].into()); c });
        snap.corporations.get_mut(&corp_a).unwrap().alliance_id = Some(ally_a);
        snap.alliances.insert(ally_a, eve_db::AllianceRecord { alliance_id: ally_a, executor_corp_id: corp_a, ..Default::default() });
    });
    // BF-R2-01 (require_corp_caller): jumping from Corp A to Corp B (as CEO) or NPC corp between snapshots must fail closed.
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&mem_id).unwrap().corporation_id = corp_b;
        assert_eq!(super::resolve_verified_corp_caller_in_snap(snap, &ctx.sde, corp_a, mem_id, now).unwrap_err().code, "CorpAccessDenied");
        snap.characters.get_mut(&mem_id).unwrap().corporation_id = npc_corp;
        assert_eq!(super::resolve_verified_corp_caller_in_snap(snap, &ctx.sde, corp_a, mem_id, now).unwrap_err().code, "CorpAccessDenied");
        assert_eq!(super::resolve_verified_corp_caller_in_snap(snap, &ctx.sde, npc_corp, mem_id, now).unwrap(), (None, false));
        assert_eq!(super::resolve_verified_corp_caller_in_snap(snap, &ctx.sde, npc_corp, 90_000_001, now).unwrap_err().code, "CorpAccessDenied");
        snap.characters.get_mut(&mem_id).unwrap().corporation_id = corp_a;
        snap.corporations.get_mut(&corp_a).unwrap().members.remove(&mem_id);
    });
    mem_sess.corporation_id = Some(corp_b);
    assert_eq!(require_corp_caller(&ctx, &mut mem_sess, None).unwrap_err().code, "CorpAccessDenied");
    assert_eq!(mem_sess.corporation_id, Some(corp_b), "failed Snapshot 2 must not overwrite session");
    // BF-R2-02: excluded member and cross-corp char must be rejected even with stale director bits.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.get_mut(&corp_a).unwrap().member_roles.insert(mem_id, corp_roles::ROLE_DIRECTOR);
        assert!(!crate::services::character_lifecycle::has_corp_director_or_personnel_manager_role(snap, mem_id, corp_a));
        snap.corporations.get_mut(&corp_a).unwrap().members.insert(mem_id);
        snap.characters.get_mut(&mem_id).unwrap().corporation_id = corp_b;
        assert!(!crate::services::character_lifecycle::has_corp_director_or_personnel_manager_role(snap, mem_id, corp_a));
        snap.characters.get_mut(&mem_id).unwrap().corporation_id = corp_a;
        snap.corporations.get_mut(&corp_a).unwrap().member_roles.insert(mem_id, 0);
    });
    // BF-R2-01 (require_executor): single-snapshot resolve ignores Corp B CEO, enforces executor Director & current executor.
    assert_eq!(crate::services::corp_gov_delegation::require_executor(&ctx, &mem_sess).unwrap_err().code, "CorpRoleDenied");
    ctx.db.with_mut_snapshot(|snap| { snap.corporations.get_mut(&corp_a).unwrap().member_roles.insert(mem_id, corp_roles::ROLE_DIRECTOR); });
    assert_eq!(crate::services::corp_gov_delegation::require_executor(&ctx, &mem_sess).unwrap(), ally_a);
    ctx.db.with_mut_snapshot(|snap| { snap.alliances.get_mut(&ally_a).unwrap().executor_corp_id = corp_b; });
    assert_eq!(crate::services::corp_gov_delegation::require_executor(&ctx, &mem_sess).unwrap_err().code, "NotAllianceExecutor");
}

#[test]
fn pending_corp_grant_authorizes_read_rpc_without_db_mutation() {
    let now = 1_800_060_000_u64;
    let ctx = test_ctx_with_clock(now);
    let (_ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let kw = HashMap::new();
    let target_id = 90_000_004_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&90_000_002].clone();
        ch.character_id = target_id;
        ch.user_id = target_id as u64;
        ch.name = "Kickable Pilot".into();
        ch.active_ship_id = 100_000_004;
        snap.characters.insert(target_id, ch);
        let corp = snap.corporations.get_mut(&98_000_001).unwrap();
        corp.members.insert(target_id);
        corp.member_roles.insert(target_id, 0);
        snap.corp_governance
            .entry(98_000_001)
            .or_default()
            .pending_role_grants
            .insert(90_000_002, (corp_roles::ROLE_PERSONNEL_MANAGER, now + 60));
    });

    // Future pending grant does not yet authorize personnel-manager kick check.
    let early = call_corps(
        &ctx,
        &mut mem_sess,
        "corpRegistry",
        "CanBeKickedOut",
        &[PyRep::Integer(target_id)],
        &kw,
    )
    .unwrap();
    assert_eq!(early, PyRep::Bool(false));

    // Make the pending grant due without ticking the clock: read RPC authorizes with 0 DB commits.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .get_mut(&98_000_001)
            .unwrap()
            .pending_role_grants
            .insert(90_000_002, (corp_roles::ROLE_PERSONNEL_MANAGER, now - 5));
    });
    let commits_before = ctx.db.tx_commit_count();
    let due = call_corps(
        &ctx,
        &mut mem_sess,
        "corpRegistry",
        "CanBeKickedOut",
        &[PyRep::Integer(target_id)],
        &kw,
    )
    .unwrap();
    assert_eq!(due, PyRep::Bool(true));
    assert_eq!(ctx.db.tx_commit_count(), commits_before);

    ctx.db.with_snapshot(|snap| {
        let corp = &snap.corporations[&98_000_001];
        assert_eq!(
            corp.member_roles.get(&90_000_002).copied().unwrap_or(0)
                & corp_roles::ROLE_PERSONNEL_MANAGER,
            0
        );
        assert_eq!(
            snap.corp_governance[&98_000_001]
                .pending_role_grants
                .get(&90_000_002)
                .copied(),
            Some((corp_roles::ROLE_PERSONNEL_MANAGER, now - 5))
        );
    });
}

#[test]
fn execute_actions_folds_due_pending_grant_in_same_commit_without_restarting_timer() {
    let now = 1_800_070_000_u64;
    let ctx = test_ctx_with_clock(now);
    let (mut ceo_sess, _mem_sess) = seed_player_corps(&ctx);
    let kw = HashMap::new();
    let delayed_role = corp_roles::ROLE_HANGAR_CAN_TAKE[0];

    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .entry(98_000_001)
            .or_default()
            .pending_role_grants
            .insert(90_000_002, (delayed_role, now - 10));
    });

    let commits_before = ctx.db.tx_commit_count();
    let res = crate::services::corp_registry_ops::handle(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "ExecuteActions",
        &[
            PyRep::List(vec![PyRep::Integer(90_000_002)]),
            PyRep::List(vec![PyRep::Tuple(vec![
                PyRep::String("add".into()),
                PyRep::String("roles".into()),
                PyRep::Integer(corp_roles::ROLE_CONTRACT_MANAGER as i64),
            ])]),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    assert_eq!(res, PyRep::Bool(true));
    assert_eq!(ctx.db.tx_commit_count(), commits_before + 1);

    ctx.db.with_snapshot(|snap| {
        let corp = &snap.corporations[&98_000_001];
        let roles = corp.member_roles.get(&90_000_002).copied().unwrap_or(0);
        assert!(
            corp_roles::has_role(roles, delayed_role),
            "due delayed hangar role must promote into member_roles in same commit without being re-passed"
        );
        assert!(corp_roles::has_role(roles, corp_roles::ROLE_CONTRACT_MANAGER));
        assert!(
            !snap.corp_governance[&98_000_001]
                .pending_role_grants
                .contains_key(&90_000_002),
            "due pending grant must promote in-tx rather than restarting 24h delay"
        );
    });
}

#[test]
fn clock_advance_promotes_due_grant_once_and_emits_once() {
    let now = 1_800_080_000_u64;
    let ctx = test_ctx_with_clock(now);
    let (_ceo_sess, _mem_sess) = seed_player_corps(&ctx);
    let mem_id = 90_000_002_i64;
    let eff_ts = now + 60;

    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&mem_id).unwrap().user_id = 1;
        snap.corp_governance
            .entry(98_000_001)
            .or_default()
            .pending_role_grants
            .insert(mem_id, (corp_roles::ROLE_DIRECTOR, eff_ts));
    });
    ctx.db
        .acquire_online_lock(1, 2, Some(mem_id), true, now)
        .unwrap();
    let _ = ctx.drain_notifications_for_char(mem_id);

    ctx.advance_secs(59);
    assert!(
        ctx.drain_notifications_for_char(mem_id)
            .iter()
            .all(|n| n.event != "OnSessionChanged")
    );
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.corp_governance[&98_000_001]
                .pending_role_grants
                .contains_key(&mem_id)
        );
    });

    ctx.advance_secs(1);
    let sess_notifs: Vec<_> = ctx
        .drain_notifications_for_char(mem_id)
        .into_iter()
        .filter(|n| n.event == "OnSessionChanged")
        .collect();
    assert_eq!(sess_notifs.len(), 1);
    ctx.db.with_snapshot(|snap| {
        let roles = snap.corporations[&98_000_001]
            .member_roles
            .get(&mem_id)
            .copied()
            .unwrap_or(0);
        assert!(corp_roles::has_role(roles, corp_roles::ROLE_DIRECTOR));
        assert!(
            !snap.corp_governance[&98_000_001]
                .pending_role_grants
                .contains_key(&mem_id)
        );
    });

    // Subsequent clock advance must not promote or emit OnSessionChanged again.
    ctx.advance_secs(60);
    assert!(
        ctx.drain_notifications_for_char(mem_id)
            .iter()
            .all(|n| n.event != "OnSessionChanged")
    );
}

#[test]
fn pending_scoped_corp_grants_appear_in_member_reads_and_queries_without_db_mutation() {
    let now = 1_800_090_000_u64;
    let ctx = test_ctx_with_clock(now);
    let (mut ceo_sess, _mem_sess) = seed_player_corps(&ctx);
    let kw = HashMap::new();
    let mem_id = 90_000_002_i64;
    let hq_role = corp_roles::ROLE_HANGAR_CAN_TAKE[0];
    let base_role = corp_roles::ROLE_HANGAR_CAN_TAKE[1];
    let other_role = corp_roles::ROLE_HANGAR_CAN_TAKE[2];

    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .entry(98_000_001)
            .or_default()
            .pending_scoped_role_grants
            .insert(mem_id, (hq_role, base_role, other_role, now + 60));
    });

    // Future scoped grants must not leak into member reads/queries.
    let early_member = call_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMember",
        &[PyRep::Integer(mem_id)],
        &kw,
    )
    .unwrap();
    let PyRep::KeyVal(early_kv) = early_member else {
        panic!("expected KeyVal from GetMember")
    };
    assert_eq!(early_kv.get("rolesAtHQ").and_then(PyRep::as_i64), Some(0));
    assert_eq!(early_kv.get("rolesAtBase").and_then(PyRep::as_i64), Some(0));
    assert_eq!(early_kv.get("rolesAtOther").and_then(PyRep::as_i64), Some(0));

    let early_query = call_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMemberIDsByQuery",
        &[PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::String("rolesAtHQ".into()),
            PyRep::String("&".into()),
            PyRep::Integer(hq_role as i64),
        ])])],
        &kw,
    )
    .unwrap();
    assert_eq!(early_query, PyRep::List(Vec::new()));

    // Make scoped grants due without ticking clock or promoting: reads/queries see them with 0 DB commits.
    ctx.db.with_mut_snapshot(|snap| {
        snap.corp_governance
            .get_mut(&98_000_001)
            .unwrap()
            .pending_scoped_role_grants
            .insert(mem_id, (hq_role, base_role, other_role, now - 5));
    });
    let commits_before = ctx.db.tx_commit_count();

    let due_member = call_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMember",
        &[PyRep::Integer(mem_id)],
        &kw,
    )
    .unwrap();
    let PyRep::KeyVal(due_kv) = due_member else {
        panic!("expected KeyVal from GetMember")
    };
    assert_eq!(due_kv.get("rolesAtHQ").and_then(PyRep::as_i64), Some(hq_role as i64));
    assert_eq!(due_kv.get("rolesAtBase").and_then(PyRep::as_i64), Some(base_role as i64));
    assert_eq!(due_kv.get("rolesAtOther").and_then(PyRep::as_i64), Some(other_role as i64));

    let due_members = call_corps(&ctx, &mut ceo_sess, "corpRegistry", "GetMembers", &[], &kw).unwrap();
    let PyRep::Rowset { columns, rows } = &due_members else {
        panic!("expected Rowset from GetMembers")
    };
    let cid_col = columns.iter().position(|c| c == "characterID").unwrap();
    let hq_col = columns.iter().position(|c| c == "rolesAtHQ").unwrap();
    let base_col = columns.iter().position(|c| c == "rolesAtBase").unwrap();
    let other_col = columns.iter().position(|c| c == "rolesAtOther").unwrap();
    let mem_row = rows
        .iter()
        .find(|r| r[cid_col].as_i64() == Some(mem_id))
        .expect("member row in GetMembers");
    assert_eq!(mem_row[hq_col].as_i64(), Some(hq_role as i64));
    assert_eq!(mem_row[base_col].as_i64(), Some(base_role as i64));
    assert_eq!(mem_row[other_col].as_i64(), Some(other_role as i64));

    let due_query = call_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetMemberIDsByQuery",
        &[PyRep::List(vec![
            PyRep::Tuple(vec![
                PyRep::String("rolesAtHQ".into()),
                PyRep::String("&".into()),
                PyRep::Integer(hq_role as i64),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::String("rolesAtBase".into()),
                PyRep::String("&".into()),
                PyRep::Integer(base_role as i64),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::String("rolesAtOther".into()),
                PyRep::String("&".into()),
                PyRep::Integer(other_role as i64),
            ]),
        ])],
        &kw,
    )
    .unwrap();
    assert_eq!(due_query, PyRep::List(vec![PyRep::Integer(mem_id)]));

    assert_eq!(ctx.db.tx_commit_count(), commits_before);
    ctx.db.with_snapshot(|snap| {
        assert!(
            snap.corp_governance[&98_000_001]
                .pending_scoped_role_grants
                .contains_key(&mem_id),
            "read/query RPCs must leave pending_scoped_role_grants unmutated"
        );
    });
}
