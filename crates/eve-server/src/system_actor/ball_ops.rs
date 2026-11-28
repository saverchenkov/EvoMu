//! Bounded-deadline actor mailbox commands and shared actor-side handlers for external ballpark
//! migrations (`RFC-1 §7.3`, `#1342`).
//!
//! Async callers use [`call_actor_result_with_deadline`] with [`ACTOR_CMD_TIMEOUT`] so stopped,
//! dropped, or backpressured solar system actors return explicit errors rather than silently
//! falling back to external `ballpark` locks. Synchronous callers run the same mailbox commands
//! via `tokio::task::block_in_place` on multi-thread runtimes, via the system's `CtActorDriver`
//! runtime on `current_thread` runtimes, or via transient `current_thread` runtimes outside
//! Tokio, with all direct `ballpark` / `actor_state` lock helpers retired (`#1342` Track 4D-5).

#![forbid(unsafe_code)]

use super::{
    celestials, defer_barrier_cmd_reply, defer_cmd_reply, defer_reply, protection, ActorExecCtx,
    CharacterId, CooldownPersistGate, ExplicitUpdateStateGuard, ExtendedNpcAttackActorInput,
    ExtendedNpcAttackActorOutput, LogiSoakCycleActorInput, ModuleDeactivationHook,
    ReconnectLiveShipRequest, SpawnReconnectedShipRequest, SystemActorState, SystemCommand,
    SystemHandle, WarpProvenance,
};
use eve_db::lock_rank::{RankedMutex, RankedRwLock, RANK_LEAF};
use eve_destiny::{Ball, BallId, Ballpark, CombatState, DestinyEvent, MovementMode, SlimItemMeta};
use eve_sde::SolarSystemId;
use glam::DVec3;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::oneshot;

/// Default bounded mailbox deadline for actor-backed ballpark commands (`#1342`).
pub const ACTOR_CMD_TIMEOUT: Duration = Duration::from_secs(5);

pub(in crate::system_actor) struct BallOpsCtx<'a> {
    pub(in crate::system_actor) solar_system_id: SolarSystemId,
    pub(in crate::system_actor) ballpark: &'a mut Ballpark,
    pub(in crate::system_actor) actor_st: &'a mut SystemActorState,
    pub(in crate::system_actor) ball_positions: &'a Arc<celestials::BallPositionSyncCache>,
    pub(in crate::system_actor) scrambled_balls: &'a Arc<RankedRwLock<HashSet<BallId>, RANK_LEAF>>,
    pub(in crate::system_actor) module_deactivation_hook: &'a Arc<RankedRwLock<Option<ModuleDeactivationHook>, RANK_LEAF>>,
    pub(in crate::system_actor) deactivation_sink_hook: Option<ModuleDeactivationHook>,
    explicit_update_state_guards:
        Option<&'a Arc<RankedMutex<HashMap<(u64, i64), ExplicitUpdateStateGuard>, RANK_LEAF>>>,
    ball_spawn_tokens: Option<&'a mut HashMap<BallId, u64>>,
    removed_checkpoint_ball: Option<&'a mut Option<BallId>>,
    pub(in crate::system_actor) post_unlock_ops: &'a mut super::post_unlock::PostUnlockOps,
    pub(in crate::system_actor) cooldown_persist_gate: Option<&'a Arc<CooldownPersistGate>>,
    pub(in crate::system_actor) last_active_totals:
        Option<&'a mut HashMap<BallId, (Option<i32>, Option<i32>)>>,
    pub(in crate::system_actor) tidi_factor: f64,
}

impl<'a> BallOpsCtx<'a> {
    pub(in crate::system_actor) fn from_exec_ctx(exec_ctx: &'a mut ActorExecCtx<'_>) -> Self {
        Self {
            solar_system_id: exec_ctx.solar_system_id,
            ballpark: exec_ctx.ballpark,
            actor_st: exec_ctx.actor_st,
            ball_positions: exec_ctx.ball_positions_actor,
            scrambled_balls: exec_ctx.scrambled_balls_actor,
            module_deactivation_hook: exec_ctx.module_deactivation_hook_actor,
            deactivation_sink_hook: exec_ctx.deactivation_sink_hook.clone(),
            explicit_update_state_guards: Some(exec_ctx.explicit_update_state_guards_actor),
            ball_spawn_tokens: Some(exec_ctx.ball_spawn_tokens),
            removed_checkpoint_ball: Some(exec_ctx.removed_checkpoint_ball),
            post_unlock_ops: exec_ctx.post_unlock_ops,
            cooldown_persist_gate: Some(exec_ctx.cooldown_persist_gate_actor),
            last_active_totals: Some(exec_ctx.last_active_totals),
            tidi_factor: celestials::clamp_tidi_factor_bits(exec_ctx.tidi_factor_bits_actor),
        }
    }

    fn read_deact_hook(&self) -> Option<ModuleDeactivationHook> {
        self.deactivation_sink_hook.clone()
    }

    pub(in crate::system_actor) fn sync_positions(&self) {
        celestials::sync_ball_positions(self.ballpark, self.ball_positions, self.scrambled_balls);
    }
}

/// Sends a [`SystemCommand`] with bounded enqueue and reply deadlines (`timeout_dur`), returning
/// the raw actor reply `Ok(T)` or an explicit `Err` when the actor is closed, timed out, or
/// drops its reply channel (`#1342`).
pub(crate) async fn call_actor_reply_with_deadline<T>(
    handle: &SystemHandle,
    label: &str,
    timeout_dur: Duration,
    make_cmd: impl FnOnce(oneshot::Sender<T>) -> SystemCommand,
) -> Result<T, String> {
    let (tx, rx) = oneshot::channel();
    let cmd = make_cmd(tx);
    let from_callback = handle.is_on_post_unlock_callback_thread();
    let worker_cb_tx = from_callback.then_some(&handle.worker_cb_tx);
    await_actor_cmd_on_channels(
        handle.cmd_sender(),
        worker_cb_tx,
        Some(&handle.actor_shutting_down),
        handle.solar_system_id,
        label,
        timeout_dur,
        cmd,
        rx,
    )
    .await
}

async fn await_actor_cmd_on_channels<T>(
    cmd_sender: &tokio::sync::mpsc::Sender<SystemCommand>,
    worker_cb_tx: Option<
        &tokio::sync::mpsc::UnboundedSender<super::post_unlock::PostUnlockActorCallback>,
    >,
    shutting_down: Option<&std::sync::atomic::AtomicBool>,
    solar_system_id: SolarSystemId,
    label: &str,
    timeout_dur: Duration,
    cmd: SystemCommand,
    rx: oneshot::Receiver<T>,
) -> Result<T, String> {
    if let Some(cb_tx) = worker_cb_tx.filter(|_| !matches!(cmd, SystemCommand::ShutdownActor { .. }))
    {
        if cb_tx.is_closed()
            || cb_tx
                .send(super::post_unlock::PostUnlockActorCallback::ActorCmd(Box::new(cmd)))
                .is_err()
        {
            return Err(format!("system {solar_system_id} actor closed"));
        }
    } else {
        if cmd_sender.is_closed()
            || shutting_down.is_some_and(|s| s.load(std::sync::atomic::Ordering::SeqCst))
        {
            return Err(format!("system {solar_system_id} actor closed"));
        }
        match tokio::time::timeout(timeout_dur, cmd_sender.send(cmd)).await {
            Err(_) => {
                return Err(format!(
                    "system {solar_system_id} actor timed out queuing {label} after {}ms",
                    timeout_dur.as_millis()
                ));
            }
            Ok(Err(_)) => return Err(format!("system {solar_system_id} actor closed")),
            Ok(Ok(())) => {}
        }
    }
    match tokio::time::timeout(timeout_dur, rx).await {
        Err(_) => Err(format!(
            "system {solar_system_id} actor timed out awaiting {label} after {}ms",
            timeout_dur.as_millis()
        )),
        Ok(Err(_)) => Err(format!("system {solar_system_id} actor dropped {label} reply")),
        Ok(Ok(reply)) => Ok(reply),
    }
}

pub(in crate::system_actor) async fn await_actor_cmd_with_deadline<T>(
    cmd_sender: &tokio::sync::mpsc::Sender<SystemCommand>,
    solar_system_id: SolarSystemId,
    label: &str,
    timeout_dur: Duration,
    cmd: SystemCommand,
    rx: oneshot::Receiver<T>,
) -> Result<T, String> {
    await_actor_cmd_on_channels(
        cmd_sender,
        None,
        None,
        solar_system_id,
        label,
        timeout_dur,
        cmd,
        rx,
    )
    .await
}

/// Sends a [`SystemCommand`] with bounded enqueue and reply deadlines (`timeout_dur`), returning an
/// explicit `Err` when the actor is closed, timed out, or drops its reply channel.
pub(crate) async fn call_actor_result_with_deadline<T>(
    handle: &SystemHandle,
    label: &str,
    timeout_dur: Duration,
    make_cmd: impl FnOnce(oneshot::Sender<Result<T, String>>) -> SystemCommand,
) -> Result<T, String> {
    call_actor_reply_with_deadline(handle, label, timeout_dur, make_cmd).await?
}

fn await_actor_cmd_via_runtime<T: Send + 'static>(
    handle: &SystemHandle,
    target_rt: &tokio::runtime::Handle,
    label: &str,
    timeout_dur: Duration,
    cmd: SystemCommand,
    reply_rx: oneshot::Receiver<T>,
    use_block_in_place: bool,
) -> Result<T, String> {
    let cmd_sender = handle.cmd_sender().clone();
    let from_callback = handle.is_on_post_unlock_callback_thread();
    let worker_cb_tx = from_callback.then(|| handle.worker_cb_tx.clone());
    let shutting_down = Arc::clone(&handle.actor_shutting_down);
    let sys_id = handle.solar_system_id;
    let label_owned = label.to_owned();
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel::<Result<T, String>>(1);
    target_rt.spawn(async move {
        let res = await_actor_cmd_on_channels(
            &cmd_sender,
            worker_cb_tx.as_ref(),
            Some(&shutting_down),
            sys_id,
            &label_owned,
            timeout_dur,
            cmd,
            reply_rx,
        )
        .await;
        let _ = done_tx.send(res);
    });
    let wait_dur = timeout_dur
        .saturating_mul(2)
        .saturating_add(Duration::from_secs(1));
    let recv_res = if use_block_in_place {
        tokio::task::block_in_place(|| done_rx.recv_timeout(wait_dur))
    } else {
        done_rx.recv_timeout(wait_dur)
    };
    match recv_res {
        Ok(res) => res,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Err(format!(
            "system {sys_id} actor timed out awaiting {label} sync reply after {}ms",
            wait_dur.as_millis()
        )),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            let closed = if from_callback {
                handle.worker_cb_tx.is_closed()
            } else {
                handle.is_actor_closed()
            };
            if closed {
                Err(format!("system {sys_id} actor closed"))
            } else {
                Err(format!(
                    "system {sys_id} actor task dropped {label} sync reply channel"
                ))
            }
        }
    }
}

fn dispatch_actor_sync_reply_with_deadline_inner<P, T: Send + 'static>(
    handle: &SystemHandle,
    label: &str,
    timeout_dur: Duration,
    _run_post_flush: bool,
    _already_finalized_and_published: bool,
    payload: P,
    make_cmd: impl FnOnce(P, oneshot::Sender<T>) -> SystemCommand,
    _direct_fn: impl FnOnce(&mut BallOpsCtx<'_>, P) -> T,
) -> Result<T, String> {
    eve_db::lock_rank::assert_no_locks_held();
    if handle.is_actor_closed_for_current_thread() {
        return Err(format!("system {} actor closed", handle.solar_system_id));
    }
    let from_callback = handle.is_on_post_unlock_callback_thread();
    match tokio::runtime::Handle::try_current() {
        Ok(rt_handle) => match rt_handle.runtime_flavor() {
            tokio::runtime::RuntimeFlavor::CurrentThread => {
                let Some(actor_rt) = handle
                    .actor_rt_handle
                    .as_ref()
                    .filter(|r| r.id() != rt_handle.id())
                else {
                    return Err(format!(
                        "system {} actor runtime unavailable for {label} on current_thread",
                        handle.solar_system_id
                    ));
                };
                let (reply_tx, reply_rx) = oneshot::channel::<T>();
                let cmd = make_cmd(payload, reply_tx);
                await_actor_cmd_via_runtime(
                    handle,
                    actor_rt,
                    label,
                    timeout_dur,
                    cmd,
                    reply_rx,
                    false,
                )
            }
            _ => {
                // Isolate sync waiting from Tokio's thread-local `CURRENT_PARKER` (no nested `block_on`).
                let (reply_tx, reply_rx) = oneshot::channel::<T>();
                let cmd = make_cmd(payload, reply_tx);
                await_actor_cmd_via_runtime(
                    handle,
                    &rt_handle,
                    label,
                    timeout_dur,
                    cmd,
                    reply_rx,
                    true,
                )
            }
        },
        Err(_) => {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .map_err(|e| format!("failed to build transient runtime for {label}: {e}"))?;
            let (reply_tx, reply_rx) = oneshot::channel::<T>();
            let cmd = make_cmd(payload, reply_tx);
            let worker_cb_tx = from_callback.then_some(&handle.worker_cb_tx);
            rt.block_on(await_actor_cmd_on_channels(
                handle.cmd_sender(),
                worker_cb_tx,
                Some(&handle.actor_shutting_down),
                handle.solar_system_id,
                label,
                timeout_dur,
                cmd,
                reply_rx,
            ))
        }
    }
}

pub(in crate::system_actor) fn dispatch_actor_sync_reply_with_deadline<P, T: Send + 'static>(
    handle: &SystemHandle,
    label: &str,
    timeout_dur: Duration,
    payload: P,
    make_cmd: impl FnOnce(P, oneshot::Sender<T>) -> SystemCommand,
    direct_fn: impl FnOnce(&mut BallOpsCtx<'_>, P) -> T,
) -> Result<T, String> {
    dispatch_actor_sync_reply_with_deadline_inner(
        handle, label, timeout_dur, true, false, payload, make_cmd, direct_fn,
    )
}

pub(in crate::system_actor) fn dispatch_actor_step_full_tick_sync<P, T: Send + 'static>(
    handle: &SystemHandle,
    label: &str,
    timeout_dur: Duration,
    payload: P,
    make_cmd: impl FnOnce(P, oneshot::Sender<T>) -> SystemCommand,
    direct_fn: impl FnOnce(&mut BallOpsCtx<'_>, P) -> T,
) -> Result<T, String> {
    dispatch_actor_sync_reply_with_deadline_inner(
        handle, label, timeout_dur, false, true, payload, make_cmd, direct_fn,
    )
}

pub(in crate::system_actor) fn dispatch_actor_sync_with_deadline<P, T: Send + 'static>(
    handle: &SystemHandle,
    label: &str,
    timeout_dur: Duration,
    payload: P,
    make_cmd: impl FnOnce(P, oneshot::Sender<Result<T, String>>) -> SystemCommand,
    direct_fn: impl FnOnce(&mut BallOpsCtx<'_>, P) -> Result<T, String>,
) -> Result<T, String> {
    dispatch_actor_sync_reply_with_deadline_inner(
        handle, label, timeout_dur, true, false, payload, make_cmd, direct_fn,
    )?
}

pub(in crate::system_actor) fn dispatch_actor_sync_no_flush_with_deadline<P, T: Send + 'static>(
    handle: &SystemHandle,
    label: &str,
    timeout_dur: Duration,
    payload: P,
    make_cmd: impl FnOnce(P, oneshot::Sender<Result<T, String>>) -> SystemCommand,
    direct_fn: impl FnOnce(&mut BallOpsCtx<'_>, P) -> Result<T, String>,
) -> Result<T, String> {
    dispatch_actor_sync_reply_with_deadline_inner(
        handle, label, timeout_dur, false, false, payload, make_cmd, direct_fn,
    )?
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
    for &(_, dead_id, _) in &out.destroyed_balls {
        super::drones::clear_drone_runtime_in_state(ctx.actor_st, dead_id);
    }
    let deact_guard = ctx.read_deact_hook();
    super::cycles_tackle::reconcile_tackle_and_bubbles_collecting(
        ctx.solar_system_id,
        ctx.ballpark,
        &mut ctx.actor_st.active_module_cycles,
        &ctx.actor_st.base_scramble_strengths,
        &ctx.actor_st.base_core_strengths,
        &ctx.actor_st.db_opt,
        deact_guard.as_ref(),
        ctx.post_unlock_ops,
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
        super::cycles_tackle::reconcile_tackle_and_bubbles_collecting(
            ctx.solar_system_id,
            ctx.ballpark,
            &mut ctx.actor_st.active_module_cycles,
            &ctx.actor_st.base_scramble_strengths,
            &ctx.actor_st.base_core_strengths,
            &ctx.actor_st.db_opt,
            deact_guard.as_ref(),
            ctx.post_unlock_ops,
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
    for &(_, dead_id, _) in &destroyed_in_tick {
        super::drones::clear_drone_runtime_in_state(ctx.actor_st, dead_id);
    }
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
    super::cycles_tackle::reconcile_tackle_and_bubbles_collecting(
        ctx.solar_system_id,
        ctx.ballpark,
        &mut ctx.actor_st.active_module_cycles,
        &ctx.actor_st.base_scramble_strengths,
        &ctx.actor_st.base_core_strengths,
        &ctx.actor_st.db_opt,
        deact_guard.as_ref(),
        ctx.post_unlock_ops,
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
    let next_pu_seq = ctx.actor_st.next_post_unlock_seq.saturating_add(1);
    crate::services::mobile_deployables_ext::step_ext_deployables_in_actor_collecting(
        ctx.ballpark,
        Some(&mut ctx.actor_st.active_module_cycles),
        sde,
        db,
        Some(mechanic_rng),
        ctx.solar_system_id,
        now_unix,
        Some((&mut ctx.actor_st.post_unlock_shadows.mmju_spool_fired_at, next_pu_seq)),
        Some((&mut ctx.actor_st.post_unlock_shadows.observatory_pings_done, next_pu_seq)),
        ctx.post_unlock_ops,
    );
    protection::sync_all_protection_registry_in_actor(ctx.ballpark, ctx.actor_st);
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
        ctx.actor_st.warp_state.clear_ball(ship_id, true);
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
        ctx.actor_st.warp_state.clear_ball(ship_id, true);
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

pub(in crate::system_actor) fn prepare_disconnected_warp_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ship_id: BallId,
    warp_speed_au_s: f64,
    current_pos: DVec3,
    dest: DVec3,
) {
    if !ctx.ballpark.balls.contains_key(&ship_id) {
        // Do not synthesize a phantom ball from stale disconnected-ship state if the ship
        // already transitioned out of this solar system.
        return;
    }
    let needs_warp_init = ctx.ballpark.balls.get(&ship_id).is_some_and(|b| {
        !matches!(b.mode, MovementMode::Warp { .. }) && (b.position - dest).length() > 5_000.0
    });
    let warp_started = if needs_warp_init {
        let start_pos = ctx
            .ballpark
            .balls
            .get(&ship_id)
            .map(|b| b.position)
            .unwrap_or(current_pos);
        let ok = ctx
            .ballpark
            .set_mode(
                ship_id,
                MovementMode::new_warp(start_pos, dest, warp_speed_au_s.max(1.5)),
            )
            .is_ok();
        if ok {
            if let Some(cs) = ctx.ballpark.combat_states.get_mut(&ship_id) {
                cs.is_emergency_warping = true;
            }
        }
        ok
    } else {
        false
    };
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    if warp_started {
        super::protection::sync_ball_protection_in_actor(ctx.ballpark, ctx.actor_st, ship_id);
        ctx.actor_st.warp_state.clear_ball(ship_id, true);
        ctx.actor_st
            .warp_state
            .provenance
            .insert(ship_id, super::WarpProvenance::DisconnectedEmergencyWarp);
    }
}

pub(in crate::system_actor) fn reconcile_disconnected_warp_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ship_id: BallId,
    dest: DVec3,
) -> (DVec3, bool) {
    let arrived = ctx.ballpark.balls.get(&ship_id).is_none_or(|b| {
        !matches!(b.mode, MovementMode::Warp { .. }) || (b.position - dest).length() <= 2_500.0
    });
    if arrived {
        if ctx.ballpark.balls.get(&ship_id).is_some_and(|b| {
            matches!(b.mode, MovementMode::Warp { .. })
                || (!ctx.ballpark.is_ball_scrambled(ship_id)
                    && (b.position - dest).length() <= 50_000.0)
        }) {
            let _ = ctx.ballpark.set_ball_position(ship_id, dest);
        }
        let _ = ctx.ballpark.set_mode(ship_id, MovementMode::Stop);
        if let Some(b) = ctx.ballpark.balls.get_mut(&ship_id) {
            b.velocity = DVec3::ZERO;
            b.speed_fraction = 0.0;
        }
    }
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    let final_pos = ctx
        .ballpark
        .balls
        .get(&ship_id)
        .map(|b| b.position)
        .unwrap_or(dest);
    super::protection::finish_disconnected_warp_protection_sync(
        ctx.ballpark,
        ctx.actor_st,
        ship_id,
    );
    if arrived {
        ctx.actor_st.warp_state.clear_ball(ship_id, false);
    }
    (final_pos, arrived)
}

fn clear_removed_ball_actor_state(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    preserve_own_db: bool,
) {
    if let Some(tokens) = ctx.ball_spawn_tokens.as_deref_mut() {
        tokens.remove(&ball_id);
    }
    let deact_guard = ctx.read_deact_hook();
    super::cycles::remove_all_ship_module_cycles_preserving_own_db_collecting(
        ctx.solar_system_id,
        ctx.ballpark,
        &mut ctx.actor_st.active_module_cycles,
        &ctx.actor_st.base_scramble_strengths,
        &ctx.actor_st.base_core_strengths,
        &ctx.actor_st.db_opt,
        ball_id,
        deact_guard.as_ref(),
        preserve_own_db,
        ctx.post_unlock_ops,
    );
    super::SystemHandle::drain_ship_nanite_repairs_collecting(
        &mut ctx.actor_st.active_nanite_repairs,
        ball_id,
        true,
        ctx.post_unlock_ops,
    );
    ctx.actor_st.base_scramble_strengths.remove(&ball_id);
    ctx.actor_st.base_core_strengths.remove(&ball_id);
    ctx.actor_st.asteroids.remove(&ball_id);
    ctx.actor_st.fit_projections.remove(&ball_id);
    ctx.actor_st
        .external_active_modules
        .retain(|&(sid, _)| sid != ball_id);
    ctx.actor_st.protection_registry.remove(&ball_id);
    super::drones::clear_removed_ball_drones_in_state(ctx.actor_st, ball_id);
    if let Some(removed_slot) = ctx.removed_checkpoint_ball.as_deref_mut() {
        *removed_slot = Some(ball_id);
    } else {
        celestials::remove_ball_checkpoint_tracking(ctx.actor_st, ball_id);
    }
    if let Some(guards) = ctx.explicit_update_state_guards {
        guards
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, g| !(g.sys_id == ctx.solar_system_id && g.ship_id == ball_id));
    }
}

#[allow(clippy::too_many_arguments)]
pub(in crate::system_actor) fn remove_ball_for_post_commit(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    actor_st: &mut SystemActorState,
    ball_positions: &Arc<celestials::BallPositionSyncCache>,
    scrambled_balls: &Arc<RankedRwLock<HashSet<BallId>, RANK_LEAF>>,
    module_deactivation_hook: &Arc<RankedRwLock<Option<ModuleDeactivationHook>, RANK_LEAF>>,
    cooldown_persist_gate: Option<&Arc<CooldownPersistGate>>,
    tidi_factor: f64,
    post_unlock_ops: &mut super::post_unlock::PostUnlockOps,
    ball_id: BallId,
) -> Option<Ball> {
    let mut ctx = BallOpsCtx {
        solar_system_id,
        ballpark,
        actor_st,
        ball_positions,
        scrambled_balls,
        module_deactivation_hook,
        deactivation_sink_hook: module_deactivation_hook.read().unwrap_or_else(|e| e.into_inner()).clone(),
        explicit_update_state_guards: None,
        ball_spawn_tokens: None,
        removed_checkpoint_ball: None,
        post_unlock_ops,
        cooldown_persist_gate,
        last_active_totals: None,
        tidi_factor,
    };
    remove_ball_in_actor(&mut ctx, ball_id, false)
}

pub(in crate::system_actor) fn remove_ball_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    preserve_own_db: bool,
) -> Option<Ball> {
    crate::services::tactical_module_mechanics::on_probe_ball_removed_in_actor(
        ctx.ballpark,
        &ctx.actor_st.db_opt,
        ctx.actor_st.tactical_state.as_ref(),
        ball_id,
    );
    clear_removed_ball_actor_state(ctx, ball_id, preserve_own_db);
    let _ = ctx.actor_st.clear_fighter_ball_and_track(ball_id);
    let removed = ctx.ballpark.remove_ball(ball_id);
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    removed
}

fn accept_docking_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ship_id: BallId,
    station_id: BallId,
    station_pos: DVec3,
) -> Option<Ball> {
    // #1081 Req 5: Stop active propulsion OnSpecialFX on grid before detaching ball on dock.
    super::emit_propulsion_stop_fx_before_detach(
        ctx.ballpark,
        &ctx.actor_st.active_module_cycles,
        &ctx.actor_st.sde_opt,
        ship_id,
    );
    clear_removed_ball_actor_state(ctx, ship_id, false);
    let removed = if ctx.ballpark.balls.contains_key(&station_id) {
        ctx.ballpark.accept_docking(ship_id, station_id)
    } else {
        ctx.ballpark.accept_docking_at(ship_id, station_id, station_pos)
    };
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    removed
}

fn set_ball_position_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    position: DVec3,
) -> bool {
    let updated = ctx.ballpark.set_ball_position(ball_id, position);
    if updated {
        super::collect_ball_positions_for_post_unlock(
            ctx.ballpark,
            &[ball_id],
            &ctx.actor_st.db_opt,
            &ctx.actor_st.ball_space_entry_epochs,
            ctx.post_unlock_ops,
        );
    }
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    updated
}

fn handoff_in_space_pilot_observer_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    from_ball_id: BallId,
    to_ball_id: BallId,
    ensure_add_to_ball: bool,
) -> Result<(), String> {
    ctx.ballpark.pending_handoff_observers.insert(to_ball_id);
    if let Some(ref mut set) = ctx.ballpark.registered_observers {
        set.remove(&from_ball_id);
        set.insert(to_ball_id);
    }
    let add_ball = if ensure_add_to_ball {
        ctx.ballpark.balls.get(&to_ball_id).cloned()
    } else {
        None
    };
    let carried = ctx.ballpark.observer_events.remove(&from_ball_id);
    let target_queue = ctx.ballpark.observer_events.entry(to_ball_id).or_default();
    if let Some(prev_events) = carried {
        target_queue.extend(prev_events);
    }
    if let Some(ball) = add_ball {
        let already_added = target_queue.iter().any(|ev| {
            matches!(
                ev,
                DestinyEvent::AddBalls2 { balls }
                    if balls.iter().any(|b| b.id == to_ball_id)
            )
        });
        if !already_added {
            target_queue.push(DestinyEvent::AddBalls2 { balls: vec![ball] });
        }
    }
    Ok(())
}

fn update_ball_dbuffs_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ball_id: BallId,
    dbuffs: Vec<(i64, f64, i64)>,
) -> Result<(), String> {
    ctx.ballpark.update_dbuffs(ball_id, dbuffs);
    Ok(())
}

fn emit_logi_soak_stop_fx_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    npc_ids: &[BallId],
    target_ship_id: BallId,
    mod_type_ids: [u32; 2],
    fx_duration_ms: f64,
    active_parity: u8,
) -> Result<(), String> {
    for &npc_id in npc_ids {
        if ctx.ballpark.balls.contains_key(&npc_id) {
            crate::services::gm_soak_commands::emit_logi_soak_fx_for_npc(
                ctx.ballpark,
                mod_type_ids,
                npc_id,
                target_ship_id,
                fx_duration_ms,
                active_parity,
                false,
            );
        }
    }
    Ok(())
}

fn launch_deadwarp_returns_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    returns: Vec<(BallId, CharacterId, DVec3, DVec3, f64)>,
) -> Result<(Vec<BallId>, Vec<BallId>), String> {
    let mut returned_ok = Vec::new();
    let mut failed = Vec::new();
    for (ship_id, char_id, curr_pos, origin, warp_speed) in returns {
        let mode = MovementMode::new_warp(curr_pos, origin, warp_speed);
        if ctx.ballpark.set_mode(ship_id, mode).is_ok() {
            ctx.actor_st
                .warp_state
                .track_player_or_fleet_warp(ship_id, char_id);
            returned_ok.push(ship_id);
        } else {
            ctx.actor_st.warp_state.clear_ball(ship_id, true);
            failed.push(ship_id);
        }
    }
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    Ok((returned_ok, failed))
}

fn step_logi_soak_cycle_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    input: LogiSoakCycleActorInput,
) -> Result<(f64, f64), String> {
    for &nid in &input.eligible_npcs {
        if let Some(b) = ctx.ballpark.balls.get(&nid) {
            if matches!(b.mode, MovementMode::Stop) {
                let _ = ctx.ballpark.set_mode(
                    nid,
                    MovementMode::Orbit {
                        target_id: input.target_ship_id,
                        range: input.orbit_range_m,
                    },
                );
            }
        }
    }

    let mut actual_shield_repaired = 0.0;
    let mut actual_armor_repaired = 0.0;
    let mut damage_ratios: Option<(f64, f64, f64)> = None;
    if let Some(cs) = ctx.ballpark.combat_states.get_mut(&input.target_ship_id) {
        let old_s = cs.shield_hp;
        let old_a = cs.armor_hp;
        cs.shield_hp = (cs.shield_hp + input.shield_total).min(cs.max_shield_hp);
        cs.armor_hp = (cs.armor_hp + input.armor_total).min(cs.max_armor_hp);
        actual_shield_repaired = (cs.shield_hp - old_s).max(0.0);
        actual_armor_repaired = (cs.armor_hp - old_a).max(0.0);
        if actual_shield_repaired > 0.0 || actual_armor_repaired > 0.0 {
            damage_ratios = Some((
                (cs.shield_hp / cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0),
                (cs.armor_hp / cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0),
                (cs.structure_hp / cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0),
            ));
        }
    }
    if let Some((sr, ar, hr)) = damage_ratios {
        ctx.ballpark.emit_event(
            Some(input.target_ship_id),
            DestinyEvent::OnDamageStateChange {
                ball_id: input.target_ship_id,
                shield_ratio: sr,
                armor_ratio: ar,
                structure_ratio: hr,
            },
        );
    }

    for &nid in &input.eligible_npcs {
        crate::services::gm_soak_commands::emit_logi_soak_fx_for_npc(
            ctx.ballpark,
            input.mod_type_ids,
            nid,
            input.target_ship_id,
            input.fx_duration_ms,
            input.fx_parity,
            true,
        );
    }
    ctx.ballpark
        .update_dbuffs(input.target_ship_id, input.wire_dbuffs);
    super::sync_ball_positions(ctx.ballpark, ctx.ball_positions, ctx.scrambled_balls);
    Ok((actual_shield_repaired, actual_armor_repaired))
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
            defer_cmd_reply(exec_ctx, reply, res);
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
            defer_cmd_reply(exec_ctx, reply, res);
            None
        }
        SystemCommand::ClearNpcScramTargets { targets, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = clear_npc_scram_targets_in_actor(&mut ctx, &targets);
            defer_cmd_reply(exec_ctx, reply, res);
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
            defer_barrier_cmd_reply(exec_ctx, reply, res);
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
        SystemCommand::PrepareDisconnectedWarp {
            ship_id,
            warp_speed_au_s,
            current_pos,
            dest,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            prepare_disconnected_warp_in_actor(&mut ctx, ship_id, warp_speed_au_s, current_pos, dest);
            defer_reply(exec_ctx.deferred_replies, reply, ());
            None
        }
        SystemCommand::ReconcileDisconnectedWarp {
            ship_id,
            dest,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = reconcile_disconnected_warp_in_actor(&mut ctx, ship_id, dest);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        cmd @ (SystemCommand::RemoveBall { .. } | SystemCommand::RemoveBallIfToken { .. }) => {
            let (ball_id, required_token, reply) = match cmd {
                SystemCommand::RemoveBall { ball_id, reply } => (ball_id, None, reply),
                SystemCommand::RemoveBallIfToken {
                    ball_id,
                    spawn_token,
                    reply,
                } => (ball_id, Some(spawn_token), reply),
                _ => unreachable!(),
            };
            if let Some(tok) = required_token {
                if exec_ctx.ball_spawn_tokens.get(&ball_id).copied() != Some(tok) {
                    defer_barrier_cmd_reply(exec_ctx, reply, None);
                    return None;
                }
            }
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let removed = remove_ball_in_actor(&mut ctx, ball_id, required_token.is_some());
            defer_barrier_cmd_reply(exec_ctx, reply, removed);
            None
        }
        SystemCommand::AcceptDocking {
            ship_id,
            station_id,
            station_pos,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let removed = accept_docking_in_actor(&mut ctx, ship_id, station_id, station_pos);
            defer_barrier_cmd_reply(exec_ctx, reply, removed);
            None
        }
        SystemCommand::EmitPackagedActions {
            ball_id,
            events,
            reply,
        } => {
            let known = exec_ctx.ballpark.balls.contains_key(&ball_id);
            if known {
                exec_ctx
                    .ballpark
                    .emit_packaged_actions(Some(ball_id), &events);
            }
            defer_reply(exec_ctx.deferred_replies, reply, known);
            None
        }
        SystemCommand::SetSpeedFraction {
            ball_id,
            fraction,
            reply,
        } => {
            let tick_res =
                super::activation_ops::apply_set_speed_fraction_in_actor(exec_ctx, ball_id, fraction);
            defer_reply(exec_ctx.deferred_replies, reply, tick_res);
            None
        }
        SystemCommand::SetBallPosition {
            ball_id,
            position,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let updated = set_ball_position_in_actor(&mut ctx, ball_id, position);
            defer_barrier_cmd_reply(exec_ctx, reply, updated);
            None
        }
        SystemCommand::EmitDestinyEvent {
            ball_id,
            event,
            reply,
        } => {
            let known =
                super::cycles_tackle::apply_destiny_event_in_actor(exec_ctx.ballpark, ball_id, event);
            if known {
                protection::sync_ball_protection_in_actor(
                    exec_ctx.ballpark,
                    exec_ctx.actor_st,
                    ball_id,
                );
            }
            defer_reply(exec_ctx.deferred_replies, reply, known);
            None
        }
        SystemCommand::EmitObserverDestinyEvent {
            observer_ball_id,
            event,
            reply,
        } => {
            let ok = exec_ctx.ballpark.balls.contains_key(&observer_ball_id);
            if ok {
                exec_ctx
                    .ballpark
                    .observer_events
                    .entry(observer_ball_id)
                    .or_default()
                    .push(event);
            }
            defer_reply(exec_ctx.deferred_replies, reply, ok);
            None
        }
        SystemCommand::PushSetState {
            observer_ball_id,
            reply,
        } => {
            let res = if exec_ctx.ballpark.queue_set_state(observer_ball_id) {
                Ok(exec_ctx.ballpark.current_tick)
            } else {
                Err(format!(
                    "Observer ball {observer_ball_id} not in system {}",
                    exec_ctx.solar_system_id
                ))
            };
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::GetBallSnapshot { ball_id, reply } => {
            let res = exec_ctx
                .ballpark
                .balls
                .get(&ball_id)
                .cloned()
                .map(|b| (exec_ctx.ballpark.current_tick, b));
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::GetBallGroupId { ball_id, reply } => {
            let gid = exec_ctx.ballpark.ball_group_id(ball_id);
            defer_reply(exec_ctx.deferred_replies, reply, gid);
            None
        }
        SystemCommand::ListBalls { reply } => {
            let balls: Vec<Ball> = exec_ctx.ballpark.balls.values().cloned().collect();
            defer_reply(exec_ctx.deferred_replies, reply, balls);
            None
        }
        SystemCommand::ListBallsInBubble {
            observer_ball_id,
            dynamic_only,
            reply,
        } => {
            let balls: Vec<Ball> = if dynamic_only {
                exec_ctx
                    .ballpark
                    .dynamic_balls_in_bubble(observer_ball_id)
                    .into_iter()
                    .cloned()
                    .collect()
            } else {
                exec_ctx
                    .ballpark
                    .visible_balls_for_observer(observer_ball_id)
                    .into_iter()
                    .cloned()
                    .collect()
            };
            defer_reply(exec_ctx.deferred_replies, reply, balls);
            None
        }
        SystemCommand::GetBubbleIdForBall { ball_id, reply } => {
            let bid = exec_ctx.ballpark.bubble_id_for_ball(ball_id);
            defer_reply(exec_ctx.deferred_replies, reply, bid);
            None
        }
        SystemCommand::HandoffInSpacePilotObserver {
            from_ball_id,
            to_ball_id,
            ensure_add_to_ball,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = handoff_in_space_pilot_observer_in_actor(
                &mut ctx,
                from_ball_id,
                to_ball_id,
                ensure_add_to_ball,
            );
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::UpdateBallDbuffs {
            ball_id,
            dbuffs,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = update_ball_dbuffs_in_actor(&mut ctx, ball_id, dbuffs);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::EmitLogiSoakStopFx {
            npc_ids,
            target_ship_id,
            mod_type_ids,
            fx_duration_ms,
            active_parity,
            reply,
        } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = emit_logi_soak_stop_fx_in_actor(
                &mut ctx,
                &npc_ids,
                target_ship_id,
                mod_type_ids,
                fx_duration_ms,
                active_parity,
            );
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::LaunchDeadwarpReturns { returns, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = launch_deadwarp_returns_in_actor(&mut ctx, returns);
            defer_reply(exec_ctx.deferred_replies, reply, res);
            None
        }
        SystemCommand::StepLogiSoakCycle { input, reply } => {
            let mut ctx = BallOpsCtx::from_exec_ctx(exec_ctx);
            let res = step_logi_soak_cycle_in_actor(&mut ctx, *input);
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

    /// Transfers pilot observer registration and queued `observer_events` across balls (`#1342`).
    pub async fn handoff_in_space_pilot_observer(
        &self,
        from_ball_id: BallId,
        to_ball_id: BallId,
        ensure_add_to_ball: bool,
    ) -> Result<(), String> {
        call_actor_result_with_deadline(
            self,
            "HandoffInSpacePilotObserver",
            ACTOR_CMD_TIMEOUT,
            |reply| SystemCommand::HandoffInSpacePilotObserver {
                from_ball_id,
                to_ball_id,
                ensure_add_to_ball,
                reply,
            },
        )
        .await
    }

    /// Updates `ball_id`'s wire dbuff state synchronously via the system actor (`#1342`).
    pub fn update_ball_dbuffs_sync(
        &self,
        ball_id: BallId,
        dbuffs: Vec<(i64, f64, i64)>,
    ) -> Result<(), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "UpdateBallDbuffs",
            ACTOR_CMD_TIMEOUT,
            dbuffs,
            |dbuffs, reply| SystemCommand::UpdateBallDbuffs {
                ball_id,
                dbuffs,
                reply,
            },
            |ctx, dbuffs| update_ball_dbuffs_in_actor(ctx, ball_id, dbuffs),
        )
    }

    /// Async mailbox path for [`Self::update_ball_dbuffs_sync`] (`#1342`).
    pub async fn update_ball_dbuffs(
        &self,
        ball_id: BallId,
        dbuffs: Vec<(i64, f64, i64)>,
    ) -> Result<(), String> {
        call_actor_result_with_deadline(self, "UpdateBallDbuffs", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::UpdateBallDbuffs {
                ball_id,
                dbuffs,
                reply,
            }
        })
        .await
    }

    /// Emits deactivation `OnSpecialFX` events for live `/soak logi` NPCs (`#1342`).
    pub async fn emit_logi_soak_stop_fx(
        &self,
        npc_ids: Vec<BallId>,
        target_ship_id: BallId,
        mod_type_ids: [u32; 2],
        fx_duration_ms: f64,
        active_parity: u8,
    ) -> Result<(), String> {
        call_actor_result_with_deadline(self, "EmitLogiSoakStopFx", ACTOR_CMD_TIMEOUT, |reply| {
            SystemCommand::EmitLogiSoakStopFx {
                npc_ids,
                target_ship_id,
                mod_type_ids,
                fx_duration_ms,
                active_parity,
                reply,
            }
        })
        .await
    }

    /// Transitions due `/deadwarp` ships into return warps via the system actor (`#1342`).
    pub fn launch_deadwarp_returns_sync(
        &self,
        returns: Vec<(BallId, CharacterId, DVec3, DVec3, f64)>,
    ) -> Result<(Vec<BallId>, Vec<BallId>), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "LaunchDeadwarpReturns",
            ACTOR_CMD_TIMEOUT,
            returns,
            |returns, reply| SystemCommand::LaunchDeadwarpReturns { returns, reply },
            launch_deadwarp_returns_in_actor,
        )
    }
}

fn spawn_ball_in_actor(ctx: &mut BallOpsCtx<'_>, ball: Ball) -> Result<u64, String> {
    let ball_id = ball.id;
    if let Some(tokens) = ctx.ball_spawn_tokens.as_deref_mut() {
        tokens.remove(&ball_id);
    }
    let was_warping = matches!(
        ctx.ballpark.balls.get(&ball_id).map(|b| &b.mode),
        Some(MovementMode::Warp { .. })
    );
    let is_warp = matches!(ball.mode, MovementMode::Warp { .. });
    let existing_meta = ctx.ballpark.ball_slim_meta.get(&ball_id).cloned();
    if ctx.ballpark.balls.contains_key(&ball_id) {
        let _ = ctx.ballpark.remove_ball(ball_id);
    }
    if let Some(meta) = existing_meta {
        ctx.ballpark.set_ball_slim_meta(ball_id, meta);
    } else {
        super::belts::ensure_default_slim_meta(ctx.ballpark, &ball, &ctx.actor_st.sde_opt);
    }
    if super::belts::should_pin_stationary_ball(&ball, ctx.ballpark.ball_slim_meta.get(&ball_id)) {
        ctx.ballpark.mark_ball_non_free(ball_id, false);
    }
    ctx.ballpark.add_ball(ball);
    if was_warping && !is_warp {
        ctx.actor_st.warp_state.clear_ball(ball_id, true);
    }
    protection::sync_ball_protection_in_actor(ctx.ballpark, ctx.actor_st, ball_id);
    ctx.sync_positions();
    Ok(ctx.ballpark.current_tick)
}

fn spawn_ball_with_combat_meta_in_actor(
    ctx: &mut BallOpsCtx<'_>,
    ball: Ball,
    combat_state: CombatState,
    slim_meta: Option<SlimItemMeta>,
) -> Result<u64, String> {
    let ball_id = ball.id;
    if let Some(tokens) = ctx.ball_spawn_tokens.as_deref_mut() {
        tokens.remove(&ball_id);
    }
    let was_warping = matches!(
        ctx.ballpark.balls.get(&ball_id).map(|b| &b.mode),
        Some(MovementMode::Warp { .. })
    );
    let is_warp = matches!(ball.mode, MovementMode::Warp { .. });
    let deact_guard = ctx.read_deact_hook();
    let tick = super::cycles_tackle::spawn_ball_with_combat_in_actor_collecting(
        ctx.solar_system_id,
        ctx.ballpark,
        &mut ctx.actor_st.active_module_cycles,
        &mut ctx.actor_st.base_scramble_strengths,
        &mut ctx.actor_st.base_core_strengths,
        &ctx.actor_st.sde_opt,
        &ctx.actor_st.db_opt,
        deact_guard.as_ref(),
        ball,
        combat_state,
        slim_meta,
        ctx.post_unlock_ops,
    );
    if was_warping && !is_warp {
        ctx.actor_st.warp_state.clear_ball(ball_id, true);
    }
    protection::sync_ball_protection_in_actor(ctx.ballpark, ctx.actor_st, ball_id);
    ctx.sync_positions();
    Ok(tick)
}

impl SystemHandle {
    /// Synchronously spawns a non-combat `ball` through the bounded actor mailbox (`#1496`).
    pub fn spawn_ball_sync(&self, ball: Ball) -> Result<u64, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "SpawnBall",
            ACTOR_CMD_TIMEOUT,
            ball,
            |ball, reply| SystemCommand::SpawnBall { ball, reply },
            spawn_ball_in_actor,
        )
    }

    /// Synchronously spawns `ball` with `combat_state` through the bounded actor mailbox (`#1496`).
    pub fn spawn_ball_with_combat_sync(
        &self,
        ball: Ball,
        combat_state: CombatState,
    ) -> Result<u64, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "SpawnBallWithCombat",
            ACTOR_CMD_TIMEOUT,
            (ball, combat_state),
            |(ball, combat_state), reply| SystemCommand::SpawnBallWithCombat {
                ball,
                combat_state,
                reply,
            },
            |ctx, (ball, combat_state)| {
                spawn_ball_with_combat_meta_in_actor(ctx, ball, combat_state, None)
            },
        )
    }

    /// Synchronously spawns `ball` with `combat_state` and `slim_meta` through the bounded actor
    /// mailbox (`#1342`).
    pub fn spawn_ball_with_meta_sync(
        &self,
        ball: Ball,
        combat_state: CombatState,
        slim_meta: SlimItemMeta,
    ) -> Result<u64, String> {
        dispatch_actor_sync_with_deadline(
            self,
            "SpawnBallWithMeta",
            ACTOR_CMD_TIMEOUT,
            (ball, combat_state, slim_meta),
            |(ball, combat_state, slim_meta), reply| SystemCommand::SpawnBallWithMeta {
                ball,
                combat_state,
                slim_meta,
                reply,
            },
            |ctx, (ball, combat_state, slim_meta)| {
                spawn_ball_with_combat_meta_in_actor(ctx, ball, combat_state, Some(slim_meta))
            },
        )
    }

    /// Executes one `/soak logi` repair, combat FX, and dbuff update cycle via the actor (`#1342`).
    pub fn step_logi_soak_cycle_sync(
        &self,
        input: LogiSoakCycleActorInput,
    ) -> Result<(f64, f64), String> {
        dispatch_actor_sync_with_deadline(
            self,
            "StepLogiSoakCycle",
            ACTOR_CMD_TIMEOUT,
            input,
            |input, reply| SystemCommand::StepLogiSoakCycle {
                input: Box::new(input),
                reply,
            },
            |ctx, input| step_logi_soak_cycle_in_actor(ctx, input),
        )
    }
}

pub(crate) fn apply_repair_combat_state_in_ballpark(
    bp: &mut Ballpark,
    ball_id: BallId,
    hull_heal: f64,
    armor_heal: f64,
    shield_heal: f64,
) -> Option<eve_destiny::CombatState> {
    let grid_id = bp.ball_grid_ids.get(&ball_id).copied();
    let cs = bp.combat_states.get_mut(&ball_id)?;
    let prev_s = cs.shield_hp;
    let prev_a = cs.armor_hp;
    let prev_h = cs.structure_hp;
    cs.shield_hp = (cs.shield_hp + shield_heal.max(0.0)).min(cs.max_shield_hp);
    cs.armor_hp = (cs.armor_hp + armor_heal.max(0.0)).min(cs.max_armor_hp);
    cs.structure_hp = (cs.structure_hp + hull_heal.max(0.0)).min(cs.max_structure_hp);
    let hp_changed = (cs.shield_hp - prev_s).abs() > 1e-6
        || (cs.armor_hp - prev_a).abs() > 1e-6
        || (cs.structure_hp - prev_h).abs() > 1e-6;
    let cloned = cs.clone();
    if hp_changed {
        let ratio = |cur: f64, max: f64| {
            if max > 0.0 {
                (cur / max).clamp(0.0, 1.0)
            } else {
                1.0
            }
        };
        bp.emit_damage_and_destruction_events(
            ball_id,
            grid_id,
            ratio(cloned.shield_hp, cloned.max_shield_hp),
            ratio(cloned.armor_hp, cloned.max_armor_hp),
            ratio(cloned.structure_hp, cloned.max_structure_hp),
            false,
        );
    }
    Some(cloned)
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
        let tethered_balls = Arc::new(RankedRwLock::new(HashSet::new()));
        let committed_warp_balls = Arc::new(RankedRwLock::new(HashMap::new()));
        let handle = SystemHandle {
            solar_system_id: 30_000_142,
            cmd_tx,
            post_unlock_tx: mpsc::unbounded_channel().0,
            worker_cb_tx: mpsc::unbounded_channel().0,
            ct_driver: None,
            actor_rt_handle: None,
            post_unlock_worker_thread_id: Arc::new(std::sync::OnceLock::new()),
            post_unlock_in_flight: Arc::new(AtomicUsize::new(0)),
            completed_post_unlock_seq: Arc::new(AtomicU64::new(0)),
            fighters_needs_hydration: Arc::new(AtomicBool::new(false)),
            fighter_ctx: Arc::new(Default::default()),
            event_tx,
            ball_positions: Arc::new(celestials::BallPositionSyncCache::new(
                Arc::clone(&committed_warp_balls),
                Arc::clone(&tethered_balls),
            )),
            scrambled_balls: Arc::new(RankedRwLock::new(HashSet::new())),
            tethered_balls,
            committed_warp_balls,
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
            panic_after_command_guards_for_test: Arc::new(AtomicBool::new(false)),
            before_step_reply_hook_for_test: Arc::new(RankedMutex::new(None)),
            npc_death_hook: Arc::new(RankedRwLock::new(None)),
            module_deactivation_hook: Arc::new(RankedRwLock::new(None)),
            module_deactivation_post_unlock_hook: Arc::new(RankedRwLock::new(None)),
            target_lock_completed_hook: Arc::new(RankedRwLock::new(None)),
            system_step_hook: Arc::new(RankedRwLock::new(None)),
            post_unlock_actions: Arc::new(RankedMutex::new(Vec::new())),
            post_unlock_actions_in_flight: Arc::new(AtomicUsize::new(0)),
            post_unlock_fallback_drain_lock: Arc::new(std::sync::Mutex::new(())),
            post_unlock_fallback_owner_tid: Arc::new(std::sync::Mutex::new(None)),
            post_unlock_fallback_scheduled: Arc::new(AtomicBool::new(false)),
            actor_shutting_down: Arc::new(AtomicBool::new(false)),
            published_live: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            cooldown_persist_gate: Arc::new(crate::system_actor::CooldownPersistGate::default()),
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
        assert!(handle
            .remove_ball_sync(1)
            .expect_err("closed actor must fail remove_ball_sync")
            .contains("closed"));
        assert!(handle
            .handoff_in_space_pilot_observer(1, 2, true)
            .await
            .expect_err("closed actor must fail handoff_in_space_pilot_observer")
            .contains("closed"));
        assert!(handle
            .update_ball_dbuffs(1, vec![(10, 15.0, 100)])
            .await
            .expect_err("closed actor must fail update_ball_dbuffs")
            .contains("closed"));
        assert!(handle
            .update_ball_dbuffs_sync(1, vec![(10, 15.0, 100)])
            .expect_err("closed actor must fail update_ball_dbuffs_sync")
            .contains("closed"));
        assert!(handle
            .emit_logi_soak_stop_fx(vec![2], 1, [3_596, 11_357], 2_000.0, 0)
            .await
            .expect_err("closed actor must fail emit_logi_soak_stop_fx")
            .contains("closed"));
        assert!(handle
            .launch_deadwarp_returns_sync(vec![(1, 90_001, DVec3::X, DVec3::ZERO, 3.0)])
            .expect_err("closed actor must fail launch_deadwarp_returns_sync")
            .contains("closed"));
        assert!(handle
            .step_logi_soak_cycle_sync(LogiSoakCycleActorInput {
                eligible_npcs: vec![2],
                target_ship_id: 1,
                orbit_range_m: 12_000.0,
                shield_total: 100.0,
                armor_total: 100.0,
                mod_type_ids: [3_596, 11_357],
                fx_duration_ms: 2_000.0,
                fx_parity: 0,
                wire_dbuffs: vec![],
            })
            .expect_err("closed actor must fail step_logi_soak_cycle_sync")
            .contains("closed"));
    }

    #[tokio::test]
    async fn reconnected_ship_else_branches_clear_stale_active_warps() {
        let reg = crate::system_actor::SystemRegistry::new();
        let handle = reg.get_or_spawn(30_000_142);
        let live_ship_id: BallId = 910_000_001;
        let spawn_ship_id: BallId = 910_000_002;
        let mk_ball = |id: BallId| Ball {
            id,
            type_id: 587,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 350.0,
            inertia: 2.0,
            mass: 1_000_000.0,
            radius: 40.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };

        handle
            .spawn_ball_with_combat(
                mk_ball(live_ship_id),
                eve_destiny::CombatState {
                    is_emergency_warping: true,
                    ..Default::default()
                },
            )
            .await
            .expect("spawn_ball_with_combat must succeed");
        handle
            .test_with_actor_state_mut_async(move |st| {
                st.warp_state.track_player_or_fleet_warp(live_ship_id, 90_001);
                st.warp_state.return_warps.insert(live_ship_id);
                st.warp_state.pending_warp_finished.insert(live_ship_id);
                st.warp_state.track_player_or_fleet_warp(spawn_ship_id, 90_002);
                st.warp_state.return_warps.insert(spawn_ship_id);
                st.warp_state.pending_warp_finished.insert(spawn_ship_id);
            })
            .await;

        let pos = handle
            .reconnect_live_ship_in_space_sync(ReconnectLiveShipRequest {
                ship_id: live_ship_id,
                char_id: 90_001,
                pre_warp_eligible: false,
                pre_warp_pos_opt: None,
                warp_speed: 3.0,
                keep_client_warp: false,
            })
            .expect("reconnect_live_ship_in_space_sync must succeed");
        assert_eq!(pos, Some(DVec3::new(10_000.0, 0.0, 0.0)));
        assert!(!handle.is_active_warp(live_ship_id));
        assert!(!handle.is_return_warp(live_ship_id));
        assert_eq!(handle.warp_provenance(live_ship_id), None);
        assert!(!handle.view().is_pending_warp_finished(live_ship_id));

        handle
            .spawn_reconnected_ship_in_space_sync(SpawnReconnectedShipRequest {
                ball: mk_ball(spawn_ship_id),
                combat_state: eve_destiny::CombatState::default(),
                slim_meta: eve_destiny::SlimItemMeta::default(),
                char_id: 90_002,
                pre_warp_eligible: false,
                pre_warp_pos_opt: None,
                effective_warp_speed: 3.0,
                resume_client_warp: false,
            })
            .expect("spawn_reconnected_ship_in_space_sync must succeed");
        assert!(!handle.is_active_warp(spawn_ship_id));
        assert!(!handle.is_return_warp(spawn_ship_id));
        assert_eq!(handle.warp_provenance(spawn_ship_id), None);
        assert!(!handle.view().is_pending_warp_finished(spawn_ship_id));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn dispatch_actor_sync_never_steals_current_parker_wakeup_in_join() {
        let (handle, mut cmd_rx) = make_test_handle(16);
        let actor_task = tokio::spawn(async move {
            let mut pending_b: Option<oneshot::Sender<Result<bool, String>>> = None;
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    SystemCommand::SetBallTypeId { reply, .. } => pending_b = Some(reply),
                    SystemCommand::ApplyNpcRemoteRepairs { peer_ids, reply, .. } => {
                        if let Some(b_reply) = pending_b.take() {
                            // Wake branch B first while branch A blocks in dispatch_actor_sync.
                            let _ = b_reply.send(Ok(true));
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                        let _ = reply.send(Ok(peer_ids[0] as usize));
                    }
                    _ => {}
                }
            }
        });

        tokio::time::timeout(Duration::from_secs(5), async {
            for i in 0..20_i64 {
                let target_id = 500_000 + i;
                let branch_a = async {
                    tokio::task::yield_now().await; // Poll B first on poll 2 via Rotator<2>.
                    dispatch_actor_sync_reply_with_deadline(
                        &handle, "ProbeSyncNoParkerSteal", Duration::from_secs(2), vec![target_id],
                        |peer_ids, reply| SystemCommand::ApplyNpcRemoteRepairs {
                            peer_ids, shield_rep: 0.0, armor_rep: 0.0, reply,
                        },
                        |_, _| panic!("MultiThread runtime must not use direct_fn"),
                    )
                };
                let branch_b = call_actor_reply_with_deadline(
                    &handle, "ProbeDelayedBranchB", Duration::from_secs(2),
                    |reply| SystemCommand::SetBallTypeId { ball_id: target_id, type_id: 587, reply },
                );
                let (a_res, b_res) = tokio::join!(branch_a, branch_b);
                assert_eq!(a_res.expect("branch A sync reply"), Ok(target_id as usize));
                assert_eq!(b_res.expect("branch B async reply"), Ok(true));
            }
        })
        .await
        .expect("tokio::join! must complete without CURRENT_PARKER lost-wakeup deadlock");

        drop(handle);
        actor_task.await.unwrap();
    }

    #[tokio::test]
    async fn current_thread_dispatch_actor_sync_routes_via_ct_driver_mailbox_never_direct_fn() {
        let reg = Arc::new(crate::system_actor::SystemRegistry::new());
        let live_handle = reg.get_or_spawn(30_000_142);
        assert!(
            live_handle.actor_rt_handle.is_some(),
            "CurrentThread system handle must retain actor_rt_handle"
        );
        let stepped = live_handle
            .step_manual(1.0)
            .await
            .expect("step_manual on live CurrentThread actor");

        let tick = dispatch_actor_sync_reply_with_deadline(
            &live_handle,
            "ProbeCurrentThreadMailbox",
            Duration::from_secs(2),
            (),
            |(), reply| SystemCommand::CurrentTick { reply },
            |_, _| panic!("CurrentThread must route via CtActorDriver mailbox, not direct_fn"),
        )
        .expect("CurrentThread dispatch_actor_sync_reply_with_deadline must succeed via CtActorDriver");
        assert_eq!(tick, stepped.tick);

        let (dummy_handle, _dummy_rx) = make_test_handle(4);
        let unavail_err = dispatch_actor_sync_reply_with_deadline(
            &dummy_handle,
            "ProbeCurrentThreadUnavailable",
            Duration::from_secs(2),
            (),
            |(), reply| SystemCommand::CurrentTick { reply },
            |_, _| panic!("dummy handle without actor_rt_handle must not invoke direct_fn"),
        )
        .expect_err("dummy unpolled handle with actor_rt_handle=None must fail closed");
        assert!(
            unavail_err.contains("actor runtime unavailable"),
            "unexpected unavailable runtime error: {unavail_err}"
        );

        assert!(reg.stop_system(30_000_142).await);
        let closed_err = dispatch_actor_sync_reply_with_deadline(
            &live_handle,
            "ProbeCurrentThreadClosed",
            Duration::from_secs(2),
            (),
            |(), reply| SystemCommand::CurrentTick { reply },
            |_, _| panic!("closed CurrentThread handle must not invoke direct_fn"),
        )
        .expect_err("closed live handle must fail with closed error");
        assert!(
            closed_err.contains("closed"),
            "unexpected closed handle error: {closed_err}"
        );
    }

    #[tokio::test]
    async fn current_thread_dispatch_actor_sync_try_queued_routes_via_ct_driver_mailbox_never_direct_fn() {
        use crate::system_actor::handle_cmds::{
            dispatch_actor_sync_try_queued_with_timeout, TryQueuedReply,
        };

        let reg = Arc::new(crate::system_actor::SystemRegistry::new());
        let live_handle = reg.get_or_spawn(30_000_142);
        let stepped = live_handle
            .step_manual(1.0)
            .await
            .expect("step_manual on live CurrentThread actor");

        match dispatch_actor_sync_try_queued_with_timeout(
            &live_handle,
            Duration::from_secs(2),
            (),
            |(), reply| SystemCommand::CurrentTick { reply },
            |_, _| panic!("CurrentThread try_queued must route via CtActorDriver mailbox, not direct_fn"),
        ) {
            TryQueuedReply::Ready(tick) => assert_eq!(tick, stepped.tick),
            TryQueuedReply::QueuedTimedOut => {
                panic!("live CurrentThread try_queued unexpectedly timed out")
            }
            TryQueuedReply::FullOrClosed => {
                panic!("live CurrentThread try_queued unexpectedly returned FullOrClosed")
            }
        }

        let (dummy_handle, _dummy_rx) = make_test_handle(4);
        assert!(
            matches!(
                dispatch_actor_sync_try_queued_with_timeout(
                    &dummy_handle,
                    Duration::from_secs(2),
                    (),
                    |(), reply| SystemCommand::CurrentTick { reply },
                    |_, _| panic!("dummy handle without actor_rt_handle must not invoke direct_fn"),
                ),
                TryQueuedReply::FullOrClosed
            ),
            "dummy handle with actor_rt_handle=None must fail closed as FullOrClosed"
        );

        assert!(reg.stop_system(30_000_142).await);
        assert!(
            matches!(
                dispatch_actor_sync_try_queued_with_timeout(
                    &live_handle,
                    Duration::from_secs(2),
                    (),
                    |(), reply| SystemCommand::CurrentTick { reply },
                    |_, _| panic!("closed CurrentThread handle must not invoke direct_fn"),
                ),
                TryQueuedReply::FullOrClosed
            ),
            "closed live handle must fail closed as FullOrClosed"
        );
    }
}
