//! PVE Agents, Missions, SDE Market Groups, Show-Info, Mining Sites & NPC Mining Ops Integration Tests.
//!
// parity: plan:D-1, plan:E-5
// parity: feat:market.market-groups, feat:ui.show-info-types, feat:pve.agent-finder, feat:pve.agent-conversation, feat:pve.mission-accept, feat:pve.mission-decline, feat:pve.mission-quit, feat:mining.ore-signatures, feat:npc.mining-op-npc, feat:mining.npc-mining-ops, feat:pve.mission-mining, feat:mining.mining-missions, feat:mining.mining-market, feat:mining.mining-goals

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::living_world::{LivingWorldConvoy, NpcArchetype};
use eve_server::router::ServerContext;
use eve_server::services::mining_sites;
use eve_server::services::sov_fw_insurgency::seed_system_mining_sites_from_sde;
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

    // RPC: marketProxy.GetRegionBest
    let best_rep = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetRegionBest", &[], &kw, None)
        .await
        .expect("marketProxy.GetRegionBest RPC");
    assert!(
        matches!(
            best_rep,
            PyRep::Dict(_) | PyRep::KeyVal(_) | PyRep::Rowset { .. } | PyRep::ObjectEx { .. }
        ),
        "Expected valid market summary from marketProxy.GetRegionBest"
    );

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

async fn bind_service_oid(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    bind_param: PyRep,
) -> u64 {
    let kw = HashMap::new();
    let res = ctx
        .dispatch_rpc(
            sess,
            service,
            None,
            "MachoBindObject",
            &[bind_param, PyRep::None],
            &kw,
            None,
        )
        .await
        .expect("MachoBindObject failed");
    let PyRep::Tuple(outer) = res else {
        panic!("Expected Tuple from MachoBindObject");
    };
    outer[0].as_i64().expect("bound_id integer") as u64
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

    let PyRep::Rowset {
        columns: agents_cols,
        rows: agents_rows,
    } = agents_rep
    else {
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
    assert!(
        !agents_rows.is_empty(),
        "Level 1 Security agents must be found"
    );

    for row in &agents_rows {
        assert_eq!(row[7].as_i64(), Some(1), "Level must be 1");
        assert_eq!(row[2].as_i64(), Some(24), "Division must be 24");
        assert_eq!(
            row[9],
            PyRep::Bool(true),
            "Level 1 agents must have hasAccess = true"
        );
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
    assert_eq!(
        l2_rows[0][9],
        PyRep::Bool(false),
        "L2 agent requires standing >= 1.0; char has 0.0"
    );

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

    let PyRep::Rowset {
        rows: l2_acc_rows, ..
    } = l2_accessible_rep
    else {
        panic!("Expected Rowset");
    };
    let accessible_row = l2_acc_rows
        .iter()
        .find(|r| r[0].as_i64() == Some(test_l2_agent_id))
        .unwrap();
    assert_eq!(
        accessible_row[9],
        PyRep::Bool(true),
        "L2 agent must now be accessible with standing 3.0"
    );

    // ------------------------------------------------------------------------
    // 2. Agent Conversation via V24 bound agentMgr moniker (feat:pve.agent-conversation)
    // ------------------------------------------------------------------------
    let l1_agent = agents_rows
        .iter()
        .find(|r| r[5].as_i64() == Some(30_000_142))
        .unwrap_or(&agents_rows[0]);
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

    // Bind agent moniker on agentMgr
    let agent_oid =
        bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(l1_agent_id)).await;

    // Verify agent details and solar system via V24 agentMgr methods
    let info_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "GetInfoServiceDetails",
            &[],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.GetInfoServiceDetails");
    assert_eq!(
        info_res.dict_get("agentID").and_then(|v| v.as_i64()),
        Some(l1_agent_id)
    );

    let agent_sys_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "GetSolarSystemOfAgent",
            &[],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.GetSolarSystemOfAgent");
    assert_eq!(agent_sys_res.as_i64(), Some(l1_agent_sys as i64));

    // Start conversation & request mission offer via DoAction(2) (agentDialogueButtonRequestMission)
    let conv_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.DoAction(RequestMission)");

    let PyRep::KeyVal(conv_kv) = conv_res else {
        panic!("Expected KeyVal from DoAction(RequestMission)");
    };
    let mission_id = conv_kv["missionID"].as_i64().expect("missionID") as u32;
    assert_eq!(conv_kv["agentID"].as_i64(), Some(l1_agent_id));
    assert_eq!(conv_kv["state"].as_str(), Some("Offered"));
    assert_eq!(conv_kv["missionState"].as_i64(), Some(1));
    assert!(
        conv_kv["briefing"].as_str().is_some(),
        "Offer must contain briefing text"
    );
    assert!(
        conv_kv["objectives"].as_str().is_some(),
        "Offer must contain objectives"
    );
    assert!(
        conv_kv["iskReward"].as_i64().unwrap() > 0,
        "Offer must contain ISK reward"
    );
    assert!(
        conv_kv["lpReward"].as_i64().unwrap() > 0,
        "Offer must contain LP reward"
    );
    assert!(
        conv_kv["expiryUnix"].as_i64().unwrap() > 0,
        "Offer must contain expiry timestamp"
    );

    // ------------------------------------------------------------------------
    // 3. Mission Accept via DoAction(3) (feat:pve.mission-accept)
    // ------------------------------------------------------------------------
    let accept_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(3)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.DoAction(Accept)");

    let PyRep::KeyVal(acc_kv) = accept_res else {
        panic!("Expected KeyVal from DoAction(Accept)");
    };
    assert_eq!(acc_kv["missionID"].as_i64(), Some(mission_id as i64));
    assert_eq!(acc_kv["state"].as_str(), Some("Accepted"));
    assert_eq!(acc_kv["missionState"].as_i64(), Some(2));
    assert!(
        acc_kv["bookmarkID"].as_i64().is_some(),
        "Accepting must create a bookmark for the pocket"
    );
    assert!(
        acc_kv["expiryUnix"].as_i64().is_some(),
        "Accepting must set expiry"
    );
    assert!(
        acc_kv["bonusExpiryUnix"].as_i64().is_some(),
        "Accepting must set bonus expiry"
    );

    // Verify bookmark exists in DB and journal lists Accepted mission
    let bm_id = acc_kv["bookmarkID"].as_i64().unwrap();
    let has_bm = ctx
        .db
        .with_snapshot(|snap| snap.bookmarks.contains_key(&bm_id));
    assert!(has_bm, "Bookmark must be persisted in snap.bookmarks");

    let journal_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetMyJournalDetails",
            &[],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.GetMyJournalDetails");
    let PyRep::Tuple(j_tuple) = journal_res else {
        panic!("Expected Tuple from GetMyJournalDetails");
    };
    let PyRep::List(j_missions) = &j_tuple[0] else {
        panic!("Expected missions List");
    };
    assert!(
        j_missions
            .iter()
            .any(|m| m.dict_get("missionID").and_then(|v| v.as_i64()) == Some(mission_id as i64)
                && m.dict_get("missionState").and_then(|v| v.as_i64()) == Some(2)),
        "GetMyJournalDetails must include the accepted mission"
    );

    let briefing_info = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "GetMissionBriefingInfo",
            &[],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.GetMissionBriefingInfo");
    assert_eq!(
        briefing_info.dict_get("missionID").and_then(|v| v.as_i64()),
        Some(mission_id as i64)
    );

    // ------------------------------------------------------------------------
    // 4. Mission Quit via DoAction(11) (feat:pve.mission-quit)
    // ------------------------------------------------------------------------
    let initial_corp_st = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&l1_agent[3].as_i64().unwrap()))
            .copied()
            .unwrap_or(0.0)
    });

    let quit_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(11)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.DoAction(Quit)");

    let PyRep::KeyVal(quit_kv) = quit_res else {
        panic!("Expected KeyVal from DoAction(Quit)");
    };
    assert_eq!(quit_kv["state"].as_str(), Some("Quit"));
    assert_eq!(quit_kv["missionState"].as_i64(), Some(6));
    assert_eq!(quit_kv["dungeonCleaned"], PyRep::Bool(true));
    assert_eq!(quit_kv["standingPenaltyApplied"], PyRep::Bool(true));

    // Verify standing penalty (-0.1) was applied
    let post_quit_corp_st = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&l1_agent[3].as_i64().unwrap()))
            .copied()
            .unwrap_or(0.0)
    });
    assert!(
        (post_quit_corp_st - (initial_corp_st - 0.1)).abs() < 1e-4,
        "Quitting must penalize corporation standing by -0.1"
    );

    // Verify bookmark was cleaned up
    let bm_exists_after_quit = ctx
        .db
        .with_snapshot(|snap| snap.bookmarks.contains_key(&bm_id));
    assert!(
        !bm_exists_after_quit,
        "Mission pocket bookmark must be cleaned up on quit"
    );

    // ------------------------------------------------------------------------
    // 5. Mission Decline via DoAction(9) (feat:pve.mission-decline)
    // ------------------------------------------------------------------------
    let offer2_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.DoAction(RequestMission) 2");
    let PyRep::KeyVal(offer2_kv) = offer2_res else {
        panic!("Expected KeyVal from DoAction(RequestMission) 2");
    };
    let _mid2 = offer2_kv["missionID"].as_i64().unwrap() as u32;

    // First decline: no standing loss within initial 4h window
    let decline1_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(9)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.DoAction(Decline) 1");

    let PyRep::KeyVal(dec1_kv) = decline1_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(dec1_kv["state"].as_str(), Some("Declined"));
    assert_eq!(dec1_kv["missionState"].as_i64(), Some(5));
    assert_eq!(
        dec1_kv["standingPenaltyApplied"],
        PyRep::Bool(false),
        "First decline in 4h incurs no standing penalty"
    );

    // Offer again immediately
    let offer3_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.DoAction(RequestMission) 3");
    let PyRep::KeyVal(offer3_kv) = offer3_res else {
        panic!("Expected KeyVal from DoAction(RequestMission) 3");
    };
    let _mid3 = offer3_kv["missionID"].as_i64().unwrap() as u32;

    // Repeated decline within 4h -> standing loss applied!
    let decline2_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(9)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.DoAction(Decline) 2");

    let PyRep::KeyVal(dec2_kv) = decline2_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(dec2_kv["state"].as_str(), Some("Declined"));
    assert_eq!(
        dec2_kv["standingPenaltyApplied"],
        PyRep::Bool(true),
        "Repeated decline within 4h MUST apply standing penalty"
    );
}

// ============================================================================
// Helper: run one dogmaIM.Activate mining cycle, approaching the target first if it's out of range
// ============================================================================
#[allow(clippy::too_many_arguments)]
async fn activate_in_range(
    ctx: &ServerContext,
    sess: &mut SessionState,
    handle: &eve_server::system_actor::SystemHandle,
    dogma_oid: u64,
    beyonce_oid: u64,
    module_id: i64,
    target_id: i64,
) -> PyRep {
    let kw = HashMap::new();
    for _attempt in 0..3 {
        match ctx
            .dispatch_rpc(
                sess,
                "dogmaIM",
                Some(dogma_oid),
                "Activate",
                &[PyRep::Integer(module_id), PyRep::Integer(target_id)],
                &kw,
                None,
            )
            .await
        {
            Ok(res) => return res,
            Err(e) if e.code == "TargetOutOfRange" => {
                ctx.dispatch_rpc(
                    sess,
                    "beyonce",
                    Some(beyonce_oid),
                    "CmdFollowBall",
                    &[PyRep::Integer(target_id), PyRep::Real(1_000.0)],
                    &kw,
                    None,
                )
                .await
                .expect("CmdFollowBall toward out-of-range rock");
                for _ in 0..60 {
                    handle.step_manual(1.0).await.unwrap();
                }
            }
            Err(e) => panic!("dogmaIM.Activate on {target_id} failed: {e:?}"),
        }
    }
    panic!("rock {target_id} still out of range after approaching");
}

// ============================================================================
// MN-15: Mining Sites: Gas Sites, Ore Anomalies, Rare Signatures & Sov Ore Sites
// ============================================================================
#[tokio::test]
// parity: feat:mining.ore-signatures
// Also exercises feat:mining.ore-anomalies (no timed expiry yet), feat:mining.gas-sites (no guarded
// sites or NPC spawns yet) and feat:mining.sov-ore-sites (one prospecting upgrade modelled); those rows stay open.
async fn test_mn15_mining_sites_gas_anomalies_signatures_sov() {
    let ctx = ServerContext::new();
    let mut sess = create_auth_session(&ctx, 1003).await;
    let kw = HashMap::new();
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_180_u32; // Osmon (The Forge), true sec 0.680 -> displayed 0.7
    let au = 149_597_870_700.0_f64;

    // Undock / place Venture (32880) with Miner I (483) and Gas Cloud Harvester I (25266) in space
    let ship_id = 700_001_500_i64;
    let miner_mod_id = 700_001_501_i64;
    let gas_mod_id = 700_001_502_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.active_ship_id = ship_id;
        }
        // Grant Gas Cloud Harvesting (25544) and Mining (3386) skills
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            3386,
            eve_db::TrainedSkillRecord {
                type_id: 3386,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        sk.insert(
            25544,
            eve_db::TrainedSkillRecord {
                type_id: 25544,
                level: 5,
                skill_points: 256_000.0,
            },
        );
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: 32880, // Venture
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            miner_mod_id,
            InventoryItemRecord {
                item_id: miner_mod_id,
                type_id: 483, // Miner I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            gas_mod_id,
            InventoryItemRecord {
                item_id: gas_mod_id,
                type_id: 25266, // Gas Cloud Harvester I
                owner_id: char_id,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);

    let handle = ctx.systems.get_or_spawn(sys_id);
    handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 32880,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 335.0,
            inertia: 2.8,
            mass: 1_200_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    let beyonce_oid =
        bind_service_oid(&ctx, &mut sess, "beyonce", PyRep::Integer(sys_id as i64)).await;
    let dogma_oid = bind_service_oid(
        &ctx,
        &mut sess,
        "dogmaIM",
        PyRep::Tuple(vec![
            PyRep::Integer(sys_id as i64),
            PyRep::Integer(5), // groupSolarSystem
        ]),
    )
    .await;

    // ------------------------------------------------------------------------
    // 0. Site selection by security band, from real SDE data
    // ------------------------------------------------------------------------
    // Every modelled site resolves against the client asteroid/gas layout with no fallback, and the
    // per-ore unit totals equal the cited wiki rows.
    for def in mining_sites::COSMIC_ORE_SITES {
        let site = mining_sites::resolve_ore_site(&ctx.sde, def)
            .unwrap_or_else(|e| panic!("{} must resolve: {e}", def.dungeon_name));
        let layout = &ctx.sde.dungeon_asteroids[&site.dungeon_id];
        let layout_total: u32 = layout.iter().map(|e| e.asteroid_count).sum();
        assert_eq!(site.asteroids.len(), layout_total as usize, "{}", def.dungeon_name);
        let wiki_total: i64 = def.rows.iter().map(|r| r.total_units).sum();
        let site_total: i64 = site.asteroids.iter().map(|a| a.units).sum();
        assert_eq!(site_total, wiki_total, "{}", def.dungeon_name);
    }
    for def in mining_sites::GAS_SITES {
        mining_sites::resolve_gas_site(&ctx.sde, def)
            .unwrap_or_else(|e| panic!("{} must resolve: {e}", def.dungeon_name));
    }
    for def in mining_sites::SOV_PROSPECTING_DEPOSITS {
        mining_sites::resolve_ore_site(&ctx.sde, &def.site)
            .unwrap_or_else(|e| panic!("{} must resolve: {e}", def.site.dungeon_name));
    }

    // Jita (true sec 0.946, displayed 0.9) gets the high-sec Hidden Omber Deposit signature but no ore
    // anomaly: high-sec ore anomalies only spawn at 0.5-0.8. Wormhole space gets no modelled ore site.
    let jita_id = 30_000_142_u32;
    seed_system_mining_sites_from_sde(&ctx, jita_id).await;
    seed_system_mining_sites_from_sde(&ctx, 31_000_005).await;
    let (jita_anoms, jita_sigs, wh_sites) = ctx.db.with_snapshot(|snap| {
        let anoms: Vec<String> = snap
            .cosmic_anomalies
            .get(&jita_id)
            .map(|l| l.iter().map(|a| a.name.clone()).collect())
            .unwrap_or_default();
        let sigs: Vec<(String, String)> = snap
            .cosmic_signatures
            .get(&jita_id)
            .map(|l| l.iter().map(|s| (s.sig_group.clone(), s.type_name.clone())).collect())
            .unwrap_or_default();
        let wh = snap.cosmic_anomalies.get(&31_000_005).map_or(0, |l| l.len())
            + snap.cosmic_signatures.get(&31_000_005).map_or(0, |l| {
                l.iter().filter(|s| matches!(s.sig_group.as_str(), "Ore" | "Gas")).count()
            });
        (anoms, sigs, wh)
    });
    assert!(jita_anoms.is_empty(), "No ore anomaly in a 0.9 system, got {jita_anoms:?}");
    assert_eq!(
        jita_sigs,
        vec![("Ore".to_string(), "Hidden Omber Deposit".to_string())]
    );
    assert_eq!(wh_sites, 0, "No modelled ore or gas site in wormhole space");

    // ------------------------------------------------------------------------
    // 1. Ore Anomalies via V24 scanSvc (feat:mining.ore-anomalies)
    // ------------------------------------------------------------------------
    // Osmon (displayed 0.7) gets the Small Kernite and Omber Deposit anomaly.
    let scan_mgr_res = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "GetSystemScanMgr",
            &[],
            &kw,
            None,
        )
        .await
        .expect("scanSvc.GetSystemScanMgr");
    let PyRep::List(anom_list) = scan_mgr_res
        .dict_get("anomalies")
        .expect("anomalies in GetSystemScanMgr")
    else {
        panic!("Expected List of anomalies");
    };
    let ore_anom = anom_list
        .iter()
        .find(|a| {
            a.dict_get("siteName").and_then(|v| v.as_str()) == Some("Small Kernite and Omber Deposit")
        })
        .expect("Small Kernite and Omber Deposit anomaly in Osmon");
    let ore_anom_id = ore_anom
        .dict_get("anomalyID")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    assert_eq!(ore_anom.dict_get("signalStrength").and_then(|v| v.as_f64()), Some(1.0));
    assert_eq!(ore_anom.dict_get("probeRequired").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(
        ore_anom.dict_get("oreTypes"),
        Some(&PyRep::List(
            [20, 1227, 17452, 17453, 17867, 17868]
                .into_iter()
                .map(PyRep::Integer)
                .collect()
        ))
    );
    let PyRep::List(anom_ast_reps) = ore_anom.dict_get("asteroidBallIDs").unwrap() else {
        panic!("Expected asteroidBallIDs");
    };
    let anom_ast_ids: Vec<i64> = anom_ast_reps.iter().map(|v| v.as_i64().unwrap()).collect();
    assert_eq!(anom_ast_ids.len(), 18, "4+7+2+2+1+2 asteroids from the client layout");

    // Per-type asteroid counts match the client layout and per-type units match the wiki totals
    // (Small_Kernite_and_Omber_Deposit.md:17-22).
    let per_type = ctx.db.with_snapshot(|snap| {
        let mut m: HashMap<u32, (u32, i64)> = HashMap::new();
        for id in &anom_ast_ids {
            let a = &snap.asteroids[id];
            let e = m.entry(a.type_id).or_default();
            e.0 += 1;
            e.1 += a.quantity;
        }
        m
    });
    let expected: HashMap<u32, (u32, i64)> = [
        (20, (4, 29_726)),
        (1227, (7, 47_013)),
        (17452, (2, 16_965)),
        (17453, (2, 11_811)),
        (17867, (1, 5_754)),
        (17868, (2, 12_921)),
    ]
    .into_iter()
    .collect();
    assert_eq!(per_type, expected);

    // Warp to the ore anomaly via beyonce.CmdWarpToStuff("scan", ore_anom_id)
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_oid),
        "CmdWarpToStuff",
        &[
            PyRep::String("scan".into()),
            PyRep::String(ore_anom_id.clone()),
        ],
        &kw,
        None,
    )
    .await
    .expect("beyonce.CmdWarpToStuff to ore anomaly");
    for _ in 0..40 {
        handle.step_manual(1.0).await.unwrap();
        let (_, b) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        if matches!(b.mode, MovementMode::Stop) {
            break;
        }
    }

    // Directional scan at the anomaly detects its asteroid balls
    let dscan_anom = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "ConeScan",
            &[PyRep::Real(360.0), PyRep::Real(50_000.0)],
            &kw,
            None,
        )
        .await
        .expect("scanSvc.ConeScan at ore anomaly");
    let PyRep::List(dscan_items) = dscan_anom else {
        panic!("Expected List from ConeScan");
    };
    assert!(anom_ast_ids.iter().all(|id| dscan_items
        .iter()
        .any(|it| it.dict_get("itemID").and_then(|v| v.as_i64()) == Some(*id))));

    // One real Miner I cycle on the first asteroid extracts ore and leaves the rock with less ore.
    let first_ast = anom_ast_ids[0];
    let before_units = ctx.db.with_snapshot(|snap| snap.asteroids[&first_ast].quantity);
    let first_cycle = activate_in_range(
        &ctx, &mut sess, &handle, dogma_oid, beyonce_oid, miner_mod_id, first_ast,
    )
    .await;
    let mined = first_cycle.dict_get("normalUnits").and_then(|v| v.as_i64()).unwrap();
    assert!(mined > 0, "Miner I cycle must extract ore: {first_cycle:?}");
    assert_eq!(first_cycle.dict_get("depleted"), Some(&PyRep::Bool(false)));
    let after_units = ctx.db.with_snapshot(|snap| snap.asteroids[&first_ast].quantity);
    assert_eq!(after_units, before_units - mined);

    // Fast-forward: emptying 18 rocks (~153k units) with a Miner I would take thousands of cycles, so
    // drain every rock to its last unit directly, then finish each one with a real dogmaIM.Activate
    // cycle and check that each cycle reports the rock as depleted.
    ctx.db.with_mut_snapshot(|snap| {
        for id in &anom_ast_ids {
            snap.asteroids.get_mut(id).unwrap().quantity = 1;
        }
    });
    for (i, id) in anom_ast_ids.iter().enumerate() {
        let res = activate_in_range(
            &ctx, &mut sess, &handle, dogma_oid, beyonce_oid, miner_mod_id, *id,
        )
        .await;
        assert_eq!(res.dict_get("normalUnits").and_then(|v| v.as_i64()), Some(1), "rock {i}");
        assert_eq!(res.dict_get("depleted"), Some(&PyRep::Bool(true)), "rock {i}");
        let still_listed = ctx.db.with_snapshot(|snap| {
            snap.cosmic_anomalies[&sys_id]
                .iter()
                .any(|a| a.anomaly_id == ore_anom_id && !a.cleared)
        });
        assert_eq!(
            still_listed,
            i + 1 < anom_ast_ids.len(),
            "The anomaly clears exactly when its last rock is emptied (rock {i})"
        );
    }

    // scanSvc.GetScanResults no longer lists the cleared anomaly
    let post_mine_scan = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "GetScanResults",
            &[],
            &kw,
            None,
        )
        .await
        .expect("scanSvc.GetScanResults after depleting anomaly");
    let PyRep::List(remaining_anoms) = post_mine_scan.dict_get("anomalies").unwrap() else {
        panic!("Expected anomalies list");
    };
    assert!(
        !remaining_anoms
            .iter()
            .any(|a| a.dict_get("anomalyID").and_then(|v| v.as_str()) == Some(&ore_anom_id)),
        "Depleted ore anomaly must despawn from scanSvc.GetScanResults"
    );

    // ------------------------------------------------------------------------
    // 2. Hidden ore signature via 2-pass probe scanning (feat:mining.ore-signatures)
    // ------------------------------------------------------------------------
    // Pass 1: wide system probe scan (8 probes at ±6 AU, ±4 AU, ±6 AU with radius 12 AU)
    let mut wide_probes = Vec::new();
    for &sx in &[-1.0_f64, 1.0] {
        for &sy in &[-1.0_f64, 1.0] {
            for &sz in &[-1.0_f64, 1.0] {
                wide_probes.push(PyRep::Tuple(vec![
                    PyRep::Real(sx * 6.0 * au),
                    PyRep::Real(sy * 4.0 * au),
                    PyRep::Real(sz * 6.0 * au),
                    PyRep::Real(12.0 * au),
                ]));
            }
        }
    }
    let pass1_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "RequestScans",
            &[PyRep::List(wide_probes)],
            &kw,
            None,
        )
        .await
        .expect("scanSvc.RequestScans wide pass 1");
    let PyRep::Rowset {
        rows: pass1_rows, ..
    } = pass1_rep
    else {
        panic!("Expected Rowset from scanSvc.RequestScans");
    };
    let ore_pass1 = pass1_rows
        .iter()
        .find(|r| r[2].as_str() == Some("Ore"))
        .expect("Wide scan must detect Ore signature group");
    let ore_sig_id = ore_pass1[1].as_str().unwrap().to_string();
    let ore_x = ore_pass1[6].as_f64().expect("Rough X revealed at >= 0.25");
    let ore_y = ore_pass1[7].as_f64().expect("Rough Y revealed at >= 0.25");
    let ore_z = ore_pass1[8].as_f64().expect("Rough Z revealed at >= 0.25");

    // Before pass 2 the signature isn't resolved, so its contents stay hidden
    let pre_pinpoint = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetScanResults", &[], &kw, None)
        .await
        .expect("scanSvc.GetScanResults before pinpoint");
    let PyRep::List(pre_sigs) = pre_pinpoint.dict_get("signatures").unwrap() else {
        panic!("Expected signatures list");
    };
    let pre_sig = pre_sigs
        .iter()
        .find(|s| s.dict_get("signatureID").and_then(|v| v.as_str()) == Some(&ore_sig_id))
        .unwrap();
    assert_eq!(pre_sig.dict_get("warpable").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(pre_sig.dict_get("oreTypes"), Some(&PyRep::List(vec![])));

    // Pass 2: pinpoint probe scan around (ore_x, ore_y, ore_z) at 0.5 AU radius -> 100% signal strength
    let mut pinpoint_ore_probes = Vec::new();
    for &sx in &[-1.0_f64, 1.0] {
        for &sy in &[-1.0_f64, 1.0] {
            for &sz in &[-1.0_f64, 1.0] {
                pinpoint_ore_probes.push(PyRep::Tuple(vec![
                    PyRep::Real(ore_x + sx * 2_000.0),
                    PyRep::Real(ore_y + sy * 2_000.0),
                    PyRep::Real(ore_z + sz * 2_000.0),
                    PyRep::Real(0.5 * au),
                ]));
            }
        }
    }
    let pass2_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "RequestScans",
            &[PyRep::List(pinpoint_ore_probes)],
            &kw,
            None,
        )
        .await
        .expect("scanSvc.RequestScans pinpoint pass 2");
    let PyRep::Rowset {
        rows: pass2_rows, ..
    } = pass2_rep
    else {
        panic!("Expected Rowset from scanSvc.RequestScans pass 2");
    };
    let ore_pass2 = pass2_rows
        .iter()
        .find(|r| r[1].as_str() == Some(&ore_sig_id))
        .unwrap();
    assert_eq!(ore_pass2[5].as_f64(), Some(1.0), "Pinpoint scan resolves the signature to 100%");
    assert_eq!(ore_pass2[3].as_str(), Some("Ore"));
    assert_eq!(ore_pass2[4].as_str(), Some("Hidden Omber Deposit"));

    // The resolved signature holds the client layout: 6 Omber II-Grade, 6 Omber III-Grade, 10 Omber IV-Grade
    let sig_results = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetScanResults", &[], &kw, None)
        .await
        .expect("scanSvc.GetScanResults");
    let PyRep::List(sigs_list) = sig_results.dict_get("signatures").unwrap() else {
        panic!("Expected signatures list");
    };
    let resolved_ore_sig = sigs_list
        .iter()
        .find(|s| s.dict_get("signatureID").and_then(|v| v.as_str()) == Some(&ore_sig_id))
        .unwrap();
    assert_eq!(resolved_ore_sig.dict_get("warpable").and_then(|v| v.as_bool()), Some(true));
    let PyRep::List(sig_types) = resolved_ore_sig.dict_get("oreTypes").unwrap() else {
        panic!("Expected oreTypes list");
    };
    let count_of = |tid: i64| sig_types.iter().filter(|v| v.as_i64() == Some(tid)).count();
    assert_eq!((count_of(17867), count_of(17868), count_of(46684)), (6, 6, 10));
    assert_eq!(sig_types.len(), 22);
    let PyRep::List(ore_ball_ids) = resolved_ore_sig.dict_get("siteBallIDs").unwrap() else {
        panic!("Expected siteBallIDs");
    };
    let sig_rock = ore_ball_ids[0].as_i64().unwrap();

    // Warp to the probed signature and mine one of its rocks
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(beyonce_oid),
        "CmdWarpToStuff",
        &[
            PyRep::String("scan".into()),
            PyRep::String(ore_sig_id.clone()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Warp to probed Ore signature");
    for _ in 0..40 {
        handle.step_manual(1.0).await.unwrap();
        let (_, b) = handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        if matches!(b.mode, MovementMode::Stop) {
            break;
        }
    }
    let sig_cycle = activate_in_range(
        &ctx, &mut sess, &handle, dogma_oid, beyonce_oid, miner_mod_id, sig_rock,
    )
    .await;
    assert!(sig_cycle.dict_get("normalUnits").and_then(|v| v.as_i64()).unwrap() > 0);

    // ------------------------------------------------------------------------
    // 3. Low-sec gas site with gas harvesting (feat:mining.gas-sites, partial)
    // ------------------------------------------------------------------------
    // Amamake (Heimatar, displayed 0.4) gets Saintly Nebula: 2 Vermillion Cytoserocin clouds of 200 and
    // 300 units (Saintly_Nebula.md:17-18). The wiki lists it as unguarded, so no NPCs spawn.
    let lowsec_sys_id = 30_002_537_u32; // Amamake (low-sec 0.4)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = lowsec_sys_id;
        }
        if let Some(ship_it) = snap.items.get_mut(&ship_id) {
            ship_it.location_id = lowsec_sys_id as i64;
        }
    });
    sess.solar_system_id = Some(lowsec_sys_id);

    let lowsec_handle = ctx.systems.get_or_spawn(lowsec_sys_id);
    lowsec_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 32880,
            position: DVec3::new(100_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 335.0,
            inertia: 2.8,
            mass: 1_200_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();

    let lowsec_beyonce_oid = bind_service_oid(
        &ctx,
        &mut sess,
        "beyonce",
        PyRep::Integer(lowsec_sys_id as i64),
    )
    .await;
    let lowsec_dogma_oid = bind_service_oid(
        &ctx,
        &mut sess,
        "dogmaIM",
        PyRep::Tuple(vec![
            PyRep::Integer(lowsec_sys_id as i64),
            PyRep::Integer(5),
        ]),
    )
    .await;

    // Low-sec band also seeds the Average Hedbergite, Hemorphite and Jaspet Deposit anomaly and the
    // Hidden Crokite, Dark Ochre and Gneiss Deposit signature.
    let (ls_anoms, ls_sigs) = ctx.db.with_snapshot(|snap| {
        (
            snap.cosmic_anomalies[&lowsec_sys_id]
                .iter()
                .map(|a| a.name.clone())
                .collect::<Vec<_>>(),
            snap.cosmic_signatures[&lowsec_sys_id]
                .iter()
                .filter(|s| s.sig_group != "OreAnomaly")
                .map(|s| (s.sig_group.clone(), s.type_name.clone()))
                .collect::<Vec<_>>(),
        )
    });
    assert_eq!(ls_anoms, vec!["Average Hedbergite, Hemorphite and Jaspet Deposit".to_string()]);
    assert_eq!(
        ls_sigs,
        vec![
            ("Ore".to_string(), "Hidden Crokite, Dark Ochre and Gneiss Deposit".to_string()),
            ("Gas".to_string(), "Saintly Nebula".to_string()),
        ]
    );

    // Pass 1 wide scan in low-sec system to locate the Gas signature
    let mut ls_wide_probes = Vec::new();
    for &sx in &[-1.0_f64, 1.0] {
        for &sy in &[-1.0_f64, 1.0] {
            for &sz in &[-1.0_f64, 1.0] {
                ls_wide_probes.push(PyRep::Tuple(vec![
                    PyRep::Real(sx * 6.0 * au),
                    PyRep::Real(sy * 4.0 * au),
                    PyRep::Real(sz * 6.0 * au),
                    PyRep::Real(12.0 * au),
                ]));
            }
        }
    }
    let ls_pass1 = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "RequestScans",
            &[PyRep::List(ls_wide_probes)],
            &kw,
            None,
        )
        .await
        .expect("Low-sec wide scan pass 1");
    let PyRep::Rowset {
        rows: ls_p1_rows, ..
    } = ls_pass1
    else {
        panic!("Expected Rowset");
    };
    let gas_p1 = ls_p1_rows
        .iter()
        .find(|r| r[2].as_str() == Some("Gas"))
        .expect("Wide scan must detect Gas signature");
    let gas_sig_id = gas_p1[1].as_str().unwrap().to_string();
    let gas_x = gas_p1[6].as_f64().unwrap();
    let gas_y = gas_p1[7].as_f64().unwrap();
    let gas_z = gas_p1[8].as_f64().unwrap();

    // Pass 2 pinpoint scan around (gas_x, gas_y, gas_z)
    let mut pinpoint_gas_probes = Vec::new();
    for &sx in &[-1.0_f64, 1.0] {
        for &sy in &[-1.0_f64, 1.0] {
            for &sz in &[-1.0_f64, 1.0] {
                pinpoint_gas_probes.push(PyRep::Tuple(vec![
                    PyRep::Real(gas_x + sx * 2_000.0),
                    PyRep::Real(gas_y + sy * 2_000.0),
                    PyRep::Real(gas_z + sz * 2_000.0),
                    PyRep::Real(0.5 * au),
                ]));
            }
        }
    }
    let ls_pass2 = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "RequestScans",
            &[PyRep::List(pinpoint_gas_probes)],
            &kw,
            None,
        )
        .await
        .expect("Low-sec pinpoint scan pass 2");
    let PyRep::Rowset {
        rows: ls_p2_rows, ..
    } = ls_pass2
    else {
        panic!("Expected Rowset");
    };
    let gas_p2 = ls_p2_rows
        .iter()
        .find(|r| r[1].as_str() == Some(&gas_sig_id))
        .unwrap();
    assert_eq!(gas_p2[5].as_f64(), Some(1.0), "Gas site signature must resolve to 100%");
    assert_eq!(gas_p2[4].as_str(), Some("Saintly Nebula"));

    let pre_gas_scan = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetScanResults", &[], &kw, None)
        .await
        .expect("scanSvc.GetScanResults before gas harvesting");
    let PyRep::List(ls_sigs_rep) = pre_gas_scan.dict_get("signatures").unwrap() else {
        panic!("Expected signatures list");
    };
    let resolved_gas_sig = ls_sigs_rep
        .iter()
        .find(|s| s.dict_get("signatureID").and_then(|v| v.as_str()) == Some(&gas_sig_id))
        .unwrap();
    assert_eq!(
        resolved_gas_sig.dict_get("oreTypes"),
        Some(&PyRep::List(vec![PyRep::Integer(25278), PyRep::Integer(25278)])),
        "Two Vermillion Cytoserocin clouds"
    );
    let PyRep::List(gas_ball_ids) = resolved_gas_sig.dict_get("siteBallIDs").unwrap() else {
        panic!("Expected siteBallIDs for gas site");
    };
    let gas_cloud_ids: Vec<i64> = gas_ball_ids.iter().map(|v| v.as_i64().unwrap()).collect();
    let mut cloud_units: Vec<i64> = ctx.db.with_snapshot(|snap| {
        gas_cloud_ids.iter().map(|id| snap.asteroids[id].quantity).collect()
    });
    cloud_units.sort_unstable();
    assert_eq!(cloud_units, vec![200, 300]);
    let gas_cloud_ball_id = gas_cloud_ids[0];

    // Warp to probed Gas site via beyonce.CmdWarpToStuff("scan", gas_sig_id)
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(lowsec_beyonce_oid),
        "CmdWarpToStuff",
        &[
            PyRep::String("scan".into()),
            PyRep::String(gas_sig_id.clone()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Warp to probed Gas site");
    for _ in 0..40 {
        lowsec_handle.step_manual(1.0).await.unwrap();
        let (_, b) = lowsec_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        if matches!(b.mode, MovementMode::Stop) {
            break;
        }
    }
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(lowsec_beyonce_oid),
        "CmdFollowBall",
        &[PyRep::Integer(gas_cloud_ball_id), PyRep::Real(500.0)],
        &kw,
        None,
    )
    .await
    .expect("Approach gas cloud within 1500m harvester range");
    for _ in 0..15 {
        lowsec_handle.step_manual(1.0).await.unwrap();
    }

    // Gas Cloud Harvester I (25266) harvests Vermillion Cytoserocin (25278)
    let gas_before = ctx.db.with_snapshot(|snap| snap.asteroids[&gas_cloud_ball_id].quantity);
    let harvest_res = ctx
        .dispatch_rpc(
            &mut sess,
            "dogmaIM",
            Some(lowsec_dogma_oid),
            "Activate",
            &[
                PyRep::Integer(gas_mod_id),
                PyRep::Integer(gas_cloud_ball_id),
            ],
            &kw,
            None,
        )
        .await
        .expect("dogmaIM.Activate Gas Cloud Harvester I on gas cloud");
    let harvested = harvest_res.dict_get("normalUnits").and_then(|v| v.as_i64()).unwrap();
    assert!(harvested > 0, "Gas Cloud Harvester must extract Vermillion Cytoserocin units");
    assert_eq!(harvest_res.dict_get("oreTypeID").and_then(|v| v.as_i64()), Some(25278));
    let gas_after = ctx.db.with_snapshot(|snap| snap.asteroids[&gas_cloud_ball_id].quantity);
    assert_eq!(gas_after, gas_before - harvested);

    // Saintly Nebula is unguarded (Saintly_Nebula.md:21), so harvesting spawns no NPCs
    let post_gas_scan = ctx
        .dispatch_rpc(&mut sess, "scanSvc", None, "GetScanResults", &[], &kw, None)
        .await
        .expect("scanSvc.GetScanResults after gas harvesting");
    assert_eq!(
        post_gas_scan.dict_get("gasSiteNpcSpawned").and_then(|v| v.as_bool()),
        Some(false)
    );

    // ------------------------------------------------------------------------
    // 4. Sov Hub Prospecting Upgrades & Sov Ore Sites via V24 sovMgr / sovHubSvc (feat:mining.sov-ore-sites)
    // ------------------------------------------------------------------------
    let sov_sys_id = 30_004_759_u32; // Delve null-sec system (1DQ1-A)
    let player_corp_id = 98_000_055_i64;
    let player_alliance_id = 99_000_055_i64;
    let sov_struct_id = 1_045_000_000_055_i64;

    ctx.db.with_mut_snapshot(|snap| {
        snap.corporations.insert(
            player_corp_id,
            eve_db::CorporationRecord {
                corporation_id: player_corp_id,
                name: "Sov Mining Corp".into(),
                ticker: "SOVMN".into(),
                ceo_id: char_id,
                alliance_id: Some(player_alliance_id),
                tax_rate: 0.05,
                master_wallet: IskCents::from_isk(100_000_000),
                wallet_divisions: Vec::new(),
                description: String::new(),
                url: String::new(),
                hq_station_id: Some(60_003_760),
                members: [char_id].into_iter().collect(),
                member_roles: [(char_id, u64::MAX)].into_iter().collect(),
                applications: Default::default(),
            },
        );
        snap.alliances.insert(
            player_alliance_id,
            eve_db::AllianceRecord {
                alliance_id: player_alliance_id,
                name: "Sov Mining Alliance".into(),
                short_name: "SVMA".into(),
                executor_corp_id: player_corp_id,
                member_corp_ids: [player_corp_id].into_iter().collect(),
            },
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.corporation_id = player_corp_id;
            ch.alliance_id = Some(player_alliance_id);
            ch.solar_system_id = sov_sys_id;
        }
        snap.structures.insert(
            sov_struct_id,
            eve_db::UpwellStructureRecord {
                structure_id: sov_struct_id,
                type_id: 35835,
                owner_corp_id: player_corp_id,
                solar_system_id: sov_sys_id,
                name: "Delve Sov Hub Citadel".into(),
                position: [100_000.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 500,
                shield_hp: 100_000.0,
                armor_hp: 100_000.0,
                hull_hp: 100_000.0,
                reinforced_until_unix: 0,
                last_damage_tick: 0,
                last_damage_unix: 0,
            },
        );
        snap.system_states
            .entry(sov_sys_id)
            .or_insert_with(|| eve_db::SystemDynamicState {
                solar_system_id: sov_sys_id,
                dbs_multiplier: 1.0,
                ess_main_bank: IskCents(0),
                ess_reserve_bank: IskCents(0),
                ess_linked_character_id: None,
                ess_link_started_tick: None,
                sov_alliance_id: None,
                military_index: 1,
                industrial_index: 1,
                strategic_index: 1,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 0,
                insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 600,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0,
            });
    });
    sess.corporation_id = Some(player_corp_id);
    sess.alliance_id = Some(player_alliance_id);
    sess.solar_system_id = Some(sov_sys_id);

    let sov_handle = ctx.systems.get_or_spawn(sov_sys_id);
    sov_handle
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 32880,
            position: DVec3::new(100_500.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 335.0,
            inertia: 2.8,
            mass: 1_200_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    sov_handle.step_manual(1.0).await.unwrap();

    // Before claiming sovereignty, SaveUpgradeConfiguration must reject with SovereigntyNotClaimed
    let unclaim_err = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect_err("SaveUpgradeConfiguration before sovereignty claim must fail");
    assert_eq!(unclaim_err.code, "SovereigntyNotClaimed");

    // Claim sovereignty in null-sec system via sovMgr.AcquireSkyhooks
    let claim_res = ctx
        .dispatch_rpc(
            &mut sess,
            "sovMgr",
            None,
            "AcquireSkyhooks",
            &[],
            &kw,
            None,
        )
        .await
        .expect("sovMgr.AcquireSkyhooks");
    assert_eq!(
        claim_res.dict_get("allianceID").and_then(|v| v.as_i64()),
        Some(player_alliance_id)
    );

    // Without a Sovereignty Hub upgrade item (SDE group 4768) in the hub or active ship, SaveUpgradeConfiguration fails
    let missing_up_err = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("SaveUpgradeConfiguration without upgrade item must fail");
    assert_eq!(missing_up_err.code, "UpgradeItemNotFound");

    // Binding beyonce with a spoofed remote solarSystemID (30002187 Amarr) is rejected and seeds nothing there (`R72-P1-3`)
    let spoof_bind_err = ctx
        .dispatch_rpc(
            &mut sess,
            "beyonce",
            None,
            "MachoBindObject",
            &[PyRep::Integer(30_002_187), PyRep::None],
            &kw,
            None,
        )
        .await
        .expect_err("MachoBindObject(beyonce, foreign_sys_id) must reject");
    assert_eq!(spoof_bind_err.code, "BindAuthorizationDenied");
    let remote_seeded = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&30_002_187)
            .is_some_and(|list| !list.is_empty())
            || snap
                .cosmic_signatures
                .get(&30_002_187)
                .is_some_and(|list| !list.is_empty())
    });
    assert!(
        !remote_seeded,
        "MachoBindObject(beyonce, foreign_sys_id) must not seed mining sites in a remote system"
    );

    // A deprecated pre-Equinox array (2042, group 1020) in the hub isn't a Sovereignty Hub upgrade
    let other_struct_id = 1_045_000_000_056_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            720_000_053,
            InventoryItemRecord {
                item_id: 720_000_053,
                type_id: 2042, // Deprecated Ore Prospecting Array 3 (group 1020)
                owner_id: char_id,
                location_id: sov_struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        // Tritanium Prospecting Array 3 (87702) at a remote station and in a second structure in the
        // same system: neither is the hub or the active ship, so neither may be used (`R72-P1-2`).
        snap.items.insert(
            720_000_054,
            InventoryItemRecord {
                item_id: 720_000_054,
                type_id: 87702,
                owner_id: char_id,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        let mut other = snap.structures[&sov_struct_id].clone();
        other.structure_id = other_struct_id;
        other.name = "Delve Second Citadel".into();
        snap.structures.insert(other_struct_id, other);
        snap.items.insert(
            720_000_056,
            InventoryItemRecord {
                item_id: 720_000_056,
                type_id: 87702,
                owner_id: char_id,
                location_id: other_struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let remote_up_err = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("Deprecated, remote and other-structure upgrade items must all be rejected");
    assert_eq!(remote_up_err.code, "UpgradeItemNotFound");
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.remove(&other_struct_id);
        snap.items.remove(&720_000_056);
    });

    // A prospecting upgrade without a modelled deposit (Tritanium Prospecting Array 1, 82579) is refused
    // explicitly and is not consumed.
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            720_000_057,
            InventoryItemRecord {
                item_id: 720_000_057,
                type_id: 82579,
                owner_id: char_id,
                location_id: sov_struct_id,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let unmodelled_err = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[],
            &[("typeID".to_string(), PyRep::Integer(82579))].into_iter().collect(),
            None,
        )
        .await
        .expect_err("Unmodelled prospecting upgrade must be refused");
    assert_eq!(unmodelled_err.code, "ProspectingDepositNotModelled");
    assert_eq!(
        ctx.db.with_snapshot(|snap| snap.items[&720_000_057].flag),
        InventoryFlag::Hangar
    );
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&720_000_057);
    });

    // Tritanium Prospecting Array 3 (87702) locked in the hub -> UpgradeItemLocked (`R72-P1-2`)
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            720_000_055,
            InventoryItemRecord {
                item_id: 720_000_055,
                type_id: 87702, // Tritanium Prospecting Array 3
                owner_id: char_id,
                location_id: sov_struct_id,
                flag: InventoryFlag::Locked,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    let locked_up_err = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[],
            &kw,
            None,
        )
        .await
        .expect_err("SaveUpgradeConfiguration with locked upgrade item must fail");
    assert_eq!(locked_up_err.code, "UpgradeItemLocked");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(it) = snap.items.get_mut(&720_000_055) {
            it.flag = InventoryFlag::Hangar;
        }
    });

    // Install Tritanium Prospecting Array 3 -> Large Veldspar Deposit (Asteroids_and_ore.md:239-242)
    let upgrade_res = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[PyRep::List(vec![PyRep::Integer(87702)])],
            &kw,
            None,
        )
        .await
        .expect("sovHubSvc.SaveUpgradeConfiguration");
    let PyRep::KeyVal(up_kv) = upgrade_res else {
        panic!("Expected KeyVal from SaveUpgradeConfiguration");
    };
    assert_eq!(up_kv["solarSystemID"].as_i64(), Some(sov_sys_id as i64));
    assert_eq!(up_kv["prospectingLevel"].as_i64(), Some(3));
    assert_eq!(up_kv["upgradeTypeID"].as_i64(), Some(87702));
    assert_eq!(up_kv["online"], PyRep::Bool(true));
    let installed_item = ctx.db.with_snapshot(|snap| snap.items[&720_000_055].clone());
    assert_eq!(installed_item.location_id, sov_struct_id);
    assert_eq!(installed_item.flag, InventoryFlag::SubSystemSlot0);

    // GetInstalledUpgradesLocal reports the installed upgrade type and the spawned deposit
    let sov_sites_res = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "GetInstalledUpgradesLocal",
            &[],
            &kw,
            None,
        )
        .await
        .expect("sovHubSvc.GetInstalledUpgradesLocal");
    let PyRep::List(installed) = sov_sites_res.dict_get("upgrades").unwrap() else {
        panic!("Expected upgrades list");
    };
    assert_eq!(installed.len(), 1);
    assert_eq!(installed[0].dict_get("typeID").and_then(|v| v.as_i64()), Some(87702));
    let PyRep::List(sov_anoms) = sov_sites_res.dict_get("anomalies").unwrap() else {
        panic!("Expected anomalies list from GetInstalledUpgradesLocal");
    };
    assert_eq!(sov_anoms.len(), 1);
    assert_eq!(
        sov_anoms[0].dict_get("siteName").and_then(|v| v.as_str()),
        Some("Large Veldspar Deposit")
    );

    // The deposit uses the client layout (31 Veldspar, 6 Scordite, 4 Plagioclase asteroids) and the wiki
    // per-ore totals (Large_Veldspar_Deposit.md:19-21).
    let sov_anom_id = sov_anoms[0]
        .dict_get("anomalyID")
        .and_then(|v| v.as_str())
        .unwrap()
        .to_string();
    let sov_pos = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies[&sov_sys_id]
            .iter()
            .find(|a| a.anomaly_id == sov_anom_id)
            .map(|a| DVec3::from_array(a.position))
            .unwrap()
    });
    let per_group = ctx.db.with_snapshot(|snap| {
        let mut m: HashMap<u32, (u32, i64)> = HashMap::new();
        for a in snap.asteroids.values().filter(|a| {
            a.solar_system_id == sov_sys_id
                && DVec3::new(a.position.0, a.position.1, a.position.2).distance(sov_pos) <= 50_000.0
        }) {
            let gid = ctx.sde.types[&a.type_id].group_id;
            let e = m.entry(gid).or_default();
            e.0 += 1;
            e.1 += a.quantity;
        }
        m
    });
    let expected_groups: HashMap<u32, (u32, i64)> = [
        (462, (31, 114_769_310)),
        (460, (6, 14_166_663)),
        (458, (4, 4_614_271)),
    ]
    .into_iter()
    .collect();
    assert_eq!(per_group, expected_groups);

    // Warp to the deposit and find a Veldspar (1230) rock on grid
    let sov_beyonce_oid = bind_service_oid(
        &ctx,
        &mut sess,
        "beyonce",
        PyRep::Integer(sov_sys_id as i64),
    )
    .await;
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        Some(sov_beyonce_oid),
        "CmdWarpToStuff",
        &[
            PyRep::String("scan".into()),
            PyRep::String(sov_anom_id.clone()),
        ],
        &kw,
        None,
    )
    .await
    .expect("Warp to Sov Ore Anomaly");
    for _ in 0..40 {
        sov_handle.step_manual(1.0).await.unwrap();
        let (_, b) = sov_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap();
        if matches!(b.mode, MovementMode::Stop) {
            break;
        }
    }
    let dscan_sov = ctx
        .dispatch_rpc(
            &mut sess,
            "scanSvc",
            None,
            "ConeScan",
            &[PyRep::Real(360.0), PyRep::Real(50_000.0)],
            &kw,
            None,
        )
        .await
        .expect("scanSvc.ConeScan at Sov Ore Anomaly");
    let PyRep::List(sov_grid_items) = dscan_sov else {
        panic!("Expected List from ConeScan");
    };
    let veld_ball_id = sov_grid_items
        .iter()
        .find(|it| it.dict_get("typeID").and_then(|v| v.as_i64()) == Some(1230))
        .and_then(|it| it.dict_get("itemID").and_then(|v| v.as_i64()))
        .expect("Veldspar (1230) asteroid ball must be on grid at the Large Veldspar Deposit");

    // Empty that rock in the system actor and check a repeat SaveUpgradeConfiguration doesn't respawn it
    let veld_units = ctx.db.with_snapshot(|snap| snap.asteroids[&veld_ball_id].quantity) as u64;
    sov_handle
        .mine_asteroid(veld_ball_id, veld_units, 0.0)
        .await
        .unwrap();
    let repeat = ctx
        .dispatch_rpc(
            &mut sess,
            "sovHubSvc",
            None,
            "SaveUpgradeConfiguration",
            &[],
            &kw,
            None,
        )
        .await
        .expect("Idempotent SaveUpgradeConfiguration");
    assert_eq!(
        repeat.dict_get("spawnedAnomalies"),
        Some(&PyRep::List(vec![PyRep::String(sov_anom_id.clone())]))
    );
    let veld_after = sov_handle
        .get_asteroid_state(veld_ball_id)
        .await
        .unwrap()
        .unwrap();
    assert!(
        veld_after.depleted && veld_after.remaining_units == 0,
        "Repeat SaveUpgradeConfiguration must not resurrect depleted SovOre asteroids"
    );
}

// ============================================================================
// MN-17: NPC Mining Operations & Hauler Interactions
// ============================================================================
#[tokio::test]
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

    let PyRep::Rowset {
        columns: ops_cols,
        rows: ops_rows,
    } = ops_rep
    else {
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
    let fleet_row = ops_rows
        .iter()
        .find(|r| r[0].as_i64() == Some(convoy_id))
        .expect("Mining op fleet found");
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
    assert_eq!(
        step_kv["minedUnits"].as_i64(),
        Some(250),
        "NPC laser cycle extracted 250 units"
    );
    assert!(
        step_kv["remainingAsteroidUnits"].as_i64().unwrap() > 0,
        "Asteroid volume remains"
    );
    assert_eq!(
        step_kv["hauledToStation"],
        PyRep::Bool(true),
        "Hauler transported cargo to station"
    );
    assert_eq!(
        step_kv["convoyCargoUnits"].as_i64(),
        Some(0),
        "Cargo delivered to station"
    );

    // Verify regional destination station market sell orders were replenished
    let market_order = ctx.db.with_snapshot(|snap| {
        snap.market_orders
            .values()
            .find(|o| !o.is_bid && o.station_id == dest_station_id && o.type_id == ore_type_id)
            .cloned()
    });
    assert!(
        market_order.is_some(),
        "NPC sell order must exist at destination station"
    );
    assert!(
        market_order.unwrap().vol_remaining >= 5_000,
        "Market volume was replenished"
    );
}

// ============================================================================
// MN-18: Mining Missions, Ore/Mineral Market & Mining Goals from Ledger
// ============================================================================
#[tokio::test]
// parity: feat:pve.mission-mining, feat:mining.mining-missions, feat:mining.mining-market, feat:mining.mining-goals
async fn test_mn18_mining_missions_market_and_goals() {
    let ctx = ServerContext::new();
    let mut sess = create_auth_session(&ctx, 1005).await;
    let kw = HashMap::new();

    let char_id = 90_000_001_i64;
    let now = ctx.now_unix();

    // ------------------------------------------------------------------------
    // 1. Ore/Mineral Market Volumes & Orders (feat:mining.mining-market)
    // ------------------------------------------------------------------------
    let veldspar_type = ctx.sde.types.get(&1230).expect("Veldspar type");
    let tritanium_type = ctx.sde.types.get(&34).expect("Tritanium type");
    let comp_veldspar_type = ctx.sde.types.get(&28432).expect("Compressed Veldspar type");

    assert!(
        (veldspar_type.volume - 0.1).abs() < 1e-4,
        "Veldspar unit volume must be 0.1 m3"
    );
    assert!(
        (tritanium_type.volume - 0.01).abs() < 1e-4,
        "Tritanium unit volume must be 0.01 m3"
    );
    assert!(
        (comp_veldspar_type.volume - 0.15).abs() < 1e-4,
        "Compressed Veldspar unit volume must be 0.15 m3"
    );

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
        .dispatch_rpc(
            &mut sess,
            "marketProxy",
            None,
            "GetOrders",
            &[PyRep::Integer(1230)],
            &kw,
            None,
        )
        .await
        .expect("marketProxy.GetOrders for Veldspar");

    let PyRep::Tuple(tuple_orders) = orders_res else {
        panic!("Expected (sells, buys) Tuple from GetOrders");
    };
    assert!(tuple_orders[0].is_crowset(), "Expected CRowset sells");
    assert!(
        !tuple_orders[0].crowset_rows().unwrap().is_empty(),
        "Veldspar orders must be listed on market"
    );

    // ------------------------------------------------------------------------
    // 2. Mining Mission Lifecycle via V24 bound agentMgr moniker (feat:pve.mission-mining, feat:mining.mining-missions)
    // ------------------------------------------------------------------------
    let agents_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            None,
            "GetAgents",
            &[PyRep::Integer(1), PyRep::Integer(23)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.GetAgents for L1 Mining");
    let PyRep::Rowset {
        rows: mining_agents,
        ..
    } = agents_rep
    else {
        panic!("Expected Rowset");
    };
    let m_agent = &mining_agents[0];
    let m_agent_id = m_agent[0].as_i64().unwrap();
    let system_id = m_agent[5].as_i64().unwrap() as u32;
    let m_station_id = m_agent[6].as_i64().unwrap_or(station_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = system_id;
            ch.station_id = Some(m_station_id);
        }
    });
    sess.solar_system_id = Some(system_id);
    sess.station_id = Some(m_station_id);

    let agent_oid =
        bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(m_agent_id)).await;

    // Request mining mission via DoAction(2)
    let offer_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.DoAction(RequestMission)");
    let mission_id = offer_res
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;

    // Accept mission via DoAction(3) -> spawns pocket asteroid
    let accept_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(3)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.DoAction(Accept)");

    let PyRep::KeyVal(accept_kv) = accept_res else {
        panic!("Expected KeyVal from DoAction(Accept)");
    };
    assert_eq!(accept_kv["state"].as_str(), Some("Accepted"));

    // Verify pocket asteroid was spawned in system
    let pocket_ast_id = 910_000_000 + (mission_id as i64) * 1000 + 1;
    let handle = ctx.systems.get_or_spawn(system_id);
    let ast_opt = handle.get_asteroid_state(pocket_ast_id).await.unwrap();
    assert!(
        ast_opt.is_some(),
        "Private pocket asteroid must be spawned"
    );
    assert_eq!(ast_opt.unwrap().type_id, 1230); // Veldspar

    // Clear starter ore to test incomplete delivery
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .retain(|_, it| !(it.owner_id == char_id && it.type_id == 1230));
    });

    // Attempt completion via DoAction(6) without delivering ore -> must fail
    let incomplete = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(6)],
            &kw,
            None,
        )
        .await;
    assert!(
        incomplete.is_err(),
        "Must fail when required ore is not in hangar"
    );
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
                location_id: m_station_id,
                flag: InventoryFlag::Hangar,
                quantity: 500,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    // Complete mission via DoAction(6) -> succeeds, consumes ore, pays rewards
    let comp_res = ctx
        .dispatch_rpc(
            &mut sess,
            "agentMgr",
            Some(agent_oid),
            "DoAction",
            &[PyRep::Integer(6)],
            &kw,
            None,
        )
        .await
        .expect("agentMgr.DoAction(Complete)");

    let PyRep::KeyVal(comp_kv) = comp_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(comp_kv["missionID"].as_i64(), Some(mission_id as i64));

    // Verify 500 units ore was consumed
    let remaining_ore = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&ore_item_id)
            .map(|it| it.quantity)
            .unwrap_or(0)
    });
    assert_eq!(
        remaining_ore, 0,
        "Delivery must consume the 500 units of ore"
    );

    // ------------------------------------------------------------------------
    // 3. Career Goals from Mining Ledger via V24 seasonManager (feat:mining.mining-goals)
    // ------------------------------------------------------------------------
    let _ = ctx
        .db
        .record_mining_ledger_entry(char_id, system_id, 1230, 4_000, now);
    let _ = ctx
        .db
        .record_mining_ledger_entry(char_id, system_id, 1228, 2_500, now);

    // Query goal progress via V24 seasonManager.get_character_progress_by_character_id
    let goal_res = ctx
        .dispatch_rpc(
            &mut sess,
            "seasonManager",
            None,
            "get_character_progress_by_character_id",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("seasonManager.get_character_progress_by_character_id");

    let PyRep::KeyVal(g_kv) = goal_res else {
        panic!("Expected KeyVal");
    };
    assert_eq!(g_kv["goalID"].as_i64(), Some(2));
    assert_eq!(
        g_kv["currentProgress"].as_i64(),
        Some(6_500),
        "Must equal exact ledger sum (4,000 + 2,500)"
    );
    assert_eq!(g_kv["verifiedFromLedger"], PyRep::Bool(true));

    // Claim career goal reward via V24 seasonManager.claim_goal_reward
    let claim_goal_res = ctx
        .dispatch_rpc(
            &mut sess,
            "seasonManager",
            None,
            "claim_goal_reward",
            &[PyRep::Integer(2)],
            &kw,
            None,
        )
        .await
        .expect("seasonManager.claim_goal_reward");
    assert_eq!(
        claim_goal_res.dict_get("spReward").and_then(|v| v.as_f64()),
        Some(50_000.0)
    );
}
