//! PVE Agents, Missions, SDE Market Groups, Show-Info, Mining Sites & NPC Mining Ops Integration Tests.
//!
// parity: plan:D-1, plan:E-5, plan:MN-15, plan:MN-17, plan:MN-18
// parity: feat:market.market-groups, feat:ui.show-info-types, feat:pve.agent-finder, feat:pve.agent-conversation, feat:pve.mission-accept, feat:pve.mission-decline, feat:pve.mission-quit, feat:mining.gas-sites, feat:mining.ore-anomalies, feat:mining.ore-signatures, feat:mining.sov-ore-sites, feat:npc.mining-op-npc, feat:mining.npc-mining-ops, feat:pve.mission-mining, feat:mining.mining-missions, feat:mining.mining-market, feat:mining.mining-goals

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::living_world::{LivingWorldConvoy, NpcArchetype};
use eve_server::router::ServerContext;
use eve_server::system_actor::AsteroidState;
use glam::DVec3;
use std::collections::HashMap;


// ============================================================================
// Helper: Authenticate session with character 90_000_001
// ============================================================================
async fn create_auth_session(ctx: &ServerContext, session_id: u64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);

    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[
            PyRep::String("demo".into()),
            PyRep::String("demo-secret".into()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Login failed");

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
    .expect("SelectCharacterID failed");

    sess.solar_system_id = Some(30_000_142); // Jita
    sess.station_id = Some(60_003_760); // Jita 4-4
    sess.region_id = Some(10_000_002); // The Forge
    sess
}

// ============================================================================
// D-1: SDE Market Groups and Show-Info Dynamic Data
// ============================================================================
#[tokio::test]
// parity: plan:D-1
// parity: feat:market.market-groups, feat:ui.show-info-types
async fn test_d1_market_groups_and_show_info() {
    let ctx = ServerContext::new();
    let mut sess = create_auth_session(&ctx, 1001).await;
    let kw = HashMap::new();

    // ------------------------------------------------------------------------
    // 1. SDE Market Groups Tree and Published Types
    // ------------------------------------------------------------------------
    assert!(!ctx.sde.market_groups.is_empty(), "SDE must load market groups");
    assert!(ctx.sde.market_groups.contains_key(&4), "Root group 4 'Ships' must exist");
    assert!(ctx.sde.market_groups.contains_key(&9), "Root group 9 'Ship Equipment' must exist");

    // Root groups have parent_group_id == None
    let roots = ctx.sde.market_groups_for_parent(None);
    assert!(!roots.is_empty(), "Must find root market groups");
    assert!(roots.iter().any(|mg| mg.market_group_id == 4));

    // Child groups of Ships (4)
    let ship_children = ctx.sde.market_groups_for_parent(Some(4));
    assert!(!ship_children.is_empty(), "Ships group must contain child categories (Frigates, Cruisers, etc.)");

    // Published types assigned to market groups
    let pub_types = ctx.sde.published_market_types();
    assert!(!pub_types.is_empty(), "SDE must contain published market types");

    // RPC: marketProxy.GetMarketGroups
    let mg_rep = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetMarketGroups", &[], &kw, None)
        .await
        .expect("marketProxy.GetMarketGroups RPC");

    let PyRep::Rowset { columns, rows } = &mg_rep else {
        panic!("Expected Rowset from marketProxy.GetMarketGroups, got {:?}", mg_rep);
    };
    assert_eq!(columns, &["marketGroupID", "parentGroupID", "marketGroupName", "description", "iconID", "hasTypes"]);
    assert!(!rows.is_empty(), "Market group rows must not be empty");

    // RPC: marketProxy.GetMarketGroupTypes
    let types_rep = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetMarketGroupTypes", &[PyRep::Integer(4)], &kw, None)
        .await
        .expect("marketProxy.GetMarketGroupTypes RPC");

    let PyRep::Rowset { columns: types_cols, .. } = types_rep else {
        panic!("Expected Rowset from marketProxy.GetMarketGroupTypes");
    };
    assert_eq!(types_cols, &["typeID", "typeName", "groupID", "categoryID", "volume", "basePrice"]);

    // Cache retrieval test: ("Method Call", "server", ("marketProxy", "GetMarketGroups"))
    let cache_obj = ctx.get_or_populate_cached_object(&ServerContext::method_call_object_id("marketProxy", "GetMarketGroups"), true);
    assert!(cache_obj.is_some(), "Market groups must be cached and synthesized");

    // ------------------------------------------------------------------------
    // 2. Type Show-Info Dynamic Data
    // ------------------------------------------------------------------------
    let rifter_tid = 587_u32;
    let sde_info = ctx.sde.show_info_for_type(rifter_tid).expect("Show-info for Rifter");
    assert_eq!(sde_info.type_id, rifter_tid);
    assert_eq!(sde_info.name, "Rifter");
    assert!(sde_info.mass > 0.0);
    assert!(sde_info.volume > 0.0);
    assert!(sde_info.capacity > 0.0);
    assert!(sde_info.published);
    assert!(sde_info.default_attributes.contains_key(&attr_ids::HP_STRUCT));

    // RPC: dogmaIM.ItemGetInfo with TypeID
    let type_info_res = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(rifter_tid as i64)], &kw, None)
        .await
        .expect("dogmaIM.ItemGetInfo with TypeID");

    let PyRep::KeyVal(kv) = type_info_res else {
        panic!("Expected KeyVal from ItemGetInfo");
    };
    assert_eq!(kv["typeID"].as_i64(), Some(rifter_tid as i64));
    assert_eq!(kv["typeName"].as_str(), Some("Rifter"));
    assert_eq!(kv["ownerID"], PyRep::None);

    // RPC: dogmaIM.ItemGetInfo with ItemID having dynamic mutaplasmid attributes
    let dynamic_item_id = 700_000_101_i64;
    let char_id = 90_000_001_i64;
    let mut dyn_attrs = HashMap::new();
    dyn_attrs.insert(attr_ids::MAX_VELOCITY, 350.0); // Boosted max velocity
    dyn_attrs.insert(attr_ids::DAMAGE_MULTIPLIER, 1.25); // Rolled damage

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            dynamic_item_id,
            InventoryItemRecord {
                item_id: dynamic_item_id,
                type_id: rifter_tid,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: dyn_attrs,
                source_mutaplasmid_id: Some(47841), // Abyssal mutaplasmid
                ..Default::default()
            },
        );
    });

    let item_info_res = ctx
        .dispatch_rpc(&mut sess, "dogmaIM", None, "ItemGetInfo", &[PyRep::Integer(dynamic_item_id)], &kw, None)
        .await
        .expect("dogmaIM.ItemGetInfo with ItemID");

    let PyRep::KeyVal(item_kv) = item_info_res else {
        panic!("Expected KeyVal from ItemGetInfo");
    };
    assert_eq!(item_kv["itemID"].as_i64(), Some(dynamic_item_id));
    assert_eq!(item_kv["typeID"].as_i64(), Some(rifter_tid as i64));
    assert_eq!(item_kv["ownerID"].as_i64(), Some(char_id));
    assert_eq!(item_kv["singleton"], PyRep::Bool(true));

    // Dynamic attributes must be present
    let PyRep::Dict(dyn_rep) = &item_kv["dynamicAttributes"] else {
        panic!("Expected dynamicAttributes Dict");
    };
    assert!(!dyn_rep.is_empty(), "Dynamic attributes must be returned");
}

// ============================================================================
// E-5: PVE Agent Finder, Conversation, Mission Accept/Decline/Quit
// ============================================================================
#[tokio::test]
// parity: plan:E-5
// parity: feat:pve.agent-finder, feat:pve.agent-conversation, feat:pve.mission-accept, feat:pve.mission-decline, feat:pve.mission-quit
async fn test_e5_agent_finder_and_conversation_lifecycle() {
    let ctx = ServerContext::new();
    let mut sess = create_auth_session(&ctx, 1002).await;
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;

    // ------------------------------------------------------------------------
    // 1. Agent Finder (feat:pve.agent-finder)
    // ------------------------------------------------------------------------
    // Query Level 1 agents in division 24 (Security)
    let agents_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetAgents",
            &[PyRep::Integer(1), PyRep::Integer(24)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.GetAgents");

    let PyRep::Rowset { columns: agents_cols, rows: agents_rows } = agents_rep else {
        panic!("Expected Rowset from GetAgents");
    };
    assert_eq!(
        agents_cols,
        vec![
            "agentID",
            "agentTypeID",
            "divisionID",
            "corporationID",
            "factionID",
            "solarSystemID",
            "stationID",
            "level",
            "quality",
            "hasAccess",
        ]
    );
    assert!(!agents_rows.is_empty(), "Level 1 Security agents must be found");

    // All Level 1 agents have access = true
    for row in &agents_rows {
        assert_eq!(row[7].as_i64(), Some(1), "Level must be 1");
        assert_eq!(row[2].as_i64(), Some(24), "Division must be 24");
        assert_eq!(row[9], PyRep::Bool(true), "Level 1 agents must have hasAccess = true");
    }

    // Query Level 2 agents with zero character standing -> hasAccess must be false
    let l2_agents_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetAgents",
            &[PyRep::Integer(2), PyRep::Integer(24)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.GetAgents L2");

    let PyRep::Rowset { rows: l2_rows, .. } = l2_agents_rep else {
        panic!("Expected Rowset");
    };
    assert!(!l2_rows.is_empty(), "Level 2 Security agents must be found");
    let test_l2_agent_id = l2_rows[0][0].as_i64().unwrap();
    let test_l2_corp_id = l2_rows[0][3].as_i64().unwrap();
    assert_eq!(l2_rows[0][9], PyRep::Bool(false), "L2 agent requires standing >= 1.0; char has 0.0");

    // Standing gate update: raise character standing with corporation to +3.0
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(test_l2_corp_id, 3.0);
        }
    });

    // Re-query L2 agents -> hasAccess must now be true!
    let l2_accessible_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetAgents",
            &[PyRep::Integer(2), PyRep::Integer(24)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.GetAgents L2 with standing");

    let PyRep::Rowset { rows: l2_acc_rows, .. } = l2_accessible_rep else {
        panic!("Expected Rowset");
    };
    let accessible_row = l2_acc_rows.iter().find(|r| r[0].as_i64() == Some(test_l2_agent_id)).unwrap();
    assert_eq!(accessible_row[9], PyRep::Bool(true), "L2 agent must now be accessible with standing 3.0");

    // ------------------------------------------------------------------------
    // 2. Agent Conversation (feat:pve.agent-conversation)
    // ------------------------------------------------------------------------
    // Pick an accessible L1 agent in Jita / station
    let l1_agent = agents_rows.iter().find(|r| r[5].as_i64() == Some(30_000_142)).unwrap_or(&agents_rows[0]);
    let l1_agent_id = l1_agent[0].as_i64().unwrap();
    let l1_agent_sys = l1_agent[5].as_i64().unwrap() as u32;
    let l1_agent_sta = l1_agent[6].as_i64();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = l1_agent_sys;
            ch.station_id = l1_agent_sta;
        }
    });
    sess.solar_system_id = Some(l1_agent_sys);
    sess.station_id = l1_agent_sta;

    let conv_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "StartConversation",
            &[PyRep::Integer(l1_agent_id)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.StartConversation");

    let PyRep::KeyVal(conv_kv) = conv_res else {
        panic!("Expected KeyVal from StartConversation");
    };
    let mission_id = conv_kv["missionID"].as_i64().expect("missionID") as u32;
    assert_eq!(conv_kv["agentID"].as_i64(), Some(l1_agent_id));
    assert_eq!(conv_kv["state"].as_str(), Some("Offered"));
    assert!(conv_kv["briefing"].as_str().is_some(), "Offer must contain briefing text");
    assert!(conv_kv["objectives"].as_str().is_some(), "Offer must contain objectives");
    assert!(conv_kv["iskReward"].as_i64().unwrap() > 0, "Offer must contain ISK reward");
    assert!(conv_kv["lpReward"].as_i64().unwrap() > 0, "Offer must contain LP reward");
    assert!(conv_kv["expiryUnix"].as_i64().unwrap() > 0, "Offer must contain expiry timestamp");

    // ------------------------------------------------------------------------
    // 3. Mission Accept (feat:pve.mission-accept)
    // ------------------------------------------------------------------------
    let accept_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "AcceptMission",
            &[PyRep::Integer(mission_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.AcceptMission");

    let PyRep::KeyVal(acc_kv) = accept_res else {
        panic!("Expected KeyVal from AcceptMission");
    };
    assert_eq!(acc_kv["missionID"].as_i64(), Some(mission_id as i64));
    assert_eq!(acc_kv["state"].as_str(), Some("Accepted"));
    assert!(acc_kv["bookmarkID"].as_i64().is_some(), "Accepting must create a bookmark for the pocket");
    assert!(acc_kv["expiryUnix"].as_i64().is_some(), "Accepting must set expiry");
    assert!(acc_kv["bonusExpiryUnix"].as_i64().is_some(), "Accepting must set bonus expiry");

    // Verify bookmark exists in DB
    let bm_id = acc_kv["bookmarkID"].as_i64().unwrap();
    let has_bm = ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bm_id));
    assert!(has_bm, "Bookmark must be persisted in snap.bookmarks");

    // ------------------------------------------------------------------------
    // 4. Mission Quit (feat:pve.mission-quit)
    // ------------------------------------------------------------------------
    let initial_corp_st = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).and_then(|c| c.standings.get(&l1_agent[3].as_i64().unwrap())).copied().unwrap_or(0.0)
    });

    let quit_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "QuitMission",
            &[PyRep::Integer(mission_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.QuitMission");

    let PyRep::KeyVal(quit_kv) = quit_res else {
        panic!("Expected KeyVal from QuitMission");
    };
    assert_eq!(quit_kv["state"].as_str(), Some("Quit"));
    assert_eq!(quit_kv["dungeonCleaned"], PyRep::Bool(true));
    assert_eq!(quit_kv["standingPenaltyApplied"], PyRep::Bool(true));

    // Verify standing penalty (-0.1) was applied
    let post_quit_corp_st = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&char_id).and_then(|c| c.standings.get(&l1_agent[3].as_i64().unwrap())).copied().unwrap_or(0.0)
    });
    assert!((post_quit_corp_st - (initial_corp_st - 0.1)).abs() < 1e-4, "Quitting must penalize corporation standing by -0.1");

    // Verify bookmark was cleaned up
    let bm_exists_after_quit = ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bm_id));
    assert!(!bm_exists_after_quit, "Mission pocket bookmark must be cleaned up on quit");

    // ------------------------------------------------------------------------
    // 5. Mission Decline (feat:pve.mission-decline)
    // ------------------------------------------------------------------------
    // Start conversation again for a fresh offer
    let offer2_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "StartConversation",
            &[PyRep::Integer(l1_agent_id)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.StartConversation 2");
    let PyRep::KeyVal(offer2_kv) = offer2_res else {
        panic!("Expected KeyVal from StartConversation 2");
    };
    let mid2 = offer2_kv["missionID"].as_i64().unwrap() as u32;

    // First decline: no standing loss within initial 4h window
    let decline1_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "DeclineMission",
            &[PyRep::Integer(mid2 as i64)],
            &kw,
            None,
        )
        .await
        .expect("DeclineMission 1");

    let PyRep::KeyVal(dec1_kv) = decline1_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(dec1_kv["state"].as_str(), Some("Declined"));
    assert_eq!(dec1_kv["standingPenaltyApplied"], PyRep::Bool(false), "First decline in 4h incurs no standing penalty");

    // Offer again immediately
    let offer3_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "StartConversation",
            &[PyRep::Integer(l1_agent_id)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.StartConversation 3");
    let PyRep::KeyVal(offer3_kv) = offer3_res else {
        panic!("Expected KeyVal from StartConversation 3");
    };
    let mid3 = offer3_kv["missionID"].as_i64().unwrap() as u32;

    // Repeated decline within 4h -> standing loss applied!
    let decline2_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "DeclineMission",
            &[PyRep::Integer(mid3 as i64)],
            &kw,
            None,
        )
        .await
        .expect("DeclineMission 2");

    let PyRep::KeyVal(dec2_kv) = decline2_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(dec2_kv["state"].as_str(), Some("Declined"));
    assert_eq!(dec2_kv["standingPenaltyApplied"], PyRep::Bool(true), "Repeated decline within 4h MUST apply standing penalty");
}

// ============================================================================
// MN-15: Mining Sites: Gas Sites, Ore Anomalies, Rare Signatures & Sov Ore Sites
// ============================================================================
#[tokio::test]
// parity: plan:MN-15
// parity: feat:mining.gas-sites, feat:mining.ore-anomalies, feat:mining.ore-signatures, feat:mining.sov-ore-sites
async fn test_mn15_mining_sites_gas_anomalies_signatures_sov() {
    let ctx = ServerContext::new();
    let mut sess = create_auth_session(&ctx, 1003).await;
    let kw = HashMap::new();
    let sys_id = 30_000_142_u32; // Jita

    // ------------------------------------------------------------------------
    // 1. Gas Sites & Rare Ore Signatures in Cosmic Signatures (feat:mining.gas-sites, feat:mining.ore-signatures)
    // ------------------------------------------------------------------------
    let sigs_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "GetSystemSignatures",
            &[PyRep::Integer(sys_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetSystemSignatures");

    let PyRep::Rowset { columns: sigs_cols, .. } = sigs_rep else {
        panic!("Expected Rowset from GetSystemSignatures");
    };
    assert_eq!(sigs_cols, vec!["signatureID", "group", "type", "signalStrength"]);

    // Find gas site signature (starts unresolved with 0.0 signal strength)
    let gas_sig_id = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures[&sys_id]
            .iter()
            .find(|s| s.sig_group == "Gas Site")
            .map(|s| s.signature_id.clone())
            .expect("Gas Site signature must exist in system")
    });

    // Find rare ore signature (starts unresolved with 0.0 signal strength)
    let ore_sig_id = ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures[&sys_id]
            .iter()
            .find(|s| s.sig_group == "Ore Site")
            .map(|s| s.signature_id.clone())
            .expect("Ore Site signature must exist in system")
    });

    // Query gas site details
    let gas_details_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "GetGasSiteDetails",
            &[PyRep::String(gas_sig_id.clone())],
            &kw,
            None,
        )
        .await
        .expect("GetGasSiteDetails");

    let PyRep::KeyVal(gas_kv) = gas_details_rep else {
        panic!("Expected KeyVal from GetGasSiteDetails");
    };
    assert_eq!(gas_kv["gasTypeID"].as_i64(), Some(25268), "Amber Cytoserocin gas type");
    assert_eq!(gas_kv["cloudCount"].as_i64(), Some(2));
    assert_eq!(gas_kv["cloudUnits"].as_i64(), Some(3_000));
    assert_eq!(gas_kv["npcDelaySeconds"].as_i64(), Some(900));

    // Query rare ore site details
    let ore_details_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "GetOreSiteDetails",
            &[PyRep::String(ore_sig_id.clone())],
            &kw,
            None,
        )
        .await
        .expect("GetOreSiteDetails");

    let PyRep::KeyVal(ore_kv) = ore_details_rep else {
        panic!("Expected KeyVal from GetOreSiteDetails");
    };
    assert_eq!(ore_kv["isRareSignature"], PyRep::Bool(true));
    let PyRep::List(rare_types) = &ore_kv["rareOreTypes"] else {
        panic!("Expected rareOreTypes List");
    };
    assert!(rare_types.contains(&PyRep::Integer(1226)), "Must contain Jaspet (1226)");
    assert!(rare_types.contains(&PyRep::Integer(1231)), "Must contain Hemorphite (1231)");
    assert!(rare_types.contains(&PyRep::Integer(21)), "Must contain Hedbergite (21)");

    // ------------------------------------------------------------------------
    // 2. Ore Anomalies in System (feat:mining.ore-anomalies)
    // ------------------------------------------------------------------------
    let ano_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "GetSystemAnomalies",
            &[PyRep::Integer(sys_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetSystemAnomalies");

    let PyRep::Rowset { columns: ano_cols, rows: ano_rows } = ano_rep else {
        panic!("Expected Rowset from GetSystemAnomalies");
    };
    assert_eq!(
        ano_cols,
        vec![
            "anomalyID",
            "name",
            "anomalyType",
            "solarSystemID",
            "signalStrength",
            "oreTypes",
            "remainingVolume",
            "cleared",
        ]
    );
    assert!(!ano_rows.is_empty(), "Ore anomalies must spawn per system");

    // Anomalies are 100% resolved without probing (signalStrength = 1.0)
    for row in &ano_rows {
        assert_eq!(row[4].as_f64(), Some(1.0), "Cosmic anomalies are directly visible at 100% signal strength");
        assert_eq!(row[2].as_str(), Some("Ore"));
    }

    // Despawn on clear: Clear anomaly
    let target_ano_id = ano_rows[0][0].as_str().unwrap().to_string();
    let clear_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "ClearAnomaly",
            &[PyRep::String(target_ano_id.clone())],
            &kw,
            None,
        )
        .await
        .expect("ClearAnomaly");

    let PyRep::KeyVal(clear_map) = clear_rep else {
        panic!("Expected KeyVal from ClearAnomaly");
    };
    assert_eq!(clear_map["cleared"], PyRep::Bool(true));

    // Re-query anomalies -> cleared anomaly must be despawned from the list
    let ano2_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanMgr",
            None,
            "GetSystemAnomalies",
            &[PyRep::Integer(sys_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetSystemAnomalies 2");

    let PyRep::Rowset { rows: ano2_rows, .. } = ano2_rep else {
        panic!("Expected Rowset");
    };
    assert!(
        !ano2_rows.iter().any(|r| r[0].as_str() == Some(&target_ano_id)),
        "Cleared anomaly must despawn and not be returned"
    );

    // ------------------------------------------------------------------------
    // 3. Sov Hub Prospecting Upgrades & Sov Ore Sites (feat:mining.sov-ore-sites)
    // ------------------------------------------------------------------------
    let sov_sys_id = 30_002_000_u32; // Nullsec system
    let upgrade_res = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "InstallProspectingUpgrade",
            &[PyRep::Integer(sov_sys_id as i64), PyRep::Integer(2)], // Level 2 prospecting
            &kw,
            None,
        )
        .await
        .expect("InstallProspectingUpgrade");

    let PyRep::KeyVal(up_kv) = upgrade_res else {
        panic!("Expected KeyVal from InstallProspectingUpgrade");
    };
    assert_eq!(up_kv["solarSystemID"].as_i64(), Some(sov_sys_id as i64));
    assert_eq!(up_kv["prospectingLevel"].as_i64(), Some(2));
    assert!(up_kv["sitesSpawned"].as_i64().unwrap() >= 2, "Level 2 upgrade must spawn at least 2 sov ore sites");

    // Query sov ore sites
    let sov_sites_res = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "GetSovOreSites",
            &[PyRep::Integer(sov_sys_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetSovOreSites");

    let PyRep::Rowset { columns: sov_cols, rows: sov_rows } = sov_sites_res else {
        panic!("Expected Rowset from GetSovOreSites");
    };
    assert_eq!(sov_cols, vec!["siteID", "siteName", "solarSystemID", "oreTypes", "remainingVolume"]);
    assert!(sov_rows.iter().any(|r| r[1].as_str() == Some("Enormous Asteroid Cluster")));
    assert!(sov_rows.iter().any(|r| r[1].as_str() == Some("Colossal Asteroid Cluster")));
}

// ============================================================================
// MN-17: NPC Mining Operations & Hauler Interactions
// ============================================================================
#[tokio::test]
// parity: plan:MN-17
// parity: feat:npc.mining-op-npc, feat:mining.npc-mining-ops
async fn test_mn17_npc_mining_ops_and_interaction() {
    let ctx = ServerContext::new();
    let mut sess = create_auth_session(&ctx, 1004).await;
    let kw = HashMap::new();

    let convoy_id = 800_000_201_i64;
    let system_id = 30_000_144_u32; // Perimeter
    let dest_station_id = 60_003_766_i64;
    let ore_type_id = 1230_u32; // Veldspar
    sess.solar_system_id = Some(system_id);

    // Spawn an asteroid in Perimeter belt for NPC mining op
    let handle = ctx.systems.get_or_spawn(system_id);
    let ast_id = 750_000_288_i64;
    handle
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ast_id,
                belt_id: 40_000_002,
                type_id: ore_type_id,
                initial_units: 10_000,
                remaining_units: 10_000,
                unit_volume: 0.1,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: ast_id,
                type_id: ore_type_id,
                position: DVec3::new(4_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e8,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
        )
        .await
        .unwrap();

    // Register an NPC Mining Op fleet (miner Venture 32880 + hauler Badger 648)
    ctx.living_world.register_convoy(LivingWorldConvoy {
        convoy_id,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32880, // Venture miner
        origin_system_id: system_id,
        current_system_id: system_id,
        destination_system_id: system_id,
        destination_station_id: dest_station_id,
        cargo_type_id: ore_type_id,
        cargo_quantity: 4_900,
        lod_level: 1,
        destroyed: false,
    });

    // 1. Query NPC mining fleets in system (feat:npc.mining-op-npc)
    let ops_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "livingWorld",
            None,
            "GetMiningOps",
            &[PyRep::Integer(system_id as i64)],
            &kw,
            None,
        )
        .await
        .expect("GetMiningOps");

    let PyRep::Rowset { columns: ops_cols, rows: ops_rows } = ops_rep else {
        panic!("Expected Rowset from GetMiningOps");
    };
    assert_eq!(
        ops_cols,
        vec![
            "convoyID",
            "archetype",
            "minerShipTypeID",
            "haulerShipTypeID",
            "systemID",
            "cargoTypeID",
            "cargoQuantity",
            "destroyed",
        ]
    );
    let fleet_row = ops_rows.iter().find(|r| r[0].as_i64() == Some(convoy_id)).expect("Mining op fleet found");
    assert_eq!(fleet_row[2].as_i64(), Some(32880), "Miner ship: Venture");
    assert_eq!(fleet_row[3].as_i64(), Some(648), "Hauler ship: Badger");
    assert_eq!(fleet_row[7], PyRep::Bool(false), "Fleet active");

    // 2. Step NPC mining op cycle (feat:mining.npc-mining-ops)
    let step_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "livingWorld",
            None,
            "StepMiningOp",
            &[PyRep::Integer(convoy_id)],
            &kw,
            None,
        )
        .await
        .expect("StepMiningOp");

    let PyRep::KeyVal(step_kv) = step_rep else {
        panic!("Expected KeyVal from StepMiningOp");
    };
    assert_eq!(step_kv["convoyID"].as_i64(), Some(convoy_id));
    assert_eq!(step_kv["minedUnits"].as_i64(), Some(250), "NPC laser cycle extracted 250 units");
    assert!(step_kv["remainingAsteroidUnits"].as_i64().unwrap() > 0, "Asteroid volume remains");
    // 4,900 + 250 = 5,150 >= 5,000 hauling threshold -> triggers hauler transport to station!
    assert_eq!(step_kv["hauledToStation"], PyRep::Bool(true), "Hauler transported cargo to station");
    assert_eq!(step_kv["convoyCargoUnits"].as_i64(), Some(0), "Cargo delivered to station");

    // Verify regional destination station market sell orders were replenished
    let market_order = ctx.db.with_snapshot(|snap| {
        snap.market_orders
            .values()
            .find(|o| !o.is_bid && o.station_id == dest_station_id && o.type_id == ore_type_id)
            .cloned()
    });
    assert!(market_order.is_some(), "NPC sell order must exist at destination station");
    assert!(market_order.unwrap().vol_remaining >= 5_000, "Market volume was replenished");
}

// ============================================================================
// MN-18: Mining Missions, Ore/Mineral Market & Mining Goals from Ledger
// ============================================================================
#[tokio::test]
// parity: plan:MN-18
// parity: feat:pve.mission-mining, feat:mining.mining-missions, feat:mining.mining-market, feat:mining.mining-goals
async fn test_mn18_mining_missions_market_and_goals() {
    let ctx = ServerContext::new();
    let mut sess = create_auth_session(&ctx, 1005).await;
    let kw = HashMap::new();

    let char_id = 90_000_001_i64;
    let system_id = 30_000_142_u32;
    let mission_id = 1957_u32; // "Ore Harvesting"
    let now = ctx.now_unix();

    // ------------------------------------------------------------------------
    // 1. Ore/Mineral Market Volumes & Orders (feat:mining.mining-market)
    // ------------------------------------------------------------------------
    let veldspar_type = ctx.sde.types.get(&1230).expect("Veldspar type");
    let tritanium_type = ctx.sde.types.get(&34).expect("Tritanium type");
    let comp_veldspar_type = ctx.sde.types.get(&28432).expect("Compressed Veldspar type");

    assert!((veldspar_type.volume - 0.1).abs() < 1e-4, "Veldspar unit volume must be 0.1 m3");
    assert!((tritanium_type.volume - 0.01).abs() < 1e-4, "Tritanium unit volume must be 0.01 m3");
    assert!((comp_veldspar_type.volume - 0.15).abs() < 1e-4, "Compressed Veldspar unit volume must be 0.15 m3");

    // Place and query sell orders for raw ore, mineral, and compressed ore
    let station_id = 60_003_760_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let o1 = snap.next_entity_id;
        snap.market_orders.insert(
            o1,
            eve_db::MarketOrderRecord {
                order_id: o1,
                owner_id: char_id,
                station_id,
                region_id: 10_000_002,
                type_id: 1230, // Veldspar
                is_bid: false,
                price: IskCents::from_isk(10),
                vol_entered: 50_000,
                vol_remaining: 50_000,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
        snap.next_entity_id += 1;
        let o2 = snap.next_entity_id;
        snap.market_orders.insert(
            o2,
            eve_db::MarketOrderRecord {
                order_id: o2,
                owner_id: char_id,
                station_id,
                region_id: 10_000_002,
                type_id: 34, // Tritanium
                is_bid: false,
                price: IskCents::from_isk(5),
                vol_entered: 100_000,
                vol_remaining: 100_000,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });

    // Query market orders for Veldspar
    let orders_res = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetOrders", &[PyRep::Integer(1230)], &kw, None)
        .await
        .expect("marketProxy.GetOrders for Veldspar");

    let PyRep::Tuple(tuple_orders) = orders_res else {
        panic!("Expected (sells, buys) Tuple from GetOrders");
    };
    assert!(tuple_orders[0].is_crowset(), "Expected CRowset sells");
    assert!(!tuple_orders[0].crowset_rows().unwrap().is_empty(), "Veldspar orders must be listed on market");

    // ------------------------------------------------------------------------
    // 2. Mining Mission Lifecycle (feat:pve.mission-mining, feat:mining.mining-missions)
    // ------------------------------------------------------------------------
    // Offer mining mission
    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, mission_id),
            eve_db::ActiveMissionRecord {
                mission_id,
                agent_id: 3011400,
                character_id: char_id,
                state: "Offered".into(),
                current_room_id: 1,
                npcs_remaining: 0,
                spawned_npc_ball_ids: Vec::new(),
                solar_system_id: system_id,
            },
        );
    });

    // Accept mission -> spawns pocket asteroid
    let accept_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "AcceptMission", &[PyRep::Integer(mission_id as i64)], &kw, None)
        .await
        .expect("AcceptMission");

    let PyRep::KeyVal(accept_kv) = accept_res else {
        panic!("Expected KeyVal from AcceptMission");
    };
    assert_eq!(accept_kv["state"].as_str(), Some("Accepted"));

    // Verify pocket asteroid was spawned in system
    let pocket_ast_id = 910_000_000 + (mission_id as i64) * 1000 + 1;
    let handle = ctx.systems.get_or_spawn(system_id);
    let ast_opt = handle.get_asteroid_state(pocket_ast_id).await.unwrap();
    assert!(ast_opt.is_some(), "Private pocket asteroid must be spawned");
    assert_eq!(ast_opt.unwrap().type_id, 1230); // Veldspar

    // Clear starter ore to test incomplete delivery
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| !(it.owner_id == char_id && it.type_id == 1230));
    });

    // Attempt completion without delivering ore -> must fail
    let incomplete = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "CompleteMission", &[PyRep::Integer(mission_id as i64)], &kw, None)
        .await;
    assert!(incomplete.is_err(), "Must fail when required ore is not in hangar");
    assert_eq!(incomplete.err().unwrap().code, "MissionIncomplete");

    // Deliver required ore (500 units Veldspar) into character station hangar
    let ore_item_id = 720_000_088_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ore_item_id,
            InventoryItemRecord {
                item_id: ore_item_id,
                type_id: 1230,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Complete mission -> succeeds, consumes ore, pays rewards
    let comp_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "CompleteMission", &[PyRep::Integer(mission_id as i64)], &kw, None)
        .await
        .expect("CompleteMission");

    let PyRep::KeyVal(comp_kv) = comp_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(comp_kv["missionID"].as_i64(), Some(mission_id as i64));

    // Verify 500 units ore was consumed
    let remaining_ore = ctx.db.with_snapshot(|snap| {
        snap.items.get(&ore_item_id).map(|it| it.quantity).unwrap_or(0)
    });
    assert_eq!(remaining_ore, 0, "Delivery must consume the 500 units of ore");

    // ------------------------------------------------------------------------
    // 3. Career Goals from Mining Ledger (feat:mining.mining-goals)
    // ------------------------------------------------------------------------
    // Record ledger events
    let _ = ctx.db.record_mining_ledger_entry(char_id, system_id, 1230, 4_000, now);
    let _ = ctx.db.record_mining_ledger_entry(char_id, system_id, 1228, 2_500, now);

    // Query goal progress
    let goal_res = ctx
        .dispatch_rpc(&mut sess, "careerGoals", None, "GetMiningCareerProgress", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("GetMiningCareerProgress");

    let PyRep::KeyVal(g_kv) = goal_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(g_kv["goalID"].as_i64(), Some(2));
    assert_eq!(g_kv["currentProgress"].as_i64(), Some(6_500), "Must equal exact ledger sum (4,000 + 2,500)");
    assert_eq!(g_kv["verifiedFromLedger"], PyRep::Bool(true));
}
