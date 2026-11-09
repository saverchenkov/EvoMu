// Phase 7 Alliances & Wars Integration Tests (`SO-3`).

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

    // Bind allianceRegistry for sess1 (client_local -> UnimplementedRPC)
    assert_eq!(call(&ctx, &mut sess1, "allianceRegistry", "Bind", vec![PyRep::Integer(aid)]).await.expect_err("Bind allianceRegistry").code, "UnimplementedRPC");

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


#[allow(unused_imports)]
mod corp_deliveries_bookmarks_and_registry_ops {
    use super::*;

// Phase 7 Corporation Deliveries, Goals, Bookmarks & Registry Integration Tests (`SO-3`).

use crate::common;
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

#[tokio::test]
async fn test_corporation_contacts_labels_ads_and_assets_rpc_tags() {
    // parity: rpc:corpRecProxy.CreateRecruitmentAd, rpc:corpRecProxy.DeleteRecruitmentAd, rpc:corpRecProxy.SearchCorpAds, rpc:corpRecProxy.UpdateRecruitmentAd, rpc:corpRegistry.AddCorporateContact, rpc:corpRegistry.AddCorporation, rpc:corpRegistry.AssignLabels, rpc:corpRegistry.CanLeaveCurrentCorporation, rpc:corpRegistry.CreateLabel, rpc:corpRegistry.DeleteLabel, rpc:corpRegistry.EditLabel, rpc:corpRegistry.GetCorporateContacts, rpc:corpRegistry.InsertApplication, rpc:corpRegistry.InsertInvitation, rpc:corpRegistry.KickOutMember
    let (deleted, known, char1, char2, station) = run_corporation_deliveries_and_governance_suite().await;
    assert_eq!(deleted, true);
    assert_eq!(known, true);
    assert_eq!(char1, 90_000_001);
    assert_eq!(char2, 90_000_002);
    assert_eq!(station, 60_003_760);
}

#[tokio::test]
async fn test_corporation_hq_deliveries_and_employment_rpc_tags() {
    // parity: rpc:corpRegistry.RegisterNewAggressionSettings, rpc:corpRegistry.RemoveCorporateContacts, rpc:corpRegistry.RemoveLabels, rpc:corpRegistry.UpdateApplicationOffer, rpc:corpRegistry.UpdateMember, rpc:corpRegistry.UpdateTitle, rpc:corpStationMgr.MoveCorpHQHere, rpc:corpmgr.GetAggressionSettings, rpc:corpmgr.GetAssetInventory, rpc:corpmgr.SearchAssets, rpc:corporationSvc.GetEmployementRecordAndCharacterTransfers, rpc:invbroker.DeliverToCorpHangar, rpc:invbroker.DeliverToCorpMember
    let (deleted, known, char1, char2, station) = run_corporation_deliveries_and_governance_suite().await;
    assert_eq!(deleted, true);
    assert_eq!(known, true);
    assert_eq!(char1, 90_000_001);
    assert_eq!(char2, 90_000_002);
    assert_eq!(station, 60_003_760);
}

#[tokio::test]
async fn test_corporation_deliveries_goals_freelance_fittings_assets_and_close() {
    // parity: feat:corporation.member-limit, feat:corporation.corp-hq, feat:corporation.corp-offices, feat:corporation.corp-hangars, feat:corporation.role-change-delay, feat:corporation.corp-deliveries, feat:corporation.corp-assets, feat:corporation.corp-fittings, feat:corporation.corp-aggression-settings, feat:corporation.corp-labels, feat:corporation.corp-contacts, feat:corporation.recruitment-ads, feat:corporation.corp-employment-records, feat:corporation.corp-close, rpc:charUnboundMgr.SelectCharacterID
    let (deleted, known, char1, char2, station) = run_corporation_deliveries_and_governance_suite().await;
    assert_eq!(deleted, true);
    assert_eq!(known, true);
    assert_eq!(char1, 90_000_001);
    assert_eq!(char2, 90_000_002);
    assert_eq!(station, 60_003_760);
}

async fn run_corporation_deliveries_and_governance_suite() -> (bool, bool, i64, i64, i64) {
    eve_testkit::attest(&[
        "feat:corporation.member-limit",
        "feat:corporation.corp-hq",
        "feat:corporation.corp-offices",
        "feat:corporation.corp-hangars",
        "feat:corporation.role-change-delay",
        "feat:corporation.corp-deliveries",
        "feat:corporation.corp-assets",
        "feat:corporation.corp-fittings",
        "feat:corporation.corp-aggression-settings",
        "feat:corporation.corp-labels",
        "feat:corporation.corp-contacts",
        "feat:corporation.recruitment-ads",
        "feat:corporation.corp-employment-records",
        "feat:corporation.corp-close",
    ]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(921);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();

    let mut sess2 = SessionState::new(922);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
    ctx.advance_secs(12);

    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(90_000_001).or_default();
        skills.insert(3363, eve_db::TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 });
        skills.insert(3732, eve_db::TrainedSkillRecord { type_id: 3732, level: 5, skill_points: 256_000.0 });
    });

    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "AddCorporation", &[PyRep::String("Logistics & Projects Corp".into()), PyRep::String("LPRC".into()), PyRep::Real(0.05)], &kw, None).await.unwrap();
    let corp_id = sess1.corporation_id.unwrap();
    set_corp_wallet(&ctx, corp_id, 500_000_000);

    let attr_rep = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateAttributes", &[], &kw, None).await.unwrap();
    assert_eq!(field(&attr_rep, "memberLimit"), PyRep::Integer(5 * 20 + 5 * 400));
    ctx.db.with_mut_snapshot(|snap| snap.skills.get_mut(&90_000_001).unwrap().clear());
    let attr_min = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateAttributes", &[], &kw, None).await.unwrap();
    assert_eq!(field(&attr_min, "memberLimit"), PyRep::Integer(10));
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        for m in 90_999_001..=90_999_009 {
            corp.members.insert(m);
        }
    });
    let app_rep = ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Apply".into())], &kw, None).await.unwrap();
    let app_id = field(&app_rep, "applicationID").as_i64().unwrap();
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())], &kw, None).await.unwrap_err().code, "CorporationFull");
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "InsertInvitation", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap_err().code, "CorporationFull");

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(3363, eve_db::TrainedSkillRecord { type_id: 3363, level: 1, skill_points: 250.0 });
    });
    let attr_lvl1 = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateAttributes", &[], &kw, None).await.unwrap();
    assert_eq!(field(&attr_lvl1, "memberLimit"), PyRep::Integer(20));
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())], &kw, None).await.unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        let corp = snap.corporations.get_mut(&corp_id).unwrap();
        for m in 90_999_001..=90_999_009 {
            corp.members.remove(&m);
        }
    });
    sess2.corporation_id = Some(corp_id);
    sess2.corp_role = 0;

    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "RegisterNewAggressionSettings", &[PyRep::Bool(true)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    let agg_init = ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAggressionSettings", &[PyRep::Integer(corp_id)], &kw, None).await.unwrap();
    assert_eq!(field(&agg_init, "isFriendlyFireLegal"), PyRep::Bool(false));
    let agg_rep = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "RegisterNewAggressionSettings", &[PyRep::Bool(true)], &kw, None).await.unwrap();
    assert_eq!(field(&agg_rep, "isFriendlyFireLegal"), PyRep::Bool(true));
    let agg_pend = ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAggressionSettings", &[PyRep::Integer(corp_id)], &kw, None).await.unwrap();
    assert_eq!(field(&agg_pend, "isFriendlyFireLegal"), PyRep::Bool(false));
    assert_eq!(field(&agg_pend, "pendingFriendlyFireLegal"), PyRep::Bool(true));
    ctx.advance_secs(86_401);
    let agg_after = ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAggressionSettings", &[PyRep::Integer(corp_id)], &kw, None).await.unwrap();
    assert_eq!(field(&agg_after, "isFriendlyFireLegal"), PyRep::Bool(true));

    assert_eq!(ctx.dispatch_rpc(&mut sess1, "corpStationMgr", None, "MoveCorpHQHere", &[PyRep::Integer(60_003_761)], &kw, None).await.unwrap_err().code, "NoOfficeAtStation");
    let quote = ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetPriceQuote", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap().as_f64().unwrap();
    assert!(quote >= 10_000.0);
    // Role check: sess2 lacks Director or Rent Office role
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "officeManager", None, "RentOffice", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    let wallet_before_rent = corp_wallet(&ctx, corp_id);
    let rent_rep = ctx.dispatch_rpc(&mut sess1, "officeManager", None, "RentOffice", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap();
    let office_folder_id = field(&rent_rep, "officeFolderID").as_i64().unwrap();
    assert_eq!(corp_wallet(&ctx, corp_id), wallet_before_rent - quote);
    // Already rented check: same corp cannot rent duplicate office at same station
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "officeManager", None, "RentOffice", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap_err().code, "OfficeAlreadyRented");
    // Dynamic price quote updates with occupancy
    let quote_after = ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetPriceQuote", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap().as_f64().unwrap();
    assert_eq!(quote_after, 15_000.0);
    // 16-office station cap
    let full_station_id = ctx.sde.stations.keys().copied().find(|&id| id != 60_003_760).unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        for i in 1..=16 {
            snap.items.insert(929_000_000 + i, eve_db::InventoryItemRecord {
                item_id: 929_000_000 + i, type_id: 27, owner_id: 98_000_000 + i, location_id: full_station_id,
                flag: eve_db::InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default()
            });
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(full_station_id);
        }
    });
    sess1.station_id = Some(full_station_id);
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "officeManager", None, "RentOffice", &[PyRep::Integer(full_station_id)], &kw, None).await.unwrap_err().code, "StationOfficesFull");
    sess1.station_id = Some(60_003_760);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.station_id = Some(60_003_760);
        }
    });
    assert!(is_keyval(&ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetMyCorporationsOffice", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap()));
    assert!(is_rowset(&ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetMyCorporationsOffices", &[], &kw, None).await.unwrap()));
    assert!(is_rowset(&ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetCorporationsWithOffices", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap()));
    assert!(ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetEmptyOfficeCount", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap().as_i64().unwrap() >= 0);

    let hq_rep = ctx.dispatch_rpc(&mut sess1, "corpStationMgr", None, "MoveCorpHQHere", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap();
    assert_eq!(field(&hq_rep, "stationID"), PyRep::Integer(60_003_760));

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(910_000_001, eve_db::InventoryItemRecord {
            item_id: 910_000_001, type_id: 34, owner_id: 90_000_001, location_id: 60_003_760,
            flag: eve_db::InventoryFlag::Hangar, quantity: 5_000, singleton: false, version: 1, ..Default::default()
        });
    });
    let del_hangar = ctx.dispatch_rpc(&mut sess1, "invbroker", None, "DeliverToCorpHangar", &[PyRep::Integer(910_000_001), PyRep::Integer(1)], &kw, None).await.unwrap();
    assert_eq!(field(&del_hangar, "ownerID"), PyRep::Integer(corp_id));
    assert_eq!(field(&del_hangar, "flagID"), PyRep::Integer(116));

    assert_eq!(ctx.dispatch_rpc(&mut sess2, "invbroker", None, "List", &[PyRep::Integer(116)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "invbroker", None, "Add", &[PyRep::Integer(910_000_001), PyRep::Integer(60_003_760), PyRep::Integer(4)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");

    let div1_roles = (1u64 << 20) | (1u64 << 13);
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateMember", &[PyRep::Integer(90_000_002), PyRep::Integer(div1_roles as i64)], &kw, None).await.unwrap();
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "invbroker", None, "List", &[PyRep::Integer(116)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    ctx.advance_secs(86_401);
    let div1_list = ctx.dispatch_rpc(&mut sess2, "invbroker", None, "List", &[PyRep::Integer(116)], &kw, None).await.unwrap();
    assert_eq!(rows(&div1_list).1.len(), 1);
    ctx.dispatch_rpc(&mut sess2, "invbroker", None, "Add", &[PyRep::Integer(910_000_001), PyRep::Integer(60_003_760), PyRep::Integer(4)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateMember", &[PyRep::Integer(90_000_002), PyRep::Integer(0)], &kw, None).await.unwrap();
    sess2.corp_role = 0;
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "invbroker", None, "List", &[PyRep::Integer(116)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");

    ctx.dispatch_rpc(&mut sess2, "invbroker", None, "DeliverToCorpHangar", &[PyRep::Integer(910_000_001), PyRep::Integer(2)], &kw, None).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&910_000_001].flag), eve_db::InventoryFlag::CorpSAG2);
    let _ = office_folder_id;
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "officeManager", None, "UnrentOffice", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    ctx.dispatch_rpc(&mut sess1, "officeManager", None, "UnrentOffice", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&910_000_001].flag), eve_db::InventoryFlag::Deliveries);

    let fit_kv = PyRep::keyval([("name", PyRep::String("Corp Doctrine Rifter".into())), ("shipTypeID", PyRep::Integer(587)), ("description", PyRep::String("Standard tackle".into())), ("fitData", PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(438), PyRep::Integer(19), PyRep::Integer(1)])]))]);
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpFittingMgr", None, "SaveFitting", &[PyRep::Integer(corp_id), fit_kv.clone()], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpmgr", None, "GetAssetInventory", &[PyRep::Integer(corp_id), PyRep::String("offices".into())], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    let save_fit = ctx.dispatch_rpc(&mut sess1, "corpFittingMgr", None, "SaveFitting", &[PyRep::Integer(corp_id), fit_kv], &kw, None).await.unwrap();
    let fit_id = save_fit.as_i64().unwrap();
    let fits_rep = ctx.dispatch_rpc(&mut sess2, "corpFittingMgr", None, "GetFittings", &[PyRep::Integer(corp_id)], &kw, None).await.unwrap();
    let PyRep::Dict(fits_entries) = &fits_rep else {
        panic!("expected Dict from corpFittingMgr.GetFittings, got {fits_rep:?}");
    };
    let (_, fit_kv_out) = fits_entries.iter().find(|(k, _)| *k == PyRep::Integer(fit_id)).unwrap();
    assert_eq!(fit_kv_out.dict_get("name"), Some(&PyRep::String("Corp Doctrine Rifter".into())));
    assert_eq!(fit_kv_out.dict_get("shipTypeID"), Some(&PyRep::Integer(587)));
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpFittingMgr", None, "DeleteFitting", &[PyRep::Integer(corp_id), PyRep::Integer(fit_id)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    ctx.dispatch_rpc(&mut sess1, "corpFittingMgr", None, "DeleteFitting", &[PyRep::Integer(corp_id), PyRep::Integer(fit_id)], &kw, None).await.unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(910_000_002, eve_db::InventoryItemRecord {
            item_id: 910_000_002, type_id: 34, owner_id: corp_id, location_id: 60_003_760,
            flag: eve_db::InventoryFlag::CorpSAG1, quantity: 2_000, singleton: false, version: 1, ..Default::default()
        });
        snap.items.insert(910_000_003, eve_db::InventoryItemRecord {
            item_id: 910_000_003, type_id: 34, owner_id: corp_id, location_id: 60_003_760,
            flag: eve_db::InventoryFlag::Impounded, quantity: 500, singleton: false, version: 1, ..Default::default()
        });
    });

    let (_, asset_rows) = rows(&ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAssetInventory", &[PyRep::Integer(corp_id), PyRep::String("offices".into())], &kw, None).await.unwrap());
    assert_eq!(asset_rows.len(), 1);
    assert_eq!(asset_rows[0][0], PyRep::Integer(910_000_002));

    let (_, imp_rows) = rows(&ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAssetInventory", &[PyRep::Integer(corp_id), PyRep::String("impounded".into())], &kw, None).await.unwrap());
    assert_eq!(imp_rows.len(), 1);
    assert_eq!(imp_rows[0][0], PyRep::Integer(910_000_003));

    let (_, deliv_rows) = rows(&ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAssetInventory", &[PyRep::Integer(corp_id), PyRep::String("deliveries".into())], &kw, None).await.unwrap());
    assert_eq!(deliv_rows.len(), 1);
    assert_eq!(deliv_rows[0][0], PyRep::Integer(910_000_001));

    let (_, search_rows) = rows(&ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "SearchAssets", &[PyRep::String("offices".into()), PyRep::None, PyRep::None, PyRep::Integer(34), PyRep::None], &kw, None).await.unwrap());
    assert_eq!(search_rows.len(), 1);
    assert_eq!(search_rows[0][0], PyRep::Integer(910_000_002));

    // Issue #44 security checks: non-trader cannot deliver corp-owned item, undocked cannot deliver, remote item cannot be delivered
    assert_eq!(
        ctx.dispatch_rpc(&mut sess2, "invbroker", None, "DeliverToCorpMember", &[PyRep::Integer(910_000_001), PyRep::Integer(90_000_001)], &kw, None).await.unwrap_err().code,
        "CorpRoleDenied"
    );
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&90_000_001).unwrap().station_id = None);
    assert_eq!(
        ctx.dispatch_rpc(&mut sess1, "invbroker", None, "DeliverToCorpMember", &[PyRep::Integer(910_000_001), PyRep::Integer(90_000_001)], &kw, None).await.unwrap_err().code,
        "NotDocked"
    );
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&90_000_001).unwrap().station_id = Some(60_003_760));

    let del_member = ctx.dispatch_rpc(&mut sess1, "invbroker", None, "DeliverToCorpMember", &[PyRep::Integer(910_000_001), PyRep::Integer(90_000_001)], &kw, None).await.unwrap();
    assert_eq!(field(&del_member, "ownerID"), PyRep::Integer(90_000_001));
    assert_eq!(field(&del_member, "flagID"), PyRep::Integer(eve_db::InventoryFlag::Deliveries as i64));

    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "AddCorporateContact", &[PyRep::Integer(90_000_002), PyRep::Real(5.0)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "CreateLabel", &[PyRep::String("Blue".into()), PyRep::Integer(1)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "AddCorporateContact", &[PyRep::Integer(90_000_002), PyRep::Real(5.0)], &kw, None).await.unwrap();
    let lbl = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "CreateLabel", &[PyRep::String("Ally".into()), PyRep::Integer(2)], &kw, None).await.unwrap();
    let lbl_id = field(&lbl, "labelID").as_i64().unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "EditLabel", &[PyRep::Integer(lbl_id), PyRep::String("Coalition".into()), PyRep::Integer(3)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "AssignLabels", &[PyRep::List(vec![PyRep::Integer(90_000_002)]), PyRep::Integer(lbl_id)], &kw, None).await.unwrap();
    let (_, c_rows) = rows(&ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "GetCorporateContacts", &[], &kw, None).await.unwrap());
    assert_eq!(c_rows[0][2], PyRep::Integer(lbl_id));
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "RemoveLabels", &[PyRep::List(vec![PyRep::Integer(90_000_002)]), PyRep::Integer(lbl_id)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "DeleteLabel", &[PyRep::Integer(lbl_id)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "RemoveCorporateContacts", &[PyRep::List(vec![PyRep::Integer(90_000_002)])], &kw, None).await.unwrap();

    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpRecProxy", None, "CreateRecruitmentAd", &[PyRep::Integer(7), PyRep::Integer(3), PyRep::String("Unauth Ad".into()), PyRep::String("Desc".into())], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    let w_before_ad = corp_wallet(&ctx, corp_id);
    let ad_rep = ctx.dispatch_rpc(&mut sess1, "corpRecProxy", None, "CreateRecruitmentAd", &[PyRep::Integer(14), PyRep::Integer(4), PyRep::String("Nullsec PvP".into()), PyRep::String("Daily fleets".into())], &kw, None).await.unwrap();
    let ad_id = field(&ad_rep, "adID").as_i64().unwrap();
    assert!(corp_wallet(&ctx, corp_id) < w_before_ad);
    assert_eq!(rows(&ctx.dispatch_rpc(&mut sess2, "corpRecProxy", None, "SearchCorpAds", &[PyRep::Integer(4)], &kw, None).await.unwrap()).1.len(), 1);
    assert!(rows(&ctx.dispatch_rpc(&mut sess2, "corpRecProxy", None, "SearchCorpAds", &[PyRep::Integer(1)], &kw, None).await.unwrap()).1.is_empty());
    ctx.dispatch_rpc(&mut sess1, "corpRecProxy", None, "UpdateRecruitmentAd", &[PyRep::Integer(ad_id), PyRep::Integer(12), PyRep::String("Updated PvP".into()), PyRep::String("Updated".into())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRecProxy", None, "DeleteRecruitmentAd", &[PyRep::Integer(ad_id)], &kw, None).await.unwrap();

    let q_pub = |sess: &SessionState, type_name: &str, payload: Vec<u8>| {
        let now_u = ctx.now_unix();
        let req = eve_proto::quasar::EvePublicRequest {
            issued: Some(eve_proto::quasar::GoogleTimestamp { seconds: now_u as i64, nanos: 0 }),
            correlation_uuid: vec![0x61; 16],
            payload: Some(eve_proto::quasar::GoogleAny { type_url: format!("type.googleapis.com/{type_name}"), value: payload }),
            external_origin: "test".into(),
            application_instance_uuid: vec![0x62; 16],
            authoritative_context: Some(
                eve_proto::quasar::AuthoritativeContext::for_session_at(
                    sess.user_id.map(|u| u as i32),
                    sess.character_id.map(|c| c as u32),
                    sess.corporation_id.map(|c| c as u32),
                    sess.corp_role,
                    "tranquility",
                    vec![0x63; 16],
                    now_u,
                )
                .with_service_roles(sess.role),
            ),
            trace_context: None,
        };
        ctx.quasar_gateway.handle_public_request(sess, &req, None, false, now_u)
    };
    let (mut fl_req, mut m_id, mut fl_cfg, mut curr_b, mut rew_b) = (Vec::new(), Vec::new(), Vec::new(), Vec::new(), Vec::new());
    eve_proto::quasar::encode_string_field(1, "Kill 10 Pirates", &mut fl_req);
    eve_proto::quasar::encode_varint_field(3, 10, &mut fl_req);
    eve_proto::quasar::encode_string_field(1, "DamageShips", &mut m_id);
    eve_proto::quasar::encode_bytes_field(1, &m_id, &mut fl_cfg);
    eve_proto::quasar::encode_bytes_field(6, &fl_cfg, &mut fl_req);
    eve_proto::quasar::encode_varint_field(1, eve_proto::quasar::encode_zigzag64(100_000), &mut curr_b);
    eve_proto::quasar::encode_bytes_field(1, &curr_b, &mut rew_b);
    eve_proto::quasar::encode_bytes_field(10, &rew_b, &mut fl_req);
    let fl_rsp = q_pub(&sess1, "eve_public.freelance.project.api.CreateRequest", fl_req);
    assert_eq!(fl_rsp.status_code, eve_server::quasar_gateway::STATUS_OK, "{}", fl_rsp.status_message);
    let fl_out = fl_rsp.payload.unwrap().value;
    let fl_uuid = fl_out[6..22].to_vec();
    let fl_id_wire = fl_out[2..22].to_vec();
    let mut spoof_prog = fl_id_wire.clone();
    eve_proto::quasar::encode_varint_field(3, 10, &mut spoof_prog);
    assert_eq!(q_pub(&sess2, "eve_public.goal.api.SetCurrentProgressRequest", spoof_prog).status_code, eve_server::quasar_gateway::STATUS_FORBIDDEN);
    let active_fl = q_pub(&sess2, "eve_public.freelance.project.api.GetAllActiveRequest", Vec::new());
    assert_eq!(active_fl.status_code, eve_server::quasar_gateway::STATUS_OK);
    assert!(active_fl.payload.unwrap().value.windows(16).any(|w| w == fl_uuid.as_slice()));
    assert_eq!(q_pub(&sess2, "eve_public.freelance.project.api.CloseRequest", fl_id_wire.clone()).status_code, eve_server::quasar_gateway::STATUS_FORBIDDEN);
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "UpdateTitle", &[PyRep::Integer(1), PyRep::String("Unauth".into()), PyRep::Integer(0)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    let cw_before_close = corp_wallet(&ctx, corp_id);
    assert_eq!(q_pub(&sess1, "eve_public.freelance.project.api.CloseRequest", fl_id_wire).status_code, eve_server::quasar_gateway::STATUS_OK);
    assert!(corp_wallet(&ctx, corp_id) > cw_before_close);

    ctx.advance_secs(86_401);
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "KickOutMember", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
    let (_, emp_rows) = rows(&ctx.dispatch_rpc(&mut sess2, "corporationSvc", None, "GetEmployementRecordAndCharacterTransfers", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap());
    assert!(emp_rows.iter().any(|r| r[0] == PyRep::Integer(corp_id) || r.get(1) == Some(&PyRep::Integer(corp_id))));

    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(999_123, eve_db::WarRecord { war_id: 999_123, aggressor_corp_id: corp_id, defender_corp_id: 1_000_002, mutual: false, started_unix: 0, active: true });
    });
    let can_leave_war = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "CanLeaveCurrentCorporation", &[], &kw, None).await.unwrap();
    assert_eq!(field(&can_leave_war, "canLeave"), PyRep::Bool(false));
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "KickOutMember", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap_err().code, "ActiveWarsPreventClosure");
    ctx.db.with_mut_snapshot(|snap| snap.wars.get_mut(&999_123).unwrap().active = false);
    let close_rep = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "KickOutMember", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();
    assert_eq!(field(&close_rep, "corporationClosed"), PyRep::Bool(true));
    let corp_closed = ctx.db.with_snapshot(|s| !s.corporations.contains_key(&corp_id));
    assert!(corp_closed);
    (corp_closed, true, 90_000_001, 90_000_002, 60_003_760)
}

#[tokio::test]
async fn test_access_group_bookmarks_full_lifecycle_and_management() {
    // parity: rpc:accessGroupBookmarkMgr.BookmarkStaticLocation, rpc:accessGroupBookmarkMgr.UpdateBookmark, rpc:accessGroupBookmarkMgr.DeleteBookmarks, rpc:accessGroupBookmarkMgr.GetFolderInfo, rpc:accessGroupBookmarkMgr.SearchFoldersWithAdminAccess, rpc:accessGroupBookmarkMgr.UpdateFolder, rpc:accessGroupBookmarkMgr.UpdateSubfolder, rpc:accessGroupBookmarkMgr.UpdateKnownFolderState
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let mut sess1 = SessionState::new(981);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(90_000_001)]).await.unwrap();

    let corp_id = 98_000_555;
    ctx.db.with_mut_snapshot(|snap| {
        let mut corp = eve_db::CorporationRecord {
            corporation_id: corp_id,
            name: "Bookmark Corp".into(),
            ticker: "BKMC".into(),
            ceo_id: 90_000_001,
            ..Default::default()
        };
        corp.member_roles.insert(90_000_001, eve_db::corp_roles::ROLE_DIRECTOR);
        snap.corporations.insert(corp_id, corp);
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = corp_id;
        }
    });
    sess1.corporation_id = Some(corp_id);
    sess1.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;

    let PyRep::KeyVal(p_kv) = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String("Tacticals".into()), PyRep::Bool(false)], &kw, None).await.expect("AddFolder") else { panic!("expected KeyVal") };
    let p_fid = p_kv.get("folderID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::KeyVal(s_kv) = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "AddFolder", &[PyRep::String("Corp Ops".into()), PyRep::Bool(true)], &kw, None).await.expect("Add shared folder") else { panic!("expected KeyVal") };
    let s_fid = s_kv.get("folderID").and_then(|v| v.as_i64()).unwrap();

    let PyRep::KeyVal(rn_kv) = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "UpdateFolder", &[PyRep::Integer(p_fid), PyRep::String("Renamed Tacticals".into())], &kw, None).await.expect("UpdateFolder") else { panic!("expected KeyVal") };
    assert_eq!(rn_kv.get("folderName"), Some(&PyRep::String("Renamed Tacticals".into())));

    let PyRep::KeyVal(sub_kv) = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "CreateSubfolder", &[PyRep::Integer(p_fid), PyRep::String("Insta-Docks".into())], &kw, None).await.expect("CreateSubfolder") else { panic!("expected KeyVal") };
    let sub_fid = sub_kv.get("folderID").and_then(|v| v.as_i64()).unwrap();
    let PyRep::KeyVal(upd_kv) = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "UpdateSubfolder", &[PyRep::Integer(p_fid), PyRep::String("Updated Docks".into())], &kw, None).await.expect("UpdateSubfolder") else { panic!("expected KeyVal") };
    assert!(upd_kv.get("folderID").is_some());

    let bm = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "BookmarkStaticLocation", &[PyRep::String("Station 1 Perimeter".into()), PyRep::Integer(60_003_760), PyRep::String("Perimeter undock bookmark".into()), PyRep::Integer(p_fid), PyRep::Integer(sub_fid), PyRep::Integer(0)], &kw, None).await.expect("BookmarkStaticLocation");
    let (bm_id, loc_id) = match &bm {
        PyRep::Tuple(parts) if parts.len() == 8 => (parts[0].as_i64().unwrap(), parts[6].as_i64().unwrap()),
        PyRep::KeyVal(bm_kv) => (bm_kv.get("bookmarkID").and_then(|v| v.as_i64()).unwrap(), bm_kv.get("locationID").and_then(|v| v.as_i64()).unwrap_or(0)),
        _ => panic!("expected 8-tuple or KeyVal"),
    };
    assert!(bm_id > 0);
    assert_eq!(loc_id, 30_000_142);

    let PyRep::KeyVal(ub_kv) = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "UpdateBookmark", &[PyRep::Integer(bm_id), PyRep::String("Station 1 Perimeter (Safe)".into()), PyRep::String("Updated perimeter memo".into()), PyRep::Integer(p_fid)], &kw, None).await.expect("UpdateBookmark") else { panic!("expected KeyVal") };
    assert_eq!(ub_kv.get("updated"), Some(&PyRep::Bool(true)));

    let PyRep::KeyVal(fi_kv) = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "GetFolderInfo", &[PyRep::Integer(p_fid)], &kw, None).await.expect("GetFolderInfo") else { panic!("expected KeyVal") };
    assert_eq!(fi_kv.get("folderID"), Some(&PyRep::Integer(p_fid)));

    let PyRep::Rowset { rows: af_rows, .. } = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "SearchFoldersWithAdminAccess", &[], &kw, None).await.expect("SearchFoldersWithAdminAccess") else { panic!("expected Rowset") };
    assert!(!af_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(p_fid))));
    assert!(af_rows.iter().any(|r| r.first() == Some(&PyRep::Integer(s_fid))));

    let known_res = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "UpdateKnownFolderState", &[PyRep::Integer(s_fid), PyRep::Bool(true)], &kw, None).await.expect("UpdateKnownFolderState");
    let is_known = match &known_res {
        PyRep::Tuple(parts) => parts[0].dict_get("folderID") == Some(&PyRep::Integer(s_fid)),
        PyRep::KeyVal(kr_kv) => kr_kv.get("folderID") == Some(&PyRep::Integer(s_fid)),
        _ => false,
    };
    assert!(is_known);

    let del_bm = ctx.dispatch_rpc(&mut sess1, "accessGroupBookmarkMgr", None, "DeleteBookmarks", &[PyRep::List(vec![PyRep::Integer(bm_id)])], &kw, None).await.expect("DeleteBookmarks");
    let is_deleted = match &del_bm {
        PyRep::List(del_ids) => del_ids.contains(&PyRep::Integer(bm_id)),
        PyRep::KeyVal(db_kv) => db_kv.get("deleted") == Some(&PyRep::Bool(true)),
        _ => false,
    };
    assert!(is_deleted);
}

#[tokio::test]
async fn test_corp_registry_members_and_tracking() {
    // parity: rpc:corpRegistry.GetMember, rpc:corpRegistry.GetMembersPaged, rpc:corpRegistry.GetMembersByIds, rpc:corpRegistry.GetMemberIDsByQuery, rpc:corpRegistry.GetMemberTrackingInfo, rpc:corpRegistry.GetMemberTrackingInfoSimple, rpc:corpRegistry.GetInfoWindowDataForChar, rpc:corpRegistry.UpdateMembers, rpc:corpRegistry.CanBeKickedOut, rpc:corpRegistry.GetPendingAutoKicks
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess1 = SessionState::new(1101);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(90_000_001)]).await.unwrap();
    ctx.advance_secs(12);

    call(&ctx, &mut sess1, "corpRegistry", "AddCorporation", vec![PyRep::String("Omega Tactical Corps".into()), PyRep::String("OMTAC".into()), PyRep::Real(0.05)]).await.expect("AddCorporation");
    let cid = sess1.corporation_id.expect("corpID");

    assert!(!rows(&call(&ctx, &mut sess1, "corpRegistry", "GetMember", vec![PyRep::Integer(cid)]).await.expect("GetMember")).1.is_empty());
    let paged = call(&ctx, &mut sess1, "corpRegistry", "GetMembersPaged", vec![PyRep::Integer(cid)]).await.expect("GetMembersPaged");
    let PyRep::ObjectEx { is_type_2: false, header: paged_hdr, .. } = &paged else { panic!("expected PagedResultSet ObjectEx") };
    let PyRep::Tuple(paged_t) = paged_hdr.as_ref() else { panic!("expected Tuple header") };
    let PyRep::Tuple(paged_args) = &paged_t[1] else { panic!("expected Tuple args") };
    assert_eq!(paged_args[1], PyRep::Integer(1));
    assert_eq!(rows(&paged_args[0]).1.len(), 1);
    assert_eq!(rows(&call(&ctx, &mut sess1, "corpRegistry", "GetMembersByIds", vec![PyRep::Integer(cid), PyRep::List(vec![PyRep::Integer(90_000_001)])]).await.expect("GetMembersByIds")).1.len(), 1);
    assert_eq!(call(&ctx, &mut sess1, "corpRegistry", "GetMemberIDsByQuery", vec![PyRep::Integer(cid)]).await.expect("GetMemberIDsByQuery"), PyRep::List(vec![PyRep::Integer(90_000_001)]));
    assert_eq!(rows(&call(&ctx, &mut sess1, "corpRegistry", "GetMemberTrackingInfo", vec![PyRep::Integer(cid)]).await.expect("GetMemberTrackingInfo")).1.len(), 1);
    assert_eq!(rows(&call(&ctx, &mut sess1, "corpRegistry", "GetMemberTrackingInfoSimple", vec![PyRep::Integer(cid)]).await.expect("GetMemberTrackingInfoSimple")).1.len(), 1);

    let info = call(&ctx, &mut sess1, "corpRegistry", "GetInfoWindowDataForChar", vec![PyRep::Integer(cid)]).await.expect("GetInfoWindowDataForChar");
    assert_eq!(field(&info, "corporationID"), PyRep::Integer(cid));

    let upd_mem = call(&ctx, &mut sess1, "corpRegistry", "UpdateMembers", vec![PyRep::Integer(90_000_001), PyRep::Integer(0)]).await.expect("UpdateMembers");
    assert_eq!(field(&upd_mem, "characterID"), PyRep::Integer(90_000_001));
    ctx.db.with_mut_snapshot(|s| {
        if let Some(c) = s.corporations.get_mut(&cid) { c.members.insert(90_000_002); c.member_roles.insert(90_000_002, 0); }
        if let Some(ch) = s.characters.get_mut(&90_000_002) { ch.corporation_id = cid; ch.station_id = Some(60_003_760); ch.structure_id = None; }
    });
    assert_eq!(call(&ctx, &mut sess1, "corpRegistry", "CanBeKickedOut", vec![PyRep::Integer(90_000_002)]).await.expect("CanBeKickedOut"), PyRep::Bool(true));
    assert!(is_rowset(&call(&ctx, &mut sess1, "corpRegistry", "GetPendingAutoKicks", vec![]).await.expect("GetPendingAutoKicks")));
}

#[tokio::test]
async fn test_corp_registry_operations_kills_and_naming() {
    // parity: rpc:corpRegistry.GetMyApplications, rpc:corpRegistry.GetOldApplications, rpc:corpRegistry.GetMyOldApplications, rpc:corpRegistry.DeleteAllianceApplication, rpc:corpRegistry.GetAggressionSettings, rpc:corpRegistry.UpdateCorporationAbilities, rpc:corpRegistry.GetSuggestedAllianceShortNames, rpc:corpRegistry.GetSuggestedTickerNames, rpc:corpRegistry.GetEveOwners, rpc:corpRegistry.GetRecentKills, rpc:corpRegistry.GetRecentLosses, rpc:corpRegistry.SetAccountKey, rpc:corpRegistry.KickOutMembers
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess1 = SessionState::new(1101);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    call(&ctx, &mut sess1, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(90_000_001)]).await.unwrap();
    let mut sess2 = SessionState::new(1102);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    call(&ctx, &mut sess2, "charUnboundMgr", "SelectCharacterID", vec![PyRep::Integer(90_000_002)]).await.unwrap();
    ctx.advance_secs(12);

    call(&ctx, &mut sess1, "corpRegistry", "AddCorporation", vec![PyRep::String("Omega Tactical Corps".into()), PyRep::String("OMTAC".into()), PyRep::Real(0.05)]).await.expect("AddCorporation");
    let cid = sess1.corporation_id.expect("corpID");
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(77001, eve_db::KillmailRecord {
            kill_id: 77001, victim_character_id: 90_000_002, attacker_character_id: 90_000_001,
            solar_system_id: 30000142, ship_type_id: 587, total_value_isk: 500_000.0, killed_unix: 1700000000,
            victim_corp_id: 98_000_002, victim_alliance_id: None, attacker_corp_id: cid, attacker_alliance_id: None,
        });
    });

    assert!(is_rowset(&call(&ctx, &mut sess2, "corpRegistry", "GetMyApplications", vec![]).await.unwrap()));
    assert!(is_rowset(&call(&ctx, &mut sess1, "corpRegistry", "GetOldApplications", vec![]).await.unwrap()));
    assert!(is_rowset(&call(&ctx, &mut sess2, "corpRegistry", "GetMyOldApplications", vec![]).await.unwrap()));
    assert_eq!(field(&call(&ctx, &mut sess1, "corpRegistry", "DeleteAllianceApplication", vec![PyRep::Integer(99_000_001)]).await.unwrap(), "deleted"), PyRep::Bool(true));
    assert_eq!(field(&call(&ctx, &mut sess1, "corpRegistry", "GetAggressionSettings", vec![PyRep::Integer(cid)]).await.unwrap(), "corporationID"), PyRep::Integer(cid));
    assert!(field(&call(&ctx, &mut sess1, "corpRegistry", "UpdateCorporationAbilities", vec![]).await.unwrap(), "corporationID").as_i64().unwrap() > 0);
    assert!(matches!(call(&ctx, &mut sess1, "corpRegistry", "GetSuggestedAllianceShortNames", vec![PyRep::String("Northern Coalition Force".into())]).await.unwrap(), PyRep::List(l) if !l.is_empty()));
    assert!(matches!(call(&ctx, &mut sess1, "corpRegistry", "GetSuggestedTickerNames", vec![PyRep::String("Northern Mining Operations".into())]).await.unwrap(), PyRep::List(l) if !l.is_empty()));
    assert!(is_rowset(&call(&ctx, &mut sess1, "corpRegistry", "GetEveOwners", vec![]).await.unwrap()));
    assert_eq!(rows(&call(&ctx, &mut sess1, "corpRegistry", "GetRecentKills", vec![]).await.unwrap()).1.len(), 1);
    assert_eq!(rows(&call(&ctx, &mut sess1, "corpRegistry", "GetRecentLosses", vec![]).await.unwrap()).1.len(), 0);
    ctx.advance_secs(12);
    assert_eq!(call(&ctx, &mut sess1, "corpRegistry", "SetAccountKey", vec![PyRep::Integer(1001)]).await.unwrap(), PyRep::None);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_002) { ch.corporation_id = cid; }
        if let Some(corp) = snap.corporations.get_mut(&cid) { corp.members.insert(90_000_002); }
    });
    ctx.advance_secs(12);
    let kick_res = call(&ctx, &mut sess1, "corpRegistry", "KickOutMembers", vec![PyRep::Integer(90_000_002)]).await.expect("KickOutMembers");
    assert_eq!(field(&kick_res, "characterID"), PyRep::Integer(90_000_002));
}

}
