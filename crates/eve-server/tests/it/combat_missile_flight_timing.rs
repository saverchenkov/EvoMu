#![allow(unused_imports)]
//! End-to-End Integration Tests for Guided Missile Lifecycle, Flight Timing,
//! Target-Loss Cleanup, Rapid/Auto Reload Deadlines, and TiDi Sim-Clock Scaling (`#1079`, `#1077` E7-E9).

use crate::common::{gm_pilot_in_space, pin_clock, slash_cmd, stock_ship_bay, LivePilot};

use eve_db::InventoryFlag;
use eve_destiny::{Ball, BallId, CombatState, DestinyEvent, MovementMode};
use eve_proto::{BridgeRpcError, PyRep};
use eve_server::router::ServerContext;
use eve_server::system_actor::{SystemHandle, TickBroadcast};
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, OnceLock};

const SYS_JITA: u32 = 30_000_142;

const TYPE_BANTAM: u32 = 582;
const TYPE_KESTREL: u32 = 602;
const TYPE_CARACAL: u32 = 621;
const TYPE_RAVEN: u32 = 638;

const TYPE_LML_I: u32 = 499;
const TYPE_RLML_I: u32 = 1875;
const TYPE_HML_I: u32 = 501;
const TYPE_TORP_I: u32 = 503;

const TYPE_SCOURGE_LIGHT: u32 = 210;
const TYPE_MJOLNIR_LIGHT: u32 = 212;
const TYPE_SCOURGE_HEAVY: u32 = 209;
const TYPE_INFERNO_TORP: u32 = 2510;

fn fresh_ctx() -> ServerContext {
    static BASE: OnceLock<(Arc<eve_sde::SdeRegistry>, eve_db::WorldStateSnapshot)> =
        OnceLock::new();
    let (sde, base_snap) = BASE.get_or_init(|| {
        let base_ctx = ServerContext::new();
        (base_ctx.sde.clone(), base_ctx.db.with_snapshot(Clone::clone))
    });
    let db = Arc::new(eve_db::WorldRepository::new_empty());
    db.with_mut_snapshot(|s| *s = base_snap.clone());
    let ctx = ServerContext::with_sde_and_repository(sde.clone(), db);
    pin_clock(&ctx);
    ctx
}

async fn grant_type_skills(ctx: &ServerContext, p: &mut LivePilot, type_id: u32) {
    let required: Vec<u32> = ctx
        .sde
        .types
        .get(&type_id)
        .map(|t| {
            [182_u16, 183, 184, 1285, 1289, 1290]
                .iter()
                .filter_map(|a| t.default_attributes.get(a))
                .map(|v| v.round() as u32)
                .filter(|&s| s > 0)
                .collect()
        })
        .unwrap_or_default();
    for sid in required {
        let _ = slash_cmd(ctx, &mut p.sess, &format!("/giveskill {sid} 5")).await;
    }
}

async fn fit_launcher(
    ctx: &ServerContext,
    p: &mut LivePilot,
    launcher_type_id: u32,
    flag: i64,
) -> i64 {
    grant_type_skills(ctx, p, launcher_type_id).await;
    let fitted = slash_cmd(ctx, &mut p.sess, &format!("/fit me {launcher_type_id} {flag}")).await;
    let mod_id = fitted
        .dict_get("itemID")
        .and_then(|v| v.as_i64())
        .expect("/fit me itemID");
    ctx.dispatch_rpc(
        &mut p.sess,
        "dogmaIM",
        None,
        "ShipOnlineModules",
        &[PyRep::Integer(p.ship_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("ShipOnlineModules");
    mod_id
}

async fn load_ammo_into(
    ctx: &ServerContext,
    p: &mut LivePilot,
    mod_ids: &[i64],
    charge_ids: &[i64],
    charge_type_id: u32,
) {
    grant_type_skills(ctx, p, charge_type_id).await;
    ctx.dispatch_rpc(
        &mut p.sess,
        "dogmaIM",
        None,
        "LoadAmmo",
        &[
            PyRep::Integer(p.ship_id),
            PyRep::List(mod_ids.iter().copied().map(PyRep::Integer).collect()),
            PyRep::List(charge_ids.iter().copied().map(PyRep::Integer).collect()),
        ],
        &HashMap::new(),
        None,
    )
    .await
    .expect("LoadAmmo");
}

async fn activate_launcher(
    ctx: &ServerContext,
    p: &mut LivePilot,
    mod_id: i64,
    target_id: i64,
    repeat: i64,
) -> Result<PyRep, BridgeRpcError> {
    ctx.dispatch_rpc(
        &mut p.sess,
        "dogmaIM",
        None,
        "Activate",
        &[
            PyRep::Integer(mod_id),
            PyRep::String("launcherMissilePotential".into()),
            PyRep::Integer(target_id),
            PyRep::Integer(repeat),
        ],
        &HashMap::new(),
        None,
    )
    .await
}

async fn clear_undock_invuln(ctx: &ServerContext, sys: &SystemHandle, p: &mut LivePilot) {
    let _ = ctx
        .dispatch_rpc(
            &mut p.sess,
            "beyonce",
            Some(p.beyonce),
            "CmdGotoDirection",
            &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
            &HashMap::new(),
            None,
        )
        .await;
    let _ = ctx
        .dispatch_rpc(
            &mut p.sess,
            "beyonce",
            Some(p.beyonce),
            "CmdStop",
            &[],
            &HashMap::new(),
            None,
        )
        .await;
    let _ = sys.set_ball_max_velocity(p.ship_id, 0.0).await;
    ctx.advance_secs(1);
    let _ = sys.step_manual(1.0).await.unwrap();
}

async fn lock_target(
    ctx: &ServerContext,
    sys: &SystemHandle,
    attacker: &mut LivePilot,
    target_id: i64,
) {
    ctx.dispatch_rpc(
        &mut attacker.sess,
        "dogmaIM",
        None,
        "AddTarget",
        &[PyRep::Integer(target_id)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("AddTarget");
    for _ in 0..45 {
        if sys
            .get_target_locks(attacker.ship_id)
            .await
            .unwrap()
            .iter()
            .any(|l| l.target_id == target_id && l.locked)
        {
            return;
        }
        ctx.advance_secs(1);
        let _ = sys.step_manual(1.0).await.unwrap();
    }
    panic!("target lock did not complete for {target_id}");
}

async fn advance_and_step(
    ctx: &ServerContext,
    sys: &SystemHandle,
    secs: usize,
) -> Vec<TickBroadcast> {
    let mut ticks = Vec::with_capacity(secs);
    for _ in 0..secs {
        ctx.advance_secs(1);
        ticks.push(sys.step_manual(1.0).await.unwrap());
    }
    ticks
}

fn drain_pilot_notifs(ctx: &ServerContext, p: &mut LivePilot) -> Vec<(String, Vec<PyRep>)> {
    let _ = p.sess.drain_notifications();
    ctx.drain_notifications_for_char(p.char_id)
        .into_iter()
        .map(|n| (n.event, n.args))
        .collect()
}

fn observer_events_for<'a>(tb: &'a TickBroadcast, observer_id: i64) -> &'a [DestinyEvent] {
    tb.observer_events
        .get(&observer_id)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn collect_missile_add_batches(events: &[DestinyEvent], target_id: i64) -> Vec<Vec<Ball>> {
    let mut out = Vec::new();
    for ev in events {
        if let DestinyEvent::AddBalls2 { balls } = ev {
            let mballs: Vec<Ball> = balls
                .iter()
                .filter(|b| {
                    matches!(b.mode, MovementMode::Missile { target_id: tid } if tid == target_id)
                })
                .cloned()
                .collect();
            if !mballs.is_empty() {
                out.push(mballs);
            }
        }
    }
    out
}

fn has_remove_ball(events: &[DestinyEvent], ball_id: BallId) -> bool {
    events.iter().any(|ev| match ev {
        DestinyEvent::RemoveBalls { ball_ids } => ball_ids.contains(&ball_id),
        DestinyEvent::RemoveBall { ball_id: id } => *id == ball_id,
        _ => false,
    })
}

fn has_missile_deploy_fx(events: &[DestinyEvent], ship_id: BallId, mod_id: i64) -> bool {
    events.iter().any(|ev| {
        matches!(
            ev,
            DestinyEvent::OnSpecialFX {
                ball_id,
                module_id,
                guid,
                is_active: true,
            } if *ball_id == ship_id
                && *module_id == Some(mod_id)
                && guid == "effects.MissileDeployment"
        )
    })
}

fn has_damage_state_change(events: &[DestinyEvent], target_id: BallId) -> bool {
    events.iter().any(|ev| {
        matches!(
            ev,
            DestinyEvent::OnDamageStateChange { ball_id, .. } if *ball_id == target_id
        )
    })
}

fn loaded_charge_in_flag(
    ctx: &ServerContext,
    ship_id: i64,
    flag: InventoryFlag,
    mod_id: i64,
) -> (Option<u32>, i32) {
    ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .find(|it| {
                it.location_id == ship_id
                    && it.flag == flag
                    && !it.singleton
                    && it.item_id != mod_id
                    && it.quantity > 0
            })
            .map(|it| (Some(it.type_id), it.quantity))
            .unwrap_or((None, 0))
    })
}

fn cargo_qty_of_type(ctx: &ServerContext, ship_id: i64, type_id: u32) -> i32 {
    ctx.db.with_snapshot(|s| {
        s.items
            .values()
            .filter(|it| {
                it.location_id == ship_id
                    && it.flag == InventoryFlag::Cargo
                    && it.type_id == type_id
                    && it.quantity > 0
            })
            .map(|it| it.quantity)
            .sum()
    })
}

async fn shield_of(sys: &SystemHandle, ball_id: BallId) -> f64 {
    sys.get_combat_state(ball_id)
        .await
        .unwrap()
        .expect("combat state")
        .shield_hp
}

fn godma_ship_effect_starts(notifs: &[(String, Vec<PyRep>)], mod_id: i64) -> usize {
    notifs
        .iter()
        .filter(|n| {
            n.0 == "OnGodmaShipEffect"
                && n.1.first().and_then(PyRep::as_i64) == Some(mod_id)
                && n.1.get(3).and_then(PyRep::as_i64) == Some(1)
        })
        .count()
}

fn count_remote_msg(notifs: &[(String, Vec<PyRep>)], key: &str) -> usize {
    notifs
        .iter()
        .filter(|n| {
            n.0 == "OnRemoteMessage"
                && n.1.first().and_then(PyRep::as_str) == Some(key)
        })
        .count()
}

fn charge_reload_notifs(notifs: &[(String, Vec<PyRep>)], mod_id: i64) -> Vec<f64> {
    notifs
        .iter()
        .filter(|n| {
            n.0 == "OnChargeBeingLoadedToModule"
                && n.1.first().and_then(|v| match v {
                    PyRep::List(items) => Some(items.iter().any(|x| x.as_i64() == Some(mod_id))),
                    _ => None,
                }) == Some(true)
        })
        .filter_map(|n| n.1.get(2).and_then(PyRep::as_f64))
        .collect()
}

#[tokio::test]
async fn missile_launch_follow_ball_then_delayed_impact() {
    // parity: rpc:dogmaIM.Activate, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.AddTarget, rpc:dogmaIM.ShipOnlineModules, destiny:AddBalls2, notify:OnSpecialFX, notify:OnDamageStateChange, notify:OnDamageMessage, destiny:RemoveBalls, notify:OnGodmaShipEffect
    let ctx = fresh_ctx();
    let mut attacker = gm_pilot_in_space(&ctx, 8101, 910_000_001, TYPE_KESTREL, SYS_JITA).await;
    let mut victim = gm_pilot_in_space(&ctx, 8102, 910_000_002, TYPE_BANTAM, SYS_JITA).await;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);

    let lml = fit_launcher(&ctx, &mut attacker, TYPE_LML_I, 27).await;
    let ammo = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_LIGHT, 20, 5).await;
    load_ammo_into(&ctx, &mut attacker, &[lml], &[ammo], TYPE_SCOURGE_LIGHT).await;
    let _ = advance_and_step(&ctx, &sys, 12).await;

    clear_undock_invuln(&ctx, &sys, &mut attacker).await;
    clear_undock_invuln(&ctx, &sys, &mut victim).await;
    assert!(sys.set_ball_position(attacker.ship_id, DVec3::ZERO).await);

    // Negative check: target beyond maxVelocity * flightTime + radii (within 50 km lock range) fails with TargetTooFar and spawns zero balls.
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(48_000.0, 0.0, 0.0)).await);
    lock_target(&ctx, &sys, &mut attacker, victim.ship_id).await;
    let err = activate_launcher(&ctx, &mut attacker, lml, victim.ship_id, 1)
        .await
        .unwrap_err();
    assert_eq!(err.code, "TargetTooFar");
    let _ = drain_pilot_notifs(&ctx, &mut attacker);
    let oob_ticks = advance_and_step(&ctx, &sys, 2).await;
    assert!(oob_ticks.iter().all(|t| collect_missile_add_batches(&t.events, victim.ship_id).is_empty()));

    // Move target to 16,875m (flight ~3.0s at 5,625 m/s) and verify delayed lifecycle ordering.
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(16_875.0, 0.0, 0.0)).await);
    let init_shield = shield_of(&sys, victim.ship_id).await;
    let _ = drain_pilot_notifs(&ctx, &mut attacker);
    let _ = drain_pilot_notifs(&ctx, &mut victim);

    activate_launcher(&ctx, &mut attacker, lml, victim.ship_id, 1)
        .await
        .expect("Activate missile launcher");
    assert_eq!(shield_of(&sys, victim.ship_id).await, init_shield);
    assert_eq!(godma_ship_effect_starts(&drain_pilot_notifs(&ctx, &mut attacker), lml), 1);

    // Tick T1: 1 missile ball arrives in AddBalls2 with charge radius, charge speed, and slim metadata.
    let t1 = advance_and_step(&ctx, &sys, 1).await.pop().unwrap();
    let owner_t1 = collect_missile_add_batches(observer_events_for(&t1, attacker.ship_id), victim.ship_id);
    let victim_t1 = collect_missile_add_batches(observer_events_for(&t1, victim.ship_id), victim.ship_id);
    assert_eq!(owner_t1.len(), 1);
    assert_eq!(owner_t1[0].len(), 1);
    assert_eq!(victim_t1.len(), 1);
    let mball = &owner_t1[0][0];
    assert_eq!(mball.radius, 300.0);
    assert!(mball.max_velocity >= 3750.0);
    assert_eq!(shield_of(&sys, victim.ship_id).await, init_shield);
    assert!(!has_remove_ball(&t1.events, mball.id));
    let slim = sys.get_ball_slim_item(mball.id).await.unwrap().expect("missile slim");
    assert_eq!(slim.dict_get("sourceShipID").and_then(PyRep::as_i64), Some(attacker.ship_id));
    let launch_mods = slim.dict_get("launchModules").and_then(|v| match v {
        PyRep::List(xs) => Some(xs.iter().filter_map(PyRep::as_i64).collect::<Vec<_>>()),
        _ => None,
    });
    assert_eq!(launch_mods, Some(vec![lml]));

    // Tick T2: deployment OnSpecialFX emitted to owner and observer.
    let t2 = advance_and_step(&ctx, &sys, 1).await.pop().unwrap();
    assert!(has_missile_deploy_fx(observer_events_for(&t2, attacker.ship_id), attacker.ship_id, lml));
    assert!(has_missile_deploy_fx(observer_events_for(&t2, victim.ship_id), attacker.ship_id, lml));

    // Step until impact (~1-3s more): damage applied, OnDamageStateChange, OnDamageMessage, RemoveBalls emitted.
    let rest = advance_and_step(&ctx, &sys, 4).await;
    let final_shield = shield_of(&sys, victim.ship_id).await;
    let dmg_taken = init_shield - final_shield;
    assert!(dmg_taken > 10.0 && dmg_taken < 83.0, "expected DRF/resist-scaled missile damage, got {dmg_taken}");
    assert!(rest.iter().any(|t| has_damage_state_change(&t.events, victim.ship_id)));
    assert!(rest.iter().any(|t| has_remove_ball(observer_events_for(t, attacker.ship_id), mball.id)));
    assert!(rest.iter().any(|t| has_remove_ball(observer_events_for(t, victim.ship_id), mball.id)));
    assert!(sys.get_ball_snapshot(mball.id).await.unwrap().is_none());
    let atk_notifs = drain_pilot_notifs(&ctx, &mut attacker);
    let vic_notifs = drain_pilot_notifs(&ctx, &mut victim);
    assert!(atk_notifs.iter().any(|n| n.0 == "OnDamageMessage"));
    assert!(vic_notifs.iter().any(|n| n.0 == "OnDamageMessage"));
}

#[tokio::test]
async fn missile_cleanup_on_target_loss() {
    // parity: rpc:dogmaIM.Activate, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.AddTarget, destiny:AddBalls2, destiny:RemoveBalls, notify:OnDamageMessage
    let ctx = fresh_ctx();
    let mut attacker = gm_pilot_in_space(&ctx, 8103, 910_000_003, TYPE_KESTREL, SYS_JITA).await;
    let mut victim = gm_pilot_in_space(&ctx, 8104, 910_000_004, TYPE_BANTAM, SYS_JITA).await;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);

    let lml = fit_launcher(&ctx, &mut attacker, TYPE_LML_I, 27).await;
    let ammo = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_LIGHT, 20, 5).await;
    load_ammo_into(&ctx, &mut attacker, &[lml], &[ammo], TYPE_SCOURGE_LIGHT).await;
    let _ = advance_and_step(&ctx, &sys, 12).await;

    clear_undock_invuln(&ctx, &sys, &mut attacker).await;
    clear_undock_invuln(&ctx, &sys, &mut victim).await;
    assert!(sys.set_ball_position(attacker.ship_id, DVec3::ZERO).await);
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(16_875.0, 0.0, 0.0)).await);
    lock_target(&ctx, &sys, &mut attacker, victim.ship_id).await;

    activate_launcher(&ctx, &mut attacker, lml, victim.ship_id, 1)
        .await
        .expect("Activate");
    let t1 = advance_and_step(&ctx, &sys, 1).await.pop().unwrap();
    let mball_id = collect_missile_add_batches(&t1.events, victim.ship_id)[0][0].id;
    assert!(sys.get_ball_snapshot(mball_id).await.unwrap().is_some());
    let _ = drain_pilot_notifs(&ctx, &mut attacker);
    let _ = drain_pilot_notifs(&ctx, &mut victim);

    // Target leaves grid before impact -> missile removed within 1 tick with zero damage.
    assert!(sys.remove_ball(victim.ship_id).await.is_some());
    let t_loss = advance_and_step(&ctx, &sys, 1).await.pop().unwrap();
    assert!(has_remove_ball(observer_events_for(&t_loss, attacker.ship_id), mball_id));
    assert!(sys.get_ball_snapshot(mball_id).await.unwrap().is_none());
    let _ = advance_and_step(&ctx, &sys, 4).await;
    assert!(!drain_pilot_notifs(&ctx, &mut attacker).iter().any(|n| n.0 == "OnDamageMessage"));
    assert!(!drain_pilot_notifs(&ctx, &mut victim).iter().any(|n| n.0 == "OnDamageMessage"));
}

#[tokio::test]
async fn launcher_auto_reload_resumes_at_deadline_not_before() {
    // parity: rpc:dogmaIM.Activate, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.AddTarget, destiny:AddBalls2, notify:OnChargeBeingLoadedToModule, notify:OnRemoteMessage
    let ctx = fresh_ctx();
    let mut attacker = gm_pilot_in_space(&ctx, 8105, 910_000_005, TYPE_KESTREL, SYS_JITA).await;
    let mut victim = gm_pilot_in_space(&ctx, 8106, 910_000_006, TYPE_RAVEN, SYS_JITA).await;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);

    let lml = fit_launcher(&ctx, &mut attacker, TYPE_LML_I, 27).await;
    let one_round = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_LIGHT, 1, 5).await;
    load_ammo_into(&ctx, &mut attacker, &[lml], &[one_round], TYPE_SCOURGE_LIGHT).await;
    let _ = advance_and_step(&ctx, &sys, 12).await;

    clear_undock_invuln(&ctx, &sys, &mut attacker).await;
    clear_undock_invuln(&ctx, &sys, &mut victim).await;
    assert!(sys.set_ball_position(attacker.ship_id, DVec3::ZERO).await);
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(11_250.0, 0.0, 0.0)).await);
    lock_target(&ctx, &sys, &mut attacker, victim.ship_id).await;

    // Positive path: 1 round loaded, 40 rounds spare in cargo -> auto-reloads for 10s, fires at deadline leaving 39.
    let _ = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_LIGHT, 40, 5).await;
    let _ = drain_pilot_notifs(&ctx, &mut attacker);
    activate_launcher(&ctx, &mut attacker, lml, victim.ship_id, 10)
        .await
        .expect("Activate with auto-reload");

    let t1 = advance_and_step(&ctx, &sys, 1).await;
    assert_eq!(collect_missile_add_batches(&t1[0].events, victim.ship_id).len(), 1);
    // Cycle 1 completes at 14.4s (LML I 16s base * 0.90 MLO V) and starts the 10s auto-reload at cycle end.
    let _cycle_end = advance_and_step(&ctx, &sys, 14).await;
    let reload_durs = charge_reload_notifs(&drain_pilot_notifs(&ctx, &mut attacker), lml);
    assert_eq!(reload_durs, vec![10_000.0]);

    // First 8s after cycle end (8s < 10s reloadTime): zero new launches.
    let pre_deadline = advance_and_step(&ctx, &sys, 8).await;
    let pre_spawns: usize = pre_deadline
        .iter()
        .map(|t| collect_missile_add_batches(&t.events, victim.ship_id).len())
        .sum();
    assert_eq!(pre_spawns, 0, "must not launch before 10s reload deadline");

    // Cross the 10s deadline + next-tick flush (3s): reloads 40 rounds and immediately fires 1 -> 39 remaining.
    let at_deadline = advance_and_step(&ctx, &sys, 3).await;
    let post_spawns: usize = at_deadline
        .iter()
        .map(|t| collect_missile_add_batches(&t.events, victim.ship_id).len())
        .sum();
    assert_eq!(post_spawns, 1, "must launch at reload deadline");
    let (_, rem_after_reload) = loaded_charge_in_flag(&ctx, attacker.ship_id, InventoryFlag::HiSlot0, lml);
    assert_eq!(rem_after_reload, 39);

    // Negative path: 1 round loaded and 0 in cargo -> deactivates with NoCharges at cycle end (14.4s) and schedules no reload.
    let _ = sys.remove_ball(attacker.ship_id).await;
    let mut attacker2 = gm_pilot_in_space(&ctx, 8107, 910_000_007, TYPE_KESTREL, SYS_JITA).await;
    let lml2 = fit_launcher(&ctx, &mut attacker2, TYPE_LML_I, 27).await;
    let last_one = stock_ship_bay(&ctx, &mut attacker2, TYPE_SCOURGE_LIGHT, 1, 5).await;
    load_ammo_into(&ctx, &mut attacker2, &[lml2], &[last_one], TYPE_SCOURGE_LIGHT).await;
    let _ = advance_and_step(&ctx, &sys, 12).await;
    clear_undock_invuln(&ctx, &sys, &mut attacker2).await;
    assert!(sys.set_ball_position(attacker2.ship_id, DVec3::new(0.0, 200.0, 0.0)).await);
    lock_target(&ctx, &sys, &mut attacker2, victim.ship_id).await;
    let _ = drain_pilot_notifs(&ctx, &mut attacker2);
    activate_launcher(&ctx, &mut attacker2, lml2, victim.ship_id, 5)
        .await
        .expect("Activate final round");
    let empty_ticks = advance_and_step(&ctx, &sys, 16).await;
    let atk2_notifs = drain_pilot_notifs(&ctx, &mut attacker2);
    assert_eq!(count_remote_msg(&atk2_notifs, "NoCharges"), 1);
    assert!(charge_reload_notifs(&atk2_notifs, lml2).is_empty());
    let total_empty_spawns: usize = empty_ticks
        .iter()
        .map(|t| collect_missile_add_batches(observer_events_for(t, attacker2.ship_id), victim.ship_id).len())
        .sum();
    assert_eq!(total_empty_spawns, 1);
}

#[tokio::test]
async fn missile_acquires_batch_on_next_tick() {
    // parity: rpc:dogmaIM.Activate, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.AddTarget, destiny:AddBalls2
    let ctx = fresh_ctx();
    let mut attacker = gm_pilot_in_space(&ctx, 8108, 910_000_008, TYPE_KESTREL, SYS_JITA).await;
    let mut victim = gm_pilot_in_space(&ctx, 8109, 910_000_009, TYPE_BANTAM, SYS_JITA).await;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);

    let m1 = fit_launcher(&ctx, &mut attacker, TYPE_LML_I, 27).await;
    let m2 = fit_launcher(&ctx, &mut attacker, TYPE_LML_I, 28).await;
    let a1 = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_LIGHT, 10, 5).await;
    load_ammo_into(&ctx, &mut attacker, &[m1], &[a1], TYPE_SCOURGE_LIGHT).await;
    let a2 = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_LIGHT, 10, 5).await;
    load_ammo_into(&ctx, &mut attacker, &[m2], &[a2], TYPE_SCOURGE_LIGHT).await;
    let _ = advance_and_step(&ctx, &sys, 12).await;

    clear_undock_invuln(&ctx, &sys, &mut attacker).await;
    clear_undock_invuln(&ctx, &sys, &mut victim).await;
    assert!(sys.set_ball_position(attacker.ship_id, DVec3::ZERO).await);
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(14_000.0, 0.0, 0.0)).await);
    lock_target(&ctx, &sys, &mut attacker, victim.ship_id).await;

    activate_launcher(&ctx, &mut attacker, m1, victim.ship_id, 1).await.unwrap();
    activate_launcher(&ctx, &mut attacker, m2, victim.ship_id, 1).await.unwrap();

    let t1 = advance_and_step(&ctx, &sys, 1).await.pop().unwrap();
    let batches = collect_missile_add_batches(&t1.events, victim.ship_id);
    assert_eq!(batches.len(), 1, "must batch multi-launcher acquires into one AddBalls2");
    assert_eq!(batches[0].len(), 2);
}

#[tokio::test]
async fn missile_ball_uses_charge_radius_and_velocity_clamp() {
    // parity: rpc:dogmaIM.Activate, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.AddTarget, destiny:AddBalls2
    let ctx = fresh_ctx();
    let mut attacker = gm_pilot_in_space(&ctx, 8110, 910_000_010, TYPE_CARACAL, SYS_JITA).await;
    let mut victim = gm_pilot_in_space(&ctx, 8111, 910_000_011, TYPE_RAVEN, SYS_JITA).await;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);

    let hml = fit_launcher(&ctx, &mut attacker, TYPE_HML_I, 27).await;
    let heavy_ammo = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_HEAVY, 10, 5).await;
    load_ammo_into(&ctx, &mut attacker, &[hml], &[heavy_ammo], TYPE_SCOURGE_HEAVY).await;
    let _ = advance_and_step(&ctx, &sys, 12).await;

    clear_undock_invuln(&ctx, &sys, &mut attacker).await;
    clear_undock_invuln(&ctx, &sys, &mut victim).await;
    assert!(sys.set_ball_position(attacker.ship_id, DVec3::ZERO).await);
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(15_000.0, 0.0, 0.0)).await);
    lock_target(&ctx, &sys, &mut attacker, victim.ship_id).await;

    activate_launcher(&ctx, &mut attacker, hml, victim.ship_id, 1).await.unwrap();
    let t1 = advance_and_step(&ctx, &sys, 1).await.pop().unwrap();
    let batch = collect_missile_add_batches(&t1.events, victim.ship_id);
    assert_eq!(batch.len(), 1);
    let b = &batch[0][0];
    assert_eq!(b.radius, 300.0);
    // Caracal (attr 657 = 10%/lvl * Caldari Cruiser V = +50%): 4300 * 1.5 = 6450 m/s.
    assert!((b.max_velocity - 6_450.0).abs() < 1e-3, "Caracal heavy missile speed expected 6450 m/s, got {}", b.max_velocity);
    assert!(matches!(b.mode, MovementMode::Missile { target_id } if target_id == victim.ship_id));
}

#[tokio::test]
async fn volley_positions_distinct_and_range_compensated() {
    // parity: rpc:dogmaIM.Activate, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.AddTarget, destiny:AddBalls2, notify:OnSpecialFX, notify:OnGodmaShipEffect
    let ctx = fresh_ctx();
    let mut attacker = gm_pilot_in_space(&ctx, 8112, 910_000_012, TYPE_RAVEN, SYS_JITA).await;
    let mut victim = gm_pilot_in_space(&ctx, 8113, 910_000_013, TYPE_RAVEN, SYS_JITA).await;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);

    let t1_id = fit_launcher(&ctx, &mut attacker, TYPE_TORP_I, 27).await;
    let t2_id = fit_launcher(&ctx, &mut attacker, TYPE_TORP_I, 28).await;
    let t3_id = fit_launcher(&ctx, &mut attacker, TYPE_TORP_I, 29).await;
    for (mid, flag_i) in [(t1_id, 27), (t2_id, 28), (t3_id, 29)] {
        let _ = flag_i;
        let a = stock_ship_bay(&ctx, &mut attacker, TYPE_INFERNO_TORP, 10, 5).await;
        load_ammo_into(&ctx, &mut attacker, &[mid], &[a], TYPE_INFERNO_TORP).await;
    }
    let _ = advance_and_step(&ctx, &sys, 12).await;

    clear_undock_invuln(&ctx, &sys, &mut attacker).await;
    clear_undock_invuln(&ctx, &sys, &mut victim).await;
    assert!(sys.set_ball_position(attacker.ship_id, DVec3::ZERO).await);
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(10_000.0, 0.0, 0.0)).await);
    lock_target(&ctx, &sys, &mut attacker, victim.ship_id).await;
    let _ = drain_pilot_notifs(&ctx, &mut attacker);

    activate_launcher(&ctx, &mut attacker, t1_id, victim.ship_id, 2).await.unwrap();
    activate_launcher(&ctx, &mut attacker, t2_id, victim.ship_id, 0).await.unwrap();
    activate_launcher(&ctx, &mut attacker, t3_id, victim.ship_id, 0).await.unwrap();

    let tick1 = advance_and_step(&ctx, &sys, 1).await.pop().unwrap();
    let batches = collect_missile_add_batches(&tick1.events, victim.ship_id);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 3);
    let p0 = batches[0][0].position;
    let p1 = batches[0][1].position;
    let p2 = batches[0][2].position;
    assert!(p0.distance(p1) > 0.1 && p0.distance(p2) > 0.1 && p1.distance(p2) > 0.1);
    let torp_vel = batches[0][0].max_velocity;

    // Step across cycle 2 to verify deployment OnSpecialFX repeats without duplicate OnGodmaShipEffect start.
    let cycle_ticks = advance_and_step(&ctx, &sys, 14).await;
    let fx_count = cycle_ticks
        .iter()
        .filter(|t| has_missile_deploy_fx(observer_events_for(t, attacker.ship_id), attacker.ship_id, t1_id))
        .count();
    assert!(fx_count >= 2, "expected deployment FX on repeat cycle, got {fx_count}");
    let drained = drain_pilot_notifs(&ctx, &mut attacker);
    assert_eq!(godma_ship_effect_starts(&drained, t1_id), 1);

    // Large-hull launch-radius compensation: Raven (250m) + Raven (250m) = 500m margin past raw center range.
    let raw_range = torp_vel * 7.2;
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(raw_range + 250.0, 0.0, 0.0)).await);
    activate_launcher(&ctx, &mut attacker, t2_id, victim.ship_id, 1)
        .await
        .expect("surface-compensated torpedo range must succeed");
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(raw_range + 650.0, 0.0, 0.0)).await);
    let err = activate_launcher(&ctx, &mut attacker, t3_id, victim.ship_id, 1)
        .await
        .unwrap_err();
    assert_eq!(err.code, "TargetTooFar");
}

#[tokio::test]
async fn missile_eta_tracks_moving_target() {
    // parity: rpc:dogmaIM.Activate, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.AddTarget, destiny:AddBalls2, notify:OnDamageStateChange, destiny:RemoveBalls
    let ctx = fresh_ctx();
    let mut atk1 = gm_pilot_in_space(&ctx, 8114, 910_000_014, TYPE_KESTREL, SYS_JITA).await;
    let mut vic1 = gm_pilot_in_space(&ctx, 8115, 910_000_015, TYPE_BANTAM, SYS_JITA).await;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);

    let lml1 = fit_launcher(&ctx, &mut atk1, TYPE_LML_I, 27).await;
    let a1 = stock_ship_bay(&ctx, &mut atk1, TYPE_SCOURGE_LIGHT, 10, 5).await;
    load_ammo_into(&ctx, &mut atk1, &[lml1], &[a1], TYPE_SCOURGE_LIGHT).await;
    let _ = advance_and_step(&ctx, &sys, 12).await;

    clear_undock_invuln(&ctx, &sys, &mut atk1).await;
    clear_undock_invuln(&ctx, &sys, &mut vic1).await;
    assert!(sys.set_ball_position(atk1.ship_id, DVec3::ZERO).await);
    assert!(sys.set_ball_position(vic1.ship_id, DVec3::new(11_250.0, 0.0, 0.0)).await);
    lock_target(&ctx, &sys, &mut atk1, vic1.ship_id).await;

    activate_launcher(&ctx, &mut atk1, lml1, vic1.ship_id, 0).await.unwrap();
    let mut stationary_ticks = 0usize;
    for step_idx in 1..=8 {
        let t = advance_and_step(&ctx, &sys, 1).await.pop().unwrap();
        if has_damage_state_change(&t.events, vic1.ship_id) {
            stationary_ticks = step_idx;
            break;
        }
    }
    assert!(stationary_ticks > 0);

    // Moving-away target at 1,800 m/s from same 11,250m takes strictly more ticks to impact.
    assert!(sys.set_ball_position(vic1.ship_id, DVec3::new(11_250.0, 0.0, 0.0)).await);
    assert!(sys.set_ball_max_velocity(vic1.ship_id, 1_800.0).await.is_ok_and(|b| b));
    let _ = ctx
        .dispatch_rpc(
            &mut vic1.sess,
            "beyonce",
            Some(vic1.beyonce),
            "CmdGotoDirection",
            &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
            &HashMap::new(),
            None,
        )
        .await;
    let wait_after_stationary = 16usize.saturating_sub(stationary_ticks).max(1);
    let _ = advance_and_step(&ctx, &sys, wait_after_stationary).await;
    assert!(sys.set_ball_position(vic1.ship_id, DVec3::new(11_250.0, 0.0, 0.0)).await);
    activate_launcher(&ctx, &mut atk1, lml1, vic1.ship_id, 0).await.unwrap();
    let mut moving_ticks = 0usize;
    for step_idx in 1..=10 {
        let t = advance_and_step(&ctx, &sys, 1).await.pop().unwrap();
        if has_damage_state_change(&t.events, vic1.ship_id) {
            moving_ticks = step_idx;
            break;
        }
    }
    assert!(moving_ticks > stationary_ticks, "moving {moving_ticks} <= stationary {stationary_ticks}");
}

#[tokio::test]
async fn expired_missile_no_damage() {
    // parity: rpc:dogmaIM.Activate, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.AddTarget, destiny:AddBalls2, destiny:RemoveBalls, notify:OnDamageMessage
    let ctx = fresh_ctx();
    let mut attacker = gm_pilot_in_space(&ctx, 8116, 910_000_016, TYPE_KESTREL, SYS_JITA).await;
    let mut victim = gm_pilot_in_space(&ctx, 8117, 910_000_017, TYPE_BANTAM, SYS_JITA).await;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);

    let lml = fit_launcher(&ctx, &mut attacker, TYPE_LML_I, 27).await;
    let ammo = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_LIGHT, 10, 5).await;
    load_ammo_into(&ctx, &mut attacker, &[lml], &[ammo], TYPE_SCOURGE_LIGHT).await;
    let _ = advance_and_step(&ctx, &sys, 12).await;

    clear_undock_invuln(&ctx, &sys, &mut attacker).await;
    clear_undock_invuln(&ctx, &sys, &mut victim).await;
    assert!(sys.set_ball_position(attacker.ship_id, DVec3::ZERO).await);
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(26_000.0, 0.0, 0.0)).await);
    lock_target(&ctx, &sys, &mut attacker, victim.ship_id).await;

    // Target burns away at 4,000 m/s once launched so missile flight budget expires in space.
    assert!(sys.set_ball_max_velocity(victim.ship_id, 4_000.0).await.is_ok_and(|b| b));
    let _ = ctx
        .dispatch_rpc(
            &mut victim.sess,
            "beyonce",
            Some(victim.beyonce),
            "CmdGotoDirection",
            &[PyRep::Real(1.0), PyRep::Real(0.0), PyRep::Real(0.0)],
            &HashMap::new(),
            None,
        )
        .await;
    let _ = advance_and_step(&ctx, &sys, 5).await;
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(26_000.0, 0.0, 0.0)).await);
    let init_shield = shield_of(&sys, victim.ship_id).await;
    let _ = drain_pilot_notifs(&ctx, &mut attacker);
    let _ = drain_pilot_notifs(&ctx, &mut victim);

    activate_launcher(&ctx, &mut attacker, lml, victim.ship_id, 0).await.unwrap();
    let t1 = advance_and_step(&ctx, &sys, 1).await.pop().unwrap();
    let mball_id = collect_missile_add_batches(&t1.events, victim.ship_id)[0][0].id;
    let exp_ticks = advance_and_step(&ctx, &sys, 7).await;
    assert!(exp_ticks.iter().any(|t| has_remove_ball(&t.events, mball_id)));
    assert_eq!(shield_of(&sys, victim.ship_id).await, init_shield);
    assert!(!drain_pilot_notifs(&ctx, &mut attacker).iter().any(|n| n.0 == "OnDamageMessage"));

    // Point-blank fast impact still holds minimum visual floor and applies single impact without timeout.
    let _ = sys.set_ball_max_velocity(victim.ship_id, 0.0).await;
    let _ = ctx
        .dispatch_rpc(
            &mut victim.sess,
            "beyonce",
            Some(victim.beyonce),
            "CmdStop",
            &[PyRep::Integer(victim.ship_id)],
            &HashMap::new(),
            None,
        )
        .await;
    let _ = advance_and_step(&ctx, &sys, 15).await;
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(500.0, 0.0, 0.0)).await);
    lock_target(&ctx, &sys, &mut attacker, victim.ship_id).await;
    let _ = advance_and_step(&ctx, &sys, 10).await;
    activate_launcher(&ctx, &mut attacker, lml, victim.ship_id, 0).await.unwrap();
    let pb_t1 = advance_and_step(&ctx, &sys, 1).await.pop().unwrap();
    let pb_id = collect_missile_add_batches(&pb_t1.events, victim.ship_id)[0][0].id;
    assert!(!has_remove_ball(&pb_t1.events, pb_id));
    let _ = advance_and_step(&ctx, &sys, 3).await;
    assert!(shield_of(&sys, victim.ship_id).await < init_shield);
}

#[tokio::test]
async fn rapid_light_reload_35s_blocks_fire() {
    // parity: rpc:dogmaIM.Activate, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.AddTarget, destiny:AddBalls2, notify:OnChargeBeingLoadedToModule
    let ctx = fresh_ctx();
    let mut attacker = gm_pilot_in_space(&ctx, 8118, 910_000_018, TYPE_CARACAL, SYS_JITA).await;
    let mut victim = gm_pilot_in_space(&ctx, 8119, 910_000_019, TYPE_RAVEN, SYS_JITA).await;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);

    let rlml = fit_launcher(&ctx, &mut attacker, TYPE_RLML_I, 27).await;
    let one = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_LIGHT, 1, 5).await;
    load_ammo_into(&ctx, &mut attacker, &[rlml], &[one], TYPE_SCOURGE_LIGHT).await;
    let _ = advance_and_step(&ctx, &sys, 36).await;

    clear_undock_invuln(&ctx, &sys, &mut attacker).await;
    clear_undock_invuln(&ctx, &sys, &mut victim).await;
    assert!(sys.set_ball_position(attacker.ship_id, DVec3::ZERO).await);
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(10_000.0, 0.0, 0.0)).await);
    lock_target(&ctx, &sys, &mut attacker, victim.ship_id).await;

    let _ = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_LIGHT, 20, 5).await;
    let _ = drain_pilot_notifs(&ctx, &mut attacker);
    activate_launcher(&ctx, &mut attacker, rlml, victim.ship_id, 5).await.unwrap();

    let t1 = advance_and_step(&ctx, &sys, 1).await;
    assert_eq!(collect_missile_add_batches(&t1[0].events, victim.ship_id).len(), 1);
    // RLML I (7.8s base * 0.90 MLO V = 7.02s) cycle completes after 8s total and starts 35s reload.
    let _cycle_end = advance_and_step(&ctx, &sys, 7).await;
    assert_eq!(charge_reload_notifs(&drain_pilot_notifs(&ctx, &mut attacker), rlml), vec![35_000.0]);

    // 33 more seconds (33s < 35s rapid reload): no second missile fires and manual Activate cannot bypass.
    let pre = advance_and_step(&ctx, &sys, 33).await;
    let pre_count: usize = pre.iter().map(|t| collect_missile_add_batches(&t.events, victim.ship_id).len()).sum();
    assert_eq!(pre_count, 0);
    assert!(activate_launcher(&ctx, &mut attacker, rlml, victim.ship_id, 1).await.is_err());

    // Cross 35s deadline (+3s): reloads 19 capacity rounds and launches leaving 18.
    let post = advance_and_step(&ctx, &sys, 3).await;
    let post_count: usize = post.iter().map(|t| collect_missile_add_batches(&t.events, victim.ship_id).len()).sum();
    assert_eq!(post_count, 1);
    let (_, rem) = loaded_charge_in_flag(&ctx, attacker.ship_id, InventoryFlag::HiSlot0, rlml);
    assert_eq!(rem, 18);
}

#[tokio::test]
async fn grouped_launchers_consume_one_charge_each() {
    // parity: rpc:dogmaIM.Activate, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.LinkWeapons, rpc:dogmaIM.AddTarget, destiny:AddBalls2
    let ctx = fresh_ctx();
    let mut attacker = gm_pilot_in_space(&ctx, 8120, 910_000_020, TYPE_KESTREL, SYS_JITA).await;
    let mut victim = gm_pilot_in_space(&ctx, 8121, 910_000_021, TYPE_BANTAM, SYS_JITA).await;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);

    let m1 = fit_launcher(&ctx, &mut attacker, TYPE_LML_I, 27).await;
    let m2 = fit_launcher(&ctx, &mut attacker, TYPE_LML_I, 28).await;
    let a1 = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_LIGHT, 5, 5).await;
    load_ammo_into(&ctx, &mut attacker, &[m1], &[a1], TYPE_SCOURGE_LIGHT).await;
    let a2 = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_LIGHT, 5, 5).await;
    load_ammo_into(&ctx, &mut attacker, &[m2], &[a2], TYPE_SCOURGE_LIGHT).await;
    let _ = advance_and_step(&ctx, &sys, 12).await;

    ctx.dispatch_rpc(
        &mut attacker.sess,
        "dogmaIM",
        None,
        "LinkWeapons",
        &[PyRep::Integer(attacker.ship_id), PyRep::Integer(m1), PyRep::Integer(m2)],
        &HashMap::new(),
        None,
    )
    .await
    .expect("LinkWeapons");

    clear_undock_invuln(&ctx, &sys, &mut attacker).await;
    clear_undock_invuln(&ctx, &sys, &mut victim).await;
    assert!(sys.set_ball_position(attacker.ship_id, DVec3::ZERO).await);
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(14_000.0, 0.0, 0.0)).await);
    lock_target(&ctx, &sys, &mut attacker, victim.ship_id).await;

    activate_launcher(&ctx, &mut attacker, m1, victim.ship_id, 1).await.unwrap();
    let t1 = advance_and_step(&ctx, &sys, 1).await.pop().unwrap();
    let batches = collect_missile_add_batches(&t1.events, victim.ship_id);
    assert_eq!(batches.len(), 1);
    assert_eq!(batches[0].len(), 2);

    let (_, rem1) = loaded_charge_in_flag(&ctx, attacker.ship_id, InventoryFlag::HiSlot0, m1);
    let (_, rem2) = loaded_charge_in_flag(&ctx, attacker.ship_id, InventoryFlag::HiSlot1, m2);
    assert_eq!(rem1, 4);
    assert_eq!(rem2, 4);
}

#[tokio::test]
async fn manual_reload_swap_returns_old_clip_to_cargo() {
    // parity: rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.Activate, rpc:dogmaIM.AddTarget, destiny:AddBalls2, notify:OnDamageMessage
    let ctx = fresh_ctx();
    let mut attacker = gm_pilot_in_space(&ctx, 8122, 910_000_022, TYPE_KESTREL, SYS_JITA).await;
    let mut victim = gm_pilot_in_space(&ctx, 8123, 910_000_023, TYPE_BANTAM, SYS_JITA).await;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);

    let lml = fit_launcher(&ctx, &mut attacker, TYPE_LML_I, 27).await;
    let scourge = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_LIGHT, 20, 5).await;
    load_ammo_into(&ctx, &mut attacker, &[lml], &[scourge], TYPE_SCOURGE_LIGHT).await;
    let _ = advance_and_step(&ctx, &sys, 12).await;

    let mjolnir = stock_ship_bay(&ctx, &mut attacker, TYPE_MJOLNIR_LIGHT, 20, 5).await;
    assert_eq!(cargo_qty_of_type(&ctx, attacker.ship_id, TYPE_SCOURGE_LIGHT), 0);
    load_ammo_into(&ctx, &mut attacker, &[lml], &[mjolnir], TYPE_MJOLNIR_LIGHT).await;
    assert_eq!(cargo_qty_of_type(&ctx, attacker.ship_id, TYPE_SCOURGE_LIGHT), 20);
    assert_eq!(
        loaded_charge_in_flag(&ctx, attacker.ship_id, InventoryFlag::HiSlot0, lml),
        (Some(TYPE_MJOLNIR_LIGHT), 20)
    );

    let _ = advance_and_step(&ctx, &sys, 12).await;
    clear_undock_invuln(&ctx, &sys, &mut attacker).await;
    clear_undock_invuln(&ctx, &sys, &mut victim).await;
    assert!(sys.set_ball_position(attacker.ship_id, DVec3::ZERO).await);
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(11_250.0, 0.0, 0.0)).await);
    lock_target(&ctx, &sys, &mut attacker, victim.ship_id).await;
    activate_launcher(&ctx, &mut attacker, lml, victim.ship_id, 1).await.unwrap();
    let ticks = advance_and_step(&ctx, &sys, 5).await;
    assert!(ticks.iter().any(|t| has_damage_state_change(&t.events, victim.ship_id)));
}

#[tokio::test]
async fn missile_flight_follows_sim_clock_under_tidi() {
    // parity: rpc:dogmaIM.Activate, rpc:dogmaIM.LoadAmmo, rpc:dogmaIM.AddTarget, destiny:AddBalls2, notify:OnDamageStateChange, destiny:RemoveBalls
    let ctx = fresh_ctx();
    let mut attacker = gm_pilot_in_space(&ctx, 8124, 910_000_024, TYPE_KESTREL, SYS_JITA).await;
    let mut victim = gm_pilot_in_space(&ctx, 8125, 910_000_025, TYPE_RAVEN, SYS_JITA).await;
    let sys = ctx.systems.get_or_spawn(SYS_JITA);

    let lml = fit_launcher(&ctx, &mut attacker, TYPE_LML_I, 27).await;
    let one = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_LIGHT, 1, 5).await;
    load_ammo_into(&ctx, &mut attacker, &[lml], &[one], TYPE_SCOURGE_LIGHT).await;
    let _ = advance_and_step(&ctx, &sys, 12).await;

    clear_undock_invuln(&ctx, &sys, &mut attacker).await;
    clear_undock_invuln(&ctx, &sys, &mut victim).await;
    assert!(sys.set_ball_position(attacker.ship_id, DVec3::ZERO).await);
    assert!(sys.set_ball_position(victim.ship_id, DVec3::new(16_875.0, 0.0, 0.0)).await);
    lock_target(&ctx, &sys, &mut attacker, victim.ship_id).await;
    let _ = stock_ship_bay(&ctx, &mut attacker, TYPE_SCOURGE_LIGHT, 20, 5).await;

    // 50% TiDi: 1.0s wall = 0.5s sim, so flight (~3s sim -> ~6s wall) & reload (10s sim -> 20s wall) double.
    sys.set_tidi_factor_raw(0.5);
    let init_shield = shield_of(&sys, victim.ship_id).await;
    activate_launcher(&ctx, &mut attacker, lml, victim.ship_id, 5).await.unwrap();

    // First 4 wall ticks (~2.0s sim): 1st missile still in flight (< 3.0s sim), target shield untouched.
    let early = advance_and_step(&ctx, &sys, 4).await;
    assert_eq!(collect_missile_add_batches(&early[0].events, victim.ship_id).len(), 1);
    assert_eq!(shield_of(&sys, victim.ship_id).await, init_shield);

    // Next 6 wall ticks (10s wall total = 5s sim): 1st missile impacts; LML I cycle (14.4s sim = 28.8s wall) + 10s sim reload (20s wall) not done.
    let mid = advance_and_step(&ctx, &sys, 6).await;
    assert!(shield_of(&sys, victim.ship_id).await < init_shield);
    assert!(mid.iter().any(|t| has_damage_state_change(&t.events, victim.ship_id)));
    let mid_spawns: usize = mid.iter().map(|t| collect_missile_add_batches(&t.events, victim.ship_id).len()).sum();
    assert_eq!(mid_spawns, 0, "reload must not finish at 10s wall under 0.5 TiDi");

    // Step through remaining cycle + partial reload (46s wall total = 23s sim < 24.4s sim): still zero new spawns.
    let pre_reload = advance_and_step(&ctx, &sys, 36).await;
    let pre_reload_spawns: usize = pre_reload.iter().map(|t| collect_missile_add_batches(&t.events, victim.ship_id).len()).sum();
    assert_eq!(pre_reload_spawns, 0, "reload must not finish before cycle end (28.8s) + 20s reload wall under 0.5 TiDi");

    // Cross 48.8s wall total (+6s = 52s wall = 26s sim): reload completes and 2nd missile spawns.
    let late = advance_and_step(&ctx, &sys, 6).await;
    let late_spawns: usize = late.iter().map(|t| collect_missile_add_batches(&t.events, victim.ship_id).len()).sum();
    assert_eq!(late_spawns, 1, "reload must complete around cycle end + 20s wall under 0.5 TiDi");
}
