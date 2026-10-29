//! PVE SDE Market Groups, Show-Info, Mining Missions, Market & Career Goals Integration Tests.

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::attr_ids;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

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

    sess.solar_system_id = Some(30_000_142);
    sess.station_id = Some(60_003_760);
    sess.region_id = Some(10_000_002);
    sess
}

// ============================================================================
// D-1: SDE Market Groups and Show-Info Dynamic Data
// ============================================================================
#[tokio::test]
async fn test_d1_market_groups_and_show_info() {
    // parity: plan:D-1, feat:market.market-groups, feat:ui.show-info-types, rpc:marketProxy.GetRegionBest
    eve_testkit::attest(&["plan:D-1", "feat:market.market-groups", "feat:ui.show-info-types"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = create_auth_session(&ctx, 1001).await;
    let kw = HashMap::new();

    assert!(!ctx.sde.market_groups.is_empty(), "SDE must load market groups");
    assert!(ctx.sde.market_groups.contains_key(&4), "Root group 4 'Ships' must exist");
    assert!(ctx.sde.market_groups.contains_key(&9), "Root group 9 'Ship Equipment' must exist");

    let roots = ctx.sde.market_groups_for_parent(None);
    assert!(!roots.is_empty(), "Must find root market groups");
    assert!(roots.iter().any(|mg| mg.market_group_id == 4));

    let ship_children = ctx.sde.market_groups_for_parent(Some(4));
    assert!(!ship_children.is_empty(), "Ships group must contain child categories");

    let pub_types = ctx.sde.published_market_types();
    assert!(!pub_types.is_empty(), "SDE must contain published market types");

    let mg_rep = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetMarketGroups", &[], &kw, None)
        .await
        .expect("marketProxy.GetMarketGroups RPC");

    let PyRep::Rowset { columns, rows } = &mg_rep else {
        panic!("Expected Rowset from marketProxy.GetMarketGroups, got {:?}", mg_rep);
    };
    assert_eq!(columns, &["marketGroupID", "parentGroupID", "marketGroupName", "description", "iconID", "hasTypes"]);
    assert!(!rows.is_empty(), "Market group rows must not be empty");

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

    let cache_obj = ctx.get_or_populate_cached_object(&ServerContext::method_call_object_id("marketProxy", "GetMarketGroups"), true);
    assert!(cache_obj.is_some(), "Market groups must be cached and synthesized");

    let rifter_tid = 587_u32;
    let sde_info = ctx.sde.show_info_for_type(rifter_tid).expect("Show-info for Rifter");
    assert_eq!(sde_info.type_id, rifter_tid);
    assert_eq!(sde_info.name, "Rifter");
    assert!(sde_info.mass > 0.0);
    assert!(sde_info.volume > 0.0);
    assert!(sde_info.capacity > 0.0);
    assert!(sde_info.published);
    assert!(sde_info.default_attributes.contains_key(&attr_ids::HP_STRUCT));

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

    let dynamic_item_id = 700_000_101_i64;
    let char_id = 90_000_001_i64;
    let mut dyn_attrs = HashMap::new();
    dyn_attrs.insert(attr_ids::MAX_VELOCITY, 350.0);
    dyn_attrs.insert(attr_ids::DAMAGE_MULTIPLIER, 1.25);

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
                source_mutaplasmid_id: Some(47841),
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
// MN-18: Mining Missions, Ore/Mineral Market & Mining Goals from Ledger
// ============================================================================
#[tokio::test]
async fn test_mn18_mining_missions_market_and_goals() {
    // parity: feat:pve.mission-mining, feat:mining.mining-missions, feat:mining.mining-market, feat:mining.mining-goals, rpc:agentMgr.GetAgents, rpc:marketProxy.GetOrders, rpc:seasonManager.claim_goal_reward, rpc:seasonManager.get_character_progress_by_character_id
    eve_testkit::attest(&["feat:pve.mission-mining", "feat:mining.mining-missions", "feat:mining.mining-market", "feat:mining.mining-goals"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let mut sess = create_auth_session(&ctx, 1005).await;
    let kw = HashMap::new();

    let char_id = 90_000_001_i64;
    let now = ctx.now_unix();

    let veldspar_type = ctx.sde.types.get(&1230).expect("Veldspar type");
    let tritanium_type = ctx.sde.types.get(&34).expect("Tritanium type");
    let comp_veldspar_type = ctx.sde.types.get(&28432).expect("Compressed Veldspar type");

    assert!((veldspar_type.volume - 0.1).abs() < 1e-4);
    assert!((tritanium_type.volume - 0.01).abs() < 1e-4);
    assert!((comp_veldspar_type.volume - 0.15).abs() < 1e-4);

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
                type_id: 1230,
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
                type_id: 34,
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

    let orders_res = ctx
        .dispatch_rpc(&mut sess, "marketProxy", None, "GetOrders", &[PyRep::Integer(1230)], &kw, None)
        .await
        .expect("marketProxy.GetOrders for Veldspar");

    let PyRep::Tuple(tuple_orders) = orders_res else {
        panic!("Expected (sells, buys) Tuple from GetOrders");
    };
    assert!(tuple_orders[0].is_crowset());
    assert!(!tuple_orders[0].crowset_rows().unwrap().is_empty());

    let agents_rep = ctx
        .dispatch_rpc(&mut sess, "agentMgr", None, "GetAgents", &[PyRep::Integer(1), PyRep::Integer(23)], &kw, None)
        .await
        .expect("agentMgr.GetAgents for L1 Mining");
    let PyRep::Rowset { rows: mining_agents, .. } = agents_rep else {
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

    let agent_oid = bind_service_oid(&ctx, &mut sess, "agentMgr", PyRep::Integer(m_agent_id)).await;

    let offer_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("agentMgr.DoAction(RequestMission)");
    let mission_id = offer_res.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;

    let accept_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(3)], &kw, None)
        .await
        .expect("agentMgr.DoAction(Accept)");

    let PyRep::KeyVal(accept_kv) = accept_res else {
        panic!("Expected KeyVal from DoAction(Accept)");
    };
    assert_eq!(accept_kv["state"].as_str(), Some("Accepted"));

    let pocket_ast_id = 910_000_000 + (mission_id as i64) * 1000 + 1;
    let handle = ctx.systems.get_or_spawn(system_id);
    let pocket_ast = handle.get_asteroid_state(pocket_ast_id).await.unwrap().expect("pocket asteroid");
    assert_eq!(pocket_ast.type_id, 1230);
    let (_, pocket_ball) = handle.get_ball_snapshot(pocket_ast_id).await.unwrap().unwrap();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.retain(|_, it| !(it.owner_id == char_id && it.type_id == 1230));
    });

    let incomplete = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(6)], &kw, None)
        .await;
    assert_eq!(incomplete.err().unwrap().code, "MissionIncomplete");

    let m_ship_id = 720_000_080_i64;
    let m_laser_id = 720_000_081_i64;
    ctx.db.with_mut_snapshot(|snap| {
        let skills = snap.skills.entry(char_id).or_default();
        for sid in [3386u32, 3410, 17940] {
            skills.insert(sid, eve_db::TrainedSkillRecord { type_id: sid, level: 5, skill_points: 500_000.0 });
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.active_ship_id = m_ship_id;
        }
        snap.items.insert(
            m_ship_id,
            InventoryItemRecord {
                item_id: m_ship_id,
                type_id: 17478,
                owner_id: char_id,
                location_id: system_id as i64,
                flag: InventoryFlag::Hangar,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            m_laser_id,
            InventoryItemRecord {
                item_id: m_laser_id,
                type_id: 17482,
                owner_id: char_id,
                location_id: m_ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });
    sess.station_id = None;
    sess.ship_id = Some(m_ship_id);
    handle
        .spawn_ball(Ball {
            id: m_ship_id,
            type_id: 17478,
            position: pocket_ball.position + DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 130.0,
            inertia: 1.0,
            mass: 20_000_000.0,
            radius: 150.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .unwrap();
    let mine_pocket_c1 = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, m_laser_id, pocket_ast_id)
        .await
        .expect("first mining cycle");
    assert_eq!(mine_pocket_c1.units_delivered, 2_964);
    assert_eq!(mine_pocket_c1.stop, None);
    let mine_pocket_c2 = crate::mining_cycle_engine::mine_once(&ctx, &mut sess, m_laser_id, pocket_ast_id)
        .await
        .expect("second mining cycle");
    assert_eq!(mine_pocket_c2.units_delivered, 2_036);
    assert_eq!(
        mine_pocket_c2.stop,
        Some(eve_server::services::mining_cycle::MiningStopReason::AsteroidDepleted)
    );
    let mined_mission_units = mine_pocket_c1.units_delivered + mine_pocket_c2.units_delivered;
    assert_eq!(mined_mission_units, 5_000);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(m_station_id);
        }
    });
    sess.station_id = Some(m_station_id);

    let comp_res = ctx
        .dispatch_rpc(&mut sess, "agentMgr", Some(agent_oid), "DoAction", &[PyRep::Integer(6)], &kw, None)
        .await
        .expect("agentMgr.DoAction(Complete)");
    let PyRep::KeyVal(comp_kv) = comp_res else { panic!() };
    assert_eq!(comp_kv["missionID"].as_i64(), Some(mission_id as i64));

    let remaining_ore: i32 = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|it| it.owner_id == char_id && it.location_id == m_ship_id && it.type_id == 1230)
            .map(|it| it.quantity)
            .sum()
    });
    assert_eq!(remaining_ore, 4_500);

    let _ = now;
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

    let PyRep::KeyVal(g_kv) = goal_res else { panic!() };
    assert_eq!(g_kv["goalID"].as_i64(), Some(2));
    assert_eq!(g_kv["currentProgress"].as_i64(), Some(mined_mission_units));
    assert_eq!(g_kv["verifiedFromLedger"], PyRep::Bool(true));

    let claim_goal_res = ctx
        .dispatch_rpc(&mut sess, "seasonManager", None, "claim_goal_reward", &[PyRep::Integer(2)], &kw, None)
        .await
        .expect("seasonManager.claim_goal_reward");
    assert_eq!(claim_goal_res.dict_get("spReward").and_then(|v| v.as_f64()), Some(50_000.0));
}
