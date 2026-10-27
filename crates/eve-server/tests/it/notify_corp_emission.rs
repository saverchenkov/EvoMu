//! Corporation, governance, medal, alliance, and war notification emission tests.
//! Verifies V24.01 payload layouts and recipient scoping (`NotificationTarget::Corporation`,
//! `NotificationTarget::Character`, `NotificationTarget::Alliance`) for:
//! - `OnCorporationChanged`, `OnCorporationMemberChanged`, `OnCorporationApplicationChanged`
//! - `OnCorporationRecruitmentAdChanged`, `OnCorporationWelcomeMailChanged`, `OnCorpAggressionSettingsChange`
//! - `OnTitleChanged`, `OnShareChange`, `OnVoteCaseChanged`, `OnVoteCast`, `OnSanctionedActionChanged`
//! - `OnCorporationMedalAdded`, `OnMedalIssued`, `OnMedalStatusChanged`
//! - `OnAllianceChanged`, `OnAllianceApplicationChanged`, `OnAllianceMemberChanged`, `OnAllianceRelationshipChanged`
//! - `OnWarChanged`

use crate::common;

use eve_db::IskCents;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;
use std::sync::Arc;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn select(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut s = SessionState::new(sid);
    common::login_pilot(ctx, &mut s, user, pass).await;
    ctx.dispatch_rpc(
        &mut s,
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
    s
}

fn events(ctx: &ServerContext, char_id: i64, event: &str) -> Vec<Vec<PyRep>> {
    ctx.drain_notifications_for_char(char_id)
        .into_iter()
        .filter(|n| n.event == event)
        .map(|n| n.args)
        .collect()
}

fn fund_pilot(ctx: &ServerContext, char_id: i64, isk: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.balance = IskCents::from_isk(isk);
        }
    });
}

#[tokio::test]
async fn test_corporation_lifecycle_recruitment_and_aggression_notifications() {
    // parity: notify:OnCorporationChanged, notify:OnCorporationMemberChanged, notify:OnCorporationApplicationChanged, notify:OnCorporationRecruitmentAdChanged, notify:OnCorporationWelcomeMailChanged, notify:OnCorpAggressionSettingsChange, rpc:corpRegistry.UpdateCorporation, rpc:corpRegistry.SetCorpWelcomeMail
    eve_testkit::attest(&[
        "notify:OnCorporationChanged",
        "notify:OnCorporationMemberChanged",
        "notify:OnCorporationApplicationChanged",
        "notify:OnCorporationRecruitmentAdChanged",
        "notify:OnCorporationWelcomeMailChanged",
        "notify:OnCorpAggressionSettingsChange",
    ]);
    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    fund_pilot(&ctx, P1, 500_000_000);
    let mut s1 = select(&ctx, 801, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 802, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("corp_p3", "corp_p3-secret");
    let p3 = ctx
        .db
        .create_character(uid3, "Corp Outsider Three", 1, 1, 11, 17, &ctx.sde)
        .expect("create p3")
        .character_id;
    let _s3 = select(&ctx, 803, "corp_p3", "corp_p3-secret", p3).await;
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // AddCorporation -> OnCorporationChanged(corpID, change_dict) to founder P1 only.
    let created = ctx
        .dispatch_rpc(
            &mut s1,
            "corpRegistry",
            None,
            "AddCorporation",
            &[PyRep::String("Apex Logistics".into()), PyRep::String("APXL".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("AddCorporation");
    let corp_id = created.dict_get("corporationID").and_then(PyRep::as_i64).expect("corp_id");
    assert_eq!(
        events(&ctx, P1, "OnCorporationChanged"),
        vec![vec![
            PyRep::Integer(corp_id),
            PyRep::Dict(vec![
                (PyRep::String(("corporationID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(corp_id)])),
                (PyRep::String(("corporationName").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String("Apex Logistics".into())])),
                (PyRep::String(("tickerName").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String("APXL".into())])),
                (PyRep::String(("ceoID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(P1)])),
            ]),
        ]]
    );
    assert!(events(&ctx, P2, "OnCorporationChanged").is_empty());
    assert!(events(&ctx, p3, "OnCorporationChanged").is_empty());

    // InsertApplication by P2 -> OnCorporationApplicationChanged(corpID, applicantID, applicationID, newApplication)
    // to corp member P1 and applicant P2; uninvolved P3 gets nothing.
    let now_ts = ctx.now_unix() as i64;
    let app = ctx
        .dispatch_rpc(
            &mut s2,
            "corpRegistry",
            None,
            "InsertApplication",
            &[PyRep::Integer(corp_id), PyRep::String("Ready to haul".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("InsertApplication");
    let app_id = app.dict_get("applicationID").and_then(PyRep::as_i64).expect("app_id");
    let expected_applied = vec![
        PyRep::Integer(corp_id),
        PyRep::Integer(P2),
        PyRep::Integer(app_id),
        PyRep::keyval([
            ("applicationID", PyRep::Integer(app_id)),
            ("corporationID", PyRep::Integer(corp_id)),
            ("characterID", PyRep::Integer(P2)),
            ("applicationText", PyRep::String("Ready to haul".into())),
            ("status", PyRep::String("Applied".into())),
            ("applicationDateTime", PyRep::Integer(now_ts)),
        ]),
    ];
    assert_eq!(events(&ctx, P1, "OnCorporationApplicationChanged"), vec![expected_applied.clone()]);
    assert_eq!(events(&ctx, P2, "OnCorporationApplicationChanged"), vec![expected_applied]);
    assert!(events(&ctx, p3, "OnCorporationApplicationChanged").is_empty());

    // UpdateApplicationOffer(app_id, "Accepted") by CEO P1 -> OnCorporationMemberChanged(corp_id, P2, {"corporationID": (1000009, corp_id)})
    // and OnCorporationApplicationChanged(..., status="Accepted") to P1 and P2; P3 gets nothing.
    ctx.dispatch_rpc(
        &mut s1,
        "corpRegistry",
        None,
        "UpdateApplicationOffer",
        &[PyRep::Integer(app_id), PyRep::String("Accepted".into())],
        &HashMap::new(),
        None,
    )
    .await
    .expect("UpdateApplicationOffer");
    s2.corporation_id = Some(corp_id);
    let expected_join = vec![
        PyRep::Integer(corp_id),
        PyRep::Integer(P2),
        PyRep::Dict(vec![(
            PyRep::String(("corporationID").into()),
            PyRep::Tuple(vec![PyRep::Integer(1_000_045), PyRep::Integer(corp_id)]),
        )]),
    ];
    let p1_accept_notifs = ctx.drain_notifications_for_char(P1);
    let p2_accept_notifs = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        p1_accept_notifs.iter().filter(|n| n.event == "OnCorporationMemberChanged").map(|n| n.args.clone()).collect::<Vec<_>>(),
        vec![expected_join.clone()]
    );
    assert_eq!(
        p2_accept_notifs.iter().filter(|n| n.event == "OnCorporationMemberChanged").map(|n| n.args.clone()).collect::<Vec<_>>(),
        vec![expected_join]
    );
    assert!(events(&ctx, p3, "OnCorporationMemberChanged").is_empty());

    let expected_accepted = vec![
        PyRep::Integer(corp_id),
        PyRep::Integer(P2),
        PyRep::Integer(app_id),
        PyRep::keyval([
            ("applicationID", PyRep::Integer(app_id)),
            ("corporationID", PyRep::Integer(corp_id)),
            ("characterID", PyRep::Integer(P2)),
            ("applicationText", PyRep::String("Ready to haul".into())),
            ("status", PyRep::String("Accepted".into())),
            ("applicationDateTime", PyRep::Integer(now_ts)),
        ]),
    ];
    assert_eq!(
        p1_accept_notifs.iter().filter(|n| n.event == "OnCorporationApplicationChanged").map(|n| n.args.clone()).collect::<Vec<_>>(),
        vec![expected_accepted.clone()]
    );
    assert_eq!(
        p2_accept_notifs.iter().filter(|n| n.event == "OnCorporationApplicationChanged").map(|n| n.args.clone()).collect::<Vec<_>>(),
        vec![expected_accepted]
    );
    assert!(events(&ctx, p3, "OnCorporationApplicationChanged").is_empty());

    // UpdateCorporation: non-director P2 is rejected with CorpRoleDenied and emits nothing; CEO P1 succeeds.
    assert_eq!(
        ctx.dispatch_rpc(
            &mut s2,
            "corpRegistry",
            None,
            "UpdateCorporation",
            &[PyRep::String("Hijacked".into()), PyRep::String("https://bad".into()), PyRep::Real(0.5)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("non-director denied")
        .code,
        "CorpRoleDenied"
    );
    assert!(events(&ctx, P1, "OnCorporationChanged").is_empty());

    ctx.dispatch_rpc(
        &mut s1,
        "corpRegistry",
        None,
        "UpdateCorporation",
        &[PyRep::String("Wormhole Freight".into()), PyRep::String("https://apxl.eve".into()), PyRep::Real(0.125)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("UpdateCorporation");
    let expected_corp_update = vec![
        PyRep::Integer(corp_id),
        PyRep::Dict(vec![
            (PyRep::String(("description").into()), PyRep::Tuple(vec![PyRep::String(String::new()), PyRep::String("Wormhole Freight".into())])),
            (PyRep::String(("url").into()), PyRep::Tuple(vec![PyRep::String(String::new()), PyRep::String("https://apxl.eve".into())])),
            (PyRep::String(("taxRate").into()), PyRep::Tuple(vec![PyRep::Real(0.1), PyRep::Real(0.125)])),
        ]),
    ];
    assert_eq!(events(&ctx, P1, "OnCorporationChanged"), vec![expected_corp_update.clone()]);
    assert_eq!(events(&ctx, P2, "OnCorporationChanged"), vec![expected_corp_update]);
    assert!(events(&ctx, p3, "OnCorporationChanged").is_empty());

    // UpdateMember(P2, 128) -> OnCorporationMemberChanged(corp_id, P2, {"roles": (0, 128)}) to P1 and P2 only.
    ctx.dispatch_rpc(
        &mut s1,
        "corpRegistry",
        None,
        "UpdateMember",
        &[PyRep::Integer(P2), PyRep::Integer(128)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("UpdateMember");
    let expected_role_change = vec![
        PyRep::Integer(corp_id),
        PyRep::Integer(P2),
        PyRep::Dict(vec![(
            PyRep::String(("roles").into()),
            PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(128)]),
        )]),
    ];
    assert_eq!(events(&ctx, P1, "OnCorporationMemberChanged"), vec![expected_role_change.clone()]);
    assert_eq!(events(&ctx, P2, "OnCorporationMemberChanged"), vec![expected_role_change]);
    assert!(events(&ctx, p3, "OnCorporationMemberChanged").is_empty());

    // SetCorpWelcomeMail -> OnCorporationWelcomeMailChanged(characterID, changeDate) to corp members P1 & P2 only.
    ctx.dispatch_rpc(
        &mut s1,
        "corpRegistry",
        None,
        "SetCorpWelcomeMail",
        &[PyRep::String("Welcome aboard".into()), PyRep::String("Read MOTD".into()), PyRep::Bool(true)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SetCorpWelcomeMail");
    let expected_welcome = vec![PyRep::Integer(P1), PyRep::Integer(now_ts)];
    assert_eq!(events(&ctx, P1, "OnCorporationWelcomeMailChanged"), vec![expected_welcome.clone()]);
    assert_eq!(events(&ctx, P2, "OnCorporationWelcomeMailChanged"), vec![expected_welcome]);
    assert!(events(&ctx, p3, "OnCorporationWelcomeMailChanged").is_empty());

    // CreateRecruitmentAd & DeleteRecruitmentAd -> OnCorporationRecruitmentAdChanged(corpID, adID) to P1 & P2 only.
    let ad = ctx
        .dispatch_rpc(
            &mut s1,
            "corpRecProxy",
            None,
            "CreateRecruitmentAd",
            &[PyRep::String("Industrialists Wanted".into()), PyRep::String("Ore buyback".into()), PyRep::Integer(7)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CreateRecruitmentAd");
    let ad_id = ad.dict_get("adID").and_then(PyRep::as_i64).expect("ad_id");
    let expected_ad = vec![PyRep::Integer(corp_id), PyRep::Integer(ad_id)];
    assert_eq!(events(&ctx, P1, "OnCorporationRecruitmentAdChanged"), vec![expected_ad.clone()]);
    assert_eq!(events(&ctx, P2, "OnCorporationRecruitmentAdChanged"), vec![expected_ad.clone()]);
    assert!(events(&ctx, p3, "OnCorporationRecruitmentAdChanged").is_empty());

    ctx.dispatch_rpc(&mut s1, "corpRecProxy", None, "DeleteRecruitmentAd", &[PyRep::Integer(ad_id)], &HashMap::new(), None)
        .await
        .expect("DeleteRecruitmentAd");
    assert_eq!(events(&ctx, P1, "OnCorporationRecruitmentAdChanged"), vec![expected_ad.clone()]);
    assert_eq!(events(&ctx, P2, "OnCorporationRecruitmentAdChanged"), vec![expected_ad]);
    assert!(events(&ctx, p3, "OnCorporationRecruitmentAdChanged").is_empty());

    // RegisterNewAggressionSettings -> OnCorpAggressionSettingsChange(aggressionSettings) to P1 & P2 only.
    ctx.dispatch_rpc(
        &mut s1,
        "corpRegistry",
        None,
        "RegisterNewAggressionSettings",
        &[PyRep::Bool(true)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("RegisterNewAggressionSettings");
    let expected_aggr = vec![PyRep::keyval([
        ("corporationID", PyRep::Integer(corp_id)),
        ("isFriendlyFireLegal", PyRep::Bool(true)),
        ("changeAtTime", PyRep::Integer(now_ts + 86_400)),
    ])];
    assert_eq!(events(&ctx, P1, "OnCorpAggressionSettingsChange"), vec![expected_aggr.clone()]);
    assert_eq!(events(&ctx, P2, "OnCorpAggressionSettingsChange"), vec![expected_aggr]);
    assert!(events(&ctx, p3, "OnCorpAggressionSettingsChange").is_empty());
}

#[tokio::test]
async fn test_corporation_governance_shares_titles_votes_and_medals_notifications() {
    // parity: notify:OnTitleChanged, notify:OnShareChange, notify:OnVoteCaseChanged, notify:OnVoteCast, notify:OnSanctionedActionChanged, notify:OnCorporationMedalAdded, notify:OnMedalIssued, notify:OnMedalStatusChanged, rpc:corpRegistry.DeleteTitle, rpc:corpRegistry.MoveCompanyShares
    eve_testkit::attest(&[
        "notify:OnTitleChanged",
        "notify:OnShareChange",
        "notify:OnVoteCaseChanged",
        "notify:OnVoteCast",
        "notify:OnSanctionedActionChanged",
        "notify:OnCorporationMedalAdded",
        "notify:OnMedalIssued",
        "notify:OnMedalStatusChanged",
    ]);
    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    fund_pilot(&ctx, P1, 500_000_000);
    let mut s1 = select(&ctx, 811, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 812, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("gov_p3", "gov_p3-secret");
    let p3 = ctx
        .db
        .create_character(uid3, "Gov Outsider Three", 1, 1, 11, 17, &ctx.sde)
        .expect("create p3")
        .character_id;
    let _s3 = select(&ctx, 813, "gov_p3", "gov_p3-secret", p3).await;

    let created = ctx
        .dispatch_rpc(
            &mut s1,
            "corpRegistry",
            None,
            "AddCorporation",
            &[PyRep::String("Sovereign Syndicate".into()), PyRep::String("SOVS".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("AddCorporation");
    let corp_id = created.dict_get("corporationID").and_then(PyRep::as_i64).expect("corp_id");
    assert_eq!(
        ctx.dispatch_rpc(
            &mut s1,
            "corpRegistry",
            None,
            "InsertInvitation",
            &[PyRep::Integer(P2), PyRep::Bool(true)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("force-accepting invitation for another character must be denied")
        .code,
        "AccessDenied"
    );
    let inv_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "corpRegistry",
            None,
            "InsertInvitation",
            &[PyRep::Integer(P2), PyRep::Bool(false)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("InsertInvitation");
    let inv_app_id = inv_rep.dict_get("applicationID").and_then(PyRep::as_i64).expect("inv_app_id");
    assert_eq!(
        ctx.dispatch_rpc(
            &mut s1,
            "corpRegistry",
            None,
            "UpdateApplicationOffer",
            &[PyRep::Integer(inv_app_id), PyRep::String("Accepted".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("only invited character may accept invitation")
        .code,
        "AccessDenied"
    );
    ctx.dispatch_rpc(
        &mut s2,
        "corpRegistry",
        None,
        "UpdateApplicationOffer",
        &[PyRep::Integer(inv_app_id), PyRep::String("Accepted".into())],
        &HashMap::new(),
        None,
    )
    .await
    .expect("P2 accepts invitation");
    s2.corporation_id = Some(corp_id);
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // UpdateTitle & DeleteTitle -> OnTitleChanged(corpID, titleID, change) to corp members P1 & P2 only.
    assert_eq!(
        ctx.dispatch_rpc(
            &mut s2,
            "corpRegistry",
            None,
            "UpdateTitle",
            &[PyRep::Integer(2), PyRep::String("Fleet Commander".into()), PyRep::Integer(256)],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("non-director denied")
        .code,
        "CorpRoleDenied"
    );
    assert!(events(&ctx, P1, "OnTitleChanged").is_empty());

    ctx.dispatch_rpc(
        &mut s1,
        "corpRegistry",
        None,
        "UpdateTitle",
        &[PyRep::Integer(2), PyRep::String("Fleet Commander".into()), PyRep::Integer(256)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("UpdateTitle");
    let expected_title = vec![
        PyRep::Integer(corp_id),
        PyRep::Integer(2),
        PyRep::Dict(vec![
            (PyRep::String(("titleName").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String("Fleet Commander".into())])),
            (PyRep::String(("roles").into()), PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(256)])),
        ]),
    ];
    assert_eq!(events(&ctx, P1, "OnTitleChanged"), vec![expected_title.clone()]);
    assert_eq!(events(&ctx, P2, "OnTitleChanged"), vec![expected_title]);
    assert!(events(&ctx, p3, "OnTitleChanged").is_empty());

    ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "DeleteTitle", &[PyRep::Integer(2)], &HashMap::new(), None)
        .await
        .expect("DeleteTitle");
    let expected_del_title = vec![
        PyRep::Integer(corp_id),
        PyRep::Integer(2),
        PyRep::Dict(vec![
            (PyRep::String(("titleName").into()), PyRep::Tuple(vec![PyRep::String("Fleet Commander".into()), PyRep::String(("").into())])),
            (PyRep::String(("roles").into()), PyRep::Tuple(vec![PyRep::Integer(256), PyRep::Integer(0)])),
        ]),
    ];
    assert_eq!(events(&ctx, P1, "OnTitleChanged"), vec![expected_del_title.clone()]);
    assert_eq!(events(&ctx, P2, "OnTitleChanged"), vec![expected_del_title]);
    assert!(events(&ctx, p3, "OnTitleChanged").is_empty());

    // MoveCompanyShares(P2, 400) -> OnShareChange for P1 (1000 -> 600) and P2 (0 -> 400) to corp members P1 & P2.
    ctx.dispatch_rpc(
        &mut s1,
        "corpRegistry",
        None,
        "MoveCompanyShares",
        &[PyRep::Integer(P2), PyRep::Integer(400)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("MoveCompanyShares");
    let expected_shares = vec![
        vec![
            PyRep::Integer(corp_id),
            PyRep::Integer(corp_id),
            PyRep::Dict(vec![(PyRep::String(("shares").into()), PyRep::Tuple(vec![PyRep::Integer(1_000), PyRep::Integer(600)]))]),
        ],
        vec![
            PyRep::Integer(P2),
            PyRep::Integer(corp_id),
            PyRep::Dict(vec![(PyRep::String(("shares").into()), PyRep::Tuple(vec![PyRep::Integer(0), PyRep::Integer(400)]))]),
        ],
    ];
    assert_eq!(events(&ctx, P1, "OnShareChange"), expected_shares);
    assert_eq!(events(&ctx, P2, "OnShareChange"), expected_shares);
    assert!(events(&ctx, p3, "OnShareChange").is_empty());

    // InsertVoteCase -> OnVoteCaseChanged(corpID, voteCaseID) and OnSanctionedActionChanged(corpID, voteCaseID) to P1 & P2.
    let vc = ctx
        .dispatch_rpc(
            &mut s1,
            "voteManager",
            None,
            "InsertVoteCase",
            &[
                PyRep::String("Issue 500 Shares".into()),
                PyRep::String("Expand capital".into()),
                PyRep::Integer(5),
                PyRep::Integer(500),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .expect("InsertVoteCase");
    let case_id = vc.dict_get("voteCaseID").and_then(PyRep::as_i64).expect("case_id");
    let expected_vc = vec![PyRep::Integer(corp_id), PyRep::Integer(case_id)];
    let p1_vc_notifs = ctx.drain_notifications_for_char(P1);
    let p2_vc_notifs = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        p1_vc_notifs.iter().filter(|n| n.event == "OnVoteCaseChanged").map(|n| n.args.clone()).collect::<Vec<_>>(),
        vec![expected_vc.clone()]
    );
    assert_eq!(
        p2_vc_notifs.iter().filter(|n| n.event == "OnVoteCaseChanged").map(|n| n.args.clone()).collect::<Vec<_>>(),
        vec![expected_vc.clone()]
    );
    assert_eq!(
        p1_vc_notifs.iter().filter(|n| n.event == "OnSanctionedActionChanged").map(|n| n.args.clone()).collect::<Vec<_>>(),
        vec![expected_vc.clone()]
    );
    assert_eq!(
        p2_vc_notifs.iter().filter(|n| n.event == "OnSanctionedActionChanged").map(|n| n.args.clone()).collect::<Vec<_>>(),
        vec![expected_vc.clone()]
    );
    assert!(ctx.drain_notifications_for_char(p3).is_empty());

    // InsertVote -> OnVoteCast(corpID, voteCaseID) to P1 & P2.
    ctx.dispatch_rpc(
        &mut s1,
        "voteManager",
        None,
        "InsertVote",
        &[PyRep::Integer(case_id), PyRep::Integer(1)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("InsertVote");
    assert_eq!(events(&ctx, P1, "OnVoteCast"), vec![expected_vc.clone()]);
    assert_eq!(events(&ctx, P2, "OnVoteCast"), vec![expected_vc.clone()]);
    assert!(events(&ctx, p3, "OnVoteCast").is_empty());

    // ActivateSanctionedAction after voting window closes -> OnSanctionedActionChanged(corpID, voteCaseID) + OnShareChange.
    ctx.advance_virtual_clock(86_401);
    ctx.dispatch_rpc(
        &mut s1,
        "voteManager",
        None,
        "ActivateSanctionedAction",
        &[PyRep::Integer(case_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("ActivateSanctionedAction");
    let p1_act_notifs = ctx.drain_notifications_for_char(P1);
    let p2_act_notifs = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        p1_act_notifs.iter().filter(|n| n.event == "OnSanctionedActionChanged").map(|n| n.args.clone()).collect::<Vec<_>>(),
        vec![expected_vc.clone()]
    );
    assert_eq!(
        p2_act_notifs.iter().filter(|n| n.event == "OnSanctionedActionChanged").map(|n| n.args.clone()).collect::<Vec<_>>(),
        vec![expected_vc]
    );
    assert!(ctx.drain_notifications_for_char(p3).is_empty());
    let expected_minted = vec![vec![
        PyRep::Integer(corp_id),
        PyRep::Integer(corp_id),
        PyRep::Dict(vec![(PyRep::String(("shares").into()), PyRep::Tuple(vec![PyRep::Integer(600), PyRep::Integer(1_100)]))]),
    ]];
    assert_eq!(
        p1_act_notifs.iter().filter(|n| n.event == "OnShareChange").map(|n| n.args.clone()).collect::<Vec<_>>(),
        expected_minted
    );
    assert_eq!(
        p2_act_notifs.iter().filter(|n| n.event == "OnShareChange").map(|n| n.args.clone()).collect::<Vec<_>>(),
        expected_minted
    );

    // CreateMedal -> OnCorporationMedalAdded(medalID, corpID) to corp members P1 & P2.
    let medal = ctx
        .dispatch_rpc(
            &mut s1,
            "corporationSvc",
            None,
            "CreateMedal",
            &[PyRep::String("Crimson Star".into()), PyRep::String("Heroic defense".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CreateMedal");
    let medal_id = medal.dict_get("medalID").and_then(PyRep::as_i64).expect("medal_id");
    let expected_medal_added = vec![PyRep::Integer(medal_id), PyRep::Integer(corp_id)];
    assert_eq!(events(&ctx, P1, "OnCorporationMedalAdded"), vec![expected_medal_added.clone()]);
    assert_eq!(events(&ctx, P2, "OnCorporationMedalAdded"), vec![expected_medal_added]);
    assert!(events(&ctx, p3, "OnCorporationMedalAdded").is_empty());

    // GiveMedalToCharacters(medal_id, P2, "Fleet save") -> OnCorporationMedalAdded(medalID, P2) to corp (P1 & P2)
    // and OnMedalIssued(medalID, corpID, issuerID) to recipient P2 only.
    ctx.dispatch_rpc(
        &mut s1,
        "corporationSvc",
        None,
        "GiveMedalToCharacters",
        &[PyRep::Integer(medal_id), PyRep::Integer(P2), PyRep::String("Fleet save".into())],
        &HashMap::new(),
        None,
    )
    .await
    .expect("GiveMedalToCharacters");
    let expected_recipient_added = vec![PyRep::Integer(medal_id), PyRep::Integer(P2)];
    let p1_medal_notifs = ctx.drain_notifications_for_char(P1);
    let p2_medal_notifs = ctx.drain_notifications_for_char(P2);
    assert_eq!(
        p1_medal_notifs.iter().filter(|n| n.event == "OnCorporationMedalAdded").map(|n| n.args.clone()).collect::<Vec<_>>(),
        vec![expected_recipient_added.clone()]
    );
    assert_eq!(
        p2_medal_notifs.iter().filter(|n| n.event == "OnCorporationMedalAdded").map(|n| n.args.clone()).collect::<Vec<_>>(),
        vec![expected_recipient_added]
    );
    assert!(ctx.drain_notifications_for_char(p3).is_empty());
    assert_eq!(
        p2_medal_notifs.iter().filter(|n| n.event == "OnMedalIssued").map(|n| n.args.clone()).collect::<Vec<_>>(),
        vec![vec![PyRep::Integer(medal_id), PyRep::Integer(corp_id), PyRep::Integer(P1)]]
    );
    assert!(
        p1_medal_notifs.iter().all(|n| n.event != "OnMedalIssued"),
        "issuer does not receive OnMedalIssued"
    );

    // SetMedalStatus(medal_id, 3) by P2 -> OnMedalStatusChanged(medalID, 3) to P2 only.
    ctx.dispatch_rpc(
        &mut s2,
        "corporationSvc",
        None,
        "SetMedalStatus",
        &[PyRep::Integer(medal_id), PyRep::Integer(3)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SetMedalStatus");
    assert_eq!(
        events(&ctx, P2, "OnMedalStatusChanged"),
        vec![vec![PyRep::Integer(medal_id), PyRep::Integer(3)]]
    );
    assert!(events(&ctx, P1, "OnMedalStatusChanged").is_empty());
    assert!(events(&ctx, p3, "OnMedalStatusChanged").is_empty());
}
