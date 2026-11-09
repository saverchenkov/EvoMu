//! Agents, Missions, Standings, and Loyalty Point Store (`E-5`) complete parity integration tests.

use crate::common;
use eve_db::{
    ActiveMissionRecord, InventoryFlag, InventoryItemRecord, TypeId,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::agents_missions::*;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;

async fn setup_pilot(ctx: &ServerContext, session_id: u64, username: &str, password: &str, char_id: i64) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(session_id);
    common::login_pilot(ctx, &mut sess, username, password).await;
    ctx.dispatch_rpc(&mut sess, "charUnboundMgr", None, "SelectCharacterID", &[PyRep::Integer(char_id)], &kw, None).await.expect("SelectCharacterID");
    let (ship_id, corp_id) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id);
        (ch.map(|c| c.active_ship_id), ch.map(|c| c.corporation_id))
    });
    sess.ship_id = ship_id;
    sess.corporation_id = corp_id;
    sess
}

async fn rpc_call(ctx: &ServerContext, sess: &mut SessionState, service: &str, method: &str, args: &[PyRep]) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn colocate_with_agent(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, agent_id: i64) -> (u32, i64) {
    let agent = ctx.sde.agents.get(&agent_id).expect("agent in SDE");
    let sta_id = agent.station_id.unwrap_or(JITA_STATION_ID);
    let st_info = ctx.sde.stations.get(&sta_id);
    let sys_id = st_info.map(|s| s.solar_system_id).or(agent.solar_system_id).unwrap_or(JITA_SOLAR_SYSTEM_ID);
    let reg_id = st_info.map(|s| s.region_id).or_else(|| ctx.sde.solar_systems.get(&sys_id).map(|s| s.region_id)).unwrap_or(10_000_002);
    let const_id = st_info.map(|s| s.constellation_id).or_else(|| ctx.sde.solar_systems.get(&sys_id).map(|s| s.constellation_id)).unwrap_or(20_000_001);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id; ch.constellation_id = const_id; ch.region_id = reg_id;
            ch.station_id = Some(sta_id); ch.structure_id = None;
            let ship_id = ch.active_ship_id;
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                ship.location_id = sta_id;
            }
        }
    });
    sess.solar_system_id = Some(sys_id); sess.constellation_id = Some(const_id);
    sess.region_id = Some(reg_id); sess.station_id = Some(sta_id); sess.structure_id = None;
    (sys_id, sta_id)
}

fn make_mission(char_id: i64, mission_id: u32, agent_id: i64, sys_id: u32, state: &str) -> ActiveMissionRecord {
    ActiveMissionRecord {
        mission_id, character_id: char_id, agent_id, solar_system_id: sys_id,
        state: state.into(), current_room_id: 1, npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
    }
}

fn make_item(item_id: i64, type_id: TypeId, owner_id: i64, loc_id: i64, qty: i32, singleton: bool) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id, type_id, owner_id, location_id: loc_id, flag: InventoryFlag::Hangar,
        quantity: qty, singleton, version: 1, ..Default::default()
    }
}

#[tokio::test]
async fn test_mission_archetypes_combat_courier_mining_and_burner() {
    // parity: feat:pve.anomic-burner-missions
    eve_testkit::attest(&["feat:pve.anomic-burner-missions"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 503, "demo", "demo-secret", char_id).await;

    let (dist_agent_id, dist_corp_id) = ctx.sde.agents.values()
        .filter(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 22 && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id).map(|a| (a.agent_id, a.corporation_id)).expect("Distribution agent in SDE");
    let (dist_sys_id, dist_station_id) = colocate_with_agent(&ctx, &mut sess, char_id, dist_agent_id);
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.standings.insert(dist_corp_id, 2.0); } });

    // 1. Courier Mission archetype offered by Distribution agent (division 22)
    let courier_offer = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)]).await.expect("Offer courier mission");
    let courier_mid = courier_offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;

    let (dest_station_id, courier_cargo_type, courier_cargo_qty, origin_sid) = ctx.db.with_snapshot(|snap| {
        let meta = load_mission_meta(snap, char_id, courier_mid).expect("courier mission meta");
        (meta.destination_station_id.expect("dest station"), meta.required_delivery_type_id.unwrap_or(3683), meta.required_delivery_qty.max(1), meta.origin_station_id.expect("origin station"))
    });
    assert_ne!(dest_station_id, dist_station_id, "Courier destination must differ from offer station");
    let agent_sde_station = ctx.sde.agents.get(&dist_agent_id).and_then(|a| a.station_id).unwrap();
    assert_eq!(origin_sid, agent_sde_station, "origin_station_id must match SDE station");

    // Accept courier mission -> grants cargo into pilot station hangar
    rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(courier_mid as i64)]).await.expect("Accept courier mission");
    let has_cargo = ctx.db.with_snapshot(|snap| snap.items.values().any(|it| it.owner_id == char_id && it.type_id == courier_cargo_type && it.quantity >= courier_cargo_qty && it.flag == InventoryFlag::Hangar && it.location_id == dist_station_id));
    assert!(has_cargo, "Courier cargo must be deposited into pilot station hangar");

    // Completing while still at agent station (not destination) must fail
    let not_at_dest = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(courier_mid as i64)]).await.expect_err("fail not at dest");
    assert_eq!(not_at_dest.code, "CourierNotAtDestinationStation");

    // Fly / dock at destination station
    let dest_sys_id = ctx.sde.stations.get(&dest_station_id).expect("dest station").solar_system_id;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = Some(dest_station_id); ch.solar_system_id = dest_sys_id;
            let ship_id = ch.active_ship_id;
            for it in snap.items.values_mut() {
                if it.owner_id == char_id && it.type_id == courier_cargo_type { it.location_id = ship_id; it.flag = InventoryFlag::Cargo; }
            }
        }
    });
    sess.station_id = Some(dest_station_id); sess.solar_system_id = Some(dest_sys_id);

    // Test: item with forged custom name "courier_cargo:<char>:<mid>X" is rejected on delivery (RV-COURIER-TAG-FORGE-01)
    let forged_id = 999_777_111;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(forged_id, make_item(forged_id, courier_cargo_type, char_id, dest_station_id, courier_cargo_qty, false));
        snap.item_custom_names.insert(forged_id, format!("courier_cargo:{char_id}:{courier_mid}X"));
        for it in snap.items.values_mut() {
            if it.owner_id == char_id && it.item_id != forged_id && it.type_id == courier_cargo_type { it.quantity = 0; }
        }
    });
    let forged_err = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(courier_mid as i64)]).await.expect_err("Forged tag must fail delivery");
    assert_eq!(forged_err.code, "MissionIncomplete");
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.remove(&forged_id); snap.item_custom_names.remove(&forged_id);
        for it in snap.items.values_mut() {
            if it.owner_id == char_id && it.type_id == courier_cargo_type { it.quantity = courier_cargo_qty; }
        }
    });

    // Complete courier mission at destination station -> consumes cargo
    let courier_done = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(courier_mid as i64)]).await.expect("Complete courier mission");
    assert_eq!(courier_done.dict_get("state").and_then(|v| v.as_str()), Some("Completed"));
    let cargo_consumed = ctx.db.with_snapshot(|snap| !snap.items.values().any(|it| it.owner_id == char_id && it.type_id == courier_cargo_type && it.flag == InventoryFlag::Cargo));
    assert!(cargo_consumed, "Courier cargo must be consumed on completion");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = dist_sys_id;
            ch.station_id = Some(dist_station_id);
        }
    });
    sess.solar_system_id = Some(dist_sys_id);
    sess.station_id = Some(dist_station_id);

    // 2. Trade Mission archetype: DERIVED required goods and quantity from template title heuristic
    // Template 1419 ("The Drill", DERIVED: type 21632, quantity 18)
    let trade_mid = 1419_u32;
    ctx.db.with_mut_snapshot(|snap| { snap.active_missions.insert((char_id, trade_mid), make_mission(char_id, trade_mid, dist_agent_id, dist_station_id as u32, "Accepted")); });
    let trade_err = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(trade_mid as i64)]).await.expect_err("Trade fail");
    assert_eq!(trade_err.code, "MissionIncomplete");
    ctx.db.with_mut_snapshot(|snap| { snap.next_entity_id += 1; let tid_item = snap.next_entity_id; snap.items.insert(tid_item, make_item(tid_item, 21632, char_id, dist_station_id, 18, false)); });
    let trade_done = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(trade_mid as i64)]).await.expect("Trade completes");
    assert_eq!(trade_done.dict_get("state").and_then(|v| v.as_str()), Some("Completed"));

    // 3. Mining Mission archetype: DERIVED required ore and units from template title heuristic
    // Mission 4802: "Bountiful Banidine", DERIVED: requires 500 units of Banidine (28617)
    let mining_mid = 4802_u32;
    ctx.db.with_mut_snapshot(|snap| { snap.active_missions.insert((char_id, mining_mid), make_mission(char_id, mining_mid, dist_agent_id, dist_station_id as u32, "Accepted")); });
    let mining_err = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mining_mid as i64)]).await.expect_err("Mining fail");
    assert_eq!(mining_err.code, "MissionIncomplete");
    ctx.db.with_mut_snapshot(|snap| { snap.next_entity_id += 1; let oid = snap.next_entity_id; snap.items.insert(oid, make_item(oid, 28617, char_id, dist_station_id, 500, false)); });
    let mining_done = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mining_mid as i64)]).await.expect("Mining completes");
    assert_eq!(mining_done.dict_get("state").and_then(|v| v.as_str()), Some("Completed"));

    // 4. Security Mission encounter & Anomic Burner Acceleration Gate
    let (agent_id, corp_id) = ctx.sde.agents.values()
        .find(|a| a.level == 4 && a.agent_type_id == 2 && a.division_id == 24 && a.solar_system_id.is_some())
        .map(|a| (a.agent_id, a.corporation_id)).expect("Level 4 Security agent in SDE");
    let (agent_sys_id, _) = colocate_with_agent(&ctx, &mut sess, char_id, agent_id);
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.standings.insert(corp_id, 6.0); } });

    let burner_offer = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)])
        .await.expect("Offer Anomic Burner");
    let burner_mid = burner_offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;

    // Resolve SDE encounter to check derived NPC spawn group types
    let burner_tmpl = ctx.sde.missions.get(&burner_mid).expect("mission template");
    let burner_agent = ctx.sde.agents.get(&agent_id).expect("agent");
    let enc = resolve_security_mission_encounter(&ctx, burner_tmpl, burner_agent, agent_sys_id);
    assert!(!enc.npc_type_ids.is_empty(), "Security encounter must have non-empty NPC type IDs");

    let npcs_expected = ctx.db.with_snapshot(|snap| snap.active_missions.get(&(char_id, burner_mid)).expect("active mission").npcs_remaining);
    assert_eq!(npcs_expected, enc.wave_npc_count);

    rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(burner_mid as i64)])
        .await.expect("Accept Anomic Burner mission");

    let ball_ids = ctx.db.with_snapshot(|snap| {
        let m = snap.active_missions.get(&(char_id, burner_mid)).expect("active mission");
        m.spawned_npc_ball_ids.clone()
    });
    assert_eq!(ball_ids.len(), enc.wave_npc_count as usize);

    // Independently derive expected NPC spawn group from SDE (R-MSN-TEST-02)
    let agent_faction = ctx
        .sde
        .npc_corporations
        .get(&burner_agent.corporation_id)
        .and_then(|c| c.faction_id)
        .unwrap_or(0);
    let target_ship_class = match burner_agent.level {
        1 => 1,
        2 | 3 => 2,
        _ => 3,
    };
    let independent_spawn_group = ctx
        .sde
        .npc_spawn_groups
        .values()
        .filter(|sg| {
            (sg.faction_id == agent_faction || sg.group_name.contains("Deadspace"))
                && !sg.npc_type_ids.is_empty()
        })
        .min_by_key(|sg| (sg.ship_class.abs_diff(target_ship_class), sg.spawn_group_id))
        .expect("Matching NPC spawn group in SDE");
    assert!(!independent_spawn_group.npc_type_ids.is_empty(), "SDE spawn group must have NPC types");

    // Verify spawned NPC types match expected SDE spawn group NPC type IDs derived independently (R-MSN-TEST-02)
    let sys_handle = ctx.systems.get_or_spawn(agent_sys_id);
    for bid in &ball_ids {
        let (_, ball) = sys_handle
            .get_ball_snapshot(*bid)
            .await
            .ok()
            .flatten()
            .expect("spawned NPC ball in system");
        assert!(
            independent_spawn_group.npc_type_ids.contains(&ball.type_id),
            "Spawned NPC type {} must match independently derived SDE spawn group (id={}, name={:?})",
            ball.type_id,
            independent_spawn_group.spawn_group_id,
            independent_spawn_group.group_name,
        );
        let sde_type = ctx.sde.types.get(&ball.type_id).expect("NPC type exists in SDE");
        assert!(sde_type.group_id > 0, "NPC type must have valid group");
    }

    // Completing before killing NPCs must fail
    let incomplete_err = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(burner_mid as i64)])
        .await.expect_err("Security mission cannot complete while NPCs remain");
    assert_eq!(incomplete_err.code, "MissionIncomplete");

    // 5. Test ship size restriction on acceleration gate
    let battleship_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let bsid = snap.next_entity_id;
        snap.items.insert(bsid, make_item(bsid, 638, char_id, agent_sys_id as i64, 1, true));
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.station_id = None; ch.active_ship_id = bsid; }
        if let Some(m) = snap.active_missions.get_mut(&(char_id, burner_mid)) { m.npcs_remaining = 0; }
        bsid
    });
    sess.station_id = None; sess.ship_id = Some(battleship_id);

    let gate_err = rpc_call(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(burner_mid as i64)])
        .await.expect_err("Battleship must be rejected by Anomic Burner acceleration gate");
    assert_eq!(gate_err.code, "ShipRestrictedByGate");

    ctx.db.with_mut_snapshot(|snap| { if let Some(ship) = snap.items.get_mut(&battleship_id) { ship.type_id = 587; } });
    let gate_ok = rpc_call(&ctx, &mut sess, "keeper", "ActivateAccelerationGate", &[PyRep::Integer(burner_mid as i64)])
        .await.expect("Frigate passes acceleration gate");
    assert_eq!(
        gate_ok.dict_get("currentRoomID").and_then(|v| v.as_i64()),
        Some(2)
    );
}

#[tokio::test]
async fn test_courier_mission_accept_quit_reclaims_cargo_regression() {
    // parity: feat:pve.mission-courier
    eve_testkit::attest(&["feat:pve.mission-courier"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 504, "demo", "demo-secret", char_id).await;

    let (dist_agent_id, dist_corp_id) = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 22 && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .map(|a| (a.agent_id, a.corporation_id))
        .expect("Distribution agent in SDE");
    let (_, dist_station_id) = colocate_with_agent(&ctx, &mut sess, char_id, dist_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(dist_corp_id, 2.0);
        }
    });

    let offer = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)])
        .await.expect("Offer courier mission");
    let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;

    let (cargo_tid, cargo_qty) = ctx.db.with_snapshot(|snap| {
        let meta = load_mission_meta(snap, char_id, mid).unwrap();
        (meta.required_delivery_type_id.unwrap(), meta.required_delivery_qty)
    });

    // 1. Accept courier mission -> grants cargo into player's station hangar (R-MSN-COURIER-HANGAR)
    rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid as i64)])
        .await.expect("Accept courier mission");

    let granted_cid = ctx.db.with_snapshot(|snap| {
        let meta = load_mission_meta(snap, char_id, mid).expect("meta");
        assert_eq!(meta.granted_courier_qty, cargo_qty);
        let cid = meta.granted_courier_item_id.expect("granted cargo ID");
        let it = snap.items.get(&cid).expect("granted cargo in snapshot");
        assert_eq!(it.location_id, dist_station_id, "Courier cargo must be placed in agent station hangar");
        assert_eq!(it.flag, InventoryFlag::Hangar, "Courier cargo must be placed in hangar");
        cid
    });

    // Seed player's own items of the same type in the same hangar
    let player_item_id = 999_888_777;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(player_item_id, InventoryItemRecord {
            item_id: player_item_id, type_id: cargo_tid, owner_id: char_id, location_id: dist_station_id,
            flag: InventoryFlag::Hangar, quantity: 50, singleton: false, version: 1, ..Default::default()
        });
    });

    // 2. Attempt SetLabel on courier cargo -> must fail and preserve tracking tag (R-MSN-MINT-03)
    let rename_err = rpc_call(&ctx, &mut sess, "invbroker", "SetLabel", &[PyRep::Integer(granted_cid), PyRep::String("Stripped".into())])
        .await.expect_err("SetLabel on courier cargo must fail");
    assert_eq!(rename_err.code, "CustomNameNotAllowed");

    // SetLabel anti-forgery check: attempting to assign courier_cargo: prefix must fail with CustomNameNotAllowed (RV-COURIER-TAG-FORGE-01)
    let forge_err = rpc_call(&ctx, &mut sess, "invbroker", "SetLabel", &[PyRep::Integer(player_item_id), PyRep::String("courier_cargo:test".into())])
        .await.expect_err("SetLabel with courier_cargo: prefix must fail");
    assert_eq!(forge_err.code, "CustomNameNotAllowed");

    // Calling Accept on an already "Accepted" mission after offer_expiry_unix -> must error with InvalidMissionState, keep state Accepted and cargo intact (RV-MSN-EXPIRE-STATE-01)
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut meta) = load_mission_meta(snap, char_id, mid) {
            meta.offer_expiry_unix = 1;
            save_mission_meta(snap, char_id, &meta);
        }
    });
    let reaccept_err = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid as i64)])
        .await.expect_err("Accepting already-accepted mission after offer expiry must fail");
    assert_eq!(reaccept_err.code, "InvalidMissionState");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_missions.get(&(char_id, mid)).map(|m| m.state.as_str()), Some("Accepted"));
        assert!(snap.items.contains_key(&granted_cid), "Courier cargo must remain intact");
    });

    // Attempt to sell courier cargo on market -> must fail with ItemCannotBeSold (RV-03)
    let sell_entry = PyRep::keyval([
        ("stationID", PyRep::Integer(dist_station_id)), ("typeID", PyRep::Integer(cargo_tid as i64)),
        ("itemID", PyRep::Integer(granted_cid)), ("price", PyRep::Real(100.0)),
        ("quantity", PyRep::Integer(1)),
    ]);
    let sell_err = rpc_call(&ctx, &mut sess, "marketProxy", "PlaceMultiSellOrder", &[PyRep::List(vec![sell_entry])])
        .await.expect_err("Market sell on courier cargo must fail");
    assert_eq!(sell_err.code, "ItemCannotBeSold");

    // Attempt to place courier cargo into contract -> must fail with ItemCannotBeContracted (RV-MINT-CONTRACT-01)
    let contract_args = vec![
        PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1440), PyRep::Integer(1),
        PyRep::Integer(dist_station_id), PyRep::Integer(dist_station_id),
        PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0),
        PyRep::String("Mint Contract".into()), PyRep::String("".into()),
        PyRep::List(vec![PyRep::Integer(granted_cid)]), PyRep::List(vec![]),
    ];
    let contract_err = ctx.dispatch_rpc(&mut sess, "contractProxy", None, "CreateContract", &contract_args, &HashMap::new(), None)
        .await.expect_err("Courier cargo contract creation must fail");
    assert_eq!(contract_err.code, "ItemCannotBeContracted");

    // Attempt to deliver courier cargo to corp hangar -> must fail with ItemCannotBeTransferred (RV-MINT-CONTRACT-01)
    let corp_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "DeliverToCorpHangar", &[PyRep::Integer(granted_cid), PyRep::Integer(1)], &HashMap::new(), None)
        .await.expect_err("Courier cargo transfer to corp hangar must fail");
    assert_eq!(corp_err.code, "ItemCannotBeTransferred");

    // 3. Attempt MultiMerge on courier cargo -> must fail (R-MSN-MINT-03)
    let merge_err = rpc_call(&ctx, &mut sess, "invbroker", "MultiMerge", &[
        PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(player_item_id), PyRep::Integer(granted_cid), PyRep::None])]),
    ]).await.expect_err("MultiMerge on courier cargo must fail");
    assert_eq!(merge_err.code, "StackFailed");

    // 4. Attempt StackAll at hangar -> must not merge courier cargo (R-MSN-MINT-03)
    let stack_kw = HashMap::from([
        ("locationID".into(), PyRep::Integer(dist_station_id)),
        ("flag".into(), PyRep::Integer(InventoryFlag::Hangar as i64)),
    ]);
    let stack_res = ctx.dispatch_rpc(&mut sess, "invbroker", None, "StackAll", &[], &stack_kw, None).await.expect("StackAll");
    assert_eq!(stack_res.dict_get("mergedCount").and_then(|v| v.as_i64()), Some(0));

    // 5. Split stack -> split child is tracked and reclaimed on quit (R-MSN-MINT-02, R-MSN-MINT-03)
    let (_, split_item) = ctx.db.split_stack(char_id, granted_cid, 1).expect("split courier cargo stack");
    let split_id = split_item.item_id;

    // Quit mission (Action 11)
    let quit_res = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(11), PyRep::Integer(mid as i64)])
        .await.expect("Quit courier mission");
    assert_eq!(quit_res.dict_get("state").and_then(|v| v.as_str()), Some("Quit"));

    // Verify granted cargo (both original and split child) was fully reclaimed and player's item preserved
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&granted_cid), "Original courier cargo must be reclaimed");
        assert!(!snap.items.contains_key(&split_id), "Split courier cargo must be reclaimed");
        assert_eq!(snap.items.get(&player_item_id).map(|i| i.quantity), Some(50), "Player's own cargo must be preserved");
    });

    // 6. Test calling AcceptMission on an "Offered" mission after offer expiry:
    // verify MissionOfferExpired error, persisted "Expired" state, and reclaimed cargo (RV-MSN-TEST-CLAIMS-01)
    let offer2 = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)]).await.expect("Offer mission 2");
    let mid2 = offer2.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut meta) = load_mission_meta(snap, char_id, mid2) {
            meta.offer_expiry_unix = 1;
            save_mission_meta(snap, char_id, &meta);
        }
    });
    let exp_err = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid2 as i64)])
        .await.expect_err("Accepting expired offer must fail");
    assert_eq!(exp_err.code, "MissionOfferExpired");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.active_missions.get(&(char_id, mid2)).map(|m| m.state.as_str()), Some("Expired"));
        let courier_tag2 = format!("courier_cargo:{char_id}:{mid2}");
        assert!(!snap.items.values().any(|it| snap.item_custom_names.get(&it.item_id) == Some(&courier_tag2)));
    });
}

#[tokio::test]
async fn test_non_courier_offers_decline_and_expiry_preserve_player_inventory_regression() {
    // parity: feat:pve.mission-decline, feat:pve.mission-expire
    eve_testkit::attest(&[
        "feat:pve.mission-decline",
        "feat:pve.mission-expire",
    ]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 505, "demo", "demo-secret", char_id).await;
    let ship_id = sess.ship_id.expect("active ship");

    let (tri_h, tri_c, ore_h, trade_c) = (888_001, 888_002, 888_003, 888_004);
    ctx.db.with_mut_snapshot(|snap| {
        let cargo_it = |id, tid, qty| {
            let mut it = make_item(id, tid, char_id, ship_id, qty, false);
            it.flag = InventoryFlag::Cargo;
            it
        };
        snap.items.insert(tri_h, make_item(tri_h, 34, char_id, JITA_STATION_ID, 25_000, false));
        snap.items.insert(tri_c, cargo_it(tri_c, 34, 500));
        snap.items.insert(ore_h, make_item(ore_h, 1230, char_id, JITA_STATION_ID, 5_000, false));
        snap.items.insert(trade_c, cargo_it(trade_c, 3683, 100));
    });

    // 1. Decline R&D offer -> player inventory must be 100% untouched (R-MSN-RECLAIM-01)
    let (rnd_agent_id, rnd_corp_id) = ctx.sde.agents.values()
        .filter(|a| a.agent_type_id == 4 && a.level == 1 && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .map(|a| (a.agent_id, a.corporation_id)).expect("R&D agent in SDE");
    colocate_with_agent(&ctx, &mut sess, char_id, rnd_agent_id);
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.standings.insert(rnd_corp_id, 5.0); }
        let entries = vec![RndResearchEntry {
            agent_id: rnd_agent_id, research_type_id: 11444, points_per_day: 120.0, initial_points: 50.0, started_unix: now,
        }];
        save_rnd_research_entries(snap, char_id, &entries);
    });
    let rnd_offer = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(rnd_agent_id)])
        .await.expect("Offer R&D mission");
    let rnd_mid = rnd_offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;

    let decline_res = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(9), PyRep::Integer(rnd_mid as i64)])
        .await.expect("Decline R&D offer");
    assert_eq!(decline_res.dict_get("state").and_then(|v| v.as_str()), Some("Declined"));

    ctx.db.with_snapshot(|snap| {
        assert_eq!((snap.items.get(&tri_h).map(|i| i.quantity), snap.items.get(&tri_c).map(|i| i.quantity)), (Some(25_000), Some(500)));
        assert_eq!((snap.items.get(&ore_h).map(|i| i.quantity), snap.items.get(&trade_c).map(|i| i.quantity)), (Some(5_000), Some(100)));
    });

    // 2. Expire Trade/Mining offer -> player inventory must remain 100% untouched (R-MSN-RECLAIM-01)
    let (dist_agent_id, dist_corp_id) = ctx.sde.agents.values()
        .filter(|a| a.agent_type_id == 2 && a.level == 1 && a.division_id == 22 && a.solar_system_id.is_some())
        .min_by_key(|a| a.agent_id)
        .map(|a| (a.agent_id, a.corporation_id)).expect("Distribution agent in SDE");
    colocate_with_agent(&ctx, &mut sess, char_id, dist_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.standings.insert(dist_corp_id, 5.0); }
    });
    let trade_offer = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)])
        .await.expect("Offer mission");
    let trade_mid = trade_offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(mut meta) = load_mission_meta(snap, char_id, trade_mid) {
            meta.offer_expiry_unix = 1;
            save_mission_meta(snap, char_id, &meta);
        }
    });
    let _ = rpc_call(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[]).await;

    ctx.db.with_snapshot(|snap| {
        assert_eq!((snap.items.get(&tri_h).map(|i| i.quantity), snap.items.get(&tri_c).map(|i| i.quantity)), (Some(25_000), Some(500)));
        assert_eq!((snap.items.get(&ore_h).map(|i| i.quantity), snap.items.get(&trade_c).map(|i| i.quantity)), (Some(5_000), Some(100)));
    });
}

#[tokio::test]
async fn test_courier_cargo_anti_stacking_in_invbroker_add_regression() {
    // parity: feat:pve.mission-courier
    eve_testkit::attest(&["feat:pve.mission-courier"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 506, "demo", "demo-secret", char_id).await;

    let (dist_agent_id, dist_corp_id) = ctx.sde.agents.values()
        .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 22 && a.solar_system_id.is_some())
        .map(|a| (a.agent_id, a.corporation_id)).expect("Distribution agent");
    let (dist_sys_id, dist_station_id) = colocate_with_agent(&ctx, &mut sess, char_id, dist_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) { ch.standings.insert(dist_corp_id, 2.0); }
    });

    let offer = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(dist_agent_id)])
        .await.expect("Offer courier mission");
    let mid = offer.dict_get("missionID").and_then(|v| v.as_i64()).unwrap() as u32;

    let (cargo_tid, cargo_qty) = ctx.db.with_snapshot(|snap| {
        let meta = load_mission_meta(snap, char_id, mid).unwrap();
        (meta.required_delivery_type_id.unwrap(), meta.required_delivery_qty)
    });

    // Accept courier mission -> grants cargo into player's station hangar
    rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid as i64)])
        .await.expect("Accept courier mission");

    let granted_cid = ctx.db.with_snapshot(|snap| {
        load_mission_meta(snap, char_id, mid).expect("meta").granted_courier_item_id.expect("cargo ID")
    });

    let ship_id = sess.ship_id.expect("active ship");
    let player_cargo_id = 99_887_702;
    let foreign_container_id = 99_887_799;
    ctx.db.with_mut_snapshot(|snap| {
        // Pilot already has 10 units of the same commodity untagged in their active ship cargo
        snap.items.insert(player_cargo_id, InventoryItemRecord {
            item_id: player_cargo_id, type_id: cargo_tid, owner_id: char_id, location_id: ship_id,
            flag: InventoryFlag::Cargo, quantity: 10, singleton: false, version: 1, ..Default::default()
        });
        snap.items.insert(foreign_container_id, InventoryItemRecord {
            item_id: foreign_container_id, type_id: 3293, owner_id: 99_999_999, location_id: dist_station_id,
            flag: InventoryFlag::Hangar, quantity: 1, singleton: true, version: 1, ..Default::default()
        });
    });

    // Block moving courier cargo into foreign container or corp division (RV-COURIER-ADD-MERGE-01)
    let foreign_err = rpc_call(&ctx, &mut sess, "invbroker", "Add", &[
        PyRep::Integer(granted_cid), PyRep::Integer(foreign_container_id), PyRep::Integer(InventoryFlag::Cargo as i64),
    ]).await.expect_err("foreign container add must fail");
    assert_eq!(foreign_err.code, "CannotDeliverCourierCargo");

    let corp_div_err = rpc_call(&ctx, &mut sess, "invbroker", "Add", &[
        PyRep::Integer(granted_cid), PyRep::Integer(dist_station_id), PyRep::Integer(116),
    ]).await.expect_err("corp div add must fail");
    assert_eq!(corp_div_err.code, "CannotDeliverCourierCargo");

    // RV-ADD-SPLIT-BEFORE-VALIDATE-01: Defer stack split in invbroker.Add until pre-move validations pass
    let pre_split_count = ctx.db.with_snapshot(|s| s.items.len());
    let mut split_kw = HashMap::new();
    split_kw.insert("qty".into(), PyRep::Integer(1));
    let split_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[
        PyRep::Integer(granted_cid), PyRep::Integer(dist_station_id), PyRep::Integer(116),
    ], &split_kw, None).await.expect_err("split add to corp div must fail");
    assert_eq!(split_err.code, "CannotDeliverCourierCargo");
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.len(), pre_split_count, "Stack must NOT be split on failed pre-move validation (RV-ADD-SPLIT-BEFORE-VALIDATE-01)");
        assert_eq!(snap.items.get(&granted_cid).map(|i| i.quantity), Some(cargo_qty));
    });

    // RV-COURIER-MULTIADD-BYPASS-01 & RV-CLAIMS-ERRCODES-TESTS-01: invbroker.MultiAdd into division 116
    let multi_div_err = rpc_call(&ctx, &mut sess, "invbroker", "MultiAdd", &[
        PyRep::List(vec![PyRep::Integer(granted_cid)]), PyRep::Integer(dist_station_id), PyRep::Integer(116),
    ]).await.expect_err("corp div multiadd must fail");
    assert_eq!(multi_div_err.code, "CannotDeliverCourierCargo");

    // RV-CLAIMS-ERRCODES-TESTS-01: invbroker.DeliverToCorpMember
    let dlv_err = rpc_call(&ctx, &mut sess, "invbroker", "DeliverToCorpMember", &[PyRep::Integer(granted_cid), PyRep::Integer(90_000_002)]).await.expect_err("deliver to corp member must fail");
    assert_eq!(dlv_err.code, "ItemCannotBeTransferred");

    // RV-CLAIMS-ERRCODES-TESTS-01: contractProxy.CreateContract
    let contract_args = vec![
        PyRep::Integer(1), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(1440), PyRep::Integer(1),
        PyRep::Integer(dist_station_id), PyRep::Integer(dist_station_id),
        PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(0.0),
        PyRep::String("Courier Test Contract".into()), PyRep::String("".into()),
        PyRep::List(vec![PyRep::Integer(granted_cid)]), PyRep::List(vec![]),
    ];
    let contract_err = ctx.dispatch_rpc(&mut sess, "contractProxy", None, "CreateContract", &contract_args, &HashMap::new(), None).await.expect_err("courier cargo contract creation must fail");
    assert_eq!(contract_err.code, "ItemCannotBeContracted");

    // RV-CLAIMS-ERRCODES-TESTS-01: trademgr.AddItem
    let partner_id = 90_000_002;
    let mut sess2 = setup_pilot(&ctx, 507, "other", "other-secret", partner_id).await;
    colocate_with_agent(&ctx, &mut sess2, partner_id, dist_agent_id);
    let tr_rep = rpc_call(&ctx, &mut sess, "trademgr", "InitiateTrade", &[PyRep::Integer(partner_id)]).await.expect("init trade");
    let tr_id = tr_rep.dict_get("tradeContainerID").and_then(|v| v.as_i64()).unwrap();
    let tr_err = rpc_call(&ctx, &mut sess, "trademgr", "AddItem", &[PyRep::Integer(tr_id), PyRep::Integer(granted_cid), PyRep::Integer(1)]).await.expect_err("trademgr.AddItem courier cargo must fail");
    assert_eq!(tr_err.code, "InvalidTradeItem");

    // Move courier cargo into ship cargo via invbroker.Add
    let add_res = rpc_call(&ctx, &mut sess, "invbroker", "Add", &[PyRep::Integer(granted_cid), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)]).await.expect("Move courier cargo into ship cargo");
    assert_eq!(add_res.dict_get("itemID").and_then(|v| v.as_i64()), Some(granted_cid));

    // RV-CLAIMS-ERRCODES-TESTS-01: ship.Jettison while in space
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.station_id = None; } });
    sess.station_id = None; sess.structure_id = None;
    let jet_err = ctx.dispatch_rpc(&mut sess, "ship", None, "Jettison", &[PyRep::List(vec![PyRep::Integer(granted_cid)])], &HashMap::new(), None).await.expect_err("ship.Jettison courier cargo must fail");
    assert_eq!(jet_err.code, "ItemCannotBeJettisoned");
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = dist_sys_id;
            ch.station_id = Some(dist_station_id);
        }
    });
    sess.solar_system_id = Some(dist_sys_id);
    sess.station_id = Some(dist_station_id);

    // Verify courier cargo is NOT merged into the untagged stack; two distinct item records exist with courier tag.
    let courier_tag = format!("courier_cargo:{char_id}:{mid}");
    ctx.db.with_snapshot(|snap| {
        let player_item = snap.items.get(&player_cargo_id).expect("player cargo item exists");
        assert_eq!((player_item.quantity, player_item.location_id, player_item.flag), (10, ship_id, InventoryFlag::Cargo));
        assert!(!snap.item_custom_names.contains_key(&player_cargo_id));
        let courier_item = snap.items.get(&granted_cid).expect("courier item exists");
        assert_eq!((courier_item.quantity, courier_item.location_id, courier_item.flag), (cargo_qty, ship_id, InventoryFlag::Cargo));
        assert_eq!(snap.item_custom_names.get(&granted_cid).map(String::as_str), Some(courier_tag.as_str()));
    });

    // Quit mission and verify all granted courier units are cleanly reclaimed, while the pilot's 10 original untagged units remain untouched!
    let quit_res = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(11), PyRep::Integer(mid as i64)]).await.expect("Quit courier mission");
    assert_eq!(quit_res.dict_get("state").and_then(|v| v.as_str()), Some("Quit"));

    ctx.db.with_snapshot(|snap| {
        assert!(!snap.items.contains_key(&granted_cid), "Granted courier item must be reclaimed on quit");
        let player_item = snap.items.get(&player_cargo_id).expect("player cargo item still exists");
        assert_eq!((player_item.quantity, player_item.location_id), (10, ship_id), "Player's 10 original untagged units must remain untouched");
    });
}

#[tokio::test]
async fn test_invbroker_partial_split_container_and_multiadd_validation_regression() {
    // parity: feat:inventory.containers, feat:ships.cargo-capacity
    eve_testkit::attest(&["feat:inventory.containers", "feat:ships.cargo-capacity"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 508, "demo", "demo-secret", char_id).await;
    let ship_id = sess.ship_id.expect("active ship");

    let (container_id, large_stack_id, item_a_id, item_b_id) = (880_001, 880_002, 880_003, 880_004);
    // Small Standard Container (type 3293, capacity 100 m3). Tritanium (type 34, 0.01 m3/unit): 20k units = 200 m3 > 100 m3 cap.
    // Metal Scraps (type 15331, 5 m3/unit): 200 units = 1000 m3 > Corvette cargo cap (~135 m3).
    ctx.db.with_mut_snapshot(|snap| {
        for (id, tid, qty, sing) in [(container_id, 3293, 1, true), (large_stack_id, 34, 20_000, false), (item_a_id, 34, 50_000, false), (item_b_id, 34, 50_000, false)] {
            snap.items.insert(id, make_item(id, tid, char_id, JITA_STATION_ID, qty, sing));
        }
    });

    // 1. Partial split move into container succeeds when split volume fits even if full stack exceeds capacity (RV-ADD-PARTIAL-SPLIT-ORDER-01)
    let split_res = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(large_stack_id), PyRep::Integer(container_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &HashMap::from([("qty".into(), PyRep::Integer(5_000))]), None).await.expect("Partial split into container must succeed");
    let split_new_id = split_res.dict_get("itemID").and_then(|v| v.as_i64()).unwrap();
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.items.get(&large_stack_id).map(|i| i.quantity), Some(15_000));
        assert_eq!(snap.items.get(&split_new_id).map(|i| (i.quantity, i.location_id)), Some((5_000, container_id)));
    });

    // 2. Add rejects invalid quantity <= 0 (BF-ADD-QTY-CAPACITY-BYPASS-01)
    let qty_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "Add", &[PyRep::Integer(large_stack_id), PyRep::Integer(container_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &HashMap::from([("qty".into(), PyRep::Integer(0))]), None).await.expect_err("qty 0 rejected");
    assert_eq!(qty_err.code, "InvalidQuantity");
    // 3. MultiAdd rejects fitting module flags (RV-MULTIADD-BYPASS-VALIDATION-01)
    let fit_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "MultiAdd", &[PyRep::List(vec![PyRep::Integer(large_stack_id)]), PyRep::Integer(ship_id), PyRep::Integer(11)], &HashMap::new(), None).await.expect_err("MultiAdd cannot fit modules");
    assert_eq!(fit_err.code, "InvalidLocation");
    // 4. MultiAdd rejects active ship move (BF-MULTIADD-NONATOMIC-BYPASS-01)
    let act_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "MultiAdd", &[PyRep::List(vec![PyRep::Integer(ship_id)]), PyRep::Integer(JITA_STATION_ID), PyRep::Integer(InventoryFlag::Hangar as i64)], &HashMap::new(), None).await.expect_err("CannotMoveActiveShip");
    assert_eq!(act_err.code, "CannotMoveActiveShip");

    // 5. MultiAdd fails and rolls back when batch exceeds cargo capacity (RV-MULTIADD-BYPASS-VALIDATION-01)
    let cap_err = ctx.dispatch_rpc(&mut sess, "invbroker", None, "MultiAdd", &[PyRep::List(vec![PyRep::Integer(item_a_id), PyRep::Integer(item_b_id)]), PyRep::Integer(ship_id), PyRep::Integer(InventoryFlag::Cargo as i64)], &HashMap::new(), None).await.expect_err("MultiAdd exceeding capacity must fail");
    assert_eq!(cap_err.code, "NotEnoughCargoSpace");
    ctx.db.with_snapshot(|snap| {
        assert_eq!((snap.items.get(&item_a_id).map(|i| i.location_id), snap.items.get(&item_b_id).map(|i| i.location_id)), (Some(JITA_STATION_ID), Some(JITA_STATION_ID)));
    });
}


pub(crate) mod mission_lifecycle_and_standings {
    #[allow(unused_imports)]
    use super::*;

// Integration tests for Mission Lifecycle and Standings Formulas.

use crate::common;
use eve_db::{
    ActiveMissionRecord, TrainedSkillRecord, TypeId,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const CALDARI_NAVY_CORP_ID: i64 = 1_000_035;
const CALDARI_STATE_FACTION_ID: i64 = 500_001;
const CONNECTIONS_SKILL: TypeId = 3359;
const DIPLOMACY_SKILL: TypeId = 3357;
const CRIMINAL_CONNECTIONS_SKILL: TypeId = 3361;
const NEGOTIATION_SKILL: TypeId = 3356;
const SECURITY_CONNECTIONS_SKILL: TypeId = 3893;

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
    sess
}

async fn rpc_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

fn colocate_with_agent(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    agent_id: i64,
) -> (u32, i64) {
    let agent = ctx.sde.agents.get(&agent_id).expect("agent in SDE");
    let sys_id = agent.solar_system_id.unwrap_or(JITA_SOLAR_SYSTEM_ID);
    let sta_id = agent.station_id.unwrap_or(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = agent.station_id;
            ch.structure_id = None;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = agent.station_id;
    sess.structure_id = None;
    (sys_id, sta_id)
}

fn is_storyline(m: &eve_sde::MissionTemplate) -> bool {
    m.content_template
        .as_deref()
        .unwrap_or_default()
        .contains("Storyline")
}

fn find_agent_by_level_and_type(
    ctx: &ServerContext,
    level: u8,
    agent_type_id: u32,
) -> (i64, i64, Option<i64>) {
    let mut agents: Vec<_> = ctx.sde.agents.values().collect();
    agents.sort_by_key(|a| a.agent_id);
    agents
        .into_iter()
        .find(|a| a.level == level && a.agent_type_id == agent_type_id && a.solar_system_id.is_some())
        .map(|a| (a.agent_id, a.corporation_id, a.station_id))
        .expect("SDE agent with requested level and type must exist")
}

fn make_mission(char_id: i64, mission_id: u32, agent_id: i64, sys_id: u32, state: &str) -> ActiveMissionRecord {
    ActiveMissionRecord {
        mission_id,
        character_id: char_id,
        agent_id,
        solar_system_id: sys_id,
        state: state.into(),
        current_room_id: 1,
        npcs_remaining: 0,
        spawned_npc_ball_ids: Vec::new(),
        bookmark_id: None,
    }
}

#[tokio::test]
async fn test_mission_lifecycle_offer_accept_complete_decline_abandon_and_expiration() {
    // parity: feat:pve.mission-accept, feat:pve.mission-decline, feat:pve.mission-quit, feat:pve.mission-expire, feat:pve.mission-journal, feat:pve.mission-tracker
    eve_testkit::attest(&["feat:pve.mission-accept", "feat:pve.mission-decline", "feat:pve.mission-quit", "feat:pve.mission-expire", "feat:pve.mission-journal", "feat:pve.mission-tracker"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 502, "demo", "demo-secret", char_id).await;

    let (agent_id, corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 2);
    colocate_with_agent(&ctx, &mut sess, char_id, agent_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.clear();
            ch.standings.insert(corp_id, 2.0);
        }
        let sk = snap.skills.entry(char_id).or_default();
        sk.insert(
            NEGOTIATION_SKILL,
            TrainedSkillRecord {
                type_id: NEGOTIATION_SKILL,
                level: 3,
                skill_points: 8_000.0,
            },
        );
        sk.insert(
            SECURITY_CONNECTIONS_SKILL,
            TrainedSkillRecord {
                type_id: SECURITY_CONNECTIONS_SKILL,
                level: 4,
                skill_points: 45_255.0,
            },
        );
    });

    // 1. Request mission offer (action 1)
    let offer = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(agent_id)],
    )
    .await
    .expect("Offer mission");
    let mission_id = offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .expect("missionID") as u32;
    assert!(offer.dict_get("iskReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(offer.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);

    let journal = rpc_call(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[])
        .await
        .expect("GetMyJournalDetails");
    let PyRep::Tuple(journal_sections) = journal else {
        panic!("Expected tuple from GetMyJournalDetails");
    };
    let PyRep::List(missions_list) = &journal_sections[0] else {
        panic!("Expected missions list");
    };
    assert!(!missions_list.is_empty());

    // 2. First Decline within 4h window -> no standing penalty
    let decline_1 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(9), PyRep::Integer(mission_id as i64)],
    )
    .await
    .expect("First decline");
    assert_eq!(
        decline_1
            .dict_get("standingPenaltyApplied")
            .and_then(|v| v.as_bool()),
        Some(false)
    );

    // Re-offer and second Decline within 4h window -> applies standing penalty
    let offer_2 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(agent_id)],
    )
    .await
    .expect("Second offer");
    let mission_id_2 = offer_2
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;
    let standing_before_decline2 = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&corp_id).copied())
            .unwrap_or(0.0)
    });
    let decline_2 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(9), PyRep::Integer(mission_id_2 as i64)],
    )
    .await
    .expect("Second decline");
    assert_eq!(
        decline_2
            .dict_get("standingPenaltyApplied")
            .and_then(|v| v.as_bool()),
        Some(true)
    );
    let standing_after_decline2 = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.standings.get(&corp_id).copied())
            .unwrap_or(0.0)
    });
    assert!(standing_after_decline2 < standing_before_decline2);

    // 3. Offer, Accept (action 3), and Abandon (`RemoveOfferFromJournal`)
    let offer_3 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(agent_id)],
    )
    .await
    .expect("Third offer");
    let mission_id_3 = offer_3
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;
    let accept_3 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mission_id_3 as i64)],
    )
    .await
    .expect("Accept third mission");
    assert_eq!(
        accept_3.dict_get("state").and_then(|v| v.as_str()),
        Some("Accepted")
    );
    let bm3_id = accept_3.dict_get("bookmarkID").and_then(|v| v.as_i64()).unwrap_or(0);
    assert!(bm3_id > 0);
    assert!(ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bm3_id)));

    // R-P0-01 negative regression: insert pilot's personal bookmark in same system & another mission pocket bookmark
    let (personal_bm_id, other_bm_id) = ctx.db.with_mut_snapshot(|snap| {
        let m_sys = snap.active_missions[&(char_id, mission_id_3)].solar_system_id;
        let p_id = 991_001i64;
        let o_id = 991_002i64;
        snap.bookmarks.insert(p_id, eve_db::BookmarkRecord {
            bookmark_id: p_id, owner_id: char_id, solar_system_id: m_sys, label: "Personal Spot".into(),
            x: 100.0, y: 200.0, z: 300.0, item_id: None, memo: "Safe Spot".into(), folder_id: None, is_shared: false,
        });
        snap.bookmarks.insert(o_id, eve_db::BookmarkRecord {
            bookmark_id: o_id, owner_id: char_id, solar_system_id: m_sys, label: "Other Pocket".into(),
            x: 50_000.0, y: 0.0, z: 0.0, item_id: None, memo: "Mission Pocket Destination".into(), folder_id: None, is_shared: false,
        });
        (p_id, o_id)
    });

    let st_before_quit = ctx.db.with_snapshot(|snap| {
        snap.characters[&char_id].standings.get(&corp_id).copied().unwrap_or(0.0)
    });
    let abandon_3 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "RemoveOfferFromJournal",
        &[PyRep::Integer(mission_id_3 as i64)],
    )
    .await
    .expect("Abandon accepted mission");
    assert_eq!(abandon_3.dict_get("state").and_then(|v| v.as_str()), Some("Quit"));
    assert_eq!(abandon_3.dict_get("standingPenaltyApplied").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(abandon_3.dict_get("dungeonCleaned").and_then(|v| v.as_bool()), Some(true));
    assert!(!ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&bm3_id)));
    assert!(ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&personal_bm_id)), "personal bookmark in same system must survive mission abandon (R-P0-01)");
    assert!(ctx.db.with_snapshot(|snap| snap.bookmarks.contains_key(&other_bm_id)), "other mission pocket bookmark must survive mission abandon (R-P0-01)");
    let st_after_quit = ctx.db.with_snapshot(|snap| {
        snap.characters[&char_id].standings.get(&corp_id).copied().unwrap_or(0.0)
    });
    assert!(st_after_quit < st_before_quit, "Quitting accepted mission must penalize standing");
    let journal_after_quit = rpc_call(&ctx, &mut sess, "agentMgr", "GetMyJournalDetails", &[])
        .await
        .expect("GetMyJournalDetails after quit");
    let PyRep::Tuple(jq_sections) = journal_after_quit else { panic!() };
    let PyRep::List(jq_missions) = &jq_sections[0] else { panic!() };
    assert!(jq_missions.is_empty(), "Abandoned mission must be removed from journal");

    // 4. Mission Expiration check
    let offer_4 = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(agent_id)],
    )
    .await
    .expect("Fourth offer");
    let mission_id_4 = offer_4
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;
    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(mission_id_4 as i64)],
    )
    .await
    .expect("Accept fourth mission");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, mission_id_4)) {
            m.npcs_remaining = 0;
        }
        for note in snap.owner_notes.values_mut() {
            if note.item_id == mission_id_4 as i64 && note.note.contains("accept_expiry_unix") {
                if let Ok(mut val) = serde_json::from_str::<serde_json::Value>(&note.note) {
                    val["accept_expiry_unix"] = serde_json::json!(1000_u64);
                    note.note = val.to_string();
                }
            }
        }
    });
    let expired_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(mission_id_4 as i64)],
    )
    .await
    .expect_err("Expired mission must fail on completion attempt");
    assert_eq!(expired_err.code, "MissionExpired");
}

#[tokio::test]
async fn test_standings_formulas_derived_effective_corp_standing_data_and_transactions() {
    // parity: feat:pve.mission-faction-standing-loss, feat:standings.npc-standings, feat:standings.standing-gain-missions, feat:standings.standing-loss, feat:standings.standing-compositions, feat:standings.effective-standing, feat:standings.corp-standings, feat:standings.standing-notifications, feat:standings.trig-edencom-standings
    eve_testkit::attest(&["feat:pve.mission-faction-standing-loss", "feat:standings.npc-standings", "feat:standings.standing-gain-missions", "feat:standings.standing-loss", "feat:standings.standing-compositions", "feat:standings.effective-standing", "feat:standings.corp-standings", "feat:standings.standing-notifications", "feat:standings.trig-edencom-standings"]);
    let ctx = ServerContext::new();
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 505, "demo", "demo-secret", char_id).await;

    let pirate_corp_id = ctx
        .sde
        .npc_corporations
        .values()
        .find(|c| matches!(c.faction_id, Some(500_010 | 500_011 | 500_012)))
        .map(|c| c.corporation_id)
        .unwrap_or(1_000_125);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.clear();
            ch.standings.insert(CALDARI_NAVY_CORP_ID, 5.0);
            ch.standings.insert(pirate_corp_id, 2.0);
            ch.standings.insert(CALDARI_STATE_FACTION_ID, -5.0);
        }
        let sk = snap.skills.entry(char_id).or_default();
        for stid in [
            CONNECTIONS_SKILL,
            DIPLOMACY_SKILL,
            CRIMINAL_CONNECTIONS_SKILL,
        ] {
            sk.insert(
                stid,
                TrainedSkillRecord {
                    type_id: stid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });

    // Connections V on +5.0 -> 6.0; Diplomacy V on -5.0 -> -2.0; Criminal Connections V on +2.0 -> 3.6
    let mut eff_tenths = Vec::new();
    for (target_id, expected_eff) in [
        (CALDARI_NAVY_CORP_ID, 6.0_f64),
        (CALDARI_STATE_FACTION_ID, -2.0_f64),
        (pirate_corp_id, 3.6_f64),
    ] {
        let comp = rpc_call(
            &ctx,
            &mut sess,
            "standingMgr",
            "GetStandingCompositions",
            &[PyRep::Integer(target_id)],
        )
        .await
        .expect("GetStandingCompositions");
        let PyRep::List(items) = comp else {
            panic!("Expected list from GetStandingCompositions");
        };
        let eff = items[0]
            .dict_get("effectiveStanding")
            .and_then(|v| v.as_f64())
            .unwrap();
        assert!((eff - expected_eff).abs() < 1e-6);
        eff_tenths.push((eff * 10.0).round() as i64);
    }
    assert_eq!(eff_tenths[0], 60);
    assert_eq!(eff_tenths[1], -20);
    assert_eq!(eff_tenths[2], 36);
    assert_eq!(eff_tenths.len(), 3);

    let corp_st = rpc_call(&ctx, &mut sess, "standingMgr", "GetCorpStandings", &[])
        .await
        .expect("GetCorpStandings");
    assert!(!matches!(corp_st, PyRep::None));

    let npc_st = rpc_call(&ctx, &mut sess, "standingMgr", "GetNPCNPCStandings", &[])
        .await
        .expect("GetNPCNPCStandings");
    let PyRep::Rowset { columns: npc_cols, rows: npc_rows, .. } = npc_st else {
        panic!("Expected Rowset from GetNPCNPCStandings");
    };
    assert_eq!(npc_cols, vec!["fromID", "toID", "standing"]);
    assert!(!npc_rows.is_empty(), "GetNPCNPCStandings must return inter-faction rows");

    // Verify EVE Wiki derived standing formula (NPC_standings.md:140-156) dynamically against table relations:
    // +2% Minmatar (500_002, size 5.0) -> -1% Amarr (500_003, size 5.0, wiki rel -5.0) & +1% Thukker (500_015, size 3.0, wiki rel +3.0)
    use eve_server::services::agents_missions::{
        apply_faction_standing_with_derived, apply_mission_completion_standings, static_npc_faction_standings,
    };
    ctx.db.with_mut_snapshot(|snap| {
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
        let mut chg = Vec::new();
        apply_faction_standing_with_derived(&ctx, snap, char_id, 500_002, 2.0, true, &mut chg);
        let st = &snap.characters[&char_id].standings;
        assert!((st[&500_002] - 0.20).abs() < 1e-9, "Minmatar +2% gain -> +0.20");
        assert!((st[&500_003] - (-0.10)).abs() < 1e-9, "Amarr -1% derived -> -0.10, got {}", st[&500_003]);
        assert!((st[&500_015] - 0.10).abs() < 1e-9, "Thukker +1% derived -> +0.10, got {}", st[&500_015]);
        let primary_size = ctx.sde.factions.get(&500_002).map_or(5.0, |f| f.size_factor);
        for &(sec_f, target_f, rel) in static_npc_faction_standings() {
            if target_f == 500_002 && rel != 0.0 {
                let sec_size = ctx.sde.factions.get(&(sec_f as u32)).map_or(5.0, |f| f.size_factor);
                let derived_pct = 2.0 * (rel * (primary_size / sec_size) / 10.0);
                let expected = if derived_pct >= 0.0 {
                    (10.0_f64 * (derived_pct / 100.0)).min(rel)
                } else {
                    (-10.0_f64 * (derived_pct.abs() / 100.0)).max(rel)
                };
                assert!((st[&sec_f] - expected).abs() < 1e-9, "Derived formula mismatch for {sec_f}: got {}, expected {expected}", st[&sec_f]);
            }
        }

        // Cap at secondary faction's standing toward primary (-5.00 for Amarr -> Minmatar per NPC_standings.md:156)
        snap.characters.get_mut(&char_id).unwrap().standings.insert(500_003, -4.98);
        apply_faction_standing_with_derived(&ctx, snap, char_id, 500_002, 2.0, true, &mut chg);
        assert!((snap.characters[&char_id].standings[&500_003] - (-5.0)).abs() < 1e-9, "Amarr must cap at -5.00");
        apply_faction_standing_with_derived(&ctx, snap, char_id, 500_002, 2.0, true, &mut chg);
        assert!((snap.characters[&char_id].standings[&500_003] - (-5.0)).abs() < 1e-9, "Amarr at -5.00 must not drop further");

        // Asymmetric standing loss (NPC_standings.md:154): -2% Minmatar lowers friends (rel > 0), never raises enemies (rel < 0)
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
        apply_faction_standing_with_derived(&ctx, snap, char_id, 500_002, -2.0, true, &mut chg);
        let st_loss = &snap.characters[&char_id].standings;
        assert!((st_loss[&500_002] - (-0.20)).abs() < 1e-9);
        assert!((st_loss[&500_015] - (-0.10)).abs() < 1e-9, "Friend Thukker must suffer -1% derived loss");
        for &(sec_f, target_f, rel) in static_npc_faction_standings() {
            if target_f == 500_002 && rel < 0.0 {
                assert_eq!(st_loss.get(&sec_f).copied().unwrap_or(0.0), 0.0, "Enemy {sec_f} must NOT gain positive derived standing");
            }
        }

        // Per EVE Wiki (NPC_standings.md:160): intermediate Career Agent missions propagate derived
        // standings, while only the final career mission (e.g. "(10 of 10)") suppresses them.
        let career_ag = ctx
            .sde
            .agents
            .values()
            .find(|a| a.agent_type_id == 12 && ctx.sde.npc_corporations.get(&a.corporation_id).and_then(|c| c.faction_id) == Some(500_001))
            .expect("Caldari career agent (agent_type_id == 12) must exist in SDE");
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
        let (_, mid_changes) = apply_mission_completion_standings(&ctx, snap, char_id, career_ag.agent_id, 3096, career_ag.corporation_id, 5.0, false);
        assert!(mid_changes.iter().any(|&(eid, _, delta)| eid == 500_004 && delta < 0.0), "Intermediate career mission (1 of 10) must propagate derived standings");
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
        let (_, fin_changes) = apply_mission_completion_standings(&ctx, snap, char_id, career_ag.agent_id, 3173, career_ag.corporation_id, 5.0, false);
        assert!(!fin_changes.iter().any(|&(eid, _, _)| eid == 500_004), "Final career mission (10 of 10) must suppress derived standings");
        snap.characters.get_mut(&char_id).unwrap().standings.clear();
    });

    // Complete a Storyline mission -> verifies asymptotic gain formula, derived faction propagation, and GetStandingTransactions
    let (sl_agent_id, sl_corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 6);
    let sl_faction_id = ctx.sde.npc_corporations.get(&sl_corp_id).and_then(|c| c.faction_id).map(i64::from).unwrap_or(500_001);
    let (sl_sys_id, _) = colocate_with_agent(&ctx, &mut sess, char_id, sl_agent_id);
    let mut storyline_missions: Vec<&eve_sde::MissionTemplate> = ctx
        .sde
        .missions
        .values()
        .filter(|m| {
            is_storyline(m)
                && m.has_standing_rewards
                && !m.is_courier
                && m.content_template
                    .as_deref()
                    .unwrap_or_default()
                    .contains("KillMission")
                && !m.title.to_lowercase().contains("mining")
                && !m.title.to_lowercase().contains("ore")
                && !m.title.to_lowercase().contains("mineral")
        })
        .collect();
    storyline_missions.sort_by_key(|m| m.mission_id);
    let sl_mid = storyline_missions.first().map(|m| m.mission_id).unwrap_or(1259);

    ctx.db.with_mut_snapshot(|snap| {
        snap.active_missions.insert(
            (char_id, sl_mid),
            make_mission(char_id, sl_mid, sl_agent_id, sl_sys_id, "Accepted"),
        );
    });
    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(sl_mid as i64)],
    )
    .await
    .expect("Complete storyline mission for derived standings");

    ctx.db.with_snapshot(|snap| {
        let st = &snap.characters[&char_id].standings;
        assert!(st.get(&sl_faction_id).copied().unwrap_or(0.0) > 0.0, "Storyline must grant primary faction standing");
        assert!(st.values().any(|&v| v < 0.0), "Storyline must propagate negative derived standing to hostile factions");
    });

    let txs = rpc_call(
        &ctx,
        &mut sess,
        "standingMgr",
        "GetStandingTransactions",
        &[PyRep::Integer(sl_corp_id), PyRep::Integer(char_id)],
    )
    .await
    .expect("GetStandingTransactions");
    assert!(!matches!(txs, PyRep::None));
}

#[tokio::test]
async fn test_agent_dialogue_and_access_standings() {
    // parity: feat:pve.agent-finder, feat:pve.agent-conversation, feat:pve.mission-levels, feat:pve.mission-standing-gate
    eve_testkit::attest(&["feat:pve.agent-finder", "feat:pve.agent-conversation", "feat:pve.mission-levels", "feat:pve.mission-standing-gate"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 501, "demo", "demo-secret", char_id).await;

    let (l1_agent_id, l1_corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 2);
    let (l2_agent_id, l2_corp_id, _) = find_agent_by_level_and_type(&ctx, 2, 2);
    let (l3_agent_id, l3_corp_id, _) = find_agent_by_level_and_type(&ctx, 3, 2);
    let (l4_agent_id, l4_corp_id, _) = find_agent_by_level_and_type(&ctx, 4, 2);
    let (l5_agent_id, l5_corp_id, _) = find_agent_by_level_and_type(&ctx, 5, 2);

    // 1. GetAgents and GetSolarSystemOfAgent
    let agents_rep = rpc_call(&ctx, &mut sess, "agentMgr", "GetAgents", &[PyRep::Integer(4)]).await.expect("GetAgents");
    assert!(!matches!(agents_rep, PyRep::None));

    let sys_rep = rpc_call(&ctx, &mut sess, "agentMgr", "GetSolarSystemOfAgent", &[PyRep::Integer(l1_agent_id)]).await.expect("GetSolarSystemOfAgent");
    assert!(sys_rep.as_i64().unwrap_or(0) >= 30_000_000);

    // 2. GetInfoServiceDetails returns authentic SDE attributes
    let details = rpc_call(&ctx, &mut sess, "agentMgr", "GetInfoServiceDetails", &[PyRep::Integer(l4_agent_id)]).await.expect("GetInfoServiceDetails");
    assert_eq!(details.dict_get("agentID").and_then(|v| v.as_i64()), Some(l4_agent_id));
    assert_eq!(details.dict_get("level").and_then(|v| v.as_i64()), Some(4));

    // 3. L1 agent accessible at 0.0 standing, rejected at <= -2.0
    colocate_with_agent(&ctx, &mut sess, char_id, l1_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.clear();
        }
    });
    let l1_ok = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(l1_agent_id)]).await.expect("L1 agent must allow access at 0.0 standing");
    let l1_mid = l1_ok.dict_get("missionID").and_then(|v| v.as_i64()).unwrap();
    assert!(!l1_ok.dict_get("briefing").and_then(|v| v.as_str()).unwrap_or("").is_empty());
    assert!(!l1_ok.dict_get("objectives").and_then(|v| v.as_str()).unwrap_or("").is_empty());
    assert!(l1_ok.dict_get("iskReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    assert!(l1_ok.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    let briefing_info = rpc_call(&ctx, &mut sess, "agentMgr", "GetMissionBriefingInfo", &[PyRep::Integer(l1_mid)]).await.expect("GetMissionBriefingInfo");
    assert_eq!(briefing_info.dict_get("missionID").and_then(|v| v.as_i64()), Some(l1_mid));
    assert!(!briefing_info.dict_get("briefing").and_then(|v| v.as_str()).unwrap_or("").is_empty());

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(l1_corp_id, -2.0);
        }
    });
    let l1_hostile = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(l1_agent_id)]).await.expect_err("L1 agent must reject character with <= -2.0 standing");
    assert_eq!(l1_hostile.code, "InsufficientAgentStanding");

    // 4. L2 (1.0), L3 (3.0), L4 (5.0), L5 (7.0) thresholds enforced on MAX(agent, corp, faction)
    for (agent_id, corp_id, req_threshold) in [
        (l2_agent_id, l2_corp_id, 1.0_f64),
        (l3_agent_id, l3_corp_id, 3.0_f64),
        (l4_agent_id, l4_corp_id, 5.0_f64),
        (l5_agent_id, l5_corp_id, 7.0_f64),
    ] {
        colocate_with_agent(&ctx, &mut sess, char_id, agent_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.standings.clear();
                ch.standings.insert(corp_id, req_threshold - 0.1);
            }
        });
        let below_err = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)]).await.expect_err("Should reject below level standing threshold");
        assert_eq!(below_err.code, "InsufficientAgentStanding");

        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.standings.clear();
                ch.standings.insert(agent_id, req_threshold);
            }
        });
        let at_threshold = rpc_call(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(1), PyRep::Integer(agent_id)]).await.expect("MAX(agent, corp, faction) at threshold must succeed");
        assert_eq!(
            at_threshold.dict_get("state").and_then(|v| v.as_str()),
            Some("Offered")
        );
    }
}
}


pub(crate) mod special_agents_arcs_and_lp_store {
    #[allow(unused_imports)]
    use super::*;

// Integration tests for Special Agents, Arcs, and LP Store.

use crate::common;
use eve_db::{
    ActiveMissionRecord, InventoryFlag, InventoryItemRecord, IskCents, TrainedSkillRecord, TypeId,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use std::collections::HashMap;

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SOLAR_SYSTEM_ID: u32 = 30_000_142;
const CALDARI_NAVY_CORP_ID: i64 = 1_000_035;

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
    sess
}

async fn rpc_call(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let kw = HashMap::new();
    ctx.dispatch_rpc(sess, service, None, method, args, &kw, None)
        .await
}

fn colocate_with_agent(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    agent_id: i64,
) -> (u32, i64) {
    let agent = ctx.sde.agents.get(&agent_id).expect("agent in SDE");
    let sys_id = agent.solar_system_id.unwrap_or(JITA_SOLAR_SYSTEM_ID);
    let sta_id = agent.station_id.unwrap_or(JITA_STATION_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = agent.station_id;
            ch.structure_id = None;
        }
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = agent.station_id;
    sess.structure_id = None;
    (sys_id, sta_id)
}

fn is_storyline(m: &eve_sde::MissionTemplate) -> bool {
    m.content_template
        .as_deref()
        .unwrap_or_default()
        .contains("Storyline")
}

fn find_agent_by_level_and_type(
    ctx: &ServerContext,
    level: u8,
    agent_type_id: u32,
) -> (i64, i64, Option<i64>) {
    let mut agents: Vec<_> = ctx.sde.agents.values().collect();
    agents.sort_by_key(|a| a.agent_id);
    agents
        .into_iter()
        .find(|a| a.level == level && a.agent_type_id == agent_type_id && a.solar_system_id.is_some())
        .map(|a| (a.agent_id, a.corporation_id, a.station_id))
        .expect("SDE agent with requested level and type must exist")
}

fn make_mission(char_id: i64, mission_id: u32, agent_id: i64, sys_id: u32, state: &str) -> ActiveMissionRecord {
    ActiveMissionRecord {
        mission_id, character_id: char_id, agent_id, solar_system_id: sys_id,
        state: state.into(), current_room_id: 1, npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
    }
}

fn make_item(
    item_id: i64,
    type_id: TypeId,
    owner_id: i64,
    loc_id: i64,
    qty: i32,
    singleton: bool,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id: loc_id,
        flag: InventoryFlag::Hangar,
        quantity: qty,
        singleton,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

#[tokio::test]
async fn test_special_agents_storyline_epic_arcs_cosmos_datacenter_career_rd_and_locator() {
    // parity: feat:pve.mission-storyline, feat:pve.epic-arcs, feat:pve.storyline-arcs-soe, feat:pve.data-centers, feat:pve.career-agents, feat:pve.research-agents, feat:pve.locator-agents
    eve_testkit::attest(&["feat:pve.mission-storyline", "feat:pve.epic-arcs", "feat:pve.storyline-arcs-soe", "feat:pve.data-centers", "feat:pve.career-agents", "feat:pve.research-agents", "feat:pve.locator-agents"]);
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 504, "demo", "demo-secret", char_id).await;

    // 1. Storyline tally: 15 completed regular missions + 16th completion triggers Storyline offer
    let (reg_agent_id, reg_corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 2);
    let (reg_sys_id, _) = colocate_with_agent(&ctx, &mut sess, char_id, reg_agent_id);
    let reg_mid = ctx
        .sde
        .missions
        .values()
        .find(|m| {
            m.has_standing_rewards
                && !is_storyline(m)
                && m.content_template
                    .as_deref()
                    .unwrap_or_default()
                    .contains("BasicKill")
                && !m.title.to_lowercase().contains("mining")
        })
        .map(|m| m.mission_id)
        .expect("Regular combat mission with standing rewards");

    ctx.db.with_mut_snapshot(|snap| {
        snap.mission_tally.insert(char_id, 15);
        snap.active_missions.insert(
            (char_id, reg_mid),
            make_mission(char_id, reg_mid, reg_agent_id, reg_sys_id, "Accepted"),
        );
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(reg_corp_id, 2.0);
        }
    });

    let complete_16th = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(reg_mid as i64)],
    )
    .await
    .expect("Complete 16th mission");
    assert_eq!(
        complete_16th
            .dict_get("missionTally")
            .and_then(|v| v.as_i64()),
        Some(16)
    );
    assert_eq!(
        complete_16th
            .dict_get("storylineTriggered")
            .and_then(|v| v.as_bool()),
        Some(true)
    );

    // 2. Epic Arcs (`agent_type_id == 10`): intermediate missions keep arc Active without cooldown;
    //    only completing the finale mission triggers the 90-day repeat cooldown.
    let (epic_agent_id, epic_corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 10);
    let (epic_sys_id, _) = colocate_with_agent(&ctx, &mut sess, char_id, epic_agent_id);
    let (arc_id, arc_fac_id) = eve_server::services::agents_missions::resolve_epic_arc_id_for_agent(&ctx, epic_agent_id);
    let arc_total = ctx
        .sde
        .epic_arcs
        .values()
        .find(|ea| ea.faction_id == Some(arc_fac_id as u32))
        .and_then(|ea| (ea.missions_count > 0).then_some(ea.missions_count as usize))
        .unwrap_or(3)
        .max(2);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(epic_corp_id, 5.0);
        }
        snap.active_missions.insert(
            (char_id, reg_mid),
            make_mission(char_id, reg_mid, epic_agent_id, epic_sys_id, "Accepted"),
        );
    });
    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(reg_mid as i64)],
    )
    .await
    .expect("Complete Epic Arc mission 1");

    let mid_status = rpc_call(&ctx, &mut sess, "agentMgr", "GetMyEpicArcStatus", &[])
        .await
        .expect("GetMyEpicArcStatus after mission 1");
    let PyRep::Dict(mid_arcs) = mid_status else {
        panic!("Expected dict from GetMyEpicArcStatus (#639)");
    };
    let (_, PyRep::Dict(mid_missions)) = mid_arcs
        .iter()
        .find(|(k, _)| k.as_i64() == Some(arc_id as i64))
        .expect("Active epic arc entry present after mission 1") else {
        panic!("Expected mission dict for active epic arc");
    };
    let active_arc = &mid_missions[0].1;
    assert_eq!(active_arc.dict_get("state").and_then(|v| v.as_str()), Some("Active"));
    assert_eq!(active_arc.dict_get("onCooldown"), Some(&PyRep::Bool(false)));

    // Completing Mission #1 must NOT lock out Mission #2
    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(2), PyRep::Integer(epic_agent_id)],
    )
    .await
    .expect("Epic Arc Mission #2 must remain offerable after Mission #1");

    // Advance completed_missions to (arc_total - 1) and complete final mission -> 90-day cooldown
    let finale_mid = reg_mid;
    ctx.db.with_mut_snapshot(|snap| {
        let mut arcs = eve_server::services::agents_missions::load_epic_arc_states(snap, char_id);
        let arc = arcs.iter_mut().find(|a| a.epic_arc_id == arc_id).unwrap();
        arc.completed_missions = (0..(arc_total - 1)).map(|i| 90_000 + i as u32).collect();
        eve_server::services::agents_missions::save_epic_arc_states(snap, char_id, &arcs);
        snap.active_missions.insert(
            (char_id, finale_mid),
            make_mission(char_id, finale_mid, epic_agent_id, epic_sys_id, "Accepted"),
        );
    });
    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(finale_mid as i64)],
    )
    .await
    .expect("Complete final Epic Arc mission");

    let epic_status = rpc_call(&ctx, &mut sess, "agentMgr", "GetMyEpicArcStatus", &[])
        .await
        .expect("GetMyEpicArcStatus");
    let PyRep::Dict(arcs) = epic_status else {
        panic!("Expected dict from GetMyEpicArcStatus (#639)");
    };
    let (_, PyRep::Dict(done_missions)) = arcs
        .iter()
        .find(|(k, _)| k.as_i64() == Some(arc_id as i64))
        .expect("Completed epic arc entry") else {
        panic!("Expected mission dict for completed epic arc");
    };
    let done_arc = &done_missions[0].1;
    assert_eq!(done_arc.dict_get("state").and_then(|v| v.as_str()), Some("Cooldown"));
    assert_eq!(done_arc.dict_get("onCooldown"), Some(&PyRep::Bool(true)));

    let epic_cooldown_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(epic_agent_id)],
    )
    .await
    .expect_err("Epic Arc must enforce 90-day cooldown after final mission");
    assert_eq!(epic_cooldown_err.code, "EpicArcOnCooldown");

    // 3. Career (`agent_type_id == 12`): one-time lockouts + GetCompletedCareerAgentIDs
    let (career_agent_id, career_corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 12);
    colocate_with_agent(&ctx, &mut sess, char_id, career_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(career_corp_id, 2.0);
        }
    });
    let career_offer = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(career_agent_id)],
    )
    .await
    .expect("Career agent offer");
    let career_mid = career_offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(m) = snap.active_missions.get_mut(&(char_id, career_mid)) {
            m.state = "Completed".into();
        }
    });
    let career_repeat_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(career_agent_id)],
    )
    .await
    .expect_err("Career agent must reject repeat completion");
    assert_eq!(career_repeat_err.code, "OneTimeAgentMissionCompleted");

    let career_ids = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "GetCompletedCareerAgentIDs",
        &[PyRep::List(vec![
            PyRep::Integer(career_agent_id),
            PyRep::Integer(3_018_935),
        ])],
    )
    .await
    .expect("GetCompletedCareerAgentIDs");
    let PyRep::Dict(completed_careers) = career_ids else {
        panic!("Expected dict from GetCompletedCareerAgentIDs");
    };
    assert!(completed_careers.contains(&(PyRep::Integer(career_agent_id), PyRep::Bool(true))));
    assert!(completed_careers.contains(&(PyRep::Integer(3_018_935), PyRep::Bool(false))));

    // 4. Data Center (`agent_type_id == 5`): Tag Turn-In (requires 1x tag 17202, one-time lockout)
    let (dc_agent_id, dc_corp_id, _) = find_agent_by_level_and_type(&ctx, 1, 5);
    let (_, dc_station_id) = colocate_with_agent(&ctx, &mut sess, char_id, dc_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(dc_corp_id, 2.0);
        }
        snap.next_entity_id += 1;
        let tid = snap.next_entity_id;
        snap.items
            .insert(tid, make_item(tid, 17_202, char_id, dc_station_id, 1, false));
    });
    let dc_offer = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(dc_agent_id)],
    )
    .await
    .expect("Data Center offer");
    let dc_mid = dc_offer
        .dict_get("missionID")
        .and_then(|v| v.as_i64())
        .unwrap() as u32;
    rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(3), PyRep::Integer(dc_mid as i64)],
    )
    .await
    .expect("Accept Data Center tag mission");
    let dc_done = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(6), PyRep::Integer(dc_mid as i64)],
    )
    .await
    .expect("Complete Data Center tag turn-in");
    assert_eq!(
        dc_done.dict_get("state").and_then(|v| v.as_str()),
        Some("Completed")
    );

    let dc_repeat_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[PyRep::Integer(1), PyRep::Integer(dc_agent_id)],
    )
    .await
    .expect_err("Repeat Data Center mission must be rejected");
    assert_eq!(dc_repeat_err.code, "OneTimeAgentMissionCompleted");

    // 5. R&D Agents (`DoAction` action 12 = StartResearch, action 14 = BuyDatacores)
    let (rd_agent_id, rd_corp_id) = ctx.sde.agents.values().find(|a| a.level == 1 && a.agent_type_id == 4 && a.station_id.is_some() && a.research_skills.contains(&11_448)).map(|a| (a.agent_id, a.corporation_id)).expect("L1 R&D agent for 11448");
    colocate_with_agent(&ctx, &mut sess, char_id, rd_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(rd_corp_id, 5.0);
            ch.balance = IskCents::from_isk(10_000_000);
        }
        snap.skills.entry(char_id).or_default().insert(
            11_448,
            TrainedSkillRecord {
                type_id: 11_448,
                level: 4,
                skill_points: 45_255.0,
            },
        );
        for sid in [3_402_u32, 3_392_u32] {
            snap.skills.entry(char_id).or_default().insert(
                sid,
                TrainedSkillRecord {
                    type_id: sid,
                    level: 5,
                    skill_points: 256_000.0,
                },
            );
        }
    });
    let start_rd = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(12),
            PyRep::Integer(rd_agent_id),
            PyRep::Integer(11_448),
        ],
    )
    .await
    .expect("Start R&D Research");
    assert!(
        start_rd
            .dict_get("pointsPerDay")
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0)
            > 0.0
    );

    ctx.db.with_mut_snapshot(|snap| {
        for note in snap.owner_notes.values_mut() {
            if note.note.contains("points_per_day") {
                if let Ok(mut arr) = serde_json::from_str::<serde_json::Value>(&note.note) {
                    if let Some(first) = arr.get_mut(0) {
                        first["started_unix"] = serde_json::json!(1_u64);
                    }
                    note.note = arr.to_string();
                }
            }
        }
    });
    let redeem_rd = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(14),
            PyRep::Integer(rd_agent_id),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("Redeem R&D Datacores");
    assert_eq!(
        redeem_rd
            .dict_get("datacoreTypeID")
            .and_then(|v| v.as_i64()),
        Some(20_417)
    );

    // 6. Locator Agents (`DoAction` action 15 = LocateCharacter)
    let locator_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.is_locator && a.solar_system_id.is_some())
        .cloned()
        .expect("Locator agent in SDE");
    let locator_agent_id = locator_agent.agent_id;
    let agent_sys = locator_agent.solar_system_id.unwrap();
    colocate_with_agent(&ctx, &mut sess, char_id, locator_agent_id);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.standings.insert(locator_agent_id, 8.0);
        }
        if let Some(target) = snap.characters.get_mut(&90_000_002) {
            target.solar_system_id = agent_sys;
        }
    });
    let locate_res = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(15),
            PyRep::Integer(locator_agent_id),
            PyRep::Integer(90_000_002),
        ],
    )
    .await
    .expect("LocateCharacter via DoAction 15");
    assert_eq!(
        locate_res
            .dict_get("targetCharacterID")
            .and_then(|v| v.as_i64()),
        Some(90_000_002)
    );

    let locate_cd_err = rpc_call(
        &ctx,
        &mut sess,
        "agentMgr",
        "DoAction",
        &[
            PyRep::Integer(15),
            PyRep::Integer(locator_agent_id),
            PyRep::Integer(90_000_002),
        ],
    )
    .await
    .expect_err("Locator agent must enforce cooldown");
    assert_eq!(locate_cd_err.code, "LocatorAgentOnCooldown");
}

#[tokio::test]
async fn test_lp_store_offers_exchange_rates_and_purchase_validation() {
    // parity: feat:lp.lp-balance, feat:lp.lp-corp-wallet, feat:lp.concord-lp, feat:lp.lp-reward-logs
    eve_testkit::attest(&["feat:lp.lp-balance", "feat:lp.lp-corp-wallet", "feat:lp.concord-lp", "feat:lp.lp-reward-logs"]);
    let ctx = ServerContext::new();
    crate::common::seed_test_lp_store_offers(&ctx);
    let char_id = 90_000_001;
    let mut sess = setup_pilot(&ctx, 506, "demo", "demo-secret", char_id).await;
    sess.station_id = Some(JITA_STATION_ID);

    let offers = rpc_call(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "GetAvailableOffersFromCorp",
        &[PyRep::Integer(CALDARI_NAVY_CORP_ID)],
    )
    .await
    .expect("GetAvailableOffersFromCorp");
    let PyRep::List(offer_list) = offers else {
        panic!("Expected list of LP offers");
    };
    assert!(offer_list.len() >= 3);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.loyalty_points.insert(CALDARI_NAVY_CORP_ID, 100);
        }
    });
    let lp_err = rpc_call(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP_ID),
            PyRep::Integer(1002),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect_err("TakeOfferForCharacter must fail with insufficient LP");
    assert_eq!(lp_err.code, "InsufficientLoyaltyPoints");

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.loyalty_points.insert(CALDARI_NAVY_CORP_ID, 50_000);
            ch.balance = IskCents::from_isk(100_000_000);
        }
    });
    let purchase = rpc_call(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP_ID),
            PyRep::Integer(1002),
            PyRep::Integer(2),
        ],
    )
    .await
    .expect("TakeOfferForCharacter 2x offer 1002");
    assert_eq!(
        purchase
            .dict_get("deliveredTypeID")
            .and_then(|v| v.as_i64()),
        Some(27_361)
    );
    assert_eq!(
        purchase.dict_get("quantity").and_then(|v| v.as_i64()),
        Some(2_000)
    );
    assert_eq!(
        purchase.dict_get("remainingLP").and_then(|v| v.as_i64()),
        Some(49_000)
    );

    // Offer 1004 requires 100x Tritanium (type 34) trade-in; clear any default Tritanium first
    ctx.db.with_mut_snapshot(|snap| {
        snap.items
            .retain(|_, it| !(it.owner_id == char_id && it.type_id == 34));
    });
    let req_item_err = rpc_call(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP_ID),
            PyRep::Integer(1004),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect_err("Offer 1004 must fail without required trade-in item 34");
    assert_eq!(req_item_err.code, "MissingRequiredItems");

    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let req_id = snap.next_entity_id;
        snap.items
            .insert(req_id, make_item(req_id, 34, char_id, JITA_STATION_ID, 100, false));
    });
    let tradein_purchase = rpc_call(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "TakeOfferForCharacter",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP_ID),
            PyRep::Integer(1004),
            PyRep::Integer(1),
        ],
    )
    .await
    .expect("Offer 1004 succeeds when required trade-in item 34 is in station hangar");
    assert_eq!(
        tradein_purchase
            .dict_get("remainingLP")
            .and_then(|v| v.as_i64()),
        Some(47_500)
    );

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.loyalty_points.insert(1_000_125, 1_000);
        }
    });
    let exch = rpc_call(
        &ctx,
        &mut sess,
        "LPStoreMgr",
        "ExchangeConcordLP",
        &[
            PyRep::Integer(CALDARI_NAVY_CORP_ID),
            PyRep::Integer(1_000),
        ],
    )
    .await
    .expect("ExchangeConcordLP");
    assert_eq!(
        exch.dict_get("convertedLP").and_then(|v| v.as_i64()),
        Some(800)
    );
    assert_eq!(
        exch.dict_get("newTargetCorpLP").and_then(|v| v.as_i64()),
        Some(48_300)
    );
}
}


#[allow(unused_imports, dead_code, unused_variables, unused_mut)]
mod pve_agent_missions_ext {
    use super::*;

// Integration tests for PvE Agent Missions remediation:
// Courier, Trade, Security combat waves, and Mining missions.

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
                    offered_level: None,
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
                    spawned_npc_ball_ids: Vec::new(), bookmark_id: None,
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

}
