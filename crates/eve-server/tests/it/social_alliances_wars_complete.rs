//! Phase 7 Alliances & Wars Integration Tests (`SO-3` Part 1).

use crate::common;
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

#[tokio::test]
async fn test_alliances_lifecycle_governance_contacts_bulletins_and_rankings() {
    // parity: feat:alliance.create-alliance, feat:alliance.apply-alliance, feat:alliance.leave-alliance, feat:alliance.alliance-executor, feat:alliance.alliance-info, feat:alliance.alliance-contacts, feat:alliance.alliance-bulletins, feat:alliance.alliance-rankings, feat:alliance.alliance-close, rpc:corpRegistry.GetAllianceApplications
    eve_testkit::attest(&[
        "feat:alliance.create-alliance",
        "feat:alliance.apply-alliance",
        "feat:alliance.leave-alliance",
        "feat:alliance.alliance-executor",
        "feat:alliance.alliance-info",
        "feat:alliance.alliance-contacts",
        "feat:alliance.alliance-bulletins",
        "feat:alliance.alliance-rankings",
        "feat:alliance.alliance-close",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess1 = SessionState::new(901);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(90_000_001)]).await.unwrap();
    let mut sess2 = SessionState::new(902);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    call(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(90_000_002)]).await.unwrap();
    let p3_uid = ctx.db.provision_player_account("alliance_three", "alliance-three-secret");
    let p3_char = ctx.db.create_character(p3_uid, "Alliance Three", 1, 1, 11, 17, &ctx.sde).expect("pilot 3").character_id;
    let mut sess3 = SessionState::new(903);
    common::login_pilot(&ctx, &mut sess3, "alliance_three", "alliance-three-secret").await;
    call(&ctx, &mut sess3, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(p3_char)]).await.unwrap();
    ctx.advance_secs(12);
    let p4_uid = ctx.db.provision_player_account("alliance_nondir", "alliance-nondir-secret");
    let p4_char = ctx.db.create_character(p4_uid, "Alliance NonDirector", 1, 1, 11, 17, &ctx.sde).expect("pilot 4").character_id;
    let mut sess_nondir = SessionState::new(904);
    common::login_pilot(&ctx, &mut sess_nondir, "alliance_nondir", "alliance-nondir-secret").await;
    call(&ctx, &mut sess_nondir, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(p4_char)]).await.unwrap();
    ctx.advance_secs(12);

    ctx.db.with_mut_snapshot(|snap| {
        let sk = eve_db::TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 };
        snap.skills.entry(90_000_001).or_default().insert(3363, sk.clone());
        snap.skills.entry(90_000_002).or_default().insert(3363, sk.clone());
        snap.skills.entry(p3_char).or_default().insert(3363, sk);
        snap.characters.get_mut(&90_000_001).unwrap().balance = eve_db::IskCents::from_isk(5_000_000_000);
        snap.characters.get_mut(&90_000_002).unwrap().balance = eve_db::IskCents::from_isk(5_000_000_000);
        snap.characters.get_mut(&p3_char).unwrap().balance = eve_db::IskCents::from_isk(5_000_000_000);
    });
    call(&ctx, &mut sess1, "corpRegistry", "AddCorporation", vec![PyRep::String("Vanguard Executor Corp".into()), PyRep::String("VEXEC".into()), PyRep::Real(0.05)]).await.unwrap();
    call(&ctx, &mut sess2, "corpRegistry", "AddCorporation", vec![PyRep::String("Frontier Member Corp".into()), PyRep::String("FMEMB".into()), PyRep::Real(0.05)]).await.unwrap();
    call(&ctx, &mut sess3, "corpRegistry", "AddCorporation", vec![PyRep::String("Lonely Third Corp".into()), PyRep::String("LONE".into()), PyRep::Real(0.05)]).await.unwrap();
    let (corp1, corp2, corp3) = (sess1.corporation_id.unwrap(), sess2.corporation_id.unwrap(), sess3.corporation_id.unwrap());
    let create = |name: &str, ticker: &str| vec![PyRep::String(name.into()), PyRep::String(ticker.into()), PyRep::String("Sovereignty alliance".into()), PyRep::String("https://nc.eve".into())];

    // create-alliance: fee from the master wallet, ticker validation, unique name, executor = creator corp.
    set_corp_wallet(&ctx, corp1, 0);
    assert_eq!(call(&ctx, &mut sess1, "corpRegistry", "CreateAlliance", create("Northern Coalition Prime", "NCPRM")).await.expect_err("no fee").code, "InsufficientFunds");
    assert_eq!(call(&ctx, &mut sess1, "corpRegistry", "CreateAlliance", create("Northern Coalition Prime", "TOOLONG")).await.expect_err("ticker").code, "InvalidAllianceName");
    set_corp_wallet(&ctx, corp1, 2_500_000_000);
    let created = call(&ctx, &mut sess1, "corpRegistry", "CreateAlliance", create("Northern Coalition Prime", "NCPRM")).await.expect("CreateAlliance");
    let aid = field(&created, "allianceID").as_i64().unwrap();
    assert_eq!(field(&created, "executorCorpID"), PyRep::Integer(corp1));
    assert_eq!(sess1.alliance_id, Some(aid));
    assert_eq!(corp_wallet(&ctx, corp1), 1_500_000_000.0, "1B ISK alliance fee");
    assert_eq!(call(&ctx, &mut sess2, "corpRegistry", "CreateAlliance", create("northern coalition prime", "XYZ")).await.expect_err("dup").code, "AllianceNameTaken");

    // Put non-director pilot into executor corp1 with 0 roles
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&p4_char) {
            ch.corporation_id = corp1;
            ch.alliance_id = Some(aid);
        }
        if let Some(corp) = snap.corporations.get_mut(&corp1) {
            corp.members.insert(p4_char);
            corp.member_roles.insert(p4_char, 0);
        }
    });
    sess_nondir.corporation_id = Some(corp1);
    sess_nondir.alliance_id = Some(aid);
    sess_nondir.corp_role = 0;

    // Bind allianceRegistry for sess1
    assert_eq!(call(&ctx, &mut sess1, "allianceRegistry", "Bind", vec![PyRep::Integer(aid)]).await.expect("Bind allianceRegistry"), PyRep::None);

    // apply-alliance: application -> executor accepts -> joins after 24h.
    let applied = call(&ctx, &mut sess2, "corpRegistry", "ApplyToJoinAlliance", vec![PyRep::Integer(aid), PyRep::String("Requesting membership".into())]).await.unwrap();
    assert_eq!(field(&applied, "status"), PyRep::String("Pending".into()));
    let (acols, arows) = rows(&call(&ctx, &mut sess1, "corpRegistry", "GetAllianceApplications", vec![]).await.unwrap());
    assert_eq!(arows.len(), 1);
    assert_eq!(arows[0][col(&acols, "corporationID")], PyRep::Integer(corp2));
    assert_eq!(call(&ctx, &mut sess2, "allianceRegistry", "UpdateApplication", vec![PyRep::Integer(corp2)]).await.expect_err("not in alliance").code, "NotInAlliance");
    assert_eq!(call(&ctx, &mut sess_nondir, "allianceRegistry", "UpdateApplication", vec![PyRep::Integer(corp2)]).await.expect_err("non-director").code, "CorpRoleDenied");
    let accept = call(&ctx, &mut sess1, "allianceRegistry", "UpdateApplication", vec![PyRep::Integer(corp2), PyRep::String("Welcome".into())]).await.unwrap();
    assert_eq!(field(&accept, "joinEffectiveTime"), PyRep::Integer(ctx.now_unix() as i64 + 86_400));
    assert_eq!(rows(&call(&ctx, &mut sess1, "allianceRegistry", "GetAllianceMembers", vec![PyRep::Integer(aid)]).await.unwrap()).1.len(), 1, "not yet joined");
    ctx.advance_secs(86_401);
    assert_eq!(rows(&call(&ctx, &mut sess1, "allianceRegistry", "GetAllianceMembers", vec![PyRep::Integer(aid)]).await.unwrap()).1.len(), 2);
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp2].alliance_id), Some(aid));
    sess2.alliance_id = Some(aid);

    // alliance-info: GetAlliance / GetAlliancePublicInfo / GetAllianceMembers / GetEmploymentRecord.
    assert_eq!(call(&ctx, &mut sess2, "allianceRegistry", "UpdateAlliance", vec![PyRep::String("x".into()), PyRep::String("y".into())]).await.expect_err("member corp").code, "NotAllianceExecutor");
    assert_eq!(call(&ctx, &mut sess_nondir, "allianceRegistry", "UpdateAlliance", vec![PyRep::String("x".into()), PyRep::String("y".into())]).await.expect_err("non-director").code, "CorpRoleDenied");
    call(&ctx, &mut sess1, "allianceRegistry", "UpdateAlliance", vec![PyRep::String("Updated Northern Coalition Desc".into()), PyRep::String("https://nc-updated.eve".into())]).await.unwrap();
    let info1 = call(&ctx, &mut sess2, "allianceRegistry", "GetAlliance", vec![PyRep::Integer(aid)]).await.unwrap();
    assert_eq!(field(&info1, "description"), PyRep::String("Updated Northern Coalition Desc".into()));
    assert_eq!(field(&info1, "memberCount"), PyRep::Integer(2));
    assert_eq!(field(&info1, "executorCorpID"), PyRep::Integer(corp1));
    let info2 = call(&ctx, &mut sess2, "allianceRegistry", "GetAlliancePublicInfo", vec![PyRep::Integer(aid)]).await.unwrap();
    assert_eq!(field(&info2, "shortName"), PyRep::String("NCPRM".into()));
    let (ecols, erows) = rows(&call(&ctx, &mut sess1, "allianceRegistry", "GetEmploymentRecord", vec![PyRep::Integer(aid)]).await.unwrap());
    let emp_corps: Vec<PyRep> = erows.iter().map(|r| r[col(&ecols, "corporationID")].clone()).collect();
    assert_eq!(emp_corps, vec![PyRep::Integer(corp1), PyRep::Integer(corp2)]);

    // alliance-rankings: ranked by member count.
    set_corp_wallet(&ctx, corp3, 1_000_000_000);
    let small = call(&ctx, &mut sess3, "corpRegistry", "CreateAlliance", create("Tiny Alliance", "TINY")).await.unwrap();
    let small_id = field(&small, "allianceID").as_i64().unwrap();
    let (rcols, rrows) = rows(&call(&ctx, &mut sess1, "allianceRegistry", "GetRankedAlliances", vec![PyRep::Integer(50)]).await.unwrap());
    let ranked: Vec<(PyRep, PyRep)> = rrows.iter().map(|r| (r[col(&rcols, "allianceID")].clone(), r[col(&rcols, "memberCount")].clone())).collect();
    assert_eq!(ranked, vec![(PyRep::Integer(aid), PyRep::Integer(3)), (PyRep::Integer(small_id), PyRep::Integer(1))]);

    // alliance-contacts: executor director/CEO-managed, visible to member corps; non-director in executor corp rejected.
    assert_eq!(call(&ctx, &mut sess2, "allianceRegistry", "AddAllianceContact", vec![PyRep::Integer(p3_char), PyRep::Real(10.0), PyRep::Integer(1)]).await.expect_err("member corp").code, "NotAllianceExecutor");
    assert_eq!(call(&ctx, &mut sess_nondir, "allianceRegistry", "AddAllianceContact", vec![PyRep::Integer(p3_char), PyRep::Real(10.0), PyRep::Integer(1)]).await.expect_err("non-director").code, "CorpRoleDenied");
    assert_eq!(call(&ctx, &mut sess1, "allianceRegistry", "AddAllianceContact", vec![PyRep::Integer(p3_char), PyRep::Real(11.0), PyRep::Integer(1)]).await.expect_err("standing range").code, "InvalidStanding");
    call(&ctx, &mut sess1, "allianceRegistry", "AddAllianceContact", vec![PyRep::Integer(p3_char), PyRep::Real(10.0), PyRep::Integer(1)]).await.unwrap();
    call(&ctx, &mut sess1, "allianceRegistry", "EditAllianceContact", vec![PyRep::Integer(p3_char), PyRep::Real(-5.0), PyRep::Integer(3)]).await.unwrap();
    let (_, crows) = rows(&call(&ctx, &mut sess2, "allianceRegistry", "GetAllianceContacts", vec![]).await.unwrap());
    assert_eq!(crows, vec![vec![PyRep::Integer(p3_char), PyRep::Real(-5.0), PyRep::Integer(3)]], "member corp sees executor contacts");
    assert_eq!(call(&ctx, &mut sess2, "allianceRegistry", "RemoveAllianceContacts", vec![PyRep::List(vec![PyRep::Integer(p3_char)])]).await.expect_err("member corp").code, "NotAllianceExecutor");
    assert_eq!(call(&ctx, &mut sess_nondir, "allianceRegistry", "RemoveAllianceContacts", vec![PyRep::List(vec![PyRep::Integer(p3_char)])]).await.expect_err("non-director").code, "CorpRoleDenied");
    call(&ctx, &mut sess1, "allianceRegistry", "RemoveAllianceContacts", vec![PyRep::List(vec![PyRep::Integer(p3_char)])]).await.unwrap();
    assert!(rows(&call(&ctx, &mut sess2, "allianceRegistry", "GetAllianceContacts", vec![]).await.unwrap()).1.is_empty());

    // alliance-bulletins: executor director/CEO-managed, visible to all member corps; non-director in executor corp rejected.
    assert_eq!(call(&ctx, &mut sess2, "allianceRegistry", "AddBulletin", vec![PyRep::String("t".into()), PyRep::String("b".into())]).await.expect_err("member corp").code, "NotAllianceExecutor");
    assert_eq!(call(&ctx, &mut sess_nondir, "allianceRegistry", "AddBulletin", vec![PyRep::String("t".into()), PyRep::String("b".into())]).await.expect_err("non-director").code, "CorpRoleDenied");
    let bul = call(&ctx, &mut sess1, "allianceRegistry", "AddBulletin", vec![PyRep::String("Alliance Sov Policy".into()), PyRep::String("Defend all TCUs".into())]).await.unwrap();
    let bul_id = field(&bul, "bulletinID").as_i64().unwrap();
    call(&ctx, &mut sess1, "allianceRegistry", "UpdateBulletin", vec![PyRep::Integer(bul_id), PyRep::String("Updated Sov Policy".into()), PyRep::String("Updated body".into())]).await.unwrap();
    let (bcols, brows) = rows(&call(&ctx, &mut sess2, "allianceRegistry", "GetAllianceBulletins", vec![]).await.unwrap());
    assert_eq!(brows.len(), 1);
    assert_eq!(brows[0][col(&bcols, "title")], PyRep::String("Updated Sov Policy".into()));
    assert_eq!(brows[0][col(&bcols, "editCharacterID")], PyRep::Integer(90_000_001));
    assert_eq!(call(&ctx, &mut sess2, "allianceRegistry", "DeleteBulletin", vec![PyRep::Integer(bul_id)]).await.expect_err("member corp").code, "NotAllianceExecutor");
    assert_eq!(call(&ctx, &mut sess_nondir, "allianceRegistry", "DeleteBulletin", vec![PyRep::Integer(bul_id)]).await.expect_err("non-director").code, "CorpRoleDenied");
    assert_eq!(call(&ctx, &mut sess1, "allianceRegistry", "DeleteBulletin", vec![PyRep::Integer(bul_id)]).await.unwrap(), PyRep::Bool(true));
    assert!(rows(&call(&ctx, &mut sess2, "allianceRegistry", "GetAllianceBulletins", vec![]).await.unwrap()).1.is_empty());

    // alliance-executor: support votes elect the executor (majority of member corps); non-director denied; OnAllianceChanged emitted.
    assert_eq!(call(&ctx, &mut sess_nondir, "allianceRegistry", "DeclareExecutorSupport", vec![PyRep::Integer(corp2)]).await.expect_err("non-director").code, "CorpRoleDenied");
    let one_vote = call(&ctx, &mut sess2, "allianceRegistry", "DeclareExecutorSupport", vec![PyRep::Integer(corp2)]).await.unwrap();
    assert_eq!(field(&one_vote, "executorCorpID"), PyRep::Integer(corp1), "1 of 2 votes is not a majority");
    ctx.drain_notifications_for_char(90_000_002);
    let majority = call(&ctx, &mut sess1, "allianceRegistry", "DeclareExecutorSupport", vec![PyRep::Integer(corp2)]).await.unwrap();
    assert_eq!(field(&majority, "executorCorpID"), PyRep::Integer(corp2));
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnAllianceChanged"), "executor election must emit OnAllianceChanged");
    assert_eq!(call(&ctx, &mut sess1, "allianceRegistry", "KickMember", vec![PyRep::Integer(corp2)]).await.expect_err("no longer executor").code, "NotAllianceExecutor");

    // leave-alliance: blocked while the corp is in an active war; non-director denied.
    assert_eq!(call(&ctx, &mut sess_nondir, "allianceRegistry", "DeleteMember", vec![PyRep::Integer(corp1)]).await.expect_err("non-director").code, "CorpRoleDenied");
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(777_001, eve_db::WarRecord { war_id: 777_001, aggressor_corp_id: corp3, defender_corp_id: corp1, mutual: true, started_unix: 0, active: true });
    });
    assert_eq!(call(&ctx, &mut sess1, "allianceRegistry", "DeleteMember", vec![PyRep::Integer(corp1)]).await.expect_err("war").code, "CannotLeaveAllianceDuringWar");
    ctx.db.with_mut_snapshot(|snap| snap.wars.get_mut(&777_001).unwrap().active = false);
    assert_eq!(call(&ctx, &mut sess1, "allianceRegistry", "DeleteMember", vec![PyRep::Integer(corp2)]).await.expect_err("only self").code, "CorpRoleDenied");

    // executor kicks corp 1 (immediate).
    let kicked = call(&ctx, &mut sess2, "allianceRegistry", "KickMember", vec![PyRep::Integer(corp1)]).await.unwrap();
    assert_eq!(field(&kicked, "removed"), PyRep::Bool(true));
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp1].alliance_id), None);

    // leave-alliance + alliance-close: the last corp leaves after the 24h delay -> alliance closes and emits OnAllianceMemberChanged.
    let leave = call(&ctx, &mut sess2, "allianceRegistry", "DeleteMember", vec![PyRep::Integer(corp2)]).await.unwrap();
    assert_eq!(field(&leave, "leaveEffectiveTime"), PyRep::Integer(ctx.now_unix() as i64 + 86_400));
    assert_eq!(field(&leave, "allianceClosed"), PyRep::Bool(false));
    assert_eq!(rows(&call(&ctx, &mut sess2, "allianceRegistry", "GetAllianceMembers", vec![PyRep::Integer(aid)]).await.unwrap()).1.len(), 1, "leave pending");
    ctx.drain_notifications_for_char(90_000_002);
    ctx.advance_secs(86_401);
    assert!(ctx.drain_notifications_for_char(90_000_002).iter().any(|n| n.event == "OnAllianceMemberChanged"), "maturing 24h leave must emit OnAllianceMemberChanged");
    assert!(rows(&call(&ctx, &mut sess2, "allianceRegistry", "GetAllianceMembers", vec![PyRep::Integer(aid)]).await.unwrap()).1.is_empty());
    assert_eq!(call(&ctx, &mut sess2, "allianceRegistry", "GetAlliance", vec![PyRep::Integer(aid)]).await.expect_err("closed").code, "AllianceNotFound");
    assert!(ctx.db.with_snapshot(|s| !s.alliances.contains_key(&aid) && s.corporations[&corp2].alliance_id.is_none()));
}

async fn call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: Vec<PyRep>,
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, &args, &HashMap::new(), None).await
}

fn field(rep: &PyRep, key: &str) -> PyRep {
    rep.dict_get(key).cloned().unwrap_or_else(|| panic!("missing {key} in {rep:?}"))
}

fn rows(rep: &PyRep) -> (Vec<String>, Vec<Vec<PyRep>>) {
    match rep {
        PyRep::Rowset { columns, rows } => (columns.clone(), rows.clone()),
        PyRep::ObjectEx { list, .. } => {
            let r: Vec<Vec<PyRep>> = list
                .iter()
                .map(|item| {
                    if let PyRep::DBRow { columns, .. } = item {
                        columns.iter().map(|(_, v)| v.clone()).collect()
                    } else if let PyRep::List(cols) | PyRep::Tuple(cols) = item {
                        cols.clone()
                    } else {
                        vec![item.clone()]
                    }
                })
                .collect();
            (Vec::new(), r)
        }
        other => panic!("expected Rowset, got {other:?}"),
    }
}

fn col(cols: &[String], name: &str) -> usize {
    cols.iter().position(|c| c == name).unwrap_or_else(|| panic!("column {name} missing in {cols:?}"))
}

fn structure(id: i64, owner: i64, name: &str) -> eve_db::UpwellStructureRecord {
    eve_db::UpwellStructureRecord {
        structure_id: id,
        type_id: 35832,
        owner_corp_id: owner,
        solar_system_id: 30_000_142,
        name: name.into(),
        position: [200_000.0, 0.0, 0.0],
        state: "ShieldVulnerable".into(),
        quantum_core_installed: true,
        fuel_blocks: 1_000,
        shield_hp: 5_000.0,
        armor_hp: 5_000.0,
        hull_hp: 5_000.0,
        reinforced_until_unix: 0,
        last_damage_tick: 0,
        last_damage_unix: 0, self_destruct_at_unix: None,
    }
}

fn corp_wallet(ctx: &ServerContext, corp_id: i64) -> f64 {
    ctx.db.with_snapshot(|snap| snap.corporations[&corp_id].wallet_divisions[0].balance.to_wire_f64())
}

fn set_corp_wallet(ctx: &ServerContext, corp_id: i64, isk: i64) {
    ctx.db.with_mut_snapshot(|snap| {
        let c = snap.corporations.get_mut(&corp_id).unwrap();
        c.ensure_wallet_divisions();
        c.wallet_divisions[0].balance = eve_db::IskCents::from_isk(isk);
        c.master_wallet = c.wallet_divisions[0].balance;
    });
}

fn war_field(ctx_rep: &PyRep, war_id: i64, name: &str) -> Option<PyRep> {
    let (cols, rs) = rows(ctx_rep);
    rs.iter().find(|r| r[col(&cols, "warID")] == PyRep::Integer(war_id)).map(|r| r[col(&cols, name)].clone())
}

#[tokio::test]
async fn test_wars_declaration_hq_eligibility_mutual_peace_allies_and_stats() {
    // parity: feat:war.declare-war, feat:war.war-hq, feat:war.war-eligibility-change, feat:war.war-list, feat:war.mutual-war, feat:war.peace-treaty, feat:war.allies, feat:war.war-stats, feat:war.war-retract, feat:corporation.corp-war-eligibility, feat:corporation.corp-ally-cost, rpc:corpRegistry.DeclareWarAgainst, rpc:corpRegistry.CharGetAllyBaseCost
    eve_testkit::attest(&[
        "feat:war.declare-war",
        "feat:war.war-hq",
        "feat:war.war-eligibility-change",
        "feat:war.war-list",
        "feat:war.mutual-war",
        "feat:war.peace-treaty",
        "feat:war.allies",
        "feat:war.war-stats",
        "feat:war.war-retract",
        "feat:corporation.corp-war-eligibility",
        "feat:corporation.corp-ally-cost",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess1 = SessionState::new(911);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(90_000_001)]).await.unwrap();
    let mut sess2 = SessionState::new(912);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    call(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(90_000_002)]).await.unwrap();
    let p3_uid = ctx.db.provision_player_account("war_ally", "war-ally-secret");
    let p3_char = ctx.db.create_character(p3_uid, "War Ally Pilot", 1, 1, 11, 17, &ctx.sde).expect("create ally pilot").character_id;
    let mut sess3 = SessionState::new(913);
    common::login_pilot(&ctx, &mut sess3, "war_ally", "war-ally-secret").await;
    call(&ctx, &mut sess3, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(p3_char)]).await.unwrap();
    ctx.advance_secs(12);

    ctx.db.with_mut_snapshot(|snap| {
        let sk = eve_db::TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 };
        snap.skills.entry(90_000_001).or_default().insert(3363, sk.clone());
        snap.skills.entry(90_000_002).or_default().insert(3363, sk.clone());
        snap.skills.entry(p3_char).or_default().insert(3363, sk);
        snap.characters.get_mut(&90_000_001).unwrap().balance = eve_db::IskCents::from_isk(2_000_000_000);
        snap.characters.get_mut(&90_000_002).unwrap().balance = eve_db::IskCents::from_isk(2_000_000_000);
        snap.characters.get_mut(&p3_char).unwrap().balance = eve_db::IskCents::from_isk(2_000_000_000);
    });
    call(&ctx, &mut sess1, "corpRegistry", "AddCorporation", vec![PyRep::String("Aggressor Syndicate".into()), PyRep::String("AGGR".into()), PyRep::Real(0.05)]).await.unwrap();
    call(&ctx, &mut sess2, "corpRegistry", "AddCorporation", vec![PyRep::String("Defender Logistics".into()), PyRep::String("DEFL".into()), PyRep::Real(0.05)]).await.unwrap();
    call(&ctx, &mut sess3, "corpRegistry", "AddCorporation", vec![PyRep::String("Mercenary Allies".into()), PyRep::String("MERC".into()), PyRep::Real(0.05)]).await.unwrap();
    let (agg, def, ally) = (sess1.corporation_id.unwrap(), sess2.corporation_id.unwrap(), sess3.corporation_id.unwrap());
    set_corp_wallet(&ctx, agg, 1_000_000_000);
    set_corp_wallet(&ctx, def, 1_000_000_000);
    set_corp_wallet(&ctx, ally, 1_000_000_000);
    let declare = |hq: i64| vec![PyRep::Integer(def), PyRep::Integer(hq)];

    // corp-war-eligibility / war-hq: the attacker needs its own War HQ structure ...
    assert_eq!(call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect_err("no war HQ").code, "WarHqRequired");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(995_000_001, structure(995_000_001, agg, "Aggressor War HQ"));
    });
    assert_eq!(call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect_err("ineligible defender").code, "DefenderNotWarEligible");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(995_000_002, structure(995_000_002, def, "Defender Raitaru"));
    });
    set_corp_wallet(&ctx, agg, 50_000_000);
    assert_eq!(call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect_err("fee unaffordable").code, "InsufficientFunds");
    set_corp_wallet(&ctx, agg, 1_000_000_000);
    let declared_at = ctx.now_unix() as i64;
    let war = call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect("DeclareWarAgainst");
    let war_id = field(&war, "warID").as_i64().unwrap();
    assert_eq!(field(&war, "warHQ"), PyRep::Integer(995_000_001));
    assert_eq!(field(&war, "timeStarted"), PyRep::Integer(declared_at + 86_400), "24h spool-up");
    assert_eq!(corp_wallet(&ctx, agg), 900_000_000.0, "100M ISK war bill");
    assert_eq!(call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect_err("duplicate war").code, "AlreadyAtWar");

    // war-list: GetWarsByOwnerID / GetTop50 / GetPublicWarInfo / GetWarsForStructure.
    let by_owner = call(&ctx, &mut sess2, "warsInfoMgr", "GetWarsByOwnerID", vec![PyRep::Integer(def)]).await.unwrap();
    assert_eq!(war_field(&by_owner, war_id, "declaredByID"), Some(PyRep::Integer(agg)));
    assert_eq!(war_field(&by_owner, war_id, "combatActive"), Some(PyRep::Bool(false)), "still spooling up");
    let top = call(&ctx, &mut sess2, "warsInfoMgr", "GetTop50", vec![]).await.unwrap();
    assert!(war_field(&top, war_id, "warID").is_some());
    let for_hq = call(&ctx, &mut sess2, "warsInfoMgr", "GetWarsForStructure", vec![PyRep::Integer(995_000_001)]).await.unwrap();
    assert!(war_field(&for_hq, war_id, "warID").is_some());
    ctx.advance_secs(86_401);
    let public = call(&ctx, &mut sess3, "warsInfoMgr", "GetPublicWarInfo", vec![PyRep::Integer(war_id)]).await.unwrap();
    assert_eq!(field(&public, "againstID"), PyRep::Integer(def));
    assert_eq!(field(&public, "combatActive"), PyRep::Bool(true), "active after 24h");

    // corp-ally-cost: SDE baseDefenderAllyCost (attr 1820) reduced by Diplomatic Relations (attr 1821 per level).
    assert_eq!(call(&ctx, &mut sess2, "corpRegistry", "CharGetAllyBaseCost", vec![]).await.unwrap(), PyRep::Real(10_000_000.0));
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_002).or_default().insert(3368, eve_db::TrainedSkillRecord { type_id: 3368, level: 4, skill_points: 45_255.0 });
    });
    assert_eq!(call(&ctx, &mut sess2, "corpRegistry", "CharGetAllyBaseCost", vec![]).await.unwrap(), PyRep::Real(8_000_000.0), "-5% per Diplomatic Relations level");

    // allies: defender opens the war, ally offers, defender pays and the ally joins after 24h.
    let offer_args = vec![PyRep::Integer(war_id), PyRep::Real(25_000_000.0), PyRep::String("Mercenary assistance".into())];
    assert_eq!(call(&ctx, &mut sess3, "warsInfoMgr", "CreateWarAllyOffer", offer_args.clone()).await.expect_err("not open").code, "WarNotOpenForAllies");
    assert_eq!(call(&ctx, &mut sess1, "warsInfoMgr", "SetOpenForAllies", vec![PyRep::Integer(war_id), PyRep::Bool(true)]).await.expect_err("aggressor").code, "NotWarDefender");
    call(&ctx, &mut sess2, "warsInfoMgr", "SetOpenForAllies", vec![PyRep::Integer(war_id), PyRep::Bool(true)]).await.unwrap();
    let needing = call(&ctx, &mut sess3, "warsInfoMgr", "GetWarsRequiringAssistance", vec![]).await.unwrap();
    assert_eq!(war_field(&needing, war_id, "openForAllies"), Some(PyRep::Bool(true)));
    let offer = call(&ctx, &mut sess3, "warsInfoMgr", "CreateWarAllyOffer", offer_args).await.expect("CreateWarAllyOffer");
    let offer_id = field(&offer, "negotiationID").as_i64().unwrap();
    assert_eq!(call(&ctx, &mut sess1, "warsInfoMgr", "AcceptAllyNegotiation", vec![PyRep::Integer(offer_id)]).await.expect_err("only defender").code, "NotNegotiationCounterparty");
    let accepted = call(&ctx, &mut sess2, "warsInfoMgr", "AcceptAllyNegotiation", vec![PyRep::Integer(offer_id)]).await.expect("accept ally");
    assert_eq!(field(&accepted, "allyID"), PyRep::Integer(ally));
    assert_eq!(corp_wallet(&ctx, def), 975_000_000.0, "defender pays the ally fee");
    assert_eq!(corp_wallet(&ctx, ally), 1_025_000_000.0, "ally receives the fee");
    let (acols, arows) = rows(&call(&ctx, &mut sess2, "warsInfoMgr", "GetAllyRowsByWarID", vec![PyRep::Integer(war_id)]).await.unwrap());
    assert_eq!(arows[0][col(&acols, "active")], PyRep::Bool(false), "ally joins after 24h");
    ctx.advance_secs(86_401);
    let (acols, arows) = rows(&call(&ctx, &mut sess2, "warsInfoMgr", "GetAllyRowsByWarID", vec![PyRep::Integer(war_id)]).await.unwrap());
    assert_eq!(arows[0][col(&acols, "active")], PyRep::Bool(true));

    // war-stats: kills/ISK come from combat ship destruction between the war sides.
    let km1 = eve_server::services::combat_crimewatch::process_ship_destruction_with_aggression(
        &ctx, 90_000_002, 100_000_002, 587, 90_000_001, 587, 30_000_142, glam::DVec3::ZERO, false,
    );
    let km2 = eve_server::services::combat_crimewatch::process_ship_destruction_with_aggression(
        &ctx, 90_000_001, 100_000_001, 603, 90_000_002, 587, 30_000_142, glam::DVec3::ZERO, false,
    );
    let base = call(&ctx, &mut sess1, "warStatisticMgr", "GetBaseInfo", vec![PyRep::Integer(war_id)]).await.unwrap();
    assert_eq!(field(&base, "killsForAggressor"), PyRep::Integer(1));
    assert_eq!(field(&base, "killsForDefender"), PyRep::Integer(1));
    assert!(field(&base, "iskDestroyedByAggressor").as_f64().unwrap() > 0.0);
    assert!(field(&base, "iskDestroyedByDefender").as_f64().unwrap() > 0.0);
    let (_, krows) = rows(&call(&ctx, &mut sess1, "warStatisticMgr", "GetKills", vec![PyRep::Integer(war_id)]).await.unwrap());
    assert_eq!(krows.len(), 2);
    let frigate_group = ctx.sde.types[&587].group_id as i64;
    let (_, grows) = rows(&call(&ctx, &mut sess1, "warStatisticMgr", "GetKillsByGroup", vec![PyRep::Integer(war_id)]).await.unwrap());
    assert_eq!(grows[0][0], PyRep::Integer(frigate_group));
    assert_eq!(grows[0][1], PyRep::Integer(2));
    let km = call(&ctx, &mut sess1, "warStatisticMgr", "GetKillMail", vec![PyRep::Integer(km1)]).await.unwrap();
    let _ = km2;
    assert!(field(&km, "iskLost").as_f64().unwrap() > 0.0);

    // peace-treaty: defender offers surrender, aggressor accepts; war ends 24h later, 2-week treaty.
    let surr = call(&ctx, &mut sess2, "warsInfoMgr", "CreateSurrenderNegotiation", vec![PyRep::Integer(war_id), PyRep::Real(50_000_000.0), PyRep::String("Peace terms".into())]).await.unwrap();
    let neg_id = field(&surr, "negotiationID").as_i64().unwrap();
    assert_eq!(call(&ctx, &mut sess2, "warsInfoMgr", "AcceptSurrender", vec![PyRep::Integer(neg_id)]).await.expect_err("own offer").code, "NotNegotiationCounterparty");
    let accepted_at = ctx.now_unix() as i64;
    let acc = call(&ctx, &mut sess1, "warsInfoMgr", "AcceptSurrender", vec![PyRep::Integer(neg_id)]).await.expect("AcceptSurrender");
    assert_eq!(field(&acc, "timeFinished"), PyRep::Integer(accepted_at + 86_400));
    assert_eq!(field(&acc, "peaceTreatyExpiry"), PyRep::Integer(accepted_at + 86_400 + 14 * 86_400));
    assert_eq!(corp_wallet(&ctx, agg), 950_000_000.0, "surrender ISK paid to the aggressor");
    assert_eq!(field(&call(&ctx, &mut sess1, "warsInfoMgr", "GetPublicWarInfo", vec![PyRep::Integer(war_id)]).await.unwrap(), "active"), PyRep::Bool(true));
    ctx.advance_secs(86_401);
    assert_eq!(field(&call(&ctx, &mut sess1, "warsInfoMgr", "GetPublicWarInfo", vec![PyRep::Integer(war_id)]).await.unwrap(), "active"), PyRep::Bool(false));
    let (tcols, trows) = rows(&call(&ctx, &mut sess1, "peaceTreatyMgr", "GetPeaceTreatiesForSession", vec![]).await.unwrap());
    assert_eq!(trows[0][col(&tcols, "otherOwnerID")], PyRep::Integer(def));
    assert_eq!(call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect_err("treaty").code, "PeaceTreatyActive");
    ctx.advance_secs(14 * 86_400);
    assert!(rows(&call(&ctx, &mut sess1, "peaceTreatyMgr", "GetPeaceTreatiesForSession", vec![]).await.unwrap()).1.is_empty());

    // war-eligibility-change: the defender loses its last structure -> ineligible, war ends.
    let war2 = field(&call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect("redeclare after treaty"), "warID").as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&995_000_002).unwrap().state = "Destroyed".into());
    let list = call(&ctx, &mut sess1, "warsInfoMgr", "GetWarsByOwnerID", vec![PyRep::Integer(agg)]).await.unwrap();
    assert_eq!(war_field(&list, war2, "active"), Some(PyRep::Bool(false)));
    assert_eq!(call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect_err("ineligible").code, "DefenderNotWarEligible");

    // war-hq: destroying the attacker's War HQ ends the war.
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(995_000_003, structure(995_000_003, def, "Defender Replacement"));
    });
    let war3 = field(&call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect("third war"), "warID").as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&995_000_001).unwrap().state = "Destroyed".into());
    assert_eq!(field(&call(&ctx, &mut sess1, "warsInfoMgr", "GetPublicWarInfo", vec![PyRep::Integer(war3)]).await.unwrap(), "active"), PyRep::Bool(false));

    // mutual-war: invite, response requires a pending invite, accepted war is mutual.
    assert_eq!(call(&ctx, &mut sess3, "mutualWarInviteMgr", "RespondToInviteByPlayer", vec![PyRep::Integer(agg), PyRep::Bool(true)]).await.expect_err("no invite").code, "MutualWarInviteNotFound");
    call(&ctx, &mut sess1, "mutualWarInviteMgr", "SendInviteByPlayer", vec![PyRep::Integer(ally)]).await.unwrap();
    let (_, pend) = rows(&call(&ctx, &mut sess3, "mutualWarInviteMgr", "GetPendingInvitesForSession", vec![]).await.unwrap());
    assert_eq!(pend, vec![vec![PyRep::Integer(agg), PyRep::Integer(ally), PyRep::Integer(ctx.now_unix() as i64)]]);
    let resp = call(&ctx, &mut sess3, "mutualWarInviteMgr", "RespondToInviteByPlayer", vec![PyRep::Integer(agg), PyRep::Bool(true)]).await.unwrap();
    let mwar = field(&resp, "warID").as_i64().unwrap();
    let minfo = call(&ctx, &mut sess3, "warsInfoMgr", "GetPublicWarInfo", vec![PyRep::Integer(mwar)]).await.unwrap();
    assert_eq!(field(&minfo, "mutual"), PyRep::Bool(true));
    assert_eq!(field(&minfo, "active"), PyRep::Bool(true));
    let ret = call(&ctx, &mut sess3, "warsInfoMgr", "RetractMutualWar", vec![PyRep::Integer(mwar)]).await.unwrap();
    assert_eq!(field(&ret, "timeFinished"), PyRep::Integer(ctx.now_unix() as i64 + 86_400));
    ctx.advance_secs(86_401);
    assert_eq!(field(&call(&ctx, &mut sess3, "warsInfoMgr", "GetPublicWarInfo", vec![PyRep::Integer(mwar)]).await.unwrap(), "active"), PyRep::Bool(false));
}
