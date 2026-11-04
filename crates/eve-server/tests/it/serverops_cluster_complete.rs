//! End-to-end integration tests for cluster server operations:
//! - Per-system / per-node Time Dilation (`0.10..=1.0`) scaling physics & module cycle progression
//!   and emitting `DoSimClockRebase((oldSimTime, newSimTime))`.
//! - Worker-node load balancing, overloaded-node migration, and fleet-fight dedicated reinforced nodes.
//! - Daily 11:00 UTC downtime countdown warnings (`OnClusterShutdownInitiated` at 60m, 15m, 5m),
//!   active session disconnection, `ClusterShuttingDown` login rejection, and cluster restart recovery.
//! - Daily world-state maintenance jobs (SDE asteroid & ice belt respawn, 24h market history roll-up,
//!   28d industry SCI recompute, decay across military/industrial/strategic sov indices, mission/ESS
//!   daily resets, and expired orders/contracts/auto-bills/raffles).

use crate::common;
use eve_db::ledger::MarketTransactionRecord;
use eve_db::snapshot_ext::MarketOrderMeta;
use eve_db::{
    ActiveMissionRecord, AsteroidRecord, IndustryJobRecord, IskCents, MarketOrderRecord,
    SystemDynamicState,
};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::services::serverops_cluster::{
    assign_or_reinforce_system_node, get_system_tidi_factor, get_system_worker_node,
    get_worker_node_load, rebalance_overloaded_worker_nodes, recompute_cluster_node_tidi,
    reinforce_solar_system_for_fleet_fight, run_daily_downtime_and_jobs,
    set_cluster_node_load_threshold, set_system_tidi_factor, step_downtime_countdown_warnings,
};
use eve_server::session_cluster_lifecycle::tick_background_cluster;
use eve_server::system_actor::{ActiveModuleCycle, AsteroidState};
use glam::DVec3;
use std::collections::{BTreeSet, HashMap};

const JITA_STATION_ID: i64 = 60_003_760;
const JITA_SYSTEM_ID: u32 = 30_000_142;
const PERIMETER_SYSTEM_ID: u32 = 30_000_144;
const AMARR_SYSTEM_ID: u32 = 30_002_187;
const THE_FORGE_REGION_ID: u32 = 10_000_002;
const P1: i64 = 90_000_001;
const P2: i64 = 90_000_002;

async fn setup_pilot(ctx: &ServerContext, sid: u64, user: &str, pass: &str, char_id: i64) -> SessionState {
    let mut sess = SessionState::new(sid);
    common::login_pilot(ctx, &mut sess, user, pass).await;
    rpc(ctx, &mut sess, "charUnboundMgr", "SelectCharacterID", &[PyRep::Integer(char_id)])
        .await
        .expect("SelectCharacterID");
    sess
}

async fn rpc(
    ctx: &ServerContext,
    sess: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(sess, service, None, method, args, &HashMap::new(), None).await
}

fn spawn_test_ship_ball(ball_id: i64) -> Ball {
    Ball {
        id: ball_id,
        type_id: 587,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 2.5,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    }
}

#[tokio::test]
async fn test_time_dilation_scales_physics_and_module_cycles_and_emits_clock_rebase() {
    // parity: feat:serverops.time-dilation, notify:DoSimClockRebase
    eve_testkit::attest(&["feat:serverops.time-dilation", "notify:DoSimClockRebase"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s_jita = setup_pilot(&ctx, 19_001, "demo", "demo-secret", P1).await;
    let mut s_amarr = setup_pilot(&ctx, 19_002, "other", "other-secret", P2).await;
    s_jita.station_id = None;
    s_jita.solar_system_id = Some(JITA_SYSTEM_ID);
    s_amarr.station_id = None;
    s_amarr.solar_system_id = Some(AMARR_SYSTEM_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.station_id = None;
            c1.solar_system_id = JITA_SYSTEM_ID;
        }
        if let Some(c2) = snap.characters.get_mut(&P2) {
            c2.station_id = None;
            c2.solar_system_id = AMARR_SYSTEM_ID;
        }
    });
    let _ = ctx.drain_notifications_for_char(P1);
    let _ = ctx.drain_notifications_for_char(P2);

    // 1. Setting Jita TiDi to 0.50 clamps/applies to live SystemActor and emits DoSimClockRebase((old, new)) ONLY in Jita
    let applied = set_system_tidi_factor(&ctx, JITA_SYSTEM_ID, 0.50);
    assert!((applied - 0.50).abs() < 1e-6);
    assert!((get_system_tidi_factor(&ctx, JITA_SYSTEM_ID) - 0.50).abs() < 1e-6);
    assert!((get_system_tidi_factor(&ctx, AMARR_SYSTEM_ID) - 1.0).abs() < 1e-6);

    let jita_notes = ctx.drain_notifications_for_char(P1);
    let rebase = jita_notes
        .iter()
        .find(|n| n.event == "DoSimClockRebase")
        .expect("DoSimClockRebase must be pushed to sessions in Jita");
    assert_eq!(rebase.args.len(), 1, "DoSimClockRebase takes 1 tuple argument ((oldSimTime, newSimTime))");
    let PyRep::Tuple(pair) = &rebase.args[0] else {
        panic!("expected 2-tuple in DoSimClockRebase, got {:?}", rebase.args);
    };
    assert_eq!(pair.len(), 2);
    let old_sim = pair[0].as_i64().expect("oldSimTime must be i64");
    let new_sim = pair[1].as_i64().expect("newSimTime must be i64");
    let auth_now_ft = ctx.now_blueos_filetime_ticks();
    assert!(old_sim < new_sim, "DoSimClockRebase must advance sim clock forward (old={old_sim}, new={new_sim})");
    assert!(
        (auth_now_ft - new_sim).abs() <= 10_000_000,
        "DoSimClockRebase newSimTime ({new_sim}) must match authoritative destiny sim clock ({auth_now_ft})"
    );
    assert!(
        ctx.drain_notifications_for_char(P2)
            .iter()
            .all(|n| n.event != "DoSimClockRebase"),
        "Pilots in unaffected Amarr must not receive Jita's DoSimClockRebase"
    );

    // Verify 10% floor clamp
    let floor = set_system_tidi_factor(&ctx, JITA_SYSTEM_ID, 0.01);
    assert!((floor - 0.10).abs() < 1e-6);
    let _ = set_system_tidi_factor(&ctx, JITA_SYSTEM_ID, 0.50);

    // 2. Spawn identical ships with identicalGoto orders and 4.0s non-repeating module cycles in Jita (0.50 TiDi) vs Amarr (1.0 normal)
    let jita_sys = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
    let amarr_sys = ctx.systems.get_or_spawn(AMARR_SYSTEM_ID);
    let jita_ship = 810_001_i64;
    let amarr_ship = 810_002_i64;
    let cs = CombatState::new();

    jita_sys.spawn_ball_with_combat(spawn_test_ship_ball(jita_ship), cs.clone()).await.unwrap();
    amarr_sys.spawn_ball_with_combat(spawn_test_ship_ball(amarr_ship), cs).await.unwrap();
    jita_sys.goto_point(jita_ship, DVec3::new(100_000.0, 0.0, 0.0)).await.unwrap();
    amarr_sys.goto_point(amarr_ship, DVec3::new(100_000.0, 0.0, 0.0)).await.unwrap();

    let mk_cycle = |ship_id: i64, mod_id: i64| ActiveModuleCycle {
        ship_id,
        module_id: mod_id,
        module_type_id: 10_838,
        duration_s: 4.0,
        repeat: 0,
        armor_repair_amount: 40.0,
        damage_vector: DamageVector::new(0.0, 0.0, 0.0, 0.0),
        ..Default::default()
    };
    jita_sys.add_module_cycle(mk_cycle(jita_ship, 910_001)).await.unwrap();
    amarr_sys.add_module_cycle(mk_cycle(amarr_ship, 910_002)).await.unwrap();

    // Advance 4 wall-clock 1.0s ticks in both systems:
    // - Amarr (1.0 TiDi): 4.0 simulated seconds -> module cycle finishes, ship advances full 4s distance
    // - Jita (0.50 TiDi): 2.0 simulated seconds -> module cycle STILL running, ship advances ~half distance
    for _ in 0..4 {
        jita_sys.step_manual(1.0).await.unwrap();
        amarr_sys.step_manual(1.0).await.unwrap();
    }
    assert!((amarr_sys.sim_elapsed_secs() - 4.0).abs() < 1e-6);
    assert!((jita_sys.sim_elapsed_secs() - 2.0).abs() < 1e-6);
    assert!(
        !amarr_sys.has_module_cycle(amarr_ship, 910_002).await,
        "4.0s module cycle in 1.0x Amarr must finish after 4 wall ticks"
    );
    assert!(
        jita_sys.has_module_cycle(jita_ship, 910_001).await,
        "4.0s module cycle in 0.50x TiDi Jita must still be mid-cycle at 2.0s sim time"
    );
    let (_, jita_ball_4) = jita_sys.get_ball_snapshot(jita_ship).await.unwrap().unwrap();
    let (_, amarr_ball_4) = amarr_sys.get_ball_snapshot(amarr_ship).await.unwrap().unwrap();
    assert!(
        amarr_ball_4.position.x > jita_ball_4.position.x * 1.4,
        "Normal 1.0x ship ({:.1}m) must travel further in 4 wall ticks than 0.50x TiDi ship ({:.1}m)",
        amarr_ball_4.position.x,
        jita_ball_4.position.x
    );

    // After 4 MORE wall-clock ticks (total 8 wall ticks = 4.0s sim time in Jita), Jita's 4.0s cycle completes and matches Amarr's 4-tick position!
    for _ in 0..4 {
        jita_sys.step_manual(1.0).await.unwrap();
    }
    assert!((jita_sys.sim_elapsed_secs() - 4.0).abs() < 1e-6);
    assert!(
        !jita_sys.has_module_cycle(jita_ship, 910_001).await,
        "4.0s module cycle in 0.50x TiDi Jita must complete at 8 wall ticks (4.0s sim time)"
    );
    let (_, jita_ball_8) = jita_sys.get_ball_snapshot(jita_ship).await.unwrap().unwrap();
    assert!(
        (jita_ball_8.position.x - amarr_ball_4.position.x).abs() < 1e-3,
        "8 wall ticks at 0.50 TiDi must produce identical physics integration as 4 wall ticks at 1.0x"
    );
}

#[tokio::test]
async fn test_worker_node_load_balancing_and_reinforced_fleet_fight_isolation() {
    // parity: feat:serverops.node-balancing
    eve_testkit::attest(&["feat:serverops.node-balancing"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    let mut s_perim = setup_pilot(&ctx, 19_011, "demo", "demo-secret", P1).await;
    s_perim.station_id = None;
    s_perim.solar_system_id = Some(PERIMETER_SYSTEM_ID);
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&P1) {
            c1.station_id = None;
            c1.solar_system_id = PERIMETER_SYSTEM_ID;
        }
    });

    // Jita (30000142) and Perimeter (30000144) belong to the same Kimotoro constellation (20000020)
    // and therefore start colocated on the same general worker node.
    let jita_initial_node = get_system_worker_node(&ctx, JITA_SYSTEM_ID);
    let perim_initial_node = get_system_worker_node(&ctx, PERIMETER_SYSTEM_ID);
    assert_eq!(jita_initial_node, perim_initial_node);

    set_cluster_node_load_threshold(&ctx, 10);
    let jita_sys = ctx.systems.get_or_spawn(JITA_SYSTEM_ID);
    let perim_sys = ctx.systems.get_or_spawn(PERIMETER_SYSTEM_ID);
    let cs = CombatState::new();

    // Spawn 16 fleet ships in Jita and 2 quiet ships in Perimeter on the shared worker node (load = 18 > threshold 10)
    for idx in 0..16_i64 {
        jita_sys
            .spawn_ball_with_combat(spawn_test_ship_ball(820_000 + idx), cs.clone())
            .await
            .unwrap();
    }
    for idx in 0..2_i64 {
        perim_sys
            .spawn_ball_with_combat(spawn_test_ship_ball(821_000 + idx), cs.clone())
            .await
            .unwrap();
    }
    let _ = ctx.drain_notifications_for_char(P1);

    let tidi_before = recompute_cluster_node_tidi(&ctx);
    assert!(
        tidi_before[&JITA_SYSTEM_ID] < 1.0 && tidi_before[&PERIMETER_SYSTEM_ID] < 1.0,
        "Shared overloaded worker node must induce TiDi on both colocated systems before balancing"
    );
    assert!(
        ctx.drain_notifications_for_char(P1)
            .iter()
            .any(|n| n.event == "DoSimClockRebase"),
        "Load-induced TiDi must notify Perimeter pilot via DoSimClockRebase"
    );

    // 1. Automatic worker-node rebalancing migrates quiet Perimeter off the overloaded Jita worker onto a healthy worker
    let migrated = rebalance_overloaded_worker_nodes(&ctx);
    assert!(migrated >= 1, "rebalance_overloaded_worker_nodes must migrate secondary system off overloaded worker");
    let perim_rebalanced_node = get_system_worker_node(&ctx, PERIMETER_SYSTEM_ID);
    assert_ne!(perim_rebalanced_node, jita_initial_node);
    assert!((get_system_tidi_factor(&ctx, PERIMETER_SYSTEM_ID) - 1.0).abs() < 1e-6, "Perimeter recovers to 1.0x after migration");
    assert!(get_system_tidi_factor(&ctx, JITA_SYSTEM_ID) < 1.0, "Heavy Jita fleet fight stays in TiDi");

    // 2. Fleet-fight reinforcement isolates Jita onto a dedicated reinforced node (>= 100) and registers its address in machoNet
    let reinforced_node = reinforce_solar_system_for_fleet_fight(&ctx, JITA_SYSTEM_ID);
    assert!(reinforced_node >= 100);
    assert_eq!(assign_or_reinforce_system_node(&ctx, JITA_SYSTEM_ID, None), reinforced_node);
    assert_eq!(get_worker_node_load(&ctx, reinforced_node), 16);

    let port = 26_000 + reinforced_node;
    let resolved_node = rpc(
        &ctx,
        &mut s_perim,
        "machoNet",
        "GetNodeFromAddress",
        &[PyRep::String(format!("127.0.0.1:{port}"))],
    )
    .await
    .expect("GetNodeFromAddress for reinforced node");
    assert_eq!(resolved_node, PyRep::Integer(reinforced_node as i64));
}

#[tokio::test]
async fn test_downtime_countdown_warnings_session_disconnect_login_rejection_and_restart() {
    // parity: feat:serverops.downtime
    eve_testkit::attest(&["feat:serverops.downtime"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // Align virtual clock to 09:50 UTC (70m before 11:00 UTC downtime = 39,600s of day)
    let cur_mod = ctx.now_unix() % 86_400;
    let nine_fifty_utc = 9 * 3_600 + 50 * 60;
    let delta_to_0950 = if cur_mod <= nine_fifty_utc {
        nine_fifty_utc - cur_mod
    } else {
        86_400 - cur_mod + nine_fifty_utc
    };
    ctx.advance_secs(delta_to_0950);

    let s1 = setup_pilot(&ctx, 19_021, "demo", "demo-secret", P1).await;
    let (closer_tx, mut closer_rx) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(s1.session_id, closer_tx);
    assert_eq!(ctx.db.online_session_count(), 1);
    let _ = ctx.drain_notifications_for_char(P1);

    // At 09:50 UTC (> 60m): no warning
    assert!(step_downtime_countdown_warnings(&ctx, ctx.now_unix()).is_empty());

    // Advance to 10:00 UTC (T-60m), 10:45 UTC (T-15m), and 10:55 UTC (T-5m) via tick_background_cluster
    for advance_s in [10 * 60_u64, 45 * 60, 10 * 60] {
        ctx.advance_secs(advance_s);
        tick_background_cluster(&ctx).await;
        // Repeated tick at same timestamp must NOT duplicate countdown notification
        tick_background_cluster(&ctx).await;
    }

    let warnings: Vec<_> = ctx
        .drain_notifications_for_char(P1)
        .into_iter()
        .filter(|n| n.event == "OnClusterShutdownInitiated")
        .collect();
    assert_eq!(
        warnings.len(),
        3,
        "Pilots must receive exactly 3 OnClusterShutdownInitiated warnings at 60m, 15m, and 5m"
    );
    for w in &warnings {
        assert_eq!(
            w.args.len(),
            3,
            "OnClusterShutdownInitiated(explanationLabel, when, duration) requires 3 args"
        );
        assert!(w.args[0].as_str().is_some());
        assert!(w.args[1].as_i64().unwrap_or(0) > 130_000_000_000_000_000);
        assert_eq!(w.args[2], PyRep::Integer(15));
    }

    // Advance 5m to 11:00 UTC: GM TCP session stays connected, non-GM session is disconnected,
    // and scheduled 11:00-11:15 UTC downtime window keeps shutdown_in_progress=true until 11:15 UTC.
    let (gm_uid, gm_cid) = ctx.db.with_mut_snapshot(|snap| {
        let uid = 99_901_u64;
        let cid = 90_999_001_i64;
        let mut acc = snap.accounts.get("demo").unwrap().clone();
        acc.user_id = uid;
        acc.username = "gm_ops".into();
        acc.password_token = "gm-secret".into();
        acc.role = eve_db::service_roles::ROLE_GM_MASK;
        snap.accounts.insert("gm_ops".into(), acc);
        let mut ch = snap.characters.get(&P1).unwrap().clone();
        ch.character_id = cid;
        ch.user_id = uid;
        ch.name = "GM Ops".into();
        snap.characters.insert(cid, ch);
        (uid, cid)
    });
    let mut gm_sess = SessionState::new(19_029);
    gm_sess.user_id = Some(gm_uid);
    gm_sess.character_id = Some(gm_cid);
    let _ = ctx.db.acquire_online_lock(gm_uid, gm_sess.session_id, Some(gm_cid), false, 0);
    let (gm_closer_tx, mut gm_closer_rx) = tokio::sync::mpsc::unbounded_channel();
    ctx.register_session_closer(gm_sess.session_id, gm_closer_tx);

    ctx.advance_secs(5 * 60);
    tick_background_cluster(&ctx).await;
    assert!(ctx.is_shutting_down(), "11:00 UTC downtime window must persist across background ticks");
    assert!(gm_closer_rx.try_recv().is_err(), "GM TCP session must NOT be disconnected by downtime");
    assert_eq!(ctx.db.online_session_count(), 1, "Only GM online session remains");
    let close_msg = closer_rx.try_recv().expect("non-GM session_closers channel must receive shutdown close signal");
    assert_eq!(close_msg.0, "ClusterShutdown");

    // Non-GM login AND character selection during active shutdown must fail with ClusterShuttingDown
    let mut login_attempt = SessionState::new(19_022);
    let login_err = ctx
        .dispatch_rpc(
            &mut login_attempt,
            "authentication",
            None,
            "Login",
            &[PyRep::String("demo".into()), PyRep::String("demo-secret".into())],
            &HashMap::new(),
            None,
        )
        .await
        .expect_err("Non-GM Login during active cluster shutdown must fail");
    assert_eq!(login_err.code, "ClusterShuttingDown");

    // Advance 15m to 11:15 UTC -> tick_background_cluster automatically completes cluster restart
    ctx.advance_secs(15 * 60);
    tick_background_cluster(&ctx).await;
    assert!(!ctx.is_shutting_down(), "11:15 UTC tick must complete downtime restart");
    let _s1_reconnected = setup_pilot(&ctx, 19_023, "demo", "demo-secret", P1).await;
    assert_eq!(ctx.db.online_session_count(), 2);
}

#[tokio::test]
async fn test_daily_maintenance_jobs_mutate_belts_ice_market_history_sci_sov_missions_ess_and_orders() {
    // parity: feat:serverops.daily-jobs
    eve_testkit::attest(&["feat:serverops.daily-jobs"]);
    let ctx = ServerContext::new();
    common::pin_clock(&ctx);

    // Align clock to 11:02 UTC inside the 11:00 UTC maintenance window
    let cur_mod = ctx.now_unix() % 86_400;
    let target_1102 = 11 * 3_600 + 120;
    let delta = if cur_mod <= target_1102 {
        target_1102 - cur_mod
    } else {
        86_400 - cur_mod + target_1102
    };
    ctx.advance_secs(delta);
    let now = ctx.now_unix();

    // 1. Populate Perimeter SDE asteroid belts, completely empty belt #1 (`Asteroids_and_ore.md:69`),
    //    partly mine 1 asteroid in belt #2, and spawn + deplete an SDE White Glaze ice block (`16265`, group `465`).
    let perim_sys = ctx.systems.get_or_spawn(PERIMETER_SYSTEM_ID);
    let belt_asts = perim_sys.populate_asteroid_belt_entities().await.unwrap();
    let belt_ids: BTreeSet<i64> = belt_asts.iter().map(|a| a.belt_id).collect();
    let mut belt_iter = belt_ids.into_iter();
    let empty_belt_id = belt_iter.next().expect("belt 1");
    let partial_belt_id = belt_iter.next().expect("belt 2");

    let empty_belt_asts: Vec<_> = belt_asts.iter().filter(|a| a.belt_id == empty_belt_id).cloned().collect();
    for a in &empty_belt_asts {
        let res = perim_sys.mine_asteroid(a.asteroid_id, a.initial_units, 0.0).await.unwrap();
        assert!(res.depleted);
    }
    let partial_first = belt_asts.iter().find(|a| a.belt_id == partial_belt_id).unwrap().clone();
    let _ = perim_sys.mine_asteroid(partial_first.asteroid_id, partial_first.initial_units, 0.0).await.unwrap();

    let ice_ast_id = 799_500_001_i64;
    perim_sys
        .spawn_asteroid(
            AsteroidState {
                asteroid_id: ice_ast_id,
                belt_id: 0,
                type_id: 16_265, // White Glaze (Ice, group 465)
                initial_units: 500,
                remaining_units: 500,
                unit_volume: 1_000.0,
                depleted: false,
                respawn_at_tick: None,
            },
            Ball {
                id: ice_ast_id,
                type_id: 16_265,
                position: DVec3::new(25_000.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 1.0e12,
                radius: 800.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            },
        )
        .await
        .unwrap();
    ctx.db.with_mut_snapshot(|snap| {
        snap.asteroids.insert(
            ice_ast_id,
            AsteroidRecord {
                asteroid_id: ice_ast_id,
                belt_id: 0,
                type_id: 16_265,
                solar_system_id: PERIMETER_SYSTEM_ID,
                position: (25_000.0, 0.0, 0.0),
                quantity: 500,
                initial_quantity: 500,
                radius: 800.0,
            },
        );
    });
    let ice_mined = perim_sys.mine_asteroid(ice_ast_id, 500, 0.0).await.unwrap();
    assert!(ice_mined.depleted);

    // 2. Seed 24h market fills, 28d industry job, elevated military/industrial/strategic sov indices + ESS bank,
    //    expired mission decline cooldown + accepted mission, and an expired market order.
    let null_sov_sys: u32 = 30_004_759; // 1DQ1-A
    let hub_sov_sys: u32 = 30_004_760;
    let sov_hub_id: i64 = 950_777;
    let expired_oid = 889_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.market_transactions.push(MarketTransactionRecord {
            transaction_id: 770_001,
            date_unix: now - 7_200,
            type_id: 34,
            quantity: 1_000,
            price: IskCents::from_f64_isk(5.0),
            location_id: JITA_STATION_ID,
            buyer_id: P1,
            seller_id: P2,
            buyer_account_key: 1000,
            seller_account_key: 1000,
            journal_ref_id: 1,
        });
        snap.market_transactions.push(MarketTransactionRecord {
            transaction_id: 770_002,
            date_unix: now - 1_800,
            type_id: 34,
            quantity: 3_000,
            price: IskCents::from_f64_isk(7.0),
            location_id: JITA_STATION_ID,
            buyer_id: P1,
            seller_id: P2,
            buyer_account_key: 1000,
            seller_account_key: 1000,
            journal_ref_id: 2,
        });

        snap.industry_jobs.insert(
            660_001,
            IndustryJobRecord {
                job_id: 660_001,
                installer_id: P1,
                facility_id: JITA_STATION_ID,
                blueprint_item_id: 501,
                blueprint_type_id: 687,
                product_type_id: 587,
                activity_id: 1,
                runs: 10,
                status: "Active".into(),
                start_unix: now - 3_600,
                duration_seconds: 7_200,
            },
        );

        snap.system_states.insert(
            null_sov_sys,
            SystemDynamicState {
                solar_system_id: null_sov_sys,
                dbs_multiplier: 1.0,
                ess_main_bank: IskCents::from_isk(1_000_000), ess_reserve_bank: IskCents::from_isk(200_000),
                ess_linked_character_id: Some(P1), ess_link_started_tick: Some(42), ess_cycle_started_unix: now.saturating_sub(600),
                ess_main_bank_contributors: Default::default(),
                sov_alliance_id: Some(99_000_001),
                military_index: 4,
                industrial_index: 3,
                strategic_index: 3,
                fw_contested_pct: 0.0,
                insurgency_corruption_stage: 0,
                insurgency_suppression_stage: 0,
                wormhole_remaining_mass_kg: 0.0,
                wormhole_destination_system_id: None,
                signature_position: None,
                fw_complex_position: None,
                fw_capture_seconds_elapsed: 0,
                fw_capture_duration_seconds: 0,
                last_fw_capture_tick: 0,
                last_sov_claim_tick: 0,
            },
        );
        snap.system_states.insert(hub_sov_sys, snap.system_states[&null_sov_sys].clone());
        if let Some(hub_st) = snap.system_states.get_mut(&hub_sov_sys) {
            hub_st.solar_system_id = hub_sov_sys;
        }
        snap.structures.insert(
            sov_hub_id,
            eve_db::UpwellStructureRecord {
                structure_id: sov_hub_id, type_id: eve_server::services::sov_services::TYPE_SOVEREIGNTY_HUB,
                owner_corp_id: 98_000_001, solar_system_id: hub_sov_sys, name: "ADM Rollover Sov Hub".into(),
                position: [15_000.0, 0.0, 0.0], state: "ShieldVulnerable".into(), quantum_core_installed: true,
                fuel_blocks: 1_000, shield_hp: 1.0, armor_hp: 1.0, hull_hp: 1.0,
                reinforced_until_unix: 0, last_damage_tick: 0, last_damage_unix: 0, self_destruct_at_unix: None,
            },
        );
        snap.sov_runtime.adm_vulnerability_windows.insert(sov_hub_id, (12 * 3_600, 14 * 3_600));

        snap.last_mission_decline_times.insert(P1, now - 5 * 3_600);
        snap.active_missions.insert(
            (P1, 3_019_001),
            ActiveMissionRecord {
                mission_id: 551_001,
                character_id: P1,
                agent_id: 3_019_001,
                solar_system_id: PERIMETER_SYSTEM_ID,
                state: "Accepted".into(),
                current_room_id: 2,
                npcs_remaining: 1,
                spawned_npc_ball_ids: Vec::new(),
                bookmark_id: None,
            },
        );

        snap.market_orders.insert(
            expired_oid,
            MarketOrderRecord {
                order_id: expired_oid,
                owner_id: P1,
                station_id: JITA_STATION_ID,
                region_id: THE_FORGE_REGION_ID,
                type_id: 34,
                is_bid: true,
                price: IskCents::from_isk(4),
                vol_entered: 100,
                vol_remaining: 100,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents::from_isk(400),
            },
        );
        snap.market_order_meta.insert(
            expired_oid,
            MarketOrderMeta {
                issued_unix: now - 10 * 86_400,
                duration_days: 1,
                range: "station".into(),
                issued_by: Some(P1),
                wallet_division: None,
                is_corporation: false,
                state: None,
            },
        );
    });

    // Execute daily downtime & maintenance jobs
    let report = run_daily_downtime_and_jobs(&ctx);
    assert_eq!(report.get("is1100UtcWindow"), Some(&1));
    assert!(report.get("respawnedBeltAsteroids").copied().unwrap_or(0) >= (empty_belt_asts.len() as i64 + 1));
    assert!(report.get("rolledUpMarketHistoryPairs").copied().unwrap_or(0) >= 1);
    assert!(report.get("recomputedIndustrySystems").copied().unwrap_or(0) >= 5_000);
    assert!(report.get("sovSystemsDecayed").copied().unwrap_or(0) >= 1);
    assert!(report.get("essBanksRolledOver").copied().unwrap_or(0) >= 1);
    assert_eq!(report.get("missionDeclinesReset"), Some(&1));
    assert!(report.get("expiredMarketOrders").copied().unwrap_or(0) >= 1);
    assert_eq!(report.get("missionNpcsRespawned"), Some(&1));

    // Verify empty belt #1 AND White Glaze ice respawned, while partly mined belt #2 stayed un-respawned (`Asteroids_and_ore.md:69`)
    for a in &empty_belt_asts {
        let st = perim_sys.get_asteroid_state(a.asteroid_id).await.unwrap().unwrap();
        assert!(!st.depleted && st.remaining_units == a.initial_units);
        assert!(perim_sys.get_ball_snapshot(a.asteroid_id).await.unwrap().is_some());
    }
    let st_partial = perim_sys.get_asteroid_state(partial_first.asteroid_id).await.unwrap().unwrap();
    assert!(st_partial.depleted, "Partly-mined ore belt must NOT respawn while sibling asteroids remain");

    let st_ice = perim_sys.get_asteroid_state(ice_ast_id).await.unwrap().unwrap();
    assert!(!st_ice.depleted && st_ice.remaining_units == 500, "Depleted SDE Ice block must respawn on downtime");
    assert!(perim_sys.get_ball_snapshot(ice_ast_id).await.unwrap().is_some());

    // Verify 24h market history candle, 28d SCI blob, all 3 decayed sov indices, ESS rollover, mission reset, and expired order removal
    let today = eve_db::snapshot_ext::unix_secs_to_date_string(now);
    ctx.db.with_snapshot(|snap| {
        let hist = snap
            .market_history
            .get(&(THE_FORGE_REGION_ID, 34))
            .expect("24h market history rolled up for (The Forge, Tritanium)");
        let candle = hist.iter().find(|d| d.date == today).expect("today's OHLCV candle");
        assert_eq!(candle.volume, 4_000);
        assert_eq!(candle.order_count, 2);
        assert!((candle.lowest - 5.0).abs() < 1e-6);
        assert!((candle.highest - 7.0).abs() < 1e-6);
        assert!((candle.average - 6.5).abs() < 1e-6); // (1000*5 + 3000*7) / 4000 = 6.5
        assert_eq!(candle.open_price, Some(5.0));
        assert_eq!(candle.close_price, Some(7.0));

        let sci_meta: serde_json::Value = serde_json::from_str(
            snap.service_state_blobs
                .get("industry_system_cost_indices_daily")
                .expect("28d SCI daily recompute metadata"),
        )
        .unwrap();
        assert_eq!(sci_meta["recomputed_at_unix"].as_u64(), Some(now));
        assert!(sci_meta["active_systems"].as_i64().unwrap_or(0) >= 1);

        let sov = &snap.system_states[&null_sov_sys];
        assert_eq!(sov.military_index, 3, "military_index must decay 4 -> 3");
        assert_eq!(sov.industrial_index, 2, "industrial_index must decay 3 -> 2");
        assert_eq!(sov.strategic_index, 3, "strategic_index must not decay while sov is held");
        let (vuln_start, vuln_end) = snap.sov_runtime.adm_vulnerability_windows[&sov_hub_id];
        assert!(vuln_end - vuln_start > 7_200, "Sov Hub ADM decay must widen vulnerability window");
        assert_eq!(sov.ess_main_bank, IskCents::from_isk(1_000_000));
        assert_eq!(sov.ess_reserve_bank, IskCents::from_isk(200_000));
        assert_eq!(sov.ess_linked_character_id, None, "Daily maintenance must reset stale ESS links");

        assert!(!snap.last_mission_decline_times.contains_key(&P1));
        assert_eq!(snap.active_missions[&(P1, 3_019_001)].current_room_id, 1);
        assert_eq!(snap.active_missions[&(P1, 3_019_001)].npcs_remaining, 4);
        assert!(snap.market_orders.get(&expired_oid).map_or(true, |o| o.vol_remaining == 0));
    });
}

pub(crate) mod place_pilot_spawn_ship {
    // Regression tests for blocking findings B-01 through B-05:
    // - B-01: Atomic `contribute_fw_beacon_material_inner` (no TOCTOU panic/cargo-loss/uncapped-LP)
    // - B-02: Dilated `DoSimClockRebase` progression & `SystemHandle::step_manual` TiDi scaling
    // - B-03: FW beacon deployment security-status, warzone, and per-system uniqueness gates
    // - B-04: Scheduled 11:00 UTC downtime without pre-11:00 countdown arm & real ESS rollover
    // - B-05: Unified `OnShipcasterTargetStateChanged` `solarsystemid` scope & online-only delivery

    use crate::common;
    use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, SystemDynamicState};
    use eve_destiny::{Ball, MovementMode};
    use eve_machonet::SessionState;
    use eve_proto::PyRep;
    use eve_server::router::ServerContext;
    use eve_server::services::cluster_macho_misc_services::with_misc_state;
    use eve_server::services::serverops_cluster::{
        execute_daily_maintenance_jobs, set_scheduled_downtime_enabled, set_system_tidi_factor,
    };
    use eve_server::services::shipcaster_ops::{
        contribute_fw_beacon_material, deploy_fw_beacon_constructor, link_fw_landing_pad,
        on_landing_pad_ball_destroyed,
    };
    use eve_server::services::space_destiny::{
        get_ess_reserve_pulses, set_ess_reserve_pulses, EssReservePulseState,
    };
    use eve_server::session_cluster_lifecycle::tick_background_cluster;
    use glam::DVec3;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn place_pilot(ctx: &ServerContext, sess: &mut SessionState, char_id: i64, ship_id: i64, sys_id: u32) {
        sess.station_id = None;
        sess.structure_id = None;
        sess.ship_id = Some(ship_id);
        sess.solar_system_id = Some(sys_id);
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&char_id) {
                ch.solar_system_id = sys_id;
                ch.station_id = None;
                ch.structure_id = None;
                ch.active_ship_id = ship_id;
                ch.war_faction_id = Some(500_001);
            }
            if let Some(ship) = snap.items.get_mut(&ship_id) {
                ship.location_id = sys_id as i64;
            }
        });
    }

    async fn spawn_ship(ctx: &ServerContext, sys_id: u32, char_id: i64, ship_id: i64, pos: DVec3) {
        let handle = ctx.systems.get_or_spawn(sys_id);
        let _ = handle.remove_ball(ship_id).await;
        let _ = handle
            .spawn_ball_with_combat(
                Ball {
                    id: ship_id,
                    type_id: 601,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 1000.0,
                    inertia: 1.0,
                    mass: 100_000.0,
                    radius: 50.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_ship_combat_state(char_id, ship_id, 601),
            )
            .await;
    }

    async fn setup_pilot_session() -> (ServerContext, SessionState, i64, i64) {
        let ctx = ServerContext::new();
        common::pin_clock(&ctx);
        ctx.advance_secs(4_200);
        let mut sess = SessionState::new(30_199);
        common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
        let char_id = 90_000_001_i64;
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
        let ship_id = sess.ship_id.unwrap_or(100_000_001);
        (ctx, sess, char_id, ship_id)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn b01_concurrent_deposit_and_destroyed_pad_atomicity() {
        let (ctx, mut sess, char_id, ship_id) = setup_pilot_session().await;
        let fw_sys = 30002811_u32; // Onatoh (Caldari FW lowsec)
        let pos = DVec3::new(100_000.0, 0.0, 0.0);

        place_pilot(&ctx, &mut sess, char_id, ship_id, fw_sys);
        spawn_ship(&ctx, fw_sys, char_id, ship_id, pos + DVec3::new(500.0, 0.0, 0.0)).await;

        let comps = ctx.sde.shipcaster_sde_components().expect("shipcaster SDE");
        let spec = &comps.under_construction[&75933];
        let (&mat_tid, entry) = spec.input_items.iter().next().expect("input_items");
        let req_qty = entry.required_qty;

        let (ctor_id, stack_a, stack_b) = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let ctor_id = snap.next_entity_id;
            snap.items.insert(
                ctor_id,
                InventoryItemRecord {
                    item_id: ctor_id,
                    type_id: 75933,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    ..Default::default()
                },
            );
            snap.next_entity_id += 1;
            let sa = snap.next_entity_id;
            snap.items.insert(
                sa,
                InventoryItemRecord {
                    item_id: sa,
                    type_id: mat_tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: (req_qty as i32) * 2,
                    ..Default::default()
                },
            );
            snap.next_entity_id += 1;
            let sb = snap.next_entity_id;
            snap.items.insert(
                sb,
                InventoryItemRecord {
                    item_id: sb,
                    type_id: mat_tid,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: (req_qty as i32) * 2,
                    ..Default::default()
                },
            );
            (ctor_id, sa, sb)
        });

        let pad = deploy_fw_beacon_constructor(&ctx, &sess, ctor_id, pos)
            .await
            .expect("deploy beacon in FW lowsec");

        // Concurrent deposits must never over-consume cargo or mint uncapped LP
        let (res_a, res_b) = tokio::join!(
            contribute_fw_beacon_material(&ctx, &sess, pad.item_id, stack_a, req_qty),
            contribute_fw_beacon_material(&ctx, &sess, pad.item_id, stack_b, req_qty),
        );
        assert!(res_a.is_ok() || res_b.is_ok(), "at least one concurrent deposit must succeed");

        let (total_remaining, lp_after_concurrent) = ctx.db.with_snapshot(|snap| {
            let qa = snap.items.get(&stack_a).map(|i| i.quantity).unwrap_or(0);
            let qb = snap.items.get(&stack_b).map(|i| i.quantity).unwrap_or(0);
            let lp = snap
                .characters
                .get(&char_id)
                .and_then(|c| entry.lp_corp_id.and_then(|corp| c.loyalty_points.get(&corp).copied()))
                .unwrap_or(0);
            (qa + qb, lp)
        });
        let total_consumed = (req_qty as i32) * 4 - total_remaining;
        assert!(
            total_consumed <= req_qty as i32,
            "concurrent deposits must never consume more than req_qty ({total_consumed} > {req_qty})"
        );
        let max_lp = i64::from(entry.lp_per_unit) * i64::from(req_qty);
        assert!(
            lp_after_concurrent <= max_lp,
            "concurrent deposits must never mint uncapped LP ({lp_after_concurrent} > {max_lp})"
        );

        // Destroying a pad before deposit must fail cleanly without cargo loss or panic
        let ctor_id2 = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: 75933,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    ..Default::default()
                },
            );
            id
        });
        let fw_sys2 = 30002813_u32; // Tama
        place_pilot(&ctx, &mut sess, char_id, ship_id, fw_sys2);
        spawn_ship(&ctx, fw_sys2, char_id, ship_id, pos + DVec3::new(500.0, 0.0, 0.0)).await;
        let pad2 = deploy_fw_beacon_constructor(&ctx, &sess, ctor_id2, pos)
            .await
            .expect("deploy second beacon");
        on_landing_pad_ball_destroyed(&ctx, pad2.item_id).await;

        let before_qty = ctx.db.with_snapshot(|s| s.items.get(&stack_a).map(|i| i.quantity).unwrap_or(0));
        let err = contribute_fw_beacon_material(&ctx, &sess, pad2.item_id, stack_a, 1)
            .await
            .expect_err("deposit into destroyed pad must fail");
        assert_eq!(err.code, "LandingPadNotFound");
        let after_qty = ctx.db.with_snapshot(|s| s.items.get(&stack_a).map(|i| i.quantity).unwrap_or(0));
        assert_eq!(before_qty, after_qty, "failed deposit on destroyed pad must not delete cargo");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn b02_sim_clock_rebase_dilated_progression_and_step_manual_tidi() {
        let (ctx, mut sess, char_id, ship_id) = setup_pilot_session().await;
        let jita = 30000142_u32;
        place_pilot(&ctx, &mut sess, char_id, ship_id, jita);
        let _ = ctx.drain_notifications_for_char(char_id);

        ctx.db.clock().set_paused(true);
        let _ = set_system_tidi_factor(&ctx, jita, 0.50);
        let _ = ctx.drain_notifications_for_char(char_id);

        // Advance 10 wall seconds under 0.50 TiDi -> sim clock must advance 5.0s (50_000_000 ticks), NOT 10.0s
        ctx.db.clock().advance_secs(10);
        let _ = set_system_tidi_factor(&ctx, jita, 0.25);
        let notes1 = ctx.drain_notifications_for_char(char_id);
        let rebase1 = notes1.iter().find(|n| n.event == "DoSimClockRebase").expect("DoSimClockRebase #1");
        let PyRep::Tuple(pair1) = &rebase1.args[0] else { panic!("expected 2-tuple"); };
        let old1 = pair1[0].as_i64().unwrap();
        let new1 = pair1[1].as_i64().unwrap();
        assert_eq!(new1 - old1, 50_000_000, "0.50 TiDi over 10s must advance sim clock by 5.0s");
        assert!(new1 < ctx.now_blueos_filetime_ticks(), "sim clock must stay dilated behind wall clock");

        // Advance 20 wall seconds under 0.25 TiDi -> sim clock must advance 5.0s (50_000_000 ticks)
        ctx.db.clock().advance_secs(20);
        let _ = set_system_tidi_factor(&ctx, jita, 1.0);
        let notes2 = ctx.drain_notifications_for_char(char_id);
        let rebase2 = notes2.iter().find(|n| n.event == "DoSimClockRebase").expect("DoSimClockRebase #2");
        let PyRep::Tuple(pair2) = &rebase2.args[0] else { panic!("expected 2-tuple"); };
        let old2 = pair2[0].as_i64().unwrap();
        let new2 = pair2[1].as_i64().unwrap();
        assert_eq!(old2, new1);
        assert_eq!(new2 - old2, 50_000_000, "0.25 TiDi over 20s must advance sim clock by 5.0s");

        // Verify step_manual scales dt by tidi_factor for system_step_hook
        let observed_dts: Arc<Mutex<Vec<f64>>> = Arc::new(Mutex::new(Vec::new()));
        let obs_clone = Arc::clone(&observed_dts);
        ctx.systems.set_system_step_hook(Arc::new(move |_sys_id, dt, _destroyed| {
            let obs = Arc::clone(&obs_clone);
            Box::pin(async move {
                obs.lock().unwrap().push(dt);
            })
        }));
        let _ = set_system_tidi_factor(&ctx, jita, 0.25);
        let jita_handle = ctx.systems.get_or_spawn(jita);
        let _ = jita_handle.step_manual(4.0).await.expect("step_manual");
        let dts = observed_dts.lock().unwrap().clone();
        assert_eq!(dts.len(), 1);
        assert!((dts[0] - 1.0).abs() < 1e-6, "step_manual(4.0) at 0.25 TiDi must pass eff_dt=1.0, got {}", dts[0]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn b03_beacon_deployment_rejects_nullsec_non_fw_lowsec_and_duplicates() {
        let (ctx, mut sess, char_id, ship_id) = setup_pilot_session().await;
        let pos = DVec3::new(100_000.0, 0.0, 0.0);

        let ctor_id = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            let id = snap.next_entity_id;
            snap.items.insert(
                id,
                InventoryItemRecord {
                    item_id: id,
                    type_id: 75933,
                    owner_id: char_id,
                    location_id: ship_id,
                    flag: InventoryFlag::Cargo,
                    quantity: 5,
                    ..Default::default()
                },
            );
            id
        });

        // 1. Nullsec system (1DQ1-A: 30004759, security_status <= 0.0) must be rejected
        let nullsec_sys = 30004759_u32;
        place_pilot(&ctx, &mut sess, char_id, ship_id, nullsec_sys);
        let err_null = deploy_fw_beacon_constructor(&ctx, &sess, ctor_id, pos)
            .await
            .expect_err("nullsec deployment must fail");
        assert_eq!(err_null.code, "InvalidBeaconDeploymentSystem");

        // 2. Non-FW lowsec system (remove a lowsec system from fw_system_occupations to simulate non-FW lowsec)
        let non_fw_lowsec = ctx
            .sde
            .solar_systems
            .values()
            .find(|s| {
                let r = (s.security_status * 10.0).round() / 10.0;
                s.solar_system_id < 31_000_000
                    && s.solar_system_id != 30_100_000
                    && r > 0.0
                    && r < 0.45
                    && !ctx.db.with_snapshot(|snap| snap.fw_system_occupations.contains_key(&s.solar_system_id))
            })
            .map(|s| s.solar_system_id)
            .expect("at least one non-FW lowsec system in SDE");
        place_pilot(&ctx, &mut sess, char_id, ship_id, non_fw_lowsec);
        let err_non_fw = deploy_fw_beacon_constructor(&ctx, &sess, ctor_id, pos)
            .await
            .expect_err("non-FW lowsec deployment must fail");
        assert_eq!(err_non_fw.code, "InvalidBeaconDeploymentSystem");

        // 3. Valid FW lowsec system (Onatoh 30002811) succeeds once, then rejects duplicate
        let fw_sys = 30002811_u32;
        place_pilot(&ctx, &mut sess, char_id, ship_id, fw_sys);
        let _first = deploy_fw_beacon_constructor(&ctx, &sess, ctor_id, pos)
            .await
            .expect("first FW lowsec deployment must succeed");
        let err_dup = deploy_fw_beacon_constructor(&ctx, &sess, ctor_id, pos + DVec3::new(50_000.0, 0.0, 0.0))
            .await
            .expect_err("duplicate same-system beacon deployment must fail");
        assert_eq!(err_dup.code, "BeaconAlreadyExistsInSystem");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn b04_scheduled_downtime_without_pre_arm_and_ess_reserve_reset() {
        let (ctx, _sess, _char_id, _ship_id) = setup_pilot_session().await;
        ctx.db.clock().set_paused(true);

        // Seed active ESS main-bank link AND ESS reserve-bank pulse state
        let ess_sys = 30004759_u32;
        ctx.db.with_mut_snapshot(|snap| {
            snap.system_states.insert(
                ess_sys,
                SystemDynamicState {
                    solar_system_id: ess_sys,
                    dbs_multiplier: 1.0,
                    ess_main_bank: IskCents::from_isk(500_000),
                    ess_reserve_bank: IskCents::from_isk(250_000),
                    ess_linked_character_id: Some(90_000_001),
                    ess_link_started_tick: Some(10),
                    ess_cycle_started_unix: 1_700_000_000,
                    ess_main_bank_contributors: HashMap::new(),
                    ..Default::default()
                },
            );
            set_ess_reserve_pulses(
                snap,
                ess_sys,
                &EssReservePulseState {
                    pulses_total: 10,
                    pulses_remaining: 7,
                    last_pulse_sec: Some(1_700_000_100),
                    reserve_linked_character_id: Some(90_000_001),
                    reserve_link_started_tick: Some(12),
                    link_started_unix: Some(1_700_000_050),
                    unlocked_until_unix: 1_700_002_000,
                },
            );
        });

        // Opt-out flag prevents automatic shutdown when disabled
        set_scheduled_downtime_enabled(&ctx, false);
        let now = ctx.now_unix();
        let day_start = now - (now % 86_400);
        let in_dt_window = day_start + 86_400 + 11 * 3_600 + 120; // Next day 11:02 UTC (no pre-arm tick!)
        ctx.db.clock().advance_secs(in_dt_window.saturating_sub(now));
        tick_background_cluster(&ctx).await;
        assert!(!ctx.is_shutting_down(), "opt-out flag must skip automatic downtime shutdown");

        // Re-enable scheduled downtime and advance to the following day 11:03 UTC without any 10:xx countdown tick
        set_scheduled_downtime_enabled(&ctx, true);
        let next_dt_window = in_dt_window + 86_400 + 60;
        ctx.db.clock().advance_secs(next_dt_window - in_dt_window);
        tick_background_cluster(&ctx).await;
        assert!(
            ctx.is_shutting_down(),
            "entering 11:00..11:15 UTC must trigger scheduled downtime even without a prior 10:xx countdown tick"
        );

        // Verify ESS main-bank link & reserve-bank pulse state were reset while balances remain intact
        ctx.db.with_snapshot(|snap| {
            let sov = &snap.system_states[&ess_sys];
            assert_eq!(sov.ess_main_bank, IskCents::from_isk(500_000));
            assert_eq!(sov.ess_reserve_bank, IskCents::from_isk(250_000));
            assert_eq!(sov.ess_linked_character_id, None);
            assert_eq!(sov.ess_link_started_tick, None);
            assert_eq!(sov.ess_cycle_started_unix, 0);
            let pulses = get_ess_reserve_pulses(snap, ess_sys);
            assert_eq!(pulses.pulses_remaining, 0);
            assert_eq!(pulses.reserve_linked_character_id, None);
        });

        // Also verify execute_daily_maintenance_jobs reports essBanksRolledOver >= 1 when ESS state is active
        ctx.db.with_mut_snapshot(|snap| {
            if let Some(sov) = snap.system_states.get_mut(&ess_sys) {
                sov.ess_linked_character_id = Some(90_000_001);
            }
        });
        let report = execute_daily_maintenance_jobs(&ctx);
        assert!(report.get("essBanksRolledOver").copied().unwrap_or(0) >= 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn b05_shipcaster_target_state_changed_unified_scope_and_online_only() {
        let (ctx, mut sess, online_char, ship_id) = setup_pilot_session().await;
        let fw_sys = 30002811_u32;
        let pos = DVec3::new(100_000.0, 0.0, 0.0);

        place_pilot(&ctx, &mut sess, online_char, ship_id, fw_sys);
        spawn_ship(&ctx, fw_sys, online_char, ship_id, pos + DVec3::new(500.0, 0.0, 0.0)).await;

        // Seed an offline character in the same solar system (no online session)
        let offline_char = 90_000_777_i64;
        ctx.db.with_mut_snapshot(|snap| {
            let mut off_ch = snap.characters[&online_char].clone();
            off_ch.character_id = offline_char;
            off_ch.user_id = 999_777;
            off_ch.solar_system_id = fw_sys;
            snap.characters.insert(offline_char, off_ch);
        });

        // Seed a built pad in fw_sys and link it -> emits OnShipcasterTargetStateChanged with id_type == "solarsystemid"
        let pad_id = 109_888_001_i64;
        with_misc_state(&ctx, |st| {
            st.landing_pads.push(eve_server::services::cluster_macho_misc_services::LandingPadRecord {
                item_id: pad_id,
                type_id: 75929,
                built_type_id: 75929,
                solar_system_id: fw_sys,
                faction_id: 500_001,
                is_built: true,
                link_timestamp: 0,
                is_disrupted: false,
                position: pos,
                required_materials: Default::default(),
                contributed_materials: Default::default(),
            });
        });
        let _ = ctx.drain_notifications_for_char(online_char);
        let _ = ctx.drain_notifications_for_char(offline_char);

        let _ = link_fw_landing_pad(&ctx, pad_id).expect("link pad");
        let link_notes = ctx.drain_notifications_for_char(online_char);
        let link_ev = link_notes
            .iter()
            .find(|n| n.event == "OnShipcasterTargetStateChanged")
            .expect("link OnShipcasterTargetStateChanged");
        assert_eq!(link_ev.id_type, "solarsystemid");
        assert!(ctx.drain_notifications_for_char(offline_char).is_empty(), "offline char must not receive notification");

        // Destroy pad -> emits OnShipcasterTargetStateChanged with identical id_type == "solarsystemid" and online-only
        on_landing_pad_ball_destroyed(&ctx, pad_id).await;
        let destroy_notes = ctx.drain_notifications_for_char(online_char);
        let destroy_ev = destroy_notes
            .iter()
            .find(|n| n.event == "OnShipcasterTargetStateChanged")
            .expect("destroy OnShipcasterTargetStateChanged");
        assert_eq!(destroy_ev.id_type, "solarsystemid");
        assert!(
            ctx.drain_notifications_for_char(offline_char).is_empty(),
            "offline char must not receive pad destruction notification"
        );
    }
}
