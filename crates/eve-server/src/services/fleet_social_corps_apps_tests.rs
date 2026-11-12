//! Unit tests for `fleet_social_corps` applications, invitations, leave/kick checks, and division cash transfers.

use super::super::*;
use super::seed_player_corps;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::{
    clone_char_with, test_ctx_with_clock,
};
use eve_db::{IskCents, ServerClock};
use eve_machonet::SessionState;
use std::collections::HashMap;

#[test]
fn corp_applications_invitations_welcome_mail_and_resign_flows() {
    let ctx = test_ctx_with_clock(1_800_011_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let kw = HashMap::new();

    // Configure welcome mail on 98_000_001
    with_corp_gov(&ctx, |g| {
        g.entry(98_000_001).or_default().welcome_mail = (
            true,
            "Welcome to Alpha Holdings".into(),
            "Read our doctrine channel.".into(),
        );
    });

    // Cannot apply to NPC corporation
    let mut app_sess = SessionState::new(10);
    app_sess.user_id = Some(90_000_003);
    app_sess.character_id = Some(90_000_003);
    app_sess.corporation_id = Some(98_000_002);
    app_sess.station_id = Some(60_003_760);
    assert_eq!(
        handle_corps(
            &ctx,
            &mut app_sess,
            "corpRegistry",
            "InsertApplication",
            &[PyRep::Integer(1_000_035)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CannotApplyToNpcCorporation"
    );

    // Character 90_000_003 applies to 98_000_001, then withdraws
    let app1 = handle_corps(
        &ctx,
        &mut app_sess,
        "corpRegistry",
        "InsertApplication",
        &[PyRep::Integer(98_000_001), PyRep::String("Logistics main".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(a1_kv) = app1 else { panic!() };
    let app1_id = a1_kv.get("applicationID").unwrap().as_i64().unwrap();
    assert_eq!(a1_kv.get("status").unwrap().as_str().unwrap(), "Applied");

    let wd = handle_corps(
        &ctx,
        &mut app_sess,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app1_id), PyRep::String("Withdrawn".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(wd_kv) = wd else { panic!() };
    assert_eq!(wd_kv.get("status").unwrap().as_str().unwrap(), "Withdrawn");

    // Second application rejected by CEO, third application accepted by CEO
    let app2 = handle_corps(
        &ctx,
        &mut app_sess,
        "corpmgr",
        "ApplyForMembership",
        &[PyRep::Integer(98_000_001), PyRep::String("Try 2".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(a2_kv) = app2 else { panic!() };
    let app2_id = a2_kv.get("applicationID").unwrap().as_i64().unwrap();
    let rej = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(app2_id), PyRep::String("Rejected".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(rej_kv) = rej else { panic!() };
    assert_eq!(rej_kv.get("status").unwrap().as_str().unwrap(), "Rejected");

    // CEO invites 90_000_003 (accept_now=true rejected, accept_now=false succeeds), then 90_000_003 accepts invitation!
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "InsertInvitation",
            &[PyRep::Integer(90_000_003), PyRep::Bool(true)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    let inv = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "InsertInvitation",
        &[PyRep::Integer(90_000_003), PyRep::Bool(false)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(inv_kv) = inv else { panic!() };
    let inv_id = inv_kv.get("applicationID").unwrap().as_i64().unwrap();

    // CEO cannot accept invitation on invitee's behalf
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(inv_id), PyRep::String("Accepted".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );

    // Invitee 90_000_003 accepts invitation -> moves to corp 98_000_001, session updated, welcome mail created
    let acc = handle_corps(
        &ctx,
        &mut app_sess,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(inv_id), PyRep::String("Accepted".into())],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(acc_kv) = acc else { panic!() };
    assert_eq!(acc_kv.get("status").unwrap().as_str().unwrap(), "Accepted");
    assert_eq!(app_sess.corporation_id, Some(98_000_001));
    ctx.db.with_snapshot(|snap| {
        assert!(snap.corporations.get(&98_000_001).unwrap().members.contains(&90_000_003));
        assert!(!snap.corporations.get(&98_000_002).unwrap().members.contains(&90_000_003));
        assert!(snap.mails.iter().any(|m| m.recipient_id == 90_000_003 && m.subject == "Welcome to Alpha Holdings"));
    });

    // CanLeaveCurrentCorporation (CEO with members -> false; member -> true) & CanBeKickedOut
    let ceo_can = handle_corps(&ctx, &mut ceo_sess, "corpRegistry", "CanLeaveCurrentCorporation", &[], &kw)
        .unwrap()
        .unwrap();
    let PyRep::KeyVal(cc_kv) = ceo_can else { panic!() };
    assert_eq!(cc_kv.get("canLeave").unwrap(), &PyRep::Bool(false));

    let mem_can = handle_corps(&ctx, &mut mem_sess, "corpRegistry", "CanLeaveCurrentCorporation", &[], &kw)
        .unwrap()
        .unwrap();
    let PyRep::KeyVal(mc_kv) = mem_can else { panic!() };
    assert_eq!(mc_kv.get("canLeave").unwrap(), &PyRep::Bool(true));

    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "CanBeKickedOut",
            &[PyRep::Integer(90_000_002)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );
}

#[test]
fn corp_wallet_divisions_transfer_and_error_guards() {
    let ctx = test_ctx_with_clock(1_800_012_000);
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let kw = HashMap::new();

    // Non-accountant / non-director denied GiveCashFromCorpAccount
    assert_eq!(
        handle_corps(
            &ctx,
            &mut mem_sess,
            "account",
            "GiveCashFromCorpAccount",
            &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(100_000.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Same division or non-positive amount rejected
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "account",
            "GiveCashFromCorpAccount",
            &[PyRep::Integer(1), PyRep::Integer(1), PyRep::Real(100_000.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidTransferDivision"
    );
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "account",
            "GiveCashFromCorpAccount",
            &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(0.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidArgument"
    );

    // Valid transfer div 1 -> div 2 (5,000,000 ISK)
    let xfer = handle_corps(
        &ctx,
        &mut ceo_sess,
        "account",
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(5_000_000.0)],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(x_kv) = xfer else { panic!() };
    assert!((x_kv.get("transferredISK").unwrap().as_f64().unwrap() - 5_000_000.0).abs() < 1e-6);

    ctx.db.with_snapshot(|snap| {
        let corp = snap.corporations.get(&98_000_001).unwrap();
        assert_eq!(corp.division_balance(1), IskCents::from_isk(45_000_000));
        assert_eq!(corp.division_balance(2), IskCents::from_isk(5_000_000));
        assert_eq!(
            snap.corp_wallet_journals.get(&(98_000_001, 1)).map(|v| v.len()).unwrap_or(0),
            1
        );
        assert_eq!(
            snap.corp_wallet_journals.get(&(98_000_001, 2)).map(|v| v.len()).unwrap_or(0),
            1
        );
    });
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

#[test]
fn wave2_pending_role_promotion_allow_war_division_aliases_and_account_take_roles() {
    let (ctx, clock) =
        crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock(
            1_800_020_000,
        );
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let kw = HashMap::new();
    mark_online(&ctx, 90_000_001, 1001, 1_800_020_000);
    mark_online(&ctx, 90_000_002, 1002, 1_800_020_000);

    // Schedule delayed pending role grants + scoped roles + friendly fire at +86_400s for 90_000_002
    let eff_ts = 1_800_020_000 + 86_400;
    with_corp_gov(&ctx, |g| {
        let gov = g.entry(98_000_001).or_default();
        gov.pending_role_grants
            .insert(90_000_002, (corp_roles::ROLE_PERSONNEL_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[6], eff_ts));
        gov.pending_scoped_role_grants
            .insert(90_000_002, (1, 2, 4, eff_ts));
        gov.pending_friendly_fire = Some((true, eff_ts));
    });

    // Before eff_ts: member cannot access UpdateCorporation or GiveCashFromCorpAccount(div 7)
    assert_eq!(
        handle_corps(
            &ctx,
            &mut mem_sess,
            "corpRegistry",
            "UpdateCorporation",
            &[PyRep::String("Nope".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    assert_eq!(
        handle_corps(
            &ctx,
            &mut mem_sess,
            "account",
            "GiveCashFromCorpAccount",
            &[PyRep::Integer(7), PyRep::Integer(2), PyRep::Real(1_000.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );

    // Advance 86_399s (still 1s short): still blocked
    clock.advance_secs(86_399);
    let (_, _, has_dir_early) = require_corp_caller(&ctx, &mut mem_sess, Some(98_000_001)).unwrap();
    assert!(!has_dir_early);

    // Advance +1s to exact eff_ts -> promotes roles + scoped roles + friendly_fire!
    clock.advance_secs(1);
    let (_, _, has_dir_due) = require_corp_caller(&ctx, &mut mem_sess, Some(98_000_001)).unwrap();
    assert!(has_dir_due);
    ctx.db.with_snapshot(|snap| {
        let ra = &snap.corp_role_assignments[&(98_000_001, 90_000_002)];
        assert_eq!(ra.roles_at_hq, 1);
        assert_eq!(ra.roles_at_base, 2);
        assert_eq!(ra.roles_at_other, 4);
        assert!(snap.corp_governance[&98_000_001].friendly_fire_legal);
    });

    // Member has ROLE_ACCOUNT_CAN_TAKE[6] (div 7), NOT div 1 or ROLE_DIRECTOR
    assert_eq!(
        handle_corps(
            &ctx,
            &mut mem_sess,
            "account",
            "GiveCashFromCorpAccount",
            &[PyRep::Integer(1), PyRep::Integer(2), PyRep::Real(1_000.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorpRoleDenied"
    );
    // Transfer from div 7 -> div 2 succeeds with div 7 take bit, fails InsufficientDivisionFunds if > 1_000_000 ISK
    assert_eq!(
        handle_corps(
            &ctx,
            &mut mem_sess,
            "account",
            "GiveCashFromCorpAccount",
            &[PyRep::Integer(7), PyRep::Integer(2), PyRep::Real(2_000_000.0)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InsufficientDivisionFunds"
    );
    assert!(handle_corps(
        &ctx,
        &mut mem_sess,
        "account",
        "GiveCashFromCorpAccount",
        &[PyRep::Integer(7), PyRep::Integer(2), PyRep::Real(250_000.0)],
        &kw,
    )
    .unwrap()
    .is_ok());

    // Wave 2 GiveCashFromCorpAccount invalid types / out-of-range division -> InvalidTransferDivision
    for args in [
        vec![PyRep::String("bad".into()), PyRep::Integer(2), PyRep::Real(100.0)],
        vec![PyRep::Integer(1), PyRep::String("bad".into()), PyRep::Real(100.0)],
        vec![PyRep::Integer(0), PyRep::Integer(2), PyRep::Real(100.0)],
        vec![PyRep::Integer(1), PyRep::Integer(8), PyRep::Real(100.0)],
    ] {
        assert_eq!(
            handle_corps(&ctx, &mut ceo_sess, "account", "GiveCashFromCorpAccount", &args, &kw)
                .unwrap()
                .unwrap_err()
                .code,
            "InvalidTransferDivision"
        );
    }

    // Wave 2 UpdateCorporation -> snap.fleet_and_social_ops.corp_allow_war + OnAllowWarUpdated + tax clamp
    let _ = ctx.drain_notifications_for_char(90_000_001);
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let upd = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::Integer(98_000_001),
            PyRep::String("New Desc".into()),
            PyRep::String("https://alph.test".into()),
            PyRep::Real(1.5), // clamped to 1.0
            PyRep::Bool(false),
        ],
        &kw,
    )
    .unwrap()
    .unwrap();
    let PyRep::KeyVal(upd_kv) = upd else { panic!() };
    assert_eq!(upd_kv.get("taxRate").and_then(|v| v.as_f64()), Some(1.0));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.fleet_and_social_ops.corp_allow_war.get(&98_000_001), Some(&false));
    });
    let PyRep::KeyVal(pub_info) = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "GetCorporation",
        &[],
        &kw,
    )
    .unwrap()
    .unwrap() else { panic!() };
    assert_eq!(pub_info.get("allowWar").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(pub_info.get("friendlyFire").and_then(|v| v.as_bool()), Some(true));
    let ceo_notes = ctx.drain_notifications_for_char(90_000_001);
    assert!(ceo_notes.iter().any(|n| n.event == "OnCorporationChanged"));
    assert!(ceo_notes.iter().any(|n| n.event == "OnAllowWarUpdated"));

    // Updating with unchanged allowWar does NOT emit OnAllowWarUpdated again
    let _ = ctx.drain_notifications_for_char(90_000_001);
    assert!(handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateCorporation",
        &[
            PyRep::String("Desc2".into()),
            PyRep::String("https://alph.test".into()),
            PyRep::Real(-0.2), // clamped to 0.0
            PyRep::Bool(false),
        ],
        &kw,
    )
    .unwrap()
    .is_ok());
    let ceo_notes2 = ctx.drain_notifications_for_char(90_000_001);
    assert!(!ceo_notes2.iter().any(|n| n.event == "OnAllowWarUpdated"));

    // UpdateDivisionNames aliases: 115..=121 (116 -> div 2) and 1000..=1006 (1002 -> div 3) and DivisionNotFound (99)
    assert!(handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateDivisionNames",
        &[PyRep::Integer(116), PyRep::String("Loot Hangar".into()), PyRep::String("Loot Wallet".into())],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert!(handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateDivisionNames",
        &[PyRep::Integer(1002), PyRep::String("Ops Hangar".into()), PyRep::String("Ops Wallet".into())],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "UpdateDivisionNames",
            &[PyRep::Integer(99), PyRep::String("Invalid".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "DivisionNotFound"
    );
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.corp_hangar_divisions[&98_000_001][&2], "Loot Hangar");
        assert_eq!(snap.corp_hangar_divisions[&98_000_001][&3], "Ops Hangar");
    });
}

#[test]
fn wave2_application_offer_guards_invitations_leave_kick_and_session_transitions() {
    let (ctx, clock) =
        crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock(
            1_800_030_000,
        );
    let (mut ceo_sess, mut mem_sess) = seed_player_corps(&ctx);
    let kw = HashMap::new();
    mark_online(&ctx, 90_000_001, 2001, 1_800_030_000);
    mark_online(&ctx, 90_000_003, 2003, 1_800_030_000);

    // UpdateApplicationOffer missing arg / unknown app ID
    assert_eq!(
        handle_corps(&ctx, &mut ceo_sess, "corpRegistry", "UpdateApplicationOffer", &[], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(999_888), PyRep::String("Accepted".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ApplicationNotFound"
    );

    // InsertInvitation guards: non-positive target, accept_now=true, unknown target, CorporationFull
    assert_eq!(
        handle_corps(&ctx, &mut ceo_sess, "corpRegistry", "InsertInvitation", &[PyRep::Integer(0)], &kw)
            .unwrap()
            .unwrap_err()
            .code,
        "InvalidArgument"
    );
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "InsertInvitation",
            &[PyRep::Integer(90_000_003), PyRep::Bool(true)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "InsertInvitation",
            &[PyRep::Integer(90_999_999)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CharacterNotFound"
    );
    with_corp_gov(&ctx, |g| {
        g.entry(98_000_001).or_default().member_limit = Some(2);
    });
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "InsertInvitation",
            &[PyRep::Integer(90_000_003)],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CorporationFull"
    );
    with_corp_gov(&ctx, |g| {
        g.get_mut(&98_000_001).unwrap().member_limit = Some(10);
    });

    // Valid invitation created by CEO; CEO cannot Accept an invitation on target's behalf, but CAN Withdraw it
    let PyRep::KeyVal(inv_kv) = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "InsertInvitation",
        &[PyRep::Integer(90_000_003)],
        &kw,
    )
    .unwrap()
    .unwrap() else { panic!() };
    let inv_id = inv_kv.get("applicationID").and_then(|v| v.as_i64()).unwrap();
    assert_eq!(inv_kv.get("joined").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(inv_id), PyRep::String("Accepted".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    let PyRep::KeyVal(w_kv) = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(inv_id), PyRep::String("Withdrawn".into())],
        &kw,
    )
    .unwrap()
    .unwrap() else {
        panic!()
    };
    assert_eq!(
        w_kv.get("status").and_then(|s| s.as_str()),
        Some("Withdrawn")
    );
    // Already closed -> ApplicationAlreadyClosed
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(inv_id), PyRep::String("Accepted".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "ApplicationAlreadyClosed"
    );

    // Second invitation so 90_000_003 can test InvalidApplicationAction, SessionChangeInProgress,
    // MustBeDockedToJoinCorp, CannotLeaveOldCorpWithRoles, and finally successful self-acceptance!
    let PyRep::KeyVal(inv2_kv) = handle_corps(
        &ctx,
        &mut ceo_sess,
        "corpRegistry",
        "InsertInvitation",
        &[PyRep::Integer(90_000_003)],
        &kw,
    )
    .unwrap()
    .unwrap() else { panic!() };
    let inv2_id = inv2_kv.get("applicationID").and_then(|v| v.as_i64()).unwrap();

    let mut target_sess = SessionState::new(903);
    target_sess.user_id = Some(90_000_003);
    target_sess.character_id = Some(90_000_003);
    target_sess.corporation_id = Some(98_000_002);
    target_sess.station_id = Some(60_003_760);

    // Random bystander cannot withdraw target's invitation
    assert_eq!(
        handle_corps(
            &ctx,
            &mut mem_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(inv2_id), PyRep::String("Withdrawn".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "AccessDenied"
    );
    assert_eq!(
        handle_corps(
            &ctx,
            &mut target_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(inv2_id), PyRep::String("InvalidVerb".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "InvalidApplicationAction"
    );

    // Active 10s session-change timer blocks acceptance
    crate::services::space_destiny::record_session_change_timer(
        &ctx,
        90_000_003,
        1_800_030_000,
        30_000_142,
        0,
        "recent_dock",
    );
    assert_eq!(
        handle_corps(
            &ctx,
            &mut target_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(inv2_id), PyRep::String("Accepted".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "SessionChangeInProgress"
    );
    clock.advance_secs(11);

    // Undocked applicant blocked by MustBeDockedToJoinCorp
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_003).unwrap().station_id = None;
    });
    assert_eq!(
        handle_corps(
            &ctx,
            &mut target_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(inv2_id), PyRep::String("Accepted".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "MustBeDockedToJoinCorp"
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_003).unwrap().station_id = Some(60_003_760);
        snap.corporations
            .get_mut(&98_000_002)
            .unwrap()
            .member_roles
            .insert(90_000_003, corp_roles::ROLE_DIRECTOR);
    });
    assert_eq!(
        handle_corps(
            &ctx,
            &mut target_sess,
            "corpRegistry",
            "UpdateApplicationOffer",
            &[PyRep::Integer(inv2_id), PyRep::String("Accepted".into())],
            &kw,
        )
        .unwrap()
        .unwrap_err()
        .code,
        "CannotLeaveOldCorpWithRoles"
    );

    // Clear blocker -> self-acceptance succeeds and transitions target_sess in place
    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&98_000_002)
            .unwrap()
            .member_roles
            .insert(90_000_003, 0);
    });
    with_corp_gov(&ctx, |g| {
        g.get_mut(&98_000_001).unwrap().welcome_mail = (
            true,
            "Invite Welcome".into(),
            "Welcome body".into(),
        );
    });
    let _ = ctx.drain_notifications_for_char(90_000_001);
    let _ = ctx.drain_notifications_for_char(90_000_003);
    assert!(handle_corps(
        &ctx,
        &mut target_sess,
        "corpRegistry",
        "UpdateApplicationOffer",
        &[PyRep::Integer(inv2_id), PyRep::String("Accepted".into())],
        &kw,
    )
    .unwrap()
    .is_ok());
    assert_eq!(target_sess.corporation_id, Some(98_000_001));
    assert_eq!(target_sess.alliance_id, Some(99_000_001));
    assert_eq!(target_sess.corp_role, 0);
    let target_notes = ctx.drain_notifications_for_char(90_000_003);
    assert!(target_notes.iter().any(|n| n.event == "OnSessionChanged"));
    assert!(target_notes.iter().any(|n| n.event == "OnMailSent"));

    // CanBeKickedOut & CanLeaveCurrentCorporation edge cases
    // Undocked member cannot be kicked
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_002).unwrap().station_id = None;
    });
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "CanBeKickedOut",
            &[PyRep::Integer(90_000_002)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(false)
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&90_000_002).unwrap().station_id = Some(60_003_760);
    });
    assert_eq!(
        handle_corps(
            &ctx,
            &mut ceo_sess,
            "corpRegistry",
            "CanBeKickedOut",
            &[PyRep::Integer(90_000_002)],
            &kw,
        )
        .unwrap()
        .unwrap(),
        PyRep::Bool(true)
    );

    // NPC corporation cannot leave
    let mut npc_sess = SessionState::new(904);
    npc_sess.user_id = Some(90_000_099);
    npc_sess.character_id = Some(90_000_099);
    npc_sess.corporation_id = Some(1_000_035);
    let PyRep::KeyVal(npc_kv) =
        handle_corps(&ctx, &mut npc_sess, "corpRegistry", "CanLeaveCurrentCorporation", &[], &kw)
            .unwrap()
            .unwrap()
    else {
        panic!()
    };
    assert_eq!(npc_kv.get("canLeave").and_then(|b| b.as_bool()), Some(false));

    // Solo CEO with active war on alliance cannot leave until war deactivated
    ctx.db.with_mut_snapshot(|snap| {
        let mut c4 = clone_char_with(snap, 90_000_004, "Beta CEO", 98_000_002, 30_000_142, 100_000_004);
        c4.station_id = Some(60_003_760);
        snap.characters.insert(90_000_004, c4);
        let beta = snap.corporations.get_mut(&98_000_002).unwrap();
        beta.members = HashSet::from([90_000_004]);
        beta.alliance_id = Some(99_000_002);
        snap.wars.insert(
            777_001,
            eve_db::WarRecord {
                war_id: 777_001,
                aggressor_corp_id: 98_555_001,
                defender_corp_id: 99_000_002,
                mutual: false,
                started_unix: 1_800_030_000,
                active: true,
            },
        );
    });
    let mut beta_ceo = SessionState::new(905);
    beta_ceo.user_id = Some(90_000_004);
    beta_ceo.character_id = Some(90_000_004);
    beta_ceo.corporation_id = Some(98_000_002);
    let res_war = handle_corps(&ctx, &mut beta_ceo, "corpRegistry", "CanLeaveCurrentCorporation", &[], &kw)
        .unwrap()
        .unwrap();
    let PyRep::KeyVal(rw_kv) = res_war else { panic!() };
    assert_eq!(rw_kv.get("canLeave").and_then(|b| b.as_bool()), Some(false));
    assert_eq!(rw_kv.get("reason").and_then(|s| s.as_str()), Some("Corporation has active wars"));

    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.get_mut(&777_001).unwrap().active = false;
    });
    let PyRep::KeyVal(ok_kv) =
        handle_corps(&ctx, &mut beta_ceo, "corpRegistry", "CanLeaveCurrentCorporation", &[], &kw)
            .unwrap()
            .unwrap()
    else {
        panic!()
    };
    assert_eq!(ok_kv.get("canLeave").and_then(|b| b.as_bool()), Some(true));
}

