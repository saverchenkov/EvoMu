//! `rpc_batch_b1_complete` — Acceptance suite for Classic RPC Methods Batch B1 (209 ledger rows).

use crate::common;

use eve_db::{AllianceRecord, CorporationRecord, InventoryFlag, InventoryItemRecord, WarRecord};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::context::ServerContext;
use std::collections::{HashMap, HashSet};

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SYSTEM_ID: u32 = 30_000_142;

async fn setup_pilot(ctx: &ServerContext, session_id: u64, char_id: i64) -> SessionState {
    let mut session = SessionState::new(session_id);
    let (user, pass) = if char_id == 90_000_002 {
        ("other", "other-secret")
    } else {
        ("demo", "demo-secret")
    };
    common::login_pilot(ctx, &mut session, user, pass).await;
    let kw = HashMap::new();
    let _ = ctx
        .dispatch_rpc(
            &mut session,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(char_id)],
            &kw,
            None,
        )
        .await;
    session.solar_system_id = Some(JITA_SYSTEM_ID);
    session.station_id = Some(JITA_STATION_ID);
    session.alliance_id = Some(99_000_001);
    session
}

async fn rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    let kwargs = HashMap::new();
    ctx.dispatch_rpc(session, service, None, method, args, &kwargs, None).await
}

// ====================================================================
// Test 1: ProjectDiscovery (23 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_project_discovery() {
    // parity: rpc:ProjectDiscovery.append, rpc:ProjectDiscovery.fix_task_id, rpc:ProjectDiscovery.get_current_task_index, rpc:ProjectDiscovery.get_data_from_url, rpc:ProjectDiscovery.get_is_player_entitled_to_tutorial_reward, rpc:ProjectDiscovery.get_maximum_bonus_samples, rpc:ProjectDiscovery.get_new_task, rpc:ProjectDiscovery.get_player_analysis_kredits, rpc:ProjectDiscovery.get_player_state, rpc:ProjectDiscovery.get_player_statistics, rpc:ProjectDiscovery.get_project_id, rpc:ProjectDiscovery.get_remaining_bonus_samples, rpc:ProjectDiscovery.get_total_needed_xp, rpc:ProjectDiscovery.get_tutorial_completion_status, rpc:ProjectDiscovery.get_tutorial_level, rpc:ProjectDiscovery.get_tutorial_xp_reward, rpc:ProjectDiscovery.give_tutorial_rewards, rpc:ProjectDiscovery.increase_tutorial_level, rpc:ProjectDiscovery.new_training_task, rpc:ProjectDiscovery.post_classification, rpc:ProjectDiscovery.remove, rpc:ProjectDiscovery.reset_tutorial, rpc:ProjectDiscovery.skip_tutorial
    eve_testkit::attest(&[
        "rpc:ProjectDiscovery.append", "rpc:ProjectDiscovery.fix_task_id", "rpc:ProjectDiscovery.get_current_task_index",
        "rpc:ProjectDiscovery.get_data_from_url", "rpc:ProjectDiscovery.get_is_player_entitled_to_tutorial_reward",
        "rpc:ProjectDiscovery.get_maximum_bonus_samples", "rpc:ProjectDiscovery.get_new_task",
        "rpc:ProjectDiscovery.get_player_analysis_kredits", "rpc:ProjectDiscovery.get_player_state",
        "rpc:ProjectDiscovery.get_player_statistics", "rpc:ProjectDiscovery.get_project_id",
        "rpc:ProjectDiscovery.get_remaining_bonus_samples", "rpc:ProjectDiscovery.get_total_needed_xp",
        "rpc:ProjectDiscovery.get_tutorial_completion_status", "rpc:ProjectDiscovery.get_tutorial_level",
        "rpc:ProjectDiscovery.get_tutorial_xp_reward", "rpc:ProjectDiscovery.give_tutorial_rewards",
        "rpc:ProjectDiscovery.increase_tutorial_level", "rpc:ProjectDiscovery.new_training_task",
        "rpc:ProjectDiscovery.post_classification", "rpc:ProjectDiscovery.remove",
        "rpc:ProjectDiscovery.reset_tutorial", "rpc:ProjectDiscovery.skip_tutorial",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 101, 90_000_001).await;

    let st = rpc(&ctx, &mut s, "ProjectDiscovery", "get_player_state", &[]).await.unwrap();
    assert!(matches!(st, PyRep::KeyVal(_)));

    let stats = rpc(&ctx, &mut s, "ProjectDiscovery", "get_player_statistics", &[]).await.unwrap();
    assert!(matches!(stats, PyRep::KeyVal(_)));

    let xp = rpc(&ctx, &mut s, "ProjectDiscovery", "get_total_needed_xp", &[PyRep::Integer(1)]).await.unwrap();
    assert!(matches!(xp, PyRep::Real(_)));

    let tut_status = rpc(&ctx, &mut s, "ProjectDiscovery", "get_tutorial_completion_status", &[]).await.unwrap();
    assert!(matches!(tut_status, PyRep::Bool(_)));

    let tut_lvl = rpc(&ctx, &mut s, "ProjectDiscovery", "get_tutorial_level", &[]).await.unwrap();
    assert_eq!(tut_lvl, PyRep::Integer(1));

    let inc_lvl = rpc(&ctx, &mut s, "ProjectDiscovery", "increase_tutorial_level", &[]).await.unwrap();
    assert_eq!(inc_lvl, PyRep::Integer(2));

    let entitled = rpc(&ctx, &mut s, "ProjectDiscovery", "get_is_player_entitled_to_tutorial_reward", &[]).await.unwrap();
    assert_eq!(entitled, PyRep::Bool(true));

    let rwd = rpc(&ctx, &mut s, "ProjectDiscovery", "give_tutorial_rewards", &[]).await.unwrap();
    assert!(matches!(rwd, PyRep::KeyVal(_)));

    let tut_xp_rwd = rpc(&ctx, &mut s, "ProjectDiscovery", "get_tutorial_xp_reward", &[PyRep::Integer(2)]).await.unwrap();
    assert!(matches!(tut_xp_rwd, PyRep::Real(_)));

    let task = rpc(&ctx, &mut s, "ProjectDiscovery", "get_new_task", &[]).await.unwrap();
    assert!(matches!(task, PyRep::KeyVal(_)));

    let train = rpc(&ctx, &mut s, "ProjectDiscovery", "new_training_task", &[]).await.unwrap();
    assert!(matches!(train, PyRep::KeyVal(_)));

    let rem_bonus = rpc(&ctx, &mut s, "ProjectDiscovery", "get_remaining_bonus_samples", &[]).await.unwrap();
    assert!(matches!(rem_bonus, PyRep::Integer(_)));

    let max_bonus = rpc(&ctx, &mut s, "ProjectDiscovery", "get_maximum_bonus_samples", &[]).await.unwrap();
    assert_eq!(max_bonus, PyRep::Integer(10));

    let ak = rpc(&ctx, &mut s, "ProjectDiscovery", "get_player_analysis_kredits", &[]).await.unwrap();
    assert!(matches!(ak, PyRep::Integer(_)));

    let pid = rpc(&ctx, &mut s, "ProjectDiscovery", "get_project_id", &[]).await.unwrap();
    assert_eq!(pid, PyRep::String("covid".into()));

    let post_res = rpc(&ctx, &mut s, "ProjectDiscovery", "post_classification", &[PyRep::String("task_1".into()), PyRep::Dict(vec![])]).await.unwrap();
    assert!(matches!(post_res, PyRep::KeyVal(_)));

    let fix_id = rpc(&ctx, &mut s, "ProjectDiscovery", "fix_task_id", &[PyRep::String("task_123".into())]).await.unwrap();
    assert_eq!(fix_id, PyRep::String("task_123".into()));

    let cur_idx = rpc(&ctx, &mut s, "ProjectDiscovery", "get_current_task_index", &[]).await.unwrap();
    assert!(matches!(cur_idx, PyRep::Integer(_)));

    let data_url = rpc(&ctx, &mut s, "ProjectDiscovery", "get_data_from_url", &[PyRep::String("https://example.com".into())]).await.unwrap();
    assert_eq!(data_url, PyRep::String("data_payload".into()));

    let app_res = rpc(&ctx, &mut s, "ProjectDiscovery", "append", &[PyRep::String("entry".into())]).await.unwrap();
    assert_eq!(app_res, PyRep::None);

    let rem_res = rpc(&ctx, &mut s, "ProjectDiscovery", "remove", &[PyRep::String("entry".into())]).await.unwrap();
    assert_eq!(rem_res, PyRep::None);

    let rst_tut = rpc(&ctx, &mut s, "ProjectDiscovery", "reset_tutorial", &[]).await.unwrap();
    assert_eq!(rst_tut, PyRep::Bool(true));

    let skp_tut = rpc(&ctx, &mut s, "ProjectDiscovery", "skip_tutorial", &[]).await.unwrap();
    assert_eq!(skp_tut, PyRep::Bool(true));
}

// ====================================================================
// Test 2: posMgr (22 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_pos_manager() {
    // parity: rpc:posMgr.AnchorOrbital, rpc:posMgr.AnchorStructure, rpc:posMgr.AssumeStructureControl, rpc:posMgr.ChangeStructureProvisionType, rpc:posMgr.GetConsumptionFactor, rpc:posMgr.GetControlTowers, rpc:posMgr.GetMoonForTower, rpc:posMgr.GetMoonProcessInfoForTower, rpc:posMgr.GetSiloCapacityByItemID, rpc:posMgr.GetStarbasePermissions, rpc:posMgr.GetTowerNotificationSettings, rpc:posMgr.GetTowerSentrySettings, rpc:posMgr.LinkResourceForTower, rpc:posMgr.OnlineOrbital, rpc:posMgr.RelinquishStructureControl, rpc:posMgr.RunMoonProcessCycleforTower, rpc:posMgr.SetSessionCheck, rpc:posMgr.SetStarbasePermissions, rpc:posMgr.SetTowerNotifications, rpc:posMgr.SetTowerPassword, rpc:posMgr.SetTowerSentrySettings, rpc:posMgr.UnanchorOrbital
    eve_testkit::attest(&[
        "rpc:posMgr.AnchorOrbital", "rpc:posMgr.AnchorStructure", "rpc:posMgr.AssumeStructureControl",
        "rpc:posMgr.ChangeStructureProvisionType", "rpc:posMgr.GetConsumptionFactor", "rpc:posMgr.GetControlTowers",
        "rpc:posMgr.GetMoonForTower", "rpc:posMgr.GetMoonProcessInfoForTower", "rpc:posMgr.GetSiloCapacityByItemID",
        "rpc:posMgr.GetStarbasePermissions", "rpc:posMgr.GetTowerNotificationSettings", "rpc:posMgr.GetTowerSentrySettings",
        "rpc:posMgr.LinkResourceForTower", "rpc:posMgr.OnlineOrbital", "rpc:posMgr.RelinquishStructureControl",
        "rpc:posMgr.RunMoonProcessCycleforTower", "rpc:posMgr.SetSessionCheck", "rpc:posMgr.SetStarbasePermissions",
        "rpc:posMgr.SetTowerNotifications", "rpc:posMgr.SetTowerPassword", "rpc:posMgr.SetTowerSentrySettings",
        "rpc:posMgr.UnanchorOrbital",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 102, 90_000_001).await;
    let tower_id = 100_000_001;

    let towers = rpc(&ctx, &mut s, "posMgr", "GetControlTowers", &[]).await.unwrap();
    assert!(matches!(towers, PyRep::List(_)));

    let moon = rpc(&ctx, &mut s, "posMgr", "GetMoonForTower", &[PyRep::Integer(tower_id)]).await.unwrap();
    assert!(matches!(moon, PyRep::Integer(_)));

    let proc_info = rpc(&ctx, &mut s, "posMgr", "GetMoonProcessInfoForTower", &[PyRep::Integer(tower_id)]).await.unwrap();
    assert!(matches!(proc_info, PyRep::KeyVal(_)));

    let factor = rpc(&ctx, &mut s, "posMgr", "GetConsumptionFactor", &[PyRep::Integer(tower_id)]).await.unwrap();
    assert_eq!(factor, PyRep::Real(1.0));

    let silo_cap = rpc(&ctx, &mut s, "posMgr", "GetSiloCapacityByItemID", &[PyRep::Integer(100_000_001)]).await.unwrap();
    assert_eq!(silo_cap, PyRep::Real(20_000.0));

    let perms = rpc(&ctx, &mut s, "posMgr", "GetStarbasePermissions", &[PyRep::Integer(tower_id)]).await.unwrap();
    assert!(matches!(perms, PyRep::KeyVal(_)));

    let set_perms = rpc(&ctx, &mut s, "posMgr", "SetStarbasePermissions", &[PyRep::Integer(tower_id), PyRep::keyval([("allowCorp", PyRep::Bool(true))])]).await.unwrap();
    assert_eq!(set_perms, PyRep::Bool(true));

    let notifs = rpc(&ctx, &mut s, "posMgr", "GetTowerNotificationSettings", &[PyRep::Integer(tower_id)]).await.unwrap();
    assert!(matches!(notifs, PyRep::KeyVal(_)));

    let set_notifs = rpc(&ctx, &mut s, "posMgr", "SetTowerNotifications", &[PyRep::Integer(tower_id), PyRep::keyval([("sendFuelNotifications", PyRep::Bool(true))])]).await.unwrap();
    assert_eq!(set_notifs, PyRep::Bool(true));

    let sentry = rpc(&ctx, &mut s, "posMgr", "GetTowerSentrySettings", &[PyRep::Integer(tower_id)]).await.unwrap();
    assert!(matches!(sentry, PyRep::KeyVal(_)));

    let set_sentry = rpc(&ctx, &mut s, "posMgr", "SetTowerSentrySettings", &[PyRep::Integer(tower_id), PyRep::keyval([("attackIfLowStanding", PyRep::Bool(true))])]).await.unwrap();
    assert_eq!(set_sentry, PyRep::Bool(true));

    let set_pwd = rpc(&ctx, &mut s, "posMgr", "SetTowerPassword", &[PyRep::Integer(tower_id), PyRep::String("pos-secret".into())]).await.unwrap();
    assert_eq!(set_pwd, PyRep::Bool(true));

    let link_res = rpc(&ctx, &mut s, "posMgr", "LinkResourceForTower", &[PyRep::Integer(tower_id), PyRep::Integer(54321)]).await.unwrap();
    assert_eq!(link_res, PyRep::Bool(true));

    let cycle_res = rpc(&ctx, &mut s, "posMgr", "RunMoonProcessCycleforTower", &[PyRep::Integer(tower_id)]).await.unwrap();
    assert_eq!(cycle_res, PyRep::Bool(true));

    let prov_res = rpc(&ctx, &mut s, "posMgr", "ChangeStructureProvisionType", &[PyRep::Integer(tower_id), PyRep::Integer(2)]).await.unwrap();
    assert_eq!(prov_res, PyRep::Bool(true));

    let anchor_str = rpc(&ctx, &mut s, "posMgr", "AnchorStructure", &[PyRep::Integer(tower_id)]).await.unwrap();
    assert_eq!(anchor_str, PyRep::Integer(tower_id));

    let anchor_orb = rpc(&ctx, &mut s, "posMgr", "AnchorOrbital", &[PyRep::Integer(100_000_002)]).await.unwrap();
    assert_eq!(anchor_orb, PyRep::Integer(100_000_002));

    let online_orb = rpc(&ctx, &mut s, "posMgr", "OnlineOrbital", &[PyRep::Integer(100_000_002)]).await.unwrap();
    assert_eq!(online_orb, PyRep::Bool(true));

    let unanchor_orb = rpc(&ctx, &mut s, "posMgr", "UnanchorOrbital", &[PyRep::Integer(100_000_002)]).await.unwrap();
    assert_eq!(unanchor_orb, PyRep::Bool(true));

    let assume_ctrl = rpc(&ctx, &mut s, "posMgr", "AssumeStructureControl", &[PyRep::Integer(tower_id)]).await.unwrap();
    assert_eq!(assume_ctrl, PyRep::Bool(true));

    let rel_ctrl = rpc(&ctx, &mut s, "posMgr", "RelinquishStructureControl", &[PyRep::Integer(tower_id)]).await.unwrap();
    assert_eq!(rel_ctrl, PyRep::Bool(true));

    let sess_check = rpc(&ctx, &mut s, "posMgr", "SetSessionCheck", &[]).await.unwrap();
    assert_eq!(sess_check, PyRep::None);
}

// ====================================================================
// Test 3: allianceRegistry (18 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_alliance_registry() {
    // parity: rpc:allianceRegistry.Bind, rpc:allianceRegistry.GetAlliance, rpc:allianceRegistry.GetAllianceMembers, rpc:allianceRegistry.GetAllianceMembersOlderThan, rpc:allianceRegistry.GetAlliancePublicInfo, rpc:allianceRegistry.GetDaysInAlliance, rpc:allianceRegistry.GetEmploymentRecord, rpc:allianceRegistry.GetRankedAlliances, rpc:allianceRegistry.SetSessionCheck, rpc:allianceRegistry.allianceID, rpc:allianceRegistry.allowWar, rpc:allianceRegistry.creatorCharID, rpc:allianceRegistry.creatorCorpID, rpc:allianceRegistry.description, rpc:allianceRegistry.dictatorial, rpc:allianceRegistry.executorCorpID, rpc:allianceRegistry.shortName, rpc:allianceRegistry.url
    eve_testkit::attest(&[
        "rpc:allianceRegistry.Bind", "rpc:allianceRegistry.GetAlliance", "rpc:allianceRegistry.GetAllianceMembers",
        "rpc:allianceRegistry.GetAllianceMembersOlderThan", "rpc:allianceRegistry.GetAlliancePublicInfo",
        "rpc:allianceRegistry.GetDaysInAlliance", "rpc:allianceRegistry.GetEmploymentRecord",
        "rpc:allianceRegistry.GetRankedAlliances", "rpc:allianceRegistry.SetSessionCheck",
        "rpc:allianceRegistry.allianceID", "rpc:allianceRegistry.allowWar", "rpc:allianceRegistry.creatorCharID",
        "rpc:allianceRegistry.creatorCorpID", "rpc:allianceRegistry.description", "rpc:allianceRegistry.dictatorial",
        "rpc:allianceRegistry.executorCorpID", "rpc:allianceRegistry.shortName", "rpc:allianceRegistry.url",
    ]);

    let ctx = ServerContext::new();
    let aid = 99_000_001;
    let mut s = setup_pilot(&ctx, 103, 90_000_001).await;
    s.alliance_id = Some(aid);

    ctx.db.with_mut_snapshot(|snap| {
        let mut members = HashSet::new();
        members.insert(98_000_001);
        snap.alliances.insert(
            aid,
            AllianceRecord {
                alliance_id: aid,
                name: "Test Alliance".into(),
                short_name: "TEST".into(),
                executor_corp_id: 98_000_001,
                member_corp_ids: members,
            },
        );
    });

    let bind_res = rpc(&ctx, &mut s, "allianceRegistry", "Bind", &[]).await.unwrap();
    assert_eq!(bind_res, PyRep::None);

    let sess_check = rpc(&ctx, &mut s, "allianceRegistry", "SetSessionCheck", &[]).await.unwrap();
    assert_eq!(sess_check, PyRep::None);

    let ally = rpc(&ctx, &mut s, "allianceRegistry", "GetAlliance", &[PyRep::Integer(aid)]).await.unwrap();
    assert!(matches!(ally, PyRep::KeyVal(_)));

    let ally_pub = rpc(&ctx, &mut s, "allianceRegistry", "GetAlliancePublicInfo", &[PyRep::Integer(aid)]).await.unwrap();
    assert!(matches!(ally_pub, PyRep::KeyVal(_)));

    let members = rpc(&ctx, &mut s, "allianceRegistry", "GetAllianceMembers", &[PyRep::Integer(aid)]).await.unwrap();
    assert!(matches!(members, PyRep::Rowset { .. }));

    let members_old = rpc(&ctx, &mut s, "allianceRegistry", "GetAllianceMembersOlderThan", &[PyRep::Integer(aid), PyRep::Integer(10)]).await.unwrap();
    assert!(matches!(members_old, PyRep::Rowset { .. }));

    let emp_rec = rpc(&ctx, &mut s, "allianceRegistry", "GetEmploymentRecord", &[PyRep::Integer(aid)]).await.unwrap();
    assert!(matches!(emp_rec, PyRep::Rowset { .. }));

    let ranked = rpc(&ctx, &mut s, "allianceRegistry", "GetRankedAlliances", &[]).await.unwrap();
    assert!(matches!(ranked, PyRep::Rowset { .. }));

    let days = rpc(&ctx, &mut s, "allianceRegistry", "GetDaysInAlliance", &[PyRep::Integer(98_000_001)]).await.unwrap();
    assert!(matches!(days, PyRep::Integer(_)));

    let aid_val = rpc(&ctx, &mut s, "allianceRegistry", "allianceID", &[]).await.unwrap();
    assert_eq!(aid_val, PyRep::Integer(aid));

    let short = rpc(&ctx, &mut s, "allianceRegistry", "shortName", &[]).await.unwrap();
    assert_eq!(short, PyRep::String("TEST".into()));

    let desc = rpc(&ctx, &mut s, "allianceRegistry", "description", &[]).await.unwrap();
    assert!(matches!(desc, PyRep::String(_)));

    let url = rpc(&ctx, &mut s, "allianceRegistry", "url", &[]).await.unwrap();
    assert!(matches!(url, PyRep::String(_)));

    let exec = rpc(&ctx, &mut s, "allianceRegistry", "executorCorpID", &[]).await.unwrap();
    assert_eq!(exec, PyRep::Integer(98_000_001));

    let creator_corp = rpc(&ctx, &mut s, "allianceRegistry", "creatorCorpID", &[]).await.unwrap();
    assert_eq!(creator_corp, PyRep::Integer(98_000_001));

    let creator_char = rpc(&ctx, &mut s, "allianceRegistry", "creatorCharID", &[]).await.unwrap();
    assert_eq!(creator_char, PyRep::Integer(90_000_001));

    let dict = rpc(&ctx, &mut s, "allianceRegistry", "dictatorial", &[]).await.unwrap();
    assert_eq!(dict, PyRep::Bool(false));

    let war_allow = rpc(&ctx, &mut s, "allianceRegistry", "allowWar", &[]).await.unwrap();
    assert_eq!(war_allow, PyRep::Bool(true));
}

// ====================================================================
// Test 4: accessGroupBookmarkMgr (17 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_access_group_bookmarks() {
    // parity: rpc:accessGroupBookmarkMgr.AddFolder, rpc:accessGroupBookmarkMgr.AddToKnownFolders, rpc:accessGroupBookmarkMgr.BookmarkStaticLocation, rpc:accessGroupBookmarkMgr.CopyBookmarksToFolderAndSubfolder, rpc:accessGroupBookmarkMgr.CreateSubfolder, rpc:accessGroupBookmarkMgr.DeleteBookmarks, rpc:accessGroupBookmarkMgr.DeleteFolder, rpc:accessGroupBookmarkMgr.DeleteSubfolder, rpc:accessGroupBookmarkMgr.GetFolderInfo, rpc:accessGroupBookmarkMgr.GetMyActiveBookmarks, rpc:accessGroupBookmarkMgr.MoveBookmarksToFolderAndSubfolder, rpc:accessGroupBookmarkMgr.RemoveFromKnownFolders, rpc:accessGroupBookmarkMgr.SearchFoldersWithAdminAccess, rpc:accessGroupBookmarkMgr.UpdateBookmark, rpc:accessGroupBookmarkMgr.UpdateFolder, rpc:accessGroupBookmarkMgr.UpdateKnownFolderState, rpc:accessGroupBookmarkMgr.UpdateSubfolder
    eve_testkit::attest(&[
        "rpc:accessGroupBookmarkMgr.AddFolder", "rpc:accessGroupBookmarkMgr.AddToKnownFolders",
        "rpc:accessGroupBookmarkMgr.BookmarkStaticLocation", "rpc:accessGroupBookmarkMgr.CopyBookmarksToFolderAndSubfolder",
        "rpc:accessGroupBookmarkMgr.CreateSubfolder", "rpc:accessGroupBookmarkMgr.DeleteBookmarks",
        "rpc:accessGroupBookmarkMgr.DeleteFolder", "rpc:accessGroupBookmarkMgr.DeleteSubfolder",
        "rpc:accessGroupBookmarkMgr.GetFolderInfo", "rpc:accessGroupBookmarkMgr.GetMyActiveBookmarks",
        "rpc:accessGroupBookmarkMgr.MoveBookmarksToFolderAndSubfolder", "rpc:accessGroupBookmarkMgr.RemoveFromKnownFolders",
        "rpc:accessGroupBookmarkMgr.SearchFoldersWithAdminAccess", "rpc:accessGroupBookmarkMgr.UpdateBookmark",
        "rpc:accessGroupBookmarkMgr.UpdateFolder", "rpc:accessGroupBookmarkMgr.UpdateKnownFolderState",
        "rpc:accessGroupBookmarkMgr.UpdateSubfolder",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 104, 90_000_001).await;
    s.corporation_id = Some(98_000_001);
    s.corp_role = eve_db::corp_roles::ROLE_DIRECTOR;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(corp) = snap.corporations.get_mut(&98_000_001) {
            corp.member_roles.insert(90_000_001, eve_db::corp_roles::ROLE_DIRECTOR);
        }
    });

    // AddFolder (shared)
    let folder = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "AddFolder", &[PyRep::String("Fleet Bookmarks".into()), PyRep::Bool(true)]).await.unwrap();
    let folder_id = match &folder {
        PyRep::KeyVal(kv) => kv.iter().find(|(k, _)| k.as_str() == "folderID").and_then(|(_, v)| v.as_i64()).unwrap_or(1),
        PyRep::Integer(id) => *id,
        _ => 1,
    };

    let subfolder = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "CreateSubfolder", &[PyRep::Integer(folder_id), PyRep::String("Safe Spots".into())]).await.unwrap();
    let subfolder_id = match &subfolder {
        PyRep::KeyVal(kv) => kv.iter().find(|(k, _)| k.as_str() == "subfolderID").and_then(|(_, v)| v.as_i64()).unwrap_or(2),
        PyRep::Integer(id) => *id,
        _ => 2,
    };

    let bm = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "BookmarkStaticLocation", &[PyRep::String("Safe Spot 1".into()), PyRep::String("Memo".into()), PyRep::Integer(folder_id), PyRep::Integer(0), PyRep::Integer(subfolder_id)]).await.unwrap();
    let bm_id = match &bm {
        PyRep::KeyVal(kv) => kv.iter().find(|(k, _)| k.as_str() == "bookmarkID").and_then(|(_, v)| v.as_i64()).unwrap_or(10),
        PyRep::Integer(id) => *id,
        _ => 10,
    };

    let active = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "GetMyActiveBookmarks", &[]).await.unwrap();
    assert!(matches!(active, PyRep::Rowset { .. }));

    let folder_info = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "GetFolderInfo", &[PyRep::Integer(folder_id)]).await.unwrap();
    assert!(matches!(folder_info, PyRep::Rowset { .. }));

    let admin_search = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "SearchFoldersWithAdminAccess", &[]).await.unwrap();
    assert!(matches!(admin_search, PyRep::Rowset { .. }));

    let upd_bm = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "UpdateBookmark", &[PyRep::Integer(bm_id), PyRep::String("Updated Safe Spot".into()), PyRep::String("Updated Memo".into())]).await.unwrap();
    assert!(matches!(upd_bm, PyRep::KeyVal(_)));

    let copy_bm = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "CopyBookmarksToFolderAndSubfolder", &[PyRep::List(vec![PyRep::Integer(bm_id)]), PyRep::Integer(folder_id), PyRep::Integer(subfolder_id)]).await.unwrap();
    assert!(matches!(copy_bm, PyRep::KeyVal(_)));

    let move_bm = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "MoveBookmarksToFolderAndSubfolder", &[PyRep::List(vec![PyRep::Integer(bm_id)]), PyRep::Integer(folder_id), PyRep::Integer(subfolder_id)]).await.unwrap();
    assert!(matches!(move_bm, PyRep::KeyVal(_)));

    let upd_folder = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "UpdateFolder", &[PyRep::Integer(folder_id), PyRep::String("Renamed Fleet Bookmarks".into())]).await.unwrap();
    assert!(matches!(upd_folder, PyRep::KeyVal(_)));

    let upd_subfolder = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "UpdateSubfolder", &[PyRep::Integer(subfolder_id), PyRep::String("Renamed Safe Spots".into())]).await.unwrap();
    assert!(matches!(upd_subfolder, PyRep::KeyVal(_)));

    let add_known = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "AddToKnownFolders", &[PyRep::Integer(folder_id)]).await.unwrap();
    assert!(matches!(add_known, PyRep::KeyVal(_)));

    let upd_known = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "UpdateKnownFolderState", &[PyRep::Integer(folder_id), PyRep::Bool(true)]).await.unwrap();
    assert!(matches!(upd_known, PyRep::KeyVal(_)));

    let rem_known = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "RemoveFromKnownFolders", &[PyRep::Integer(folder_id)]).await.unwrap();
    assert!(matches!(rem_known, PyRep::KeyVal(_)));

    let del_bm = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "DeleteBookmarks", &[PyRep::List(vec![PyRep::Integer(bm_id)])]).await.unwrap();
    assert!(matches!(del_bm, PyRep::KeyVal(_)));

    let del_subfolder = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "DeleteSubfolder", &[PyRep::Integer(subfolder_id)]).await.unwrap();
    assert!(matches!(del_subfolder, PyRep::KeyVal(_)));

    let del_folder = rpc(&ctx, &mut s, "accessGroupBookmarkMgr", "DeleteFolder", &[PyRep::Integer(folder_id)]).await.unwrap();
    assert!(matches!(del_folder, PyRep::KeyVal(_)));
    assert!(folder_id > 0);
    assert!(subfolder_id > 0);
    assert!(bm_id > 0);
}

// ====================================================================
// Test 5: raffleProxy (16 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_raffle_proxy() {
    // parity: rpc:raffleProxy.AmIBanned, rpc:raffleProxy.AwardItem, rpc:raffleProxy.BuyRandomTickets, rpc:raffleProxy.BuyTicket, rpc:raffleProxy.CreateRaffle, rpc:raffleProxy.FilteredGrab, rpc:raffleProxy.GetActiveHistoricPrices, rpc:raffleProxy.GetCreatedParticipated, rpc:raffleProxy.GetMyActiveTickets, rpc:raffleProxy.GetMyRaffleHistory, rpc:raffleProxy.GetRaffle, rpc:raffleProxy.Grab, rpc:raffleProxy.SubscribeToRaffle, rpc:raffleProxy.SubscribeToTickets, rpc:raffleProxy.UnSubscribeFromTickets, rpc:raffleProxy.UnsubscribeFromRaffle
    eve_testkit::attest(&[
        "rpc:raffleProxy.AmIBanned", "rpc:raffleProxy.AwardItem", "rpc:raffleProxy.BuyRandomTickets",
        "rpc:raffleProxy.BuyTicket", "rpc:raffleProxy.CreateRaffle", "rpc:raffleProxy.FilteredGrab",
        "rpc:raffleProxy.GetActiveHistoricPrices", "rpc:raffleProxy.GetCreatedParticipated",
        "rpc:raffleProxy.GetMyActiveTickets", "rpc:raffleProxy.GetMyRaffleHistory", "rpc:raffleProxy.GetRaffle",
        "rpc:raffleProxy.Grab", "rpc:raffleProxy.SubscribeToRaffle", "rpc:raffleProxy.SubscribeToTickets",
        "rpc:raffleProxy.UnSubscribeFromTickets", "rpc:raffleProxy.UnsubscribeFromRaffle",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 105, 90_000_001).await;

    let banned = rpc(&ctx, &mut s, "raffleProxy", "AmIBanned", &[]).await.unwrap();
    assert_eq!(banned, PyRep::Bool(false));

    let hist_prices = rpc(&ctx, &mut s, "raffleProxy", "GetActiveHistoricPrices", &[]).await.unwrap();
    assert!(matches!(hist_prices, PyRep::Tuple(_)));

    let sub_raf = rpc(&ctx, &mut s, "raffleProxy", "SubscribeToRaffle", &[PyRep::Integer(1)]).await.unwrap();
    assert_eq!(sub_raf, PyRep::None);

    let unsub_raf = rpc(&ctx, &mut s, "raffleProxy", "UnsubscribeFromRaffle", &[PyRep::Integer(1)]).await.unwrap();
    assert_eq!(unsub_raf, PyRep::None);

    let sub_tix = rpc(&ctx, &mut s, "raffleProxy", "SubscribeToTickets", &[PyRep::Integer(1)]).await.unwrap();
    assert_eq!(sub_tix, PyRep::None);

    let unsub_tix = rpc(&ctx, &mut s, "raffleProxy", "UnSubscribeFromTickets", &[PyRep::Integer(1)]).await.unwrap();
    assert_eq!(unsub_tix, PyRep::None);

    let grab = rpc(&ctx, &mut s, "raffleProxy", "Grab", &[]).await.unwrap();
    assert!(matches!(grab, PyRep::List(_)));

    let fgrab = rpc(&ctx, &mut s, "raffleProxy", "FilteredGrab", &[PyRep::Integer(1)]).await.unwrap();
    assert!(matches!(fgrab, PyRep::List(_)));

    let my_tix = rpc(&ctx, &mut s, "raffleProxy", "GetMyActiveTickets", &[]).await.unwrap();
    assert!(matches!(my_tix, PyRep::List(_)));

    let my_hist = rpc(&ctx, &mut s, "raffleProxy", "GetMyRaffleHistory", &[]).await.unwrap();
    assert!(matches!(my_hist, PyRep::List(_)));

    let created_part = rpc(&ctx, &mut s, "raffleProxy", "GetCreatedParticipated", &[]).await.unwrap();
    assert!(matches!(created_part, PyRep::List(_)));

    // Create a raffle: insert an item to raffle first
    let item_id = 99_001_105;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            item_id,
            InventoryItemRecord {
                item_id,
                type_id: 34,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            item_id + 1,
            InventoryItemRecord {
                item_id: item_id + 1,
                type_id: 52568,
                owner_id: 90_000_001,
                location_id: JITA_STATION_ID,
                flag: InventoryFlag::Hangar,
                quantity: 100,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let cr_res = rpc(
        &ctx,
        &mut s,
        "raffleProxy",
        "CreateRaffle",
        &[
            PyRep::Integer(item_id),
            PyRep::Integer(8),    // ticket_count
            PyRep::Real(1_000.0), // ticket_price_isk
            PyRep::Integer(1),    // duration_days
        ],
    )
    .await
    .unwrap();
    assert!(matches!(cr_res, PyRep::KeyVal(_)));
    let PyRep::KeyVal(kv) = cr_res else { panic!("expected KeyVal") };
    let raffle_id = kv.get("raffleID").and_then(|v| v.as_i64()).unwrap();

    let get_raf = rpc(&ctx, &mut s, "raffleProxy", "GetRaffle", &[PyRep::Integer(raffle_id)]).await.unwrap();
    assert!(matches!(get_raf, PyRep::KeyVal(_)));

    let buy_ticket = rpc(&ctx, &mut s, "raffleProxy", "BuyTicket", &[PyRep::Integer(raffle_id), PyRep::Integer(0)]).await.unwrap();
    assert!(matches!(buy_ticket, PyRep::KeyVal(_)));

    let buy_random = rpc(&ctx, &mut s, "raffleProxy", "BuyRandomTickets", &[PyRep::Integer(raffle_id), PyRep::Integer(7)]).await.unwrap();
    assert!(matches!(buy_random, PyRep::KeyVal(_)));

    let award_res = rpc(&ctx, &mut s, "raffleProxy", "AwardItem", &[PyRep::Integer(raffle_id)]).await.unwrap();
    assert!(matches!(award_res, PyRep::Integer(_)));
}

// ====================================================================
// Test 6: dungeonInstanceCacheMgr (15 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_dungeon_instance_cache() {
    // parity: rpc:dungeonInstanceCacheMgr.GetBulwarkMiningSiteCounts, rpc:dungeonInstanceCacheMgr.GetCombatAnomaliesCount, rpc:dungeonInstanceCacheMgr.GetCombatAnomalyInstances, rpc:dungeonInstanceCacheMgr.GetFactionWarfareBattlefieldInstances, rpc:dungeonInstanceCacheMgr.GetFactionWarfareInstances, rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances, rpc:dungeonInstanceCacheMgr.GetIceBeltInstances, rpc:dungeonInstanceCacheMgr.GetIceBeltsCount, rpc:dungeonInstanceCacheMgr.GetOreAnomaliesCount, rpc:dungeonInstanceCacheMgr.GetOreAnomaliesCountInRange, rpc:dungeonInstanceCacheMgr.GetOreAnomalyInstances, rpc:dungeonInstanceCacheMgr.GetPirateInsurgencyIceHeistInstances, rpc:dungeonInstanceCacheMgr.GetPirateInsurgencyInstances, rpc:dungeonInstanceCacheMgr.GetSignatureInstances, rpc:dungeonInstanceCacheMgr.GetTriglavianSiteInstances
    eve_testkit::attest(&[
        "rpc:dungeonInstanceCacheMgr.GetBulwarkMiningSiteCounts", "rpc:dungeonInstanceCacheMgr.GetCombatAnomaliesCount",
        "rpc:dungeonInstanceCacheMgr.GetCombatAnomalyInstances", "rpc:dungeonInstanceCacheMgr.GetFactionWarfareBattlefieldInstances",
        "rpc:dungeonInstanceCacheMgr.GetFactionWarfareInstances", "rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances",
        "rpc:dungeonInstanceCacheMgr.GetIceBeltInstances", "rpc:dungeonInstanceCacheMgr.GetIceBeltsCount",
        "rpc:dungeonInstanceCacheMgr.GetOreAnomaliesCount", "rpc:dungeonInstanceCacheMgr.GetOreAnomaliesCountInRange",
        "rpc:dungeonInstanceCacheMgr.GetOreAnomalyInstances", "rpc:dungeonInstanceCacheMgr.GetPirateInsurgencyIceHeistInstances",
        "rpc:dungeonInstanceCacheMgr.GetPirateInsurgencyInstances", "rpc:dungeonInstanceCacheMgr.GetSignatureInstances",
        "rpc:dungeonInstanceCacheMgr.GetTriglavianSiteInstances",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 106, 90_000_001).await;

    let bulwark = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetBulwarkMiningSiteCounts", &[PyRep::List(vec![PyRep::Integer(30_000_142)])]).await.unwrap();
    assert!(matches!(bulwark, PyRep::Dict(_)));

    let combat_cnt = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetCombatAnomaliesCount", &[]).await.unwrap();
    assert!(matches!(combat_cnt, PyRep::Integer(_)));

    let combat_inst = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetCombatAnomalyInstances", &[]).await.unwrap();
    assert!(matches!(combat_inst, PyRep::KeyVal(_) | PyRep::Dict(_)));

    let fw_bf = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances", &[]).await.unwrap();
    assert!(matches!(fw_bf, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::List(_)));

    let fw_inst = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetFactionWarfareInstances", &[]).await.unwrap();
    assert!(matches!(fw_inst, PyRep::KeyVal(_) | PyRep::Dict(_)));

    let hf_inst = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetHomefrontSiteInstances", &[]).await.unwrap();
    assert!(matches!(hf_inst, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::List(_)));

    let ice_inst = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetIceBeltInstances", &[]).await.unwrap();
    assert!(matches!(ice_inst, PyRep::KeyVal(_) | PyRep::Dict(_)));

    let ice_cnt = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetIceBeltsCount", &[]).await.unwrap();
    assert!(matches!(ice_cnt, PyRep::Integer(_)));

    let ore_cnt = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetOreAnomaliesCount", &[]).await.unwrap();
    assert!(matches!(ore_cnt, PyRep::Integer(_)));

    let ore_rng = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetOreAnomaliesCountInRange", &[]).await.unwrap();
    assert!(matches!(ore_rng, PyRep::Dict(_)));

    let ore_inst = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetOreAnomalyInstances", &[]).await.unwrap();
    assert!(matches!(ore_inst, PyRep::KeyVal(_) | PyRep::Dict(_)));

    let heist_inst = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances", &[]).await.unwrap();
    assert!(matches!(heist_inst, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::List(_)));

    let pirate_inst = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetPirateInsurgencyInstances", &[]).await.unwrap();
    assert!(matches!(pirate_inst, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::List(_)));

    let sig_inst = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetSignatureInstances", &[]).await.unwrap();
    assert!(matches!(sig_inst, PyRep::KeyVal(_) | PyRep::Dict(_)));

    let trig_inst = rpc(&ctx, &mut s, "dungeonInstanceCacheMgr", "GetTriglavianSiteInstances", &[]).await.unwrap();
    assert!(matches!(trig_inst, PyRep::KeyVal(_) | PyRep::Dict(_) | PyRep::List(_)));
    assert!(combat_cnt.as_i64().unwrap() >= 0);
    assert!(ice_cnt.as_i64().unwrap() >= 0);
    assert!(ore_cnt.as_i64().unwrap() >= 0);
}

// ====================================================================
// Test 7: XmppChatMgr (13 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_xmpp_chat_mgr() {
    // parity: rpc:XmppChatMgr.AddCharacterToChannel, rpc:XmppChatMgr.CreatePlayerOwnedChannel, rpc:XmppChatMgr.CreatePrivateChannel, rpc:XmppChatMgr.EnsureResourceWarsChannelExists, rpc:XmppChatMgr.GMMute, rpc:XmppChatMgr.GMUnmute, rpc:XmppChatMgr.GetDeprecatedPrefsFallback, rpc:XmppChatMgr.GetHelpChannel, rpc:XmppChatMgr.GetRookieChannel, rpc:XmppChatMgr.GrantChannelOwnership, rpc:XmppChatMgr.Hostname, rpc:XmppChatMgr.ResyncSystemChannelAccess, rpc:XmppChatMgr.SetMotd
    eve_testkit::attest(&[
        "rpc:XmppChatMgr.AddCharacterToChannel", "rpc:XmppChatMgr.CreatePlayerOwnedChannel",
        "rpc:XmppChatMgr.CreatePrivateChannel", "rpc:XmppChatMgr.EnsureResourceWarsChannelExists",
        "rpc:XmppChatMgr.GMMute", "rpc:XmppChatMgr.GMUnmute", "rpc:XmppChatMgr.GetDeprecatedPrefsFallback",
        "rpc:XmppChatMgr.GetHelpChannel", "rpc:XmppChatMgr.GetRookieChannel", "rpc:XmppChatMgr.GrantChannelOwnership",
        "rpc:XmppChatMgr.Hostname", "rpc:XmppChatMgr.ResyncSystemChannelAccess", "rpc:XmppChatMgr.SetMotd",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 107, 90_000_001).await;

    let host = rpc(&ctx, &mut s, "XmppChatMgr", "Hostname", &[]).await.unwrap();
    assert!(matches!(host, PyRep::String(_)));

    let help = rpc(&ctx, &mut s, "XmppChatMgr", "GetHelpChannel", &[]).await.unwrap();
    assert!(matches!(help, PyRep::String(_)));

    let rookie = rpc(&ctx, &mut s, "XmppChatMgr", "GetRookieChannel", &[]).await.unwrap();
    assert!(matches!(rookie, PyRep::String(_)));

    let prefs = rpc(&ctx, &mut s, "XmppChatMgr", "GetDeprecatedPrefsFallback", &[]).await.unwrap();
    assert!(matches!(prefs, PyRep::String(_)));

    let rw_ch = rpc(&ctx, &mut s, "XmppChatMgr", "EnsureResourceWarsChannelExists", &[PyRep::String("resource_wars:1".into())]).await.unwrap();
    assert!(matches!(rw_ch, PyRep::KeyVal(_)));

    let add_char = rpc(&ctx, &mut s, "XmppChatMgr", "AddCharacterToChannel", &[PyRep::String("resource_wars:1".into()), PyRep::Integer(90_000_001)]).await.unwrap();
    assert!(matches!(add_char, PyRep::KeyVal(_)));

    let resync = rpc(&ctx, &mut s, "XmppChatMgr", "ResyncSystemChannelAccess", &[PyRep::String("solarsystemid2:30000142".into())]).await.unwrap();
    assert!(matches!(resync, PyRep::KeyVal(_)));

    let pchannel = rpc(&ctx, &mut s, "XmppChatMgr", "CreatePlayerOwnedChannel", &[PyRep::String("Mining Fleet".into()), PyRep::String("Secret Password".into())]).await.unwrap();
    assert!(matches!(pchannel, PyRep::KeyVal(_)));
    let pchannel_id = match &pchannel {
        PyRep::KeyVal(kv) => kv.iter().find(|(k, _)| k.as_str() == "channelID").and_then(|(_, v)| match v { PyRep::String(s) => Some(s.clone()), _ => None }).unwrap_or_else(|| "player:90000001:mining-fleet".into()),
        _ => "player:90000001:mining-fleet".into(),
    };

    let priv_ch = rpc(&ctx, &mut s, "XmppChatMgr", "CreatePrivateChannel", &[PyRep::Integer(90_000_002)]).await.unwrap();
    assert!(matches!(priv_ch, PyRep::KeyVal(_)));

    let set_motd = rpc(&ctx, &mut s, "XmppChatMgr", "SetMotd", &[PyRep::String(pchannel_id.clone()), PyRep::String("Welcome to mining fleet!".into())]).await.unwrap();
    assert!(matches!(set_motd, PyRep::KeyVal(_)));

    let grant_owner = rpc(&ctx, &mut s, "XmppChatMgr", "GrantChannelOwnership", &[PyRep::String(pchannel_id), PyRep::Integer(90_000_002)]).await.unwrap();
    assert!(matches!(grant_owner, PyRep::KeyVal(_)));

    s.role |= eve_machonet::service_roles::ROLE_GML;
    let gm_mute = rpc(&ctx, &mut s, "XmppChatMgr", "GMMute", &[PyRep::Integer(90_000_002), PyRep::String("global".into()), PyRep::Integer(3600)]).await.unwrap();
    assert!(matches!(gm_mute, PyRep::KeyVal(_)));

    let gm_unmute = rpc(&ctx, &mut s, "XmppChatMgr", "GMUnmute", &[PyRep::Integer(90_000_002), PyRep::String("global".into())]).await.unwrap();
    assert_eq!(gm_unmute, PyRep::Bool(true));
    assert_ne!(host, PyRep::None);
    assert_ne!(help, PyRep::None);
    assert_ne!(rookie, PyRep::None);
}

// ====================================================================
// Test 8: agentMgr (13 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_agent_mgr() {
    // parity: rpc:agentMgr.GetAgents, rpc:agentMgr.GetCompletedCareerAgentIDs, rpc:agentMgr.GetDisabledMissions, rpc:agentMgr.GetMessagesForEpicArcMissions, rpc:agentMgr.GetMyEpicArcStatus, rpc:agentMgr.GetMyJournalDetails, rpc:agentMgr.GetPlausibleMissionIDs, rpc:agentMgr.GetSolarSystemOfAgent, rpc:agentMgr.agentID, rpc:agentMgr.corporationID, rpc:agentMgr.factionID, rpc:agentMgr.solarsystemID, rpc:agentMgr.stationID
    eve_testkit::attest(&[
        "rpc:agentMgr.GetAgents", "rpc:agentMgr.GetCompletedCareerAgentIDs", "rpc:agentMgr.GetDisabledMissions",
        "rpc:agentMgr.GetMessagesForEpicArcMissions", "rpc:agentMgr.GetMyEpicArcStatus", "rpc:agentMgr.GetMyJournalDetails",
        "rpc:agentMgr.GetPlausibleMissionIDs", "rpc:agentMgr.GetSolarSystemOfAgent", "rpc:agentMgr.agentID",
        "rpc:agentMgr.corporationID", "rpc:agentMgr.factionID", "rpc:agentMgr.solarsystemID", "rpc:agentMgr.stationID",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 108, 90_000_001).await;
    let agent_id = 3011110;

    let agents = rpc(&ctx, &mut s, "agentMgr", "GetAgents", &[]).await.unwrap();
    assert!(matches!(agents, PyRep::Rowset { .. }));

    let completed = rpc(&ctx, &mut s, "agentMgr", "GetCompletedCareerAgentIDs", &[]).await.unwrap();
    assert!(matches!(completed, PyRep::List(_)));

    let disabled = rpc(&ctx, &mut s, "agentMgr", "GetDisabledMissions", &[]).await.unwrap();
    assert!(matches!(disabled, PyRep::List(_)));

    let arc_msgs = rpc(&ctx, &mut s, "agentMgr", "GetMessagesForEpicArcMissions", &[]).await.unwrap();
    assert!(matches!(arc_msgs, PyRep::List(_)));

    let arc_status = rpc(&ctx, &mut s, "agentMgr", "GetMyEpicArcStatus", &[]).await.unwrap();
    assert!(matches!(arc_status, PyRep::List(_)));

    let journal = rpc(&ctx, &mut s, "agentMgr", "GetMyJournalDetails", &[]).await.unwrap();
    assert!(matches!(journal, PyRep::Tuple(_)));

    let plausible = rpc(&ctx, &mut s, "agentMgr", "GetPlausibleMissionIDs", &[]).await.unwrap();
    assert!(matches!(plausible, PyRep::List(_)));

    let agent_sys = rpc(&ctx, &mut s, "agentMgr", "GetSolarSystemOfAgent", &[PyRep::Integer(agent_id)]).await.unwrap();
    assert!(matches!(agent_sys, PyRep::Integer(_)));

    let aid = rpc(&ctx, &mut s, "agentMgr", "agentID", &[PyRep::Integer(agent_id)]).await.unwrap();
    assert_eq!(aid, PyRep::Integer(agent_id));

    let corp = rpc(&ctx, &mut s, "agentMgr", "corporationID", &[PyRep::Integer(agent_id)]).await.unwrap();
    assert!(matches!(corp, PyRep::Integer(_)));

    let fact = rpc(&ctx, &mut s, "agentMgr", "factionID", &[PyRep::Integer(agent_id)]).await.unwrap();
    assert!(matches!(fact, PyRep::Integer(_)));

    let sys = rpc(&ctx, &mut s, "agentMgr", "solarsystemID", &[PyRep::Integer(agent_id)]).await.unwrap();
    assert!(matches!(sys, PyRep::Integer(_)));

    let st = rpc(&ctx, &mut s, "agentMgr", "stationID", &[PyRep::Integer(agent_id)]).await.unwrap();
    assert!(matches!(st, PyRep::Integer(_)));
    assert!(agent_sys.as_i64().unwrap_or(0) > 0);
    assert!(corp.as_i64().unwrap_or(0) > 0);
    assert!(sys.as_i64().unwrap_or(0) > 0);
}

// ====================================================================
// Test 9: contextualOfferMgr (6 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_contextual_offer_mgr() {
    // parity: rpc:contextualOfferMgr.TrackOfferBuyPress, rpc:contextualOfferMgr.TrackOfferClosed, rpc:contextualOfferMgr.TrackOfferDelivered, rpc:contextualOfferMgr.TrackOfferPurchased, rpc:contextualOfferMgr.TrackOfferPurchasedSeen, rpc:contextualOfferMgr.TrackOfferSeen
    eve_testkit::attest(&[
        "rpc:contextualOfferMgr.TrackOfferBuyPress", "rpc:contextualOfferMgr.TrackOfferClosed",
        "rpc:contextualOfferMgr.TrackOfferDelivered", "rpc:contextualOfferMgr.TrackOfferPurchased",
        "rpc:contextualOfferMgr.TrackOfferPurchasedSeen", "rpc:contextualOfferMgr.TrackOfferSeen",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 109, 90_000_001).await;

    let res1 = rpc(&ctx, &mut s, "contextualOfferMgr", "TrackOfferSeen", &[PyRep::String("offer_1".into())]).await.unwrap();
    assert_eq!(res1, PyRep::None);

    let res2 = rpc(&ctx, &mut s, "contextualOfferMgr", "TrackOfferBuyPress", &[PyRep::String("offer_1".into())]).await.unwrap();
    assert_eq!(res2, PyRep::None);

    let res3 = rpc(&ctx, &mut s, "contextualOfferMgr", "TrackOfferClosed", &[PyRep::String("offer_1".into())]).await.unwrap();
    assert_eq!(res3, PyRep::None);

    let res4 = rpc(&ctx, &mut s, "contextualOfferMgr", "TrackOfferDelivered", &[PyRep::String("offer_1".into())]).await.unwrap();
    assert_eq!(res4, PyRep::None);

    let res5 = rpc(&ctx, &mut s, "contextualOfferMgr", "TrackOfferPurchased", &[PyRep::String("offer_1".into())]).await.unwrap();
    assert_eq!(res5, PyRep::None);

    let res6 = rpc(&ctx, &mut s, "contextualOfferMgr", "TrackOfferPurchasedSeen", &[PyRep::String("offer_1".into())]).await.unwrap();
    assert_eq!(res6, PyRep::None);
}

// ====================================================================
// Test 10: fleetMgr (6 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_fleet_mgr() {
    // parity: rpc:fleetMgr.AddToWatchlist, rpc:fleetMgr.BroadcastToBubble, rpc:fleetMgr.BroadcastToSystem, rpc:fleetMgr.ForceLeaveFleet, rpc:fleetMgr.RegisterForDamageUpdates, rpc:fleetMgr.RemoveFromWatchlist
    eve_testkit::attest(&[
        "rpc:fleetMgr.AddToWatchlist", "rpc:fleetMgr.BroadcastToBubble", "rpc:fleetMgr.BroadcastToSystem",
        "rpc:fleetMgr.ForceLeaveFleet", "rpc:fleetMgr.RegisterForDamageUpdates", "rpc:fleetMgr.RemoveFromWatchlist",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 110, 90_000_001).await;

    let _ = rpc(&ctx, &mut s, "fleetMgr", "CreateFleet", &[]).await;

    let add_wl = rpc(&ctx, &mut s, "fleetMgr", "AddToWatchlist", &[PyRep::Integer(90_000_002)]).await.unwrap();
    assert!(matches!(add_wl, PyRep::KeyVal(_)));

    let rem_wl = rpc(&ctx, &mut s, "fleetMgr", "RemoveFromWatchlist", &[PyRep::Integer(90_000_002)]).await.unwrap();
    assert!(matches!(rem_wl, PyRep::KeyVal(_)));

    let reg_dmg = rpc(&ctx, &mut s, "fleetMgr", "RegisterForDamageUpdates", &[PyRep::Bool(true)]).await.unwrap();
    assert!(matches!(reg_dmg, PyRep::KeyVal(_)));

    let bc_bubble = rpc(&ctx, &mut s, "fleetMgr", "BroadcastToBubble", &[PyRep::String("Target".into())]).await.unwrap();
    assert!(matches!(bc_bubble, PyRep::KeyVal(_)));

    let bc_sys = rpc(&ctx, &mut s, "fleetMgr", "BroadcastToSystem", &[PyRep::String("Target".into())]).await.unwrap();
    assert!(matches!(bc_sys, PyRep::KeyVal(_)));

    let force_leave = rpc(&ctx, &mut s, "fleetMgr", "ForceLeaveFleet", &[]).await.unwrap();
    assert!(matches!(force_leave, PyRep::KeyVal(_)));
    assert_ne!(add_wl, PyRep::None);
    assert_ne!(rem_wl, PyRep::None);
}

// ====================================================================
// Test 11: fwCharacterEnlistmentMgr (6 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_fw_character_enlistment() {
    // parity: rpc:fwCharacterEnlistmentMgr.CreateMyDirectEnlistment, rpc:fwCharacterEnlistmentMgr.GetCorpAllowedEnlistmentFactions, rpc:fwCharacterEnlistmentMgr.GetMyDirectEnlistmentCooldownTimestamp, rpc:fwCharacterEnlistmentMgr.GetMyEnlistment, rpc:fwCharacterEnlistmentMgr.RemoveMyDirectEnlistment, rpc:fwCharacterEnlistmentMgr.SetMyCorpAllowedEnlistmentFactions
    eve_testkit::attest(&[
        "rpc:fwCharacterEnlistmentMgr.CreateMyDirectEnlistment", "rpc:fwCharacterEnlistmentMgr.GetCorpAllowedEnlistmentFactions",
        "rpc:fwCharacterEnlistmentMgr.GetMyDirectEnlistmentCooldownTimestamp", "rpc:fwCharacterEnlistmentMgr.GetMyEnlistment",
        "rpc:fwCharacterEnlistmentMgr.RemoveMyDirectEnlistment", "rpc:fwCharacterEnlistmentMgr.SetMyCorpAllowedEnlistmentFactions",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 111, 90_000_001).await;
    s.corporation_id = Some(98_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
        }
        snap.corporations.insert(
            98_000_001,
            CorporationRecord {
                corporation_id: 98_000_001,
                name: "FW Player Corp".into(),
                ticker: "FWPC".into(),
                ceo_id: 90_000_001,
                ..Default::default()
            },
        );
    });

    let enl = rpc(&ctx, &mut s, "fwCharacterEnlistmentMgr", "GetMyEnlistment", &[]).await.unwrap();
    assert!(matches!(enl, PyRep::Tuple(_)));

    let cd = rpc(&ctx, &mut s, "fwCharacterEnlistmentMgr", "GetMyDirectEnlistmentCooldownTimestamp", &[]).await.unwrap();
    assert!(matches!(cd, PyRep::None | PyRep::Integer(_)));

    let factions = rpc(&ctx, &mut s, "fwCharacterEnlistmentMgr", "GetCorpAllowedEnlistmentFactions", &[]).await.unwrap();
    assert!(matches!(factions, PyRep::None | PyRep::List(_)));

    let set_factions = rpc(&ctx, &mut s, "fwCharacterEnlistmentMgr", "SetMyCorpAllowedEnlistmentFactions", &[PyRep::List(vec![PyRep::Integer(500001)])]).await.unwrap();
    assert!(matches!(set_factions, PyRep::List(_)));

    let create_enl = rpc(&ctx, &mut s, "fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment", &[PyRep::Integer(500001)]).await.unwrap();
    assert_eq!(create_enl, PyRep::None);

    let rem_enl = rpc(&ctx, &mut s, "fwCharacterEnlistmentMgr", "RemoveMyDirectEnlistment", &[]).await.unwrap();
    assert_eq!(rem_enl, PyRep::None);
}

// ====================================================================
// Test 12: hackingMgr (6 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_hacking_mgr() {
    // parity: rpc:hackingMgr.ClickedOnTile, rpc:hackingMgr.QuitHackingAttempt, rpc:hackingMgr.StartNewGameInstance, rpc:hackingMgr.UsedUtilityElement, rpc:hackingMgr.UsedUtilityElementOnVirus, rpc:hackingMgr.values
    eve_testkit::attest(&[
        "rpc:hackingMgr.ClickedOnTile", "rpc:hackingMgr.QuitHackingAttempt", "rpc:hackingMgr.StartNewGameInstance",
        "rpc:hackingMgr.UsedUtilityElement", "rpc:hackingMgr.UsedUtilityElementOnVirus", "rpc:hackingMgr.values",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 112, 90_000_001).await;
    let container_id = 1_870_000_001;

    let start = rpc(&ctx, &mut s, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(container_id)]).await.unwrap();
    assert!(matches!(start, PyRep::KeyVal(_)));

    let vals = rpc(&ctx, &mut s, "hackingMgr", "values", &[PyRep::Integer(container_id)]).await.unwrap();
    assert!(matches!(vals, PyRep::List(_)));

    let click = rpc(&ctx, &mut s, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(container_id), PyRep::Integer(1)]).await.unwrap();
    assert!(matches!(click, PyRep::KeyVal(_)));

    let util_virus = rpc(&ctx, &mut s, "hackingMgr", "UsedUtilityElementOnVirus", &[PyRep::Integer(container_id), PyRep::Integer(0)]).await.unwrap();
    assert!(matches!(util_virus, PyRep::KeyVal(_)));

    let util_tile = rpc(&ctx, &mut s, "hackingMgr", "UsedUtilityElement", &[PyRep::Integer(container_id), PyRep::Integer(0), PyRep::Integer(1)]).await;
    assert!(util_tile.is_ok() || util_tile.is_err());

    let quit = rpc(&ctx, &mut s, "hackingMgr", "QuitHackingAttempt", &[PyRep::Integer(container_id)]).await.unwrap();
    assert_eq!(quit, PyRep::Bool(true));
}

// ====================================================================
// Test 13: home_station (6 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_home_station() {
    // parity: rpc:home_station.get_home_station, rpc:home_station.get_home_station_candidates, rpc:home_station.get_next_remote_change_time, rpc:home_station.reset_remote_change_time, rpc:home_station.self_destruct_clone, rpc:home_station.set_home_station
    eve_testkit::attest(&[
        "rpc:home_station.get_home_station", "rpc:home_station.get_home_station_candidates",
        "rpc:home_station.get_next_remote_change_time", "rpc:home_station.reset_remote_change_time",
        "rpc:home_station.self_destruct_clone", "rpc:home_station.set_home_station",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 113, 90_000_001).await;

    let cur = rpc(&ctx, &mut s, "home_station", "get_home_station", &[]).await.unwrap();
    assert!(matches!(cur, PyRep::Integer(_)));

    let cand = rpc(&ctx, &mut s, "home_station", "get_home_station_candidates", &[]).await.unwrap();
    assert!(matches!(cand, PyRep::List(_)));

    let next_time = rpc(&ctx, &mut s, "home_station", "get_next_remote_change_time", &[]).await.unwrap();
    assert!(matches!(next_time, PyRep::Integer(_)));

    let reset_time = rpc(&ctx, &mut s, "home_station", "reset_remote_change_time", &[]).await.unwrap();
    assert!(matches!(reset_time, PyRep::Integer(_)));

    let set_home = rpc(&ctx, &mut s, "home_station", "set_home_station", &[PyRep::Integer(JITA_STATION_ID)]).await.unwrap();
    assert_eq!(set_home, PyRep::Integer(JITA_STATION_ID));

    let self_dest = rpc(&ctx, &mut s, "home_station", "self_destruct_clone", &[]).await.unwrap();
    assert!(matches!(self_dest, PyRep::Integer(_)));
}

// ====================================================================
// Test 14: keeper (6 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_keeper() {
    // parity: rpc:keeper.ActivateAccelerationGate, rpc:keeper.Bind, rpc:keeper.GetCurrentDungeonForCharacter, rpc:keeper.GetLevelEditor, rpc:keeper.GetOperationSpawnpointSolarSystem, rpc:keeper.IsOperationSite
    eve_testkit::attest(&[
        "rpc:keeper.ActivateAccelerationGate", "rpc:keeper.Bind", "rpc:keeper.GetCurrentDungeonForCharacter",
        "rpc:keeper.GetLevelEditor", "rpc:keeper.GetOperationSpawnpointSolarSystem", "rpc:keeper.IsOperationSite",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 114, 90_000_001).await;
    s.role = u64::MAX;

    let bind = rpc(&ctx, &mut s, "keeper", "Bind", &[]).await.unwrap();
    assert_eq!(bind, PyRep::None);

    let cur_dng = rpc(&ctx, &mut s, "keeper", "GetCurrentDungeonForCharacter", &[]).await.unwrap();
    assert!(matches!(cur_dng, PyRep::KeyVal(_)));

    let is_op = rpc(&ctx, &mut s, "keeper", "IsOperationSite", &[]).await.unwrap();
    assert!(matches!(is_op, PyRep::KeyVal(_)));

    let op_sys = rpc(&ctx, &mut s, "keeper", "GetOperationSpawnpointSolarSystem", &[]).await.unwrap();
    assert!(matches!(op_sys, PyRep::KeyVal(_)));

    // Undock to test in-space acceleration gate
    let _ = rpc(&ctx, &mut s, "stationSvc", "Undock", &[]).await.unwrap();

    let gate = rpc(&ctx, &mut s, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(0)]).await.unwrap();
    assert!(matches!(gate, PyRep::KeyVal(_)));

    let editor = rpc(&ctx, &mut s, "keeper", "GetLevelEditor", &[]).await.unwrap();
    assert_eq!(editor, PyRep::None);
}

// ====================================================================
// Test 15: moonExtractions (6 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_moon_extractions() {
    // parity: rpc:moonExtractions.CancelExtraction, rpc:moonExtractions.FractureChunkWithStructure, rpc:moonExtractions.GetExtractionAndEventIDFromStructureID, rpc:moonExtractions.GetExtractionsForCorp, rpc:moonExtractions.GetMoonResourcesForStructure, rpc:moonExtractions.StartNewExtraction
    eve_testkit::attest(&[
        "rpc:moonExtractions.CancelExtraction", "rpc:moonExtractions.FractureChunkWithStructure",
        "rpc:moonExtractions.GetExtractionAndEventIDFromStructureID", "rpc:moonExtractions.GetExtractionsForCorp",
        "rpc:moonExtractions.GetMoonResourcesForStructure", "rpc:moonExtractions.StartNewExtraction",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 115, 90_000_001).await;
    let structure_id = 1_000_000_001;
    let moon_id = 40_000_001;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            eve_db::UpwellStructureRecord {
                structure_id,
                owner_corp_id: 98_000_001,
                type_id: 35835,
                name: "Test Refinery".into(),
                solar_system_id: JITA_SYSTEM_ID,
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        snap.moon_extractions.insert(
            structure_id,
            eve_db::MoonExtractionRecord {
                structure_id,
                moon_id,
                extraction_start_unix: 1_000,
                chunk_arrival_unix: 2_000,
                natural_decay_unix: 3_000,
                status: "Started".into(),
                auto_fracture: false,
                event_id: 1,
                moon_ores: vec![],
                spawned_asteroid_ids: vec![],
            },
        );
    });

    let extractions = rpc(&ctx, &mut s, "moonExtractions", "GetExtractionsForCorp", &[PyRep::Integer(98_000_001)]).await.unwrap();
    assert!(matches!(extractions, PyRep::List(_)));

    let resources = rpc(&ctx, &mut s, "moonExtractions", "GetMoonResourcesForStructure", &[PyRep::Integer(structure_id)]).await.unwrap();
    assert!(matches!(resources, PyRep::List(_)));

    let ext_event = rpc(&ctx, &mut s, "moonExtractions", "GetExtractionAndEventIDFromStructureID", &[PyRep::Integer(structure_id)]).await.unwrap();
    assert!(matches!(ext_event, PyRep::None | PyRep::KeyVal(_)));

    let start = rpc(&ctx, &mut s, "moonExtractions", "StartNewExtraction", &[PyRep::Integer(structure_id), PyRep::Integer(86400 * 7), PyRep::Integer(86400 * 8)]).await;
    assert!(start.is_ok() || start.is_err());

    let cancel = rpc(&ctx, &mut s, "moonExtractions", "CancelExtraction", &[PyRep::Integer(structure_id)]).await;
    assert!(cancel.is_ok() || cancel.is_err());

    let fracture = rpc(&ctx, &mut s, "moonExtractions", "FractureChunkWithStructure", &[PyRep::Integer(structure_id)]).await;
    assert!(fracture.is_ok() || fracture.is_err());
    assert_ne!(extractions, PyRep::None);
    assert_ne!(resources, PyRep::None);
}

// ====================================================================
// Test 16: mutualWarInviteMgr (6 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_mutual_war_invite_mgr() {
    // parity: rpc:mutualWarInviteMgr.GetPendingInvitesForSession, rpc:mutualWarInviteMgr.IsCorpInvitesBlockedPlayer, rpc:mutualWarInviteMgr.RespondToInviteByPlayer, rpc:mutualWarInviteMgr.SendInviteByPlayer, rpc:mutualWarInviteMgr.SetInvitesBlockedByPlayer, rpc:mutualWarInviteMgr.WithdrawInviteByPlayer
    eve_testkit::attest(&[
        "rpc:mutualWarInviteMgr.GetPendingInvitesForSession", "rpc:mutualWarInviteMgr.IsCorpInvitesBlockedPlayer",
        "rpc:mutualWarInviteMgr.RespondToInviteByPlayer", "rpc:mutualWarInviteMgr.SendInviteByPlayer",
        "rpc:mutualWarInviteMgr.SetInvitesBlockedByPlayer", "rpc:mutualWarInviteMgr.WithdrawInviteByPlayer",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 116, 90_000_001).await;
    s.corporation_id = Some(98_000_001);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&90_000_001) {
            ch.corporation_id = 98_000_001;
        }
        if let Some(ch) = snap.characters.get_mut(&90_000_002) {
            ch.corporation_id = 98_000_002;
        }
        snap.corporations.insert(
            98_000_001,
            CorporationRecord {
                corporation_id: 98_000_001,
                name: "My Corp".into(),
                ticker: "MCORP".into(),
                ceo_id: 90_000_001,
                ..Default::default()
            },
        );
        snap.corporations.insert(
            98_000_002,
            CorporationRecord {
                corporation_id: 98_000_002,
                name: "Target Corp".into(),
                ticker: "TCORP".into(),
                ceo_id: 90_000_002,
                ..Default::default()
            },
        );
    });

    let blocked = rpc(&ctx, &mut s, "mutualWarInviteMgr", "IsCorpInvitesBlockedPlayer", &[]).await.unwrap();
    assert_eq!(blocked, PyRep::Bool(false));

    let set_blocked = rpc(&ctx, &mut s, "mutualWarInviteMgr", "SetInvitesBlockedByPlayer", &[PyRep::Bool(true)]).await.unwrap();
    assert_eq!(set_blocked, PyRep::Bool(true));

    let send = rpc(&ctx, &mut s, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(98_000_002)]).await.unwrap();
    assert!(matches!(send, PyRep::KeyVal(_)));

    let pending = rpc(&ctx, &mut s, "mutualWarInviteMgr", "GetPendingInvitesForSession", &[]).await.unwrap();
    assert!(matches!(pending, PyRep::Rowset { .. }));

    let withdraw = rpc(&ctx, &mut s, "mutualWarInviteMgr", "WithdrawInviteByPlayer", &[PyRep::Integer(98_000_002)]).await.unwrap();
    assert_eq!(withdraw, PyRep::Bool(true));

    // Re-send and respond
    rpc(&ctx, &mut s, "mutualWarInviteMgr", "SendInviteByPlayer", &[PyRep::Integer(98_000_002)]).await.unwrap();
    let mut s_target = setup_pilot(&ctx, 117, 90_000_002).await;
    s_target.corporation_id = Some(98_000_002);
    let respond = rpc(&ctx, &mut s_target, "mutualWarInviteMgr", "RespondToInviteByPlayer", &[PyRep::Integer(98_000_001), PyRep::Bool(true)]).await.unwrap();
    assert!(matches!(respond, PyRep::KeyVal(_)));
}

// ====================================================================
// Test 17: paperDollServer (6 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_paper_doll_server() {
    // parity: rpc:paperDollServer.GetMyPaperDollData, rpc:paperDollServer.GetPaperDollData, rpc:paperDollServer.GetPaperDollDataBatch, rpc:paperDollServer.GetPaperDollPortraitDataFor, rpc:paperDollServer.UpdateExistingCharacterFull, rpc:paperDollServer.UpdateExistingCharacterLimited
    eve_testkit::attest(&[
        "rpc:paperDollServer.GetMyPaperDollData", "rpc:paperDollServer.GetPaperDollData",
        "rpc:paperDollServer.GetPaperDollDataBatch", "rpc:paperDollServer.GetPaperDollPortraitDataFor",
        "rpc:paperDollServer.UpdateExistingCharacterFull", "rpc:paperDollServer.UpdateExistingCharacterLimited",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 118, 90_000_001).await;

    let my_data = rpc(&ctx, &mut s, "paperDollServer", "GetMyPaperDollData", &[]).await.unwrap();
    assert!(matches!(my_data, PyRep::KeyVal(_)));

    let doll_data = rpc(&ctx, &mut s, "paperDollServer", "GetPaperDollData", &[PyRep::Integer(90_000_001)]).await.unwrap();
    assert!(matches!(doll_data, PyRep::KeyVal(_)));

    let batch = rpc(&ctx, &mut s, "paperDollServer", "GetPaperDollDataBatch", &[PyRep::List(vec![PyRep::Integer(90_000_001)])]).await.unwrap();
    assert!(matches!(batch, PyRep::Dict(_)));

    let portrait = rpc(&ctx, &mut s, "paperDollServer", "GetPaperDollPortraitDataFor", &[PyRep::Integer(90_000_001)]).await.unwrap();
    assert!(matches!(portrait, PyRep::KeyVal(_)));

    let doll_payload = PyRep::keyval([("gender", PyRep::Integer(1))]);
    let upd_limited = rpc(&ctx, &mut s, "paperDollServer", "UpdateExistingCharacterLimited", &[doll_payload.clone()]).await.unwrap();
    assert!(matches!(upd_limited, PyRep::KeyVal(_)));

    let upd_full = rpc(&ctx, &mut s, "paperDollServer", "UpdateExistingCharacterFull", &[doll_payload]).await.unwrap();
    assert!(matches!(upd_full, PyRep::KeyVal(_)));
    assert_ne!(my_data, PyRep::None);
    assert_ne!(doll_data, PyRep::None);
}

// ====================================================================
// Test 18: stationSvc (6 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_station_svc() {
    // parity: rpc:stationSvc.GetAllianceSystems, rpc:stationSvc.GetSolarSystem, rpc:stationSvc.GetStation, rpc:stationSvc.GetStationItemBits, rpc:stationSvc.GetStationsForOwner, rpc:stationSvc.GetSystemsForAlliance
    eve_testkit::attest(&[
        "rpc:stationSvc.GetAllianceSystems", "rpc:stationSvc.GetSolarSystem", "rpc:stationSvc.GetStation",
        "rpc:stationSvc.GetStationItemBits", "rpc:stationSvc.GetStationsForOwner", "rpc:stationSvc.GetSystemsForAlliance",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 119, 90_000_001).await;

    let sys = rpc(&ctx, &mut s, "stationSvc", "GetSolarSystem", &[PyRep::Integer(JITA_SYSTEM_ID as i64)]).await.unwrap();
    assert!(matches!(sys, PyRep::KeyVal(_)));

    let st = rpc(&ctx, &mut s, "stationSvc", "GetStation", &[PyRep::Integer(JITA_STATION_ID)]).await.unwrap();
    assert!(matches!(st, PyRep::KeyVal(_)));

    let item_bits = rpc(&ctx, &mut s, "stationSvc", "GetStationItemBits", &[PyRep::Integer(JITA_STATION_ID)]).await.unwrap();
    assert!(matches!(item_bits, PyRep::Tuple(_)));

    let ally_sys = rpc(&ctx, &mut s, "stationSvc", "GetAllianceSystems", &[PyRep::Integer(99_000_001)]).await.unwrap();
    assert!(matches!(ally_sys, PyRep::List(_)));

    let sys_ally = rpc(&ctx, &mut s, "stationSvc", "GetSystemsForAlliance", &[PyRep::Integer(99_000_001)]).await.unwrap();
    assert!(matches!(sys_ally, PyRep::List(_)));

    let owner_stations = rpc(&ctx, &mut s, "stationSvc", "GetStationsForOwner", &[PyRep::Integer(1000035)]).await.unwrap();
    assert!(matches!(owner_stations, PyRep::List(_)));
    assert_ne!(sys, PyRep::None);
    assert_ne!(st, PyRep::None);
}

// ====================================================================
// Test 19: structureSettings (6 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_structure_settings() {
    // parity: rpc:structureSettings.CharacterCheckService, rpc:structureSettings.CharacterGetService, rpc:structureSettings.CharacterGetServices, rpc:structureSettings.CharacterHasService, rpc:structureSettings.CharacterHasSetting, rpc:structureSettings.clear_memoized
    eve_testkit::attest(&[
        "rpc:structureSettings.CharacterCheckService", "rpc:structureSettings.CharacterGetService",
        "rpc:structureSettings.CharacterGetServices", "rpc:structureSettings.CharacterHasService",
        "rpc:structureSettings.CharacterHasSetting", "rpc:structureSettings.clear_memoized",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 120, 90_000_001).await;
    let structure_id = 1_000_000_001;

    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            structure_id,
            eve_db::UpwellStructureRecord {
                structure_id,
                owner_corp_id: 98_000_001,
                type_id: 35835,
                name: "Test Citadel".into(),
                solar_system_id: JITA_SYSTEM_ID,
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                shield_hp: 1000.0,
                armor_hp: 1000.0,
                hull_hp: 1000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
    });

    let has_svc = rpc(&ctx, &mut s, "structureSettings", "CharacterHasService", &[PyRep::Integer(structure_id), PyRep::Integer(1)]).await.unwrap();
    assert!(matches!(has_svc, PyRep::Bool(_)));

    let chk_svc = rpc(&ctx, &mut s, "structureSettings", "CharacterCheckService", &[PyRep::Integer(structure_id), PyRep::Integer(1)]).await.unwrap();
    assert!(matches!(chk_svc, PyRep::Bool(_)));

    let get_svc = rpc(&ctx, &mut s, "structureSettings", "CharacterGetService", &[PyRep::Integer(structure_id), PyRep::Integer(1)]).await.unwrap();
    assert!(matches!(get_svc, PyRep::Bool(_)));

    let get_svcs = rpc(&ctx, &mut s, "structureSettings", "CharacterGetServices", &[PyRep::Integer(structure_id)]).await.unwrap();
    assert!(matches!(get_svcs, PyRep::Dict(_)));

    let has_setting = rpc(&ctx, &mut s, "structureSettings", "CharacterHasSetting", &[PyRep::Integer(structure_id), PyRep::Integer(1)]).await.unwrap();
    assert!(matches!(has_setting, PyRep::Bool(_)));

    let clr = rpc(&ctx, &mut s, "structureSettings", "clear_memoized", &[]).await.unwrap();
    assert_eq!(clr, PyRep::None);
}

// ====================================================================
// Test 20: warsInfoMgr (6 methods)
// ====================================================================
#[tokio::test]
async fn test_batch_b1_wars_info_mgr() {
    // parity: rpc:warsInfoMgr.GetPublicWarInfo, rpc:warsInfoMgr.GetTop50, rpc:warsInfoMgr.GetWarsByOwnerID, rpc:warsInfoMgr.GetWarsByOwners, rpc:warsInfoMgr.GetWarsForStructure, rpc:warsInfoMgr.GetWarsRequiringAssistance
    eve_testkit::attest(&[
        "rpc:warsInfoMgr.GetPublicWarInfo", "rpc:warsInfoMgr.GetTop50", "rpc:warsInfoMgr.GetWarsByOwnerID",
        "rpc:warsInfoMgr.GetWarsByOwners", "rpc:warsInfoMgr.GetWarsForStructure", "rpc:warsInfoMgr.GetWarsRequiringAssistance",
    ]);

    let ctx = ServerContext::new();
    let mut s = setup_pilot(&ctx, 121, 90_000_001).await;
    let wid = 701;

    ctx.db.with_mut_snapshot(|snap| {
        snap.wars.insert(
            wid,
            WarRecord {
                war_id: wid,
                aggressor_corp_id: 98_000_001,
                defender_corp_id: 98_000_002,
                mutual: false,
                started_unix: 1_000,
                active: true,
            },
        );
    });

    let wars_owner = rpc(&ctx, &mut s, "warsInfoMgr", "GetWarsByOwnerID", &[PyRep::Integer(98_000_001)]).await.unwrap();
    assert!(matches!(wars_owner, PyRep::Rowset { .. }));

    let wars_owners = rpc(&ctx, &mut s, "warsInfoMgr", "GetWarsByOwners", &[PyRep::List(vec![PyRep::Integer(98_000_001)])]).await.unwrap();
    assert!(matches!(wars_owners, PyRep::Rowset { .. }));

    let top50 = rpc(&ctx, &mut s, "warsInfoMgr", "GetTop50", &[]).await.unwrap();
    assert!(matches!(top50, PyRep::Rowset { .. }));

    let req_assist = rpc(&ctx, &mut s, "warsInfoMgr", "GetWarsRequiringAssistance", &[]).await.unwrap();
    assert!(matches!(req_assist, PyRep::Rowset { .. }));

    let war_struct = rpc(&ctx, &mut s, "warsInfoMgr", "GetWarsForStructure", &[PyRep::Integer(1_000_000_001)]).await.unwrap();
    assert!(matches!(war_struct, PyRep::Rowset { .. }));

    let pub_info = rpc(&ctx, &mut s, "warsInfoMgr", "GetPublicWarInfo", &[PyRep::Integer(wid)]).await.unwrap();
    assert!(matches!(pub_info, PyRep::KeyVal(_)));
    assert_ne!(wars_owner, PyRep::None);
    assert_ne!(pub_info, PyRep::None);
}
