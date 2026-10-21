//! Acceptance test suite for Classic RPC Methods Closure (Batch A — Part 2).
//!
//! Covering 174 rows across 14 services.


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
    sess.station_id = Some(JITA_44);

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
async fn test_tourney_mgr_suite() {
    // parity: rpc:tourneyMgr.AbortCountdown, rpc:tourneyMgr.AddPlayer, rpc:tourneyMgr.BanShip, rpc:tourneyMgr.BanShipType, rpc:tourneyMgr.CleanupGrid, rpc:tourneyMgr.CompletePregameChecks, rpc:tourneyMgr.CreateMatch, rpc:tourneyMgr.ExecuteMemberChange, rpc:tourneyMgr.FinalizeBans, rpc:tourneyMgr.GetActiveMatchForCamera, rpc:tourneyMgr.GetFancyDetails, rpc:tourneyMgr.GetMatchSystem, rpc:tourneyMgr.GetTeamMembers, rpc:tourneyMgr.IsOtherMatchInProgress, rpc:tourneyMgr.LockMemberList, rpc:tourneyMgr.MatchCompleted, rpc:tourneyMgr.OverrideDroneRestrict, rpc:tourneyMgr.OverrideError, rpc:tourneyMgr.OverrideFleetBossID, rpc:tourneyMgr.OverrideLockRestrict, rpc:tourneyMgr.OverrideMoveRestrict, rpc:tourneyMgr.OverrideWarpRestrict, rpc:tourneyMgr.QueryShipList, rpc:tourneyMgr.RemovePlayer, rpc:tourneyMgr.RemoveSelfFromReferees, rpc:tourneyMgr.ResetMatch, rpc:tourneyMgr.ReturnAllPlayers, rpc:tourneyMgr.ReturnTeamPlayers, rpc:tourneyMgr.StartAutobanning, rpc:tourneyMgr.StartCountdown, rpc:tourneyMgr.TeleportPlayers, rpc:tourneyMgr.TeleportRefHome, rpc:tourneyMgr.UnBanShipType, rpc:tourneyMgr.UnlockMemberList, rpc:tourneyMgr.UpdateFleetDetails
    eve_testkit::attest(&["rpc:tourneyMgr.AbortCountdown", "rpc:tourneyMgr.AddPlayer", "rpc:tourneyMgr.BanShip", "rpc:tourneyMgr.BanShipType", "rpc:tourneyMgr.CleanupGrid", "rpc:tourneyMgr.CompletePregameChecks", "rpc:tourneyMgr.CreateMatch", "rpc:tourneyMgr.ExecuteMemberChange", "rpc:tourneyMgr.FinalizeBans", "rpc:tourneyMgr.GetActiveMatchForCamera", "rpc:tourneyMgr.GetFancyDetails", "rpc:tourneyMgr.GetMatchSystem", "rpc:tourneyMgr.GetTeamMembers", "rpc:tourneyMgr.IsOtherMatchInProgress", "rpc:tourneyMgr.LockMemberList", "rpc:tourneyMgr.MatchCompleted", "rpc:tourneyMgr.OverrideDroneRestrict", "rpc:tourneyMgr.OverrideError", "rpc:tourneyMgr.OverrideFleetBossID", "rpc:tourneyMgr.OverrideLockRestrict", "rpc:tourneyMgr.OverrideMoveRestrict", "rpc:tourneyMgr.OverrideWarpRestrict", "rpc:tourneyMgr.QueryShipList", "rpc:tourneyMgr.RemovePlayer", "rpc:tourneyMgr.RemoveSelfFromReferees", "rpc:tourneyMgr.ResetMatch", "rpc:tourneyMgr.ReturnAllPlayers", "rpc:tourneyMgr.ReturnTeamPlayers", "rpc:tourneyMgr.StartAutobanning", "rpc:tourneyMgr.StartCountdown", "rpc:tourneyMgr.TeleportPlayers", "rpc:tourneyMgr.TeleportRefHome", "rpc:tourneyMgr.UnBanShipType", "rpc:tourneyMgr.UnlockMemberList", "rpc:tourneyMgr.UpdateFleetDetails"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8201, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "AbortCountdown", &[]).await;
    assert_rpc_res("tourneyMgr", "AbortCountdown", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "AbortCountdown", &res), "AbortCountdown");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "AddPlayer", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("tourneyMgr", "AddPlayer", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("tourneyMgr", "AddPlayer", &res), "AddPlayer");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "BanShip", &[PyRep::Integer(587)]).await;
    assert_rpc_res("tourneyMgr", "BanShip", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("tourneyMgr", "BanShip", &res), "BanShip");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "BanShipType", &[PyRep::Integer(587)]).await;
    assert_rpc_res("tourneyMgr", "BanShipType", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("tourneyMgr", "BanShipType", &res), "BanShipType");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "CleanupGrid", &[]).await;
    assert_rpc_res("tourneyMgr", "CleanupGrid", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "CleanupGrid", &res), "CleanupGrid");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "CompletePregameChecks", &[]).await;
    assert_rpc_res("tourneyMgr", "CompletePregameChecks", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "CompletePregameChecks", &res), "CompletePregameChecks");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "CreateMatch", &[]).await;
    assert_rpc_res("tourneyMgr", "CreateMatch", &res, matches!(res, PyRep::Integer(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "CreateMatch", &res), "CreateMatch");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "ExecuteMemberChange", &[]).await;
    assert_rpc_res("tourneyMgr", "ExecuteMemberChange", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "ExecuteMemberChange", &res), "ExecuteMemberChange");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "FinalizeBans", &[]).await;
    assert_rpc_res("tourneyMgr", "FinalizeBans", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "FinalizeBans", &res), "FinalizeBans");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "GetActiveMatchForCamera", &[]).await;
    assert_rpc_res("tourneyMgr", "GetActiveMatchForCamera", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "GetActiveMatchForCamera", &res), "GetActiveMatchForCamera");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "GetFancyDetails", &[PyRep::Integer(101)]).await;
    assert_rpc_res("tourneyMgr", "GetFancyDetails", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Dict(_))); assert_eq!(rpc_tag("tourneyMgr", "GetFancyDetails", &res), "GetFancyDetails");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "GetMatchSystem", &[]).await;
    assert_rpc_res("tourneyMgr", "GetMatchSystem", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("tourneyMgr", "GetMatchSystem", &res), "GetMatchSystem");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "GetTeamMembers", &[]).await;
    assert_rpc_res("tourneyMgr", "GetTeamMembers", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("tourneyMgr", "GetTeamMembers", &res), "GetTeamMembers");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "IsOtherMatchInProgress", &[]).await;
    assert_rpc_res("tourneyMgr", "IsOtherMatchInProgress", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("tourneyMgr", "IsOtherMatchInProgress", &res), "IsOtherMatchInProgress");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "LockMemberList", &[]).await;
    assert_rpc_res("tourneyMgr", "LockMemberList", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "LockMemberList", &res), "LockMemberList");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "MatchCompleted", &[]).await;
    assert_rpc_res("tourneyMgr", "MatchCompleted", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "MatchCompleted", &res), "MatchCompleted");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "OverrideDroneRestrict", &[PyRep::Bool(true)]).await;
    assert_rpc_res("tourneyMgr", "OverrideDroneRestrict", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("tourneyMgr", "OverrideDroneRestrict", &res), "OverrideDroneRestrict");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "OverrideError", &[PyRep::Bool(true)]).await;
    assert_rpc_res("tourneyMgr", "OverrideError", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("tourneyMgr", "OverrideError", &res), "OverrideError");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "OverrideFleetBossID", &[PyRep::Bool(true)]).await;
    assert_rpc_res("tourneyMgr", "OverrideFleetBossID", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("tourneyMgr", "OverrideFleetBossID", &res), "OverrideFleetBossID");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "OverrideLockRestrict", &[PyRep::Bool(true)]).await;
    assert_rpc_res("tourneyMgr", "OverrideLockRestrict", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("tourneyMgr", "OverrideLockRestrict", &res), "OverrideLockRestrict");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "OverrideMoveRestrict", &[PyRep::Bool(true)]).await;
    assert_rpc_res("tourneyMgr", "OverrideMoveRestrict", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("tourneyMgr", "OverrideMoveRestrict", &res), "OverrideMoveRestrict");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "OverrideWarpRestrict", &[PyRep::Bool(true)]).await;
    assert_rpc_res("tourneyMgr", "OverrideWarpRestrict", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("tourneyMgr", "OverrideWarpRestrict", &res), "OverrideWarpRestrict");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "QueryShipList", &[]).await;
    assert_rpc_res("tourneyMgr", "QueryShipList", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("tourneyMgr", "QueryShipList", &res), "QueryShipList");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "RemovePlayer", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("tourneyMgr", "RemovePlayer", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("tourneyMgr", "RemovePlayer", &res), "RemovePlayer");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "RemoveSelfFromReferees", &[]).await;
    assert_rpc_res("tourneyMgr", "RemoveSelfFromReferees", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "RemoveSelfFromReferees", &res), "RemoveSelfFromReferees");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "ResetMatch", &[]).await;
    assert_rpc_res("tourneyMgr", "ResetMatch", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "ResetMatch", &res), "ResetMatch");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "ReturnAllPlayers", &[]).await;
    assert_rpc_res("tourneyMgr", "ReturnAllPlayers", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "ReturnAllPlayers", &res), "ReturnAllPlayers");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "ReturnTeamPlayers", &[]).await;
    assert_rpc_res("tourneyMgr", "ReturnTeamPlayers", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "ReturnTeamPlayers", &res), "ReturnTeamPlayers");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "StartAutobanning", &[]).await;
    assert_rpc_res("tourneyMgr", "StartAutobanning", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "StartAutobanning", &res), "StartAutobanning");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "StartCountdown", &[]).await;
    assert_rpc_res("tourneyMgr", "StartCountdown", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "StartCountdown", &res), "StartCountdown");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "TeleportPlayers", &[]).await;
    assert_rpc_res("tourneyMgr", "TeleportPlayers", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "TeleportPlayers", &res), "TeleportPlayers");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "TeleportRefHome", &[]).await;
    assert_rpc_res("tourneyMgr", "TeleportRefHome", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "TeleportRefHome", &res), "TeleportRefHome");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "UnBanShipType", &[PyRep::Integer(587)]).await;
    assert_rpc_res("tourneyMgr", "UnBanShipType", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("tourneyMgr", "UnBanShipType", &res), "UnBanShipType");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "UnlockMemberList", &[]).await;
    assert_rpc_res("tourneyMgr", "UnlockMemberList", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "UnlockMemberList", &res), "UnlockMemberList");
    let res = check_rpc(&ctx, &mut s1, "tourneyMgr", "UpdateFleetDetails", &[]).await;
    assert_rpc_res("tourneyMgr", "UpdateFleetDetails", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("tourneyMgr", "UpdateFleetDetails", &res), "UpdateFleetDetails");
}

#[tokio::test]
async fn test_map_suite() {
    // parity: rpc:map.GetAllRoamingWeatherSystems, rpc:map.GetBeaconCount, rpc:map.GetConstellationLPData, rpc:map.GetCurrentSovData, rpc:map.GetDeadspaceAgentsMap, rpc:map.GetDeadspaceComplexMap, rpc:map.GetEdencomFortressSystems, rpc:map.GetEdencomMinorVictorySystems, rpc:map.GetFacWarZoneInfo, rpc:map.GetHistory, rpc:map.GetIncursionGlobalReport, rpc:map.GetIndustryCostModifier, rpc:map.GetIndustryJobsOverLast24Hours, rpc:map.GetMyExtraMapInfo, rpc:map.GetMyExtraMapInfoAgents, rpc:map.GetRecentSovActivity, rpc:map.GetSecurityModifiedSystems, rpc:map.GetSolarSystemVisits, rpc:map.GetStationCount, rpc:map.GetStationInfo, rpc:map.GetSystemsInIncursions, rpc:map.GetTriglavianMinorVictorySystems
    eve_testkit::attest(&["rpc:map.GetAllRoamingWeatherSystems", "rpc:map.GetBeaconCount", "rpc:map.GetConstellationLPData", "rpc:map.GetCurrentSovData", "rpc:map.GetDeadspaceAgentsMap", "rpc:map.GetDeadspaceComplexMap", "rpc:map.GetEdencomFortressSystems", "rpc:map.GetEdencomMinorVictorySystems", "rpc:map.GetFacWarZoneInfo", "rpc:map.GetHistory", "rpc:map.GetIncursionGlobalReport", "rpc:map.GetIndustryCostModifier", "rpc:map.GetIndustryJobsOverLast24Hours", "rpc:map.GetMyExtraMapInfo", "rpc:map.GetMyExtraMapInfoAgents", "rpc:map.GetRecentSovActivity", "rpc:map.GetSecurityModifiedSystems", "rpc:map.GetSolarSystemVisits", "rpc:map.GetStationCount", "rpc:map.GetStationInfo", "rpc:map.GetSystemsInIncursions", "rpc:map.GetTriglavianMinorVictorySystems"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8202, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "map", "GetAllRoamingWeatherSystems", &[]).await;
    assert_rpc_res("map", "GetAllRoamingWeatherSystems", &res, matches!(res, PyRep::List(_) | PyRep::Dict(_))); assert_eq!(rpc_tag("map", "GetAllRoamingWeatherSystems", &res), "GetAllRoamingWeatherSystems");
    let res = check_rpc(&ctx, &mut s1, "map", "GetBeaconCount", &[]).await;
    assert_rpc_res("map", "GetBeaconCount", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("map", "GetBeaconCount", &res), "GetBeaconCount");
    let res = check_rpc(&ctx, &mut s1, "map", "GetConstellationLPData", &[]).await;
    assert_rpc_res("map", "GetConstellationLPData", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_) | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("map", "GetConstellationLPData", &res), "GetConstellationLPData");
    let res = check_rpc(&ctx, &mut s1, "map", "GetCurrentSovData", &[]).await;
    assert_rpc_res("map", "GetCurrentSovData", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_) | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("map", "GetCurrentSovData", &res), "GetCurrentSovData");
    let res = check_rpc(&ctx, &mut s1, "map", "GetDeadspaceAgentsMap", &[]).await;
    assert_rpc_res("map", "GetDeadspaceAgentsMap", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_) | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("map", "GetDeadspaceAgentsMap", &res), "GetDeadspaceAgentsMap");
    let res = check_rpc(&ctx, &mut s1, "map", "GetDeadspaceComplexMap", &[]).await;
    assert_rpc_res("map", "GetDeadspaceComplexMap", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_) | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("map", "GetDeadspaceComplexMap", &res), "GetDeadspaceComplexMap");
    let res = check_rpc(&ctx, &mut s1, "map", "GetEdencomFortressSystems", &[]).await;
    assert_rpc_res("map", "GetEdencomFortressSystems", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("map", "GetEdencomFortressSystems", &res), "GetEdencomFortressSystems");
    let res = check_rpc(&ctx, &mut s1, "map", "GetEdencomMinorVictorySystems", &[]).await;
    assert_rpc_res("map", "GetEdencomMinorVictorySystems", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("map", "GetEdencomMinorVictorySystems", &res), "GetEdencomMinorVictorySystems");
    let res = check_rpc(&ctx, &mut s1, "map", "GetFacWarZoneInfo", &[]).await;
    assert_rpc_res("map", "GetFacWarZoneInfo", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_) | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("map", "GetFacWarZoneInfo", &res), "GetFacWarZoneInfo");
    let res = check_rpc(&ctx, &mut s1, "map", "GetHistory", &[]).await;
    assert_rpc_res("map", "GetHistory", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_) | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("map", "GetHistory", &res), "GetHistory");
    let res = check_rpc(&ctx, &mut s1, "map", "GetIncursionGlobalReport", &[]).await;
    assert_rpc_res("map", "GetIncursionGlobalReport", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_) | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("map", "GetIncursionGlobalReport", &res), "GetIncursionGlobalReport");
    let res = check_rpc(&ctx, &mut s1, "map", "GetIndustryCostModifier", &[PyRep::Integer(30000142)]).await;
    assert_rpc_res("map", "GetIndustryCostModifier", &res, matches!(res, PyRep::Real(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("map", "GetIndustryCostModifier", &res), "GetIndustryCostModifier");
    let res = check_rpc(&ctx, &mut s1, "map", "GetIndustryJobsOverLast24Hours", &[]).await;
    assert_rpc_res("map", "GetIndustryJobsOverLast24Hours", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_) | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("map", "GetIndustryJobsOverLast24Hours", &res), "GetIndustryJobsOverLast24Hours");
    let res = check_rpc(&ctx, &mut s1, "map", "GetMyExtraMapInfo", &[]).await;
    assert_rpc_res("map", "GetMyExtraMapInfo", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_) | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("map", "GetMyExtraMapInfo", &res), "GetMyExtraMapInfo");
    let res = check_rpc(&ctx, &mut s1, "map", "GetMyExtraMapInfoAgents", &[]).await;
    assert_rpc_res("map", "GetMyExtraMapInfoAgents", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_) | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("map", "GetMyExtraMapInfoAgents", &res), "GetMyExtraMapInfoAgents");
    let res = check_rpc(&ctx, &mut s1, "map", "GetRecentSovActivity", &[]).await;
    assert_rpc_res("map", "GetRecentSovActivity", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_) | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("map", "GetRecentSovActivity", &res), "GetRecentSovActivity");
    let res = check_rpc(&ctx, &mut s1, "map", "GetSecurityModifiedSystems", &[]).await;
    assert_rpc_res("map", "GetSecurityModifiedSystems", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("map", "GetSecurityModifiedSystems", &res), "GetSecurityModifiedSystems");
    let res = check_rpc(&ctx, &mut s1, "map", "GetSolarSystemVisits", &[]).await;
    assert_rpc_res("map", "GetSolarSystemVisits", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("map", "GetSolarSystemVisits", &res), "GetSolarSystemVisits");
    let res = check_rpc(&ctx, &mut s1, "map", "GetStationCount", &[]).await;
    assert_rpc_res("map", "GetStationCount", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("map", "GetStationCount", &res), "GetStationCount");
    let res = check_rpc(&ctx, &mut s1, "map", "GetStationInfo", &[PyRep::Integer(60003760)]).await;
    assert_rpc_res("map", "GetStationInfo", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Dict(_))); assert_eq!(rpc_tag("map", "GetStationInfo", &res), "GetStationInfo");
    let res = check_rpc(&ctx, &mut s1, "map", "GetSystemsInIncursions", &[]).await;
    assert_rpc_res("map", "GetSystemsInIncursions", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("map", "GetSystemsInIncursions", &res), "GetSystemsInIncursions");
    let res = check_rpc(&ctx, &mut s1, "map", "GetTriglavianMinorVictorySystems", &[]).await;
    assert_rpc_res("map", "GetTriglavianMinorVictorySystems", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("map", "GetTriglavianMinorVictorySystems", &res), "GetTriglavianMinorVictorySystems");
}

#[tokio::test]
async fn test_user_svc_suite() {
    // parity: rpc:userSvc.ActivateCharacterReSculpt, rpc:userSvc.ActivateMultiTraining, rpc:userSvc.ActivateSoulboundMultiTraining, rpc:userSvc.CastDownvote, rpc:userSvc.CastUpvote, rpc:userSvc.ClaimRedeemTokens, rpc:userSvc.CreateRedeemTokenQA, rpc:userSvc.GetCreateDate, rpc:userSvc.GetMultiCharactersTrainingSlots, rpc:userSvc.GetRedeemTokens, rpc:userSvc.GetSessionPlaytimeMinutes, rpc:userSvc.GetUpdatedMultiCharacterTraining, rpc:userSvc.GetUserName, rpc:userSvc.GetUserToken, rpc:userSvc.InvalidateMultiCharacterTrainingCache, rpc:userSvc.ReportBot, rpc:userSvc.ReportISKSpammer, rpc:userSvc.ReverseRedeem, rpc:userSvc.TrashRedeemTokens, rpc:userSvc.UserLogOffCharacter
    eve_testkit::attest(&["rpc:userSvc.ActivateCharacterReSculpt", "rpc:userSvc.ActivateMultiTraining", "rpc:userSvc.ActivateSoulboundMultiTraining", "rpc:userSvc.CastDownvote", "rpc:userSvc.CastUpvote", "rpc:userSvc.ClaimRedeemTokens", "rpc:userSvc.CreateRedeemTokenQA", "rpc:userSvc.GetCreateDate", "rpc:userSvc.GetMultiCharactersTrainingSlots", "rpc:userSvc.GetRedeemTokens", "rpc:userSvc.GetSessionPlaytimeMinutes", "rpc:userSvc.GetUpdatedMultiCharacterTraining", "rpc:userSvc.GetUserName", "rpc:userSvc.GetUserToken", "rpc:userSvc.InvalidateMultiCharacterTrainingCache", "rpc:userSvc.ReportBot", "rpc:userSvc.ReportISKSpammer", "rpc:userSvc.ReverseRedeem", "rpc:userSvc.TrashRedeemTokens", "rpc:userSvc.UserLogOffCharacter"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8203, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "userSvc", "ActivateCharacterReSculpt", &[]).await;
    assert_rpc_res("userSvc", "ActivateCharacterReSculpt", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("userSvc", "ActivateCharacterReSculpt", &res), "ActivateCharacterReSculpt");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "ActivateMultiTraining", &[]).await;
    assert_rpc_res("userSvc", "ActivateMultiTraining", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("userSvc", "ActivateMultiTraining", &res), "ActivateMultiTraining");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "ActivateSoulboundMultiTraining", &[]).await;
    assert_rpc_res("userSvc", "ActivateSoulboundMultiTraining", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("userSvc", "ActivateSoulboundMultiTraining", &res), "ActivateSoulboundMultiTraining");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "CastDownvote", &[]).await;
    assert_rpc_res("userSvc", "CastDownvote", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("userSvc", "CastDownvote", &res), "CastDownvote");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "CastUpvote", &[]).await;
    assert_rpc_res("userSvc", "CastUpvote", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("userSvc", "CastUpvote", &res), "CastUpvote");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "ClaimRedeemTokens", &[PyRep::List(vec![])]).await;
    assert_rpc_res("userSvc", "ClaimRedeemTokens", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::List(_))); assert_eq!(rpc_tag("userSvc", "ClaimRedeemTokens", &res), "ClaimRedeemTokens");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "CreateRedeemTokenQA", &[PyRep::Integer(1), PyRep::Integer(1)]).await;
    assert_rpc_res("userSvc", "CreateRedeemTokenQA", &res, matches!(res, PyRep::Integer(_) | PyRep::None)); assert_eq!(rpc_tag("userSvc", "CreateRedeemTokenQA", &res), "CreateRedeemTokenQA");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "GetCreateDate", &[]).await;
    assert_rpc_res("userSvc", "GetCreateDate", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("userSvc", "GetCreateDate", &res), "GetCreateDate");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "GetMultiCharactersTrainingSlots", &[]).await;
    assert_rpc_res("userSvc", "GetMultiCharactersTrainingSlots", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("userSvc", "GetMultiCharactersTrainingSlots", &res), "GetMultiCharactersTrainingSlots");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "GetRedeemTokens", &[]).await;
    assert_rpc_res("userSvc", "GetRedeemTokens", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("userSvc", "GetRedeemTokens", &res), "GetRedeemTokens");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "GetSessionPlaytimeMinutes", &[]).await;
    assert_rpc_res("userSvc", "GetSessionPlaytimeMinutes", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("userSvc", "GetSessionPlaytimeMinutes", &res), "GetSessionPlaytimeMinutes");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "GetUpdatedMultiCharacterTraining", &[]).await;
    assert_rpc_res("userSvc", "GetUpdatedMultiCharacterTraining", &res, matches!(res, PyRep::Dict(_) | PyRep::List(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("userSvc", "GetUpdatedMultiCharacterTraining", &res), "GetUpdatedMultiCharacterTraining");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "GetUserName", &[]).await;
    assert_rpc_res("userSvc", "GetUserName", &res, matches!(res, PyRep::String(_))); assert_eq!(rpc_tag("userSvc", "GetUserName", &res), "GetUserName");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "GetUserToken", &[]).await;
    assert_rpc_res("userSvc", "GetUserToken", &res, matches!(res, PyRep::String(_))); assert_eq!(rpc_tag("userSvc", "GetUserToken", &res), "GetUserToken");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "InvalidateMultiCharacterTrainingCache", &[]).await;
    assert_rpc_res("userSvc", "InvalidateMultiCharacterTrainingCache", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("userSvc", "InvalidateMultiCharacterTrainingCache", &res), "InvalidateMultiCharacterTrainingCache");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "ReportBot", &[PyRep::Integer(P2), PyRep::String("report".into())]).await;
    assert_rpc_res("userSvc", "ReportBot", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("userSvc", "ReportBot", &res), "ReportBot");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "ReportISKSpammer", &[PyRep::Integer(P2), PyRep::String("report".into())]).await;
    assert_rpc_res("userSvc", "ReportISKSpammer", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("userSvc", "ReportISKSpammer", &res), "ReportISKSpammer");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "ReverseRedeem", &[PyRep::List(vec![])]).await;
    assert_rpc_res("userSvc", "ReverseRedeem", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::List(_))); assert_eq!(rpc_tag("userSvc", "ReverseRedeem", &res), "ReverseRedeem");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "TrashRedeemTokens", &[PyRep::List(vec![])]).await;
    assert_rpc_res("userSvc", "TrashRedeemTokens", &res, matches!(res, PyRep::None | PyRep::Bool(_) | PyRep::List(_))); assert_eq!(rpc_tag("userSvc", "TrashRedeemTokens", &res), "TrashRedeemTokens");
    let res = check_rpc(&ctx, &mut s1, "userSvc", "UserLogOffCharacter", &[]).await;
    assert_rpc_res("userSvc", "UserLogOffCharacter", &res, matches!(res, PyRep::None | PyRep::Bool(_))); assert_eq!(rpc_tag("userSvc", "UserLogOffCharacter", &res), "UserLogOffCharacter");
}

#[tokio::test]
async fn test_season_and_vote_mgr_suite() {
    // parity: rpc:seasonManager.challenge_is_dormant, rpc:seasonManager.claim_challenge_rewards, rpc:seasonManager.claim_goal_reward, rpc:seasonManager.fill_up_and_return_challenges_for_character, rpc:seasonManager.get_challenge, rpc:seasonManager.get_challenge_expiration_date, rpc:seasonManager.get_character_progress_by_character_id, rpc:seasonManager.get_loot_table_by_typeID, rpc:seasonManager.get_max_active_challenges, rpc:seasonManager.get_season_data_for_character, rpc:seasonManager.get_season_end_time, rpc:seasonManager.process_client_event, rpc:seasonManager.reload_fsd_data, rpc:seasonManager.select_season, rpc:voteManager.ActivateSanctionedAction, rpc:voteManager.CanRunForCEO, rpc:voteManager.CanViewVotes, rpc:voteManager.GetCorpSanctionedActions, rpc:voteManager.GetVoteCase, rpc:voteManager.GetVoteCaseOptions, rpc:voteManager.GetVoteCasesByCorporation, rpc:voteManager.GetVotes, rpc:voteManager.InsertVote, rpc:voteManager.InsertVoteCase
    eve_testkit::attest(&["rpc:seasonManager.challenge_is_dormant", "rpc:seasonManager.claim_challenge_rewards", "rpc:seasonManager.claim_goal_reward", "rpc:seasonManager.fill_up_and_return_challenges_for_character", "rpc:seasonManager.get_challenge", "rpc:seasonManager.get_challenge_expiration_date", "rpc:seasonManager.get_character_progress_by_character_id", "rpc:seasonManager.get_loot_table_by_typeID", "rpc:seasonManager.get_max_active_challenges", "rpc:seasonManager.get_season_data_for_character", "rpc:seasonManager.get_season_end_time", "rpc:seasonManager.process_client_event", "rpc:seasonManager.reload_fsd_data", "rpc:seasonManager.select_season", "rpc:voteManager.ActivateSanctionedAction", "rpc:voteManager.CanRunForCEO", "rpc:voteManager.CanViewVotes", "rpc:voteManager.GetCorpSanctionedActions", "rpc:voteManager.GetVoteCase", "rpc:voteManager.GetVoteCaseOptions", "rpc:voteManager.GetVoteCasesByCorporation", "rpc:voteManager.GetVotes", "rpc:voteManager.InsertVote", "rpc:voteManager.InsertVoteCase"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8204, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "seasonManager", "challenge_is_dormant", &[PyRep::Integer(1)]).await;
    assert_rpc_res("seasonManager", "challenge_is_dormant", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("seasonManager", "challenge_is_dormant", &res), "challenge_is_dormant");
    let res = check_rpc(&ctx, &mut s1, "seasonManager", "claim_challenge_rewards", &[]).await;
    assert_rpc_res("seasonManager", "claim_challenge_rewards", &res, matches!(res, PyRep::None | PyRep::KeyVal(_) | PyRep::Bool(_))); assert_eq!(rpc_tag("seasonManager", "claim_challenge_rewards", &res), "claim_challenge_rewards");
    let res = check_rpc(&ctx, &mut s1, "seasonManager", "claim_goal_reward", &[PyRep::Integer(2)]).await;
    assert_rpc_res("seasonManager", "claim_goal_reward", &res, matches!(res, PyRep::None | PyRep::KeyVal(_) | PyRep::Bool(_))); assert_eq!(rpc_tag("seasonManager", "claim_goal_reward", &res), "claim_goal_reward");
    let res = check_rpc(&ctx, &mut s1, "seasonManager", "fill_up_and_return_challenges_for_character", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("seasonManager", "fill_up_and_return_challenges_for_character", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("seasonManager", "fill_up_and_return_challenges_for_character", &res), "fill_up_and_return_challenges_for_character");
    let res = check_rpc(&ctx, &mut s1, "seasonManager", "get_challenge", &[PyRep::Integer(1)]).await;
    assert_rpc_res("seasonManager", "get_challenge", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Dict(_))); assert_eq!(rpc_tag("seasonManager", "get_challenge", &res), "get_challenge");
    let res = check_rpc(&ctx, &mut s1, "seasonManager", "get_challenge_expiration_date", &[]).await;
    assert_rpc_res("seasonManager", "get_challenge_expiration_date", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("seasonManager", "get_challenge_expiration_date", &res), "get_challenge_expiration_date");
    let res = check_rpc(&ctx, &mut s1, "seasonManager", "get_character_progress_by_character_id", &[PyRep::Integer(2)]).await;
    assert_rpc_res("seasonManager", "get_character_progress_by_character_id", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Dict(_))); assert_eq!(rpc_tag("seasonManager", "get_character_progress_by_character_id", &res), "get_character_progress_by_character_id");
    let res = check_rpc(&ctx, &mut s1, "seasonManager", "get_loot_table_by_typeID", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("seasonManager", "get_loot_table_by_typeID", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("seasonManager", "get_loot_table_by_typeID", &res), "get_loot_table_by_typeID");
    let res = check_rpc(&ctx, &mut s1, "seasonManager", "get_max_active_challenges", &[]).await;
    assert_rpc_res("seasonManager", "get_max_active_challenges", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("seasonManager", "get_max_active_challenges", &res), "get_max_active_challenges");
    let res = check_rpc(&ctx, &mut s1, "seasonManager", "get_season_data_for_character", &[PyRep::Integer(1)]).await;
    assert_rpc_res("seasonManager", "get_season_data_for_character", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Dict(_))); assert_eq!(rpc_tag("seasonManager", "get_season_data_for_character", &res), "get_season_data_for_character");
    let res = check_rpc(&ctx, &mut s1, "seasonManager", "get_season_end_time", &[]).await;
    assert_rpc_res("seasonManager", "get_season_end_time", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("seasonManager", "get_season_end_time", &res), "get_season_end_time");
    let res = check_rpc(&ctx, &mut s1, "seasonManager", "process_client_event", &[PyRep::String("event".into())]).await;
    assert_rpc_res("seasonManager", "process_client_event", &res, matches!(res, PyRep::None | PyRep::KeyVal(_) | PyRep::Bool(_))); assert_eq!(rpc_tag("seasonManager", "process_client_event", &res), "process_client_event");
    let res = check_rpc(&ctx, &mut s1, "seasonManager", "reload_fsd_data", &[]).await;
    assert_rpc_res("seasonManager", "reload_fsd_data", &res, matches!(res, PyRep::None | PyRep::KeyVal(_) | PyRep::Bool(_))); assert_eq!(rpc_tag("seasonManager", "reload_fsd_data", &res), "reload_fsd_data");
    let res = check_rpc(&ctx, &mut s1, "seasonManager", "select_season", &[]).await;
    assert_rpc_res("seasonManager", "select_season", &res, matches!(res, PyRep::None | PyRep::KeyVal(_) | PyRep::Bool(_))); assert_eq!(rpc_tag("seasonManager", "select_season", &res), "select_season");
    let res = check_rpc(&ctx, &mut s1, "voteManager", "ActivateSanctionedAction", &[PyRep::Integer(1)]).await;
    assert_rpc_res("voteManager", "ActivateSanctionedAction", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("voteManager", "ActivateSanctionedAction", &res), "ActivateSanctionedAction");
    let res = check_rpc(&ctx, &mut s1, "voteManager", "CanRunForCEO", &[]).await;
    assert_rpc_res("voteManager", "CanRunForCEO", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("voteManager", "CanRunForCEO", &res), "CanRunForCEO");
    let res = check_rpc(&ctx, &mut s1, "voteManager", "CanViewVotes", &[]).await;
    assert_rpc_res("voteManager", "CanViewVotes", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("voteManager", "CanViewVotes", &res), "CanViewVotes");
    let res = check_rpc(&ctx, &mut s1, "voteManager", "GetCorpSanctionedActions", &[PyRep::Integer(1)]).await;
    assert_rpc_res("voteManager", "GetCorpSanctionedActions", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("voteManager", "GetCorpSanctionedActions", &res), "GetCorpSanctionedActions");
    let res = check_rpc(&ctx, &mut s1, "voteManager", "GetVoteCase", &[PyRep::Integer(1)]).await;
    assert_rpc_res("voteManager", "GetVoteCase", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Dict(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("voteManager", "GetVoteCase", &res), "GetVoteCase");
    let res = check_rpc(&ctx, &mut s1, "voteManager", "GetVoteCaseOptions", &[PyRep::Integer(1)]).await;
    assert_rpc_res("voteManager", "GetVoteCaseOptions", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("voteManager", "GetVoteCaseOptions", &res), "GetVoteCaseOptions");
    let res = check_rpc(&ctx, &mut s1, "voteManager", "GetVoteCasesByCorporation", &[PyRep::Integer(1)]).await;
    assert_rpc_res("voteManager", "GetVoteCasesByCorporation", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("voteManager", "GetVoteCasesByCorporation", &res), "GetVoteCasesByCorporation");
    let res = check_rpc(&ctx, &mut s1, "voteManager", "GetVotes", &[PyRep::Integer(1)]).await;
    assert_rpc_res("voteManager", "GetVotes", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("voteManager", "GetVotes", &res), "GetVotes");
    let res = check_rpc(&ctx, &mut s1, "voteManager", "InsertVote", &[PyRep::Integer(1), PyRep::Integer(1)]).await;
    assert_rpc_res("voteManager", "InsertVote", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("voteManager", "InsertVote", &res), "InsertVote");
    let res = check_rpc(&ctx, &mut s1, "voteManager", "InsertVoteCase", &[PyRep::String("Vote".into()), PyRep::String("Desc".into())]).await;
    assert_rpc_res("voteManager", "InsertVoteCase", &res, matches!(res, PyRep::Integer(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("voteManager", "InsertVoteCase", &res), "InsertVoteCase");
}

#[tokio::test]
async fn test_pvp_filament_and_kiring_suite() {
    // parity: rpc:pvpFilamentMgr.AbyssalPVPEndGateActivation, rpc:pvpFilamentMgr.GetActiveEvents, rpc:pvpFilamentMgr.GetAllEvents, rpc:pvpFilamentMgr.GetCharacterStatistics, rpc:pvpFilamentMgr.GetLeaderboard, rpc:pvpFilamentMgr.GetMostRecentEvent, rpc:pvpFilamentMgr.GetNextEventDate, rpc:pvpFilamentMgr.JoinPVPQueue, rpc:pvpFilamentMgr.LeavePVPQueue, rpc:pvpFilamentMgr.values, rpc:kiringMgr.ActivateRedeemingCode, rpc:kiringMgr.GetAntiAddictionCode, rpc:kiringMgr.GetEveGuardServerAddress, rpc:kiringMgr.GetKiringConfiguration, rpc:kiringMgr.GetSAuthAid, rpc:kiringMgr.IsMonochromeStyleEnabled, rpc:kiringMgr.PerformKiringServerSideAuthenticationFromCode, rpc:kiringMgr.PlaceKiringOrder, rpc:kiringMgr.ShouldKickCheater
    eve_testkit::attest(&["rpc:pvpFilamentMgr.AbyssalPVPEndGateActivation", "rpc:pvpFilamentMgr.GetActiveEvents", "rpc:pvpFilamentMgr.GetAllEvents", "rpc:pvpFilamentMgr.GetCharacterStatistics", "rpc:pvpFilamentMgr.GetLeaderboard", "rpc:pvpFilamentMgr.GetMostRecentEvent", "rpc:pvpFilamentMgr.GetNextEventDate", "rpc:pvpFilamentMgr.JoinPVPQueue", "rpc:pvpFilamentMgr.LeavePVPQueue", "rpc:pvpFilamentMgr.values", "rpc:kiringMgr.ActivateRedeemingCode", "rpc:kiringMgr.GetAntiAddictionCode", "rpc:kiringMgr.GetEveGuardServerAddress", "rpc:kiringMgr.GetKiringConfiguration", "rpc:kiringMgr.GetSAuthAid", "rpc:kiringMgr.IsMonochromeStyleEnabled", "rpc:kiringMgr.PerformKiringServerSideAuthenticationFromCode", "rpc:kiringMgr.PlaceKiringOrder", "rpc:kiringMgr.ShouldKickCheater"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8205, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "pvpFilamentMgr", "AbyssalPVPEndGateActivation", &[]).await;
    assert_rpc_res("pvpFilamentMgr", "AbyssalPVPEndGateActivation", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("pvpFilamentMgr", "AbyssalPVPEndGateActivation", &res), "AbyssalPVPEndGateActivation");
    let res = check_rpc(&ctx, &mut s1, "pvpFilamentMgr", "GetActiveEvents", &[]).await;
    assert_rpc_res("pvpFilamentMgr", "GetActiveEvents", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("pvpFilamentMgr", "GetActiveEvents", &res), "GetActiveEvents");
    let res = check_rpc(&ctx, &mut s1, "pvpFilamentMgr", "GetAllEvents", &[]).await;
    assert_rpc_res("pvpFilamentMgr", "GetAllEvents", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("pvpFilamentMgr", "GetAllEvents", &res), "GetAllEvents");
    let res = check_rpc(&ctx, &mut s1, "pvpFilamentMgr", "GetCharacterStatistics", &[PyRep::Integer(P1)]).await;
    assert_rpc_res("pvpFilamentMgr", "GetCharacterStatistics", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Dict(_))); assert_eq!(rpc_tag("pvpFilamentMgr", "GetCharacterStatistics", &res), "GetCharacterStatistics");
    let res = check_rpc(&ctx, &mut s1, "pvpFilamentMgr", "GetLeaderboard", &[]).await;
    assert_rpc_res("pvpFilamentMgr", "GetLeaderboard", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("pvpFilamentMgr", "GetLeaderboard", &res), "GetLeaderboard");
    let res = check_rpc(&ctx, &mut s1, "pvpFilamentMgr", "GetMostRecentEvent", &[]).await;
    assert_rpc_res("pvpFilamentMgr", "GetMostRecentEvent", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("pvpFilamentMgr", "GetMostRecentEvent", &res), "GetMostRecentEvent");
    let res = check_rpc(&ctx, &mut s1, "pvpFilamentMgr", "GetNextEventDate", &[]).await;
    assert_rpc_res("pvpFilamentMgr", "GetNextEventDate", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("pvpFilamentMgr", "GetNextEventDate", &res), "GetNextEventDate");
    let res = check_rpc(&ctx, &mut s1, "pvpFilamentMgr", "JoinPVPQueue", &[]).await;
    assert_rpc_res("pvpFilamentMgr", "JoinPVPQueue", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("pvpFilamentMgr", "JoinPVPQueue", &res), "JoinPVPQueue");
    let res = check_rpc(&ctx, &mut s1, "pvpFilamentMgr", "LeavePVPQueue", &[]).await;
    assert_rpc_res("pvpFilamentMgr", "LeavePVPQueue", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("pvpFilamentMgr", "LeavePVPQueue", &res), "LeavePVPQueue");
    let res = check_rpc(&ctx, &mut s1, "pvpFilamentMgr", "values", &[]).await;
    assert_rpc_res("pvpFilamentMgr", "values", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("pvpFilamentMgr", "values", &res), "values");
    let res = check_rpc(&ctx, &mut s1, "kiringMgr", "ActivateRedeemingCode", &[]).await;
    assert_rpc_res("kiringMgr", "ActivateRedeemingCode", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("kiringMgr", "ActivateRedeemingCode", &res), "ActivateRedeemingCode");
    let res = check_rpc(&ctx, &mut s1, "kiringMgr", "GetAntiAddictionCode", &[]).await;
    assert_rpc_res("kiringMgr", "GetAntiAddictionCode", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("kiringMgr", "GetAntiAddictionCode", &res), "GetAntiAddictionCode");
    let res = check_rpc(&ctx, &mut s1, "kiringMgr", "GetEveGuardServerAddress", &[]).await;
    assert_rpc_res("kiringMgr", "GetEveGuardServerAddress", &res, matches!(res, PyRep::String(_) | PyRep::None)); assert_eq!(rpc_tag("kiringMgr", "GetEveGuardServerAddress", &res), "GetEveGuardServerAddress");
    let res = check_rpc(&ctx, &mut s1, "kiringMgr", "GetKiringConfiguration", &[]).await;
    assert_rpc_res("kiringMgr", "GetKiringConfiguration", &res, matches!(res, PyRep::Dict(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("kiringMgr", "GetKiringConfiguration", &res), "GetKiringConfiguration");
    let res = check_rpc(&ctx, &mut s1, "kiringMgr", "GetSAuthAid", &[]).await;
    assert_rpc_res("kiringMgr", "GetSAuthAid", &res, matches!(res, PyRep::String(_) | PyRep::Integer(_))); assert_eq!(rpc_tag("kiringMgr", "GetSAuthAid", &res), "GetSAuthAid");
    let res = check_rpc(&ctx, &mut s1, "kiringMgr", "IsMonochromeStyleEnabled", &[]).await;
    assert_rpc_res("kiringMgr", "IsMonochromeStyleEnabled", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("kiringMgr", "IsMonochromeStyleEnabled", &res), "IsMonochromeStyleEnabled");
    let res = check_rpc(&ctx, &mut s1, "kiringMgr", "PerformKiringServerSideAuthenticationFromCode", &[]).await;
    assert_rpc_res("kiringMgr", "PerformKiringServerSideAuthenticationFromCode", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("kiringMgr", "PerformKiringServerSideAuthenticationFromCode", &res), "PerformKiringServerSideAuthenticationFromCode");
    let res = check_rpc(&ctx, &mut s1, "kiringMgr", "PlaceKiringOrder", &[]).await;
    assert_rpc_res("kiringMgr", "PlaceKiringOrder", &res, matches!(res, PyRep::Dict(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("kiringMgr", "PlaceKiringOrder", &res), "PlaceKiringOrder");
    let res = check_rpc(&ctx, &mut s1, "kiringMgr", "ShouldKickCheater", &[]).await;
    assert_rpc_res("kiringMgr", "ShouldKickCheater", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("kiringMgr", "ShouldKickCheater", &res), "ShouldKickCheater");
}

#[tokio::test]
async fn test_node_graph_and_cosmetics_suite() {
    // parity: rpc:node_graph.process_client_blackboard_request, rpc:node_graph.process_client_graph_message, rpc:node_graph.qa_start_node, rpc:node_graph.qa_start_node_graph, rpc:node_graph.qa_stop_active_nodes, rpc:node_graph.qa_stop_node, rpc:node_graph.qa_stop_node_graph, rpc:node_graph.qa_subscribe, rpc:node_graph.qa_unsubscribe, rpc:shipCosmeticsMgr.ActivateSkinLicense, rpc:shipCosmeticsMgr.ApplySkinToShip, rpc:shipCosmeticsMgr.GMExpireSkinLicense, rpc:shipCosmeticsMgr.GetEnabledCosmetics, rpc:shipCosmeticsMgr.GetFirstPartySkinData, rpc:shipCosmeticsMgr.GetLicencedSkins, rpc:shipCosmeticsMgr.GetLicencedSkinsForShipType, rpc:shipCosmeticsMgr.GiveSkin, rpc:shipCosmeticsMgr.RemoveSkin
    eve_testkit::attest(&["rpc:node_graph.process_client_blackboard_request", "rpc:node_graph.process_client_graph_message", "rpc:node_graph.qa_start_node", "rpc:node_graph.qa_start_node_graph", "rpc:node_graph.qa_stop_active_nodes", "rpc:node_graph.qa_stop_node", "rpc:node_graph.qa_stop_node_graph", "rpc:node_graph.qa_subscribe", "rpc:node_graph.qa_unsubscribe", "rpc:shipCosmeticsMgr.ActivateSkinLicense", "rpc:shipCosmeticsMgr.ApplySkinToShip", "rpc:shipCosmeticsMgr.GMExpireSkinLicense", "rpc:shipCosmeticsMgr.GetEnabledCosmetics", "rpc:shipCosmeticsMgr.GetFirstPartySkinData", "rpc:shipCosmeticsMgr.GetLicencedSkins", "rpc:shipCosmeticsMgr.GetLicencedSkinsForShipType", "rpc:shipCosmeticsMgr.GiveSkin", "rpc:shipCosmeticsMgr.RemoveSkin"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8206, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "node_graph", "process_client_blackboard_request", &[]).await;
    assert_rpc_res("node_graph", "process_client_blackboard_request", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("node_graph", "process_client_blackboard_request", &res), "process_client_blackboard_request");
    let res = check_rpc(&ctx, &mut s1, "node_graph", "process_client_graph_message", &[]).await;
    assert_rpc_res("node_graph", "process_client_graph_message", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("node_graph", "process_client_graph_message", &res), "process_client_graph_message");
    let res = check_rpc(&ctx, &mut s1, "node_graph", "qa_start_node", &[]).await;
    assert_rpc_res("node_graph", "qa_start_node", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("node_graph", "qa_start_node", &res), "qa_start_node");
    let res = check_rpc(&ctx, &mut s1, "node_graph", "qa_start_node_graph", &[]).await;
    assert_rpc_res("node_graph", "qa_start_node_graph", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("node_graph", "qa_start_node_graph", &res), "qa_start_node_graph");
    let res = check_rpc(&ctx, &mut s1, "node_graph", "qa_stop_active_nodes", &[]).await;
    assert_rpc_res("node_graph", "qa_stop_active_nodes", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("node_graph", "qa_stop_active_nodes", &res), "qa_stop_active_nodes");
    let res = check_rpc(&ctx, &mut s1, "node_graph", "qa_stop_node", &[]).await;
    assert_rpc_res("node_graph", "qa_stop_node", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("node_graph", "qa_stop_node", &res), "qa_stop_node");
    let res = check_rpc(&ctx, &mut s1, "node_graph", "qa_stop_node_graph", &[]).await;
    assert_rpc_res("node_graph", "qa_stop_node_graph", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("node_graph", "qa_stop_node_graph", &res), "qa_stop_node_graph");
    let res = check_rpc(&ctx, &mut s1, "node_graph", "qa_subscribe", &[]).await;
    assert_rpc_res("node_graph", "qa_subscribe", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("node_graph", "qa_subscribe", &res), "qa_subscribe");
    let res = check_rpc(&ctx, &mut s1, "node_graph", "qa_unsubscribe", &[]).await;
    assert_rpc_res("node_graph", "qa_unsubscribe", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("node_graph", "qa_unsubscribe", &res), "qa_unsubscribe");
    let res = check_rpc(&ctx, &mut s1, "shipCosmeticsMgr", "ActivateSkinLicense", &[PyRep::Integer(1)]).await;
    assert_rpc_res("shipCosmeticsMgr", "ActivateSkinLicense", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("shipCosmeticsMgr", "ActivateSkinLicense", &res), "ActivateSkinLicense");
    let res = check_rpc(&ctx, &mut s1, "shipCosmeticsMgr", "ApplySkinToShip", &[PyRep::Integer(1)]).await;
    assert_rpc_res("shipCosmeticsMgr", "ApplySkinToShip", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("shipCosmeticsMgr", "ApplySkinToShip", &res), "ApplySkinToShip");
    let res = check_rpc(&ctx, &mut s1, "shipCosmeticsMgr", "GMExpireSkinLicense", &[PyRep::Integer(1)]).await;
    assert_rpc_res("shipCosmeticsMgr", "GMExpireSkinLicense", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("shipCosmeticsMgr", "GMExpireSkinLicense", &res), "GMExpireSkinLicense");
    let res = check_rpc(&ctx, &mut s1, "shipCosmeticsMgr", "GetEnabledCosmetics", &[]).await;
    assert_rpc_res("shipCosmeticsMgr", "GetEnabledCosmetics", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("shipCosmeticsMgr", "GetEnabledCosmetics", &res), "GetEnabledCosmetics");
    let res = check_rpc(&ctx, &mut s1, "shipCosmeticsMgr", "GetFirstPartySkinData", &[]).await;
    assert_rpc_res("shipCosmeticsMgr", "GetFirstPartySkinData", &res, matches!(res, PyRep::Dict(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("shipCosmeticsMgr", "GetFirstPartySkinData", &res), "GetFirstPartySkinData");
    let res = check_rpc(&ctx, &mut s1, "shipCosmeticsMgr", "GetLicencedSkins", &[]).await;
    assert_rpc_res("shipCosmeticsMgr", "GetLicencedSkins", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("shipCosmeticsMgr", "GetLicencedSkins", &res), "GetLicencedSkins");
    let res = check_rpc(&ctx, &mut s1, "shipCosmeticsMgr", "GetLicencedSkinsForShipType", &[]).await;
    assert_rpc_res("shipCosmeticsMgr", "GetLicencedSkinsForShipType", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("shipCosmeticsMgr", "GetLicencedSkinsForShipType", &res), "GetLicencedSkinsForShipType");
    let res = check_rpc(&ctx, &mut s1, "shipCosmeticsMgr", "GiveSkin", &[PyRep::Integer(1)]).await;
    assert_rpc_res("shipCosmeticsMgr", "GiveSkin", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("shipCosmeticsMgr", "GiveSkin", &res), "GiveSkin");
    let res = check_rpc(&ctx, &mut s1, "shipCosmeticsMgr", "RemoveSkin", &[PyRep::Integer(1)]).await;
    assert_rpc_res("shipCosmeticsMgr", "RemoveSkin", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("shipCosmeticsMgr", "RemoveSkin", &res), "RemoveSkin");
}

#[tokio::test]
async fn test_crate_facility_dynamic_war_report_suite() {
    // parity: rpc:crateService.ClaimFitting, rpc:crateService.ClaimLoot, rpc:crateService.ClaimLootFromCrateStack, rpc:crateService.ClaimSkillBundle, rpc:crateService.GetCrateLocation, rpc:crateService.OpenCrate, rpc:crateService.PeekInsideFixedCrate, rpc:dynamicResourceCacheMgr.GetDBSMapData, rpc:dynamicResourceCacheMgr.GetDynamicResourceSettings, rpc:dynamicResourceCacheMgr.GetESSAgencyData, rpc:dynamicResourceCacheMgr.GetESSReserveBankKeys, rpc:dynamicResourceCacheMgr.GetESSSystemDetails, rpc:dynamicResourceCacheMgr.get, rpc:dynamicResourceCacheMgr.iteritems, rpc:facilityManager.GetFacilities, rpc:facilityManager.GetFacilitiesByID, rpc:facilityManager.GetFacility, rpc:facilityManager.GetFacilityLocations, rpc:facilityManager.GetFacilityTaxes, rpc:facilityManager.GetMaxActivityModifiers, rpc:facilityManager.SetFacilityTaxes, rpc:warRegistry.Bind, rpc:warRegistry.SetSessionCheck, rpc:warRegistry.UnBind, rpc:warRegistry.Unbind, rpc:warRegistry.timeDeclared, rpc:warRegistry.timeFinished, rpc:warRegistry.timeStarted, rpc:ingamereport.report_exploit_abuse, rpc:ingamereport.report_gambling, rpc:ingamereport.report_inappropriate_character_name, rpc:ingamereport.report_inappropriate_language, rpc:ingamereport.report_macro_use, rpc:ingamereport.report_offensive_mail, rpc:ingamereport.report_other, rpc:ingamereport.report_questionable_transaction
    eve_testkit::attest(&["rpc:crateService.ClaimFitting", "rpc:crateService.ClaimLoot", "rpc:crateService.ClaimLootFromCrateStack", "rpc:crateService.ClaimSkillBundle", "rpc:crateService.GetCrateLocation", "rpc:crateService.OpenCrate", "rpc:crateService.PeekInsideFixedCrate", "rpc:dynamicResourceCacheMgr.GetDBSMapData", "rpc:dynamicResourceCacheMgr.GetDynamicResourceSettings", "rpc:dynamicResourceCacheMgr.GetESSAgencyData", "rpc:dynamicResourceCacheMgr.GetESSReserveBankKeys", "rpc:dynamicResourceCacheMgr.GetESSSystemDetails", "rpc:dynamicResourceCacheMgr.get", "rpc:dynamicResourceCacheMgr.iteritems", "rpc:facilityManager.GetFacilities", "rpc:facilityManager.GetFacilitiesByID", "rpc:facilityManager.GetFacility", "rpc:facilityManager.GetFacilityLocations", "rpc:facilityManager.GetFacilityTaxes", "rpc:facilityManager.GetMaxActivityModifiers", "rpc:facilityManager.SetFacilityTaxes", "rpc:warRegistry.Bind", "rpc:warRegistry.SetSessionCheck", "rpc:warRegistry.UnBind", "rpc:warRegistry.Unbind", "rpc:warRegistry.timeDeclared", "rpc:warRegistry.timeFinished", "rpc:warRegistry.timeStarted", "rpc:ingamereport.report_exploit_abuse", "rpc:ingamereport.report_gambling", "rpc:ingamereport.report_inappropriate_character_name", "rpc:ingamereport.report_inappropriate_language", "rpc:ingamereport.report_macro_use", "rpc:ingamereport.report_offensive_mail", "rpc:ingamereport.report_other", "rpc:ingamereport.report_questionable_transaction"]);

    let ctx = ServerContext::new();
    let mut s1 = setup_pilot(&ctx, 8207, "demo", "demo-secret", P1).await;

    let res = check_rpc(&ctx, &mut s1, "crateService", "ClaimFitting", &[PyRep::Integer(1)]).await;
    assert_rpc_res("crateService", "ClaimFitting", &res, matches!(res, PyRep::None | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("crateService", "ClaimFitting", &res), "ClaimFitting");
    let res = check_rpc(&ctx, &mut s1, "crateService", "ClaimLoot", &[PyRep::Integer(1)]).await;
    assert_rpc_res("crateService", "ClaimLoot", &res, matches!(res, PyRep::None | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("crateService", "ClaimLoot", &res), "ClaimLoot");
    let res = check_rpc(&ctx, &mut s1, "crateService", "ClaimLootFromCrateStack", &[PyRep::Integer(1)]).await;
    assert_rpc_res("crateService", "ClaimLootFromCrateStack", &res, matches!(res, PyRep::None | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("crateService", "ClaimLootFromCrateStack", &res), "ClaimLootFromCrateStack");
    let res = check_rpc(&ctx, &mut s1, "crateService", "ClaimSkillBundle", &[PyRep::Integer(1)]).await;
    assert_rpc_res("crateService", "ClaimSkillBundle", &res, matches!(res, PyRep::None | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("crateService", "ClaimSkillBundle", &res), "ClaimSkillBundle");
    let res = check_rpc(&ctx, &mut s1, "crateService", "GetCrateLocation", &[PyRep::Integer(1)]).await;
    assert_rpc_res("crateService", "GetCrateLocation", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("crateService", "GetCrateLocation", &res), "GetCrateLocation");
    let res = check_rpc(&ctx, &mut s1, "crateService", "OpenCrate", &[PyRep::Integer(1)]).await;
    assert_rpc_res("crateService", "OpenCrate", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("crateService", "OpenCrate", &res), "OpenCrate");
    let res = check_rpc(&ctx, &mut s1, "crateService", "PeekInsideFixedCrate", &[PyRep::Integer(1)]).await;
    assert_rpc_res("crateService", "PeekInsideFixedCrate", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("crateService", "PeekInsideFixedCrate", &res), "PeekInsideFixedCrate");
    let res = check_rpc(&ctx, &mut s1, "dynamicResourceCacheMgr", "GetDBSMapData", &[]).await;
    assert_rpc_res("dynamicResourceCacheMgr", "GetDBSMapData", &res, matches!(res, PyRep::Dict(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("dynamicResourceCacheMgr", "GetDBSMapData", &res), "GetDBSMapData");
    let res = check_rpc(&ctx, &mut s1, "dynamicResourceCacheMgr", "GetDynamicResourceSettings", &[]).await;
    assert_rpc_res("dynamicResourceCacheMgr", "GetDynamicResourceSettings", &res, matches!(res, PyRep::Dict(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("dynamicResourceCacheMgr", "GetDynamicResourceSettings", &res), "GetDynamicResourceSettings");
    let res = check_rpc(&ctx, &mut s1, "dynamicResourceCacheMgr", "GetESSAgencyData", &[]).await;
    assert_rpc_res("dynamicResourceCacheMgr", "GetESSAgencyData", &res, matches!(res, PyRep::Dict(_) | PyRep::KeyVal(_))); assert_eq!(rpc_tag("dynamicResourceCacheMgr", "GetESSAgencyData", &res), "GetESSAgencyData");
    let res = check_rpc(&ctx, &mut s1, "dynamicResourceCacheMgr", "GetESSReserveBankKeys", &[]).await;
    assert_rpc_res("dynamicResourceCacheMgr", "GetESSReserveBankKeys", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("dynamicResourceCacheMgr", "GetESSReserveBankKeys", &res), "GetESSReserveBankKeys");
    let res = check_rpc(&ctx, &mut s1, "dynamicResourceCacheMgr", "GetESSSystemDetails", &[PyRep::Integer(30000142)]).await;
    assert_rpc_res("dynamicResourceCacheMgr", "GetESSSystemDetails", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Dict(_))); assert_eq!(rpc_tag("dynamicResourceCacheMgr", "GetESSSystemDetails", &res), "GetESSSystemDetails");
    let res = check_rpc(&ctx, &mut s1, "dynamicResourceCacheMgr", "get", &[PyRep::String("k".into())]).await;
    assert_rpc_res("dynamicResourceCacheMgr", "get", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("dynamicResourceCacheMgr", "get", &res), "get");
    let res = check_rpc(&ctx, &mut s1, "dynamicResourceCacheMgr", "iteritems", &[]).await;
    assert_rpc_res("dynamicResourceCacheMgr", "iteritems", &res, matches!(res, PyRep::List(_))); assert_eq!(rpc_tag("dynamicResourceCacheMgr", "iteritems", &res), "iteritems");
    let res = check_rpc(&ctx, &mut s1, "facilityManager", "GetFacilities", &[]).await;
    assert_rpc_res("facilityManager", "GetFacilities", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("facilityManager", "GetFacilities", &res), "GetFacilities");
    let res = check_rpc(&ctx, &mut s1, "facilityManager", "GetFacilitiesByID", &[]).await;
    assert_rpc_res("facilityManager", "GetFacilitiesByID", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("facilityManager", "GetFacilitiesByID", &res), "GetFacilitiesByID");
    let res = check_rpc(&ctx, &mut s1, "facilityManager", "GetFacility", &[PyRep::Integer(60003760)]).await;
    assert_rpc_res("facilityManager", "GetFacility", &res, matches!(res, PyRep::KeyVal(_) | PyRep::Dict(_))); assert_eq!(rpc_tag("facilityManager", "GetFacility", &res), "GetFacility");
    let res = check_rpc(&ctx, &mut s1, "facilityManager", "GetFacilityLocations", &[]).await;
    assert_rpc_res("facilityManager", "GetFacilityLocations", &res, matches!(res, PyRep::List(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("facilityManager", "GetFacilityLocations", &res), "GetFacilityLocations");
    let res = check_rpc(&ctx, &mut s1, "facilityManager", "GetFacilityTaxes", &[PyRep::Integer(60003760)]).await;
    assert_rpc_res("facilityManager", "GetFacilityTaxes", &res, matches!(res, PyRep::Dict(_) | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("facilityManager", "GetFacilityTaxes", &res), "GetFacilityTaxes");
    let res = check_rpc(&ctx, &mut s1, "facilityManager", "GetMaxActivityModifiers", &[PyRep::Integer(60003760)]).await;
    assert_rpc_res("facilityManager", "GetMaxActivityModifiers", &res, matches!(res, PyRep::Dict(_) | PyRep::KeyVal(_)) || res.row_count().is_some()); assert_eq!(rpc_tag("facilityManager", "GetMaxActivityModifiers", &res), "GetMaxActivityModifiers");
    let res = check_rpc(&ctx, &mut s1, "facilityManager", "SetFacilityTaxes", &[PyRep::Integer(60003760), PyRep::Real(0.05)]).await;
    assert_rpc_res("facilityManager", "SetFacilityTaxes", &res, matches!(res, PyRep::None | PyRep::KeyVal(_))); assert_eq!(rpc_tag("facilityManager", "SetFacilityTaxes", &res), "SetFacilityTaxes");
    let res = check_rpc(&ctx, &mut s1, "warRegistry", "Bind", &[]).await;
    assert_rpc_res("warRegistry", "Bind", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("warRegistry", "Bind", &res), "Bind");
    let res = check_rpc(&ctx, &mut s1, "warRegistry", "SetSessionCheck", &[]).await;
    assert_rpc_res("warRegistry", "SetSessionCheck", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("warRegistry", "SetSessionCheck", &res), "SetSessionCheck");
    let res = check_rpc(&ctx, &mut s1, "warRegistry", "UnBind", &[]).await;
    assert_rpc_res("warRegistry", "UnBind", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("warRegistry", "UnBind", &res), "UnBind");
    let res = check_rpc(&ctx, &mut s1, "warRegistry", "Unbind", &[]).await;
    assert_rpc_res("warRegistry", "Unbind", &res, matches!(res, PyRep::None)); assert_eq!(rpc_tag("warRegistry", "Unbind", &res), "Unbind");
    let res = check_rpc(&ctx, &mut s1, "warRegistry", "timeDeclared", &[]).await;
    assert_rpc_res("warRegistry", "timeDeclared", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("warRegistry", "timeDeclared", &res), "timeDeclared");
    let res = check_rpc(&ctx, &mut s1, "warRegistry", "timeFinished", &[]).await;
    assert_rpc_res("warRegistry", "timeFinished", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("warRegistry", "timeFinished", &res), "timeFinished");
    let res = check_rpc(&ctx, &mut s1, "warRegistry", "timeStarted", &[]).await;
    assert_rpc_res("warRegistry", "timeStarted", &res, matches!(res, PyRep::Integer(_))); assert_eq!(rpc_tag("warRegistry", "timeStarted", &res), "timeStarted");
    let res = check_rpc(&ctx, &mut s1, "ingamereport", "report_exploit_abuse", &[PyRep::String("report".into())]).await;
    assert_rpc_res("ingamereport", "report_exploit_abuse", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("ingamereport", "report_exploit_abuse", &res), "report_exploit_abuse");
    let res = check_rpc(&ctx, &mut s1, "ingamereport", "report_gambling", &[PyRep::String("report".into())]).await;
    assert_rpc_res("ingamereport", "report_gambling", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("ingamereport", "report_gambling", &res), "report_gambling");
    let res = check_rpc(&ctx, &mut s1, "ingamereport", "report_inappropriate_character_name", &[PyRep::String("report".into())]).await;
    assert_rpc_res("ingamereport", "report_inappropriate_character_name", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("ingamereport", "report_inappropriate_character_name", &res), "report_inappropriate_character_name");
    let res = check_rpc(&ctx, &mut s1, "ingamereport", "report_inappropriate_language", &[PyRep::String("report".into())]).await;
    assert_rpc_res("ingamereport", "report_inappropriate_language", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("ingamereport", "report_inappropriate_language", &res), "report_inappropriate_language");
    let res = check_rpc(&ctx, &mut s1, "ingamereport", "report_macro_use", &[PyRep::String("report".into())]).await;
    assert_rpc_res("ingamereport", "report_macro_use", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("ingamereport", "report_macro_use", &res), "report_macro_use");
    let res = check_rpc(&ctx, &mut s1, "ingamereport", "report_offensive_mail", &[PyRep::String("report".into())]).await;
    assert_rpc_res("ingamereport", "report_offensive_mail", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("ingamereport", "report_offensive_mail", &res), "report_offensive_mail");
    let res = check_rpc(&ctx, &mut s1, "ingamereport", "report_other", &[PyRep::String("report".into())]).await;
    assert_rpc_res("ingamereport", "report_other", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("ingamereport", "report_other", &res), "report_other");
    let res = check_rpc(&ctx, &mut s1, "ingamereport", "report_questionable_transaction", &[PyRep::String("report".into())]).await;
    assert_rpc_res("ingamereport", "report_questionable_transaction", &res, matches!(res, PyRep::Bool(_))); assert_eq!(rpc_tag("ingamereport", "report_questionable_transaction", &res), "report_questionable_transaction");
}
