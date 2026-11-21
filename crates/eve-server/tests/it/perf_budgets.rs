//! Performance budget regression tests for GH #1077 Section E.25.
//!
//! Functional and state-invariant assertions always run; tight wall-clock timing
//! assertions are gated on `EVOMU_PERF_TESTS=1` so debug builds under parallel
//! load remain deterministic.

use crate::common;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents};
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode};
use eve_machonet::SessionState;
use eve_proto::PyRep;
use eve_server::combat::crimewatch::credit_specialized_npc_bounty_and_sec_status;
use eve_server::router::TestContext;
use eve_server::services::combat_npc_encounters::flush_npc_bounty_tick;
use eve_server::system_actor::ActiveModuleCycle;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

fn new_perf_ctx() -> TestContext {
    let sde = Arc::new(
        eve_sde::SdeRegistry::open(&eve_sde::SdeRegistry::default_db_path()).expect("sde"),
    );
    let db = Arc::new(eve_db::WorldRepository::with_demo_seed());
    let ctx = TestContext::with_sde_and_repository(sde, db).with_rpc_rate_budget_disabled();
    common::pin_clock(&ctx);
    // Bypass per-session RPC token buckets so multi-thousand-call latency benchmarks
    // neither trip `RateLimited` nor pay `rpc_rate_buckets` mutex overhead.
    ctx
}

fn perf_gate_enabled() -> bool {
    matches!(
        std::env::var("EVOMU_PERF_TESTS").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE")
    )
}

#[tokio::test]
async fn perf_module_cycle_under_50us() {
    // parity: feat:dogma.module-cycle
    eve_testkit::attest(&["feat:dogma.module-cycle"]);

    let ctx = new_perf_ctx();
    let sys = ctx.systems.get_or_spawn(30_000_142);

    let attacker_id = 950_100_i64;
    let target_id = 950_101_i64;
    for (bid, pos) in [
        (attacker_id, DVec3::ZERO),
        (target_id, DVec3::new(1_000.0, 0.0, 0.0)),
    ] {
        let mut cs = ctx.build_type_combat_state(587);
        if bid == target_id {
            cs.shield_hp = 100_000.0;
            cs.max_shield_hp = 100_000.0;
        }
        sys.spawn_ball_with_combat(
            Ball {
                id: bid,
                type_id: 587,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
        )
        .await
        .expect("spawn_ball_with_combat");
    }

    let shield_before = sys
        .get_combat_state(target_id)
        .await
        .unwrap()
        .expect("target combat state")
        .shield_hp;

    let module_count = 500_usize;
    let repeat_steps = 10_usize;
    for i in 0..module_count {
        sys.add_module_cycle(ActiveModuleCycle {
            ship_id: attacker_id,
            module_id: 960_000 + i as i64,
            module_type_id: 3528,
            effect_name: "targetAttack".into(),
            target_id: Some(target_id),
            repeat: repeat_steps as i32,
            duration_s: 1.0,
            damage_vector: DamageVector::new(1.0, 0.0, 0.0, 0.0),
            ..Default::default()
        })
        .await
        .expect("add_module_cycle");
    }

    let t0 = Instant::now();
    for _ in 0..repeat_steps {
        let _ = sys.step_manual(1.0).await.expect("step_manual");
    }
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1_000.0;
    let total_cycles = (module_count * repeat_steps) as f64;
    let avg_ms = elapsed_ms / total_cycles;

    let shield_after = sys
        .get_combat_state(target_id)
        .await
        .unwrap()
        .expect("target combat state after")
        .shield_hp;
    assert!(
        shield_after < shield_before,
        "module cycles must damage target shield ({shield_before} -> {shield_after})"
    );
    assert!(
        sys.get_combat_state(999_999_999).await.unwrap().is_none(),
        "missing ball has no combat state"
    );
    if perf_gate_enabled() {
        assert!(
            avg_ms < 0.05,
            "per-module cycle budget exceeded: {avg_ms:.6} ms (budget < 0.05 ms)"
        );
    }
}

#[tokio::test]
async fn perf_bounty_1000_accruals_under_10ms_single_wallet_write() {
    // parity: feat:combat.bounty-payout-npc
    eve_testkit::attest(&["feat:combat.bounty-payout-npc"]);

    let ctx = new_perf_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(9502);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    let sys_id = 30_000_142_u32;
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

    let bal_before = ctx.db.with_snapshot(|s| s.characters.get(&char_id).unwrap().balance);
    let journal_before = ctx.db.with_snapshot(|s| s.wallet_journal.len());

    let t0 = Instant::now();
    for i in 0..1_000_i64 {
        credit_specialized_npc_bounty_and_sec_status(
            &ctx,
            sys_id,
            700_000_000 + i,
            char_id,
            50.0,
        );
    }
    let payout = flush_npc_bounty_tick(&ctx, sys_id, char_id, 0.0);
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1_000.0;

    let (bal_after, journal_after) = ctx.db.with_snapshot(|s| {
        (
            s.characters.get(&char_id).unwrap().balance,
            s.wallet_journal.len(),
        )
    });
    assert_eq!(
        journal_after - journal_before,
        1,
        "1,000 NPC bounty accruals must flush in exactly 1 wallet write"
    );
    assert_eq!(payout.pilot_isk, 50_000.0);
    assert_eq!(
        bal_after.0 - bal_before.0,
        IskCents::from_f64_isk(50_000.0).0,
        "wallet balance must reflect total 1,000-kill batched bounty"
    );

    // Negative variant: zero-bounty kill writes nothing on flush
    credit_specialized_npc_bounty_and_sec_status(&ctx, sys_id, 700_999_999, char_id, 0.0);
    let zero_payout = flush_npc_bounty_tick(&ctx, sys_id, char_id, 0.0);
    assert_eq!(zero_payout.pilot_isk, 0.0);
    assert_eq!(
        ctx.db.with_snapshot(|s| s.wallet_journal.len()),
        journal_after,
        "zero bounty must not write a wallet journal entry"
    );

    if perf_gate_enabled() {
        assert!(
            elapsed_ms < 10.0,
            "1,000 NPC bounty accruals exceeded budget: {elapsed_ms:.3} ms (budget < 10 ms)"
        );
    }
}

#[tokio::test]
async fn perf_reprocess_quotes_5000_avg_under_200us() {
    // parity: feat:mining.reprocessing-yield
    eve_testkit::attest(&["feat:mining.reprocessing-yield"]);

    let ctx = new_perf_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(9503);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
    let station_id = 60_003_760_i64;
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

    let ore_id = 970_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            ore_id,
            InventoryItemRecord {
                item_id: ore_id,
                type_id: 1230,
                owner_id: char_id,
                location_id: station_id,
                flag: InventoryFlag::Hangar,
                quantity: 10_000,
                singleton: false,
                version: 1,
                ..Default::default()
            },
        );
    });

    let args = [PyRep::Integer(ore_id)];
    let warmup = ctx
        .dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &args, &kw, None)
        .await
        .expect("warmup GetQuotes");
    let PyRep::Tuple(parts) = warmup else {
        panic!("expected 3-tuple from GetQuotes");
    };
    assert_eq!(parts.len(), 3, "GetQuotes must return 3-tuple (tax, eff, quotes)");

    let iterations = 5_000_usize;
    let t0 = Instant::now();
    for _ in 0..iterations {
        let rep = ctx
            .dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &args, &kw, None)
            .await
            .expect("GetQuotes");
        debug_assert!(matches!(rep, PyRep::Tuple(_)));
    }
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1_000.0;
    let avg_ms = elapsed_ms / iterations as f64;

    // Negative variant: undocked GetQuotes must fail with NotDocked
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
        }
    });
    sess.station_id = None;
    sess.structure_id = None;
    let undocked_err = ctx
        .dispatch_rpc(&mut sess, "reprocessingSvc", None, "GetQuotes", &args, &kw, None)
        .await
        .expect_err("undocked GetQuotes must fail");
    assert_eq!(undocked_err.code, "NotDocked");

    if perf_gate_enabled() {
        assert!(
            avg_ms < 0.2,
            "5,000 reprocessing quotes exceeded budget: {avg_ms:.6} ms avg (budget < 0.2 ms)"
        );
    }
}

#[tokio::test]
async fn perf_system_actor_tick_100_ships_200_missiles_under_500us() {
    // parity: feat:serverops.server-tick
    eve_testkit::attest(&["feat:serverops.server-tick"]);

    let ctx = new_perf_ctx();
    let sys = ctx.systems.get_or_spawn(30_000_144);
    let target_ship_id = 980_000_i64;

    for i in 0..100_i64 {
        let bid = 980_000 + i;
        sys.spawn_ball_with_combat(
            Ball {
                id: bid,
                type_id: 587,
                position: DVec3::new((i * 1_000) as f64, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 300.0,
                inertia: 0.5,
                mass: 1_000_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            ctx.build_type_combat_state(587),
        )
        .await
        .expect("spawn ship");
    }

    let missile_start = DVec3::new(10_000.0, 0.0, 0.0);
    for j in 0..200_i64 {
        sys.spawn_ball_with_combat(
            Ball {
                id: 981_000 + j,
                type_id: 209,
                position: missile_start + DVec3::new(0.0, j as f64, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 3_000.0,
                inertia: 0.1,
                mass: 1_000.0,
                radius: 1.0,
                speed_fraction: 1.0,
                mode: MovementMode::Missile {
                    target_id: target_ship_id,
                },
            },
            CombatState::default(),
        )
        .await
        .expect("spawn missile");
    }

    let m0_before = sys.get_ball_snapshot(981_000).await.unwrap().unwrap().1.position;
    let t0 = Instant::now();
    let broadcast = sys.step_manual(1.0).await.expect("step_manual");
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1_000.0;

    let m0_after = sys.get_ball_snapshot(981_000).await.unwrap().unwrap().1.position;
    assert!(broadcast.tick >= 1, "SystemActor tick must advance");
    assert!(
        m0_after.distance(DVec3::ZERO) < m0_before.distance(DVec3::ZERO),
        "missile ball must advance toward target ship"
    );
    assert!(
        sys.get_ball_snapshot(999_999_999).await.unwrap().is_none(),
        "unknown ball snapshot returns None"
    );

    if perf_gate_enabled() {
        assert!(
            elapsed_ms < 0.5,
            "SystemActor tick (100 ships + 200 missiles) exceeded budget: {elapsed_ms:.3} ms (budget < 0.5 ms)"
        );
    }
}

#[tokio::test]
async fn perf_calendar_month_fetch_20000_avg_under_50us() {
    // parity: feat:social.calendar-personal
    eve_testkit::attest(&["feat:social.calendar-personal"]);

    let ctx = new_perf_ctx();
    let kw = HashMap::new();
    let mut sess = SessionState::new(9505);
    common::login_pilot(&ctx, &mut sess, "demo", "demo-secret").await;
    let char_id = 90_000_001_i64;
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

    let _ = ctx
        .dispatch_rpc(
            &mut sess,
            "calendarMgr",
            None,
            "CreatePersonalEvent",
            &[
                PyRep::Integer(135_500_000_000_000_000),
                PyRep::Integer(60),
                PyRep::String("Perf Formup".into()),
                PyRep::String("Ops".into()),
                PyRep::Integer(1),
                PyRep::List(vec![]),
            ],
            &kw,
            None,
        )
        .await
        .expect("CreatePersonalEvent");

    let fetch_args = [PyRep::Integer(5), PyRep::Integer(2030)];
    let warmup = ctx
        .dispatch_rpc(&mut sess, "calendarMgr", None, "GetEventList", &fetch_args, &kw, None)
        .await
        .expect("warmup GetEventList");
    assert!(format!("{warmup:?}").contains("Perf Formup"));

    let iterations = 20_000_usize;
    let t0 = Instant::now();
    for _ in 0..iterations {
        let rep = ctx
            .dispatch_rpc(&mut sess, "calendarMgr", None, "GetEventList", &fetch_args, &kw, None)
            .await
            .expect("GetEventList");
        debug_assert!(matches!(rep, PyRep::ObjectEx { .. }));
    }
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1_000.0;
    let avg_ms = elapsed_ms / iterations as f64;

    // Negative variant: missing eventID returns CalendarEventNotFound
    let missing_err = ctx
        .dispatch_rpc(
            &mut sess,
            "calendarMgr",
            None,
            "GetEventDetails",
            &[PyRep::Integer(999_999_999)],
            &kw,
            None,
        )
        .await
        .expect_err("missing event details must fail");
    assert_eq!(missing_err.code, "CalendarEventNotFound");

    if perf_gate_enabled() {
        assert!(
            avg_ms < 0.05,
            "20,000 calendar month fetches exceeded budget: {avg_ms:.6} ms avg (budget < 0.05 ms)"
        );
    }
}
