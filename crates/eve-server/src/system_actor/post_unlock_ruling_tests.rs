//! Mandatory `#1170` Track 1A ordering, serialization, worker-reentrancy, and lock-awareness tests.

use super::{
    NaniteRepairTickDbOp, PostUnlockIntent, PostUnlockMsg, PostUnlockOps, PostUnlockWorkItem,
};
use crate::router::ServerContext;
use crate::system_actor::{ActiveModuleCycle, ActiveNaniteRepair, TryRepairCombatOutcome};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn test_ctx() -> ServerContext {
    if let Some(sde) = SHARED_SDE.get() {
        ServerContext::with_sde_and_repository(Arc::clone(sde), Arc::new(WorldRepository::with_demo_seed()))
            .with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED)
    } else {
        let ctx = ServerContext::new();
        let _ = SHARED_SDE.set(Arc::clone(&ctx.sde));
        ctx
    }
}

fn make_item(
    item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32,
) -> InventoryItemRecord {
    InventoryItemRecord {
        item_id, type_id, owner_id, location_id, flag, quantity,
        singleton: quantity == 1 && flag != InventoryFlag::Cargo,
        version: 1, dynamic_attributes: HashMap::new(), source_mutaplasmid_id: None,
        insured_payout_isk: 0.0, is_blueprint_copy: false, blueprint_runs_remaining: None,
    }
}

fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball {
        id, type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0,
        inertia: 2.0, mass: 1_000_000.0, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop,
    }
}

fn make_combat_state(shield: f64, armor: f64, hull: f64, cap: f64) -> CombatState {
    CombatState {
        shield_hp: shield, max_shield_hp: shield, armor_hp: armor, max_armor_hp: armor,
        structure_hp: hull, max_structure_hp: hull, capacitor: cap, max_capacitor: cap,
        ..CombatState::default()
    }
}

fn hold_db_writer(db: &Arc<WorldRepository>) -> (std::sync::mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (held_tx, held_rx) = std::sync::mpsc::channel();
    let (rel_tx, rel_rx) = std::sync::mpsc::channel();
    let db_hold = Arc::clone(db);
    let h = std::thread::spawn(move || {
        db_hold.with_mut_snapshot(|_| {
            held_tx.send(()).unwrap();
            let _ = rel_rx.recv();
        });
    });
    held_rx.recv().unwrap();
    (rel_tx, h)
}

async fn wait_until(msg: &str, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
    while !cond() {
        assert!(tokio::time::Instant::now() < deadline, "{msg}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
}

/// Ruling Test 1 (`W1`, `W2`): Two producers with DB writer held:
/// Direction A (`actor p1 -> helper p2 => DB p2`) and mirrored Direction B (`helper p3 -> actor p4 => DB p4`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ruling_w1_w2_two_producers_actor_and_helper_preserve_order_in_both_directions() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (char_id, ship_id) = (90_000_001_i64, 778_001_001_i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, make_item(ship_id, 606, char_id, i64::from(sys_id), InventoryFlag::None, 1));
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.active_ship_id = ship_id;
            ch.solar_system_id = sys_id;
            ch.station_id = None;
            ch.structure_id = None;
        }
    });

    let (p0, p1, p2, p3, p4) = (
        DVec3::new(10.0, 0.0, 0.0), DVec3::new(100.0, 0.0, 0.0), DVec3::new(200.0, 0.0, 0.0),
        DVec3::new(300.0, 0.0, 0.0), DVec3::new(400.0, 0.0, 0.0),
    );
    sys.spawn_ball(make_ball(ship_id, 606, p0)).await.unwrap();

    // Direction A: actor `set_ball_position(p1)` then helper `try_set_ball_position(p2)` -> DB ends with `p2`.
    let (rel_a, writer_a) = hold_db_writer(&ctx.db);
    let fut_p1 = tokio::spawn({ let s = sys.clone(); async move { s.set_ball_position(ship_id, p1).await } });
    wait_until("p1 never reached memory", || sys.get_ball_position_sync(ship_id) == Some(p1)).await;
    let helper_p2 = std::thread::spawn({ let s = sys.clone(); move || s.try_set_ball_position(ship_id, p2) });
    wait_until("p2 never reached memory", || sys.get_ball_position_sync(ship_id) == Some(p2)).await;
    assert!(!fut_p1.is_finished(), "W4 reply must still be waiting on DB writer");
    assert!(!helper_p2.is_finished(), "W3 unlocked helper must wait on DB writer");
    let _ = rel_a.send(());
    writer_a.join().unwrap();
    assert!(fut_p1.await.unwrap());
    helper_p2.join().unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()), Some([p2.x, p2.y, p2.z]));

    // Direction B (mirrored): helper `try_set_ball_position(p3)` then actor `set_ball_position(p4)` -> DB ends with `p4`.
    let (rel_b, writer_b) = hold_db_writer(&ctx.db);
    let helper_p3 = std::thread::spawn({ let s = sys.clone(); move || s.try_set_ball_position(ship_id, p3) });
    wait_until("p3 never reached memory", || sys.get_ball_position_sync(ship_id) == Some(p3)).await;
    let fut_p4 = tokio::spawn({ let s = sys.clone(); async move { s.set_ball_position(ship_id, p4).await } });
    wait_until("p4 never reached memory", || sys.get_ball_position_sync(ship_id) == Some(p4)).await;
    assert!(!helper_p3.is_finished(), "W3 helper must still wait on DB writer");
    assert!(!fut_p4.is_finished(), "W4 actor reply must still wait on DB writer");
    let _ = rel_b.send(());
    writer_b.join().unwrap();
    helper_p3.join().unwrap();
    assert!(fut_p4.await.unwrap());
    assert_eq!(ctx.db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()), Some([p4.x, p4.y, p4.z]));
}

/// Ruling Test 2 (`W5`): Nanite-repair cancel queued behind an earlier write to the same module
/// (`NaniteRepairTick`) is applied once and strictly after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ruling_w5_nanite_repair_cancel_queued_behind_earlier_tick_applies_once_and_after_it() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (char_id, ship_id, mod_id, paste_id) = (90_000_001_i64, 778_002_001_i64, 778_002_002_i64, 778_002_003_i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, make_item(ship_id, 606, char_id, i64::from(sys_id), InventoryFlag::None, 1));
        let mut mod_item = make_item(mod_id, 2048, char_id, ship_id, InventoryFlag::LoSlot0, 1);
        mod_item.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 120.0);
        snap.items.insert(mod_id, mod_item);
        snap.items.insert(paste_id, make_item(paste_id, 28668, char_id, ship_id, InventoryFlag::Cargo, 10));
    });
    sys.spawn_ball(make_ball(ship_id, 606, DVec3::ZERO)).await.unwrap();
    sys.actor_state.write().unwrap().active_nanite_repairs.push(ActiveNaniteRepair {
        ship_id, char_id, module_id: mod_id, initial_damage_hp: 100.0, duration_s: 10.0,
        elapsed_s: 6.0, paste_total_needed: 10, paste_consumed_so_far: 10,
    });

    let (rel_w, writer) = hold_db_writer(&ctx.db);
    // Queue earlier NaniteRepairTick (`10 - 6 = 4` paste, `DAMAGE = 95.0`) ahead of cancel (`+4 = 8` paste, `DAMAGE = 40.0`).
    let mut earlier_ops = PostUnlockOps::default();
    earlier_ops.push(PostUnlockIntent::NaniteRepairTick(NaniteRepairTickDbOp {
        char_id, ship_id, module_id: mod_id, paste_delta: 6, rem_dmg: 95.0,
    }));
    sys.post_unlock_tx.send(PostUnlockMsg::Work(PostUnlockWorkItem {
        db_opt: Some(Arc::clone(&ctx.db)), sde_opt: Some(Arc::clone(&ctx.sde)),
        ops: earlier_ops, replies: Vec::new(),
    })).unwrap();

    let cancel_thread = std::thread::spawn({ let s = sys.clone(); move || s.cancel_ship_nanite_repairs_sync(ship_id, true) });
    wait_until("cancel never drained actor state", || sys.actor_state.read().unwrap().active_nanite_repairs.is_empty()).await;
    let _ = rel_w.send(());
    writer.join().unwrap();
    cancel_thread.join().unwrap();

    let (dmg, paste_qty) = ctx.db.with_snapshot(|snap| {
        let d = snap.items.get(&mod_id).and_then(|it| it.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied());
        let p: i32 = snap.items.values().filter(|it| it.location_id == ship_id && it.flag == InventoryFlag::Cargo && it.type_id == 28668).map(|it| it.quantity).sum();
        (d, p)
    });
    assert!(dmg.is_some_and(|v| (v - 40.0).abs() < 1e-6), "got {dmg:?}");
    assert_eq!(paste_qty, 8, "cancel must apply once (10 - 6 + 4 = 8 paste)");

    sys.cancel_ship_nanite_repairs_sync(ship_id, true);
    let paste_repeat: i32 = ctx.db.with_snapshot(|snap| {
        snap.items.values().filter(|it| it.location_id == ship_id && it.flag == InventoryFlag::Cargo && it.type_id == 28668).map(|it| it.quantity).sum()
    });
    assert_eq!(paste_repeat, 8);
}

/// Ruling Test 3 (`W3`, `W5`): A `module_deactivation_hook` writing to its own system
/// (`try_set_ball_position`, `try_repair_combat_state_sync`) does NOT deadlock and lands strictly after prior writes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ruling_w3_w5_deactivation_hook_helper_for_own_system_no_deadlock_and_runs_strictly_after() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (char_id, ship_a, mod_a, ship_b) = (90_000_001_i64, 778_003_001_i64, 778_003_002_i64, 778_003_003_i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_a, make_item(ship_a, 606, char_id, i64::from(sys_id), InventoryFlag::None, 1));
        snap.items.insert(mod_a, make_item(mod_a, 11578, char_id, ship_a, InventoryFlag::HiSlot0, 1));
        snap.items.insert(ship_b, make_item(ship_b, 606, char_id, i64::from(sys_id), InventoryFlag::None, 1));
        snap.module_active_states.insert(mod_a);
    });

    let (p_init, p_before_hook, p_in_hook, p_barrier) = (
        DVec3::new(10.0, 0.0, 0.0), DVec3::new(111.0, 0.0, 0.0),
        DVec3::new(222.0, 0.0, 0.0), DVec3::new(333.0, 0.0, 0.0),
    );
    sys.spawn_ball(make_ball(ship_a, 606, p_init)).await.unwrap();
    let mut cs_b = make_combat_state(500.0, 500.0, 500.0, 200.0);
    cs_b.shield_hp = 100.0;
    sys.spawn_ball_with_combat(make_ball(ship_b, 606, p_init), cs_b).await.unwrap();
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id: ship_a, owner_char_id: Some(char_id), module_id: mod_a, module_type_id: 11578,
        effect_name: "cloaking".into(), duration_s: 10.0, repeat: 1, ..ActiveModuleCycle::default()
    }).await.unwrap();

    let (hook_tx, hook_rx) = std::sync::mpsc::channel();
    let (db_hook, ctx_w) = (Arc::clone(&ctx.db), ctx.downgrade());
    *sys.module_deactivation_hook.write().unwrap() = Some(Arc::new(move |sid, _cid, _cyc| {
        let Some(c) = ctx_w.upgrade() else { return; };
        let sys_hook = c.systems.get_or_spawn(sid);
        let mod_active = db_hook.with_snapshot(|s| s.module_active_states.contains(&mod_a));
        let pos_when_ran = db_hook.with_snapshot(|s| s.item_positions.get(&ship_b).copied());
        sys_hook.try_set_ball_position(ship_b, p_in_hook);
        let rep = sys_hook.try_repair_combat_state_sync(ship_b, 0.0, 0.0, 75.0);
        let pos_after_enq = db_hook.with_snapshot(|s| s.item_positions.get(&ship_b).copied());
        let _ = hook_tx.send((mod_active, pos_when_ran, rep, pos_after_enq));
    }));

    let (rel_w, writer) = hold_db_writer(&ctx.db);
    let fut_before = tokio::spawn({ let s = sys.clone(); async move { s.set_ball_position(ship_b, p_before_hook).await } });
    wait_until("p_before_hook never reached memory", || sys.get_ball_position_sync(ship_b) == Some(p_before_hook)).await;
    let fut_rem = tokio::spawn({ let s = sys.clone(); async move { s.stop_non_warp_module_cycles(ship_a).await } });
    wait_until("cycle never removed", || sys.actor_state.read().unwrap().active_module_cycles.is_empty()).await;
    let _ = rel_w.send(());
    writer.join().unwrap();
    assert!(fut_before.await.unwrap());
    fut_rem.await.unwrap().unwrap();

    let (mod_active, pos_when_ran, rep, pos_after_enq) =
        hook_rx.recv_timeout(Duration::from_secs(2)).expect("hook must run without deadlock");
    assert!(!mod_active, "module cleanup must commit BEFORE deactivation hook");
    assert_eq!(pos_when_ran, Some([p_before_hook.x, p_before_hook.y, p_before_hook.z]));
    assert_eq!(rep, TryRepairCombatOutcome::Applied);
    assert_eq!(pos_after_enq, Some([p_before_hook.x, p_before_hook.y, p_before_hook.z]), "W3: worker thread enqueues behind itself");

    sys.try_set_ball_position(ship_a, p_barrier);
    let (pos_final, hp_final) = ctx.db.with_snapshot(|s| {
        (s.item_positions.get(&ship_b).copied(), s.sp6_lifecycle.ship_combat_hp.get(&ship_b).copied())
    });
    assert_eq!(pos_final, Some([p_in_hook.x, p_in_hook.y, p_in_hook.z]));
    assert!(hp_final.is_some_and(|hp| (hp.shield_hp - 175.0).abs() < 1e-6), "{hp_final:?}");
}

/// Ruling Test 4 (`W3`): Unlocked helper returns with write committed in DB;
/// caller holding `with_mut_snapshot` returns immediately and commits after release.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ruling_w3_unlocked_helper_waits_for_db_while_db_writer_holder_returns_immediately() {
    let ctx = test_ctx();
    let sys_id = 30_000_142_u32;
    let sys = ctx.systems.get_or_spawn(sys_id);
    let (char_id, ship_id, mod_a, mod_b, paste_id) = (
        90_000_001_i64, 778_004_001_i64, 778_004_002_i64, 778_004_003_i64, 778_004_004_i64,
    );

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, make_item(ship_id, 606, char_id, i64::from(sys_id), InventoryFlag::None, 1));
        for (mid, dmg) in [(mod_a, 100.0), (mod_b, 90.0)] {
            let mut m = make_item(mid, 2048, char_id, ship_id, InventoryFlag::LoSlot0, 1);
            m.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, dmg);
            snap.items.insert(mid, m);
        }
        snap.items.insert(paste_id, make_item(paste_id, 28668, char_id, ship_id, InventoryFlag::Cargo, 4));
    });
    let (p_init, p_unlocked) = (DVec3::new(10.0, 0.0, 0.0), DVec3::new(500.0, 0.0, 0.0));
    let mut cs = make_combat_state(500.0, 500.0, 500.0, 200.0);
    cs.shield_hp = 100.0;
    sys.spawn_ball_with_combat(make_ball(ship_id, 606, p_init), cs).await.unwrap();

    // Part A: Unlocked caller waits for commit and sees writes immediately on return (`0ms` polling).
    sys.actor_state.write().unwrap().active_nanite_repairs.push(ActiveNaniteRepair {
        ship_id, char_id, module_id: mod_a, initial_damage_hp: 50.0, duration_s: 10.0,
        elapsed_s: 0.0, paste_total_needed: 4, paste_consumed_so_far: 4,
    });
    sys.try_set_ball_position(ship_id, p_unlocked);
    assert_eq!(sys.try_repair_combat_state_sync(ship_id, 0.0, 0.0, 150.0), TryRepairCombatOutcome::Applied);
    sys.cancel_ship_nanite_repairs_sync(ship_id, true);
    let (pos_a, hp_a, dmg_a) = ctx.db.with_snapshot(|s| (
        s.item_positions.get(&ship_id).copied(),
        s.sp6_lifecycle.ship_combat_hp.get(&ship_id).copied(),
        s.items.get(&mod_a).and_then(|it| it.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied()),
    ));
    assert_eq!(pos_a, Some([p_unlocked.x, p_unlocked.y, p_unlocked.z]));
    assert!(hp_a.is_some_and(|hp| (hp.shield_hp - 250.0).abs() < 1e-6));
    assert!(dmg_a.is_some_and(|d| (d - 50.0).abs() < 1e-6), "got {dmg_a:?}");

    // Part B: Caller holding `RANK_WORLD_INNER` (`with_mut_snapshot`) returns immediately and commits after release.
    sys.actor_state.write().unwrap().active_nanite_repairs.push(ActiveNaniteRepair {
        ship_id, char_id, module_id: mod_b, initial_damage_hp: 30.0, duration_s: 10.0,
        elapsed_s: 0.0, paste_total_needed: 2, paste_consumed_so_far: 2,
    });
    let inside_elapsed = ctx.db.with_mut_snapshot(|snap| {
        let t0 = std::time::Instant::now();
        sys.cancel_ship_nanite_repairs_sync(ship_id, true);
        let pre_dmg = snap.items.get(&mod_b).and_then(|it| it.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied());
        assert!(pre_dmg.is_some_and(|d| (d - 90.0).abs() < 1e-6), "must still see pre-release DB DAMAGE: {pre_dmg:?}");
        t0.elapsed()
    });
    assert!(inside_elapsed < Duration::from_millis(250), "elapsed={inside_elapsed:?}");

    wait_until("writes queued under held DB writer never committed after release", || {
        ctx.db.with_snapshot(|s| {
            s.items.get(&mod_b).and_then(|it| it.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied())
                .is_some_and(|d| (d - 30.0).abs() < 1e-6)
        })
    }).await;
}
