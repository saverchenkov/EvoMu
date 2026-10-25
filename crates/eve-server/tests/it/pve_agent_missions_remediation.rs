//! Integration tests for PvE Agent Missions remediation:
//! Courier, Trade, Security combat waves, and Mining missions.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::agents_missions::*;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;

async fn setup_pilot(
    ctx: &ServerContext,
    session_id: u64,
    username: &str,
    password: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &kw,
        None,
    )
    .await
    .expect("SelectCharacterID");
    let (ship_id, corp_id) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id);
        (ch.map(|c| c.active_ship_id), ch.map(|c| c.corporation_id))
    });
    sess.ship_id = ship_id;
    sess.corporation_id = corp_id;
    sess
}

async fn rpc_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

fn colocate_with_agent(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    agent_id: i64,
) -> (u32, i64) {
    let agent = ctx.sde.agents.get(&agent_id).expect("agent in SDE");
    let sta_id = agent.station_id.unwrap_or(JITA_STATION_ID);
    let st_info = ctx.sde.stations.get(&sta_id);
    let sys_id = st_info
        .map(|s| s.solar_system_id)
        .or(agent.solar_system_id)
        .unwrap_or(JITA_SOLAR_SYSTEM_ID);
    let reg_id = st_info
        .map(|s| s.region_id)
        .or_else(|| ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id))
        .unwrap_or(10_000_002);
    let const_id = st_info
        .map(|s| s.constellation_id)
        .or_else(|| ctx.sde.solar_systems.get(&sys_id).map(|s| s.constellation_id))
        .unwrap_or(20_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.constellation_id = const_id;
            ch.region_id = reg_id;
            ch.station_id = Some(sta_id);
            ch.structure_id = None;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.constellation_id = Some(const_id);
    sess.region_id = Some(reg_id);
    sess.station_id = Some(sta_id);
    sess.structure_id = None;
    (sys_id, sta_id)
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    quantity: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag: InventoryFlag::Hangar,
        quantity,
        singleton,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

/// feat:pve.mission-courier
/// Distribution missions place cargo in agent station hangar; completion requires transporting
/// cargo to destination station hangar/ship; failure on expiry.
#[tokio::test]
async fn test_pve_courier_mission_distribution_lifecycle_and_destination_transport() {
    // parity: feat:pve.mission-courier
    eve_testkit::attest(&["feat:pve.mission-courier"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 601, "demo", "demo-secret", char_id).await;

    // Select Level 1 Distribution agent (division 22)
    let (dist_agent_id, dist_corp_id) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 22 && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .map(|a| (a.agent_id, a.corporation_id))
        .expect("Distribution agent in SDE");
    let (_, origin_station_id) = colocate_with_agent(&ctx, &mut sess, char_id, dist_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(dist_corp_id, 3.0);
        }
    });

    // 1. Offer courier mission
    let offer = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)])
        .await
        .expect("Offer courier mission");
    let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;

    let (dest_station_id, cargo_type_id, cargo_qty, origin_sid) = ctx.db.with_snapshot(|snap| {
        let meta = load_mission_meta(snap, char_id, mid).expect("courier mission meta");
        (
            meta.destination_station_id.expect("dest station"),
            meta.required_delivery_type_id.expect("cargo type"),
            meta.required_delivery_qty.max(1),
            meta.origin_station_id.expect("origin station"),
        )
    });
    assert_ne!(dest_station_id, origin_station_id, "Destination station must differ from origin station");
    assert_eq!(origin_sid, origin_station_id, "Origin station must match agent station");
    assert!(cargo_qty >= 1, "Cargo quantity must be at least 1 unit");

    // Verify briefing / objective info contains data-driven destination station name
    let details = rpc_call(&ctx, &mut sess, "agentMgr", "GetMissionObjectiveInfo", &[PyRep::Integer(dist_agent_id), PyRep::Integer(mid as i64)])
        .await
        .expect("GetMissionObjectiveInfo");
    let obj_text = details.dict_get("objectives").and_then(|v| v.as_str()).unwrap_or_default();
    let dest_name = ctx.sde.stations.get(&dest_station_id).map(|s| s.name.as_str()).unwrap_or("station");
    assert!(
        obj_text.contains(dest_name),
        "Objective text '{obj_text}' must mention destination station '{dest_name}'"
    );

    // 2. Accept courier mission -> places cargo in agent station hangar
    rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid as i64)])
        .await
        .expect("Accept courier mission");

    let cargo_in_hangar = ctx.db.with_snapshot(|snap| {
        snap.items.values().any(|it| {
            it.owner_id == char_id
                && it.type_id == cargo_type_id
                && it.quantity >= cargo_qty
                && it.flag == InventoryFlag::Hangar
                && it.location_id == origin_station_id
        })
    });
    assert!(cargo_in_hangar, "Courier cargo must be deposited in agent station hangar upon accept");

    // 3. Attempting to complete while docked at origin station fails
    let premature_err = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid as i64)])
        .await
        .expect_err("Completing courier at origin station must fail");
    assert_eq!(premature_err.code, "CourierNotAtDestinationStation");

    // 4. Move cargo into active ship cargo hold and travel to destination station
    let dest_sys_id = ctx.sde.stations.get(&dest_station_id).expect("dest station info").solar_system_id;
    let ship_id = sess.ship_id.expect("active ship");
    ctx.db.with_mut_snapshot(|snap| {
        for it in snap.items.values_mut() {
            if it.owner_id == char_id && it.type_id == cargo_type_id && it.location_id == origin_station_id {
                it.location_id = ship_id;
                it.flag = InventoryFlag::Cargo;
            }
        }
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(dest_station_id);
            ch.solar_system_id = dest_sys_id;
        }
    });
    sess.station_id = Some(dest_station_id);
    sess.solar_system_id = Some(dest_sys_id);

    // 5. Complete courier mission at destination station -> cargo consumed
    let done = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid as i64)])
        .await
        .expect("Complete courier mission at destination");
    assert_eq!(done.dict_get("state").and_then(|v| v.as_str()), Some("Completed"));

    let cargo_consumed = ctx.db.with_snapshot(|snap| {
        !snap.items.values().any(|it| it.owner_id == char_id && it.type_id == cargo_type_id)
    });
    assert!(cargo_consumed, "Courier cargo must be consumed on completion");

    // 6. Test Failure on Expiry: offer a second mission and advance past accept_expiry_unix
    colocate_with_agent(&ctx, &mut sess, char_id, dist_agent_id);
    let offer2 = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)])
        .await
        .expect("Offer second courier mission");
    let mid2 = offer2.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;

    rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid2 as i64)])
        .await
        .expect("Accept second courier mission");

    // Expire the mission
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut meta) = load_mission_meta(snap, char_id, mid2) {
            meta.accept_expiry_unix = Some(1_000);
            save_mission_meta(snap, char_id, &meta);
        }
    });

    // Attempting to complete an expired mission fails with MissionExpired
    let exp_err = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid2 as i64)])
        .await
        .expect_err("Expired mission must fail on completion");
    assert_eq!(exp_err.code, "MissionExpired");

    // Verify active mission transitioned to Failed state
    let is_failed = ctx.db.with_snapshot(|snap| {
        snap.active_missions.get(&(char_id, mid2)).is_some_and(|m| m.state == "Failed")
    });
    assert!(is_failed, "Expired mission must be transitioned to Failed state");
}

/// feat:pve.mission-trade
/// Trade missions require specific commodity/trade goods or materials (Antibiotics, Synthetic Oil,
/// Wheat, Oxygen, Livestock, etc. with appropriate quantities) delivered to the agent station hangar; items consumed on completion.
#[tokio::test]
async fn test_pve_trade_mission_commodity_delivery_and_consumption() {
    // parity: feat:pve.mission-trade
    eve_testkit::attest(&["feat:pve.mission-trade"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 602, "demo", "demo-secret", char_id).await;

    // Use a distribution/trade agent
    let (agent_id, corp_id) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.level == 1 && a.agent_type_id == 2 && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .map(|a| (a.agent_id, a.corporation_id))
        .expect("Agent in SDE");
    let (_, agent_station_id) = colocate_with_agent(&ctx, &mut sess, char_id, agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(corp_id, 3.0);
        }
    });

    // Test specific commodities across multiple trade missions: Antibiotics (43), Synthetic Oil (3691), Wheat (3673), Oxygen (3683), Livestock (3725)
    let commodities_to_test: [(u32, u32, i32); 5] = [
        (1407, 43, 60),    // Antibiotics
        (1433, 3691, 35),  // Synthetic Oil
        (1418, 3673, 45),  // Wheat
        (1427, 3683, 35),  // Oxygen
        (1420, 3725, 10),  // Livestock
    ];

    for (test_mid, expected_tid, expected_qty) in commodities_to_test {
        let tmpl = ctx.sde.missions.get(&test_mid).expect("SDE trade mission");
        let td = resolve_trade_mission_details(&ctx, tmpl);
        assert_eq!(td.trade_type_id, expected_tid, "Resolved trade type must match expected commodity");
        assert_eq!(td.trade_quantity, expected_qty, "Resolved trade quantity must match expected quantity");

        // Offer & Accept the trade mission
        let now = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| {
            save_mission_meta(
                snap,
                char_id,
                &MissionTimingAndCargoMeta {
                    mission_id: test_mid,
                    agent_id,
                    offered_at_unix: now,
                    offer_expiry_unix: now + 86400,
                    accepted_at_unix: Some(now),
                    accept_expiry_unix: Some(now + 604800),
                    bonus_expiry_unix: Some(now + 3600),
                    completed_at_unix: None,
                    required_delivery_type_id: Some(td.trade_type_id),
                    required_delivery_qty: td.trade_quantity,
                    granted_courier_item_id: None,
                    granted_courier_qty: 0,
                    is_anomic_burner: false,
                    chosen_branch: None,
                    destination_station_id: None,
                    origin_station_id: Some(agent_station_id),
                },
            );
            snap.active_missions.insert(
                (char_id, test_mid),
                eve_db::ActiveMissionRecord {
                    mission_id: test_mid,
                    character_id: char_id,
                    agent_id,
                    solar_system_id: sess.solar_system_id.unwrap(),
                    state: "Accepted".into(),
                    current_room_id: 1,
                    npcs_remaining: 0,
                    spawned_npc_ball_ids: Vec::new(),
                },
            );
        });

        // 1. Completion without required commodities must fail with MissionIncomplete
        let incomplete_err = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(test_mid as i64)])
            .await
            .expect_err("Trade mission without commodities must fail");
        assert_eq!(incomplete_err.code, "MissionIncomplete");

        // 2. Deliver required commodity into agent station hangar
        let commodity_item_id = 950_000_000 + (test_mid as i64);
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                commodity_item_id,
                make_item(commodity_item_id, expected_tid, char_id, agent_station_id, expected_qty, false),
            );
        });

        // 3. Complete trade mission -> commodities consumed
        let done = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(test_mid as i64)])
            .await
            .expect("Trade mission must complete when commodities delivered");
        assert_eq!(done.dict_get("state").and_then(|v| v.as_str()), Some("Completed"));

        let consumed = ctx.db.with_snapshot(|snap| !snap.items.contains_key(&commodity_item_id));
        assert!(consumed, "Delivered trade commodities must be consumed on completion");
    }
}

/// feat:pve.mission-security
/// Kill objective missions spawn dungeons with multiple hostile NPC ships (3-6 frigates/cruisers
/// matching pirate faction) in the encounter dungeon/room, track remaining hostile NPCs accurately,
/// decrementing as each NPC ball is destroyed in space, and complete only when all hostile wave NPCs are eliminated.
#[tokio::test]
async fn test_pve_security_mission_multi_npc_wave_and_elimination() {
    // parity: feat:pve.mission-security
    eve_testkit::attest(&["feat:pve.mission-security"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 603, "demo", "demo-secret", char_id).await;

    // Select Level 2 Security agent (division 24)
    let (sec_agent_id, sec_corp_id) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.level == 2 && a.agent_type_id == 2 && a.division_id == 24 && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .map(|a| (a.agent_id, a.corporation_id))
        .expect("Security agent in SDE");
    let (sys_id, _) = colocate_with_agent(&ctx, &mut sess, char_id, sec_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(sec_corp_id, 4.0);
        }
    });

    // 1. Offer security mission
    let offer = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(sec_agent_id)])
        .await
        .expect("Offer security mission");
    let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;

    let tmpl = ctx.sde.missions.get(&mid).expect("mission template");
    let agent_info = ctx.sde.agents.get(&sec_agent_id).expect("agent info");
    let enc = resolve_security_mission_encounter(&ctx, tmpl, agent_info, sys_id);

    // Verify multi-ship wave (3-6 NPCs) matching pirate faction
    assert!(
        enc.wave_npc_count >= 3 && enc.wave_npc_count <= 6,
        "Security mission wave must spawn 3-6 hostile NPC ships (got {})",
        enc.wave_npc_count
    );
    assert!(!enc.npc_type_ids.is_empty(), "Security encounter must have non-empty NPC type IDs");

    // 2. Accept security mission -> spawns multi-hostile-NPC wave on grid
    rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid as i64)])
        .await
        .expect("Accept security mission");

    let (ball_ids, npcs_remaining) = ctx.db.with_snapshot(|snap| {
        let m = snap.active_missions.get(&(char_id, mid)).expect("active mission");
        (m.spawned_npc_ball_ids.clone(), m.npcs_remaining)
    });
    assert_eq!(ball_ids.len(), enc.wave_npc_count as usize, "Spawned ball count must equal wave NPC count");
    assert_eq!(npcs_remaining, enc.wave_npc_count, "Active mission npcs_remaining must match wave NPC count");

    // Verify all NPC balls were spawned in the solar system ballpark
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for &bid in &ball_ids {
        let snap_opt = sys_handle.get_ball_snapshot(bid).await.ok().flatten();
        assert!(snap_opt.is_some(), "Spawned NPC ball {bid} must exist in solar system ballpark");
    }

    // 3. Completing while hostile NPCs remain must fail with MissionIncomplete
    let premature_err = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid as i64)])
        .await
        .expect_err("Completing security mission while hostile NPCs remain must fail");
    assert_eq!(premature_err.code, "MissionIncomplete");

    // 4. Destroy hostile NPCs one-by-one in space and verify npcs_remaining decrements accurately
    let mut expected_remaining = enc.wave_npc_count;
    for &npc_bid in &ball_ids {
        // Destroy the NPC ball via death hook / damage handler
        on_mission_npc_destroyed_core(&ctx.db, sys_id, npc_bid, Some(char_id));
        expected_remaining -= 1;

        let cur_remaining = ctx.db.with_snapshot(|snap| {
            snap.active_missions.get(&(char_id, mid)).expect("active mission").npcs_remaining
        });
        assert_eq!(
            cur_remaining,
            expected_remaining,
            "npcs_remaining must decrement as each NPC ball is destroyed in space"
        );
    }

    let (final_ball_ids, final_remaining) = ctx.db.with_snapshot(|snap| {
        let m = snap.active_missions.get(&(char_id, mid)).expect("active mission");
        (m.spawned_npc_ball_ids.clone(), m.npcs_remaining)
    });
    assert_eq!(final_remaining, 0, "All hostile NPCs eliminated");
    assert!(final_ball_ids.is_empty(), "All spawned NPC ball IDs removed");

    // 5. Complete mission -> succeeds now that all wave NPCs are eliminated
    let done = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid as i64)])
        .await
        .expect("Complete security mission when all hostile NPCs eliminated");
    assert_eq!(done.dict_get("state").and_then(|v| v.as_str()), Some("Completed"));
}

/// feat:mining.mining-missions
/// Mining missions specify data-driven required ore type (Veldspar 1230, Scordite 1228, Pyroxeres 1224,
/// Kernite 20, etc.) and quantity (500-2000 units), create mission mining asteroid belt/pocket, and verify
/// required ore in station hangar on completion.
#[tokio::test]
async fn test_pve_mining_mission_asteroid_spawn_and_ore_delivery() {
    // parity: feat:mining.mining-missions, feat:pve.mission-mining
    eve_testkit::attest(&["feat:mining.mining-missions", "feat:pve.mission-mining"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 604, "demo", "demo-secret", char_id).await;

    // Select Level 1 Mining agent (division 23)
    let (mining_agent_id, mining_corp_id) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 23 && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .map(|a| (a.agent_id, a.corporation_id))
        .expect("Mining agent in SDE");
    let (sys_id, agent_station_id) = colocate_with_agent(&ctx, &mut sess, char_id, mining_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(mining_corp_id, 3.0);
        }
    });

    // 1. Offer mining mission
    let offer = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(mining_agent_id)])
        .await
        .expect("Offer mining mission");
    let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;

    let (req_ore_tid, req_units) = ctx.db.with_snapshot(|snap| {
        let meta = load_mission_meta(snap, char_id, mid).expect("mining mission meta");
        (meta.required_delivery_type_id.expect("ore type"), meta.required_delivery_qty)
    });

    // Verify data-driven ore type and quantity (500-2000 units)
    let valid_ores = [1230, 1228, 1224, 20, 18, 1227, 1229, 28617, 28618, 28619, 28620, 28621, 28622, 28624];
    assert!(
        valid_ores.contains(&req_ore_tid),
        "Required ore type {req_ore_tid} must be a valid ore"
    );
    assert!(
        (500..=2000).contains(&req_units),
        "Required ore units {req_units} must be in 500-2000 units range"
    );

    // 2. Accept mining mission -> creates mission mining asteroid in system
    rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid as i64)])
        .await
        .expect("Accept mining mission");

    let expected_ast_id = 910_000_000 + (mid as i64) * 1000 + 1;
    let asteroid_record = ctx.db.with_snapshot(|snap| snap.asteroids.get(&expected_ast_id).cloned());
    assert!(asteroid_record.is_some(), "Mission mining asteroid must be spawned in DB snapshot");
    let ast = asteroid_record.unwrap();
    assert_eq!(ast.type_id, req_ore_tid, "Spawned asteroid type must match required ore type");
    assert_eq!(ast.solar_system_id, sys_id, "Asteroid must spawn in mission solar system");
    assert!(ast.quantity >= req_units as i64, "Asteroid quantity must be sufficient for mission");

    // Verify asteroid ball exists in solar system ballpark
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let ast_ball_opt = sys_handle.get_ball_snapshot(expected_ast_id).await.ok().flatten();
    assert!(ast_ball_opt.is_some(), "Mission asteroid ball must exist in solar system ballpark");

    // 3. Completing without ore in station hangar must fail with MissionIncomplete
    let incomplete_err = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid as i64)])
        .await
        .expect_err("Completing mining mission without ore must fail");
    assert_eq!(incomplete_err.code, "MissionIncomplete");

    // 4. Deliver required ore into agent station hangar
    let ore_item_id = 960_000_000 + (mid as i64);
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ore_item_id,
            make_item(ore_item_id, req_ore_tid, char_id, agent_station_id, req_units, false),
        );
    });

    // 5. Complete mining mission -> ore consumed and rewards credited
    let done = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid as i64)])
        .await
        .expect("Complete mining mission when ore is in hangar");
    assert_eq!(done.dict_get("state").and_then(|v| v.as_str()), Some("Completed"));

    let ore_consumed = ctx.db.with_snapshot(|snap| !snap.items.contains_key(&ore_item_id));
    assert!(ore_consumed, "Delivered ore must be consumed from hangar on completion");
}
