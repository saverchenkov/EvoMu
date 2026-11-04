//! Parity integration tests remediating open/flagged PVE feature rows:
//! - `feat:pve.dungeon-system`
//! - `feat:pve.events-sites`
//! - `feat:pve.mercenary-tactical-ops`
//! - `feat:pve.milestones`

use eve_db::d6_types::{PersistedMercenaryActivity, PersistedMercenaryDen};
use eve_db::WorldRepository;
use eve_destiny::MovementMode;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::pve_dungeon_runtime::{
    get_dungeon_instance_state, on_dungeon_npc_killed,
};
use eve_server::services::pve_dungeon_runtime_rpc::{
    on_dungeon_approach, on_dungeon_item_looted, spawn_dungeon_instance,
    try_activate_dungeon_runtime_gate,
};
use eve_server::services::pve_special_sites::{
    ensure_seasonal_event_sites_for_system, on_special_site_npc_destroyed_core,
};
use eve_server::services::season_fsd::load_season_config;
use eve_server::services::season_manager::load_season_state_from_blobs;
use eve_server::services::sov_gameplay::on_space_entity_destroyed_ext;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

const JITA: u32 = 30_000_142;
const PILOT_A: i64 = 90_000_001;

fn new_ctx() -> ServerContext {
    let sde = Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path())
            .expect("open evomu_sde.sqlite"),
    );
    let db = Arc::new(WorldRepository::with_demo_seed());
    ServerContext::with_sde_and_repository(sde, db)
}

fn hex_str(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

async fn setup_pilot(ctx: &ServerContext, char_id: i64, sys_id: u32) -> (SessionState, i64) {
    let mut sess = SessionState::new(char_id as u64);
    crate::common::login_pilot(ctx, &mut sess, "demo", "demo-secret").await;
    ctx.dispatch_rpc(
        &mut sess,
        "charUnboundMgr",
        None,
        "SelectCharacterID",
        &[PyRep::Integer(char_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("SelectCharacterID");
    let ship_id = ctx.db.with_mut_snapshot(|snap| {
        let ch = snap.characters.get_mut(&char_id).expect("demo pilot");
        ch.solar_system_id = sys_id;
        ch.station_id = None;
        ch.active_ship_id
    });
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.ship_id = Some(ship_id);

    // Ensure ship ball is present in the ballpark
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let initial_pos = DVec3::new(120_000_000.0, 0.0, 80_000_000.0);
    let ship_ball = eve_destiny::Ball {
        id: ship_id,
        type_id: 587,
        radius: 100.0,
        position: initial_pos,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.0,
        mass: 1_000_000.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball(ship_ball).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.item_positions.insert(ship_id, [initial_pos.x, initial_pos.y, initial_pos.z]);
    });

    (sess, ship_id)
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None)
        .await
}

#[tokio::test]
async fn test_pve_dungeon_system_authentic_lifecycle() {
    // parity: feat:pve.dungeon-system
    eve_testkit::attest(&["feat:pve.dungeon-system"]);

    let ctx = new_ctx();
    let (mut sess, ship_id) = setup_pilot(&ctx, PILOT_A, JITA).await;
    let _ = ctx.drain_notifications_for_char(PILOT_A);

    // 1. Define and spawn multi-room dungeon instance with objects, NPC groups, and triggers
    let room1 = PyRep::Dict(vec![
        (PyRep::String("roomName".into()), PyRep::String("Staging Pocket".into())),
        (PyRep::String("gateLocked".into()), PyRep::Bool(true)),
        (
            PyRep::String("objects".into()),
            PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("name".into()), PyRep::String("Deadspace Supply Cache".into())),
                (PyRep::String("typeID".into()), PyRep::Integer(17_996)),
                (PyRep::String("isContainer".into()), PyRep::Bool(true)),
                (PyRep::String("radius".into()), PyRep::Real(500.0)),
                (PyRep::String("offset".into()), PyRep::List(vec![PyRep::Real(1_500.0), PyRep::Real(0.0), PyRep::Real(0.0)])),
                (PyRep::String("lootItems".into()), PyRep::List(vec![
                    PyRep::List(vec![PyRep::Integer(34), PyRep::Integer(50)]),
                    PyRep::List(vec![PyRep::Integer(2838), PyRep::Integer(1)]),
                ])),
            ])]),
        ),
        (
            PyRep::String("npcGroups".into()),
            PyRep::List(vec![
                PyRep::Dict(vec![
                    (PyRep::String("groupID".into()), PyRep::Integer(1)),
                    (PyRep::String("wave".into()), PyRep::Integer(1)),
                    (PyRep::String("spawnImmediately".into()), PyRep::Bool(true)),
                    (PyRep::String("npcs".into()), PyRep::List(vec![PyRep::List(vec![PyRep::Integer(10_043), PyRep::Integer(2)])])),
                ]),
                PyRep::Dict(vec![
                    (PyRep::String("groupID".into()), PyRep::Integer(2)),
                    (PyRep::String("wave".into()), PyRep::Integer(2)),
                    (PyRep::String("spawnImmediately".into()), PyRep::Bool(false)),
                    (PyRep::String("npcs".into()), PyRep::List(vec![PyRep::List(vec![PyRep::Integer(10_043), PyRep::Integer(1)])])),
                ]),
            ]),
        ),
        (
            PyRep::String("triggers".into()),
            PyRep::List(vec![
                PyRep::Dict(vec![
                    (PyRep::String("kind".into()), PyRep::String("approach".into())),
                    (PyRep::String("action".into()), PyRep::String("TriggerMessage".into())),
                    (PyRep::String("targetObjectIndex".into()), PyRep::Integer(0)),
                    (PyRep::String("radius".into()), PyRep::Real(3_000.0)),
                ]),
                PyRep::Dict(vec![
                    (PyRep::String("kind".into()), PyRep::String("loot".into())),
                    (PyRep::String("action".into()), PyRep::String("SpawnNpcGroup".into())),
                    (PyRep::String("actionGroup".into()), PyRep::Integer(2)),
                    (PyRep::String("targetObjectIndex".into()), PyRep::Integer(0)),
                ]),
                PyRep::Dict(vec![
                    (PyRep::String("kind".into()), PyRep::String("kill".into())),
                    (PyRep::String("action".into()), PyRep::String("UnlockGate".into())),
                ]),
            ]),
        ),
    ]);

    let room2 = PyRep::Dict(vec![
        (PyRep::String("roomName".into()), PyRep::String("Sanctum Pocket".into())),
        (PyRep::String("gateLocked".into()), PyRep::Bool(false)),
        (
            PyRep::String("objects".into()),
            PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("name".into()), PyRep::String("Pirate Command Outpost".into())),
                (PyRep::String("typeID".into()), PyRep::Integer(24_690)),
                (PyRep::String("isContainer".into()), PyRep::Bool(false)),
                (PyRep::String("radius".into()), PyRep::Real(1_000.0)),
            ])]),
        ),
        (
            PyRep::String("npcGroups".into()),
            PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("groupID".into()), PyRep::Integer(1)),
                (PyRep::String("wave".into()), PyRep::Integer(1)),
                (PyRep::String("spawnImmediately".into()), PyRep::Bool(true)),
                (PyRep::String("npcs".into()), PyRep::List(vec![PyRep::List(vec![PyRep::Integer(17_615), PyRep::Integer(1)])])),
            ])]),
        ),
        (
            PyRep::String("triggers".into()),
            PyRep::List(vec![PyRep::Dict(vec![
                (PyRep::String("kind".into()), PyRep::String("kill".into())),
                (PyRep::String("action".into()), PyRep::String("CompleteDungeon".into())),
            ])]),
        ),
    ]);

    let kw = HashMap::from([
        ("solarSystemID".to_string(), PyRep::Integer(JITA as i64)),
        ("dungeonID".to_string(), PyRep::Integer(3_501)),
        ("dungeonName".to_string(), PyRep::String("Serpentis Multi-Room Complex".into())),
        ("category".to_string(), PyRep::String("Deadspace".into())),
        ("hasEntryGate".to_string(), PyRep::Bool(false)),
        (
            "position".to_string(),
            PyRep::List(vec![PyRep::Real(120_000_000.0), PyRep::Real(0.0), PyRep::Real(80_000_000.0)]),
        ),
        ("rooms".to_string(), PyRep::List(vec![room1, room2])),
    ]);

    let spawn_rep = spawn_dungeon_instance(&ctx, PILOT_A, Some(JITA), &[], &kw)
        .await
        .expect("spawn_dungeon_instance");
    let inst_id = spawn_rep
        .dict_get("instanceID")
        .and_then(|v| v.as_i64())
        .expect("dungeon instanceID");

    // 2. Verify Room 1 object, gate, and initial NPC spawning
    let sys_handle = ctx.systems.get_or_spawn(JITA);
    let (room1_gate_id, cont_ball_id, wave1_npc_ids) = ctx.db.with_snapshot(|snap| {
        let store: eve_server::services::pve_dungeon_runtime::DungeonRuntimeStore = snap
            .service_state_blobs
            .get("dungeon_runtime_store")
            .and_then(|s| serde_json::from_str(s).ok())
            .expect("dungeon store");
        let inst = &store.instances[&inst_id];
        let r1 = &inst.rooms[0];
        (
            r1.gate_ball_id.expect("room 1 gate ball id"),
            r1.objects[0].ball_id,
            r1.npc_groups[0].npc_ball_ids.clone(),
        )
    });

    assert!(sys_handle.get_ball_snapshot(room1_gate_id).await.ok().flatten().is_some());
    assert!(sys_handle.get_ball_snapshot(cont_ball_id).await.ok().flatten().is_some());
    assert_eq!(wave1_npc_ids.len(), 2);
    for &nid in &wave1_npc_ids {
        assert!(sys_handle.get_ball_snapshot(nid).await.ok().flatten().is_some());
    }

    // When far from gate (> 2500m), activating it fails with OutsideGateActivationRange
    let range_err = try_activate_dungeon_runtime_gate(
        &ctx,
        &mut sess,
        JITA,
        &[PyRep::Integer(room1_gate_id)],
    )
    .await
    .expect("gate handler result")
    .expect_err("far gate activation must fail range check");
    assert_eq!(range_err.code, "OutsideGateActivationRange");

    // Move within range of locked gate
    let gate_snap0 = sys_handle
        .get_ball_snapshot(room1_gate_id)
        .await
        .ok()
        .flatten()
        .expect("gate ball");
    let _ = sys_handle
        .set_ball_position(ship_id, gate_snap0.1.position + DVec3::new(500.0, 0.0, 0.0))
        .await;

    // Acceleration gate starts locked: activating within range fails with GateLocked
    let gate_locked_err = try_activate_dungeon_runtime_gate(
        &ctx,
        &mut sess,
        JITA,
        &[PyRep::Integer(room1_gate_id)],
    )
    .await
    .expect("gate handler result")
    .expect_err("locked gate activation must fail");
    assert_eq!(gate_locked_err.code, "GateLocked");

    // 3. Test OnApproach trigger on container object
    let approach_pos = DVec3::new(120_001_500.0, 0.0, 80_000_000.0);
    let _ = sys_handle.set_ball_position(ship_id, approach_pos).await;
    let fired_approach = on_dungeon_approach(&ctx, PILOT_A, JITA, ship_id, Some(cont_ball_id)).await;
    assert!(fired_approach >= 1, "OnApproach trigger must fire");

    // 4. Test OnLoot trigger on container: looting spawns Wave 2 NPCs
    on_dungeon_item_looted(&ctx, PILOT_A, Some(JITA), cont_ball_id, 34).await;
    let wave2_npc_ids = ctx.db.with_snapshot(|snap| {
        let store: eve_server::services::pve_dungeon_runtime::DungeonRuntimeStore = snap
            .service_state_blobs
            .get("dungeon_runtime_store")
            .and_then(|s| serde_json::from_str(s).ok())
            .expect("dungeon store");
        store.instances[&inst_id].rooms[0].npc_groups[1].npc_ball_ids.clone()
    });
    assert_eq!(wave2_npc_ids.len(), 1, "Wave 2 NPC must spawn from loot trigger");
    assert!(sys_handle.get_ball_snapshot(wave2_npc_ids[0]).await.ok().flatten().is_some());

    // 5. Test OnKill triggers & Acceleration Gate unlock
    for &nid in &wave1_npc_ids {
        on_dungeon_npc_killed(&ctx, JITA, nid, PILOT_A).await;
    }
    on_dungeon_npc_killed(&ctx, JITA, wave2_npc_ids[0], PILOT_A).await;

    let is_unlocked = ctx.db.with_snapshot(|snap| {
        let store: eve_server::services::pve_dungeon_runtime::DungeonRuntimeStore = snap
            .service_state_blobs
            .get("dungeon_runtime_store")
            .and_then(|s| serde_json::from_str(s).ok())
            .expect("dungeon store");
        !store.instances[&inst_id].rooms[0].gate_locked
    });
    assert!(is_unlocked, "Room 1 acceleration gate must unlock when NPCs are killed");

    // 6. Test authentic Room Transition through acceleration gate
    // Position ship within activation range (<= 2500m of gate)
    let gate_snap = sys_handle
        .get_ball_snapshot(room1_gate_id)
        .await
        .ok()
        .flatten()
        .expect("gate ball");
    let near_gate_pos = gate_snap.1.position + DVec3::new(500.0, 0.0, 0.0);
    let _ = sys_handle.set_ball_position(ship_id, near_gate_pos).await;

    let gate_activate_rep = try_activate_dungeon_runtime_gate(
        &ctx,
        &mut sess,
        JITA,
        &[PyRep::Integer(room1_gate_id)],
    )
    .await
    .expect("gate handler result")
    .expect("gate activation must succeed");
    assert_eq!(
        gate_activate_rep.dict_get("success").and_then(|v| v.as_bool()),
        Some(true)
    );
    assert_eq!(
        gate_activate_rep.dict_get("roomIndex").and_then(|v| v.as_i64()),
        Some(2)
    );

    // Verify pilot was translated to Room 2 coordinates
    let dest_x = gate_activate_rep.dict_get("destinationX").and_then(|v| v.as_f64()).unwrap();
    let ship_post_warp = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten().unwrap().1.position;
    assert!((ship_post_warp.x - dest_x).abs() < 10.0, "Ship ball must move to Room 2 landing");

    // Verify Room 2 objects & boss NPC spawned in ballpark
    let (room2_obj_id, room2_boss_id) = ctx.db.with_snapshot(|snap| {
        let store: eve_server::services::pve_dungeon_runtime::DungeonRuntimeStore = snap
            .service_state_blobs
            .get("dungeon_runtime_store")
            .and_then(|s| serde_json::from_str(s).ok())
            .expect("dungeon store");
        let r2 = &store.instances[&inst_id].rooms[1];
        (r2.objects[0].ball_id, r2.npc_groups[0].npc_ball_ids[0])
    });
    assert!(sys_handle.get_ball_snapshot(room2_obj_id).await.ok().flatten().is_some());
    assert!(sys_handle.get_ball_snapshot(room2_boss_id).await.ok().flatten().is_some());

    // 7. Test Dungeon Completion & Despawn mechanics
    let st_before = get_dungeon_instance_state(&ctx, inst_id).expect("state before");
    assert_eq!(st_before.dict_get("completed").and_then(|v| v.as_bool()), Some(false));
    assert_eq!(st_before.dict_get("despawned").and_then(|v| v.as_bool()), Some(false));

    let _ = ctx.drain_notifications_for_char(PILOT_A);
    // Destroying the Room 2 Boss in combat removes its ball and fires CompleteDungeon
    let _ = sys_handle.remove_ball(room2_boss_id).await;
    on_dungeon_npc_killed(&ctx, JITA, room2_boss_id, PILOT_A).await;

    let st_after = get_dungeon_instance_state(&ctx, inst_id).expect("state after");
    assert_eq!(st_after.dict_get("completed").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(st_after.dict_get("despawned").and_then(|v| v.as_bool()), Some(true));

    // Verify all dungeon entities are despawned from ballpark
    assert!(sys_handle.get_ball_snapshot(room1_gate_id).await.ok().flatten().is_none());
    assert!(sys_handle.get_ball_snapshot(cont_ball_id).await.ok().flatten().is_none());
    assert!(sys_handle.get_ball_snapshot(room2_obj_id).await.ok().flatten().is_none());
    assert!(sys_handle.get_ball_snapshot(room2_boss_id).await.ok().flatten().is_none());

    // Verify OnDungeonCompleted notification was emitted
    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert!(
        notifs.iter().any(|n| n.event == "OnDungeonCompleted"),
        "OnDungeonCompleted must be dispatched upon completion"
    );
}

#[tokio::test]
async fn test_pve_seasonal_event_sites_and_loot_tables() {
    // parity: feat:pve.events-sites
    eve_testkit::attest(&["feat:pve.events-sites"]);

    let ctx = new_ctx();
    let (mut sess, _) = setup_pilot(&ctx, PILOT_A, JITA).await;
    let _ = &mut sess;

    // 1. Establish active seasonal event window
    let now_ft = ctx.now_blueos_filetime_ticks();
    let season_cfg = ctx.db.with_mut_snapshot(|snap| {
        let mut cfg = load_season_config(snap);
        cfg.start_time_ft = now_ft - 3_600 * 10_000_000;
        cfg.end_time_ft = now_ft + 3_600 * 10_000_000;
        if let Ok(json) = serde_json::to_string(&cfg) {
            snap.service_state_blobs.insert(
                eve_server::services::season_fsd::SEASON_CONFIG_KEY.to_string(),
                json,
            );
        }
        cfg
    });

    // 2. Spawn seasonal event site in Jita
    let active_sites = ensure_seasonal_event_sites_for_system(&ctx, JITA).await;
    assert_eq!(active_sites.len(), 1, "Seasonal event site must spawn in active window");
    let site = &active_sites[0];
    let boss_bid = site.boss_npc_ball_id;
    let escort_bids = site.escort_ball_ids.clone();

    // Verify event NPCs are spawned in ballpark with combat AI
    let sys_handle = ctx.systems.get_or_spawn(JITA);
    assert!(sys_handle.get_ball_snapshot(boss_bid).await.ok().flatten().is_some());
    for &ebid in &escort_bids {
        assert!(sys_handle.get_ball_snapshot(ebid).await.ok().flatten().is_some());
    }

    // Verify cosmic anomaly registered
    let has_anom = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&JITA)
            .is_some_and(|l| l.iter().any(|a| a.anomaly_type == "SeasonalEventSite" && !a.cleared))
    });
    assert!(has_anom, "Cosmic anomaly must be present for event site");

    // 3. Destroy event boss NPC and verify authentic seasonal loot tables, ISK, and points
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    let pts_before = ctx
        .db
        .with_snapshot(|s| load_season_state_from_blobs(s, PILOT_A).season_points);

    let changes = on_special_site_npc_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        JITA,
        boss_bid,
        Some(PILOT_A),
    )
    .await;
    assert!(!changes.is_empty(), "Account balance change must be returned");

    let bal_after = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    let pts_after = ctx
        .db
        .with_snapshot(|s| load_season_state_from_blobs(s, PILOT_A).season_points);
    assert_eq!(bal_after - bal_before, 750_000 * 100, "Event site must award 750k ISK");
    assert_eq!(pts_after - pts_before, 25, "Event site must award 25 season points");

    // Verify journal entry for bounty prize
    let has_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|e| e.character_id == PILOT_A && e.entry_type == "BountyPrizes" && e.reason.contains("Seasonal Event Site"))
    });
    assert!(has_journal, "Bounty prize journal entry must be recorded");

    // Verify seasonal event loot items deposited into container or cargo
    let loot_present = ctx.db.with_snapshot(|snap| {
        snap.items
            .values()
            .any(|it| it.owner_id == PILOT_A && (it.type_id == 2838 || it.type_id == 9950 || it.type_id == 2048))
    });
    assert!(loot_present, "Seasonal event loot (Cerebral Accelerator / Booster) must be deposited");

    // Verify escorts cleaned up and anomaly cleared
    for &ebid in &escort_bids {
        assert!(sys_handle.get_ball_snapshot(ebid).await.ok().flatten().is_none());
    }
    let anom_cleared = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&JITA)
            .is_some_and(|l| l.iter().all(|a| a.anomaly_type != "SeasonalEventSite" || a.cleared))
    });
    assert!(anom_cleared, "Cosmic anomaly must be marked cleared");

    // 4. Verify despawn mechanics outside seasonal window
    let past_end_unix = ((season_cfg.end_time_ft - 116_444_736_000_000_000) / 10_000_000) as u64 + 60;
    let delta = past_end_unix.saturating_sub(ctx.now_unix());
    ctx.db.clock().advance_secs(delta);
    let expired_sites = ensure_seasonal_event_sites_for_system(&ctx, JITA).await;
    assert!(expired_sites.is_empty(), "Event sites must despawn outside active window");
}

#[tokio::test]
async fn test_pve_mercenary_tactical_operations_infomorph_and_isk() {
    // parity: feat:pve.mercenary-tactical-ops
    eve_testkit::attest(&["feat:pve.mercenary-tactical-ops"]);

    let ctx = new_ctx();
    let (mut sess, ship_id) = setup_pilot(&ctx, PILOT_A, JITA).await;
    let _ = &mut sess;

    let den_id = 1_892_100_000_i64;
    let mto_ball_id = 1_892_200_001_i64;
    let mto_uuid = vec![0x11; 16];

    // 1. Initialize mercenary den with base development, anarchy, and infomorphs
    ctx.db.with_mut_snapshot(|snap| {
        snap.sov_runtime.mercenary_dens.insert(
            den_id,
            PersistedMercenaryDen {
                den_id,
                solar_system_id: JITA,
                planet_id: 40_009_082,
                owner_char_id: PILOT_A,
                enabled: true,
                development_level: 20,
                anarchy_level: 40,
                infomorphs: 150,
                ..Default::default()
            },
        );
        let hex_key = hex_str(&mto_uuid);
        snap.sov_runtime.mercenary_activities.insert(
            hex_key.clone(),
            PersistedMercenaryActivity {
                uuid: mto_uuid.clone(),
                den_id,
                solar_system_id: JITA,
                dungeon_id: 4_500,
                started: true,
                started_by: PILOT_A,
                expiry_unix: ctx.now_unix() + 1_800,
                development_impact: 10,
                anarchy_impact: -10,
                infomorph_bonus: 50,
            },
        );
        snap.sov_runtime.mto_site_balls.insert(mto_ball_id, hex_key);
    });

    let bal_pre = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    let den_pre = ctx.db.with_snapshot(|s| s.sov_runtime.mercenary_dens[&den_id].clone());

    // 2. Complete MTO activity via combat destruction of the tactical site
    on_space_entity_destroyed_ext(&ctx, JITA, Some(PILOT_A), mto_ball_id, 24_690, true);

    let bal_post = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    let den_post = ctx.db.with_snapshot(|s| s.sov_runtime.mercenary_dens[&den_id].clone());

    // 3. Verify ISK reward credited to pilot wallet (+7,500,000 ISK)
    assert_eq!(
        bal_post - bal_pre,
        7_500_000 * 100,
        "MTO completion must award 7.5M ISK to pilot"
    );

    // Verify journal entry for MercenaryDenReward
    let has_mto_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|e| e.character_id == PILOT_A && e.entry_type == "MercenaryDenReward")
    });
    assert!(has_mto_journal, "Journal entry for MercenaryDenReward must be recorded");

    // 4. Verify Infomorph reward credited to mercenary den cargo
    assert!(
        den_post.infomorphs > den_pre.infomorphs,
        "Den infomorphs must increase upon MTO completion (was {}, now {})",
        den_pre.infomorphs,
        den_post.infomorphs
    );
    assert_eq!(
        den_post.infomorphs - den_pre.infomorphs,
        50,
        "Den must gain +50 infomorphs"
    );

    // 5. Verify development level increased (+10) and anarchy reduced (-10)
    assert_eq!(den_post.development_level, den_pre.development_level + 10);
    assert_eq!(den_post.anarchy_level, den_pre.anarchy_level - 10);

    // 6. Verify tactical site ball was removed from state
    let mto_ball_removed = ctx.db.with_snapshot(|snap| {
        !snap.sov_runtime.mto_site_balls.contains_key(&mto_ball_id)
            && !snap.sov_runtime.mercenary_activities.contains_key(&hex_str(&mto_uuid))
    });
    assert!(mto_ball_removed, "MTO site ball and activity must be removed after completion");
    let _ = ship_id;
}

#[tokio::test]
async fn test_pve_milestones_progression_tracking() {
    // parity: feat:pve.milestones
    eve_testkit::attest(&["feat:pve.milestones"]);

    let ctx = new_ctx();
    let (mut sess, _) = setup_pilot(&ctx, PILOT_A, JITA).await;

    // 1. ClaimRewards before ProcessCharacterLogon must fail
    let pre_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect_err("ClaimRewards before logon must fail");
    assert_eq!(pre_err.code, "MilestonePrerequisiteNotMet");

    // 2. ProcessCharacterLogon initializes logon tracking and emits OnMilestoneTimerStarted
    let _ = ctx.drain_notifications_for_char(PILOT_A);
    let logon_rep = rpc(&ctx, &mut sess, "milestoneMgr", "ProcessCharacterLogon", &[])
        .await
        .expect("ProcessCharacterLogon");
    assert_eq!(logon_rep, PyRep::None);

    let notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert!(
        notifs.iter().any(|n| n.event == "OnMilestoneTimerStarted"),
        "OnMilestoneTimerStarted must be dispatched on logon"
    );

    // 3. Milestone progression tracking: Milestone 2 cannot be claimed before Milestone 1
    let skipped_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(2)])
        .await
        .expect_err("claiming milestone 2 without milestone 1 must fail prerequisite");
    assert_eq!(skipped_err.code, "MilestonePrerequisiteNotMet");

    // 4. Claim Milestone 1: grants rewards once, logs journal, emits OnLoginMilestoneAchieved
    let bal_before = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    let claim1_rep = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect("ClaimRewards(1) must succeed");
    assert_eq!(claim1_rep, PyRep::None);

    let bal_mid = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    assert!(bal_mid > bal_before, "Milestone 1 must award ISK");

    let achieved_notifs = ctx.drain_notifications_for_char(PILOT_A);
    assert!(
        achieved_notifs.iter().any(|n| n.event == "OnLoginMilestoneAchieved"),
        "OnLoginMilestoneAchieved must be dispatched"
    );

    let has_journal = ctx.db.with_snapshot(|snap| {
        snap.wallet_journal
            .iter()
            .any(|e| e.character_id == PILOT_A && e.entry_type == "MilestoneReward" && e.reason.contains("Milestone 1"))
    });
    assert!(has_journal, "Journal entry for MilestoneReward must be recorded");

    // Duplicate ClaimRewards(1) must fail closed
    let dup_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(1)])
        .await
        .expect_err("duplicate ClaimRewards(1) must fail");
    assert_eq!(dup_err.code, "MilestoneAlreadyClaimed");

    // 5. Progression advances: Milestone 2 can now be claimed
    let claim2_rep = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(2)])
        .await
        .expect("ClaimRewards(2) must succeed after Milestone 1");
    assert_eq!(claim2_rep, PyRep::None);

    let bal_after = ctx.db.with_snapshot(|s| s.characters[&PILOT_A].balance.0);
    assert!(bal_after > bal_mid, "Milestone 2 must award additional ISK");

    let dup2_err = rpc(&ctx, &mut sess, "milestoneMgr", "ClaimRewards", &[PyRep::Integer(2)])
        .await
        .expect_err("duplicate ClaimRewards(2) must fail");
    assert_eq!(dup2_err.code, "MilestoneAlreadyClaimed");
}
