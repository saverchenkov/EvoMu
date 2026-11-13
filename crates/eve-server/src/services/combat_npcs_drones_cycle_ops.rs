//! Drone combat cycle execution, EWAR immunity & probabilistic ECM jam evaluation,
//! and ship/capsule death pipeline hooks (`#494`, `#495`).

#![forbid(unsafe_code)]

use super::*;
use eve_destiny::{Ball, MovementMode, TargetLock};

/// Returns `true` when `target_id` is immune to hostile drone EWAR/ECM (`#495`):
/// - Upwell structures (`is_structure_immune_to_ewar` / `category_id == 65`) and POS towers (`group_id == 365`)
/// - Supercapitals (`group_id == 30` Titan, `659` Supercarrier)
/// - Ships in active `self_ewar_immune` cycles or active Bastion (`33400`), Siege (`20280 | 4292`),
///   Triage (`27951 | 4294`), Industrial Core (`28583 | 58945`), or `group_id == 515`
/// - Ships with `disallowOffensiveModifiers` (`1045 > 0.0`) or `0.0` evaluated EWAR resistance (`2045`).
pub(crate) async fn is_target_immune_to_drone_ewar(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    target_id: i64,
    target_type_id: u32,
) -> bool {
    if ctx.db.with_snapshot(|s| {
        crate::services::structure_acl_defense_rules::is_structure_immune_to_ewar(s, target_id)
    }) {
        return true;
    }
    if let Some(t) = ctx.sde.types.get(&target_type_id) {
        if t.category_id == 65
            || matches!(t.group_id, 30 | 365 | 659)
            || t.default_attributes.get(&1045).copied().unwrap_or(0.0) > 0.0
        {
            return true;
        }
    }
    if sys_handle
        .actor_state
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .active_module_cycles
        .iter()
        .any(|c| c.ship_id == target_id && c.self_ewar_immune)
    {
        return true;
    }

    let owner_and_active_bastion_or_siege = ctx.db.with_snapshot(|snap| {
        let has_active_siege_like = snap.items.values().any(|it| {
            it.location_id == target_id
                && snap.module_active_states.contains(&it.item_id)
                && (matches!(it.type_id, 4292 | 4294 | 20280 | 27951 | 28583 | 33400 | 58945)
                    || ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 515))
        });
        let owner_opt = snap
            .characters
            .values()
            .find(|c| c.active_ship_id == target_id)
            .map(|c| c.character_id)
            .or_else(|| {
                snap.items
                    .get(&target_id)
                    .map(|i| i.owner_id)
                    .filter(|oid| snap.characters.contains_key(oid))
            });
        (has_active_siege_like, owner_opt)
    });
    if owner_and_active_bastion_or_siege.0 {
        return true;
    }
    if let Some(owner_id) = owner_and_active_bastion_or_siege.1 {
        if let Ok(t_fit) = ctx.get_or_rebuild_ship_fitting(owner_id, target_id) {
            if t_fit.ship.get_attribute(1045, &ctx.sde) > 0.0 {
                return true;
            }
            if (ctx.sde.attributes.contains_key(&2045)
                || t_fit.ship.base_attributes.contains_key(&2045))
                && t_fit.ship.get_attribute(2045, &ctx.sde) <= 1e-4
            {
                return true;
            }
        }
    }
    false
}

/// Evaluates probabilistic `compute_ecm_jam_chance` for an ECM drone (`d.ecm_strength > 0.0`),
/// respecting EWAR immunity and EVE's post-2018 ECM taunt rule (preserving locks on `d.ball_id` only).
pub(crate) async fn try_apply_drone_ecm_jam(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    d: &DroneSpaceState,
    drone_ball: &Ball,
    target_id: i64,
    target_ball: &Ball,
) -> bool {
    if d.ecm_strength <= 0.0 {
        return false;
    }
    if is_target_immune_to_drone_ewar(ctx, sys_handle, target_id, target_ball.type_id).await {
        return false;
    }
    if let Ok(Some(tcs)) = sys_handle.get_combat_state(target_id).await {
        if tcs.is_cloaked || tcs.is_tethered || tcs.is_invulnerable() {
            return false;
        }
    }

    const SENSOR_ATTRS: [u16; 4] = [211, 209, 210, 208];
    let mut target_sensor_strength = 0.0_f64;
    let mut ecm_resist = 1.0_f64;

    let target_owner_type = ctx.db.with_snapshot(|snap| {
        snap.items
            .get(&target_id)
            .map(|i| (i.owner_id, i.type_id, snap.characters.contains_key(&i.owner_id)))
    });
    if let Some((owner_id, t_tid, is_char_owner)) = target_owner_type {
        if is_char_owner {
            if let Ok(t_fit) = ctx.get_or_rebuild_ship_fitting(owner_id, target_id) {
                for &s_attr in &SENSOR_ATTRS {
                    target_sensor_strength =
                        target_sensor_strength.max(t_fit.ship.get_attribute(s_attr, &ctx.sde));
                }
                if ctx.sde.attributes.contains_key(&2253)
                    || t_fit.ship.base_attributes.contains_key(&2253)
                {
                    ecm_resist = ecm_resist.min(t_fit.ship.get_attribute(2253, &ctx.sde).clamp(0.0, 1.0));
                }
                if ctx.sde.attributes.contains_key(&2116)
                    || t_fit.ship.base_attributes.contains_key(&2116)
                {
                    ecm_resist = ecm_resist.min(t_fit.ship.get_attribute(2116, &ctx.sde).clamp(0.0, 1.0));
                }
            }
        }
        if target_sensor_strength <= 0.0 {
            if let Some(t_type) = ctx.sde.types.get(&t_tid) {
                for &s_attr in &SENSOR_ATTRS {
                    target_sensor_strength = target_sensor_strength
                        .max(t_type.default_attributes.get(&s_attr).copied().unwrap_or(0.0));
                }
            }
        }
    }
    if target_sensor_strength <= 0.0 {
        if let Some(t_type) = ctx.sde.types.get(&target_ball.type_id) {
            for &s_attr in &SENSOR_ATTRS {
                target_sensor_strength = target_sensor_strength
                    .max(t_type.default_attributes.get(&s_attr).copied().unwrap_or(0.0));
            }
        }
    }
    if target_sensor_strength <= 0.0 {
        target_sensor_strength = 10.0;
    }

    let dist = drone_ball.position.distance(target_ball.position);
    let optimal = d.optimal_range.max(5_000.0);
    let falloff = d.effective_falloff_range(ctx);
    let jam_strength = d.ecm_strength * ecm_resist;
    let jam_chance = eve_dogma::compute_ecm_jam_chance(
        jam_strength,
        target_sensor_strength,
        dist,
        optimal,
        falloff,
    );
    let roll = crate::services::inventory_fitting::csprng_unit_f64(ctx);
    if jam_chance <= 0.0 || roll >= jam_chance {
        return false;
    }

    // EVE ECM taunt rule: jammed ship loses all target locks EXCEPT any lock on the jamming drone (`d.ball_id`).
    let taunt_locks: Vec<TargetLock> = sys_handle
        .get_target_locks(target_id)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|l| l.target_id == d.ball_id)
        .collect();
    let jam_dur_s = d.cycle_duration_secs(ctx).max(20) as f64;
    let _ = sys_handle.apply_ecm_jam(target_id, jam_dur_s).await;
    for tl in taunt_locks {
        let _ = sys_handle.add_target_lock(target_id, tl).await;
    }
    with_mut_combat_runtime(ctx, |rt| {
        if let Some(npc) = rt.npc_entities.get_mut(&target_id) {
            npc.ecm_jammed = true;
        }
    });
    true
}

/// Resolves the capsuleer pilot of `target_id` (if `target_id` is a player ship or capsule).
pub(crate) fn resolve_victim_character_for_ball(ctx: &ServerContext, target_id: i64) -> Option<i64> {
    ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .find(|c| c.active_ship_id == target_id)
            .map(|c| c.character_id)
            .or_else(|| {
                snap.items
                    .get(&target_id)
                    .map(|i| i.owner_id)
                    .filter(|oid| snap.characters.contains_key(oid))
            })
    })
}

/// Renews Crimewatch weapons timer and PvP/NPC aggression logoff timers on drone hit (`#495`).
pub(crate) fn renew_drone_aggression_timers(
    ctx: &ServerContext,
    attacker_char_id: i64,
    victim_char_opt: Option<i64>,
    now_unix: u64,
) {
    if let Some(vcid) = victim_char_opt.filter(|&v| v != attacker_char_id) {
        ctx.crimewatch
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .trigger_weapon_timer(attacker_char_id, now_unix);
        crate::sp6_lifecycle::record_pvp_aggression_and_renew_logoff_timer(
            ctx,
            attacker_char_id,
            vcid,
            now_unix,
        );
    } else {
        crate::sp6_lifecycle::record_npc_aggression_timer(ctx, attacker_char_id, now_unix);
    }
}

/// Disengages all drones in `solar_system_id` currently targeting `target_id`.
pub(crate) fn disengage_drones_targeting(ctx: &ServerContext, solar_system_id: u32, target_id: i64) {
    with_mut_combat_runtime(ctx, |rt| {
        for d in rt.active_drones_in_space.values_mut() {
            if d.solar_system_id == solar_system_id && d.target_id == Some(target_id) {
                d.target_id = None;
                d.activity_state = 0;
            }
        }
    });
}

/// Disengages a single drone by `drone_ball_id`.
fn disengage_single_drone(ctx: &ServerContext, drone_ball_id: i64) {
    with_mut_combat_runtime(ctx, |rt| {
        if let Some(d) = rt.active_drones_in_space.get_mut(&drone_ball_id) {
            d.target_id = None;
            d.activity_state = 0;
        }
    });
}

/// Executes per-cycle drone combat volleys on the cluster tick (`feat:drones.engage-target`,
/// `feat:drones.gecko-augmented-mutated-drones`, `feat:drones.gecko-subverted-bandwidth`, `#494`, `#495`).
pub async fn step_engaged_drones(ctx: &ServerContext, now_unix: u64) {
    let due_drones: Vec<DroneSpaceState> = with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .filter(|d| {
                !d.is_abandoned
                    && d.activity_state == 1
                    && d.target_id.is_some()
                    && now_unix >= rt.drone_next_ready_unix.get(&d.ball_id).copied().unwrap_or(0)
            })
            .cloned()
            .collect()
    });
    for d in due_drones {
        let Some(target_id) = d.target_id else {
            continue;
        };
        let sys_handle = ctx.systems.get_or_spawn(d.solar_system_id);
        let parent_cloaked_or_tethered = crate::services::inventory_fitting::fitting_module_cycles::is_ship_actively_cloaked(
            ctx,
            &sys_handle,
            d.parent_ship_id,
        )
        .await
            || sys_handle
                .get_combat_state(d.parent_ship_id)
                .await
                .ok()
                .flatten()
                .is_some_and(|cs| cs.is_tethered);
        if parent_cloaked_or_tethered {
            disengage_single_drone(ctx, d.ball_id);
            continue;
        }
        let Some((_, drone_ball)) = sys_handle.get_ball_snapshot(d.ball_id).await.ok().flatten() else {
            continue;
        };
        let Some((_, target_ball)) = sys_handle.get_ball_snapshot(target_id).await.ok().flatten() else {
            disengage_single_drone(ctx, d.ball_id);
            continue;
        };
        let target_cs_opt = sys_handle.get_combat_state(target_id).await.ok().flatten();
        if target_cs_opt
            .as_ref()
            .is_some_and(|cs| cs.is_cloaked || cs.is_tethered)
        {
            disengage_single_drone(ctx, d.ball_id);
            continue;
        }
        if d.damage_vector.total() > 0.0 {
            let victim_char_opt = resolve_victim_character_for_ball(ctx, target_id);
            let target_sig = target_cs_opt
                .as_ref()
                .map(|cs| cs.signature_radius)
                .unwrap_or(target_ball.radius.max(35.0));
            let hit_mult = d.compute_combat_hit_multiplier(ctx, &drone_ball, &target_ball, target_sig);
            let scaled_dmg = DamageVector {
                em: d.damage_vector.em * hit_mult,
                thermal: d.damage_vector.thermal * hit_mult,
                kinetic: d.damage_vector.kinetic * hit_mult,
                explosive: d.damage_vector.explosive * hit_mult,
            };
            let cyc_s = d.cycle_duration_secs(ctx).max(1) as f64;
            if let Ok(Some((app, destroyed, _))) =
                crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
                    ctx,
                    &sys_handle,
                    d.solar_system_id,
                    d.owner_char_id,
                    target_id,
                    scaled_dmg,
                    cyc_s,
                    now_unix,
                )
                .await
            {
                record_killmail_assist_action(
                    ctx,
                    target_id,
                    d.owner_char_id,
                    d.type_id,
                    d.type_id,
                    app,
                    false,
                    now_unix,
                );
                if app > 0.0 || destroyed {
                    renew_drone_aggression_timers(ctx, d.owner_char_id, victim_char_opt, now_unix);
                }
                if destroyed {
                    disengage_drones_targeting(ctx, d.solar_system_id, target_id);
                    super::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                        ctx,
                        d.solar_system_id,
                        d.owner_char_id,
                        target_id,
                        target_ball.type_id,
                        target_ball.position,
                        now_unix,
                    )
                    .await;
                }
            }
        } else if d.ecm_strength > 0.0 {
            let victim_char_opt = resolve_victim_character_for_ball(ctx, target_id);
            if try_apply_drone_ecm_jam(ctx, &sys_handle, &d, &drone_ball, target_id, &target_ball).await {
                record_killmail_assist_action(
                    ctx,
                    target_id,
                    d.owner_char_id,
                    d.type_id,
                    d.type_id,
                    0.0,
                    true,
                    now_unix,
                );
                renew_drone_aggression_timers(ctx, d.owner_char_id, victim_char_opt, now_unix);
            }
        }
        let tidi = crate::services::serverops_cluster::get_system_tidi_factor(ctx, d.solar_system_id)
            .clamp(0.1, 1.0);
        let scaled_cycle_secs = ((d.cycle_duration_secs(ctx) as f64) / tidi).round().max(1.0) as u64;
        let next_ready = now_unix.saturating_add(scaled_cycle_secs);
        with_mut_combat_runtime(ctx, |rt| {
            rt.drone_next_ready_unix.insert(d.ball_id, next_ready);
        });
    }
}

/// Steps automatic drone aggression (`feat:drones.aggressive-passive`) and fleet Assist / Guard
/// (`feat:drones.assist-guard`, `#494`, `#495`).
pub async fn step_drone_auto_aggression_and_assist(
    ctx: &ServerContext,
    solar_system_id: u32,
    owner_char_id: i64,
    attacker_of_owner_opt: Option<i64>,
    assisted_target_opt: Option<(i64, i64)>,
    attacker_of_guarded_opt: Option<(i64, i64)>,
) -> Vec<i64> {
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let settings = with_combat_runtime(ctx, |rt| {
        rt.drone_settings
            .get(&owner_char_id)
            .cloned()
            .unwrap_or_default()
    });
    let drones: Vec<DroneSpaceState> = with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .filter(|d| {
                d.solar_system_id == solar_system_id
                    && d.owner_char_id == owner_char_id
                    && !d.is_abandoned
                    && d.activity_state != 4
            })
            .cloned()
            .collect()
    });

    let now_unix = ctx.now_unix();
    let mut engaged_drone_balls = Vec::new();
    for mut d in drones {
        if crate::services::inventory_fitting::fitting_module_cycles::is_ship_actively_cloaked(
            ctx,
            &sys_handle,
            d.parent_ship_id,
        )
        .await
            || sys_handle
                .get_combat_state(d.parent_ship_id)
                .await
                .ok()
                .flatten()
                .is_some_and(|cs| cs.is_tethered)
        {
            continue;
        }
        let mut chosen_target = None;
        if let Some((assisted_char, target_ball)) = assisted_target_opt {
            if d.assist_char_id == Some(assisted_char) {
                chosen_target = Some(target_ball);
            }
        }
        if chosen_target.is_none() {
            if let Some((guarded_char, aggressor_ball)) = attacker_of_guarded_opt {
                if d.guard_char_id == Some(guarded_char) {
                    chosen_target = Some(aggressor_ball);
                }
            }
        }
        if chosen_target.is_none() && settings.aggressive {
            if let Some(aggr) = attacker_of_owner_opt {
                chosen_target = Some(aggr);
            }
        }
        if let Some(tid) = chosen_target {
            let Some((_, target_ball)) = sys_handle.get_ball_snapshot(tid).await.ok().flatten() else {
                continue;
            };
            if sys_handle
                .get_combat_state(tid)
                .await
                .ok()
                .flatten()
                .is_some_and(|cs| cs.is_cloaked || cs.is_tethered)
            {
                continue;
            }
            d.target_id = Some(tid);
            d.activity_state = 1;
            if !d.is_sentry {
                let _ = sys_handle.set_speed_fraction(d.ball_id, 1.0).await;
                let _ = sys_handle
                    .set_movement_mode(
                        d.ball_id,
                        MovementMode::Orbit {
                            target_id: tid,
                            range: 1_500.0,
                        },
                    )
                    .await;
            }
            let mut target_destroyed = false;
            if d.damage_vector.total() > 0.0 {
                let victim_char_opt = resolve_victim_character_for_ball(ctx, tid);
                let cyc_s = d.cycle_duration_secs(ctx).max(1) as f64;
                if let Ok(Some((app, destroyed, _))) =
                    crate::services::station_structure::structure_lifecycle_rpc::apply_damage_structure_or_ball(
                        ctx,
                        &sys_handle,
                        d.solar_system_id,
                        d.owner_char_id,
                        tid,
                        d.damage_vector,
                        cyc_s,
                        now_unix,
                    )
                    .await
                {
                    record_killmail_assist_action(
                        ctx,
                        tid,
                        d.owner_char_id,
                        d.type_id,
                        d.type_id,
                        app,
                        false,
                        now_unix,
                    );
                    if app > 0.0 || destroyed {
                        renew_drone_aggression_timers(ctx, d.owner_char_id, victim_char_opt, now_unix);
                    }
                    if destroyed {
                        target_destroyed = true;
                        d.target_id = None;
                        d.activity_state = 0;
                    }
                }
            }
            engaged_drone_balls.push(d.ball_id);
            with_mut_combat_runtime(ctx, |rt| {
                rt.active_drones_in_space.insert(d.ball_id, d);
            });
            if target_destroyed {
                disengage_drones_targeting(ctx, solar_system_id, tid);
                super::combat_missiles_aoe::handle_destroyed_ball_from_missile_or_aoe(
                    ctx,
                    solar_system_id,
                    owner_char_id,
                    tid,
                    target_ball.type_id,
                    target_ball.position,
                    now_unix,
                )
                .await;
            }
        }
    }
    engaged_drone_balls
}

#[cfg(test)]
#[path = "combat_npcs_drones_cycle_ops_edges_tests.rs"]
mod combat_npcs_drones_cycle_ops_edges_tests;
