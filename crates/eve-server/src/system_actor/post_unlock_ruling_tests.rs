//! Mandatory `#1170` Track 1A ordering, serialization, worker-reentrancy, and lock-awareness tests.

use super::{NaniteRepairTickDbOp, PostUnlockIntent, PostUnlockMsg, PostUnlockOps, PostUnlockWorkItem};
use crate::router::ServerContext;
use crate::system_actor::{ActiveModuleCycle, ActiveNaniteRepair, SystemHandle, TryRepairCombatOutcome};
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::HashMap;
use std::sync::{mpsc, Arc, OnceLock};
use std::time::Duration;

const SYS_ID: u32 = 30_000_142;
const CHAR_ID: i64 = 90_000_001;
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
fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32) -> InventoryItemRecord {
    InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton: quantity == 1 && flag != InventoryFlag::Cargo, version: 1, ..Default::default() }
}
fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball {
    Ball { id, type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 2.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop }
}
fn make_combat_state(shield: f64, armor: f64, hull: f64, cap: f64) -> CombatState {
    CombatState { shield_hp: shield, max_shield_hp: shield, armor_hp: armor, max_armor_hp: armor, structure_hp: hull, max_structure_hp: hull, capacitor: cap, max_capacitor: cap, ..CombatState::default() }
}
fn hold_db_writer(db: &Arc<WorldRepository>) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let ((held_tx, held_rx), (rel_tx, rel_rx), db_hold) = (mpsc::channel(), mpsc::channel(), Arc::clone(db));
    let h = std::thread::spawn(move || { db_hold.with_mut_snapshot(|_| { held_tx.send(()).unwrap(); let _ = rel_rx.recv(); }); });
    held_rx.recv().unwrap();
    (rel_tx, h)
}
fn release_writer((rel, writer): (mpsc::Sender<()>, std::thread::JoinHandle<()>)) { let _ = rel.send(()); writer.join().unwrap(); }
async fn wait_until(msg: &str, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !cond() { assert!(tokio::time::Instant::now() < deadline, "{msg}"); tokio::time::sleep(Duration::from_millis(2)).await; }
}
fn seed_ship_char(db: &WorldRepository, sys_id: u32, char_id: i64, ship_id: i64, extra_mods: &[(i64, u32, InventoryFlag, f64)], paste: Option<(i64, i32)>) {
    db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_id, make_item(ship_id, 606, char_id, i64::from(sys_id), InventoryFlag::None, 1));
        for &(mid, tid, flag, dmg) in extra_mods {
            let mut m = make_item(mid, tid, char_id, ship_id, flag, 1);
            if dmg > 0.0 { m.dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, dmg); }
            snap.items.insert(mid, m);
        }
        if let Some((pid, qty)) = paste { snap.items.insert(pid, make_item(pid, 28668, char_id, ship_id, InventoryFlag::Cargo, qty)); }
        if let Some(ch) = snap.characters.get_mut(&char_id) { (ch.active_ship_id, ch.solar_system_id, ch.station_id, ch.structure_id) = (ship_id, sys_id, None, None); }
    });
}
async fn boot_ship(ship_id: i64, extra_mods: &[(i64, u32, InventoryFlag, f64)], paste: Option<(i64, i32)>, cs: Option<CombatState>) -> (ServerContext, SystemHandle) {
    let ctx = test_ctx();
    let sys = ctx.systems.get_or_spawn(SYS_ID);
    seed_ship_char(&ctx.db, SYS_ID, CHAR_ID, ship_id, extra_mods, paste);
    if let Some(combat) = cs { sys.spawn_ball_with_combat(make_ball(ship_id, 606, DVec3::ZERO), combat).await.unwrap(); }
    else { sys.spawn_ball(make_ball(ship_id, 606, DVec3::ZERO)).await.unwrap(); }
    (ctx, sys)
}
fn work_item(db_opt: Option<Arc<WorldRepository>>, sde_opt: Option<Arc<SdeRegistry>>, ops: PostUnlockOps) -> PostUnlockWorkItem {
    PostUnlockWorkItem { db_opt, sde_opt, ops, replies: Vec::new(), in_flight_guard: None, work_seq: 0 }
}
fn send_post_ops(sys: &SystemHandle, ctx: &ServerContext, ops: PostUnlockOps) {
    sys.post_unlock_tx.send(PostUnlockMsg::Work(work_item(Some(Arc::clone(&ctx.db)), Some(Arc::clone(&ctx.sde)), ops))).unwrap();
}
fn send_in_flight(sys: &SystemHandle, ctx: &ServerContext, ops: PostUnlockOps) {
    let _ = super::send_post_unlock_item(&sys.post_unlock_tx, &sys.post_unlock_in_flight, work_item(Some(Arc::clone(&ctx.db)), Some(Arc::clone(&ctx.sde)), ops));
}
fn spawn_pos(sys: &SystemHandle, ship_id: i64, pos: DVec3) -> tokio::task::JoinHandle<bool> {
    let s = sys.clone(); tokio::spawn(async move { s.set_ball_position(ship_id, pos).await })
}
fn mod_dmg(db: &WorldRepository, mid: i64) -> Option<f64> { db.with_snapshot(|s| s.items.get(&mid).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied())) }
fn paste_qty(db: &WorldRepository, sid: i64) -> i32 { db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == sid && i.flag == InventoryFlag::Cargo && i.type_id == 28668).map(|i| i.quantity).sum()) }
fn shortfall_of(db: &WorldRepository, sid: i64, mid: i64) -> Option<i32> { db.with_snapshot(|s| s.sp6_lifecycle.nanite_paste_shortfall.get(&(sid, mid)).copied()) }
fn push_nanite(sys: &SystemHandle, ship_id: i64, module_id: i64, initial_damage_hp: f64, elapsed_s: f64, paste_total_needed: i32, paste_consumed_so_far: i32) {
    sys.actor_state.write().unwrap().active_nanite_repairs.push(ActiveNaniteRepair {
        ship_id, char_id: CHAR_ID, module_id, initial_damage_hp, duration_s: 10.0, elapsed_s, paste_total_needed, paste_consumed_so_far,
    });
}
fn cloak_cyc(ship_id: i64, module_id: i64, duration_s: f64, elapsed_s: f64, repeat: i32) -> ActiveModuleCycle {
    ActiveModuleCycle { ship_id, owner_char_id: Some(CHAR_ID), module_id, module_type_id: 11578, effect_name: "cloaking".into(), duration_s, elapsed_s, repeat, ..Default::default() }
}
fn tick_op(ship_id: i64, module_id: i64, paste_delta: i32, initial_damage_hp: f64, paste_total_needed: i32, rem_dmg: f64, done_now: bool) -> PostUnlockIntent {
    PostUnlockIntent::NaniteRepairTick(NaniteRepairTickDbOp { char_id: CHAR_ID, ship_id, module_id, paste_delta, initial_damage_hp, paste_total_needed, rem_dmg, done_now, halted_for_paste: false })
}
fn cancel_op(ship_id: i64, module_id: i64, rem_dmg: f64, refund_paste: bool, refund: i32) -> PostUnlockIntent {
    PostUnlockIntent::NaniteRepairCancel(super::NaniteRepairCancelDbOp { char_id: CHAR_ID, ship_id, module_id, rem_dmg, refund_paste, refund })
}

/// Ruling Test 1 (`W1`, `W2`): Two producers with DB writer held preserve FIFO in both directions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ruling_w1_w2_two_producers_actor_and_helper_preserve_order_in_both_directions() {
    let (ship_id, (ctx, sys)) = (778_001_001_i64, boot_ship(778_001_001, &[], None, None).await);
    let (p1, p2, p3, p4) = (DVec3::new(100.0, 0.0, 0.0), DVec3::new(200.0, 0.0, 0.0), DVec3::new(300.0, 0.0, 0.0), DVec3::new(400.0, 0.0, 0.0));

    let held_a = hold_db_writer(&ctx.db);
    let fut_p1 = spawn_pos(&sys, ship_id, p1);
    wait_until("p1 never reached memory", || sys.get_ball_position_sync(ship_id) == Some(p1)).await;
    let helper_p2 = std::thread::spawn({ let s = sys.clone(); move || s.try_set_ball_position(ship_id, p2) });
    wait_until("p2 never reached memory", || sys.get_ball_position_sync(ship_id) == Some(p2)).await;
    assert!(!fut_p1.is_finished() && !helper_p2.is_finished());
    release_writer(held_a);
    assert!(fut_p1.await.unwrap()); helper_p2.join().unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()), Some([p2.x, p2.y, p2.z]));

    let held_b = hold_db_writer(&ctx.db);
    let helper_p3 = std::thread::spawn({ let s = sys.clone(); move || s.try_set_ball_position(ship_id, p3) });
    wait_until("p3 never reached memory", || sys.get_ball_position_sync(ship_id) == Some(p3)).await;
    let fut_p4 = spawn_pos(&sys, ship_id, p4);
    wait_until("p4 never reached memory", || sys.get_ball_position_sync(ship_id) == Some(p4)).await;
    assert!(!helper_p3.is_finished() && !fut_p4.is_finished());
    release_writer(held_b);
    helper_p3.join().unwrap(); assert!(fut_p4.await.unwrap());
    assert_eq!(ctx.db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()), Some([p4.x, p4.y, p4.z]));
}

/// Ruling Test 2 (`W5`): Nanite-repair cancel queued behind an earlier write to the same module applies once and strictly after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ruling_w5_nanite_repair_cancel_queued_behind_earlier_tick_applies_once_and_after_it() {
    let (ship_id, mod_id, paste_id) = (778_002_001_i64, 778_002_002_i64, 778_002_003_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_id, 2048, InventoryFlag::LoSlot0, 120.0)], Some((paste_id, 10)), None).await;
    push_nanite(&sys, ship_id, mod_id, 100.0, 6.0, 10, 10);

    let held = hold_db_writer(&ctx.db);
    let mut earlier_ops = PostUnlockOps::default();
    earlier_ops.push(tick_op(ship_id, mod_id, 6, 100.0, 10, 95.0, false));
    send_post_ops(&sys, &ctx, earlier_ops);
    let cancel_thread = std::thread::spawn({ let s = sys.clone(); move || s.cancel_ship_nanite_repairs_sync(ship_id, true) });
    wait_until("cancel never drained actor state", || sys.actor_state.read().unwrap().active_nanite_repairs.is_empty()).await;
    release_writer(held); cancel_thread.join().unwrap();

    assert!(mod_dmg(&ctx.db, mod_id).is_some_and(|v| (v - 40.0).abs() < 1e-6));
    assert_eq!(paste_qty(&ctx.db, ship_id), 8);
    sys.cancel_ship_nanite_repairs_sync(ship_id, true);
    assert_eq!(paste_qty(&ctx.db, ship_id), 8);
}

/// Ruling Test 3 (`W3`, `W5`): `module_deactivation_hook` writing to its own system does NOT deadlock and lands strictly after prior writes.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ruling_w3_w5_deactivation_hook_helper_for_own_system_no_deadlock_and_runs_strictly_after() {
    let (ship_a, mod_a, ship_b) = (778_003_001_i64, 778_003_002_i64, 778_003_003_i64);
    let (ctx, sys) = boot_ship(ship_a, &[(mod_a, 11578, InventoryFlag::HiSlot0, 0.0)], None, None).await;
    ctx.db.with_mut_snapshot(|s| { s.items.insert(ship_b, make_item(ship_b, 606, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1)); s.module_active_states.insert(mod_a); });
    let (p_init, p_before, p_in_hook, p_barrier) = (DVec3::new(10.0, 0.0, 0.0), DVec3::new(111.0, 0.0, 0.0), DVec3::new(222.0, 0.0, 0.0), DVec3::new(333.0, 0.0, 0.0));
    let mut cs_b = make_combat_state(500.0, 500.0, 500.0, 200.0); cs_b.shield_hp = 100.0;
    sys.spawn_ball_with_combat(make_ball(ship_b, 606, p_init), cs_b).await.unwrap();
    sys.add_module_cycle(cloak_cyc(ship_a, mod_a, 10.0, 0.0, 1)).await.unwrap();

    let ((hook_tx, hook_rx), db_hook, ctx_w) = (mpsc::channel(), Arc::clone(&ctx.db), ctx.downgrade());
    *sys.module_deactivation_hook.write().unwrap() = Some(Arc::new(move |sid, _cid, _cyc| {
        let Some(c) = ctx_w.upgrade() else { return; };
        let sh = c.systems.get_or_spawn(sid);
        let mod_active = db_hook.with_snapshot(|s| s.module_active_states.contains(&mod_a));
        let pos_when = db_hook.with_snapshot(|s| s.item_positions.get(&ship_b).copied());
        sh.try_set_ball_position(ship_b, p_in_hook);
        let rep = sh.try_repair_combat_state_sync(ship_b, 0.0, 0.0, 75.0);
        let pos_after = db_hook.with_snapshot(|s| s.item_positions.get(&ship_b).copied());
        let _ = hook_tx.send((mod_active, pos_when, rep, pos_after));
    }));

    let held = hold_db_writer(&ctx.db);
    let fut_before = spawn_pos(&sys, ship_b, p_before);
    wait_until("p_before never reached memory", || sys.get_ball_position_sync(ship_b) == Some(p_before)).await;
    let fut_rem = tokio::spawn({ let s = sys.clone(); async move { s.stop_non_warp_module_cycles(ship_a).await } });
    wait_until("cycle never removed", || sys.actor_state.read().unwrap().active_module_cycles.is_empty()).await;
    release_writer(held);
    assert!(fut_before.await.unwrap()); fut_rem.await.unwrap().unwrap();

    let (mod_active, pos_when, rep, pos_after) = hook_rx.recv_timeout(Duration::from_secs(2)).expect("hook must run without deadlock");
    assert!(!mod_active && rep == TryRepairCombatOutcome::Applied);
    assert_eq!(pos_when, Some([p_before.x, p_before.y, p_before.z]));
    assert_eq!(pos_after, Some([p_before.x, p_before.y, p_before.z]), "W3: worker thread enqueues behind itself");
    sys.try_set_ball_position(ship_a, p_barrier);
    let (pos_final, hp_final) = ctx.db.with_snapshot(|s| (s.item_positions.get(&ship_b).copied(), s.sp6_lifecycle.ship_combat_hp.get(&ship_b).copied()));
    assert_eq!(pos_final, Some([p_in_hook.x, p_in_hook.y, p_in_hook.z]));
    assert!(hp_final.is_some_and(|hp| (hp.shield_hp - 175.0).abs() < 1e-6));
}

/// Ruling Test 4 (`W3`): Unlocked helper commits before return; DB-writer holder returns immediately and commits after release.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ruling_w3_unlocked_helper_waits_for_db_while_db_writer_holder_returns_immediately() {
    let (ship_id, mod_a, mod_b, paste_id) = (778_004_001_i64, 778_004_002_i64, 778_004_003_i64, 778_004_004_i64);
    let mut cs = make_combat_state(500.0, 500.0, 500.0, 200.0); cs.shield_hp = 100.0;
    let (ctx, sys) = boot_ship(ship_id, &[(mod_a, 2048, InventoryFlag::LoSlot0, 100.0), (mod_b, 2048, InventoryFlag::LoSlot0, 90.0)], Some((paste_id, 4)), Some(cs)).await;
    let p_unlocked = DVec3::new(500.0, 0.0, 0.0);

    push_nanite(&sys, ship_id, mod_a, 50.0, 0.0, 4, 4);
    sys.try_set_ball_position(ship_id, p_unlocked);
    assert_eq!(sys.try_repair_combat_state_sync(ship_id, 0.0, 0.0, 150.0), TryRepairCombatOutcome::Applied);
    sys.cancel_ship_nanite_repairs_sync(ship_id, true);
    let (pos_a, hp_a) = ctx.db.with_snapshot(|s| (s.item_positions.get(&ship_id).copied(), s.sp6_lifecycle.ship_combat_hp.get(&ship_id).copied()));
    assert_eq!(pos_a, Some([p_unlocked.x, p_unlocked.y, p_unlocked.z]));
    assert!(hp_a.is_some_and(|hp| (hp.shield_hp - 250.0).abs() < 1e-6));
    assert!(mod_dmg(&ctx.db, mod_a).is_some_and(|d| (d - 50.0).abs() < 1e-6));

    push_nanite(&sys, ship_id, mod_b, 30.0, 0.0, 2, 2);
    let inside_elapsed = ctx.db.with_mut_snapshot(|snap| {
        let t0 = std::time::Instant::now();
        sys.cancel_ship_nanite_repairs_sync(ship_id, true);
        let pre = snap.items.get(&mod_b).and_then(|it| it.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied());
        assert!(pre.is_some_and(|d| (d - 90.0).abs() < 1e-6));
        t0.elapsed()
    });
    assert!(inside_elapsed < Duration::from_millis(250));
    wait_until("writes queued under held DB writer never committed", || mod_dmg(&ctx.db, mod_b).is_some_and(|d| (d - 30.0).abs() < 1e-6)).await;
}

/// `BF-2`: `fire_weapon_cycle_volley` waits for `post_unlock` barrier before `commit_destroyed_ball_hooks`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf2_fire_weapon_cycle_volley_waits_for_post_unlock_before_death_hooks() {
    let (a_ship, v_char, v_ship, mod_id) = (778_201_001_i64, 90_000_002_i64, 778_201_002_i64, 778_201_101_i64);
    let (ctx, sys) = boot_ship(a_ship, &[], None, None).await;
    let (p_atk, p_vic, p_prior) = (DVec3::ZERO, DVec3::new(500.0, 0.0, 0.0), DVec3::new(123.0, 0.0, 0.0));
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&CHAR_ID].clone(); (ch.character_id, ch.user_id) = (v_char, v_char as u64); snap.characters.entry(v_char).or_insert(ch);
        for (cid, sid, pos) in [(CHAR_ID, a_ship, p_atk), (v_char, v_ship, p_vic)] {
            snap.items.insert(sid, make_item(sid, 587, cid, i64::from(SYS_ID), InventoryFlag::None, 1));
            snap.item_positions.insert(sid, [pos.x, pos.y, pos.z]);
            if let Some(c) = snap.characters.get_mut(&cid) { (c.active_ship_id, c.solar_system_id, c.station_id, c.structure_id) = (sid, SYS_ID, None, None); }
        }
    });
    for (cid, sid, pos) in [(CHAR_ID, a_ship, p_atk), (v_char, v_ship, p_vic)] {
        sys.try_spawn_ball_with_meta(make_ball(sid, 587, pos), ctx.build_ship_combat_state(cid, sid, 587), ctx.build_ship_slim_meta(cid, sid, 587));
    }
    sys.add_module_cycle(ActiveModuleCycle {
        ship_id: a_ship, module_id: mod_id, module_type_id: 486, owner_char_id: Some(CHAR_ID),
        effect_name: "projectileFired".into(), target_id: Some(v_ship), duration_s: 5.0,
        damage_vector: eve_destiny::DamageVector::new(100_000.0, 0.0, 0.0, 0.0), ..Default::default()
    }).await.unwrap();

    let ((hook_tx, hook_rx), db_hook) = (mpsc::channel(), Arc::clone(&ctx.db));
    *sys.npc_death_hook.write().unwrap() = Some(Arc::new(move |_sid, dead_id, _| {
        let (db_h, tx) = (Arc::clone(&db_hook), hook_tx.clone());
        Box::pin(async move { let _ = tx.send((dead_id, db_h.with_snapshot(|s| s.item_positions.get(&a_ship).copied()))); })
    }));

    let held = hold_db_writer(&ctx.db);
    let pos_fut = spawn_pos(&sys, a_ship, p_prior);
    wait_until("prior post_unlock item should be in flight", || sys.has_in_flight_post_unlock()).await;
    let volley_fut = tokio::spawn({ let s = sys.clone(); async move { s.fire_weapon_cycle_volley(a_ship, mod_id).await } });
    wait_until("victim never destroyed in volley", || sys.get_ball_position_sync(v_ship).is_none()).await;
    assert!(hook_rx.try_recv().is_err() && !volley_fut.is_finished());

    release_writer(held);
    assert!(pos_fut.await.unwrap() && volley_fut.await.unwrap().unwrap().0.len() == 1);
    assert_eq!(hook_rx.recv_timeout(Duration::from_secs(2)).unwrap(), (v_ship, Some([p_prior.x, p_prior.y, p_prior.z])));
}

/// `BF-3`: Empty-ops `TakeBall`, empty `FlushConditionCheckpoint`, and `apply_step_output` wait FIFO.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf3_empty_ops_take_ball_flush_checkpoint_and_apply_step_output_wait_fifo() {
    let (ship_a, ship_b, p1) = (778_202_001_i64, 778_202_002_i64, DVec3::new(77.0, 88.0, 99.0));
    let (ctx, sys) = boot_ship(ship_a, &[], None, None).await;
    ctx.db.with_mut_snapshot(|s| { s.items.insert(ship_b, make_item(ship_b, 606, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1)); });
    sys.spawn_ball(make_ball(ship_b, 606, DVec3::new(100.0, 0.0, 0.0))).await.unwrap();
    let _ = sys.flush_condition_checkpoint().await.unwrap();

    let held = hold_db_writer(&ctx.db);
    let pos_fut = spawn_pos(&sys, ship_a, p1);
    wait_until("post_unlock item should be in flight", || sys.has_in_flight_post_unlock()).await;

    let ((tx_take, mut rx_take), (tx_take_a, mut rx_take_a), (tx_ckpt, mut rx_ckpt)) = (tokio::sync::oneshot::channel(), tokio::sync::oneshot::channel(), tokio::sync::oneshot::channel());
    sys.cmd_sender().send(crate::system_actor::SystemCommand::TakeBall { ball_id: ship_b, accept_docking_target: None, preserve_db_active_states: false, reply: tx_take }).await.unwrap();
    sys.cmd_sender().send(crate::system_actor::SystemCommand::TakeBall { ball_id: ship_a, accept_docking_target: None, preserve_db_active_states: true, reply: tx_take_a }).await.unwrap();
    sys.cmd_sender().send(crate::system_actor::SystemCommand::FlushConditionCheckpoint { reply: tx_ckpt }).await.unwrap();
    let step_out = sys.ask_step_tick_only(0.0).await.unwrap();
    assert!(step_out.destroyed_balls.is_empty() && step_out.condition_checkpoint.is_none() && sys.has_in_flight_post_unlock());

    let fut_step = tokio::spawn({ let (s, o) = (sys.clone(), step_out.clone()); async move { s.apply_step_output(&o).await } });
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(rx_take.try_recv().is_err() && rx_take_a.try_recv().is_err() && rx_ckpt.try_recv().is_err() && !fut_step.is_finished());

    release_writer(held);
    assert!(pos_fut.await.unwrap() && rx_take.await.unwrap().is_some() && rx_take_a.await.unwrap().is_some() && rx_ckpt.await.unwrap() == 0);
    fut_step.await.unwrap();
    assert_eq!(ctx.db.with_snapshot(|s| s.item_positions.get(&ship_a).copied()), Some([p1.x, p1.y, p1.z]));
}

async fn pause_post_unlock_worker(sys: &SystemHandle, ship_id: i64, cyc_id: i64) -> (mpsc::Sender<()>, tokio::task::JoinHandle<Result<(), String>>) {
    let ((arr_tx, arr_rx), (go_tx, go_rx)) = (tokio::sync::oneshot::channel(), mpsc::channel());
    let (arr_tx, go_rx) = (std::sync::Mutex::new(Some(arr_tx)), std::sync::Mutex::new(go_rx));
    *sys.module_deactivation_hook.write().unwrap() = Some(Arc::new(move |_, _, _| {
        if let Some(tx) = arr_tx.lock().unwrap().take() { let _ = tx.send(()); }
        let _ = go_rx.lock().unwrap().recv();
    }));
    sys.add_module_cycle(cloak_cyc(ship_id, cyc_id, 10.0, 0.0, 1)).await.unwrap();
    let stop_fut = tokio::spawn({ let s = sys.clone(); async move { s.stop_non_warp_module_cycles(ship_id).await } });
    arr_rx.await.unwrap();
    (go_tx, stop_fut)
}

/// `BF-4` + `BF-1170-1A-01`: Nanite contention defer, `deducted == 0` -> partial/full Tick 2 recovery,
/// `with_snapshot` shortfall visibility, `done_now` cleanup, and zero free repair or paste minting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf4_nanite_repair_paste_conservation_defers_contention_and_prevents_free_repair_or_minting() {
    let (ship_id, mod_a, mod_b, mod_c, cyc_id, paste_id) = (778_203_001_i64, 778_203_002_i64, 778_203_003_i64, 778_203_005_i64, 778_203_099_i64, 778_203_004_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_a, 2048, InventoryFlag::LoSlot0, 100.0), (mod_b, 2048, InventoryFlag::LoSlot0, 90.0), (mod_c, 2048, InventoryFlag::LoSlot0, 50.0)], Some((paste_id, 4)), None).await;
    push_nanite(&sys, ship_id, mod_a, 100.0, 0.0, 10, 0);

    {
        let _pub = ctx.db.hold_published_write_for_test();
        let ((mut virtual_used, mut ops), mut st) = ((HashMap::new(), PostUnlockOps::default()), sys.actor_state.write().unwrap());
        let res = super::collect_nanite_repair_tick(&ctx.db, &mut st.active_nanite_repairs[0], 4.0, 0.4, 4, &mut virtual_used, &mut ops);
        assert!(matches!(res, super::NaniteTickCollect::Defer) && ops.is_empty());
        assert_eq!(st.active_nanite_repairs[0].paste_consumed_so_far, 0);
        assert!(st.active_nanite_repairs[0].elapsed_s.abs() < 1e-9);
    }

    let (go_tx, stop_fut) = pause_post_unlock_worker(&sys, ship_id, cyc_id).await;
    let step1 = tokio::spawn({ let s = sys.clone(); async move { s.step_manual_tick_only(4.0).await } });
    wait_until("tick 1 mod_a", || sys.actor_state.read().unwrap().active_nanite_repairs.first().is_some_and(|r| r.paste_consumed_so_far == 4)).await;
    *sys.module_deactivation_hook.write().unwrap() = None;
    ctx.db.with_mut_snapshot(|s| { s.items.remove(&paste_id); });
    let _ = go_tx.send(()); stop_fut.await.unwrap().unwrap(); step1.await.unwrap().unwrap();
    assert!((mod_dmg(&ctx.db, mod_a).unwrap() - 100.0).abs() < 1e-6 && shortfall_of(&ctx.db, ship_id, mod_a) == Some(4));

    ctx.db.with_mut_snapshot(|s| { s.items.insert(paste_id, make_item(paste_id, 28668, CHAR_ID, ship_id, InventoryFlag::Cargo, 4)); });
    sys.step_manual_tick_only(3.0).await.unwrap();
    assert!(paste_qty(&ctx.db, ship_id) == 0 && (mod_dmg(&ctx.db, mod_a).unwrap() - 60.0).abs() < 1e-6 && shortfall_of(&ctx.db, ship_id, mod_a) == Some(3));

    sys.cancel_ship_nanite_repairs_sync(ship_id, true);
    assert!(paste_qty(&ctx.db, ship_id) == 0 && (mod_dmg(&ctx.db, mod_a).unwrap() - 60.0).abs() < 1e-6 && shortfall_of(&ctx.db, ship_id, mod_a).is_none());

    ctx.db.with_mut_snapshot(|s| { s.items.insert(paste_id, make_item(paste_id, 28668, CHAR_ID, ship_id, InventoryFlag::Cargo, 5)); });
    push_nanite(&sys, ship_id, mod_b, 90.0, 0.0, 10, 0);
    let (go_tx, stop_fut) = pause_post_unlock_worker(&sys, ship_id, cyc_id).await;
    let step_b1 = tokio::spawn({ let s = sys.clone(); async move { s.step_manual_tick_only(5.0).await } });
    wait_until("tick 1 mod_b", || sys.actor_state.read().unwrap().active_nanite_repairs.first().is_some_and(|r| r.paste_consumed_so_far == 5)).await;
    *sys.module_deactivation_hook.write().unwrap() = None;
    ctx.db.with_mut_snapshot(|s| { s.items.remove(&paste_id); });
    let _ = go_tx.send(()); stop_fut.await.unwrap().unwrap(); step_b1.await.unwrap().unwrap();
    assert_eq!(shortfall_of(&ctx.db, ship_id, mod_b), Some(5));

    ctx.db.with_mut_snapshot(|s| { s.items.insert(paste_id, make_item(paste_id, 28668, CHAR_ID, ship_id, InventoryFlag::Cargo, 10)); });
    sys.step_manual_tick_only(5.0).await.unwrap();
    assert!(paste_qty(&ctx.db, ship_id) == 0 && mod_dmg(&ctx.db, mod_b).is_none() && shortfall_of(&ctx.db, ship_id, mod_b).is_none());

    ctx.db.with_mut_snapshot(|s| { s.sp6_lifecycle.nanite_paste_shortfall.insert((ship_id, mod_c), 2); s.items.insert(paste_id, make_item(paste_id, 28668, CHAR_ID, ship_id, InventoryFlag::Cargo, 3)); });
    let mut ops_c = PostUnlockOps::default();
    ops_c.push(tick_op(ship_id, mod_c, 3, 50.0, 5, 0.0, true));
    let _ = super::execute_post_unlock_ops(Some(&ctx.db), None, ops_c, None, None);
    assert!(mod_dmg(&ctx.db, mod_c).is_some_and(|d| (d - 20.0).abs() < 1e-6) && shortfall_of(&ctx.db, ship_id, mod_c).is_none());
}

/// `BF-5`: `TargetLockCompletedHook` runs post-commit with zero locks; `WouldBlock` cancel enqueues to actor.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf5_target_lock_hook_runs_post_commit_flush_guarded_and_would_block_cancel_enqueued() {
    let (ship_a, ship_b, mod_b, p1) = (778_204_001_i64, 778_204_002_i64, 778_204_003_i64, DVec3::new(321.0, 0.0, 0.0));
    let (ctx, sys) = boot_ship(ship_a, &[(mod_b, 2048, InventoryFlag::LoSlot0, 90.0)], None, None).await;
    sys.spawn_ball(make_ball(ship_b, 606, DVec3::new(200.0, 0.0, 0.0))).await.unwrap();

    let ((lock_tx, lock_rx), db_hook) = (mpsc::channel(), Arc::clone(&ctx.db));
    *sys.target_lock_completed_hook.write().unwrap() = Some(Arc::new(move |src, tgt| {
        let _ = lock_tx.send((src, tgt, eve_db::lock_rank::held_lock_ranks(), db_hook.with_snapshot(|s| s.item_positions.get(&ship_a).copied())));
    }));
    sys.add_target_lock(ship_a, eve_destiny::TargetLock { target_id: ship_b, lock_time_ms: 100.0, remaining_lock_ms: 50.0, locked: false, max_range_m: None }).await.unwrap();

    let held = hold_db_writer(&ctx.db);
    let pos_fut = spawn_pos(&sys, ship_a, p1);
    wait_until("prior post_unlock item should be in flight", || sys.has_in_flight_post_unlock()).await;
    let step_fut = tokio::spawn({ let s = sys.clone(); async move { s.step_manual_tick_only(0.2).await } });
    wait_until("target lock did not complete", || sys.ballpark.read().unwrap().target_locks.get(&ship_a).is_some_and(|v| v.iter().any(|l| l.target_id == ship_b && l.locked))).await;
    assert!(lock_rx.try_recv().is_err());

    release_writer(held);
    assert!(pos_fut.await.unwrap()); step_fut.await.unwrap().unwrap();
    let (src, tgt, ranks, pos) = lock_rx.recv_timeout(Duration::from_secs(2)).expect("lock hook must fire");
    assert_eq!((src, tgt, pos), (CHAR_ID, ship_b, Some([p1.x, p1.y, p1.z])));
    assert!(ranks.is_empty());

    push_nanite(&sys, ship_a, mod_b, 30.0, 0.0, 4, 2);
    { let _guard = sys.actor_state.read().unwrap(); sys.cancel_ship_nanite_repairs_sync(ship_a, true); }
    wait_until("WouldBlock cancel did not drain actor", || sys.actor_state.read().unwrap().active_nanite_repairs.is_empty()).await;
    assert!(sys.wait_for_post_unlock_barrier().await);
    assert!((mod_dmg(&ctx.db, mod_b).unwrap() - 30.0).abs() < 1e-6 && paste_qty(&ctx.db, ship_a) == 2);
}

/// `BF-1170-1A-02`: Activation asteroid seeding stages `BeltAsteroidInsertIfAbsent` without DB writes under actor locks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_seed_asteroid_enqueues_post_unlock_without_db_write() {
    use crate::system_actor::{AsteroidState, SystemActorState, SystemContentProvider, SystemContentSeedContext, SystemWakeReason};
    let (ctx, sys_id, ast_a, ast_b) = (test_ctx(), 30_000_144_u32, 778_205_001_i64, 778_205_002_i64);
    let make_ast = |asteroid_id| AsteroidState { asteroid_id, belt_id: 40_000_001, type_id: 1230, initial_units: 250, remaining_units: 250, unit_volume: 0.1, depleted: false, respawn_at_tick: None };

    let bp_lock: eve_db::lock_rank::RankedRwLock<eve_destiny::Ballpark, { eve_db::lock_rank::RANK_BALLPARK }> = eve_db::lock_rank::RankedRwLock::new(eve_destiny::Ballpark::new(sys_id));
    let st_lock: eve_db::lock_rank::RankedRwLock<SystemActorState, { eve_db::lock_rank::RANK_ACTOR_STATE }> = eve_db::lock_rank::RankedRwLock::new(SystemActorState::default());
    let mut seed_ops = PostUnlockOps::default();
    {
        let (mut bp, mut st) = (bp_lock.write().unwrap(), st_lock.write().unwrap());
        let mut seed_ctx = SystemContentSeedContext {
            solar_system_id: sys_id, wake_reason: &SystemWakeReason::LegacyGetOrSpawn, registry: &ctx.systems,
            ballpark: &mut bp, actor_state: &mut st, sde: Some(&ctx.sde), db: Some(&ctx.db), db_read_safe: true,
            belt_respawn_interval_secs: 3600, downtime_utc_secs_of_day: 39_600, mechanic_rng: &ctx.mechanic_rng,
            staged_special_site_balls: Default::default(), staged_wh_ore_anomalies: Default::default(), staged_post_unlock_ops: &mut seed_ops,
        };
        seed_ctx.spawn_asteroid(make_ast(ast_a), make_ball(ast_a, 1230, DVec3::ZERO));
    }
    assert!(matches!(seed_ops.intents.as_slice(), [PostUnlockIntent::BeltAsteroidInsertIfAbsent(r)] if r.asteroid_id == ast_a));
    assert!(ctx.db.with_snapshot(|s| !s.asteroids.contains_key(&ast_a)));

    struct AstProvider(i64, AsteroidState);
    impl SystemContentProvider for AstProvider {
        fn name(&self) -> &'static str { "test_ast_seed" }
        fn seed(&self, sctx: &mut SystemContentSeedContext<'_>) -> Result<(), String> {
            if sctx.solar_system_id == 30_000_144 { sctx.spawn_asteroid(self.1.clone(), make_ball(self.0, 1230, DVec3::new(50.0, 0.0, 0.0))); }
            Ok(())
        }
    }
    ctx.systems.register_content_provider(Arc::new(AstProvider(ast_b, make_ast(ast_b)))).unwrap();
    let held = hold_db_writer(&ctx.db);
    let sys = ctx.systems.get_or_spawn(sys_id);
    assert!(ctx.db.with_snapshot(|s| !s.asteroids.contains_key(&ast_b)));
    release_writer(held);
    assert!(sys.wait_for_post_unlock_barrier().await && ctx.db.with_snapshot(|s| s.asteroids.contains_key(&ast_b)));
}

/// `BF-1170-1A-03` + `BF-1170-1A-05`: Activation commands & empty-ops `try_remove_module_cycle` wait FIFO behind delayed cleanup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_activation_and_try_remove_wait_fifo_behind_in_flight_cleanup() {
    use crate::system_actor::{AtomicActivateCycleRequest, ProtectionBreakReason};
    use eve_db::lock_rank::{RankedRwLock, RANK_CRIMEWATCH, RANK_SERVICE_STATE, RANK_SYSTEM_REGISTRY};
    assert!(!{ let l = RankedRwLock::<(), RANK_SYSTEM_REGISTRY>::new(()); let _g = l.read().unwrap(); super::can_run_post_unlock_sync_now() });
    assert!(!{ let l = RankedRwLock::<(), RANK_CRIMEWATCH>::new(()); let _g = l.read().unwrap(); super::can_run_post_unlock_sync_now() });
    assert!(!{ let l = RankedRwLock::<(), RANK_SERVICE_STATE>::new(()); let _g = l.read().unwrap(); super::can_run_post_unlock_sync_now() });

    let (ship_id, mod_a, mod_b) = (778_206_001_i64, 778_206_002_i64, 778_206_003_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_a, 11578, InventoryFlag::HiSlot0, 0.0), (mod_b, 2048, InventoryFlag::LoSlot0, 0.0)], None, Some(make_combat_state(500.0, 500.0, 500.0, 500.0))).await;
    ctx.db.with_mut_snapshot(|s| { s.module_active_states.insert(mod_a); });
    sys.add_module_cycle(cloak_cyc(ship_id, mod_a, 1.0, 0.9, 0)).await.unwrap();

    let ((hook_tx, hook_rx), db_h) = (mpsc::channel(), Arc::clone(&ctx.db));
    *sys.module_deactivation_hook.write().unwrap() = Some(Arc::new(move |_, _, cyc| {
        let _ = hook_tx.send((cyc.module_id, db_h.with_snapshot(|s| !s.module_active_states.contains(&cyc.module_id))));
    }));

    let proj = ctx.build_fit_projection(CHAR_ID, ship_id, eve_dogma::FitRev(1)).unwrap();
    let held = hold_db_writer(&ctx.db);
    let step_out = sys.ask_step_tick_only(0.2).await.unwrap();
    wait_until("tick cleanup never queued", || sys.actor_state.read().unwrap().active_module_cycles.is_empty() && sys.has_in_flight_post_unlock()).await;
    assert!(hook_rx.try_recv().is_err() && ctx.db.with_snapshot(|s| s.module_active_states.contains(&mod_a)));
    let rec_fut = tokio::spawn({ let s = sys.clone(); async move { s.reconcile_module_cycle_before_activate(ship_id, mod_a, 1_000).await } });
    let act_fut = tokio::spawn({ let s = sys.clone(); async move { s.activate_module_cycle_atomic(AtomicActivateCycleRequest {
        char_id: CHAR_ID, allow_while_warping: false, allow_while_cloaked: true, replace_existing_cycle: false, break_protection: false,
        break_reason: ProtectionBreakReason::ModuleActivation, require_ball_in_space: true, real_now_unix: 1_000,
        cycle: ActiveModuleCycle { ship_id, owner_char_id: Some(CHAR_ID), module_id: mod_b, module_type_id: 2048, effect_name: "armorRepair".into(), duration_s: 5.0, repeat: 1, origin_scene_epoch: Some(1), ..Default::default() },
    }).await } });
    let rem_fut = tokio::spawn({ let s = sys.clone(); async move { s.remove_module_cycle(ship_id, mod_a).await } });
    let upd_fut = tokio::spawn({ let s = sys.clone(); async move { s.update_module_overload(ship_id, mod_b, true).await } });
    let fit_fut = tokio::spawn({ let s = sys.clone(); async move { s.apply_fit_projection(ship_id, proj).await } });
    let try_rem = std::thread::spawn({ let s = sys.clone(); move || s.try_remove_module_cycle(ship_id, mod_a) });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(!rec_fut.is_finished() && !act_fut.is_finished() && !rem_fut.is_finished() && !upd_fut.is_finished() && !fit_fut.is_finished() && !try_rem.is_finished());

    release_writer(held);
    assert_eq!(rec_fut.await.unwrap(), (None, false));
    assert!(act_fut.await.unwrap().is_ok() && rem_fut.await.unwrap().is_ok() && upd_fut.await.unwrap().is_ok() && fit_fut.await.unwrap());
    try_rem.join().unwrap();
    sys.apply_step_output(&step_out).await;
    assert_eq!(hook_rx.recv_timeout(Duration::from_secs(2)).unwrap(), (mod_a, true));
}

/// `BF-1170-1A-04`: `set_combat_state_in_actor_collecting` enforces `expected_system: Some(solar_system_id)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_set_combat_state_checks_expected_system() {
    let (sys_b_id, ship_id) = (30_000_144_u32, 778_207_001_i64);
    let (ctx, sys_a) = boot_ship(ship_id, &[], None, Some(make_combat_state(500.0, 500.0, 500.0, 200.0))).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().location_id = i64::from(sys_b_id);
        snap.characters.get_mut(&CHAR_ID).unwrap().solar_system_id = sys_b_id;
        snap.sp6_lifecycle.ship_combat_hp.insert(ship_id, crate::sp6_lifecycle::PersistedCombatHp { shield_hp: 123.0, armor_hp: 234.0, hull_hp: 345.0 });
    });
    let mut stale_cs = make_combat_state(500.0, 500.0, 500.0, 200.0); stale_cs.shield_hp = 490.0;
    sys_a.set_combat_state(ship_id, stale_cs).await.unwrap();
    let hp = ctx.db.with_snapshot(|s| s.sp6_lifecycle.ship_combat_hp.get(&ship_id).copied()).unwrap();
    assert!((hp.shield_hp - 123.0).abs() < 1e-6);
}

/// `BF-1170-1A-REV-01`: `InitiateModuleRepairMany` waits behind in-flight `NaniteRepairTick`/`NaniteRepairCancel`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_rev01_initiate_module_repair_many_waits_behind_in_flight_nanite() {
    use crate::services::inventory_fitting::fitting_dogma_ops::handle_dogma_ops;
    use eve_proto::PyRep;

    let (ship_id, mod_id, paste_id) = (778_208_001_i64, 778_208_002_i64, 778_208_003_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_id, 2048, InventoryFlag::LoSlot0, 20.0)], Some((paste_id, 10)), None).await;
    let uid = ctx.db.with_snapshot(|s| s.characters.get(&CHAR_ID).map(|c| c.user_id));
    let call_repair = || {
        let (c, mut s) = (ctx.clone(), eve_machonet::SessionState { user_id: uid, character_id: Some(CHAR_ID), corporation_id: Some(1_000_009), solar_system_id: Some(SYS_ID), ship_id: Some(ship_id), ..Default::default() });
        tokio::spawn(async move { handle_dogma_ops(&c, &mut s, None, "dogmaIM", "InitiateModuleRepairMany", &[PyRep::List(vec![PyRep::Integer(mod_id)])], &HashMap::new()).await })
    };

    let held1 = hold_db_writer(&ctx.db);
    let mut ops1 = PostUnlockOps::default(); ops1.push(tick_op(ship_id, mod_id, 0, 20.0, 2, 0.0, true));
    send_post_ops(&sys, &ctx, ops1);
    let rpc1 = call_repair();
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!rpc1.is_finished());
    release_writer(held1);
    assert_eq!(rpc1.await.unwrap().unwrap().unwrap_err().code, "NoDamagedModulesToRepair");

    ctx.db.with_mut_snapshot(|s| { s.items.get_mut(&mod_id).unwrap().dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 30.0); s.items.get_mut(&paste_id).unwrap().quantity = 4; s.sp6_lifecycle.nanite_paste_shortfall.insert((ship_id, mod_id), 2); });
    let held2 = hold_db_writer(&ctx.db);
    let mut ops2 = PostUnlockOps::default(); ops2.push(cancel_op(ship_id, mod_id, 15.0, true, 5));
    send_post_ops(&sys, &ctx, ops2);
    let rpc2 = call_repair();
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!rpc2.is_finished());
    release_writer(held2);
    assert!(matches!(rpc2.await.unwrap().unwrap().unwrap(), PyRep::Tuple(ref p) if p.first() == Some(&PyRep::List(vec![PyRep::Integer(mod_id)]))));
    assert_eq!(paste_qty(&ctx.db, ship_id), 6);

    sys.cancel_ship_nanite_repairs_sync(ship_id, false);
    ctx.db.with_mut_snapshot(|s| { s.items.get_mut(&mod_id).unwrap().dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 30.0); });
    let held3 = hold_db_writer(&ctx.db);
    let mut ops3 = PostUnlockOps::default(); ops3.push(cancel_op(ship_id, mod_id, 15.0, false, 0));
    send_post_ops(&sys, &ctx, ops3);
    let rpc3 = call_repair();
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!rpc3.is_finished());
    release_writer(held3);
    assert!(matches!(rpc3.await.unwrap().unwrap().unwrap(), PyRep::Tuple(ref p) if p.first() == Some(&PyRep::List(vec![PyRep::Integer(mod_id)]))));
    assert_eq!(sys.actor_state.read().unwrap().active_nanite_repairs.iter().find(|r| r.module_id == mod_id).map(|r| r.initial_damage_hp), Some(15.0));
}

/// `BF-1170-1A-REV-02`: Closed `post_unlock_tx` skips `RunDirectAfterUnlock` when ranked locks are held.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_rev02_closed_worker_under_ranked_locks_skips_direct_db_and_actor_write() {
    use eve_db::lock_rank::{RankedRwLock, RANK_LEAF};
    let (ship_a, mod_a, ship_b, mod_b, ship_c, mod_c, paste_c) = (778_209_001_i64, 778_209_002_i64, 778_209_003_i64, 778_209_004_i64, 778_209_005_i64, 778_209_006_i64, 778_209_007_i64);
    let (ctx, sys) = boot_ship(ship_a, &[(mod_a, 2048, InventoryFlag::LoSlot0, 90.0)], None, None).await;
    seed_ship_char(&ctx.db, SYS_ID, CHAR_ID, ship_b, &[(mod_b, 2048, InventoryFlag::LoSlot0, 90.0)], None);
    seed_ship_char(&ctx.db, SYS_ID, CHAR_ID, ship_c, &[(mod_c, 2048, InventoryFlag::LoSlot0, 90.0)], Some((paste_c, 2)));
    for (sid, mid) in [(ship_a, mod_a), (ship_b, mod_b), (ship_c, mod_c)] {
        if sid != ship_a { sys.spawn_ball(make_ball(sid, 606, DVec3::ZERO)).await.unwrap(); }
        push_nanite(&sys, sid, mid, 30.0, 0.0, 4, 2);
    }
    let _ = sys.post_unlock_tx.send(PostUnlockMsg::Shutdown);
    wait_until("post_unlock_tx never closed", || sys.post_unlock_tx.is_closed()).await;

    ctx.db.with_mut_snapshot(|snap| {
        sys.cancel_ship_nanite_repairs_sync(ship_a, true); sys.try_remove_ball(ship_a);
        assert!((snap.items[&mod_a].dynamic_attributes[&eve_sde::attr_ids::DAMAGE] - 90.0).abs() < 1e-6);
    });
    let leaf: RankedRwLock<(), RANK_LEAF> = RankedRwLock::new(());
    {
        let _g = leaf.write().unwrap();
        sys.cancel_ship_nanite_repairs_sync(ship_b, true); sys.try_remove_ball(ship_b);
        let mut ops = PostUnlockOps::default(); ops.push(PostUnlockIntent::ShipCycleClearAll(ship_b)); ops.push(cancel_op(ship_b, mod_b, 15.0, false, 0));
        super::finish_post_unlock_after_unlock(&sys, None, None, super::PostUnlockWaitTicket::RunDirectAfterUnlock(Box::new(work_item(Some(Arc::clone(&ctx.db)), None, ops))));
    }
    assert!((mod_dmg(&ctx.db, mod_b).unwrap() - 90.0).abs() < 1e-6);

    sys.cancel_ship_nanite_repairs_sync(ship_c, true);
    assert!((mod_dmg(&ctx.db, mod_c).unwrap() - 30.0).abs() < 1e-6 && paste_qty(&ctx.db, ship_c) == 4);
}

/// `BF-1170-1A-R5-01` + `Runtime::drop()`: Guided-missile volley waits behind in-flight write; `Runtime::drop()` exits cleanly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r5_01_guided_missile_volley_waits_and_runtime_drop_exits_cleanly() {
    let (a_ship, v_ship, mod_id, p_prior) = (778_210_001_i64, 778_210_002_i64, 778_210_101_i64, DVec3::new(456.0, 0.0, 0.0));
    let (ctx, sys) = boot_ship(a_ship, &[], None, None).await;
    sys.spawn_ball(make_ball(v_ship, 606, DVec3::new(1_000.0, 0.0, 0.0))).await.unwrap();
    sys.add_module_cycle(ActiveModuleCycle { ship_id: a_ship, module_id: mod_id, module_type_id: 2410, owner_char_id: Some(CHAR_ID), effect_name: "missileLaunching".into(), target_id: Some(v_ship), duration_s: 5.0, weapon_is_guided_missile: true, missile_charge_type_id: Some(209), ..Default::default() }).await.unwrap();

    let held = hold_db_writer(&ctx.db);
    let pos_fut = spawn_pos(&sys, a_ship, p_prior);
    wait_until("prior post_unlock item should be in flight", || sys.has_in_flight_post_unlock()).await;
    let volley_fut = tokio::spawn({ let s = sys.clone(); async move { s.fire_weapon_cycle_volley(a_ship, mod_id).await } });
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!volley_fut.is_finished());

    release_writer(held);
    assert!(pos_fut.await.unwrap() && volley_fut.await.unwrap().unwrap().0.is_empty());
    assert_eq!(ctx.db.with_snapshot(|s| s.item_positions.get(&a_ship).copied()), Some([p_prior.x, p_prior.y, p_prior.z]));

    let reg_rt = ctx.systems.clone();
    let h = std::thread::spawn(move || { let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap(); let h = rt.block_on(async move { reg_rt.get_or_spawn(30_000_144) }); drop(rt); h }).join().unwrap();
    wait_until("post_unlock_tx never closed after Runtime::drop", || h.post_unlock_tx.is_closed()).await;
}

/// `BF-1170-1A-R6-01` (1/4): `p0 -> p1 -> p0` (`set_ball_position`) under held DB writer never skips `p0`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r6_01_p0_p1_p0_set_ball_position_under_held_writer() {
    let (ship_id, p0, p1) = (778_211_001_i64, DVec3::new(10.0, 0.0, 0.0), DVec3::new(500.0, 0.0, 0.0));
    let (ctx, sys) = boot_ship(ship_id, &[], None, None).await;
    ctx.db.with_mut_snapshot(|s| { s.item_positions.insert(ship_id, [p0.x, p0.y, p0.z]); });
    sys.set_ball_position(ship_id, p0).await;

    let held = hold_db_writer(&ctx.db);
    let f1 = spawn_pos(&sys, ship_id, p1);
    wait_until("p1 never hit memory", || sys.get_ball_position_sync(ship_id) == Some(p1)).await;
    let f2 = spawn_pos(&sys, ship_id, p0);
    wait_until("p0 never hit memory", || sys.get_ball_position_sync(ship_id) == Some(p0)).await;
    release_writer(held);
    assert!(f1.await.unwrap() && f2.await.unwrap());
    assert_eq!(ctx.db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()), Some([p0.x, p0.y, p0.z]));
}

/// `BF-1170-1A-R6-01` (2/4): TiDi stretch then TiDi relax while `ModuleReload` is queued under held writer clears stretched DB deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r6_01_tidi_stretch_then_relax_while_queued() {
    let (ctx, mod_id) = (test_ctx(), 778_212_101_i64);
    let init_until = ctx.db.clock().now_unix_secs() + 12;
    ctx.db.with_mut_snapshot(|s| { s.cluster_and_runtime.module_reload_until_unix.insert(mod_id, init_until); });
    let sys = ctx.systems.get_or_spawn(SYS_ID);
    sys.actor_state.write().unwrap().module_reload_sim_remaining.insert(mod_id, super::super::types::ModuleReloadSimEntry { remaining_sim_s: 12.0, min_wall_until_unix: init_until, tidi_stretched: false });

    let held = hold_db_writer(&ctx.db);
    let _ = sys.set_tidi_factor_raw(0.5);
    let t1 = std::thread::spawn({ let s = sys.clone(); move || s.try_step_full_tick(1.0) });
    wait_until("step1 tidi stretch never applied", || sys.actor_state.read().unwrap().module_reload_sim_remaining.get(&mod_id).is_some_and(|e| e.tidi_stretched)).await;
    let _ = sys.set_tidi_factor_raw(1.0);
    let t2 = std::thread::spawn({ let s = sys.clone(); move || s.try_step_full_tick(1.0) });
    wait_until("step2 tidi relax never applied", || sys.actor_state.read().unwrap().module_reload_sim_remaining.get(&mod_id).is_some_and(|e| !e.tidi_stretched)).await;
    release_writer(held);
    t1.join().unwrap(); t2.join().unwrap();
    let final_u = ctx.db.with_snapshot(|s| s.cluster_and_runtime.module_reload_until_unix.get(&mod_id).copied()).unwrap();
    assert!(final_u <= init_until + 4);
}

/// `BF-1170-1A-R6-01` (3/4): Back-to-back RAH cycles under held writer accumulate from actor state (`0.85 -> 0.79 -> 0.73`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r6_01_back_to_back_rah_cycles_under_held_writer() {
    let (ship_id, mod_id) = (778_213_001_i64, 778_213_101_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_id, 11269, InventoryFlag::LoSlot0, 0.0)], None, Some(make_combat_state(100.0, 100.0, 100.0, 100.0))).await;
    sys.add_module_cycle(ActiveModuleCycle { ship_id, module_id: mod_id, module_type_id: 11269, owner_char_id: Some(CHAR_ID), effect_name: "adaptiveArmorHardener".into(), duration_s: 5.0, repeat: 1, is_rah: true, rah_shift_amount: 6.0, ..Default::default() }).await.unwrap();

    let held = hold_db_writer(&ctx.db);
    let t1 = std::thread::spawn({ let s = sys.clone(); move || s.try_step_full_tick(5.0) });
    while !matches!(sys.get_combat_state(ship_id).await.unwrap().and_then(|c| c.rah_armor_resonance), Some(r) if r[0] < 0.80) { tokio::time::sleep(Duration::from_millis(2)).await; }
    let t2 = std::thread::spawn({ let s = sys.clone(); move || s.try_step_full_tick(5.0) });
    while !matches!(sys.get_combat_state(ship_id).await.unwrap().and_then(|c| c.rah_armor_resonance), Some(r) if r[0] < 0.74) { tokio::time::sleep(Duration::from_millis(2)).await; }
    release_writer(held);
    t1.join().unwrap(); t2.join().unwrap();
    let em_res = ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&mod_id).and_then(|m| m.get(&267).copied())).unwrap();
    assert!((em_res - 0.73).abs() < 1e-4);
}

/// `BF-1170-1A-R6-01` (4/4): `ShipCycleClearAll` behind queued `CycleAmmoStep` clears DB charge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r6_01_ship_cycle_clear_all_behind_queued_cycle_ammo_step() {
    let (ship_id, mod_id, ammo_id) = (778_214_001_i64, 778_214_101_i64, 778_214_201_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_id, 484, InventoryFlag::HiSlot0, 0.0)], None, None).await;
    ctx.db.with_mut_snapshot(|s| { s.items.insert(ammo_id, make_item(ammo_id, 178, CHAR_ID, ship_id, InventoryFlag::Cargo, 10)); });
    sys.add_module_cycle(cloak_cyc(ship_id, mod_id, 10.0, 0.0, 0)).await.unwrap();

    let held = hold_db_writer(&ctx.db);
    let mut ops_ammo = PostUnlockOps::default();
    ops_ammo.push(PostUnlockIntent::CycleAmmoStep(super::CycleAmmoStepDbOp {
        char_id: Some(CHAR_ID), ship_id, linked_turrets: vec![mod_id], ammo_type_id: Some(178), missile_charge_type_id: None,
        damage_vector_total: 100.0, crystal_volatility_chance: 0.0, crystal_volatility_damage: 0.0, pre_consumed_mode: false, consume_pre_round_for_next_cycle: false,
        reload_spec: Some(super::CycleReloadDbSpec { ammo_type_id: 178, linked_turrets: vec![mod_id], master_module_id: mod_id, actual_per_mod: 1, reload_time_ms: 5000.0, reload_until: 999 }),
    }));
    send_in_flight(&sys, &ctx, ops_ammo);
    let rm_fut = tokio::spawn({ let s = sys.clone(); async move { s.remove_ball(ship_id).await } });
    wait_until("remove_ball never cleared actor ball", || sys.get_ball_position_sync(ship_id).is_none()).await;
    assert!(!rm_fut.is_finished());

    release_writer(held);
    assert!(rm_fut.await.unwrap().is_some());
    ctx.db.with_snapshot(|s| {
        assert_eq!(s.items[&ammo_id].quantity, 9);
        assert!(!s.module_active_charges.contains_key(&mod_id) && !s.module_active_states.contains(&mod_id));
    });
}

/// `BF-1170-1A-R6-02`: Empty-ops `flush_dirty_condition_checkpoint_sync` and absent-ball `modify_combat_state` wait FIFO.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r6_02_empty_ops_flush_checkpoint_and_modify_combat_absent_wait_fifo() {
    let (ship_id, p0) = (778_215_001_i64, DVec3::new(321.0, 0.0, 0.0));
    let (ctx, sys) = boot_ship(ship_id, &[], None, None).await;
    sys.set_ball_position(ship_id, p0).await;
    let _ = sys.flush_condition_checkpoint().await.unwrap();
    ctx.db.with_mut_snapshot(|s| { s.item_positions.remove(&ship_id); });

    let held = hold_db_writer(&ctx.db);
    let pos_fut = spawn_pos(&sys, ship_id, p0);
    wait_until("in-flight item never started", || sys.has_in_flight_post_unlock()).await;
    let flush_h = std::thread::spawn({ let s = sys.clone(); move || s.flush_dirty_condition_checkpoint_sync() });
    let mod_fut = tokio::spawn({ let s = sys.clone(); async move { s.modify_combat_state(ship_id + 999, |_| {}).await } });
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!flush_h.is_finished() && !mod_fut.is_finished());

    release_writer(held);
    assert!(flush_h.join().unwrap() == 0 && mod_fut.await.unwrap().unwrap().is_none() && pos_fut.await.unwrap());
    assert_eq!(ctx.db.with_snapshot(|s| s.item_positions.get(&ship_id).copied()), Some([p0.x, p0.y, p0.z]));
}

/// `BF-1170-1A-R6-03`: Halting mid-repair for paste shortage clears `nanite_paste_shortfall` even when `paste_delta > 0`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r6_03_nanite_halt_for_paste_clears_shortfall() {
    let (ship_id, mod_id, paste_id) = (778_216_001_i64, 778_216_101_i64, 778_216_201_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_id, 2048, InventoryFlag::LoSlot0, 100.0)], Some((paste_id, 2)), None).await;
    ctx.db.with_mut_snapshot(|s| { s.sp6_lifecycle.nanite_paste_shortfall.insert((ship_id, mod_id), 1); });
    push_nanite(&sys, ship_id, mod_id, 100.0, 0.0, 10, 0);

    std::thread::spawn({ let s = sys.clone(); move || s.try_step_full_tick(5.0) }).join().unwrap();
    assert!(sys.actor_state.read().unwrap().active_nanite_repairs.is_empty());
    assert!(shortfall_of(&ctx.db, ship_id, mod_id).is_none() && paste_qty(&ctx.db, ship_id) == 0 && mod_dmg(&ctx.db, mod_id).unwrap() < 100.0);
}

/// `BF-1170-1A-R7-01`: `RepairTetherModuleHeal` behind in-flight `ModuleBurnout` and multi-step `FastForwardClock` `p0 -> p1 -> p0` for non-ego ball.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r7_01_tether_heal_behind_burnout_and_fast_forward_non_ego_p0_p1_p0() {
    let (ship_id, mod_id, ego_id, non_ego_id) = (778_217_001_i64, 778_217_101_i64, 778_217_002_i64, 778_217_003_i64);
    let (p0, p1) = (DVec3::new(100.0, 0.0, 0.0), DVec3::new(300_000.0, 0.0, 0.0));
    let mut cs_tether = make_combat_state(100.0, 100.0, 100.0, 100.0); cs_tether.is_tethered = true;
    let (ctx, sys) = boot_ship(ship_id, &[(mod_id, 2048, InventoryFlag::LoSlot0, 0.0)], None, Some(cs_tether)).await;
    ctx.db.with_mut_snapshot(|s| {
        for (id, pos) in [(ego_id, DVec3::ZERO), (non_ego_id, p0)] { s.items.insert(id, make_item(id, 606, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1)); s.item_positions.insert(id, [pos.x, pos.y, pos.z]); }
    });
    let mut ego_ball = make_ball(ego_id, 606, DVec3::ZERO); ego_ball.speed_fraction = 1.0;
    let mut non_ego_ball = make_ball(non_ego_id, 606, p0); non_ego_ball.max_velocity = 0.0;
    sys.spawn_ball(ego_ball).await.unwrap(); sys.spawn_ball(non_ego_ball).await.unwrap();
    sys.set_movement_mode(ego_id, MovementMode::GotoDirection { direction: DVec3::X }).await.unwrap();
    let warp_to = |src, dst| MovementMode::Warp { origin: src, destination: dst, warp_speed_au_s: 3.0, total_distance: DVec3::distance(src, dst), phase: eve_destiny::WarpPhase::Accelerating, elapsed_warp_time: 0.0 };
    sys.set_movement_mode(non_ego_id, warp_to(p0, p1)).await.unwrap();
    let tick0 = sys.ballpark.read().unwrap().current_tick;

    let held = hold_db_writer(&ctx.db);
    let mut ops_burn = PostUnlockOps::default(); ops_burn.push(PostUnlockIntent::ModuleBurnout { ship_id, module_id: mod_id });
    send_in_flight(&sys, &ctx, ops_burn);
    let tick_h = std::thread::spawn({ let s = sys.clone(); move || s.try_step_full_tick(1.0) });
    wait_until("tether step did not advance", || sys.ballpark.read().unwrap().current_tick > tick0).await;

    let ff1 = tokio::spawn({ let s = sys.clone(); async move { s.fast_forward_clock(ego_id, 40).await } });
    wait_until("non_ego never warped to p1", || sys.get_ball_position_sync(non_ego_id).is_some_and(|p| p.distance(p1) < 1e-2)).await;
    let mv_fut = tokio::spawn({ let s = sys.clone(); let m = warp_to(p1, p0); async move { s.set_movement_mode(non_ego_id, m).await } });
    wait_until("non_ego warp back never armed", || matches!(sys.ballpark.read().unwrap().balls.get(&non_ego_id).map(|b| &b.mode), Some(MovementMode::Warp { .. }))).await;
    let ff2 = tokio::spawn({ let s = sys.clone(); async move { s.fast_forward_clock(ego_id, 40).await } });
    wait_until("non_ego never warped back to p0", || sys.get_ball_position_sync(non_ego_id).is_some_and(|p| p.distance(p0) < 1e-2)).await;
    assert!(!tick_h.is_finished() && !ff1.is_finished() && !ff2.is_finished());

    release_writer(held);
    tick_h.join().unwrap(); ff1.await.unwrap().unwrap(); mv_fut.await.unwrap().unwrap(); ff2.await.unwrap().unwrap();
    assert_eq!(mod_dmg(&ctx.db, mod_id), Some(38.0));
    let final_pos = ctx.db.with_snapshot(|s| s.item_positions.get(&non_ego_id).copied()).unwrap();
    assert!((DVec3::from_array(final_pos) - p0).length() < 1e-2);
}

/// `BF-1170-1A-R7-02`: Unlocked `cancel_ship_nanite_repairs_sync` waits behind completed `NaniteRepairTick` even with empty `active_nanite_repairs`,
/// and `wait_for_post_unlock_barrier` returns `false` when the worker drops the barrier sender or shuts down.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r7_02_empty_cancel_nanite_waits_and_dropped_barrier_returns_false() {
    let (ship_id, mod_id) = (778_218_001_i64, 778_218_101_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_id, 2048, InventoryFlag::LoSlot0, 20.0)], None, None).await;

    let held = hold_db_writer(&ctx.db);
    let mut ops = PostUnlockOps::default(); ops.push(tick_op(ship_id, mod_id, 0, 20.0, 2, 0.0, true));
    send_in_flight(&sys, &ctx, ops);
    wait_until("tick never entered in-flight", || sys.has_in_flight_post_unlock()).await;
    let cancel_h = std::thread::spawn({ let s = sys.clone(); move || s.cancel_ship_nanite_repairs_sync(ship_id, true) });
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!cancel_h.is_finished());
    release_writer(held); cancel_h.join().unwrap();
    assert!(mod_dmg(&ctx.db, mod_id).is_none());

    let (mut h_drop, (tx_d, mut rx_d)) = (sys.clone(), tokio::sync::mpsc::unbounded_channel());
    h_drop.post_unlock_tx = tx_d;
    let bar_fut = tokio::spawn(async move { h_drop.wait_for_post_unlock_barrier().await });
    drop(rx_d.recv().await.unwrap());
    assert!(!bar_fut.await.unwrap());

    let _ = sys.post_unlock_tx.send(PostUnlockMsg::Shutdown);
    wait_until("post_unlock_tx never closed", || sys.post_unlock_tx.is_closed()).await;
    assert!(!sys.wait_for_post_unlock_barrier().await);
}

/// `BF-1170-1A-R8-01`: `take_ball` and `take_ball_for_docking` never block `current_thread` Tokio workers under held DB writer and drain queued `StartNaniteRepair`.
#[tokio::test]
async fn bf_1170_r8_01_take_ball_nonblocking_on_current_thread_and_drains_nanite() {
    let (ship_a, mod_a, paste_a, ship_b, mod_b, paste_b) = (778_219_001_i64, 778_219_101_i64, 778_219_201_i64, 778_219_002_i64, 778_219_102_i64, 778_219_202_i64);
    let (ctx, sys) = boot_ship(ship_a, &[(mod_a, 2048, InventoryFlag::LoSlot0, 100.0)], Some((paste_a, 10)), None).await;
    seed_ship_char(&ctx.db, SYS_ID, CHAR_ID, ship_b, &[(mod_b, 2048, InventoryFlag::LoSlot0, 100.0)], Some((paste_b, 10)));
    sys.spawn_ball(make_ball(ship_b, 606, DVec3::new(100.0, 0.0, 0.0))).await.unwrap();

    let held = hold_db_writer(&ctx.db);
    let mut ops_p = PostUnlockOps::default(); ops_p.push(PostUnlockIntent::ModuleBurnout { ship_id: ship_a, module_id: mod_a });
    send_in_flight(&sys, &ctx, ops_p);
    for (sid, mid) in [(ship_a, mod_a), (ship_b, mod_b)] {
        sys.cmd_sender().send(crate::system_actor::SystemCommand::StartNaniteRepair {
            repair: ActiveNaniteRepair { ship_id: sid, char_id: CHAR_ID, module_id: mid, initial_damage_hp: 100.0, duration_s: 10.0, elapsed_s: 6.0, paste_total_needed: 10, paste_consumed_so_far: 10 },
        }).await.unwrap();
    }
    let take_h = tokio::spawn({ let s = sys.clone(); async move { s.take_ball(ship_a).await } });
    let dock_h = tokio::spawn({ let s = sys.clone(); async move { s.take_ball_for_docking(ship_b, 60_003_760, DVec3::ZERO).await } });
    wait_until("TakeBall never cleared balls/nanites on current_thread", || {
        sys.get_ball_position_sync(ship_a).is_none() && sys.get_ball_position_sync(ship_b).is_none()
            && sys.actor_state.read().unwrap().active_nanite_repairs.is_empty()
    }).await;
    assert!(!take_h.is_finished() && !dock_h.is_finished(), "TakeBall bypassed barrier");

    release_writer(held);
    assert!(take_h.await.unwrap().is_some() && dock_h.await.unwrap().is_some());
    assert!(mod_dmg(&ctx.db, mod_a).is_some_and(|d| (d - 40.0).abs() < 1e-6) && paste_qty(&ctx.db, ship_a) == 14);
    assert!(mod_dmg(&ctx.db, mod_b).is_some_and(|d| (d - 40.0).abs() < 1e-6) && paste_qty(&ctx.db, ship_b) == 14);
}

/// `BF-1170-1A-R8-02`: Cross-tick charge/ammo/reload reservations prevent stale double consumption across Cap Booster, ASB, AAR, missile, 2-round turret, and cargo reload, while `UnloadAmmo` and `invbroker.Add` wait on the barrier.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r8_02_cross_tick_charge_reservations_and_rpc_unload_barrier() {
    use eve_proto::PyRep;
    let (ship_id, m_tur2, m_tur, m_mis, m_asb, m_aar, m_cap) = (778_220_001_i64, 778_220_101, 778_220_102, 778_220_103, 778_220_104, 778_220_105, 778_220_106);
    let mods = [(m_tur2, 484, InventoryFlag::HiSlot0, 0.0), (m_tur, 484, InventoryFlag::HiSlot1, 0.0), (m_mis, 2410, InventoryFlag::HiSlot2, 0.0), (m_asb, 32772, InventoryFlag::MedSlot0, 0.0), (m_aar, 33076, InventoryFlag::LoSlot0, 0.0), (m_cap, 2022, InventoryFlag::LoSlot1, 0.0)];
    let mut cs = make_combat_state(100.0, 100.0, 100.0, 200.0); cs.capacitor = 0.0;
    let (ctx, sys) = boot_ship(ship_id, &mods, None, Some(cs)).await;
    ctx.db.with_mut_snapshot(|s| {
        for (cid, tid, flag, q) in [(778_220_201, 178, InventoryFlag::HiSlot0, 1), (778_220_202, 178, InventoryFlag::HiSlot1, 2), (778_220_203, 178, InventoryFlag::Cargo, 1), (778_220_204, 209, InventoryFlag::HiSlot2, 1), (778_220_205, 11289, InventoryFlag::MedSlot0, 1), (778_220_206, 28668, InventoryFlag::LoSlot0, 1), (778_220_207, 11289, InventoryFlag::LoSlot1, 1)] {
            s.items.insert(cid, make_item(cid, tid, CHAR_ID, ship_id, flag, q));
        }
    });
    sys.spawn_ball(make_ball(778_220_999, 606, DVec3::new(1000.0, 0.0, 0.0))).await.unwrap();
    for cyc in [
        ActiveModuleCycle { ship_id, owner_char_id: Some(CHAR_ID), module_id: m_tur2, module_type_id: 484, effect_name: "projectileFired".into(), target_id: Some(778_220_999), duration_s: 5.0, repeat: 1, linked_turrets: vec![m_tur2], ammo_type_id: Some(178), damage_vector: eve_destiny::DamageVector::new(50.0, 0.0, 0.0, 0.0), ..Default::default() },
        ActiveModuleCycle { ship_id, owner_char_id: Some(CHAR_ID), module_id: m_tur, module_type_id: 484, effect_name: "projectileFired".into(), target_id: Some(778_220_999), duration_s: 5.0, repeat: 1, linked_turrets: vec![m_tur], ammo_type_id: Some(178), damage_vector: eve_destiny::DamageVector::new(50.0, 0.0, 0.0, 0.0), ..Default::default() },
        ActiveModuleCycle { ship_id, owner_char_id: Some(CHAR_ID), module_id: m_mis, module_type_id: 2410, effect_name: "missileLaunching".into(), target_id: Some(778_220_999), duration_s: 5.0, repeat: 1, linked_turrets: vec![m_mis], weapon_is_guided_missile: true, missile_charge_type_id: Some(209), ammo_type_id: Some(209), crystal_volatility_chance: -1.0, reload_remaining_sim_s: 0.0005, damage_vector: eve_destiny::DamageVector::new(50.0, 0.0, 0.0, 0.0), ..Default::default() },
        ActiveModuleCycle { ship_id, owner_char_id: Some(CHAR_ID), module_id: m_asb, module_type_id: 32772, effect_name: "shieldBoosting".into(), duration_s: 5.0, repeat: 1, is_asb: true, asb_fallback_cap_need: 50.0, ..Default::default() },
        ActiveModuleCycle { ship_id, owner_char_id: Some(CHAR_ID), module_id: m_aar, module_type_id: 33076, effect_name: "armorRepair".into(), duration_s: 5.0, repeat: 1, is_aar: true, aar_base_armor_repair: 20.0, aar_paste_rate: 1, armor_repair_amount: 60.0, ..Default::default() },
        ActiveModuleCycle { ship_id, owner_char_id: Some(CHAR_ID), module_id: m_cap, module_type_id: 2022, effect_name: "capacitorInject".into(), duration_s: 5.0, repeat: 1, cap_booster_inject_amount: 100.0, ..Default::default() },
    ] { sys.add_module_cycle(cyc).await.unwrap(); }

    let held = hold_db_writer(&ctx.db);
    let _out1 = sys.ask_step_tick_only(5.0).await.unwrap();
    let _out2 = sys.ask_step_tick_only(5.0).await.unwrap();
    let (st_cap, st_missiles, cyc_len, aar_rep) = (
        sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor,
        sys.actor_state.read().unwrap().active_guided_missiles.len(),
        sys.actor_state.read().unwrap().active_module_cycles.len(),
        sys.actor_state.read().unwrap().active_module_cycles.iter().find(|c| c.module_id == m_aar).map(|c| c.armor_repair_amount),
    );
    assert!((st_cap - 50.0).abs() < 1e-6 && st_missiles == 1 && aar_rep == Some(20.0), "cap={st_cap}, mis={st_missiles}, aar={aar_rep:?}, cyc={cyc_len}");

    let uid = ctx.db.with_snapshot(|s| s.characters.get(&CHAR_ID).map(|c| c.user_id));
    let mk_sess = || eve_machonet::SessionState { user_id: uid, character_id: Some(CHAR_ID), solar_system_id: Some(SYS_ID), ship_id: Some(ship_id), ..Default::default() };
    let unload_fut = tokio::spawn({ let (c, mut s) = (ctx.clone(), mk_sess()); async move { crate::services::inventory_fitting::fitting_dogma_im::handle_dogma_im(&c, &mut s, None, "dogmaIM", "UnloadAmmo", &[PyRep::Integer(ship_id), PyRep::PySet(vec![PyRep::Integer(m_tur)])], &HashMap::new()).await } });
    let add_fut = tokio::spawn({ let (c, mut s) = (ctx.clone(), mk_sess()); async move { crate::services::inventory_fitting::handle(&c, &mut s, None, "invbroker", "Add", &[], &HashMap::new()).await } });
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!unload_fut.is_finished() && !add_fut.is_finished(), "charge unload/add bypassed barrier");

    release_writer(held);
    let _ = unload_fut.await.unwrap();
    let _ = add_fut.await.unwrap();
    ctx.db.with_snapshot(|s| {
        assert!(!s.items.contains_key(&778_220_202) && !s.items.contains_key(&778_220_203) && !s.items.contains_key(&778_220_204));
        assert!(!s.items.contains_key(&778_220_205) && !s.items.contains_key(&778_220_206) && !s.items.contains_key(&778_220_207));
        assert_eq!(s.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != m_tur2).map(|i| i.quantity).sum::<i32>(), 1);
        assert!(!s.items.values().any(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot1 && i.item_id != m_tur));
    });
}
