//! Regression tests for Round-7 TB security & mechanics fixes:
//! `#435`, `#436`, `#439`, `#440`, `#442`.

use std::collections::HashMap;
use std::sync::Arc;

use eve_db::{AsteroidRecord, InventoryFlag, InventoryItemRecord, SolarSystemId, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::{service_roles, SessionState};
use eve_proto::PyRep;
use glam::DVec3;
use uuid::Uuid;

use crate::bug_reporting::{
    handle_bugs_http_request, BugIncidentBundle, MAX_BUG_ISSUE_BODY_BYTES,
};
use crate::combat::drones_salvage_tractor::execute_wreck_salvage_auto_repeat;
use crate::living_world::{LivingWorldConvoy, LivingWorldDirector, NpcArchetype};
use crate::router::context::ServerContext;
use crate::services::serverops_cluster::{
    complete_cluster_downtime_restart, is_downtime_active_in_db, run_daily_downtime_and_jobs,
    start_daily_downtime_window,
};
use crate::system_actor::SystemRegistry;

#[tokio::test]
async fn bug_reporting_blocks_unauth_session_recon_and_caps_disk_growth_435() {
    let ctx = ServerContext::new();
    let temp_dir = std::env::temp_dir().join(format!("evomu_r7_p1b_{}", Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir).expect("create temp queue dir");
    ctx.bug_reporting.set_queue_dir_override(Some(temp_dir.clone()));
    ctx.bug_reporting.set_storage_limits(3, 64 * 1024);

    let (victim_uid, victim_cid, victim_ship, _sys_id) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.values().next().expect("seeded character");
        (ch.user_id, ch.character_id, ch.active_ship_id, ch.solar_system_id)
    });
    let victim_sid: u64 = 88_001;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&victim_cid) {
            ch.station_id = None;
            ch.structure_id = None;
        }
        snap.item_positions
            .insert(victim_ship, [12_345.0, -67_890.0, 54_321.0]);
    });
    let _ = ctx.db.acquire_online_lock(
        victim_uid,
        victim_sid,
        Some(victim_cid),
        false,
        ctx.now_unix(),
    );
    ctx.flight_recorder.record(
        victim_sid,
        "beyonce",
        None,
        "CmdWarpToStuff",
        &[PyRep::Integer(50_000_001)],
        &HashMap::new(),
        std::time::Duration::from_millis(5),
        "Ok",
    );

    // 1. Unauthenticated POST /bugs/api/issue spoofing victim's originalReporterId & Session Info
    // must NOT snapshot victim's live coordinates, session ID, character ID, or FlightRecorder.
    let spoofed_payload = serde_json::json!({
        "categoryID": "Gameplay",
        "summary": "Unauth recon probe",
        "description": format!("Details\n\nSession Info:\nCharacter: SecretVictim ({victim_cid})"),
        "originalReporterId": victim_uid
    });
    let spoof_resp = handle_bugs_http_request(
        &ctx,
        "POST",
        "/bugs/api/issue",
        &HashMap::new(),
        spoofed_payload.to_string().as_bytes(),
    );
    assert_eq!(spoof_resp.status_code, 201);
    let key_json: serde_json::Value =
        serde_json::from_slice(&spoof_resp.body).expect("parse bug key");
    let spoof_key = key_json["key"].as_str().expect("report key");
    let spoof_path = ctx
        .bug_reporting
        .lookup_report_path(spoof_key)
        .expect("report path");
    let saved_bundle: BugIncidentBundle =
        serde_json::from_str(&std::fs::read_to_string(&spoof_path).unwrap()).unwrap();
    assert_eq!(saved_bundle.session.session_id, 0);
    assert_eq!(saved_bundle.session.user_id, None);
    assert_eq!(saved_bundle.session.character_id, None);
    assert!(saved_bundle.session.coordinates.is_none());
    assert!(saved_bundle.flight_recorder.is_empty());

    // 2. Oversized request body must be rejected with 413 Payload Too Large.
    let huge = vec![b'a'; MAX_BUG_ISSUE_BODY_BYTES + 64];
    let too_large =
        handle_bugs_http_request(&ctx, "POST", "/bugs/api/issue", &HashMap::new(), &huge);
    assert_eq!(too_large.status_code, 413);

    // 3. Storing more reports than max_reports (3) evicts oldest bundles and bounds report_paths.
    for i in 0..5 {
        let p = serde_json::json!({
            "categoryID": "Gameplay",
            "summary": format!("Report #{i}"),
            "description": "Routine bug report"
        });
        let r = handle_bugs_http_request(
            &ctx,
            "POST",
            "/bugs/api/issue",
            &HashMap::new(),
            p.to_string().as_bytes(),
        );
        assert_eq!(r.status_code, 201);
    }
    assert!(ctx.bug_reporting.stored_report_count() <= 3);
    let json_count = std::fs::read_dir(&temp_dir)
        .unwrap()
        .flatten()
        .filter(|e| e.path().extension().and_then(|s| s.to_str()) == Some("json"))
        .count();
    assert!(json_count <= 3);
    let _ = std::fs::remove_dir_all(&temp_dir);
}

#[tokio::test]
async fn photo_upload_requires_auth_caps_size_and_avoids_duplication_436() {
    let ctx = ServerContext::new();
    let (uid, cid) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.values().next().expect("seeded character");
        (ch.user_id, ch.character_id)
    });

    // 1. Authenticated session without selected character cannot fall back to 140000001 on 1-arg Upload.
    let mut no_char_sess = SessionState::new(101);
    no_char_sess.user_id = Some(uid);
    no_char_sess.character_id = None;
    let unauth_char_res = ctx
        .dispatch_rpc(
            &mut no_char_sess,
            "photoUploadSvc", // rpc-lint: allow legacy Crucible / parity ledger service feat:character.portrait-upload
            None,
            "Upload",
            &[PyRep::Bytes(vec![0xff, 0xd8, 0xff, 0xe0])],
            &HashMap::new(),
            None,
        )
        .await;
    assert!(unauth_char_res.is_err());

    // 2. Oversized portrait (> 512 KiB) must be rejected with PortraitTooLarge.
    let mut sess = SessionState::new(102);
    sess.user_id = Some(uid);
    sess.character_id = Some(cid);
    let oversized = vec![0xab; 512 * 1024 + 1];
    let big_res = ctx
        .dispatch_rpc(
            &mut sess,
            "photoUploadSvc", // rpc-lint: allow legacy Crucible / parity ledger service feat:character.portrait-upload
            None,
            "Upload",
            &[PyRep::Integer(cid), PyRep::Bytes(oversized)],
            &HashMap::new(),
            None,
        )
        .await;
    assert_eq!(big_res.unwrap_err().code, "PortraitTooLarge");

    // 3. Valid portrait upload stores ONLY canonical `portrait:{cid}` and no size-duplicated copies.
    let ok_res = ctx
        .dispatch_rpc(
            &mut sess,
            "photoUploadSvc", // rpc-lint: allow legacy Crucible / parity ledger service feat:character.portrait-upload
            None,
            "Upload",
            &[PyRep::Integer(cid), PyRep::Bytes(vec![0xff, 0xd8, 0xff, 0xe0])],
            &HashMap::new(),
            None,
        )
        .await
        .expect("upload ok");
    assert_eq!(ok_res, PyRep::Bool(true));

    ctx.db.with_snapshot(|snap| {
        assert_eq!(
            snap.service_state_blobs.get(&format!("portrait:{cid}")),
            Some(&"ffd8ffe0".to_string())
        );
        for sz in [32, 64, 128, 256, 512, 1024] {
            assert!(!snap
                .service_state_blobs
                .contains_key(&format!("portrait:{cid}:{sz}")));
        }
    });
}

#[tokio::test]
async fn wreck_salvage_executes_single_timed_cycle_with_cap_and_repeat_schedule_439() {
    let ctx = ServerContext::new();
    let (char_id, ship_id, sys_id) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.values().next().expect("seeded character");
        (ch.character_id, ch.active_ship_id, ch.solar_system_id)
    });
    let wreck_id: i64 = 100_700_004;
    let salvager_type_id: u32 = 25_861; // Salvager I

    ctx.db.with_mut_snapshot(|snap| {
        snap.skills
            .entry(char_id)
            .or_default()
            .entry(3438)
            .and_modify(|s| s.level = 5)
            .or_insert(eve_db::TrainedSkillRecord {
                type_id: 3438,
                level: 5,
                skill_points: 256_000.0,
            });
        snap.items.insert(
            wreck_id,
            InventoryItemRecord {
                item_id: wreck_id,
                type_id: 26_481,
                owner_id: char_id,
                location_id: sys_id as i64,
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball(Ball {
            id: ship_id,
            type_id: 587,
            position: DVec3::ZERO,
            velocity: DVec3::ZERO,
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    let _ = sys
        .spawn_ball(Ball {
            id: wreck_id,
            type_id: 26_481,
            position: DVec3::new(1_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 50_000.0,
            radius: 30.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;

    // 1. Insufficient capacitor blocks even a single salvage cycle.
    let _ = sys.set_capacitor(ship_id, 0.0).await;
    let no_cap_err = execute_wreck_salvage_auto_repeat(
        &ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_id,
        Some(salvager_type_id),
    )
    .await
    .unwrap_err();
    assert_eq!(no_cap_err.code, "NotEnoughCapacitor");

    // 2. With full capacitor and a failing roll, executes ONLY ONE attempt, consumes one cycle of cap,
    //    leaves wreck in space, and queues an ActiveModuleCycle timer instead of running 32 loops.
    let _ = sys.set_capacitor(ship_id, 500.0).await;
    let rep = execute_wreck_salvage_auto_repeat(
        &ctx,
        sys_id,
        char_id,
        ship_id,
        wreck_id,
        Some(salvager_type_id),
    )
    .await
    .expect("single cycle completes cleanly");
    if rep.dict_get("salvaged").and_then(|v| v.as_bool()) == Some(false) {
        assert_eq!(
            rep.dict_get("reason").and_then(|v| v.as_str()),
            Some("CycleRollFailed")
        );
        let cs = sys.get_combat_state(ship_id).await.unwrap().unwrap();
        assert!(cs.capacitor < 500.0);
        assert!(sys.has_active_module_cycle_sync(ship_id, wreck_id));
    }
}

#[tokio::test]
async fn npc_mining_op_merges_station_ore_stacks_and_despawns_idle_hauler_440() {
    let sde = Arc::new(eve_sde::SdeRegistry::with_canonical_seed());
    let db = Arc::new(WorldRepository::new_empty());
    let systems = Arc::new(SystemRegistry::with_sde_and_db(sde.clone(), db.clone()));
    let director = LivingWorldDirector::new();

    let sys_id: SolarSystemId = 30_000_144;
    let station_id: i64 = 60_003_766;
    let op_id: i64 = 800_700_001;
    let hauler_ball_id = op_id + 1_000_000;
    let ore_type_id: u32 = 1_230; // Veldspar

    let ast_pos = DVec3::new(100_000.0, 0.0, 0.0);
    db.with_mut_snapshot(|snap| {
        snap.asteroids.insert(
            70_700_001,
            AsteroidRecord {
                asteroid_id: 70_700_001,
                solar_system_id: sys_id,
                belt_id: 40_009_001,
                type_id: ore_type_id,
                position: (ast_pos.x, ast_pos.y, ast_pos.z),
                radius: 500.0,
                quantity: 100,
                initial_quantity: 100_000,
            },
        );
    });

    director.register_convoy(LivingWorldConvoy {
        convoy_id: op_id,
        archetype: NpcArchetype::MiningOp,
        ship_type_id: 32_880,
        origin_system_id: sys_id,
        current_system_id: sys_id,
        destination_system_id: sys_id,
        destination_station_id: station_id,
        cargo_type_id: ore_type_id,
        cargo_quantity: 0,
        lod_level: 1,
        destroyed: false,
    });

    // Tick 1: spawns miner & companion hauler.
    let r1 = director
        .step_npc_mining_op(op_id, &systems, &db, &sde)
        .await
        .expect("step 1");
    assert_eq!(r1.ore_type_id, ore_type_id);

    // Execute two separate hauls into station_id and verify station hangar ore merges into ONE stack.
    let handle = systems.get_or_spawn(sys_id);
    for qty in [600, 350] {
        db.with_mut_snapshot(|snap| {
            snap.items.insert(
                9_000_000 + qty as i64,
                InventoryItemRecord {
                    item_id: 9_000_000 + qty as i64,
                    type_id: ore_type_id,
                    owner_id: 1_000_035,
                    location_id: hauler_ball_id,
                    flag: InventoryFlag::Cargo,
                    quantity: qty,
                    ..Default::default()
                },
            );
        });
        db.clock().advance_secs(20);
        let step = director
            .step_npc_mining_op(op_id, &systems, &db, &sde)
            .await
            .expect("haul delivery");
        assert!(step.hauled_to_station);
    }

    let station_stacks: Vec<i32> = db.with_snapshot(|snap| {
        snap.items
            .values()
            .filter(|it| {
                it.owner_id == 1
                    && it.location_id == station_id
                    && it.type_id == ore_type_id
                    && it.flag == InventoryFlag::Hangar
                    && !it.singleton
            })
            .map(|it| it.quantity)
            .collect()
    });
    assert_eq!(station_stacks, vec![950]);

    // When the mining convoy finishes (no remaining asteroids / destroyed), the hauler ball
    // is cleanly removed from space rather than leaking permanently.
    director.mark_convoy_destroyed(op_id);
    db.clock().advance_secs(30);
    let _ = director.step_npc_mining_op(op_id, &systems, &db, &sde).await;
    assert!(handle.get_ball_snapshot(hauler_ball_id).await.unwrap().is_none());
}

#[tokio::test]
async fn downtime_window_disconnects_sessions_and_blocks_non_gm_rpcs_442() {
    let ctx = ServerContext::new();
    let (player_uid, player_cid) = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.values().next().expect("seeded player character");
        (ch.user_id, ch.character_id)
    });
    let player_sid: u64 = 91_010;
    let _ = ctx.db.acquire_online_lock(
        player_uid,
        player_sid,
        Some(player_cid),
        false,
        ctx.now_unix(),
    );

    let report = start_daily_downtime_window(&ctx);
    assert!(report["disconnectedSessions"] >= 1);
    assert!(is_downtime_active_in_db(&ctx));
    ctx.db.with_snapshot(|snap| {
        assert!(!snap.sessions.contains_key(&player_uid));
    });

    // Even if `ctx.cluster_state.shutdown_in_progress` is cleared in memory while DB downtime
    // remains active, non-GM gameplay RPCs are still rejected and session state cleared.
    if let Ok(mut cs) = ctx.cluster_state.write() {
        cs.shutdown_in_progress = false;
    }
    assert!(is_downtime_active_in_db(&ctx));

    let mut player_sess = SessionState::new(player_sid);
    player_sess.user_id = Some(player_uid);
    player_sess.role = service_roles::ROLE_PLAYER;
    player_sess.character_id = Some(player_cid);
    player_sess.solar_system_id = Some(30_000_142);

    let rpc_err = ctx
        .dispatch_rpc(
            &mut player_sess,
            "beyonce",
            None,
            "CmdWarpToStuff",
            &[PyRep::Integer(50_000_001)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(rpc_err.code, "ClusterShuttingDown");
    assert_eq!(player_sess.user_id, None);
    assert_eq!(player_sess.character_id, None);

    // Completing cluster restart / daily downtime job re-enables gameplay.
    let _ = complete_cluster_downtime_restart(&ctx);
    assert!(!is_downtime_active_in_db(&ctx));
    let full_rep = run_daily_downtime_and_jobs(&ctx);
    assert!(full_rep["clusterRestartEpoch"] >= 1);
    assert!(!is_downtime_active_in_db(&ctx));
}
