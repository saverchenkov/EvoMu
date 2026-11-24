//! Bounded-deadline actor mailbox commands and shared actor-side handlers for external ballpark
//! migrations (`RFC-1 §7.3`, `#1342`).
//!
//! Async callers use [`call_actor_result_with_deadline`] with [`ACTOR_CMD_TIMEOUT`] so stopped,
//! dropped, or backpressured solar system actors return explicit errors rather than silently
//! falling back to external `ballpark` locks. Synchronous callers run the same mailbox commands
//! via `tokio::task::block_in_place` on multi-thread runtimes (or transient `current_thread`
//! runtimes outside Tokio), bridging legacy `#[tokio::test(flavor = "current_thread")]` tests
//! directly through the shared actor handler only while the actor handle remains open.

#![forbid(unsafe_code)]

use super::{
    celestials, defer_reply, flush_post_unlock_actions, protection, ActorExecCtx,
    ExtendedNpcAttackActorInput, ExtendedNpcAttackActorOutput, ModuleDeactivationHook,
    ReconnectLiveShipRequest, SpawnReconnectedShipRequest, SystemActorState, SystemCommand,
    SystemHandle, WarpProvenance,
};
use eve_db::lock_rank::{RankedRwLock, RANK_LEAF};
use eve_destiny::{BallId, Ballpark, MovementMode};
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

/// Default bounded mailbox deadline for actor-backed ballpark commands (`#1342`).
pub const ACTOR_CMD_TIMEOUT: Duration = Duration::from_secs(5);

struct BallOpsCtx<'a> {
    solar_system_id: SolarSystemId,
    ballpark: &'a mut Ballpark,
    actor_st: &'a mut SystemActorState,
    ball_positions: &'a Arc<RankedRwLock<HashMap<BallId, DVec3>, RANK_LEAF>>,
    scrambled_balls: &'a Arc<RankedRwLock<HashSet<BallId>, RANK_LEAF>>,
    module_deactivation_hook: &'a Arc<RankedRwLock<Option<ModuleDeactivationHook>, RANK_LEAF>>,
}

impl<'a> BallOpsCtx<'a> {
    fn from_exec_ctx(exec_ctx: &'a mut ActorExecCtx<'_>) -> Self {
        Self {
            solar_system_id: exec_ctx.solar_system_id,
            ballpark: exec_ctx.ballpark,
            actor_st: exec_ctx.actor_st,
            ball_positions: exec_ctx.ball_positions_actor,
            scrambled_balls: exec_ctx.scrambled_balls_actor,
            module_deactivation_hook: exec_ctx.module_deactivation_hook_actor,
        }
    }

    fn read_deact_hook(&self) -> Option<ModuleDeactivationHook> {
        self.module_deactivation_hook
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

/// Sends a [`SystemCommand`] with bounded enqueue and reply deadlines (`timeout_dur`), returning an
/// explicit `Err` when the actor is closed, timed out, or drops its reply channel.
pub(crate) async fn call_actor_result_with_deadline<T>(
    handle: &SystemHandle,
    label: &str,
    timeout_dur: Duration,
    make_cmd: impl FnOnce(oneshot::Sender<Result<T, String>>) -> SystemCommand,
) -> Result<T, String> {
    let (tx, rx) = oneshot::channel();
    let cmd = make_cmd(tx);
    match tokio::time::timeout(timeout_dur, handle.cmd_sender().send(cmd)).await {
        Err(_) => {
            return Err(format!(
                "system {} actor timed out queuing {label} after {}ms",
                handle.solar_system_id,
                timeout_dur.as_millis()
            ));
        }
        Ok(Err(_)) => {
            return Err(format!("system {} actor closed", handle.solar_system_id));
        }
        Ok(Ok(())) => {}
    }
    match tokio::time::timeout(timeout_dur, rx).await {
        Err(_) => Err(format!(
            "system {} actor timed out awaiting {label} after {}ms",
            handle.solar_system_id,
            timeout_dur.as_millis()
        )),
        Ok(Err(_)) => Err(format!(
            "system {} actor dropped {label} reply",
            handle.solar_system_id
        )),
        Ok(Ok(res)) => res,
    }
}

fn run_direct_ball_op_on_current_thread<T>(
    handle: &SystemHandle,
    op: impl FnOnce(&mut BallOpsCtx<'_>) -> Result<T, String>,
) -> Result<T, String> {
    let mut ballpark = handle
        .ballpark
        .write_untracked()
        .unwrap_or_else(|e| e.into_inner());
    let mut actor_st = handle
        .actor_state
        .write()
        .unwrap_or_else(|e| e.into_inner());
    let mut ctx = BallOpsCtx {
        solar_system_id: handle.solar_system_id,
        ballpark: &mut ballpark,
        actor_st: &mut actor_st,
        ball_positions: &handle.ball_positions,
        scrambled_balls: &handle.scrambled_balls,
        module_deactivation_hook: &handle.module_deactivation_hook,
    };
    let out = op(&mut ctx);
    protection::sync_active_module_ships_cache(
        &actor_st.active_module_cycles,
        &actor_st.external_active_modules,
        &handle.active_module_ships,
    );
    celestials::update_system_hibernation_flag(
        &actor_st,
        &ballpark,
        handle.event_tx.receiver_count(),
    );
    handle.publish_view_from(&ballpark, Some(&actor_st));
    drop(actor_st);
    drop(ballpark);
    flush_post_unlock_actions(&handle.post_unlock_actions);
    out
}

fn dispatch_actor_sync_with_deadline<P, T>(
    handle: &SystemHandle,
    label: &str,
    timeout_dur: Duration,
    payload: P,
    make_cmd: impl FnOnce(P, oneshot::Sender<Result<T, String>>) -> SystemCommand,
    direct_fn: impl FnOnce(&mut BallOpsCtx<'_>, P) -> Result<T, String>,
) -> Result<T, String> {
    if handle.cmd_sender().is_closed() {
        return Err(format!("system {} actor closed", handle.solar_system_id));
    }
    match tokio::runtime::Handle::try_current() {
        Ok(rt_handle) => match rt_handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::CurrentThread => {
                // Legacy `#[tokio::test(flavor = "current_thread")]` caller running on the
                // actor's single thread cannot block synchronously on the mailbox; run the exact
                // same shared actor handler directly under the actor locks + publish view.
                run_direct_ball_op_on_current_thread(handle, |ctx| direct_fn(ctx, payload))
            }
            _ => tokio::task::block_in_place(|| {
                rt_handle.block_on(call_actor_result_with_deadline(
                    handle,
                    label,
                    timeout_dur,
                    |reply| make_cmd(payload, reply),
                ))
            }),
        },
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .map_err(|e| format!("failed to build transient runtime for {label}: {e}"))?;
            rt.block_on(call_actor_result_with_deadline(
                handle,
                label,
                timeout_dur,
                |reply| make_cmd(payload, reply),
            ))
        }
    }
}

fn convert_ball_to_rigid_at_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    position: DVec3,
) -> Result<bool, String> {
    let Some(ball) = ctx.ballpark.balls.get_mut(&ball_id) else {
        return Ok(false);
    };
    if matches!(ball.mode, MovementMode::Rigid) {
        return Ok(false);
    }
    ball.mode = MovementMode::Rigid;
    ball.position = position;
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    Ok(true)
}

fn set_ball_type_id_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    type_id: u32,
) -> Result<bool, String> {
    let Some(ball) = ctx.ballpark.balls.get_mut(&ball_id) else {
        return Ok(false);
    };
    ball.type_id = type_id;
    Ok(true)
}

fn set_combat_state_hp_bounds_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    shield_hp: f64,
    max_shield_hp: f64,
    armor_hp: f64,
    max_armor_hp: f64,
    structure_hp: f64,
    max_structure_hp: f64,
) -> Result<bool, String> {
    let Some(cs) = ctx.ballpark.combat_states.get_mut(&ball_id) else {
        return Ok(false);
    };
    cs.shield_hp = shield_hp;
    cs.max_shield_hp = max_shield_hp;
    cs.armor_hp = armor_hp;
    cs.max_armor_hp = max_armor_hp;
    cs.structure_hp = structure_hp;
    cs.max_structure_hp = max_structure_hp;
    Ok(true)
}

fn apply_npc_ai_step_effects_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    mechanic_rng: &crate::rng::MechanicRng,
    target_sigs: &HashMap<BallId, f64>,
    mode_updates: Vec<crate::combat::npc_ai::ModeUpdate>,
    dmg_actions: Vec<crate::combat::npc_ai::AiDamageAction>,
) -> Result<crate::combat::npc_ai::NpcAiWorldEffectsOutput, String> {
    let out = crate::combat::npc_ai::apply_npc_ai_world_effects_in_actor(
        ctx.ballpark,
        mechanic_rng,
        target_sigs,
        mode_updates,
        dmg_actions,
    );
    let deact_guard = ctx.read_deact_hook();
    super::reconcile_tackle_and_bubbles(
        ctx.solar_system_id,
        ctx.ballpark,
        &mut ctx.actor_st.active_module_cycles,
        &ctx.actor_st.base_scramble_strengths,
        &ctx.actor_st.base_core_strengths,
        &ctx.actor_st.db_opt,
        deact_guard.as_ref(),
    );
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    Ok(out)
}

fn apply_npc_remote_repairs_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    peer_ids: &[BallId],
    shield_rep: f64,
    armor_rep: f64,
) -> Result<usize, String> {
    let mut repaired = 0usize;
    for &pid in peer_ids {
        if let Some(pcs) = ctx.ballpark.combat_states.get_mut(&pid) {
            if shield_rep > 0.0 && pcs.shield_hp < pcs.max_shield_hp {
                pcs.shield_hp = (pcs.shield_hp + shield_rep).min(pcs.max_shield_hp);
            }
            if armor_rep > 0.0 && pcs.armor_hp < pcs.max_armor_hp {
                pcs.armor_hp = (pcs.armor_hp + armor_rep).min(pcs.max_armor_hp);
            }
            repaired += 1;
        }
    }
    Ok(repaired)
}

fn execute_extended_npc_attack_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    input: ExtendedNpcAttackActorInput,
) -> Result<ExtendedNpcAttackActorOutput, String> {
    let ExtendedNpcAttackActorInput {
        sde,
        mechanic_rng,
        mut npc,
        mut weapon_rt,
        target_id,
        target_radius,
        dist,
        target_sig_radii,
        is_capital_target,
        sleeper_sentry_can_fire,
        edencom_meta,
        destroyed_in_call,
        target_already_scrammed,
    } = input;

    let mut ewar_applied = false;
    let (eff_target_radius, live_speed_fraction) = ctx
        .ballpark
        .balls
        .get(&target_id)
        .map(|b| (b.radius, b.speed_fraction))
        .unwrap_or((target_radius, 1.0));

    if npc.web_speed_factor < 0.0
        && dist <= npc.web_range
        && !ctx
            .ballpark
            .combat_states
            .get(&target_id)
            .is_some_and(|tcs| tcs.is_web_immune)
    {
        let mult = (1.0 + npc.web_speed_factor / 100.0).clamp(0.1, 1.0);
        let _ = ctx
            .ballpark
            .set_speed_fraction(target_id, live_speed_fraction.max(1.0) * mult);
        ewar_applied = true;
    }

    let can_scram = if npc.archetype == "Sleeper" {
        npc.sleeper_wh_class >= 3
    } else {
        true
    };
    let mut scrammed_target = target_already_scrammed;
    if can_scram && npc.scram_strength > 0.0 && dist <= npc.scram_range {
        scrammed_target = true;
        ctx.actor_st
            .base_scramble_strengths
            .insert(target_id, npc.scram_strength as i32);
        if let Some(tcs) = ctx.ballpark.combat_states.get_mut(&target_id) {
            tcs.warp_scramble_strength =
                tcs.warp_scramble_strength.max(npc.scram_strength as i32);
            tcs.is_mwd_mjd_disabled = true;
        }
        let deact_guard = ctx.read_deact_hook();
        super::reconcile_tackle_and_bubbles(
            ctx.solar_system_id,
            ctx.ballpark,
            &mut ctx.actor_st.active_module_cycles,
            &ctx.actor_st.base_scramble_strengths,
            &ctx.actor_st.base_core_strengths,
            &ctx.actor_st.db_opt,
            deact_guard.as_ref(),
        );
        ewar_applied = true;
    }

    if npc.neut_amount > 0.0 && dist <= npc.optimal_range {
        if let Some(tcs) = ctx.ballpark.combat_states.get_mut(&target_id) {
            tcs.capacitor = (tcs.capacitor - npc.neut_amount).max(0.0);
            ewar_applied = true;
        }
    }

    if npc.archetype == "ZarzakhGateGun" {
        crate::services::combat_crimewatch::combat_npc_ai::apply_zarzakh_gate_gun_ewar(
            &sde,
            npc.type_id,
            ctx.ballpark,
            target_id,
            eff_target_radius,
            dist,
        );
    } else if npc.archetype == "CounterInsurgencySentry" || npc.type_id == 79_507 {
        crate::services::security_mgr::apply_counter_insurgency_sentry_ewar(
            &sde,
            ctx.ballpark,
            target_id,
            eff_target_radius,
            dist,
        );
    }

    let mut destroyed_in_tick = Vec::new();
    let mut homefront_damage_events = Vec::new();
    let weapon_fired =
        crate::combat::npc_ai_weapons_loot::execute_extended_npc_attack_with_sde_and_rng(
            &sde,
            &mechanic_rng,
            ctx.ballpark,
            &mut npc,
            &mut weapon_rt,
            target_id,
            &target_sig_radii,
            is_capital_target,
            sleeper_sentry_can_fire,
            edencom_meta.as_deref(),
            &destroyed_in_call,
            &mut destroyed_in_tick,
            &mut homefront_damage_events,
        );
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    let did_aggress = weapon_fired || ewar_applied || scrammed_target;
    Ok(ExtendedNpcAttackActorOutput {
        npc,
        weapon_rt,
        destroyed_in_tick,
        homefront_damage_events,
        scrammed_target,
        did_aggress,
    })
}

fn clear_npc_scram_targets_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    targets: &[BallId],
) -> Result<(), String> {
    for tid in targets {
        ctx.actor_st.base_scramble_strengths.remove(tid);
    }
    let deact_guard = ctx.read_deact_hook();
    super::reconcile_tackle_and_bubbles(
        ctx.solar_system_id,
        ctx.ballpark,
        &mut ctx.actor_st.active_module_cycles,
        &ctx.actor_st.base_scramble_strengths,
        &ctx.actor_st.base_core_strengths,
        &ctx.actor_st.db_opt,
        deact_guard.as_ref(),
    );
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    Ok(())
}

fn step_ext_deployables_in_ball_ops(
    ctx: &mut BallOpsCtx<'_>,
    sde: &eve_sde::SdeRegistry,
    db: &Arc<eve_db::WorldRepository>,
    mechanic_rng: &crate::rng::MechanicRng,
    now_unix: u64,
) -> Result<(), String> {
    let deact_guard = ctx.read_deact_hook();
    crate::services::mobile_deployables_ext::step_ext_deployables_in_actor(
        ctx.ballpark,
        Some(&mut ctx.actor_st.active_module_cycles),
        deact_guard.as_ref(),
        sde,
        db,
        Some(mechanic_rng),
        ctx.solar_system_id,
        now_unix,
    );
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    Ok(())
}

fn reconnect_live_ship_in_space_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    req: &ReconnectLiveShipRequest,
) -> Result<Option<DVec3>, String> {
    let ship_id = req.ship_id;
    let Some(initial_pos) = ctx.ballpark.balls.get(&ship_id).map(|b| b.position) else {
        return Ok(None);
    };
    let existing_pos = initial_pos;
    if let Some(cs) = ctx.ballpark.combat_states.get_mut(&ship_id) {
        cs.is_emergency_warping = false;
    }
    let mut started_return_warp = false;
    if req.pre_warp_eligible
        && crate::services::space_nav_movement_guard::can_warp_in_ballpark(ctx.ballpark, ship_id)
    {
        if let Some(orig_arr) = req.pre_warp_pos_opt {
            let orig_pos = DVec3::from_array(orig_arr);
            if existing_pos.distance(orig_pos) > 10_000.0 {
                if matches!(
                    ctx.ballpark.balls.get(&ship_id).map(|b| &b.mode),
                    Some(MovementMode::Warp { phase, .. })
                        if *phase != eve_destiny::WarpPhase::Aligning
                ) {
                    let _ = ctx.ballpark.force_stop_warp(ship_id);
                }
                if ctx
                    .ballpark
                    .set_mode(
                        ship_id,
                        MovementMode::new_warp(existing_pos, orig_pos, req.warp_speed),
                    )
                    .is_ok()
                {
                    started_return_warp = true;
                }
            }
        }
    }
    if req.pre_warp_pos_opt.is_some()
        && !started_return_warp
        && matches!(
            ctx.ballpark.balls.get(&ship_id).map(|b| &b.mode),
            Some(MovementMode::Warp { .. })
        )
    {
        let _ = ctx.ballpark.force_stop_warp(ship_id);
    }
    ctx.ballpark.observer_events.remove(&ship_id);
    ctx.ballpark.queue_set_state(ship_id);

    if started_return_warp {
        ctx.actor_st.warp_state.track_return_warp(ship_id);
    } else if req.keep_client_warp {
        ctx.actor_st
            .warp_state
            .track_player_or_fleet_warp(ship_id, req.char_id);
    } else {
        ctx.actor_st.warp_state.provenance.remove(&ship_id);
        ctx.actor_st.warp_state.return_warps.remove(&ship_id);
        ctx.actor_st
            .warp_state
            .pending_warp_finished
            .remove(&ship_id);
    }
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    protection::sync_ball_protection_in_actor(ctx.ballpark, ctx.actor_st, ship_id);
    Ok(Some(existing_pos))
}

fn spawn_reconnected_ship_in_space_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    req: SpawnReconnectedShipRequest,
) -> Result<(), String> {
    let SpawnReconnectedShipRequest {
        ball,
        combat_state,
        slim_meta,
        char_id,
        pre_warp_eligible,
        pre_warp_pos_opt,
        effective_warp_speed,
        resume_client_warp,
    } = req;
    let ship_id = ball.id;
    let final_pos = ball.position;
    if ctx.ballpark.balls.contains_key(&ship_id) {
        let _ = ctx.ballpark.remove_ball(ship_id);
    }
    if matches!(ball.mode, MovementMode::Rigid) {
        ctx.ballpark
            .add_static_ball_with_meta(ball.clone(), slim_meta);
        ctx.ballpark.combat_states.insert(ship_id, combat_state);
        ctx.ballpark
            .pending_events
            .push(eve_destiny::DestinyEvent::AddBall { ball: ball.clone() });
        ctx.ballpark.queue_event_for_all_grid_observers(
            Some(ship_id),
            &eve_destiny::DestinyEvent::AddBalls2 { balls: vec![ball] },
        );
    } else {
        if super::should_pin_stationary_ball(&ball, Some(&slim_meta)) {
            ctx.ballpark.mark_ball_non_free(ship_id, false);
        }
        ctx.ballpark.set_ball_slim_meta(ship_id, slim_meta);
        ctx.ballpark.add_ball(ball);
        ctx.ballpark.combat_states.insert(ship_id, combat_state);
    }

    let mut started_return_warp = false;
    if pre_warp_eligible
        && crate::services::space_nav_movement_guard::can_warp_in_ballpark(ctx.ballpark, ship_id)
    {
        if let Some(orig_arr) = pre_warp_pos_opt {
            let orig_pos = DVec3::from_array(orig_arr);
            if final_pos.distance(orig_pos) > 10_000.0
                && ctx
                    .ballpark
                    .set_mode(
                        ship_id,
                        MovementMode::new_warp(final_pos, orig_pos, effective_warp_speed),
                    )
                    .is_ok()
            {
                started_return_warp = true;
            }
        }
    }
    ctx.ballpark.observer_events.remove(&ship_id);
    ctx.ballpark.queue_set_state(ship_id);

    if started_return_warp {
        ctx.actor_st.warp_state.track_return_warp(ship_id);
    } else if resume_client_warp {
        ctx.actor_st
            .warp_state
            .track_player_or_fleet_warp(ship_id, char_id);
    } else {
        ctx.actor_st.warp_state.provenance.remove(&ship_id);
        ctx.actor_st.warp_state.return_warps.remove(&ship_id);
        ctx.actor_st
            .warp_state
            .pending_warp_finished
            .remove(&ship_id);
    }
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    protection::sync_ball_protection_in_actor(ctx.ballpark, ctx.actor_st, ship_id);
    Ok(())
}

fn start_disconnected_emergency_warp_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    current_pos: DVec3,
    dest: DVec3,
    warp_speed_au_s: f64,
) -> Result<bool, String> {
    if let Some(cs) = ctx.ballpark.combat_states.get_mut(&ball_id) {
        cs.is_emergency_warping = true;
    }
    let warped = ctx
        .ballpark
        .set_mode(
            ball_id,
            MovementMode::new_warp(current_pos, dest, warp_speed_au_s),
        )
        .is_ok();
    ctx.actor_st.warp_state.clear_ball(ball_id, false);
    ctx.actor_st
        .warp_state
        .provenance
        .insert(ball_id, WarpProvenance::DisconnectedEmergencyWarp);
    ctx.actor_st.warp_state.pending_warp_finished.remove(&ball_id);
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    protection::sync_ball_protection_in_actor(ctx.ballpark, ctx.actor_st, ball_id);
    Ok(warped)
}

pub(in crate::system_actor) fn handle_ball_ops_command(
    exec_ctx: &mut ActorExecCtx<'_>,
    cmd: SystemCommand,
) -> Option<SystemCommand> {
    match cmd {
        SystemCommand::ConvertBallToRigidAt {
            ball_id,
            position,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = convert_ball_to_rigid_at_in_actor(&mut ctx, ball_id, position);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::SetBallTypeId {
            ball_id,
            type_id,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = set_ball_type_id_in_actor(&mut ctx, ball_id, type_id);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::SetCombatStateHpBounds {
            ball_id,
            shield_hp,
            max_shield_hp,
            armor_hp,
            max_armor_hp,
            structure_hp,
            max_structure_hp,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = set_combat_state_hp_bounds_in_actor(
                &mut ctx,
                ball_id,
                shield_hp,
                max_shield_hp,
                armor_hp,
                max_armor_hp,
                structure_hp,
                max_structure_hp,
            );
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ApplyNpcAiStepEffects {
            mechanic_rng,
            target_sigs,
            mode_updates,
            dmg_actions,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = apply_npc_ai_step_effects_in_actor(
                &mut ctx,
                &mechanic_rng,
                &target_sigs,
                mode_updates,
                dmg_actions,
            );
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ApplyNpcRemoteRepairs {
            peer_ids,
            shield_rep,
            armor_rep,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = apply_npc_remote_repairs_in_actor(&mut ctx, &peer_ids, shield_rep, armor_rep);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ExecuteExtendedNpcAttack { input, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = execute_extended_npc_attack_in_actor(&mut ctx, *input);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ClearNpcScramTargets { targets, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = clear_npc_scram_targets_in_actor(&mut ctx, &targets);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::StepExtDeployables {
            sde,
            db,
            mechanic_rng,
            now_unix,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = step_ext_deployables_in_ball_ops(&mut ctx, &sde, &db, &mechanic_rng, now_unix);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::ReconnectLiveShipInSpace { request, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = reconnect_live_ship_in_space_in_actor(&mut ctx, &request);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::SpawnReconnectedShipInSpace { request, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = spawn_reconnected_ship_in_space_in_actor(&mut ctx, *request);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::StartDisconnectedEmergencyWarp {
            ball_id,
            current_pos,
            dest,
            warp_speed_au_s,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = start_disconnected_emergency_warp_in_actor(
                &mut ctx,
                ball_id,
                current_pos,
                dest,
                warp_speed_au_s,
            );
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        other => Some(other),
    }
}

impl SystemHandle {
    /// Converts an existing non-`Rigid` ball to `MovementMode::Rigid` at `position` (`#1342`).
    pub async fn convert_ball_to_rigid_at(
        &self,
        ball_id: BallId,
        position: DVec3,
    ) -> Result<bool, String> {
        call_actor_result_with_deadline(
            self,
            "ConvertBallToRigidAt",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::ConvertBallToRigidAt {
                ball_id,
                position,
                reply,
            },
        )
        .await
    }

    /// Updates `ball.type_id` in the system ballpark actor (`#1342`).
    pub async fn set_ball_type_id(&self, ball_id: BallId, type_id: u32) -> Result<bool, String> {
        call_actor_result_with_deadline(self, "SetBallTypeId", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::SetBallTypeId {
                ball_id,
                type_id,
                reply,
            }
        })
        .await
    }

    /// Synchronizes `(shield, armor, hull)` current and maximum HP on `ball_id` in the actor (`#1342`).
    #[allow(clippy::too_many_arguments)]
    pub async fn set_combat_state_hp_bounds(
        &self,
        ball_id: BallId,
        shield_hp: f64,
        max_shield_hp: f64,
        armor_hp: f64,
        max_armor_hp: f64,
        structure_hp: f64,
        max_structure_hp: f64,
    ) -> Result<bool, String> {
        call_actor_result_with_deadline(
            self,
            "SetCombatStateHpBounds",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::SetCombatStateHpBounds {
                ball_id,
                shield_hp,
                max_shield_hp,
                armor_hp,
                max_armor_hp,
                structure_hp,
                max_structure_hp,
                reply,
            },
        )
        .await
    }

    /// Applies `CMB-51` NPC AI movement mode updates and damage actions through the actor mailbox (`#1342`).
    pub fn apply_npc_ai_step_effects_sync(
        &self,
        mechanic_rng: Arc<crate::rng::MechanicRng>,
        target_sigs: HashMap<BallId, f64>,
        mode_updates: Vec<crate::combat::npc_ai::ModeUpdate>,
        dmg_actions: Vec<crate::combat::npc_ai::AiDamageAction>,
    ) -> Result<crate::combat::npc_ai::NpcAiWorldEffectsOutput, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "ApplyNpcAiStepEffects",
            ACTOR_CMD_TIMEOUT,
            (mechanic_rng, target_sigs, mode_updates, dmg_actions),
            |(mechanic_rng, target_sigs, mode_updates, dmg_actions), reply| {
                SystemCommand::ApplyNpcAiStepEffects {
                    mechanic_rng,
                    target_sigs,
                    mode_updates,
                    dmg_actions,
                    reply,
                }
            },
            |ctx, (mechanic_rng, target_sigs, mode_updates, dmg_actions)| {
                apply_npc_ai_step_effects_in_actor(
                    ctx,
                    &mechanic_rng,
                    &target_sigs,
                    mode_updates,
                    dmg_actions,
                )
            },
        )
    }

    /// Applies NPC remote shield/armor repairs to `peer_ids` through the actor mailbox (`#1342`).
    pub fn apply_npc_remote_repairs_sync(
        &self,
        peer_ids: Vec<BallId>,
        shield_rep: f64,
        armor_rep: f64,
    ) -> Result<usize, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "ApplyNpcRemoteRepairs",
            ACTOR_CMD_TIMEOUT,
            peer_ids,
            |peer_ids, reply| SystemCommand::ApplyNpcRemoteRepairs {
                peer_ids,
                shield_rep,
                armor_rep,
                reply,
            },
            |ctx, peer_ids| {
                apply_npc_remote_repairs_in_actor(ctx, &peer_ids, shield_rep, armor_rep)
            },
        )
    }

    /// Runs extended NPC EWAR + weapon application inside the actor mailbox with bounded deadline (`#1342`).
    pub(crate) fn execute_extended_npc_attack_sync(
        &self,
        input: ExtendedNpcAttackActorInput,
    ) -> Result<ExtendedNpcAttackActorOutput, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "ExecuteExtendedNpcAttack",
            ACTOR_CMD_TIMEOUT,
            input,
            |input, reply| SystemCommand::ExecuteExtendedNpcAttack {
                input: Box::new(input),
                reply,
            },
            execute_extended_npc_attack_in_actor,
        )
    }

    /// Clears expired NPC scramble targets and reconciles tackle state inside the actor (`#1342`).
    pub fn clear_npc_scram_targets_sync(&self, targets: Vec<BallId>) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "ClearNpcScramTargets",
            ACTOR_CMD_TIMEOUT,
            targets,
            |targets, reply| SystemCommand::ClearNpcScramTargets { targets, reply },
            |ctx, targets| clear_npc_scram_targets_in_actor(ctx, &targets),
        )
    }

    /// Steps mobile deployables (MMJU spools, Observatories, Siphons) inside the actor (`#1342`).
    pub fn step_ext_deployables_sync(
        &self,
        sde: Arc<eve_sde::SdeRegistry>,
        db: Arc<eve_db::WorldRepository>,
        mechanic_rng: Arc<crate::rng::MechanicRng>,
        now_unix: u64,
    ) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "StepExtDeployables",
            ACTOR_CMD_TIMEOUT,
            (sde, db, mechanic_rng),
            |(sde, db, mechanic_rng), reply| SystemCommand::StepExtDeployables {
                sde,
                db,
                mechanic_rng,
                now_unix,
                reply,
            },
            |ctx, (sde, db, mechanic_rng)| {
                step_ext_deployables_in_ball_ops(ctx, &sde, &db, &mechanic_rng, now_unix)
            },
        )
    }

    /// Atomically handles live-ship reconnect cleanup and return-warp initiation (`plan:SP-6`, `#1342`).
    pub fn reconnect_live_ship_in_space_sync(
        &self,
        request: ReconnectLiveShipRequest,
    ) -> Result<Option<DVec3>, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "ReconnectLiveShipInSpace",
            ACTOR_CMD_TIMEOUT,
            request,
            |request, reply| SystemCommand::ReconnectLiveShipInSpace { request, reply },
            |ctx, request| reconnect_live_ship_in_space_in_actor(ctx, &request),
        )
    }

    /// Async mailbox path for [`Self::reconnect_live_ship_in_space_sync`] (`plan:SP-6`, `#1342`).
    pub async fn reconnect_live_ship_in_space(
        &self,
        request: ReconnectLiveShipRequest,
    ) -> Result<Option<DVec3>, String> {
        call_actor_result_with_deadline(
            self,
            "ReconnectLiveShipInSpace",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::ReconnectLiveShipInSpace { request, reply },
        )
        .await
    }

    /// Atomically spawns a reconnected ship ball and initiates any return warp (`plan:SP-6`, `#1342`).
    pub fn spawn_reconnected_ship_in_space_sync(
        &self,
        request: SpawnReconnectedShipRequest,
    ) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "SpawnReconnectedShipInSpace",
            ACTOR_CMD_TIMEOUT,
            request,
            |request, reply| SystemCommand::SpawnReconnectedShipInSpace {
                request: Box::new(request),
                reply,
            },
            spawn_reconnected_ship_in_space_in_actor,
        )
    }

    /// Async mailbox path for [`Self::spawn_reconnected_ship_in_space_sync`] (`plan:SP-6`, `#1342`).
    pub async fn spawn_reconnected_ship_in_space(
        &self,
        request: SpawnReconnectedShipRequest,
    ) -> Result<(), String> {
        call_actor_result_with_deadline(
            self,
            "SpawnReconnectedShipInSpace",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::SpawnReconnectedShipInSpace {
                request: Box::new(request),
                reply,
            },
        )
        .await
    }

    /// Atomically flags `is_emergency_warping` and enters outbound emergency warp (`plan:SP-6`, `#1342`).
    pub fn start_disconnected_emergency_warp_sync(
        &self,
        ball_id: BallId,
        current_pos: DVec3,
        dest: DVec3,
        warp_speed_au_s: f64,
    ) -> Result<bool, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "StartDisconnectedEmergencyWarp",
            ACTOR_CMD_TIMEOUT,
            (),
            |(), reply| SystemCommand::StartDisconnectedEmergencyWarp {
                ball_id,
                current_pos,
                dest,
                warp_speed_au_s,
                reply,
            },
            |ctx, ()| {
                start_disconnected_emergency_warp_in_actor(
                    ctx,
                    ball_id,
                    current_pos,
                    dest,
                    warp_speed_au_s,
                )
            },
        )
    }

    /// Async mailbox path for [`Self::start_disconnected_emergency_warp_sync`] (`plan:SP-6`, `#1342`).
    pub async fn start_disconnected_emergency_warp(
        &self,
        ball_id: BallId,
        current_pos: DVec3,
        dest: DVec3,
        warp_speed_au_s: f64,
    ) -> Result<bool, String> {
        call_actor_result_with_deadline(
            self,
            "StartDisconnectedEmergencyWarp",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::StartDisconnectedEmergencyWarp {
                ball_id,
                current_pos,
                dest,
                warp_speed_au_s,
                reply,
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::system_actor::{BallparkHandle, PendingDestroyedOutbox, SystemView};
    use eve_db::lock_rank::RankedMutex;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize};
    use tokio::sync::{broadcast, mpsc, watch};

    fn make_test_handle(
        mailbox_cap: usize,
    ) -> (SystemHandle, mpsc::Receiver<SystemCommand>) {
        let (cmd_tx, cmd_rx) = mpsc::channel(mailbox_cap);
        let (event_tx, _) = broadcast::channel(16);
        let (view_tx, view_rx) = watch::channel(Arc::new(SystemView::default()));
        let view_tx = Arc::new(view_tx);
        let tidi_bits = Arc::new(AtomicU64::new(1.0_f64.to_bits()));
        let handle = SystemHandle {
            solar_system_id: 30_000_142,
            cmd_tx,
            event_tx,
            ball_positions: Arc::new(RankedRwLock::new(HashMap::new())),
            scrambled_balls: Arc::new(RankedRwLock::new(HashSet::new())),
            tethered_balls: Arc::new(RankedRwLock::new(HashSet::new())),
            committed_warp_balls: Arc::new(RankedRwLock::new(HashMap::new())),
            warp_stopped_at: Arc::new(RankedRwLock::new(HashMap::new())),
            active_module_ships: Arc::new(RankedRwLock::new(HashSet::new())),
            ballpark: Arc::new(BallparkHandle::new(
                Ballpark::new(30_000_142),
                Arc::clone(&view_tx),
                Arc::clone(&tidi_bits),
            )),
            actor_state: Arc::new(RankedRwLock::new(SystemActorState::default())),
            view_tx,
            view_rx,
            last_auto_stepped_unix: Arc::new(AtomicU64::new(0)),
            tidi_factor_bits: tidi_bits,
            sim_elapsed_secs_bits: Arc::new(AtomicU64::new(0.0_f64.to_bits())),
            last_visible_stamp: Arc::new(AtomicU64::new(0)),
            last_fx_stamp: Arc::new(AtomicU64::new(0)),
            hibernated: Arc::new(AtomicBool::new(true)),
            hibernated_since_unix: Arc::new(AtomicU64::new(0)),
            idle_ticks: Arc::new(AtomicU64::new(0)),
            dynamic_ball_count: Arc::new(AtomicUsize::new(0)),
            command_panics: Arc::new(AtomicU64::new(0)),
            pending_destroyed_balls: PendingDestroyedOutbox::default(),
            panic_next_command_for_test: Arc::new(AtomicBool::new(false)),
            panic_on_command_countdown_for_test: Arc::new(AtomicUsize::new(0)),
            panic_after_destruction_for_test: Arc::new(AtomicBool::new(false)),
            before_step_reply_hook_for_test: Arc::new(RankedMutex::new(None)),
            npc_death_hook: Arc::new(RankedRwLock::new(None)),
            module_deactivation_hook: Arc::new(RankedRwLock::new(None)),
            target_lock_completed_hook: Arc::new(RankedRwLock::new(None)),
            system_step_hook: Arc::new(RankedRwLock::new(None)),
            post_unlock_actions: Arc::new(RankedMutex::new(Vec::new())),
        };
        handle.ballpark.attach_actor_state(&handle.actor_state);
        (handle, cmd_rx)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn bounded_deadline_and_closed_actor_errors_never_swallowed() {
        let (handle, mut rx) = make_test_handle(1);

        // 1. Accepted into mailbox, actor never replies -> reply timeout error.
        let err_await = call_actor_result_with_deadline(
            &handle,
            "ProbeAwait",
            Duration::from_millis(20),
            |reply| SystemCommand::SetBallTypeId {
                ball_id: 1,
                type_id: 2,
                reply,
            },
        )
        .await
        .expect_err("unanswered actor command must time out explicitly");
        assert!(
            err_await.contains("timed out awaiting ProbeAwait"),
            "unexpected await timeout message: {err_await}"
        );

        // 2. Mailbox full (capacity = 1) -> enqueue timeout error.
        let err_queue = call_actor_result_with_deadline(
            &handle,
            "ProbeQueue",
            Duration::from_millis(20),
            |reply| SystemCommand::SetBallTypeId {
                ball_id: 1,
                type_id: 2,
                reply,
            },
        )
        .await
        .expect_err("full mailbox must time out explicitly instead of hanging");
        assert!(
            err_queue.contains("timed out queuing ProbeQueue"),
            "unexpected queue timeout message: {err_queue}"
        );

        // 3. Draining and dropping reply channel -> dropped reply error.
        drop(rx.recv().await);
        let h_drop = handle.clone();
        let recv_task = tokio::spawn(async move {
            let cmd = rx.recv().await;
            drop(cmd);
            rx
        });
        let err_drop = call_actor_result_with_deadline(
            &h_drop,
            "ProbeDrop",
            Duration::from_millis(200),
            |reply| SystemCommand::SetBallTypeId {
                ball_id: 1,
                type_id: 2,
                reply,
            },
        )
        .await
        .expect_err("dropped reply must return explicit error");
        assert!(
            err_drop.contains("dropped ProbeDrop reply"),
            "unexpected dropped reply message: {err_drop}"
        );

        // 4. Dropped receiver -> closed actor error across async and sync wrappers.
        let rx_after = recv_task.await.unwrap();
        drop(rx_after);
        assert!(handle
            .convert_ball_to_rigid_at(1, DVec3::ZERO)
            .await
            .expect_err("closed actor must fail convert_ball_to_rigid_at")
            .contains("closed"));
        assert!(handle
            .set_ball_type_id(1, 79_586)
            .await
            .expect_err("closed actor must fail set_ball_type_id")
            .contains("closed"));
        assert!(handle
            .set_combat_state_hp_bounds(1, 1.0, 2.0, 1.0, 2.0, 1.0, 2.0)
            .await
            .expect_err("closed actor must fail set_combat_state_hp_bounds")
            .contains("closed"));
        assert!(handle
            .apply_npc_remote_repairs_sync(vec![1], 10.0, 10.0)
            .expect_err("closed actor must fail sync repairs")
            .contains("closed"));
        assert!(handle
            .clear_npc_scram_targets_sync(vec![1])
            .expect_err("closed actor must fail clear_npc_scram_targets_sync")
            .contains("closed"));
        assert!(handle
            .start_disconnected_emergency_warp_sync(1, DVec3::ZERO, DVec3::X * 1_000_000.0, 3.0)
            .expect_err("closed actor must fail start_disconnected_emergency_warp_sync")
            .contains("closed"));
    }
}
