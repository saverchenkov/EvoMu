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
    let mut weapon_candidates: Vec<&_> = ctx
        .sde
        .types
        .values()
        .filter(|w| w.category_id == 7 && w.group_id == 55 && w.capacity >= 5.0)
        .collect();
    weapon_candidates.sort_by_key(|w| w.type_id);
    let (w_tid, c1_tid, c2_tid) = weapon_candidates
        .into_iter()
        .find_map(|w| {
            let mut charges: Vec<u32> = ctx
                .sde
                .types
                .values()
                .filter(|c| {
                    ServerContext::is_charge_compatible_with_weapon(w, c)
                        && c.volume > 0.0
                        && c.volume <= 0.1
                        && (w.capacity / c.volume) < 5_000.0
                })
                .map(|c| c.type_id)
                .collect();
            charges.sort_unstable();
            charges.truncate(2);
            (charges.len() == 2).then(|| (w.type_id, charges[0], charges[1]))
        })
        .expect("SDE has projectile turret and two compatible charges");
    {
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
            for tid in [w_tid, c1_tid, c2_tid] {
                for (sk_tid, _) in ctx.sde.required_skills_for_type(tid) {
                    snap.skills.entry(char1).or_default().insert(
                        sk_tid,
                        TrainedSkillRecord {
                            type_id: sk_tid,
                            level: 5,
                            skill_points: 256_000.0,
                        },
                    );
                }
            }
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
    let cfg_ctx = test_ctx();
    {
        let mut flags = cfg_ctx
            .feature_flags
            .write()
            .expect("feature_flags lock poisoned");
        flags.enable_living_world = false;
        flags.enable_quasar_gateway = false;
    }

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

#[test]
fn runtime_persistence_ewar_intern_blobs_notifications_probes_and_tactical_rekey() {
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    use eve_db::{OwnerNoteRecord, WorldRepository};
    use eve_proto::PyRep;

    use crate::router::cache_notifications::NotificationRecord;
    use crate::router::context::ActiveScannerProbe;
    use crate::router::context_runtime_persistence::SERVER_RUNTIME_STATE_BLOB_KEY;
    use crate::services::tactical_modules::db_ptr_key;

    let ctx1 = test_ctx();
    let sde = Arc::clone(&ctx1.sde);
    let ship_a = 100_000_701_i64;
    let ship_b = 100_000_702_i64;
    let probe_active = 700_000_001_i64;
    let probe_neg_state = 700_000_002_i64;

    // 1. Write-lock deadlock guard: persist inside with_mut_snapshot returns safely without deadlocking.
    ctx1.active_ewar_starts
        .write()
        .unwrap()
        .insert((ship_a, ship_b), vec![(1, "warpScramblerMWD")]);
    ctx1.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert("stale".into(), "keep".into());
        ctx1.persist_server_runtime_to_db();
        // Should not have written server_runtime_json while write-locked by current thread.
        assert!(snap.cluster_and_runtime.server_runtime_json.is_none());
    });

    // 2. Populate tactical_state with both instance_id-keyed (tag 1) and db_ptr_key-keyed (tag 0) entries
    let inst1 = ctx1.instance_id as usize;
    let dbk1 = db_ptr_key(&ctx1.db);
    {
        let mut tac = ctx1.tactical_state.write().unwrap();
        tac.lance_caster_lock_until.insert((inst1, ship_a), 1_001);
        tac.lance_caster_lock_until.insert((dbk1, ship_b), 1_002);
        tac.lance_victim_debuff_until.insert((inst1, ship_a), 2_001);
        tac.lance_victim_debuff_until.insert((dbk1, ship_b), 2_002);
        tac.tsb_self_penalty_until.insert((inst1, ship_a), 3_001);
        tac.tsb_self_penalty_until.insert((dbk1, ship_b), 3_002);
        tac.clone_vat_bay_activated_ships.insert((inst1, ship_a));
        tac.clone_vat_bay_activated_ships.insert((dbk1, ship_b));
        tac.entosis_target_cycles
            .insert((inst1, ship_a, 99), 4);
        tac.entosis_target_cycles
            .insert((dbk1, ship_b, 98), 5);
        tac.system_sim_time_s.insert((inst1, 30_000_142), 12.5);
        tac.system_sim_time_s.insert((dbk1, 30_000_144), 25.0);
    }

    // Populate scanners + negative probe (filtered on hydrate) and Persist.
    let make_probe = |probe_id: i64, state: i32| ActiveScannerProbe {
        probe_id,
        probe_type_id: 30_013,
        owner_char_id: 90_000_001,
        ship_id: ship_a,
        launcher_module_id: 100_000_999,
        solar_system_id: 30_000_142,
        position: [1.0, 2.0, 3.0],
        destination: [4.0, 5.0, 6.0],
        range_step: 4,
        scan_range_m: 1_000_000.0,
        base_sensor_strength: 40.0,
        max_flight_time_s: 3_600.0,
        launched_at_unix: 1_700_000_000,
        state,
        warp_end_ft: Some(133_500_000_000_000_000),
    };
    ctx1.active_scanner_probes
        .write()
        .unwrap()
        .insert(probe_active, make_probe(probe_active, 2));
    ctx1.active_scanner_probes
        .write()
        .unwrap()
        .insert(probe_neg_state, make_probe(probe_neg_state, -1));

    // Seed legacy blob key to verify persist clears legacy keys once persisted to cluster_and_runtime.
    ctx1.db.with_mut_snapshot(|snap| {
        snap.service_state_blobs
            .insert(SERVER_RUNTIME_STATE_BLOB_KEY.into(), "{}".into());
        snap.sp6_lifecycle.server_runtime_json = Some("{}".into());
    });

    ctx1.persist_server_runtime_to_db();

    let exported_sp6 = ctx1.db.with_snapshot(|snap| {
        assert!(!snap
            .service_state_blobs
            .contains_key(SERVER_RUNTIME_STATE_BLOB_KEY));
        assert!(snap.cluster_and_runtime.server_runtime_json.is_none());
        assert!(snap.sp6_lifecycle.server_runtime_json.is_none());
        assert!(snap.sp6_lifecycle.active_scanner_probes.contains_key(&probe_active));
        snap.sp6_lifecycle.active_scanner_probes.clone()
    });
    assert!(ctx1
        .db
        .with_sqlite_conn(|conn| eve_db::sqlite_store::system_checkpoint::has_any_system_checkpoints(conn))
        .expect("system_checkpoint rows written"));

    // 3. Hydrate into a distinct ServerContext sharing the same SQLite checkpoints (and via a separate
    // db2 to verify legacy monolith migration drops crash-lost `active_ewar_starts` / `entosis_target_cycles`),
    // probe state normalization (`state > 0 -> 0`, `state < 0` dropped, `warp_end_ft -> None`),
    // owner_notes `-79` notification filtering/sorting, and `macho_global_config`.
    let db2 = Arc::new(WorldRepository::with_demo_seed());
    let ctx2 = ServerContext::with_sde_and_repository(Arc::clone(&sde), Arc::clone(&db2));
    assert_ne!(ctx1.instance_id, ctx2.instance_id);
    assert_ne!(db_ptr_key(&ctx1.db), db_ptr_key(&ctx2.db));

    // Export durable tactical checkpoints from ctx1's SQLite into db2's SQLite `system_checkpoint`
    // and also inject a legacy `SERVER_RUNTIME_STATE_BLOB_KEY` payload containing crash-lost EWAR
    // to verify crash-lost entries are never resurrected.
    let durable_rows = ctx1
        .db
        .with_sqlite_conn(|conn| {
            eve_db::sqlite_store::system_checkpoint::load_all_checkpoints_for_kind::<serde_json::Value>(
                conn,
                eve_db::sqlite_store::system_checkpoint::KIND_TACTICAL_DURABLE,
            )
        })
        .expect("load durable tactical rows");
    let sim_rows = ctx1
        .db
        .with_sqlite_conn(|conn| {
            eve_db::sqlite_store::system_checkpoint::load_all_checkpoints_for_kind::<serde_json::Value>(
                conn,
                eve_db::sqlite_store::system_checkpoint::KIND_SYSTEM_SIM_TIME,
            )
        })
        .expect("load sim time rows");
    ctx2.db
        .with_sqlite_conn(|conn| -> Result<(), String> {
            eve_db::sqlite_store::system_checkpoint::ensure_system_checkpoint_schema(conn)?;
            for (sys_id, key, val) in &durable_rows {
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    *sys_id,
                    eve_db::sqlite_store::system_checkpoint::KIND_TACTICAL_DURABLE,
                    key,
                    val,
                    1_700_000_000,
                )?;
            }
            for (sys_id, key, val) in &sim_rows {
                eve_db::sqlite_store::system_checkpoint::upsert_system_checkpoint(
                    conn,
                    *sys_id,
                    eve_db::sqlite_store::system_checkpoint::KIND_SYSTEM_SIM_TIME,
                    key,
                    val,
                    1_700_000_000,
                )?;
            }
            Ok(())
        })
        .expect("seed db2 system checkpoints");

    let legacy_crash_lost_json = serde_json::json!({
        "active_ewar_starts": [[[ship_a, ship_b], [[1, "warpScramblerMWD"]]]]
    })
    .to_string();

    let n_rec_10 = NotificationRecord {
        notification_id: 10,
        type_id: 5,
        group_id: 1,
        sender_id: 1,
        receiver_id: 90_000_001,
        processed: false,
        created: 100,
        data: "ten".into(),
        deleted: false,
        interacted: false,
    };
    let mut n_rec_2 = n_rec_10.clone();
    n_rec_2.notification_id = 2;
    n_rec_2.data = "two".into();
    let mut n_rec_other = n_rec_10.clone();
    n_rec_other.notification_id = 99;
    n_rec_other.data = "ignored_char_neg_80".into();

    ctx2.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.server_runtime_json = None;
        snap.sp6_lifecycle.server_runtime_json = None;
        snap.service_state_blobs
            .insert(SERVER_RUNTIME_STATE_BLOB_KEY.into(), legacy_crash_lost_json.clone());
        snap.service_state_blobs.insert(
            "macho_global_config".into(),
            serde_json::to_string(&HashMap::from([("maxUsers".to_string(), PyRep::Integer(500))])).unwrap(),
        );
        snap.sp6_lifecycle.active_scanner_probes = exported_sp6;
        snap.owner_notes.insert(
            1,
            OwnerNoteRecord {
                note_id: 1,
                character_id: -79,
                item_id: 90_000_001,
                label: "notif:10".into(),
                note: serde_json::to_string(&n_rec_10).unwrap(),
                timestamp_unix: 10,
            },
        );
        snap.owner_notes.insert(
            2,
            OwnerNoteRecord {
                note_id: 2,
                character_id: -79,
                item_id: 90_000_001,
                label: "notif:2".into(),
                note: serde_json::to_string(&n_rec_2).unwrap(),
                timestamp_unix: 2,
            },
        );
        snap.owner_notes.insert(
            3,
            OwnerNoteRecord {
                note_id: 3,
                character_id: -79,
                item_id: 90_000_001,
                label: "notif:bad".into(),
                note: "{corrupt".into(),
                timestamp_unix: 3,
            },
        );
        snap.owner_notes.insert(
            4,
            OwnerNoteRecord {
                note_id: 4,
                character_id: -80,
                item_id: 90_000_001,
                label: "other".into(),
                note: serde_json::to_string(&n_rec_other).unwrap(),
                timestamp_unix: 4,
            },
        );
    });
    ctx2.hydrate_server_runtime_from_db();

    // Verify crash-lost active_ewar_starts remains empty and legacy monolith blob was retired
    assert!(ctx2.active_ewar_starts.read().unwrap().is_empty());
    assert!(ctx2
        .db
        .with_snapshot(|snap| !snap.service_state_blobs.contains_key(SERVER_RUNTIME_STATE_BLOB_KEY)));

    // Verify notifications sorted and `-80` / corrupt JSON excluded
    {
        let notifs = ctx2.notification_records.read().unwrap();
        let ids: Vec<i64> = notifs.iter().map(|n| n.notification_id).collect();
        assert_eq!(ids, vec![2, 10]);
    }
    assert_eq!(
        ctx2.global_config_values.read().unwrap().get("maxUsers"),
        Some(&PyRep::Integer(500))
    );

    // Verify scanner probes: state > 0 reset to 0, warp_end_ft cleared, state < 0 dropped
    {
        let probes = ctx2.active_scanner_probes.read().unwrap();
        assert!(!probes.contains_key(&probe_neg_state));
        let p = &probes[&probe_active];
        assert_eq!(p.state, 0);
        assert_eq!(p.warp_end_ft, None);
    }

    // Verify tactical_state cross-context key migration (durable kinds kept, crash-lost entosis dropped)
    let inst2 = ctx2.instance_id as usize;
    let dbk2 = db_ptr_key(&ctx2.db);
    {
        let tac = ctx2.tactical_state.read().unwrap();
        assert_eq!(tac.lance_caster_lock_until.get(&(inst2, ship_a)), Some(&1_001));
        assert_eq!(tac.lance_caster_lock_until.get(&(dbk2, ship_b)), Some(&1_002));
        assert_eq!(tac.lance_victim_debuff_until.get(&(inst2, ship_a)), Some(&2_001));
        assert_eq!(tac.lance_victim_debuff_until.get(&(dbk2, ship_b)), Some(&2_002));
        assert_eq!(tac.tsb_self_penalty_until.get(&(inst2, ship_a)), Some(&3_001));
        assert_eq!(tac.tsb_self_penalty_until.get(&(dbk2, ship_b)), Some(&3_002));
        assert!(tac.clone_vat_bay_activated_ships.contains(&(inst2, ship_a)));
        assert!(tac.clone_vat_bay_activated_ships.contains(&(dbk2, ship_b)));
        assert!(tac.entosis_target_cycles.is_empty());
        assert_eq!(tac.system_sim_time_s.get(&(inst2, 30_000_142)), Some(&12.5));
        assert_eq!(tac.system_sim_time_s.get(&(dbk2, 30_000_144)), Some(&25.0));
    }

    // 4. Dirty in-memory mutations are discarded cleanly on `rehydrate_runtime_from_db()`
    ctx2.scanned_signatures.write().unwrap().insert((90_000_001, 30_000_142, 42));
    ctx2.rehydrate_runtime_from_db();
    assert!(ctx2.scanned_signatures.read().unwrap().is_empty());

    // 5. Corrupt JSON in legacy `server_runtime_json` clears runtime state gracefully when no checkpoints exist
    ctx2.db
        .with_sqlite_conn(|conn| {
            eve_db::sqlite_store::system_checkpoint::clear_system_checkpoints_for_system(conn, 0)?;
            eve_db::sqlite_store::system_checkpoint::clear_system_checkpoints_for_system(
                conn,
                30_000_142,
            )?;
            eve_db::sqlite_store::system_checkpoint::clear_system_checkpoints_for_system(
                conn,
                30_000_144,
            )?;
            Ok::<_, String>(())
        })
        .expect("clear checkpoints");
    ctx2.db.with_mut_snapshot(|snap| {
        snap.cluster_and_runtime.server_runtime_json = Some("{invalid_json".into());
    });
    ctx2.hydrate_server_runtime_from_db();
    assert!(ctx2.active_ewar_starts.read().unwrap().is_empty());

    // 6. Diff sync of removed scanner probe & clearing empty server_runtime_json + legacy keys
    ctx2.active_scanner_probes.write().unwrap().clear();
    ctx2.persist_server_runtime_to_db();
    ctx2.db.with_snapshot(|snap| {
        assert!(snap.cluster_and_runtime.server_runtime_json.is_none());
        assert!(snap.sp6_lifecycle.server_runtime_json.is_none());
        assert!(!snap
            .service_state_blobs
            .contains_key(SERVER_RUNTIME_STATE_BLOB_KEY));
        assert!(snap.sp6_lifecycle.active_scanner_probes.is_empty());
    });
    let _ = HashSet::<i64>::new();
}

#[tokio::test]
async fn session_transition_variants_builders_resolution_and_sync_branches() {
    use eve_db::{InventoryFlag, UpwellStructureRecord};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;

    use crate::router::{
        apply_character_transition_in_snap, apply_session_from_character,
        build_session_state_keyval, clear_session_character_state, emit_on_session_changed,
        resolve_destination_location, sync_session_from_character, SessionTransitionSpec,
        TransitionKind, TransitionLocation,
    };

    // 1. All 10 TransitionKind::destination_location & target_ship_id branches
    let cid = 90_000_001_i64;
    let jita_sys = 30_000_142_u32;
    let amarr_sys = 30_002_187_u32;
    let jita_station = 60_003_760_i64;
    let upwell_id = 1_055_000_111_i64;

    assert_eq!(
        TransitionKind::Dock { dock_id: jita_station, is_structure: false }.destination_location(),
        TransitionLocation::Station { solar_system_id: None, station_id: jita_station }
    );
    assert_eq!(
        TransitionKind::Dock { dock_id: upwell_id, is_structure: true }.destination_location(),
        TransitionLocation::Structure { solar_system_id: None, structure_id: upwell_id }
    );
    assert_eq!(
        TransitionKind::Undock { solar_system_id: jita_sys }.destination_location(),
        TransitionLocation::Space { solar_system_id: jita_sys }
    );
    assert_eq!(
        TransitionKind::JumpSystem { solar_system_id: amarr_sys }.destination_location(),
        TransitionLocation::Space { solar_system_id: amarr_sys }
    );
    assert_eq!(
        TransitionKind::BoardShip { ship_id: 55 }.destination_location(),
        TransitionLocation::KeepCurrent
    );
    assert_eq!(TransitionKind::BoardShip { ship_id: 55 }.target_ship_id(), Some(55));
    assert_eq!(
        TransitionKind::EjectToPod { capsule_id: 77 }.destination_location(),
        TransitionLocation::KeepCurrent
    );
    assert_eq!(TransitionKind::EjectToPod { capsule_id: 77 }.target_ship_id(), Some(77));
    assert_eq!(
        TransitionKind::CloneJump {
            solar_system_id: jita_sys,
            dock_id: Some((upwell_id, true)),
            ship_id: Some(88),
        }
        .destination_location(),
        TransitionLocation::Structure { solar_system_id: Some(jita_sys), structure_id: upwell_id }
    );
    assert_eq!(
        TransitionKind::CloneJump {
            solar_system_id: jita_sys,
            dock_id: None,
            ship_id: None,
        }
        .destination_location(),
        TransitionLocation::Space { solar_system_id: jita_sys }
    );
    assert_eq!(
        TransitionKind::DeathRespawn {
            solar_system_id: jita_sys,
            dock_id: Some((jita_station, false)),
            ship_id: 66,
        }
        .target_ship_id(),
        Some(66)
    );
    assert_eq!(
        TransitionKind::Teleport {
            solar_system_id: amarr_sys,
            dock_id: Some((jita_station, false)),
            ship_id: Some(99),
        }
        .target_ship_id(),
        Some(99)
    );
    assert_eq!(
        TransitionKind::CharacterLogin {
            solar_system_id: jita_sys,
            dock_id: None,
            ship_id: None,
        }
        .destination_location(),
        TransitionLocation::Space { solar_system_id: jita_sys }
    );
    assert_eq!(TransitionKind::Logout.destination_location(), TransitionLocation::ClearAll);
    assert_eq!(TransitionKind::Logout.target_ship_id(), None);

    // 2. SessionTransitionSpec defaults & all 10 builders
    let pos = glam::DVec3::new(100.0, 200.0, 300.0);
    let spec = SessionTransitionSpec::for_kind(
        cid,
        TransitionKind::Teleport { solar_system_id: jita_sys, dock_id: None, ship_id: None },
        "teleport",
    )
    .with_apply_db(false)
    .with_move_ship_item(false)
    .with_validate_destination(false)
    .with_emit_on_session_changed(false)
    .with_sync_local_chat(false)
    .with_invalidate_bounds(false)
    .with_cleanup_departing_space(false)
    .with_arrive_ball_pos(pos)
    .with_accept_ballpark_dock(upwell_id, pos)
    .with_preserve_session_ship(true);
    assert!(!spec.apply_db);
    assert!(!spec.move_ship_item);
    assert!(!spec.validate_destination);
    assert!(!spec.emit_on_session_changed);
    assert!(!spec.sync_local_chat);
    assert!(!spec.invalidate_bounds);
    assert!(!spec.cleanup_departing_space);
    assert_eq!(spec.arrive_ball_pos, Some(pos));
    assert_eq!(spec.accept_ballpark_dock, Some((upwell_id, pos)));
    assert!(spec.preserve_session_ship);

    // 3. resolve_destination_location & apply_character_transition_in_snap edges
    let ctx = test_ctx();
    let ship_id = ctx.db.with_snapshot(|s| s.characters[&cid].active_ship_id);
    ctx.db.with_mut_snapshot(|snap| {
        snap.structures.insert(
            upwell_id,
            UpwellStructureRecord {
                structure_id: upwell_id,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: jita_sys,
                name: "Citadel".into(),
                position: [0.0, 0.0, 0.0],
                state: "ShieldVulnerable".into(),
                quantum_core_installed: true,
                fuel_blocks: 100,
                ..Default::default()
            },
        );
        snap.structures.insert(
            upwell_id + 1,
            UpwellStructureRecord {
                structure_id: upwell_id + 1,
                type_id: 35_832,
                owner_corp_id: 98_000_001,
                solar_system_id: jita_sys,
                name: "Wrecked".into(),
                position: [0.0, 0.0, 0.0],
                state: "Destroyed".into(),
                ..Default::default()
            },
        );
    });

    ctx.db.with_mut_snapshot(|snap| {
        // Space(0) rejected even when validate_destination == false
        let ch = snap.characters[&cid].clone();
        assert!(resolve_destination_location(
            snap,
            &ctx.sde,
            cid,
            &ch,
            TransitionLocation::Space { solar_system_id: 0 },
            false,
        )
        .is_err());

        // Unknown system accepted iff validate_destination == false
        assert!(resolve_destination_location(
            snap,
            &ctx.sde,
            cid,
            &ch,
            TransitionLocation::Space { solar_system_id: 999_999 },
            true,
        )
        .is_err());
        let res_no_val = resolve_destination_location(
            snap,
            &ctx.sde,
            cid,
            &ch,
            TransitionLocation::Space { solar_system_id: 999_999 },
            false,
        )
        .unwrap();
        assert_eq!(
            (res_no_val.solar_system_id, res_no_val.station_id, res_no_val.structure_id),
            (999_999, None, None)
        );

        // Destroyed Upwell structure rejected
        assert!(resolve_destination_location(
            snap,
            &ctx.sde,
            cid,
            &ch,
            TransitionLocation::Structure {
                solar_system_id: None,
                structure_id: upwell_id + 1,
            },
            true,
        )
        .is_err());

        // Active Upwell structure resolves system hierarchy and clears station_id
        let resolved_st = resolve_destination_location(
            snap,
            &ctx.sde,
            cid,
            &ch,
            TransitionLocation::Structure {
                solar_system_id: None,
                structure_id: upwell_id,
            },
            true,
        )
        .unwrap();
        assert_eq!(resolved_st.solar_system_id, jita_sys);
        assert_ne!(resolved_st.constellation_id, 0);
        assert_ne!(resolved_st.region_id, 0);
        assert_eq!(resolved_st.station_id, None);
        assert_eq!(resolved_st.structure_id, Some(upwell_id));

        // KeepCurrent fills 0 constellation/region without incrementing system_epoch
        let ch_mut = snap.characters.get_mut(&cid).unwrap();
        ch_mut.constellation_id = 0;
        ch_mut.region_id = 0;
        let epoch_before = ch_mut.system_epoch;
        let kept = apply_character_transition_in_snap(
            snap,
            &ctx.sde,
            cid,
            TransitionLocation::KeepCurrent,
            None,
            false,
            true,
        )
        .unwrap();
        assert_eq!(kept.system_epoch, epoch_before);
        assert_ne!(kept.constellation_id, 0);
        assert_ne!(kept.region_id, 0);

        // Moving to Space moves ship item to system with Flag None and bumps system_epoch
        let spaced = apply_character_transition_in_snap(
            snap,
            &ctx.sde,
            cid,
            TransitionLocation::Space { solar_system_id: jita_sys },
            None,
            true,
            true,
        )
        .unwrap();
        assert_eq!(spaced.system_epoch, epoch_before + 1);
        assert_eq!(snap.items[&ship_id].location_id, jita_sys as i64);
        assert_eq!(snap.items[&ship_id].flag, InventoryFlag::None);

        // Moving to Structure moves ship item to structure Hangar
        let docked_str = apply_character_transition_in_snap(
            snap,
            &ctx.sde,
            cid,
            TransitionLocation::Structure {
                solar_system_id: Some(jita_sys),
                structure_id: upwell_id,
            },
            None,
            true,
            true,
        )
        .unwrap();
        assert_eq!(docked_str.structure_id, Some(upwell_id));
        assert_eq!(snap.items[&ship_id].location_id, upwell_id);
        assert_eq!(snap.items[&ship_id].flag, InventoryFlag::Hangar);
    });

    // 4. apply_session_from_character, build_session_state_keyval (structure vs space),
    // clear_session_character_state, emit_on_session_changed, sync_session_from_character(None)
    let mut sess = SessionState::new(77_501);
    sess.user_id = Some(1);
    sess.character_id = Some(cid);
    let mut ch_for_sess = ctx.db.get_character(cid).unwrap();
    // Active ship <= 0 + structure_id Some(..) -> keyval omits shipid/stationid/solarsystemid, includes structureid
    ch_for_sess.active_ship_id = 0;
    apply_session_from_character(&mut sess, &ch_for_sess, Some(Some(ship_id)), Some(&ctx.sde));
    assert_eq!(sess.ship_id, Some(ship_id));
    apply_session_from_character(&mut sess, &ch_for_sess, None, None);
    assert_eq!(sess.ship_id, None);
    let kv_struct = build_session_state_keyval(&sess, cid);
    assert_eq!(
        kv_struct.dict_get("structureid"),
        Some(&PyRep::Integer(upwell_id))
    );
    assert_eq!(kv_struct.dict_get("solarsystemid"), Some(&PyRep::None));
    assert_eq!(kv_struct.dict_get("shipid"), Some(&PyRep::None));
    let snap_sess = sess.clone();
    let empty_diff = std::collections::HashMap::new();
    let notif_len_before = sess.notifications.len();
    emit_on_session_changed(&ctx, Some(&mut sess), &snap_sess, cid, &empty_diff);
    assert_eq!(sess.notifications.len(), notif_len_before);
    let non_empty_diff = std::collections::HashMap::from([(
        "solarsystemid".to_string(),
        (PyRep::None, PyRep::Integer(jita_sys as i64)),
    )]);
    emit_on_session_changed(&ctx, Some(&mut sess), &snap_sess, cid, &non_empty_diff);
    assert_eq!(sess.notifications.len(), notif_len_before + 1);

    // Clear session via sync_session_from_character(None) & clear_session_character_state
    sync_session_from_character(&ctx, &mut sess, Some(&snap_sess), None, "logout", false, false, false);
    assert_eq!(sess.solar_system_id, None);
    assert_eq!(sess.structure_id, None);
    clear_session_character_state(&mut sess);

    // 5. execute_session_transition_sync(None) + async Teleport + CharacterLogin
    let sync_res = ctx
        .execute_session_transition_sync(
            None,
            SessionTransitionSpec::for_kind(
                cid,
                TransitionKind::Undock { solar_system_id: jita_sys },
                "sync_undock",
            ),
        )
        .expect("sync undock without caller session");
    assert_eq!(sync_res.updated_character.as_ref().unwrap().station_id, None);
    assert_eq!(sync_res.updated_character.as_ref().unwrap().structure_id, None);
    assert!(!sync_res.diff.is_empty());

    let tp_res = ctx
        .execute_session_transition(
            Some(&mut sess),
            SessionTransitionSpec::for_kind(
                cid,
                TransitionKind::Teleport {
                    solar_system_id: amarr_sys,
                    dock_id: None,
                    ship_id: Some(ship_id),
                },
                "teleport_amarr",
            ),
        )
        .await
        .expect("teleport to Amarr space");
    assert_eq!(tp_res.updated_character.as_ref().unwrap().solar_system_id, amarr_sys);
    assert_eq!(sess.solar_system_id, Some(amarr_sys));
    assert_eq!(sess.ship_id, Some(ship_id));

    // Default CharacterLogin has apply_db=false and uses persisted character location from DB
    let login_res = ctx
        .execute_session_transition(
            Some(&mut sess),
            SessionTransitionSpec::for_kind(
                cid,
                TransitionKind::CharacterLogin {
                    solar_system_id: jita_sys,
                    dock_id: Some((jita_station, false)),
                    ship_id: Some(ship_id),
                },
                "login",
            ),
        )
        .await
        .expect("character login transition");
    assert_eq!(login_res.updated_character.as_ref().unwrap().solar_system_id, amarr_sys);
    assert_eq!(login_res.updated_character.as_ref().unwrap().station_id, None);
    assert_eq!(sess.station_id, None);

    // CharacterLogin with explicit with_apply_db(true) commits the dock destination to DB and session
    let login_db_res = ctx
        .execute_session_transition(
            Some(&mut sess),
            SessionTransitionSpec::for_kind(
                cid,
                TransitionKind::CharacterLogin {
                    solar_system_id: jita_sys,
                    dock_id: Some((jita_station, false)),
                    ship_id: Some(ship_id),
                },
                "login_commit",
            )
            .with_apply_db(true),
        )
        .await
        .expect("character login with_apply_db");
    assert_eq!(login_db_res.updated_character.as_ref().unwrap().station_id, Some(jita_station));
    assert_eq!(sess.station_id, Some(jita_station));
}
