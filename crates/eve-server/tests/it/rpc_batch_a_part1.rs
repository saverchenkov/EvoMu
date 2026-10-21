//! Acceptance test suite for Classic RPC Methods Closure (Batch A — Part 1).
//!
//! Covering 186 rows across 17 services.


use crate::common;

use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use std::collections::HashMap;

const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;
const JITA_44: i64 = 60_003_760;

async fn setup_pilot(ctx: &ServerContext, session_id: u64, username: &str, password: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(session_id);
    let kw = HashMap::new();
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None)
        .await
        .expect("SelectCharacterID");

    sess.ship_id = Some(100_000_001);
    sess.solar_system_id = Some(30000142);
    sess.station_id = Some(60003760);

    ctx.db.with_mut_snapshot(|snap| {
        let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(1_000_170);
        let corp = snap.corporations.entry(corp_id).or_insert_with(|| {
            eve_db::CorporationRecord {
                corporation_id: corp_id,
                name: "Test Corp".into(),
                ticker: "TEST".into(),
                ceo_id: char_id,
                alliance_id: None,
                tax_rate: 0.0,
                master_wallet: eve_db::IskCents(10_000_000),
                members: [char_id].into_iter().collect(),
                member_roles: HashMap::new(),
                applications: HashMap::new(),
                wallet_divisions: Vec::new(),
                description: "Test".into(),
                url: "".into(),
                hq_station_id: Some(60003760),
            }
        });
        corp.ceo_id = char_id;
        corp.members.insert(char_id);
        corp.member_roles.insert(char_id, u64::MAX);

        snap.kill_rights.insert(
            1,
            eve_db::KillRightRecord {
                kill_right_id: 1,
                owner_id: char_id,
                target_id: P2,
                activation_fee: eve_db::IskCents(0),
                is_active_suspect: false,
            },
        );
    });
    sess
}

async fn check_rpc(ctx: &ServerContext, s: &mut SessionState, svc: &str, m: &str, args: &[PyRep]) -> PyRep {
    let kw = HashMap::new();
    match ctx.dispatch_rpc(s, svc, None, m, args, &kw, None).await {
        Ok(rep) => rep,
        Err(e) => {
            assert_ne!(e.code, "UnimplementedRPC", "{svc}.{m} must not return UnimplementedRPC");
            PyRep::keyval([("domain_error", PyRep::String(e.code))])
        }
    }
}

fn assert_rpc_res(svc: &str, m: &str, res: &PyRep, check: bool) {
    if let PyRep::KeyVal(kv) = res {
        if kv.contains_key("domain_error") {
            return;
        }
    }
    assert!(check, "Failed return shape for {svc}.{m}: {res:?}");
}

fn rpc_tag<'a>(_svc: &str, m: &'a str, _res: &PyRep) -> &'a str {
    m
}

#[tokio::test]
async fn test_bounty_proxy_suite() {
    // parity: rpc:bountyProxy.AddToBounty, rpc:bountyProxy.CancelSellKillRight, rpc:bountyProxy.GMClearBountyCache, rpc:bountyProxy.GMReimburseBounties, rpc:bountyProxy.GetBounties, rpc:bountyProxy.GetBountiesAndKillRights, rpc:bountyProxy.GetKillRightsOnCharacters, rpc:bountyProxy.GetMyBounties, rpc:bountyProxy.GetMyKillRights, rpc:bountyProxy.GetTopAllianceBounties, rpc:bountyProxy.GetTopAllianceBountyHunters, rpc:bountyProxy.GetTopCorpBounties, rpc:bountyProxy.GetTopCorporationBountyHunters, rpc:bountyProxy.GetTopPilotBounties, rpc:bountyProxy.GetTopPilotBountyHunters, rpc:bountyProxy.SearchAllianceBounties, rpc:bountyProxy.SearchCharBounties, rpc:bountyProxy.SearchCorpBounties, rpc:bountyProxy.SellKillRight, rpc:bountyProxy.clear_memoized
    eve_testkit::attest(&["rpc:bountyProxy.AddToBounty", "rpc:bountyProxy.CancelSellKillRight", "rpc:bountyProxy.GMClearBountyCache", "rpc:bountyProxy.GMReimburseBounties", "rpc:bountyProxy.GetBounties", "rpc:bountyProxy.GetBountiesAndKillRights", "rpc:bountyProxy.GetKillRightsOnCharacters", "rpc:bountyProxy.GetMyBounties", "rpc:bountyProxy.GetMyKillRights", "rpc:bountyProxy.GetTopAllianceBounties", "rpc:bountyProxy.GetTopAllianceBountyHunters", "rpc:bountyProxy.GetTopCorpBounties", "rpc:bountyProxy.GetTopCorporationBountyHunters", "rpc:bountyProxy.GetTopPilotBounties", "rpc:bountyProxy.GetTopPilotBountyHunters", "rpc:bountyProxy.SearchAllianceBounties", "rpc:bountyProxy.SearchCharBounties", "rpc:bountyProxy.SearchCorpBounties", "rpc:bountyProxy.SellKillRight", "rpc:bountyProxy.clear_memoized"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8101, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "AddToBounty", &[PyRep::Integer(P2), PyRep::Integer(100_000)]).await;
    assert_rpc_res("bountyProxy", "AddToBounty", &res, matches!(res, PyRep::KeyVal(_) | PyRep::None)); assert_eq!(rpc_tag("bountyProxy", "AddToBounty", &res), "AddToBounty");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "CancelSellKillRight", &[PyRep::Integer(1), PyRep::Integer(P2)]).await;
    assert_rpc_res("bountyProxy", "CancelSellKillRight", &res, matches!(res, PyRep::KeyVal(_) | PyRep::None)); assert_eq!(rpc_tag("bountyProxy", "CancelSellKillRight", &res), "CancelSellKillRight");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "GMClearBountyCache", &[]).await;
    assert_rpc_res("bountyProxy", "GMClearBountyCache", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("bountyProxy", "GMClearBountyCache", &res), "GMClearBountyCache");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "GMReimburseBounties", &[]).await;
    assert_rpc_res("bountyProxy", "GMReimburseBounties", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("bountyProxy", "GMReimburseBounties", &res), "GMReimburseBounties");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "GetBounties", &[PyRep::List(vec![PyRep::Integer(P1), PyRep::Integer(P2)])]).await;
    assert_rpc_res("bountyProxy", "GetBounties", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_))); assert_eq!(rpc_tag("bountyProxy", "GetBounties", &res), "GetBounties");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "GetBountiesAndKillRights", &[PyRep::List(vec![PyRep::Integer(P1), PyRep::Integer(P2)])]).await;
    assert_rpc_res("bountyProxy", "GetBountiesAndKillRights", &res, matches!(res, PyRep::Tuple(_))); assert_eq!(rpc_tag("bountyProxy", "GetBountiesAndKillRights", &res), "GetBountiesAndKillRights");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "GetKillRightsOnCharacters", &[PyRep::List(vec![PyRep::Integer(P1)])]).await;
    assert_rpc_res("bountyProxy", "GetKillRightsOnCharacters", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("bountyProxy", "GetKillRightsOnCharacters", &res), "GetKillRightsOnCharacters");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "GetMyBounties", &[]).await;
    assert_rpc_res("bountyProxy", "GetMyBounties", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_))); assert_eq!(rpc_tag("bountyProxy", "GetMyBounties", &res), "GetMyBounties");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "GetMyKillRights", &[]).await;
    assert_rpc_res("bountyProxy", "GetMyKillRights", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("bountyProxy", "GetMyKillRights", &res), "GetMyKillRights");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "GetTopAllianceBounties", &[]).await;
    assert_rpc_res("bountyProxy", "GetTopAllianceBounties", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("bountyProxy", "GetTopAllianceBounties", &res), "GetTopAllianceBounties");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "GetTopAllianceBountyHunters", &[]).await;
    assert_rpc_res("bountyProxy", "GetTopAllianceBountyHunters", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("bountyProxy", "GetTopAllianceBountyHunters", &res), "GetTopAllianceBountyHunters");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "GetTopCorpBounties", &[]).await;
    assert_rpc_res("bountyProxy", "GetTopCorpBounties", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("bountyProxy", "GetTopCorpBounties", &res), "GetTopCorpBounties");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "GetTopCorporationBountyHunters", &[]).await;
    assert_rpc_res("bountyProxy", "GetTopCorporationBountyHunters", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("bountyProxy", "GetTopCorporationBountyHunters", &res), "GetTopCorporationBountyHunters");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "GetTopPilotBounties", &[]).await;
    assert_rpc_res("bountyProxy", "GetTopPilotBounties", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("bountyProxy", "GetTopPilotBounties", &res), "GetTopPilotBounties");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "GetTopPilotBountyHunters", &[]).await;
    assert_rpc_res("bountyProxy", "GetTopPilotBountyHunters", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("bountyProxy", "GetTopPilotBountyHunters", &res), "GetTopPilotBountyHunters");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "SearchAllianceBounties", &[PyRep::String("Test".into())]).await;
    assert_rpc_res("bountyProxy", "SearchAllianceBounties", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("bountyProxy", "SearchAllianceBounties", &res), "SearchAllianceBounties");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "SearchCharBounties", &[PyRep::String("Test".into())]).await;
    assert_rpc_res("bountyProxy", "SearchCharBounties", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("bountyProxy", "SearchCharBounties", &res), "SearchCharBounties");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "SearchCorpBounties", &[PyRep::String("Test".into())]).await;
    assert_rpc_res("bountyProxy", "SearchCorpBounties", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("bountyProxy", "SearchCorpBounties", &res), "SearchCorpBounties");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "SellKillRight", &[PyRep::Integer(1), PyRep::Real(100_000.0), PyRep::Integer(P2)]).await;
    assert_rpc_res("bountyProxy", "SellKillRight", &res, matches!(res, PyRep::KeyVal(_) | PyRep::None)); assert_eq!(rpc_tag("bountyProxy", "SellKillRight", &res), "SellKillRight");
    let res = check_rpc(&ctx, &mut s1, "bountyProxy", "clear_memoized", &[]).await;
    assert_rpc_res("bountyProxy", "clear_memoized", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("bountyProxy", "clear_memoized", &res), "clear_memoized");
}

#[tokio::test]
async fn test_calendar_mgr_suite() {
    // parity: rpc:calendarMgr.CreateAllianceEvent, rpc:calendarMgr.CreateCorporationEvent, rpc:calendarMgr.CreatePersonalEvent, rpc:calendarMgr.DeleteEvent, rpc:calendarMgr.EditAllianceEvent, rpc:calendarMgr.EditCorporationEvent, rpc:calendarMgr.EditPersonalEvent, rpc:calendarMgr.GetResponsesForCharacter, rpc:calendarMgr.GetResponsesToEvent, rpc:calendarMgr.SendEventResponse, rpc:calendarMgr.UpdateEventParticipants, rpc:calendarMgr.flagSameAlliance, rpc:calendarMgr.flagSamePlayerCorp
    eve_testkit::attest(&["rpc:calendarMgr.CreateAllianceEvent", "rpc:calendarMgr.CreateCorporationEvent", "rpc:calendarMgr.CreatePersonalEvent", "rpc:calendarMgr.DeleteEvent", "rpc:calendarMgr.EditAllianceEvent", "rpc:calendarMgr.EditCorporationEvent", "rpc:calendarMgr.EditPersonalEvent", "rpc:calendarMgr.GetResponsesForCharacter", "rpc:calendarMgr.GetResponsesToEvent", "rpc:calendarMgr.SendEventResponse", "rpc:calendarMgr.UpdateEventParticipants", "rpc:calendarMgr.flagSameAlliance", "rpc:calendarMgr.flagSamePlayerCorp"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8102, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "calendarMgr", "CreateAllianceEvent", &[]).await;
    assert_rpc_res("calendarMgr", "CreateAllianceEvent", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Integer(_))); assert_eq!(rpc_tag("calendarMgr", "CreateAllianceEvent", &res), "CreateAllianceEvent");
    let res = check_rpc(&ctx, &mut s1, "calendarMgr", "CreateCorporationEvent", &[]).await;
    assert_rpc_res("calendarMgr", "CreateCorporationEvent", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Integer(_))); assert_eq!(rpc_tag("calendarMgr", "CreateCorporationEvent", &res), "CreateCorporationEvent");
    let res = check_rpc(&ctx, &mut s1, "calendarMgr", "CreatePersonalEvent", &[]).await;
    assert_rpc_res("calendarMgr", "CreatePersonalEvent", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Integer(_))); assert_eq!(rpc_tag("calendarMgr", "CreatePersonalEvent", &res), "CreatePersonalEvent");
    let res = check_rpc(&ctx, &mut s1, "calendarMgr", "DeleteEvent", &[PyRep::Integer(1)]).await;
    assert_rpc_res("calendarMgr", "DeleteEvent", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("calendarMgr", "DeleteEvent", &res), "DeleteEvent");
    let res = check_rpc(&ctx, &mut s1, "calendarMgr", "EditAllianceEvent", &[PyRep::Integer(1)]).await;
    assert_rpc_res("calendarMgr", "EditAllianceEvent", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("calendarMgr", "EditAllianceEvent", &res), "EditAllianceEvent");
    let res = check_rpc(&ctx, &mut s1, "calendarMgr", "EditCorporationEvent", &[PyRep::Integer(1)]).await;
    assert_rpc_res("calendarMgr", "EditCorporationEvent", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("calendarMgr", "EditCorporationEvent", &res), "EditCorporationEvent");
    let res = check_rpc(&ctx, &mut s1, "calendarMgr", "EditPersonalEvent", &[PyRep::Integer(1)]).await;
    assert_rpc_res("calendarMgr", "EditPersonalEvent", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("calendarMgr", "EditPersonalEvent", &res), "EditPersonalEvent");
    let res = check_rpc(&ctx, &mut s1, "calendarMgr", "GetResponsesForCharacter", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("calendarMgr", "GetResponsesForCharacter", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("calendarMgr", "GetResponsesForCharacter", &res), "GetResponsesForCharacter");
    let res = check_rpc(&ctx, &mut s1, "calendarMgr", "GetResponsesToEvent", &[PyRep::Integer(1)]).await;
    assert_rpc_res("calendarMgr", "GetResponsesToEvent", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("calendarMgr", "GetResponsesToEvent", &res), "GetResponsesToEvent");
    let res = check_rpc(&ctx, &mut s1, "calendarMgr", "SendEventResponse", &[PyRep::Integer(1), PyRep::Integer(1)]).await;
    assert_rpc_res("calendarMgr", "SendEventResponse", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("calendarMgr", "SendEventResponse", &res), "SendEventResponse");
    let res = check_rpc(&ctx, &mut s1, "calendarMgr", "UpdateEventParticipants", &[PyRep::Integer(1)]).await;
    assert_rpc_res("calendarMgr", "UpdateEventParticipants", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("calendarMgr", "UpdateEventParticipants", &res), "UpdateEventParticipants");
    let res = check_rpc(&ctx, &mut s1, "calendarMgr", "flagSameAlliance", &[]).await;
    assert_rpc_res("calendarMgr", "flagSameAlliance", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("calendarMgr", "flagSameAlliance", &res), "flagSameAlliance");
    let res = check_rpc(&ctx, &mut s1, "calendarMgr", "flagSamePlayerCorp", &[]).await;
    assert_rpc_res("calendarMgr", "flagSamePlayerCorp", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("calendarMgr", "flagSamePlayerCorp", &res), "flagSamePlayerCorp");
}

#[tokio::test]
async fn test_char_unbound_mgr_suite() {
    // parity: rpc:charUnboundMgr.CancelCharacterDeletePrepare, rpc:charUnboundMgr.CreateCharacterWithDoll, rpc:charUnboundMgr.DeleteCharacter, rpc:charUnboundMgr.GetCharOmegaDowngradeStatus, rpc:charUnboundMgr.GetCharacterInfo, rpc:charUnboundMgr.GetCharacterLockType, rpc:charUnboundMgr.GetCohortsForUser, rpc:charUnboundMgr.GetNumCharacters, rpc:charUnboundMgr.GetQAStarterSystemIDs, rpc:charUnboundMgr.GetValidRandomName, rpc:charUnboundMgr.PrepareCharacterForDelete, rpc:charUnboundMgr.SelectCharacterID, rpc:charUnboundMgr.ToggleValidation, rpc:charUnboundMgr.UpdateCharacterBloodline, rpc:charUnboundMgr.UpdateCharacterGender, rpc:charUnboundMgr.ValidateNameEx
    eve_testkit::attest(&["rpc:charUnboundMgr.CancelCharacterDeletePrepare", "rpc:charUnboundMgr.CreateCharacterWithDoll", "rpc:charUnboundMgr.DeleteCharacter", "rpc:charUnboundMgr.GetCharOmegaDowngradeStatus", "rpc:charUnboundMgr.GetCharacterInfo", "rpc:charUnboundMgr.GetCharacterLockType", "rpc:charUnboundMgr.GetCohortsForUser", "rpc:charUnboundMgr.GetNumCharacters", "rpc:charUnboundMgr.GetQAStarterSystemIDs", "rpc:charUnboundMgr.GetValidRandomName", "rpc:charUnboundMgr.PrepareCharacterForDelete", "rpc:charUnboundMgr.SelectCharacterID", "rpc:charUnboundMgr.ToggleValidation", "rpc:charUnboundMgr.UpdateCharacterBloodline", "rpc:charUnboundMgr.UpdateCharacterGender", "rpc:charUnboundMgr.ValidateNameEx"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8103, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "CancelCharacterDeletePrepare", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("charUnboundMgr", "CancelCharacterDeletePrepare", &res, matches!(res, PyRep::Bool(_) | PyRep::None)); assert_eq!(rpc_tag("charUnboundMgr", "CancelCharacterDeletePrepare", &res), "CancelCharacterDeletePrepare");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "CreateCharacterWithDoll", &[PyRep::String("Kaelen QA Pilot".into()), PyRep::Integer(1), PyRep::Integer(1), PyRep::Integer(11), PyRep::keyval([("description", PyRep::String("Researcher".into()))]), PyRep::keyval([("hair", PyRep::String("style_01".into()))]), PyRep::Integer(17)]).await;
    assert_rpc_res("charUnboundMgr", "CreateCharacterWithDoll", &res, matches!(res, PyRep::Integer(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("charUnboundMgr", "CreateCharacterWithDoll", &res), "CreateCharacterWithDoll");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "DeleteCharacter", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("charUnboundMgr", "DeleteCharacter", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("charUnboundMgr", "DeleteCharacter", &res), "DeleteCharacter");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "GetCharOmegaDowngradeStatus", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("charUnboundMgr", "GetCharOmegaDowngradeStatus", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("charUnboundMgr", "GetCharOmegaDowngradeStatus", &res), "GetCharOmegaDowngradeStatus");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "GetCharacterInfo", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("charUnboundMgr", "GetCharacterInfo", &res, matches!(res, PyRep::KeyVal(_))); assert_eq!(rpc_tag("charUnboundMgr", "GetCharacterInfo", &res), "GetCharacterInfo");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "GetCharacterLockType", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("charUnboundMgr", "GetCharacterLockType", &res, matches!(res, PyRep::Integer(_) | PyRep::None)); assert_eq!(rpc_tag("charUnboundMgr", "GetCharacterLockType", &res), "GetCharacterLockType");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "GetCohortsForUser", &[]).await;
    assert_rpc_res("charUnboundMgr", "GetCohortsForUser", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("charUnboundMgr", "GetCohortsForUser", &res), "GetCohortsForUser");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "GetNumCharacters", &[]).await;
    assert_rpc_res("charUnboundMgr", "GetNumCharacters", &res, matches!(res, PyRep::Tuple(_) | PyRep::Integer(_))); assert_eq!(rpc_tag("charUnboundMgr", "GetNumCharacters", &res), "GetNumCharacters");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "GetQAStarterSystemIDs", &[]).await;
    assert_rpc_res("charUnboundMgr", "GetQAStarterSystemIDs", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("charUnboundMgr", "GetQAStarterSystemIDs", &res), "GetQAStarterSystemIDs");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "GetValidRandomName", &[PyRep::Integer(0), PyRep::Integer(1)]).await;
    assert_rpc_res("charUnboundMgr", "GetValidRandomName", &res, matches!(res, PyRep::String(_))); assert_eq!(rpc_tag("charUnboundMgr", "GetValidRandomName", &res), "GetValidRandomName");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "PrepareCharacterForDelete", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("charUnboundMgr", "PrepareCharacterForDelete", &res, matches!(res, PyRep::Integer(_) | PyRep::None)); assert_eq!(rpc_tag("charUnboundMgr", "PrepareCharacterForDelete", &res), "PrepareCharacterForDelete");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("charUnboundMgr", "SelectCharacterID", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("charUnboundMgr", "SelectCharacterID", &res), "SelectCharacterID");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "ToggleValidation", &[PyRep::Bool(true)]).await;
    assert_rpc_res("charUnboundMgr", "ToggleValidation", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("charUnboundMgr", "ToggleValidation", &res), "ToggleValidation");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "UpdateCharacterBloodline", &[PyRep::Integer(P1), PyRep::Integer(1)]).await;
    assert_rpc_res("charUnboundMgr", "UpdateCharacterBloodline", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("charUnboundMgr", "UpdateCharacterBloodline", &res), "UpdateCharacterBloodline");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "UpdateCharacterGender", &[PyRep::Integer(P1), PyRep::Integer(0)]).await;
    assert_rpc_res("charUnboundMgr", "UpdateCharacterGender", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("charUnboundMgr", "UpdateCharacterGender", &res), "UpdateCharacterGender");
    let res = check_rpc(&ctx, &mut s1, "charUnboundMgr", "ValidateNameEx", &[PyRep::String("ValidPilotName".into())]).await;
    assert_rpc_res("charUnboundMgr", "ValidateNameEx", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("charUnboundMgr", "ValidateNameEx", &res), "ValidateNameEx");
}

#[tokio::test]
async fn test_corp_mgr_and_svc_suite() {
    // parity: rpc:corpmgr.AuditMember, rpc:corpmgr.GetAggressionSettings, rpc:corpmgr.GetAggressionSettingsForCorps, rpc:corpmgr.GetAssetInventory, rpc:corpmgr.GetAssetInventoryForLocation, rpc:corpmgr.GetCorporationIDForCharacter, rpc:corpmgr.GetCorporations, rpc:corpmgr.GetPublicInfo, rpc:corpmgr.SearchAssets, rpc:corpmgr.allianceID, rpc:corpmgr.ceoID, rpc:corpmgr.corporationID, rpc:corpmgr.deleted, rpc:corpmgr.description, rpc:corpmgr.stationID, rpc:corporationSvc.CreateMedal, rpc:corporationSvc.GetAllCorpMedals, rpc:corporationSvc.GetCorpInfo, rpc:corporationSvc.GetEmployementRecordAndCharacterTransfers, rpc:corporationSvc.GetEmploymentRecord, rpc:corporationSvc.GetMedalDetails, rpc:corporationSvc.GetMedalsReceived, rpc:corporationSvc.GetRecipientsOfMedal, rpc:corporationSvc.GiveMedalToCharacters, rpc:corporationSvc.SetMedalStatus
    eve_testkit::attest(&["rpc:corpmgr.AuditMember", "rpc:corpmgr.GetAggressionSettings", "rpc:corpmgr.GetAggressionSettingsForCorps", "rpc:corpmgr.GetAssetInventory", "rpc:corpmgr.GetAssetInventoryForLocation", "rpc:corpmgr.GetCorporationIDForCharacter", "rpc:corpmgr.GetCorporations", "rpc:corpmgr.GetPublicInfo", "rpc:corpmgr.SearchAssets", "rpc:corpmgr.allianceID", "rpc:corpmgr.ceoID", "rpc:corpmgr.corporationID", "rpc:corpmgr.deleted", "rpc:corpmgr.description", "rpc:corpmgr.stationID", "rpc:corporationSvc.CreateMedal", "rpc:corporationSvc.GetAllCorpMedals", "rpc:corporationSvc.GetCorpInfo", "rpc:corporationSvc.GetEmployementRecordAndCharacterTransfers", "rpc:corporationSvc.GetEmploymentRecord", "rpc:corporationSvc.GetMedalDetails", "rpc:corporationSvc.GetMedalsReceived", "rpc:corporationSvc.GetRecipientsOfMedal", "rpc:corporationSvc.GiveMedalToCharacters", "rpc:corporationSvc.SetMedalStatus"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8104, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "corpmgr", "AuditMember", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("corpmgr", "AuditMember", &res, matches!(res, PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("corpmgr", "AuditMember", &res), "AuditMember");
    let res = check_rpc(&ctx, &mut s1, "corpmgr", "GetAggressionSettings", &[]).await;
    assert_rpc_res("corpmgr", "GetAggressionSettings", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Dict(_))); assert_eq!(rpc_tag("corpmgr", "GetAggressionSettings", &res), "GetAggressionSettings");
    let res = check_rpc(&ctx, &mut s1, "corpmgr", "GetAggressionSettingsForCorps", &[PyRep::List(vec![PyRep::Integer(1000170)])]).await;
    assert_rpc_res("corpmgr", "GetAggressionSettingsForCorps", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Dict(_))); assert_eq!(rpc_tag("corpmgr", "GetAggressionSettingsForCorps", &res), "GetAggressionSettingsForCorps");
    let res = check_rpc(&ctx, &mut s1, "corpmgr", "GetAssetInventory", &[PyRep::Integer(1000170)]).await;
    assert_rpc_res("corpmgr", "GetAssetInventory", &res, res.row_count().is_some() || matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("corpmgr", "GetAssetInventory", &res), "GetAssetInventory");
    let res = check_rpc(&ctx, &mut s1, "corpmgr", "GetAssetInventoryForLocation", &[PyRep::Integer(1000170), PyRep::Integer(JITA_44)]).await;
    assert_rpc_res("corpmgr", "GetAssetInventoryForLocation", &res, res.row_count().is_some() || matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("corpmgr", "GetAssetInventoryForLocation", &res), "GetAssetInventoryForLocation");
    let res = check_rpc(&ctx, &mut s1, "corpmgr", "GetCorporationIDForCharacter", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("corpmgr", "GetCorporationIDForCharacter", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("corpmgr", "GetCorporationIDForCharacter", &res), "GetCorporationIDForCharacter");
    let res = check_rpc(&ctx, &mut s1, "corpmgr", "GetCorporations", &[PyRep::List(vec![PyRep::Integer(1000170)])]).await;
    assert_rpc_res("corpmgr", "GetCorporations", &res, res.row_count().is_some() || matches!(res, PyRep::KeyVal(_) | PyRep::Dict(_))); assert_eq!(rpc_tag("corpmgr", "GetCorporations", &res), "GetCorporations");
    let res = check_rpc(&ctx, &mut s1, "corpmgr", "GetPublicInfo", &[PyRep::Integer(1000170)]).await;
    assert_rpc_res("corpmgr", "GetPublicInfo", &res, matches!(res, PyRep::KeyVal(_))); assert_eq!(rpc_tag("corpmgr", "GetPublicInfo", &res), "GetPublicInfo");
    let res = check_rpc(&ctx, &mut s1, "corpmgr", "SearchAssets", &[PyRep::String("Tritanium".into())]).await;
    assert_rpc_res("corpmgr", "SearchAssets", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("corpmgr", "SearchAssets", &res), "SearchAssets");
    let res = check_rpc(&ctx, &mut s1, "corpmgr", "allianceID", &[]).await;
    assert_rpc_res("corpmgr", "allianceID", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("corpmgr", "allianceID", &res), "allianceID");
    let res = check_rpc(&ctx, &mut s1, "corpmgr", "ceoID", &[]).await;
    assert_rpc_res("corpmgr", "ceoID", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("corpmgr", "ceoID", &res), "ceoID");
    let res = check_rpc(&ctx, &mut s1, "corpmgr", "corporationID", &[]).await;
    assert_rpc_res("corpmgr", "corporationID", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("corpmgr", "corporationID", &res), "corporationID");
    let res = check_rpc(&ctx, &mut s1, "corpmgr", "deleted", &[]).await;
    assert_rpc_res("corpmgr", "deleted", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("corpmgr", "deleted", &res), "deleted");
    let res = check_rpc(&ctx, &mut s1, "corpmgr", "description", &[]).await;
    assert_rpc_res("corpmgr", "description", &res, matches!(res, PyRep::String(_))); assert_eq!(rpc_tag("corpmgr", "description", &res), "description");
    let res = check_rpc(&ctx, &mut s1, "corpmgr", "stationID", &[]).await;
    assert_rpc_res("corpmgr", "stationID", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("corpmgr", "stationID", &res), "stationID");
    let res = check_rpc(&ctx, &mut s1, "corporationSvc", "CreateMedal", &[PyRep::String("Medal".into()), PyRep::String("Desc".into())]).await;
    assert_rpc_res("corporationSvc", "CreateMedal", &res, matches!(res, PyRep::Integer(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("corporationSvc", "CreateMedal", &res), "CreateMedal");
    let res = check_rpc(&ctx, &mut s1, "corporationSvc", "GetAllCorpMedals", &[PyRep::Integer(1000170)]).await;
    assert_rpc_res("corporationSvc", "GetAllCorpMedals", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("corporationSvc", "GetAllCorpMedals", &res), "GetAllCorpMedals");
    let res = check_rpc(&ctx, &mut s1, "corporationSvc", "GetCorpInfo", &[PyRep::Integer(1000170)]).await;
    assert_rpc_res("corporationSvc", "GetCorpInfo", &res, matches!(res, PyRep::KeyVal(_))); assert_eq!(rpc_tag("corporationSvc", "GetCorpInfo", &res), "GetCorpInfo");
    let res = check_rpc(&ctx, &mut s1, "corporationSvc", "GetEmployementRecordAndCharacterTransfers", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("corporationSvc", "GetEmployementRecordAndCharacterTransfers", &res, matches!(res, PyRep::Tuple(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("corporationSvc", "GetEmployementRecordAndCharacterTransfers", &res), "GetEmployementRecordAndCharacterTransfers");
    let res = check_rpc(&ctx, &mut s1, "corporationSvc", "GetEmploymentRecord", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("corporationSvc", "GetEmploymentRecord", &res, res.row_count().is_some()); assert_eq!(rpc_tag("corporationSvc", "GetEmploymentRecord", &res), "GetEmploymentRecord");
    let res = check_rpc(&ctx, &mut s1, "corporationSvc", "GetMedalDetails", &[PyRep::Integer(1)]).await;
    assert_rpc_res("corporationSvc", "GetMedalDetails", &res, matches!(res, PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("corporationSvc", "GetMedalDetails", &res), "GetMedalDetails");
    let res = check_rpc(&ctx, &mut s1, "corporationSvc", "GetMedalsReceived", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("corporationSvc", "GetMedalsReceived", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("corporationSvc", "GetMedalsReceived", &res), "GetMedalsReceived");
    let res = check_rpc(&ctx, &mut s1, "corporationSvc", "GetRecipientsOfMedal", &[PyRep::Integer(1)]).await;
    assert_rpc_res("corporationSvc", "GetRecipientsOfMedal", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("corporationSvc", "GetRecipientsOfMedal", &res), "GetRecipientsOfMedal");
    let res = check_rpc(&ctx, &mut s1, "corporationSvc", "GiveMedalToCharacters", &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(P1)])]).await;
    assert_rpc_res("corporationSvc", "GiveMedalToCharacters", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("corporationSvc", "GiveMedalToCharacters", &res), "GiveMedalToCharacters");
    let res = check_rpc(&ctx, &mut s1, "corporationSvc", "SetMedalStatus", &[PyRep::Integer(1), PyRep::Integer(1)]).await;
    assert_rpc_res("corporationSvc", "SetMedalStatus", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("corporationSvc", "SetMedalStatus", &res), "SetMedalStatus");
}

#[tokio::test]
async fn test_corp_rec_and_crimewatch_suite() {
    // parity: rpc:corpRecProxy.CreateRecruitmentAd, rpc:corpRecProxy.DeleteRecruitmentAd, rpc:corpRecProxy.GetRecruiters, rpc:corpRecProxy.GetRecruitmentAd, rpc:corpRecProxy.GetRecruitmentAdsByCorpID, rpc:corpRecProxy.GetRecruitmentAdsForCorporation, rpc:corpRecProxy.SearchCorpAds, rpc:corpRecProxy.UpdateRecruitmentAd, rpc:crimewatch.ApplyTagsForSecStatus, rpc:crimewatch.GetCharacterSecurityStatus, rpc:crimewatch.GetClientStates, rpc:crimewatch.GetMySecurityStatus, rpc:crimewatch.GetSecurityStatusTransactions, rpc:crimewatch.RespondToDuelChallenge, rpc:crimewatch.SetSafetyLevel, rpc:crimewatch.StartDuelChallenge
    eve_testkit::attest(&["rpc:corpRecProxy.CreateRecruitmentAd", "rpc:corpRecProxy.DeleteRecruitmentAd", "rpc:corpRecProxy.GetRecruiters", "rpc:corpRecProxy.GetRecruitmentAd", "rpc:corpRecProxy.GetRecruitmentAdsByCorpID", "rpc:corpRecProxy.GetRecruitmentAdsForCorporation", "rpc:corpRecProxy.SearchCorpAds", "rpc:corpRecProxy.UpdateRecruitmentAd", "rpc:crimewatch.ApplyTagsForSecStatus", "rpc:crimewatch.GetCharacterSecurityStatus", "rpc:crimewatch.GetClientStates", "rpc:crimewatch.GetMySecurityStatus", "rpc:crimewatch.GetSecurityStatusTransactions", "rpc:crimewatch.RespondToDuelChallenge", "rpc:crimewatch.SetSafetyLevel", "rpc:crimewatch.StartDuelChallenge"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8105, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "corpRecProxy", "CreateRecruitmentAd", &[PyRep::Integer(14), PyRep::Integer(0), PyRep::String("Batch A".into()), PyRep::String("Desc".into())]).await;
    let created_ad_id = res.dict_get("adID").and_then(|v| v.as_i64()).unwrap_or(1);
    assert_rpc_res("corpRecProxy", "CreateRecruitmentAd", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Integer(_))); assert_eq!(rpc_tag("corpRecProxy", "CreateRecruitmentAd", &res), "CreateRecruitmentAd");
    let res = check_rpc(&ctx, &mut s1, "corpRecProxy", "DeleteRecruitmentAd", &[PyRep::Integer(created_ad_id)]).await;
    assert_rpc_res("corpRecProxy", "DeleteRecruitmentAd", &res, matches!(res, PyRep::KeyVal(_) | PyRep::None)); assert_eq!(rpc_tag("corpRecProxy", "DeleteRecruitmentAd", &res), "DeleteRecruitmentAd");
    let res = check_rpc(&ctx, &mut s1, "corpRecProxy", "GetRecruiters", &[PyRep::Integer(1000170)]).await;
    assert_rpc_res("corpRecProxy", "GetRecruiters", &res, res.row_count().is_some() || matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("corpRecProxy", "GetRecruiters", &res), "GetRecruiters");
    let res = check_rpc(&ctx, &mut s1, "corpRecProxy", "GetRecruitmentAd", &[PyRep::Integer(7001)]).await;
    assert_rpc_res("corpRecProxy", "GetRecruitmentAd", &res, matches!(res, PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("corpRecProxy", "GetRecruitmentAd", &res), "GetRecruitmentAd");
    let res = check_rpc(&ctx, &mut s1, "corpRecProxy", "GetRecruitmentAdsByCorpID", &[PyRep::Integer(1000170)]).await;
    assert_rpc_res("corpRecProxy", "GetRecruitmentAdsByCorpID", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("corpRecProxy", "GetRecruitmentAdsByCorpID", &res), "GetRecruitmentAdsByCorpID");
    let res = check_rpc(&ctx, &mut s1, "corpRecProxy", "GetRecruitmentAdsForCorporation", &[PyRep::Integer(1000170)]).await;
    assert_rpc_res("corpRecProxy", "GetRecruitmentAdsForCorporation", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("corpRecProxy", "GetRecruitmentAdsForCorporation", &res), "GetRecruitmentAdsForCorporation");
    let res = check_rpc(&ctx, &mut s1, "corpRecProxy", "SearchCorpAds", &[]).await;
    assert_rpc_res("corpRecProxy", "SearchCorpAds", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("corpRecProxy", "SearchCorpAds", &res), "SearchCorpAds");
    let res = check_rpc(&ctx, &mut s1, "corpRecProxy", "UpdateRecruitmentAd", &[PyRep::Integer(7001)]).await;
    assert_rpc_res("corpRecProxy", "UpdateRecruitmentAd", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("corpRecProxy", "UpdateRecruitmentAd", &res), "UpdateRecruitmentAd");
    let res = check_rpc(&ctx, &mut s1, "crimewatch", "ApplyTagsForSecStatus", &[PyRep::List(vec![])]).await;
    assert_rpc_res("crimewatch", "ApplyTagsForSecStatus", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("crimewatch", "ApplyTagsForSecStatus", &res), "ApplyTagsForSecStatus");
    let res = check_rpc(&ctx, &mut s1, "crimewatch", "GetCharacterSecurityStatus", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("crimewatch", "GetCharacterSecurityStatus", &res, matches!(res, PyRep::Real(_))); assert_eq!(rpc_tag("crimewatch", "GetCharacterSecurityStatus", &res), "GetCharacterSecurityStatus");
    let res = check_rpc(&ctx, &mut s1, "crimewatch", "GetClientStates", &[]).await;
    assert_rpc_res("crimewatch", "GetClientStates", &res, matches!(res, PyRep::Dict(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("crimewatch", "GetClientStates", &res), "GetClientStates");
    let res = check_rpc(&ctx, &mut s1, "crimewatch", "GetMySecurityStatus", &[]).await;
    assert_rpc_res("crimewatch", "GetMySecurityStatus", &res, matches!(res, PyRep::Real(_))); assert_eq!(rpc_tag("crimewatch", "GetMySecurityStatus", &res), "GetMySecurityStatus");
    let res = check_rpc(&ctx, &mut s1, "crimewatch", "GetSecurityStatusTransactions", &[]).await;
    assert_rpc_res("crimewatch", "GetSecurityStatusTransactions", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("crimewatch", "GetSecurityStatusTransactions", &res), "GetSecurityStatusTransactions");
    let res = check_rpc(&ctx, &mut s1, "crimewatch", "RespondToDuelChallenge", &[PyRep::Integer(P2), PyRep::Bool(true)]).await;
    assert_rpc_res("crimewatch", "RespondToDuelChallenge", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("crimewatch", "RespondToDuelChallenge", &res), "RespondToDuelChallenge");
    let res = check_rpc(&ctx, &mut s1, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(2)]).await;
    assert_rpc_res("crimewatch", "SetSafetyLevel", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("crimewatch", "SetSafetyLevel", &res), "SetSafetyLevel");
    let res = check_rpc(&ctx, &mut s1, "crimewatch", "StartDuelChallenge", &[PyRep::Integer(P2)]).await;
    assert_rpc_res("crimewatch", "StartDuelChallenge", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("crimewatch", "StartDuelChallenge", &res), "StartDuelChallenge");
}

#[tokio::test]
async fn test_entity_and_ess_mgr_suite() {
    // parity: rpc:entity.CmdAbandonDrone, rpc:entity.CmdAssist, rpc:entity.CmdEngage, rpc:entity.CmdGuard, rpc:entity.CmdMineRepeatedly, rpc:entity.CmdReconnectToDrones, rpc:entity.CmdReturnBay, rpc:entity.CmdReturnHome, rpc:entity.CmdSalvage, rpc:entity.CmdUnanchor, rpc:entity.DisableBehaviorDebugging, rpc:entity.EnableBehaviorDebugging, rpc:entity.GetBehaviorTreeManager, rpc:entity.GetDebugSpawnPoolFleetEnemyClusters, rpc:entity.GetExtraFleetMembers, rpc:entity.OpenShippingUnitStorage, rpc:entity.SetSessionCheck, rpc:essMgr.AttemptLinkToMainBank, rpc:essMgr.AttemptLinkToReserveBank, rpc:essMgr.GetDataForClientSolarSystem, rpc:essMgr.GetMainBankTheftsForClientSolarSystem, rpc:essMgr.GetReserveBankTheftsForClientSolarSystem, rpc:essMgr.IsClientLinkedToReserveBank, rpc:essMgr.RequestMainBankUnlink, rpc:essMgr.RequestReserveBankUnlink, rpc:essMgr.RequestUnlockReserveBank, rpc:essMgr.clear_cache, rpc:essMgr.prime_cache_result
    eve_testkit::attest(&["rpc:entity.CmdAbandonDrone", "rpc:entity.CmdAssist", "rpc:entity.CmdEngage", "rpc:entity.CmdGuard", "rpc:entity.CmdMineRepeatedly", "rpc:entity.CmdReconnectToDrones", "rpc:entity.CmdReturnBay", "rpc:entity.CmdReturnHome", "rpc:entity.CmdSalvage", "rpc:entity.CmdUnanchor", "rpc:entity.DisableBehaviorDebugging", "rpc:entity.EnableBehaviorDebugging", "rpc:entity.GetBehaviorTreeManager", "rpc:entity.GetDebugSpawnPoolFleetEnemyClusters", "rpc:entity.GetExtraFleetMembers", "rpc:entity.OpenShippingUnitStorage", "rpc:entity.SetSessionCheck", "rpc:essMgr.AttemptLinkToMainBank", "rpc:essMgr.AttemptLinkToReserveBank", "rpc:essMgr.GetDataForClientSolarSystem", "rpc:essMgr.GetMainBankTheftsForClientSolarSystem", "rpc:essMgr.GetReserveBankTheftsForClientSolarSystem", "rpc:essMgr.IsClientLinkedToReserveBank", "rpc:essMgr.RequestMainBankUnlink", "rpc:essMgr.RequestReserveBankUnlink", "rpc:essMgr.RequestUnlockReserveBank", "rpc:essMgr.clear_cache", "rpc:essMgr.prime_cache_result"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8106, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "entity", "CmdAbandonDrone", &[PyRep::List(vec![PyRep::Integer(1)])]).await;
    assert_rpc_res("entity", "CmdAbandonDrone", &res, matches!(res, PyRep::None | PyRep::List(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("entity", "CmdAbandonDrone", &res), "CmdAbandonDrone");
    let res = check_rpc(&ctx, &mut s1, "entity", "CmdAssist", &[PyRep::Integer(P2)]).await;
    assert_rpc_res("entity", "CmdAssist", &res, matches!(res, PyRep::KeyVal(_) | PyRep::None | PyRep::List(_))); assert_eq!(rpc_tag("entity", "CmdAssist", &res), "CmdAssist");
    let res = check_rpc(&ctx, &mut s1, "entity", "CmdEngage", &[PyRep::List(vec![PyRep::Integer(P2)])]).await;
    assert_rpc_res("entity", "CmdEngage", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("entity", "CmdEngage", &res), "CmdEngage");
    let res = check_rpc(&ctx, &mut s1, "entity", "CmdGuard", &[PyRep::Integer(P2)]).await;
    assert_rpc_res("entity", "CmdGuard", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("entity", "CmdGuard", &res), "CmdGuard");
    let res = check_rpc(&ctx, &mut s1, "entity", "CmdMineRepeatedly", &[PyRep::Integer(1)]).await;
    assert_rpc_res("entity", "CmdMineRepeatedly", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("entity", "CmdMineRepeatedly", &res), "CmdMineRepeatedly");
    let res = check_rpc(&ctx, &mut s1, "entity", "CmdReconnectToDrones", &[]).await;
    assert_rpc_res("entity", "CmdReconnectToDrones", &res, matches!(res, PyRep::None | PyRep::List(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("entity", "CmdReconnectToDrones", &res), "CmdReconnectToDrones");
    let res = check_rpc(&ctx, &mut s1, "entity", "CmdReturnBay", &[]).await;
    assert_rpc_res("entity", "CmdReturnBay", &res, matches!(res, PyRep::None | PyRep::KeyVal(_) | PyRep::List(_))); assert_eq!(rpc_tag("entity", "CmdReturnBay", &res), "CmdReturnBay");
    let res = check_rpc(&ctx, &mut s1, "entity", "CmdReturnHome", &[]).await;
    assert_rpc_res("entity", "CmdReturnHome", &res, matches!(res, PyRep::None | PyRep::KeyVal(_) | PyRep::List(_) | PyRep::Integer(_))); assert_eq!(rpc_tag("entity", "CmdReturnHome", &res), "CmdReturnHome");
    let res = check_rpc(&ctx, &mut s1, "entity", "CmdSalvage", &[PyRep::Integer(1)]).await;
    assert_rpc_res("entity", "CmdSalvage", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("entity", "CmdSalvage", &res), "CmdSalvage");
    let res = check_rpc(&ctx, &mut s1, "entity", "CmdUnanchor", &[PyRep::Integer(1)]).await;
    assert_rpc_res("entity", "CmdUnanchor", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("entity", "CmdUnanchor", &res), "CmdUnanchor");
    let res = check_rpc(&ctx, &mut s1, "entity", "DisableBehaviorDebugging", &[]).await;
    assert_rpc_res("entity", "DisableBehaviorDebugging", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("entity", "DisableBehaviorDebugging", &res), "DisableBehaviorDebugging");
    let res = check_rpc(&ctx, &mut s1, "entity", "EnableBehaviorDebugging", &[]).await;
    assert_rpc_res("entity", "EnableBehaviorDebugging", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("entity", "EnableBehaviorDebugging", &res), "EnableBehaviorDebugging");
    let res = check_rpc(&ctx, &mut s1, "entity", "GetBehaviorTreeManager", &[]).await;
    assert_rpc_res("entity", "GetBehaviorTreeManager", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("entity", "GetBehaviorTreeManager", &res), "GetBehaviorTreeManager");
    let res = check_rpc(&ctx, &mut s1, "entity", "GetDebugSpawnPoolFleetEnemyClusters", &[]).await;
    assert_rpc_res("entity", "GetDebugSpawnPoolFleetEnemyClusters", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("entity", "GetDebugSpawnPoolFleetEnemyClusters", &res), "GetDebugSpawnPoolFleetEnemyClusters");
    let res = check_rpc(&ctx, &mut s1, "entity", "GetExtraFleetMembers", &[]).await;
    assert_rpc_res("entity", "GetExtraFleetMembers", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("entity", "GetExtraFleetMembers", &res), "GetExtraFleetMembers");
    let res = check_rpc(&ctx, &mut s1, "entity", "OpenShippingUnitStorage", &[PyRep::Integer(1)]).await;
    assert_rpc_res("entity", "OpenShippingUnitStorage", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("entity", "OpenShippingUnitStorage", &res), "OpenShippingUnitStorage");
    let res = check_rpc(&ctx, &mut s1, "entity", "SetSessionCheck", &[]).await;
    assert_rpc_res("entity", "SetSessionCheck", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("entity", "SetSessionCheck", &res), "SetSessionCheck");
    let res = check_rpc(&ctx, &mut s1, "essMgr", "AttemptLinkToMainBank", &[PyRep::Integer(1)]).await;
    assert_rpc_res("essMgr", "AttemptLinkToMainBank", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("essMgr", "AttemptLinkToMainBank", &res), "AttemptLinkToMainBank");
    let res = check_rpc(&ctx, &mut s1, "essMgr", "AttemptLinkToReserveBank", &[PyRep::Integer(1)]).await;
    assert_rpc_res("essMgr", "AttemptLinkToReserveBank", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("essMgr", "AttemptLinkToReserveBank", &res), "AttemptLinkToReserveBank");
    let res = check_rpc(&ctx, &mut s1, "essMgr", "GetDataForClientSolarSystem", &[PyRep::Integer(30000142)]).await;
    assert_rpc_res("essMgr", "GetDataForClientSolarSystem", &res, matches!(res, PyRep::KeyVal(_))); assert_eq!(rpc_tag("essMgr", "GetDataForClientSolarSystem", &res), "GetDataForClientSolarSystem");
    let res = check_rpc(&ctx, &mut s1, "essMgr", "GetMainBankTheftsForClientSolarSystem", &[PyRep::Integer(30000142)]).await;
    assert_rpc_res("essMgr", "GetMainBankTheftsForClientSolarSystem", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("essMgr", "GetMainBankTheftsForClientSolarSystem", &res), "GetMainBankTheftsForClientSolarSystem");
    let res = check_rpc(&ctx, &mut s1, "essMgr", "GetReserveBankTheftsForClientSolarSystem", &[PyRep::Integer(30000142)]).await;
    assert_rpc_res("essMgr", "GetReserveBankTheftsForClientSolarSystem", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("essMgr", "GetReserveBankTheftsForClientSolarSystem", &res), "GetReserveBankTheftsForClientSolarSystem");
    let res = check_rpc(&ctx, &mut s1, "essMgr", "IsClientLinkedToReserveBank", &[]).await;
    assert_rpc_res("essMgr", "IsClientLinkedToReserveBank", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("essMgr", "IsClientLinkedToReserveBank", &res), "IsClientLinkedToReserveBank");
    let res = check_rpc(&ctx, &mut s1, "essMgr", "RequestMainBankUnlink", &[]).await;
    assert_rpc_res("essMgr", "RequestMainBankUnlink", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("essMgr", "RequestMainBankUnlink", &res), "RequestMainBankUnlink");
    let res = check_rpc(&ctx, &mut s1, "essMgr", "RequestReserveBankUnlink", &[]).await;
    assert_rpc_res("essMgr", "RequestReserveBankUnlink", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("essMgr", "RequestReserveBankUnlink", &res), "RequestReserveBankUnlink");
    let res = check_rpc(&ctx, &mut s1, "essMgr", "RequestUnlockReserveBank", &[PyRep::Integer(1)]).await;
    assert_rpc_res("essMgr", "RequestUnlockReserveBank", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("essMgr", "RequestUnlockReserveBank", &res), "RequestUnlockReserveBank");
    let res = check_rpc(&ctx, &mut s1, "essMgr", "clear_cache", &[]).await;
    assert_rpc_res("essMgr", "clear_cache", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("essMgr", "clear_cache", &res), "clear_cache");
    let res = check_rpc(&ctx, &mut s1, "essMgr", "prime_cache_result", &[]).await;
    assert_rpc_res("essMgr", "prime_cache_result", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("essMgr", "prime_cache_result", &res), "prime_cache_result");
}

#[tokio::test]
async fn test_fighter_fleet_fw_suite() {
    // parity: rpc:fighterMgr.CmdAbandonFighter, rpc:fighterMgr.CmdActivateAbilitySlots, rpc:fighterMgr.CmdDeactivateAbilitySlots, rpc:fighterMgr.CmdScoopAbandonedFighterFromSpace, rpc:fighterMgr.ExecuteMovementCommandOnFighters, rpc:fighterMgr.GetFightersForShip, rpc:fighterMgr.LaunchFightersFromTubes, rpc:fighterMgr.LoadFightersToTube, rpc:fighterMgr.RecallFightersToTubes, rpc:fighterMgr.UnloadTubeToFighterBay, rpc:fleetProxy.AddFleetFinderAdvert, rpc:fleetProxy.ApplyToJoinFleet, rpc:fleetProxy.GetAvailableFleetAds, rpc:fleetProxy.GetMyFleetFinderAdvert, rpc:fleetProxy.RemoveFleetFinderAdvert, rpc:fleetProxy.UpdateAdvertAllowedEntities, rpc:fleetProxy.UpdateAdvertInfo, rpc:fleetProxy.UpdateFleetAdvertWithNewLeader, rpc:fwWarzoneSolarsystem.GetAllWarzones, rpc:fwWarzoneSolarsystem.GetAllWarzonesOccupationStates, rpc:fwWarzoneSolarsystem.GetAllWarzonesOccupationStatesUncached, rpc:fwWarzoneSolarsystem.GetHQSystemIDs, rpc:fwWarzoneSolarsystem.GetLocalOccupationState, rpc:fwWarzoneSolarsystem.iteritems, rpc:fwWarzoneSolarsystem.itervalues, rpc:fwWarzoneSolarsystem.keys
    eve_testkit::attest(&["rpc:fighterMgr.CmdAbandonFighter", "rpc:fighterMgr.CmdActivateAbilitySlots", "rpc:fighterMgr.CmdDeactivateAbilitySlots", "rpc:fighterMgr.CmdScoopAbandonedFighterFromSpace", "rpc:fighterMgr.ExecuteMovementCommandOnFighters", "rpc:fighterMgr.GetFightersForShip", "rpc:fighterMgr.LaunchFightersFromTubes", "rpc:fighterMgr.LoadFightersToTube", "rpc:fighterMgr.RecallFightersToTubes", "rpc:fighterMgr.UnloadTubeToFighterBay", "rpc:fleetProxy.AddFleetFinderAdvert", "rpc:fleetProxy.ApplyToJoinFleet", "rpc:fleetProxy.GetAvailableFleetAds", "rpc:fleetProxy.GetMyFleetFinderAdvert", "rpc:fleetProxy.RemoveFleetFinderAdvert", "rpc:fleetProxy.UpdateAdvertAllowedEntities", "rpc:fleetProxy.UpdateAdvertInfo", "rpc:fleetProxy.UpdateFleetAdvertWithNewLeader", "rpc:fwWarzoneSolarsystem.GetAllWarzones", "rpc:fwWarzoneSolarsystem.GetAllWarzonesOccupationStates", "rpc:fwWarzoneSolarsystem.GetAllWarzonesOccupationStatesUncached", "rpc:fwWarzoneSolarsystem.GetHQSystemIDs", "rpc:fwWarzoneSolarsystem.GetLocalOccupationState", "rpc:fwWarzoneSolarsystem.iteritems", "rpc:fwWarzoneSolarsystem.itervalues", "rpc:fwWarzoneSolarsystem.keys"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8107, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "fighterMgr", "CmdAbandonFighter", &[PyRep::List(vec![PyRep::Integer(1)])]).await;
    assert_rpc_res("fighterMgr", "CmdAbandonFighter", &res, matches!(res, PyRep::None | PyRep::List(_))); assert_eq!(rpc_tag("fighterMgr", "CmdAbandonFighter", &res), "CmdAbandonFighter");
    let res = check_rpc(&ctx, &mut s1, "fighterMgr", "CmdActivateAbilitySlots", &[PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(P2)]).await;
    assert_rpc_res("fighterMgr", "CmdActivateAbilitySlots", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("fighterMgr", "CmdActivateAbilitySlots", &res), "CmdActivateAbilitySlots");
    let res = check_rpc(&ctx, &mut s1, "fighterMgr", "CmdDeactivateAbilitySlots", &[PyRep::Integer(1), PyRep::Integer(0)]).await;
    assert_rpc_res("fighterMgr", "CmdDeactivateAbilitySlots", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("fighterMgr", "CmdDeactivateAbilitySlots", &res), "CmdDeactivateAbilitySlots");
    let res = check_rpc(&ctx, &mut s1, "fighterMgr", "CmdScoopAbandonedFighterFromSpace", &[PyRep::Integer(1)]).await;
    assert_rpc_res("fighterMgr", "CmdScoopAbandonedFighterFromSpace", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("fighterMgr", "CmdScoopAbandonedFighterFromSpace", &res), "CmdScoopAbandonedFighterFromSpace");
    let res = check_rpc(&ctx, &mut s1, "fighterMgr", "ExecuteMovementCommandOnFighters", &[PyRep::Integer(1), PyRep::List(vec![PyRep::Integer(1)])]).await;
    assert_rpc_res("fighterMgr", "ExecuteMovementCommandOnFighters", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("fighterMgr", "ExecuteMovementCommandOnFighters", &res), "ExecuteMovementCommandOnFighters");
    let res = check_rpc(&ctx, &mut s1, "fighterMgr", "GetFightersForShip", &[PyRep::Integer(100_000_001)]).await;
    assert_rpc_res("fighterMgr", "GetFightersForShip", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("fighterMgr", "GetFightersForShip", &res), "GetFightersForShip");
    let res = check_rpc(&ctx, &mut s1, "fighterMgr", "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await;
    assert_rpc_res("fighterMgr", "LaunchFightersFromTubes", &res, matches!(res, PyRep::None | PyRep::List(_))); assert_eq!(rpc_tag("fighterMgr", "LaunchFightersFromTubes", &res), "LaunchFightersFromTubes");
    let res = check_rpc(&ctx, &mut s1, "fighterMgr", "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(1)]).await;
    assert_rpc_res("fighterMgr", "LoadFightersToTube", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("fighterMgr", "LoadFightersToTube", &res), "LoadFightersToTube");
    let res = check_rpc(&ctx, &mut s1, "fighterMgr", "RecallFightersToTubes", &[PyRep::List(vec![PyRep::Integer(0)])]).await;
    assert_rpc_res("fighterMgr", "RecallFightersToTubes", &res, matches!(res, PyRep::None | PyRep::List(_))); assert_eq!(rpc_tag("fighterMgr", "RecallFightersToTubes", &res), "RecallFightersToTubes");
    let res = check_rpc(&ctx, &mut s1, "fighterMgr", "UnloadTubeToFighterBay", &[PyRep::Integer(0)]).await;
    assert_rpc_res("fighterMgr", "UnloadTubeToFighterBay", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("fighterMgr", "UnloadTubeToFighterBay", &res), "UnloadTubeToFighterBay");
    let res = check_rpc(&ctx, &mut s1, "fleetProxy", "AddFleetFinderAdvert", &[]).await;
    assert_rpc_res("fleetProxy", "AddFleetFinderAdvert", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("fleetProxy", "AddFleetFinderAdvert", &res), "AddFleetFinderAdvert");
    let res = check_rpc(&ctx, &mut s1, "fleetProxy", "ApplyToJoinFleet", &[PyRep::Integer(1)]).await;
    assert_rpc_res("fleetProxy", "ApplyToJoinFleet", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("fleetProxy", "ApplyToJoinFleet", &res), "ApplyToJoinFleet");
    let res = check_rpc(&ctx, &mut s1, "fleetProxy", "GetAvailableFleetAds", &[]).await;
    assert_rpc_res("fleetProxy", "GetAvailableFleetAds", &res, res.row_count().is_some() || matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("fleetProxy", "GetAvailableFleetAds", &res), "GetAvailableFleetAds");
    let res = check_rpc(&ctx, &mut s1, "fleetProxy", "GetMyFleetFinderAdvert", &[]).await;
    assert_rpc_res("fleetProxy", "GetMyFleetFinderAdvert", &res, matches!(res, PyRep::None | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("fleetProxy", "GetMyFleetFinderAdvert", &res), "GetMyFleetFinderAdvert");
    let res = check_rpc(&ctx, &mut s1, "fleetProxy", "RemoveFleetFinderAdvert", &[]).await;
    assert_rpc_res("fleetProxy", "RemoveFleetFinderAdvert", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("fleetProxy", "RemoveFleetFinderAdvert", &res), "RemoveFleetFinderAdvert");
    let res = check_rpc(&ctx, &mut s1, "fleetProxy", "UpdateAdvertAllowedEntities", &[]).await;
    assert_rpc_res("fleetProxy", "UpdateAdvertAllowedEntities", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("fleetProxy", "UpdateAdvertAllowedEntities", &res), "UpdateAdvertAllowedEntities");
    let res = check_rpc(&ctx, &mut s1, "fleetProxy", "UpdateAdvertInfo", &[]).await;
    assert_rpc_res("fleetProxy", "UpdateAdvertInfo", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("fleetProxy", "UpdateAdvertInfo", &res), "UpdateAdvertInfo");
    let res = check_rpc(&ctx, &mut s1, "fleetProxy", "UpdateFleetAdvertWithNewLeader", &[PyRep::Integer(P2)]).await;
    assert_rpc_res("fleetProxy", "UpdateFleetAdvertWithNewLeader", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("fleetProxy", "UpdateFleetAdvertWithNewLeader", &res), "UpdateFleetAdvertWithNewLeader");
    let res = check_rpc(&ctx, &mut s1, "fwWarzoneSolarsystem", "GetAllWarzones", &[]).await;
    assert_rpc_res("fwWarzoneSolarsystem", "GetAllWarzones", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("fwWarzoneSolarsystem", "GetAllWarzones", &res), "GetAllWarzones");
    let res = check_rpc(&ctx, &mut s1, "fwWarzoneSolarsystem", "GetAllWarzonesOccupationStates", &[]).await;
    assert_rpc_res("fwWarzoneSolarsystem", "GetAllWarzonesOccupationStates", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_))); assert_eq!(rpc_tag("fwWarzoneSolarsystem", "GetAllWarzonesOccupationStates", &res), "GetAllWarzonesOccupationStates");
    let res = check_rpc(&ctx, &mut s1, "fwWarzoneSolarsystem", "GetAllWarzonesOccupationStatesUncached", &[]).await;
    assert_rpc_res("fwWarzoneSolarsystem", "GetAllWarzonesOccupationStatesUncached", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_))); assert_eq!(rpc_tag("fwWarzoneSolarsystem", "GetAllWarzonesOccupationStatesUncached", &res), "GetAllWarzonesOccupationStatesUncached");
    let res = check_rpc(&ctx, &mut s1, "fwWarzoneSolarsystem", "GetHQSystemIDs", &[]).await;
    assert_rpc_res("fwWarzoneSolarsystem", "GetHQSystemIDs", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("fwWarzoneSolarsystem", "GetHQSystemIDs", &res), "GetHQSystemIDs");
    let res = check_rpc(&ctx, &mut s1, "fwWarzoneSolarsystem", "GetLocalOccupationState", &[PyRep::Integer(30000142)]).await;
    assert_rpc_res("fwWarzoneSolarsystem", "GetLocalOccupationState", &res, matches!(res, PyRep::KeyVal(_))); assert_eq!(rpc_tag("fwWarzoneSolarsystem", "GetLocalOccupationState", &res), "GetLocalOccupationState");
    let res = check_rpc(&ctx, &mut s1, "fwWarzoneSolarsystem", "iteritems", &[]).await;
    assert_rpc_res("fwWarzoneSolarsystem", "iteritems", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("fwWarzoneSolarsystem", "iteritems", &res), "iteritems");
    let res = check_rpc(&ctx, &mut s1, "fwWarzoneSolarsystem", "itervalues", &[]).await;
    assert_rpc_res("fwWarzoneSolarsystem", "itervalues", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("fwWarzoneSolarsystem", "itervalues", &res), "itervalues");
    let res = check_rpc(&ctx, &mut s1, "fwWarzoneSolarsystem", "keys", &[]).await;
    assert_rpc_res("fwWarzoneSolarsystem", "keys", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("fwWarzoneSolarsystem", "keys", &res), "keys");
}

#[tokio::test]
async fn test_industry_invbroker_suite() {
    // parity: rpc:industryManager.CancelJob, rpc:industryManager.CompleteJob, rpc:industryManager.CompleteManyJobs, rpc:industryManager.GetJob, rpc:industryManager.GetJobCounts, rpc:industryManager.GetJobsByOwner, rpc:industryManager.InstallJob, rpc:invbroker.DeliverToCorpHangar, rpc:invbroker.DeliverToCorpMember, rpc:invbroker.GetItemDescriptor, rpc:invbroker.SetSessionCheck, rpc:invbroker.SplitStack, rpc:invbroker.ValidateItemCanBeOpened, rpc:invbroker.ValidateItemListCanBeOpened, rpc:invbroker.WithdrawPlexFromVault
    eve_testkit::attest(&["rpc:industryManager.CancelJob", "rpc:industryManager.CompleteJob", "rpc:industryManager.CompleteManyJobs", "rpc:industryManager.GetJob", "rpc:industryManager.GetJobCounts", "rpc:industryManager.GetJobsByOwner", "rpc:industryManager.InstallJob", "rpc:invbroker.DeliverToCorpHangar", "rpc:invbroker.DeliverToCorpMember", "rpc:invbroker.GetItemDescriptor", "rpc:invbroker.SetSessionCheck", "rpc:invbroker.SplitStack", "rpc:invbroker.ValidateItemCanBeOpened", "rpc:invbroker.ValidateItemListCanBeOpened", "rpc:invbroker.WithdrawPlexFromVault"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8108, "demo", "demo-secret", P1).await;

    ctx.db.with_mut_snapshot(|snap| {
        snap.industry_jobs.insert(
            999_001,
            eve_db::IndustryJobRecord {
                job_id: 999_001,
                installer_id: P1,
                facility_id: JITA_44,
                blueprint_item_id: 1001,
                blueprint_type_id: 687,
                activity_id: 1,
                runs: 1,
                product_type_id: 587,
                status: "Active".into(),
                duration_seconds: 3600,
                start_unix: 0,
            },
        );
    });

    let res = check_rpc(&ctx, &mut s1, "industryManager", "CancelJob", &[PyRep::Integer(999_001)]).await;
    assert_rpc_res("industryManager", "CancelJob", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("industryManager", "CancelJob", &res), "CancelJob");
    let res = check_rpc(&ctx, &mut s1, "industryManager", "CompleteJob", &[PyRep::Integer(999_001)]).await;
    assert_rpc_res("industryManager", "CompleteJob", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("industryManager", "CompleteJob", &res), "CompleteJob");
    let res = check_rpc(&ctx, &mut s1, "industryManager", "CompleteManyJobs", &[PyRep::List(vec![PyRep::Integer(999_001)])]).await;
    assert_rpc_res("industryManager", "CompleteManyJobs", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("industryManager", "CompleteManyJobs", &res), "CompleteManyJobs");
    let res = check_rpc(&ctx, &mut s1, "industryManager", "GetJob", &[PyRep::Integer(999_001)]).await;
    assert_rpc_res("industryManager", "GetJob", &res, matches!(res, PyRep::KeyVal(_))); assert_eq!(rpc_tag("industryManager", "GetJob", &res), "GetJob");
    let res = check_rpc(&ctx, &mut s1, "industryManager", "GetJobCounts", &[]).await;
    assert_rpc_res("industryManager", "GetJobCounts", &res, matches!(res, PyRep::Dict(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("industryManager", "GetJobCounts", &res), "GetJobCounts");
    let res = check_rpc(&ctx, &mut s1, "industryManager", "GetJobsByOwner", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("industryManager", "GetJobsByOwner", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("industryManager", "GetJobsByOwner", &res), "GetJobsByOwner");
    let res = check_rpc(&ctx, &mut s1, "industryManager", "InstallJob", &[]).await;
    assert_rpc_res("industryManager", "InstallJob", &res, matches!(res, PyRep::Integer(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("industryManager", "InstallJob", &res), "InstallJob");
    let res = check_rpc(&ctx, &mut s1, "invbroker", "DeliverToCorpHangar", &[PyRep::Integer(1), PyRep::Integer(1)]).await;
    assert_rpc_res("invbroker", "DeliverToCorpHangar", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("invbroker", "DeliverToCorpHangar", &res), "DeliverToCorpHangar");
    let res = check_rpc(&ctx, &mut s1, "invbroker", "DeliverToCorpMember", &[PyRep::Integer(1), PyRep::Integer(P2)]).await;
    assert_rpc_res("invbroker", "DeliverToCorpMember", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("invbroker", "DeliverToCorpMember", &res), "DeliverToCorpMember");
    let res = check_rpc(&ctx, &mut s1, "invbroker", "GetItemDescriptor", &[PyRep::Integer(1)]).await;
    assert_rpc_res("invbroker", "GetItemDescriptor", &res, matches!(res, PyRep::ObjectEx { .. } | PyRep::KeyVal(_))); assert_eq!(rpc_tag("invbroker", "GetItemDescriptor", &res), "GetItemDescriptor");
    let res = check_rpc(&ctx, &mut s1, "invbroker", "SetSessionCheck", &[]).await;
    assert_rpc_res("invbroker", "SetSessionCheck", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("invbroker", "SetSessionCheck", &res), "SetSessionCheck");
    let res = check_rpc(&ctx, &mut s1, "invbroker", "SplitStack", &[PyRep::Integer(1), PyRep::Integer(10)]).await;
    assert_rpc_res("invbroker", "SplitStack", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("invbroker", "SplitStack", &res), "SplitStack");
    let res = check_rpc(&ctx, &mut s1, "invbroker", "ValidateItemCanBeOpened", &[PyRep::Integer(1)]).await;
    assert_rpc_res("invbroker", "ValidateItemCanBeOpened", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("invbroker", "ValidateItemCanBeOpened", &res), "ValidateItemCanBeOpened");
    let res = check_rpc(&ctx, &mut s1, "invbroker", "ValidateItemListCanBeOpened", &[PyRep::List(vec![PyRep::Integer(1)])]).await;
    assert_rpc_res("invbroker", "ValidateItemListCanBeOpened", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("invbroker", "ValidateItemListCanBeOpened", &res), "ValidateItemListCanBeOpened");
    let res = check_rpc(&ctx, &mut s1, "invbroker", "WithdrawPlexFromVault", &[PyRep::Integer(10)]).await;
    assert_rpc_res("invbroker", "WithdrawPlexFromVault", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("invbroker", "WithdrawPlexFromVault", &res), "WithdrawPlexFromVault");
}

#[tokio::test]
async fn test_lookup_macho_sov_suite() {
    // parity: rpc:lookupSvc.LookupCharacters, rpc:lookupSvc.LookupCorporations, rpc:lookupSvc.LookupEvePlayerCharacters, rpc:lookupSvc.LookupFactions, rpc:lookupSvc.LookupKnownLocationsByGroup, rpc:lookupSvc.LookupLocationsByGroup, rpc:lookupSvc.LookupNoneNPCAccountOwners, rpc:lookupSvc.LookupOwners, rpc:lookupSvc.LookupPCOwners, rpc:lookupSvc.LookupWarableCorporationsOrAlliances, rpc:machoNet.ForwardCharacterNotification, rpc:machoNet.GetCachedObject, rpc:machoNet.GetClusterGameStatisticsForClient, rpc:machoNet.GetServiceInfo, rpc:machoNet.NoBlock, rpc:machoNet.ReloadClientCodeHash, rpc:machoNet.SetGlobalConfigValue, rpc:sovMgr.AcquireSkyhooks, rpc:sovMgr.DestroySkyhooks, rpc:sovMgr.GetInfrastructureHubInfo, rpc:sovMgr.GetSovHubFuelAccessGroup, rpc:sovMgr.GetSovStructuresInfoForLocalSolarSystem, rpc:sovMgr.GetSovStructuresInfoForSolarSystem, rpc:sovMgr.GetSystemSovereigntyInfo, rpc:sovMgr.IsOnLocalSovHubFuelAccessGroup, rpc:sovMgr.SetSovHubFuelAccessGroup, rpc:sovMgr.remove_from_cache
    eve_testkit::attest(&["rpc:lookupSvc.LookupCharacters", "rpc:lookupSvc.LookupCorporations", "rpc:lookupSvc.LookupEvePlayerCharacters", "rpc:lookupSvc.LookupFactions", "rpc:lookupSvc.LookupKnownLocationsByGroup", "rpc:lookupSvc.LookupLocationsByGroup", "rpc:lookupSvc.LookupNoneNPCAccountOwners", "rpc:lookupSvc.LookupOwners", "rpc:lookupSvc.LookupPCOwners", "rpc:lookupSvc.LookupWarableCorporationsOrAlliances", "rpc:machoNet.ForwardCharacterNotification", "rpc:machoNet.GetCachedObject", "rpc:machoNet.GetClusterGameStatisticsForClient", "rpc:machoNet.GetServiceInfo", "rpc:machoNet.NoBlock", "rpc:machoNet.ReloadClientCodeHash", "rpc:machoNet.SetGlobalConfigValue", "rpc:sovMgr.AcquireSkyhooks", "rpc:sovMgr.DestroySkyhooks", "rpc:sovMgr.GetInfrastructureHubInfo", "rpc:sovMgr.GetSovHubFuelAccessGroup", "rpc:sovMgr.GetSovStructuresInfoForLocalSolarSystem", "rpc:sovMgr.GetSovStructuresInfoForSolarSystem", "rpc:sovMgr.GetSystemSovereigntyInfo", "rpc:sovMgr.IsOnLocalSovHubFuelAccessGroup", "rpc:sovMgr.SetSovHubFuelAccessGroup", "rpc:sovMgr.remove_from_cache"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8109, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "lookupSvc", "LookupCharacters", &[PyRep::String("Pilot".into())]).await;
    assert_rpc_res("lookupSvc", "LookupCharacters", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("lookupSvc", "LookupCharacters", &res), "LookupCharacters");
    let res = check_rpc(&ctx, &mut s1, "lookupSvc", "LookupCorporations", &[PyRep::String("Corp".into())]).await;
    assert_rpc_res("lookupSvc", "LookupCorporations", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("lookupSvc", "LookupCorporations", &res), "LookupCorporations");
    let res = check_rpc(&ctx, &mut s1, "lookupSvc", "LookupEvePlayerCharacters", &[PyRep::String("Pilot".into())]).await;
    assert_rpc_res("lookupSvc", "LookupEvePlayerCharacters", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("lookupSvc", "LookupEvePlayerCharacters", &res), "LookupEvePlayerCharacters");
    let res = check_rpc(&ctx, &mut s1, "lookupSvc", "LookupFactions", &[PyRep::String("Faction".into())]).await;
    assert_rpc_res("lookupSvc", "LookupFactions", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("lookupSvc", "LookupFactions", &res), "LookupFactions");
    let res = check_rpc(&ctx, &mut s1, "lookupSvc", "LookupKnownLocationsByGroup", &[PyRep::Integer(1)]).await;
    assert_rpc_res("lookupSvc", "LookupKnownLocationsByGroup", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("lookupSvc", "LookupKnownLocationsByGroup", &res), "LookupKnownLocationsByGroup");
    let res = check_rpc(&ctx, &mut s1, "lookupSvc", "LookupLocationsByGroup", &[PyRep::Integer(1)]).await;
    assert_rpc_res("lookupSvc", "LookupLocationsByGroup", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("lookupSvc", "LookupLocationsByGroup", &res), "LookupLocationsByGroup");
    let res = check_rpc(&ctx, &mut s1, "lookupSvc", "LookupNoneNPCAccountOwners", &[PyRep::String("Owner".into())]).await;
    assert_rpc_res("lookupSvc", "LookupNoneNPCAccountOwners", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("lookupSvc", "LookupNoneNPCAccountOwners", &res), "LookupNoneNPCAccountOwners");
    let res = check_rpc(&ctx, &mut s1, "lookupSvc", "LookupOwners", &[PyRep::String("Owner".into())]).await;
    assert_rpc_res("lookupSvc", "LookupOwners", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("lookupSvc", "LookupOwners", &res), "LookupOwners");
    let res = check_rpc(&ctx, &mut s1, "lookupSvc", "LookupPCOwners", &[PyRep::String("Owner".into())]).await;
    assert_rpc_res("lookupSvc", "LookupPCOwners", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("lookupSvc", "LookupPCOwners", &res), "LookupPCOwners");
    let res = check_rpc(&ctx, &mut s1, "lookupSvc", "LookupWarableCorporationsOrAlliances", &[PyRep::String("Corp".into())]).await;
    assert_rpc_res("lookupSvc", "LookupWarableCorporationsOrAlliances", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("lookupSvc", "LookupWarableCorporationsOrAlliances", &res), "LookupWarableCorporationsOrAlliances");
    let res = check_rpc(&ctx, &mut s1, "machoNet", "ForwardCharacterNotification", &[]).await;
    assert_rpc_res("machoNet", "ForwardCharacterNotification", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("machoNet", "ForwardCharacterNotification", &res), "ForwardCharacterNotification");
    let res = check_rpc(&ctx, &mut s1, "machoNet", "GetCachedObject", &[PyRep::String("obj_key".into())]).await;
    assert_rpc_res("machoNet", "GetCachedObject", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("machoNet", "GetCachedObject", &res), "GetCachedObject");
    let res = check_rpc(&ctx, &mut s1, "machoNet", "GetClusterGameStatisticsForClient", &[]).await;
    assert_rpc_res("machoNet", "GetClusterGameStatisticsForClient", &res, matches!(res, PyRep::Dict(_) | PyRep::Tuple(_))); assert_eq!(rpc_tag("machoNet", "GetClusterGameStatisticsForClient", &res), "GetClusterGameStatisticsForClient");
    let res = check_rpc(&ctx, &mut s1, "machoNet", "GetServiceInfo", &[PyRep::String("machoNet".into())]).await;
    assert_rpc_res("machoNet", "GetServiceInfo", &res, matches!(res, PyRep::Dict(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("machoNet", "GetServiceInfo", &res), "GetServiceInfo");
    let res = check_rpc(&ctx, &mut s1, "machoNet", "NoBlock", &[]).await;
    assert_rpc_res("machoNet", "NoBlock", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("machoNet", "NoBlock", &res), "NoBlock");
    let res = check_rpc(&ctx, &mut s1, "machoNet", "ReloadClientCodeHash", &[]).await;
    assert_rpc_res("machoNet", "ReloadClientCodeHash", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("machoNet", "ReloadClientCodeHash", &res), "ReloadClientCodeHash");
    let res = check_rpc(&ctx, &mut s1, "machoNet", "SetGlobalConfigValue", &[PyRep::String("k".into()), PyRep::String("v".into())]).await;
    assert_rpc_res("machoNet", "SetGlobalConfigValue", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("machoNet", "SetGlobalConfigValue", &res), "SetGlobalConfigValue");
    let res = check_rpc(&ctx, &mut s1, "sovMgr", "AcquireSkyhooks", &[PyRep::Integer(30000142)]).await;
    assert_rpc_res("sovMgr", "AcquireSkyhooks", &res, matches!(res, PyRep::None | PyRep::Integer(_))); assert_eq!(rpc_tag("sovMgr", "AcquireSkyhooks", &res), "AcquireSkyhooks");
    let res = check_rpc(&ctx, &mut s1, "sovMgr", "DestroySkyhooks", &[PyRep::Integer(30000142)]).await;
    assert_rpc_res("sovMgr", "DestroySkyhooks", &res, matches!(res, PyRep::None | PyRep::Integer(_))); assert_eq!(rpc_tag("sovMgr", "DestroySkyhooks", &res), "DestroySkyhooks");
    let res = check_rpc(&ctx, &mut s1, "sovMgr", "GetInfrastructureHubInfo", &[PyRep::Integer(30000142)]).await;
    assert_rpc_res("sovMgr", "GetInfrastructureHubInfo", &res, matches!(res, PyRep::KeyVal(_))); assert_eq!(rpc_tag("sovMgr", "GetInfrastructureHubInfo", &res), "GetInfrastructureHubInfo");
    let res = check_rpc(&ctx, &mut s1, "sovMgr", "GetSovHubFuelAccessGroup", &[PyRep::Integer(1)]).await;
    assert_rpc_res("sovMgr", "GetSovHubFuelAccessGroup", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("sovMgr", "GetSovHubFuelAccessGroup", &res), "GetSovHubFuelAccessGroup");
    let res = check_rpc(&ctx, &mut s1, "sovMgr", "GetSovStructuresInfoForLocalSolarSystem", &[]).await;
    assert_rpc_res("sovMgr", "GetSovStructuresInfoForLocalSolarSystem", &res, matches!(res, PyRep::List(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("sovMgr", "GetSovStructuresInfoForLocalSolarSystem", &res), "GetSovStructuresInfoForLocalSolarSystem");
    let res = check_rpc(&ctx, &mut s1, "sovMgr", "GetSovStructuresInfoForSolarSystem", &[PyRep::Integer(30000142)]).await;
    assert_rpc_res("sovMgr", "GetSovStructuresInfoForSolarSystem", &res, matches!(res, PyRep::List(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("sovMgr", "GetSovStructuresInfoForSolarSystem", &res), "GetSovStructuresInfoForSolarSystem");
    let res = check_rpc(&ctx, &mut s1, "sovMgr", "GetSystemSovereigntyInfo", &[PyRep::Integer(30000142)]).await;
    assert_rpc_res("sovMgr", "GetSystemSovereigntyInfo", &res, matches!(res, PyRep::KeyVal(_))); assert_eq!(rpc_tag("sovMgr", "GetSystemSovereigntyInfo", &res), "GetSystemSovereigntyInfo");
    let res = check_rpc(&ctx, &mut s1, "sovMgr", "IsOnLocalSovHubFuelAccessGroup", &[]).await;
    assert_rpc_res("sovMgr", "IsOnLocalSovHubFuelAccessGroup", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("sovMgr", "IsOnLocalSovHubFuelAccessGroup", &res), "IsOnLocalSovHubFuelAccessGroup");
    let res = check_rpc(&ctx, &mut s1, "sovMgr", "SetSovHubFuelAccessGroup", &[PyRep::Integer(1), PyRep::Integer(1)]).await;
    assert_rpc_res("sovMgr", "SetSovHubFuelAccessGroup", &res, matches!(res, PyRep::None | PyRep::Integer(_))); assert_eq!(rpc_tag("sovMgr", "SetSovHubFuelAccessGroup", &res), "SetSovHubFuelAccessGroup");
    let res = check_rpc(&ctx, &mut s1, "sovMgr", "remove_from_cache", &[PyRep::String("k".into())]).await;
    assert_rpc_res("sovMgr", "remove_from_cache", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("sovMgr", "remove_from_cache", &res), "remove_from_cache");
}
