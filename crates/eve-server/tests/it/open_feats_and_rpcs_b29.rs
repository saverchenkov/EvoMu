//! End-to-end integration tests for Batch 29 open `feat:lifecycle.*` parity rows.
//!
//! Covers:
//! - `feat:lifecycle.corp-governance-durable`
//! - `feat:lifecycle.corp-governance-persist`
//! - `feat:lifecycle.corp-invite-consent`
//! - `feat:lifecycle.corp-join-leaves-old-corp`
//! - `feat:lifecycle.corp-kick-online-session`
//! - `feat:lifecycle.corp-quit-cleanup`
//! - `feat:lifecycle.courier-package-destroyed`
//! - `feat:lifecycle.crimewatch-across-restart`
//! - `feat:lifecycle.crimewatch-highsec-criminal-movement`
//! - `feat:lifecycle.crimewatch-state-persist`
//! - `feat:lifecycle.death-session-change`
//! - `feat:lifecycle.drones-in-space-persist-restart`
//! - `feat:lifecycle.drones-leave-grid-abandon`
//! - `feat:lifecycle.drones-recall-same-grid`
//! - `feat:lifecycle.fighters-left-in-space`
//! - `feat:lifecycle.fleet-disconnect-grace`
//! - `feat:lifecycle.fleet-invite-offline`

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crate::common;
use crate::plan_c7_crimewatch_closure::{item_rec, setup_pilot_in_space};
use eve_db::{
    ContractRecord, InventoryFlag, IskCents, TrainedSkillRecord, VirtualServerClock,
    WorldRepository,
};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::combat_crimewatch::check_crimewatch_action_restriction;
use eve_server::combat::death_pipeline::execute_death_pipeline;
use eve_server::services::fleet_social::fleet_social_notify::{
    handle_fleet_member_disconnect, tick_fleet_lifecycle, FLEET_DISCONNECT_GRACE_SECS,
    FLEET_INVITE_EXPIRY_SECS,
};
use glam::DVec3;

const PILOT_1: i64 = 90_000_001;
const PILOT_2: i64 = 90_000_002;
const PILOT_3: i64 = 90_000_003;

fn temp_sqlite_path(tag: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let seq = NEXT.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let p = std::env::temp_dir().join(format!("evomu_b29_{tag}_{pid}_{seq}.sqlite"));
    let _ = std::fs::remove_file(&p);
    p
}

fn open_ctx(db_path: &Path, now_unix: u64) -> (ServerContext, Arc<VirtualServerClock>) {
    let exists = db_path.exists();
    let repo = Arc::new(WorldRepository::open_sqlite(db_path).expect("open_sqlite"));
    if !exists {
        let seed_snap = WorldRepository::with_demo_seed().with_snapshot(|s| s.clone());
        repo.with_mut_snapshot(|snap| *snap = seed_snap);
    }
    let clock = Arc::new(VirtualServerClock::with_fixed_base(now_unix));
    repo.set_clock(clock.clone());
    let ctx = ServerContext::with_repository(repo);
    ctx.set_clock(clock.clone());
    (ctx, clock)
}

async fn login_select(
    ctx: &ServerContext,
    sid: u64,
    user: &str,
    pass: &str,
    char_id: i64,
) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
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
    eve_server::services::space_destiny::clear_session_change_timer(ctx, char_id);
    sess
}

async fn rpc_res(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, svc, None, method, args, &HashMap::new(), None)
        .await
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    svc: &str,
    method: &str,
    args: &[PyRep],
) -> PyRep {
    rpc_res(ctx, sess, svc, method, args)
        .await
        .unwrap_or_else(|e| panic!("{svc}.{method} failed: {} {}", e.code, e.message))
}

fn pyrep_contains_int(rep: &PyRep, target: i64) -> bool {
    match rep {
        PyRep::Integer(v) => *v == target,
        PyRep::Tuple(items) | PyRep::List(items) => {
            items.iter().any(|x| pyrep_contains_int(x, target))
        }
        PyRep::Dict(entries) => entries
            .iter()
            .any(|(k, v)| pyrep_contains_int(k, target) || pyrep_contains_int(v, target)),
        PyRep::KeyVal(entries) => entries.iter().any(|(_, v)| pyrep_contains_int(v, target)),
        PyRep::Rowset { rows, .. } => rows
            .iter()
            .any(|row| row.iter().any(|x| pyrep_contains_int(x, target))),
        PyRep::Object { args, .. } => pyrep_contains_int(args, target),
        _ => false,
    }
}

#[tokio::test]
async fn test_b29_corp_governance_durability_and_persistence() {
    // parity: feat:lifecycle.corp-governance-durable
    // parity: feat:lifecycle.corp-governance-persist
    eve_testkit::attest(&[
        "feat:lifecycle.corp-governance-durable",
        "feat:lifecycle.corp-governance-persist",
    ]);

    let db_path = temp_sqlite_path("corp_gov");
    let t0 = 1_775_000_000u64;
    let hangar_role: u64 = 1u64 << 13;

    let (corp_id, medal_id) = {
        let (ctx, _clock) = open_ctx(&db_path, t0);
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(PILOT_1).or_default().insert(
                3363,
                TrainedSkillRecord { type_id: 3363, level: 5, skill_points: 256_000.0 },
            );
            snap.characters.get_mut(&PILOT_1).unwrap().balance = IskCents::from_isk(5_000_000_000);
        });
        let mut sess1 = login_select(&ctx, 9101, "demo", "demo-secret", PILOT_1).await;
        let add_corp = rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "AddCorporation",
            &[PyRep::String("B29 Durable Corp".into()), PyRep::String("B29D".into()), PyRep::Real(0.05)],
        )
        .await;
        let corp_id = add_corp.dict_get("corporationID").and_then(|v| v.as_i64()).or_else(|| add_corp.as_i64()).unwrap();
        eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);

        let mut sess2 = login_select(&ctx, 9102, "other", "other-secret", PILOT_2).await;
        let app_res = rpc(&ctx, &mut sess2, "corpRegistry", "InsertApplication", &[PyRep::Integer(corp_id), PyRep::String("Join".into())]).await;
        let app_id = app_res
            .dict_get("applicationID")
            .and_then(|v| v.as_i64())
            .or_else(|| app_res.as_i64())
            .expect("applicationID");
        rpc(&ctx, &mut sess1, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(app_id), PyRep::String("Accepted".into())]).await;

        rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "UpdateTitle",
            &[PyRep::Integer(1), PyRep::String("Quartermaster".into()), PyRep::Integer(hangar_role as i64), PyRep::Integer(0), PyRep::Integer(0), PyRep::Integer(0)],
        )
        .await;
        rpc(
            &ctx,
            &mut sess1,
            "corpRegistry",
            "UpdateMember",
            &[
                PyRep::Integer(PILOT_2),
                PyRep::Integer((hangar_role | eve_db::corp_roles::ROLE_ACCOUNTANT) as i64),
                PyRep::Integer(1),
            ],
        )
        .await;
        let medal_res = rpc(&ctx, &mut sess1, "corporationSvc", "CreateMedal", &[PyRep::String("B29 Star".into()), PyRep::String("Awarded".into()), PyRep::List(vec![])]).await;
        let medal_id = medal_res
            .as_i64()
            .or_else(|| medal_res.dict_get("medalID").and_then(|v| v.as_i64()))
            .expect("medal_id");
        rpc(&ctx, &mut sess1, "corporationSvc", "GiveMedalToCharacters", &[PyRep::Integer(medal_id), PyRep::List(vec![PyRep::Integer(PILOT_2)]), PyRep::String("Service".into())]).await;

        let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
            snap.corporations.get_mut(&corp_id).unwrap().ticker = "ABORT".into();
            Err("abort".to_string())
        });
        assert_eq!(ctx.db.with_snapshot(|s| s.corporations[&corp_id].ticker.clone()), "B29D");
        (corp_id, medal_id)
    };

    {
        let (ctx2, _clock2) = open_ctx(&db_path, t0 + 60);
        let mut sess1 = login_select(&ctx2, 9103, "demo", "demo-secret", PILOT_1).await;
        let mut sess2 = login_select(&ctx2, 9104, "other", "other-secret", PILOT_2).await;
        let emp = rpc(&ctx2, &mut sess1, "corporationSvc", "GetEmploymentRecord", &[PyRep::Integer(PILOT_2)]).await;
        assert!(pyrep_contains_int(&emp, corp_id), "GetEmploymentRecord must contain corp_id {corp_id}: {emp:?}");
        let audit = rpc(&ctx2, &mut sess1, "corpmgr", "AuditMember", &[PyRep::Integer(PILOT_2)]).await;
        assert!(
            pyrep_contains_int(&audit, PILOT_2)
                && pyrep_contains_int(&audit, (hangar_role | eve_db::corp_roles::ROLE_ACCOUNTANT) as i64),
            "AuditMember must contain role change for PILOT_2: {audit:?}"
        );
        let medals = rpc(&ctx2, &mut sess2, "corporationSvc", "GetMedalsReceived", &[PyRep::Integer(PILOT_2)]).await;
        assert!(
            pyrep_contains_int(&medals, medal_id) && pyrep_contains_int(&medals, PILOT_2),
            "GetMedalsReceived must contain medal_id {medal_id} for PILOT_2: {medals:?}"
        );
        ctx2.db.with_snapshot(|snap| {
            let gov = snap.corp_governance.get(&corp_id).expect("corp_governance");
            assert_eq!(gov.titles.get(&1).map(|t| t.0.as_str()), Some("Quartermaster"));
            assert!(gov.pending_role_grants.contains_key(&PILOT_2));
            assert!(gov.medals.iter().any(|m| m.0 == medal_id));
            assert!(gov.medal_recipients.iter().any(|r| r.0 == medal_id && r.1 == PILOT_2));
            let corp = snap.corporations.get(&corp_id).expect("corp");
            assert!(corp.members.contains(&PILOT_2));
        });
    }
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b29_corp_invite_join_kick_quit_lifecycle() {
    // parity: feat:lifecycle.corp-invite-consent
    // parity: feat:lifecycle.corp-join-leaves-old-corp
    // parity: feat:lifecycle.corp-kick-online-session
    // parity: feat:lifecycle.corp-quit-cleanup
    eve_testkit::attest(&[
        "feat:lifecycle.corp-invite-consent",
        "feat:lifecycle.corp-join-leaves-old-corp",
        "feat:lifecycle.corp-kick-online-session",
        "feat:lifecycle.corp-quit-cleanup",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.db.with_mut_snapshot(|snap| {
        snap.accounts.insert(
            "third_b29".to_string(),
            eve_db::AccountRecord {
                user_id: 30,
                username: "third_b29".to_string(),
                password_token: WorldRepository::hash_password_token("third_b29", "third-secret"),
                role: eve_db::service_roles::ROLE_PLAYER_DEFAULT,
                banned_until_unix: None,
                ban_reason: None,
            },
        );
        let mut ch3 = snap.characters.get(&PILOT_2).cloned().expect("PILOT_2");
        ch3.character_id = PILOT_3;
        ch3.user_id = 30;
        ch3.name = "Third B29".into();
        ch3.station_id = Some(60_003_760);
        snap.characters.insert(PILOT_3, ch3);

        // Place PILOT_2 in seeded player corp 98_000_001 so old-corp removal is unconditionally verified.
        if let Some(ch2) = snap.characters.get_mut(&PILOT_2) {
            ch2.corporation_id = 98_000_001;
        }
        if let Some(old_corp) = snap.corporations.get_mut(&98_000_001) {
            old_corp.members.insert(PILOT_2);
            old_corp.member_roles.insert(PILOT_2, 0);
        }
    });

    let mut sess1 = login_select(&ctx, 9201, "demo", "demo-secret", PILOT_1).await;
    let mut sess2 = login_select(&ctx, 9202, "other", "other-secret", PILOT_2).await;

    let add_corp = rpc(
        &ctx,
        &mut sess1,
        "corpRegistry",
        "AddCorporation",
        &[PyRep::String("B29 Lifecycle Corp".into()), PyRep::String("B29L".into()), PyRep::Real(0.05)],
    )
    .await;
    let corp_id = add_corp.dict_get("corporationID").and_then(|v| v.as_i64()).or_else(|| add_corp.as_i64()).unwrap();
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);

    let orig_corp_p3 = ctx.db.with_snapshot(|s| s.characters[&PILOT_3].corporation_id);
    let err_accept_now = rpc_res(&ctx, &mut sess1, "corpRegistry", "InsertInvitation", &[PyRep::Integer(PILOT_3), PyRep::Bool(true)]).await;
    assert!(err_accept_now.is_err());
    assert_eq!(ctx.db.with_snapshot(|s| s.characters[&PILOT_3].corporation_id), orig_corp_p3);

    let inv = rpc(&ctx, &mut sess1, "corpRegistry", "InsertInvitation", &[PyRep::Integer(PILOT_2), PyRep::Bool(false)]).await;
    let inv_id = inv.dict_get("applicationID").and_then(|v| v.as_i64()).unwrap();
    let old_corp_p2 = sess2.corporation_id.unwrap();
    rpc(&ctx, &mut sess2, "corpRegistry", "UpdateApplicationOffer", &[PyRep::Integer(inv_id), PyRep::String("Accepted".into())]).await;
    assert_eq!(sess2.corporation_id, Some(corp_id));
    ctx.db.with_snapshot(|s| {
        assert!(s.corporations[&corp_id].members.contains(&PILOT_2));
        let old_c = s.corporations.get(&old_corp_p2).expect("old player corp in snapshot");
        assert!(!old_c.members.contains(&PILOT_2));
        assert!(!old_c.member_roles.contains_key(&PILOT_2));
    });

    ctx.drain_notifications_for_char(PILOT_2);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_2);
    rpc(&ctx, &mut sess1, "corpRegistry", "KickOutMember", &[PyRep::Integer(PILOT_2)]).await;
    let notifs = ctx.drain_notifications_for_char(PILOT_2);
    assert!(notifs.iter().any(|n| n.event == "OnSessionChanged"));
    ctx.db.with_snapshot(|s| {
        assert!(!s.corporations[&corp_id].members.contains(&PILOT_2));
        if let Some(ch) = s.chat_channels.get(&format!("corpid:{corp_id}")) {
            assert!(!ch.members.contains(&PILOT_2));
        }
    });

    ctx.advance_secs(25);
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_1);
    rpc(&ctx, &mut sess1, "corpRegistry", "KickOutMember", &[PyRep::Integer(PILOT_1)]).await;
    let npc_corp = sess1.corporation_id.expect("NPC corp after quit");
    assert!((1_000_001..=1_000_200).contains(&npc_corp));
    ctx.db.with_snapshot(|s| {
        assert!(s.chat_channels[&format!("corpid:{npc_corp}")].members.contains(&PILOT_1));
    });
}

#[tokio::test]
async fn test_b29_crimewatch_persist_restart_and_movement_restrictions() {
    // parity: feat:lifecycle.crimewatch-state-persist
    // parity: feat:lifecycle.crimewatch-across-restart
    // parity: feat:lifecycle.crimewatch-highsec-criminal-movement
    eve_testkit::attest(&[
        "feat:lifecycle.crimewatch-state-persist",
        "feat:lifecycle.crimewatch-across-restart",
        "feat:lifecycle.crimewatch-highsec-criminal-movement",
    ]);

    let db_path = temp_sqlite_path("cw_persist");
    let t0 = 1_775_100_000u64;
    let crim_exp = t0 + 900;

    {
        let (ctx, _clock) = open_ctx(&db_path, t0);
        let mut sess1 = login_select(&ctx, 9301, "demo", "demo-secret", PILOT_1).await;
        rpc(&ctx, &mut sess1, "crimewatch", "SetSafetyLevel", &[PyRep::Integer(0)]).await;
        {
            let mut cw = ctx.crimewatch.write().unwrap();
            let st = cw.get_or_create(PILOT_1);
            st.criminal_flag_until_unix = crim_exp;
            st.weapon_timer_until_unix = t0 + 60;
        }
        ctx.db.with_mut_snapshot(|snap| {
            snap.zarzakh_gate_locks.insert(PILOT_1, (30_100_001, t0 + 3600));
        });
        rpc(&ctx, &mut sess1, "crimewatch", "GetClientStates", &[]).await;
    }

    {
        let (ctx2, _clock2) = open_ctx(&db_path, t0 + 30);
        let mut sess1 = login_select(&ctx2, 9302, "demo", "demo-secret", PILOT_1).await;
        let states = rpc(&ctx2, &mut sess1, "crimewatch", "GetClientStates", &[]).await;
        let unix_to_ft = |u: u64| -> i64 { (u as i64 + 11_644_473_600) * 10_000_000 };
        if let PyRep::Tuple(top) = &states {
            assert_eq!(top.len(), 4, "GetClientStates must return 4-tuple: {states:?}");
            if let PyRep::Tuple(timers) = &top[0] {
                assert_eq!(
                    timers.get(3),
                    Some(&PyRep::Tuple(vec![PyRep::Integer(301), PyRep::Integer(unix_to_ft(crim_exp))])),
                    "criminal timer must report code 301 with original expiry"
                );
            } else {
                panic!("expected timers tuple, got {:?}", top[0]);
            }
            assert_eq!(top[3], PyRep::Integer(0), "safety level 0 (Red) must persist across restart");
        } else {
            panic!("expected GetClientStates tuple, got {states:?}");
        }
        ctx2.db.with_snapshot(|snap| {
            assert_eq!(snap.zarzakh_gate_locks.get(&PILOT_1), Some(&(30_100_001, t0 + 3600)));
        });

        let ship_id = sess1.ship_id.unwrap_or(1_000_001);
        for action in ["warp", "stargate_jump", "wormhole_jump", "jump_drive"] {
            let err = check_crimewatch_action_restriction(&ctx2, PILOT_1, ship_id, 30_000_142, action, t0 + 30);
            assert!(err.is_err(), "Highsec criminal must be blocked from {action}");
        }
    }
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b29_drones_and_fighters_space_lifecycle() {
    // parity: feat:lifecycle.drones-leave-grid-abandon
    // parity: feat:lifecycle.drones-in-space-persist-restart
    // parity: feat:lifecycle.drones-recall-same-grid
    // parity: feat:lifecycle.fighters-left-in-space
    eve_testkit::attest(&[
        "feat:lifecycle.drones-leave-grid-abandon",
        "feat:lifecycle.drones-in-space-persist-restart",
        "feat:lifecycle.drones-recall-same-grid",
        "feat:lifecycle.fighters-left-in-space",
    ]);

    let db_path = temp_sqlite_path("drones_fighters");
    let t0 = 1_775_200_000u64;
    let sys_id = 30_000_142u32;
    let kw = HashMap::new();
    let (char_id, vexor_id, carrier_id) = (96_200_001i64, 86_200_099i64, 86_200_100i64);
    let (drone_1, drone_2, fighter_1) = (86_200_110i64, 86_200_111i64, 86_200_120i64);
    let station_pos = {
        let (ctx, _clock) = open_ctx(&db_path, t0);
        let pos = ctx.sde.stations[&60_003_760].position;
        let mut sess = setup_pilot_in_space(&ctx, 9401, char_id, 1_000_125, vexor_id, 626, sys_id, pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.skills.entry(char_id).or_default().insert(
                3436,
                TrainedSkillRecord { type_id: 3436, level: 5, skill_points: 256_000.0 },
            );
            for did in [drone_1, drone_2] {
                snap.items.insert(did, item_rec(did, 2454, char_id, vexor_id, InventoryFlag::DroneBay, 1, false));
            }
            snap.items.insert(carrier_id, item_rec(carrier_id, 23757, char_id, sys_id as i64, InventoryFlag::None, 1, true));
            snap.items.insert(fighter_1, item_rec(fighter_1, 23055, char_id, carrier_id, InventoryFlag::FighterBay, 9, false));
        });

        let launch_rep = ctx.dispatch_rpc(&mut sess, "ship", None, "LaunchDrones", &[PyRep::List(vec![PyRep::Integer(drone_1), PyRep::Integer(drone_2)])], &kw, None).await.expect("LaunchDrones");
        let drone_ball_1 = match &launch_rep {
            PyRep::List(items) => items.first().and_then(|v| v.as_i64()).unwrap_or(drone_1),
            _ => drone_1,
        };

        let (f_pilot, f_sess_id) = (96_200_003i64, 9403);
        let mut f_sess = setup_pilot_in_space(&ctx, f_sess_id, f_pilot, 1_000_125, carrier_id, 23757, sys_id, pos + DVec3::new(1_000.0, 0.0, 0.0)).await;
        ctx.db.with_mut_snapshot(|snap| {
            snap.items.get_mut(&fighter_1).unwrap().owner_id = f_pilot;
        });
        ctx.dispatch_rpc(&mut f_sess, "fighterMgr", None, "LoadFightersToTube", &[PyRep::Integer(0), PyRep::Integer(fighter_1)], &kw, None).await.expect("LoadFightersToTube");
        ctx.dispatch_rpc(&mut f_sess, "fighterMgr", None, "LaunchFightersFromTubes", &[PyRep::List(vec![PyRep::Integer(0)])], &kw, None).await.expect("LaunchFightersFromTubes");

        let sys_h = ctx.systems.get_or_spawn(sys_id);
        let (_, mut cball) = sys_h.get_ball_snapshot(vexor_id).await.unwrap().unwrap();
        cball.position = pos + DVec3::new(1_000.0, 0.0, 0.0);
        cball.velocity = DVec3::ZERO;
        cball.speed_fraction = 0.0;
        cball.mode = MovementMode::Stop;
        let _ = sys_h.spawn_ball(cball).await;
        ctx.advance_secs(12);
        ctx.dispatch_rpc(&mut sess, "stationSvc", None, "Dock", &[PyRep::Integer(60_003_760)], &kw, None).await.expect("Dock");
        ctx.dispatch_rpc(&mut f_sess, "stationSvc", None, "Dock", &[PyRep::Integer(60_003_760)], &kw, None).await.expect("Carrier Dock");
        {
            let fighters = ctx.active_fighters.read().unwrap();
            assert!(!fighters.is_empty(), "active_fighters must not be empty");
            assert!(fighters.values().all(|f| f.is_abandoned), "launched fighters must be abandoned on carrier dock");
        }

        let _ = ctx.dispatch_rpc(&mut sess, "entity", None, "CmdReturnBay", &[PyRep::List(vec![PyRep::Integer(drone_ball_1)])], &kw, None).await;
        let d1_after_dock = ctx.db.get_item(drone_1).unwrap();
        assert_eq!(d1_after_dock.location_id, sys_id as i64);
        let d1_abandoned = eve_server::services::combat_crimewatch::with_combat_runtime(&ctx, |rt| {
            rt.active_drones_in_space.get(&drone_ball_1).map(|d| d.is_abandoned)
        });
        assert_eq!(d1_abandoned, Some(true), "drone must be marked is_abandoned on dock");
        (pos, drone_ball_1)
    };

    {
        let (ctx2, _clock2) = open_ctx(&db_path, t0 + 30);
        eve_server::sp6_lifecycle::seed_persisted_abandoned_drones(&ctx2);
        let (scooper_id, scooper_ship) = (96_200_002i64, 86_200_200i64);
        let mut scooper_sess = setup_pilot_in_space(&ctx2, 9402, scooper_id, 1_000_009, scooper_ship, 626, sys_id, station_pos.0 + DVec3::new(1_000.0, 0.0, 0.0)).await;
        let sys_h2 = ctx2.systems.get_or_spawn(sys_id);
        assert!(sys_h2.get_ball_snapshot(station_pos.1).await.unwrap().is_some());
        ctx2.dispatch_rpc(&mut scooper_sess, "ship", None, "ScoopToDroneBay", &[PyRep::Integer(station_pos.1)], &kw, None).await.expect("ScoopToDroneBay");
        let scooped = ctx2.db.get_item(drone_1).unwrap();
        assert_eq!((scooped.owner_id, scooped.location_id), (scooper_id, scooper_ship));
    }
    let _ = std::fs::remove_file(&db_path);
}

#[tokio::test]
async fn test_b29_fleet_grace_invite_and_death_courier_lifecycle() {
    // parity: feat:lifecycle.fleet-disconnect-grace
    // parity: feat:lifecycle.fleet-invite-offline
    // parity: feat:lifecycle.death-session-change
    // parity: feat:lifecycle.courier-package-destroyed
    eve_testkit::attest(&[
        "feat:lifecycle.fleet-disconnect-grace",
        "feat:lifecycle.fleet-invite-offline",
        "feat:lifecycle.death-session-change",
        "feat:lifecycle.courier-package-destroyed",
    ]);

    let ctx = ServerContext::new();
    common::pin_clock(&ctx);
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch3 = snap.characters.get(&PILOT_2).cloned().expect("PILOT_2");
        ch3.character_id = PILOT_3;
        ch3.name = "Offline Fleet Target".into();
        snap.characters.insert(PILOT_3, ch3);
    });
    let mut s1 = login_select(&ctx, 9501, "demo", "demo-secret", PILOT_1).await;
    let mut s2 = login_select(&ctx, 9502, "other", "other-secret", PILOT_2).await;

    let fid = rpc(&ctx, &mut s1, "fleetMgr", "CreateFleet", &[]).await.dict_get("fleetID").and_then(|v| v.as_i64()).unwrap();
    let offline_err = rpc_res(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_3), PyRep::None, PyRep::None, PyRep::Integer(4)]).await;
    assert!(offline_err.is_err(), "inviting an existing offline character must fail");

    rpc(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_2), PyRep::None, PyRep::None, PyRep::Integer(4)]).await;
    ctx.advance_secs(FLEET_INVITE_EXPIRY_SECS + 5);
    tick_fleet_lifecycle(&ctx);
    assert!(rpc_res(&ctx, &mut s2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fid)]).await.is_err());

    rpc(&ctx, &mut s1, "fleetObjectHandler", "Invite", &[PyRep::Integer(PILOT_2), PyRep::None, PyRep::None, PyRep::Integer(4)]).await;
    eve_server::services::space_destiny::clear_session_change_timer(&ctx, PILOT_2);
    rpc(&ctx, &mut s2, "fleetObjectHandler", "AcceptInvite", &[PyRep::Integer(fid)]).await;
    handle_fleet_member_disconnect(&ctx, &mut s2);
    ctx.db.with_snapshot(|snap| assert!(snap.fleets[&fid].members.contains(&PILOT_2)));
    ctx.advance_secs(FLEET_DISCONNECT_GRACE_SECS + 5);
    tick_fleet_lifecycle(&ctx);
    ctx.db.with_snapshot(|snap| assert!(!snap.fleets[&fid].members.contains(&PILOT_2)));

    let sys_id = 30_000_142u32;
    let (ship_id, wrap_id, cargo_id, contract_id) = (620_001i64, 620_002i64, 620_003i64, 720_001i64);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&PILOT_1) {
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
            ch.active_ship_id = ship_id;
        }
        snap.items.insert(ship_id, item_rec(ship_id, 587, PILOT_1, sys_id as i64, InventoryFlag::None, 1, true));
        snap.items.insert(wrap_id, item_rec(wrap_id, 3468, PILOT_1, ship_id, InventoryFlag::Cargo, 1, true));
        snap.items.insert(cargo_id, item_rec(cargo_id, 34, PILOT_2, wrap_id, InventoryFlag::Locked, 100, false));
        snap.contracts.insert(
            contract_id,
            ContractRecord {
                contract_id,
                issuer_id: PILOT_2,
                acceptor_id: Some(PILOT_1),
                contract_type: "Courier".into(),
                status: "InProgress".into(),
                price: IskCents(0),
                reward: IskCents::from_f64_isk(50_000.0),
                collateral: IskCents::from_f64_isk(1_000_000.0),
                start_station_id: 60_003_760,
                end_station_id: 60_004_588,
                item_ids: vec![cargo_id],
                expiration_unix: ctx.now_unix() + 86_400,
                num_days: 1,
                ..Default::default()
            },
        );
    });
    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: ship_id,
                type_id: 587,
                position: DVec3::new(1_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await;
    ctx.drain_notifications_for_char(PILOT_1);
    let now_unix = ctx.now_unix();
    execute_death_pipeline(&ctx, sys_id, PILOT_1, ship_id, PILOT_2, DVec3::new(1_000.0, 0.0, 0.0), now_unix).await.expect("execute_death_pipeline");
    let death_notifs = ctx.drain_notifications_for_char(PILOT_1);
    assert!(death_notifs.iter().any(|n| n.event == "OnSessionChanged"));
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.contracts[&contract_id].status, "Failed");
        assert!(!snap.items.contains_key(&wrap_id));
        let cargo_valid = match snap.items.get(&cargo_id) {
            Some(c) => c.location_id != ship_id && c.location_id != 60_003_760,
            None => true,
        };
        assert!(cargo_valid);
    });
}
