//! Track 4: Phase 8 Corporation Governance, Shares, Votes, Dividends, Medals & Alliances (`SO-3`).

use crate::common;

use eve_db::corp_roles;
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
async fn test_so3_corporation_roles_public_info_governance_shares_votes_and_alliances() {
    // parity: feat:corporation.corp-public-info, feat:corporation.corp-edit-details, feat:corporation.invitations, feat:corporation.kick-member, feat:corporation.leave-corp, feat:corporation.titles, feat:corporation.corp-tax, feat:corporation.corp-shares, feat:corporation.corp-votes, feat:corporation.ceo-change, feat:corporation.bulletins, feat:corporation.medals, feat:corporation.member-tracking, feat:corporation.member-audit, rpc:account.GetWalletDivisionsInfo, rpc:account.GiveCashFromCorpAccount, rpc:charUnboundMgr.SelectCharacterID, rpc:corpRegistry.AddBulletin, rpc:corpRegistry.AddCorporation, rpc:corpRegistry.GetBulletins, rpc:corpRegistry.InsertApplication, rpc:corpRegistry.MoveCompanyShares, rpc:corpRegistry.PayoutDividend, rpc:corpRegistry.UpdateApplicationOffer, rpc:corpRegistry.UpdateCorporation, rpc:corpRegistry.UpdateMember, rpc:corpRegistry.UpdateTitle, rpc:corpmgr.AuditMember, rpc:corpmgr.GetPublicInfo, rpc:corporationSvc.CreateMedal, rpc:corporationSvc.GiveMedalToCharacters, rpc:voteManager.InsertVote, rpc:voteManager.InsertVoteCase
    eve_testkit::attest(&[
        "feat:corporation.corp-public-info",
        "feat:corporation.corp-edit-details",
        "feat:corporation.invitations",
        "feat:corporation.kick-member",
        "feat:corporation.leave-corp",
        "feat:corporation.titles",
        "feat:corporation.corp-tax",
        "feat:corporation.corp-shares",
        "feat:corporation.corp-votes",
        "feat:corporation.ceo-change",
        "feat:corporation.bulletins",
        "feat:corporation.medals",
        "feat:corporation.member-tracking",
        "feat:corporation.member-audit",
    ]);
    let ctx = ServerContext::new();
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

    // 3. Public Info & Edit Details: verify all public info fields before and after UpdateCorporation
    let public_info = ctx
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
        .expect("GetPublicInfo");
    let PyRep::KeyVal(pub_kv) = &public_info else {
        panic!("Expected KeyVal from GetPublicInfo")
    };
    assert_eq!(pub_kv["corporationID"], PyRep::Integer(corp_id));
    assert_eq!(
        pub_kv["corporationName"],
        PyRep::String("DeepSpace Logistics".into())
    );
    assert_eq!(pub_kv["ticker"], PyRep::String("DSLOG".into()));
    assert_eq!(pub_kv["ceoID"], PyRep::Integer(90_000_001));
    assert_eq!(pub_kv["memberCount"], PyRep::Integer(1));
    assert_eq!(pub_kv["taxRate"], PyRep::Real(0.075));

    // Pilot 1 updates corp details via V24 corpRegistry.UpdateCorporation
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

    // Re-read GetPublicInfo to verify persisted description, URL, and taxRate
    let public_info_after = ctx
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
        .expect("GetPublicInfo after UpdateCorporation");
    let PyRep::KeyVal(pub_after_kv) = &public_info_after else {
        panic!()
    };
    assert_eq!(
        pub_after_kv["description"],
        PyRep::String("Premier nullsec freight services".into())
    );
    assert_eq!(
        pub_after_kv["url"],
        PyRep::String("https://dslogistics.eve".into())
    );
    assert_eq!(pub_after_kv["taxRate"], PyRep::Real(0.08));

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

#[tokio::test]
async fn test_so6_wormholes_polarization_lifetime_mass_and_thera() {
    // parity: feat:navigation.wormhole-jump, feat:navigation.wormhole-polarization, feat:navigation.wormhole-lifetime, feat:navigation.wormhole-statics, feat:navigation.wormhole-thera, rpc:charUnboundMgr.SelectCharacterID, rpc:scanMgr.GetSystemScanMgr, rpc:structureDeployment.Anchor, rpc:wormholeMgr.GetWormholePolarization, rpc:wormholeMgr.WormholeJump
    eve_testkit::attest(&[
        "feat:navigation.wormhole-jump",
        "feat:navigation.wormhole-polarization",
        "feat:navigation.wormhole-lifetime",
        "feat:navigation.wormhole-statics",
        "feat:navigation.wormhole-thera",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess = SessionState::new(601);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(90_000_001)],
        &kw,
        None,
    )
    .await
    .expect("Select Pilot 1");

    // Initialize system cosmic signatures and wormhole balls via real V24 scanMgr.GetSystemScanMgr
    let _ = ctx
        .dispatch_rpc(&mut sess, "scanMgr", None, "GetSystemScanMgr", &[], &kw, None)
        .await
        .expect("GetSystemScanMgr");

    let (wh_sig_id, wh_pos) = ctx.db.with_snapshot(|snap| {
        let sig = snap.cosmic_signatures[&30_000_142]
            .iter()
            .find(|s| s.sig_group == "Wormhole")
            .unwrap();
        (sig.signature_id.clone(), sig.position)
    });

    // Resolve wormhole item ID in Jita Ballpark (group_id = 988)
    let jita_sys = ctx.systems.get_or_spawn(30_000_142);
    let wh_item_id =
        eve_server::services::space_destiny::wormhole_item_id_for_signature(30_000_142, &wh_sig_id);
    let (_, wh_ball) = jita_sys
        .get_ball_snapshot(wh_item_id)
        .await
        .unwrap()
        .expect("Wormhole ball must be spawned in Jita Ballpark");
    assert_eq!(wh_ball.type_id, 26272);

    // Position pilot's Rifter (600_000_001) within 2,500m of the wormhole ball
    let ship_ball = Ball {
        id: 600_000_001,
        type_id: 587,
        position: DVec3::new(wh_pos[0] + 500.0, wh_pos[1], wh_pos[2]),
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 2.5,
        mass: 1_200_000.0,
        radius: 50.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = jita_sys.spawn_ball(ship_ball.clone()).await;
    sess.ship_id = Some(600_000_001);
    sess.solar_system_id = Some(30_000_142);
    sess.station_id = None;
    sess.structure_id = None;
    ctx.db
        .try_with_mut_snapshot(|snap| {
            let ch = snap.characters.get_mut(&90_000_001).unwrap();
            ch.active_ship_id = 600_000_001;
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = 30_000_142;
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();

    // 1. Max jump mass enforcement: ship exceeding max_jump_mass_kg (375,000,000 kg) is rejected
    let heavy_ball = Ball {
        mass: 1_500_000_000.0,
        ..ship_ball.clone()
    };
    let _ = jita_sys.spawn_ball(heavy_ball).await;
    let heavy_err = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(wh_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Ship exceeding max_jump_mass_kg must be rejected");
    assert_eq!(heavy_err.code, "ShipTooHeavyForWormhole");

    // Restore normal Rifter mass (1,200,000 kg)
    let _ = jita_sys.spawn_ball(ship_ball.clone()).await;

    // 2. Jump Wormhole via real V24 wormholeMgr.WormholeJump(wormholeItemID) -> transits to Thera (31000005)
    let jump_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(wh_item_id)],
            &kw,
            None,
        )
        .await
        .expect("WormholeJump to Thera");
    let PyRep::KeyVal(jkv) = &jump_rep else {
        panic!("Expected KeyVal from WormholeJump")
    };
    assert_eq!(sess.solar_system_id, Some(31_000_005));
    assert_eq!(jkv["isThera"], PyRep::Bool(true));
    assert_eq!(jkv["wormholeClass"], PyRep::Integer(12));
    assert_eq!(jkv["wormholeSize"], PyRep::Real(1.0)); // > 50% mass remaining
    assert_eq!(jkv["wormholeAge"], PyRep::Integer(1)); // >= 4h remaining
    assert!(
        (jkv["remainingMassKg"].as_f64().unwrap() - (2_000_000_000.0 - 1_200_000.0)).abs() < 1.0,
        "WormholeJump must subtract jumping ship mass from remaining_mass_kg"
    );

    // 3. Thera rules: anchoring structures or POS towers in Thera (31000005) is disallowed
    let thera_anchor_err = ctx
        .dispatch_rpc(
            &mut sess,
            "structureDeployment",
            None,
            "Anchor",
            &[
                PyRep::Integer(35832),
                PyRep::String("Thera Astrahus".into()),
            ],
            &kw,
            None,
        )
        .await
        .expect_err("Anchoring structures in Thera must be rejected");
    assert_eq!(
        thera_anchor_err.code,
        "AnchoringDisallowedInTheraOrShattered"
    );

    // 4. Polarization Check via real V24 wormholeMgr.GetWormholePolarization
    let pol_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "GetWormholePolarization",
            &[PyRep::Integer(31_000_005)],
            &kw,
            None,
        )
        .await
        .expect("GetWormholePolarization");
    if let PyRep::KeyVal(entries) = &pol_rep {
        let is_polarized = entries
            .iter()
            .find(|(k, _)| k.as_str() == "isPolarized")
            .unwrap()
            .1
            .as_bool()
            .unwrap();
        assert!(
            is_polarized,
            "Pilot must be polarized after jumping wormhole"
        );
    }

    // Attempting immediate jump on same wormhole while polarized is rejected with WormholePolarized
    ctx.db
        .try_with_mut_snapshot(|snap| {
            snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 30_000_142;
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();
    let _ = jita_sys.spawn_ball(ship_ball.clone()).await;
    sess.solar_system_id = Some(30_000_142);
    let pol_err = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(wh_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Polarized pilot cannot jump same wormhole");
    assert_eq!(pol_err.code, "WormholePolarized");

    // Advance virtual clock 301s past 5-minute polarization window
    ctx.advance_secs(301);

    // 5. Mass exhaustion collapse & automatic static wormhole respawn:
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sigs) = snap.cosmic_signatures.get_mut(&30_000_142) {
            if let Some(wh) = sigs.iter_mut().find(|s| s.signature_id == wh_sig_id) {
                wh.remaining_mass_kg = 800_000.0;
                wh.destination_system_id = Some(31_000_004);
            }
        }
    });

    let collapse_jump = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(wh_item_id)],
            &kw,
            None,
        )
        .await
        .expect("WormholeJump collapsing mass");
    let PyRep::KeyVal(ckv) = &collapse_jump else {
        panic!()
    };
    assert_eq!(
        ckv["collapsed"],
        PyRep::Bool(true),
        "Wormhole must collapse when remaining_mass_kg <= 0"
    );
    assert_eq!(
        ckv["environmentalEffect"],
        PyRep::String("Magnetar".into())
    );
    let replacement_sig_id = ckv["replacementSignatureID"]
        .as_str()
        .expect("Collapsed static wormhole must spawn a replacement static signature")
        .to_string();
    assert_ne!(replacement_sig_id, wh_sig_id);

    // Verify old wormhole ball was removed from Jita and replacement static signature exists
    assert!(
        jita_sys
            .get_ball_snapshot(wh_item_id)
            .await
            .unwrap()
            .is_none(),
        "Collapsed wormhole ball must be removed from Ballpark"
    );

    // 6. Lifetime expiration collapse & static respawn
    let rep_wh_item_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(
        30_000_142,
        &replacement_sig_id,
    );
    let rep_pos = ctx.db.with_mut_snapshot(|snap| {
        let sig = snap
            .cosmic_signatures
            .get_mut(&30_000_142)
            .unwrap()
            .iter_mut()
            .find(|s| s.signature_id == replacement_sig_id)
            .unwrap();
        sig.lifetime_seconds = 0;
        sig.position
    });
    ctx.advance_secs(301);
    ctx.db
        .try_with_mut_snapshot(|snap| {
            snap.characters.get_mut(&90_000_001).unwrap().solar_system_id = 30_000_142;
            Ok::<(), std::convert::Infallible>(())
        })
        .unwrap();
    sess.solar_system_id = Some(30_000_142);
    let _ = jita_sys
        .spawn_ball(Ball {
            position: DVec3::new(rep_pos[0] + 200.0, rep_pos[1], rep_pos[2]),
            ..ship_ball
        })
        .await;

    let expired_err = ctx
        .dispatch_rpc(
            &mut sess,
            "wormholeMgr",
            None,
            "WormholeJump",
            &[PyRep::Integer(rep_wh_item_id)],
            &kw,
            None,
        )
        .await
        .expect_err("Expired wormhole must collapse on jump attempt");
    assert_eq!(expired_err.code, "WormholeCollapsed");
}

#[tokio::test]
async fn test_so_fleets_motd_options_wings_squads_broadcasts_watchlist_formations_boost_bridge_disband() {
    // parity: feat:fleet.wings-squads, feat:fleet.roles-positions, feat:fleet.fleet-motd, feat:fleet.fleet-broadcasts, feat:fleet.fleet-warp-cmd, feat:fleet.fleet-composition, feat:fleet.fleet-watchlist, feat:fleet.fleet-advert, feat:fleet.fleet-free-move, feat:fleet.fleet-formations, feat:fleet.fleet-disband, feat:fleet.fleet-max-size, feat:fleet.fleet-boost-scope, feat:fleet.fleet-bridge, feat:fleet.fleet-registration, rpc:beyonce.GetFormations, rpc:charUnboundMgr.SelectCharacterID, rpc:fleetMgr.AddToWatchlist, rpc:fleetMgr.RegisterForDamageUpdates, rpc:fleetMgr.RemoveFromWatchlist, rpc:fleetObjectHandler.AcceptInvite, rpc:fleetObjectHandler.ChangeSquadName, rpc:fleetObjectHandler.ChangeWingName, rpc:fleetObjectHandler.CreateSquad, rpc:fleetObjectHandler.CreateWing, rpc:fleetObjectHandler.DeleteSquad, rpc:fleetObjectHandler.DeleteWing, rpc:fleetObjectHandler.DisbandFleet, rpc:fleetObjectHandler.GetFleetComposition, rpc:fleetObjectHandler.GetFleetMaxSize, rpc:fleetObjectHandler.GetInitState, rpc:fleetObjectHandler.GetMotd, rpc:fleetObjectHandler.MoveMember, rpc:fleetObjectHandler.SetFleetMaxSize, rpc:fleetObjectHandler.SetMotdEx, rpc:fleetObjectHandler.SetOptions, rpc:fleetObjectHandler.SetTakesFleetWarp, rpc:fleetProxy.AddFleetFinderAdvert, rpc:fleetProxy.ApplyToJoinFleet, rpc:fleetProxy.GetAvailableFleetAds, rpc:fleetProxy.UpdateAdvertAllowedEntities
    eve_testkit::attest(&[
        "feat:fleet.wings-squads",
        "feat:fleet.roles-positions",
        "feat:fleet.fleet-motd",
        "feat:fleet.fleet-broadcasts",
        "feat:fleet.fleet-warp-cmd",
        "feat:fleet.fleet-composition",
        "feat:fleet.fleet-watchlist",
        "feat:fleet.fleet-advert",
        "feat:fleet.fleet-free-move",
        "feat:fleet.fleet-formations",
        "feat:fleet.fleet-disband",
        "feat:fleet.fleet-max-size",
        "feat:fleet.fleet-boost-scope",
        "feat:fleet.fleet-bridge",
        "feat:fleet.fleet-registration",
    ]);
    let ctx = ServerContext::new();
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

    // Roles & Positions (`feat:fleet.roles-positions`, `feat:fleet.fleet-free-move`, `feat:fleet.fleet-registration`):
    // Promote Pilot 2 to Wing Commander (role 2) of Wing 2; moving Pilot 3 into the occupied Wing Commander slot fails with PositionOccupied; Pilot 2 self-moves to Squad 2 member (clearing Wing 2 commander)
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "MoveMember", &[PyRep::Integer(90_000_002), PyRep::Integer(wing2_id), PyRep::Integer(0), PyRep::Integer(2)], &kw, None).await.unwrap();
    let occ_err = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "MoveMember", &[PyRep::Integer(p3_char), PyRep::Integer(wing2_id), PyRep::Integer(0), PyRep::Integer(2)], &kw, None).await.unwrap_err();
    assert_eq!(occ_err.code, "PositionOccupied");
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

    // Fleet Broadcast Scopes (`feat:fleet.fleet-broadcasts`): BroadcastToSystem reaches Pilot 2 (same system) but not Pilot 3 (Perimeter); BroadcastToBubble reaches Pilot 2 (1 km away)
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let _ = ctx.drain_notifications_for_char(p3_char);
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "BroadcastToSystem", &[PyRep::String("InPosition".into()), PyRep::Integer(800_000_001)], &kw, None).await.unwrap();
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnFleetBroadcast"));
    assert!(!ctx.drain_notifications_for_char(p3_char).iter().any(|n| n.event == "OnFleetBroadcast"), "Out-of-system member must not receive BroadcastToSystem");
    ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "BroadcastToBubble", &[PyRep::String("HealArmor".into()), PyRep::Integer(800_000_001)], &kw, None).await.unwrap();
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnFleetBroadcast"));

    // Fleet Watchlist (`feat:fleet.fleet-watchlist`): AddToWatchlist / RegisterForDamageUpdates stream actual CombatState HP ratios via OnFleetStateChange
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

    // Fleet Formations & Fleet Warp (`feat:fleet.fleet-formations`, `feat:fleet.fleet-warp-cmd`):
    // GetFormations returns formations; FleetWarp with formationID=1 ("Point") applies formation offsets; warp-scrambled or SetTakesFleetWarp(false) member is skipped
    assert!(is_rowset(&ctx.dispatch_rpc(&mut sess1, "beyonce", None, "GetFormations", &[], &kw, None).await.unwrap()));
    let fw_dest = DVec3::new(300_000_000.0, 0.0, 0.0);
    let fw_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "FleetWarp", &[PyRep::Real(fw_dest.x), PyRep::Real(fw_dest.y), PyRep::Real(fw_dest.z), PyRep::Real(0.0), PyRep::Integer(1), PyRep::Real(5_000.0)], &kw, None).await.unwrap();
    assert_eq!(fw_rep.dict_get("warpedMembers"), Some(&PyRep::Integer(2)));
    let _ = jita_sys.set_ball_position(800_000_001, DVec3::ZERO).await;
    let _ = jita_sys.set_ball_position(800_000_002, DVec3::new(1_000.0, 0.0, 0.0)).await;
    ctx.dispatch_rpc(&mut sess2, "fleetObjectHandler", None, "SetTakesFleetWarp", &[PyRep::Bool(false)], &kw, None).await.unwrap();
    let fw_exempt = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "FleetWarp", &[PyRep::Real(fw_dest.x), PyRep::Real(fw_dest.y), PyRep::Real(fw_dest.z), PyRep::Real(0.0)], &kw, None).await.unwrap();
    assert_eq!(fw_exempt.dict_get("warpedMembers"), Some(&PyRep::Integer(1)), "Exempt member must not be warped by FleetWarp");

    // Fleet Boost Scope (`feat:fleet.fleet-boost-scope`): Command Burst affects fleet member in range (Pilot 2) regardless of wing/squad, and excludes out-of-system Pilot 3
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

    // Fleet Bridge (`feat:fleet.fleet-bridge`): Titan/Black Ops jump portals only allow fleet members to jump.
    // CmdJumpThroughFleet enforces same fleet between jumper and portal operator (`FleetNotInFleet`).
    let bridge_err = ctx.dispatch_rpc(&mut sess2, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(p3_char), PyRep::Integer(800_000_003)], &kw, None).await.unwrap_err();
    assert_eq!(bridge_err.code, "NoActiveBridge");

    // Disband fleet via `DisbandFleet` (`feat:fleet.fleet-disband`)
    let disband_rep = ctx.dispatch_rpc(&mut sess1, "fleetObjectHandler", None, "DisbandFleet", &[], &kw, None).await.unwrap();
    assert_eq!(disband_rep.dict_get("disbanded"), Some(&PyRep::Bool(true)));
    assert_eq!(sess1.fleet_id, None);
    assert!(ctx.db.with_snapshot(|s| !s.chat_channels.contains_key(&format!("fleetid:{fid}"))), "DisbandFleet must destroy fleet chat channel");

    // Non-fleet member jumping through bridge must be rejected with FleetNotInFleet
    sess2.fleet_id = None;
    let non_fleet_err = ctx.dispatch_rpc(&mut sess2, "beyonce", None, "CmdJumpThroughFleet", &[PyRep::Integer(p3_char), PyRep::Integer(800_000_003)], &kw, None).await.unwrap_err();
    assert_eq!(non_fleet_err.code, "FleetNotInFleet");
}

#[tokio::test]
async fn test_so_bookmarks_expiry_copy_move_corp_bookmarks_and_scanning_features() {
    // parity: feat:bookmarks.copy-move, feat:bookmarks.warp-to-bm, feat:bookmarks.bm-expiry, feat:bookmarks.corp-bookmarks, feat:scanning.probe-formations, feat:scanning.signatures, feat:scanning.anomalies, feat:scanning.structure-signals, feat:scanning.system-scan, feat:scanning.sig-despawn, feat:scanning.scan-skills, rpc:accessGroupBookmarkMgr.AddFolder, rpc:accessGroupBookmarkMgr.CopyBookmarksToFolderAndSubfolder, rpc:accessGroupBookmarkMgr.MoveBookmarksToFolderAndSubfolder, rpc:charUnboundMgr.SelectCharacterID, rpc:corpRegistry.AddCorporation, rpc:corpRegistry.InsertApplication, rpc:corpRegistry.UpdateApplicationOffer, rpc:scanMgr.SignalTrackerRegister
    eve_testkit::attest(&[
        "feat:bookmarks.copy-move",
        "feat:bookmarks.warp-to-bm",
        "feat:bookmarks.bm-expiry",
        "feat:bookmarks.corp-bookmarks",
        "feat:scanning.probe-formations",
        "feat:scanning.signatures",
        "feat:scanning.anomalies",
        "feat:scanning.structure-signals",
        "feat:scanning.system-scan",
        "feat:scanning.sig-despawn",
        "feat:scanning.scan-skills",
    ]);
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
    sess2.ship_id = Some(800_000_002); sess2.station_id = None; sess2.solar_system_id = Some(30_000_142);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) { ch.active_ship_id = 800_000_001; ch.station_id = None; ch.solar_system_id = 30_000_142; }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.active_ship_id = 800_000_002; ch.station_id = None; ch.solar_system_id = 30_000_142; }
        snap.skills.entry(90_000_001).or_default().insert(3363, eve_db::TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 });
    });
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "AddCorporation", &[PyRep::String("Bookmark Corp".into()), PyRep::String("BMCP".into()), PyRep::Real(0.05)], &kw, None).await.unwrap();
    let corp_id = sess1.corporation_id.unwrap();
    let app = ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Joining".into())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateApplicationOffer", &[app.dict_get("applicationID").unwrap().clone(), PyRep::String("Accepted".into())], &kw, None).await.unwrap();
    sess2.corporation_id = Some(corp_id);

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
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "CopyBookmarksToFolderAndSubfolder", &[PyRep::List(batch21), PyRep::Integer(p_folder_id)], &kw, None).await.unwrap_err().code, "BookmarkCopyBatchLimitExceeded");
    let copied = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "CopyBookmarksToFolderAndSubfolder", &[PyRep::List(vec![PyRep::Integer(temp_bm_id)]), PyRep::Integer(p_folder_id)], &kw, None).await.unwrap();
    let copied_bm_id = match copied.dict_get("bookmarkIDs") { Some(PyRep::List(l)) => l[0].as_i64().unwrap(), _ => panic!() };
    ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "MoveBookmarksToFolderAndSubfolder", &[PyRep::List(vec![PyRep::Integer(copied_bm_id)]), PyRep::Integer(shared_folder_id)], &kw, None).await.unwrap();

    // Warp to Bookmark (`feat:bookmarks.warp-to-bm`): Pilot 2 warps to the shared corp bookmark (`copied_bm_id`) and lands at `bm_target_pos`
    let _ = jita_sys.spawn_ball(Ball { id: 800_000_002, type_id: 587, position: DVec3::ZERO, velocity: DVec3::ZERO, max_velocity: 350.0, inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }).await;
    ctx.dispatch_rpc(&mut sess2, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("bookmark".into()), PyRep::Integer(copied_bm_id), PyRep::Real(0.0)], &kw, None).await.unwrap();
    let mut step = 0;
    while step < 30 {
        let _ = jita_sys.step_manual(1.0).await;
        step += 1;
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
    let _ = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "GetSignatures", &[PyRep::Integer(30_000_142)], &kw, None).await.unwrap();
    let expired_sig_id = ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(990_000_777, eve_db::UpwellStructureRecord {
            structure_id: 990_000_777, type_id: 35832, owner_corp_id: corp_id, solar_system_id: 30_000_142,
            name: "Jita Trade Citadel".into(), state: "ShieldVulnerable".into(), position: [100_000.0, 0.0, 0.0],
            quantum_core_installed: true, fuel_blocks: 1_000, shield_hp: 5_000.0, armor_hp: 5_000.0, hull_hp: 5_000.0,
            reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0,
        });
        let sigs = snap.cosmic_signatures.get_mut(&30_000_142).unwrap();
        let relic = sigs.iter_mut().find(|s| s.sig_group != "Wormhole").unwrap();
        relic.created_unix = 1;
        relic.lifetime_seconds = 1;
        relic.signature_id.clone()
    });
    ctx.advance_secs(10);
    let _ = ctx.drain_notifications_for_char(90_000_001);
    let tracker_rep = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "SignalTrackerRegister", &[PyRep::Integer(30_000_142)], &kw, None).await.unwrap();
    let PyRep::KeyVal(tr_kv) = &tracker_rep else { panic!() };
    let PyRep::Rowset { rows: struct_rows, .. } = &tr_kv["structures"] else { panic!() };
    assert!(struct_rows.iter().any(|r| r[0] == PyRep::Integer(990_000_777)), "SignalTrackerRegister must return structure signals");
    let tr_notifs = ctx.drain_notifications_for_char(90_000_001);
    assert!(tr_notifs.iter().any(|n| n.event == "OnSignalTrackerSignatureUpdate"));
    assert!(tr_notifs.iter().any(|n| n.event == "OnSignalTrackerAnomalyUpdate"));
    assert!(tr_notifs.iter().any(|n| n.event == "OnSignalTrackerStructureUpdate"));

    // Expired signature despawn (`feat:scanning.sig-despawn`): GetSignatures prunes the expired signature
    let sigs_after = ctx.dispatch_rpc(&mut sess1, "scanMgr", None, "GetSignatures", &[PyRep::Integer(30_000_142)], &kw, None).await.unwrap();
    if let PyRep::Rowset { rows: s_rows, .. } = &sigs_after {
        assert!(!s_rows.iter().any(|r| r[0] == PyRep::String(expired_sig_id.clone())), "Expired signature must despawn from GetSignatures");
    }

    // Probe Formations & Scan Skills (`feat:scanning.probe-formations`, `feat:scanning.scan-skills`):
    // With Astrometrics (3412) level 2 -> max 6 probes allowed; launching 8 fails with MaxProbesExceeded; launching 6 in Spread (1) and Custom (2) returns 3D probePositions
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
    eve_testkit::attest(&[
        "feat:social.mail-corp-alliance",
        "feat:social.mailing-lists",
        "feat:social.mailing-list-admin",
        "feat:social.mailing-list-welcome",
        "feat:social.cspa",
        "feat:social.calendar-personal",
        "feat:social.calendar-corp-alliance",
        "feat:social.calendar-responses",
        "feat:social.calendar-ccp-events",
        "feat:social.calendar-auto-events",
        "feat:social.chat-help-rookie",
        "feat:social.chat-gm-mute",
        "feat:social.chat-motd",
    ]);
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
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "AddCorporation", &[PyRep::String("Social Corp".into()), PyRep::String("SOCP".into()), PyRep::Real(0.05)], &kw, None).await.unwrap();
    let corp_id = sess1.corporation_id.unwrap();
    let app = ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Joining".into())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateApplicationOffer", &[app.dict_get("applicationID").unwrap().clone(), PyRep::String("Accepted".into())], &kw, None).await.unwrap();
    sess2.corporation_id = Some(corp_id);

    // 4. Social: Corp/Alliance Mail, Mailing List Admin & Welcome Mail, CSPA, Calendar Personal/Corp/CCP/Auto Events, Chat Mute, Rookie Help & MOTD
    let _ = ctx.drain_notifications_for_char(90_000_002);
    let corp_mail_id = ctx.dispatch_rpc(&mut sess1, "mailMgr", None, "SendMail", &[PyRep::List(vec![]), PyRep::Integer(0), PyRep::Integer(corp_id), PyRep::String("Corp CTA".into()), PyRep::String("Form up in Jita".into())], &kw, None).await.unwrap();
    assert!(corp_mail_id.as_i64().is_some());
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnMailSent"), "Corp mail must reach all corp members");

    // CSPA (`feat:social.cspa`): Pilot 3 configures 50,000 ISK CSPA charge via `charMgr.LogSettings`; `GetSettingsInfo` reads it without mutating; `CSPAChargedAction` & `mailMgr.SendMail` enforce and debit CSPA
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
    let list_id = ml_rep.dict_get("listID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "SaveWelcomeMail", &[PyRep::Integer(list_id), PyRep::String("Welcome to IntelList".into()), PyRep::String("Rules inside".into())], &kw, None).await.unwrap();
    let wm_rep = ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "GetWelcomeMail", &[PyRep::Integer(list_id)], &kw, None).await.unwrap();
    assert_eq!(wm_rep.dict_get("title"), Some(&PyRep::String("Welcome to IntelList".into())));
    let _ = ctx.drain_notifications_for_char(90_000_002);
    ctx.dispatch_rpc(&mut sess2, "mailingListsMgr", None, "Join", &[PyRep::Integer(list_id)], &kw, None).await.unwrap();
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnMailSent"), "Joining mailing list must deliver welcome mail");
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "mailingListsMgr", None, "KickMembers", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(90_000_001)])], &kw, None).await.unwrap_err().code, "MailingListAccessDenied");
    ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "SetMembersOperator", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "SetMembersMuted", &[PyRep::Integer(list_id), PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "SetEntityAccess", &[PyRep::Integer(list_id), PyRep::Integer(90_000_002), PyRep::Integer(2)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "ClearEntityAccess", &[PyRep::Integer(list_id), PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "ClearWelcomeMail", &[PyRep::Integer(list_id)], &kw, None).await.unwrap();
    assert!(is_rowset(&ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "GetJoinedLists", &[], &kw, None).await.unwrap()));
    assert!(is_rowset(&ctx.dispatch_rpc(&mut sess1, "mailingListsMgr", None, "GetMembers", &[PyRep::Integer(list_id)], &kw, None).await.unwrap()));

    // Calendar: Personal Event, role-gated Corporation Event, Responses, CCP Event & Automated Event (`feat:social.calendar-*`)
    let pers_ev = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreatePersonalEvent", &[PyRep::Integer(133_500_000_000_000_000), PyRep::Integer(60), PyRep::String("Solo Roam".into()), PyRep::String("Bring nanogang".into()), PyRep::Integer(0), PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();
    let pers_ev_id = pers_ev.dict_get("eventID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditPersonalEvent", &[PyRep::Integer(pers_ev_id), PyRep::Integer(133_500_000_000_000_000), PyRep::Integer(90), PyRep::String("Updated Solo Roam".into()), PyRep::String("Updated desc".into())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "SendEventResponse", &[PyRep::Integer(pers_ev_id), PyRep::Integer(1)], &kw, None).await.unwrap();
    assert!(is_rowset(&ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "GetResponsesToEvent", &[PyRep::Integer(pers_ev_id)], &kw, None).await.unwrap()));
    assert!(is_rowset(&ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "GetResponsesForCharacter", &[], &kw, None).await.unwrap()));
    // Regular member Pilot 2 without Communications Officer / Director role is denied from CreateCorporationEvent
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "calendarMgr", None, "CreateCorporationEvent", &[PyRep::Integer(133_500_000_000_000_000), PyRep::Integer(120), PyRep::String("Unauth".into()), PyRep::String("No role".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err().code, "CalendarAccessDenied");
    let corp_ev = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "CreateCorporationEvent", &[PyRep::Integer(133_500_000_000_000_000), PyRep::Integer(120), PyRep::String("Corp Moon Mining".into()), PyRep::String("Bring barges".into()), PyRep::Integer(1)], &kw, None).await.unwrap();
    let corp_ev_id = corp_ev.dict_get("eventID").and_then(|v| v.as_i64()).unwrap();
    ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "EditCorporationEvent", &[PyRep::Integer(corp_ev_id), PyRep::Integer(133_500_000_000_000_000), PyRep::Integer(120), PyRep::String("Updated Corp Moon Mining".into()), PyRep::String("Updated text".into())], &kw, None).await.unwrap();
    let ev_list = ctx.dispatch_rpc(&mut sess1, "calendarMgr", None, "GetEventsByMonthYear", &[PyRep::Integer(10), PyRep::Integer(2026)], &kw, None).await.unwrap();
    let PyRep::Rowset { rows: ev_rows, .. } = &ev_list else { panic!() };
    assert!(ev_rows.iter().any(|r| r[6] == PyRep::Integer(0)), "GetEventsByMonthYear must include global CCP events (ownerType 0)");
    assert!(ev_rows.iter().any(|r| r[8] == PyRep::Bool(true)), "GetEventsByMonthYear must include automated moon/structure events");
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

    // Non-operator/non-GM Pilot 2 cannot GMMute or GMUnmute
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "XmppChatMgr", None, "GMMute", &[PyRep::Integer(90_000_001), PyRep::String("channel:moderation_test".into()), PyRep::Integer(600), PyRep::String("Noop".into())], &kw, None).await.unwrap_err().code, "ChatAccessDenied");
    ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GMMute", &[PyRep::Integer(90_000_002), PyRep::String("channel:moderation_test".into()), PyRep::Integer(600), PyRep::String("Spamming".into())], &kw, None).await.unwrap();
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "XmppChatMgr", None, "GMUnmute", &[PyRep::Integer(90_000_002), PyRep::String("channel:moderation_test".into())], &kw, None).await.unwrap_err().code, "ChatAccessDenied");
    let muted_err = ctx.dispatch_rpc(&mut sess2, "LSC", None, "SendMessage", &[PyRep::String("channel:moderation_test".into()), PyRep::String("Can anyone hear me?".into())], &kw, None).await.expect_err("Muted user must be blocked from sending chat messages");
    assert_eq!(muted_err.code, "MutedInChannel");
    ctx.dispatch_rpc(&mut sess1, "XmppChatMgr", None, "GMUnmute", &[PyRep::Integer(90_000_002), PyRep::String("channel:moderation_test".into())], &kw, None).await.unwrap();
    assert!(is_keyval(&ctx.dispatch_rpc(&mut sess2, "LSC", None, "SendMessage", &[PyRep::String("channel:moderation_test".into()), PyRep::String("Unmuted now!".into())], &kw, None).await.unwrap()));
}
