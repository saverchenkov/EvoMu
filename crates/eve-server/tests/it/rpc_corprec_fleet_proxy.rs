//! Integration tests for Corporation Recruitment and Fleet RPCs:
//! - `corpRecProxy`: `GetRecruiters`, `GetRecruitmentAd`, `GetRecruitmentAdsByCorpID`, `GetRecruitmentAdsForCorporation`
//! - `fleetProxy`: `GetMyFleetFinderAdvert`, `RemoveFleetFinderAdvert`, `UpdateAdvertInfo`, `UpdateFleetAdvertWithNewLeader`
//! - `fleetMgr`: `BroadcastToBubble`, `BroadcastToSystem`, `ForceLeaveFleet`

#![forbid(unsafe_code)]

use std::collections::HashMap;

use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;

use super::common;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;

fn field<'a>(rep: &'a PyRep, key: &str) -> &'a PyRep {
    match rep {
        PyRep::KeyVal(kv) => kv
            .get(key)
            .unwrap_or_else(|| panic!("missing keyval field: {key}")),
        PyRep::Dict(entries) => entries
            .iter()
            .find(|(k, _)| k.as_str() == Some(key))
            .map(|(_, v)| v)
            .unwrap_or_else(|| panic!("missing dict field: {key}")),
        other => panic!("expected KeyVal or Dict, got {other:?}"),
    }
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None).await
}

async fn login_pilot_char(
    ctx: &ServerContext,
    sess_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sess_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

#[tokio::test]
// parity: rpc:corpRecProxy.GetRecruiters
// parity: rpc:corpRecProxy.GetRecruitmentAd
// parity: rpc:corpRecProxy.GetRecruitmentAdsByCorpID
// parity: rpc:corpRecProxy.GetRecruitmentAdsForCorporation
async fn test_corprec_proxy_rpcs() {
    let ctx = ServerContext::new();
    let corp_id = 2_000_088;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.corporation_id = corp_id;
            ch.balance = eve_db::IskCents::from_isk(10_000_000);
        }
    });

    let mut sess = login_pilot_char(&ctx, 28_001, "demo", "demo-secret", PILOT_1).await;

    // 1. Create a recruitment ad to establish state
    let create_args = vec![
        PyRep::String("Frontier Exploits Recruiting".into()),
        PyRep::String("Join our highsec mining & exploration ops".into()),
        PyRep::Integer(16),
    ];
    let created = rpc(&ctx, &mut sess, "corpRecProxy", "CreateRecruitmentAd", &create_args)
        .await
        .expect("CreateRecruitmentAd");
    let ad_id = field(&created, "adID").as_i64().expect("adID");
    assert!(ad_id > 0);

    // 2. corpRecProxy.GetRecruitmentAdsForCorporation: queries ads for caller's corporation
    let for_corp = rpc(&ctx, &mut sess, "corpRecProxy", "GetRecruitmentAdsForCorporation", &[])
        .await
        .expect("GetRecruitmentAdsForCorporation");
    match for_corp {
        PyRep::Rowset { rows, columns, .. } => {
            assert!(rows.iter().any(|r| r.first().and_then(|v| v.as_i64()) == Some(ad_id)));
            assert!(columns.contains(&"adID".into()));
            assert!(columns.contains(&"corporationID".into()));
        }
        other => panic!("expected Rowset from GetRecruitmentAdsForCorporation, got {other:?}"),
    }

    // 3. corpRecProxy.GetRecruitmentAdsByCorpID: queries ads by specific corpID
    let by_corp = rpc(&ctx, &mut sess, "corpRecProxy", "GetRecruitmentAdsByCorpID", &[PyRep::Integer(corp_id)])
        .await
        .expect("GetRecruitmentAdsByCorpID");
    match by_corp {
        PyRep::Rowset { rows, .. } => {
            assert!(rows.iter().any(|r| r.first().and_then(|v| v.as_i64()) == Some(ad_id)));
        }
        other => panic!("expected Rowset from GetRecruitmentAdsByCorpID, got {other:?}"),
    }

    // 4. corpRecProxy.GetRecruitmentAd: queries specific ad by adID
    let single_ad = rpc(&ctx, &mut sess, "corpRecProxy", "GetRecruitmentAd", &[PyRep::Integer(ad_id)])
        .await
        .expect("GetRecruitmentAd");
    match single_ad {
        PyRep::Rowset { rows, .. } => {
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].first().and_then(|v| v.as_i64()), Some(ad_id));
        }
        other => panic!("expected Rowset from GetRecruitmentAd, got {other:?}"),
    }

    // 5. corpRecProxy.GetRecruiters: queries recruiter list for corporation and ad
    let recruiters_rep = rpc(
        &ctx,
        &mut sess,
        "corpRecProxy",
        "GetRecruiters",
        &[PyRep::Integer(corp_id), PyRep::Integer(ad_id)],
    )
    .await
    .expect("GetRecruiters");
    match recruiters_rep {
        PyRep::List(items) => {
            assert!(!items.is_empty());
            assert!(items.iter().any(|v| v.as_i64() == Some(PILOT_1)));
        }
        other => panic!("expected List from GetRecruiters, got {other:?}"),
    }

    // Clean up created recruitment ad so it does not pollute other test suites
    let _ = rpc(&ctx, &mut sess, "corpRecProxy", "DeleteRecruitmentAd", &[PyRep::Integer(ad_id)])
        .await
        .expect("DeleteRecruitmentAd");
}

#[tokio::test]
// parity: rpc:fleetProxy.GetMyFleetFinderAdvert
// parity: rpc:fleetProxy.RemoveFleetFinderAdvert
// parity: rpc:fleetProxy.UpdateAdvertInfo
// parity: rpc:fleetProxy.UpdateFleetAdvertWithNewLeader
// parity: rpc:fleetMgr.BroadcastToBubble
// parity: rpc:fleetMgr.BroadcastToSystem
// parity: rpc:fleetMgr.ForceLeaveFleet
async fn test_fleet_proxy_and_fleet_mgr_rpcs() {
    let ctx = ServerContext::new();
    let mut sess1 = login_pilot_char(&ctx, 28_002, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = login_pilot_char(&ctx, 28_003, "other", "other-secret", PILOT_2).await;

    // Both pilots in same solar system for system/bubble broadcasts
    sess1.solar_system_id = Some(30_000_142);
    sess2.solar_system_id = Some(30_000_142);

    // 1. Create fleet with PILOT_1 as Boss
    let fleet_rep = rpc(&ctx, &mut sess1, "fleetMgr", "CreateFleet", &[])
        .await
        .expect("CreateFleet");
    let fleet_id = match fleet_rep {
        PyRep::KeyVal(kv) => kv.get("fleetID").and_then(|v| v.as_i64()).expect("fleetID"),
        PyRep::Integer(id) => id,
        other => panic!("unexpected CreateFleet response: {other:?}"),
    };
    sess1.fleet_id = Some(fleet_id);

    // 2. Invite and join PILOT_2
    let invite_args = vec![
        PyRep::Integer(PILOT_2),
        PyRep::Integer(1), // Wing
        PyRep::Integer(1), // Squad
        PyRep::Integer(4), // Member
    ];
    rpc(&ctx, &mut sess1, "fleetMgr", "Invite", &invite_args)
        .await
        .expect("Invite");
    rpc(&ctx, &mut sess2, "fleetMgr", "AcceptInvite", &[PyRep::Integer(fleet_id)])
        .await
        .expect("AcceptInvite");
    sess2.fleet_id = Some(fleet_id);

    // 3. Register fleet advert: fleetProxy.AddFleetFinderAdvert
    let ad_args = vec![
        PyRep::String("Jita Standing Defense".into()),
        PyRep::String("Join for gate and mission security".into()),
        PyRep::Integer(3), // public
        PyRep::Bool(true),
        PyRep::Real(-10.0),
        PyRep::Real(-10.0),
    ];
    rpc(&ctx, &mut sess1, "fleetProxy", "AddFleetFinderAdvert", &ad_args)
        .await
        .expect("AddFleetFinderAdvert");

    // 4. fleetProxy.GetMyFleetFinderAdvert: queries advert for caller's fleet
    let my_ad = rpc(&ctx, &mut sess1, "fleetProxy", "GetMyFleetFinderAdvert", &[])
        .await
        .expect("GetMyFleetFinderAdvert");
    match my_ad {
        PyRep::Rowset { rows, columns, .. } => {
            assert_eq!(rows.len(), 1);
            let fid_idx = columns.iter().position(|h| h == "fleetID").expect("fleetID col");
            let name_idx = columns.iter().position(|h| h == "fleetName").expect("fleetName col");
            let lead_idx = columns.iter().position(|h| h == "leaderID").expect("leaderID col");
            assert_eq!(rows[0][fid_idx].as_i64(), Some(fleet_id));
            assert_eq!(rows[0][name_idx].as_str(), Some("Jita Standing Defense"));
            assert_eq!(rows[0][lead_idx].as_i64(), Some(PILOT_1));
        }
        other => panic!("expected Rowset from GetMyFleetFinderAdvert, got {other:?}"),
    }

    // 5. fleetProxy.UpdateAdvertInfo: updates name and description
    let update_info_args = vec![
        PyRep::String("Jita Incursion Wing".into()),
        PyRep::String("Armor fleet forming".into()),
        PyRep::Integer(2),
    ];
    let updated_info = rpc(&ctx, &mut sess1, "fleetProxy", "UpdateAdvertInfo", &update_info_args)
        .await
        .expect("UpdateAdvertInfo");
    assert_eq!(field(&updated_info, "fleetName").as_str(), Some("Jita Incursion Wing"));

    // Verify change is persisted in advert query
    let my_ad_v2 = rpc(&ctx, &mut sess1, "fleetProxy", "GetMyFleetFinderAdvert", &[])
        .await
        .expect("GetMyFleetFinderAdvert v2");
    match my_ad_v2 {
        PyRep::Rowset { rows, columns, .. } => {
            let name_idx = columns.iter().position(|h| h == "fleetName").unwrap();
            assert_eq!(rows[0][name_idx].as_str(), Some("Jita Incursion Wing"));
        }
        other => panic!("expected Rowset from GetMyFleetFinderAdvert, got {other:?}"),
    }

    // 6. fleetProxy.UpdateFleetAdvertWithNewLeader: updates leader in advert
    let update_lead_rep = rpc(
        &ctx,
        &mut sess1,
        "fleetProxy",
        "UpdateFleetAdvertWithNewLeader",
        &[PyRep::Integer(PILOT_2)],
    )
    .await
    .expect("UpdateFleetAdvertWithNewLeader");
    assert_eq!(field(&update_lead_rep, "leaderID").as_i64(), Some(PILOT_2));

    // Verify leader changed in query
    let my_ad_v3 = rpc(&ctx, &mut sess1, "fleetProxy", "GetMyFleetFinderAdvert", &[])
        .await
        .expect("GetMyFleetFinderAdvert v3");
    match my_ad_v3 {
        PyRep::Rowset { rows, columns, .. } => {
            let lead_idx = columns.iter().position(|h| h == "leaderID").unwrap();
            assert_eq!(rows[0][lead_idx].as_i64(), Some(PILOT_2));
        }
        other => panic!("expected Rowset from GetMyFleetFinderAdvert, got {other:?}"),
    }

    // 7. fleetMgr.BroadcastToBubble: broadcasts tactical alert to local bubble
    let bubble_args = vec![
        PyRep::String("Target".into()),
        PyRep::Integer(0),    // all members
        PyRep::Integer(1001), // itemID
        PyRep::Integer(601),  // typeID
    ];
    let bubble_rep = rpc(&ctx, &mut sess1, "fleetMgr", "BroadcastToBubble", &bubble_args)
        .await
        .expect("BroadcastToBubble");
    assert_eq!(field(&bubble_rep, "broadcast").as_str(), Some("Target"));

    // 8. fleetMgr.BroadcastToSystem: broadcasts tactical alert to entire system
    let system_args = vec![
        PyRep::String("HealShield".into()),
        PyRep::Integer(0),
        PyRep::Integer(PILOT_1),
        PyRep::Integer(587),
    ];
    let system_rep = rpc(&ctx, &mut sess1, "fleetMgr", "BroadcastToSystem", &system_args)
        .await
        .expect("BroadcastToSystem");
    assert_eq!(field(&system_rep, "broadcast").as_str(), Some("HealShield"));

    // 9. fleetMgr.ForceLeaveFleet: boss kicks or forces member to leave fleet
    let force_rep = rpc(&ctx, &mut sess1, "fleetMgr", "ForceLeaveFleet", &[PyRep::Integer(PILOT_2)])
        .await
        .expect("ForceLeaveFleet");
    assert_eq!(field(&force_rep, "kickedCharacterID").as_i64(), Some(PILOT_2));

    // Verify PILOT_2 is no longer in fleet
    ctx.db.with_snapshot(|snap| {
        let fleet = snap.fleets.get(&fleet_id).expect("fleet exists");
        assert!(!fleet.member_details.contains_key(&PILOT_2));
        assert_eq!(fleet.members.len(), 1);
    });

    // 10. fleetProxy.RemoveFleetFinderAdvert: removes advert and deregisters fleet
    let remove_rep = rpc(&ctx, &mut sess1, "fleetProxy", "RemoveFleetFinderAdvert", &[])
        .await
        .expect("RemoveFleetFinderAdvert");
    assert_eq!(field(&remove_rep, "removed").as_bool(), Some(true));

    // Verify advert is removed
    let my_ad_empty = rpc(&ctx, &mut sess1, "fleetProxy", "GetMyFleetFinderAdvert", &[])
        .await
        .expect("GetMyFleetFinderAdvert empty");
    match my_ad_empty {
        PyRep::Rowset { rows, .. } => assert!(rows.is_empty()),
        other => panic!("expected empty Rowset from GetMyFleetFinderAdvert, got {other:?}"),
    }
}
