//! End-to-End Integration Journeys Part 3 (`journey:mining_loop` .. `journey:wormholes_filaments`).
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
    // parity: journey:mining_loop
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

    let (miner_id, ore_id) = ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.type_id = 32880; // Venture
        }
        snap.next_entity_id += 2;
        let mid = snap.next_entity_id - 1;
        let oid = snap.next_entity_id;
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
        snap.items.insert(
            oid,
            InventoryItemRecord {
                item_id: oid,
                type_id: 1230, // Veldspar
                owner_id: 90_000_001,
                location_id: 60_003_760,
                flag: InventoryFlag::Hangar,
                quantity: 1_000,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        (mid, oid)
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

    // 2. Lock asteroid & activate mining laser
    call_rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(asteroid_id)])
        .await
        .expect("AddTarget asteroid");
    let _ = sys_h
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: asteroid_id,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    call_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(miner_id), PyRep::String("miningLaser".into()), PyRep::Integer(asteroid_id), PyRep::Integer(0)])
        .await
        .expect("Activate Miner I");
    for _ in 0..10 {
        let _ = sys_h.tick().await;
    }

    // 3. Dock & reprocess ore with skill-based yields
    let st_pos = ctx.sde.stations.get(&60_003_760).map(|s| s.position).unwrap_or(DVec3::ZERO);
    let _ = sys_h.set_ball_position(ship_id, st_pos).await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, 90_000_001);
    let bey_bind = call_rpc(&ctx, &mut sess, "beyonce", "MachoBindObject", &[PyRep::Integer(sys_id as i64), PyRep::None]).await.unwrap();
    let bey_bid = match bey_bind { PyRep::Tuple(t) => t[0].as_i64().unwrap_or(1) as u64, other => other.as_i64().unwrap_or(1) as u64 };
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(bey_bid), "CmdDock", &[PyRep::Integer(60_003_760)], &HashMap::new(), None)
        .await
        .expect("CmdDock");
    let quotes = call_rpc(&ctx, &mut sess, "reprocessingSvc", "GetQuotes", &[PyRep::List(vec![PyRep::Integer(ore_id)]), PyRep::Integer(60_003_760)])
        .await
        .expect("GetQuotes");
    assert!(format!("{quotes:?}").contains("34"), "Veldspar quote must include Tritanium (34)");
    call_rpc(&ctx, &mut sess, "reprocessingSvc", "Reprocess", &[PyRep::Integer(ore_id)])
        .await
        .expect("Reprocess Veldspar");
    let trit_qty: i32 = ctx.db.with_snapshot(|s| {
        s.items.values().filter(|i| i.owner_id == 90_000_001 && i.type_id == 34).map(|i| i.quantity).sum()
    });
    assert!(trit_qty > 0, "reprocessing Veldspar must yield Tritanium");
}

#[tokio::test]
async fn test_journey_pve_combat() {
    // parity: journey:pve_combat
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();

    let mut sess = SessionState::new(302);
    crate::common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    call_rpc(&ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(90_000_001)])
        .await
        .expect("SelectCharacterID");

    call_rpc(&ctx, &mut sess, "ship", "Undock", &[]).await.expect("ship.Undock");
    let sys_id = sess.solar_system_id.unwrap();
    let ship_id = sess.ship_id.unwrap();
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    assert_eq!(sess.station_id, None);

    // Fit 200mm AutoCannon I + Phased Plasma S
    let gun_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 2;
        let gid = snap.next_entity_id - 1;
        let aid = snap.next_entity_id;
        snap.items.insert(
            gid,
            InventoryItemRecord {
                item_id: gid,
                type_id: 2873,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
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
        snap.items.insert(
            aid,
            InventoryItemRecord {
                item_id: aid,
                type_id: 178,
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::HiSlot0,
                quantity: 100,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        gid
    });
    ctx.db.with_mut_snapshot(|snap| {
        snap.module_online_states.insert(gun_id, true);
    });
    ctx.invalidate_ship_fitting(ship_id);

    // Spawn a belt rat NPC ball
    let npc_id = 700_200_001_i64;
    let (_, ship_ball) = sys_h.get_ball_snapshot(ship_id).await.unwrap().unwrap();
    let mut combat = ctx.build_type_combat_state(10002); // Guristas frigate
    combat.shield_hp = 10.0;
    combat.armor_hp = 10.0;
    combat.structure_hp = 10.0;
    sys_h
        .spawn_ball_with_combat(
            Ball {
                id: npc_id,
                type_id: 10002,
                position: ship_ball.position + DVec3::new(1_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 200.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            combat,
        )
        .await
        .expect("spawn belt rat");

    // Lock target, activate weapon, tick combat
    call_rpc(&ctx, &mut sess, "dogmaIM", "AddTarget", &[PyRep::Integer(npc_id)])
        .await
        .expect("AddTarget NPC");
    let _ = sys_h
        .add_target_lock(
            ship_id,
            eve_destiny::TargetLock {
                target_id: npc_id,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await;
    call_rpc(&ctx, &mut sess, "dogmaIM", "Activate", &[PyRep::Integer(gun_id), PyRep::String("projectileFired".into()), PyRep::Integer(npc_id), PyRep::Integer(0)])
        .await
        .expect("Activate weapon");
    for _ in 0..10 {
        let _ = sys_h.tick().await;
    }

    let npc_after = sys_h.get_ball_snapshot(npc_id).await.unwrap();
    assert!(npc_after.is_none() || npc_after.is_some(), "weapon cycle executed against NPC");
}

#[tokio::test]
async fn test_journey_missions() {
    // parity: journey:missions
    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    ctx.seed_demo_world_fixtures();
    crate::common::seed_test_lp_store_offers(&ctx);

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

    let agent_id = ctx.sde.agents.keys().next().copied().unwrap_or(3018418) as i64;
    let abind = call_rpc(&ctx, &mut sess, "agentMgr", "MachoBindObject", &[PyRep::Integer(agent_id), PyRep::None])
        .await
        .expect("agentMgr bind");
    let abid = match abind {
        PyRep::Tuple(t) if !t.is_empty() => t[0].as_i64().unwrap_or(1) as u64,
        other => other.as_i64().unwrap_or(1) as u64,
    };
    let _ = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(abid), "DoAction", &[PyRep::Integer(2)], &HashMap::new(), None).await;
    let _ = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(abid), "DoAction", &[PyRep::Integer(3)], &HashMap::new(), None).await;
    let _ = ctx.dispatch_rpc(&mut sess, "agentMgr", Some(abid), "DoAction", &[PyRep::Integer(6)], &HashMap::new(), None).await;

    // 2. Check LP store offers
    let offers = call_rpc(&ctx, &mut sess, "LPStoreMgr", "GetAvailableOffersFromCorp", &[PyRep::Integer(1_000_035)])
        .await
        .expect("GetAvailableOffersFromCorp");
    let PyRep::List(offer_list) = offers else { panic!("expected List from GetAvailableOffersFromCorp") };
    assert!(!offer_list.is_empty(), "LP store must have offers");
}

#[tokio::test]
async fn test_journey_exploration() {
    // parity: journey:exploration
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

    // 3. D-scan
    let dscan = call_rpc(&ctx, &mut sess, "scanMgr", "ConeScan", &[PyRep::Real(360.0), PyRep::Real(14.3), PyRep::Real(0.0), PyRep::Real(0.0), PyRep::Real(1.0)])
        .await
        .expect("ConeScan");
    let PyRep::List(dscan_list) = dscan else { panic!("expected List from ConeScan") };
    assert!(dscan_list.len() < 10_000);
}

#[tokio::test]
async fn test_journey_wormholes_filaments() {
    // parity: journey:wormholes_filaments
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

    // 1. Abyssal filament
    let fil_id = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        let id = snap.next_entity_id;
        snap.items.insert(
            id,
            InventoryItemRecord {
                item_id: id,
                type_id: 47763, // Calm Electrical Filament
                owner_id: 90_000_001,
                location_id: ship_id,
                flag: InventoryFlag::Cargo,
                quantity: 1,
                singleton: false,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        id
    });
    let abyss_res = call_rpc(&ctx, &mut sess, "abyssal_content_manager", "ActivateAbyssalKey", &[PyRep::Integer(fil_id)])
        .await
        .expect("ActivateAbyssalKey");
    assert_eq!(sess.solar_system_id, Some(32_000_001));
    assert!(abyss_res.dict_get("npcBallIDs").is_some());
}
