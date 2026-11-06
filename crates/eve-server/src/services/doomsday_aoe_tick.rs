//! `doomsday_aoe_tick` — Persisted 1 Hz tick execution and state management for Titan AoE
//! superweapons (Bosonic Field Generator cone DoT, GTFO delayed subcapital sphere teleport +
//! 60s warp suppression, and Reaper progressive beam sweep) (`feat:modules.doomsday-bosonic-field`,
//! `feat:modules.doomsday-gtfo-reaper`, `plan:D-6`).

#![forbid(unsafe_code)]

use crate::rng::splitmix64_next;
use crate::router::ServerContext;
use crate::services::tactical_modules::is_combatant_ship_or_entity_ball;
use eve_destiny::{BallId, DVec3, DamageVector};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub(crate) const SUPERWEAPON_RUNTIME_BLOB_KEY: &str = "aoe_superweapon_runtime";
pub(crate) const BOSONIC_CONE_COS_HALF_ANGLE: f64 = 0.9659258262890683; // cos(15 deg)
pub(crate) const GTFO_WARP_SUPPRESSION_SECS: u64 = 60;
pub(crate) const GTFO_WARP_SUPPRESSION_STRENGTH: i32 = 25;
pub(crate) const MAX_GTFO_TARGETS: usize = 25;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SuperweaponCycleKind {
    Cone {
        origin_pos: [f64; 3],
        dir_vec: [f64; 3],
        cone_len_m: f64,
        tick_dmg: [f64; 4],
        sig_radius_m: f64,
    },
    SpatialGtfo {
        center: [f64; 3],
        sphere_radius_m: f64,
    },
    ReaperSweep {
        start_pt: [f64; 3],
        end_pt: [f64; 3],
        beam_radius_m: f64,
        sweep_dmg: [f64; 4],
        sig_radius_m: f64,
        already_hit_balls: Vec<i64>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedSuperweaponCycle {
    pub sys_id: u32,
    pub char_id: i64,
    pub ship_id: i64,
    pub ship_type_id: u32,
    pub mod_type_id: u32,
    pub kind: SuperweaponCycleKind,
    pub started_at_unix: u64,
    pub warmup_secs: u64,
    pub total_ticks: usize,
    pub ticks_applied: usize,
    pub immobilize_until_unix: u64,
    pub orig_max_velocity: f64,
    pub orig_warp_scramble_strength: i32,
    pub orig_disallow_assistance: bool,
    pub mobility_restored: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PersistedWarpSuppression {
    pub sys_id: u32,
    pub ship_id: i64,
    pub suppressed_until_unix: u64,
    pub orig_warp_scramble_strength: i32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PersistedSuperweaponRuntime {
    pub active_cycles: Vec<PersistedSuperweaponCycle>,
    pub warp_suppressions: Vec<PersistedWarpSuppression>,
}

pub fn read_superweapon_runtime(
    db: &eve_db::WorldRepository,
) -> Option<PersistedSuperweaponRuntime> {
    db.with_snapshot(|snap| {
        snap.service_state_blobs
            .get(SUPERWEAPON_RUNTIME_BLOB_KEY)
            .and_then(|raw| serde_json::from_str(raw).ok())
    })
}

pub fn mutate_superweapon_runtime<R>(
    db: &eve_db::WorldRepository,
    f: impl FnOnce(&mut PersistedSuperweaponRuntime) -> R,
) -> R {
    db.with_mut_snapshot(|snap| {
        let mut rt: PersistedSuperweaponRuntime = snap
            .service_state_blobs
            .get(SUPERWEAPON_RUNTIME_BLOB_KEY)
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_default();
        let out = f(&mut rt);
        if rt.active_cycles.is_empty() && rt.warp_suppressions.is_empty() {
            snap.service_state_blobs.remove(SUPERWEAPON_RUNTIME_BLOB_KEY);
        } else if let Ok(encoded) = serde_json::to_string(&rt) {
            snap.service_state_blobs
                .insert(SUPERWEAPON_RUNTIME_BLOB_KEY.to_string(), encoded);
        }
        out
    })
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
        )
    }) || v_suspect_or_crim
        || crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.has_limited_engagement_with(vcid, attacker_char_id, now_unix)
        });
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

/// Synchronously reconciles Titan superweapon self-immobility and GTFO 60s warp-drive
/// suppression inside the `system_actor` ballpark tick using persisted `WorldStateSnapshot` state.
pub fn step_bosonic_doomsday_in_ballpark(
    ballpark: &mut eve_destiny::Ballpark,
    base_scramble_strengths: &mut HashMap<BallId, i32>,
    _sde_opt: Option<&std::sync::Arc<eve_sde::SdeRegistry>>,
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    sys_id: u32,
    now_unix: u64,
    _destroyed_balls: &mut Vec<(i64, Option<i64>)>,
) {
    let Some(db) = db_opt else {
        return;
    };
    let has_blob = db.with_snapshot(|s| s.service_state_blobs.contains_key(SUPERWEAPON_RUNTIME_BLOB_KEY));
    if !has_blob {
        return;
    }
    mutate_superweapon_runtime(db, |rt| {
        for cycle in &mut rt.active_cycles {
            if cycle.sys_id != sys_id {
                continue;
            }
            if now_unix >= cycle.immobilize_until_unix && !cycle.mobility_restored {
                if let Some(titan_ball) = ballpark.balls.get_mut(&cycle.ship_id) {
                    titan_ball.max_velocity = cycle.orig_max_velocity;
                }
                base_scramble_strengths.insert(cycle.ship_id, cycle.orig_warp_scramble_strength);
                if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                    cs.warp_scramble_strength = cycle.orig_warp_scramble_strength;
                    cs.disallow_assistance = cycle.orig_disallow_assistance;
                }
                cycle.mobility_restored = true;
            } else if !cycle.mobility_restored {
                if let Some(titan_ball) = ballpark.balls.get_mut(&cycle.ship_id) {
                    titan_ball.max_velocity = 0.0;
                }
                base_scramble_strengths.insert(cycle.ship_id, 100);
                if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                    cs.warp_scramble_strength = cs.warp_scramble_strength.max(100);
                    cs.disallow_assistance = true;
                    cs.is_tethered = false;
                }
            }
        }
        rt.active_cycles
            .retain(|c| !c.mobility_restored || c.ticks_applied < c.total_ticks);

        rt.warp_suppressions.retain(|sup| {
            if sup.sys_id != sys_id {
                return true;
            }
            if now_unix >= sup.suppressed_until_unix {
                base_scramble_strengths.insert(sup.ship_id, sup.orig_warp_scramble_strength);
                if let Some(cs) = ballpark.combat_states.get_mut(&sup.ship_id) {
                    cs.warp_scramble_strength = sup.orig_warp_scramble_strength;
                }
                false
            } else {
                let enforced = sup
                    .orig_warp_scramble_strength
                    .max(GTFO_WARP_SUPPRESSION_STRENGTH);
                base_scramble_strengths.insert(sup.ship_id, enforced);
                if let Some(cs) = ballpark.combat_states.get_mut(&sup.ship_id) {
                    cs.warp_scramble_strength = cs.warp_scramble_strength.max(enforced);
                }
                true
            }
        });
    });
}

/// Advances all persisted AoE superweapon cycles (`Cone`, `SpatialGtfo`, `ReaperSweep`) on the
/// 1 Hz system step after their SDE warmup delay (`attr 2262`).
pub async fn step_bosonic_doomsday_system(ctx: &ServerContext, sys_id: u32) {
    let has_blob = ctx
        .db
        .with_snapshot(|s| s.service_state_blobs.contains_key(SUPERWEAPON_RUNTIME_BLOB_KEY));
    if !has_blob {
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
                let elapsed = now_unix.saturating_sub(cycle.started_at_unix);
                if elapsed >= cycle.warmup_secs && cycle.ticks_applied < cycle.total_ticks {
                    let prev_ticks = cycle.ticks_applied;
                    let target_ticks =
                        ((elapsed - cycle.warmup_secs + 1) as usize).min(cycle.total_ticks);
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

    let sys = ctx.systems.get_or_spawn(sys_id);
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
                            || !is_combatant_ship_or_entity_ball(
                                Some(&ctx.sde),
                                Some(&ctx.db),
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
                            || !is_combatant_ship_or_entity_ball(
                                Some(&ctx.sde),
                                Some(&ctx.db),
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
        let Some(target_char_id) = target_char_opt else {
            continue;
        };
        if crate::services::space_destiny::is_location_change_invulnerable(
            ctx,
            target_char_id,
            now_unix,
        ) {
            continue;
        }
        if let Ok(Some(cs)) = sys.get_combat_state(b.id).await {
            if cs.is_tethered
                || cs.is_invulnerable()
                || cs.undock_invulnerable_remaining_ticks > 0
                || cs.warp_scramble_strength >= 50
                || cs.disallow_assistance
            {
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
        new_suppressions.push(PersistedWarpSuppression {
            sys_id,
            ship_id: b.id,
            suppressed_until_unix: now_unix.saturating_add(GTFO_WARP_SUPPRESSION_SECS),
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

