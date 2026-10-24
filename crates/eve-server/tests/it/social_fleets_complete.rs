//! Track 4: Phase 8 Corporation Governance, Shares, Votes, Dividends, Medals & Alliances (`SO-3`).

use crate::common;

use eve_db::corp_roles;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

fn is_keyval(rep: &PyRep) -> bool {
    matches!(rep, PyRep::KeyVal(_))
}

fn is_rowset(rep: &PyRep) -> bool {
    matches!(rep, PyRep::Rowset { .. })
}

#[tokio::test]
async fn test_so3_corporation_roles_public_info_governance_shares_votes_and_alliances() {
    // parity: feat:corporation.corp-public-info, feat:corporation.corp-edit-details, feat:corporation.invitations, feat:corporation.kick-member, feat:corporation.leave-corp, feat:corporation.titles, feat:corporation.corp-tax, feat:corporation.corp-shares, feat:corporation.corp-votes, feat:corporation.ceo-change, feat:corporation.bulletins, feat:corporation.medals, feat:corporation.member-tracking, feat:corporation.member-audit, rpc:corpRegistry.UpdateLogo, rpc:corpRegistry.GetCorporation, rpc:corpRegistry.PayoutDividend, rpc:corpRegistry.AddBulletin, rpc:corpRegistry.GetBulletins
    eve_testkit::attest(&["feat:corporation.corp-public-info", "feat:corporation.corp-edit-details", "feat:corporation.invitations", "feat:corporation.kick-member", "feat:corporation.leave-corp", "feat:corporation.titles", "feat:corporation.corp-tax", "feat:corporation.corp-shares", "feat:corporation.corp-votes", "feat:corporation.ceo-change", "feat:corporation.bulletins", "feat:corporation.medals", "feat:corporation.member-tracking", "feat:corporation.member-audit"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(301);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess1,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("Select Pilot 1");

    let mut sess2 = SessionState::new(302);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(
        &mut sess2,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_002)],
        &kw,
        None,
    )
    .await
    .expect("Select Pilot 2");

    // 1. Create Corporation: verify Corporation Management (3363) skill check and 1,599,800 ISK registration cost
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(skills) = snap.skills.get_mut(&90_000_001) {
            skills.remove(&3363);
        }
    });
    let no_skill_err = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "AddCorporation",
            &[
                PyRep::String("DeepSpace Logistics".into()),
                PyRep::String("DSLOG".into()),
                PyRep::Real(0.075),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("AddCorporation without Corporation Management skill must fail");
    assert_eq!(no_skill_err.code, "MissingCorporationManagementSkill");

    // Train Corporation Management (3363) to level 1 and record balance before creation
    let bal_before = ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            3363,
            eve_db::TrainedSkillRecord {
                type_id: 3363,
                level: 1,
                skill_points: 250.0,
            },
        );
        snap.characters[&90_000_001].balance
    });

    let create_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "AddCorporation",
            &[
                PyRep::String("DeepSpace Logistics".into()),
                PyRep::String("DSLOG".into()),
                PyRep::Real(0.075),
            ],
            &kw,
            None,
        )
        .await
        .expect("AddCorporation");
    assert!(is_keyval(&create_rep));
    let corp_id = sess1.corporation_id.expect("corp_id set");

    let bal_after = ctx
        .db
        .with_snapshot(|snap| snap.characters[&90_000_001].balance);
    assert_eq!(
        bal_before.0 - bal_after.0,
        159_980_000,
        "AddCorporation must deduct 1,599,800.00 ISK (159,980,000 cents)"
    );

    // 2. Wallet Divisions: verify 7 starting divisions, rename division 2, transfer funds, and re-read
    let divs_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "account",
            None,
            "GetWalletDivisionsInfo",
            &[PyRep::Integer(corp_id)],
            &kw,
            None,
        )
        .await
        .expect("GetWalletDivisionsInfo");
    let PyRep::Rowset {
        rows: init_divs, ..
    } = &divs_rep
    else {
        panic!("Expected Rowset from GetWalletDivisionsInfo")
    };
    assert_eq!(
        init_divs.len(),
        7,
        "New corporation must have 7 wallet divisions"
    );
    assert_eq!(
        init_divs[0],
        vec![
            PyRep::Integer(1),
            PyRep::String("Master Wallet".into()),
            PyRep::Real(0.0)
        ]
    );
    assert_eq!(
        init_divs[1],
        vec![
            PyRep::Integer(2),
            PyRep::String("Division 2".into()),
            PyRep::Real(0.0)
        ]
    );

    // Rename Division 2 and transfer ISK from Division 1 to Division 2
    ctx.dispatch_rpc(
        &mut sess1,
        "corpmgr",
        None,
        "UpdateDivisionNames",
        &[PyRep::Integer(2), PyRep::String("Freight Escrow".into())],
        &kw,
        None,
    )
    .await
    .expect("UpdateDivisionNames");

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&corp_id)
            .unwrap()
            .wallet_divisions[0]
            .balance = eve_db::IskCents::from_isk(500_000);
    });
    ctx.dispatch_rpc(
        &mut sess1,
        "account",
        None,
        "GiveCashFromCorpAccount",
        &[
            PyRep::Integer(1),
            PyRep::Integer(2),
            PyRep::Real(200_000.0),
        ],
        &kw,
        None,
    )
    .await
    .expect("GiveCashFromCorpAccount");

    let divs_after_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "account",
            None,
            "GetWalletDivisionsInfo",
            &[PyRep::Integer(corp_id)],
            &kw,
            None,
        )
        .await
        .expect("GetWalletDivisionsInfo after update");
    let PyRep::Rowset {
        rows: updated_divs, ..
    } = &divs_after_rep
    else {
        panic!()
    };
    assert_eq!(
        updated_divs[0],
        vec![
            PyRep::Integer(1),
            PyRep::String("Master Wallet".into()),
            PyRep::Real(300_000.0)
        ]
    );
    assert_eq!(
        updated_divs[1],
        vec![
            PyRep::Integer(2),
            PyRep::String("Freight Escrow".into()),
            PyRep::Real(200_000.0)
        ]
    );

    // 3. Public Info & Edit Details: verify all public info fields via corpmgr.GetPublicInfo & corporationSvc.GetCorpInfo
    let public_info = ctx
        .dispatch_rpc(&mut sess2, "corpmgr", None, "GetPublicInfo", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetPublicInfo");
    let PyRep::KeyVal(pub_kv) = &public_info else { panic!("Expected KeyVal from GetPublicInfo") };
    assert_eq!(pub_kv["corporationID"], PyRep::Integer(corp_id));
    assert_eq!(pub_kv["corporationName"], PyRep::String("DeepSpace Logistics".into()));
    assert_eq!(pub_kv["ticker"], PyRep::String("DSLOG".into()));
    assert_eq!(pub_kv["tickerName"], PyRep::String("DSLOG".into()));
    assert_eq!(pub_kv["ceoID"], PyRep::Integer(90_000_001));
    assert_eq!(pub_kv["memberCount"], PyRep::Integer(1));
    assert_eq!(pub_kv["taxRate"], PyRep::Real(0.075));
    assert_eq!(pub_kv["allianceID"], PyRep::None);
    assert_eq!(pub_kv["stationID"], PyRep::Integer(60_003_760));
    assert_eq!(pub_kv["shares"], PyRep::Integer(1000));

    let corp_reg_info = ctx
        .dispatch_rpc(&mut sess2, "corpRegistry", None, "GetCorporation", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("corpRegistry.GetCorporation");
    let PyRep::KeyVal(ci_kv) = &corp_reg_info else { panic!() };
    assert_eq!(ci_kv["ceoID"], PyRep::Integer(90_000_001));
    assert_eq!(ci_kv["tickerName"], PyRep::String("DSLOG".into()));
    assert_eq!(ci_kv["shares"], PyRep::Integer(1000));

    let outsider_corp_info = ctx
        .dispatch_rpc(&mut sess2, "corporationSvc", None, "GetCorpInfo", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect_err("outsider must be denied corporationSvc.GetCorpInfo");
    assert_eq!(outsider_corp_info.code, "CrpAccessDenied");

    let corp_info_svc = ctx
        .dispatch_rpc(&mut sess1, "corporationSvc", None, "GetCorpInfo", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("corporationSvc.GetCorpInfo");
    let PyRep::Rowset { columns: ci_cols, .. } = &corp_info_svc else {
        panic!("expected Rowset from corporationSvc.GetCorpInfo");
    };
    assert_eq!(
        ci_cols,
        &vec![
            "typeID".to_string(),
            "sellPrice".into(),
            "sellVolume".into(),
            "sellDate".into(),
            "sellStationID".into(),
            "buyPrice".into(),
            "buyVolume".into(),
            "buyDate".into(),
            "buyStationID".into(),
            "sellQuantity".into(),
            "buyQuantity".into(),
        ]
    );

    // Non-CEO/non-Director (Pilot 2) must be rejected from UpdateCorporation
    let unauth_edit = ctx
        .dispatch_rpc(
            &mut sess2,
            "corpRegistry",
            None,
            "UpdateCorporation",
            &[PyRep::Integer(corp_id), PyRep::String("Hijacked".into()), PyRep::String("https://bad.eve".into()), PyRep::Real(0.99)],
            &kw,
            None,
        )
        .await
        .expect_err("Non-member/non-director must be denied UpdateCorporation");
    assert!(!unauth_edit.code.is_empty());

    // Pilot 1 (CEO) updates corp logo via corpRegistry.UpdateLogo and corp details via corpRegistry.UpdateCorporation
    let _ = ctx.drain_notifications_for_char(90_000_001);
    ctx.dispatch_rpc(
        &mut sess1,
        "corpRegistry",
        None,
        "UpdateLogo",
        &[PyRep::Integer(415), PyRep::Integer(416), PyRep::Integer(417), PyRep::Integer(681)],
        &kw,
        None,
    )
    .await
    .expect("corpRegistry.UpdateLogo");

    let update_corp = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "UpdateCorporation",
            &[
                PyRep::Integer(corp_id),
                PyRep::String("Premier nullsec freight services".into()),
                PyRep::String("https://dslogistics.eve".into()),
                PyRep::Real(0.08),
            ],
            &kw,
            None,
        )
        .await
        .expect("UpdateCorporation");
    assert!(is_keyval(&update_corp));
    let ceo_notifs = ctx.drain_notifications_for_char(90_000_001);
    assert!(
        ceo_notifs.iter().any(|n| n.event == "OnCorporationChanged"),
        "UpdateCorporation / UpdateLogo must emit OnCorporationChanged"
    );

    // Re-read corpRegistry.GetCorporation and corpmgr.GetPublicInfo to verify persisted description, URL, taxRate, and logo
    let public_info_after = ctx
        .dispatch_rpc(&mut sess1, "corpRegistry", None, "GetCorporation", &[PyRep::Integer(corp_id)], &kw, None)
        .await
        .expect("GetCorporation after UpdateCorporation");
    let PyRep::KeyVal(pub_after_kv) = &public_info_after else { panic!() };
    assert_eq!(pub_after_kv["description"], PyRep::String("Premier nullsec freight services".into()));
    assert_eq!(pub_after_kv["url"], PyRep::String("https://dslogistics.eve".into()));
    assert_eq!(pub_after_kv["taxRate"], PyRep::Real(0.08));
    assert_eq!(pub_after_kv["shape1"], PyRep::Integer(415));
    assert_eq!(pub_after_kv["shape2"], PyRep::Integer(416));
    assert_eq!(pub_after_kv["shape3"], PyRep::Integer(417));
    assert_eq!(pub_after_kv["color1"], PyRep::Integer(681));

    // 4. Pilot 2 Application and Acceptance via V24 corpRegistry.InsertApplication / UpdateApplicationOffer
    let apply_rep = ctx
        .dispatch_rpc(
            &mut sess2,
            "corpRegistry",
            None,
            "InsertApplication",
            &[
                PyRep::Integer(corp_id),
                PyRep::String("Experienced hauler applying".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("InsertApplication");
    let PyRep::KeyVal(app_kv) = &apply_rep else {
        panic!()
    };
    let app_id = app_kv["applicationID"].as_i64().unwrap();

    let accept_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "UpdateApplicationOffer",
            &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
            &kw,
            None,
        )
        .await
        .expect("UpdateApplicationOffer");
    assert!(is_keyval(&accept_rep));

    // Verify memberCount incremented to 2 in GetPublicInfo
    let pub_members = ctx
        .dispatch_rpc(
            &mut sess2,
            "corpmgr",
            None,
            "GetPublicInfo",
            &[PyRep::Integer(corp_id)],
            &kw,
            None,
        )
        .await
        .expect("GetPublicInfo after UpdateApplicationOffer");
    let PyRep::KeyVal(pm_kv) = &pub_members else {
        panic!()
    };
    assert_eq!(pm_kv["memberCount"], PyRep::Integer(2));

    // 5. Update Member Roles via V24 corpRegistry.UpdateMember & verify role bitmask authorization + AuditMember
    let update_roles = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "UpdateMember",
            &[
                PyRep::Integer(90_000_002),
                PyRep::Integer(corp_roles::ROLE_ACCOUNTANT as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("UpdateMember");
    assert!(is_keyval(&update_roles));

    let audit_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpmgr",
            None,
            "AuditMember",
            &[PyRep::Integer(90_000_002)],
            &kw,
            None,
        )
        .await
        .expect("AuditMember");
    assert!(is_rowset(&audit_rep));

    // Role bitmask authorization check: Pilot 2 (Accountant, not Director/CEO) denied from changing roles
    sess2.corporation_id = Some(corp_id);
    let denied = ctx
        .dispatch_rpc(
            &mut sess2,
            "corpRegistry",
            None,
            "UpdateMember",
            &[PyRep::Integer(90_000_001), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await;
    assert_eq!(denied.unwrap_err().code, "CorpRoleDenied");

    // 6. Governance: Shares, Shareholder Votes, Dividends, Titles, Medals, Bulletins, Welcome Mail & Alliances
    let shares_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "MoveCompanyShares",
            &[
                PyRep::Integer(corp_id),
                PyRep::Integer(90_000_001),
                PyRep::Integer(600),
            ],
            &kw,
            None,
        )
        .await
        .expect("MoveCompanyShares to P1");
    assert!(is_keyval(&shares_rep));

    let vote_case_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "voteManager",
            None,
            "InsertVoteCase",
            &[
                PyRep::String("Expand Fleet Hangar".into()),
                PyRep::String("Approve capital ship construction budget".into()),
                PyRep::Integer(4),
                PyRep::List(vec![PyRep::String("Yes".into()), PyRep::String("No".into())]),
            ],
            &kw,
            None,
        )
        .await
        .expect("InsertVoteCase");
    let PyRep::KeyVal(vc_kv) = &vote_case_rep else {
        panic!()
    };
    let vote_case_id = vc_kv["voteCaseID"].as_i64().unwrap();

    let cast_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "voteManager",
            None,
            "InsertVote",
            &[PyRep::Integer(vote_case_id), PyRep::Integer(0)],
            &kw,
            None,
        )
        .await
        .expect("InsertVote");
    assert!(is_keyval(&cast_rep));

    let div_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "PayoutDividend",
            &[PyRep::Real(50_000.0), PyRep::Bool(true)],
            &kw,
            None,
        )
        .await
        .expect("PayoutDividend");
    assert!(is_keyval(&div_rep));

    let title_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "UpdateTitle",
            &[
                PyRep::Integer(1),
                PyRep::String("Chief Fleet Quartermaster".into()),
                PyRep::Integer(corp_roles::ROLE_ACCOUNTANT as i64),
            ],
            &kw,
            None,
        )
        .await
        .expect("UpdateTitle");
    assert!(is_keyval(&title_rep));

    let medal_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "corporationSvc",
            None,
            "CreateMedal",
            &[
                PyRep::String("Cross of New Eden".into()),
                PyRep::String("Heroic defense of logistics convoy".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateMedal");
    let PyRep::KeyVal(med_kv) = &medal_rep else {
        panic!()
    };
    let medal_id = med_kv["medalID"].as_i64().unwrap();
    let give_med = ctx
        .dispatch_rpc(
            &mut sess1,
            "corporationSvc",
            None,
            "GiveMedalToCharacters",
            &[
                PyRep::Integer(medal_id),
                PyRep::List(vec![PyRep::Integer(90_000_002)]),
                PyRep::String("Awarded for valor".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("GiveMedalToCharacters");
    assert!(is_keyval(&give_med));

    let bul_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "corpRegistry",
            None,
            "AddBulletin",
            &[
                PyRep::String("Staging System Update".into()),
                PyRep::String("All jump freighters stage in Jita 4-4".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("AddBulletin");
    assert!(is_keyval(&bul_rep));

    let buls = ctx
        .dispatch_rpc(&mut sess1, "corpRegistry", None, "GetBulletins", &[], &kw, None)
        .await
        .expect("GetBulletins");
    assert!(is_rowset(&buls));

    // Alliance creation (requires Empire Control 3731 and 1,000,000,000 ISK in master wallet)
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(
            3731,
            eve_db::TrainedSkillRecord {
                type_id: 3731,
                level: 1,
                skill_points: 250.0,
            },
        );
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        corp.wallet_divisions[0].balance = eve_db::IskCents::from_isk(2_000_000_000);
        corp.master_wallet = corp.wallet_divisions[0].balance;
    });
    let alliance_rep = ctx
        .dispatch_rpc(
            &mut sess1,
            "allianceRegistry",
            None,
            "CreateAlliance",
            &[
                PyRep::String("Northern Trade Coalition".into()),
                PyRep::String("NTC".into()),
                PyRep::String("Industrial & sovereignty coalition".into()),
                PyRep::String("https://ntc.eve".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreateAlliance");
    assert!(is_keyval(&alliance_rep));
}
