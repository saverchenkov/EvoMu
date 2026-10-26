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

fn colocate_with_agent(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    agent_id: i64,
) -> (u32, i64) {
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
        state: state.into(), current_room_id: 1, npcs_remaining: 0, spawned_npc_ball_ids: Vec::new(),
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
    let (_, dist_station_id) = colocate_with_agent(&ctx, &mut sess, char_id, dist_agent_id);
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

    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.station_id = Some(dist_station_id); } });
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
    let (_, dist_station_id) = colocate_with_agent(&ctx, &mut sess, char_id, dist_agent_id);
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
    ctx.db.with_mut_snapshot(|snap| { if let Some(ch) = snap.characters.get_mut(&char_id) { ch.station_id = Some(dist_station_id); } });
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
