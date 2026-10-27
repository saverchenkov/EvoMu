//! Alliance, war, and corporation governance notification emission tests (part 2 of `notify_corp_emission`).

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
async fn test_alliance_and_war_notifications() {
    // parity: notify:OnAllianceChanged, notify:OnAllianceApplicationChanged, notify:OnAllianceMemberChanged, notify:OnAllianceRelationshipChanged, notify:OnWarChanged, rpc:corpRegistry.CreateAlliance, rpc:corpRegistry.ApplyToJoinAlliance
    eve_testkit::attest(&[
        "notify:OnAllianceChanged",
        "notify:OnAllianceApplicationChanged",
        "notify:OnAllianceMemberChanged",
        "notify:OnAllianceRelationshipChanged",
        "notify:OnWarChanged",
    ]);
    let ctx = Arc::new(ServerContext::new());
    common::pin_clock(&ctx);
    fund_pilot(&ctx, P1, 5_000_000_000);
    fund_pilot(&ctx, P2, 5_000_000_000);
    let mut s1 = select(&ctx, 821, "demo", "demo-secret", P1).await;
    let mut s2 = select(&ctx, 822, "other", "other-secret", P2).await;
    let uid3 = ctx.db.provision_player_account("ally_p3", "ally_p3-secret");
    let p3 = ctx
        .db
        .create_character(uid3, "Alliance Outsider Three", 1, 1, 11, 17, &ctx.sde)
        .expect("create p3")
        .character_id;
    let _s3 = select(&ctx, 823, "ally_p3", "ally_p3-secret", p3).await;

    let c1 = ctx
        .dispatch_rpc(
            &mut s1,
            "corpRegistry",
            None,
            "AddCorporation",
            &[PyRep::String("Executor Corp".into()), PyRep::String("EXCP".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("AddCorporation 1");
    let corp1 = c1.dict_get("corporationID").and_then(PyRep::as_i64).expect("corp1");
    let c2 = ctx
        .dispatch_rpc(
            &mut s2,
            "corpRegistry",
            None,
            "AddCorporation",
            &[PyRep::String("Applicant Corp".into()), PyRep::String("APCP".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("AddCorporation 2");
    let corp2 = c2.dict_get("corporationID").and_then(PyRep::as_i64).expect("corp2");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&corp1) {
            corp.ensure_wallet_divisions();
            corp.wallet_divisions[0].balance = IskCents::from_isk(2_000_000_000);
            corp.master_wallet = corp.wallet_divisions[0].balance;
        }
    });
    for c in [P1, P2, p3] {
        ctx.drain_notifications_for_char(c);
    }

    // CreateAlliance & UpdateAlliance -> OnAllianceChanged(allianceID, change) to alliance member P1 only.
    let al = ctx
        .dispatch_rpc(
            &mut s1,
            "corpRegistry",
            None,
            "CreateAlliance",
            &[PyRep::String("Northern Pact".into()), PyRep::String("NPCT".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect("CreateAlliance");
    let aid = al.dict_get("allianceID").and_then(PyRep::as_i64).expect("aid");
    assert_eq!(
        events(&ctx, P1, "OnAllianceChanged"),
        vec![vec![
            PyRep::Integer(aid),
            PyRep::Dict(vec![
                (PyRep::String(("allianceID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(aid)])),
                (PyRep::String(("allianceName").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String("Northern Pact".into())])),
                (PyRep::String(("shortName").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String("NPCT".into())])),
                (PyRep::String(("executorCorpID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(corp1)])),
            ]),
        ]]
    );
    assert!(events(&ctx, P2, "OnAllianceChanged").is_empty());
    assert!(events(&ctx, p3, "OnAllianceChanged").is_empty());

    // ApplyToJoinAlliance by P2 & UpdateApplication by P1 -> OnAllianceApplicationChanged(aid, corp2, change)
    // to alliance member P1 and applicant corp2 member P2; P3 gets nothing.
    ctx.dispatch_rpc(
        &mut s2,
        "corpRegistry",
        None,
        "ApplyToJoinAlliance",
        &[PyRep::Integer(aid), PyRep::String("Requesting entry".into())],
        &HashMap::new(),
        None,
    )
    .await
    .expect("ApplyToJoinAlliance");
    let expected_ally_app = vec![
        PyRep::Integer(aid),
        PyRep::Integer(corp2),
        PyRep::Dict(vec![
            (PyRep::String(("allianceID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(aid)])),
            (PyRep::String(("corporationID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(corp2)])),
            (PyRep::String(("applicationText").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String("Requesting entry".into())])),
            (PyRep::String(("state").into()), PyRep::Tuple(vec![PyRep::None, PyRep::String(("Pending").into())])),
        ]),
    ];
    assert_eq!(events(&ctx, P1, "OnAllianceApplicationChanged"), vec![expected_ally_app.clone()]);
    assert_eq!(events(&ctx, P2, "OnAllianceApplicationChanged"), vec![expected_ally_app]);
    assert!(events(&ctx, p3, "OnAllianceApplicationChanged").is_empty());

    let accept_ts = ctx.now_unix() as i64;
    ctx.dispatch_rpc(
        &mut s1,
        "allianceRegistry",
        None,
        "UpdateApplication",
        &[PyRep::Integer(corp2)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("UpdateApplication");
    let expected_app_accepted = vec![
        PyRep::Integer(aid),
        PyRep::Integer(corp2),
        PyRep::Dict(vec![
            (PyRep::String(("state").into()), PyRep::Tuple(vec![PyRep::String(("Pending").into()), PyRep::String(("Accepted").into())])),
            (PyRep::String(("joinEffectiveTime").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(accept_ts + 86_400)])),
        ]),
    ];
    assert_eq!(events(&ctx, P1, "OnAllianceApplicationChanged"), vec![expected_app_accepted.clone()]);
    assert_eq!(events(&ctx, P2, "OnAllianceApplicationChanged"), vec![expected_app_accepted]);
    assert!(events(&ctx, p3, "OnAllianceApplicationChanged").is_empty());

    // Advance 24h join timer -> corp2 joins alliance, emitting OnAllianceMemberChanged(aid, corp2, change) to P1 & P2.
    ctx.advance_virtual_clock(86_401);
    ctx.dispatch_rpc(&mut s1, "allianceRegistry", None, "GetAllianceMembers", &[PyRep::Integer(aid)], &HashMap::new(), None)
        .await
        .expect("GetAllianceMembers");
    s2.alliance_id = Some(aid);
    let expected_member_join = vec![
        PyRep::Integer(aid),
        PyRep::Integer(corp2),
        PyRep::Dict(vec![
            (PyRep::String(("allianceID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(aid)])),
            (PyRep::String(("corporationID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(corp2)])),
        ]),
    ];
    assert_eq!(events(&ctx, P1, "OnAllianceMemberChanged"), vec![expected_member_join.clone()]);
    assert_eq!(events(&ctx, P2, "OnAllianceMemberChanged"), vec![expected_member_join]);
    assert!(events(&ctx, p3, "OnAllianceMemberChanged").is_empty());

    // AddAllianceContact -> OnAllianceRelationshipChanged(aid, toID, change) to alliance members P1 & P2 only.
    ctx.dispatch_rpc(
        &mut s1,
        "allianceRegistry",
        None,
        "AddAllianceContact",
        &[PyRep::Integer(p3), PyRep::Real(-10.0), PyRep::Integer(4)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("AddAllianceContact");
    let expected_rel = vec![
        PyRep::Integer(aid),
        PyRep::Integer(p3),
        PyRep::Dict(vec![
            (PyRep::String(("toID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(p3)])),
            (PyRep::String(("relationshipID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Real(-10.0)])),
            (PyRep::String(("labelMask").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(4)])),
        ]),
    ];
    assert_eq!(events(&ctx, P1, "OnAllianceRelationshipChanged"), vec![expected_rel.clone()]);
    assert_eq!(events(&ctx, P2, "OnAllianceRelationshipChanged"), vec![expected_rel]);
    assert!(events(&ctx, p3, "OnAllianceRelationshipChanged").is_empty());

    // KickMember(corp2) -> OnAllianceMemberChanged(aid, corp2, {"allianceID": (aid, None), "corporationID": (corp2, None)})
    // to P1 and P2; P3 gets nothing.
    ctx.dispatch_rpc(&mut s1, "allianceRegistry", None, "KickMember", &[PyRep::Integer(corp2)], &HashMap::new(), None)
        .await
        .expect("KickMember");
    s2.alliance_id = None;
    let expected_kick = vec![
        PyRep::Integer(aid),
        PyRep::Integer(corp2),
        PyRep::Dict(vec![
            (PyRep::String(("allianceID").into()), PyRep::Tuple(vec![PyRep::Integer(aid), PyRep::None])),
            (PyRep::String(("corporationID").into()), PyRep::Tuple(vec![PyRep::Integer(corp2), PyRep::None])),
        ]),
    ];
    assert_eq!(events(&ctx, P1, "OnAllianceMemberChanged"), vec![expected_kick.clone()]);
    assert_eq!(events(&ctx, P2, "OnAllianceMemberChanged"), vec![expected_kick]);
    assert!(events(&ctx, p3, "OnAllianceMemberChanged").is_empty());

    // Mutual war invite + accept -> OnWarChanged(war_kv, [corp1, corp2], change_dict) to war participants P1 & P2 only.
    ctx.dispatch_rpc(&mut s1, "mutualWarInviteMgr", None, "SendInviteByPlayer", &[PyRep::Integer(corp2)], &HashMap::new(), None)
        .await
        .expect("SendInviteByPlayer");
    let war_ts = ctx.now_unix() as i64;
    let war_res = ctx
        .dispatch_rpc(
            &mut s2,
            "mutualWarInviteMgr",
            None,
            "RespondToInviteByPlayer",
            &[PyRep::Integer(corp1), PyRep::Bool(true)],
            &HashMap::new(),
            None,
        )
        .await
        .expect("RespondToInviteByPlayer");
    let war_id = war_res.dict_get("warID").and_then(PyRep::as_i64).expect("war_id");
    let expected_war = vec![
        PyRep::keyval([
            ("warID", PyRep::Integer(war_id)),
            ("declaredByID", PyRep::Integer(corp1)),
            ("againstID", PyRep::Integer(corp2)),
            ("timeDeclared", PyRep::Integer(war_ts)),
            ("timeStarted", PyRep::Integer(war_ts + 86_400)),
            ("timeFinished", PyRep::None),
            ("mutual", PyRep::Bool(true)),
            ("openForAllies", PyRep::Bool(false)),
            ("warHQ", PyRep::None),
            ("active", PyRep::Bool(true)),
            ("combatActive", PyRep::Bool(false)),
        ]),
        PyRep::List(vec![PyRep::Integer(corp1), PyRep::Integer(corp2)]),
        PyRep::Dict(vec![
            (PyRep::String(("warID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(war_id)])),
            (PyRep::String(("declaredByID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(corp1)])),
            (PyRep::String(("againstID").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Integer(corp2)])),
            (PyRep::String(("mutual").into()), PyRep::Tuple(vec![PyRep::None, PyRep::Bool(true)])),
        ]),
    ];
    assert_eq!(events(&ctx, P1, "OnWarChanged"), vec![expected_war.clone()]);
    assert_eq!(events(&ctx, P2, "OnWarChanged"), vec![expected_war]);
    assert!(events(&ctx, p3, "OnWarChanged").is_empty(), "uninvolved character receives no OnWarChanged");
}

#[tokio::test]
async fn test_corp_registry_governance_structures_contacts_and_reinforce() {
    let ctx = ServerContext::new();
    let kw = HashMap::new();
    let mut s1 = select(&ctx, 1201, "demo", "demo-secret", P1).await;
    let mut _s2 = select(&ctx, 1202, "other", "other-secret", P2).await;

    // Create corporation for governance tests
    let _corp_rep = ctx
        .dispatch_rpc(
            &mut s1,
            "corpRegistry",
            None,
            "AddCorporation",
            &[PyRep::String("Aegis Defense Alliance".into()), PyRep::String("AEGIS".into()), PyRep::Real(0.05)],
            &kw,
            None,
        )
        .await
        .expect("AddCorporation");
    let corp_id = s1.corporation_id.expect("corp_id");

    // 1. MoveCompanyShares, MovePrivateShares & Shareholder queries
    ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "MoveCompanyShares", &[PyRep::Integer(corp_id), PyRep::Integer(P1), PyRep::Integer(500)], &kw, None).await.expect("MoveCompanyShares");
    let move_sh = ctx
        .dispatch_rpc(&mut s1, "corpRegistry", None, "MovePrivateShares", &[PyRep::Integer(P1), PyRep::Integer(100)], &kw, None)
        .await
        .expect("MovePrivateShares");
    let PyRep::KeyVal(sh_kv) = move_sh else { panic!("expected KeyVal") };
    assert_eq!(sh_kv.get("toShareholderID"), Some(&PyRep::Integer(P1)));
    assert_eq!(sh_kv.get("sharesTransferred"), Some(&PyRep::Integer(100)));

    let shs = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "GetShareholders", &[], &kw, None).await.expect("GetShareholders");
    let PyRep::Rowset { rows: sh_rows, .. } = shs else { panic!("expected Rowset") };
    assert!(!sh_rows.is_empty());

    let sh_by = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "GetSharesByShareholder", &[PyRep::Integer(P1)], &kw, None).await.expect("GetSharesByShareholder");
    assert!(matches!(sh_by, PyRep::Rowset { .. }));

    let sh_avg = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "GetMemberIDsWithMoreThanAvgShares", &[], &kw, None).await.expect("GetMemberIDsWithMoreThanAvgShares");
    let PyRep::List(avg_ids) = sh_avg else { panic!("expected List") };
    assert!(!avg_ids.is_empty());

    // 2. Titles (UpdateTitles, GetTitles)
    let upd_t = ctx
        .dispatch_rpc(&mut s1, "corpRegistry", None, "UpdateTitles", &[PyRep::Integer(1), PyRep::String("Fleet Commander".into()), PyRep::Integer(512)], &kw, None)
        .await
        .expect("UpdateTitles");
    let PyRep::KeyVal(ut_kv) = upd_t else { panic!("expected KeyVal") };
    assert_eq!(ut_kv.get("titleID"), Some(&PyRep::Integer(1)));

    let titles = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "GetTitles", &[], &kw, None).await.expect("GetTitles");
    let PyRep::Rowset { rows: t_rows, .. } = titles else { panic!("expected Rowset") };
    assert!(!t_rows.is_empty());

    // 3. Potential CEOs
    let pot = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "GetNumberOfPotentialCEOs", &[], &kw, None).await.expect("GetNumberOfPotentialCEOs");
    assert_eq!(pot.as_i64(), Some(0));

    // 4. Bulletins (UpdateBulletinOrder, DeleteBulletin)
    let _b1 = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "AddBulletin", &[PyRep::String("Welcome".into()), PyRep::String("Fly safe".into())], &kw, None).await.expect("AddBulletin");
    let ord = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "UpdateBulletinOrder", &[PyRep::Integer(1), PyRep::Integer(0)], &kw, None).await.expect("UpdateBulletinOrder");
    assert_eq!(ord, PyRep::Bool(true));

    let del_b = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "DeleteBulletin", &[PyRep::Integer(1)], &kw, None).await.expect("DeleteBulletin");
    let PyRep::KeyVal(db_kv) = del_b else { panic!("expected KeyVal") };
    assert_eq!(db_kv.get("deleted"), Some(&PyRep::Bool(true)));

    // 5. Contacts & Labels (EditCorporateContact, EditContactsRelationshipID, GetLabels)
    let ec = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "EditCorporateContact", &[PyRep::Integer(P2), PyRep::Real(5.0)], &kw, None).await.expect("EditCorporateContact");
    let PyRep::KeyVal(ec_kv) = ec else { panic!("expected KeyVal") };
    assert_eq!(ec_kv.get("contactID"), Some(&PyRep::Integer(P2)));

    let er = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "EditContactsRelationshipID", &[PyRep::Integer(P2), PyRep::Real(10.0)], &kw, None).await.expect("EditContactsRelationshipID");
    let PyRep::KeyVal(er_kv) = er else { panic!("expected KeyVal") };
    assert_eq!(er_kv.get("contactID"), Some(&PyRep::Integer(P2)));

    let _cl = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "CreateLabel", &[PyRep::String("Allies".into()), PyRep::Integer(1)], &kw, None).await.expect("CreateLabel");
    let labels = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "GetLabels", &[], &kw, None).await.expect("GetLabels");
    let PyRep::Rowset { rows: l_rows, .. } = labels else { panic!("expected Rowset") };
    assert!(!l_rows.is_empty());

    // 6. Mail & Structures Settings
    let _ = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "SetCorpWelcomeMail", &[PyRep::String("Greetings".into()), PyRep::String("Welcome to the corp".into())], &kw, None).await.expect("SetCorpWelcomeMail");
    let wmail = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "GetCorpWelcomeMail", &[], &kw, None).await.expect("GetCorpWelcomeMail");
    let PyRep::KeyVal(wm_kv) = wmail else { panic!("expected KeyVal") };
    assert_eq!(wm_kv.get("subject"), Some(&PyRep::String("Greetings".into())));

    let r_mail = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "RegisterNewCorpMailRestrictionSettings", &[PyRep::Bool(true)], &kw, None).await.expect("RegisterNewCorpMailRestrictionSettings");
    assert_eq!(r_mail, PyRep::Bool(true));

    let does_mail = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "DoesCorpRestrictCorpMails", &[], &kw, None).await.expect("DoesCorpRestrictCorpMails");
    assert_eq!(does_mail, PyRep::Bool(true));

    let r_struct = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "RegisterNewAcceptStructureSettings", &[PyRep::Bool(true)], &kw, None).await.expect("RegisterNewAcceptStructureSettings");
    assert_eq!(r_struct, PyRep::Bool(true));

    let does_struct = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "DoesMyCorpAcceptStructures", &[], &kw, None).await.expect("DoesMyCorpAcceptStructures");
    assert_eq!(does_struct, PyRep::Bool(true));

    // 7. Structure Reinforce Default (SetStructureReinforceDefault, GetStructureReinforceDefault)
    let set_reinf = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "SetStructureReinforceDefault", &[PyRep::Integer(18), PyRep::Integer(4)], &kw, None).await.expect("SetStructureReinforceDefault");
    assert_eq!(set_reinf, PyRep::Integer(18));

    let get_reinf = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "GetStructureReinforceDefault", &[], &kw, None).await.expect("GetStructureReinforceDefault");
    assert_eq!(get_reinf, PyRep::Integer(18));

    // 8. Introspection methods (Unbind, SetSessionCheck, clear_memoized, columns, header, itervalues)
    let unb = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "Unbind", &[], &kw, None).await.expect("Unbind");
    assert_eq!(unb, PyRep::None);

    let sc = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "SetSessionCheck", &[], &kw, None).await.expect("SetSessionCheck");
    assert_eq!(sc, PyRep::None);

    let cm = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "clear_memoized", &[], &kw, None).await.expect("clear_memoized");
    assert_eq!(cm, PyRep::None);

    let cols = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "columns", &[], &kw, None).await.expect("columns");
    let PyRep::List(c_list) = cols else { panic!("expected List") };
    assert!(!c_list.is_empty());

    let hdr = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "header", &[], &kw, None).await.expect("header");
    let PyRep::Tuple(h_list) = hdr else { panic!("expected Tuple") };
    assert!(!h_list.is_empty());

    let itv = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "itervalues", &[], &kw, None).await.expect("itervalues");
    let PyRep::List(it_list) = itv else { panic!("expected List") };
    assert!(it_list.is_empty());

    // 9. ResignFromCEO
    let resign = ctx.dispatch_rpc(&mut s1, "corpRegistry", None, "ResignFromCEO", &[PyRep::Integer(P1)], &kw, None).await.expect("ResignFromCEO");
    let PyRep::KeyVal(res_kv) = resign else { panic!("expected KeyVal") };
    assert_eq!(res_kv.get("corporationID"), Some(&PyRep::Integer(corp_id)));
}
