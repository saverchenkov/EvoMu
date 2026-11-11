//! Additional branch-coverage and mutation-killing unit tests for `crimewatch_bounty.rs`.

#![forbid(unsafe_code)]

use crate::services::combat_crimewatch::combat_fighters::combat_fighters_tests::make_fighter_ctx_and_session;
use crate::services::combat_crimewatch::*;
use eve_db::{AllianceRecord, CorporationRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};

pub(super) fn call_bounty_rpc(
    ctx: &crate::router::ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_bounty_proxy(ctx, session, service, method, args)
}

pub(super) fn call_sec_tags_rpc(
    ctx: &crate::router::ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    handle_apply_tags_for_sec_status(ctx, session, args)
}

#[test]
fn kill_right_sale_restrictions_expiry_and_queries_kill_mutations() {
    assert_eq!(BountyTargetType::Pilot, BountyTargetType::Pilot);
    assert_ne!(BountyTargetType::Corporation, BountyTargetType::Alliance);
    assert_ne!(BountyTargetType::All, BountyTargetType::Pilot);
    assert_eq!(BountySearchType::Pilot, BountySearchType::Pilot);
    assert_ne!(BountySearchType::Corporation, BountySearchType::Alliance);

    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let other_char_id = 90_000_077_i64;
    let target_char_id = 90_000_078_i64;
    let corp_id = 2_000_077_i64;
    let ally_id = 3_000_077_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let base = snap.characters.get(&char_id).cloned().expect("base char");
        let mut c2 = base.clone();
        c2.character_id = other_char_id;
        c2.name = "OtherPilot".into();
        snap.characters.insert(other_char_id, c2);

        let mut c3 = base;
        c3.character_id = target_char_id;
        c3.name = "SuspectTarget".into();
        snap.characters.insert(target_char_id, c3);

        snap.corporations.insert(
            corp_id,
            { let mut __corp = CorporationRecord::default();
                __corp.corporation_id = corp_id;
                __corp.name = "RestrictionCorp".into();
                __corp.ticker = "RCRP".into();
                __corp.ceo_id = char_id;
                __corp.alliance_id = Some(ally_id);
                __corp },
        );
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "RestrictionAlliance".into(),
                short_name: "RALL".into(),
                executor_corp_id: corp_id,
                member_corp_ids: std::collections::HashSet::from([corp_id]),
            },
        );
    });

    // Unauthenticated session is rejected across endpoints
    let mut unauth = SessionState::default();
    for m in [
        "SellKillRight", "CancelSellKillRight", "GetMyKillRights", "AddToBounty", "GetBounty",
        "GetMyBounties", "SearchBounties", "clear_memoized", "GetTopPilotBounties", "ClearBounty",
    ] {
        let res = call_bounty_rpc(&ctx, &mut unauth, "bountyProxy", m, &[]).unwrap();
        assert!(res.is_err(), "{m} should reject unauthenticated session");
    }
    assert!(call_bounty_rpc(&ctx, &mut session, "petitioner", "GetBounty", &[]).is_none());

    // SellKillRight validation branches
    create_kill_right(&ctx, 1801, char_id, target_char_id, 0.0);
    let err_missing_price =
        call_bounty_rpc(&ctx, &mut session, "killRightMgr", "SellKillRight", &[PyRep::Integer(1801)])
            .unwrap()
            .unwrap_err();
    assert_eq!(err_missing_price.code, "InvalidPrice");

    let err_nan_price =
        call_bounty_rpc(&ctx, &mut session, "killRightMgr", "SellKillRight", &[PyRep::Integer(1801), PyRep::Real(f64::NAN)])
            .unwrap()
            .unwrap_err();
    assert_eq!(err_nan_price.code, "InvalidPrice");

    let err_not_found =
        call_bounty_rpc(&ctx, &mut session, "bountyProxy", "SellKillRight", &[PyRep::Integer(999_999), PyRep::Real(100.0)])
            .unwrap()
            .unwrap_err();
    assert_eq!(err_not_found.code, "KillRightNotFound");

    // Non-owner cannot sell or cancel
    let mut other_sess = session.clone();
    other_sess.character_id = Some(other_char_id);
    let err_not_owner = call_bounty_rpc(
        &ctx,
        &mut other_sess,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(1801), PyRep::Real(100.0)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_not_owner.code, "NotKillRightOwner");

    let err_cancel_not_owner = call_bounty_rpc(
        &ctx,
        &mut other_sess,
        "killRightMgr",
        "CancelSellKillRight",
        &[PyRep::Integer(1801)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_cancel_not_owner.code, "NotKillRightOwner");

    let err_cancel_missing = call_bounty_rpc(
        &ctx,
        &mut session,
        "killRightMgr",
        "CancelSellKillRight",
        &[PyRep::Integer(999_999)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_cancel_missing.code, "KillRightNotFound");

    // Invalid restriction target rejected
    let err_bad_restr = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(1801), PyRep::Real(0.0), PyRep::Integer(888_888_888)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_bad_restr.code, "InvalidKillRightRestrictionTarget");

    // Zero price with valid alliance restriction sets forSale = true
    let ok_zero_restr = call_bounty_rpc(
        &ctx,
        &mut session,
        "killRightMgr",
        "SellKillRight",
        &[PyRep::Integer(1801), PyRep::Real(0.0), PyRep::Integer(ally_id)],
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(list) = ok_zero_restr {
        assert_eq!(list[0].dict_get("forSale"), Some(&PyRep::Bool(true)));
        assert_eq!(list[0].dict_get("restrictedTo"), Some(&PyRep::Integer(ally_id)));
    } else {
        panic!("expected List");
    }

    // Zero price with no restriction sets forSale = false and clears restrictedTo
    let ok_zero_public = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(1801), PyRep::Real(0.0), PyRep::Integer(0)],
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(list) = ok_zero_public {
        assert_eq!(list[0].dict_get("forSale"), Some(&PyRep::Bool(false)));
        assert_eq!(list[0].dict_get("restrictedTo"), Some(&PyRep::None));
    } else {
        panic!("expected List");
    }

    // Active suspect kill right cannot be listed
    ctx.db.with_mut_snapshot(|snap| {
        snap.kill_rights.get_mut(&1801).unwrap().is_active_suspect = true;
    });
    let err_active = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(1801), PyRep::Real(500.0)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_active.code, "KillRightAlreadyActivated");

    // Expired kill right cannot be listed and is filtered out of GetMyKillRights / GetKillRightsOnCharacters
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.kill_rights.get_mut(&1801).unwrap().is_active_suspect = false;
        snap.combat_crimewatch_state.kill_right_expires_unix.insert(1801, now);
    });
    let err_exp = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(1801), PyRep::Real(500.0)],
    )
    .unwrap()
    .unwrap_err();
    assert_eq!(err_exp.code, "KillRightExpired");

    // Create a second non-expired kill right between other_char_id and target_char_id
    create_kill_right(&ctx, 1802, other_char_id, target_char_id, 250_000.0);
    ctx.db.with_mut_snapshot(|snap| {
        snap.combat_crimewatch_state.kill_right_expires_unix.insert(1802, now + 3600);
        snap.combat_crimewatch_state.kill_right_restricted_to.insert(1802, other_char_id);
    });

    let my_empty = call_bounty_rpc(&ctx, &mut session, "bountyProxy", "GetMyKillRights", &[])
        .unwrap()
        .unwrap();
    assert_eq!(my_empty, PyRep::List(vec![]));

    let on_chars = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "GetKillRightsOnCharacters",
        &[PyRep::List(vec![PyRep::Integer(target_char_id)])],
    )
    .unwrap()
    .unwrap();
    if let PyRep::List(list) = on_chars {
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].dict_get("killRightID"), Some(&PyRep::Integer(1802)));
        assert_eq!(list[0].dict_get("restrictedTo"), Some(&PyRep::Integer(other_char_id)));
    } else {
        panic!("expected List");
    }
}

#[test]
fn mutant_kill_bounty_proxy_cancel_floor_restriction_gm_and_sec_tags() {
    let (ctx, mut session, char_id, _ship_id, _sys_id) = make_fighter_ctx_and_session();
    let target_char_id = 90_000_079_i64;
    let ally_id = 3_000_079_i64;

    ctx.db.with_mut_snapshot(|snap| {
        let mut tc = snap.characters[&char_id].clone();
        tc.character_id = target_char_id;
        tc.name = "BountyTarget".into();
        snap.characters.insert(target_char_id, tc);
        snap.characters.get_mut(&char_id).unwrap().balance = eve_db::IskCents::from_f64_isk(500_000.0);
        snap.alliances.insert(
            ally_id,
            AllianceRecord {
                alliance_id: ally_id,
                name: "TestRestrAlly".into(),
                short_name: "TRA".into(),
                executor_corp_id: 2_000_079,
                member_corp_ids: std::collections::HashSet::from([2_000_079]),
            },
        );
    });

    // Unknown method must return None (kills Some(Ok(Default::default())))
    assert!(call_bounty_rpc(&ctx, &mut session, "bountyProxy", "unmatched_method", &[]).is_none());

    // SellKillRight negative restrictedTo (< 0 filtered out) vs positive restrictedTo, then CancelSellKillRight happy path
    create_kill_right(&ctx, 1901, char_id, target_char_id, 0.0);
    let neg_restr = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(1901), PyRep::Real(0.0), PyRep::Integer(-5)],
    )
    .unwrap()
    .unwrap();
    let PyRep::List(neg_rows) = neg_restr else {
        panic!("expected List");
    };
    assert_eq!(neg_rows[0].dict_get("forSale"), Some(&PyRep::Bool(false)));
    assert_eq!(neg_rows[0].dict_get("restrictedTo"), Some(&PyRep::None));

    let sold = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "SellKillRight",
        &[PyRep::Integer(1901), PyRep::Real(50_000.0), PyRep::Integer(ally_id)],
    )
    .unwrap()
    .unwrap();
    let PyRep::List(sold_rows) = sold else {
        panic!("expected List");
    };
    assert_eq!(sold_rows[0].dict_get("forSale"), Some(&PyRep::Bool(true)));
    assert_eq!(sold_rows[0].dict_get("restrictedTo"), Some(&PyRep::Integer(ally_id)));

    let cancelled = call_bounty_rpc(
        &ctx,
        &mut session,
        "killRightMgr",
        "CancelSellKillRight",
        &[PyRep::Integer(1901)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(cancelled.dict_get("killRightID"), Some(&PyRep::Integer(1901)));
    assert_eq!(cancelled.dict_get("forSale"), Some(&PyRep::Bool(false)));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.kill_rights[&1901].activation_fee, eve_db::IskCents(0));
        assert!(!snap.combat_crimewatch_state.kill_right_restricted_to.contains_key(&1901));
    });

    // AddToBounty boundary checks: 99_999.99 < 100k rejected, 100_000.0 accepted, 150_000.0 accepted
    assert_eq!(
        call_bounty_rpc(
            &ctx,
            &mut session,
            "bountyProxy",
            "AddToBounty",
            &[PyRep::Integer(target_char_id), PyRep::Real(99_999.99)],
        )
        .unwrap()
        .unwrap_err()
        .code,
        "BountyAmountTooLow"
    );
    let exact = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "AddToBounty",
        &[PyRep::Integer(target_char_id), PyRep::Real(100_000.0)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(exact.dict_get("totalBountyISK"), Some(&PyRep::Real(100_000.0)));
    let above = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "AddToBounty",
        &[PyRep::Integer(target_char_id), PyRep::Real(150_000.0)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(above.dict_get("totalBountyISK"), Some(&PyRep::Real(250_000.0)));

    // GM bounty admin returns exact keyval with action & amountISK
    session.role = eve_machonet::service_roles::ROLE_GMH;
    let gm_reimb = call_bounty_rpc(
        &ctx,
        &mut session,
        "bountyProxy",
        "GMReimburseBounties",
        &[PyRep::Integer(target_char_id)],
    )
    .unwrap()
    .unwrap();
    assert_eq!(gm_reimb.dict_get("action"), Some(&PyRep::String("reimburse".into())));
    assert_eq!(gm_reimb.dict_get("targetID"), Some(&PyRep::Integer(target_char_id)));
    assert_eq!(gm_reimb.dict_get("amountISK"), Some(&PyRep::Real(250_000.0)));

    // handle_apply_tags_for_sec_status returns real error (kills None & Some(Ok(Default::default())))
    let unauth = SessionState::default();
    let tag_err = call_sec_tags_rpc(&ctx, &unauth, &[]).unwrap().unwrap_err();
    assert!(!tag_err.code.is_empty());
    session.station_id = None;
    let undocked_err = call_sec_tags_rpc(&ctx, &session, &[]).unwrap().unwrap_err();
    assert_eq!(undocked_err.code, "NotDockedInStation");
}
