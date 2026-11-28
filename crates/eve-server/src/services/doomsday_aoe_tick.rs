//! `doomsday_aoe_tick` — Persisted 1 Hz tick execution and state management for Titan AoE
//! superweapons (Bosonic Field Generator cone DoT, GTFO delayed subcapital sphere teleport +
//! 60s warp suppression, and Reaper progressive beam sweep) (`feat:modules.doomsday-bosonic-field`,
//! `feat:modules.doomsday-gtfo-reaper`, `plan:D-6`).

#![forbid(unsafe_code)]

use crate::rng::splitmix64_next;
use crate::router::ServerContext;
use crate::services::tactical_modules::is_combatant_ship_or_entity_ball;
use crate::system_actor::post_unlock::{
    execute_post_unlock_ops_for_standalone, PostUnlockIntent, PostUnlockOps,
};
use eve_db::WorldStateSnapshot;
use eve_destiny::{BallId, DVec3, DamageVector};
use std::collections::HashMap;

pub use eve_db::{
    PersistedSuperweaponCycle, PersistedSuperweaponRuntime, PersistedWarpSuppression,
    SuperweaponCycleKind,
};

pub(crate) const BOSONIC_CONE_COS_HALF_ANGLE: f64 = 0.9659258262890683; // cos(15 deg)
pub(crate) const GTFO_WARP_SUPPRESSION_SECS: u64 = 60;
pub(crate) const GTFO_WARP_SUPPRESSION_STRENGTH: i32 = 25;
pub(crate) const MAX_GTFO_TARGETS: usize = 25;

pub fn read_superweapon_runtime(
    db: &eve_db::WorldRepository,
) -> Option<PersistedSuperweaponRuntime> {
    db.with_snapshot(|snap| {
        let rt = &snap.superweapon_runtime;
        (!rt.active_cycles.is_empty() || !rt.warp_suppressions.is_empty()).then(|| rt.as_inner().clone())
    })
}

pub fn mutate_superweapon_runtime<R>(
    db: &eve_db::WorldRepository,
    f: impl FnOnce(&mut PersistedSuperweaponRuntime) -> R,
) -> R {
    db.with_mut_snapshot(|snap| f(&mut snap.superweapon_runtime))
}

/// Returns `true` strictly for subcapital combat/industrial hulls eligible for GTFO teleportation
/// (excludes capsules `29` and all capital/supercapital/freighter groups `30 | 485 | 513 | 547 | 659 | 883 | 902 | 1538 | 4594`).
pub(crate) fn is_subcapital_ship_for_gtfo(sde: &eve_sde::SdeRegistry, type_id: u32) -> bool {
    sde.types.get(&type_id).is_some_and(|t| {
        t.category_id == 6
            && !matches!(
                t.group_id,
                29 | 30 | 485 | 513 | 547 | 659 | 883 | 902 | 1538 | 4594
            )
            && t.default_attributes.get(&1980).copied().unwrap_or(0.0) <= 0.0
            && t.default_attributes.get(&872).copied().unwrap_or(0.0) <= 0.0
    })
}

async fn resolve_target_signature_radius_m(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    b: &eve_destiny::Ball,
) -> f64 {
    if ctx.db.with_snapshot(|s| s.structures.contains_key(&b.id)) {
        return 10_000.0;
    }
    if let Ok(Some(cs)) = sys.get_combat_state(b.id).await {
        if cs.signature_radius > 0.0 {
            return cs.signature_radius;
        }
    }
    ctx.sde
        .types
        .get(&b.type_id)
        .and_then(|t| t.default_attributes.get(&552).copied())
        .filter(|&sig| sig > 0.0)
        .unwrap_or_else(|| (b.radius * 2.0).max(25.0))
}

pub(crate) fn apply_doomsday_capsuleer_crimewatch(
    ctx: &ServerContext,
    attacker_char_id: i64,
    attacker_ship_id: i64,
    sys_id: u32,
    victim_ball_id: i64,
    victim_type_id: u32,
    now_unix: u64,
) {
    let victim_char_opt = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .find(|c| c.active_ship_id == victim_ball_id && c.character_id != attacker_char_id)
            .map(|c| c.character_id)
    });
    let Some(vcid) = victim_char_opt else {
        return;
    };
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.get_or_create(attacker_char_id).pvp_timer_until_unix = now_unix.saturating_add(900);
        cw.get_or_create(vcid).pvp_timer_until_unix = now_unix.saturating_add(900);
    }
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if sys_sec <= 0.0 {
        return;
    }
    let v_suspect_or_crim = {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let st = cw.get_or_create(vcid);
        st.is_suspect(now_unix) || st.is_criminal(now_unix)
    };
    let is_legal = ctx.db.with_snapshot(|snap| {
        crate::services::combat_crimewatch::is_legal_war_or_suspect_target(
            snap,
            attacker_char_id,
            vcid,
            now_unix,
        ) || crate::services::combat_crimewatch::has_limited_engagement_in_snap(
            snap,
            vcid,
            attacker_char_id,
            now_unix,
        )
    }) || v_suspect_or_crim;
    if is_legal {
        return;
    }
    let is_pod = matches!(victim_type_id, 670 | 33_328);
    let is_new_agg = crate::services::combat_crimewatch::crimewatch_timers::is_new_pvp_aggression(
        ctx,
        attacker_char_id,
        vcid,
        is_pod,
        now_unix,
    );
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        cw.system_corruption_stages.insert(
            sys_id,
            crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, sys_id),
        );
        let _ = cw.handle_combat_aggression_with_pod(
            attacker_char_id,
            vcid,
            attacker_ship_id,
            sys_id,
            sys_sec,
            false,
            is_pod,
            now_unix,
        );
        if sys_sec < 0.45 && is_pod {
            let st = cw.get_or_create(attacker_char_id);
            st.criminal_flag_until_unix = now_unix.saturating_add(900);
            st.suspect_flag_until_unix = 0;
        }
    }
    crate::services::combat_crimewatch::emit_crimewatch_timer_notifications(
        ctx,
        attacker_char_id,
        now_unix,
    );
    crate::services::combat_crimewatch::crimewatch_timers::record_illegal_pvp_penalties_if_new(
        ctx,
        attacker_char_id,
        vcid,
        sys_id,
        sys_sec,
        is_pod,
        is_new_agg,
        now_unix,
    );
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct BosonicDoomsdayRuntimeDbOp {
    pub sys_id: u32,
    pub now_unix: u64,
}

pub(crate) fn apply_bosonic_doomsday_runtime_in_snap(
    snap: &mut WorldStateSnapshot,
    op: &BosonicDoomsdayRuntimeDbOp,
) {
    let rt = &mut snap.superweapon_runtime;
    for cycle in &mut rt.active_cycles {
        if cycle.sys_id == op.sys_id
            && op.now_unix >= cycle.immobilize_until_unix
            && !cycle.mobility_restored
        {
            cycle.mobility_restored = true;
        }
    }
    rt.active_cycles
        .retain(|c| c.sys_id != op.sys_id || !c.mobility_restored || c.ticks_applied < c.total_ticks);
    rt.warp_suppressions
        .retain(|sup| sup.sys_id != op.sys_id || op.now_unix < sup.suppressed_until_unix);
}

/// Synchronously reconciles Titan superweapon self-immobility and GTFO 60s warp-drive
/// suppression inside the `system_actor` ballpark tick, collecting runtime DB updates into `post_unlock` (`#1170`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn step_bosonic_doomsday_in_ballpark_collecting(
    ballpark: &mut eve_destiny::Ballpark,
    base_scramble_strengths: &mut HashMap<BallId, i32>,
    _sde_opt: Option<&std::sync::Arc<eve_sde::SdeRegistry>>,
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    sys_id: u32,
    now_unix: u64,
    _destroyed_balls: &mut Vec<(i64, Option<i64>)>,
    mut restored_shadow: Option<(
        &mut HashMap<(BallId, u64, u32), u64>,
        &mut HashMap<(BallId, u64), u64>,
        u64,
    )>,
    post_unlock: &mut PostUnlockOps,
) {
    let Some(db) = db_opt else {
        return;
    };
    let (sys_cycles, sys_suppressions, needs_db_prune) = db.with_snapshot(|s| {
        let rt = &s.superweapon_runtime;
        if rt.active_cycles.is_empty() && rt.warp_suppressions.is_empty() {
            return (Vec::new(), Vec::new(), false);
        }
        let cycles: Vec<PersistedSuperweaponCycle> = rt
            .active_cycles
            .iter()
            .filter(|c| c.sys_id == sys_id)
            .cloned()
            .collect();
        let sups: Vec<PersistedWarpSuppression> = rt
            .warp_suppressions
            .iter()
            .filter(|sup| sup.sys_id == sys_id)
            .cloned()
            .collect();
        let dirty = cycles.iter().any(|c| {
            (now_unix >= c.immobilize_until_unix && !c.mobility_restored)
                || (c.mobility_restored && c.ticks_applied >= c.total_ticks)
                || (now_unix >= c.immobilize_until_unix && c.ticks_applied >= c.total_ticks)
        }) || sups.iter().any(|sup| now_unix >= sup.suppressed_until_unix);
        (cycles, sups, dirty)
    });
    if sys_cycles.is_empty() && sys_suppressions.is_empty() {
        return;
    }

    for cycle in &sys_cycles {
        let cycle_key = (cycle.ship_id, cycle.started_at_unix, cycle.mod_type_id);
        let already_restored = restored_shadow
            .as_ref()
            .is_some_and(|(cyc_sh, _, _)| cyc_sh.contains_key(&cycle_key));
        if now_unix >= cycle.immobilize_until_unix && !cycle.mobility_restored {
            if already_restored {
                continue;
            }
            if let Some((cyc_sh, _, seq)) = restored_shadow.as_mut() {
                cyc_sh.insert(cycle_key, *seq);
            }
            base_scramble_strengths.insert(cycle.ship_id, cycle.orig_warp_scramble_strength);
            if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                cs.warp_scramble_strength = cycle.orig_warp_scramble_strength;
                cs.disallow_assistance = cycle.orig_disallow_assistance;
                if cs.external_velocity_multiplier == Some(0.0) {
                    cs.external_velocity_multiplier = None;
                }
                if cs.base_max_velocity.is_none() && cycle.orig_max_velocity > 0.0 {
                    cs.base_max_velocity = Some(cycle.orig_max_velocity);
                }
            }
            ballpark.recompute_ball_attributes(cycle.ship_id);
            if ballpark
                .combat_states
                .get(&cycle.ship_id)
                .is_none_or(|cs| cs.base_max_velocity.is_none())
            {
                if let Some(titan_ball) = ballpark.balls.get_mut(&cycle.ship_id) {
                    titan_ball.max_velocity = cycle.orig_max_velocity;
                }
            }
        } else if !cycle.mobility_restored && !already_restored {
            base_scramble_strengths.insert(cycle.ship_id, 100);
            if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                cs.warp_scramble_strength = cs.warp_scramble_strength.max(100);
                cs.disallow_assistance = true;
                cs.is_tethered = false;
                if cs.base_max_velocity.is_none() && cycle.orig_max_velocity > 0.0 {
                    cs.base_max_velocity = Some(cycle.orig_max_velocity);
                }
                cs.external_velocity_multiplier = Some(0.0);
            }
            ballpark.recompute_ball_attributes(cycle.ship_id);
            if let Some(titan_ball) = ballpark.balls.get_mut(&cycle.ship_id) {
                titan_ball.max_velocity = 0.0;
            }
        }
    }

    for sup in &sys_suppressions {
        let sup_key = (sup.ship_id, sup.suppressed_until_unix);
        let already_restored = restored_shadow
            .as_ref()
            .is_some_and(|(_, sup_sh, _)| sup_sh.contains_key(&sup_key));
        if now_unix >= sup.suppressed_until_unix {
            if already_restored {
                continue;
            }
            if let Some((_, sup_sh, seq)) = restored_shadow.as_mut() {
                sup_sh.insert(sup_key, *seq);
            }
            base_scramble_strengths.insert(sup.ship_id, sup.orig_warp_scramble_strength);
            if let Some(cs) = ballpark.combat_states.get_mut(&sup.ship_id) {
                cs.warp_scramble_strength = sup.orig_warp_scramble_strength;
            }
        } else if !already_restored {
            let enforced = sup
                .orig_warp_scramble_strength
                .max(GTFO_WARP_SUPPRESSION_STRENGTH);
            base_scramble_strengths.insert(sup.ship_id, enforced);
            if let Some(cs) = ballpark.combat_states.get_mut(&sup.ship_id) {
                cs.warp_scramble_strength = cs.warp_scramble_strength.max(enforced);
            }
        }
    }

    if needs_db_prune {
        post_unlock.push(PostUnlockIntent::BosonicDoomsdayRuntime(
            BosonicDoomsdayRuntimeDbOp { sys_id, now_unix },
        ));
    }
}

/// Synchronously reconciles Titan superweapon self-immobility and GTFO 60s warp-drive
/// suppression inside the `system_actor` ballpark tick using persisted `WorldStateSnapshot` state.
/// Standalone/test wrapper that executes collected post-unlock ops immediately (`assert_no_actor_locks_before_db_tx`).
pub fn step_bosonic_doomsday_in_ballpark(
    ballpark: &mut eve_destiny::Ballpark,
    base_scramble_strengths: &mut HashMap<BallId, i32>,
    _sde_opt: Option<&std::sync::Arc<eve_sde::SdeRegistry>>,
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    sys_id: u32,
    now_unix: u64,
    _destroyed_balls: &mut Vec<(i64, Option<i64>)>,
) {
    let mut ops = PostUnlockOps::default();
    step_bosonic_doomsday_in_ballpark_collecting(
        ballpark,
        base_scramble_strengths,
        _sde_opt,
        db_opt,
        sys_id,
        now_unix,
        _destroyed_balls,
        None,
        &mut ops,
    );
    execute_post_unlock_ops_for_standalone(
        db_opt.map(|d| &**d),
        db_opt,
        None,
        ops,
        Some(ballpark),
        None,
    );
}

/// Advances all persisted AoE superweapon cycles (`Cone`, `SpatialGtfo`, `ReaperSweep`) on the
/// 1 Hz system step after their SDE warmup delay (`attr 2262`).
/// Scales a base doomsday delay (warmup, immobilize, warp suppression) by active system TiDi (`#1081`).
pub(crate) fn scale_delay_secs_for_tidi(base_secs: u64, tidi: f64) -> u64 {
    let clamped = if tidi.is_finite() {
        tidi.clamp(0.10, 1.0)
    } else {
        1.0
    };
    if clamped < 0.995 && base_secs > 0 {
        ((base_secs as f64) / clamped).ceil().max(1.0) as u64
    } else {
        base_secs
    }
}

pub async fn step_bosonic_doomsday_system(ctx: &ServerContext, sys_id: u32) {
    let has_runtime = ctx.db.with_snapshot(|s| {
        !s.superweapon_runtime.active_cycles.is_empty()
            || !s.superweapon_runtime.warp_suppressions.is_empty()
    });
    if !has_runtime {
        return;
    }
    let now_unix = ctx.now_unix_secs();
    let pending_batches: Vec<(usize, PersistedSuperweaponCycle, usize, usize)> =
        mutate_superweapon_runtime(&ctx.db, |rt| {
            let mut batches = Vec::new();
            for (idx, cycle) in rt.active_cycles.iter_mut().enumerate() {
                if cycle.sys_id != sys_id {
                    continue;
                }
                let tidi = crate::services::serverops_cluster::get_system_tidi_factor(ctx, sys_id)
                    .clamp(0.10, 1.0);
                let elapsed = now_unix.saturating_sub(cycle.started_at_unix);
                if elapsed >= cycle.warmup_secs && cycle.ticks_applied < cycle.total_ticks {
                    let prev_ticks = cycle.ticks_applied;
                    let since_fire = elapsed - cycle.warmup_secs;
                    let sim_ticks = if tidi < 0.995 {
                        (((since_fire as f64) * tidi).floor() as usize).saturating_add(1)
                    } else {
                        (since_fire + 1) as usize
                    };
                    let target_ticks = sim_ticks.min(cycle.total_ticks);
                    if target_ticks > prev_ticks {
                        cycle.ticks_applied = target_ticks;
                        batches.push((idx, cycle.clone(), prev_ticks, target_ticks));
                    }
                }
            }
            batches
        });

    if pending_batches.is_empty() {
        return;
    }

    let Some(sys) = ctx.systems.get_handle(sys_id) else {
        // When a solar system is cold, no caster ship ball can be alive in space, so prune
        // every queued cycle for those ships without waking the system actor.
        let dead_ships: std::collections::HashSet<i64> =
            pending_batches.iter().map(|(_, cycle, _, _)| cycle.ship_id).collect();
        mutate_superweapon_runtime(&ctx.db, |rt| {
            rt.active_cycles.retain(|c| !dead_ships.contains(&c.ship_id));
        });
        return;
    };
    for (_, cycle, prev_ticks, target_ticks) in pending_batches {
        let caster_alive = sys
            .list_balls()
            .await
            .unwrap_or_default()
            .iter()
            .any(|b| b.id == cycle.ship_id);
        if !caster_alive {
            mutate_superweapon_runtime(&ctx.db, |rt| {
                rt.active_cycles.retain(|c| c.ship_id != cycle.ship_id);
            });
            continue;
        }
        match cycle.kind.clone() {
            SuperweaponCycleKind::Cone {
                origin_pos,
                dir_vec,
                cone_len_m,
                tick_dmg,
                sig_radius_m,
            } => {
                let origin = DVec3::from_array(origin_pos);
                let dir = DVec3::from_array(dir_vec);
                let base_dmg =
                    DamageVector::new(tick_dmg[0], tick_dmg[1], tick_dmg[2], tick_dmg[3]);
                for _ in prev_ticks..target_ticks {
                    let balls = sys.list_balls().await.unwrap_or_default();
                    if !balls.iter().any(|b| b.id == cycle.ship_id) {
                        break;
                    }
                    for b in balls {
                        if b.id == cycle.ship_id
                            || !crate::services::combat_crimewatch::combat_missiles_aoe::is_combatant_ship_or_entity_ball(
                                ctx,
                                b.id,
                                b.type_id,
                            )
                        {
                            continue;
                        }
                        let delta = b.position - origin;
                        let dist = delta.length();
                        if dist <= 0.0
                            || dist > cone_len_m
                            || delta.normalize().dot(dir) < BOSONIC_CONE_COS_HALF_ANGLE
                        {
                            continue;
                        }
                        if is_doomsday_target_protected(ctx, &sys, sys_id, &b).await {
                            continue;
                        }
                        let target_sig = resolve_target_signature_radius_m(ctx, &sys, &b).await;
                        let sig_scale = (target_sig / sig_radius_m.max(1.0)).clamp(0.01, 1.0);
                        let scaled_dmg = DamageVector::new(
                            base_dmg.em * sig_scale,
                            base_dmg.thermal * sig_scale,
                            base_dmg.kinetic * sig_scale,
                            base_dmg.explosive * sig_scale,
                        );
                        apply_superweapon_tick_damage(
                            ctx,
                            &sys,
                            sys_id,
                            &cycle,
                            &b,
                            scaled_dmg,
                            1.0,
                            now_unix,
                        )
                        .await;
                    }
                }
            }
            SuperweaponCycleKind::SpatialGtfo {
                center,
                sphere_radius_m,
            } => {
                if prev_ticks == 0 && target_ticks >= 1 {
                    execute_scheduled_gtfo_teleport(
                        ctx,
                        &sys,
                        sys_id,
                        &cycle,
                        DVec3::from_array(center),
                        sphere_radius_m,
                        now_unix,
                    )
                    .await;
                }
            }
            SuperweaponCycleKind::ReaperSweep {
                start_pt,
                end_pt,
                beam_radius_m,
                sweep_dmg,
                sig_radius_m,
                mut already_hit_balls,
            } => {
                let start = DVec3::from_array(start_pt);
                let end = DVec3::from_array(end_pt);
                let base_dmg =
                    DamageVector::new(sweep_dmg[0], sweep_dmg[1], sweep_dmg[2], sweep_dmg[3]);
                let denom = (cycle.total_ticks.max(1)) as f64;
                for tick_idx in prev_ticks..target_ticks {
                    let seg_a = start.lerp(end, (tick_idx as f64) / denom);
                    let seg_b = start.lerp(end, ((tick_idx + 1) as f64) / denom);
                    let seg = seg_b - seg_a;
                    let seg_len_sq = seg.length_squared();
                    let balls = sys.list_balls().await.unwrap_or_default();
                    if !balls.iter().any(|b| b.id == cycle.ship_id) {
                        break;
                    }
                    for b in balls {
                        if b.id == cycle.ship_id
                            || already_hit_balls.contains(&b.id)
                            || !crate::services::combat_crimewatch::combat_missiles_aoe::is_combatant_ship_or_entity_ball(
                                ctx,
                                b.id,
                                b.type_id,
                            )
                        {
                            continue;
                        }
                        let closest = if seg_len_sq > 1e-6 {
                            let t = ((b.position - seg_a).dot(seg) / seg_len_sq).clamp(0.0, 1.0);
                            seg_a + seg * t
                        } else {
                            seg_a
                        };
                        if (b.position - closest).length() <= beam_radius_m {
                            if is_doomsday_target_protected(ctx, &sys, sys_id, &b).await {
                                continue;
                            }
                            already_hit_balls.push(b.id);
                            let target_sig = resolve_target_signature_radius_m(ctx, &sys, &b).await;
                            let sig_scale = (target_sig / sig_radius_m.max(1.0)).clamp(0.01, 1.0);
                            let scaled_dmg = DamageVector::new(
                                base_dmg.em * sig_scale,
                                base_dmg.thermal * sig_scale,
                                base_dmg.kinetic * sig_scale,
                                base_dmg.explosive * sig_scale,
                            );
                            apply_superweapon_tick_damage(
                                ctx,
                                &sys,
                                sys_id,
                                &cycle,
                                &b,
                                scaled_dmg,
                                1.0,
                                now_unix,
                            )
                            .await;
                        }
                    }
                }
                mutate_superweapon_runtime(&ctx.db, |rt| {
                    if let Some(stored) = rt.active_cycles.iter_mut().find(|c| {
                        c.ship_id == cycle.ship_id && c.started_at_unix == cycle.started_at_unix
                    }) {
                        if let SuperweaponCycleKind::ReaperSweep {
                            already_hit_balls: ref mut hit_vec,
                            ..
                        } = stored.kind
                        {
                            *hit_vec = already_hit_balls;
                        }
                    }
                });
            }
        }
    }

    mutate_superweapon_runtime(&ctx.db, |rt| {
        rt.active_cycles
            .retain(|c| !c.mobility_restored || c.ticks_applied < c.total_ticks);
    });
}

async fn is_doomsday_target_protected(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
    b: &eve_destiny::Ball,
) -> bool {
    if let Ok(Some(mut cs)) = sys.get_combat_state(b.id).await {
        cs.sync_action_breaks();
        if cs.blocks_area_effect() {
            return true;
        }
    }
    crate::services::inventory_fitting::fitting_ship_forcefield::is_pos_forcefield_protected(
        ctx,
        sys_id,
        b.id,
        b.position,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn apply_superweapon_tick_damage(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
    cycle: &PersistedSuperweaponCycle,
    b: &eve_destiny::Ball,
    dmg: DamageVector,
    cycle_s: f64,
    now_unix: u64,
) {
    if is_doomsday_target_protected(ctx, sys, sys_id, b).await {
        return;
    }
    let (applied, destroyed) =
        crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
            ctx,
            sys,
            sys_id,
            cycle.char_id,
            b.id,
            dmg,
            cycle_s,
            now_unix,
        )
        .await
        .ok()
        .flatten()
        .map(|(a, d, _)| (a, d))
        .unwrap_or((0.0, false));
    if applied > 0.0 {
        crate::services::combat_crimewatch::record_killmail_assist_action(
            ctx,
            b.id,
            cycle.char_id,
            cycle.ship_type_id,
            cycle.mod_type_id,
            applied,
            false,
            now_unix,
        );
        apply_doomsday_capsuleer_crimewatch(
            ctx,
            cycle.char_id,
            cycle.ship_id,
            sys_id,
            b.id,
            b.type_id,
            now_unix,
        );
    }
    if destroyed {
        crate::services::combat_crimewatch::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
            ctx,
            sys_id,
            cycle.char_id,
            b.id,
            b.type_id,
            b.position,
            now_unix,
        )
        .await;
    }
}

async fn execute_scheduled_gtfo_teleport(
    ctx: &ServerContext,
    sys: &crate::system_actor::SystemHandle,
    sys_id: u32,
    cycle: &PersistedSuperweaponCycle,
    center: DVec3,
    sphere_radius_m: f64,
    now_unix: u64,
) {
    let mut all_balls = sys.list_balls().await.unwrap_or_default();
    all_balls.sort_by_key(|b| b.id);

    let mut candidate_anchors: Vec<DVec3> = ctx
        .sde
        .celestials_in_system(sys_id)
        .iter()
        .map(|c| c.position)
        .filter(|p| p.distance(center) > 500_000.0)
        .collect();
    if candidate_anchors.is_empty() {
        candidate_anchors = all_balls
            .iter()
            .filter(|b| {
                !is_combatant_ship_or_entity_ball(Some(&ctx.sde), Some(&ctx.db), b.id, b.type_id)
                    && b.position.distance(center) > 500_000.0
            })
            .map(|b| b.position)
            .collect();
    }

    let mut teleported_count = 0usize;
    let mut new_suppressions = Vec::new();
    for (idx, b) in all_balls.iter().enumerate() {
        if teleported_count >= MAX_GTFO_TARGETS {
            break;
        }
        if b.id == cycle.ship_id
            || b.max_velocity <= 0.0
            || !is_subcapital_ship_for_gtfo(&ctx.sde, b.type_id)
        {
            continue;
        }
        let target_char_opt = ctx.db.with_snapshot(|snap| {
            snap.characters
                .values()
                .find(|c| {
                    c.active_ship_id == b.id && c.station_id.is_none() && c.structure_id.is_none()
                })
                .map(|c| c.character_id)
        });
        if target_char_opt.is_none() {
            continue;
        }
        if is_doomsday_target_protected(ctx, sys, sys_id, b).await {
            continue;
        }
        if let Ok(Some(cs)) = sys.get_combat_state(b.id).await {
            if cs.warp_scramble_strength >= 50 || cs.disallow_assistance {
                continue;
            }
        }
        if (b.position - center).length() > sphere_radius_m {
            continue;
        }

        let mut fallback_seed =
            (b.id as u64) ^ ((idx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)) ^ now_unix;
        let h = ctx
            .mechanic_rng
            .try_next_u64()
            .unwrap_or_else(|| splitmix64_next(&mut fallback_seed));
        let dest = if !candidate_anchors.is_empty() {
            let anchor = candidate_anchors[(h as usize) % candidate_anchors.len()];
            let angle = ((h >> 16) as f64) * 0.001;
            let jitter = 15_000.0 + ((h >> 32) % 30_000) as f64;
            anchor + DVec3::new(angle.cos() * jitter, angle.sin() * jitter, 5_000.0)
        } else {
            let angle = ((h >> 8) as f64) * 0.001;
            let dist = 1_500_000.0 + ((h >> 24) % 25_000_000) as f64;
            center
                + DVec3::new(
                    angle.cos() * dist,
                    angle.sin() * dist,
                    ((h % 1_000_000) as f64) - 500_000.0,
                )
        };
        let _ = sys.set_ball_position(b.id, dest).await;
        let _ = sys
            .set_movement_mode(b.id, eve_destiny::MovementMode::Stop)
            .await;
        let _ = sys
            .set_warp_scramble_strength(b.id, GTFO_WARP_SUPPRESSION_STRENGTH)
            .await;
        let tidi = crate::services::serverops_cluster::get_system_tidi_factor(ctx, sys_id);
        let sup_secs = scale_delay_secs_for_tidi(GTFO_WARP_SUPPRESSION_SECS, tidi);
        new_suppressions.push(PersistedWarpSuppression {
            sys_id,
            ship_id: b.id,
            suppressed_until_unix: now_unix.saturating_add(sup_secs),
            orig_warp_scramble_strength: 0,
        });
        apply_doomsday_capsuleer_crimewatch(
            ctx,
            cycle.char_id,
            cycle.ship_id,
            sys_id,
            b.id,
            b.type_id,
            now_unix,
        );
        teleported_count += 1;
    }

    if !new_suppressions.is_empty() {
        mutate_superweapon_runtime(&ctx.db, |rt| {
            for sup in new_suppressions {
                if let Some(existing) = rt
                    .warp_suppressions
                    .iter_mut()
                    .find(|s| s.sys_id == sup.sys_id && s.ship_id == sup.ship_id)
                {
                    existing.suppressed_until_unix =
                        existing.suppressed_until_unix.max(sup.suppressed_until_unix);
                } else {
                    rt.warp_suppressions.push(sup);
                }
            }
        });
    }
}

#[cfg(test)]
#[path = "doomsday_aoe_tick_tests.rs"]
mod doomsday_aoe_tick_tests;

