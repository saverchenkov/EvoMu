//! End-to-End Integration Journeys: Mining, Combat, Missions, Exploration, Wormholes/Filaments.
//!
//! Covering:
//! - `journey:mining_loop`
//! - `journey:pve_combat`
//! - `journey:missions`
//! - `journey:exploration`
//! - `journey:wormholes_filaments`

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use glam::DVec3;
use std::collections::HashMap;

async fn call_rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, eve_proto::BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_journey_mining_loop() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(301);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    // 1. Undock & warp to asteroid belt
    call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
    let sys_id = sess.solar_system_id.unwrap();
    let ship_id = sess.ship_id.unwrap();
    let sys_h = ctx.systems.get_or_spawn(sys_id);

    // Spawn an asteroid ball in space and fit a Miner I (483) + Survey Scanner I (444)
    let asteroid_id = 700_100_001_i64;
    let (_, ship_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    sys_h
        .spawn_ball(Ball {
            id: asteroid_id,
            type_id: 1230, // Veldspar
            position: ship_ball.position + DVec3::new(2_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 100.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await
        .expect("spawn asteroid");

    let miner_id = ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 32880; // Venture
        }
        snap.next_entity_id += 1;
        let mid = snap.next_entity_id;
        snap.items.insert(
            mid,
            InventoryItemRecord {
                item_id: mid,
                type_id: 483, // Miner I
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot1,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        mid
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(miner_id, true);
        snap.skills.entry(90_000_001).or_default().insert(
            3386,
            eve_db::TrainedSkillRecord { type_id: 3386, level: 5, skill_points: 256_000.0 },
        );
        snap.asteroids.insert(
            asteroid_id,
            eve_db::AsteroidRecord {
                asteroid_id,
                solar_system_id: sys_id,
                belt_id: 0,
                type_id: 1230,
                quantity: 1_000,
                initial_quantity: 1_000,
                position: (ship_ball.position.x + 2_000.0, ship_ball.position.y, ship_ball.position.z),
                radius: 100.0,
            },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);

    // 2. Lock asteroid via dogmaIM.AddTarget & activate mining laser
    call_rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(asteroid_id)])
        .await
        .expect("AddTarget asteroid");
    for _ in 0..15 {
        let _ = sys_h.tick().await;
        if sys_h
            .get_target_locks(ship_id)
            .await
            .unwrap_or_default()
            .iter()
            .any(|l| l.target_id == asteroid_id && l.locked)
        {
            break;
        }
    }
    call_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(miner_id), PyRep::String("miningLaser".into()), PyRep::Integer(asteroid_id), PyRep::Integer(0)])
        .await
        .expect("Activate Miner I");
    ctx.advance_secs(65);
    let _ = eve_server::services::mining_cycle::process_mining_cycles(&ctx).await;

    let (mined_ore_id, mined_ore_qty) = ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|i| i.owner_id == 90_000_001 && i.location_id == ship_id && i.type_id == 1230)
            .map(|i| (i.item_id, i.quantity))
            .unwrap_or((0, 0))
    });
    assert!(mined_ore_qty > 0, "mining cycle must yield Veldspar into ship hold");

    // 3. Dock & reprocess the mined ore with skill-based yields
    let st_pos = ctx.sde.stations.get(&60_003_760).map(|s| s.position).unwrap_or(DVec3::ZERO);
    let _ = sys_h.set_ball_position(ship_id, st_pos).await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
    let bey_bind = call_rpc(&ctx, &mut sess, "beyonce", "MachoBindObject", &[PyRep::Integer(sys_id as i64), PyRep::None]).await.unwrap();
    let bey_bid = match bey_bind { PyRep::Tuple(t) => t[0].as_i64().unwrap_or(1) as u64, other => other.as_i64().unwrap_or(1) as u64 };
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(bey_bid), "CmdDock", &[PyRep::Integer(60_003_760)], &HashMap::new(), None)
        .await
        .expect("CmdDock");
    let inv_bind = call_rpc(&ctx, &mut sess, "invbroker", "MachoBindObject", &[PyRep::Integer(60_003_760), PyRep::None])
        .await
        .expect("invbroker bind");
    let inv_bid = match inv_bind { PyRep::Tuple(t) => t[0].as_i64().unwrap_or(1) as u64, other => other.as_i64().unwrap_or(1) as u64 };
    ctx.dispatch_rpc(&mut sess, "invbroker", Some(inv_bid), "Add", &[PyRep::Integer(mined_ore_id), PyRep::Integer(ship_id), PyRep::Integer(4)], &HashMap::new(), None)
        .await
        .expect("invbroker.Add");
    let quotes = call_rpc(&ctx, &mut sess, "reprocessingSvc", "GetQuotes", &[PyRep::List(vec![PyRep::Integer(mined_ore_id)]), PyRep::Integer(60_003_760)])
        .await
        .expect("GetQuotes");
    assert!(format!("{quotes:?}").contains("34"), "Veldspar quote must include Tritanium (34)");
    call_rpc(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(mined_ore_id)])
        .await
        .expect("Reprocess Veldspar");
    let trit_qty: i32 = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.owner_id == 90_000_001 && i.type_id == 34).map(|i| i.quantity).sum()
    });
    assert!(trit_qty > 0, "reprocessing Veldspar must yield Tritanium");
}

#[tokio::test]
async fn test_journey_pve_combat() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(302);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    let ship_id = 100_000_001_i64;
    let t1_id = 100_000_005_i64; // Seeded 200mm AutoCannon II in HiSlot0
    let (t2_id, t3_id) = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 5;
        let base = snap.next_entity_id - 4;
        for (id, tid, flag, qty, sing) in [
            (base, 2889, InventoryFlag::HiSlot1, 1, true),
            (base + 1, 2889, InventoryFlag::HiSlot2, 1, true),
            (base + 2, 21924, InventoryFlag::HiSlot0, 120, false),
            (base + 3, 21924, InventoryFlag::HiSlot1, 120, false),
            (base + 4, 21924, InventoryFlag::HiSlot2, 120, false),
        ] {
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: tid,
                    owner_id: 90_000_001,
                    location_id: ship_id,
                    flag,
                    quantity: qty,
                    singleton: sing,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        for gid in [t1_id, base, base + 1] {
            snap.module_online_states.insert(gid, true);
        }
        snap.skills.entry(90_000_001).or_default().insert(
            12201,
            eve_db::TrainedSkillRecord { type_id: 12201, level: 5, skill_points: 256_000.0 },
        );
        (base, base + 1)
    });
    ctx.invalidate_ship_fitting(ship_id);
    for slave_id in [t2_id, t3_id] {
        call_rpc(&ctx, &mut sess, "dogmaIM", "LinkWeapons", &[PyRep::Integer(ship_id), PyRep::Integer(t1_id), PyRep::Integer(slave_id)])
            .await
            .expect("LinkWeapons");
    }

    call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
    let sys_id = sess.solar_system_id.unwrap();
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    assert_eq!(sess.station_id, None);

    // Spawn a real SDE belt rat frigate with unmodified HP and positive bounty
    let rat_type_id = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.default_attributes.get(&481).copied().unwrap_or(0.0) > 0.0
                && t.default_attributes.get(&263).copied().unwrap_or(9999.0) <= 150.0
                && t.default_attributes.get(&265).copied().unwrap_or(9999.0) <= 150.0
                && t.default_attributes.get(&9).copied().unwrap_or(9999.0) <= 150.0
        })
        .map(|t| t.type_id)
        .min()
        .unwrap_or(10_035);
    let (_, ship_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let npc_id = eve_server::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
        &ctx,
        sys_id,
        rat_type_id,
        "BeltRat",
        1,
        ship_ball.position + DVec3::new(500.0, 0.0, 0.0),
    )
    .await;

    // Lock target via dogmaIM.AddTarget, activate repeating weapon bank, tick real combat
    let wallet_before = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance);
    call_rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(npc_id)])
        .await
        .expect("AddTarget NPC");
    for _ in 0..15 {
        let _ = sys_h.tick().await;
        if sys_h
            .get_target_locks(ship_id)
            .await
            .unwrap_or_default()
            .iter()
            .any(|l| l.target_id == npc_id && l.locked)
        {
            break;
        }
    }
    call_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(t1_id), PyRep::String("projectileFired".into()), PyRep::Integer(npc_id), PyRep::Integer(1)])
        .await
        .expect("Activate weapon");
    for _ in 0..60 {
        let _ = sys_h.tick().await;
        if sys_h.get_ball_snapshot(npc_id).await.unwrap().is_none() {
            break;
        }
    }

    let npc_after = sys_h.get_ball_snapshot(npc_id).await.unwrap();
    assert!(npc_after.is_none(), "NPC must be destroyed by weapon fire");
    ctx.advance_secs(1_205);
    eve_server::sp6_lifecycle::tick_background_cluster(&ctx).await;
    let wallet_after = ctx.db.with_snapshot(|s| s.characters[&90_000_001].balance);
    assert!(wallet_after > wallet_before, "wallet balance must increase after NPC bounty payout");
}

#[tokio::test]
async fn test_journey_missions() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(303);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    // 1. Browse agents & accept/complete mission
    let agents = call_rpc(&ctx, &mut sess, "agentMgr", "GetAgents", &[PyRep::Integer(1), PyRep::Integer(23)])
        .await
        .expect("GetAgents");
    assert!(format!("{agents:?}").contains("agentID"));

    let agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.level == 1 && a.agent_type_id == 2 && a.division_id == 24 && a.solar_system_id.is_some() && a.station_id.is_some())
        .expect("level 1 security agent")
        .clone();
    let agent_id = agent.agent_id;
    let agent_sys = agent.solar_system_id.unwrap();
    ctx.db.with_mut_snapshot(|s| {
        let ch = s.characters.get_mut(&90_000_001).unwrap();
        ch.solar_system_id = agent_sys;
        ch.station_id = agent.station_id;
    });
    sess.solar_system_id = Some(agent_sys);
    sess.station_id = agent.station_id;
    let req_m = call_rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(2), PyRep::Integer(agent_id)])
        .await
        .expect("DoAction(RequestMission)");
    let PyRep::KeyVal(ref req_kv) = req_m else { panic!("expected KeyVal from DoAction(2)") };
    let mid = req_kv.get("missionID").and_then(|v| v.as_i64()).expect("missionID");
    let acc_m = call_rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(3), PyRep::Integer(mid)])
        .await
        .expect("DoAction(AcceptMission)");
    assert!(matches!(acc_m, PyRep::KeyVal(_)), "DoAction(3) must return mission KeyVal");

    // Complete mission objective and turn in via DoAction(6)
    let active_agent_id = acc_m.dict_get("agentID").and_then(|v| v.as_i64()).unwrap_or(agent_id);
    let agent_corp = ctx.sde.agents[&active_agent_id].corporation_id;
    let (isk_before, lp_before, standing_before) = ctx.db.with_mut_snapshot(|s| {
        if let Some(m) = s.active_missions.get_mut(&(90_000_001, mid as u32)) {
            m.npcs_remaining = 0;
        }
        let ch = s.characters.get_mut(&90_000_001).unwrap();
        let isk = ch.balance.0;
        let lp = ch.loyalty_points.get(&agent_corp).copied().unwrap_or(0);
        let st = ch.standings.get(&agent_corp).copied().unwrap_or(0.0);
        (isk, lp, st)
    });
    let comp_m = call_rpc(&ctx, &mut sess, "agentMgr", "DoAction", &[PyRep::Integer(6), PyRep::Integer(mid)])
        .await
        .expect("DoAction(CompleteMission)");
    assert!(comp_m.dict_get("iskReward").and_then(|v| v.as_f64()).unwrap_or(0.0) > 0.0);
    assert!(comp_m.dict_get("lpReward").and_then(|v| v.as_i64()).unwrap_or(0) > 0);
    let (isk_after, lp_after, standing_after) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&90_000_001];
        (
            ch.balance.0,
            ch.loyalty_points.get(&agent_corp).copied().unwrap_or(0),
            ch.standings.get(&agent_corp).copied().unwrap_or(0.0),
        )
    });
    assert!(isk_after > isk_before && lp_after > lp_before && standing_after > standing_before);

    // 2. Check LP store offers
    let offers = call_rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_035)])
        .await
        .expect("GetAvailableOffersFromCorp");
    let PyRep::List(offer_list) = offers else { panic!("expected List from GetAvailableOffersFromCorp") };
    assert!(!offer_list.is_empty(), "LP store must have offers");
}

#[tokio::test]
async fn test_journey_exploration() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(304);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");

    // 1. Launch probes & scan signatures
    let scan_rows = crate::common::probe_scan(&ctx, &mut sess, DVec3::ZERO, 4.0 * 149_597_870_700.0).await;
    assert!(!scan_rows.is_empty(), "probe_scan must return cosmic signatures");

    // 2. Hacking minigame
    eve_server::services::pve_exploration::ensure_system_pve_sites_seeded(&ctx, 30_000_142).await;
    let (can_id, site_pos) = eve_server::services::pve_exploration::with_pve_exploration_reg(&ctx, |reg| {
        let site = reg.sites_by_system[&30_000_142]
            .iter()
            .find(|s| s.site_category == "Data" && !s.hackable_can_ids.is_empty())
            .expect("Seeded Data site in Jita");
        (site.hackable_can_ids[0], site.position)
    });
    let ship_id = sess.ship_id.unwrap();
    let sys_h = ctx.systems.get_or_spawn(30_000_142);
    let _ = sys_h.set_ball_position(ship_id, DVec3::from_array(site_pos)).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let aid = snap.next_entity_id;
        snap.items.insert(
            aid,
            InventoryItemRecord {
                item_id: aid,
                type_id: 22175, // Data Analyzer I
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.module_online_states.insert(aid, true);
        snap.skills.entry(90_000_001).or_default().insert(
            21718, // Hacking
            eve_db::TrainedSkillRecord { type_id: 21718, level: 5, skill_points: 256_000.0 },
        );
    });
    ctx.invalidate_ship_fitting(ship_id);
    let hack_start = call_rpc(&ctx, &mut sess, "hackingMgr", "StartNewGameInstance", &[PyRep::Integer(can_id)])
        .await
        .expect("StartNewGameInstance");
    assert!(hack_start.dict_get("board").is_some() || hack_start.dict_get("containerID").is_some());
    let click = call_rpc(&ctx, &mut sess, "hackingMgr", "ClickedOnTile", &[PyRep::Integer(can_id), PyRep::Integer(1)])
        .await
        .expect("ClickedOnTile");
    assert_eq!(click.dict_get("containerID").and_then(|v| v.as_i64()).unwrap_or(can_id), can_id);

    // 3. D-scan (14.3 AU in meters)
    let dscan = call_rpc(
        &ctx,
        &mut sess,
        "scanMgr",
        "ConeScan",
        &[PyRep::Real(360.0), PyRep::Real(14.3 * 149_597_870_700.0), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)],
    )
    .await
    .expect("ConeScan");
    let PyRep::List(dscan_list) = dscan else { panic!("expected List from ConeScan") };
    assert!(!dscan_list.is_empty(), "D-scan at 14.3 AU must detect in-space entities");
    assert!(dscan_list.iter().any(|e| e.dict_get("typeID").and_then(|v| v.as_i64()).unwrap_or(0) > 0));
}

#[tokio::test]
async fn test_journey_wormholes_filaments() {
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(305);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
    let ship_id = sess.ship_id.unwrap();

    // 1. Wormhole jump into J-space (31_000_005) and mass deduction check
    let now_u = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_signatures.insert(
            30_000_142,
            vec![eve_db::CosmicSignatureRecord {
                signature_id: "WH-J1".into(),
                solar_system_id: 30_000_142,
                sig_group: "Wormhole".into(),
                type_name: "Unstable Wormhole (K162)".into(),
                position: [15_000.0, 0.0, 0.0],
                signal_strength: 1.0,
                wormhole_type_id: Some(26_272),
                destination_system_id: Some(31_000_005),
                created_unix: now_u,
                lifetime_seconds: 86_400,
                max_mass_kg: 2_000_000_000.0,
                remaining_mass_kg: 2_000_000_000.0,
                max_jump_mass_kg: 375_000_000.0,
                collapsed: false,
            }],
        );
    });
    let _ = call_rpc(&ctx, &mut sess, "scanMgr", "GetSystemScanMgr", &[]).await.expect("GetSystemScanMgr");
    let wh_item_id = eve_server::services::space_destiny::wormhole_item_id_for_signature(30_000_142, "WH-J1");
    let _ = ctx
        .systems
        .get_or_spawn(30_000_142)
        .set_ball_position(ship_id, DVec3::new(15_500.0, 0.0, 0.0))
        .await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
    let jump_rep = call_rpc(&ctx, &mut sess, "wormholeMgr", "WormholeJump", &[PyRep::Integer(wh_item_id)])
        .await
        .expect("WormholeJump");
    assert_eq!(sess.solar_system_id, Some(31_000_005));
    let rem_mass = jump_rep.dict_get("remainingMassKg").and_then(|v| v.as_f64()).unwrap_or(2_000_000_000.0);
    assert!(rem_mass < 2_000_000_000.0, "wormhole jump must deduct ship mass");

    // Return to Jita (30_000_142) for T0 Tranquil Abyssal (56131) + Needlejack filament journeys
    ctx.db.with_mut_snapshot(|s| s.characters.get_mut(&90_000_001).unwrap().solar_system_id = 30_000_142);
    sess.solar_system_id = Some(30_000_142);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);

    // 2. V24 Abyssal filament 3-room run via abyssalMgr
    call_rpc(&ctx, &mut sess, "fleetObjectHandler", "CreateFleet", &[]).await.expect("CreateFleet");
    let fil_id = ctx.db.add_or_stack_item(90_000_001, ship_id, InventoryFlag::Cargo, 56131, 3).unwrap().item_id;
    let dep = call_rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceDeployment", &[PyRep::Integer(fil_id)])
        .await
        .expect("AbyssalEntranceDeployment");
    let trace_id = dep.dict_get("traceBallID").and_then(|v| v.as_i64()).unwrap();
    let enter = call_rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEntranceGateActivation", &[PyRep::Integer(trace_id)])
        .await
        .expect("AbyssalEntranceGateActivation");
    assert_eq!(sess.solar_system_id, Some(32_000_001));
    let pid = enter.dict_get("pocketID").and_then(|v| v.as_i64()).unwrap();
    call_rpc(&ctx, &mut sess, "abyssalMgr", "ClientIsReady", &[]).await.expect("abyssalMgr.ClientIsReady");

    for room_idx in 1..=3_i64 {
        crate::abyssal_room_combat::destroy_room_npcs_via_damage(&ctx, 90_000_001, (room_idx - 1) as usize).await;
        let gate_id = 1_895_000_000 + pid * 10 + room_idx;
        if room_idx < 3 {
            let adv = call_rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalGateActivation", &[PyRep::Integer(gate_id)])
                .await
                .expect("AbyssalGateActivation");
            assert_eq!(adv.dict_get("room").and_then(|v| v.as_i64()), Some(room_idx + 1));
        } else {
            let exit = call_rpc(&ctx, &mut sess, "abyssalMgr", "AbyssalEndGateActivation", &[PyRep::Integer(gate_id)])
                .await
                .expect("AbyssalEndGateActivation");
            assert_eq!(exit.dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(30_000_142));
        }
    }
    assert_eq!(sess.solar_system_id, Some(30_000_142));

    // 3. Needlejack filament jump to nullsec via randomJumpMgr.ActivateRandomJumpFilament
    let nj_id = ctx.db.add_or_stack_item(90_000_001, ship_id, InventoryFlag::Cargo, 53977, 1).unwrap().item_id;
    call_rpc(&ctx, &mut sess, "randomJumpMgr", "ActivateRandomJumpFilament", &[PyRep::Integer(nj_id)])
        .await
        .expect("ActivateRandomJumpFilament");
    assert_ne!(sess.solar_system_id, Some(30_000_142), "Needlejack filament must jump pilot out of origin system");
}
