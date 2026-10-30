//! Integration tests for `dungeonExplorationMgr.GetMyEscalatingPathDetails` (V24 journal shape,
//! SQLite restart persistence, expiration, and error paths) and all 7 PvE dungeon/escalation
//! notifications (`notify:OnDistributionDungeonEntered`, `notify:OnDungeonCompleted`,
//! `notify:OnEnteringDungeonRoom`, `notify:OnEscalatingPathChange`,
//! `notify:OnEscalatingPathDungeonEntered`, `notify:OnEscalatingPathMessage`,
//! `notify:OnExitingDungeon`).

#![forbid(unsafe_code)]

use eve_db::{InventoryFlag, InventoryItemRecord, VirtualServerClock, WorldRepository};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::router::ServerContext;
use eve_server::services::pve_expeditions::{advance_expedition_step, create_escalation_expedition};
use eve_server::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg,
};
use glam::DVec3;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const JITA_SYS: u32 = 30_000_142;
const JITA_4_4_STATION: i64 = 60_003_760;
const TYPE_RIFTER: u32 = 587;

fn open_sqlite_ctx_at(path: &Path, now_unix: u64) -> Arc<ServerContext> {
    if !path.exists() {
        let seed_ctx = ServerContext::new();
        seed_ctx.db.save_to_sqlite(path).expect("seed_sqlite");
    }
    let db = Arc::new(WorldRepository::open_sqlite(path).expect("open_sqlite"));
    let ctx = Arc::new(ServerContext::with_repository(db));
    ctx.set_clock(Arc::new(VirtualServerClock::with_fixed_base(now_unix)));
    ctx
}

async fn login_pilot_session(
    ctx: &ServerContext,
    sess_id: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let kw = HashMap::new();
    let mut sess = SessionState::new(sess_id);
    ctx.dispatch_rpc(
        &mut sess,
        "authentication",
        None,
        "Login",
        &[PyRep::String(user.into()), PyRep::String(pass.into())],
        &kw,
        None,
    )
    .await
    .expect("Login");
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

async fn put_pilot_in_space(
    ctx: &ServerContext,
    sess: &mut SessionState,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    pos: DVec3,
) {
    sess.solar_system_id = Some(sys_id);
    sess.station_id = None;
    sess.structure_id = None;
    sess.ship_id = Some(ship_id);

    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(
            ship_id,
            InventoryItemRecord {
                item_id: ship_id,
                type_id: TYPE_RIFTER,
                owner_id: char_id,
                location_id: i64::from(sys_id),
                flag: InventoryFlag::None,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
        snap.item_positions.insert(ship_id, pos.to_array());
    });

    let sys = ctx.systems.get_or_spawn(sys_id);
    let _ = sys
        .spawn_ball_with_meta(
            Ball {
                id: ship_id,
                type_id: TYPE_RIFTER,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 365.0,
                inertia: 1.0,
                mass: 1_200_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_ship_combat_state(char_id, ship_id, TYPE_RIFTER),
            ctx.build_ship_slim_meta(char_id, TYPE_RIFTER),
        )
        .await;
}

// ============================================================================
// 1. dungeonExplorationMgr.GetMyEscalatingPathDetails + Escalation Notifications
// ============================================================================
#[tokio::test]
async fn test_get_my_escalating_path_details_rpc_shape_and_sqlite_persistence() {
    // parity: rpc:dungeonExplorationMgr.GetMyEscalatingPathDetails, notify:OnEscalatingPathChange, notify:OnEscalatingPathMessage
    eve_testkit::attest(&[
        "rpc:dungeonExplorationMgr.GetMyEscalatingPathDetails",
        "notify:OnEscalatingPathChange",
        "notify:OnEscalatingPathMessage",
    ]);

    let tmp_dir = std::env::temp_dir().join(format!(
        "evomu_esc_sqlite_{}_{}",
        std::process::id(),
        9301
    ));
    let _ = std::fs::remove_dir_all(&tmp_dir);
    std::fs::create_dir_all(&tmp_dir).expect("create temp dir");
    let db_path = tmp_dir.join("world.sqlite");
    let base_ts = 1_760_000_000_u64;
    let kw = HashMap::new();

    let (instance_id, dest_sys_id, exp_dungeon_id) = {
        let ctx = open_sqlite_ctx_at(&db_path, base_ts);

        // 1. Unauthenticated caller raises UserError / Unauthenticated
        let mut unauth_sess = SessionState::new(9300);
        let unauth_err = ctx
            .dispatch_rpc(
                &mut unauth_sess,
                "dungeonExplorationMgr",
                None,
                "GetMyEscalatingPathDetails",
                &[],
                &kw,
                None,
            )
            .await
            .expect_err("Unauthenticated GetMyEscalatingPathDetails must fail");
        assert!(!unauth_err.code.is_empty());

        let mut sess = login_pilot_session(&ctx, 9301, "demo", "demo-secret", PILOT_1).await;
        put_pilot_in_space(&ctx, &mut sess, PILOT_1, 760_001_001, JITA_SYS, DVec3::ZERO).await;

        // Create fleet via real RPCs so fleetmate PILOT_2 receives OnFleetEscalatingPathMessage on escalation
        let mut sess_mate = login_pilot_session(&ctx, 9302, "other", "other-secret", PILOT_2).await;
        ctx.advance_secs(12);
        ctx.dispatch_rpc(&mut sess, "fleetMgr", None, "CreateFleet", &[], &kw, None)
            .await
            .expect("CreateFleet");
        let fleet_id = sess.fleet_id.expect("fleet_id");
        ctx.dispatch_rpc(
            &mut sess,
            "fleetObjectHandler",
            None,
            "Invite",
            &[PyRep::Integer(PILOT_2)],
            &kw,
            None,
        )
        .await
        .expect("Invite");
        ctx.dispatch_rpc(
            &mut sess_mate,
            "fleetObjectHandler",
            None,
            "AcceptInvite",
            &[PyRep::Integer(fleet_id)],
            &kw,
            None,
        )
        .await
        .expect("AcceptInvite");
        let _ = ctx.drain_notifications_for_char(PILOT_2);

        // 2. Empty journal returns exact 17-column V24 Rowset
        let empty_rep = ctx
            .dispatch_rpc(
                &mut sess,
                "dungeonExplorationMgr",
                None,
                "GetMyEscalatingPathDetails",
                &[],
                &kw,
                None,
            )
            .await
            .expect("Empty GetMyEscalatingPathDetails");
        let PyRep::Rowset { columns, rows } = &empty_rep else {
            panic!("Expected Rowset from GetMyEscalatingPathDetails");
        };
        assert_eq!(
            columns,
            &vec![
                "instanceID".to_string(),
                "sourceDungeonID".to_string(),
                "dungeonID".to_string(),
                "dungeonName".to_string(),
                "dungeonNameID".to_string(),
                "destinationSolarSystemID".to_string(),
                "solarSystemID".to_string(),
                "stepIndex".to_string(),
                "totalSteps".to_string(),
                "journalText".to_string(),
                "createdAt".to_string(),
                "expiresAt".to_string(),
                "expiryTime".to_string(),
                "journalEntryTemplateID".to_string(),
                "dungeon".to_string(),
                "destDungeon".to_string(),
                "srcDungeon".to_string(),
            ]
        );
        assert!(rows.is_empty());

        // 3. Clear Combat Anomaly with force_escalate -> triggers Escalation + OnEscalatingPathChange & OnEscalatingPathMessage
        ensure_system_pve_sites_seeded(&ctx, JITA_SYS).await;
        let site_id = "PVE-ANOM-30000142-1";
        let _ = sess.drain_notifications();
        let _ = ctx.drain_notifications_for_char(PILOT_1);
        let sys_h = ctx.systems.get_or_spawn(JITA_SYS);
        for _wave_step in 0..5 {
            let (cleared, npcs) = with_pve_exploration_reg(&ctx, |reg| {
                reg.sites_by_system[&JITA_SYS]
                    .iter()
                    .find(|s| s.site_id == site_id)
                    .map(|s| (s.cleared, s.active_npc_ball_ids.clone()))
                    .unwrap_or((true, Vec::new()))
            });
            if cleared {
                break;
            }
            if npcs.is_empty() {
                let _ = ctx
                    .dispatch_rpc(
                        &mut sess,
                        "scanSvc",
                        None,
                        "EnterCombatAnomaly",
                        &[PyRep::String(site_id.into()), PyRep::Bool(true)],
                        &kw,
                        None,
                    )
                    .await
                    .expect("EnterCombatAnomaly wave");
                continue;
            }
            ctx.set_mechanic_rng_seed(Some(3));
            for nid in npcs {
                let _ = sys_h
                    .apply_damage_with_attacker(
                        nid,
                        DamageVector {
                            em: 1.0e7,
                            thermal: 1.0e7,
                            kinetic: 1.0e7,
                            explosive: 1.0e7,
                        },
                        Some(PILOT_1),
                    )
                    .await;
                let _ = sys_h.remove_ball(nid).await;
            }
            tokio::time::sleep(std::time::Duration::from_millis(40)).await;
        }

        // Verify OnEscalatingPathChange (0 args, charid) & OnEscalatingPathMessage (1 arg [instanceID], charid)
        let mut all_p1_notifs: Vec<(String, Vec<PyRep>)> = sess.drain_notifications();
        all_p1_notifs.extend(
            ctx.drain_notifications_for_char(PILOT_1)
                .into_iter()
                .map(|n| (n.event, n.args)),
        );
        let chg = all_p1_notifs
            .iter()
            .find(|(ev, _)| ev == "OnEscalatingPathChange")
            .expect("OnEscalatingPathChange must be emitted on escalation");
        assert!(chg.1.is_empty(), "OnEscalatingPathChange takes 0 args");
        let msg = all_p1_notifs
            .iter()
            .find(|(ev, _)| ev == "OnEscalatingPathMessage")
            .expect("OnEscalatingPathMessage must be emitted on escalation");
        assert_eq!(msg.1.len(), 1, "OnEscalatingPathMessage takes [instanceID]");
        let notif_inst_id = msg.1[0].as_i64().expect("instanceID integer");

        // Verify fleetmate received OnFleetEscalatingPathMessage([charID, templateID])
        let fleet_notif = ctx
            .drain_notifications_for_char(PILOT_2)
            .into_iter()
            .find(|n| n.event == "OnFleetEscalatingPathMessage")
            .expect("Fleetmate must receive OnFleetEscalatingPathMessage");
        assert_eq!(fleet_notif.id_type, "fleetid");
        assert_eq!(fleet_notif.args.len(), 2);
        assert_eq!(fleet_notif.args[0].as_i64(), Some(PILOT_1));

        // 4. Inspect GetMyEscalatingPathDetails row values & nested dungeon KeyVals
        let details = ctx
            .dispatch_rpc(
                &mut sess,
                "dungeonExplorationMgr",
                None,
                "GetMyEscalatingPathDetails",
                &[],
                &kw,
                None,
            )
            .await
            .expect("GetMyEscalatingPathDetails active");
        let PyRep::Rowset { rows: active_rows, .. } = &details else {
            panic!("Expected Rowset");
        };
        assert_eq!(active_rows.len(), 1);
        let row = &active_rows[0];
        let inst_id = row[0].as_i64().unwrap();
        assert_eq!(inst_id, notif_inst_id);
        let exp_did = row[2].as_i64().unwrap();
        let dest_sys = row[5].as_i64().unwrap();
        assert_eq!(row[6].as_i64(), Some(dest_sys), "solarSystemID aliases destinationSolarSystemID");
        assert_eq!(row[7].as_i64(), Some(1), "stepIndex == 1");
        assert!(row[8].as_i64().unwrap() >= 3, "totalSteps >= 3");
        assert!(row[9].as_str().is_some_and(|s| !s.is_empty()), "journalText non-empty");
        let created_ts = base_ts + 12;
        assert_eq!(row[10].as_i64(), Some(created_ts as i64));
        assert_eq!(row[11].as_i64(), Some((created_ts + 86_400) as i64));
        assert_eq!(
            row[12].as_i64(),
            Some(((created_ts as i64 + 86_400) + 11_644_473_600) * 10_000_000)
        );
        for kv_col in [14usize, 15, 16] {
            assert!(row[kv_col].dict_get("dungeonID").is_some(), "dungeon KeyVal must contain dungeonID");
            assert!(row[kv_col].dict_get("dungeonName").is_some(), "dungeon KeyVal must contain dungeonName");
        }
        assert_eq!(row[14].dict_get("instanceID").and_then(|v| v.as_i64()), Some(inst_id));
        assert_eq!(row[14].dict_get("solarSystemID").and_then(|v| v.as_i64()), Some(dest_sys));
        (inst_id, dest_sys, exp_did)
    };

    // 5. Reopen SQLite database in a fresh ServerContext (simulated server restart)
    let ctx2 = open_sqlite_ctx_at(&db_path, base_ts + 600);
    let mut sess2 = login_pilot_session(&ctx2, 9303, "demo", "demo-secret", PILOT_1).await;
    let persisted = ctx2
        .dispatch_rpc(
            &mut sess2,
            "dungeonExplorationMgr",
            None,
            "GetMyEscalatingPathDetails",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyEscalatingPathDetails after SQLite restart");
    let PyRep::Rowset { rows: persisted_rows, .. } = &persisted else {
        panic!("Expected Rowset after restart");
    };
    assert_eq!(persisted_rows.len(), 1, "Escalation must survive SQLite restart");
    assert_eq!(persisted_rows[0][0].as_i64(), Some(instance_id));
    assert_eq!(persisted_rows[0][2].as_i64(), Some(exp_dungeon_id));
    assert_eq!(persisted_rows[0][5].as_i64(), Some(dest_sys_id));

    // 6. Advancing expedition step updates stepIndex to 2 and emits OnEscalatingPathChange + OnEscalatingPathMessage
    let _ = ctx2.drain_notifications_for_char(PILOT_1);
    let adv = advance_expedition_step(&ctx2, PILOT_1, instance_id)
        .await
        .expect("advance_expedition_step")
        .expect("Updated step 2");
    assert_eq!(adv.step_index, 2);
    let adv_notifs = ctx2.drain_notifications_for_char(PILOT_1);
    assert!(adv_notifs.iter().any(|n| n.event == "OnEscalatingPathChange"));
    assert!(adv_notifs.iter().any(|n| n.event == "OnEscalatingPathMessage"));

    let step2_rep = ctx2
        .dispatch_rpc(
            &mut sess2,
            "dungeonExplorationMgr",
            None,
            "GetMyEscalatingPathDetails",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyEscalatingPathDetails step 2");
    let PyRep::Rowset { rows: step2_rows, .. } = &step2_rep else {
        panic!();
    };
    assert_eq!(step2_rows[0][7].as_i64(), Some(2));

    // 7. Advancing clock past 24h expiry prunes expired escalation & emits OnEscalatingPathChange
    let ctx3 = open_sqlite_ctx_at(&db_path, base_ts + 90_000);
    let mut sess3 = login_pilot_session(&ctx3, 9304, "demo", "demo-secret", PILOT_1).await;
    let _ = ctx3.drain_notifications_for_char(PILOT_1);
    let expired_rep = ctx3
        .dispatch_rpc(
            &mut sess3,
            "dungeonExplorationMgr",
            None,
            "GetMyEscalatingPathDetails",
            &[],
            &kw,
            None,
        )
        .await
        .expect("GetMyEscalatingPathDetails after expiry");
    let PyRep::Rowset { rows: exp_rows, .. } = &expired_rep else {
        panic!();
    };
    assert!(exp_rows.is_empty(), "Expired escalation must be pruned");
    let exp_notifs = ctx3.drain_notifications_for_char(PILOT_1);
    assert!(exp_notifs.iter().any(|n| n.event == "OnEscalatingPathChange"));

    let _ = std::fs::remove_dir_all(&tmp_dir);
}

// ============================================================================
// 2. Dungeon & Escalation Pocket Notifications (Entered, Room, Completed, Exiting)
// ============================================================================
#[tokio::test]
async fn test_dungeon_and_escalation_room_entry_completion_and_exit_notifications() {
    // parity: notify:OnDistributionDungeonEntered, notify:OnDungeonCompleted, notify:OnEnteringDungeonRoom, notify:OnEscalatingPathDungeonEntered, notify:OnExitingDungeon
    eve_testkit::attest(&[
        "notify:OnDistributionDungeonEntered",
        "notify:OnDungeonCompleted",
        "notify:OnEnteringDungeonRoom",
        "notify:OnEscalatingPathDungeonEntered",
        "notify:OnExitingDungeon",
    ]);

    let ctx = ServerContext::new();
    crate::common::pin_clock(&ctx);
    let kw = HashMap::new();
    let ship_id = 760_002_001i64;
    let mut sess = login_pilot_session(&ctx, 9310, "demo", "demo-secret", PILOT_1).await;
    put_pilot_in_space(&ctx, &mut sess, PILOT_1, ship_id, JITA_SYS, DVec3::ZERO).await;

    // ------------------------------------------------------------------------
    // Part A: Naturally Seeded Deadspace Dungeon Lifecycle Notifications
    // ------------------------------------------------------------------------
    ensure_system_pve_sites_seeded(&ctx, JITA_SYS).await;
    let (dsg_pos, dsg_inst_id, dsg_did) = with_pve_exploration_reg(&ctx, |reg| {
        let s = reg.sites_by_system[&JITA_SYS]
            .iter()
            .find(|s| s.site_id == "DSG-30000142")
            .unwrap();
        (s.position, s.expedition_instance_id.unwrap(), i64::from(s.dungeon_id))
    });
    let probes: Vec<PyRep> = (0..8)
        .map(|_| PyRep::List(vec![PyRep::Real(dsg_pos[0]), PyRep::Real(dsg_pos[1]), PyRep::Real(dsg_pos[2])]))
        .collect();
    ctx.dispatch_rpc(&mut sess, "scanMgr", None, "RequestScans", &[PyRep::Real(600_000_000_000.0), PyRep::Real(120.0), PyRep::List(probes)], &kw, None)
        .await
        .unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("scan".into()), PyRep::String("DSG-30000142".into())], &kw, None)
        .await
        .unwrap();
    let sys_handle = ctx.systems.get_or_spawn(JITA_SYS);
    for _ in 0..120 {
        let _ = sys_handle.step_manual(1.0).await;
        if sys_handle.get_ball_snapshot(ship_id).await.unwrap().unwrap().1.mode == MovementMode::Stop {
            break;
        }
    }
    ctx.dispatch_rpc(&mut sess, "beyonce", None, "UpdateStateRequest", &[], &kw, None).await.unwrap();
    let inst = eve_server::services::pve_dungeon_runtime::get_dungeon_instance_state(&ctx, dsg_inst_id).unwrap();
    let entry_gate_id = inst.dict_get("entryGateBallID").and_then(|v| v.as_i64()).unwrap();

    // 1. Activate entry acceleration gate -> OnEnteringDungeonRoom + OnDistributionDungeonEntered
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    let gate1 = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(entry_gate_id)], &kw, None)
        .await
        .expect("Activate entry gate");

    let mut n1: Vec<(String, Vec<PyRep>)> = sess.drain_notifications();
    n1.extend(ctx.drain_notifications_for_char(PILOT_1).into_iter().map(|n| (n.event, n.args)));
    let gate1_inst_id = gate1.dict_get("instanceID").and_then(|v| v.as_i64()).unwrap();
    let gate1_room_id = gate1.dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    let enter_room_1 = n1.iter().find(|(ev, _)| ev == "OnEnteringDungeonRoom").expect("OnEnteringDungeonRoom emitted");
    assert_eq!(enter_room_1.1.len(), 5);
    assert_eq!(enter_room_1.1[0].as_i64(), Some(dsg_did));
    assert_eq!(enter_room_1.1[1].as_i64(), Some(gate1_room_id));
    assert_eq!(enter_room_1.1[3].as_i64(), Some(gate1_inst_id));
    let dist_entered = n1.iter().find(|(ev, _)| ev == "OnDistributionDungeonEntered").expect("OnDistributionDungeonEntered emitted");
    assert_eq!(dist_entered.1.len(), 1);
    assert!(!n1.iter().any(|(ev, _)| ev == "OnEscalatingPathDungeonEntered"));

    // 2. Clear Room 1 NPCs -> unlock Room 1 gate -> enter Room 2 -> OnEnteringDungeonRoom(room 2)
    let PyRep::List(r1_npcs) = gate1.dict_get("npcBallIDs").unwrap() else { panic!() };
    for nid in r1_npcs {
        let _ = sys_handle
            .apply_damage_with_attacker(
                nid.as_i64().unwrap(),
                DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 },
                Some(PILOT_1),
            )
            .await;
    }
    let r1_gate_id = gate1.dict_get("nextGateBallID").and_then(|v| v.as_i64()).unwrap();
    let beyonce_oid = ctx.bind_object(&sess, "beyonce", i64::from(JITA_SYS)).unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdSetSpeedFraction", &[PyRep::Real(1.0)], &kw, None).await.unwrap();
    ctx.dispatch_rpc(&mut sess, "beyonce", Some(beyonce_oid), "CmdFollowBall", &[PyRep::Integer(r1_gate_id), PyRep::Real(500.0)], &kw, None).await.unwrap();
    for _ in 0..60 {
        let _ = sys_handle.step_manual(1.0).await;
    }
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(PILOT_1);

    let gate2 = ctx
        .dispatch_rpc(&mut sess, "keeper", None, "ActivateAccelerationGate", &[PyRep::Integer(r1_gate_id)], &kw, None)
        .await
        .expect("Activate Room 1 -> Room 2 gate");
    let mut n2 = sess.drain_notifications();
    n2.extend(ctx.drain_notifications_for_char(PILOT_1).into_iter().map(|n| (n.event, n.args)));
    let gate2_inst_id = gate2.dict_get("instanceID").and_then(|v| v.as_i64()).unwrap();
    let gate2_room_id = gate2.dict_get("roomID").and_then(|v| v.as_i64()).unwrap();
    let enter_room_2 = n2.iter().find(|(ev, _)| ev == "OnEnteringDungeonRoom").expect("OnEnteringDungeonRoom Room 2");
    assert_eq!(enter_room_2.1[1].as_i64(), Some(gate2_room_id));
    assert_eq!(enter_room_2.1[3].as_i64(), Some(gate2_inst_id));

    // 3. Warping out of the dungeon pocket to Jita 4-4 emits OnExitingDungeon(dungeonID) at warp start
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    ctx.dispatch_rpc(&mut sess, "beyonce", None, "CmdWarpToStuff", &[PyRep::String("item".into()), PyRep::Integer(JITA_4_4_STATION)], &kw, None)
        .await
        .expect("Warp out of dungeon to station");
    let mut n_exit = sess.drain_notifications();
    n_exit.extend(ctx.drain_notifications_for_char(PILOT_1).into_iter().map(|n| (n.event, n.args)));
    let exit_notif = n_exit.iter().find(|(ev, _)| ev == "OnExitingDungeon").expect("OnExitingDungeon emitted");
    assert_eq!(exit_notif.1, vec![PyRep::Integer(dsg_did)]);

    // 4. Killing Room 2 Overseer fires CompleteDungeon trigger -> emits OnDungeonCompleted(dungeonID)
    let PyRep::List(r2_npcs) = gate2.dict_get("npcBallIDs").unwrap() else { panic!() };
    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    for nid in r2_npcs {
        let _ = sys_handle
            .apply_damage_with_attacker(
                nid.as_i64().unwrap(),
                DamageVector { em: 1.0e7, thermal: 1.0e7, kinetic: 1.0e7, explosive: 1.0e7 },
                Some(PILOT_1),
            )
            .await;
    }
    let mut done_notifs = sess.drain_notifications();
    done_notifs.extend(ctx.drain_notifications_for_char(PILOT_1).into_iter().map(|n| (n.event, n.args)));
    let done = done_notifs.iter().find(|(ev, _)| ev == "OnDungeonCompleted").expect("OnDungeonCompleted emitted");
    assert_eq!(done.1, vec![PyRep::Integer(dsg_did)]);

    // ------------------------------------------------------------------------
    // Part B: Escalation Warp-Arrival Entry (NOT emitted mid-warp, emitted on landing!)
    // ------------------------------------------------------------------------
    let esc = create_escalation_expedition(
        &ctx,
        PILOT_1,
        JITA_SYS,
        1420,
        "Guristas Hideaway",
        [80_000_000.0, 0.0, 60_000_000.0],
    )
    .await
    .expect("create_escalation_expedition");
    let dest_sys = esc.destination_solar_system_id;
    let ship_id_dest = 760_003_001i64;
    put_pilot_in_space(&ctx, &mut sess, PILOT_1, ship_id_dest, dest_sys, DVec3::ZERO).await;

    let _ = sess.drain_notifications();
    let _ = ctx.drain_notifications_for_char(PILOT_1);
    ctx.dispatch_rpc(
        &mut sess,
        "beyonce",
        None,
        "CmdWarpToStuff",
        &[PyRep::String("epinstance".into()), PyRep::Integer(esc.instance_id)],
        &kw,
        None,
    )
    .await
    .expect("Warp to escalation epinstance");

    // Immediately after CmdWarpToStuff (still in warp at origin), entry notifications MUST NOT have fired yet!
    let mut mid_warp = sess.drain_notifications();
    mid_warp.extend(ctx.drain_notifications_for_char(PILOT_1).into_iter().map(|n| (n.event, n.args)));
    assert!(
        !mid_warp.iter().any(|(ev, _)| ev == "OnEnteringDungeonRoom" || ev == "OnEscalatingPathDungeonEntered"),
        "Dungeon entry notifications must NOT fire at warp start before warp landing"
    );

    // Step destiny ticks until warp lands, then dispatch UpdateStateRequest -> emits OnEnteringDungeonRoom + OnEscalatingPathDungeonEntered
    let dest_handle = ctx.systems.get_or_spawn(dest_sys);
    for _ in 0..120 {
        let _ = dest_handle.step_manual(1.0).await;
        if dest_handle.get_ball_snapshot(ship_id_dest).await.unwrap().unwrap().1.mode == MovementMode::Stop {
            break;
        }
    }
    ctx.dispatch_rpc(&mut sess, "beyonce", None, "UpdateStateRequest", &[], &kw, None)
        .await
        .expect("UpdateStateRequest on escalation landing");

    let mut n_esc = sess.drain_notifications();
    n_esc.extend(ctx.drain_notifications_for_char(PILOT_1).into_iter().map(|n| (n.event, n.args)));
    let enter_esc_room = n_esc
        .iter()
        .find(|(ev, _)| ev == "OnEnteringDungeonRoom")
        .expect("Landing in escalation pocket must emit OnEnteringDungeonRoom");
    assert_eq!(enter_esc_room.1[3].as_i64(), Some(esc.instance_id));
    let esc_entered = n_esc
        .iter()
        .find(|(ev, _)| ev == "OnEscalatingPathDungeonEntered")
        .expect("Landing in escalation pocket must emit OnEscalatingPathDungeonEntered");
    assert_eq!(esc_entered.1.len(), 1);
    assert!(
        !n_esc.iter().any(|(ev, _)| ev == "OnDistributionDungeonEntered"),
        "Escalation pocket must emit OnEscalatingPathDungeonEntered instead of OnDistributionDungeonEntered"
    );
}
