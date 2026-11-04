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
    ctx.advance_secs(12);

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

pub(crate) mod so6_wormholes_polarization_lifetime_mass {
    // Track 4: Wormholes, Fleets, Bookmarks, Scanning & Social Features (`SO-6`, Fleets, Bookmarks, Social).

    use crate::common;

    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use glam::DVec3;
    use std::collections::HashMap;

    fn is_keyval(rep: &PyRep) -> bool {
        matches!(rep, PyRep::KeyVal(_))
    }

    fn is_rowset(rep: &PyRep) -> bool {
        matches!(rep, PyRep::Rowset { .. })
    }

    #[tokio::test]
    async fn test_so6_wormholes_polarization_lifetime_mass_and_thera() {
        // parity: feat:navigation.wormhole-jump, feat:navigation.wormhole-polarization, feat:navigation.wormhole-lifetime, feat:navigation.wormhole-statics, feat:navigation.wormhole-thera, rpc:charUnboundMgr.SelectCharacterID, rpc:scanMgr.GetSystemScanMgr, rpc:structureDeployment.Anchor, rpc:wormholeMgr.GetWormholePolarization, rpc:wormholeMgr.WormholeJump
        eve_testkit::attest(&["feat:navigation.wormhole-jump", "feat:navigation.wormhole-polarization", "feat:navigation.wormhole-lifetime", "feat:navigation.wormhole-statics", "feat:navigation.wormhole-thera"]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();

        let mut sess = SessionState::new(601);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.expect("Select Pilot 1");

        // Seed Jita's test wormhole signature explicitly (production templates seed K-space wormholes probabilistically)
        ctx.db.with_mut_snapshot(|snap| {
            let now_u = ctx.now_unix();
            snap.cosmic_signatures.insert(30_000_142, vec![eve_db::CosmicSignatureRecord {
                signature_id: "JMM-142".into(), solar_system_id: 30_000_142, sig_group: "Wormhole".into(),
                type_name: "Unstable Wormhole (K162)".into(),
                position: [12.0 * 149_597_870_700.0, 2.5 * 149_597_870_700.0, -4.0 * 149_597_870_700.0],
                signal_strength: 0.25, wormhole_type_id: Some(26_272), destination_system_id: Some(31_000_005),
                created_unix: now_u, lifetime_seconds: 86_400, max_mass_kg: 2_000_000_000.0,
                remaining_mass_kg: 2_000_000_000.0, max_jump_mass_kg: 375_000_000.0, collapsed: false,
            }]);
        });
        let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None).await.expect("GetSystemScanMgr");
        let (wh_sig_id, wh_pos) = ctx.db.with_snapshot(|snap| {
            let sig = snap.cosmic_signatures[&30_000_142].iter().find(|s| s.sig_group == "Wormhole").unwrap();
            (sig.signature_id.clone(), sig.position)
        });
        let jita_sys = ctx.systems.get_or_spawn(30_000_142);
        let wh_item_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(30_000_142, &wh_sig_id);
        let (_, wh_ball) = jita_sys.get_ball_snapshot(wh_item_id).await.unwrap().expect("Wormhole ball must be spawned in Jita Ballpark");
        assert_eq!(wh_ball.type_id, 26272);

        let ship_ball = Ball {
            id: 600_000_001, type_id: 587, position: DVec3::new(wh_pos[0] + 500.0, wh_pos[1], wh_pos[2]),
            velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.5, mass: 1_200_000.0, radius: 50.0,
            speed_fraction: 0.0, mode: MovementMode::Stop,
        };
        let _ = jita_sys.spawn_ball(ship_ball.clone()).await;
        (sess.ship_id, sess.solar_system_id, sess.station_id, sess.structure_id) = (Some(600_000_001), Some(30_000_142), None, None);
        ctx.db.with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&90_000_001).unwrap();
            (ch.active_ship_id, ch.station_id, ch.structure_id, ch.solar_system_id) = (600_000_001, None, None, 30_000_142);
        });
        ctx.advance_secs(12);

        let _ = jita_sys.spawn_ball(Ball { position: DVec3::new(wh_pos[0] + 10_000.0, wh_pos[1], wh_pos[2]), ..ship_ball.clone() }).await;
        assert_eq!(ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.expect_err("OOR").code, "OutsideWormholeJumpRange");
        let _ = jita_sys.spawn_ball(Ball { mass: 1_500_000_000.0, ..ship_ball.clone() }).await;
        assert_eq!(ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.expect_err("heavy").code, "ShipTooHeavyForWormhole");

        let _ = jita_sys.spawn_ball(Ball { mass: 1_700_000.0, ..ship_ball.clone() }).await;
        let jump_rep = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.expect("WormholeJump to Thera");
        let PyRep::KeyVal(jkv) = &jump_rep else { panic!("Expected KeyVal from WormholeJump") };
        assert_eq!((sess.solar_system_id, &jkv["isThera"], &jkv["wormholeClass"]), (Some(31_000_005), &PyRep::Bool(true), &PyRep::Integer(12)));
        assert_eq!((&jkv["massStage"], &jkv["wormholeSize"], &jkv["lifetimeStage"], &jkv["wormholeAge"]), (&PyRep::String("Stable".into()), &PyRep::Real(1.0), &PyRep::String("Healthy".into()), &PyRep::Integer(1)));
        assert!((jkv["remainingMassKg"].as_f64().unwrap() - (2_000_000_000.0 - 1_700_000.0)).abs() < 1.0);

        // 3. Thera & Shattered W-space rules (`feat:navigation.wormhole-thera`)
        let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(31_000_005)], &kw, None).await.expect("seed Thera sigs");
        ctx.db.with_mut_snapshot(|snap| {
            for s in snap.cosmic_signatures.get_mut(&31_000_005).into_iter().flatten() {
                s.signal_strength = 1.0;
            }
        });
        let thera_scan = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(31_000_005)], &kw, None).await.expect("GetSystemScanMgr in Thera");
        let PyRep::KeyVal(ts_kv) = &thera_scan else { panic!() };
        assert_eq!(ts_kv["isThera"], PyRep::Bool(true));
        let PyRep::List(thera_sigs) = &ts_kv["signatures"] else { panic!() };
        let thera_wh_count = thera_sigs.iter().filter(|s| s.dict_get("typeID").is_some_and(|v| v != &PyRep::None)).count();
        assert!(thera_wh_count >= 3, "Thera must have multiple static wormhole connections (got {thera_wh_count})");

        let thera_anchor_err = ctx.dispatch_rpc(&mut sess, "structureDeployment", None, "Anchor", &[PyRep::Integer(35832), PyRep::String("Thera Astrahus".into())], &kw, None).await.expect_err("Anchoring structures in Thera must be rejected");
        assert_eq!(thera_anchor_err.code, "AnchoringDisallowedInTheraOrShattered");

        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 31_002_585;
        });
        sess.solar_system_id = Some(31_002_585);
        let shat_scan = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(31_002_585)], &kw, None).await.unwrap();
        assert_eq!(shat_scan.dict_get("isShattered"), Some(&PyRep::Bool(true)));
        let shat_anchor_err = ctx.dispatch_rpc(&mut sess, "structureDeployment", None, "Anchor", &[PyRep::Integer(35832), PyRep::String("Shattered Astrahus".into())], &kw, None).await.unwrap_err();
        assert_eq!(shat_anchor_err.code, "AnchoringDisallowedInTheraOrShattered");

        // 4. Polarization Check (`feat:navigation.wormhole-polarization`) via wormholeMgr.GetWormholePolarization
        let pol_rep = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "GetWormholePolarization", &[PyRep::Integer(31_000_005)], &kw, None).await.expect("GetWormholePolarization");
        let PyRep::Tuple(pol_tuple) = pol_rep else { panic!("Expected 2-tuple (polarizationEndTime, polarizationDuration) from GetWormholePolarization, got {pol_rep:?}") };
        assert_eq!(pol_tuple.len(), 2);
        assert!(pol_tuple[0].as_i64().unwrap() > 0);
        assert_eq!(pol_tuple[1], PyRep::Integer(300 * 10_000_000));

        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 30_000_142;
        });
        let _ = jita_sys.spawn_ball(ship_ball.clone()).await;
        sess.solar_system_id = Some(30_000_142);
        ctx.advance_secs(12);
        let pol_err = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.expect_err("Polarized pilot cannot jump same wormhole");
        assert_eq!(pol_err.code, "WormholePolarized");

        ctx.advance_secs(301);
        let pol_after = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "GetWormholePolarization", &[PyRep::Integer(31_000_005)], &kw, None).await.unwrap();
        assert_eq!(pol_after, PyRep::None);

        // 5. Intermediate mass & lifetime stages (`feat:navigation.wormhole-lifetime`)
        ctx.db.with_mut_snapshot(|snap| {
            let wh = snap.cosmic_signatures.get_mut(&30_000_142).unwrap().iter_mut().find(|s| s.signature_id == wh_sig_id).unwrap();
            wh.remaining_mass_kg = 800_000_000.0;
            wh.destination_system_id = Some(31_000_028);
        });
        let destab_jump = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.expect("Jump through destabilized wormhole into Wolf-Rayet W-space");
        let PyRep::KeyVal(dkv) = &destab_jump else { panic!() };
        assert_eq!(dkv["massStage"], PyRep::String("Destabilized".into()));
        assert_eq!(dkv["wormholeSize"], PyRep::Real(0.5));
        assert_eq!(dkv["environmentalEffect"], PyRep::String("Wolf-Rayet".into()));

        let base_rifter_cs = ctx.build_ship_combat_state(90_000_001, 600_000_001, 587);
        let wr_handle = ctx.systems.get_or_spawn(31_000_028);
        let wr_cs = wr_handle.get_combat_state(600_000_001).await.unwrap().unwrap();
        assert!((wr_cs.max_armor_hp - base_rifter_cs.max_armor_hp * 1.30).abs() < 1.0);
        assert!((wr_cs.signature_radius - base_rifter_cs.signature_radius * 0.85).abs() < 1.0);

        // 6. Mass exhaustion collapse & automatic static wormhole respawn (`feat:navigation.wormhole-statics`)
        ctx.advance_secs(301);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 30_000_142;
            if let Some(sigs) = snap.cosmic_signatures.get_mut(&30_000_142) {
                if let Some(wh) = sigs.iter_mut().find(|s| s.signature_id == wh_sig_id) {
                    wh.remaining_mass_kg = 800_000.0;
                    wh.destination_system_id = Some(31_001_886);
                }
            }
        });
        sess.solar_system_id = Some(30_000_142);
        let _ = jita_sys.spawn_ball(ship_ball.clone()).await;

        let collapse_jump = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(wh_item_id)], &kw, None).await.expect("WormholeJump collapsing mass");
        let PyRep::KeyVal(ckv) = &collapse_jump else { panic!() };
        assert_eq!(ckv["collapsed"], PyRep::Bool(true));
        assert_eq!(ckv["massStage"], PyRep::String("Collapsed_Exhausted".into()));
        assert_eq!(ckv["wormholeSize"], PyRep::Real(0.0));
        assert_eq!(ckv["environmentalEffect"], PyRep::String("Magnetar".into()));
        let replacement_sig_id = ckv["replacementSignatureID"].as_str().expect("Collapsed static wormhole must spawn a replacement static signature").to_string();
        assert_ne!(replacement_sig_id, wh_sig_id);
        assert!(jita_sys.get_ball_snapshot(wh_item_id).await.unwrap().is_none(), "Collapsed wormhole ball must be removed from Ballpark");

        let _ = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(31_001_886)], &kw, None).await.unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            let now_u = ctx.now_unix();
            let list = snap.cosmic_signatures.entry(31_001_886).or_default();
            if !list.iter().any(|s| s.signature_id.starts_with("WND-")) {
                list.push(eve_db::CosmicSignatureRecord {
                    signature_id: "WND-501".into(),
                    solar_system_id: 31_001_886,
                    sig_group: "Wormhole".into(),
                    type_name: "Wandering Wormhole".into(),
                    position: [-8.0 * 149_597_870_700.0, 5.0 * 149_597_870_700.0, 3.0 * 149_597_870_700.0],
                    signal_strength: 1.0,
                    wormhole_type_id: Some(30_698),
                    destination_system_id: Some(31_000_007),
                    created_unix: now_u,
                    lifetime_seconds: 57_600,
                    max_mass_kg: 2_000_000_000.0,
                    remaining_mass_kg: 2_000_000_000.0,
                    max_jump_mass_kg: 375_000_000.0,
                    collapsed: false,
                });
            }
            for s in list.iter_mut() {
                s.signal_strength = 1.0;
            }
            for s in snap.cosmic_signatures.get_mut(&30_000_142).into_iter().flatten() {
                if s.signature_id == replacement_sig_id {
                    s.signal_strength = 1.0;
                }
            }
        });
        let c5_scan = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(31_001_886)], &kw, None).await.unwrap();
        let Some(PyRep::List(c5_sigs)) = c5_scan.dict_get("signatures") else { panic!() };
        assert!(c5_sigs.iter().any(|s| s.dict_get("signatureID").and_then(|v| v.as_str()).is_some_and(|id| id.starts_with("WST-")) && s.dict_get("typeID").and_then(|v| v.as_i64()).is_some_and(|tid| (30698..=30703).contains(&tid))));
        assert!(c5_sigs.iter().any(|s| s.dict_get("signatureID").and_then(|v| v.as_str()).is_some_and(|id| id.starts_with("WND-")) && s.dict_get("typeID").and_then(|v| v.as_i64()).is_some_and(|tid| (30698..=30706).contains(&tid))));

        // 7. Natural lifetime progression & natural expiration collapse
        let rep_wh_item_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(30_000_142, &replacement_sig_id);
        let rep_pos = ctx.db.with_snapshot(|snap| {
            snap.cosmic_signatures[&30_000_142].iter().find(|s| s.signature_id == replacement_sig_id).unwrap().position
        });
        ctx.advance_secs(75_000);
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 30_000_142;
        });
        sess.solar_system_id = Some(30_000_142);
        let jita_scan_eol = ctx.dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[PyRep::Integer(30_000_142)], &kw, None).await.unwrap();
        let Some(PyRep::List(jita_sigs_eol)) = jita_scan_eol.dict_get("signatures") else { panic!() };
        let rep_sig_eol = jita_sigs_eol.iter().find(|s| s.dict_get("signatureID") == Some(&PyRep::String(replacement_sig_id.clone()))).expect("Replacement wormhole in EOL stage");
        assert_eq!(rep_sig_eol.dict_get("lifetimeStage"), Some(&PyRep::String("ReachingEndOfLifetime".into())));
        assert_eq!(rep_sig_eol.dict_get("wormholeAge"), Some(&PyRep::Integer(2)));

        ctx.advance_secs(20_000);
        let _ = jita_sys.spawn_ball(Ball { position: DVec3::new(rep_pos[0] + 200.0, rep_pos[1], rep_pos[2]), ..ship_ball }).await;

        let expired_err = ctx.dispatch_rpc(&mut sess, "wormholeMgr", None, "WormholeJump", &[PyRep::Integer(rep_wh_item_id)], &kw, None).await.expect_err("Naturally expired wormhole must collapse on jump attempt");
        assert_eq!(expired_err.code, "WormholeCollapsed");
    }

    #[tokio::test]
    async fn test_so_fleets_motd_options_wings_squads_broadcasts_watchlist_formations_boost_bridge_disband() {
        // parity: feat:fleet.wings-squads, feat:fleet.roles-positions, feat:fleet.fleet-motd, feat:fleet.fleet-broadcasts, feat:fleet.fleet-warp-cmd, feat:fleet.fleet-composition, feat:fleet.fleet-watchlist, feat:fleet.fleet-advert, feat:fleet.fleet-free-move, feat:fleet.fleet-formations, feat:fleet.fleet-disband, feat:fleet.fleet-max-size, feat:fleet.fleet-boost-scope, feat:fleet.fleet-bridge, feat:fleet.fleet-registration, rpc:beyonce.GetFormations, rpc:charUnboundMgr.SelectCharacterID, rpc:fleetMgr.AddToWatchlist, rpc:fleetMgr.RegisterForDamageUpdates, rpc:fleetMgr.RemoveFromWatchlist, rpc:fleetObjectHandler.AcceptInvite, rpc:fleetObjectHandler.ChangeSquadName, rpc:fleetObjectHandler.ChangeWingName, rpc:fleetObjectHandler.CreateSquad, rpc:fleetObjectHandler.CreateWing, rpc:fleetObjectHandler.DeleteSquad, rpc:fleetObjectHandler.DeleteWing, rpc:fleetObjectHandler.DisbandFleet, rpc:fleetObjectHandler.GetFleetComposition, rpc:fleetObjectHandler.GetFleetMaxSize, rpc:fleetObjectHandler.GetInitState, rpc:fleetObjectHandler.GetMotd, rpc:fleetObjectHandler.MoveMember, rpc:fleetObjectHandler.SetFleetMaxSize, rpc:fleetObjectHandler.SetMotdEx, rpc:fleetObjectHandler.SetOptions, rpc:fleetObjectHandler.SetTakesFleetWarp, rpc:fleetProxy.AddFleetFinderAdvert, rpc:fleetProxy.ApplyToJoinFleet, rpc:fleetProxy.GetAvailableFleetAds, rpc:fleetProxy.UpdateAdvertAllowedEntities
        eve_testkit::attest(&["feat:fleet.wings-squads", "feat:fleet.roles-positions", "feat:fleet.fleet-motd", "feat:fleet.fleet-broadcasts", "feat:fleet.fleet-warp-cmd", "feat:fleet.fleet-composition", "feat:fleet.fleet-watchlist", "feat:fleet.fleet-advert", "feat:fleet.fleet-free-move", "feat:fleet.fleet-formations", "feat:fleet.fleet-disband", "feat:fleet.fleet-max-size", "feat:fleet.fleet-boost-scope", "feat:fleet.fleet-bridge", "feat:fleet.fleet-registration"]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();

        let mut sess1 = SessionState::new(801);
        common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();

        let mut sess2 = SessionState::new(802);
        common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
        ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();

        let p3_uid = ctx.db.provision_player_account("fleet_pilot3", "fp3-secret");
        let p3_char = ctx.db.create_character(p3_uid, "Fleet Pilot Three", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;
        let mut sess3 = SessionState::new(803);
        common::login_pilot(&ctx, &mut sess3, "fleet_pilot3", "fp3-secret").await;
        ctx.dispatch_rpc(&mut sess3, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(p3_char)], &kw, None).await.unwrap();

        // 1. Fleet MOTD, Free-Move, Registration, Max Size, Wings/Squads, Roles/Positions, Broadcasts, Watchlist, Formations, Boost Scope, Bridge & Disband
        let create_fl = ctx.dispatch_rpc(&mut sess1, "fleetMgr", None, "CreateFleet", &[PyRep::String("Apex Strike".into()), PyRep::String("Initial MOTD".into())], &kw, None).await.unwrap();
        assert!(is_keyval(&create_fl));
        let fid = sess1.fleet_id.unwrap();

        let motd_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "SetMotdEx", &[PyRep::String("Anchor on FC, prop mod on".into())], &kw, None).await.unwrap();
        assert!(is_keyval(&motd_rep));
        let got_motd = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetMotd", &[], &kw, None).await.unwrap();
        assert_eq!(got_motd, PyRep::String("Anchor on FC, prop mod on".into()));

        let opt_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "SetOptions", &[PyRep::Bool(true), PyRep::Bool(true)], &kw, None).await.unwrap();
        let PyRep::KeyVal(opt_kv) = &opt_rep else { panic!() };
        assert_eq!(opt_kv["isFreeMove"], PyRep::Bool(true));
        assert_eq!(opt_kv["isRegistered"], PyRep::Bool(true));

        // Max size cap enforcement (`feat:fleet.fleet-max-size`): cap at 1 blocks Invite with FleetFull, then raise to 50
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "SetFleetMaxSize", &[PyRep::Integer(1)], &kw, None).await.unwrap();
        let full_err = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(90_000_002)], &kw, None).await.expect_err("Invite beyond maxSize must fail");
        assert_eq!(full_err.code, "FleetFull");
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "SetFleetMaxSize", &[PyRep::Integer(50)], &kw, None).await.unwrap();
        assert_eq!(ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetFleetMaxSize", &[], &kw, None).await.unwrap(), PyRep::Integer(50));

        // Wings & Squads (`feat:fleet.wings-squads`): CreateWing, ChangeWingName, CreateSquad, ChangeSquadName, DeleteSquad, DeleteWing
        let w2 = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateWing", &[PyRep::String("Wing 2".into())], &kw, None).await.unwrap();
        let wing2_id = w2.dict_get("wingID").and_then(|v| v.as_i64()).unwrap();
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "ChangeWingName", &[PyRep::Integer(wing2_id), PyRep::String("Recon Wing".into())], &kw, None).await.unwrap();
        let s2 = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateSquad", &[PyRep::Integer(wing2_id), PyRep::String("Squad 2".into())], &kw, None).await.unwrap();
        let squad2_id = s2.dict_get("squadID").and_then(|v| v.as_i64()).unwrap();
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "ChangeSquadName", &[PyRep::Integer(squad2_id), PyRep::String("CovOps Squad".into())], &kw, None).await.unwrap();
        let w3 = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateWing", &[PyRep::String("Temp Wing".into())], &kw, None).await.unwrap();
        let wing3_id = w3.dict_get("wingID").and_then(|v| v.as_i64()).unwrap();
        let s3 = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "CreateSquad", &[PyRep::Integer(wing3_id), PyRep::String("Temp Squad".into())], &kw, None).await.unwrap();
        let squad3_id = s3.dict_get("squadID").and_then(|v| v.as_i64()).unwrap();
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "DeleteSquad", &[PyRep::Integer(squad3_id)], &kw, None).await.unwrap();
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "DeleteWing", &[PyRep::Integer(wing3_id)], &kw, None).await.unwrap();

        // Invite & AcceptInvite (`feat:fleet.fleet-motd`): joining member receives OnFleetMotdChanged on join and on subsequent SetMotdEx
        let _ = ctx.drain_notifications_for_char(90_000_002);
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "Invite", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
        ctx.advance_secs(12);
        ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "AcceptInvite", &[PyRep::Integer(fid)], &kw, None).await.unwrap();
        let join_notifs = ctx.drain_notifications_for_char(90_000_002);
        assert!(join_notifs.iter().any(|n| n.event == "OnFleetMotdChanged"), "Joining member must receive OnFleetMotdChanged");
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "SetMotdEx", &[PyRep::String("Updated MOTD Live".into())], &kw, None).await.unwrap();
        assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnFleetMotdChanged"), "Members must receive OnFleetMotdChanged on change");

        // Fleet Advert & Auto-Join (`feat:fleet.fleet-advert`, `feat:fleet.fleet-free-move`): AddFleetFinderAdvert, UpdateAdvertAllowedEntities, GetAvailableFleetAds, ApplyToJoinFleet
        ctx.dispatch_rpc(&mut sess1, "fleetProxy", None, "AddFleetFinderAdvert", &[PyRep::String("Apex Public".into()), PyRep::String("Autojoin".into()), PyRep::Integer(3), PyRep::Bool(true)], &kw, None).await.unwrap();
        ctx.dispatch_rpc(&mut sess1, "fleetProxy", None, "UpdateAdvertAllowedEntities", &[PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();
        let denied_ads = ctx.dispatch_rpc(&mut sess3, "fleetProxy", None, "GetAvailableFleetAds", &[], &kw, None).await.unwrap();
        if let PyRep::Rowset { rows, .. } = &denied_ads { assert!(rows.is_empty(), "Unlisted entity must not see restricted advert"); }
        assert_eq!(ctx.dispatch_rpc(&mut sess3, "fleetProxy", None, "ApplyToJoinFleet", &[PyRep::Integer(fid)], &kw, None).await.unwrap_err().code, "FleetJoinDenied");
        ctx.dispatch_rpc(&mut sess1, "fleetProxy", None, "UpdateAdvertAllowedEntities", &[PyRep::List(vec![PyRep::Integer(p3_char)])], &kw, None).await.unwrap();
        let p3_join = ctx.dispatch_rpc(&mut sess3, "fleetProxy", None, "ApplyToJoinFleet", &[PyRep::Integer(fid)], &kw, None).await.unwrap();
        assert_eq!(p3_join.dict_get("joined"), Some(&PyRep::Bool(true)));
        sess3.fleet_id = Some(fid);

        // Roles & Positions (`feat:fleet.roles-positions`, `feat:fleet.fleet-free-move`, `feat:fleet.fleet-registration`)
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "MoveMember", &[PyRep::Integer(90_000_002), PyRep::Integer(wing2_id), PyRep::Integer(0), PyRep::Integer(2)], &kw, None).await.unwrap();
        // Wing 2 Commander (Pilot 2) cannot kick or move Pilot 3 (who is in Wing 1)
        assert_eq!(ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "KickMember", &[PyRep::Integer(p3_char)], &kw, None).await.unwrap_err().code, "FleetPermissionDenied");
        assert_eq!(ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "MoveMember", &[PyRep::Integer(p3_char), PyRep::Integer(wing2_id), PyRep::Integer(squad2_id), PyRep::Integer(4)], &kw, None).await.unwrap_err().code, "FleetPermissionDenied");
        let occ_err = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "MoveMember", &[PyRep::Integer(p3_char), PyRep::Integer(wing2_id), PyRep::Integer(0), PyRep::Integer(2)], &kw, None).await.unwrap_err();
        assert_eq!(occ_err.code, "PositionOccupied");
        ctx.advance_secs(12);
        let fm_move = ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "MoveMember", &[PyRep::Integer(90_000_002), PyRep::Integer(wing2_id), PyRep::Integer(squad2_id), PyRep::Integer(4)], &kw, None).await.unwrap();
        assert_eq!(fm_move.dict_get("squadID"), Some(&PyRep::Integer(squad2_id)));
        let init_st = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetInitState", &[], &kw, None).await.unwrap();
        assert_eq!(init_st.dict_get("isRegistered"), Some(&PyRep::Bool(true)));
        assert_eq!(init_st.dict_get("isFreeMove"), Some(&PyRep::Bool(true)));

        // Fleet Composition (`feat:fleet.fleet-composition`): SquadMember denied with FleetRoleDenied; FC receives composition rowset
        assert_eq!(ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "GetFleetComposition", &[], &kw, None).await.unwrap_err().code, "FleetRoleDenied");
        assert!(is_rowset(&ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "GetFleetComposition", &[], &kw, None).await.unwrap()));

        // Spawn ships in Jita (30000142) for Pilot 1 (at origin) and Pilot 2 (1,000m away), and Pilot 3 in Perimeter (30000144)
        let jita_sys = ctx.systems.get_or_spawn(30_000_142);
        let perim_sys = ctx.systems.get_or_spawn(30_000_144);
        let _ = jita_sys.spawn_ball(Ball { id: 800_000_001, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
        let _ = jita_sys.spawn_ball(Ball { id: 800_000_002, type_id: 587, position: DVec3::new(1_000.0, 0.0, 0.0), velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
        let _ = perim_sys.spawn_ball(Ball { id: 800_000_003, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
        sess1.ship_id = Some(800_000_001); sess1.station_id = None; sess1.solar_system_id = Some(30_000_142);
        sess2.ship_id = Some(800_000_002); sess2.station_id = None; sess2.solar_system_id = Some(30_000_142);
        sess3.ship_id = Some(800_000_003); sess3.station_id = None; sess3.solar_system_id = Some(30_000_144);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.active_ship_id = 800_000_001; ch.station_id = None; ch.solar_system_id = 30_000_142; }
            if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.active_ship_id = 800_000_002; ch.station_id = None; ch.solar_system_id = 30_000_142; }
            if let Some(ch) = snap.characters.get_mut(&p3_char) { ch.active_ship_id = 800_000_003; ch.station_id = None; ch.solar_system_id = 30_000_144; }
        });

        // Fleet Broadcast Scopes (`feat:fleet.fleet-broadcasts`)
        let _ = ctx.drain_notifications_for_char(90_000_002);
        let _ = ctx.drain_notifications_for_char(p3_char);
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "BroadcastToSystem", &[PyRep::String("InPosition".into()), PyRep::Integer(800_000_001)], &kw, None).await.unwrap();
        assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnFleetBroadcast"));
        assert!(!ctx.drain_notifications_for_char(p3_char).iter().any(|n| n.event == "OnFleetBroadcast"), "Out-of-system member must not receive BroadcastToSystem");
        ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "BroadcastToBubble", &[PyRep::String("HealArmor".into()), PyRep::Integer(800_000_001)], &kw, None).await.unwrap();
        assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnFleetBroadcast"));

        // Fleet Watchlist (`feat:fleet.fleet-watchlist`)
        let mut p2_cs = eve_destiny::CombatState::default();
        p2_cs.shield_hp = 250.0; p2_cs.max_shield_hp = 500.0; p2_cs.armor_hp = 300.0; p2_cs.max_armor_hp = 400.0; p2_cs.structure_hp = 350.0; p2_cs.max_structure_hp = 350.0;
        let _ = jita_sys.set_combat_state(800_000_002, p2_cs).await;
        let _ = ctx.drain_notifications_for_char(90_000_001);
        let wl_rep = ctx.dispatch_rpc(&mut sess1, "fleetMgr", None, "AddToWatchlist", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
        assert_eq!(wl_rep.dict_get("shieldRatio"), Some(&PyRep::Real(0.5)));
        assert_eq!(wl_rep.dict_get("armorRatio"), Some(&PyRep::Real(0.75)));
        assert!(ctx.drain_notifications_for_char(90_000_001).iter().any(|n| n.event == "OnFleetStateChange"));
        ctx.dispatch_rpc(&mut sess1, "fleetMgr", None, "RegisterForDamageUpdates", &[PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();
        ctx.dispatch_rpc(&mut sess1, "fleetMgr", None, "RemoveFromWatchlist", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();

        // Fleet Formations & Fleet Warp (`feat:fleet.fleet-formations`, `feat:fleet.fleet-warp-cmd`)
        let formations = ctx.dispatch_rpc(&mut sess1, "beyonce", None, "GetFormations", &[], &kw, None).await.unwrap();
        let PyRep::Tuple(form_items) = &formations else { panic!("beyonce.GetFormations must return a 2-tuple of formations") };
        assert_eq!(form_items.len(), 2);
        let fw_dest = DVec3::new(300_000_000.0, 0.0, 0.0);
        let fw_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "FleetWarp", &[PyRep::Real(fw_dest.x), PyRep::Real(fw_dest.y), PyRep::Real(fw_dest.z), PyRep::Real(0.0), PyRep::Integer(1), PyRep::Real(5_000.0)], &kw, None).await.unwrap();
        assert_eq!(fw_rep.dict_get("warpedMembers"), Some(&PyRep::Integer(2)));
        let _ = jita_sys.set_ball_position(800_000_001, DVec3::ZERO).await;
        let _ = jita_sys.set_ball_position(800_000_002, DVec3::new(1_000.0, 0.0, 0.0)).await;
        ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "SetTakesFleetWarp", &[PyRep::Bool(false)], &kw, None).await.unwrap();
        let fw_exempt = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "FleetWarp", &[PyRep::Real(fw_dest.x), PyRep::Real(fw_dest.y), PyRep::Real(fw_dest.z), PyRep::Real(0.0)], &kw, None).await.unwrap();
        assert_eq!(fw_exempt.dict_get("warpedMembers"), Some(&PyRep::Integer(1)), "Exempt member must not be warped by FleetWarp");

        // Fleet Boost Scope (`feat:fleet.fleet-boost-scope`)
        let _ = jita_sys.set_ball_position(800_000_001, DVec3::ZERO).await;
        let _ = jita_sys.set_ball_position(800_000_002, DVec3::new(2_000.0, 0.0, 0.0)).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(880_101, eve_db::InventoryItemRecord { item_id: 880_101, type_id: 43552, owner_id: 90_000_001, location_id: 800_000_001, flag: eve_db::InventoryFlag::HiSlot0, quantity: 1, singleton: true, version: 1, ..Default::default() });
            snap.items.insert(880_102, eve_db::InventoryItemRecord { item_id: 880_102, type_id: 42836, owner_id: 90_000_001, location_id: 800_000_001, flag: eve_db::InventoryFlag::HiSlot0, quantity: 100, singleton: false, version: 1, ..Default::default() });
            snap.module_online_states.insert(880_101, true);
        });
        ctx.invalidate_ship_fitting(800_000_001);
        let burst_rep = ctx.dispatch_rpc(&mut sess1, "dogmaIM", None, "Activate", &[PyRep::Integer(880_101)], &kw, None).await.unwrap();
        let affected = burst_rep.dict_get("affectedMemberIDs").and_then(|v| match v { PyRep::List(l) => Some(l.clone()), _ => None }).unwrap_or_default();
        assert!(affected.contains(&PyRep::Integer(90_000_002)), "Fleet member in range in another wing/squad must receive command burst");
        assert!(!affected.contains(&PyRep::Integer(p3_char)), "Fleet member in another system must not receive command burst");

        // Fleet Bridge (`feat:fleet.fleet-bridge`)
        let bridge_err = ctx.dispatch_rpc(&mut sess2, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(p3_char), PyRep::Integer(800_000_003)], &kw, None).await.unwrap_err();
        assert_eq!(bridge_err.code, "NoActiveBridge");

        // Disband fleet via `DisbandFleet` (`feat:fleet.fleet-disband`)
        let disband_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "DisbandFleet", &[], &kw, None).await.unwrap();
        assert_eq!(disband_rep.dict_get("disbanded"), Some(&PyRep::Bool(true)));
        assert_eq!(sess1.fleet_id, None);
        assert!(ctx.db.with_snapshot(|s| !s.chat_channels.contains_key(&format!("fleetid:{fid}"))), "DisbandFleet must destroy fleet chat channel");

        sess2.fleet_id = None;
        let non_fleet_err = ctx.dispatch_rpc(&mut sess2, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(p3_char), PyRep::Integer(800_000_003)], &kw, None).await.unwrap_err();
        assert_eq!(non_fleet_err.code, "FleetNotInFleet");
    }

    #[tokio::test]
    async fn test_so_bookmarks_expiry_copy_move_corp_bookmarks_and_scanning_features() {
        // parity: feat:bookmarks.copy-move, feat:bookmarks.warp-to-bm, feat:bookmarks.bm-expiry, feat:bookmarks.corp-bookmarks, feat:scanning.probe-formations, feat:scanning.signatures, feat:scanning.anomalies, feat:scanning.structure-signals, feat:scanning.system-scan, feat:scanning.sig-despawn, feat:scanning.scan-skills, rpc:accessGroupBookmarkMgr.AddFolder, rpc:accessGroupBookmarkMgr.CopyBookmarksToFolderAndSubfolder, rpc:accessGroupBookmarkMgr.MoveBookmarksToFolderAndSubfolder, rpc:charUnboundMgr.SelectCharacterID, rpc:corpRegistry.AddCorporation, rpc:corpRegistry.InsertApplication, rpc:corpRegistry.UpdateApplicationOffer, rpc:scanMgr.SignalTrackerRegister
        eve_testkit::attest(&["feat:bookmarks.copy-move", "feat:bookmarks.warp-to-bm", "feat:bookmarks.bm-expiry", "feat:bookmarks.corp-bookmarks", "feat:scanning.probe-formations", "feat:scanning.signatures", "feat:scanning.anomalies", "feat:scanning.structure-signals", "feat:scanning.system-scan", "feat:scanning.sig-despawn", "feat:scanning.scan-skills"]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let mut sess1 = SessionState::new(811);
        common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();
        let mut sess2 = SessionState::new(812);
        common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
        ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
        let jita_sys = ctx.systems.get_or_spawn(30_000_142);
        let _ = jita_sys.spawn_ball(Ball { id: 800_000_001, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
        sess1.ship_id = Some(800_000_001); sess1.station_id = None; sess1.solar_system_id = Some(30_000_142);
        sess2.ship_id = Some(800_000_002); sess2.station_id = Some(60_003_760); sess2.solar_system_id = Some(30_000_142);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.active_ship_id = 800_000_001; ch.station_id = None; ch.solar_system_id = 30_000_142; }
            if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.active_ship_id = 800_000_002; ch.station_id = Some(60_003_760); ch.solar_system_id = 30_000_142; }
            snap.skills.entry(90_000_001).or_default().insert(3363, eve_db::TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 });
        });
        ctx.advance_secs(12);
        ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "AddCorporation", &[PyRep::String("Bookmark Corp".into()), PyRep::String("BMCP".into()), PyRep::Real(0.05)], &kw, None).await.unwrap();
        ctx.advance_secs(7 * 86_400);
        let corp_id = sess1.corporation_id.unwrap();
        let app = ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Joining".into())], &kw, None).await.unwrap();
        ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateApplicationOffer", &[app.dict_get("applicationID").unwrap().clone(), PyRep::String("Accepted".into())], &kw, None).await.unwrap();
        sess2.corporation_id = Some(corp_id);
        sess2.station_id = None;
        ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.station_id = None; } });

        let shared_f = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String("Corp Shared".into()), PyRep::Bool(true)], &kw, None).await.unwrap();
        let shared_folder_id = shared_f.dict_get("folderID").and_then(|v| v.as_i64()).unwrap();

        // Create temporary bookmark with 3h expiry (`expiry_hours = 3`) in shared corp folder (`feat:bookmarks.corp-bookmarks`, `feat:bookmarks.bm-expiry`)
        let bm_target_pos = DVec3::new(500_000_000.0, 0.0, 0.0);
        let _ = jita_sys.set_ball_position(800_000_001, bm_target_pos).await;
        let _ = ctx.drain_notifications_for_char(90_000_002);
        let temp_bm = ctx.dispatch_rpc(&mut sess1, "bookmark", None, "BookmarkLocation", &[PyRep::String("Temp Safe".into()), PyRep::Integer(800_000_001), PyRep::String("3h safe".into()), PyRep::Integer(shared_folder_id), PyRep::None, PyRep::Integer(3)], &kw, None).await.unwrap();
        let temp_bm_id = temp_bm.dict_get("bookmarkID").and_then(|v| v.as_i64()).unwrap();
        assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnSharedBookmarksFolderUpdated"), "Corp member must receive OnSharedBookmarksFolderUpdated on shared bookmark creation");

        // Copy and Move bookmarks across folders (`feat:bookmarks.copy-move`) including >20 batch limit check
        let p_folder = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String("Personal Tactical".into())], &kw, None).await.unwrap();
        let p_folder_id = p_folder.dict_get("folderID").and_then(|v| v.as_i64()).unwrap();
        let batch21: Vec<PyRep> = (0..21).map(|_| PyRep::Integer(temp_bm_id)).collect();
        assert_eq!(ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "CopyBookmarksToFolderAndSubfolder", &[PyRep::Integer(p_folder_id), PyRep::None, PyRep::List(batch21), PyRep::None], &kw, None).await.unwrap_err().code, "BookmarkCopyBatchLimitExceeded");
        let copied = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "CopyBookmarksToFolderAndSubfolder", &[PyRep::Integer(p_folder_id), PyRep::None, PyRep::List(vec![PyRep::Integer(temp_bm_id)]), PyRep::None], &kw, None).await.unwrap();
        let copied_bm_id = match &copied {
            PyRep::Tuple(parts) => match &parts[0] {
                PyRep::Dict(entries) => entries.first().and_then(|(k, _)| k.as_i64()).unwrap(),
                _ => panic!("expected dict in CopyBookmarksToFolderAndSubfolder[0]"),
            },
            PyRep::KeyVal(_) => match copied.dict_get("bookmarkIDs") {
                Some(PyRep::List(l)) => l[0].as_i64().unwrap(),
                _ => panic!(),
            },
            _ => panic!("unexpected CopyBookmarksToFolderAndSubfolder shape"),
        };
        // While copied_bm_id is in Pilot 1's personal folder, Pilot 2 cannot warp to it
        let _ = jita_sys.spawn_ball(Ball { id: 800_000_002, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
        // rpc-lint: allow error code check
        assert_eq!(ctx.dispatch_rpc(&mut sess2, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(copied_bm_id), PyRep::Real(0.0)], &kw, None).await.unwrap_err().code, "BookmarkNotFound");

        ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "MoveBookmarksToFolderAndSubfolder", &[PyRep::Integer(shared_folder_id), PyRep::None, PyRep::List(vec![PyRep::Integer(copied_bm_id)]), PyRep::None], &kw, None).await.unwrap();

        // Warp to Bookmark (`feat:bookmarks.warp-to-bm`): Pilot 2 warps to the shared corp bookmark (`copied_bm_id`) and lands at `bm_target_pos`
        ctx.dispatch_rpc(&mut sess2, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(copied_bm_id), PyRep::Real(0.0)], &kw, None).await.unwrap();
        for _ in 0..30 {
            let _ = jita_sys.step_manual(1.0).await;
        }
        let (_, b2_bm_landed) = jita_sys.get_ball_snapshot(800_000_002).await.unwrap().unwrap();
        assert!(b2_bm_landed.position.distance(bm_target_pos) < 2_500.0, "CmdWarpToStuff('bookmark') must land at bookmark coordinates");

        // Advance virtual clock 4 hours -> temporary bookmark (3h expiry) is pruned from `GetBookmarks` and warping to it fails with `BookmarkExpired` (`feat:bookmarks.bm-expiry`)
        ctx.advance_secs(4 * 3600);
        let bms_after_exp = ctx.dispatch_rpc(&mut sess1, "bookmark", None, "GetBookmarks", &[], &kw, None).await.unwrap();
        let PyRep::Rowset { rows: bm_rows, .. } = &bms_after_exp else { panic!() };
        assert!(!bm_rows.iter().any(|r| r[0] == PyRep::Integer(temp_bm_id)), "Expired 3h bookmark must be filtered out");
        let warp_stuff_kind = "bookmark";
        assert_eq!(ctx.dispatch_rpc(&mut sess1, "beyonce", None, "CmdWarpToStuff", &[PyRep::String(warp_stuff_kind.into()), PyRep::Integer(temp_bm_id), PyRep::Real(0.0)], &kw, None).await.unwrap_err().code, "BookmarkExpired");

        // 3. Scanning: Anomalies, Structure Signals, System Scan, Probe Formations, Scan Skills & Signature Despawn
        let scan_sys_id = 30_000_144u32;
        let scan_sys = ctx.systems.get_or_spawn(scan_sys_id);
        sess1.solar_system_id = Some(scan_sys_id);
        let _ = scan_sys.spawn_ball(Ball { id: 800_000_001, type_id: 587, position: DVec3::new(0.0, 5.0e9, 0.0), velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
        let _ = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "GetSignatures", &[PyRep::Integer(i64::from(scan_sys_id))], &kw, None).await.unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.solar_system_id = scan_sys_id; }
            snap.structures.insert(990_000_777, eve_db::UpwellStructureRecord {
                structure_id: 990_000_777, type_id: 35832, owner_corp_id: corp_id, solar_system_id: scan_sys_id,
                name: "Perimeter Trade Citadel".into(), state: "ShieldVulnerable".into(), position: [100_000.0, 0.0, 0.0],
                quantum_core_installed: true, fuel_blocks: 1_000, shield_hp: 5_000.0, armor_hp: 5_000.0, hull_hp: 5_000.0,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
            snap.items.insert(880_201, eve_db::InventoryItemRecord {
                item_id: 880_201, type_id: 35_832, owner_id: 90_000_001, location_id: 800_000_001,
                flag: eve_db::InventoryFlag::Cargo, quantity: 1, singleton: false, version: 1, ..Default::default()
            });
        });
        let _ = ctx.drain_notifications_for_char(90_000_001);
        let tracker_rep = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "SignalTrackerRegister", &[PyRep::Integer(i64::from(scan_sys_id))], &kw, None).await.unwrap();
        let PyRep::KeyVal(tr_kv) = &tracker_rep else { panic!() };
        let PyRep::Rowset { rows: struct_rows, .. } = &tr_kv["structures"] else { panic!() };
        assert!(struct_rows.iter().any(|r| r[0] == PyRep::Integer(990_000_777)), "SignalTrackerRegister must return structure signals");
        assert!(!ctx.drain_notifications_for_char(90_000_001).iter().any(|n| matches!(n.event.as_str(), "OnSignalTrackerSignatureUpdate" | "OnSignalTrackerAnomalyUpdate" | "OnSignalTrackerStructureUpdate")), "SignalTrackerRegister must not emit empty initial spam");

        // Trigger real anomaly, structure, and expired-signature deltas (`feat:scanning.anomalies`, `feat:scanning.structure-signals`, `feat:scanning.sig-despawn`)
        eve_server::services::pve_exploration_seeding::ensure_system_pve_sites_seeded(&ctx, scan_sys_id).await;
        let anchor_rep = ctx.dispatch_rpc(&mut sess1, "structureDeployment", None, "Anchor", &[PyRep::Integer(35_832), PyRep::String("Perimeter Scanner Citadel".into())], &kw, None).await.unwrap();
        let anchored_id = anchor_rep.dict_get("structureID").and_then(|v| v.as_i64()).unwrap();
        let expired_sig_id = ctx.db.with_mut_snapshot(|snap| {
            let relic = snap.cosmic_signatures.get_mut(&scan_sys_id).unwrap().iter_mut().find(|s| s.sig_group != "Wormhole").unwrap();
            relic.created_unix = 1;
            relic.lifetime_seconds = 1;
            relic.signature_id.clone()
        });
        ctx.advance_secs(10);
        let sigs_after = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "GetSignatures", &[PyRep::Integer(i64::from(scan_sys_id))], &kw, None).await.unwrap();
        if let PyRep::Rowset { rows: s_rows, .. } = &sigs_after {
            assert!(!s_rows.iter().any(|r| r[0] == PyRep::String(expired_sig_id.clone())), "Expired signature must despawn from GetSignatures");
        }
        let tr_notifs = ctx.drain_notifications_for_char(90_000_001);
        assert!(tr_notifs.iter().any(|n| n.event == "OnSignalTrackerSignatureUpdate" && n.args.len() == 3 && n.args[0] == PyRep::Integer(i64::from(scan_sys_id)) && matches!(&n.args[2], PyRep::List(rem) if rem.contains(&PyRep::String(expired_sig_id.clone())))));
        assert!(tr_notifs.iter().any(|n| n.event == "OnSignalTrackerAnomalyUpdate" && n.args.len() == 3 && n.args[0] == PyRep::Integer(i64::from(scan_sys_id)) && matches!(&n.args[1], PyRep::Dict(d) if !d.is_empty()) && n.args[2] == PyRep::List(vec![])));
        assert!(tr_notifs.iter().any(|n| n.event == "OnSignalTrackerStructureUpdate" && n.args.len() == 3 && n.args[0] == PyRep::Integer(i64::from(scan_sys_id)) && matches!(&n.args[1], PyRep::Dict(d) if d.iter().any(|(k, _)| k == &PyRep::Integer(anchored_id))) && n.args[2] == PyRep::List(vec![])));

        // Probe Formations & Scan Skills (`feat:scanning.probe-formations`, `feat:scanning.scan-skills`)
        ctx.db.with_mut_snapshot(|snap| {
            let sk = snap.skills.entry(90_000_001).or_default();
            sk.insert(3412, eve_db::TrainedSkillRecord { type_id: 3412, level: 2, skill_points: 1_415.0 });
            sk.insert(25810, eve_db::TrainedSkillRecord { type_id: 25810, level: 4, skill_points: 90_510.0 });
            sk.insert(25811, eve_db::TrainedSkillRecord { type_id: 25811, level: 4, skill_points: 90_510.0 });
            sk.insert(25812, eve_db::TrainedSkillRecord { type_id: 25812, level: 4, skill_points: 90_510.0 });
        });
        assert_eq!(ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "LaunchProbes", &[PyRep::Integer(8), PyRep::Integer(0)], &kw, None).await.unwrap_err().code, "MaxProbesExceeded");
        let lp_spread = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "LaunchProbes", &[PyRep::Integer(6), PyRep::Integer(1), PyRep::List(vec![PyRep::Real(1_000.0), PyRep::Real(2_000.0), PyRep::Real(3_000.0)])], &kw, None).await.unwrap();
        assert_eq!(lp_spread.dict_get("formationName"), Some(&PyRep::String("Spread".into())));
        assert_eq!(lp_spread.dict_get("maxProbesAllowed"), Some(&PyRep::Integer(6)));
        let lp_custom = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "LaunchProbes", &[PyRep::Integer(2), PyRep::Integer(2), PyRep::List(vec![PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0)]), PyRep::List(vec![PyRep::List(vec![PyRep::Real(100.0), PyRep::Real(0.0), PyRep::Real(0.0)]), PyRep::List(vec![PyRep::Real(-100.0), PyRep::Real(0.0), PyRep::Real(0.0)])])], &kw, None).await.unwrap();
        assert_eq!(lp_custom.dict_get("formationName"), Some(&PyRep::String("Custom".into())));
    }

    #[tokio::test]
    async fn test_so_social_corp_mail_mailing_lists_cspa_calendar_and_chat_features() {
        // parity: feat:social.mail-corp-alliance, feat:social.mailing-lists, feat:social.mailing-list-admin, feat:social.mailing-list-welcome, feat:social.cspa, feat:social.calendar-personal, feat:social.calendar-corp-alliance, feat:social.calendar-responses, feat:social.calendar-ccp-events, feat:social.calendar-auto-events, feat:social.chat-help-rookie, feat:social.chat-gm-mute, feat:social.chat-motd, rpc:XmppChatMgr.GMMute, rpc:XmppChatMgr.GMUnmute, rpc:XmppChatMgr.GetHelpChannel, rpc:XmppChatMgr.GetRookieChannel, rpc:XmppChatMgr.SetMotd, rpc:calendarMgr.CreateCorporationEvent, rpc:calendarMgr.CreatePersonalEvent, rpc:calendarMgr.DeleteEvent, rpc:calendarMgr.EditCorporationEvent, rpc:calendarMgr.EditPersonalEvent, rpc:calendarMgr.GetResponsesForCharacter, rpc:calendarMgr.GetResponsesToEvent, rpc:calendarMgr.SendEventResponse, rpc:charMgr.GetSettingsInfo, rpc:charMgr.LogSettings, rpc:charUnboundMgr.SelectCharacterID, rpc:corpRegistry.AddCorporation, rpc:corpRegistry.InsertApplication, rpc:corpRegistry.UpdateApplicationOffer, rpc:mailingListsMgr.ClearEntityAccess, rpc:mailingListsMgr.ClearWelcomeMail, rpc:mailingListsMgr.Create, rpc:mailingListsMgr.GetJoinedLists, rpc:mailingListsMgr.GetMembers, rpc:mailingListsMgr.GetWelcomeMail, rpc:mailingListsMgr.Join, rpc:mailingListsMgr.KickMembers, rpc:mailingListsMgr.SaveWelcomeMail, rpc:mailingListsMgr.SetMembersMuted, rpc:mailingListsMgr.SetMembersOperator
        eve_testkit::attest(&["feat:social.mail-corp-alliance", "feat:social.mailing-lists", "feat:social.mailing-list-admin", "feat:social.mailing-list-welcome", "feat:social.cspa", "feat:social.calendar-personal", "feat:social.calendar-corp-alliance", "feat:social.calendar-responses", "feat:social.calendar-ccp-events", "feat:social.calendar-auto-events", "feat:social.chat-help-rookie", "feat:social.chat-gm-mute", "feat:social.chat-motd"]);
        let ctx = ServerContext::new();
        crate::common::pin_clock(&ctx);
        let kw = HashMap::new();
        let mut sess1 = SessionState::new(821);
        common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();
        let mut sess2 = SessionState::new(822);
        common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
        ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
        let p3_uid = ctx.db.provision_player_account("social_pilot3", "sp3-secret");
        let p3_char = ctx.db.create_character(p3_uid, "Social Pilot Three", 1, 1, 11, 17, &ctx.sde).unwrap().character_id;
        let mut sess3 = SessionState::new(823);
        common::login_pilot(&ctx, &mut sess3, "social_pilot3", "sp3-secret").await;
        ctx.dispatch_rpc(&mut sess3, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(p3_char)], &kw, None).await.unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(90_000_001).or_default().insert(3363, eve_db::TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 });
        });
        ctx.advance_secs(12);
        ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "AddCorporation", &[PyRep::String("Social Corp".into()), PyRep::String("SOCP".into()), PyRep::Real(0.05)], &kw, None).await.unwrap();
        let corp_id = sess1.corporation_id.unwrap();
        let app = ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Joining".into())], &kw, None).await.unwrap();
        ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateApplicationOffer", &[app.dict_get("applicationID").unwrap().clone(), PyRep::String("Accepted".into())], &kw, None).await.unwrap();
        sess2.corporation_id = Some(corp_id);
        sess2.station_id = None;
        ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.station_id = None; } });

        // 4. Social: Corp/Alliance Mail, Mailing List Admin & Welcome Mail, CSPA, Calendar Personal/Corp/CCP/Auto Events, Chat Mute, Rookie Help & MOTD
        let _ = ctx.drain_notifications_for_char(90_000_002);
        let corp_mail_id = ctx.dispatch_rpc(&mut sess1, "mailMgr", None, "SendMail", &[PyRep::List(vec![]), PyRep::Integer(0), PyRep::Integer(corp_id), PyRep::String("Corp CTA".into()), PyRep::String("Form up in Jita".into())], &kw, None).await.unwrap();
        assert!(corp_mail_id.as_i64().is_some());
        assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnMailSent"), "Corp mail must reach all corp members");

        // CSPA (`feat:social.cspa`)
        ctx.dispatch_rpc(&mut sess3, "charMgr", None, "LogSettings", &[PyRep::Real(50_000.0)], &kw, None).await.unwrap();
        assert_eq!(ctx.dispatch_rpc(&mut sess3, "charMgr", None, "GetSettingsInfo", &[], &kw, None).await.unwrap(), eve_server::services::client_login::settings_info_blob());
        ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&90_000_002).unwrap().balance = eve_db::IskCents::from_isk(100));
        assert_eq!(ctx.dispatch_rpc(&mut sess2, "charMgr", None, "CSPAChargedAction", &[PyRep::Integer(p3_char)], &kw, None).await.unwrap_err().code, "InsufficientFundsForCSPA");
        assert_eq!(ctx.dispatch_rpc(&mut sess2, "mailMgr", None, "SendMail", &[PyRep::Integer(p3_char), PyRep::String("Hi".into()), PyRep::String("Body".into())], &kw, None).await.unwrap_err().code, "InsufficientFundsForCSPA");
        ctx.db.with_mut_snapshot(|snap| snap.characters.get_mut(&90_000_002).unwrap().balance = eve_db::IskCents::from_isk(100_000));
        ctx.dispatch_rpc(&mut sess2, "mailMgr", None, "SendMail", &[PyRep::Integer(p3_char), PyRep::String("Paid CSPA".into()), PyRep::String("Body".into())], &kw, None).await.unwrap();
        assert_eq!(ctx.db.with_snapshot(|s| s.characters[&90_000_002].balance.to_wire_f64()), 50_000.0);

        // Mailing Lists, Admin & Welcome Mail (`feat:social.mailing-lists`, `feat:social.mailing-list-admin`, `feat:social.mailing-list-welcome`)
        let ml_rep = ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "Create", &[PyRep::String("IntelList".into())], &kw, None).await.unwrap();
        let list_id = ml_rep.as_i64().or_else(|| ml_rep.dict_get("listID").and_then(|v| v.as_i64())).unwrap();
        ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "SaveWelcomeMail", &[PyRep::Integer(list_id), PyRep::String("Welcome to IntelList".into()), PyRep::String("Rules inside".into())], &kw, None).await.unwrap();
        let wm_rep = ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "GetWelcomeMail", &[PyRep::Integer(list_id)], &kw, None).await.unwrap();
        let PyRep::List(ref wm_list) = wm_rep else { panic!("expected List from GetWelcomeMail") };
        assert_eq!(wm_list[0].dict_get("title"), Some(&PyRep::String("Welcome to IntelList".into())));
        let _ = ctx.drain_notifications_for_char(90_000_002);
        ctx.dispatch_rpc(&mut sess2, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &kw, None).await.unwrap();
        assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnMailSent"), "Joining mailing list must deliver welcome mail");
        assert_eq!(ctx.dispatch_rpc(&mut sess2, "mailingListsMgr", None, "KickMembers", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(90_000_001)])], &kw, None).await.unwrap_err().code, "UserError");
        ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "SetMembersOperator", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();
        ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "SetMembersMuted", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();
        ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "SetEntityAccess", &[PyRep::Integer(list_id), PyRep::Integer(90_000_002), PyRep::Integer(2)], &kw, None).await.unwrap();
        ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "ClearEntityAccess", &[PyRep::Integer(list_id), PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
        ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "ClearWelcomeMail", &[PyRep::Integer(list_id)], &kw, None).await.unwrap();
        assert!(matches!(ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "GetJoinedLists", &[], &kw, None).await.unwrap(), PyRep::Dict(_)));
        assert!(matches!(ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "GetMembers", &[PyRep::Integer(list_id)], &kw, None).await.unwrap(), PyRep::Dict(_)));

        // Calendar: Personal Event, role-gated Corporation Event, Responses, CCP Event & Automated Event (`feat:social.calendar-*`)
        let pers_ev = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreatePersonalEvent", &[PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(60), PyRep::String("Solo Roam".into()), PyRep::String("Bring nanogang".into()), PyRep::Integer(0), PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();
        let pers_ev_id = pers_ev.dict_get("eventID").and_then(|v| v.as_i64()).unwrap();
        ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditPersonalEvent", &[PyRep::Integer(pers_ev_id), PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(90), PyRep::String("Updated Solo Roam".into()), PyRep::String("Updated desc".into())], &kw, None).await.unwrap();
        ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "SendEventResponse", &[PyRep::Integer(pers_ev_id), PyRep::Integer(90_000_001), PyRep::Integer(4)], &kw, None).await.unwrap();
        assert!(is_rowset(&ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "GetResponsesToEvent", &[PyRep::Integer(pers_ev_id)], &kw, None).await.unwrap()));
        assert!(is_rowset(&ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "GetResponsesForCharacter", &[], &kw, None).await.unwrap()));
        assert_eq!(ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "CreateCorporationEvent", &[PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(120), PyRep::String("Unauth".into()), PyRep::String("No role".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err().code, "CalendarAccessDenied");
        ctx.db.with_mut_snapshot(|snap| {
            snap.corporations.get_mut(&corp_id).unwrap().member_roles.insert(90_000_002, 1u64 << 55);
        });
        let corp_ev = ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "CreateCorporationEvent", &[PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(120), PyRep::String("Corp Moon Mining".into()), PyRep::String("Bring barges".into()), PyRep::Integer(1)], &kw, None).await.unwrap();
        let corp_ev_id = corp_ev.dict_get("eventID").and_then(|v| v.as_i64()).unwrap();
        ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "EditCorporationEvent", &[PyRep::Integer(corp_ev_id), PyRep::Integer(135_500_000_000_000_000), PyRep::Integer(120), PyRep::String("Updated Corp Moon Mining".into()), PyRep::String("Updated text".into())], &kw, None).await.unwrap();
        ctx.db.with_mut_snapshot(|snap| {
            snap.structures.insert(990_000_888, eve_db::UpwellStructureRecord {
                structure_id: 990_000_888, type_id: 35835, name: "Social Corp Refinery".into(), owner_corp_id: corp_id,
                solar_system_id: 30_000_142, position: [100_000.0, 0.0, 0.0], state: "ShieldVulnerable".into(),
                quantum_core_installed: true, fuel_blocks: 1_000, shield_hp: 5_000.0, armor_hp: 5_000.0, hull_hp: 5_000.0,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            });
        });
        let ext_rep = ctx.dispatch_rpc(&mut sess1, "moonExtractions", None, "StartNewExtraction", &[PyRep::Integer(990_000_888), PyRep::Integer(518_400)], &kw, None).await.expect("StartNewExtraction");
        let auto_ev_id = ext_rep.dict_get("eventID").and_then(|v| v.as_i64()).unwrap();
        let arrival_unix = ext_rep.dict_get("chunkArrivalTime").and_then(|v| v.as_i64()).unwrap();
        let arrival_ft = (arrival_unix + 11_644_473_600) * 10_000_000;
        let moon_id = ext_rep.dict_get("moonID").and_then(|v| v.as_i64()).unwrap();
        let expected_moon_name = ctx.sde.celestials_in_system(30_000_142).into_iter().find(|c| c.celestial_id == moon_id).map(|c| c.name).unwrap();
        assert_eq!(ctx.dispatch_rpc(&mut sess2, "slash", None, "SlashCmd", &[PyRep::String(format!("/createccpevent {arrival_ft} 1440 2 CCP Crimson Harvest | Global event"))], &kw, None).await.unwrap_err().code, "InsufficientRole");
        let prev_role = sess1.role;
        sess1.role |= eve_machonet::service_roles::ROLE_GMH;
        let ccp_rep = ctx.dispatch_rpc(&mut sess1, "slash", None, "SlashCmd", &[PyRep::String(format!("/createccpevent {arrival_ft} 1440 2 CCP Crimson Harvest | Global event"))], &kw, None).await.expect("slash /createccpevent");
        sess1.role = prev_role;
        let ccp_ev_id = ccp_rep.dict_get("eventID").and_then(|v| v.as_i64()).unwrap();
        let ev_list = ctx.dispatch_rpc(&mut sess1, "calendarProxy", None, "GetEventList", &[PyRep::Integer(1), PyRep::Integer(2027)], &kw, None).await.unwrap();
        let PyRep::List(ev_rowsets) = &ev_list else { panic!("expected List of rowsets from calendarProxy.GetEventList") };
        let PyRep::Rowset { rows: ev_rows, .. } = &ev_rowsets[0] else { panic!() };
        assert!(ev_rows.iter().any(|r| r[0] == PyRep::Integer(ccp_ev_id) && r[1] == PyRep::Integer(1) && r[2] == PyRep::Integer(arrival_ft) && r[3] == PyRep::Integer(1440) && r[5] == PyRep::String("CCP Crimson Harvest".into()) && r[6] == PyRep::Integer(0)), "calendarProxy.GetEventList must include GM-created global CCP event");
        assert!(ev_rows.iter().any(|r| r[0] == PyRep::Integer(auto_ev_id) && r[1] == PyRep::Integer(corp_id) && r[2] == PyRep::Integer(arrival_ft) && r[3] == PyRep::Integer(2880) && r[8] == PyRep::Bool(true) && r[5].as_str().is_some_and(|t| t.contains(&expected_moon_name))), "calendarProxy.GetEventList must include automated moon extraction event with SDE moon name");
        ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "DeleteEvent", &[PyRep::Integer(pers_ev_id)], &kw, None).await.unwrap();

        // Chat: Rookie Help, MOTD & GM Mute enforcement (`feat:social.chat-help-rookie`, `feat:social.chat-motd`, `feat:social.chat-gm-mute`)
        let now_u = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            snap.characters.get_mut(&90_000_001).unwrap().create_date_unix = now_u - 5 * 86_400;
            snap.characters.get_mut(&90_000_002).unwrap().create_date_unix = now_u - 45 * 86_400;
        });
        assert_eq!(ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GetRookieChannel", &[], &kw, None).await.unwrap(), PyRep::String("rookie_help".into()));
        assert_eq!(ctx.dispatch_rpc(&mut sess2, "XmppChatMgr", None, "GetRookieChannel", &[], &kw, None).await.unwrap(), PyRep::None);
        assert_eq!(ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GetHelpChannel", &[], &kw, None).await.unwrap(), PyRep::String("help_en".into()));
        assert_eq!(ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GetHelpChannel", &[PyRep::String("de".into())], &kw, None).await.unwrap(), PyRep::String("help_de".into()));

        ctx.dispatch_rpc(&mut sess1, "LSC", None, "JoinChannel", &[PyRep::String("channel:moderation_test".into())], &kw, None).await.unwrap();
        ctx.dispatch_rpc(&mut sess2, "LSC", None, "JoinChannel", &[PyRep::String("channel:moderation_test".into())], &kw, None).await.unwrap();
        let set_motd = ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "SetMotd", &[PyRep::String("channel:moderation_test".into()), PyRep::String("Be polite in chat".into())], &kw, None).await.unwrap();
        assert_eq!(set_motd.dict_get("motd"), Some(&PyRep::String("Be polite in chat".into())));

        assert_eq!(ctx.dispatch_rpc(&mut sess2, "XmppChatMgr", None, "GMMute", &[PyRep::Integer(90_000_001), PyRep::String("channel:moderation_test".into()), PyRep::Integer(600), PyRep::String("Noop".into())], &kw, None).await.unwrap_err().code, "ChatAccessDenied");
        ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GMMute", &[PyRep::Integer(90_000_002), PyRep::String("channel:moderation_test".into()), PyRep::Integer(600), PyRep::String("Spamming".into())], &kw, None).await.unwrap();
        assert_eq!(ctx.dispatch_rpc(&mut sess2, "XmppChatMgr", None, "GMUnmute", &[PyRep::Integer(90_000_002), PyRep::String("channel:moderation_test".into())], &kw, None).await.unwrap_err().code, "ChatAccessDenied");
        let muted_err = ctx.dispatch_rpc(&mut sess2, "LSC", None, "SendMessage", &[PyRep::String("channel:moderation_test".into()), PyRep::String("Can anyone hear me?".into())], &kw, None).await.expect_err("Muted user must be blocked from sending chat messages");
        assert_eq!(muted_err.code, "MutedInChannel");
        ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GMUnmute", &[PyRep::Integer(90_000_002), PyRep::String("channel:moderation_test".into())], &kw, None).await.unwrap();
        assert!(is_keyval(&ctx.dispatch_rpc(&mut sess2, "LSC", None, "SendMessage", &[PyRep::String("channel:moderation_test".into()), PyRep::String("Unmuted now!".into())], &kw, None).await.unwrap()));
    }
}

pub(crate) mod fleet_movement_optins_formations_setups {
    // Integration tests for fleet movement opt-ins, formations, setups, boss handover, join requests, watchlist, broadcasts, and target tagging.
    //
    // Parity rows:
    // - `feat:fleet.member-movement-optins`
    // - `feat:fleet.fleet-setups`
    // - `feat:fleet.join-requests`
    // - `feat:fleet.watchlist-broadcast-tracking`
    // - `feat:fleet.broadcast-scope-and-filter`
    // - `feat:fleet.takes-fleet-warp-toggle`


    use std::collections::HashMap;

    use crate::common;
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;

    const PILOT_1: i64 = 90_000_001;
    const PILOT_2: i64 = 90_000_002;

    async fn select_pilot(
        ctx: &ServerContext,
        session_id: u64,
        username: &str,
        password: &str,
        char_id: i64,
    ) -> SessionState {
        let mut sess = SessionState::new(session_id);
        common::login_pilot(ctx, &mut sess, username, password).await;
        ctx.dispatch_rpc(
            &mut sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("SelectCharacterID");
        ctx.advance_secs(12);
        sess
    }

    async fn rpc(
        ctx: &ServerContext,
        sess: &mut SessionState,
        service: &str,
        method: &str,
        args: &[PyRep],
    ) -> Result<PyRep, eve_proto::BridgeRpcError> {
        ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
            .await
    }

    fn field(rep: &PyRep, key: &str) -> PyRep {
        match rep {
            PyRep::KeyVal(map) => map.get(key).cloned().unwrap_or(PyRep::None),
            PyRep::Dict(entries) => entries
                .iter()
                .find_map(|(k, v)| (k.as_str() == Some(key)).then(|| v.clone()))
                .unwrap_or(PyRep::None),
            _ => PyRep::None,
        }
    }

    #[tokio::test]
    async fn test_fleet_movement_optins_formations_and_setups() {
        // parity: feat:fleet.member-movement-optins
        // parity: feat:fleet.takes-fleet-warp-toggle
        // parity: feat:fleet.fleet-setups
        eve_testkit::attest(&[
            "feat:fleet.member-movement-optins",
            "feat:fleet.takes-fleet-warp-toggle",
            "feat:fleet.fleet-setups",
        ]);

        let ctx = ServerContext::new();
        let mut sess1 = select_pilot(&ctx, 64_001, "demo", "demo-secret", PILOT_1).await;
        let mut sess2 = select_pilot(&ctx, 64_002, "other", "other-secret", PILOT_2).await;

        // Pilot 1 creates a fleet
        let create_rep = rpc(&ctx, &mut sess1, "fleetObjectHandler", "CreateFleet", &[])
            .await
            .expect("CreateFleet");
        let fid = create_rep.as_i64().expect("fleetID");
        sess1.fleet_id = Some(fid);

        // Invite and join Pilot 2 into the fleet
        rpc(
            &ctx,
            &mut sess1,
            "fleetObjectHandler",
            "Invite",
            &[PyRep::Integer(PILOT_2), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)],
        )
        .await
        .expect("Invite Pilot 2");

        rpc(
            &ctx,
            &mut sess2,
            "fleetObjectHandler",
            "AcceptInvite",
            &[PyRep::Integer(fid)],
        )
        .await
        .expect("AcceptInvite Pilot 2");
        sess2.fleet_id = Some(fid);

        // 1. feat:fleet.member-movement-optins & feat:fleet.takes-fleet-warp-toggle
        // Pilot 2 disables takes_fleet_warp
        let warp_opt = rpc(
            &ctx,
            &mut sess2,
            "fleetObjectHandler",
            "SetTakesFleetWarp",
            &[PyRep::Bool(false)],
        )
        .await
        .expect("SetTakesFleetWarp false");
        assert_eq!(field(&warp_opt, "takesFleetWarp"), PyRep::Bool(false));

        // Pilot 2 disables regroup opt-in
        let regroup_opt = rpc(
            &ctx,
            &mut sess2,
            "fleetObjectHandler",
            "SetAcceptsRegroupValue",
            &[PyRep::Bool(false)],
        )
        .await
        .expect("SetAcceptsRegroupValue false");
        assert_eq!(field(&regroup_opt, "acceptsRegroup"), PyRep::Bool(false));

        // Pilot 2 disables conduit jumps opt-in
        let conduit_opt = rpc(
            &ctx,
            &mut sess2,
            "fleetObjectHandler",
            "SetAcceptsConduitJumpsValue",
            &[PyRep::Bool(false)],
        )
        .await
        .expect("SetAcceptsConduitJumpsValue false");
        assert_eq!(field(&conduit_opt, "acceptsConduitJumps"), PyRep::Bool(false));

        // Verify preferences in fleet member record
        ctx.db.with_snapshot(|s| {
            let fleet = s.fleets.get(&fid).expect("fleet");
            let m2 = fleet.member_details.get(&PILOT_2).expect("pilot 2 member");
            assert!(!m2.takes_fleet_warp, "Member takes_fleet_warp must be false");
            assert!(!m2.accepts_regroup, "Member accepts_regroup must be false");
            assert!(!m2.accepts_conduit_jumps, "Member accepts_conduit_jumps must be false");
        });

        // 2. feat:fleet.stored-formations-and-warp-shapes
        // Set Arrow formation (id 3, spacing 5000m, size 10000m)
        let form_rep = rpc(
            &ctx,
            &mut sess1,
            "fleetObjectHandler",
            // rpc-lint: allow auditor acceptance text feat:fleet.stored-formations-and-warp-shapes
            "SetFleetFormation",
            &[PyRep::Integer(3), PyRep::Real(5_000.0), PyRep::Real(10_000.0)],
        )
        .await
        .expect("SetFleetFormation");
        assert_eq!(field(&form_rep, "formationID"), PyRep::Integer(3));
        assert_eq!(field(&form_rep, "formationName"), PyRep::String("Arrow".into()));

        // rpc-lint: allow auditor acceptance text feat:fleet.stored-formations-and-warp-shapes
        let get_form = rpc(&ctx, &mut sess1, "fleetObjectHandler", "GetFleetFormation", &[])
            .await
            .expect("GetFleetFormation");
        assert_eq!(field(&get_form, "formationID"), PyRep::Integer(3));
        assert_eq!(field(&get_form, "formationName"), PyRep::String("Arrow".into()));

        // 3. feat:fleet.fleet-setups
        // Boss stores fleet setup
        let store_setup = rpc(
            &ctx,
            &mut sess1,
            "fleetObjectHandler",
            "StoreFleetSetup",
            &[
                PyRep::String("Alpha Strike Setup".into()),
                PyRep::String("Form up at gate".into()),
                PyRep::Bool(false),
                PyRep::Integer(128),
            ],
        )
        .await
        .expect("StoreFleetSetup");
        assert_eq!(
            field(&store_setup, "setupName"),
            PyRep::String("Alpha Strike Setup".into())
        );

        let get_setups = rpc(&ctx, &mut sess1, "fleetObjectHandler", "GetFleetSetups", &[])
            .await
            .expect("GetFleetSetups");
        let PyRep::Dict(setup_dict) = get_setups else { panic!("expected dict") };
        assert!(setup_dict.iter().any(|(k, _)| k == &PyRep::String("Alpha Strike Setup".into())));

        // Boss loads fleet setup
        let load_setup = rpc(
            &ctx,
            &mut sess1,
            "fleetObjectHandler",
            "LoadFleetSetup",
            &[PyRep::String("Alpha Strike Setup".into())],
        )
        .await
        .expect("LoadFleetSetup");
        assert_eq!(
            field(&load_setup, "setupName"),
            PyRep::String("Alpha Strike Setup".into())
        );

        // Delete fleet setup
        let del_setup = rpc(
            &ctx,
            &mut sess1,
            "fleetObjectHandler",
            "DeleteFleetSetup",
            &[PyRep::String("Alpha Strike Setup".into())],
        )
        .await
        .expect("DeleteFleetSetup");
        assert_eq!(field(&del_setup, "deleted"), PyRep::Bool(true));

        // 4. feat:fleet.boss-handover
        // Non-boss Pilot 2 cannot call MakeLeader
        let unauth_lead = rpc(
            &ctx,
            &mut sess2,
            "fleetObjectHandler",
            "MakeLeader",
            &[PyRep::Integer(PILOT_1)],
        )
        .await;
        assert!(unauth_lead.is_err(), "Non-boss member cannot transfer leadership");

        // Boss Pilot 1 transfers boss to Pilot 2
        let lead_rep = rpc(
            &ctx,
            &mut sess1,
            "fleetObjectHandler",
            "MakeLeader",
            &[PyRep::Integer(PILOT_2)],
        )
        .await
        .expect("MakeLeader");
        assert_eq!(field(&lead_rep, "newBossID"), PyRep::Integer(PILOT_2));
        ctx.db.with_snapshot(|s| {
            let fleet = s.fleets.get(&fid).unwrap();
            assert_eq!(fleet.boss_id, PILOT_2, "Pilot 2 is now fleet boss");
        });
    }

    #[tokio::test]
    async fn test_fleet_tactical_broadcasts_watchlist_and_tagging() {
        // parity: feat:fleet.join-requests
        // parity: feat:fleet.watchlist-broadcast-tracking
        // parity: feat:fleet.broadcast-scope-and-filter
        eve_testkit::attest(&[
            "feat:fleet.join-requests",
            "feat:fleet.watchlist-broadcast-tracking",
            "feat:fleet.broadcast-scope-and-filter",
        ]);

        let ctx = ServerContext::new();
        let mut sess1 = select_pilot(&ctx, 64_101, "demo", "demo-secret", PILOT_1).await;
        let mut sess2 = select_pilot(&ctx, 64_102, "other", "other-secret", PILOT_2).await;

        // Pilot 1 creates fleet
        let create_rep = rpc(&ctx, &mut sess1, "fleetObjectHandler", "CreateFleet", &[])
            .await
            .expect("CreateFleet");
        let fid = create_rep.as_i64().expect("fleetID");
        sess1.fleet_id = Some(fid);

        // Boss adds fleet advert so pilots can apply
        rpc(
            &ctx,
            &mut sess1,
            "fleetProxy",
            "AddFleetFinderAdvert",
            &[
                PyRep::String("Tactical Fleet".into()),
                PyRep::String("Join for ops".into()),
                PyRep::Integer(3),
                PyRep::Bool(false),
            ],
        )
        .await
        .expect("AddFleetFinderAdvert");

        // 1. feat:fleet.join-requests
        // Pilot 2 applies to join fleet
        rpc(
            &ctx,
            &mut sess2,
            "fleetProxy",
            "ApplyToJoinFleet",
            &[PyRep::Integer(fid)],
        )
        .await
        .expect("ApplyToJoinFleet");

        // Boss views join requests
        let reqs_rep = rpc(&ctx, &mut sess1, "fleetObjectHandler", "GetJoinRequests", &[])
            .await
            .expect("GetJoinRequests");
        let PyRep::Dict(reqs) = reqs_rep else { panic!("expected dict") };
        assert!(reqs.iter().any(|(k, _)| k == &PyRep::Integer(PILOT_2)), "Pilot 2 must appear in join requests");

        // Boss rejects request
        rpc(
            &ctx,
            &mut sess1,
            "fleetObjectHandler",
            "RejectJoinRequest",
            &[PyRep::Integer(PILOT_2)],
        )
        .await
        .expect("RejectJoinRequest");

        // Verify rejection notification sent to Pilot 2
        let p2_notifs = ctx.drain_notifications_for_char(PILOT_2);
        assert!(
            p2_notifs.iter().any(|n| n.event == "OnFleetJoinRejected"),
            "Pilot 2 must receive OnFleetJoinRejected notification"
        );

        // Pilot 2 applies again and Boss accepts
        rpc(
            &ctx,
            &mut sess2,
            "fleetProxy",
            "ApplyToJoinFleet",
            &[PyRep::Integer(fid)],
        )
        .await
        .expect("ApplyToJoinFleet 2nd time");

        rpc(
            &ctx,
            &mut sess1,
            "fleetObjectHandler",
            "AcceptJoinRequest",
            &[PyRep::Integer(PILOT_2)],
        )
        .await
        .expect("AcceptJoinRequest");
        sess2.fleet_id = Some(fid);

        // 2. feat:fleet.watchlist-broadcast-tracking
        let add_watch = rpc(
            &ctx,
            &mut sess2,
            "fleetMgr",
            "AddToWatchlist",
            &[PyRep::List(vec![PyRep::Integer(PILOT_1)])],
        )
        .await
        .expect("AddToWatchlist");
        assert_eq!(field(&add_watch, "watched"), PyRep::Bool(true));

        let reg_dam = rpc(
            &ctx,
            &mut sess2,
            "fleetMgr",
            "RegisterForDamageUpdates",
            &[PyRep::List(vec![PyRep::Integer(PILOT_1)])],
        )
        .await
        .expect("RegisterForDamageUpdates");
        assert_eq!(field(&reg_dam, "watched"), PyRep::Bool(true));

        let rem_watch = rpc(
            &ctx,
            &mut sess2,
            "fleetMgr",
            "RemoveFromWatchlist",
            &[PyRep::List(vec![PyRep::Integer(PILOT_1)])],
        )
        .await
        .expect("RemoveFromWatchlist");
        assert_eq!(field(&rem_watch, "watched"), PyRep::Bool(false));

        // 3. feat:fleet.broadcast-scope-and-filter
        // Mute NeedShield broadcasts
        let set_filt = rpc(
            &ctx,
            &mut sess2,
            "fleetObjectHandler",
            // rpc-lint: allow auditor acceptance text feat:fleet.broadcast-scope-and-filter
            "SetBroadcastFilter",
            &[PyRep::String("NeedShield".into()), PyRep::Bool(false)],
        )
        .await
        .expect("SetBroadcastFilter");
        assert_eq!(field(&set_filt, "enabled"), PyRep::Bool(false));

        // rpc-lint: allow auditor acceptance text feat:fleet.broadcast-scope-and-filter
        let get_filt = rpc(&ctx, &mut sess2, "fleetObjectHandler", "GetBroadcastFilter", &[])
            .await
            .expect("GetBroadcastFilter");
        let PyRep::List(muted) = field(&get_filt, "mutedBroadcasts") else { panic!("expected list") };
        assert!(muted.contains(&PyRep::String("NeedShield".into())));

        // Send broadcast
        let bc_rep = rpc(
            &ctx,
            &mut sess1,
            "fleetObjectHandler",
            "SendBroadcast",
            &[
                PyRep::String("NeedArmor".into()),
                PyRep::Integer(1), // Entire fleet scope
            ],
        )
        .await
        .expect("SendBroadcast");
        assert_eq!(field(&bc_rep, "broadcast"), PyRep::String("NeedArmor".into()));

        // Rapid duplicate broadcast triggers 2s spam cooldown
        let spam_bc = rpc(
            &ctx,
            &mut sess1,
            "fleetObjectHandler",
            "SendBroadcast",
            &[
                PyRep::String("NeedArmor".into()),
                PyRep::Integer(1),
            ],
        )
        .await;
        assert!(spam_bc.is_err(), "Duplicate broadcast within 2s must be rejected with BroadcastSpamCooldown");

        // Pilot 2 (squad member) attempts restricted broadcast ("Target") and is rejected with CannotBroadcastInFleet
        let unauth_bc = rpc(
            &ctx,
            &mut sess2,
            "fleetObjectHandler",
            "SendBroadcast",
            &[
                PyRep::String("Target".into()),
                PyRep::Integer(1),
            ],
        )
        .await;
        let err = unauth_bc.expect_err("Unauthorized member must not send restricted broadcast");
        assert_eq!(err.code, "CannotBroadcastInFleet");

        // 4. feat:fleet.target-tagging
        // Boss spawns and locks target entity 999001 on grid, then assigns tag 'A'
        let sys_id = sess1.solar_system_id.unwrap_or(30_000_142);
        let ship1 = sess1.ship_id.unwrap_or(1);
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle
            .spawn_ball(eve_destiny::Ball {
                id: 999001,
                type_id: 587,
                position: glam::DVec3::new(10_000.0, 0.0, 0.0),
                velocity: glam::DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            })
            .await;
        let _ = sys_handle
            .add_target_lock(
                ship1,
                eve_destiny::TargetLock {
                    target_id: 999001,
                    lock_time_ms: 0.0,
                    remaining_lock_ms: 0.0,
                    locked: true,
                    max_range_m: None,
                },
            )
            .await;
        let tag_rep = rpc(
            &ctx,
            &mut sess1,
            "fleetObjectHandler",
            // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
            "SetTargetTag",
            &[PyRep::Integer(999001), PyRep::String("A".into())],
        )
        .await
        .expect("SetTargetTag");
        assert_eq!(field(&tag_rep, "targetID"), PyRep::Integer(999001));
        assert_eq!(field(&tag_rep, "tag"), PyRep::String("A".into()));

        // Clear target tags
        let clear_tags = rpc(
            &ctx,
            &mut sess1,
            "fleetObjectHandler",
            // rpc-lint: allow auditor acceptance text feat:fleet.target-tagging
            "ClearTargetTags",
            &[],
        )
        .await
        .expect("ClearTargetTags");
        assert_eq!(field(&clear_tags, "fleetID"), PyRep::Integer(fid));
    }

    #[tokio::test]
    async fn test_fleet_boss_handover_closure() {
        // parity: feat:fleet.boss-handover
        let ctx = std::sync::Arc::new(ServerContext::new());
        crate::common::pin_clock(&ctx);

        let sys_id = 30_000_142_u32;
        let boss_id = 90_000_001_i64;
        let fc_id = 90_000_002_i64;

        ctx.db.with_mut_snapshot(|snap| {
            for (cid, uid) in [(boss_id, 1_u64), (fc_id, 2_u64)] {
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    ch.user_id = uid;
                    ch.solar_system_id = sys_id;
                    ch.station_id = None;
                }
            }
        });

        let mut sess1 = SessionState::new(64_001);
        crate::common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
        let mut sess2 = SessionState::new(64_002);
        crate::common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;

        for (s, cid) in [(&mut sess1, boss_id), (&mut sess2, fc_id)] {
            rpc(&ctx, s, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(cid)])
                .await
                .expect("SelectCharacterID");
        }
        ctx.advance_secs(15);

        let created = rpc(&ctx, &mut sess1, "fleetObjectHandler", "CreateFleet", &[])
            .await
            .expect("CreateFleet");
        let fid = match created {
            PyRep::Integer(v) => v,
            _ => match field(&created, "fleetID") {
                PyRep::Integer(v) => v,
                other => panic!("expected fleetID, got {other:?}"),
            },
        };
        sess1.fleet_id = Some(fid);

        let _ = rpc(
            &ctx,
            &mut sess1,
            "fleetObjectHandler",
            "Invite",
            &[PyRep::Integer(fc_id), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)],
        )
        .await
        .expect("Invite");
        let _ = rpc(&ctx, &mut sess2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fid)])
            .await
            .expect("AcceptInvite");
        sess2.fleet_id = Some(fid);

        // Boss handover (`feat:fleet.boss-handover`): MakeLeader, LeaveFleet, and disconnect handover
        let _ = rpc(&ctx, &mut sess1, "fleetObjectHandler", "MakeLeader", &[PyRep::Integer(fc_id)])
            .await
            .expect("MakeLeader");
        assert_eq!(ctx.db.with_snapshot(|s| s.fleets.get(&fid).unwrap().boss_id), fc_id);
        assert!(ctx.drain_notifications_for_char(boss_id).iter().any(|n| n.event == "OnFleetMemberChanged"));

        // Boss (fc_id) leaves -> boss passes automatically to boss_id
        let left = rpc(&ctx, &mut sess2, "fleetObjectHandler", "LeaveFleet", &[])
            .await
            .expect("LeaveFleet");
        assert_eq!(field(&left, "disbanded"), PyRep::Bool(false));
        assert_eq!(ctx.db.with_snapshot(|s| s.fleets.get(&fid).unwrap().boss_id), boss_id);
        assert!(ctx.drain_notifications_for_char(boss_id).iter().any(|n| n.event == "OnFleetMemberChanged"));

        // Re-invite fc_id, then disconnect boss_id -> boss passes automatically to fc_id
        let _ = rpc(&ctx, &mut sess1, "fleetObjectHandler", "Invite", &[PyRep::Integer(fc_id), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(4)])
            .await
            .expect("Re-invite fc_id");
        ctx.advance_secs(15);
        let _ = rpc(&ctx, &mut sess2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fid)])
            .await
            .expect("AcceptInvite 2");
        let _ = ctx.drain_notifications_for_char(fc_id);
        let _ = eve_server::services::fleet_social::fleet_social_notify::remove_character_from_all_fleets(&ctx, boss_id);
        assert_eq!(ctx.db.with_snapshot(|s| s.fleets.get(&fid).unwrap().boss_id), fc_id);
        assert!(ctx.drain_notifications_for_char(fc_id).iter().any(|n| n.event == "OnFleetMemberChanged"));
    }
}
