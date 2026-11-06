//! Advanced unit tests covering router binding, notifications, charge swapping, skill queues, and RPC dispatch.

use super::{NotificationTarget, ServerContext};
use eve_db::{
    CosmicAnomalyRecord, InventoryFlag, InventoryItemRecord, SkillQueueEntry, TrainedSkillRecord,
    WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(
            Arc::clone(sde),
            Arc::new(WorldRepository::with_demo_seed()),
        )
        .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_item(
    item_id: i64,
    type_id: u32,
    owner_id: i64,
    location_id: i64,
    flag: InventoryFlag,
    quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id,
        type_id,
        owner_id,
        location_id,
        flag,
        quantity,
        singleton: quantity == 1 && flag == InventoryFlag::None,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: false,
        blueprint_runs_remaining: None,
    }
}

#[tokio::test]
async fn test_binding_containers_and_notifications() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let char1 = 90_000_001_i64;
    let char2 = 90_000_002_i64;

    let mut session = SessionState::new(101);
    assert!(ctx.bind_object(&mut session, "i2", 1).is_err());

    session.user_id = Some(1001);
    session.character_id = Some(char1);
    session.solar_system_id = Some(sys_id);
    session.station_id = None;
    session.structure_id = None;

    assert!(ctx.bind_object(&mut session, "populationCap", char1).is_ok());
    assert!(ctx.bind_object(&mut session, "populationCap", char2).is_err());
    assert!(ctx.bind_object(&mut session, "i2", 5).is_err());
    assert!(ctx.bind_object(&mut session, "trademgr", 10_000_002).is_ok());
    assert!(ctx.bind_object(&mut session, "trademgr", 999_999).is_err());
    assert!(ctx.bind_object(&mut session, "contractProxy", char1).is_ok());
    assert!(ctx.bind_object(&mut session, "contractProxy", char2).is_err());
    assert!(ctx.bind_object(&mut session, "nonExistentSvc", 1).is_err());

    let mut gm_session = session.clone();
    gm_session.session_id = 102;
    gm_session.role = u64::MAX;
    assert!(ctx.bind_object(&mut gm_session, "i2", 77).is_ok());

    // Spawn ship + near wreck + far wreck + non-container to exercise get_container_contents branches
    let ship_id = 910_000_001_i64;
    let near_wreck = 910_000_002_i64;
    let far_wreck = 910_000_003_i64;
    let enemy_ship = 910_000_004_i64;
    let loot_id = 910_000_005_i64;
    session.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ship_id,
            make_item(ship_id, 587, char1, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            near_wreck,
            make_item(near_wreck, 26557, char2, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            far_wreck,
            make_item(far_wreck, 26557, char1, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            enemy_ship,
            make_item(enemy_ship, 587, char2, sys_id as i64, InventoryFlag::None, 1),
        );
        snap.items.insert(
            loot_id,
            make_item(loot_id, 34, char2, near_wreck, InventoryFlag::None, 10),
        );
        if let Some(c) = snap.characters.get_mut(&char1) {
            c.alliance_id = Some(99_000_001);
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for (bid, tid, pos) in [
        (ship_id, 587_u32, DVec3::ZERO),
        (near_wreck, 26557_u32, DVec3::new(1_000.0, 0.0, 0.0)),
        (far_wreck, 26557_u32, DVec3::new(9_000.0, 0.0, 0.0)),
    ] {
        sys_handle.try_spawn_ball_with_combat(
            Ball {
                id: bid,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 1000.0,
                radius: 50.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            eve_destiny::CombatState::default(),
        );
    }

    assert!(ctx
        .get_container_contents(&session, None, char1, near_wreck)
        .await
        .is_ok());
    assert!(ctx
        .get_container_contents(&session, None, char1, far_wreck)
        .await
        .is_err());
    assert!(ctx
        .get_container_contents(&session, None, char1, enemy_ship)
        .await
        .is_err());

    let wrong_bind = super::context::BoundObjectEntry {
        bound_id: 999,
        session_id: session.session_id,
        user_id: 1001,
        character_id: char1,
        service: "invbroker".to_string(),
        object_id: ship_id,
        oid_str: "N=1:999".to_string(),
        ref_id: 1,
    };
    assert!(ctx
        .get_container_contents(&session, Some(&wrong_bind), char1, near_wreck)
        .await
        .is_err());

    // Notifications & local chat sync
    ctx.sync_character_local_chat(char1, None, sys_id);
    ctx.sync_character_local_chat(char1, Some(sys_id), 30_000_144);
    let _ = ctx.notify_item_change(char1, PyRep::Integer(loot_id), vec![(2, PyRep::Integer(1))]);
    let _ = ctx.notify_fleet_broadcast(1, "NeedArmor", char1, ship_id);
    let _ = ctx.notify(
        NotificationTarget::Alliance(99_000_001),
        "OnClientEvent_ChatMessageSent",
        "allianceid",
        vec![PyRep::Integer(1)],
    );
    let _ = ctx.notify(
        NotificationTarget::All,
        "OnBallparkSetState",
        "clientID",
        vec![PyRep::Integer(1)],
    );
}

#[tokio::test]
async fn test_sqlite_context_charges_skill_queue_and_dispatch_edges() {
    let mut db_path = std::env::temp_dir();
    db_path.push(format!("evomu_router_cov_{}.sqlite", std::process::id()));
    let _ = std::fs::remove_file(&db_path);
    {
        let sqlite_ctx = ServerContext::from_sqlite(&db_path).expect("sqlite ctx");
        assert!(!sqlite_ctx.pause_on_tick());
        sqlite_ctx.set_pause_on_tick(true);
        assert!(sqlite_ctx.pause_on_tick());
        let _ = sqlite_ctx.tick_wall_second();
    }
    let _ = std::fs::remove_file(&db_path);

    let ctx = test_ctx();
    let char1 = 90_000_001_i64;
    let ship_id = 910_000_010_i64;

    // Weapon charge compatibility (including Deep Core Mining Laser 28748 + mining crystal)
    let deep_core = &ctx.sde.types[&28_748];
    if let Some(crystal) = ctx.sde.types.values().find(|t| t.category_id == 8 && t.group_id == 482)
    {
        assert!(ServerContext::is_charge_compatible_with_weapon(deep_core, crystal));
    }

    // Charge swap & partial split in weapon slot
    let charge_a_id = 910_000_011_i64;
    let charge_b_id = 910_000_012_i64;
    let loaded_prev_id = 910_000_013_i64;
    if let Some((w_tid, c1_tid, c2_tid)) = ctx.sde.types.values().find_map(|w| {
        if w.category_id == 7 && w.group_id == 55 && w.capacity >= 5.0 {
            let charges: Vec<u32> = ctx
                .sde
                .types
                .values()
                .filter(|c| ServerContext::is_charge_compatible_with_weapon(w, c) && c.volume <= 0.1)
                .map(|c| c.type_id)
                .take(2)
                .collect();
            (charges.len() == 2).then(|| (w.type_id, charges[0], charges[1]))
        } else {
            None
        }
    }) {
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.insert(
                ship_id,
                make_item(ship_id, 587, char1, 60_003_760, InventoryFlag::None, 1),
            );
            snap.items.insert(
                910_000_014,
                make_item(910_000_014, w_tid, char1, ship_id, InventoryFlag::HiSlot0, 1),
            );
            snap.items.insert(
                loaded_prev_id,
                make_item(loaded_prev_id, c2_tid, char1, ship_id, InventoryFlag::HiSlot0, 10),
            );
            snap.items.insert(
                charge_a_id,
                make_item(charge_a_id, c1_tid, char1, ship_id, InventoryFlag::Cargo, 5_000),
            );
            snap.items.insert(
                charge_b_id,
                make_item(charge_b_id, c2_tid, char1, ship_id, InventoryFlag::Cargo, 20),
            );
        });
        let loaded = ctx
            .load_charge_into_weapon_slot(
                char1,
                ship_id,
                InventoryFlag::HiSlot0,
                charge_a_id,
                Some(60_003_760),
            )
            .expect("swap and split charge");
        assert!(loaded.2 > 0);
        // Calling again with full clip returns room == 0 early exit
        let full = ctx
            .load_charge_into_weapon_slot(
                char1,
                ship_id,
                InventoryFlag::HiSlot0,
                charge_a_id,
                Some(60_003_760),
            )
            .expect("full clip idempotent");
        assert_eq!(full.2, loaded.2);
    }

    // Skill queue advancement for Alpha clone & Omega lapse
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c) = snap.characters.get_mut(&char1) {
            c.clone_grade = 1;
        }
        snap.skill_queues.insert(
            char1,
            vec![SkillQueueEntry {
                type_id: 3438,
                target_level: 1,
                start_sp: 0.0,
                end_sp: 250.0,
                duration_seconds: 60.0,
                started_at_unix: Some(now.saturating_sub(120)),
            }],
        );
        snap.skills.entry(char1).or_default().insert(
            3402,
            TrainedSkillRecord {
                type_id: 3402,
                level: 5,
                skill_points: 5_000_100.0,
            },
        );
    });
    ctx.advance_secs(60);

    // Feature flag disabled dispatch checks + MachoBindObject piggyback
    let mut cfg_ctx = test_ctx();
    cfg_ctx.feature_flags.enable_living_world = false;
    cfg_ctx.feature_flags.enable_quasar_gateway = false;

    let mut session = SessionState::new(202);
    session.user_id = Some(1001);
    session.character_id = Some(char1);
    session.solar_system_id = Some(30_000_142);

    assert!(cfg_ctx
        .dispatch_rpc(
            &mut session,
            "taleMgr",
            None,
            "GetGlobalWorldEventTales",
            &[],
            &HashMap::new(),
            None
        )
        .await
        .is_err());
    assert!(cfg_ctx
        .dispatch_rpc(
            &mut session,
            "publicGatewaySvc",
            None,
            "PublishEvent",
            &[],
            &HashMap::new(),
            None
        )
        .await
        .is_err());
    assert!(ctx
        .dispatch_rpc(
            &mut session,
            "timeControl",
            None,
            "AdvanceClock",
            &[],
            &HashMap::new(),
            None
        )
        .await
        .is_err());

    // Piggybacked invalid method error
    assert!(ctx
        .dispatch_rpc(
            &mut session,
            "charMgr",
            None,
            "MachoBindObject",
            &[
                PyRep::Integer(char1),
                PyRep::Tuple(vec![PyRep::Integer(123)]),
            ],
            &HashMap::new(),
            None,
        )
        .await
        .is_err());

    // Sov prospecting deposit respawn tick
    let null_sys = 30_002_162_u32;
    ctx.db.with_mut_snapshot(|snap| {
        snap.cosmic_anomalies.entry(null_sys).or_default().push(CosmicAnomalyRecord {
            anomaly_id: "880001".to_string(),
            solar_system_id: null_sys,
            name: "Prospecting Array".to_string(),
            anomaly_type: "SovOre".to_string(),
            signal_strength: 1.0,
            ore_types: vec![],
            position: [1_000.0, 0.0, 0.0],
            cleared: false,
            created_unix: now.saturating_sub(10),
            lifetime_seconds: 3_600,
            remaining_volume: 0.0,
        });
    });
    super::dispatch::tick_sov_prospecting_deposit_respawn(&ctx, null_sys).await;
}
