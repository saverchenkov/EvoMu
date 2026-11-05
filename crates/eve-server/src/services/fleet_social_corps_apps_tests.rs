//! Unit tests for `fleet_social_corps` applications, invitations, leave/kick checks, and division cash transfers.

use super::super::*;
use super::seed_player_corps;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_with_clock;
use eve_db::IskCents;
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
