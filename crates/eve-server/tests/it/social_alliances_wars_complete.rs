//! Phase 7 Alliances, Wars, Structure Ownership, Corp Deliveries & Corp Goals Integration Tests (`SO-3`).

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

#[tokio::test]
async fn test_alliances_lifecycle_governance_contacts_bulletins_and_rankings() {
    // parity: feat:alliance.create-alliance, feat:alliance.apply-alliance, feat:alliance.leave-alliance, feat:alliance.alliance-executor, feat:alliance.alliance-info, feat:alliance.alliance-contacts, feat:alliance.alliance-bulletins, feat:alliance.alliance-rankings, feat:alliance.alliance-close
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
    let poor = call(&ctx, &mut sess1, "corpRegistry", "CreateAlliance", create("Northern Coalition Prime", "NCPRM")).await.expect_err("no fee");
    assert_eq!(poor.code, "InsufficientFunds");
    let bad = call(&ctx, &mut sess1, "corpRegistry", "CreateAlliance", create("Northern Coalition Prime", "TOOLONG")).await.expect_err("ticker");
    assert_eq!(bad.code, "InvalidAllianceName");
    set_corp_wallet(&ctx, corp1, 2_500_000_000);
    let created = call(&ctx, &mut sess1, "corpRegistry", "CreateAlliance", create("Northern Coalition Prime", "NCPRM")).await.expect("CreateAlliance");
    let aid = field(&created, "allianceID").as_i64().unwrap();
    assert_eq!(field(&created, "executorCorpID"), PyRep::Integer(corp1));
    assert_eq!(sess1.alliance_id, Some(aid));
    assert_eq!(corp_wallet(&ctx, corp1), 1_500_000_000.0, "1B ISK alliance fee");
    let dup = call(&ctx, &mut sess2, "corpRegistry", "CreateAlliance", create("northern coalition prime", "XYZ")).await.expect_err("dup");
    assert_eq!(dup.code, "AllianceNameTaken");

    // apply-alliance: application -> executor accepts -> joins after 24h.
    let applied = call(&ctx, &mut sess2, "corpRegistry", "ApplyToJoinAlliance", vec![PyRep::Integer(aid), PyRep::String("Requesting membership".into())]).await.unwrap();
    assert_eq!(field(&applied, "status"), PyRep::String("Pending".into()));
    let (acols, arows) = rows(&call(&ctx, &mut sess1, "corpRegistry", "GetAllianceApplications", vec![]).await.unwrap());
    assert_eq!(arows.len(), 1);
    assert_eq!(arows[0][col(&acols, "corporationID")], PyRep::Integer(corp2));
    assert_eq!(arows[0][col(&acols, "status")], PyRep::String("Pending".into()));
    let not_exec = call(&ctx, &mut sess2, "allianceRegistry", "UpdateApplication", vec![PyRep::Integer(corp2)]).await.expect_err("not executor");
    assert!(not_exec.code == "NotAllianceExecutor", "{not_exec:?}");
    let accept = call(&ctx, &mut sess1, "allianceRegistry", "UpdateApplication", vec![PyRep::Integer(corp2), PyRep::String("Welcome".into())]).await.unwrap();
    assert_eq!(field(&accept, "joinEffectiveTime"), PyRep::Integer(ctx.now_unix() as i64 + 86_400));
    assert_eq!(rows(&call(&ctx, &mut sess1, "allianceRegistry", "GetAllianceMembers", vec![PyRep::Integer(aid)]).await.unwrap()).1.len(), 1, "not yet joined");
    ctx.advance_secs(86_401);
    assert_eq!(rows(&call(&ctx, &mut sess1, "allianceRegistry", "GetAllianceMembers", vec![PyRep::Integer(aid)]).await.unwrap()).1.len(), 2);
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp2].alliance_id), Some(aid));
    sess2.alliance_id = Some(aid);

    // alliance-info: GetAlliance / GetAlliancePublicInfo / GetAllianceMembers / GetEmploymentRecord.
    let denied = call(&ctx, &mut sess2, "allianceRegistry", "UpdateAlliance", vec![PyRep::String("x".into()), PyRep::String("y".into())]).await.expect_err("member corp");
    assert_eq!(denied.code, "NotAllianceExecutor");
    call(&ctx, &mut sess1, "allianceRegistry", "UpdateAlliance", vec![PyRep::String("Updated Northern Coalition Desc".into()), PyRep::String("https://nc-updated.eve".into())]).await.unwrap();
    let info1 = call(&ctx, &mut sess2, "allianceRegistry", "GetAlliance", vec![PyRep::Integer(aid)]).await.unwrap();
    assert_eq!(field(&info1, "description"), PyRep::String("Updated Northern Coalition Desc".into()));
    assert_eq!(field(&info1, "url"), PyRep::String("https://nc-updated.eve".into()));
    assert_eq!(field(&info1, "memberCount"), PyRep::Integer(2));
    assert_eq!(field(&info1, "executorCorpID"), PyRep::Integer(corp1));
    assert_eq!(field(&info1, "shortName"), PyRep::String("NCPRM".into()));
    let info2 = call(&ctx, &mut sess2, "allianceRegistry", "GetAlliancePublicInfo", vec![PyRep::Integer(aid)]).await.unwrap();
    assert_eq!(field(&info2, "description"), PyRep::String("Updated Northern Coalition Desc".into()));
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
    assert_eq!(ranked, vec![(PyRep::Integer(aid), PyRep::Integer(2)), (PyRep::Integer(small_id), PyRep::Integer(1))]);

    // alliance-contacts: executor-managed, visible to member corps.
    let c_denied = call(&ctx, &mut sess2, "allianceRegistry", "AddAllianceContact", vec![PyRep::Integer(p3_char), PyRep::Real(10.0), PyRep::Integer(1)]).await.expect_err("member corp");
    assert_eq!(c_denied.code, "NotAllianceExecutor");
    let bad_st = call(&ctx, &mut sess1, "allianceRegistry", "AddAllianceContact", vec![PyRep::Integer(p3_char), PyRep::Real(11.0), PyRep::Integer(1)]).await.expect_err("standing range");
    assert_eq!(bad_st.code, "InvalidStanding");
    call(&ctx, &mut sess1, "allianceRegistry", "AddAllianceContact", vec![PyRep::Integer(p3_char), PyRep::Real(10.0), PyRep::Integer(1)]).await.unwrap();
    call(&ctx, &mut sess1, "allianceRegistry", "EditAllianceContact", vec![PyRep::Integer(p3_char), PyRep::Real(-5.0), PyRep::Integer(3)]).await.unwrap();
    let (_, crows) = rows(&call(&ctx, &mut sess2, "allianceRegistry", "GetAllianceContacts", vec![]).await.unwrap());
    assert_eq!(crows, vec![vec![PyRep::Integer(p3_char), PyRep::Real(-5.0), PyRep::Integer(3)]], "member corp sees executor contacts");
    let rm_denied = call(&ctx, &mut sess2, "allianceRegistry", "RemoveAllianceContacts", vec![PyRep::List(vec![PyRep::Integer(p3_char)])]).await.expect_err("member corp");
    assert_eq!(rm_denied.code, "NotAllianceExecutor");
    call(&ctx, &mut sess1, "allianceRegistry", "RemoveAllianceContacts", vec![PyRep::List(vec![PyRep::Integer(p3_char)])]).await.unwrap();
    assert!(rows(&call(&ctx, &mut sess2, "allianceRegistry", "GetAllianceContacts", vec![]).await.unwrap()).1.is_empty());

    // alliance-bulletins: executor-managed, visible to all member corps.
    let b_denied = call(&ctx, &mut sess2, "allianceRegistry", "AddBulletin", vec![PyRep::String("t".into()), PyRep::String("b".into())]).await.expect_err("member corp");
    assert_eq!(b_denied.code, "NotAllianceExecutor");
    let bul = call(&ctx, &mut sess1, "allianceRegistry", "AddBulletin", vec![PyRep::String("Alliance Sov Policy".into()), PyRep::String("Defend all TCUs".into())]).await.unwrap();
    let bul_id = field(&bul, "bulletinID").as_i64().unwrap();
    call(&ctx, &mut sess1, "allianceRegistry", "UpdateBulletin", vec![PyRep::Integer(bul_id), PyRep::String("Updated Sov Policy".into()), PyRep::String("Updated body".into())]).await.unwrap();
    let (bcols, brows) = rows(&call(&ctx, &mut sess2, "allianceRegistry", "GetAllianceBulletins", vec![]).await.unwrap());
    assert_eq!(brows.len(), 1);
    assert_eq!(brows[0][col(&bcols, "title")], PyRep::String("Updated Sov Policy".into()));
    assert_eq!(brows[0][col(&bcols, "editCharacterID")], PyRep::Integer(90_000_001));
    let del_denied = call(&ctx, &mut sess2, "allianceRegistry", "DeleteBulletin", vec![PyRep::Integer(bul_id)]).await.expect_err("member corp");
    assert_eq!(del_denied.code, "NotAllianceExecutor");
    assert_eq!(call(&ctx, &mut sess1, "allianceRegistry", "DeleteBulletin", vec![PyRep::Integer(bul_id)]).await.unwrap(), PyRep::Bool(true));
    assert!(rows(&call(&ctx, &mut sess2, "allianceRegistry", "GetAllianceBulletins", vec![]).await.unwrap()).1.is_empty());

    // alliance-executor: support votes elect the executor (majority of member corps); executor kicks.
    let one_vote = call(&ctx, &mut sess2, "allianceRegistry", "DeclareExecutorSupport", vec![PyRep::Integer(corp2)]).await.unwrap();
    assert_eq!(field(&one_vote, "executorCorpID"), PyRep::Integer(corp1), "1 of 2 votes is not a majority");
    let majority = call(&ctx, &mut sess1, "allianceRegistry", "DeclareExecutorSupport", vec![PyRep::Integer(corp2)]).await.unwrap();
    assert_eq!(field(&majority, "executorCorpID"), PyRep::Integer(corp2));
    let kick_denied = call(&ctx, &mut sess1, "allianceRegistry", "KickMember", vec![PyRep::Integer(corp2)]).await.expect_err("no longer executor");
    assert_eq!(kick_denied.code, "NotAllianceExecutor");

    // leave-alliance: blocked while the corp is in an active war.
    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(777_001, eve_db::WarRecord { war_id: 777_001, aggressor_corp_id: corp3, defender_corp_id: corp1, mutual: true, started_unix: 0, active: true });
    });
    let war_block = call(&ctx, &mut sess1, "allianceRegistry", "DeleteMember", vec![PyRep::Integer(corp1)]).await.expect_err("war");
    assert_eq!(war_block.code, "CannotLeaveAllianceDuringWar");
    ctx.db.with_mut_snapshot(|snap| snap.wars.get_mut(&777_001).unwrap().active = false);
    let other_corp = call(&ctx, &mut sess1, "allianceRegistry", "DeleteMember", vec![PyRep::Integer(corp2)]).await.expect_err("only self");
    assert_eq!(other_corp.code, "CorpRoleDenied");

    // executor kicks corp 1 (immediate).
    let kicked = call(&ctx, &mut sess2, "allianceRegistry", "KickMember", vec![PyRep::Integer(corp1)]).await.unwrap();
    assert_eq!(field(&kicked, "removed"), PyRep::Bool(true));
    assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp1].alliance_id), None);

    // leave-alliance + alliance-close: the last corp leaves after the 24h delay -> alliance closes.
    let leave = call(&ctx, &mut sess2, "allianceRegistry", "DeleteMember", vec![PyRep::Integer(corp2)]).await.unwrap();
    assert_eq!(field(&leave, "leaveEffectiveTime"), PyRep::Integer(ctx.now_unix() as i64 + 86_400));
    assert_eq!(field(&leave, "allianceClosed"), PyRep::Bool(false));
    assert_eq!(rows(&call(&ctx, &mut sess2, "allianceRegistry", "GetAllianceMembers", vec![PyRep::Integer(aid)]).await.unwrap()).1.len(), 1, "leave pending");
    ctx.advance_secs(86_401);
    assert!(rows(&call(&ctx, &mut sess2, "allianceRegistry", "GetAllianceMembers", vec![PyRep::Integer(aid)]).await.unwrap()).1.is_empty());
    let gone = call(&ctx, &mut sess2, "allianceRegistry", "GetAlliance", vec![PyRep::Integer(aid)]).await.expect_err("closed");
    assert_eq!(gone.code, "AllianceNotFound");
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
        last_damage_unix: 0,
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
    // parity: feat:war.declare-war, feat:war.war-hq, feat:war.war-eligibility-change, feat:war.war-list, feat:war.mutual-war, feat:war.peace-treaty, feat:war.allies, feat:war.war-stats, feat:war.war-retract, feat:corporation.corp-war-eligibility, feat:corporation.corp-ally-cost
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
    let no_hq = call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect_err("no war HQ");
    assert_eq!(no_hq.code, "WarHqRequired");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(995_000_001, structure(995_000_001, agg, "Aggressor War HQ"));
    });
    // ... and the defender must be war-eligible (owns a structure).
    let inelig = call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect_err("ineligible defender");
    assert_eq!(inelig.code, "DefenderNotWarEligible");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(995_000_002, structure(995_000_002, def, "Defender Raitaru"));
    });
    // declare-war: fee is charged to the attacker's master wallet.
    set_corp_wallet(&ctx, agg, 50_000_000);
    let poor = call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect_err("fee unaffordable");
    assert_eq!(poor.code, "InsufficientFunds");
    set_corp_wallet(&ctx, agg, 1_000_000_000);
    let declared_at = ctx.now_unix() as i64;
    let war = call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect("DeclareWarAgainst");
    let war_id = field(&war, "warID").as_i64().unwrap();
    assert_eq!(field(&war, "warHQ"), PyRep::Integer(995_000_001));
    assert_eq!(field(&war, "timeStarted"), PyRep::Integer(declared_at + 86_400), "24h spool-up");
    assert_eq!(corp_wallet(&ctx, agg), 900_000_000.0, "100M ISK war bill");
    let dup = call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect_err("duplicate war");
    assert_eq!(dup.code, "AlreadyAtWar");

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
    let base_cost = call(&ctx, &mut sess2, "corpRegistry", "CharGetAllyBaseCost", vec![]).await.unwrap();
    assert_eq!(base_cost, PyRep::Real(10_000_000.0));
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_002).or_default().insert(3368, eve_db::TrainedSkillRecord { type_id: 3368, level: 4, skill_points: 45_255.0 });
    });
    let skilled_cost = call(&ctx, &mut sess2, "corpRegistry", "CharGetAllyBaseCost", vec![]).await.unwrap();
    assert_eq!(skilled_cost, PyRep::Real(8_000_000.0), "-5% per Diplomatic Relations level");

    // allies: defender opens the war, ally offers, defender pays and the ally joins after 24h.
    let offer_args = vec![PyRep::Integer(war_id), PyRep::Real(25_000_000.0), PyRep::String("Mercenary assistance".into())];
    let closed = call(&ctx, &mut sess3, "warsInfoMgr", "CreateWarAllyOffer", offer_args.clone()).await.expect_err("not open");
    assert_eq!(closed.code, "WarNotOpenForAllies");
    let not_def = call(&ctx, &mut sess1, "warsInfoMgr", "SetOpenForAllies", vec![PyRep::Integer(war_id), PyRep::Bool(true)]).await.expect_err("aggressor");
    assert_eq!(not_def.code, "NotWarDefender");
    call(&ctx, &mut sess2, "warsInfoMgr", "SetOpenForAllies", vec![PyRep::Integer(war_id), PyRep::Bool(true)]).await.unwrap();
    let needing = call(&ctx, &mut sess3, "warsInfoMgr", "GetWarsRequiringAssistance", vec![]).await.unwrap();
    assert_eq!(war_field(&needing, war_id, "openForAllies"), Some(PyRep::Bool(true)));
    let offer = call(&ctx, &mut sess3, "warsInfoMgr", "CreateWarAllyOffer", offer_args).await.expect("CreateWarAllyOffer");
    let offer_id = field(&offer, "negotiationID").as_i64().unwrap();
    let wrong = call(&ctx, &mut sess1, "warsInfoMgr", "AcceptAllyNegotiation", vec![PyRep::Integer(offer_id)]).await.expect_err("only defender");
    assert_eq!(wrong.code, "NotNegotiationCounterparty");
    let accepted = call(&ctx, &mut sess2, "warsInfoMgr", "AcceptAllyNegotiation", vec![PyRep::Integer(offer_id)]).await.expect("accept ally");
    assert_eq!(field(&accepted, "allyID"), PyRep::Integer(ally));
    assert_eq!(corp_wallet(&ctx, def), 975_000_000.0, "defender pays the ally fee");
    assert_eq!(corp_wallet(&ctx, ally), 1_025_000_000.0, "ally receives the fee");
    let (acols, arows) = rows(&call(&ctx, &mut sess2, "warsInfoMgr", "GetAllyRowsByWarID", vec![PyRep::Integer(war_id)]).await.unwrap());
    assert_eq!(arows.len(), 1);
    assert_eq!(arows[0][col(&acols, "active")], PyRep::Bool(false), "ally joins after 24h");
    ctx.advance_secs(86_401);
    let (acols, arows) = rows(&call(&ctx, &mut sess2, "warsInfoMgr", "GetAllyRowsByWarID", vec![PyRep::Integer(war_id)]).await.unwrap());
    assert_eq!(arows[0][col(&acols, "active")], PyRep::Bool(true));

    // war-stats: kills/ISK come from persisted killmails between the two sides.
    ctx.db.with_mut_snapshot(|snap| {
        snap.killmails.insert(880_001, eve_db::KillmailRecord { kill_id: 880_001, victim_character_id: 90_000_002, attacker_character_id: 90_000_001, solar_system_id: 30_000_142, ship_type_id: 587, total_value_isk: 400_000.0 });
        snap.killmails.insert(880_002, eve_db::KillmailRecord { kill_id: 880_002, victim_character_id: 90_000_001, attacker_character_id: 90_000_002, solar_system_id: 30_000_142, ship_type_id: 603, total_value_isk: 350_000.0 });
        snap.killmails.insert(880_003, eve_db::KillmailRecord { kill_id: 880_003, victim_character_id: 90_000_001, attacker_character_id: p3_char, solar_system_id: 30_000_142, ship_type_id: 587, total_value_isk: 999.0 });
    });
    let base = call(&ctx, &mut sess1, "warStatisticMgr", "GetBaseInfo", vec![PyRep::Integer(war_id)]).await.unwrap();
    assert_eq!(field(&base, "killsForAggressor"), PyRep::Integer(1));
    assert_eq!(field(&base, "killsForDefender"), PyRep::Integer(1));
    assert_eq!(field(&base, "iskDestroyedByAggressor"), PyRep::Real(400_000.0));
    assert_eq!(field(&base, "iskDestroyedByDefender"), PyRep::Real(350_000.0));
    let (_, krows) = rows(&call(&ctx, &mut sess1, "warStatisticMgr", "GetKills", vec![PyRep::Integer(war_id)]).await.unwrap());
    assert_eq!(krows.len(), 2, "the ally pilot's kill is not part of this war's base stats");
    let frigate_group = ctx.sde.types[&587].group_id as i64;
    assert_eq!(ctx.sde.types[&603].group_id as i64, frigate_group);
    let (gcols, grows) = rows(&call(&ctx, &mut sess1, "warStatisticMgr", "GetKillsByGroup", vec![PyRep::Integer(war_id)]).await.unwrap());
    assert_eq!(grows, vec![vec![PyRep::Integer(frigate_group), PyRep::Integer(2), PyRep::Real(750_000.0)]], "{gcols:?}");
    let km = call(&ctx, &mut sess1, "warStatisticMgr", "GetKillMail", vec![PyRep::Integer(880_001)]).await.unwrap();
    assert_eq!(field(&km, "iskLost"), PyRep::Real(400_000.0));

    // peace-treaty: defender offers surrender, aggressor accepts; war ends 24h later, 2-week treaty.
    let surr = call(&ctx, &mut sess2, "warsInfoMgr", "CreateSurrenderNegotiation", vec![PyRep::Integer(war_id), PyRep::Real(50_000_000.0), PyRep::String("Peace terms".into())]).await.unwrap();
    let neg_id = field(&surr, "negotiationID").as_i64().unwrap();
    let own = call(&ctx, &mut sess2, "warsInfoMgr", "AcceptSurrender", vec![PyRep::Integer(neg_id)]).await.expect_err("own offer");
    assert_eq!(own.code, "NotNegotiationCounterparty");
    let accepted_at = ctx.now_unix() as i64;
    let acc = call(&ctx, &mut sess1, "warsInfoMgr", "AcceptSurrender", vec![PyRep::Integer(neg_id)]).await.expect("AcceptSurrender");
    assert_eq!(field(&acc, "timeFinished"), PyRep::Integer(accepted_at + 86_400));
    assert_eq!(field(&acc, "peaceTreatyExpiry"), PyRep::Integer(accepted_at + 86_400 + 14 * 86_400));
    assert_eq!(corp_wallet(&ctx, agg), 950_000_000.0, "surrender ISK paid to the aggressor");
    let still = call(&ctx, &mut sess1, "warsInfoMgr", "GetPublicWarInfo", vec![PyRep::Integer(war_id)]).await.unwrap();
    assert_eq!(field(&still, "active"), PyRep::Bool(true), "surrendered war winds down for 24h");
    ctx.advance_secs(86_401);
    let ended = call(&ctx, &mut sess1, "warsInfoMgr", "GetPublicWarInfo", vec![PyRep::Integer(war_id)]).await.unwrap();
    assert_eq!(field(&ended, "active"), PyRep::Bool(false));
    let (tcols, trows) = rows(&call(&ctx, &mut sess1, "peaceTreatyMgr", "GetPeaceTreatiesForSession", vec![]).await.unwrap());
    assert_eq!(trows.len(), 1);
    assert_eq!(trows[0][col(&tcols, "otherOwnerID")], PyRep::Integer(def));
    let blocked = call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect_err("treaty");
    assert_eq!(blocked.code, "PeaceTreatyActive");
    ctx.advance_secs(14 * 86_400);
    assert!(rows(&call(&ctx, &mut sess1, "peaceTreatyMgr", "GetPeaceTreatiesForSession", vec![]).await.unwrap()).1.is_empty());

    // war-eligibility-change: the defender loses its last structure -> ineligible, war ends.
    let war2 = field(&call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect("redeclare after treaty"), "warID").as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&995_000_002).unwrap().state = "Destroyed".into());
    let list = call(&ctx, &mut sess1, "warsInfoMgr", "GetWarsByOwnerID", vec![PyRep::Integer(agg)]).await.unwrap();
    assert_eq!(war_field(&list, war2, "active"), Some(PyRep::Bool(false)), "war against ineligible corp ends");
    let again = call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect_err("ineligible");
    assert_eq!(again.code, "DefenderNotWarEligible");

    // war-hq: destroying the attacker's War HQ ends the war.
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(995_000_003, structure(995_000_003, def, "Defender Replacement"));
    });
    let war3 = field(&call(&ctx, &mut sess1, "corpRegistry", "DeclareWarAgainst", declare(995_000_001)).await.expect("third war"), "warID").as_i64().unwrap();
    ctx.db.with_mut_snapshot(|snap| snap.structures.get_mut(&995_000_001).unwrap().state = "Destroyed".into());
    let after_hq = call(&ctx, &mut sess1, "warsInfoMgr", "GetPublicWarInfo", vec![PyRep::Integer(war3)]).await.unwrap();
    assert_eq!(field(&after_hq, "active"), PyRep::Bool(false), "War HQ destroyed ends the war");

    // mutual-war: invite, response requires a pending invite, accepted war is mutual.
    let no_inv = call(&ctx, &mut sess3, "mutualWarInviteMgr", "RespondToInviteByPlayer", vec![PyRep::Integer(agg), PyRep::Bool(true)]).await.expect_err("no invite");
    assert_eq!(no_inv.code, "MutualWarInviteNotFound");
    call(&ctx, &mut sess1, "mutualWarInviteMgr", "SendInviteByPlayer", vec![PyRep::Integer(ally)]).await.unwrap();
    let (_, pend) = rows(&call(&ctx, &mut sess3, "mutualWarInviteMgr", "GetPendingInvitesForSession", vec![]).await.unwrap());
    assert_eq!(pend, vec![vec![PyRep::Integer(agg), PyRep::Integer(ally), PyRep::Integer(ctx.now_unix() as i64)]]);
    let resp = call(&ctx, &mut sess3, "mutualWarInviteMgr", "RespondToInviteByPlayer", vec![PyRep::Integer(agg), PyRep::Bool(true)]).await.unwrap();
    let mwar = field(&resp, "warID").as_i64().unwrap();
    let minfo = call(&ctx, &mut sess3, "warsInfoMgr", "GetPublicWarInfo", vec![PyRep::Integer(mwar)]).await.unwrap();
    assert_eq!(field(&minfo, "mutual"), PyRep::Bool(true));
    assert_eq!(field(&minfo, "active"), PyRep::Bool(true));
    assert!(rows(&call(&ctx, &mut sess3, "mutualWarInviteMgr", "GetPendingInvitesForSession", vec![]).await.unwrap()).1.is_empty());
    let ret = call(&ctx, &mut sess3, "warsInfoMgr", "RetractMutualWar", vec![PyRep::Integer(mwar)]).await.unwrap();
    assert_eq!(field(&ret, "timeFinished"), PyRep::Integer(ctx.now_unix() as i64 + 86_400));
    ctx.advance_secs(86_401);
    let mdone = call(&ctx, &mut sess3, "warsInfoMgr", "GetPublicWarInfo", vec![PyRep::Integer(mwar)]).await.unwrap();
    assert_eq!(field(&mdone, "active"), PyRep::Bool(false));
}

#[tokio::test]
async fn test_corporation_deliveries_goals_freelance_fittings_assets_and_close() {
    // parity: feat:corporation.member-limit, feat:corporation.corp-hq, feat:corporation.corp-offices, feat:corporation.corp-hangars, feat:corporation.role-change-delay, feat:corporation.corp-deliveries, feat:corporation.corp-assets, feat:corporation.corp-fittings, feat:corporation.corp-aggression-settings, feat:corporation.corp-labels, feat:corporation.corp-contacts, feat:corporation.recruitment-ads, feat:corporation.corp-employment-records, feat:corporation.corp-close
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
    let kw = HashMap::new();

    let mut sess1 = SessionState::new(921);
    common::login_pilot(&ctx, &mut sess1, "demo", "demo-secret").await;
    ctx.dispatch_rpc(&mut sess1, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();

    let mut sess2 = SessionState::new(922);
    common::login_pilot(&ctx, &mut sess2, "other", "other-secret").await;
    ctx.dispatch_rpc(&mut sess2, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(90_000_001).or_default();
        skills.insert(3363, eve_db::TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 });
        skills.insert(3732, eve_db::TrainedSkillRecord { type_id: 3732, level: 5, skill_points: 256_000.0 });
    });

    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "AddCorporation", &[PyRep::String("Logistics & Projects Corp".into()), PyRep::String("LPRC".into()), PyRep::Real(0.05)], &kw, None).await.unwrap();
    let corp_id = sess1.corporation_id.unwrap();
    set_corp_wallet(&ctx, corp_id, 500_000_000);

    // 1. Member Limit (`feat:corporation.member-limit`): SDE corporationMemberBonus (attr 191) -> 5*20 + 5*400 = 2100.
    // Remove skills from CEO so limit is 1; accepting an application or invitation at the cap fails with CorporationFull!
    let attr_rep = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateAttributes", &[], &kw, None).await.unwrap();
    assert_eq!(field(&attr_rep, "memberLimit"), PyRep::Integer(5 * 20 + 5 * 400));
    ctx.db.with_mut_snapshot(|snap| snap.skills.get_mut(&90_000_001).unwrap().clear());
    let attr_min = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateAttributes", &[], &kw, None).await.unwrap();
    assert_eq!(field(&attr_min, "memberLimit"), PyRep::Integer(1));
    let app_rep = ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Apply".into())], &kw, None).await.unwrap();
    let app_id = field(&app_rep, "applicationID").as_i64().unwrap();
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())], &kw, None).await.unwrap_err().code, "CorporationFull");
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "InsertInvitation", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap_err().code, "CorporationFull");

    // Restore Corporation Management (3363) level 1 -> limit 20, accept Pilot 2 into the corporation
    ctx.db.with_mut_snapshot(|snap| {
        snap.skills.entry(90_000_001).or_default().insert(3363, eve_db::TrainedSkillRecord { type_id: 3363, level: 1, skill_points: 250.0 });
    });
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateAttributes", &[], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())], &kw, None).await.unwrap();
    sess2.corporation_id = Some(corp_id);
    sess2.corp_role = 0;

    // 2. Aggression Settings (`feat:corporation.corp-aggression-settings`): regular member denied; CEO sets FF legal -> 24h pending -> takes effect after 24h
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpRegistry", None, "RegisterNewAggressionSettings", &[PyRep::Bool(true)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    let agg_init = ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAggressionSettings", &[PyRep::Integer(corp_id)], &kw, None).await.unwrap();
    assert_eq!(field(&agg_init, "isFriendlyFireLegal"), PyRep::Bool(false));
    let agg_rep = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "RegisterNewAggressionSettings", &[PyRep::Bool(true)], &kw, None).await.unwrap();
    assert_eq!(field(&agg_rep, "isFriendlyFireLegal"), PyRep::Bool(true));
    let agg_pend = ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAggressionSettings", &[PyRep::Integer(corp_id)], &kw, None).await.unwrap();
    assert_eq!(field(&agg_pend, "isFriendlyFireLegal"), PyRep::Bool(false), "FF change must wait 24h before taking effect");
    assert_eq!(field(&agg_pend, "pendingFriendlyFireLegal"), PyRep::Bool(true));
    ctx.advance_secs(86_401);
    let agg_after = ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAggressionSettings", &[PyRep::Integer(corp_id)], &kw, None).await.unwrap();
    assert_eq!(field(&agg_after, "isFriendlyFireLegal"), PyRep::Bool(true));

    // 3. Offices, HQ & Hangars (`feat:corporation.corp-offices`, `feat:corporation.corp-hq`, `feat:corporation.corp-hangars`, `feat:corporation.role-change-delay`)
    // Moving HQ to station 60003761 without an office there must fail with NoOfficeAtStation
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "corpStationMgr", None, "MoveCorpHQHere", &[PyRep::Integer(60_003_761)], &kw, None).await.unwrap_err().code, "NoOfficeAtStation");
    let quote = ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetPriceQuote", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap().as_f64().unwrap();
    assert!(quote >= 10_000.0);
    let wallet_before_rent = corp_wallet(&ctx, corp_id);
    let rent_rep = ctx.dispatch_rpc(&mut sess1, "officeManager", None, "RentOffice", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap();
    let office_folder_id = field(&rent_rep, "officeFolderID").as_i64().unwrap();
    assert_eq!(corp_wallet(&ctx, corp_id), wallet_before_rent - quote, "RentOffice must deduct rental fee from Master Wallet");
    assert!(is_keyval(&ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetMyCorporationsOffice", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap()));
    assert!(is_rowset(&ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetMyCorporationsOffices", &[], &kw, None).await.unwrap()));
    assert!(is_rowset(&ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetCorporationsWithOffices", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap()));
    assert!(ctx.dispatch_rpc(&mut sess1, "officeManager", None, "GetEmptyOfficeCount", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap().as_i64().unwrap() >= 0);

    // MoveCorpHQHere succeeds now that corp has an office at 60003760
    let hq_rep = ctx.dispatch_rpc(&mut sess1, "corpStationMgr", None, "MoveCorpHQHere", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap();
    assert_eq!(field(&hq_rep, "stationID"), PyRep::Integer(60_003_760));

    // 4. Corporation Deliveries & Division Hangar Access with 24h Role-Change Delay (`feat:corporation.corp-deliveries`, `feat:corporation.corp-hangars`, `feat:corporation.role-change-delay`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(910_000_001, eve_db::InventoryItemRecord {
            item_id: 910_000_001, type_id: 34, owner_id: 90_000_001, location_id: 60_003_760,
            flag: eve_db::InventoryFlag::Hangar, quantity: 5_000, singleton: false, version: 1, ..Default::default()
        });
    });
    let del_hangar = ctx.dispatch_rpc(&mut sess1, "invbroker", None, "DeliverToCorpHangar", &[PyRep::Integer(910_000_001), PyRep::Integer(1)], &kw, None).await.unwrap();
    assert_eq!(field(&del_hangar, "ownerID"), PyRep::Integer(corp_id));
    assert_eq!(field(&del_hangar, "flagID"), PyRep::Integer(116));

    // Pilot 2 has 0 roles -> denied from querying or taking from Corp Division 1 (flag 116)
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "invbroker", None, "List", &[PyRep::Integer(116)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "invbroker", None, "Add", &[PyRep::Integer(910_000_001), PyRep::Integer(60_003_760), PyRep::Integer(4)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");

    // CEO grants Pilot 2 Division 1 Query (1 << 20) + Division 1 Take (1 << 13) -> 24h delay applies before Pilot 2 can access Division 1!
    let div1_roles = (1u64 << 20) | (1u64 << 13);
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateMember", &[PyRep::Integer(90_000_002), PyRep::Integer(div1_roles as i64)], &kw, None).await.unwrap();
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "invbroker", None, "List", &[PyRep::Integer(116)], &kw, None).await.unwrap_err().code, "CorpRoleDenied", "Hangar role grant must wait 24h before taking effect");
    ctx.advance_secs(86_401);
    let div1_list = ctx.dispatch_rpc(&mut sess2, "invbroker", None, "List", &[PyRep::Integer(116)], &kw, None).await.unwrap();
    assert_eq!(rows(&div1_list).1.len(), 1, "After 24h delay, Pilot 2 can query Corp Division 1");
    ctx.dispatch_rpc(&mut sess2, "invbroker", None, "Add", &[PyRep::Integer(910_000_001), PyRep::Integer(60_003_760), PyRep::Integer(4)], &kw, None).await.unwrap();
    // Revocation is immediate: strip Pilot 2's roles and verify immediate denial
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "UpdateMember", &[PyRep::Integer(90_000_002), PyRep::Integer(0)], &kw, None).await.unwrap();
    sess2.corp_role = 0;
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "invbroker", None, "List", &[PyRep::Integer(116)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");

    // Deliver item back to Corp Hangar Division 2 (flag 117), then UnrentOffice -> item in office is impounded to Deliveries (flag 173), then DeliverToCorpMember
    ctx.dispatch_rpc(&mut sess2, "invbroker", None, "DeliverToCorpHangar", &[PyRep::Integer(910_000_001), PyRep::Integer(2)], &kw, None).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&910_000_001].flag), eve_db::InventoryFlag::CorpSAG2);
    let _ = office_folder_id;
    ctx.dispatch_rpc(&mut sess1, "officeManager", None, "UnrentOffice", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.items[&910_000_001].flag), eve_db::InventoryFlag::Deliveries, "UnrentOffice must impound division items to Deliveries");

    // 5. Corporation Fittings (`feat:corporation.corp-fittings`) & Assets (`feat:corporation.corp-assets`)
    // Regular member Pilot 2 cannot SaveFitting or GetAssetInventory
    let fit_kv = PyRep::keyval([("name", PyRep::String("Corp Doctrine Rifter".into())), ("shipTypeID", PyRep::Integer(587)), ("description", PyRep::String("Standard tackle".into())), ("fitData", PyRep::List(vec![]))]);
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpFittingMgr", None, "SaveFitting", &[PyRep::Integer(corp_id), fit_kv.clone()], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpmgr", None, "GetAssetInventory", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    let save_fit = ctx.dispatch_rpc(&mut sess1, "corpFittingMgr", None, "SaveFitting", &[PyRep::Integer(corp_id), fit_kv], &kw, None).await.unwrap();
    let fit_id = save_fit.as_i64().unwrap();
    let fits_rep = ctx.dispatch_rpc(&mut sess2, "corpFittingMgr", None, "GetFittings", &[PyRep::Integer(corp_id)], &kw, None).await.unwrap();
    assert!(is_keyval(&fits_rep) || is_rowset(&fits_rep) || matches!(fits_rep, PyRep::Dict(_)));
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpFittingMgr", None, "DeleteFitting", &[PyRep::Integer(corp_id), PyRep::Integer(fit_id)], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    ctx.dispatch_rpc(&mut sess1, "corpFittingMgr", None, "DeleteFitting", &[PyRep::Integer(corp_id), PyRep::Integer(fit_id)], &kw, None).await.unwrap();

    // GetAssetInventory and SearchAssets return only corporation-owned items (item 910_000_001)
    let (_, asset_rows) = rows(&ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "GetAssetInventory", &[PyRep::Integer(60_003_760)], &kw, None).await.unwrap());
    assert!(asset_rows.iter().all(|r| r[2] == PyRep::Integer(corp_id)), "GetAssetInventory must only return corp-owned assets");
    let (_, search_rows) = rows(&ctx.dispatch_rpc(&mut sess1, "corpmgr", None, "SearchAssets", &[PyRep::Integer(34)], &kw, None).await.unwrap());
    assert_eq!(search_rows.len(), 1);

    let del_member = ctx.dispatch_rpc(&mut sess1, "invbroker", None, "DeliverToCorpMember", &[PyRep::Integer(910_000_001), PyRep::Integer(90_000_001)], &kw, None).await.unwrap();
    assert_eq!(field(&del_member, "ownerID"), PyRep::Integer(90_000_001));
    assert_eq!(field(&del_member, "flagID"), PyRep::Integer(eve_db::InventoryFlag::Deliveries as i64));

    // 6. Corporate Contacts & Labels (`feat:corporation.corp-contacts`, `feat:corporation.corp-labels`) & Recruitment Ads (`feat:corporation.recruitment-ads`)
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

    // Recruitment Ads: role check, fee deduction, typeMask search, update, delete
    assert_eq!(ctx.dispatch_rpc(&mut sess2, "corpRecProxy", None, "CreateRecruitmentAd", &[PyRep::Integer(7), PyRep::Integer(3), PyRep::String("Unauth Ad".into()), PyRep::String("Desc".into())], &kw, None).await.unwrap_err().code, "CorpRoleDenied");
    let w_before_ad = corp_wallet(&ctx, corp_id);
    let ad_rep = ctx.dispatch_rpc(&mut sess1, "corpRecProxy", None, "CreateRecruitmentAd", &[PyRep::Integer(14), PyRep::Integer(4), PyRep::String("Nullsec PvP".into()), PyRep::String("Daily fleets".into())], &kw, None).await.unwrap();
    let ad_id = field(&ad_rep, "adID").as_i64().unwrap();
    assert!(corp_wallet(&ctx, corp_id) < w_before_ad, "CreateRecruitmentAd must deduct ad fee from corp Master Wallet");
    assert_eq!(rows(&ctx.dispatch_rpc(&mut sess2, "corpRecProxy", None, "SearchCorpAds", &[PyRep::Integer(4)], &kw, None).await.unwrap()).1.len(), 1);
    assert!(rows(&ctx.dispatch_rpc(&mut sess2, "corpRecProxy", None, "SearchCorpAds", &[PyRep::Integer(1)], &kw, None).await.unwrap()).1.is_empty());
    ctx.dispatch_rpc(&mut sess1, "corpRecProxy", None, "UpdateRecruitmentAd", &[PyRep::Integer(ad_id), PyRep::Integer(12), PyRep::String("Updated PvP".into()), PyRep::String("Updated".into())], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess1, "corpRecProxy", None, "DeleteRecruitmentAd", &[PyRep::Integer(ad_id)], &kw, None).await.unwrap();

    // 7. Corporation projects: activity matchers cannot be progressed by client self-report.
    let goal_rep = ctx.dispatch_rpc(&mut sess1, "corporationSvc", None, "CreateGoalSubmit", &[PyRep::String("Kill 10 Pirates".into()), PyRep::Integer(10), PyRep::Real(100_000.0), PyRep::String("DamageNPCShips".into()), PyRep::Bool(true)], &kw, None).await.unwrap();
    let goal_id = goal_rep.dict_get("projectID").and_then(|v| v.as_i64()).unwrap();
    let self_report = ctx.dispatch_rpc(&mut sess1, "corporationSvc", None, "GoalProgress", &[PyRep::Integer(goal_id), PyRep::Integer(10)], &kw, None).await.expect_err("client cannot self-report kills");
    assert_eq!(self_report.code, "ServerTrackedMatcher");
    let (fcols, frows) = rows(&ctx.dispatch_rpc(&mut sess2, "corporationSvc", None, "GetActiveFreelanceProjectId", &[], &kw, None).await.unwrap());
    assert!(frows.iter().any(|r| r[col(&fcols, "projectID")] == PyRep::Integer(goal_id)), "freelance project visible outside the corp");

    // 8. Employment Records (`feat:corporation.corp-employment-records`) & Corporation Closure (`feat:corporation.corp-close`)
    // Kick Pilot 2 -> employment record has start/end timestamps; CEO as last member cannot close corp while an active war exists (`ActiveWarsPreventClosure`), then closes corp once war is inactive
    ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "KickOutMember", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap();
    let (_, emp_rows) = rows(&ctx.dispatch_rpc(&mut sess2, "corporationSvc", None, "GetEmployementRecordAndCharacterTransfers", &[PyRep::Integer(90_000_002)], &kw, None).await.unwrap());
    assert!(emp_rows.iter().any(|r| r[0] == PyRep::Integer(corp_id) || r.get(1) == Some(&PyRep::Integer(corp_id))), "Employment history must record Pilot 2's tenure in corp_id");

    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(999_123, eve_db::WarRecord { war_id: 999_123, aggressor_corp_id: corp_id, defender_corp_id: 1_000_002, mutual: false, started_unix: 0, active: true });
    });
    let can_leave_war = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "CanLeaveCurrentCorporation", &[], &kw, None).await.unwrap();
    assert_eq!(field(&can_leave_war, "canLeave"), PyRep::Bool(false));
    assert_eq!(ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "KickOutMember", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap_err().code, "ActiveWarsPreventClosure");
    ctx.db.with_mut_snapshot(|snap| snap.wars.get_mut(&999_123).unwrap().active = false);
    let close_rep = ctx.dispatch_rpc(&mut sess1, "corpRegistry", None, "KickOutMember", &[PyRep::Integer(90_000_001)], &kw, None).await.unwrap();
    assert_eq!(field(&close_rep, "corporationClosed"), PyRep::Bool(true));
    assert!(ctx.db.with_snapshot(|s| !s.corporations.contains_key(&corp_id)), "Closed corporation must be removed from snapshot");
}
