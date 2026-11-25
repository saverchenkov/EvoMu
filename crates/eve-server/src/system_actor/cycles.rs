//! Module cycle simulation, thermodynamics, RAH adaptation, and tackle reconciliation for `system_actor`.

#![forbid(unsafe_code)]

use crate::router::notify_registry::{
    build_pending_typed_notification_in_ballpark, BallparkScene, FailLockTarget,
    OnCapacitorChange, OnChargeBeingLoadedToModule, OnDamageMessage, OnDamageMessages,
    OnDamageStateChange, OnModuleAttributeChanges, OnMultiEvent, OnRemoteMessage, OnTarget,
    TypedNotification,
};
use crate::system_actor::*;
#[cfg(test)]
#[allow(unused_imports)]
pub(crate) use super::post_unlock_test_shims::{
    apply_weapon_cycle_damage, cleanup_removed_module_cycle, emit_cycle_deactivation,
    remove_all_ship_module_cycles, remove_all_ship_module_cycles_preserving_own_db,
    remove_non_warp_ship_module_cycles, restore_taken_own_cycles_in_actor,
    step_cycles_and_capacitor, step_cycles_and_capacitor_with_epochs,
    step_cycles_and_capacitor_with_missiles_and_tidi,
    teardown_attacker_locks_and_cycles_for_destroyed_balls,
};
use eve_db::lock_rank::AnyRwLock;
use eve_destiny::{BallId, Ballpark, MovementMode};
use glam::DVec3;
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) fn step_rah_resonances(current_resonances: [f64; 4], dmg: [f64; 4], shift_pct: f64) -> [f64; 4] {
    // current_resonances is [em (267), exp (268), kin (269), therm (270)]
    // dmg is [em, therm, kin, exp] -> map to [em, exp, kin, therm]
    let d = [dmg[0].max(0.0), dmg[3].max(0.0), dmg[2].max(0.0), dmg[1].max(0.0)];
    let total_dmg: f64 = d.iter().sum();
    let d = if total_dmg > 1e-9 {
        d
    } else {
        [1.0, 0.0, 0.0, 0.0]
    };
    let total_d: f64 = d.iter().sum();
    let shares = [d[0] / total_d, d[1] / total_d, d[2] / total_d, d[3] / total_d];

    let mut res = [
        (1.0 - current_resonances[0]).clamp(0.0, 0.60),
        (1.0 - current_resonances[1]).clamp(0.0, 0.60),
        (1.0 - current_resonances[2]).clamp(0.0, 0.60),
        (1.0 - current_resonances[3]).clamp(0.0, 0.60),
    ];
    let shift = (shift_pct / 100.0).clamp(0.01, 0.20);
    let donors: Vec<usize> = (0..4).filter(|&i| shares[i] < 1e-6 && res[i] > 1e-6).collect();
    let donors = if donors.is_empty() {
        (0..4).filter(|&i| shares[i] < (res[i] / 0.60) && res[i] > 1e-6).collect::<Vec<usize>>()
    } else {
        donors
    };
    if donors.is_empty() {
        return current_resonances;
    }
    let per_donor = shift / (donors.len() as f64);
    let mut taken = 0.0;
    for &i in &donors {
        let take = res[i].min(per_donor);
        res[i] -= take;
        taken += take;
    }
    for i in 0..4 {
        if shares[i] > 1e-6 {
            res[i] = (res[i] + taken * shares[i]).min(0.60);
        }
    }
    [
        (1.0 - res[0]).clamp(0.40, 1.0),
        (1.0 - res[1]).clamp(0.40, 1.0),
        (1.0 - res[2]).clamp(0.40, 1.0),
        (1.0 - res[3]).clamp(0.40, 1.0),
    ]
}

pub fn on_ball_destroyed_by_damage(
    solar_system_id: SolarSystemId,
    ball_id: BallId,
    attacker_id: Option<CharacterId>,
    hook: Option<&NpcDeathHook>,
) -> Option<super::NpcDeathFuture> {
    hook.map(|h| h(solar_system_id, ball_id, attacker_id))
}

pub(crate) fn is_capital_remote_repair_module(
    module_type_id: u32,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
) -> bool {
    sde_opt
        .as_ref()
        .and_then(|sde| sde.types.get(&module_type_id))
        .is_some_and(|t| {
            let req1 = t.default_attributes.get(&182).copied().unwrap_or(0.0) as u32;
            let pwr = t.default_attributes.get(&30).copied().unwrap_or(0.0);
            matches!(req1, 24568 | 24571 | 27936) || pwr >= 10_000.0
        })
}

#[cfg(test)]
pub(crate) fn sync_cycle_from_fit_projection(
    cycle: &mut ActiveModuleCycle,
    fit_projections: &HashMap<BallId, eve_dogma::FitProjection>,
) {
    let _ = sync_cycle_from_fit_projection_with_shield_boost_mult(cycle, fit_projections, 1.0);
}

pub(crate) fn sync_cycle_from_fit_projection_with_shield_boost_mult(
    cycle: &mut ActiveModuleCycle,
    fit_projections: &HashMap<BallId, eve_dogma::FitProjection>,
    shield_boost_multiplier: f64,
) -> bool {
    let Some(proj) = fit_projections.get(&cycle.ship_id) else {
        return false;
    };
    let Some(cp) = proj.modules.get(&cycle.module_id) else {
        if let Some(prop) = proj.propulsion.as_ref() {
            if prop.module_id == cycle.module_id && prop.is_overloaded == cycle.is_overloaded {
                if prop.speed_boost > 0.0 && prop.speed_boost.is_finite() {
                    cycle.speed_boost_factor = prop.speed_boost;
                }
                if prop.is_mwd
                    && prop.signature_radius_bonus_pct > 0.0
                    && prop.signature_radius_bonus_pct.is_finite()
                {
                    cycle.hic_sig_multiplier = Some(1.0 + prop.signature_radius_bonus_pct / 100.0);
                }
                if prop.mass_addition > 0.0 && prop.mass_addition.is_finite() {
                    cycle.hic_mass_addition = Some(prop.mass_addition);
                }
                return true;
            }
        }
        return false;
    };
    if cp.is_overloaded != cycle.is_overloaded {
        return false;
    }
    if !cycle.grants_interdiction_nullification && cp.duration_ms > 0.0 && cp.duration_ms.is_finite()
    {
        let mult = if cycle.shield_boost_amount > 0.0
            && shield_boost_multiplier > 0.0
            && shield_boost_multiplier.is_finite()
        {
            shield_boost_multiplier
        } else {
            1.0
        };
        cycle.duration_s = ((cp.duration_ms / 1000.0) * mult).max(0.05);
    }
    if !cycle.is_asb && cp.capacitor_need >= 0.0 && cp.capacitor_need.is_finite() {
        cycle.cap_need_per_cycle = cp.capacitor_need;
    }
    let bank_mult = (cycle.linked_turrets.len().max(1)) as f64;
    if cp.is_turret {
        if cp.optimal_range_m > 0.0 {
            cycle.weapon_optimal_m = cp.optimal_range_m;
        }
        cycle.weapon_falloff_m = cp.falloff_m.max(0.0);
        if cp.tracking_speed > 0.0 {
            cycle.weapon_tracking_speed = cp.tracking_speed;
        }
        if cp.sig_resolution_m > 0.0 {
            cycle.weapon_sig_resolution_m = cp.sig_resolution_m;
        }
        let dmg_sum: f64 = cp.damage.iter().sum();
        if dmg_sum > 0.0 && dmg_sum.is_finite() {
            cycle.damage_vector = eve_destiny::DamageVector::new(
                cp.damage[0] * bank_mult,
                cp.damage[1] * bank_mult,
                cp.damage[2] * bank_mult,
                cp.damage[3] * bank_mult,
            );
        }
    } else if cp.is_missile {
        if cp.optimal_range_m > 0.0 {
            cycle.weapon_optimal_m = cp.optimal_range_m;
        }
        if cp.missile_explosion_radius_m > 0.0 {
            cycle.missile_explosion_radius_m = cp.missile_explosion_radius_m;
        }
        if cp.missile_explosion_velocity_ms > 0.0 {
            cycle.missile_explosion_velocity_ms = cp.missile_explosion_velocity_ms;
        }
        if cp.missile_drf > 0.0 {
            cycle.missile_drf = cp.missile_drf;
        }
        let dmg_sum: f64 = cp.damage.iter().sum();
        if dmg_sum > 0.0 && dmg_sum.is_finite() {
            cycle.damage_vector = eve_destiny::DamageVector::new(
                cp.damage[0] * bank_mult,
                cp.damage[1] * bank_mult,
                cp.damage[2] * bank_mult,
                cp.damage[3] * bank_mult,
            );
        }
    }
    let matching_prop_speed_boost = proj
        .propulsion
        .as_ref()
        .filter(|prop| {
            prop.module_id == cycle.module_id
                && prop.is_overloaded == cycle.is_overloaded
                && prop.speed_boost > 0.0
                && prop.speed_boost.is_finite()
        })
        .map(|prop| prop.speed_boost);
    if let Some(speed_boost) = matching_prop_speed_boost {
        cycle.speed_boost_factor = speed_boost;
    } else if cp.prop_speed_boost > 0.0 && cp.prop_speed_boost.is_finite() {
        cycle.speed_boost_factor = cp.prop_speed_boost;
    }
    if cp.is_cloak
        && cycle.effect_name.starts_with("cloaking")
        && cp.cloak_velocity_bonus_pct.is_finite()
        && cp.cloak_velocity_bonus_pct < 0.0
    {
        cycle.speed_boost_factor = cp.cloak_velocity_bonus_pct.max(-1.0);
    }
    if cp.is_mwd && cp.sig_radius_bonus_pct > 0.0 && cp.sig_radius_bonus_pct.is_finite() {
        cycle.hic_sig_multiplier = Some(1.0 + cp.sig_radius_bonus_pct / 100.0);
    }
    if (cp.is_mwd || cp.is_afterburner)
        && cp.mass_addition > 0.0
        && cp.mass_addition.is_finite()
    {
        cycle.hic_mass_addition = Some(cp.mass_addition);
    }
    if cycle.shield_boost_amount > 0.0
        && cp.shield_boost_amount > 0.0
        && cp.shield_boost_amount.is_finite()
    {
        cycle.shield_boost_amount = cp.shield_boost_amount;
    }
    if !cycle.is_aar
        && cycle.armor_repair_amount > 0.0
        && cp.armor_repair_amount > 0.0
        && cp.armor_repair_amount.is_finite()
    {
        cycle.armor_repair_amount = cp.armor_repair_amount;
    }
    if cycle.hull_repair_amount > 0.0
        && cp.hull_repair_amount > 0.0
        && cp.hull_repair_amount.is_finite()
    {
        cycle.hull_repair_amount = cp.hull_repair_amount;
    }
    true
}

pub(crate) fn compute_missile_effective_damage(
    base_damage: eve_destiny::DamageVector,
    explosion_radius_m: f64,
    explosion_velocity_ms: f64,
    drf: f64,
    target_id: BallId,
    ballpark: &Ballpark,
) -> eve_destiny::DamageVector {
    let Some(tb) = ballpark.balls.get(&target_id) else {
        return eve_destiny::DamageVector::default();
    };
    let target_sig = ballpark
        .combat_states
        .get(&target_id)
        .map(|cs| {
            if cs.signature_radius > 0.0 {
                cs.signature_radius
            } else {
                cs.base_signature_radius.unwrap_or(0.0)
            }
        })
        .filter(|&s| s > 0.0)
        .unwrap_or_else(|| tb.radius.max(35.0));
    let target_speed = if tb.velocity.length() > 0.0 {
        tb.velocity.length()
    } else {
        tb.max_velocity * tb.speed_fraction
    };
    let exp_rad = if explosion_radius_m > 0.0 { explosion_radius_m } else { 40.0 };
    let exp_vel = if explosion_velocity_ms > 0.0 { explosion_velocity_ms } else { 170.0 };
    let drf_eff = if drf > 0.0 { drf } else { 0.604 };
    let factor = crate::formulas::missile_damage_factor(
        target_sig,
        target_speed,
        exp_rad,
        exp_vel,
        drf_eff,
    );
    base_damage.scaled(factor)
}

fn resolve_cycle_combat_char_ids(
    ballpark: &Ballpark,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ship_id: BallId,
    owner_char_id: Option<CharacterId>,
    target_id: BallId,
) -> (Option<CharacterId>, Option<CharacterId>) {
    // Why: Never fall back to `m.owner_id` or unfiltered `items.owner_id`; NPC corp/faction IDs
    // (e.g. CONCORD 1_000_125 or faction 500_001) are not characters and would accumulate
    // undrained `pending_notifications` (#1247).
    let resolve_ball_char = |ball_id: BallId| {
        ballpark
            .ball_slim_meta
            .get(&ball_id)
            .and_then(|m| m.char_id)
            .filter(|&cid| cid > 0)
            .or_else(|| {
                db_opt.as_ref().and_then(|db| {
                    db.with_snapshot(|s| {
                        s.characters
                            .values()
                            .find(|c| c.active_ship_id == ball_id)
                            .map(|c| c.character_id)
                            .or_else(|| {
                                s.items
                                    .get(&ball_id)
                                    .map(|it| it.owner_id)
                                    .filter(|oid| *oid > 0 && s.characters.contains_key(oid))
                            })
                    })
                })
            })
            .filter(|&cid| cid > 0)
    };
    let attacker_char_id = owner_char_id
        .filter(|&cid| cid > 0)
        .or_else(|| resolve_ball_char(ship_id));
    let target_char_id = resolve_ball_char(target_id).filter(|&cid| Some(cid) != attacker_char_id);
    (attacker_char_id, target_char_id)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_resolved_weapon_damage(
    ship_id: BallId,
    owner_char_id: Option<CharacterId>,
    module_type_id: u32,
    target_id: BallId,
    effective_damage: eve_destiny::DamageVector,
    ballpark: &mut Ballpark,
    rah_incoming_damage: &mut HashMap<BallId, [f64; 4]>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    pending_cycle_actions: &mut Vec<PendingCycleAction>,
    destroyed_balls: &mut Vec<(BallId, Option<CharacterId>)>,
    pending_destroyed_outbox: Option<&PendingDestroyedOutbox>,
) {
    if effective_damage.total() <= 0.0 {
        // Why: Turret/weapon shots that miss still emit combat-log miss messages (#1247).
        if ballpark.balls.contains_key(&target_id) {
            let (attacker_char_id, target_char_id) =
                resolve_cycle_combat_char_ids(ballpark, db_opt, ship_id, owner_char_id, target_id);
            if attacker_char_id.is_some_and(|c| c > 0) || target_char_id.is_some_and(|c| c > 0) {
                pending_cycle_actions.push(super::PendingCycleAction::CycleWeaponMiss {
                    attacker_char_id,
                    attacker_ship_id: ship_id,
                    target_id,
                    target_char_id,
                    module_type_id,
                    notified: false,
                });
            }
        }
        return;
    }
    let now_unix = db_opt.as_ref().map_or(0, |db| db.clock().now_unix_secs());
    let sys_id = ballpark.solar_system_id;
    let dmg_res = crate::services::mobile_deployables::handle_ball_damage_in_actor(
        db_opt,
        sde_opt,
        sys_id,
        target_id,
        effective_damage,
        ballpark,
        now_unix,
    )
    .or_else(|| ballpark.apply_damage_to_ball(target_id, effective_damage));
    let entry = rah_incoming_damage.entry(target_id).or_insert([0.0; 4]);
    entry[0] += effective_damage.em;
    entry[1] += effective_damage.thermal;
    entry[2] += effective_damage.kinetic;
    entry[3] += effective_damage.explosive;
    if let Some((applied_dmg, is_destroyed, _)) = dmg_res {
        let (attacker_char_id, target_char_id) =
            resolve_cycle_combat_char_ids(ballpark, db_opt, ship_id, owner_char_id, target_id);
        let attacker_ship_type_id = ballpark
            .balls
            .get(&ship_id)
            .map(|b| b.type_id)
            .unwrap_or(0);
        if applied_dmg > 0.0
            && (attacker_char_id.is_some_and(|c| c > 0) || target_char_id.is_some_and(|c| c > 0))
        {
            pending_cycle_actions.push(super::PendingCycleAction::CycleWeaponDamage {
                attacker_char_id,
                attacker_ship_id: ship_id,
                attacker_ship_type_id,
                target_id,
                target_char_id,
                module_type_id,
                applied_dmg,
                notified: false,
            });
        }
        if is_destroyed {
            destroyed_balls.push((target_id, attacker_char_id));
            if let Some(outbox) = pending_destroyed_outbox {
                outbox.record_one(target_id, attacker_char_id);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_weapon_cycle_damage_collecting(
    solar_system_id: SolarSystemId,
    cycle: &ActiveModuleCycle,
    ballpark: &mut Ballpark,
    rah_incoming_damage: &mut HashMap<BallId, [f64; 4]>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    pending_cycle_actions: &mut Vec<PendingCycleAction>,
    destroyed_balls: &mut Vec<(BallId, Option<CharacterId>)>,
    pending_destroyed_outbox: Option<&PendingDestroyedOutbox>,
    post_unlock: &mut super::post_unlock::PostUnlockOps,
) {
    let Some(target_id) = cycle.target_id else {
        return;
    };
    if cycle.damage_vector.total() <= 0.0 {
        return;
    }
    let effective_damage = if cycle.weapon_is_turret {
        if let (Some(sb), Some(tb)) = (
            ballpark.balls.get(&cycle.ship_id),
            ballpark.balls.get(&target_id),
        ) {
            // Why: EVE turret optimal/falloff are surface-to-surface; center-to-center fails on large ships like Titans (#1247).
            let dist = sb.surface_distance(tb);
            // Why: Out-of-range turrets must not roll wrecking hits or emit spurious miss logs (#1247).
            if cycle.max_effective_range_m.is_some_and(|max_r| dist > max_r) {
                return;
            }
            let angular_vel = eve_destiny::combat::compute_angular_velocity(
                sb.position,
                sb.velocity,
                tb.position,
                tb.velocity,
            );
            let attacker_pylon_mult = ballpark
                .combat_states
                .get(&cycle.ship_id)
                .map(|cs| cs.tracking_speed_multiplier)
                .filter(|&m| m > 0.0)
                .unwrap_or(1.0);
            let tracking_speed = cycle.weapon_tracking_speed.max(1e-6) * attacker_pylon_mult;
            let sig_res = if cycle.weapon_sig_resolution_m > 0.0 {
                cycle.weapon_sig_resolution_m
            } else {
                40_000.0
            };
            let target_sig = ballpark
                .combat_states
                .get(&target_id)
                .map(|cs| {
                    if cs.signature_radius > 0.0 {
                        cs.signature_radius
                    } else {
                        cs.base_signature_radius.unwrap_or(0.0)
                    }
                })
                .filter(|&s| s > 0.0)
                .unwrap_or_else(|| tb.radius.max(35.0));
            let p_hit = crate::formulas::turret_chance_to_hit_with_res(
                angular_vel,
                tracking_speed,
                sig_res,
                target_sig,
                dist,
                cycle.weapon_optimal_m.max(1.0),
                cycle.weapon_falloff_m.max(1.0),
            );
            let mut roll_seed = ((cycle.ship_id ^ cycle.module_id) as u64)
                .wrapping_add((target_id as u64).wrapping_mul(0x517c_c1b7_2722_0a95))
                .wrapping_add(ballpark.current_tick);
            let hit_roll = crate::rng::u64_to_unit_f64(
                crate::rng::splitmix64_next(&mut roll_seed),
            );
            crate::formulas::turret_shot_damage(cycle.damage_vector, p_hit, hit_roll).0
        } else {
            eve_destiny::DamageVector::default()
        }
    } else if cycle.weapon_is_missile {
        if let Some(tb) = ballpark.balls.get(&target_id) {
            let out_of_range = ballpark.balls.get(&cycle.ship_id).is_some_and(|sb| {
                // Why: Both guided missiles and unguided `weapon_is_missile` (e.g. Vorton Projector, group 4060) measure surface distance (#1247).
                let surface_dist = sb.surface_distance(tb);
                (cycle.weapon_optimal_m > 0.0 && surface_dist > cycle.weapon_optimal_m)
                    || cycle
                        .max_effective_range_m
                        .is_some_and(|max_r| surface_dist > max_r)
            });
            if out_of_range {
                // Why: Out-of-range missiles/Vortons must return early without emitting spurious `CycleWeaponMiss` notifications (#1247).
                return;
            }
            compute_missile_effective_damage(
                cycle.damage_vector,
                cycle.missile_explosion_radius_m,
                cycle.missile_explosion_velocity_ms,
                cycle.missile_drf,
                target_id,
                ballpark,
            )
        } else {
            return;
        }
    } else {
        cycle.damage_vector
    };
    apply_resolved_weapon_damage(
        cycle.ship_id,
        cycle.owner_char_id,
        cycle.module_type_id,
        target_id,
        effective_damage,
        ballpark,
        rah_incoming_damage,
        sde_opt,
        db_opt,
        pending_cycle_actions,
        destroyed_balls,
        pending_destroyed_outbox,
    );
    if effective_damage.total() > 0.0 && db_opt.is_some() {
        if let Some(cs) = ballpark.combat_states.get(&target_id) {
            post_unlock.push_ship_combat_hp(super::post_unlock::CombatHpDbUpdate {
                ball_id: target_id,
                combat_state: cs.clone(),
                expected_system: Some(solar_system_id),
                require_persisted_entity: true,
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn step_cycles_and_capacitor_with_missiles_and_tidi_collecting(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    pending_anchoring_disruptors: &mut Vec<PendingAnchoringDisruptor>,
    active_nanite_repairs: &mut Vec<ActiveNaniteRepair>,
    ship_rack_heat: &mut HashMap<BallId, [f64; 3]>,
    rah_incoming_damage: &mut HashMap<BallId, [f64; 4]>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ball_space_entry_epochs: &HashMap<BallId, u64>,
    fit_projections: &HashMap<BallId, eve_dogma::FitProjection>,
    weapon_cycle_start_modules: &mut std::collections::HashSet<(BallId, i64)>,
    pending_missile_launches: &mut Vec<super::PendingMissileLaunch>,
    mechanic_rng_opt: &Option<Arc<crate::rng::MechanicRng>>,
    tactical_state: &dyn AnyRwLock<crate::services::tactical_modules::TacticalModulesState>,
    pending_cycle_actions: &mut Vec<PendingCycleAction>,
    sim_elapsed_secs: f64,
    dt: f64,
    tidi_factor: f64,
    mut reload_sim_remaining: Option<&mut HashMap<i64, ModuleReloadSimEntry>>,
    deactivation_hook: Option<&ModuleDeactivationHook>,
    pending_destroyed_outbox: Option<&PendingDestroyedOutbox>,
    post_unlock: &mut super::post_unlock::PostUnlockOps,
) -> Vec<(BallId, Option<CharacterId>)> {
    let tidi_factor = if tidi_factor.is_finite() {
        tidi_factor.clamp(0.10, 1.0)
    } else {
        1.0
    };
    let mut destroyed_balls = Vec::new();
    let mut virtual_paste_used: HashMap<BallId, i32> = HashMap::new();
    let mut virtual_item_qty: HashMap<i64, i32> = HashMap::new();
    let mut virtual_crystal_dmg: HashMap<i64, f64> = HashMap::new();
    crate::services::tactical_modules::step_wubble_probes_and_debuffs(
        ballpark,
        sde_opt,
        db_opt,
        tactical_state,
        dt,
    );
    crate::services::tactical_module_mechanics::step_interdiction_probes_lifetime(
        ballpark,
        db_opt,
        tactical_state,
        dt,
    );

    // Per-tick rack heat generation (`sum(heatAbsorbtionRateModifier)` * evaluated `heatGenerationMultiplier`)
    // and passive exponential dissipation (`feat:modules.overheat-rack-heat-generation`)
    crate::services::tactical_module_mechanics::step_rack_heat_per_tick(
        ship_rack_heat,
        active_module_cycles,
        sde_opt,
        db_opt,
        dt,
    );

    // Upwell structure tethering repairs fitted module heat damage (`feat:modules.nanite-repair-paste-in-space`)
    let tethered_ships: Vec<BallId> = ballpark.combat_states.iter().filter_map(|(&bid, cs)| cs.is_tethered.then_some(bid)).collect();
    if !tethered_ships.is_empty() {
        let tether_repair_hp = 2.0 * dt.max(0.0);
        if let Some(db_ref) = db_opt {
            // Non-blocking read pre-check avoids taking the global DB write lock on every 1 Hz tick
            // when tethered ships have no fitted module heat damage (`#1207`); if DB write lock is held
            // elsewhere, enqueue best-effort and let `post_unlock_worker` skip no-ops inside `wtx`.
            let maybe_has_heat_damage = db_ref
                .try_with_snapshot(|snap| {
                    snap.items.values().any(|it| {
                        tethered_ships.contains(&it.location_id)
                            && matches!(it.flag as u16, 11..=34 | 92..=94 | 125..=132 | 164..=171)
                            && it.dynamic_attributes.contains_key(&eve_sde::attr_ids::DAMAGE)
                    })
                })
                .unwrap_or(true);
            if maybe_has_heat_damage {
                post_unlock.push(super::post_unlock::PostUnlockIntent::RepairTetherModuleHeal {
                    ship_ids: tethered_ships,
                    heal_hp: tether_repair_hp,
                });
            }
        }
    }

    // Advance in-space Nanite Repair Paste jobs (`feat:modules.nanite-repair-paste-in-space`):
    // consume paste incrementally per tick and halt cleanly if cargo paste is exhausted mid-repair.
    active_nanite_repairs.retain_mut(|nr| {
        if !ballpark.balls.contains_key(&nr.ship_id)
            || active_module_cycles.iter().any(|c| c.ship_id == nr.ship_id && c.module_id == nr.module_id)
        {
            return false;
        }
        let next_elapsed = (nr.elapsed_s + dt).min(nr.duration_s);
        let next_progress = (next_elapsed / nr.duration_s.max(0.1)).clamp(0.0, 1.0);
        let total_needed = nr.paste_total_needed.max(1);
        let target_consumed = ((total_needed as f64) * next_progress - 1e-9)
            .ceil()
            .clamp(1.0, total_needed as f64) as i32;
        if let Some(db_ref) = db_opt.as_ref() {
            match super::post_unlock::collect_nanite_repair_tick(
                db_ref,
                nr,
                next_elapsed,
                next_progress,
                target_consumed,
                &mut virtual_paste_used,
                post_unlock,
            ) {
                super::post_unlock::NaniteTickCollect::Defer => return true,
                super::post_unlock::NaniteTickCollect::Applied {
                    halted_for_paste,
                    effective_progress,
                } => {
                    if halted_for_paste {
                        nr.elapsed_s = nr.duration_s * effective_progress;
                        return false;
                    }
                }
            }
        }
        nr.elapsed_s = next_elapsed;
        nr.elapsed_s + 1e-6 < nr.duration_s
    });

    pending_anchoring_disruptors.retain_mut(|d| {
        d.remaining_s -= dt;
        if d.remaining_s <= 1e-6 {
            if let Some(ball) = ballpark.balls.get_mut(&d.id) {
                ball.mode = MovementMode::Field;
                ball.radius = d.radius.max(1.0);
            } else {
                let _ = ballpark.add_mobile_warp_disruptor(d.id, d.position, d.radius);
            }
            ballpark.add_warp_bubble(d.id, d.position, d.radius);
            false
        } else {
            true
        }
    });

    // Count active Capital remote repairers per target for Capital Remote Assistance Impedance
    let mut cap_remote_rep_ranks: HashMap<(BallId, i64), usize> = HashMap::new();
    {
        let mut by_target: HashMap<BallId, Vec<i64>> = HashMap::new();
        for c in active_module_cycles.iter() {
            if let Some(tid) = c.target_id {
                if (c.remote_shield_amount > 0.0 || c.remote_armor_amount > 0.0 || c.remote_hull_amount > 0.0)
                    && is_capital_remote_repair_module(c.module_type_id, sde_opt)
                {
                    by_target.entry(tid).or_default().push(c.module_id);
                }
            }
        }
        for (tid, mut mods) in by_target {
            mods.sort_unstable();
            for (rank, mid) in mods.into_iter().enumerate() {
                cap_remote_rep_ranks.insert((tid, mid), rank);
            }
        }
    }

    let mut auto_target_aggressors: HashMap<BallId, Vec<BallId>> = HashMap::new();
    for c in active_module_cycles.iter() {
        if let Some(tid) = c.target_id.filter(|&tid| tid != c.ship_id) {
            if crate::services::mining_phased_autotarget::is_offensive_aggressor_cycle(c, sde_opt.as_deref()) {
                auto_target_aggressors.entry(tid).or_default().push(c.ship_id);
            }
        }
    }
    
    // Sum SDE heatAbsorbtionRateModifier (1180) across overloaded modules per (ship_id, rack_idx)
    let mut rack_absorption_sums: HashMap<(BallId, usize), f64> = HashMap::new();
    for c in active_module_cycles.iter() {
        if c.is_overloaded {
            let abs_mod = sde_opt
                .as_ref()
                .and_then(|sde| sde.types.get(&c.module_type_id))
                .and_then(|t| t.default_attributes.get(&1180).copied())
                .filter(|&v| v > 0.0)
                .unwrap_or(0.02);
            *rack_absorption_sums.entry((c.ship_id, c.rack_idx.min(2))).or_insert(0.0) += abs_mod;
        }
    }
    let mut finished_indices = Vec::new();
    let mut burned_ship_modules: Vec<(BallId, i64)> = Vec::new();
    for (idx, cycle) in active_module_cycles.iter_mut().enumerate() {
        // If the source ship was destroyed / removed from space mid-cycle (or decloaked by proximity/physics), cancel module cycle (`feat:combat.active-armor-repair-cycle-end`).
        let cloak_decloaked = cycle.effect_name.starts_with("cloaking") && ballpark.combat_states.get(&cycle.ship_id).is_some_and(|cs| !cs.is_cloaked && cs.cloak_velocity_multiplier.is_none());
        if !ballpark.balls.contains_key(&cycle.ship_id) || cloak_decloaked {
            finished_indices.push(idx);
            continue;
        }
        // [plan:DG-4] Targeted module cycles automatically deactivate if target leaves ballpark or exceeds maximum effective range.
        if let Some(target_id) = cycle.target_id {
            let source_ball = ballpark.balls.get(&cycle.ship_id);
            let target_ball = ballpark.balls.get(&target_id);
            let target_warping = target_ball.is_some_and(|b| matches!(b.mode, MovementMode::Warp { .. }));
            let target_dead_or_cloaked = cycle.weapon_is_guided_missile
                && ballpark
                    .combat_states
                    .get(&target_id)
                    .is_some_and(|cs| cs.structure_hp <= 0.0 || cs.blocks_targeted_effect());
            let out_of_range = !cycle.weapon_is_guided_missile
                && matches!((source_ball, target_ball, cycle.max_effective_range_m), (Some(sb), Some(tb), Some(max_r)) if sb.surface_distance(tb) > max_r);
            if target_ball.is_none() || target_warping || target_dead_or_cloaked || out_of_range {
                finished_indices.push(idx);
                continue;
            }
            if cycle.target_velocity_multiplier.is_some() {
                if let (Some(sb), Some(tb)) = (source_ball, target_ball) {
                    let is_grappler = cycle.grappler_speed_factor_pct != 0.0
                        || sde_opt
                            .as_ref()
                            .and_then(|sde| sde.types.get(&cycle.module_type_id))
                            .is_some_and(|t| t.group_id == 1672);
                    if is_grappler {
                        let dist = sb.surface_distance(tb);
                        let sde_mt = sde_opt.as_ref().and_then(|sde| sde.types.get(&cycle.module_type_id));
                        let opt = if cycle.grappler_optimal_m > 0.0 {
                            cycle.grappler_optimal_m
                        } else {
                            sde_mt.and_then(|mt| mt.default_attributes.get(&54).copied()).unwrap_or(1_000.0)
                        };
                        let fo = if cycle.grappler_falloff_m > 0.0 {
                            cycle.grappler_falloff_m
                        } else {
                            sde_mt
                                .and_then(|mt| {
                                    mt.default_attributes
                                        .get(&2044)
                                        .copied()
                                        .or_else(|| mt.default_attributes.get(&158).copied())
                                })
                                .unwrap_or(8_000.0)
                        };
                        let raw_sf = if cycle.grappler_speed_factor_pct != 0.0 {
                            cycle.grappler_speed_factor_pct
                        } else {
                            sde_mt.and_then(|mt| mt.default_attributes.get(&20).copied()).unwrap_or(-80.0)
                        };
                        let att = eve_dogma::compute_falloff_attenuation(dist, opt, fo);
                        let eff_sf = if dist <= opt + 3.0 * fo.max(1.0) {
                            raw_sf * att
                        } else {
                            0.0
                        };
                        cycle.target_velocity_multiplier =
                            Some((1.0 + eff_sf / 100.0).clamp(0.05, 1.0));
                    }
                }
            }
        }
        if cycle.weapon_is_guided_missile && cycle.reload_remaining_sim_s > 0.0 {
            cycle.reload_remaining_sim_s -= dt;
            if cycle.reload_remaining_sim_s > 1e-6 {
                continue;
            }
            cycle.reload_remaining_sim_s = 0.0;
            cycle.elapsed_s = 0.0;
            if cycle.repeat == 0 {
                finished_indices.push(idx);
                continue;
            }
            let out_of_range_now = matches!(
                (
                    ballpark.balls.get(&cycle.ship_id),
                    cycle.target_id.and_then(|tid| ballpark.balls.get(&tid)),
                    cycle.max_effective_range_m,
                ),
                (Some(sb), Some(tb), Some(max_r)) if sb.surface_distance(tb) > max_r
            );
            if out_of_range_now {
                finished_indices.push(idx);
                continue;
            }
            if cycle.cap_need_per_cycle > 0.0 {
                let mut has_cap = true;
                if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                    if cs.capacitor >= cycle.cap_need_per_cycle {
                        cs.capacitor -= cycle.cap_need_per_cycle;
                    } else {
                        has_cap = false;
                    }
                }
                if !has_cap {
                    finished_indices.push(idx);
                    continue;
                }
            }
            let mut consumed_ok = true;
            if let Some(db_ref) = db_opt {
                let launchers: Vec<i64> = if cycle.linked_turrets.is_empty() {
                    vec![cycle.module_id]
                } else {
                    cycle.linked_turrets.clone()
                };
                let eff_char_id = cycle.owner_char_id.filter(|&c| c > 0);
                consumed_ok = super::post_unlock::collect_launcher_consume(
                    db_ref,
                    eff_char_id,
                    cycle.ship_id,
                    &launchers,
                    cycle.ammo_type_id,
                    &mut virtual_item_qty,
                    post_unlock,
                );
            }
            if !consumed_ok {
                if let Some(cid) = cycle.owner_char_id.filter(|&c| c > 0) {
                    pending_cycle_actions.push(PendingCycleAction::NoChargesDeactivated {
                        char_id: cid,
                        module_type_id: cycle.module_type_id,
                        notified: false,
                    });
                }
                finished_indices.push(idx);
                continue;
            }
            super::missiles::enqueue_missile_launches_for_cycle(
                cycle,
                ballpark,
                db_opt,
                pending_missile_launches,
            );
            continue;
        }
        cycle.elapsed_s += dt;
        if cycle.elapsed_s + 1e-6 >= cycle.duration_s {
            let mod_grp = sde_opt.as_ref().and_then(|sde| sde.types.get(&cycle.module_type_id)).map(|t| t.group_id).unwrap_or(0);
            // 1. Local armor repair and hull repair healing at cycle completion (`t = duration_s`, `feat:combat.active-armor-repair-cycle-end`)
            //    (Shield boosters apply at cycle start `t = 0`, `feat:combat.active-shield-boost-cycle-start`)
            if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                if cycle.armor_repair_amount > 0.0 {
                    cs.armor_hp = (cs.armor_hp + cycle.armor_repair_amount).min(cs.max_armor_hp);
                }
                if cycle.hull_repair_amount > 0.0 {
                    cs.structure_hp = (cs.structure_hp + cycle.hull_repair_amount).min(cs.max_structure_hp);
                }
            }

            // 1a. Reactive Armor Hardener resistance shifting (`feat:modules.reactive-armor-hardener`)
            if cycle.is_rah {
                let inc_dmg = rah_incoming_damage
                    .remove(&cycle.ship_id)
                    .unwrap_or([1.0, 0.0, 0.0, 0.0]);
                if let Some(db_ref) = db_opt {
                    let fallback_cur = ballpark
                        .combat_states
                        .get(&cycle.ship_id)
                        .and_then(|cs| cs.rah_armor_resonance)
                        .map(|r| [r[0], r[3], r[2], r[1]])
                        .unwrap_or([0.85, 0.85, 0.85, 0.85]);
                    let cur = db_ref
                        .try_with_snapshot(|snap| {
                            if let Some(tr) = snap.transient_item_attributes.get(&cycle.module_id) {
                                [
                                    tr.get(&267).copied().unwrap_or(0.85),
                                    tr.get(&268).copied().unwrap_or(0.85),
                                    tr.get(&269).copied().unwrap_or(0.85),
                                    tr.get(&270).copied().unwrap_or(0.85),
                                ]
                            } else {
                                fallback_cur
                            }
                        })
                        .unwrap_or(fallback_cur);
                    let new_resonances =
                        step_rah_resonances(cur, inc_dmg, cycle.rah_shift_amount);
                    post_unlock.push(super::post_unlock::PostUnlockIntent::RahResonances {
                        ship_id: cycle.ship_id,
                        module_id: cycle.module_id,
                        resonances: new_resonances,
                    });
                    if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                        cs.rah_armor_resonance = Some([
                            new_resonances[0].clamp(0.01, 1.0),
                            new_resonances[3].clamp(0.01, 1.0),
                            new_resonances[2].clamp(0.01, 1.0),
                            new_resonances[1].clamp(0.01, 1.0),
                        ]);
                    }
                    ballpark.recompute_ball_attributes(cycle.ship_id);
                }
            }

            // 1b. Micro Jump Drive / Field Generator translation upon spool-up completion (`feat:modules.micro-jump-drive`)
            if let Some(jump_dist) = cycle.mjd_jump_distance_m {
                let mjd_disabled = ballpark.combat_states.get(&cycle.ship_id).is_some_and(|cs| cs.is_mwd_mjd_disabled);
                if !mjd_disabled {
                    if let Some((radius_m, max_ships, max_caps)) = cycle.mjfg_field_params {
                        if let Ok(jumped_ids) = ballpark.micro_jump_field_with_capitals(
                            cycle.ship_id,
                            jump_dist,
                            radius_m,
                            max_ships,
                            max_caps,
                        ) {
                            super::collect_ball_positions_for_post_unlock(
                                ballpark,
                                &jumped_ids,
                                db_opt,
                                ball_space_entry_epochs,
                                post_unlock,
                            );
                        }
                    } else if ballpark.micro_jump(cycle.ship_id, jump_dist).is_ok() {
                        super::collect_ball_positions_for_post_unlock(
                            ballpark,
                            &[cycle.ship_id],
                            db_opt,
                            ball_space_entry_epochs,
                            post_unlock,
                        );
                    }
                }
            }

            // 1c. Auto-Targeting System (`feat:modules.auto-targeting-system`)
            if cycle.is_auto_targeter {
                let agg = auto_target_aggressors.get(&cycle.ship_id).map(|v| v.as_slice()).unwrap_or(&[]);
                let fleet_ships = crate::services::mining_phased_autotarget::fleet_ship_ids_for_ship(db_opt.as_ref(), cycle.ship_id);
                let char_faction = db_opt.as_ref().and_then(|db| {
                    db.with_snapshot(|s| {
                        let owner = cycle.owner_char_id.or_else(|| s.items.get(&cycle.ship_id).map(|i| i.owner_id))?;
                        s.characters.get(&owner).and_then(|c| c.war_faction_id)
                    })
                });
                crate::services::mining_phased_autotarget::run_auto_targeter_ballpark_tick(
                    ballpark, sde_opt.as_deref(), char_faction, cycle.ship_id, cycle.auto_target_max_range, cycle.auto_target_max_locks, cycle.auto_target_scan_res, agg, &fleet_ships,
                );
            }

            // 2. Target interactions: damage, cap drain, Nosferatu cap transfer, remote shield/armor/hull/cap (`DG-4`, `DG-5`)
            if let Some(target_id) = cycle.target_id {
                if cycle.damage_vector.total() > 0.0
                    && !cycle.weapon_is_guided_missile
                    && !weapon_cycle_start_modules.contains(&(cycle.ship_id, cycle.module_id))
                {
                    apply_weapon_cycle_damage_collecting(
                        solar_system_id,
                        cycle,
                        ballpark,
                        rah_incoming_damage,
                        sde_opt,
                        db_opt,
                        pending_cycle_actions,
                        &mut destroyed_balls,
                        pending_destroyed_outbox,
                        post_unlock,
                    );
                }
                let src_cap_info = ballpark
                    .combat_states
                    .get(&cycle.ship_id)
                    .map(|s| (s.capacitor, s.max_capacitor));
                let impedance = if is_capital_remote_repair_module(cycle.module_type_id, sde_opt) {
                    let cap_rank = cap_remote_rep_ranks
                        .get(&(target_id, cycle.module_id))
                        .copied()
                        .unwrap_or(0);
                    1.0 / (1.0 + 0.25 * (cap_rank as f64))
                } else {
                    1.0
                };
                // Covers: mutadaptive remote armor repairer spool progression
                let spool_mult = if cycle.mutadaptive_spool_step > 0.0 {
                    let mult = 1.0
                        + ((cycle.current_spool_cycles as f64) * cycle.mutadaptive_spool_step)
                            .min(cycle.mutadaptive_spool_max);
                    cycle.current_spool_cycles = cycle.current_spool_cycles.saturating_add(1);
                    if db_opt.is_some() {
                        let spooled_rep = cycle.remote_armor_amount * mult;
                        post_unlock.push(super::post_unlock::PostUnlockIntent::TransientAttrSets {
                            item_id: cycle.module_id,
                            attrs: vec![(84, spooled_rep)],
                        });
                    }
                    mult
                } else {
                    1.0
                };

                let is_sleeper_target =
                    super::cycles_tackle::is_sleeper_or_drifter_capacitor_immune(
                        ballpark, target_id, sde_opt,
                    );
                let mut nos_transferred = 0.0;
                let mut remote_armor_repaired = 0.0;
                if let Some(cs) = ballpark.combat_states.get_mut(&target_id) {
                    // Nosferatu transfer rule (`feat:combat.nosferatu-drain-cap`, `feat:modules.nosferatu-transfer-rule`, `feat:npc.sleeper-capacitor-immunity-and-leash`):
                    // Transfers at cycle end (`t = duration_s`) when target capacitor > self capacitor, or when Blood Raider hull bonus overrides the cap comparison;
                    // Sleepers have effectively infinite capacitor and are never drained by player neutralizers/nosferatus.
                    if cycle.cap_transfer_target > 0.0 {
                        let mut att = 1.0;
                        if cycle.remote_falloff > 0.0 || cycle.remote_optimal > 0.0 {
                            if let (Some(sb), Some(tb)) = (ballpark.balls.get(&cycle.ship_id), ballpark.balls.get(&target_id)) {
                                let dist = sb.surface_distance(tb);
                                att = eve_dogma::compute_falloff_with_cutoff(dist, cycle.remote_optimal, cycle.remote_falloff);
                            }
                        }
                        let effective_transfer = cycle.cap_transfer_target * att;
                        if let Some((src_cap, src_max_cap)) = src_cap_info {
                            // wiki:Capacitor_warfare: compared by absolute capacitor, not percentage.
                            let can_drain = is_sleeper_target || cycle.nosferatu_blood_raider_override
                                || (src_cap < cs.capacitor && cs.capacitor > 0.0);
                            if can_drain {
                                let max_pull = if is_sleeper_target { effective_transfer } else { cs.capacitor };
                                let transferred = effective_transfer.min(max_pull).min((src_max_cap - src_cap).max(0.0));
                                if !is_sleeper_target {
                                    cs.capacitor = (cs.capacitor - transferred).max(0.0);
                                }
                                nos_transferred = transferred;
                            }
                        }
                    }
                    // Remote armor / hull assistance at cycle end (shield/cap applied at cycle start; blocked if target has `disallow_assistance`)
                    if !cs.disallow_assistance {
                        let mut att = 1.0;
                        if cycle.remote_falloff > 0.0 || cycle.remote_optimal > 0.0 {
                            if let (Some(sb), Some(tb)) = (ballpark.balls.get(&cycle.ship_id), ballpark.balls.get(&target_id)) {
                                let dist = sb.surface_distance(tb);
                                att = eve_dogma::compute_falloff_with_cutoff(dist, cycle.remote_optimal, cycle.remote_falloff);
                            }
                        }
                        if cycle.remote_armor_amount > 0.0 {
                            let old_armor = cs.armor_hp;
                            cs.armor_hp = (cs.armor_hp
                                + cycle.remote_armor_amount * att * spool_mult * impedance)
                                .min(cs.max_armor_hp);
                            remote_armor_repaired = (cs.armor_hp - old_armor).max(0.0);
                        }
                        if cycle.remote_hull_amount > 0.0 {
                            cs.structure_hp = (cs.structure_hp + cycle.remote_hull_amount * att * impedance)
                                .min(cs.max_structure_hp);
                        }
                    }
                }
                if nos_transferred > 0.0 {
                    if let Some(src_cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                        src_cs.capacitor =
                            (src_cs.capacitor + nos_transferred).min(src_cs.max_capacitor);
                    }
                    if !is_sleeper_target {
                        if let Some(cid) = cycle.owner_char_id {
                            pending_cycle_actions.push(PendingCycleAction::CareerProgressEvent {
                                char_id: cid,
                                source_ship_id: cycle.ship_id,
                                target_id,
                                activity_kind: "cap_warfare",
                                sub_kind: "nosferatu",
                                amount: nos_transferred,
                            });
                        }
                    }
                }
                if remote_armor_repaired > 0.0 {
                    if let Some(cid) = cycle.owner_char_id {
                        pending_cycle_actions.push(PendingCycleAction::CareerProgressEvent {
                            char_id: cid,
                            source_ship_id: cycle.ship_id,
                            target_id,
                            activity_kind: "remote_repair",
                            sub_kind: "armor_repair",
                            amount: remote_armor_repaired,
                        });
                    }
                }
                if let Some(cid) = cycle.owner_char_id {
                    if let Some(mt) = sde_opt.as_ref().and_then(|s| s.types.get(&cycle.module_type_id)) {
                        if let Some(jam_t) = crate::services::inventory_fitting::fitting_ewar_capital::ewar_jamming_type(
                            mt,
                            cycle.ammo_type_id,
                            cycle.disables_target_mwd_mjd,
                            cycle.hic_bubble_radius_m,
                        ) {
                            pending_cycle_actions.push(PendingCycleAction::CareerProgressEvent {
                                char_id: cid,
                                source_ship_id: cycle.ship_id,
                                target_id,
                                activity_kind: "ewar",
                                sub_kind: jam_t,
                                amount: cycle.duration_s,
                            });
                        }
                    }
                }
            }

            // 3. Thermodynamics & rack heat accumulation + slot-adjacency heat damage (`feat:dogma.heat-rack-thermodynamics`)
            // parity: feat:modules.emergency-hull-energizer
            let mut burned_out = false;
            if cycle.ehe_hull_resists.is_some() || matches!(cycle.module_type_id, 40714..=40718) {
                burned_out = true;
                if db_opt.is_some() {
                    post_unlock.push(super::post_unlock::PostUnlockIntent::ModuleBurnout {
                        ship_id: cycle.ship_id,
                        module_id: cycle.module_id,
                    });
                }
            }
            if cycle.is_overloaded {
                crate::services::tactical_module_mechanics::apply_overheat_rack_and_damage_on_cycle(
                    cycle,
                    ship_rack_heat,
                    sde_opt,
                    db_opt,
                    mechanic_rng_opt,
                    &mut burned_out,
                    &mut burned_ship_modules,
                );
                if burned_out {
                    pending_cycle_actions.push(PendingCycleAction::InvalidateShipFitting {
                        char_id: cycle.owner_char_id,
                        ship_id: cycle.ship_id,
                    });
                }
            }

            // 4. Weapon ammo & T2/Faction/Mining crystal volatility damage (`feat:modules.t2-crystal-volatility-destruction`)
            let target_destroyed_in_cycle = cycle.target_id.is_some_and(|tid| {
                destroyed_balls.iter().any(|&(bid, _)| bid == tid)
                    || !ballpark.balls.contains_key(&tid)
                    || ballpark
                        .combat_states
                        .get(&tid)
                        .is_some_and(|cs| cs.structure_hp <= 0.0)
            });
            let guided_next_out_of_range = cycle.weapon_is_guided_missile
                && cycle.repeat > 0
                && matches!(
                    (
                        ballpark.balls.get(&cycle.ship_id),
                        cycle.target_id.and_then(|tid| ballpark.balls.get(&tid)),
                        cycle.max_effective_range_m,
                    ),
                    (Some(sb), Some(tb), Some(max_r)) if sb.surface_distance(tb) > max_r
                );
            let eff_char_id = cycle
                .owner_char_id
                .filter(|&c| c > 0)
                .or_else(|| {
                    ballpark
                        .ball_slim_meta
                        .get(&cycle.ship_id)
                        .map(|m| m.owner_id)
                        .filter(|&c| c > 0)
                });
            let (crystal_shattered_empty, guided_reload_started, guided_reload_ms) =
                if let Some(db_ref) = db_opt {
                    super::post_unlock::collect_cycle_ammo_step(
                        db_ref,
                        cycle,
                        eff_char_id,
                        sde_opt,
                        sim_elapsed_secs,
                        tidi_factor,
                        burned_out,
                        target_destroyed_in_cycle,
                        guided_next_out_of_range,
                        &mut reload_sim_remaining,
                        pending_cycle_actions,
                        &mut virtual_item_qty,
                        &mut virtual_crystal_dmg,
                        post_unlock,
                    )
                } else {
                    (false, false, 0.0)
                };

            let mut salvage_next_cap_deducted = false;
            let mut salvage_deducted_cap_gj = 0.0;
            let mut stop_cycle = cycle.repeat == 0
                || burned_out
                || guided_next_out_of_range
                || (crystal_shattered_empty && !guided_reload_started)
                || matches!(cycle.effect_name.as_str(), "superWeaponLanceWarmup" | "superWeaponLanceBeam");
            if stop_cycle {
                finished_indices.push(idx);
            } else {
                cycle.elapsed_s -= cycle.duration_s;
                if let Some(db_ref) = db_opt { cycle.started_at_unix = db_ref.clock().now_unix_secs(); }
                // Handle ASB, AAR, and Cap Booster charge consumption for the next cycle (`DG-4`)
                if cycle.is_asb {
                    let consumed_charge = db_opt.as_deref().is_some_and(|db_ref| {
                        super::post_unlock::collect_ship_charge_consume(
                            db_ref,
                            eff_char_id,
                            cycle.ship_id,
                            cycle.module_id,
                            super::post_unlock::ShipChargeConsumeKind::Asb,
                            &mut virtual_item_qty,
                            post_unlock,
                        )
                    });
                    if consumed_charge {
                        cycle.cap_need_per_cycle = 0.0;
                    } else {
                        cycle.cap_need_per_cycle = cycle.asb_fallback_cap_need;
                    }
                } else if cycle.is_aar {
                    let fueled = db_opt.as_deref().is_some_and(|db_ref| {
                        super::post_unlock::collect_ship_charge_consume(
                            db_ref,
                            eff_char_id,
                            cycle.ship_id,
                            cycle.module_id,
                            super::post_unlock::ShipChargeConsumeKind::AarPaste {
                                paste_rate: cycle.aar_paste_rate,
                            },
                            &mut virtual_item_qty,
                            post_unlock,
                        )
                    });
                    let rep_amt = if fueled {
                        cycle.aar_base_armor_repair * 3.0
                    } else {
                        cycle.aar_base_armor_repair
                    };
                    if matches!(cycle.module_type_id, 41476..=41479) {
                        cycle.remote_armor_amount = rep_amt;
                    } else {
                        cycle.armor_repair_amount = rep_amt;
                    }
                } else if cycle.cap_booster_inject_amount > 0.0 {
                    let consumed_charge = db_opt.as_deref().is_some_and(|db_ref| {
                        super::post_unlock::collect_ship_charge_consume(
                            db_ref,
                            eff_char_id,
                            cycle.ship_id,
                            cycle.module_id,
                            super::post_unlock::ShipChargeConsumeKind::CapBooster,
                            &mut virtual_item_qty,
                            post_unlock,
                        )
                    });
                    if consumed_charge {
                        if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                            cs.capacitor = (cs.capacitor + cycle.cap_booster_inject_amount)
                                .min(cs.max_capacitor);
                        }
                    } else {
                        stop_cycle = true;
                    }
                }

                if !stop_cycle && !guided_reload_started && cycle.cap_need_per_cycle > 0.0 {
                    let mut has_cap = true;
                    if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                        if cs.capacitor >= cycle.cap_need_per_cycle {
                            cs.capacitor -= cycle.cap_need_per_cycle;
                        } else {
                            has_cap = false;
                        }
                    }
                    if has_cap {
                        salvage_next_cap_deducted = true;
                        salvage_deducted_cap_gj = cycle.cap_need_per_cycle;
                    } else {
                        stop_cycle = true;
                    }
                }
                if stop_cycle {
                    finished_indices.push(idx);
                } else {
                    if let Some(next_ovl) = cycle.pending_overloaded.take() {
                        crate::system_actor::cycles_tackle::apply_overload_transition_to_cycle_with_projection(
                            cycle,
                            next_ovl,
                            ballpark,
                            sde_opt,
                            fit_projections.get(&cycle.ship_id),
                        );
                        pending_cycle_actions.push(PendingCycleAction::SyncOverloadState {
                            char_id: cycle.owner_char_id,
                            ship_id: cycle.ship_id,
                            module_id: cycle.module_id,
                            is_overloaded: next_ovl,
                        });
                    }
                    let shield_boost_mult = ballpark
                        .combat_states
                        .get(&cycle.ship_id)
                        .map(|cs| cs.shield_boost_multiplier)
                        .unwrap_or(1.0);
                    let _ = sync_cycle_from_fit_projection_with_shield_boost_mult(
                        cycle,
                        fit_projections,
                        shield_boost_mult,
                    );
                    if cycle.weapon_is_guided_missile {
                        if guided_reload_started {
                            cycle.reload_remaining_sim_s = (guided_reload_ms / 1000.0).max(0.001);
                            cycle.elapsed_s = 0.0;
                        } else {
                            super::missiles::enqueue_missile_launches_for_cycle(
                                cycle,
                                ballpark,
                                db_opt,
                                pending_missile_launches,
                            );
                        }
                    } else if weapon_cycle_start_modules.contains(&(cycle.ship_id, cycle.module_id))
                        && cycle.damage_vector.total() > 0.0
                    {
                        apply_weapon_cycle_damage_collecting(
                            solar_system_id,
                            cycle,
                            ballpark,
                            rah_incoming_damage,
                            sde_opt,
                            db_opt,
                            pending_cycle_actions,
                            &mut destroyed_balls,
                            pending_destroyed_outbox,
                            post_unlock,
                        );
                    }
                    if matches!(mod_grp, 72 | 80 | 201 | 353 | 650) {
                        if let Some(cid) = cycle.owner_char_id {
                            pending_cycle_actions.push(PendingCycleAction::RepeatCycleStart {
                                char_id: cid,
                                ship_id: cycle.ship_id,
                                module_id: cycle.module_id,
                                module_type_id: cycle.module_type_id,
                                target_id: cycle.target_id,
                                duration_s: cycle.duration_s,
                                is_overloaded: cycle.is_overloaded,
                                effect_name: cycle.effect_name.clone(),
                            });
                        }
                    }
                    // Apply start-of-next-cycle (`t = 0`) effects: active shield boost, energy neutralizer drain, remote shield/cap
                    if cycle.shield_boost_amount > 0.0 {
                        if let Some(cs) = ballpark.combat_states.get_mut(&cycle.ship_id) {
                            let boost_mult = if cs.shield_boost_multiplier > 0.0 { cs.shield_boost_multiplier } else { 1.0 };
                            cs.shield_hp = (cs.shield_hp + cycle.shield_boost_amount * boost_mult).min(cs.max_shield_hp);
                        }
                    }
                    if let Some(target_id) = cycle.target_id {
                        let is_sleeper_target =
                            super::cycles_tackle::is_sleeper_or_drifter_capacitor_immune(
                                ballpark, target_id, sde_opt,
                            );
                        let impedance = if is_capital_remote_repair_module(cycle.module_type_id, sde_opt) {
                            let cap_rank = cap_remote_rep_ranks
                                .get(&(target_id, cycle.module_id))
                                .copied()
                                .unwrap_or(0);
                            1.0 / (1.0 + 0.25 * (cap_rank as f64))
                        } else {
                            1.0
                        };
                        let mut rollover_cap_drained = 0.0;
                        let mut rollover_cap_transferred = 0.0;
                        let mut rollover_shield_repaired = 0.0;
                        if let Some(cs) = ballpark.combat_states.get_mut(&target_id) {
                            let mut att = 1.0;
                            if cycle.remote_falloff > 0.0 || cycle.remote_optimal > 0.0 {
                                if let (Some(sb), Some(tb)) = (ballpark.balls.get(&cycle.ship_id), ballpark.balls.get(&target_id)) {
                                    let dist = sb.surface_distance(tb);
                                    att = eve_dogma::compute_falloff_with_cutoff(dist, cycle.remote_optimal, cycle.remote_falloff);
                                }
                            }
                            if cycle.cap_drain_target > 0.0 && !is_sleeper_target {
                                let old_cap = cs.capacitor;
                                cs.capacitor = (cs.capacitor - cycle.cap_drain_target * att).max(0.0);
                                rollover_cap_drained = (old_cap - cs.capacitor).max(0.0);
                            }
                            if !cs.disallow_assistance {
                                if cycle.remote_shield_amount > 0.0 {
                                    let old_shield = cs.shield_hp;
                                    cs.shield_hp = (cs.shield_hp
                                        + cycle.remote_shield_amount * att * impedance)
                                        .min(cs.max_shield_hp);
                                    rollover_shield_repaired = (cs.shield_hp - old_shield).max(0.0);
                                }
                                if cycle.remote_cap_amount > 0.0 {
                                    let old_cap = cs.capacitor;
                                    cs.capacitor = (cs.capacitor + cycle.remote_cap_amount * att)
                                        .min(cs.max_capacitor);
                                    rollover_cap_transferred = (cs.capacitor - old_cap).max(0.0);
                                }
                            }
                        }
                        if let Some(cid) = cycle.owner_char_id {
                            if rollover_cap_drained > 0.0 {
                                pending_cycle_actions.push(PendingCycleAction::CareerProgressEvent {
                                    char_id: cid,
                                    source_ship_id: cycle.ship_id,
                                    target_id,
                                    activity_kind: "cap_warfare",
                                    sub_kind: "neutralizer",
                                    amount: rollover_cap_drained,
                                });
                            }
                            if rollover_cap_transferred > 0.0 {
                                pending_cycle_actions.push(PendingCycleAction::CareerProgressEvent {
                                    char_id: cid,
                                    source_ship_id: cycle.ship_id,
                                    target_id,
                                    activity_kind: "cap_warfare",
                                    sub_kind: "cap_transfer",
                                    amount: rollover_cap_transferred,
                                });
                            }
                            if rollover_shield_repaired > 0.0 {
                                pending_cycle_actions.push(PendingCycleAction::CareerProgressEvent {
                                    char_id: cid,
                                    source_ship_id: cycle.ship_id,
                                    target_id,
                                    activity_kind: "remote_repair",
                                    sub_kind: "shield_boost",
                                    amount: rollover_shield_repaired,
                                });
                            }
                        }
                    }
                }
            }
            if mod_grp == 1122 {
                if let (Some(cid), Some(wid)) = (cycle.owner_char_id, cycle.target_id) {
                    pending_cycle_actions.push(PendingCycleAction::SalvageEnd {
                        char_id: cid,
                        ship_id: cycle.ship_id,
                        module_id: cycle.module_id,
                        module_type_id: cycle.module_type_id,
                        wreck_id: wid,
                        repeat: if stop_cycle { 0 } else { cycle.repeat },
                        effect_name: cycle.effect_name.clone(),
                        next_cycle_cap_deducted: salvage_next_cap_deducted,
                        deducted_cap_gj: salvage_deducted_cap_gj,
                    });
                }
            }
        }
    }
    for (burned_ship_id, burned_mod_id) in burned_ship_modules {
        let burned_char_id = active_module_cycles
            .iter()
            .find(|c| c.ship_id == burned_ship_id)
            .and_then(|c| c.owner_char_id);
        pending_cycle_actions.push(PendingCycleAction::InvalidateShipFitting {
            char_id: burned_char_id,
            ship_id: burned_ship_id,
        });
        for (other_idx, c) in active_module_cycles.iter().enumerate() {
            if c.ship_id == burned_ship_id && c.module_id == burned_mod_id {
                finished_indices.push(other_idx);
            }
        }
    }
    finished_indices.sort_unstable();
    finished_indices.dedup();
    let mut lance_requeues = Vec::new();
    for idx in finished_indices.into_iter().rev() {
        let mut finished = active_module_cycles.remove(idx);
        let is_lance = matches!(finished.effect_name.as_str(), "superWeaponLanceWarmup" | "superWeaponLanceBeam");
        if is_lance && finished.elapsed_s >= finished.duration_s {
            let prev_len = destroyed_balls.len();
            crate::services::tactical_modules::fire_lance_warmup_in_ballpark(
                ballpark, &finished, sde_opt, db_opt, tactical_state, pending_cycle_actions, &mut destroyed_balls,
            );
            if let Some(outbox) = pending_destroyed_outbox {
                outbox.record_batch(&destroyed_balls[prev_len..]);
            }
            if finished.repeat > 0 {
                finished.effect_name = "superWeaponLanceBeam".into();
                finished.repeat -= 1;
                finished.elapsed_s = 0.0;
                finished.duration_s = 1.0;
                lance_requeues.push(finished);
                continue;
            }
        }
        cleanup_removed_module_cycle_collecting(ballpark, &finished, db_opt, post_unlock);
        emit_cycle_deactivation_collecting(
            solar_system_id,
            &finished,
            ballpark,
            deactivation_hook.is_some(),
            post_unlock,
        );
    }
    active_module_cycles.extend(lance_requeues);
    weapon_cycle_start_modules.retain(|&(sid, mid)| {
        active_module_cycles
            .iter()
            .any(|c| c.ship_id == sid && c.module_id == mid)
    });
    super::cycles_tackle::reconcile_tackle_and_bubbles_collecting(
        solar_system_id,
        ballpark,
        active_module_cycles,
        base_scramble_strengths,
        base_core_strengths,
        db_opt,
        deactivation_hook,
        post_unlock,
    );
    destroyed_balls
}

pub(crate) fn cleanup_removed_module_cycle_collecting(
    ballpark: &mut Ballpark,
    removed: &ActiveModuleCycle,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    post_unlock: &mut super::post_unlock::PostUnlockOps,
) {
    let mut needs_recompute = false;
    if removed.target_id.is_some() {
        ballpark.remove_module_ecm_jam(removed.ship_id, removed.module_id, removed.target_id);
    }
    if removed.effect_name.starts_with("cloaking") {
        if let Some(cs) = ballpark.combat_states.get_mut(&removed.ship_id) {
            cs.is_covert_cloak = false;
            cs.cloak_stabilization_remaining_s = 0.0;
            cs.cloak_velocity_multiplier = None;
            if cs.gate_cloak_remaining_ticks == 0 {
                cs.decloak_targeting_delay_remaining_s = cs
                    .decloak_targeting_delay_remaining_s
                    .max(cs.cloak_configured_targeting_delay_s);
            }
        }
        needs_recompute = true;
    }
    if removed.is_adc {
        if let Some(cs) = ballpark.combat_states.get_mut(&removed.ship_id) {
            cs.adc_active = false;
        }
        needs_recompute = true;
    }
    if removed.ehe_hull_resists.is_some() {
        if let Some(cs) = ballpark.combat_states.get_mut(&removed.ship_id) {
            cs.ehe_hull_resists = None;
        }
        needs_recompute = true;
    }
    if removed.is_rah {
        if let Some(cs) = ballpark.combat_states.get_mut(&removed.ship_id) {
            cs.rah_armor_resonance = None;
        }
        needs_recompute = true;
    }
    if removed.immobilize_self {
        if let Some(cs) = ballpark.combat_states.get_mut(&removed.ship_id) {
            cs.module_immobilized = false;
        }
        needs_recompute = true;
    }
    if needs_recompute {
        ballpark.recompute_ball_attributes(removed.ship_id);
    }
    if removed.hic_bubble_radius_m.is_some() {
        ballpark.remove_warp_bubble(removed.module_id);
    }
    if removed.grants_interdiction_nullification {
        ballpark.set_interdiction_nullified(removed.ship_id, false);
    }
    if db_opt.is_some() {
        post_unlock.push(super::post_unlock::PostUnlockIntent::ModuleCycleCleanup(
            super::post_unlock::ModuleCycleCleanupDbOp::from_cycle(removed),
        ));
    }
}

pub(crate) fn emit_cycle_deactivation_collecting(
    solar_system_id: SolarSystemId,
    cycle: &ActiveModuleCycle,
    ballpark: &Ballpark,
    has_deact_hook: bool,
    post_unlock: &mut super::post_unlock::PostUnlockOps,
) {
    if !has_deact_hook
        || cycle.module_id < 0
        || super::cycles_tackle::parse_minion_cycle_parent(&cycle.effect_name).is_some()
    {
        return;
    }
    let char_id = cycle
        .owner_char_id
        .or_else(|| ballpark.ball_slim_meta.get(&cycle.ship_id).and_then(|m| m.char_id));
    let fallback_owner_id = ballpark
        .ball_slim_meta
        .get(&cycle.ship_id)
        .and_then(|m| (m.owner_id > 0).then_some(m.owner_id));
    post_unlock.push(super::post_unlock::PostUnlockIntent::DeactivationHook(
        super::post_unlock::DeferredDeactivationHookOp {
            solar_system_id,
            char_id,
            fallback_owner_id,
            cycle: cycle.clone(),
        },
    ));
}

/// Parity: [plan:DG-4] Entering warp halts active non-warp module cycles (weapons, scrams, webs, mining lasers, etc.)
/// Only modules that specifically grant interdiction nullification (warp core stabilizers / nullifiers) remain.
pub(crate) fn remove_non_warp_ship_module_cycles_collecting(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ship_id: BallId,
    deactivation_hook: Option<&ModuleDeactivationHook>,
    post_unlock: &mut super::post_unlock::PostUnlockOps,
) {
    let mut removed_cycles = Vec::new();
    let mut i = 0;
    while i < active_module_cycles.len() {
        if active_module_cycles[i].ship_id == ship_id && !active_module_cycles[i].grants_interdiction_nullification {
            let removed = active_module_cycles.swap_remove(i);
            cleanup_removed_module_cycle_collecting(ballpark, &removed, db_opt, post_unlock);
            removed_cycles.push(removed);
        } else {
            i += 1;
        }
    }
    if !removed_cycles.is_empty() {
        super::cycles_tackle::reconcile_tackle_and_bubbles_collecting(
            solar_system_id,
            ballpark,
            active_module_cycles,
            base_scramble_strengths,
            base_core_strengths,
            db_opt,
            deactivation_hook,
            post_unlock,
        );
        ballpark.recompute_ball_attributes(ship_id);
        for removed in &removed_cycles {
            emit_cycle_deactivation_collecting(
                solar_system_id,
                removed,
                ballpark,
                deactivation_hook.is_some(),
                post_unlock,
            );
        }
    }
}

/// Removes all active module cycles owned by or targeting `ship_id`.
///
/// When `preserve_own_db_active_states` is `true` (used by DB-preserving stargate `TakeBall` and
/// tokenized `RemoveBallIfToken` post-spawn stargate rollbacks), DB
/// `module_active_states`/`module_active_charges`/`transient_item_attributes` cleanup and
/// `module_deactivation_hook` emission are suppressed **only** for cycles owned by `ship_id`
/// (`removed.ship_id == ship_id`) so DB rollback can restore or keep them intact cleanly. Any
/// other ship's cycle targeting `ship_id` (`removed.ship_id != ship_id`) and any secondary cycle
/// aborted by `reconcile_tackle_and_bubbles` still run full DB cleanup and
/// `module_deactivation_hook` so attackers/fleetmates clear DB active state, receive
/// deactivation notifications, and flush `projected_ewar` / `remote_assistance` before
/// destination `build_ship_combat_state` runs or during rollback (`#1237`, `#1467`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn remove_all_ship_module_cycles_preserving_own_db_collecting(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<i64, i32>,
    base_core_strengths: &HashMap<i64, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ship_id: i64,
    deactivation_hook: Option<&ModuleDeactivationHook>,
    preserve_own_db_active_states: bool,
    post_unlock: &mut super::post_unlock::PostUnlockOps,
) -> Vec<(ActiveModuleCycle, Option<i64>)> {
    let no_db = None;
    let mut removed_cycles = Vec::new();
    for i in (0..active_module_cycles.len()).rev() {
        if active_module_cycles[i].ship_id == ship_id || active_module_cycles[i].target_id == Some(ship_id) {
            let removed = active_module_cycles.swap_remove(i);
            let cycle_db = if preserve_own_db_active_states && removed.ship_id == ship_id {
                &no_db
            } else {
                db_opt
            };
            cleanup_removed_module_cycle_collecting(ballpark, &removed, cycle_db, post_unlock);
            let char_id = removed.owner_char_id.or_else(|| {
                ballpark
                    .ball_slim_meta
                    .get(&removed.ship_id)
                    .and_then(|m| m.char_id)
            });
            removed_cycles.push((removed, char_id));
        }
    }
    if !preserve_own_db_active_states {
        if let Some(db_ref) = db_opt {
            let maybe_dirty = db_ref
                .try_with_snapshot(|s| {
                    (!s.module_active_states.is_empty() || !s.module_active_charges.is_empty())
                        && s.items.values().any(|it| {
                            it.location_id == ship_id
                                && (s.module_active_states.contains(&it.item_id)
                                    || s.module_active_charges.contains_key(&it.item_id))
                        })
                })
                .unwrap_or(true);
            if maybe_dirty {
                post_unlock.push(super::post_unlock::PostUnlockIntent::ShipCycleClearAll(ship_id));
            }
        }
    }
    if !removed_cycles.is_empty() {
        super::cycles_tackle::reconcile_tackle_and_bubbles_collecting(
            solar_system_id,
            ballpark,
            active_module_cycles,
            base_scramble_strengths,
            base_core_strengths,
            db_opt,
            deactivation_hook,
            post_unlock,
        );
        for (removed, _) in &removed_cycles {
            if preserve_own_db_active_states && removed.ship_id == ship_id {
                continue;
            }
            emit_cycle_deactivation_collecting(
                solar_system_id,
                removed,
                ballpark,
                deactivation_hook.is_some(),
                post_unlock,
            );
        }
    }
    removed_cycles
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn remove_all_ship_module_cycles_collecting(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<i64, i32>,
    base_core_strengths: &HashMap<i64, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ship_id: i64,
    deactivation_hook: Option<&ModuleDeactivationHook>,
    post_unlock: &mut super::post_unlock::PostUnlockOps,
) -> Vec<(ActiveModuleCycle, Option<i64>)> {
    remove_all_ship_module_cycles_preserving_own_db_collecting(
        solar_system_id,
        ballpark,
        active_module_cycles,
        base_scramble_strengths,
        base_core_strengths,
        db_opt,
        ship_id,
        deactivation_hook,
        false,
        post_unlock,
    )
}

/// Restores preserved own ship module cycles after a failed DB-preserving `TakeBall`
/// rollback, cleaning DB/hook state for any targeted cycles whose target locks were lost (`#1237`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn restore_taken_own_cycles_in_actor_collecting(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<i64, i32>,
    base_core_strengths: &HashMap<i64, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    ball_id: i64,
    deactivation_hook: Option<&ModuleDeactivationHook>,
    removed_own_cycles: Vec<ActiveModuleCycle>,
    force_recompute: bool,
    post_unlock: &mut super::post_unlock::PostUnlockOps,
) {
    if removed_own_cycles.is_empty() {
        if force_recompute {
            ballpark.recompute_ball_attributes(ball_id);
        }
        return;
    }
    let mut restored_any = false;
    let mut broken_lock_cycles = Vec::new();
    for cycle in removed_own_cycles {
        if cycle.target_id.is_none_or(|tid| tid == ball_id) {
            if !active_module_cycles
                .iter()
                .any(|c| c.ship_id == cycle.ship_id && c.module_id == cycle.module_id)
            {
                active_module_cycles.push(cycle);
                restored_any = true;
            }
        } else {
            broken_lock_cycles.push(cycle);
        }
    }
    for cycle in &broken_lock_cycles {
        cleanup_removed_module_cycle_collecting(ballpark, cycle, db_opt, post_unlock);
    }
    if restored_any {
        super::cycles_tackle::reconcile_tackle_and_bubbles_collecting(
            solar_system_id,
            ballpark,
            active_module_cycles,
            base_scramble_strengths,
            base_core_strengths,
            db_opt,
            deactivation_hook,
            post_unlock,
        );
        ballpark.recompute_ball_attributes(ball_id);
    } else if force_recompute {
        ballpark.recompute_ball_attributes(ball_id);
    }
    for cycle in &broken_lock_cycles {
        emit_cycle_deactivation_collecting(
            solar_system_id,
            cycle,
            ballpark,
            deactivation_hook.is_some(),
            post_unlock,
        );
    }
}

/// Emits Destiny `OnSpecialFX { is_active: false }` stop events for every active propulsion
/// cycle (`MicroWarpDrive` / `Afterburner`) on `ball_id` prior to ball detach during docking (`#1081`).
pub(crate) fn emit_propulsion_stop_fx_before_detach(
    ballpark: &mut Ballpark,
    active_module_cycles: &[ActiveModuleCycle],
    sde_opt: &Option<Arc<eve_sde::SdeRegistry>>,
    ball_id: BallId,
) {
    let prop_fx_stops: Vec<DestinyEvent> = active_module_cycles
        .iter()
        .filter(|c| {
            c.ship_id == ball_id
                && (c.speed_boost_factor > 1.0
                    || c.effect_name.eq_ignore_ascii_case("moduleBonusAfterburner")
                    || c.effect_name.eq_ignore_ascii_case("moduleBonusMicrowarpdrive")
                    || c.effect_name.to_ascii_lowercase().contains("afterburner")
                    || c.effect_name.to_ascii_lowercase().contains("microwarpdrive"))
        })
        .map(|c| {
            let guid = sde_opt
                .as_ref()
                .and_then(|sde| {
                    crate::router::binding::special_fx_guid_for_module_sde(
                        sde,
                        c.module_type_id,
                        0,
                        Some(&c.effect_name),
                    )
                })
                .map(|(g, _)| g)
                .unwrap_or_else(|| {
                    if c.effect_name.to_ascii_lowercase().contains("microwarpdrive") {
                        "effects.MicroWarpDrive".to_string()
                    } else {
                        "effects.Afterburner".to_string()
                    }
                });
            DestinyEvent::OnSpecialFX {
                ball_id,
                module_id: Some(c.module_id),
                guid,
                is_active: false,
            }
        })
        .collect();
    for ev in prop_fx_stops {
        ballpark.emit_event(Some(ball_id), ev);
    }
}

impl SystemHandle {
    pub fn has_active_modules_or_cloak_sync(&self, ball_id: BallId) -> bool {
        if self.active_module_ships.read().unwrap_or_else(|e| e.into_inner()).contains(&ball_id) {
            return true;
        }
        let bp = self.ballpark.read().unwrap_or_else(|e| e.into_inner());
        bp.combat_states.get(&ball_id).is_some_and(eve_destiny::CombatState::has_any_cloak)
            || bp.ball_slim_meta.get(&ball_id).is_some_and(|m| m.is_cloaked)
    }

    pub fn set_last_auto_stepped_unix(&self, now_unix: u64) {
        self.last_auto_stepped_unix.store(now_unix, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn take_auto_stepped_at(&self, now_unix: u64) -> bool {
        now_unix > 0 && self.last_auto_stepped_unix.swap(0, std::sync::atomic::Ordering::SeqCst) == now_unix
    }

    pub fn try_step_full_tick(&self, dt: f64) -> u64 {
        let caller_can_wait = super::post_unlock::caller_can_wait_for_handle(self);
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
        let SystemActorState {
            active_module_cycles, pending_anchoring_disruptors, active_nanite_repairs,
            ship_rack_heat, rah_incoming_damage, base_scramble_strengths, base_core_strengths,
            asteroids, asteroid_templates, sde_opt, db_opt, mechanic_rng_opt,
            sim_elapsed_secs, pending_cycle_actions, tactical_state,
            pending_notifications_opt, notify_tx_opt, ball_space_entry_epochs,
            fit_projections, weapon_cycle_start_modules,
            module_reload_sim_deadlines, module_reload_sim_remaining,
            pending_missile_launches, pending_missile_deploy_fx,
            active_guided_missiles, next_missile_ball_id, pending_destroyed_balls,
            warp_state, external_active_modules, active_drone_miners, drone_next_ready_unix, ..
        } = &mut *st;
        let mut post_unlock_ops = super::post_unlock::PostUnlockOps::default();
        let tidi = self.tidi_factor();
        let dt_sim = if dt > 0.0 && dt.is_finite() {
            dt * tidi
        } else {
            0.0
        };
        if dt_sim > 0.0 {
            *sim_elapsed_secs += dt_sim;
            self.sim_elapsed_secs_bits.store(
                sim_elapsed_secs.to_bits(),
                std::sync::atomic::Ordering::Release,
            );
        }
        let dt = dt_sim;
        super::belts::respawn_due_asteroids_collecting(
            &mut bp,
            asteroids,
            asteroid_templates,
            sde_opt,
            db_opt,
            &mut post_unlock_ops,
        );
        sync_player_slim_items(&mut bp, db_opt);
        if let Some(db) = db_opt.as_ref() {
            let now_unix = db.clock().now_unix_secs();
            if now_unix > bp.sim_unix_secs {
                bp.sim_unix_secs = now_unix;
            }
        }
        let pre_tick_caps = snapshot_player_ship_capacitors(&bp, db_opt.as_ref());
        let pre_missile_positions =
            super::missiles::snapshot_missile_positions(&bp, active_guided_missiles);
        let events = bp.step(dt);
        super::missiles::flush_pending_missile_deploy_fx(&mut bp, pending_missile_deploy_fx);
        super::collect_landed_warps_for_post_unlock(
            &bp,
            &events,
            db_opt,
            ball_space_entry_epochs,
            &mut post_unlock_ops,
        );
        super::clear_landed_warp_tracking_from_events(&bp, &events, warp_state);
        flush_target_lost_notifications(
            &bp,
            &events,
            db_opt.as_ref(),
            pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>),
            notify_tx_opt.as_ref(),
        );
        let lock_hook_opt = self
            .target_lock_completed_hook
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if lock_hook_opt.is_some() {
            for ev in &events {
                if let eve_destiny::DestinyEvent::OnTarget { mode, source_id, target_id } = ev {
                    if mode == "locked" {
                        let _ = super::cycles_tackle::emit_target_lock_completed_collecting(
                            &bp,
                            db_opt,
                            *source_id,
                            *target_id,
                            true,
                            &mut post_unlock_ops,
                        );
                    }
                }
            }
        }
        let sup_dead = crate::services::pve_abyssal_pylons::step_abyssal_hazards_and_pylons_in_ballpark_for_tick(
            &mut bp, sde_opt.as_deref(), db_opt.as_ref(), self.solar_system_id,
        );
        let pg_dead = crate::services::pvp_filament_arena::step_proving_grounds_arena_hazards_in_ballpark_for_tick(
            &mut bp, sde_opt.as_deref(), db_opt.as_ref(), self.solar_system_id,
            pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>), notify_tx_opt.as_ref(),
        );
        let step_now_unix = db_opt.as_ref().map(|db| db.clock().now_unix_secs()).unwrap_or(0);
        let deact_hook_guard = self.module_deactivation_hook.read().unwrap_or_else(|e| e.into_inner()).clone();
        for ev in &events {
            if let eve_destiny::DestinyEvent::OnTarget { mode, source_id, target_id } = ev {
                if mode == "lost" {
                    super::cycles_tackle::on_target_unlocked_in_actor_collecting(
                        self.solar_system_id,
                        &mut bp,
                        active_module_cycles,
                        base_scramble_strengths,
                        base_core_strengths,
                        db_opt,
                        *source_id,
                        Some(*target_id),
                        deact_hook_guard.as_ref(),
                        &mut post_unlock_ops,
                    );
                }
            }
        }
        let fallback_rng = crate::rng::MechanicRng::default();
        let rng_ref = mechanic_rng_opt.as_deref().unwrap_or(&fallback_rng);
        crate::services::mobile_deployables::step_mobile_deployables_in_actor(
            &mut bp, active_module_cycles, rng_ref, deact_hook_guard.as_ref(),
            sde_opt.as_deref(), db_opt.as_ref(), mechanic_rng_opt.as_deref(), self.solar_system_id, dt, step_now_unix,
        );
        let hook_guard = self.npc_death_hook.read().unwrap_or_else(|e| e.into_inner()).clone();
        super::post_unlock::step_module_reloads_sim_clock_collecting(
            module_reload_sim_remaining,
            Some(module_reload_sim_deadlines.as_ref()),
            db_opt,
            dt,
            tidi,
            &mut post_unlock_ops,
        );
        let mut destroyed = Vec::new();
        let pending_destroyed = pending_destroyed_balls.clone();
        super::missiles::step_guided_missiles_in_actor(
            &mut bp,
            active_guided_missiles,
            &pre_missile_positions,
            rah_incoming_damage,
            sde_opt,
            db_opt,
            pending_cycle_actions,
            &mut destroyed,
            Some(&pending_destroyed),
            dt,
        );
        destroyed.extend(step_cycles_and_capacitor_with_missiles_and_tidi_collecting(
            self.solar_system_id,
            &mut bp, active_module_cycles, pending_anchoring_disruptors, active_nanite_repairs,
            ship_rack_heat, rah_incoming_damage, base_scramble_strengths, base_core_strengths,
            sde_opt, db_opt, ball_space_entry_epochs, fit_projections, weapon_cycle_start_modules,
            pending_missile_launches,
            mechanic_rng_opt, tactical_state.as_ref(), pending_cycle_actions, *sim_elapsed_secs, dt, tidi,
            Some(module_reload_sim_remaining), deact_hook_guard.as_ref(), Some(&pending_destroyed),
            &mut post_unlock_ops,
        ));
        super::missiles::flush_pending_missile_launches(
            &mut bp,
            pending_missile_launches,
            active_guided_missiles,
            pending_missile_deploy_fx,
            next_missile_ball_id,
            sde_opt,
        );
        {
            let mut dead_guard = module_reload_sim_deadlines
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for action in pending_cycle_actions.iter() {
                if let PendingCycleAction::AutoReloadStarted {
                    module_ids,
                    sim_deadline_s,
                    ..
                } = action
                {
                    for &mid in module_ids {
                        dead_guard.insert(
                            mid,
                            (self.solar_system_id, *sim_elapsed_secs, *sim_deadline_s),
                        );
                    }
                }
            }
        }
        record_player_ship_capacitor_deltas(&bp, &pre_tick_caps, pending_cycle_actions);
        flush_pending_cycle_damage_notifications_with_ballpark(
            &bp,
            sde_opt.as_deref(),
            db_opt.as_ref(),
            pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>),
            notify_tx_opt.as_ref(),
            pending_cycle_actions,
        );
        for bid in sup_dead.into_iter().chain(pg_dead) {
            destroyed.push((bid, None));
            pending_destroyed.record_one(bid, None);
        }
        let zk_dead = crate::services::space_navigation_advanced::step_zarzakh_gravity_hazard_with_notifications(
            &mut bp,
            sde_opt.as_deref(),
            db_opt.as_ref(),
            self.solar_system_id,
            pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>),
            notify_tx_opt.as_ref(),
        );
        for bid in zk_dead {
            destroyed.push((bid, None));
            pending_destroyed.record_one(bid, None);
        }
        let dead_ids: Vec<BallId> = destroyed.iter().map(|&(bid, _)| bid).collect();
        if !dead_ids.is_empty() {
            external_active_modules.retain(|(sid, _)| !dead_ids.contains(sid));
            for &did in &dead_ids {
                active_drone_miners.remove(&did);
                drone_next_ready_unix.remove(&did);
            }
        }
        teardown_attacker_locks_and_cycles_for_destroyed_balls_collecting(
            self.solar_system_id,
            &mut bp,
            active_module_cycles,
            base_scramble_strengths,
            base_core_strengths,
            db_opt,
            pending_notifications_opt.as_deref().map(|r| r as &dyn AnyRwLock<_>),
            notify_tx_opt.as_ref(),
            deact_hook_guard.as_ref(),
            &dead_ids,
            &mut post_unlock_ops,
        );
        for &(target_ball_id, _) in &destroyed {
            crate::services::tactical_module_mechanics::on_probe_ball_removed_in_actor(
                &mut bp,
                db_opt,
                tactical_state.as_ref(),
                target_ball_id,
            );
            if let Some(db) = db_opt.as_ref() {
                crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_in_ballpark(&mut bp, db, self.solar_system_id, target_ball_id);
            }
        }
        let (drained_destroyed, destroyed_lease) = pending_destroyed.checkout_for_step();
        let mut death_futs = Vec::new();
        for &(target_ball_id, owner_char_id) in &drained_destroyed {
            if let Some(fut) = on_ball_destroyed_by_damage(self.solar_system_id, target_ball_id, owner_char_id, hook_guard.as_ref()) {
                death_futs.push(fut);
            }
        }
        let step_hook_guard = self.system_step_hook.read().unwrap_or_else(|e| e.into_inner()).clone();
        let step_fut = step_hook_guard.map(|hook| hook(self.solar_system_id, dt, drained_destroyed));
        if db_opt.is_some() {
            for (&bid, cs) in &bp.combat_states {
                if bp.balls.contains_key(&bid)
                    && !bp.is_static_ball(bid)
                    && bp.ball_slim_meta.get(&bid).is_none_or(|m| m.category_id != 11)
                {
                    post_unlock_ops.push_ship_combat_hp(super::post_unlock::CombatHpDbUpdate {
                        ball_id: bid,
                        combat_state: cs.clone(),
                        expected_system: Some(self.solar_system_id),
                        require_persisted_entity: true,
                    });
                }
            }
        }
        sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
        {
            let mut mod_guard = self.active_module_ships.write().unwrap_or_else(|e| e.into_inner());
            mod_guard.clear();
            mod_guard.extend(active_module_cycles.iter().map(|c| c.ship_id));
            mod_guard.extend(external_active_modules.iter().map(|&(sid, _)| sid));
        }
        let tick = bp.current_tick;
        let db_opt_cloned = db_opt.clone();
        let sde_opt_cloned = sde_opt.clone();
        let wait_ticket = super::post_unlock::enqueue_post_unlock_under_locks(
            self,
            db_opt_cloned.as_ref(),
            sde_opt_cloned.as_ref(),
            post_unlock_ops,
            caller_can_wait,
        );
        let broadcast_seq_gate = Arc::clone(&st.broadcast_seq_gate);
        super::protection::sync_all_protection_registry_in_actor(&bp, &mut st);
        drop(st);
        let broadcast = self.build_tick_broadcast_for_handle(&mut bp, events);
        let warp_events = broadcast.events.clone();
        crate::system_actor::send_tick_broadcast_sequenced_owned(
            &self.event_tx,
            &broadcast_seq_gate,
            broadcast,
        );
        drop(bp);
        super::post_unlock::finish_post_unlock_after_unlock(
            self,
            deact_hook_guard.as_ref(),
            lock_hook_opt.as_ref(),
            wait_ticket,
        );
        self.record_warp_stops_from_events(&warp_events);
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            if !death_futs.is_empty() || step_fut.is_some() || destroyed_lease.is_some() {
                let _ = handle.spawn(async move {
                    for fut in death_futs {
                        fut.await;
                    }
                    if let Some(sf) = step_fut {
                        sf.await;
                    }
                    if let Some(lease) = destroyed_lease {
                        lease.ack();
                    }
                });
            }
        } else {
            // Without an active Tokio runtime we cannot execute downstream async hooks;
            // drop `destroyed_lease` un-acked so records roll back into `pending_destroyed_balls`.
            drop(destroyed_lease);
        }
        tick
    }

    #[allow(clippy::too_many_arguments)]
    pub fn prepare_disconnected_warp_sync(
        &self, ship_id: BallId, _type_id: u32, _radius: f64, _mass: f64,
        _inertia: f64, _max_velocity: f64, warp_speed_au_s: f64, current_pos: DVec3, dest: DVec3,
    ) {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        if !bp.balls.contains_key(&ship_id) {
            // Do not synthesize a phantom ball from stale disconnected-ship state if the ship
            // already transitioned out of this solar system.
            return;
        }
        let needs_warp_init = bp.balls.get(&ship_id).is_some_and(|b| {
            !matches!(b.mode, MovementMode::Warp { .. }) && (b.position - dest).length() > 5_000.0
        });
        let warp_started = if needs_warp_init {
            let start_pos = bp.balls.get(&ship_id).map(|b| b.position).unwrap_or(current_pos);
            let ok = bp
                .set_mode(ship_id, MovementMode::new_warp(start_pos, dest, warp_speed_au_s.max(1.5)))
                .is_ok();
            if ok {
                if let Some(cs) = bp.combat_states.get_mut(&ship_id) {
                    cs.is_emergency_warping = true;
                }
            }
            ok
        } else {
            false
        };
        sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
        if warp_started {
            let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
            super::protection::sync_ball_protection_in_actor(&bp, &mut st, ship_id);
            st.warp_state.clear_ball(ship_id, true);
            st.warp_state.provenance.insert(ship_id, super::WarpProvenance::DisconnectedEmergencyWarp);
        }
        drop(bp);
        if warp_started {
            self.publish_view();
        }
    }

    pub fn reconcile_disconnected_warp_sync(&self, ship_id: BallId, dest: DVec3) -> (DVec3, bool) {
        let mut bp = self.ballpark.write().unwrap_or_else(|e| e.into_inner());
        let arrived = bp.balls.get(&ship_id).is_none_or(|b| {
            !matches!(b.mode, MovementMode::Warp { .. }) || (b.position - dest).length() <= 2_500.0
        });
        if arrived {
            if bp.balls.get(&ship_id).is_some_and(|b| {
                matches!(b.mode, MovementMode::Warp { .. })
                    || (!bp.is_ball_scrambled(ship_id) && (b.position - dest).length() <= 50_000.0)
            }) {
                let _ = bp.set_ball_position(ship_id, dest);
            }
            let _ = bp.set_mode(ship_id, MovementMode::Stop);
            if let Some(b) = bp.balls.get_mut(&ship_id) {
                b.velocity = DVec3::ZERO;
                b.speed_fraction = 0.0;
            }
        }
        sync_ball_positions(&bp, &self.ball_positions, &self.scrambled_balls);
        let final_pos = bp.balls.get(&ship_id).map(|b| b.position).unwrap_or(dest);
        {
            let mut st = self.actor_state.write().unwrap_or_else(|e| e.into_inner());
            super::protection::finish_disconnected_warp_protection_sync(&mut bp, &mut st, ship_id);
            if arrived {
                st.warp_state.clear_ball(ship_id, false);
            }
            self.publish_view_from(&bp, Some(&st));
        }
        drop(bp);
        (final_pos, arrived)
    }
}

pub(super) fn snapshot_player_ship_capacitors(
    ballpark: &Ballpark,
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
) -> HashMap<BallId, (CharacterId, f64)> {
    let mut out = HashMap::new();
    for (&bid, cs) in &ballpark.combat_states {
        if ballpark.is_static_ball(bid) || !cs.capacitor.is_finite() {
            continue;
        }
        if ballpark
            .ball_slim_meta
            .get(&bid)
            .is_some_and(|m| m.category_id == 11 || m.category_id == 22)
        {
            continue;
        }
        if let Some(cid) = resolve_ball_pilot_char_id(ballpark, db_opt, bid) {
            out.insert(bid, (cid, cs.capacitor));
        }
    }
    out
}

pub(super) fn record_player_ship_capacitor_deltas(
    ballpark: &Ballpark,
    pre_tick_caps: &HashMap<BallId, (CharacterId, f64)>,
    pending_cycle_actions: &mut Vec<super::PendingCycleAction>,
) {
    for (&bid, &(char_id, old_cap)) in pre_tick_caps {
        let Some(cs) = ballpark.combat_states.get(&bid) else {
            continue;
        };
        if !cs.capacitor.is_finite() || !cs.max_capacitor.is_finite() {
            continue;
        }
        let delta = (cs.capacitor - old_cap).abs();
        let hit_empty = old_cap > 1e-6 && cs.capacitor <= 1e-6;
        let hit_full =
            cs.max_capacitor > 0.0 && old_cap < cs.max_capacitor - 1e-6 && cs.capacitor >= cs.max_capacitor - 1e-6;
        if delta >= 0.25 || hit_empty || hit_full {
            pending_cycle_actions.push(super::PendingCycleAction::ShipCapacitorChanged {
                char_id,
                ship_id: bid,
                new_cap: cs.capacitor,
                max_cap: cs.max_capacitor,
                old_cap,
                notified: false,
            });
        }
    }
}

fn resolve_ball_pilot_char_id(
    ballpark: &eve_destiny::Ballpark,
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    ball_id: i64,
) -> Option<i64> {
    ballpark
        .ball_slim_meta
        .get(&ball_id)
        .and_then(|m| m.char_id)
        .or_else(|| {
            db_opt.and_then(|db| {
                db.with_snapshot(|s| {
                    s.characters
                        .values()
                        .find(|c| c.active_ship_id == ball_id)
                        .map(|c| c.character_id)
                        .or_else(|| {
                            s.items
                                .get(&ball_id)
                                .map(|i| i.owner_id)
                                .filter(|oid| s.characters.contains_key(oid))
                        })
                })
            })
        })
        .filter(|&c| c > 0)
}

fn resolve_bound_ballpark_scene(
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    cid: i64,
    uid: Option<u64>,
    scene: BallparkScene,
) -> Option<BallparkScene> {
    let ep_opt = scene.scene_epoch().or_else(|| {
        db_opt
            .zip(uid)
            .and_then(|(db, u)| db.active_scene_epoch_for_char_and_user(cid, u))
    });
    match ep_opt {
        Some(ep) => BallparkScene::try_with_epoch(scene.solar_system_id(), ep),
        None => BallparkScene::try_solar_system(scene.solar_system_id()),
    }
}

fn enqueue_target_lost_typed<N: TypedNotification>(
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    pending: &dyn AnyRwLock<
        std::collections::HashMap<i64, Vec<crate::router::PendingMachoNotification>>,
    >,
    notify_tx_opt: Option<
        &tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>,
    >,
    now: i64,
    cid: i64,
    id_type: &str,
    scene: BallparkScene,
    notif: N,
) {
    if cid <= 0 {
        return;
    }
    let uid = db_opt.and_then(|db| db.with_snapshot(|s| s.characters.get(&cid).map(|c| c.user_id)));
    let Some(bound_scene) = resolve_bound_ballpark_scene(db_opt, cid, uid, scene) else {
        return;
    };
    let pending_notif = build_pending_typed_notification_in_ballpark(
        cid,
        uid,
        id_type,
        now,
        bound_scene,
        notif,
    );
    crate::router::cache_notifications::push_or_stage_pending_notification(
        Some(pending),
        notify_tx_opt,
        pending_notif,
        None,
    );
}

fn on_target_multi_event(mode: &str, ball_id: i64) -> OnMultiEvent {
    OnMultiEvent {
        events: vec![eve_proto::PyRep::Tuple(vec![
            eve_proto::PyRep::Tuple(vec![
                eve_proto::PyRep::String("OnTarget".into()),
                eve_proto::PyRep::String(mode.into()),
                eve_proto::PyRep::Integer(ball_id),
                eve_proto::PyRep::String(String::new()),
            ]),
            eve_proto::PyRep::Integer(0),
        ])],
    }
}

pub(crate) fn flush_target_lost_notifications(
    ballpark: &eve_destiny::Ballpark,
    events: &[eve_destiny::DestinyEvent],
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    pending_opt: Option<
        &dyn AnyRwLock<
            std::collections::HashMap<i64, Vec<crate::router::PendingMachoNotification>>,
        >,
    >,
    notify_tx_opt: Option<
        &tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>,
    >,
) {
    let Some(pending) = pending_opt else {
        return;
    };
    let now = db_opt
        .map(|db| db.clock().now_blueos_filetime_ticks())
        .unwrap_or(133_500_000_000_000_000);
    let Some(scene) = BallparkScene::try_solar_system(ballpark.solar_system_id) else {
        return;
    };
    let enqueue_otherlost = |t_char: i64, source_id: i64| {
        enqueue_target_lost_typed(
            db_opt,
            pending,
            notify_tx_opt,
            now,
            t_char,
            OnMultiEvent::DEFAULT_ID_TYPE,
            scene,
            on_target_multi_event("otherlost", source_id),
        );
    };
    for ev in events {
        if let eve_destiny::DestinyEvent::TargetLockFailed {
            source_id,
            target_id,
        } = ev
        {
            let source_char_id = resolve_ball_pilot_char_id(ballpark, db_opt, *source_id);
            let target_char_id = resolve_ball_pilot_char_id(ballpark, db_opt, *target_id)
                .filter(|&c| Some(c) != source_char_id);
            if let Some(cid) = source_char_id {
                enqueue_target_lost_typed(
                    db_opt,
                    pending,
                    notify_tx_opt,
                    now,
                    cid,
                    FailLockTarget::DEFAULT_ID_TYPE,
                    scene,
                    FailLockTarget {
                        target_id: *target_id,
                    },
                );
            }
            if let Some(t_char) = target_char_id {
                enqueue_otherlost(t_char, *source_id);
            }
            continue;
        }
        if let eve_destiny::DestinyEvent::OnTarget {
            mode,
            source_id,
            target_id,
        } = ev
        {
            if mode != "lost" {
                continue;
            }
            let source_char_id = resolve_ball_pilot_char_id(ballpark, db_opt, *source_id);
            let target_char_id = resolve_ball_pilot_char_id(ballpark, db_opt, *target_id)
                .filter(|&c| Some(c) != source_char_id);
            if let Some(cid) = source_char_id {
                enqueue_target_lost_typed(
                    db_opt,
                    pending,
                    notify_tx_opt,
                    now,
                    cid,
                    OnTarget::DEFAULT_ID_TYPE,
                    scene,
                    OnTarget {
                        scene,
                        arg0: eve_proto::PyRep::Integer(*target_id),
                        arg1: eve_proto::PyRep::String("lost".into()),
                        reason: None,
                    },
                );
                enqueue_target_lost_typed(
                    db_opt,
                    pending,
                    notify_tx_opt,
                    now,
                    cid,
                    OnMultiEvent::DEFAULT_ID_TYPE,
                    scene,
                    on_target_multi_event("lost", *target_id),
                );
            }
            if let Some(t_char) = target_char_id {
                enqueue_otherlost(t_char, *source_id);
            }
        }
    }
}

/// Immediately clears all locks and stops every attacker's target-bound module cycles
/// targeting any ball in `destroyed_ids` within the same tick (`#1082`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn teardown_attacker_locks_and_cycles_for_destroyed_balls_collecting(
    solar_system_id: SolarSystemId,
    ballpark: &mut Ballpark,
    active_module_cycles: &mut Vec<ActiveModuleCycle>,
    base_scramble_strengths: &HashMap<BallId, i32>,
    base_core_strengths: &HashMap<BallId, i32>,
    db_opt: &Option<Arc<eve_db::WorldRepository>>,
    pending_opt: Option<
        &dyn AnyRwLock<
            std::collections::HashMap<i64, Vec<crate::router::PendingMachoNotification>>,
        >,
    >,
    notify_tx_opt: Option<
        &tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>,
    >,
    deactivation_hook: Option<&ModuleDeactivationHook>,
    destroyed_ids: &[BallId],
    post_unlock: &mut super::post_unlock::PostUnlockOps,
) {
    if destroyed_ids.is_empty() {
        return;
    }
    let mut lost_events = Vec::new();
    let mut attacker_targets: Vec<(BallId, BallId)> = Vec::new();
    for &dead_id in destroyed_ids {
        let source_ids: Vec<BallId> = ballpark.target_locks.keys().copied().collect();
        for source_id in source_ids {
            if let Some(locks) = ballpark.target_locks.get_mut(&source_id) {
                let mut had_locked = false;
                let mut had_pending = false;
                let mut had_any = false;
                locks.retain(|l| {
                    if l.target_id == dead_id {
                        had_any = true;
                        if l.locked {
                            had_locked = true;
                        } else {
                            had_pending = true;
                        }
                        false
                    } else {
                        true
                    }
                });
                if had_any {
                    attacker_targets.push((source_id, dead_id));
                    if had_locked {
                        lost_events.push(eve_destiny::DestinyEvent::OnTarget {
                            mode: "lost".to_string(),
                            source_id,
                            target_id: dead_id,
                        });
                    }
                    if had_pending {
                        lost_events.push(eve_destiny::DestinyEvent::TargetLockFailed {
                            source_id,
                            target_id: dead_id,
                        });
                    }
                }
            }
        }
        for cycle in active_module_cycles.iter() {
            if cycle.target_id == Some(dead_id) {
                attacker_targets.push((cycle.ship_id, dead_id));
                if let Some((carrier_id, _)) =
                    super::cycles_tackle::parse_siren_tackle_ids(&cycle.effect_name)
                {
                    attacker_targets.push((carrier_id, dead_id));
                }
            }
        }
    }
    attacker_targets.sort_unstable();
    attacker_targets.dedup();
    for (source_id, dead_id) in attacker_targets {
        super::cycles_tackle::on_target_unlocked_in_actor_collecting(
            solar_system_id,
            ballpark,
            active_module_cycles,
            base_scramble_strengths,
            base_core_strengths,
            db_opt,
            source_id,
            Some(dead_id),
            deactivation_hook,
            post_unlock,
        );
    }
    let new_lost_count = lost_events.len();
    for ev in &ballpark.pending_events {
        if let eve_destiny::DestinyEvent::OnTarget { mode, source_id, target_id } = ev {
            if mode == "lost" && destroyed_ids.contains(target_id) {
                let already = lost_events.iter().any(|le| {
                    matches!(
                        le,
                        eve_destiny::DestinyEvent::OnTarget { source_id: ls, target_id: lt, .. }
                        if ls == source_id && lt == target_id
                    )
                });
                if !already {
                    lost_events.push(ev.clone());
                }
            }
        }
    }
    if !lost_events.is_empty() {
        flush_target_lost_notifications(
            ballpark,
            &lost_events,
            db_opt.as_ref(),
            pending_opt,
            notify_tx_opt,
        );
        ballpark.pending_events.extend(
            lost_events
                .into_iter()
                .take(new_lost_count)
                .filter(|e| !matches!(e, eve_destiny::DestinyEvent::TargetLockFailed { .. })),
        );
    }
}

/// Returns `(telemetry_kind, ship_id)` when `notif` is a periodic ship-capacitor update
/// whose stale queued snapshots can be superseded or evicted ahead of state notifications.
fn capacitor_telemetry_key(n: &crate::router::PendingMachoNotification) -> Option<(&'static str, i64)> {
    if n.event == "OnCapacitorChange" {
        return n
            .args
            .first()
            .and_then(eve_proto::PyRep::as_i64)
            .map(|sid| ("OnCapacitorChange", sid));
    }
    if n.event == "OnModuleAttributeChanges" {
        if let [eve_proto::PyRep::List(entries)] = n.args.as_slice() {
            if let [eve_proto::PyRep::Tuple(t)] = entries.as_slice() {
                if t.len() >= 6
                    && t[0].as_str() == Some("OnModuleAttributeChange")
                    && t[3].as_i64() == Some(18)
                {
                    return t[2]
                        .as_i64()
                        .map(|sid| ("OnModuleAttributeChanges:18", sid));
                }
            }
        }
    }
    None
}

#[cfg(test)]
fn enqueue_cycle_char_notification<N: TypedNotification>(
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    pending: &dyn AnyRwLock<
        std::collections::HashMap<i64, Vec<crate::router::PendingMachoNotification>>,
    >,
    notify_tx_opt: Option<
        &tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>,
    >,
    now: i64,
    cid: i64,
    notif: N,
) {
    enqueue_cycle_char_notification_with_scope(
        db_opt,
        pending,
        notify_tx_opt,
        now,
        cid,
        None,
        notif,
    );
}

fn enqueue_cycle_char_notification_with_scope<N: TypedNotification>(
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    pending: &dyn AnyRwLock<
        std::collections::HashMap<i64, Vec<crate::router::PendingMachoNotification>>,
    >,
    notify_tx_opt: Option<
        &tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>,
    >,
    now: i64,
    cid: i64,
    scene_opt: Option<BallparkScene>,
    notif: N,
) {
    let uid = db_opt.and_then(|db| db.with_snapshot(|s| s.characters.get(&cid).map(|c| c.user_id)));
    let notif = match scene_opt {
        Some(scene) => {
            let Some(bound_scene) = resolve_bound_ballpark_scene(db_opt, cid, uid, scene) else {
                return;
            };
            build_pending_typed_notification_in_ballpark(
                cid,
                uid,
                N::DEFAULT_ID_TYPE,
                now,
                bound_scene,
                notif,
            )
        }
        None => {
            if N::IS_BALLPARK_SCOPED {
                return;
            }
            crate::router::notify_registry::build_pending_typed_notification(
                cid,
                uid,
                N::DEFAULT_ID_TYPE,
                now,
                notif,
            )
        }
    };
    {
        let mut q_guard = pending.write().unwrap_or_else(|e| e.into_inner());
        let q = q_guard.entry(cid).or_default();
        if let Some(key) = capacitor_telemetry_key(&notif) {
            // Keep only the latest unread capacitor telemetry per ship so passive regen ticks
            // do not fill the character queue and evict discrete state notifications (#1156).
            q.retain(|existing| capacitor_telemetry_key(existing) != Some(key));
        }
        if q.len() >= crate::router::MAX_PENDING_NOTIFICATIONS_PER_CHAR {
            if let Some(evict_idx) = q
                .iter()
                .position(|existing| capacitor_telemetry_key(existing).is_some())
            {
                q.remove(evict_idx);
            } else {
                q.remove(0);
            }
        }
        q.push(notif.clone());
    }
    if let Some(tx) = notify_tx_opt {
        let _ = tx.send((cid, notif));
    }
}

pub(crate) fn flush_pending_cycle_damage_notifications_with_ballpark(
    ballpark: &Ballpark,
    sde_opt: Option<&eve_sde::SdeRegistry>,
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    pending_opt: Option<
        &dyn AnyRwLock<
            std::collections::HashMap<i64, Vec<crate::router::PendingMachoNotification>>,
        >,
    >,
    notify_tx_opt: Option<
        &tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>,
    >,
    pending_cycle_actions: &mut [super::PendingCycleAction],
) {
    let scene_opt = BallparkScene::try_solar_system(ballpark.solar_system_id);
    if let (Some(pending), Some(scene)) = (pending_opt, scene_opt) {
        let now = db_opt
            .map(|db| db.clock().now_blueos_filetime_ticks())
            .unwrap_or(133_500_000_000_000_000);
        for action in pending_cycle_actions.iter() {
            let super::PendingCycleAction::CycleWeaponDamage {
                attacker_char_id,
                target_id,
                target_char_id,
                applied_dmg,
                notified,
                ..
            } = action
            else {
                continue;
            };
            if *notified || *applied_dmg <= 0.0 {
                continue;
            }
            let (sr, ar, hr) = if let Some(t_cs) = ballpark.combat_states.get(target_id) {
                (
                    (t_cs.shield_hp / t_cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0),
                    (t_cs.armor_hp / t_cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0),
                    (t_cs.structure_hp / t_cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0),
                )
            } else {
                (0.0, 0.0, 0.0)
            };
            let damage_tuple =
                crate::services::notify_emitters_space_dungeons::build_damage_state_tuple_raw(
                    db_opt.map(|d| d.as_ref()),
                    sde_opt,
                    *target_id,
                    sr,
                    ar,
                    hr,
                );
            let owner_cid = attacker_char_id.unwrap_or(0);
            if let Some(acid) = attacker_char_id.filter(|&c| c > 0) {
                enqueue_cycle_char_notification_with_scope(
                    db_opt,
                    pending,
                    notify_tx_opt,
                    now,
                    acid,
                    Some(scene),
                    OnDamageStateChange {
                        scene,
                        ship_id: *target_id,
                        damage_state: damage_tuple.clone(),
                    },
                );
            }
            if let Some(vcid) = target_char_id.filter(|&c| c > 0 && c != owner_cid) {
                enqueue_cycle_char_notification_with_scope(
                    db_opt,
                    pending,
                    notify_tx_opt,
                    now,
                    vcid,
                    Some(scene),
                    OnDamageStateChange {
                        scene,
                        ship_id: *target_id,
                        damage_state: damage_tuple,
                    },
                );
            }
        }
    }
    flush_pending_cycle_damage_notifications_in_scene(
        db_opt,
        pending_opt,
        notify_tx_opt,
        scene_opt,
        pending_cycle_actions,
    );
}

/// Builds the shared V24 `OnDamageMessage` / `OnDamageMessages` payload dictionary.
pub(crate) fn build_cycle_damage_message_dict(
    damage: f64,
    attacker_ship_id: BallId,
    target_id: BallId,
    owner_cid: CharacterId,
    module_type_id: u32,
    hit_quality: i64,
    attack_type: &str,
) -> eve_proto::PyRep {
    let mut entries = vec![
        (
            eve_proto::PyRep::String("damage".into()),
            eve_proto::PyRep::Real(damage),
        ),
        (
            eve_proto::PyRep::String("source".into()),
            eve_proto::PyRep::Integer(attacker_ship_id),
        ),
        (
            eve_proto::PyRep::String("attackerID".into()),
            eve_proto::PyRep::Integer(attacker_ship_id),
        ),
        (
            eve_proto::PyRep::String("target".into()),
            eve_proto::PyRep::Integer(target_id),
        ),
        (
            eve_proto::PyRep::String("target_ID".into()),
            eve_proto::PyRep::Integer(target_id),
        ),
        (
            eve_proto::PyRep::String("weapon".into()),
            eve_proto::PyRep::Integer(i64::from(module_type_id)),
        ),
        (
            eve_proto::PyRep::String("hitQuality".into()),
            eve_proto::PyRep::Integer(hit_quality),
        ),
        (
            eve_proto::PyRep::String("attackType".into()),
            eve_proto::PyRep::String(attack_type.into()),
        ),
        (
            eve_proto::PyRep::String("isWrecking".into()),
            eve_proto::PyRep::Bool(false),
        ),
        (
            eve_proto::PyRep::String("isBanked".into()),
            eve_proto::PyRep::Bool(false),
        ),
    ];
    // Why: NPC entities have no character owner (`owner_cid == 0`); omit `"owner"` so the client resolves NPC names from `"source"`/`"attackerID"` (#1247).
    if owner_cid > 0 {
        entries.push((
            eve_proto::PyRep::String("owner".into()),
            eve_proto::PyRep::Tuple(vec![
                eve_proto::PyRep::Integer(2),
                eve_proto::PyRep::Integer(owner_cid),
            ]),
        ));
    }
    eve_proto::PyRep::Dict(entries)
}

struct CycleNotifySink<'a> {
    scene_opt: Option<BallparkScene>,
    db_opt: Option<&'a std::sync::Arc<eve_db::WorldRepository>>,
    pending: &'a dyn AnyRwLock<
        std::collections::HashMap<i64, Vec<crate::router::PendingMachoNotification>>,
    >,
    notify_tx_opt: Option<
        &'a tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>,
    >,
    now: i64,
}

impl CycleNotifySink<'_> {
    fn push<N: TypedNotification>(&self, cid: i64, notif: N) {
        enqueue_cycle_char_notification_with_scope(
            self.db_opt,
            self.pending,
            self.notify_tx_opt,
            self.now,
            cid,
            self.scene_opt,
            notif,
        );
    }
}

fn enqueue_cycle_weapon_log_pair(
    sink: &CycleNotifySink<'_>,
    scene: BallparkScene,
    recipient_cid: CharacterId,
    msg_key: &str,
    dict: eve_proto::PyRep,
) {
    let msg_tuple = vec![eve_proto::PyRep::String(msg_key.into()), dict.clone()];
    sink.push(
        recipient_cid,
        OnDamageMessage {
            scene,
            damage_messages_args: dict,
        },
    );
    sink.push(
        recipient_cid,
        OnDamageMessages {
            scene,
            dmgmsgs: vec![eve_proto::PyRep::Tuple(msg_tuple)],
        },
    );
}

fn flush_cycle_weapon_log_entries(
    sink: &CycleNotifySink<'_>,
    attacker_char_id: Option<CharacterId>,
    attacker_ship_id: BallId,
    target_id: BallId,
    target_char_id: Option<CharacterId>,
    module_type_id: u32,
    hit_damage: Option<f64>,
) {
    let Some(scene) = sink.scene_opt else {
        return;
    };
    let (damage, hit_quality, attacker_msg_key, victim_msg_key) = match hit_damage {
        Some(dmg) => (dmg, 3, "AttackHit", "AttackedBy"),
        None => (0.0, 0, "AttackMiss", "AttackMissBy"),
    };
    let owner_cid = attacker_char_id.unwrap_or(0);
    if let Some(acid) = attacker_char_id.filter(|&c| c > 0) {
        let dmg_dict = build_cycle_damage_message_dict(
            damage,
            attacker_ship_id,
            target_id,
            acid,
            module_type_id,
            hit_quality,
            "me",
        );
        enqueue_cycle_weapon_log_pair(sink, scene, acid, attacker_msg_key, dmg_dict);
    }
    if let Some(vcid) = target_char_id.filter(|&c| c > 0 && c != owner_cid) {
        let vic_dict = build_cycle_damage_message_dict(
            damage,
            attacker_ship_id,
            target_id,
            owner_cid,
            module_type_id,
            hit_quality,
            "other",
        );
        enqueue_cycle_weapon_log_pair(sink, scene, vcid, victim_msg_key, vic_dict);
    }
}

#[cfg(test)]
pub(crate) fn flush_pending_cycle_damage_notifications(
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    pending_opt: Option<
        &dyn AnyRwLock<
            std::collections::HashMap<i64, Vec<crate::router::PendingMachoNotification>>,
        >,
    >,
    notify_tx_opt: Option<
        &tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>,
    >,
    pending_cycle_actions: &mut [super::PendingCycleAction],
) {
    let test_sys = db_opt
        .and_then(|db| db.with_snapshot(|s| s.characters.values().next().map(|c| c.solar_system_id)))
        .filter(|&s| s > 0)
        .unwrap_or(30_000_142);
    flush_pending_cycle_damage_notifications_in_scene(
        db_opt,
        pending_opt,
        notify_tx_opt,
        BallparkScene::try_solar_system(test_sys),
        pending_cycle_actions,
    );
}

fn flush_pending_cycle_damage_notifications_in_scene(
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    pending_opt: Option<
        &dyn AnyRwLock<
            std::collections::HashMap<i64, Vec<crate::router::PendingMachoNotification>>,
        >,
    >,
    notify_tx_opt: Option<
        &tokio::sync::broadcast::Sender<(i64, crate::router::PendingMachoNotification)>,
    >,
    scene_opt: Option<BallparkScene>,
    pending_cycle_actions: &mut [super::PendingCycleAction],
) {
    let Some(pending) = pending_opt else {
        return;
    };
    let now = db_opt
        .map(|db| db.clock().now_blueos_filetime_ticks())
        .unwrap_or(133_500_000_000_000_000);
    let sink = CycleNotifySink {
        scene_opt,
        db_opt,
        pending,
        notify_tx_opt,
        now,
    };
    for action in pending_cycle_actions.iter_mut() {
        if let super::PendingCycleAction::ShipCapacitorChanged {
            char_id,
            ship_id,
            new_cap,
            max_cap,
            old_cap,
            notified,
        } = action
        {
            if *notified || *char_id <= 0 || *ship_id <= 0 || !new_cap.is_finite() || !max_cap.is_finite() {
                continue;
            }
            *notified = true;
            sink.push(
                *char_id,
                OnCapacitorChange {
                    ship_id: *ship_id,
                    new_cap: *new_cap,
                    max_cap: *max_cap,
                },
            );
            sink.push(
                *char_id,
                OnModuleAttributeChanges {
                    changes: eve_proto::PyRep::List(vec![
                        crate::router::ServerContext::build_ship_capacitor_attr_change_rep(
                            *char_id,
                            *ship_id,
                            crate::router::host_wall_blueos_filetime_ticks(),
                            *new_cap,
                            if old_cap.is_finite() { *old_cap } else { *new_cap },
                        ),
                    ]),
                },
            );
            continue;
        }
        if let super::PendingCycleAction::AutoReloadStarted {
            char_id,
            module_ids,
            charge_type_id,
            time_ms,
            notified,
            ..
        } = action
        {
            if *notified || *char_id <= 0 || module_ids.is_empty() {
                continue;
            }
            *notified = true;
            sink.push(
                *char_id,
                OnChargeBeingLoadedToModule {
                    item_ids: module_ids.clone(),
                    charge_type_id: i64::from(*charge_type_id),
                    time_ms: *time_ms,
                },
            );
            continue;
        }
        if let super::PendingCycleAction::NoChargesDeactivated {
            char_id,
            module_type_id,
            notified,
        } = action
        {
            if *notified || *char_id <= 0 {
                continue;
            }
            *notified = true;
            let dict = eve_proto::PyRep::Dict(vec![(
                eve_proto::PyRep::String("type".into()),
                eve_proto::PyRep::Tuple(vec![
                    eve_proto::PyRep::Integer(4),
                    eve_proto::PyRep::Integer(i64::from(*module_type_id)),
                ]),
            )]);
            sink.push(
                *char_id,
                OnRemoteMessage {
                    msg_id: "NoCharges".into(),
                    dict: Some(dict),
                    kw: None,
                },
            );
            continue;
        }
        if let super::PendingCycleAction::CycleWeaponDamage {
            attacker_char_id,
            attacker_ship_id,
            target_id,
            target_char_id,
            module_type_id,
            applied_dmg,
            notified,
            ..
        } = action
        {
            if *notified || *applied_dmg <= 0.0 {
                continue;
            }
            *notified = true;
            flush_cycle_weapon_log_entries(
                &sink,
                *attacker_char_id,
                *attacker_ship_id,
                *target_id,
                *target_char_id,
                *module_type_id,
                Some(*applied_dmg),
            );
            continue;
        }
        if let super::PendingCycleAction::CycleWeaponMiss {
            attacker_char_id,
            attacker_ship_id,
            target_id,
            target_char_id,
            module_type_id,
            notified,
        } = action
        {
            if *notified {
                continue;
            }
            *notified = true;
            flush_cycle_weapon_log_entries(
                &sink,
                *attacker_char_id,
                *attacker_ship_id,
                *target_id,
                *target_char_id,
                *module_type_id,
                None,
            );
        }
    }
}

#[cfg(test)]
#[path = "cycles_tests.rs"]
mod cycles_tests;

#[cfg(test)]
mod remote_cap_transfer_tests {
    use super::*;
    use eve_destiny::{Ball, Ballpark, CombatState, MovementMode};
    use eve_sde::SdeRegistry;
    use glam::DVec3;
    use std::collections::HashMap;
    use std::sync::{Arc, RwLock};

    #[test]
    fn remote_cap_transfer_rollover_emits_career_progress_event() {
        let mut bp = Ballpark::new(30_000_142);
        let src_ship = 91_001_i64;
        let tgt_dread = 91_002_i64;
        let blocked_tgt = 91_003_i64;
        for (id, tid, pos) in [
            (src_ship, 587_u32, DVec3::ZERO),
            (tgt_dread, 19_720_u32, DVec3::new(1_000.0, 0.0, 0.0)),
            (blocked_tgt, 19_720_u32, DVec3::new(1_500.0, 0.0, 0.0)),
        ] {
            bp.add_ball(Ball {
                id,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 1.0,
                mass: 1_000_000.0,
                radius: 40.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            });
        }
        bp.combat_states.insert(
            src_ship,
            CombatState {
                capacitor: 5_000.0,
                max_capacitor: 5_000.0,
                ..CombatState::default()
            },
        );
        bp.combat_states.insert(
            tgt_dread,
            CombatState {
                capacitor: 0.0,
                max_capacitor: 120_000.0,
                ..CombatState::default()
            },
        );
        bp.combat_states.insert(
            blocked_tgt,
            CombatState {
                capacitor: 0.0,
                max_capacitor: 120_000.0,
                disallow_assistance: true,
                ..CombatState::default()
            },
        );
        let mut cycles = vec![
            ActiveModuleCycle {
                ship_id: src_ship,
                owner_char_id: Some(90_000_001),
                module_id: 92_001,
                module_type_id: 3558,
                target_id: Some(tgt_dread),
                remote_cap_amount: 500.0,
                remote_optimal: 10_000.0,
                remote_falloff: 5_000.0,
                duration_s: 1.0,
                repeat: 1,
                ..Default::default()
            },
            ActiveModuleCycle {
                ship_id: src_ship,
                owner_char_id: Some(90_000_001),
                module_id: 92_002,
                module_type_id: 3558,
                target_id: Some(blocked_tgt),
                remote_cap_amount: 500.0,
                remote_optimal: 10_000.0,
                remote_falloff: 5_000.0,
                duration_s: 1.0,
                repeat: 1,
                ..Default::default()
            },
        ];
        let mut pending_disruptors = Vec::new();
        let mut nanite_repairs = Vec::new();
        let mut heat = HashMap::new();
        let mut rah_accum = HashMap::new();
        let mut pending_actions = Vec::new();
        let sde_opt: Option<Arc<SdeRegistry>> = None;
        let db_opt: Option<Arc<eve_db::WorldRepository>> = None;
        let rng_opt: Option<Arc<crate::rng::MechanicRng>> = None;
        step_cycles_and_capacitor(
            bp.solar_system_id,
            &mut bp,
            &mut cycles,
            &mut pending_disruptors,
            &mut nanite_repairs,
            &mut heat,
            &mut rah_accum,
            &HashMap::new(),
            &HashMap::new(),
            &sde_opt,
            &db_opt,
            &rng_opt,
            &Arc::new(RwLock::new(Default::default())),
            &mut pending_actions,
            1.0,
            None,
        );
        assert!(pending_actions.iter().any(|a| matches!(
            a,
            PendingCycleAction::CareerProgressEvent {
                char_id: 90_000_001,
                source_ship_id,
                target_id,
                activity_kind: "cap_warfare",
                sub_kind: "cap_transfer",
                amount,
            } if *source_ship_id == src_ship && *target_id == tgt_dread && *amount > 400.0
        )));
        assert!(!pending_actions.iter().any(|a| matches!(
            a,
            PendingCycleAction::CareerProgressEvent {
                target_id,
                sub_kind: "cap_transfer",
                ..
            } if *target_id == blocked_tgt
        )));
    }
}
