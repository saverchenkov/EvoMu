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
    if let Some(sde) = SHARED_SDE.get() { ServerContext::with_sde_and_repository(Arc::clone(sde), Arc::new(WorldRepository::with_demo_seed())).with_seeded_rng(crate::rng::TEST_MODE_DEFAULT_SEED) }
    else { let ctx = ServerContext::new(); let _ = SHARED_SDE.set(Arc::clone(&ctx.sde)); ctx }
}
fn make_item(item_id: i64, type_id: u32, owner_id: i64, location_id: i64, flag: InventoryFlag, quantity: i32) -> InventoryItemRecord { InventoryItemRecord { item_id, type_id, owner_id, location_id, flag, quantity, singleton: quantity == 1 && flag != InventoryFlag::Cargo, version: 1, ..Default::default() } }
fn make_ball(id: i64, type_id: u32, pos: DVec3) -> Ball { Ball { id, type_id, position: pos, velocity: DVec3::ZERO, max_velocity: 300.0, inertia: 2.0, mass: 1e6, radius: 40.0, speed_fraction: 0.0, mode: MovementMode::Stop } }
fn make_combat_state(shield: f64, armor: f64, hull: f64, cap: f64) -> CombatState { CombatState { shield_hp: shield, max_shield_hp: shield, armor_hp: armor, max_armor_hp: armor, structure_hp: hull, max_structure_hp: hull, capacitor: cap, max_capacitor: cap, ..CombatState::default() } }
fn hold_db_writer(db: &Arc<WorldRepository>) -> (mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let ((held_tx, held_rx), (rel_tx, rel_rx), db_hold) = (mpsc::channel(), mpsc::channel(), Arc::clone(db));
    let h = std::thread::spawn(move || { db_hold.with_mut_snapshot(|_| { held_tx.send(()).unwrap(); let _ = rel_rx.recv(); }); });
    held_rx.recv().unwrap(); (rel_tx, h)
}
fn release_writer((rel, writer): (mpsc::Sender<()>, std::thread::JoinHandle<()>)) { let _ = rel.send(()); writer.join().unwrap(); }
async fn wait_until(msg: &str, mut cond: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !cond() { assert!(tokio::time::Instant::now() < deadline, "{msg}"); tokio::time::sleep(Duration::from_millis(2)).await; }
}
async fn wait_until_async<F, Fut>(msg: &str, mut cond: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !cond().await {
        assert!(tokio::time::Instant::now() < deadline, "{msg}");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
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
    let ctx = test_ctx(); let sys = ctx.systems.get_or_spawn(SYS_ID);
    seed_ship_char(&ctx.db, SYS_ID, CHAR_ID, ship_id, extra_mods, paste);
    if let Some(combat) = cs { sys.spawn_ball_with_combat(make_ball(ship_id, 606, DVec3::ZERO), combat).await.unwrap(); } else { sys.spawn_ball(make_ball(ship_id, 606, DVec3::ZERO)).await.unwrap(); }
    (ctx, sys)
}
fn work_item(db_opt: Option<Arc<WorldRepository>>, sde_opt: Option<Arc<SdeRegistry>>, ops: PostUnlockOps) -> PostUnlockWorkItem { PostUnlockWorkItem { db_opt, sde_opt, ops, replies: Vec::new(), in_flight_guard: None, work_seq: 0 } }
fn send_intent(sys: &SystemHandle, ctx: &ServerContext, intent: PostUnlockIntent, in_flight: bool) {
    let mut ops = PostUnlockOps::default(); ops.push(intent);
    let item = work_item(Some(Arc::clone(&ctx.db)), Some(Arc::clone(&ctx.sde)), ops);
    if in_flight { let _ = super::send_post_unlock_item(&sys.post_unlock_tx, &sys.post_unlock_in_flight, item); } else { sys.post_unlock_tx.send(PostUnlockMsg::Work(item)).unwrap(); }
}
fn send_in_flight(sys: &SystemHandle, ctx: &ServerContext, ops: PostUnlockOps) { let _ = super::send_post_unlock_item(&sys.post_unlock_tx, &sys.post_unlock_in_flight, work_item(Some(Arc::clone(&ctx.db)), Some(Arc::clone(&ctx.sde)), ops)); }
fn spawn_pos(sys: &SystemHandle, ship_id: i64, pos: DVec3) -> tokio::task::JoinHandle<bool> { let s = sys.clone(); tokio::spawn(async move { s.set_ball_position(ship_id, pos).await }) }
fn pos_of(db: &WorldRepository, id: i64) -> Option<[f64; 3]> { db.with_snapshot(|s| s.item_positions.get(&id).copied()) }
fn mod_dmg(db: &WorldRepository, mid: i64) -> Option<f64> { db.with_snapshot(|s| s.items.get(&mid).and_then(|i| i.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied())) }
fn paste_qty(db: &WorldRepository, sid: i64) -> i32 { db.with_snapshot(|s| s.items.values().filter(|i| i.location_id == sid && i.flag == InventoryFlag::Cargo && i.type_id == 28668).map(|i| i.quantity).sum()) }
fn shortfall_of(db: &WorldRepository, sid: i64, mid: i64) -> Option<i32> { db.with_snapshot(|s| s.sp6_lifecycle.nanite_paste_shortfall.get(&(sid, mid)).copied()) }
async fn push_nanite(sys: &SystemHandle, ship_id: i64, module_id: i64, initial_damage_hp: f64, elapsed_s: f64, paste_total_needed: i32, paste_consumed_so_far: i32) {
    sys.test_with_actor_state_mut_async(move |st| {
        st.active_nanite_repairs.push(ActiveNaniteRepair { ship_id, char_id: CHAR_ID, module_id, initial_damage_hp, duration_s: 10.0, elapsed_s, paste_total_needed, paste_consumed_so_far });
    })
    .await;
}
fn cloak_cyc(ship_id: i64, module_id: i64, duration_s: f64, elapsed_s: f64, repeat: i32) -> ActiveModuleCycle { ActiveModuleCycle { ship_id, owner_char_id: Some(CHAR_ID), module_id, module_type_id: 11578, effect_name: "cloaking".into(), duration_s, elapsed_s, repeat, ..Default::default() } }
fn tick_op(ship_id: i64, module_id: i64, paste_delta: i32, initial_damage_hp: f64, paste_total_needed: i32, rem_dmg: f64, done_now: bool) -> PostUnlockIntent { PostUnlockIntent::NaniteRepairTick(NaniteRepairTickDbOp { char_id: CHAR_ID, ship_id, module_id, paste_delta, initial_damage_hp, paste_total_needed, rem_dmg, done_now, halted_for_paste: false }) }
fn cancel_op(ship_id: i64, module_id: i64, rem_dmg: f64, refund_paste: bool, refund: i32) -> PostUnlockIntent { PostUnlockIntent::NaniteRepairCancel(super::NaniteRepairCancelDbOp { char_id: CHAR_ID, ship_id, module_id, rem_dmg, refund_paste, refund }) }
fn mk_sess(db: &WorldRepository, ship_id: i64) -> eve_machonet::SessionState {
    let uid = db.with_snapshot(|s| s.characters.get(&CHAR_ID).map(|c| c.user_id));
    eve_machonet::SessionState { user_id: uid, character_id: Some(CHAR_ID), corporation_id: Some(1_000_009), solar_system_id: Some(SYS_ID), ship_id: Some(ship_id), ..Default::default() }
}
fn spawn_rpc(ctx: &ServerContext, ship_id: i64, svc: &'static str, m: &'static str, args: Vec<eve_proto::PyRep>) -> tokio::task::JoinHandle<Result<eve_proto::PyRep, eve_proto::BridgeRpcError>> {
    let (c, mut s) = (ctx.clone(), mk_sess(&ctx.db, ship_id));
    tokio::spawn(async move { c.dispatch_rpc(&mut s, svc, None, m, &args, &HashMap::new(), None).await })
}
fn reload_ammo_op(ship_id: i64, module_id: i64, reload_until: u64) -> PostUnlockIntent {
    PostUnlockIntent::CycleAmmoStep(super::CycleAmmoStepDbOp {
        char_id: Some(CHAR_ID), ship_id, linked_turrets: vec![module_id], ammo_type_id: Some(178), missile_charge_type_id: None,
        damage_vector_total: 100.0, crystal_volatility_chance: 0.0, crystal_volatility_damage: 0.0, pre_consumed_mode: false, consume_pre_round_for_next_cycle: false,
        reload_spec: Some(super::CycleReloadDbSpec { ammo_type_id: 178, linked_turrets: vec![module_id], master_module_id: module_id, actual_per_mod: 1, reload_time_ms: 5000.0, reload_until }),
    })
}

/// Ruling Test 1 (`W1`, `W2`): Two producers with DB writer held preserve FIFO in both directions.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ruling_w1_w2_two_producers_actor_and_helper_preserve_order_in_both_directions() {
    let (ship_id, (ctx, sys)) = (778_001_001_i64, boot_ship(778_001_001, &[], None, None).await);
    let (p1, p2, p3, p4) = (DVec3::new(100.0, 0.0, 0.0), DVec3::new(200.0, 0.0, 0.0), DVec3::new(300.0, 0.0, 0.0), DVec3::new(400.0, 0.0, 0.0));
    let held_a = hold_db_writer(&ctx.db);
    let fut_p1 = spawn_pos(&sys, ship_id, p1); wait_until("p1 memory", || sys.get_ball_position_sync(ship_id) == Some(p1)).await;
    let helper_p2 = std::thread::spawn({ let s = sys.clone(); move || s.try_set_ball_position(ship_id, p2) });
    wait_until("p2 memory", || sys.get_ball_position_sync(ship_id) == Some(p2)).await;
    assert!(!fut_p1.is_finished() && !helper_p2.is_finished()); release_writer(held_a);
    assert!(fut_p1.await.unwrap()); helper_p2.join().unwrap(); assert_eq!(pos_of(&ctx.db, ship_id), Some(p2.to_array()));

    let held_b = hold_db_writer(&ctx.db);
    let helper_p3 = std::thread::spawn({ let s = sys.clone(); move || s.try_set_ball_position(ship_id, p3) });
    wait_until("p3 memory", || sys.get_ball_position_sync(ship_id) == Some(p3)).await;
    let fut_p4 = spawn_pos(&sys, ship_id, p4); wait_until("p4 memory", || sys.get_ball_position_sync(ship_id) == Some(p4)).await;
    assert!(!helper_p3.is_finished() && !fut_p4.is_finished()); release_writer(held_b);
    helper_p3.join().unwrap(); assert!(fut_p4.await.unwrap()); assert_eq!(pos_of(&ctx.db, ship_id), Some(p4.to_array()));
}

/// Ruling Test 2 (`W5`): Nanite-repair cancel queued behind an earlier write applies once and strictly after it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ruling_w5_nanite_repair_cancel_queued_behind_earlier_tick_applies_once_and_after_it() {
    let (ship_id, mod_id, paste_id) = (778_002_001_i64, 778_002_002_i64, 778_002_003_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_id, 2048, InventoryFlag::LoSlot0, 120.0)], Some((paste_id, 10)), None).await;
    push_nanite(&sys, ship_id, mod_id, 100.0, 6.0, 10, 10).await;
    let held = hold_db_writer(&ctx.db);
    send_intent(&sys, &ctx, tick_op(ship_id, mod_id, 6, 100.0, 10, 95.0, false), false);
    let cancel_thread = std::thread::spawn({ let s = sys.clone(); move || s.cancel_ship_nanite_repairs_sync(ship_id, true) });
    wait_until("cancel never drained", || sys.view().active_nanite_repairs.is_empty()).await;
    release_writer(held); cancel_thread.join().unwrap();
    assert!(mod_dmg(&ctx.db, mod_id).is_some_and(|v| (v - 40.0).abs() < 1e-6) && paste_qty(&ctx.db, ship_id) == 8);
    sys.cancel_ship_nanite_repairs_sync(ship_id, true); assert_eq!(paste_qty(&ctx.db, ship_id), 8);
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
        let Some(c) = ctx_w.upgrade() else { return; }; let sh = c.systems.get_or_spawn(sid);
        let (mod_active, pos_when) = (db_hook.with_snapshot(|s| s.module_active_states.contains(&mod_a)), pos_of(&db_hook, ship_b));
        sh.try_set_ball_position(ship_b, p_in_hook);
        let rep = sh.try_repair_combat_state_sync(ship_b, 0.0, 0.0, 75.0);
        let _ = hook_tx.send((mod_active, pos_when, rep, pos_of(&db_hook, ship_b)));
    }));
    let held = hold_db_writer(&ctx.db); let fut_before = spawn_pos(&sys, ship_b, p_before);
    wait_until("p_before memory", || sys.get_ball_position_sync(ship_b) == Some(p_before)).await;
    let fut_rem = tokio::spawn({ let s = sys.clone(); async move { s.stop_non_warp_module_cycles(ship_a).await } });
    wait_until("cycle removed", || sys.view().active_module_cycles.is_empty()).await;
    release_writer(held); assert!(fut_before.await.unwrap()); fut_rem.await.unwrap().unwrap();
    let (mod_active, pos_when, rep, pos_after) = hook_rx.recv_timeout(Duration::from_secs(2)).expect("hook must run without deadlock");
    assert!(!mod_active && rep == TryRepairCombatOutcome::Applied && (pos_when, pos_after) == (Some(p_before.to_array()), Some(p_before.to_array())));
    sys.try_set_ball_position(ship_a, p_barrier);
    let (pos_final, hp_final) = (pos_of(&ctx.db, ship_b), ctx.db.with_snapshot(|s| s.sp6_lifecycle.ship_combat_hp.get(&ship_b).copied()));
    assert!(pos_final == Some(p_in_hook.to_array()) && hp_final.is_some_and(|hp| (hp.shield_hp - 175.0).abs() < 1e-6));
}

/// Ruling Test 4 (`W3`): Unlocked helper commits before return; DB-writer holder returns immediately and commits after release.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ruling_w3_unlocked_helper_waits_for_db_while_db_writer_holder_returns_immediately() {
    let (ship_id, mod_a, mod_b, paste_id) = (778_004_001_i64, 778_004_002_i64, 778_004_003_i64, 778_004_004_i64);
    let mut cs = make_combat_state(500.0, 500.0, 500.0, 200.0); cs.shield_hp = 100.0;
    let (ctx, sys) = boot_ship(ship_id, &[(mod_a, 2048, InventoryFlag::LoSlot0, 100.0), (mod_b, 2048, InventoryFlag::LoSlot0, 90.0)], Some((paste_id, 4)), Some(cs)).await;
    let p_unlocked = DVec3::new(500.0, 0.0, 0.0);
    push_nanite(&sys, ship_id, mod_a, 50.0, 0.0, 4, 4).await; sys.try_set_ball_position(ship_id, p_unlocked);
    assert_eq!(sys.try_repair_combat_state_sync(ship_id, 0.0, 0.0, 150.0), TryRepairCombatOutcome::Applied);
    sys.cancel_ship_nanite_repairs_sync(ship_id, true);
    let hp_a = ctx.db.with_snapshot(|s| s.sp6_lifecycle.ship_combat_hp.get(&ship_id).copied());
    assert!(pos_of(&ctx.db, ship_id) == Some(p_unlocked.to_array()) && hp_a.is_some_and(|hp| (hp.shield_hp - 250.0).abs() < 1e-6) && mod_dmg(&ctx.db, mod_a).is_some_and(|d| (d - 50.0).abs() < 1e-6));
    push_nanite(&sys, ship_id, mod_b, 30.0, 0.0, 2, 2).await;
    let inside_elapsed = ctx.db.with_mut_snapshot(|snap| {
        let t0 = std::time::Instant::now(); sys.cancel_ship_nanite_repairs_sync(ship_id, true);
        assert!(snap.items.get(&mod_b).and_then(|it| it.dynamic_attributes.get(&eve_sde::attr_ids::DAMAGE).copied()).is_some_and(|d| (d - 90.0).abs() < 1e-6));
        t0.elapsed()
    });
    assert!(inside_elapsed < Duration::from_millis(250));
    wait_until("writes never committed", || mod_dmg(&ctx.db, mod_b).is_some_and(|d| (d - 30.0).abs() < 1e-6)).await;
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
            snap.items.insert(sid, make_item(sid, 587, cid, i64::from(SYS_ID), InventoryFlag::None, 1)); snap.item_positions.insert(sid, pos.to_array());
            if let Some(c) = snap.characters.get_mut(&cid) { (c.active_ship_id, c.solar_system_id, c.station_id, c.structure_id) = (sid, SYS_ID, None, None); }
        }
    });
    for (cid, sid, pos) in [(CHAR_ID, a_ship, p_atk), (v_char, v_ship, p_vic)] { sys.try_spawn_ball_with_meta(make_ball(sid, 587, pos), ctx.build_ship_combat_state(cid, sid, 587), ctx.build_ship_slim_meta(cid, 587)); }
    sys.add_module_cycle(ActiveModuleCycle { ship_id: a_ship, module_id: mod_id, module_type_id: 486, owner_char_id: Some(CHAR_ID), effect_name: "projectileFired".into(), target_id: Some(v_ship), duration_s: 5.0, damage_vector: eve_destiny::DamageVector::new(100_000.0, 0.0, 0.0, 0.0), ..Default::default() }).await.unwrap();
    let ((hook_tx, hook_rx), db_hook) = (mpsc::channel(), Arc::clone(&ctx.db));
    *sys.npc_death_hook.write().unwrap() = Some(Arc::new(move |_sid, dead_id, _| { let (db_h, tx) = (Arc::clone(&db_hook), hook_tx.clone()); Box::pin(async move { let _ = tx.send((dead_id, pos_of(&db_h, a_ship))); }) }));
    let held = hold_db_writer(&ctx.db); let pos_fut = spawn_pos(&sys, a_ship, p_prior);
    wait_until("in_flight", || sys.has_in_flight_post_unlock()).await;
    let volley_fut = tokio::spawn({ let s = sys.clone(); async move { s.fire_weapon_cycle_volley(a_ship, mod_id).await } });
    wait_until("victim destroyed", || sys.get_ball_position_sync(v_ship).is_none()).await;
    assert!(hook_rx.try_recv().is_err() && !volley_fut.is_finished());
    release_writer(held);
    assert!(pos_fut.await.unwrap() && volley_fut.await.unwrap().unwrap().0.len() == 1);
    assert_eq!(hook_rx.recv_timeout(Duration::from_secs(2)).unwrap(), (v_ship, Some(p_prior.to_array())));
}

/// `BF-3`: Empty-ops `TakeBall`, empty `FlushConditionCheckpoint`, and `apply_step_output` wait FIFO.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf3_empty_ops_take_ball_flush_checkpoint_and_apply_step_output_wait_fifo() {
    let (ship_a, ship_b, p1) = (778_202_001_i64, 778_202_002_i64, DVec3::new(77.0, 88.0, 99.0));
    let (ctx, sys) = boot_ship(ship_a, &[], None, None).await;
    ctx.db.with_mut_snapshot(|s| { s.items.insert(ship_b, make_item(ship_b, 606, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1)); });
    sys.spawn_ball(make_ball(ship_b, 606, DVec3::new(100.0, 0.0, 0.0))).await.unwrap();
    let _ = sys.flush_condition_checkpoint().await.unwrap();
    let held = hold_db_writer(&ctx.db); let pos_fut = spawn_pos(&sys, ship_a, p1);
    wait_until("in_flight", || sys.has_in_flight_post_unlock()).await;
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
    assert_eq!(pos_of(&ctx.db, ship_a), Some(p1.to_array()));
}

async fn pause_post_unlock_worker(sys: &SystemHandle, ship_id: i64, cyc_id: i64) -> (mpsc::Sender<()>, tokio::task::JoinHandle<Result<(), String>>) {
    let ((arr_tx, arr_rx), (go_tx, go_rx)) = (tokio::sync::oneshot::channel(), mpsc::channel());
    let (arr_tx, go_rx) = (std::sync::Mutex::new(Some(arr_tx)), std::sync::Mutex::new(go_rx));
    *sys.module_deactivation_hook.write().unwrap() = Some(Arc::new(move |_, _, _| { if let Some(tx) = arr_tx.lock().unwrap().take() { let _ = tx.send(()); } let _ = go_rx.lock().unwrap().recv(); }));
    sys.add_module_cycle(cloak_cyc(ship_id, cyc_id, 10.0, 0.0, 1)).await.unwrap();
    let stop_fut = tokio::spawn({ let s = sys.clone(); async move { s.stop_non_warp_module_cycles(ship_id).await } });
    arr_rx.await.unwrap(); (go_tx, stop_fut)
}

/// `BF-4` + `BF-1170-1A-01`: Nanite contention defer, partial/full recovery, shortfall cleanup, zero free repair or paste minting.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf4_nanite_repair_paste_conservation_defers_contention_and_prevents_free_repair_or_minting() {
    let (ship_id, mod_a, mod_b, mod_c, cyc_id, paste_id) = (778_203_001_i64, 778_203_002_i64, 778_203_003_i64, 778_203_005_i64, 778_203_099_i64, 778_203_004_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_a, 2048, InventoryFlag::LoSlot0, 100.0), (mod_b, 2048, InventoryFlag::LoSlot0, 90.0), (mod_c, 2048, InventoryFlag::LoSlot0, 50.0)], Some((paste_id, 4)), None).await;
    push_nanite(&sys, ship_id, mod_a, 100.0, 0.0, 10, 0).await;
    let mut nr = sys
        .test_with_actor_state_async(|st| st.active_nanite_repairs[0].clone())
        .await;
    {
        let (_pub, mut vu, mut ops) = (
            ctx.db.hold_published_write_for_test(),
            HashMap::new(),
            PostUnlockOps::default(),
        );
        let res = super::collect_nanite_repair_tick(
            &ctx.db,
            &mut nr,
            4.0,
            0.4,
            4,
            &mut vu,
            &mut ops,
        );
        assert!(
            matches!(res, super::NaniteTickCollect::Defer)
                && ops.is_empty()
                && nr.paste_consumed_so_far == 0
                && nr.elapsed_s.abs() < 1e-9
        );
    }
    let (go_tx, stop_fut) = pause_post_unlock_worker(&sys, ship_id, cyc_id).await;
    let step1 = tokio::spawn({ let s = sys.clone(); async move { s.step_manual_tick_only(4.0).await } });
    wait_until("tick 1 mod_a", || sys.view().active_nanite_repairs.first().is_some_and(|r| r.paste_consumed_so_far == 4)).await;
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
    push_nanite(&sys, ship_id, mod_b, 90.0, 0.0, 10, 0).await;
    let (go_tx, stop_fut) = pause_post_unlock_worker(&sys, ship_id, cyc_id).await;
    let step_b1 = tokio::spawn({ let s = sys.clone(); async move { s.step_manual_tick_only(5.0).await } });
    wait_until("tick 1 mod_b", || sys.view().active_nanite_repairs.first().is_some_and(|r| r.paste_consumed_so_far == 5)).await;
    *sys.module_deactivation_hook.write().unwrap() = None;
    ctx.db.with_mut_snapshot(|s| { s.items.remove(&paste_id); });
    let _ = go_tx.send(()); stop_fut.await.unwrap().unwrap(); step_b1.await.unwrap().unwrap();
    assert_eq!(shortfall_of(&ctx.db, ship_id, mod_b), Some(5));

    ctx.db.with_mut_snapshot(|s| { s.items.insert(paste_id, make_item(paste_id, 28668, CHAR_ID, ship_id, InventoryFlag::Cargo, 10)); });
    sys.step_manual_tick_only(5.0).await.unwrap();
    assert!(paste_qty(&ctx.db, ship_id) == 0 && mod_dmg(&ctx.db, mod_b).is_none() && shortfall_of(&ctx.db, ship_id, mod_b).is_none());

    ctx.db.with_mut_snapshot(|s| { s.sp6_lifecycle.nanite_paste_shortfall.insert((ship_id, mod_c), 2); s.items.insert(paste_id, make_item(paste_id, 28668, CHAR_ID, ship_id, InventoryFlag::Cargo, 3)); });
    let mut ops_c = PostUnlockOps::default(); ops_c.push(tick_op(ship_id, mod_c, 3, 50.0, 5, 0.0, true));
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
    *sys.target_lock_completed_hook.write().unwrap() = Some(Arc::new(move |src, tgt| { let _ = lock_tx.send((src, tgt, eve_db::lock_rank::held_lock_ranks(), pos_of(&db_hook, ship_a))); }));
    sys.add_target_lock(ship_a, eve_destiny::TargetLock { target_id: ship_b, lock_time_ms: 100.0, remaining_lock_ms: 50.0, locked: false, max_range_m: None }).await.unwrap();
    let held = hold_db_writer(&ctx.db);
    let pos_fut = spawn_pos(&sys, ship_a, p1);
    wait_until("in_flight", || sys.has_in_flight_post_unlock()).await;
    let step_fut = tokio::spawn({ let s = sys.clone(); async move { s.step_manual_tick_only(0.2).await } });
    wait_until_async("target locked", || {
        let s = sys.clone();
        async move {
            s.test_with_ballpark_async(move |bp| {
                bp.target_locks
                    .get(&ship_a)
                    .is_some_and(|v| v.iter().any(|l| l.target_id == ship_b && l.locked))
            })
            .await
        }
    })
    .await;
    assert!(lock_rx.try_recv().is_err());
    release_writer(held);
    assert!(pos_fut.await.unwrap()); step_fut.await.unwrap().unwrap();
    let (src, tgt, ranks, pos) = lock_rx.recv_timeout(Duration::from_secs(2)).expect("lock hook");
    assert!((src, tgt, pos) == (CHAR_ID, ship_b, Some(p1.to_array())) && ranks.is_empty());

    push_nanite(&sys, ship_a, mod_b, 30.0, 0.0, 4, 2).await;
    {
        let (held_tx, held_rx) = mpsc::channel();
        let (rel_tx, rel_rx) = mpsc::channel();
        let sys_bg = sys.clone();
        let pauser = std::thread::spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                sys_bg.test_with_actor_state(move |_| {
                    let _ = held_tx.send(());
                    let _ = rel_rx.recv();
                });
            }));
        });
        held_rx.recv().unwrap();
        let st_lock: eve_db::lock_rank::RankedRwLock<(), { eve_db::lock_rank::RANK_ACTOR_STATE }> =
            eve_db::lock_rank::RankedRwLock::new(());
        {
            let _guard = st_lock.read().unwrap();
            sys.cancel_ship_nanite_repairs_sync(ship_a, true);
        }
        let _ = rel_tx.send(());
        pauser.join().unwrap();
    }
    wait_until("WouldBlock cancel", || sys.view().active_nanite_repairs.is_empty()).await;
    assert!(sys.wait_for_post_unlock_barrier().await && (mod_dmg(&ctx.db, mod_b).unwrap() - 30.0).abs() < 1e-6 && paste_qty(&ctx.db, ship_a) == 2);
}

/// `BF-1170-1A-02`: Activation asteroid seeding stages `BeltAsteroidInsertIfAbsent` without DB writes under actor locks.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_seed_asteroid_enqueues_post_unlock_without_db_write() {
    use crate::system_actor::{AsteroidState, SystemActorState, SystemContentProvider, SystemContentSeedContext, SystemWakeReason};
    let (ctx, sys_id, ast_a, ast_b) = (test_ctx(), 30_000_144_u32, 778_205_001_i64, 778_205_002_i64);
    let make_ast = |asteroid_id| AsteroidState { asteroid_id, belt_id: 40_000_001, type_id: 1230, initial_units: 250, remaining_units: 250, unit_volume: 0.1, depleted: false, respawn_at_tick: None };
    let (bp_lock, st_lock) = (
        eve_db::lock_rank::RankedRwLock::<eve_destiny::Ballpark, { eve_db::lock_rank::RANK_BALLPARK }>::new(eve_destiny::Ballpark::new(sys_id)),
        eve_db::lock_rank::RankedRwLock::<SystemActorState, { eve_db::lock_rank::RANK_ACTOR_STATE }>::new(SystemActorState::default()),
    );
    let mut seed_ops = PostUnlockOps::default();
    {
        let (mut bp, mut st) = (bp_lock.write().unwrap(), st_lock.write().unwrap());
        let mut seed_ctx = SystemContentSeedContext {
            solar_system_id: sys_id, wake_reason: &SystemWakeReason::LegacyGetOrSpawn, registry: &ctx.systems, ballpark: &mut bp, actor_state: &mut st, sde: Some(&ctx.sde),
            db: Some(&ctx.db), db_read_safe: true, belt_respawn_interval_secs: 3600, downtime_utc_secs_of_day: 39_600, mechanic_rng: &ctx.mechanic_rng,
            staged_special_site_balls: Default::default(), staged_wh_ore_anomalies: Default::default(), staged_post_unlock_ops: &mut seed_ops,
        };
        seed_ctx.spawn_asteroid(make_ast(ast_a), make_ball(ast_a, 1230, DVec3::ZERO));
    }
    assert!(matches!(seed_ops.intents.as_slice(), [PostUnlockIntent::BeltAsteroidInsertIfAbsent(r)] if r.asteroid_id == ast_a) && ctx.db.with_snapshot(|s| !s.asteroids.contains_key(&ast_a)));
    struct AstProvider(i64, AsteroidState);
    impl SystemContentProvider for AstProvider {
        fn name(&self) -> &'static str { "test_ast_seed" }
        fn seed(&self, sctx: &mut SystemContentSeedContext<'_>) -> Result<(), String> {
            if sctx.solar_system_id == 30_000_144 { sctx.spawn_asteroid(self.1.clone(), make_ball(self.0, 1230, DVec3::new(50.0, 0.0, 0.0))); } Ok(())
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
    assert!(!{ let l = RankedRwLock::<(), RANK_SYSTEM_REGISTRY>::new(()); let _g = l.read().unwrap(); super::can_run_post_unlock_sync_now() }
        && !{ let l = RankedRwLock::<(), RANK_CRIMEWATCH>::new(()); let _g = l.read().unwrap(); super::can_run_post_unlock_sync_now() }
        && !{ let l = RankedRwLock::<(), RANK_SERVICE_STATE>::new(()); let _g = l.read().unwrap(); super::can_run_post_unlock_sync_now() });
    let (ship_id, mod_a, mod_b) = (778_206_001_i64, 778_206_002_i64, 778_206_003_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_a, 11578, InventoryFlag::HiSlot0, 0.0), (mod_b, 2048, InventoryFlag::LoSlot0, 0.0)], None, Some(make_combat_state(500.0, 500.0, 500.0, 500.0))).await;
    ctx.db.with_mut_snapshot(|s| { s.module_active_states.insert(mod_a); });
    sys.add_module_cycle(cloak_cyc(ship_id, mod_a, 1.0, 0.9, 0)).await.unwrap();
    let ((hook_tx, hook_rx), db_h) = (mpsc::channel(), Arc::clone(&ctx.db));
    *sys.module_deactivation_hook.write().unwrap() = Some(Arc::new(move |_, _, cyc| { let _ = hook_tx.send((cyc.module_id, db_h.with_snapshot(|s| !s.module_active_states.contains(&cyc.module_id)))); }));
    let proj = ctx.build_fit_projection(CHAR_ID, ship_id, eve_dogma::FitRev(1)).unwrap();
    let held = hold_db_writer(&ctx.db);
    let step_out = sys.ask_step_tick_only(0.2).await.unwrap();
    wait_until("tick cleanup", || sys.view().active_module_cycles.is_empty() && sys.has_in_flight_post_unlock()).await;
    assert!(hook_rx.try_recv().is_err() && ctx.db.with_snapshot(|s| s.module_active_states.contains(&mod_a)));
    let rec_fut = tokio::spawn({ let s = sys.clone(); async move { s.reconcile_module_cycle_before_activate(ship_id, mod_a, 1_000).await } });
    let act_fut = tokio::spawn({ let s = sys.clone(); async move { s.activate_module_cycle_atomic(AtomicActivateCycleRequest {
        char_id: CHAR_ID, allow_while_warping: false, allow_while_cloaked: true, replace_existing_cycle: false, break_protection: false, break_reason: ProtectionBreakReason::ModuleActivation, require_ball_in_space: true, real_now_unix: 1_000,
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
    try_rem.join().unwrap(); sys.apply_step_output(&step_out).await;
    assert_eq!(hook_rx.recv_timeout(Duration::from_secs(2)).unwrap(), (mod_a, true));
}

/// `BF-1170-1A-04`: `set_combat_state_in_actor_collecting` enforces `expected_system: Some(solar_system_id)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_set_combat_state_checks_expected_system() {
    let (sys_b_id, ship_id) = (30_000_144_u32, 778_207_001_i64);
    let (ctx, sys_a) = boot_ship(ship_id, &[], None, Some(make_combat_state(500.0, 500.0, 500.0, 200.0))).await;
    ctx.db.with_mut_snapshot(|snap| { snap.items.get_mut(&ship_id).unwrap().location_id = i64::from(sys_b_id); snap.characters.get_mut(&CHAR_ID).unwrap().solar_system_id = sys_b_id; snap.sp6_lifecycle.ship_combat_hp.insert(ship_id, crate::sp6_lifecycle::PersistedCombatHp { shield_hp: 123.0, armor_hp: 234.0, hull_hp: 345.0 }); });
    let mut stale_cs = make_combat_state(500.0, 500.0, 500.0, 200.0); stale_cs.shield_hp = 490.0;
    sys_a.set_combat_state(ship_id, stale_cs).await.unwrap();
    assert!((ctx.db.with_snapshot(|s| s.sp6_lifecycle.ship_combat_hp.get(&ship_id).copied()).unwrap().shield_hp - 123.0).abs() < 1e-6);
}

/// `BF-1170-1A-REV-01`: `InitiateModuleRepairMany` waits behind in-flight `NaniteRepairTick`/`NaniteRepairCancel`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_rev01_initiate_module_repair_many_waits_behind_in_flight_nanite() {
    use crate::services::inventory_fitting::fitting_dogma_ops::handle_dogma_ops;
    use eve_proto::PyRep;
    let (ship_id, mod_id, paste_id) = (778_208_001_i64, 778_208_002_i64, 778_208_003_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_id, 2048, InventoryFlag::LoSlot0, 20.0)], Some((paste_id, 10)), None).await;
    let call_repair = || { let (c, mut s) = (ctx.clone(), mk_sess(&ctx.db, ship_id)); tokio::spawn(async move { handle_dogma_ops(&c, &mut s, None, "dogmaIM", "InitiateModuleRepairMany", &[PyRep::List(vec![PyRep::Integer(mod_id)])], &HashMap::new()).await }) };
    let held1 = hold_db_writer(&ctx.db);
    send_intent(&sys, &ctx, tick_op(ship_id, mod_id, 0, 20.0, 2, 0.0, true), false);
    let rpc1 = call_repair(); tokio::time::sleep(Duration::from_millis(25)).await; assert!(!rpc1.is_finished());
    release_writer(held1);
    assert_eq!(rpc1.await.unwrap().unwrap().unwrap_err().code, "NoDamagedModulesToRepair");

    ctx.db.with_mut_snapshot(|s| { s.items.get_mut(&mod_id).unwrap().dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 30.0); s.items.get_mut(&paste_id).unwrap().quantity = 4; s.sp6_lifecycle.nanite_paste_shortfall.insert((ship_id, mod_id), 2); });
    let held2 = hold_db_writer(&ctx.db);
    send_intent(&sys, &ctx, cancel_op(ship_id, mod_id, 15.0, true, 5), false);
    let rpc2 = call_repair(); tokio::time::sleep(Duration::from_millis(25)).await; assert!(!rpc2.is_finished());
    release_writer(held2);
    assert!(matches!(rpc2.await.unwrap().unwrap().unwrap(), PyRep::Tuple(ref p) if p.first() == Some(&PyRep::List(vec![PyRep::Integer(mod_id)]))) && paste_qty(&ctx.db, ship_id) == 6);

    sys.cancel_ship_nanite_repairs_sync(ship_id, false);
    ctx.db.with_mut_snapshot(|s| { s.items.get_mut(&mod_id).unwrap().dynamic_attributes.insert(eve_sde::attr_ids::DAMAGE, 30.0); });
    let held3 = hold_db_writer(&ctx.db);
    send_intent(&sys, &ctx, cancel_op(ship_id, mod_id, 15.0, false, 0), false);
    let rpc3 = call_repair(); tokio::time::sleep(Duration::from_millis(25)).await; assert!(!rpc3.is_finished());
    release_writer(held3);
    assert!(matches!(rpc3.await.unwrap().unwrap().unwrap(), PyRep::Tuple(ref p) if p.first() == Some(&PyRep::List(vec![PyRep::Integer(mod_id)]))));
    assert_eq!(
        sys.test_with_actor_state_async(move |st| {
            st.active_nanite_repairs.iter().find(|r| r.module_id == mod_id).map(|r| r.initial_damage_hp)
        })
        .await,
        Some(15.0)
    );
}

/// `BF-1170-1A-REV-02`: Closed `post_unlock_tx` skips `RunDirectAfterUnlock` when ranked locks are held.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_rev02_closed_worker_under_ranked_locks_skips_direct_db_and_actor_write() {
    use eve_db::lock_rank::{RankedRwLock, RANK_LEAF};
    let (ship_a, mod_a, ship_b, mod_b, ship_c, mod_c, paste_c) = (778_209_001_i64, 778_209_002_i64, 778_209_003_i64, 778_209_004_i64, 778_209_005_i64, 778_209_006_i64, 778_209_007_i64);
    let (ctx, sys) = boot_ship(ship_a, &[(mod_a, 2048, InventoryFlag::LoSlot0, 90.0)], None, None).await;
    seed_ship_char(&ctx.db, SYS_ID, CHAR_ID, ship_b, &[(mod_b, 2048, InventoryFlag::LoSlot0, 90.0)], None);
    seed_ship_char(&ctx.db, SYS_ID, CHAR_ID, ship_c, &[(mod_c, 2048, InventoryFlag::LoSlot0, 90.0)], Some((paste_c, 2)));
    for (sid, mid) in [(ship_a, mod_a), (ship_b, mod_b), (ship_c, mod_c)] { if sid != ship_a { sys.spawn_ball(make_ball(sid, 606, DVec3::ZERO)).await.unwrap(); } push_nanite(&sys, sid, mid, 30.0, 0.0, 4, 2).await; }
    let _ = sys.post_unlock_tx.send(PostUnlockMsg::Shutdown);
    wait_until("tx closed", || sys.post_unlock_tx.is_closed()).await;
    ctx.db.with_mut_snapshot(|snap| { sys.cancel_ship_nanite_repairs_sync(ship_a, true); sys.try_remove_ball(ship_a); assert!((snap.items[&mod_a].dynamic_attributes[&eve_sde::attr_ids::DAMAGE] - 90.0).abs() < 1e-6); });
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
    let held = hold_db_writer(&ctx.db); let pos_fut = spawn_pos(&sys, a_ship, p_prior);
    wait_until("in_flight", || sys.has_in_flight_post_unlock()).await;
    let volley_fut = tokio::spawn({ let s = sys.clone(); async move { s.fire_weapon_cycle_volley(a_ship, mod_id).await } });
    tokio::time::sleep(Duration::from_millis(25)).await; assert!(!volley_fut.is_finished());
    release_writer(held);
    assert!(pos_fut.await.unwrap() && volley_fut.await.unwrap().unwrap().0.is_empty() && pos_of(&ctx.db, a_ship) == Some(p_prior.to_array()));
    let reg_rt = ctx.systems.clone();
    let h = std::thread::spawn(move || { let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap(); let h = rt.block_on(async move { reg_rt.get_or_spawn(30_000_144) }); drop(rt); h }).join().unwrap();
    wait_until("tx closed after Runtime::drop", || h.post_unlock_tx.is_closed()).await;
}

/// `BF-1170-1A-R6-01` (1/4): `p0 -> p1 -> p0` (`set_ball_position`) under held DB writer never skips `p0`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r6_01_p0_p1_p0_set_ball_position_under_held_writer() {
    let (ship_id, p0, p1) = (778_211_001_i64, DVec3::new(10.0, 0.0, 0.0), DVec3::new(500.0, 0.0, 0.0));
    let (ctx, sys) = boot_ship(ship_id, &[], None, None).await;
    ctx.db.with_mut_snapshot(|s| { s.item_positions.insert(ship_id, p0.to_array()); }); sys.set_ball_position(ship_id, p0).await;
    let held = hold_db_writer(&ctx.db);
    let f1 = spawn_pos(&sys, ship_id, p1); wait_until("p1", || sys.get_ball_position_sync(ship_id) == Some(p1)).await;
    let f2 = spawn_pos(&sys, ship_id, p0); wait_until("p0", || sys.get_ball_position_sync(ship_id) == Some(p0)).await;
    release_writer(held);
    assert!(f1.await.unwrap() && f2.await.unwrap() && pos_of(&ctx.db, ship_id) == Some(p0.to_array()));
}

/// `BF-1170-1A-R6-01` (2/4): TiDi stretch then relax while `ModuleReload` is queued clears stretched DB deadline.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r6_01_tidi_stretch_then_relax_while_queued() {
    let (ctx, mod_id) = (test_ctx(), 778_212_101_i64);
    let init_until = ctx.db.clock().now_unix_secs() + 12;
    ctx.db.with_mut_snapshot(|s| { s.cluster_and_runtime.module_reload_until_unix.insert(mod_id, init_until); });
    let sys = ctx.systems.get_or_spawn(SYS_ID);
    sys.test_with_actor_state_mut_async(move |st| {
        st.module_reload_sim_remaining.insert(mod_id, super::super::types::ModuleReloadSimEntry { remaining_sim_s: 12.0, min_wall_until_unix: init_until, tidi_stretched: false });
    })
    .await;
    let held = hold_db_writer(&ctx.db); let _ = sys.set_tidi_factor_raw(0.5);
    let t1 = std::thread::spawn({ let s = sys.clone(); move || s.try_step_full_tick(1.0) });
    wait_until_async("tidi stretch", || {
        let s = sys.clone();
        async move {
            s.test_with_actor_state_async(move |st| st.module_reload_sim_remaining.get(&mod_id).is_some_and(|e| e.tidi_stretched))
                .await
        }
    })
    .await;
    let _ = sys.set_tidi_factor_raw(1.0);
    let t2 = std::thread::spawn({ let s = sys.clone(); move || s.try_step_full_tick(1.0) });
    wait_until_async("tidi relax", || {
        let s = sys.clone();
        async move {
            s.test_with_actor_state_async(move |st| st.module_reload_sim_remaining.get(&mod_id).is_some_and(|e| !e.tidi_stretched))
                .await
        }
    })
    .await;
    release_writer(held); t1.join().unwrap(); t2.join().unwrap();
    assert!(ctx.db.with_snapshot(|s| s.cluster_and_runtime.module_reload_until_unix.get(&mod_id).copied()).unwrap() <= init_until + 4);
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
    release_writer(held); t1.join().unwrap(); t2.join().unwrap();
    assert!((ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&mod_id).and_then(|m| m.get(&267).copied())).unwrap() - 0.73).abs() < 1e-4);
}

/// `BF-1170-1A-R6-01` (4/4): `ShipCycleClearAll` behind queued `CycleAmmoStep` clears DB charge.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r6_01_ship_cycle_clear_all_behind_queued_cycle_ammo_step() {
    let (ship_id, mod_id, ammo_id) = (778_214_001_i64, 778_214_101_i64, 778_214_201_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_id, 484, InventoryFlag::HiSlot0, 0.0)], None, None).await;
    ctx.db.with_mut_snapshot(|s| { s.items.insert(ammo_id, make_item(ammo_id, 178, CHAR_ID, ship_id, InventoryFlag::Cargo, 10)); });
    sys.add_module_cycle(cloak_cyc(ship_id, mod_id, 10.0, 0.0, 0)).await.unwrap();
    let held = hold_db_writer(&ctx.db);
    send_intent(&sys, &ctx, reload_ammo_op(ship_id, mod_id, 999), true);
    let rm_fut = tokio::spawn({ let s = sys.clone(); async move { s.remove_ball(ship_id).await } });
    wait_until("remove_ball cleared actor", || sys.get_ball_position_sync(ship_id).is_none()).await;
    assert!(!rm_fut.is_finished()); release_writer(held); assert!(rm_fut.await.unwrap().is_some());
    ctx.db.with_snapshot(|s| { assert!(s.items[&ammo_id].quantity == 9 && !s.module_active_charges.contains_key(&mod_id) && !s.module_active_states.contains(&mod_id)); });
}

/// `BF-1170-1A-R6-02`: Empty-ops `flush_dirty_condition_checkpoint_sync` and absent-ball `modify_combat_state` wait FIFO.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r6_02_empty_ops_flush_checkpoint_and_modify_combat_absent_wait_fifo() {
    let (ship_id, p0) = (778_215_001_i64, DVec3::new(321.0, 0.0, 0.0));
    let (ctx, sys) = boot_ship(ship_id, &[], None, None).await;
    sys.set_ball_position(ship_id, p0).await; let _ = sys.flush_condition_checkpoint().await.unwrap();
    ctx.db.with_mut_snapshot(|s| { s.item_positions.remove(&ship_id); });
    let held = hold_db_writer(&ctx.db); let pos_fut = spawn_pos(&sys, ship_id, p0);
    wait_until("in-flight", || sys.has_in_flight_post_unlock()).await;
    let flush_h = std::thread::spawn({ let s = sys.clone(); move || s.flush_dirty_condition_checkpoint_sync() });
    let mod_fut = tokio::spawn({ let s = sys.clone(); async move { s.modify_combat_state(ship_id + 999, |_| {}).await } });
    tokio::time::sleep(Duration::from_millis(25)).await; assert!(!flush_h.is_finished() && !mod_fut.is_finished());
    release_writer(held);
    assert!(flush_h.join().unwrap() == 0 && mod_fut.await.unwrap().unwrap().is_none() && pos_fut.await.unwrap() && pos_of(&ctx.db, ship_id) == Some(p0.to_array()));
}

/// `BF-1170-1A-R6-03`: Halting mid-repair for paste shortage clears `nanite_paste_shortfall` even when `paste_delta > 0`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r6_03_nanite_halt_for_paste_clears_shortfall() {
    let (ship_id, mod_id, paste_id) = (778_216_001_i64, 778_216_101_i64, 778_216_201_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_id, 2048, InventoryFlag::LoSlot0, 100.0)], Some((paste_id, 2)), None).await;
    ctx.db.with_mut_snapshot(|s| { s.sp6_lifecycle.nanite_paste_shortfall.insert((ship_id, mod_id), 1); });
    push_nanite(&sys, ship_id, mod_id, 100.0, 0.0, 10, 0).await;
    std::thread::spawn({ let s = sys.clone(); move || s.try_step_full_tick(5.0) }).join().unwrap();
    assert!(sys.view().active_nanite_repairs.is_empty() && shortfall_of(&ctx.db, ship_id, mod_id).is_none() && paste_qty(&ctx.db, ship_id) == 0 && mod_dmg(&ctx.db, mod_id).unwrap() < 100.0);
}

/// `BF-1170-1A-R7-01`: `RepairTetherModuleHeal` behind in-flight `ModuleBurnout` and `FastForwardClock` `p0 -> p1 -> p0` for non-ego ball.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r7_01_tether_heal_behind_burnout_and_fast_forward_non_ego_p0_p1_p0() {
    let (ship_id, mod_id, ego_id, non_ego_id) = (778_217_001_i64, 778_217_101_i64, 778_217_002_i64, 778_217_003_i64);
    let (p0, p1) = (DVec3::new(100.0, 0.0, 0.0), DVec3::new(300_000.0, 0.0, 0.0));
    let mut cs_tether = make_combat_state(100.0, 100.0, 100.0, 100.0); cs_tether.is_tethered = true;
    let (ctx, sys) = boot_ship(ship_id, &[(mod_id, 2048, InventoryFlag::LoSlot0, 0.0)], None, Some(cs_tether)).await;
    ctx.db.with_mut_snapshot(|s| { for (id, pos) in [(ego_id, DVec3::ZERO), (non_ego_id, p0)] { s.items.insert(id, make_item(id, 606, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1)); s.item_positions.insert(id, pos.to_array()); } });
    let mut ego_ball = make_ball(ego_id, 606, DVec3::ZERO); ego_ball.speed_fraction = 1.0;
    let mut non_ego_ball = make_ball(non_ego_id, 606, p0); non_ego_ball.max_velocity = 0.0;
    sys.spawn_ball(ego_ball).await.unwrap(); sys.spawn_ball(non_ego_ball).await.unwrap();
    sys.set_movement_mode(ego_id, MovementMode::GotoDirection { direction: DVec3::X }).await.unwrap();
    let warp_to = |src, dst| MovementMode::Warp { origin: src, destination: dst, warp_speed_au_s: 3.0, total_distance: DVec3::distance(src, dst), phase: eve_destiny::WarpPhase::Accelerating, elapsed_warp_time: 0.0 };
    sys.set_movement_mode(non_ego_id, warp_to(p0, p1)).await.unwrap();
    let tick0 = sys.view().current_tick;
    let held = hold_db_writer(&ctx.db);
    send_intent(&sys, &ctx, PostUnlockIntent::ModuleBurnout { ship_id, module_id: mod_id }, true);
    let tick_h = std::thread::spawn({ let s = sys.clone(); move || s.try_step_full_tick(1.0) });
    wait_until("tether step", || sys.view().current_tick > tick0).await;
    let ff1 = tokio::spawn({ let s = sys.clone(); async move { s.fast_forward_clock(ego_id, 40).await } });
    wait_until("non_ego -> p1", || sys.get_ball_position_sync(non_ego_id).is_some_and(|p| p.distance(p1) < 1e-2)).await;
    let mv_fut = tokio::spawn({ let s = sys.clone(); let m = warp_to(p1, p0); async move { s.set_movement_mode(non_ego_id, m).await } });
    wait_until("non_ego warp back", || matches!(sys.view().ball(non_ego_id).map(|b| &b.mode), Some(MovementMode::Warp { .. }))).await;
    let ff2 = tokio::spawn({ let s = sys.clone(); async move { s.fast_forward_clock(ego_id, 40).await } });
    wait_until("non_ego -> p0", || sys.get_ball_position_sync(non_ego_id).is_some_and(|p| p.distance(p0) < 1e-2)).await;
    assert!(!tick_h.is_finished() && !ff1.is_finished() && !ff2.is_finished());
    release_writer(held);
    tick_h.join().unwrap(); ff1.await.unwrap().unwrap(); mv_fut.await.unwrap().unwrap(); ff2.await.unwrap().unwrap();
    assert!(mod_dmg(&ctx.db, mod_id) == Some(38.0) && (DVec3::from_array(pos_of(&ctx.db, non_ego_id).unwrap()) - p0).length() < 1e-2);
}

/// `BF-1170-1A-R7-02`: Empty `cancel_ship_nanite_repairs_sync` waits behind tick; dropped/shutdown barrier returns `false`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r7_02_empty_cancel_nanite_waits_and_dropped_barrier_returns_false() {
    let (ship_id, mod_id) = (778_218_001_i64, 778_218_101_i64);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_id, 2048, InventoryFlag::LoSlot0, 20.0)], None, None).await;
    let held = hold_db_writer(&ctx.db);
    send_intent(&sys, &ctx, tick_op(ship_id, mod_id, 0, 20.0, 2, 0.0, true), true);
    wait_until("in-flight", || sys.has_in_flight_post_unlock()).await;
    let cancel_h = std::thread::spawn({ let s = sys.clone(); move || s.cancel_ship_nanite_repairs_sync(ship_id, true) });
    tokio::time::sleep(Duration::from_millis(25)).await; assert!(!cancel_h.is_finished());
    release_writer(held); cancel_h.join().unwrap(); assert!(mod_dmg(&ctx.db, mod_id).is_none());
    let (mut h_drop, (tx_d, mut rx_d)) = (sys.clone(), tokio::sync::mpsc::unbounded_channel());
    h_drop.post_unlock_tx = tx_d;
    let bar_fut = tokio::spawn(async move { h_drop.wait_for_post_unlock_barrier().await });
    drop(rx_d.recv().await.unwrap()); assert!(!bar_fut.await.unwrap());
    let _ = sys.post_unlock_tx.send(PostUnlockMsg::Shutdown);
    wait_until("tx closed", || sys.post_unlock_tx.is_closed()).await;
    assert!(!sys.wait_for_post_unlock_barrier().await);
}

/// `BF-1170-1A-R8-01`: `take_ball` and `take_ball_for_docking` never block `current_thread` Tokio workers and drain queued `StartNaniteRepair`.
#[tokio::test]
async fn bf_1170_r8_01_take_ball_nonblocking_on_current_thread_and_drains_nanite() {
    let (ship_a, mod_a, paste_a, ship_b, mod_b, paste_b) = (778_219_001_i64, 778_219_101_i64, 778_219_201_i64, 778_219_002_i64, 778_219_102_i64, 778_219_202_i64);
    let (ctx, sys) = boot_ship(ship_a, &[(mod_a, 2048, InventoryFlag::LoSlot0, 100.0)], Some((paste_a, 10)), None).await;
    seed_ship_char(&ctx.db, SYS_ID, CHAR_ID, ship_b, &[(mod_b, 2048, InventoryFlag::LoSlot0, 100.0)], Some((paste_b, 10)));
    sys.spawn_ball(make_ball(ship_b, 606, DVec3::new(100.0, 0.0, 0.0))).await.unwrap();
    let held = hold_db_writer(&ctx.db);
    send_intent(&sys, &ctx, PostUnlockIntent::ModuleBurnout { ship_id: ship_a, module_id: mod_a }, true);
    for (sid, mid) in [(ship_a, mod_a), (ship_b, mod_b)] { sys.start_nanite_repair(ActiveNaniteRepair { ship_id: sid, char_id: CHAR_ID, module_id: mid, initial_damage_hp: 100.0, duration_s: 10.0, elapsed_s: 6.0, paste_total_needed: 10, paste_consumed_so_far: 10 }).await.unwrap(); }
    let take_h = tokio::spawn({ let s = sys.clone(); async move { s.take_ball(ship_a).await } });
    let dock_h = tokio::spawn({ let s = sys.clone(); async move { s.take_ball_for_docking(ship_b, 60_003_760, DVec3::ZERO).await } });
    wait_until("TakeBall drained", || sys.get_ball_position_sync(ship_a).is_none() && sys.get_ball_position_sync(ship_b).is_none() && sys.view().active_nanite_repairs.is_empty()).await;
    assert!(!take_h.is_finished() && !dock_h.is_finished()); release_writer(held);
    assert!(take_h.await.unwrap().is_some() && dock_h.await.unwrap().is_some());
    assert!(mod_dmg(&ctx.db, mod_a).is_some_and(|d| (d - 40.0).abs() < 1e-6) && paste_qty(&ctx.db, ship_a) == 14);
    assert!(mod_dmg(&ctx.db, mod_b).is_some_and(|d| (d - 40.0).abs() < 1e-6) && paste_qty(&ctx.db, ship_b) == 14);
}

async fn add_tank_cycles(sys: &SystemHandle, ship_id: i64, tgt: i64, m_mis: i64, m_asb: i64, m_aar: i64, m_cap: i64, repeat: i32, dmg: f64) {
    for cyc in [
        ActiveModuleCycle { ship_id, owner_char_id: Some(CHAR_ID), module_id: m_mis, module_type_id: 2410, effect_name: "missileLaunching".into(), target_id: Some(tgt), duration_s: 5.0, repeat, linked_turrets: vec![m_mis], weapon_is_guided_missile: true, missile_charge_type_id: Some(209), ammo_type_id: Some(209), crystal_volatility_chance: -1.0, reload_remaining_sim_s: 0.0005, damage_vector: eve_destiny::DamageVector::new(dmg, 0.0, 0.0, 0.0), ..Default::default() },
        ActiveModuleCycle { ship_id, owner_char_id: Some(CHAR_ID), module_id: m_asb, module_type_id: 32772, effect_name: "shieldBoosting".into(), duration_s: 5.0, repeat, is_asb: true, asb_fallback_cap_need: 50.0, ..Default::default() },
        ActiveModuleCycle { ship_id, owner_char_id: Some(CHAR_ID), module_id: m_aar, module_type_id: 33076, effect_name: "armorRepair".into(), duration_s: 5.0, repeat, is_aar: true, aar_base_armor_repair: 20.0, aar_paste_rate: 1, armor_repair_amount: 60.0, ..Default::default() },
        ActiveModuleCycle { ship_id, owner_char_id: Some(CHAR_ID), module_id: m_cap, module_type_id: 2022, effect_name: "capacitorInject".into(), duration_s: 5.0, repeat, cap_booster_inject_amount: 50.0, ..Default::default() },
    ] { sys.add_module_cycle(cyc).await.unwrap(); }
}
fn add_turret_cyc(ship_id: i64, tgt: i64, mid: i64, repeat: i32, dmg: f64) -> ActiveModuleCycle { ActiveModuleCycle { ship_id, owner_char_id: Some(CHAR_ID), module_id: mid, module_type_id: 484, effect_name: "projectileFired".into(), target_id: Some(tgt), duration_s: 5.0, repeat, linked_turrets: vec![mid], ammo_type_id: Some(178), damage_vector: eve_destiny::DamageVector::new(dmg, 0.0, 0.0, 0.0), ..Default::default() } }

/// `BF-1170-1A-R8-02`: Cross-tick reservations + in-space `UnloadAmmo` / `invbroker.Add` barriers.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r8_02_cross_tick_charge_reservations_and_rpc_unload_barrier() {
    use eve_proto::PyRep;
    let (ship_id, m_tur2, m_tur, m_mis, m_asb, m_aar, m_cap) = (778_220_001_i64, 778_220_101, 778_220_102, 778_220_103, 778_220_104, 778_220_105, 778_220_106);
    let mods = [(m_tur2, 484, InventoryFlag::HiSlot0, 0.0), (m_tur, 484, InventoryFlag::HiSlot1, 0.0), (m_mis, 2410, InventoryFlag::HiSlot2, 0.0), (m_asb, 32772, InventoryFlag::MedSlot0, 0.0), (m_aar, 33076, InventoryFlag::LoSlot0, 0.0), (m_cap, 2022, InventoryFlag::LoSlot1, 0.0)];
    let mut cs = make_combat_state(100.0, 100.0, 100.0, 200.0); cs.capacitor = 0.0; cs.capacitor_recharge_time_ms = 0.0;
    let (ctx, sys) = boot_ship(ship_id, &mods, None, Some(cs)).await;
    ctx.db.with_mut_snapshot(|s| { for (cid, tid, flag, q) in [(778_220_201, 178, InventoryFlag::HiSlot0, 1), (778_220_202, 178, InventoryFlag::HiSlot1, 2), (778_220_203, 178, InventoryFlag::Cargo, 1), (778_220_204, 209, InventoryFlag::HiSlot2, 1), (778_220_205, 11289, InventoryFlag::MedSlot0, 1), (778_220_206, 28668, InventoryFlag::LoSlot0, 1), (778_220_207, 11289, InventoryFlag::LoSlot1, 1)] { s.items.insert(cid, make_item(cid, tid, CHAR_ID, ship_id, flag, q)); } });
    sys.spawn_ball(make_ball(778_220_999, 606, DVec3::new(1000.0, 0.0, 0.0))).await.unwrap();
    for mid in [m_tur2, m_tur] { sys.add_module_cycle(add_turret_cyc(ship_id, 778_220_999, mid, 1, 50.0)).await.unwrap(); }
    add_tank_cycles(&sys, ship_id, 778_220_999, m_mis, m_asb, m_aar, m_cap, 1, 50.0).await;

    let held = hold_db_writer(&ctx.db);
    let _ = sys.ask_step_tick_only(5.0).await.unwrap(); let _ = sys.ask_step_tick_only(5.0).await.unwrap();
    let st_cap = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    let (st_missiles, cyc_len, aar_rep) = sys
        .test_with_actor_state_async(move |st| {
            (
                st.active_guided_missiles.len(),
                st.active_module_cycles.len(),
                st.active_module_cycles.iter().find(|c| c.module_id == m_aar).map(|c| c.armor_repair_amount),
            )
        })
        .await;
    assert!((st_cap - 50.0).abs() < 1e-6 && st_missiles == 1 && aar_rep == Some(20.0) && cyc_len == 2);

    let unload_fut = spawn_rpc(&ctx, ship_id, "dogmaIM", "UnloadAmmo", vec![PyRep::Integer(ship_id), PyRep::PySet(vec![PyRep::Integer(m_tur)])]);
    let add_fut = spawn_rpc(&ctx, ship_id, "invbroker", "Add", vec![]);
    tokio::time::sleep(Duration::from_millis(25)).await; assert!(!unload_fut.is_finished() && !add_fut.is_finished());
    release_writer(held);
    let _ = unload_fut.await.unwrap(); let _ = add_fut.await.unwrap();
    ctx.db.with_snapshot(|s| {
        assert!([778_220_202, 778_220_203, 778_220_204, 778_220_205, 778_220_206, 778_220_207].iter().all(|id| !s.items.contains_key(id)));
        assert_eq!(s.items.values().filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot0 && i.item_id != m_tur2).map(|i| i.quantity).sum::<i32>(), 1);
        assert!(!s.items.values().any(|i| i.location_id == ship_id && i.flag == InventoryFlag::HiSlot1 && i.item_id != m_tur));
    });
}

/// `BF-1170-1A-R9-01`: Tick 2 inside post-commit / pre-hook window uses snapshot-coupled `post_unlock_committed_seq` with zero double deduction.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r9_01_post_commit_hook_window_no_double_deduction() {
    let (ship_id, m_tur, m_mis, m_asb, m_aar, m_cap, tgt) = (778_221_001_i64, 778_221_101, 778_221_102, 778_221_103, 778_221_104, 778_221_105, 778_221_999);
    let mods = [(m_tur, 484, InventoryFlag::HiSlot0, 0.0), (m_mis, 2410, InventoryFlag::HiSlot1, 0.0), (m_asb, 32772, InventoryFlag::MedSlot0, 0.0), (m_aar, 33076, InventoryFlag::LoSlot0, 0.0), (m_cap, 2022, InventoryFlag::LoSlot1, 0.0)];
    let mut cs = make_combat_state(100.0, 200.0, 100.0, 200.0); cs.armor_hp = 50.0; cs.capacitor = 0.0; cs.capacitor_recharge_time_ms = 0.0;
    let (ctx, sys) = boot_ship(ship_id, &mods, None, Some(cs)).await;
    ctx.db.with_mut_snapshot(|s| { for (cid, tid, flag, q) in [(778_221_201, 178, InventoryFlag::HiSlot0, 3), (778_221_202, 178, InventoryFlag::Cargo, 5), (778_221_203, 209, InventoryFlag::HiSlot1, 2), (778_221_204, 11289, InventoryFlag::MedSlot0, 2), (778_221_205, 28668, InventoryFlag::LoSlot0, 2), (778_221_206, 11289, InventoryFlag::LoSlot1, 2)] { s.items.insert(cid, make_item(cid, tid, CHAR_ID, ship_id, flag, q)); } });
    sys.spawn_ball(make_ball(tgt, 606, DVec3::new(50_000.0, 0.0, 0.0))).await.unwrap();
    sys.add_module_cycle(add_turret_cyc(ship_id, tgt, m_tur, 2, 10.0)).await.unwrap();
    add_tank_cycles(&sys, ship_id, tgt, m_mis, m_asb, m_aar, m_cap, 2, 10.0).await;

    let ((arr_tx, arr_rx), (go_tx, go_rx)) = (tokio::sync::oneshot::channel(), mpsc::channel());
    let (arr_tx, go_rx) = (std::sync::Mutex::new(Some(arr_tx)), std::sync::Mutex::new(go_rx));
    *sys.target_lock_completed_hook.write().unwrap() = Some(Arc::new(move |_, _| { if let Some(tx) = arr_tx.lock().unwrap().take() { let _ = tx.send(()); } let _ = go_rx.lock().unwrap().recv(); }));
    sys.add_target_lock(ship_id, eve_destiny::TargetLock { target_id: tgt, lock_time_ms: 100.0, remaining_lock_ms: 50.0, locked: false, max_range_m: None }).await.unwrap();
    let _ = sys.ask_step_tick_only(5.0).await.unwrap(); arr_rx.await.unwrap();
    *sys.target_lock_completed_hook.write().unwrap() = None;
    let _ = sys.ask_step_tick_only(5.0).await.unwrap();
    let st_cap = sys.get_combat_state(ship_id).await.unwrap().unwrap().capacitor;
    let (cyc_len, mis_len, pend_mis, reload_empty, asb_need, aar_rep) = sys
        .test_with_actor_state_async(move |st| {
            (
                st.active_module_cycles.len(),
                st.active_guided_missiles.len(),
                st.pending_missile_launches.len(),
                st.module_reload_sim_remaining.is_empty(),
                st.active_module_cycles.iter().find(|c| c.module_id == m_asb).map(|c| c.cap_need_per_cycle),
                st.active_module_cycles.iter().find(|c| c.module_id == m_aar).map(|c| c.armor_repair_amount),
            )
        })
        .await;
    assert!((st_cap - 150.0).abs() < 1e-6 && cyc_len == 5 && mis_len == 1 && pend_mis == 1 && reload_empty && asb_need == Some(0.0) && aar_rep == Some(60.0), "st_cap={st_cap} cyc_len={cyc_len} mis_len={mis_len} pend_mis={pend_mis} reload_empty={reload_empty} asb_need={asb_need:?} aar_rep={aar_rep:?}");
    let _ = go_tx.send(()); assert!(sys.wait_for_post_unlock_barrier().await);
    ctx.db.with_snapshot(|s| {
        assert!(s.items[&778_221_201].quantity == 1 && s.items[&778_221_202].quantity == 5 && !s.cluster_and_runtime.module_reload_until_unix.contains_key(&m_tur));
        assert!([778_221_203, 778_221_204, 778_221_205, 778_221_206].iter().all(|id| !s.items.contains_key(id)) && s.sp6_lifecycle.post_unlock_committed_seq.get(&SYS_ID).copied().unwrap_or(0) >= 2);
    });
}

/// `BF-1170-1A-R9-02`: In-space `dogmaIM.Activate` and `dogmaIM.SetModuleOnline` wait on the barrier and reject queued `ModuleBurnout` / `CycleAmmoStep` reload.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r9_02_in_space_activate_and_online_wait_behind_burnout_and_reload() {
    use eve_proto::PyRep;
    let (ship_id, m_rel, m_burn, tgt, ammo_id) = (778_222_001_i64, 778_222_101, 778_222_102, 778_222_999, 778_222_201);
    let (ctx, sys) = boot_ship(ship_id, &[(m_rel, 484, InventoryFlag::HiSlot0, 0.0), (m_burn, 2048, InventoryFlag::LoSlot0, 0.0)], None, Some(make_combat_state(100.0, 100.0, 100.0, 200.0))).await;
    ctx.db.with_mut_snapshot(|s| { s.items.insert(ammo_id, make_item(ammo_id, 178, CHAR_ID, ship_id, InventoryFlag::Cargo, 1)); });
    sys.spawn_ball(make_ball(tgt, 606, DVec3::new(500.0, 0.0, 0.0))).await.unwrap();
    let held = hold_db_writer(&ctx.db);
    let mut ops = PostUnlockOps::default(); ops.push(PostUnlockIntent::ModuleBurnout { ship_id, module_id: m_burn }); ops.push(reload_ammo_op(ship_id, m_rel, ctx.now_unix_secs() + 20));
    send_in_flight(&sys, &ctx, ops);
    let (act_burn, act_rel, onl_burn) = (
        spawn_rpc(&ctx, ship_id, "dogmaIM", "Activate", vec![PyRep::Integer(m_burn), PyRep::String("damageControl".into())]),
        spawn_rpc(&ctx, ship_id, "dogmaIM", "Activate", vec![PyRep::Integer(m_rel), PyRep::String("projectileFired".into()), PyRep::Integer(tgt)]),
        spawn_rpc(&ctx, ship_id, "dogmaIM", "SetModuleOnline", vec![PyRep::Integer(ship_id), PyRep::Integer(m_burn)]),
    );
    tokio::time::sleep(Duration::from_millis(25)).await; assert!(!act_burn.is_finished() && !act_rel.is_finished() && !onl_burn.is_finished());
    release_writer(held);
    assert_eq!((act_burn.await.unwrap().unwrap_err().code.as_str(), act_rel.await.unwrap().unwrap_err().code.as_str(), onl_burn.await.unwrap().unwrap_err().code.as_str()), ("ModuleBurnedOut", "ModuleIsReloading", "ModuleBurnedOut"));
}

/// `BF-1170-1A-R9-03`: In-space `contractProxy.SplitStack` waits behind in-flight `post_unlock` charge consumption on an active ship stack.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r9_03_contract_proxy_split_stack_waits_behind_in_space_charge_write() {
    use eve_proto::PyRep;
    let (ship_id, m_tur, stack_id) = (778_223_001_i64, 778_223_101, 778_223_201);
    let (ctx, sys) = boot_ship(ship_id, &[(m_tur, 484, InventoryFlag::HiSlot0, 0.0)], None, None).await;
    ctx.db.with_mut_snapshot(|s| { s.items.insert(stack_id, make_item(stack_id, 178, CHAR_ID, ship_id, InventoryFlag::Cargo, 2)); });
    let held = hold_db_writer(&ctx.db); send_intent(&sys, &ctx, reload_ammo_op(ship_id, m_tur, 999), true);
    let split_fut = spawn_rpc(&ctx, ship_id, "contractProxy", "SplitStack", vec![PyRep::Integer(ship_id), PyRep::Integer(stack_id), PyRep::Integer(1)]);
    tokio::time::sleep(Duration::from_millis(25)).await; assert!(!split_fut.is_finished());
    release_writer(held);
    assert!(split_fut.await.unwrap().unwrap_err().code == "SplitFailed" && ctx.db.with_snapshot(|s| s.items[&stack_id].quantity) == 1);
}

/// `BF-1170-1A-R10-01`: Structural `dispatch.rs` `RpcAccess::Write` barrier protects `Deactivate(..., "online")`, `OverloadRack`, `StopOverload`, `SetShipStance`, & `LinkWeapons`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn bf_1170_r10_01_dispatch_write_barrier_protects_deactivate_overload_stance_and_link() {
    use eve_proto::PyRep;
    let (ship_id, w1, w2) = (778_224_001_i64, 778_224_101, 778_224_102);
    let (ctx, sys) = boot_ship(ship_id, &[(w1, 484, InventoryFlag::HiSlot0, 0.0), (w2, 484, InventoryFlag::HiSlot1, 0.0)], None, None).await;
    ctx.db.with_mut_snapshot(|s| {
        s.items.get_mut(&ship_id).unwrap().type_id = 34317;
        s.skills.entry(CHAR_ID).or_default().insert(28164, eve_db::TrainedSkillRecord { type_id: 28164, level: 4, skill_points: 100_000.0 });
        s.ship_fit_inputs.insert(ship_id, eve_db::ShipFitInputsRecord { overloaded_modules: [w1].into_iter().collect(), ..Default::default() });
    });
    let held = hold_db_writer(&ctx.db);
    send_intent(&sys, &ctx, PostUnlockIntent::ModuleBurnout { ship_id, module_id: w1 }, true);
    let deact = spawn_rpc(&ctx, ship_id, "dogmaIM", "Deactivate", vec![PyRep::Integer(w1), PyRep::String("online".into())]);
    let ov_rack = spawn_rpc(&ctx, ship_id, "dogmaIM", "OverloadRack", vec![PyRep::Integer(0)]);
    let stop_ov = spawn_rpc(&ctx, ship_id, "dogmaIM", "StopOverload", vec![PyRep::Integer(w1)]);
    let stance = spawn_rpc(&ctx, ship_id, "shipStanceMgr", "SetShipStance", vec![PyRep::Integer(ship_id), PyRep::Integer(2)]);
    let link = spawn_rpc(&ctx, ship_id, "dogmaIM", "LinkWeapons", vec![PyRep::Integer(ship_id), PyRep::Integer(w1), PyRep::Integer(w2)]);
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(!deact.is_finished() && !ov_rack.is_finished() && !stop_ov.is_finished() && !stance.is_finished() && !link.is_finished());
    release_writer(held);
    assert_eq!(deact.await.unwrap().unwrap_err().code, "ModuleBurnedOut");
    assert_eq!(ov_rack.await.unwrap().unwrap(), PyRep::List(vec![PyRep::Integer(w2)]));
    assert!(stop_ov.await.unwrap().is_ok());
    assert_eq!(stance.await.unwrap().unwrap(), PyRep::Integer(34323));
    assert_eq!(link.await.unwrap().unwrap_err().code, "InvalidWeaponGroup");
    ctx.db.with_snapshot(|s| {
        let inputs = s.ship_fit_inputs.get(&ship_id).unwrap();
        assert!(inputs.active_stance_type_id == Some(34323) && !inputs.overloaded_modules.contains(&w1));
    });
    let _ = spawn_rpc(&ctx, ship_id, "dogmaIM", "StopOverload", vec![PyRep::Integer(w2)]).await.unwrap().unwrap();
    assert_eq!(spawn_rpc(&ctx, ship_id, "dogmaIM", "OverloadRack", vec![PyRep::Integer(0)]).await.unwrap().unwrap(), PyRep::List(vec![PyRep::Integer(w2)]));
    ctx.db.with_snapshot(|s| {
        let inputs = s.ship_fit_inputs.get(&ship_id).unwrap();
        assert!(inputs.active_stance_type_id == Some(34323) && inputs.overloaded_modules.contains(&w2) && !inputs.overloaded_modules.contains(&w1));
    });
    assert!(spawn_rpc(&ctx, ship_id, "dogmaIM", "StopOverload", vec![PyRep::Integer(w2)]).await.unwrap().is_ok());
    ctx.db.with_snapshot(|s| {
        let inputs = s.ship_fit_inputs.get(&ship_id).unwrap();
        assert!(inputs.active_stance_type_id == Some(34323) && inputs.overloaded_modules.is_empty());
    });
}

/// `#1170` Track 1D: rack heat, overheat damage/burnout, mobile deployables, and observatory
/// decloaks + `DeactivationHook` emit zero DB writes under `RANK_BALLPARK` / `RANK_ACTOR_STATE`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_1d_zero_db_writes_under_actor_locks_tactical_and_deployables() {
    use crate::services::mobile_deployables::{
        handle_ball_damage_in_actor_collecting, register_mobile_deployable,
        step_mobile_deployables_in_actor_collecting, GROUP_MOBILE_DEPOT,
    };
    use crate::services::mobile_deployables_ext::{
        step_ext_deployables_in_actor_collecting, GROUP_MOBILE_OBSERVATORY, TYPE_MOBILE_OBSERVATORY,
    };
    use crate::services::tactical_module_mechanics::{
        apply_overheat_rack_and_damage_on_cycle_collecting, step_rack_heat_per_tick_collecting,
        RACK_HEAT_ATTR_IDS,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let (ship_id, mod_hi, depot_id, loot_id, obs_id, cloak_ship, cloak_mod) = (
        779_100_001_i64, 779_100_101, 779_100_201, 779_100_202, 779_100_301, 779_100_401,
        779_100_402,
    );
    let (ctx, sys) = boot_ship(
        ship_id,
        &[(mod_hi, 484, InventoryFlag::HiSlot0, 39.5)],
        None,
        None,
    )
    .await;
    let now = ctx.now_unix();
    let deact_calls = Arc::new(AtomicUsize::new(0));
    let deact_under_actor_lock = Arc::new(AtomicBool::new(false));
    {
        let dc = Arc::clone(&deact_calls);
        let dl = Arc::clone(&deact_under_actor_lock);
        let hook: crate::system_actor::ModuleDeactivationHook = Arc::new(move |_sys, _cid, _cyc| {
            let ranks = eve_db::lock_rank::held_lock_ranks();
            if ranks.iter().any(|&r| r == eve_db::lock_rank::RANK_BALLPARK || r == eve_db::lock_rank::RANK_ACTOR_STATE) {
                dl.store(true, Ordering::SeqCst);
            }
            dc.fetch_add(1, Ordering::SeqCst);
        });
        *sys.module_deactivation_hook.write().unwrap() = Some(hook);
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&ship_id).unwrap().dynamic_attributes.insert(RACK_HEAT_ATTR_IDS[0], 0.99);
        snap.items.get_mut(&mod_hi).unwrap().dynamic_attributes.insert(eve_sde::attr_ids::HP_STRUCT, 40.0);
        snap.items.insert(depot_id, make_item(depot_id, 33474, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1));
        snap.item_positions.insert(depot_id, [1_000.0, 0.0, 0.0]);
        snap.items.insert(loot_id, make_item(loot_id, 178, CHAR_ID, depot_id, InventoryFlag::Cargo, 5));
        snap.items.insert(obs_id, make_item(obs_id, TYPE_MOBILE_OBSERVATORY, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1));
        snap.item_positions.insert(obs_id, [2_000.0, 0.0, 0.0]);
        snap.items.insert(cloak_ship, make_item(cloak_ship, 606, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1));
        snap.items.insert(cloak_mod, make_item(cloak_mod, 11578, CHAR_ID, cloak_ship, InventoryFlag::HiSlot0, 1));
        snap.module_active_states.insert(cloak_mod);
    });
    register_mobile_deployable(&ctx, depot_id, 33474, GROUP_MOBILE_DEPOT, CHAR_ID, SYS_ID, 0, DVec3::new(1_000.0, 0.0, 0.0));
    register_mobile_deployable(&ctx, obs_id, TYPE_MOBILE_OBSERVATORY, GROUP_MOBILE_OBSERVATORY, CHAR_ID, SYS_ID, 0, DVec3::new(2_000.0, 0.0, 0.0));
    sys.spawn_ball_with_combat(make_ball(depot_id, 33474, DVec3::new(1_000.0, 0.0, 0.0)), make_combat_state(0.0, 0.0, 100.0, 0.0)).await.unwrap();
    let mut cloak_cs = make_combat_state(100.0, 100.0, 100.0, 100.0);
    cloak_cs.is_cloaked = true;
    sys.spawn_ball_with_combat(make_ball(cloak_ship, 606, DVec3::new(2_500.0, 0.0, 0.0)), cloak_cs).await.unwrap();

    // Hold `db.writer` AND acquire BOTH `RANK_BALLPARK` and `RANK_ACTOR_STATE`: zero DB panic or block!
    let held = hold_db_writer(&ctx.db);
    let (sde_c, db_c, rng_c) = (Arc::clone(&ctx.sde), Arc::clone(&ctx.db), Arc::clone(&ctx.mechanic_rng));
    let (ops, burned_out) = sys
        .test_with_ballpark_and_state_mut_async(move |bp, st| {
            let mut ops = PostUnlockOps::default();
            let mut local_heat = HashMap::new();
            let mut burned_out = false;
            let mut burned_others = Vec::new();
            let cyc = ActiveModuleCycle {
                ship_id,
                module_id: mod_hi,
                module_type_id: 484,
                owner_char_id: Some(CHAR_ID),
                rack_idx: 0,
                is_overloaded: true,
                heat_damage_per_cycle: 20.0,
                ..Default::default()
            };
            st.active_module_cycles.push(cyc.clone());
            st.active_module_cycles.push(cloak_cyc(cloak_ship, cloak_mod, 5.0, 0.0, 1));

            let next_seq = st.next_post_unlock_seq.saturating_add(1);
            step_rack_heat_per_tick_collecting(&mut st.ship_rack_heat, &st.active_module_cycles, &st.sde_opt, &st.db_opt, 1.0, Some((&mut st.post_unlock_shadows.rack_heat_pending, next_seq)), &mut ops);
            let rng = Some(Arc::new(crate::rng::MechanicRng::seeded(7)));
            for _ in 0..24 {
                apply_overheat_rack_and_damage_on_cycle_collecting(&cyc, &mut st.ship_rack_heat, &st.sde_opt, &st.db_opt, &rng, &mut burned_out, &mut burned_others, Some(&mut local_heat), Some(&st.post_unlock_shadows.rack_heat_pending), &mut ops);
                if burned_out { break; }
            }
            let dmg = handle_ball_damage_in_actor_collecting(&st.db_opt, &st.sde_opt, SYS_ID, depot_id, eve_destiny::DamageVector::new(10_000.0, 0.0, 0.0, 0.0), bp, now + 100, Some((&mut st.post_unlock_shadows.depot_reinforced_until, next_seq)), &mut ops);
            assert!(dmg.is_some_and(|(_, destroyed, _)| destroyed));
            step_mobile_deployables_in_actor_collecting(bp, &mut st.active_module_cycles, &rng_c, Some(&sde_c), st.db_opt.as_ref(), Some(&rng_c), SYS_ID, 1.0, now + 5_000, Some((&mut st.post_unlock_shadows.mmju_spool_fired_at, next_seq)), Some((&mut st.post_unlock_shadows.observatory_pings_done, next_seq)), &mut ops);
            step_ext_deployables_in_actor_collecting(bp, Some(&mut st.active_module_cycles), &sde_c, &db_c, Some(&rng_c), SYS_ID, now + 5_000, Some((&mut st.post_unlock_shadows.mmju_spool_fired_at, next_seq)), Some((&mut st.post_unlock_shadows.observatory_pings_done, next_seq)), &mut ops);
            (ops, burned_out)
        })
        .await;
    assert!(burned_out);
    assert_eq!(deact_calls.load(Ordering::SeqCst), 0, "DeactivationHook must not run under actor locks");
    assert!(ctx.db.with_snapshot(|s| {
        s.items.contains_key(&depot_id)
            && s.items[&ship_id].dynamic_attributes.contains_key(&RACK_HEAT_ATTR_IDS[0])
            && s.module_online_states.get(&mod_hi).copied().unwrap_or(true)
    }), "zero DB writes allowed while actor locks / db.writer were held");

    send_in_flight(&sys, &ctx, ops);
    release_writer(held);

    wait_until("post-unlock drained tactical & deployable ops", || {
        ctx.db.with_snapshot(|s| {
            !s.items.contains_key(&depot_id)
                && !s.items[&ship_id].dynamic_attributes.contains_key(&RACK_HEAT_ATTR_IDS[0])
                && s.module_online_states.get(&mod_hi) == Some(&false)
        })
    }).await;
    assert!(!deact_under_actor_lock.load(Ordering::SeqCst));
}

/// `#1170` Track 1D: Abyssal pylons/clouds, room NPC gate clear, Bosonic tick, Proving Grounds arena,
/// and Insurgency FOB damage emit zero DB writes under `RANK_BALLPARK` / `RANK_ACTOR_STATE`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_1d_zero_db_writes_under_actor_locks_abyssal_bosonic_arena_insurgency() {
    use crate::services::doomsday_aoe_ops::step_bosonic_doomsday_in_ballpark_collecting;
    use crate::services::doomsday_aoe_tick::{mutate_superweapon_runtime, PersistedSuperweaponCycle, PersistedWarpSuppression, SuperweaponCycleKind};
    use crate::services::fw_gameplay::fw_gameplay_insurgency::handle_fob_ball_damage_in_actor_collecting;
    use crate::services::insurgency_services::{FACTION_GURISTAS, PIRATE_STRONGHOLD_GURISTAS_TYPE};
    use crate::services::pve_abyssal_pylons::step_abyssal_hazards_and_pylons_in_ballpark_for_tick_collecting;
    use crate::services::pve_special_sites_abyssal::{
        on_abyssal_room_npc_destroyed_in_ballpark_collecting, AbyssalPocketSession, AbyssalRoomRecord, AbyssalWeatherKind,
    };
    use crate::services::pvp_filament_arena::step_proving_grounds_arena_hazards_in_ballpark_for_tick_collecting;
    use crate::services::pvp_filament_mgr::{load_pvp_store, save_pvp_store, PvpArenaMatchInstance, PvpFilamentStore};
    use eve_db::{PersistedInsurgencyCampaign, UpwellStructureRecord};

    let ctx = test_ctx();
    let (abyss_sys, arena_sys, titan_sys, fob_sys) = (32_000_003_u32, 32_000_001_u32, SYS_ID, 30_002_505_u32);
    let (ab_ship, npc_id, cloud_id, gate_id) = (779_200_001_i64, 779_200_002, 1_898_000_111, 1_895_000_011);
    let (arena_char, arena_ship, titan_id, supp_ship, fob_camp, fob_id) = (
        90_000_002_i64, 779_200_101, 779_200_201, 779_200_202, 77_920_i64, 1_897_077_920_i64,
    );
    let now = ctx.now_unix();

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ab_ship, make_item(ab_ship, 587, CHAR_ID, i64::from(abyss_sys), InventoryFlag::None, 1));
        snap.abyssal_state.pockets.insert(CHAR_ID, AbyssalPocketSession {
            pocket_id: 1, character_id: CHAR_ID, ship_id: ab_ship, origin_solar_system_id: SYS_ID,
            abyssal_solar_system_id: abyss_sys, origin_position: [0.0; 3], grid_offset: [0.0; 3],
            filament_type_id: 47762, tier: 1, weather: AbyssalWeatherKind::Electrical,
            weather_penalty_pct: 0.0, weather_bonus_pct: 0.0, npc_difficulty_mult: 1.0,
            current_room: 1,
            rooms: [
                AbyssalRoomRecord { room_index: 1, dungeon_id: 1, dungeon_name: "r1".into(), npc_ball_ids: vec![npc_id], cache_item_id: 0, cleared: false, gate_unlocked_at_sec: None },
                AbyssalRoomRecord { room_index: 2, dungeon_id: 1, dungeon_name: "r2".into(), npc_ball_ids: Vec::new(), cache_item_id: 0, cleared: false, gate_unlocked_at_sec: None },
                AbyssalRoomRecord { room_index: 3, dungeon_id: 1, dungeon_name: "r3".into(), npc_ball_ids: Vec::new(), cache_item_id: 0, cleared: false, gate_unlocked_at_sec: None },
            ],
            started_at_sec: now, collapse_at_sec: now + 1200,
            completed: false, collapsed_destroyed: false, trace_ball_id: 0,
            fleet_ship_ids: vec![ab_ship], ship_room: [(ab_ship, 1)].into_iter().collect(), max_ships: 1,
        });
        snap.items.insert(arena_ship, make_item(arena_ship, 587, arena_char, i64::from(arena_sys), InventoryFlag::None, 1));
        if let Some(ch) = snap.characters.get_mut(&arena_char) {
            (ch.active_ship_id, ch.solar_system_id, ch.station_id, ch.structure_id) = (arena_ship, arena_sys, None, None);
        }
        let mut pvp = PvpFilamentStore::default();
        pvp.active_matches.insert(91_001, PvpArenaMatchInstance {
            match_id: 91_001, event_id: 1, arena_solar_system_id: arena_sys,
            team_a: vec![arena_char], origin_systems: [(arena_char, SYS_ID)].into_iter().collect(),
            started_unix: now, active: true, ..Default::default()
        });
        save_pvp_store(snap, &pvp);
        snap.structures.insert(fob_id, UpwellStructureRecord {
            structure_id: fob_id, solar_system_id: fob_sys, type_id: PIRATE_STRONGHOLD_GURISTAS_TYPE,
            state: "ShieldVulnerable".into(), shield_hp: 2_500.0, armor_hp: 2_500.0, hull_hp: 3_500.0, ..Default::default()
        });
        snap.insurgency_runtime.campaigns.insert(fob_camp, PersistedInsurgencyCampaign {
            campaign_id: fob_camp, pirate_faction_id: FACTION_GURISTAS, target_faction_id: 500_001,
            fob_solar_system_id: fob_sys, systems: vec![fob_sys], created_at_unix: now - 100,
            forecasting_end_unix: now - 50, active_end_unix: now + 5_000, winddown_end_unix: now + 10_000,
            suppression_points: (0..5).map(|i| (fob_sys + i, 1_000)).collect(), ..Default::default()
        });
    });
    mutate_superweapon_runtime(&ctx.db, |rt| {
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id: titan_sys, char_id: CHAR_ID, ship_id: titan_id, ship_type_id: 11_567, mod_type_id: 24550,
            kind: SuperweaponCycleKind::Cone {
                origin_pos: [0.0; 3], dir_vec: [1.0, 0.0, 0.0], cone_len_m: 100.0,
                tick_dmg: [0.0; 4], sig_radius_m: 100.0,
            },
            started_at_unix: now - 20, warmup_secs: 5, total_ticks: 10, ticks_applied: 10,
            immobilize_until_unix: now - 1, orig_max_velocity: 90.0,
            orig_warp_scramble_strength: 0, orig_disallow_assistance: false, mobility_restored: false,
        });
        rt.warp_suppressions.push(PersistedWarpSuppression {
            sys_id: titan_sys, ship_id: supp_ship, orig_warp_scramble_strength: 1, suppressed_until_unix: now - 1,
        });
    });

    let sys_abyss = ctx.systems.get_or_spawn(abyss_sys);
    let sys_arena = ctx.systems.get_or_spawn(arena_sys);
    let sys_titan = ctx.systems.get_or_spawn(titan_sys);
    let sys_fob = ctx.systems.get_or_spawn(fob_sys);
    sys_abyss.spawn_ball_with_combat(make_ball(ab_ship, 587, DVec3::new(90_000.0, 0.0, 0.0)), make_combat_state(100.0, 100.0, 10_000.0, 100.0)).await.unwrap();
    sys_abyss.spawn_ball(make_ball(cloud_id, 80_876, DVec3::new(90_000.0, 0.0, 0.0))).await.unwrap();
    sys_abyss.spawn_ball(make_ball(gate_id, 47_685, DVec3::ZERO)).await.unwrap();
    sys_arena.spawn_ball_with_combat(make_ball(arena_ship, 587, DVec3::new(90_000.0, 0.0, 0.0)), make_combat_state(100.0, 100.0, 10_000.0, 100.0)).await.unwrap();
    sys_titan.spawn_ball_with_combat(make_ball(titan_id, 11_567, DVec3::ZERO), make_combat_state(100.0, 100.0, 100.0, 100.0)).await.unwrap();
    sys_titan.spawn_ball_with_combat(make_ball(supp_ship, 587, DVec3::new(500.0, 0.0, 0.0)), make_combat_state(100.0, 100.0, 100.0, 100.0)).await.unwrap();
    sys_fob.spawn_ball(make_ball(fob_id, PIRATE_STRONGHOLD_GURISTAS_TYPE, DVec3::new(1_000.0, 0.0, 0.0))).await.unwrap();

    let held = hold_db_writer(&ctx.db);
    let ops_ab = {
        let (sde_c, db_c) = (Arc::clone(&ctx.sde), Arc::clone(&ctx.db));
        sys_abyss
            .test_with_ballpark_and_state_mut_async(move |bp_ab, st_ab| {
                let mut ops_ab = PostUnlockOps::default();
                let seq_ab = st_ab.next_post_unlock_seq.saturating_add(1);
                assert!(on_abyssal_room_npc_destroyed_in_ballpark_collecting(bp_ab, &db_c, abyss_sys, npc_id, &mut ops_ab));
                let _ = step_abyssal_hazards_and_pylons_in_ballpark_for_tick_collecting(bp_ab, Some(&sde_c), Some(&db_c), abyss_sys, Some((&mut st_ab.post_unlock_shadows.abyssal_boundary_ticks, seq_ab)), Some((&mut st_ab.post_unlock_shadows.abyssal_active_clouds, seq_ab)), &mut ops_ab);
                ops_ab
            })
            .await
    };
    let ops_ar = {
        let (sde_c, db_c) = (Arc::clone(&ctx.sde), Arc::clone(&ctx.db));
        sys_arena
            .test_with_ballpark_and_state_mut_async(move |bp_ar, st_ar| {
                let mut ops_ar = PostUnlockOps::default();
                let seq_ar = st_ar.next_post_unlock_seq.saturating_add(1);
                let _ = step_proving_grounds_arena_hazards_in_ballpark_for_tick_collecting(bp_ar, Some(&sde_c), Some(&db_c), arena_sys, st_ar.pending_notifications_opt.as_ref(), st_ar.notify_tx_opt.as_ref(), Some((&mut st_ar.post_unlock_shadows.arena_hazard_ticks, seq_ar)), &mut ops_ar);
                ops_ar
            })
            .await
    };
    let ops_ti = {
        let (sde_c, db_c) = (Arc::clone(&ctx.sde), Arc::clone(&ctx.db));
        sys_titan
            .test_with_ballpark_and_state_mut_async(move |bp_ti, st_ti| {
                let mut ops_ti = PostUnlockOps::default();
                let mut dead = Vec::new();
                let seq_ti = st_ti.next_post_unlock_seq.saturating_add(1);
                step_bosonic_doomsday_in_ballpark_collecting(bp_ti, &mut st_ti.base_scramble_strengths, Some(&sde_c), Some(&db_c), titan_sys, now, &mut dead, Some((&mut st_ti.post_unlock_shadows.restored_doomsday_cycles, &mut st_ti.post_unlock_shadows.restored_warp_suppressions, seq_ti)), &mut ops_ti);
                ops_ti
            })
            .await
    };
    let ops_fb = {
        let db_c = Arc::clone(&ctx.db);
        sys_fob
            .test_with_ballpark_and_state_mut_async(move |bp_fb, st_fb| {
                let mut ops_fb = PostUnlockOps::default();
                let seq_fb = st_fb.next_post_unlock_seq.saturating_add(1);
                let dmg = handle_fob_ball_damage_in_actor_collecting(Some(&db_c), fob_sys, fob_id, eve_destiny::DamageVector::new(1_500.0, 1_500.0, 0.0, 0.0), bp_fb, now, Some((&mut st_fb.post_unlock_shadows.fob_states, seq_fb)), &mut ops_fb);
                assert!(dmg.is_some_and(|(applied, destroyed, _)| applied > 0.0 && !destroyed));
                ops_fb
            })
            .await
    };

    assert!(ctx.db.with_snapshot(|s| {
        !s.abyssal_state.pockets[&CHAR_ID].rooms[0].cleared
            && s.abyssal_state.boundary_ticks.is_empty()
            && !s.superweapon_runtime.active_cycles.is_empty()
            && s.structures[&fob_id].state == "ShieldVulnerable"
    }), "zero DB writes allowed while actor locks / db.writer were held");

    assert!(!ops_ab.is_empty() && !ops_ar.is_empty() && !ops_ti.is_empty() && !ops_fb.is_empty());
    send_in_flight(&sys_abyss, &ctx, ops_ab);
    send_in_flight(&sys_arena, &ctx, ops_ar);
    send_in_flight(&sys_titan, &ctx, ops_ti);
    send_in_flight(&sys_fob, &ctx, ops_fb);
    release_writer(held);

    assert!(sys_abyss.wait_for_post_unlock_barrier().await);
    assert!(sys_arena.wait_for_post_unlock_barrier().await);
    assert!(sys_titan.wait_for_post_unlock_barrier().await);
    assert!(sys_fob.wait_for_post_unlock_barrier().await);
    ctx.db.with_snapshot(|s| {
        assert!(s.abyssal_state.pockets[&CHAR_ID].rooms[0].cleared, "abyssal room 0 not cleared");
        assert_eq!(s.abyssal_state.boundary_ticks.get(&ab_ship).copied(), Some(1), "abyssal boundary_ticks missing/mismatch");
        assert_eq!(load_pvp_store(s).hazard_ticks.get(&arena_ship).copied(), Some(1), "arena hazard_ticks missing/mismatch");
        assert!(s.superweapon_runtime.active_cycles.is_empty(), "bosonic active_cycles not drained: {:?}", s.superweapon_runtime.active_cycles);
        assert!(s.superweapon_runtime.warp_suppressions.is_empty(), "bosonic warp_suppressions not drained");
        assert_eq!(s.structures[&fob_id].state, "ArmorVulnerable", "FOB structure state mismatch");
    });
}

/// `#1170` Track 1D: holding `WorldRepository::writer` for `1.5s` does NOT block actor ticks,
/// lock acquisitions, or external service stepping; DB catches up cleanly once writer is released.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_1d_writer_held_1500ms_does_not_block_actor_ticks_for_external_services() {
    use crate::services::mobile_deployables::{register_mobile_deployable, GROUP_MOBILE_OBSERVATORY};
    use crate::services::mobile_deployables_ext::TYPE_MOBILE_OBSERVATORY;

    let (ship_id, mod_hi, obs_id) = (779_300_001_i64, 779_300_101, 779_300_201);
    let (ctx, sys) = boot_ship(ship_id, &[(mod_hi, 484, InventoryFlag::HiSlot0, 0.0)], None, Some(make_combat_state(100.0, 100.0, 100.0, 100.0))).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(obs_id, make_item(obs_id, TYPE_MOBILE_OBSERVATORY, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1));
    });
    register_mobile_deployable(&ctx, obs_id, TYPE_MOBILE_OBSERVATORY, GROUP_MOBILE_OBSERVATORY, CHAR_ID, SYS_ID, 0, DVec3::ZERO);
    sys.spawn_ball(make_ball(obs_id, TYPE_MOBILE_OBSERVATORY, DVec3::ZERO)).await.unwrap();
    sys.test_with_actor_state_mut_async(move |st| {
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id, module_id: mod_hi, module_type_id: 484, owner_char_id: Some(CHAR_ID),
            rack_idx: 0, is_overloaded: true, heat_damage_per_cycle: 10.0, duration_s: 5.0, ..Default::default()
        });
    })
    .await;

    let db_hold = Arc::clone(&ctx.db);
    let (held_tx, held_rx) = mpsc::channel();
    let holder = std::thread::spawn(move || {
        db_hold.with_mut_snapshot(|_| {
            held_tx.send(()).unwrap();
            std::thread::sleep(Duration::from_millis(1500));
        });
    });
    held_rx.recv().unwrap();

    for _ in 0..4 {
        let t0 = std::time::Instant::now();
        let _ = sys.ask_step_tick_only(1.0).await.unwrap();
        assert!(t0.elapsed() < Duration::from_millis(250), "actor step blocked on held DB writer: {:?}", t0.elapsed());
        let t1 = std::time::Instant::now();
        sys.test_with_ballpark_and_state_mut_async(|_bp, _st| ()).await;
        assert!(t1.elapsed() < Duration::from_millis(50), "actor state inspection blocked on held DB writer");
    }
    assert!(!holder.is_finished(), "1.5s writer hold must still be active after non-blocking ticks");
    holder.join().unwrap();
    let _ = sys.wait_for_post_unlock_barrier().await;

    wait_until("post-unlock persisted rack heat after 1.5s writer release", || {
        ctx.db.with_snapshot(|s| s.transient_item_attributes.get(&ship_id).is_some_and(|tr| tr.contains_key(&1175)))
    }).await;
}

/// Regression test (`BF-01`, `BF-02`): under a held `db.writer`, Mobile Depot reinforcement and Pirate FOB
/// transitions compose monotonically in actor shadows and reject stale DB ops out of order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_1d_bf01_bf02_multi_hit_held_writer_depot_reinforce_and_fob_state() {
    use crate::services::fw_gameplay::fw_gameplay_insurgency::{
        apply_fob_ball_damage_in_snap, FobBallDamageDbOp,
    };
    use crate::services::insurgency_services::{FACTION_GURISTAS, PIRATE_STRONGHOLD_GURISTAS_TYPE};
    use crate::services::mobile_deployables::{
        apply_mobile_deployable_in_snap, handle_ball_damage_in_actor_collecting,
        register_mobile_deployable, write_deployable_state, MobileDeployableDbOp, GROUP_MOBILE_DEPOT,
    };
    use eve_db::{PersistedInsurgencyCampaign, UpwellStructureRecord};
    use eve_destiny::DamageVector;

    let ctx = test_ctx();
    let now = ctx.now_unix();
    let (depot1, loot1, depot2) = (779_300_101_i64, 779_300_102_i64, 779_300_103_i64);
    let (fob_sys1, camp1, fob1) = (30_002_505_u32, 77_931_i64, 1_897_077_931_i64);
    let (fob_sys2, camp2, fob2) = (30_002_506_u32, 77_932_i64, 1_897_077_932_i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(depot1, make_item(depot1, 33474, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1));
        snap.item_positions.insert(depot1, [1_000.0, 0.0, 0.0]);
        snap.items.insert(loot1, make_item(loot1, 178, CHAR_ID, depot1, InventoryFlag::Cargo, 5));
        snap.items.insert(depot2, make_item(depot2, 33474, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1));
        snap.item_positions.insert(depot2, [2_000.0, 0.0, 0.0]);

        let s5_systems = vec![fob_sys1, 30_002_507, 30_002_508, 30_002_509, 30_002_510];
        let supp1: HashMap<u32, i64> = s5_systems.iter().map(|&s| (s, 1_000)).collect();
        snap.insurgency_runtime.campaigns.insert(camp1, PersistedInsurgencyCampaign {
            campaign_id: camp1, pirate_faction_id: FACTION_GURISTAS, target_faction_id: 500001,
            fob_solar_system_id: fob_sys1, systems: s5_systems.clone(),
            created_at_unix: now.saturating_sub(5 * 86_400),
            forecasting_end_unix: now.saturating_sub(100),
            active_end_unix: now.saturating_add(86_400),
            winddown_end_unix: now.saturating_add(2 * 86_400),
            corruption_points: HashMap::new(), suppression_points: supp1,
        });
        snap.structures.insert(fob1, UpwellStructureRecord {
            structure_id: fob1, type_id: PIRATE_STRONGHOLD_GURISTAS_TYPE, owner_corp_id: 1_000_125,
            solar_system_id: fob_sys1, name: "FOB 1".into(), position: [0.0; 3],
            state: "ShieldVulnerable".into(), shield_hp: 1_000.0, armor_hp: 1_000.0, hull_hp: 3_500.0,
            ..Default::default()
        });

        let mut s5_systems2 = vec![fob_sys2, 30_002_511, 30_002_512, 30_002_513, 30_002_514];
        let supp2: HashMap<u32, i64> = s5_systems2.iter().map(|&s| (s, 1_000)).collect();
        snap.insurgency_runtime.campaigns.insert(camp2, PersistedInsurgencyCampaign {
            campaign_id: camp2, pirate_faction_id: FACTION_GURISTAS, target_faction_id: 500001,
            fob_solar_system_id: fob_sys2, systems: std::mem::take(&mut s5_systems2),
            created_at_unix: now.saturating_sub(5 * 86_400),
            forecasting_end_unix: now.saturating_sub(100),
            active_end_unix: now.saturating_add(86_400),
            winddown_end_unix: now.saturating_add(2 * 86_400),
            corruption_points: HashMap::new(), suppression_points: supp2,
        });
        snap.insurgency_runtime.fob_armor_reinforced_until.insert(camp2, now.saturating_sub(10));
        snap.structures.insert(fob2, UpwellStructureRecord {
            structure_id: fob2, type_id: PIRATE_STRONGHOLD_GURISTAS_TYPE, owner_corp_id: 1_000_125,
            solar_system_id: fob_sys2, name: "FOB 2".into(), position: [0.0; 3],
            state: "ArmorReinforced".into(), reinforced_until_unix: now.saturating_sub(10),
            shield_hp: 0.0, armor_hp: 0.0, hull_hp: 1_000.0,
            ..Default::default()
        });
    });

    register_mobile_deployable(&ctx, depot1, 33474, GROUP_MOBILE_DEPOT, CHAR_ID, SYS_ID, 0, DVec3::new(1_000.0, 0.0, 0.0));
    register_mobile_deployable(&ctx, depot2, 33474, GROUP_MOBILE_DEPOT, CHAR_ID, SYS_ID, 0, DVec3::new(2_000.0, 0.0, 0.0));
    write_deployable_state(&ctx, |st| {
        for did in [depot1, depot2] {
            let rec = st.deployables.get_mut(&did).unwrap();
            rec.anchor_completed = true;
            rec.anchor_start_unix = now.saturating_sub(120);
        }
    });

    let sys = ctx.systems.get_or_spawn(SYS_ID);
    sys.spawn_ball_with_combat(make_ball(depot1, 33474, DVec3::new(1_000.0, 0.0, 0.0)), make_combat_state(1_000.0, 1_000.0, 1_000.0, 0.0)).await.unwrap();
    sys.spawn_ball_with_combat(make_ball(depot2, 33474, DVec3::new(2_000.0, 0.0, 0.0)), make_combat_state(1_000.0, 1_000.0, 1_000.0, 0.0)).await.unwrap();

    let sys_fob1 = ctx.systems.get_or_spawn(fob_sys1);
    sys_fob1.spawn_ball_with_combat(
        make_ball(fob1, PIRATE_STRONGHOLD_GURISTAS_TYPE, DVec3::ZERO),
        CombatState { shield_hp: 1_000.0, max_shield_hp: 2_500.0, armor_hp: 1_000.0, max_armor_hp: 2_500.0, structure_hp: 3_500.0, max_structure_hp: 3_500.0, ..Default::default() },
    ).await.unwrap();
    let sys_fob2 = ctx.systems.get_or_spawn(fob_sys2);
    sys_fob2.spawn_ball_with_combat(
        make_ball(fob2, PIRATE_STRONGHOLD_GURISTAS_TYPE, DVec3::ZERO),
        CombatState { shield_hp: 0.0, max_shield_hp: 2_500.0, armor_hp: 0.0, max_armor_hp: 2_500.0, structure_hp: 1_000.0, max_structure_hp: 3_500.0, ..Default::default() },
    ).await.unwrap();

    let held = hold_db_writer(&ctx.db);
    // BF-01 across sequential actor commands while db.writer held (`ApplyDamage` uses barrier reply, so inspect ballpark before releasing writer):
    let (tx_d1, rx_d1) = tokio::sync::oneshot::channel();
    sys.cmd_sender().send(crate::system_actor::SystemCommand::ApplyDamage {
        ball_id: depot1, damage: DamageVector::new(2_000.0, 0.0, 0.0, 0.0), reply: tx_d1,
    }).await.unwrap();
    let (tx_d2, rx_d2) = tokio::sync::oneshot::channel();
    sys.cmd_sender().send(crate::system_actor::SystemCommand::ApplyDamage {
        ball_id: depot1, damage: DamageVector::new(50_000.0, 50_000.0, 50_000.0, 50_000.0), reply: tx_d2,
    }).await.unwrap();
    let cs_d = sys.get_combat_state(depot1).await.unwrap().unwrap();
    assert_eq!(cs_d.armor_hp, 1_000.0, "reinforced shadow must block second damage command while DB writer held");
    assert!((cs_d.shield_hp - 250.0).abs() < 1e-6);

    // BF-01 same-tick batch inside one actor lock window:
    sys.test_with_ballpark_and_state_mut_async(move |bp, st| {
        let seq = st.next_post_unlock_seq.saturating_add(1);
        let mut same_ops = PostUnlockOps::default();
        let r1 = handle_ball_damage_in_actor_collecting(&st.db_opt, &st.sde_opt, SYS_ID, depot2, DamageVector::new(2_000.0, 0.0, 0.0, 0.0), bp, now, Some((&mut st.post_unlock_shadows.depot_reinforced_until, seq)), &mut same_ops);
        assert!(r1.is_some_and(|(a, d, _)| a > 700.0 && !d));
        let r2 = handle_ball_damage_in_actor_collecting(&st.db_opt, &st.sde_opt, SYS_ID, depot2, DamageVector::new(50_000.0, 0.0, 0.0, 0.0), bp, now, Some((&mut st.post_unlock_shadows.depot_reinforced_until, seq)), &mut same_ops);
        assert!(r2.is_some_and(|(a, d, _)| a == 0.0 && !d));
        assert_eq!(same_ops.intents.len(), 1, "same-tick follow-up hit must not queue DepotDestroyed");
    })
    .await;

    // BF-02 Shield -> ArmorReinforced under held writer:
    let (tx_f1a, rx_f1a) = tokio::sync::oneshot::channel();
    sys_fob1.cmd_sender().send(crate::system_actor::SystemCommand::ApplyDamage {
        ball_id: fob1, damage: DamageVector::new(2_500.0, 0.0, 0.0, 0.0), reply: tx_f1a,
    }).await.unwrap();
    let (tx_f1b, rx_f1b) = tokio::sync::oneshot::channel();
    sys_fob1.cmd_sender().send(crate::system_actor::SystemCommand::ApplyDamage {
        ball_id: fob1, damage: DamageVector::new(2_500.0, 0.0, 0.0, 0.0), reply: tx_f1b,
    }).await.unwrap();
    let cs_f1 = sys_fob1.get_combat_state(fob1).await.unwrap().unwrap();
    assert_eq!(cs_f1.shield_hp, 0.0);
    assert_eq!(cs_f1.armor_hp, 0.0);

    // BF-02 ArmorPhase sequential accumulation & destruction under held writer:
    let (tx_f2a, rx_f2a) = tokio::sync::oneshot::channel();
    sys_fob2.cmd_sender().send(crate::system_actor::SystemCommand::ApplyDamage {
        ball_id: fob2, damage: DamageVector::new(400.0, 0.0, 0.0, 0.0), reply: tx_f2a,
    }).await.unwrap();
    let (tx_f2b, rx_f2b) = tokio::sync::oneshot::channel();
    sys_fob2.cmd_sender().send(crate::system_actor::SystemCommand::ApplyDamage {
        ball_id: fob2, damage: DamageVector::new(700.0, 0.0, 0.0, 0.0), reply: tx_f2b,
    }).await.unwrap();
    assert!(sys_fob2.get_ball_snapshot(fob2).await.unwrap().is_none(), "second hit must compose off 600 hull in FOB shadow and destroy");

    release_writer(held);
    let (ap1, dest1, _) = rx_d1.await.unwrap().unwrap();
    let (ap2, dest2, _) = rx_d2.await.unwrap().unwrap();
    assert!(ap1 > 700.0 && !dest1);
    assert_eq!(ap2, 0.0);
    assert!(!dest2);
    let (fa1, fd1, _) = rx_f1a.await.unwrap().unwrap();
    let (fa2, fd2, _) = rx_f1b.await.unwrap().unwrap();
    assert!((fa1 - 2_000.0).abs() < 1e-6 && !fd1);
    assert_eq!(fa2, 0.0);
    assert!(!fd2, "pending FOB ArmorReinforced shadow must block immediate follow-up hit");
    let (fb1, fbd1, _) = rx_f2a.await.unwrap().unwrap();
    let (fb2, fbd2, _) = rx_f2b.await.unwrap().unwrap();
    assert!((fb1 - 400.0).abs() < 1e-6 && !fbd1);
    assert!((fb2 - 600.0).abs() < 1e-6 && fbd2);
    let _ = sys.wait_for_post_unlock_barrier().await;
    let _ = sys_fob1.wait_for_post_unlock_barrier().await;
    let _ = sys_fob2.wait_for_post_unlock_barrier().await;

    // DB invariants + snap-level monotonicity guards:
    let stale_depot_eff = ctx.db.with_mut_snapshot(|snap| {
        assert!(snap.deployable_state.deployables.get(&depot1).and_then(|r| r.reinforced_until_unix).is_some_and(|u| u > now));
        apply_mobile_deployable_in_snap(snap, Some(&ctx.sde), &MobileDeployableDbOp::DepotDestroyed {
            solar_system_id: SYS_ID, ball_id: depot1, owner_id: CHAR_ID, pos_arr: [1_000.0, 0.0, 0.0], now_unix: now + 10,
        })
    });
    assert!(stale_depot_eff.is_none());
    ctx.db.with_mut_snapshot(|snap| {
        assert!(snap.items.contains_key(&depot1) && snap.items.contains_key(&loot1));
        assert_eq!(snap.structures[&fob1].state, "ArmorReinforced");
        assert!(snap.insurgency_runtime.fob_armor_reinforced_until.contains_key(&camp1));
        apply_fob_ball_damage_in_snap(snap, &FobBallDamageDbOp::ShieldPhase {
            camp_id: camp1, ball_id: fob1, new_s: 500.0, new_a: 1_000.0, hull: 3_500.0, now_unix: now + 5, became_reinf: false, until: 0,
        });
        assert_eq!(snap.structures[&fob1].state, "ArmorReinforced");
        assert_eq!(snap.structures[&fob1].shield_hp, 0.0);

        assert_eq!(snap.structures[&fob2].state, "Destroyed");
        assert_eq!(snap.structures[&fob2].hull_hp, 0.0);
        apply_fob_ball_damage_in_snap(snap, &FobBallDamageDbOp::ArmorPhase {
            ball_id: fob2, destroyed: false, new_h: 250.0, now_unix: now + 5,
        });
        assert_eq!(snap.structures[&fob2].state, "Destroyed");
        assert_eq!(snap.structures[&fob2].hull_hp, 0.0);
    });
}

/// Regression test (`BF-03`): cross-tick overheat damage burnout under held `db.writer` + external rack-heat reset visibility.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_1d_bf03_cross_tick_overheat_burnout_and_external_rack_reset() {
    use crate::services::tactical_module_mechanics::RACK_HEAT_ATTR_IDS;

    let (ship_id, mod_hi) = (779_300_201_i64, 779_300_202_i64);
    let (ctx, sys) = boot_ship(
        ship_id,
        &[(mod_hi, 484, InventoryFlag::HiSlot0, 35.0)],
        None,
        Some(make_combat_state(100.0, 100.0, 100.0, 1_000.0)),
    ).await;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.get_mut(&mod_hi).unwrap().dynamic_attributes.insert(eve_sde::attr_ids::HP_STRUCT, 40.0);
        let ship_it = snap.items.get_mut(&ship_id).unwrap();
        ship_it.dynamic_attributes.insert(14, 1.0);
        ship_it.dynamic_attributes.insert(13, 0.0);
        ship_it.dynamic_attributes.insert(12, 0.0);
        ship_it.dynamic_attributes.insert(1137, 0.0);
        ship_it.dynamic_attributes.insert(1179, 0.001);
        ship_it.dynamic_attributes.insert(RACK_HEAT_ATTR_IDS[0], 1.0);
        snap.transient_item_attributes.entry(ship_id).or_default().insert(RACK_HEAT_ATTR_IDS[0], 1.0);
    });
    sys.test_with_actor_state_mut_async(move |st| {
        st.active_module_cycles.push(ActiveModuleCycle {
            ship_id,
            module_id: mod_hi,
            module_type_id: 484,
            owner_char_id: Some(CHAR_ID),
            rack_idx: 0,
            is_overloaded: true,
            heat_damage_per_cycle: 3.0,
            duration_s: 1.0,
            elapsed_s: 0.9,
            repeat: -1,
            ..Default::default()
        });
    })
    .await;

    let held = hold_db_writer(&ctx.db);
    // Step 1: accumulate first non-lethal heat damage delta (`35.0 -> 38.0`) while DB writer held.
    for _ in 0..20 {
        sys.ask_step_tick_only(0.2).await.unwrap();
        if !sys
            .test_with_actor_state_async(|st| st.post_unlock_shadows.pending_overheat_dmg_deltas.is_empty())
            .await
        {
            break;
        }
        sys.ask_step_tick_only(0.8).await.unwrap();
        if !sys
            .test_with_actor_state_async(|st| st.post_unlock_shadows.pending_overheat_dmg_deltas.is_empty())
            .await
        {
            break;
        }
    }
    assert!(
        !sys.test_with_actor_state_async(|st| st.post_unlock_shadows.pending_overheat_dmg_deltas.is_empty())
            .await
    );
    assert!(
        !sys.test_with_actor_state_async(|st| st.active_module_cycles.is_empty())
            .await
    );

    // Step 2: subsequent ticks see uncommitted delta (`38.0`) and burn out (`>= 40.0`) synchronously.
    for _ in 0..20 {
        sys.ask_step_tick_only(1.0).await.unwrap();
        if sys
            .test_with_actor_state_async(|st| st.active_module_cycles.is_empty())
            .await
        {
            break;
        }
    }
    assert!(
        sys.test_with_actor_state_async(|st| st.active_module_cycles.is_empty())
            .await,
        "cross-tick heat must burn out overloaded cycle even with DB writer held"
    );

    release_writer(held);
    let _ = sys.wait_for_post_unlock_barrier().await;
    assert!(mod_dmg(&ctx.db, mod_hi).is_some_and(|d| d >= 40.0 - 1e-6));

    // External DB rack-heat reset (e.g. station repair/session wipe) must reflect once rack_heat_pending is empty.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ship_it) = snap.items.get_mut(&ship_id) {
            for attr in RACK_HEAT_ATTR_IDS {
                ship_it.dynamic_attributes.insert(attr, 0.0);
            }
        }
        let tr = snap.transient_item_attributes.entry(ship_id).or_default();
        for attr in RACK_HEAT_ATTR_IDS {
            tr.insert(attr, 0.0);
        }
    });
    sys.ask_step_tick_only(0.1).await.unwrap();
    let _ = sys.wait_for_post_unlock_barrier().await;
    assert!(
        sys.test_with_actor_state_async(move |st| st.ship_rack_heat.get(&ship_id).map_or(0.0, |h| h[0]))
            .await
            < 1e-6
    );
}

/// Regression test (`BF-04`): unbounded `post_commit_tx` delivers >260 ballpark ops without loss,
/// marks Depot loot containers as rigid static balls without `CombatState`, and omits `CombatState` on corpses.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_1d_bf04_post_commit_no_drop_static_loot_and_noncombat_corpse() {
    use crate::services::mobile_deployables::{register_mobile_deployable, write_deployable_state, MobileDeployableDbOp, GROUP_MOBILE_DEPOT};
    use crate::services::pve_abyssal_pylons::AbyssalPylonsDbOp;

    const N_DEPOTS: i64 = 265;
    let ctx = test_ctx();
    let now = ctx.now_unix();
    let (base_depot, base_loot, pod_id) = (779_400_000_i64, 779_410_000_i64, 779_420_001_i64);

    ctx.db.with_mut_snapshot(|snap| {
        for i in 0..N_DEPOTS {
            let did = base_depot + i;
            let lid = base_loot + i;
            snap.items.insert(did, make_item(did, 33474, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1));
            snap.item_positions.insert(did, [1_000.0 + (i as f64) * 10.0, 0.0, 0.0]);
            snap.items.insert(lid, make_item(lid, 178, CHAR_ID, did, InventoryFlag::Cargo, 2));
        }
        snap.items.insert(pod_id, make_item(pod_id, 670, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1));
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.active_ship_id = pod_id;
        }
    });
    for i in 0..N_DEPOTS {
        register_mobile_deployable(&ctx, base_depot + i, 33474, GROUP_MOBILE_DEPOT, CHAR_ID, SYS_ID, 0, DVec3::new(1_000.0 + (i as f64) * 10.0, 0.0, 0.0));
    }
    write_deployable_state(&ctx, |st| {
        for i in 0..N_DEPOTS {
            if let Some(rec) = st.deployables.get_mut(&(base_depot + i)) {
                rec.anchor_completed = true;
            }
        }
    });

    let max_id_before = ctx.db.with_snapshot(|s| s.next_entity_id);
    let sys = ctx.systems.get_or_spawn(SYS_ID);
    // Production `handle_ball_damage_in_actor_collecting` removes destroyed depot/pod balls in-actor
    // before enqueueing `DepotDestroyed` / `PodBoundaryDestroyed` post-unlock ops.
    sys.test_with_ballpark_mut_async(move |bp| {
        for i in 0..N_DEPOTS {
            let _ = bp.remove_ball(base_depot + i);
        }
        let _ = bp.remove_ball(pod_id);
    })
    .await;
    sys.ask_step_tick_only(0.0).await.unwrap();
    let _ = sys.wait_for_post_unlock_barrier().await;
    assert_eq!(sys.dynamic_ball_count.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert!(sys.is_hibernated(), "system must start hibernated before post-commit dynamic corpse");
    let held = hold_db_writer(&ctx.db);
    let mut ops = PostUnlockOps::default();
    for i in 0..N_DEPOTS {
        ops.push(PostUnlockIntent::MobileDeployable(MobileDeployableDbOp::DepotDestroyed {
            solar_system_id: SYS_ID,
            ball_id: base_depot + i,
            owner_id: CHAR_ID,
            pos_arr: [1_000.0 + (i as f64) * 10.0, 0.0, 0.0],
            now_unix: now,
        }));
    }
    ops.push(PostUnlockIntent::AbyssalPylons(AbyssalPylonsDbOp::PodBoundaryDestroyed {
        solar_system_id: SYS_ID,
        ball_id: pod_id,
        b_pos: [500.0, 0.0, 0.0],
        owner_hint: CHAR_ID,
        now_unix: now,
    }));
    send_in_flight(&sys, &ctx, ops);

    release_writer(held);
    assert!(sys.wait_for_post_unlock_barrier().await);

    let (can_ids, corpse_id) = ctx.db.with_snapshot(|snap| {
        let cans: Vec<i64> = snap.items.values()
            .filter(|it| it.item_id > max_id_before && it.type_id == 23 && it.owner_id == CHAR_ID && it.location_id == i64::from(SYS_ID))
            .map(|it| it.item_id)
            .collect();
        let corpse = snap.items.values()
            .find(|it| it.item_id > max_id_before && it.type_id == 25 && it.owner_id == CHAR_ID && it.location_id == i64::from(SYS_ID))
            .map(|it| it.item_id);
        (cans, corpse)
    });
    assert_eq!(can_ids.len(), N_DEPOTS as usize, "all {} loot containers must persist in snap", N_DEPOTS);
    let corpse_id = corpse_id.expect("corpse item persisted");
    assert_eq!(sys.dynamic_ball_count.load(std::sync::atomic::Ordering::Relaxed), 1, "post-commit dynamic corpse must update dynamic_ball_count before barrier reply");
    assert!(!sys.is_hibernated(), "post-commit dynamic corpse must unhibernate system before barrier reply");
    let view = sys.view_rx.borrow().clone();
    assert!(view.balls.contains_key(&corpse_id), "SystemView must contain post-commit corpse immediately after barrier");
    assert!(!view.combat_states.contains_key(&corpse_id), "SystemView corpse must not have CombatState");
    let can_ids_check = can_ids.clone();
    sys.test_with_ballpark_async(move |bp| {
        for &cid in &can_ids_check {
            assert!(view.balls.contains_key(&cid) && view.static_ball_ids.contains(&cid), "SystemView missing rigid static loot can {cid} immediately after barrier");
            assert!(bp.balls.contains_key(&cid), "missing post-commit loot can {cid} (>256 channel test)");
            assert!(bp.is_static_ball(cid), "loot can {cid} must be rigid static ball");
            assert!(!bp.combat_states.contains_key(&cid), "loot can {cid} must not have CombatState");
        }
        assert!(bp.balls.contains_key(&corpse_id), "corpse must be added to ballpark");
        assert!(!bp.combat_states.contains_key(&corpse_id), "corpse must not have CombatState");
    })
    .await;
}

/// Regression test (`BF-05` tombstones): re-entering Abyssal/Proving Grounds boundaries or clearing all Abyssal clouds
/// while `db.writer` is held must reset tick streaks and clear stale DB clouds.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_1d_bf05_tombstone_shadows_and_empty_cloud_despawn() {
    use crate::services::pve_abyssal_hazards::TACHYON_CLOUD_TYPE_ID;
    use crate::services::pve_special_sites_abyssal::{AbyssalPocketSession, AbyssalRoomRecord, AbyssalWeatherKind};
    use crate::services::pvp_filament_mgr::{load_pvp_store, save_pvp_store, PvpArenaMatchInstance};
    use std::collections::HashSet;

    let ctx = test_ctx();
    let now = ctx.now_unix();
    let (abyss_sys, arena_sys) = (32_000_003_u32, 32_000_001_u32);
    let (ab_ship, cloud_id, arena_char, arena_ship) = (779_500_001_i64, 1_898_000_111_i64, 90_000_002_i64, 779_500_002_i64);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ab_ship, make_item(ab_ship, 587, CHAR_ID, i64::from(abyss_sys), InventoryFlag::None, 1));
        snap.abyssal_state.pockets.insert(CHAR_ID, AbyssalPocketSession {
            pocket_id: 1, character_id: CHAR_ID, ship_id: ab_ship, origin_solar_system_id: SYS_ID,
            abyssal_solar_system_id: abyss_sys, origin_position: [0.0; 3], grid_offset: [0.0; 3],
            filament_type_id: 47762, tier: 1, weather: AbyssalWeatherKind::Electrical,
            weather_penalty_pct: 0.0, weather_bonus_pct: 0.0, npc_difficulty_mult: 1.0,
            current_room: 1,
            rooms: [
                AbyssalRoomRecord { room_index: 1, dungeon_id: 1, dungeon_name: "r1".into(), npc_ball_ids: Vec::new(), cache_item_id: 0, cleared: true, gate_unlocked_at_sec: Some(now) },
                AbyssalRoomRecord { room_index: 2, dungeon_id: 1, dungeon_name: "r2".into(), npc_ball_ids: Vec::new(), cache_item_id: 0, cleared: false, gate_unlocked_at_sec: None },
                AbyssalRoomRecord { room_index: 3, dungeon_id: 1, dungeon_name: "r3".into(), npc_ball_ids: Vec::new(), cache_item_id: 0, cleared: false, gate_unlocked_at_sec: None },
            ],
            started_at_sec: now, collapse_at_sec: now + 1200,
            completed: false, collapsed_destroyed: false, trace_ball_id: 0,
            fleet_ship_ids: vec![ab_ship], ship_room: [(ab_ship, 1)].into_iter().collect(), max_ships: 1,
        });

        snap.items.insert(arena_ship, make_item(arena_ship, 587, arena_char, i64::from(arena_sys), InventoryFlag::None, 1));
        if let Some(ch) = snap.characters.get_mut(&arena_char) {
            (ch.active_ship_id, ch.solar_system_id, ch.station_id, ch.structure_id) = (arena_ship, arena_sys, None, None);
        }
        let mut store = load_pvp_store(snap);
        store.active_matches.insert(9101, PvpArenaMatchInstance {
            match_id: 9101, event_id: 171, arena_solar_system_id: arena_sys, end_gate_id: 779_500_099,
            team_a: vec![arena_char], team_b: Vec::new(), origin_systems: [(arena_char, SYS_ID)].into_iter().collect(),
            exited_pilots: HashSet::new(), winner_char_id: None, active: true, started_unix: now,
        });
        store.match_arena_offsets.insert(9101, [0.0, 0.0, 0.0]);
        save_pvp_store(snap, &store);
    });

    let sys_ab = ctx.systems.get_or_spawn(abyss_sys);
    sys_ab.spawn_ball_with_combat(make_ball(ab_ship, 587, DVec3::new(76_000.0, 0.0, 0.0)), make_combat_state(500.0, 500.0, 50_000.0, 100.0)).await.unwrap();
    sys_ab.spawn_ball(make_ball(cloud_id, TACHYON_CLOUD_TYPE_ID, DVec3::new(76_000.0, 0.0, 0.0))).await.unwrap();
    let sys_ar = ctx.systems.get_or_spawn(arena_sys);
    sys_ar.spawn_ball_with_combat(make_ball(arena_ship, 587, DVec3::new(80_000.0, 0.0, 0.0)), make_combat_state(500.0, 500.0, 50_000.0, 100.0)).await.unwrap();

    // Commit tick 1 in DB (`boundary_ticks[ab_ship] == 1`, `active_clouds[ab_ship] == TACHYON`, `arena_hazard_ticks[arena_ship] == 1`).
    sys_ab.ask_step_tick_only(1.0).await.unwrap();
    sys_ar.ask_step_tick_only(1.0).await.unwrap();
    let _ = sys_ab.wait_for_post_unlock_barrier().await;
    let _ = sys_ar.wait_for_post_unlock_barrier().await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.abyssal_state.boundary_ticks.get(&ab_ship), Some(&1));
        assert_eq!(snap.abyssal_state.active_clouds.get(&ab_ship), Some(&TACHYON_CLOUD_TYPE_ID));
        assert_eq!(load_pvp_store(snap).hazard_ticks.get(&arena_ship), Some(&1));
    });

    // Under held writer: move inside (clears streaks -> tombstone None), then back outside (must reset to 1, NOT 2).
    let held = hold_db_writer(&ctx.db);
    sys_ab
        .test_with_ballpark_mut_async(move |bp| bp.balls.get_mut(&ab_ship).unwrap().position = DVec3::new(10_000.0, 0.0, 0.0))
        .await;
    sys_ar
        .test_with_ballpark_mut_async(move |bp| bp.balls.get_mut(&arena_ship).unwrap().position = DVec3::new(10_000.0, 0.0, 0.0))
        .await;
    sys_ab.ask_step_tick_only(1.0).await.unwrap();
    sys_ar.ask_step_tick_only(1.0).await.unwrap();

    sys_ab
        .test_with_ballpark_mut_async(move |bp| bp.balls.get_mut(&ab_ship).unwrap().position = DVec3::new(0.0, 80_000.0, 0.0))
        .await;
    sys_ar
        .test_with_ballpark_mut_async(move |bp| bp.balls.get_mut(&arena_ship).unwrap().position = DVec3::new(80_000.0, 0.0, 0.0))
        .await;
    sys_ab.ask_step_tick_only(1.0).await.unwrap();
    sys_ar.ask_step_tick_only(1.0).await.unwrap();

    release_writer(held);
    let _ = sys_ab.wait_for_post_unlock_barrier().await;
    let _ = sys_ar.wait_for_post_unlock_barrier().await;
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.abyssal_state.boundary_ticks.get(&ab_ship), Some(&1));
        assert!(!snap.abyssal_state.active_clouds.contains_key(&ab_ship));
        assert_eq!(load_pvp_store(snap).hazard_ticks.get(&arena_ship), Some(&1));
    });

    // Empty-cloud cleanup test: enter cloud -> commit -> despawn cloud ball -> hold writer + tick -> DB cleared.
    sys_ab
        .test_with_ballpark_mut_async(move |bp| bp.balls.get_mut(&ab_ship).unwrap().position = DVec3::new(76_000.0, 0.0, 0.0))
        .await;
    sys_ab.ask_step_tick_only(1.0).await.unwrap();
    let _ = sys_ab.wait_for_post_unlock_barrier().await;
    assert_eq!(ctx.db.with_snapshot(|s| s.abyssal_state.active_clouds.get(&ab_ship).copied()), Some(TACHYON_CLOUD_TYPE_ID));
    sys_ab
        .test_with_ballpark_mut_async(move |bp| {
            let _ = bp.remove_ball(cloud_id);
        })
        .await;
    let held2 = hold_db_writer(&ctx.db);
    sys_ab.ask_step_tick_only(1.0).await.unwrap();
    release_writer(held2);
    let _ = sys_ab.wait_for_post_unlock_barrier().await;
    assert!(ctx.db.with_snapshot(|s| !s.abyssal_state.active_clouds.contains_key(&ab_ship)));
}

/// Regression test (`BF-05` current-thread direct ball-op): `step_ext_deployables_sync` on `CurrentThread`
/// assigns nonzero `work_seq`, blocks double MMJU fire under held DB writer, and prunes `mmju_spool_fired_at`.
#[tokio::test(flavor = "current_thread")]
async fn track_1d_bf05_current_thread_mmju_prune() {
    use crate::services::mobile_deployables::{
        register_mobile_deployable, write_deployable_state, MmjuSpoolEntry,
        GROUP_MOBILE_MICRO_JUMP_UNIT,
    };
    use crate::services::mobile_deployables_ext::{
        step_ext_deployables_in_actor_collecting, MMJU_SPOOL_SECS_U64,
    };

    let (ship_id, mmju_id, char_id2, ship_id2, mmju_id2) = (779_500_101_i64, 779_500_102_i64, CHAR_ID + 1, 779_500_103_i64, 779_500_104_i64);
    let (ctx, sys) = boot_ship(ship_id, &[], None, Some(make_combat_state(100.0, 100.0, 100.0, 100.0))).await;
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch2 = snap.characters[&CHAR_ID].clone();
        ch2.character_id = char_id2;
        ch2.user_id = char_id2 as u64;
        ch2.active_ship_id = ship_id2;
        snap.characters.insert(char_id2, ch2);
        snap.items.insert(mmju_id, make_item(mmju_id, 33591, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1));
        snap.item_positions.insert(mmju_id, [500.0, 0.0, 0.0]);
        snap.items.insert(ship_id2, make_item(ship_id2, 606, char_id2, i64::from(SYS_ID), InventoryFlag::None, 1));
        snap.items.insert(mmju_id2, make_item(mmju_id2, 33591, char_id2, i64::from(SYS_ID), InventoryFlag::None, 1));
        snap.item_positions.insert(mmju_id2, [500.0, 0.0, 0.0]);
    });
    register_mobile_deployable(&ctx, mmju_id, 33591, GROUP_MOBILE_MICRO_JUMP_UNIT, CHAR_ID, SYS_ID, 0, DVec3::new(500.0, 0.0, 0.0));
    register_mobile_deployable(&ctx, mmju_id2, 33591, GROUP_MOBILE_MICRO_JUMP_UNIT, char_id2, SYS_ID, 0, DVec3::new(500.0, 0.0, 0.0));
    write_deployable_state(&ctx, |st| {
        for mid in [mmju_id, mmju_id2] {
            let rec = st.deployables.get_mut(&mid).unwrap();
            rec.anchor_completed = true;
            rec.anchor_start_unix = now.saturating_sub(120);
        }
        st.mmju_spools.insert(ship_id, MmjuSpoolEntry {
            ship_id, char_id: CHAR_ID, unit_id: mmju_id, solar_system_id: SYS_ID,
            started_unix: now.saturating_sub(MMJU_SPOOL_SECS_U64), complete_at_unix: now,
            direction: [1.0, 0.0, 0.0],
        });
    });
    sys.spawn_ball(make_ball(mmju_id, 33591, DVec3::new(500.0, 0.0, 0.0))).await.unwrap();
    sys.spawn_ball(make_ball(mmju_id2, 33591, DVec3::new(500.0, 0.0, 0.0))).await.unwrap();
    sys.spawn_ball_with_combat(make_ball(ship_id2, 606, DVec3::ZERO), make_combat_state(100.0, 100.0, 100.0, 100.0)).await.unwrap();

    // Part A: under held DB writer, verify `mmju_spool_fired_at` blocks double fire before commit.
    let held = hold_db_writer(&ctx.db);
    for expected_double_fire_block in [false, true] {
        if expected_double_fire_block {
            sys.test_with_ballpark_mut_async(move |bp| {
                bp.balls.get_mut(&ship_id).unwrap().position = DVec3::ZERO;
            })
            .await;
        }
        let (sde_c, db_c, rng_c, sys_c) = (
            Arc::clone(&ctx.sde),
            Arc::clone(&ctx.db),
            Arc::clone(&ctx.mechanic_rng),
            sys.clone(),
        );
        sys.test_with_ballpark_and_state_mut_async(move |bp, st| {
            super::prune_actor_shadows_for_cmd(SYS_ID, st, &sys_c.completed_post_unlock_seq);
            let next_seq = st.next_post_unlock_seq.saturating_add(1);
            let mut ops = PostUnlockOps::default();
            step_ext_deployables_in_actor_collecting(
                bp, Some(&mut st.active_module_cycles), &sde_c, &db_c, Some(&rng_c), SYS_ID, now,
                Some((&mut st.post_unlock_shadows.mmju_spool_fired_at, next_seq)),
                Some((&mut st.post_unlock_shadows.observatory_pings_done, next_seq)),
                &mut ops,
            );
            let _ = super::enqueue_tick_post_unlock_or_barrier_under_locks(&sys_c, st, ops, false);
            if !expected_double_fire_block {
                assert!(bp.balls[&ship_id].position.x > 50_000.0, "MMJU must jump ship on first step");
                assert!(st.post_unlock_shadows.mmju_spool_fired_at.get(&ship_id).is_some_and(|&(_, seq)| seq > 0));
            } else {
                assert!(bp.balls[&ship_id].position.x < 1_000.0, "MMJU shadow must prevent double-fire");
            }
        })
        .await;
    }
    release_writer(held);
    let _ = sys.wait_for_post_unlock_barrier().await;

    // Part B: exercise `step_ext_deployables_sync` via `CtActorDriver` mailbox dispatch on `CurrentThread` runtime -> jumps ship_id2 and prunes committed shadows.
    write_deployable_state(&ctx, |st| {
        st.mmju_spools.insert(ship_id2, MmjuSpoolEntry {
            ship_id: ship_id2, char_id: char_id2, unit_id: mmju_id2, solar_system_id: SYS_ID,
            started_unix: now.saturating_sub(MMJU_SPOOL_SECS_U64), complete_at_unix: now,
            direction: [1.0, 0.0, 0.0],
        });
    });
    sys.step_ext_deployables_sync(Arc::clone(&ctx.sde), Arc::clone(&ctx.db), Arc::clone(&ctx.mechanic_rng), now).unwrap();
    assert!(
        !sys.test_with_actor_state_async(move |st| st.post_unlock_shadows.mmju_spool_fired_at.contains_key(&ship_id))
            .await,
        "ship_id committed shadow must be pruned"
    );
    assert!(sys.get_ball_position_sync(ship_id2).unwrap().x > 50_000.0, "MMJU must jump ship_id2 on CurrentThread");
    sys.step_ext_deployables_sync(Arc::clone(&ctx.sde), Arc::clone(&ctx.db), Arc::clone(&ctx.mechanic_rng), now).unwrap();
    assert!(
        sys.test_with_actor_state_async(|st| st.post_unlock_shadows.mmju_spool_fired_at.is_empty())
            .await,
        "current_thread command must prune committed mmju_spool_fired_at"
    );
}

/// Regression test (`BF-06`): Bosonic/GTFO expiry restore happens once under held DB writer so
/// fresh post-expiry warp scram/mobility changes on subsequent ticks are not clobbered by stale DB rows.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_1d_bf06_doomsday_expiry_shadow_preserves_new_tackle() {
    use crate::services::doomsday_aoe_tick::{
        mutate_superweapon_runtime, read_superweapon_runtime, PersistedSuperweaponCycle,
        PersistedWarpSuppression, SuperweaponCycleKind,
    };

    let ctx = test_ctx();
    let now = ctx.now_unix();
    let (titan_id, target_id) = (779_600_001_i64, 779_600_002_i64);
    let sys = ctx.systems.get_or_spawn(SYS_ID);

    let mut titan_ball = make_ball(titan_id, 11_567, DVec3::ZERO);
    titan_ball.max_velocity = 0.0;
    let mut titan_cs = make_combat_state(100.0, 100.0, 100.0, 100.0);
    titan_cs.warp_scramble_strength = 100;
    titan_cs.disallow_assistance = true;
    sys.spawn_ball_with_combat(titan_ball, titan_cs).await.unwrap();

    let mut tgt_cs = make_combat_state(100.0, 100.0, 100.0, 100.0);
    tgt_cs.warp_scramble_strength = 99;
    sys.spawn_ball_with_combat(make_ball(target_id, 606, DVec3::new(5_000.0, 0.0, 0.0)), tgt_cs).await.unwrap();

    mutate_superweapon_runtime(&ctx.db, |rt| {
        rt.active_cycles.push(PersistedSuperweaponCycle {
            sys_id: SYS_ID, char_id: CHAR_ID, ship_id: titan_id, ship_type_id: 11_567, mod_type_id: 24_550,
            kind: SuperweaponCycleKind::SpatialGtfo { center: [0.0; 3], sphere_radius_m: 10_000.0 },
            started_at_unix: now.saturating_sub(120), warmup_secs: 15, total_ticks: 1, ticks_applied: 1,
            immobilize_until_unix: now.saturating_sub(60),
            orig_max_velocity: 80.0, orig_warp_scramble_strength: 0, orig_disallow_assistance: false,
            mobility_restored: false,
        });
        rt.warp_suppressions.push(PersistedWarpSuppression {
            sys_id: SYS_ID, ship_id: target_id, suppressed_until_unix: now.saturating_sub(60),
            orig_warp_scramble_strength: 0,
        });
    });

    let held = hold_db_writer(&ctx.db);
    sys.ask_step_tick_only(1.0).await.unwrap();
    assert!((sys.get_ball_snapshot(titan_id).await.unwrap().unwrap().1.max_velocity - 80.0).abs() < 1e-6);
    assert_eq!(sys.get_combat_state(target_id).await.unwrap().unwrap().warp_scramble_strength, 0);

    // Apply fresh post-expiry tackle & speed clamp before stale DB superweapon rows are deleted.
    sys.set_warp_scramble_strength(target_id, 3).await.unwrap();
    assert!(sys.set_ball_max_velocity(titan_id, 12.0).await.unwrap());

    sys.ask_step_tick_only(1.0).await.unwrap();
    assert_eq!(sys.get_combat_state(target_id).await.unwrap().unwrap().warp_scramble_strength, 3, "shadow must prevent stale warp suppression restore from overwriting new tackle");
    assert!((sys.get_ball_snapshot(titan_id).await.unwrap().unwrap().1.max_velocity - 12.0).abs() < 1e-6, "shadow must prevent stale cycle mobility restore from overwriting new speed");

    release_writer(held);
    let _ = sys.wait_for_post_unlock_barrier().await;
    let rt_after = read_superweapon_runtime(&ctx.db);
    assert!(rt_after.is_none_or(|rt| rt.active_cycles.iter().all(|c| c.sys_id != SYS_ID) && rt.warp_suppressions.iter().all(|s| s.sys_id != SYS_ID)));
}

/// Regression test (`R2-BUG-01`): ship with non-zero legacy `dynamic_attributes` rack heat cooling to 0.0
/// clears `dynamic_attributes` and `transient_item_attributes` instead of resurrecting pre-cooling heat.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_1d_r2_bug01_legacy_rack_heat_cools_to_zero_without_resurrection() {
    use crate::services::tactical_module_mechanics::{
        step_rack_heat_per_tick_collecting, RACK_HEAT_ATTR_IDS,
    };

    let ship_id = 779_700_001_i64;
    let (ctx, sys) = boot_ship(ship_id, &[], None, Some(make_combat_state(100.0, 100.0, 100.0, 100.0))).await;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(sh) = snap.items.get_mut(&ship_id) {
            sh.dynamic_attributes.insert(RACK_HEAT_ATTR_IDS[0], 0.05);
        }
    });

    let (sde_c, db_c) = (Arc::clone(&ctx.sde), Arc::clone(&ctx.db));
    let ops = sys
        .test_with_actor_state_mut_async(move |st| {
            let mut ops = PostUnlockOps::default();
            let seq = st.next_post_unlock_seq.saturating_add(1);
            st.ship_rack_heat.insert(ship_id, [0.05, 0.0, 0.0]);
            let (rack_heat, rack_pending) = (&mut st.ship_rack_heat, &mut st.post_unlock_shadows.rack_heat_pending);
            step_rack_heat_per_tick_collecting(
                rack_heat,
                &[],
                &Some(sde_c),
                &Some(db_c),
                10_000.0,
                Some((rack_pending, seq)),
                &mut ops,
            );
            assert!(!rack_heat.contains_key(&ship_id), "cooled ship must drop from actor ship_rack_heat");
            ops
        })
        .await;
    send_in_flight(&sys, &ctx, ops);
    assert!(sys.wait_for_post_unlock_barrier().await);

    ctx.db.with_snapshot(|snap| {
        let sh = &snap.items[&ship_id];
        for aid in RACK_HEAT_ATTR_IDS {
            assert!(!sh.dynamic_attributes.contains_key(&aid), "legacy dynamic rack heat {aid} must be removed");
        }
        let tr_heat = snap
            .transient_item_attributes
            .get(&ship_id)
            .and_then(|m| m.get(&RACK_HEAT_ATTR_IDS[0]))
            .copied();
        assert_eq!(tr_heat, None, "cooled legacy rack heat must not be resurrected in transient_item_attributes");
    });
}

/// Regression test (`R2-BUG-03`): concurrent Abyssal pocket systems preserve each other's room NPC `active_clouds`,
/// while high-bit 64-bit `location_id`s whose low 32 bits match `solar_system_id` do not suppress cleanup.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_1d_r2_bug03_concurrent_abyssal_systems_preserve_npc_active_clouds() {
    use crate::services::pve_abyssal_hazards::TACHYON_CLOUD_TYPE_ID;
    use crate::services::pve_special_sites_abyssal::{AbyssalPocketSession, AbyssalRoomRecord, AbyssalWeatherKind};

    let ctx = test_ctx();
    let now = ctx.now_unix();
    let (sys_a, sys_b) = (32_000_002_u32, 32_000_003_u32);
    let (ship_a, cloud_a, npc_b, cloud_b, high_loc_ship) = (
        779_700_101_i64,
        1_898_000_101_i64,
        1_890_000_001_i64,
        1_898_000_201_i64,
        779_700_102_i64,
    );
    let alias_high_loc = (1_i64 << 32) | i64::from(sys_b);

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(ship_a, make_item(ship_a, 587, CHAR_ID, i64::from(sys_a), InventoryFlag::None, 1));
        snap.items.insert(high_loc_ship, make_item(high_loc_ship, 587, CHAR_ID, alias_high_loc, InventoryFlag::None, 1));
        snap.abyssal_state.active_clouds.insert(high_loc_ship, TACHYON_CLOUD_TYPE_ID);
        let mk_rooms = |npcs: Vec<i64>| [
            AbyssalRoomRecord { room_index: 1, dungeon_id: 1, dungeon_name: "r1".into(), npc_ball_ids: npcs, cache_item_id: 0, cleared: false, gate_unlocked_at_sec: None },
            AbyssalRoomRecord { room_index: 2, dungeon_id: 1, dungeon_name: "r2".into(), npc_ball_ids: Vec::new(), cache_item_id: 0, cleared: false, gate_unlocked_at_sec: None },
            AbyssalRoomRecord { room_index: 3, dungeon_id: 1, dungeon_name: "r3".into(), npc_ball_ids: Vec::new(), cache_item_id: 0, cleared: false, gate_unlocked_at_sec: None },
        ];
        snap.abyssal_state.pockets.insert(CHAR_ID, AbyssalPocketSession {
            pocket_id: 1, character_id: CHAR_ID, ship_id: ship_a, origin_solar_system_id: SYS_ID,
            abyssal_solar_system_id: sys_a, origin_position: [0.0; 3], grid_offset: [0.0; 3],
            filament_type_id: 47762, tier: 1, weather: AbyssalWeatherKind::Electrical,
            weather_penalty_pct: 0.0, weather_bonus_pct: 0.0, npc_difficulty_mult: 1.0,
            current_room: 1, rooms: mk_rooms(Vec::new()), started_at_sec: now, collapse_at_sec: now + 1200,
            completed: false, collapsed_destroyed: false, trace_ball_id: 0,
            fleet_ship_ids: vec![ship_a], ship_room: [(ship_a, 1)].into_iter().collect(), max_ships: 1,
        });
        snap.abyssal_state.pockets.insert(CHAR_ID + 10, AbyssalPocketSession {
            pocket_id: 2, character_id: CHAR_ID + 10, ship_id: 779_700_109, origin_solar_system_id: SYS_ID,
            abyssal_solar_system_id: sys_b, origin_position: [0.0; 3], grid_offset: [0.0; 3],
            filament_type_id: 47762, tier: 1, weather: AbyssalWeatherKind::Electrical,
            weather_penalty_pct: 0.0, weather_bonus_pct: 0.0, npc_difficulty_mult: 1.0,
            current_room: 1, rooms: mk_rooms(vec![npc_b]), started_at_sec: now, collapse_at_sec: now + 1200,
            completed: false, collapsed_destroyed: false, trace_ball_id: 0,
            fleet_ship_ids: Vec::new(), ship_room: HashMap::new(), max_ships: 1,
        });
    });

    let sys_handle_a = ctx.systems.get_or_spawn(sys_a);
    sys_handle_a.spawn_ball_with_combat(make_ball(ship_a, 587, DVec3::new(10_000.0, 0.0, 0.0)), make_combat_state(500.0, 500.0, 500.0, 100.0)).await.unwrap();
    sys_handle_a.spawn_ball(make_ball(cloud_a, TACHYON_CLOUD_TYPE_ID, DVec3::new(10_000.0, 0.0, 0.0))).await.unwrap();

    let sys_handle_b = ctx.systems.get_or_spawn(sys_b);
    sys_handle_b.spawn_ball_with_combat(make_ball(npc_b, 587, DVec3::new(10_000.0, 0.0, 0.0)), make_combat_state(500.0, 500.0, 500.0, 100.0)).await.unwrap();
    sys_handle_b.spawn_ball(make_ball(cloud_b, TACHYON_CLOUD_TYPE_ID, DVec3::new(10_000.0, 0.0, 0.0))).await.unwrap();

    sys_handle_b.ask_step_tick_only(1.0).await.unwrap();
    assert!(sys_handle_b.wait_for_post_unlock_barrier().await);
    assert_eq!(ctx.db.with_snapshot(|s| s.abyssal_state.active_clouds.get(&npc_b).copied()), Some(TACHYON_CLOUD_TYPE_ID));

    sys_handle_a.ask_step_tick_only(1.0).await.unwrap();
    assert!(sys_handle_a.wait_for_post_unlock_barrier().await);
    ctx.db.with_snapshot(|snap| {
        assert_eq!(snap.abyssal_state.active_clouds.get(&npc_b).copied(), Some(TACHYON_CLOUD_TYPE_ID), "system A ticks must not evict system B room NPC active_clouds");
        assert!(!snap.abyssal_state.active_clouds.contains_key(&high_loc_ship), "high 64-bit location_id aliasing sys_b in low 32 bits must not block stale cleanup");
    });
}

/// Regression test (`F-01`): an expiring Mobile Observatory with a due final boundary ping
/// executes its boundary ping / decloak roll at most once across multiple held-writer ticks,
/// retains `obs_ping_shadow` across expiry until `SweepExpired` commits, and prunes cleanly afterward.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_1d_f01_expiring_observatory_boundary_ping_fires_once_under_held_writer() {
    use crate::services::mobile_deployables::{
        read_deployable_state, register_mobile_deployable,
        step_mobile_deployables_in_actor_collecting, write_deployable_state, MobileDeployableDbOp,
        GROUP_MOBILE_OBSERVATORY, OBSERVATORY_PING_INTERVAL_SECS,
    };
    use crate::services::mobile_deployables_ext::{MobileExtDbOp, TYPE_MOBILE_OBSERVATORY};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (ship_id, obs1_id, obs_pre_id) = (779_800_001_i64, 779_800_101, 779_800_102);
    let cloak_ships: Vec<i64> = (0..6).map(|i| 779_800_200 + i).collect();
    let cloak_mods: Vec<i64> = (0..6).map(|i| 779_800_300 + i).collect();
    let (ctx, sys) = boot_ship(ship_id, &[], None, Some(make_combat_state(100.0, 100.0, 100.0, 100.0))).await;
    let now = ctx.now_unix();

    let deact_calls = Arc::new(AtomicUsize::new(0));
    {
        let dc = Arc::clone(&deact_calls);
        let hook: crate::system_actor::ModuleDeactivationHook = Arc::new(move |_sys, _cid, _cyc| {
            dc.fetch_add(1, Ordering::SeqCst);
        });
        *sys.module_deactivation_hook.write().unwrap() = Some(hook);
    }

    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(obs1_id, make_item(obs1_id, TYPE_MOBILE_OBSERVATORY, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1));
        snap.item_positions.insert(obs1_id, [2_000.0, 0.0, 0.0]);
        for i in 0..cloak_ships.len() {
            snap.items.insert(cloak_ships[i], make_item(cloak_ships[i], 606, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1));
            snap.items.insert(cloak_mods[i], make_item(cloak_mods[i], 11578, CHAR_ID, cloak_ships[i], InventoryFlag::HiSlot0, 1));
            snap.module_active_states.insert(cloak_mods[i]);
        }
    });
    register_mobile_deployable(&ctx, obs1_id, TYPE_MOBILE_OBSERVATORY, GROUP_MOBILE_OBSERVATORY, CHAR_ID, SYS_ID, 0, DVec3::new(2_000.0, 0.0, 0.0));
    write_deployable_state(&ctx, |st| {
        let rec = st.deployables.get_mut(&obs1_id).unwrap();
        rec.anchor_completed = true;
        rec.anchor_start_unix = now.saturating_sub(OBSERVATORY_PING_INTERVAL_SECS + 30);
        rec.expires_at_unix = now;
        st.observatory_pings_done.insert(obs1_id, 1);
    });
    sys.spawn_ball(make_ball(obs1_id, TYPE_MOBILE_OBSERVATORY, DVec3::new(2_000.0, 0.0, 0.0))).await.unwrap();
    for i in 0..cloak_ships.len() {
        let mut cloak_cs = make_combat_state(100.0, 100.0, 100.0, 100.0);
        cloak_cs.is_cloaked = true;
        sys.spawn_ball_with_combat(make_ball(cloak_ships[i], 606, DVec3::new(2_500.0 + (i as f64) * 50.0, 0.0, 0.0)), cloak_cs).await.unwrap();
        let mut cyc = cloak_cyc(cloak_ships[i], cloak_mods[i], 30.0, 0.0, 1_000);
        cyc.elapsed_s = 0.0;
        sys.test_with_actor_state_mut_async(move |st| st.active_module_cycles.push(cyc))
            .await;
    }

    // Part 1: real actor ticks across expiry while `db.writer` is held must run the final boundary ping once.
    let held = hold_db_writer(&ctx.db);
    sys.ask_step_tick_only(1.0).await.unwrap();
    let shadow_t1 = sys
        .test_with_actor_state_async(move |st| st.post_unlock_shadows.observatory_pings_done.get(&obs1_id).copied())
        .await;
    assert!(matches!(shadow_t1, Some((2, seq)) if seq > 0), "expiring Observatory must keep (2, seq) in obs_ping_shadow before commit: {shadow_t1:?}");
    assert!(
        !sys.test_with_ballpark_async(move |bp| bp.balls.contains_key(&obs1_id))
            .await,
        "expired Observatory ball must be swept on tick 1"
    );
    let cloak_ships_c = cloak_ships.clone();
    let cloaked_t1: Vec<(i64, bool)> = sys
        .test_with_ballpark_async(move |bp| {
            cloak_ships_c.iter().map(|&id| (id, bp.combat_states[&id].is_cloaked)).collect()
        })
        .await;

    for tick in 2..=3 {
        sys.ask_step_tick_only(1.0).await.unwrap();
        assert_eq!(
            sys.test_with_actor_state_async(move |st| st.post_unlock_shadows.observatory_pings_done.get(&obs1_id).copied())
                .await,
            shadow_t1,
            "held-writer tick {tick} must keep exact (2, seq) shadow without re-firing"
        );
        let cloaked_t1_c = cloaked_t1.clone();
        sys.test_with_ballpark_async(move |bp| {
            for &(cid, was_cloaked) in &cloaked_t1_c {
                assert_eq!(bp.combat_states[&cid].is_cloaked, was_cloaked, "held-writer tick {tick} must not re-roll decloak on surviving cloaked ship {cid}");
            }
        })
        .await;
    }
    release_writer(held);
    assert!(sys.wait_for_post_unlock_barrier().await);
    sys.ask_step_tick_only(1.0).await.unwrap();
    assert!(sys.wait_for_post_unlock_barrier().await);

    let expected_deact = cloaked_t1.iter().filter(|&&(_, cloaked)| !cloaked).count();
    assert_eq!(deact_calls.load(Ordering::SeqCst), expected_deact, "decloaked ships must trigger DeactivationHook at most once");
    assert!(!read_deployable_state(&ctx).deployables.contains_key(&obs1_id));
    assert!(!read_deployable_state(&ctx).observatory_pings_done.contains_key(&obs1_id));
    assert!(ctx.db.with_snapshot(|s| !s.items.contains_key(&obs1_id)));
    assert!(
        sys.test_with_actor_state_async(|st| st.post_unlock_shadows.observatory_pings_done.is_empty())
            .await,
        "post-commit prune_committed must clear obs_ping_shadow"
    );

    // Part 2: pre-expiry ping under held writer followed by crossing `expires_at_unix` before commit.
    let now2 = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(obs_pre_id, make_item(obs_pre_id, TYPE_MOBILE_OBSERVATORY, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1));
        snap.item_positions.insert(obs_pre_id, [3_000.0, 0.0, 0.0]);
    });
    register_mobile_deployable(&ctx, obs_pre_id, TYPE_MOBILE_OBSERVATORY, GROUP_MOBILE_OBSERVATORY, CHAR_ID, SYS_ID, 0, DVec3::new(3_000.0, 0.0, 0.0));
    write_deployable_state(&ctx, |st| {
        let rec = st.deployables.get_mut(&obs_pre_id).unwrap();
        rec.anchor_completed = true;
        rec.anchor_start_unix = now2.saturating_sub(OBSERVATORY_PING_INTERVAL_SECS + 30);
        rec.expires_at_unix = now2 + 10;
        st.observatory_pings_done.insert(obs_pre_id, 1);
    });
    sys.spawn_ball(make_ball(obs_pre_id, TYPE_MOBILE_OBSERVATORY, DVec3::new(3_000.0, 0.0, 0.0))).await.unwrap();

    let held2 = hold_db_writer(&ctx.db);
    let mut seq_pre = 0_u64;
    for (ts, expect_ping, expect_sweep) in [(now2 + 5, true, false), (now2 + 10, false, true), (now2 + 11, false, true)] {
        let (sde_c, rng_c, sys_c) = (Arc::clone(&ctx.sde), Arc::clone(&ctx.mechanic_rng), sys.clone());
        seq_pre = sys
            .test_with_ballpark_and_state_mut_async(move |bp, st| {
                let mut ops = PostUnlockOps::default();
                super::prune_actor_shadows_for_cmd(SYS_ID, st, &sys_c.completed_post_unlock_seq);
                let next_seq = st.next_post_unlock_seq.saturating_add(1);
                let cur_seq_pre = if expect_ping { next_seq } else { seq_pre };
                step_mobile_deployables_in_actor_collecting(
                    bp, &mut st.active_module_cycles, &rng_c, Some(&sde_c), st.db_opt.as_ref(),
                    Some(&rng_c), SYS_ID, 1.0, ts,
                    Some((&mut st.post_unlock_shadows.mmju_spool_fired_at, next_seq)),
                    Some((&mut st.post_unlock_shadows.observatory_pings_done, next_seq)),
                    &mut ops,
                );
                let ping_steps: Vec<_> = ops.intents.iter().filter(|i| matches!(i, PostUnlockIntent::MobileExt(MobileExtDbOp::ObservatoryStep { .. }))).collect();
                let sweeps = ops.intents.iter().filter(|i| matches!(i, PostUnlockIntent::MobileDeployable(MobileDeployableDbOp::SweepExpired { expired_ids }) if expired_ids.contains(&obs_pre_id))).count();
                assert_eq!(ping_steps.len(), usize::from(expect_ping), "unexpected ObservatoryStep count at ts={ts}");
                assert_eq!(sweeps, usize::from(expect_sweep), "unexpected SweepExpired count at ts={ts}");
                assert_eq!(st.post_unlock_shadows.observatory_pings_done.get(&obs_pre_id).copied(), Some((2, cur_seq_pre)), "obs_ping_shadow must survive SweepExpired queuing until commit");
                let _ = super::enqueue_tick_post_unlock_or_barrier_under_locks(&sys_c, st, ops, false);
                cur_seq_pre
            })
            .await;
    }
    release_writer(held2);
    assert!(sys.wait_for_post_unlock_barrier().await);
    sys.ask_step_tick_only(1.0).await.unwrap();
    assert!(sys.wait_for_post_unlock_barrier().await);
    assert!(!read_deployable_state(&ctx).deployables.contains_key(&obs_pre_id));
    assert!(!read_deployable_state(&ctx).observatory_pings_done.contains_key(&obs_pre_id));
    assert!(
        sys.test_with_actor_state_async(|st| st.post_unlock_shadows.observatory_pings_done.is_empty())
            .await
    );
}

/// `#1342` Track 4D-2: All 4 `worker_cb_tx` callbacks succeed with `cmd_tx` completely full
/// (`biased;` polling ahead of `next_actor_cmd_batch`), and fail closed once the actor stops.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_4d2_worker_callbacks_succeed_with_full_mailbox_and_biased_priority() {
    use super::{call_actor_callback, PostCommitBallparkOp, PostUnlockActorCallback};
    use crate::system_actor::{PendingCycleAction, SystemCommand};
    use std::sync::atomic::Ordering;

    let (ship_id, ship_rm, mod_id) = (779_900_001_i64, 779_900_002_i64, 779_900_101_i64);
    let (ctx, sys) = boot_ship(
        ship_rm,
        &[],
        None,
        Some(make_combat_state(100.0, 100.0, 100.0, 100.0)),
    )
    .await;
    seed_ship_char(
        &ctx.db,
        SYS_ID,
        CHAR_ID,
        ship_id,
        &[(mod_id, 11578, InventoryFlag::HiSlot0, 0.0)],
        None,
    );
    ctx.systems.set_module_deactivation_post_unlock_hook((
        Arc::new(|_, _, _| {}),
        Arc::new(|_, _, _, _| {}),
    ));
    let post_pair = sys
        .module_deactivation_post_unlock_hook
        .read()
        .unwrap()
        .clone();

    sys.test_with_actor_state_mut_async(|st| {
        st.weapon_effects_dirty = true;
        st.weapon_effects_seq = 1;
        st.weapon_effects_dispatched_seq = 1;
        st.weapon_effects_coord.seq.store(1, Ordering::Release);
        st.weapon_effects_coord.flushed_seq.store(1, Ordering::Release);
        st.weapon_effects_coord.dirty.store(false, Ordering::Release);
        st.weapon_effects_coord.retry_needed.store(false, Ordering::Release);
        st.weapon_effects_coord
            .has_pending_legacy_prune
            .store(false, Ordering::Release);
    })
    .await;
    assert!(!sys.is_hibernated());

    let (p1_tx, p1_rx) = mpsc::channel();
    let (r1_tx, r1_rx) = mpsc::channel();
    *sys.before_step_reply_hook_for_test.lock().unwrap() = Some(Box::new(move || {
        let _ = p1_tx.send(());
        let _ = r1_rx.recv();
    }));
    let step1 = tokio::spawn({
        let s = sys.clone();
        async move { s.ask_step_tick_only(0.1).await }
    });
    p1_rx.recv_timeout(Duration::from_secs(2)).expect("step1 reached hook");

    let (p2_tx, p2_rx) = mpsc::channel();
    let (r2_tx, r2_rx) = mpsc::channel();
    *sys.before_step_reply_hook_for_test.lock().unwrap() = Some(Box::new(move || {
        let _ = p2_tx.send(());
        let _ = r2_rx.recv();
    }));

    // Fill `cmd_tx` to full capacity (`SYSTEM_MAILBOX_CAPACITY == 256`), starting with a barrier step.
    let (step2_tx, step2_rx) = tokio::sync::oneshot::channel();
    sys.cmd_sender()
        .try_send(SystemCommand::StepManualTickOnly {
            dt: 0.1,
            reply: step2_tx,
        })
        .expect("enqueue step2 barrier");
    let mut flood_rx = Vec::new();
    loop {
        let (tx, rx) = tokio::sync::oneshot::channel();
        match sys.cmd_sender().try_send(SystemCommand::CurrentTick { reply: tx }) {
            Ok(()) => flood_rx.push(rx),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => break,
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => panic!("actor closed"),
        }
    }
    assert_eq!(sys.cmd_sender().capacity(), 0, "cmd_tx must be completely full");

    // Enqueue all 4 worker callbacks while `cmd_tx` is completely full.
    let (cb1_tx, cb1_rx) = mpsc::sync_channel(1);
    let mut deact_ops = PostUnlockOps::default();
    deact_ops.push(PostUnlockIntent::DeactivationHook(super::DeferredDeactivationHookOp {
        solar_system_id: SYS_ID,
        char_id: Some(CHAR_ID),
        fallback_owner_id: Some(CHAR_ID),
        cycle: ActiveModuleCycle {
            ship_id,
            module_id: mod_id,
            module_type_id: 11578,
            reactivation_delay_s: 20.0,
            ..Default::default()
        },
    }));
    sys.worker_cb_tx
        .send(PostUnlockActorCallback::FinalizeDeactivations {
            ops: deact_ops,
            raw: None,
            post_pair,
            reply: cb1_tx,
        })
        .unwrap();

    let (cb2_tx, cb2_rx) = mpsc::sync_channel(1);
    sys.worker_cb_tx
        .send(PostUnlockActorCallback::ApplyPostCommitBallparkOps {
            local_ops: vec![PostCommitBallparkOp::RemoveBall { ball_id: ship_rm }],
            raw_hook: None,
            post_pair: None,
            reply: cb2_tx,
        })
        .unwrap();

    let (cb3_tx, cb3_rx) = mpsc::sync_channel(1);
    sys.worker_cb_tx
        .send(PostUnlockActorCallback::ClearWeaponEffectsDirty { reply: cb3_tx })
        .unwrap();

    let (cb4_tx, cb4_rx) = mpsc::sync_channel(1);
    sys.worker_cb_tx
        .send(PostUnlockActorCallback::ExtendPendingCycleActions {
            extra_actions: vec![PendingCycleAction::InvalidateShipFitting {
                char_id: Some(CHAR_ID),
                ship_id,
            }],
            reply: cb4_tx,
        })
        .unwrap();

    // Release step1; `biased;` select must process all 4 callbacks BEFORE `step2` from `cmd_rx`.
    r1_tx.send(()).unwrap();
    p2_rx.recv_timeout(Duration::from_secs(2)).expect("step2 reached hook");

    let deact_res = cb1_rx.try_recv().expect("FinalizeDeactivations must complete ahead of cmd_rx");
    assert_eq!(deact_res.len(), 1);
    assert!(deact_res[0].cooldown_payload.is_some());

    let cleanup_ops = cb2_rx
        .try_recv()
        .expect("ApplyPostCommitBallparkOps must complete ahead of cmd_rx");
    assert!(!cleanup_ops.is_empty());

    cb3_rx
        .try_recv()
        .expect("ClearWeaponEffectsDirty must complete ahead of cmd_rx");
    cb4_rx
        .try_recv()
        .expect("ExtendPendingCycleActions must complete ahead of cmd_rx");

    let view = sys.view();
    assert!(!view.balls.contains_key(&ship_rm), "post-commit RemoveBall must update SystemView");
    assert!(
        view.module_cooldowns
            .reactivation_cooldowns
            .get(&(ship_id, mod_id, 11578))
            .is_some_and(|e| e.delay_s >= 19.0),
        "FinalizeDeactivations must record cloak cooldown in SystemView"
    );
    assert!(
        sys.is_hibernated(),
        "ClearWeaponEffectsDirty after RemoveBall must unblock hibernation immediately"
    );

    r2_tx.send(()).unwrap();
    step1.await.unwrap().unwrap();
    step2_rx.await.unwrap();
    for rx in flood_rx {
        let _ = rx.await.unwrap();
    }

    assert!(
        !sys.test_with_actor_state_async(|st| st.weapon_effects_dirty)
            .await
    );
    assert_eq!(
        sys.test_with_actor_state_async(|st| st.pending_cycle_actions.len())
            .await,
        1
    );

    assert!(ctx.systems.stop_system(SYS_ID).await);
    assert!(
        call_actor_callback(&sys.worker_cb_tx, |reply| {
            PostUnlockActorCallback::ClearWeaponEffectsDirty { reply }
        })
        .is_none(),
        "callbacks must fail closed immediately once actor has stopped"
    );
}

/// `#1342` Track 4D-2: Shutdown with a queued worker item that invokes a post-commit ballpark op
/// completes promptly by servicing `worker_cb_rx` alongside `post_unlock_worker_join` (no 5s timeout).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_4d2_shutdown_with_queued_post_commit_ballpark_op_completes_promptly() {
    use crate::services::pve_abyssal_pylons::AbyssalPylonsDbOp;
    use crate::system_actor::SystemLifecycleState;

    let pod_id = 779_910_001_i64;
    let ctx = test_ctx();
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            pod_id,
            make_item(pod_id, 670, CHAR_ID, i64::from(SYS_ID), InventoryFlag::None, 1),
        );
        if let Some(ch) = snap.characters.get_mut(&CHAR_ID) {
            ch.active_ship_id = pod_id;
        }
    });
    let max_id_before = ctx.db.with_snapshot(|s| s.next_entity_id);
    let sys = ctx.systems.get_or_spawn(SYS_ID);

    let held = hold_db_writer(&ctx.db);
    let mut ops = PostUnlockOps::default();
    ops.push(PostUnlockIntent::AbyssalPylons(
        AbyssalPylonsDbOp::PodBoundaryDestroyed {
            solar_system_id: SYS_ID,
            ball_id: pod_id,
            b_pos: [500.0, 0.0, 0.0],
            owner_hint: CHAR_ID,
            now_unix: now,
        },
    ));
    send_in_flight(&sys, &ctx, ops);
    wait_until("worker in flight", || sys.has_in_flight_post_unlock()).await;

    let stop_fut = tokio::spawn({
        let reg = Arc::clone(&ctx.systems);
        async move { reg.stop_system(SYS_ID).await }
    });
    wait_until("system entered Stopping", || {
        ctx.systems.system_lifecycle_state(SYS_ID) == SystemLifecycleState::Stopping
    })
    .await;

    let t0 = std::time::Instant::now();
    release_writer(held);
    assert!(stop_fut.await.unwrap(), "stop_system must succeed");
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "shutdown with queued post-commit ballpark op must not hit 5s timeout: {:?}",
        t0.elapsed()
    );

    let corpse_spawned_in_view = sys
        .view()
        .balls
        .keys()
        .any(|&bid| bid > max_id_before);
    assert!(
        corpse_spawned_in_view,
        "post-commit corpse ball must be applied inside actor before shutdown completes"
    );
    assert!(
        ctx.db.with_snapshot(|s| {
            s.items
                .values()
                .any(|it| it.item_id > max_id_before && it.type_id == 25 && it.owner_id == CHAR_ID)
        }),
        "corpse item must persist during shutdown drain"
    );
}

/// `#1342` Track 4D-2: Dropping a `CurrentThread` `tokio::runtime::Runtime` triggers the sentinel
/// to stop the `CtActorDriver` actor and close `post_unlock_tx` and `cmd_tx` cleanly.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_4d2_current_thread_runtime_drop_sentinel_closes_channels() {
    use crate::system_actor::SystemRegistry;

    let ctx = test_ctx();
    let mt_handle = ctx.systems.get_or_spawn(SYS_ID);
    assert!(
        mt_handle.ct_driver.is_none(),
        "MultiThread actors must incur zero CtActorDriver overhead"
    );
    assert!(ctx.systems.stop_system(SYS_ID).await);

    // Case A: `CurrentThread` runtime dropped while `SystemRegistry` and `SystemHandle` stay alive.
    let reg = Arc::new(SystemRegistry::with_sde(Arc::clone(&ctx.sde)));
    let reg_spawn = Arc::clone(&reg);
    let h = std::thread::spawn(move || {
        let ct_rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build CurrentThread runtime");
        let h = {
            let _enter = ct_rt.enter();
            reg_spawn.get_or_spawn(SYS_ID)
        };
        assert!(h.ct_driver.is_some(), "CurrentThread actor must attach CtActorDriver");
        assert!(!h.cmd_sender().is_closed());
        assert!(!h.post_unlock_tx.is_closed());
        // Exercise actor mailbox through `CtActorDriver` before dropping `ct_rt`.
        h.try_set_ball_position(779_920_001, DVec3::new(10.0, 0.0, 0.0));
        drop(ct_rt);
        h
    })
    .join()
    .expect("join CurrentThread drop thread");

    assert!(
        h.post_unlock_tx.is_closed(),
        "sentinel must close post_unlock_tx when CurrentThread Runtime drops"
    );
    assert!(
        h.cmd_sender().is_closed(),
        "sentinel must close cmd_tx when CurrentThread Runtime drops"
    );
    assert!(!reg.dead_live_system_handles().is_empty());

    // Case B: `SystemRegistry` and `SystemHandle` dropped before `CurrentThread` runtime polls the
    // actor future or drops (verifies BF-02: no 6-second stall in `CtSentinelGuard::drop`).
    let sde_b = Arc::clone(&ctx.sde);
    let case_b_start = std::time::Instant::now();
    std::thread::spawn(move || {
        let ct_rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build CurrentThread runtime B");
        {
            let _enter = ct_rt.enter();
            let reg_b = Arc::new(SystemRegistry::with_sde(sde_b));
            let h_b = reg_b.get_or_spawn(30_000_144);
            assert!(h_b.ct_driver.is_some());
        }
        // Polling while `ct_rt` stays alive must also not hit the 6s condvar timeout.
        ct_rt.block_on(tokio::task::yield_now());
        drop(ct_rt);
    })
    .join()
    .expect("dropping SystemRegistry+SystemHandle before CurrentThread Runtime must not panic");
    assert!(
        case_b_start.elapsed() < Duration::from_secs(2),
        "dropping CurrentThread actor before first poll must not stall CtSentinelGuard for 6s (elapsed: {:?})",
        case_b_start.elapsed()
    );
}

/// `#1342` Track 4D-2 (`BF-01`): Dropping the last `Arc<CtActorDriver>` / `SystemHandle` from
/// inside a `CtActorDriver` worker thread (including via `post_unlock_actions`) must not deadlock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn track_4d2_ct_actor_driver_drop_from_own_worker_and_post_unlock_actions_no_deadlock() {
    use crate::system_actor::SystemRegistry;

    let ctx = test_ctx();

    // Part A: Last `Arc<CtActorDriver>` dropped directly from a task running on `CtActorDriver`'s
    // worker threads.
    let reg_a = Arc::new(SystemRegistry::with_sde(Arc::clone(&ctx.sde)));
    let (h_a, ct_thread_a) = {
        let reg_spawn_a = Arc::clone(&reg_a);
        let (h_tx, h_rx) = std::sync::mpsc::sync_channel(1);
        let (drop_rt_tx, drop_rt_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let join_a = std::thread::spawn(move || {
            let ct_rt_a = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build CurrentThread runtime A");
            let h_a = {
                let _enter = ct_rt_a.enter();
                reg_spawn_a.get_or_spawn(SYS_ID)
            };
            let _ = h_tx.send(h_a);
            let _ = drop_rt_rx.recv();
            drop(ct_rt_a);
        });
        let h_a = h_rx.recv().unwrap();
        let driver_a = Arc::clone(h_a.ct_driver.as_ref().expect("CurrentThread attaches driver"));
        let driver_rt_a = driver_a.rt_handle().clone();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel::<()>();
        let (ext_dropped_tx, ext_dropped_rx) = tokio::sync::oneshot::channel::<()>();
        driver_rt_a.spawn(async move {
            let _ = ext_dropped_rx.await;
            drop(driver_a);
            let _ = dropped_tx.send(());
        });
        drop(h_a);
        drop(reg_a);
        let _ = ext_dropped_tx.send(());
        tokio::time::timeout(Duration::from_secs(2), dropped_rx)
            .await
            .expect("dropping last Arc<CtActorDriver> on its own worker thread must not deadlock")
            .expect("driver worker task must complete");
        let _ = drop_rt_tx.send(());
        ((), join_a)
    };
    let _ = h_a;
    ct_thread_a.join().expect("join CurrentThread runtime thread A");

    // Part B: Cloned `SystemHandle` moved into `post_unlock_actions`, executed on the
    // `post_unlock_worker` OS thread after external `SystemRegistry` + `SystemHandle` drop.
    let part_b_start = std::time::Instant::now();
    let reg_b = Arc::new(SystemRegistry::with_sde(Arc::clone(&ctx.sde)));
    let (h_b, drop_rt_tx_b, join_b) = {
        let reg_spawn_b = Arc::clone(&reg_b);
        let (h_tx, h_rx) = std::sync::mpsc::sync_channel(1);
        let (drop_rt_tx, drop_rt_rx) = std::sync::mpsc::sync_channel::<()>(1);
        let join_b = std::thread::spawn(move || {
            let ct_rt_b = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build CurrentThread runtime B");
            let h_b = {
                let _enter = ct_rt_b.enter();
                reg_spawn_b.get_or_spawn(30_000_145)
            };
            let _ = h_tx.send(h_b);
            let _ = drop_rt_rx.recv();
            drop(ct_rt_b);
        });
        (h_rx.recv().unwrap(), drop_rt_tx, join_b)
    };
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel::<()>();
    let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let (action_tx, action_rx) = tokio::sync::oneshot::channel::<()>();
    let h_b_captured = h_b.clone();
    let driver_weak = Arc::downgrade(
        h_b.ct_driver
            .as_ref()
            .expect("CurrentThread attaches driver"),
    );
    h_b.enqueue_post_unlock_action(Box::new(move || {
        // Confirm execution has already entered `post_unlock_worker`, then wait briefly
        // so external `h_b` and `reg_b` drop first and `h_b_captured` is the last strong ref.
        let _ = entered_tx.send(());
        let _ = ready_rx.recv();
        drop(h_b_captured);
        let _ = action_tx.send(());
    }));
    // Dispatch non-blocking wake command alongside `PostUnlockMsg::FlushActions` so `post_unlock_worker`
    // enters cleanly without waiting on a synchronous actor reply.
    let (tick_tx, _tick_rx) = tokio::sync::oneshot::channel();
    h_b.cmd_sender()
        .try_send(crate::system_actor::SystemCommand::CurrentTick { reply: tick_tx })
        .expect("enqueue non-blocking CurrentTick wake command");
    tokio::time::timeout(Duration::from_secs(2), entered_rx)
        .await
        .expect("post_unlock_actions closure must enter on post_unlock_worker thread")
        .expect("entered_tx must fire");
    drop(h_b);
    drop(reg_b);
    let _ = ready_tx.send(());
    tokio::time::timeout(Duration::from_secs(2), action_rx)
        .await
        .expect("post_unlock_actions closure must finish without deadlock")
        .expect("action channel must signal completion");
    let _ = drop_rt_tx_b.send(());
    join_b.join().expect("join CurrentThread runtime thread B");
    wait_until(
        "CtActorDriver weak count must reach 0 without reference cycles",
        || driver_weak.strong_count() == 0,
    )
    .await;
    assert!(
        part_b_start.elapsed() < Duration::from_secs(2),
        "Part B must complete promptly without hitting 5s sync timeout (elapsed: {:?})",
        part_b_start.elapsed()
    );
}

/// `#1342` Track 4D-2 static check: `post_unlock.rs` has zero `ballpark` / `actor_state` lock acquisitions.
#[test]
fn track_4d2_post_unlock_has_zero_ballpark_or_actor_state_locks() {
    let raw = include_str!("post_unlock.rs");
    let without_ruling_mod = raw
        .split("#[path = \"post_unlock_ruling_tests.rs\"]")
        .next()
        .unwrap_or(raw);
    let mut code_lines = String::with_capacity(without_ruling_mod.len());
    for line in without_ruling_mod.lines() {
        let trimmed = line.split("//").next().unwrap_or("");
        code_lines.push_str(trimmed);
        code_lines.push('\n');
    }
    assert!(
        !code_lines.contains("with_actor_locks"),
        "post_unlock.rs must not contain with_actor_locks"
    );
    for after_sig in code_lines.split("fn extend_pending_cycle_actions(").skip(1) {
        let trimmed_sig = after_sig.trim_start();
        assert!(
            trimmed_sig.starts_with("&self"),
            "post_unlock.rs must only declare (&self) callback methods for extend_pending_cycle_actions, not direct-lock helpers: {trimmed_sig}"
        );
    }
    for target in ["ballpark", "actor_state"] {
        for method in [
            "read(",
            "write(",
            "try_read(",
            "try_write(",
            "lock(",
            "write_untracked(",
        ] {
            let dot_pat = format!("{target}.{method}");
            let spaced_pat = format!("{target} .{method}");
            assert!(
                !code_lines.contains(&dot_pat) && !code_lines.contains(&spaced_pat),
                "post_unlock.rs must not acquire {dot_pat}"
            );
        }
    }
    let targets_struct = code_lines
        .split("struct PostCommitActorTargets")
        .nth(1)
        .and_then(|s| s.split('}').next())
        .expect("PostCommitActorTargets struct present");
    assert!(
        !targets_struct.contains("ballpark") && !targets_struct.contains("actor_state"),
        "PostCommitActorTargets must not hold ballpark or actor_state fields"
    );
    assert!(
        targets_struct.contains("worker_cb_tx"),
        "PostCommitActorTargets must route actor callbacks through worker_cb_tx"
    );
}
