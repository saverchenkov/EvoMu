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
    if sys_handle.has_self_ewar_immune_cycle(target_id) {
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
    if let Ok(Some(mut tcs)) = sys_handle.get_combat_state(target_id).await {
        tcs.sync_action_breaks();
        if tcs.blocks_targeted_effect() {
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
    let _ = sys_handle.apply_ecm_jam_from(d.ball_id, target_id, jam_dur_s).await;
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

/// Checks persisted combat runtime inside an open DB snapshot without nesting `combat_ephemeral` locks.
pub(crate) fn is_recently_destroyed_in_persisted(
    persisted: &eve_db::PersistedCombatCrimewatchState,
    ball_id: i64,
    item_id: i64,
) -> bool {
    persisted.is_recently_destroyed_item_id(ball_id)
        || persisted.is_recently_destroyed_item_id(item_id)
}

/// Deletes the backing DB item and space metadata when a drone ball is destroyed,
/// returning `false` without deleting or tombstoning if a concurrent scoop already moved
/// the item out of space in `solar_system_id`.
pub(crate) fn cleanup_destroyed_drone_db_item(
    ctx: &ServerContext,
    solar_system_id: u32,
    drone_ball_id: i64,
    claimed_item_id: Option<i64>,
) -> bool {
    ctx.db.with_mut_snapshot(|snap| {
        let item_id = claimed_item_id
            .or_else(|| snap.items.contains_key(&drone_ball_id).then_some(drone_ball_id))
            .or_else(|| {
                snap.sp6_lifecycle
                    .space_object_meta
                    .iter()
                    .find(|(_, m)| m.drone_ball_id == Some(drone_ball_id))
                    .map(|(&id, _)| id)
            });
        if let Some(iid) = item_id {
            if snap.items.get(&iid).is_some_and(|it| {
                it.location_id != i64::from(solar_system_id) || it.flag != InventoryFlag::None
            }) {
                return false;
            }
            super::purge_destroyed_drone_from_snap(snap, Some(iid), drone_ball_id);
            snap.combat_crimewatch_state
                .record_destroyed_item_id(drone_ball_id);
            snap.combat_crimewatch_state.record_destroyed_item_id(iid);
        } else {
            super::purge_destroyed_drone_from_snap(snap, None, drone_ball_id);
            snap.combat_crimewatch_state
                .record_destroyed_item_id(drone_ball_id);
        }
        true
    })
}

/// Removes any synthetic stasis-web cycle registered by `drone_ball_id`.
pub(crate) async fn clear_drone_web_cycle(
    sys_handle: &crate::system_actor::SystemHandle,
    drone_ball_id: i64,
) {
    let _ = sys_handle
        .remove_module_cycle(drone_ball_id, -drone_ball_id)
        .await;
}

/// Applies a drone's stasis webification as a target-velocity module cycle so pilot
/// throttle (`speed_fraction`) is never overwritten and web stacking penalties apply.
pub(crate) async fn apply_drone_web_cycle(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    d: &DroneSpaceState,
    drone_ball: &eve_destiny::Ball,
    target_id: i64,
    target_ball: &eve_destiny::Ball,
) -> bool {
    if d.web_speed_factor >= 0.0 {
        return false;
    }
    let max_range_m = d.effective_optimal_range(ctx);
    if drone_ball.surface_distance(target_ball) > max_range_m {
        clear_drone_web_cycle(sys_handle, d.ball_id).await;
        return false;
    }
    let (claimed_hit, ready) = check_unclaimed_engaged_drone_ready(
        sys_handle,
        d.ball_id,
        d.solar_system_id,
        Some(d.parent_ship_id),
        None,
        target_id,
    )
    .await;
    if ready.is_none() {
        if !claimed_hit {
            clear_drone_web_cycle(sys_handle, d.ball_id).await;
        }
        return false;
    }
    let mult = (1.0 + d.web_speed_factor / 100.0).clamp(0.1, 1.0);
    let synth_id = -d.ball_id;
    let dur_s = d.cycle_duration_secs(ctx).max(1) as f64;
    let _ = sys_handle.remove_module_cycle(d.ball_id, synth_id).await;
    if sys_handle
        .add_module_cycle(crate::system_actor::ActiveModuleCycle {
            ship_id: d.ball_id,
            module_id: synth_id,
            module_type_id: d.type_id,
            effect_name: format!("drone_web:{}:{}", d.parent_ship_id, d.drone_item_id),
            target_id: Some(target_id),
            duration_s: dur_s,
            cap_need_per_cycle: 0.0,
            repeat: 1_000,
            max_effective_range_m: Some(max_range_m),
            target_velocity_multiplier: Some(mult),
            ..Default::default()
        })
        .await
        .is_err()
    {
        return false;
    }
    sys_handle.has_active_module_cycle_on_target(d.ball_id, synth_id, target_id)
}

/// Atomically reconciles an engaged drone's post-cycle state without overwriting concurrent
/// abandon, scoop, or destruction mutations.
pub(crate) async fn finish_engaged_drone_cycle(
    _ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    ball_id: i64,
    char_id: i64,
    ship_id: i64,
    target_id: i64,
    target_destroyed: bool,
    cycle_ready: bool,
    next_cycle_unix: u64,
) {
    let clear_web = sys_handle
        .mutate_active_drones_with_claims(move |drones, _, timers, claimed| {
            let Some(st) = drones.get_mut(&ball_id) else {
                timers.remove(&ball_id);
                return (false, true);
            };
            if st.is_abandoned || st.parent_ship_id != ship_id || st.owner_char_id != char_id {
                return (false, !claimed.contains_key(&ball_id));
            }
            if cycle_ready {
                timers
                    .entry(ball_id)
                    .and_modify(|t| *t = (*t).max(next_cycle_unix))
                    .or_insert(next_cycle_unix);
            }
            if claimed.contains_key(&ball_id) {
                return (false, false);
            }
            if target_destroyed && st.target_id == Some(target_id) {
                st.target_id = None;
                st.activity_state = 0;
                return (true, true);
            }
            (false, st.activity_state != 1 || st.target_id != Some(target_id))
        })
        .await
        .unwrap_or(true);
    if clear_web {
        clear_drone_web_cycle(sys_handle, ball_id).await;
    }
}

/// Disengages all drones in `solar_system_id` currently targeting `target_id`.
pub(crate) fn disengage_drones_targeting(ctx: &ServerContext, solar_system_id: u32, target_id: i64) {
    let Some(sys_handle) = ctx.systems.get_handle(solar_system_id) else {
        return;
    };
    let cleared = sys_handle
        .mutate_active_drones_sync(move |drones, _, _| {
            let mut ids = Vec::new();
            for d in drones.values_mut() {
                if d.ball_id > 0 && d.solar_system_id == solar_system_id && d.target_id == Some(target_id) {
                    d.target_id = None;
                    d.activity_state = 0;
                    ids.push(d.ball_id);
                }
            }
            (!ids.is_empty(), ids)
        })
        .unwrap_or_default();
    for bid in cleared {
        sys_handle.try_remove_module_cycle(bid, -bid);
    }
}

pub(crate) async fn check_unclaimed_engaged_drone_ready(
    sys_handle: &crate::system_actor::SystemHandle,
    bid: i64,
    expected_sys: u32,
    expected_ship: Option<i64>,
    expected_owner: Option<i64>,
    target_id: i64,
) -> (bool, Option<u64>) {
    sys_handle
        .mutate_active_drones_with_claims(move |drones, _, timers, claimed| {
            let is_claimed = claimed.contains_key(&bid);
            let ok = !is_claimed
                && drones.get(&bid).is_some_and(|st| {
                    !st.is_abandoned
                        && st.solar_system_id == expected_sys
                        && expected_ship.is_none_or(|s| st.parent_ship_id == s)
                        && expected_owner.is_none_or(|o| st.owner_char_id == o)
                        && st.activity_state == 1
                        && st.target_id == Some(target_id)
                });
            (false, (is_claimed, ok.then(|| timers.get(&bid).copied().unwrap_or(0))))
        })
        .await
        .unwrap_or((false, None))
}

async fn check_unclaimed_engaged_drone(
    sys_handle: &crate::system_actor::SystemHandle,
    bid: i64,
    expected_sys: u32,
    target_id: i64,
) -> (bool, bool) {
    let (is_claimed, ready) =
        check_unclaimed_engaged_drone_ready(sys_handle, bid, expected_sys, None, None, target_id).await;
    (is_claimed, ready.is_some())
}

/// Disengages a single drone by `drone_ball_id` in `sys_handle`.
fn disengage_single_drone(sys_handle: &crate::system_actor::SystemHandle, drone_ball_id: i64) {
    let cleared = sys_handle
        .mutate_active_drones_with_claims_sync(move |drones, _, _, claimed| {
            if claimed.contains_key(&drone_ball_id) {
                return (false, false);
            }
            if let Some(d) = drones.get_mut(&drone_ball_id) {
                d.target_id = None;
                d.activity_state = 0;
                (true, true)
            } else {
                (false, false)
            }
        })
        .unwrap_or(false);
    if cleared {
        sys_handle.try_remove_module_cycle(drone_ball_id, -drone_ball_id);
    }
}

/// Executes per-cycle drone combat volleys for a single system actor on its physics tick
/// (`feat:drones.engage-target`, `feat:drones.gecko-augmented-mutated-drones`,
/// `feat:drones.gecko-subverted-bandwidth`, `#494`, `#495`, `#1541`).
pub async fn step_engaged_drones_in_system(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    now_unix: u64,
) {
    let view = sys_handle.view();
    if !view.active_drones_needs_sqlite_hydration
        && !view.active_drones_in_space.values().any(|d| {
            d.ball_id > 0 && !d.is_abandoned && d.activity_state == 1 && d.target_id.is_some()
        })
    {
        return;
    }
    let due_drones: Vec<DroneSpaceState> = sys_handle
        .mutate_active_drones_with_claims(move |drones, _, timers, claimed| {
            let due = drones
                .values()
                .filter(|d| {
                    d.ball_id > 0
                        && !d.is_abandoned
                        && d.activity_state == 1
                        && d.target_id.is_some()
                        && !claimed.contains_key(&d.ball_id)
                        && now_unix >= timers.get(&d.ball_id).copied().unwrap_or(0)
                })
                .cloned()
                .collect();
            (false, due)
        })
        .await
        .unwrap_or_default();
    for d in due_drones {
        let Some(target_id) = d.target_id else {
            continue;
        };
        let parent_cloaked_or_tethered = crate::services::inventory_fitting::fitting_module_cycles::is_ship_actively_cloaked(
            ctx,
            sys_handle,
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
            disengage_single_drone(sys_handle, d.ball_id);
            continue;
        }
        let Some((_, drone_ball)) = sys_handle.get_ball_snapshot(d.ball_id).await.ok().flatten() else {
            continue;
        };
        let Some((_, target_ball)) = sys_handle.get_ball_snapshot(target_id).await.ok().flatten() else {
            disengage_single_drone(sys_handle, d.ball_id);
            continue;
        };
        let mut target_cs_opt = sys_handle.get_combat_state(target_id).await.ok().flatten();
        if target_cs_opt
            .as_mut()
            .is_some_and(|cs| {
                cs.sync_action_breaks();
                cs.blocks_targeted_effect()
            })
        {
            disengage_single_drone(sys_handle, d.ball_id);
            continue;
        }
        let (bid, expected_sys) = (d.ball_id, d.solar_system_id);
        let (claimed_hit, still_engaged) =
            check_unclaimed_engaged_drone(sys_handle, bid, expected_sys, target_id).await;
        if !still_engaged {
            if !claimed_hit {
                clear_drone_web_cycle(sys_handle, d.ball_id).await;
            }
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
                    sys_handle,
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
            if try_apply_drone_ecm_jam(ctx, sys_handle, &d, &drone_ball, target_id, &target_ball).await {
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
        if d.web_speed_factor < 0.0 {
            let (claimed_web, still_targeting) =
                check_unclaimed_engaged_drone(sys_handle, bid, expected_sys, target_id).await;
            let in_web_range =
                drone_ball.surface_distance(&target_ball) <= d.effective_optimal_range(ctx);
            if !claimed_web
                && (!still_targeting
                    || !in_web_range
                    || is_target_immune_to_drone_ewar(ctx, sys_handle, target_id, target_ball.type_id)
                        .await)
            {
                clear_drone_web_cycle(sys_handle, d.ball_id).await;
            } else if !claimed_web
                && apply_drone_web_cycle(
                    ctx,
                    sys_handle,
                    &d,
                    &drone_ball,
                    target_id,
                    &target_ball,
                )
                .await
            {
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
                let victim_char_opt = resolve_victim_character_for_ball(ctx, target_id);
                renew_drone_aggression_timers(ctx, d.owner_char_id, victim_char_opt, now_unix);
            }
        }
        let tidi = crate::services::serverops_cluster::get_system_tidi_factor(ctx, d.solar_system_id)
            .clamp(0.1, 1.0);
        let scaled_cycle_secs = ((d.cycle_duration_secs(ctx) as f64) / tidi).round().max(1.0) as u64;
        let next_ready = now_unix.saturating_add(scaled_cycle_secs);
        let (expected_owner, expected_ship) = (d.owner_char_id, d.parent_ship_id);
        let _ = sys_handle
            .mutate_active_drones_with_claims(move |drones, _, timers, _claimed| {
                if drones.get(&bid).is_some_and(|st| {
                    !st.is_abandoned
                        && st.solar_system_id == expected_sys
                        && st.owner_char_id == expected_owner
                        && st.parent_ship_id == expected_ship
                }) {
                    timers
                        .entry(bid)
                        .and_modify(|t| *t = (*t).max(next_ready))
                        .or_insert(next_ready);
                }
                (false, ())
            })
            .await;
    }
}

/// Test compatibility wrapper stepping engaged drones across all currently active system actors.
#[cfg(any(test, feature = "testkit"))]
pub async fn step_engaged_drones(ctx: &ServerContext, now_unix: u64) {
    for (_sys_id, sys_handle) in ctx.systems.active_system_handles() {
        step_engaged_drones_in_system(ctx, &sys_handle, now_unix).await;
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
    let settings = char_drone_settings(ctx, owner_char_id);
    let drones: Vec<DroneSpaceState> = sys_handle
        .mutate_active_drones_with_claims(move |drones, _, _, claimed| {
            let out = drones
                .values()
                .filter(|d| {
                    d.ball_id > 0
                        && d.solar_system_id == solar_system_id
                        && d.owner_char_id == owner_char_id
                        && !d.is_abandoned
                        && d.activity_state != 4
                        && !claimed.contains_key(&d.ball_id)
                })
                .cloned()
                .collect();
            (false, out)
        })
        .await
        .unwrap_or_default();

    let now_unix = ctx.now_unix();
    let mut engaged_drone_balls = Vec::new();
    for d in drones {
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
                .is_some_and(|mut cs| {
                    cs.sync_action_breaks();
                    cs.blocks_targeted_effect()
                })
            {
                continue;
            }
            let target_bid = d.ball_id;
            let expected_ship = d.parent_ship_id;
            let expected_target = d.target_id;
            let expected_state = d.activity_state;
            let switched_target = sys_handle
                .mutate_active_drones_with_claims(move |drones, _, _, claimed| {
                    if claimed.contains_key(&target_bid) {
                        return (false, None);
                    }
                    let Some(st) = drones.get_mut(&target_bid) else {
                        return (false, None);
                    };
                    if st.is_abandoned
                        || st.solar_system_id != solar_system_id
                        || st.owner_char_id != owner_char_id
                        || st.parent_ship_id != expected_ship
                        || st.activity_state == 4
                        || st.target_id != expected_target
                        || st.activity_state != expected_state
                    {
                        return (false, None);
                    }
                    let switched = st.target_id.is_some() && st.target_id != Some(tid);
                    st.target_id = Some(tid);
                    st.activity_state = 1;
                    (true, Some(switched))
                })
                .await
                .ok()
                .flatten();
            let Some(switched) = switched_target else {
                continue;
            };
            if switched && d.web_speed_factor < 0.0 {
                clear_drone_web_cycle(&sys_handle, d.ball_id).await;
            }
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
            let (claimed_hit, still_engaged) =
                check_unclaimed_engaged_drone(&sys_handle, target_bid, solar_system_id, tid).await;
            if !still_engaged {
                if !claimed_hit {
                    clear_drone_web_cycle(&sys_handle, d.ball_id).await;
                }
                continue;
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
                    }
                }
            }
            engaged_drone_balls.push(d.ball_id);
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

/// Cleans up runtime/actor/DB artifacts for a destroyed drone caught during scoop and returns `DroneNotInSpace`.
async fn reject_destroyed_drone_scoop(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    drone_ball_id: i64,
    item_id_opt: Option<i64>,
    purge_db: bool,
    msg: String,
    mut scoop_guard: Option<&mut crate::system_actor::ScoopClaimGuard<'_>>,
) -> BridgeRpcError {
    if purge_db {
        let _ = cleanup_destroyed_drone_db_item(
            ctx,
            sys_handle.solar_system_id,
            drone_ball_id,
            item_id_opt,
        );
    }
    let _ = sys_handle
        .drone_scoop_claim_op(
            drone_ball_id,
            crate::system_actor::DroneScoopClaimAction::ConsumeScoop,
        )
        .await;
    if let Some(g) = scoop_guard.as_deref_mut() {
        g.disarm();
    }
    if !purge_db {
        ctx.db.with_mut_snapshot(|snap| {
            let moved_out = item_id_opt
                .or_else(|| snap.items.contains_key(&drone_ball_id).then_some(drone_ball_id))
                .is_some_and(|iid| {
                    snap.items.get(&iid).is_some_and(|it| {
                        it.location_id != i64::from(sys_handle.solar_system_id)
                            || it.flag != InventoryFlag::None
                    })
                });
            if !moved_out {
                snap.combat_crimewatch_state
                    .record_destroyed_item_id(drone_ball_id);
                if let Some(id) = item_id_opt {
                    snap.combat_crimewatch_state.record_destroyed_item_id(id);
                }
            }
        });
    }
    clear_drone_web_cycle(sys_handle, drone_ball_id).await;
    let _ = sys_handle.remove_ball(drone_ball_id).await;
    BridgeRpcError::with_client_error(
        "DroneNotInSpace",
        msg.clone(),
        eve_proto::ClientError::CustomInfo(msg),
    )
}

/// Atomically scoops a drone ball from space into the pilot's DroneBay (`ship.ScoopToDroneBay`).
pub(crate) async fn handle_scoop_to_drone_bay(
    ctx: &ServerContext,
    session: &mut SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let (char_id, ship_id, sys_id, sys_handle, ship_ball) =
        resolve_uncloaked_drone_ship(ctx, session, "scoop drones").await?;
    crate::services::tourney_service::check_tourney_drone_restricted(ctx, sys_id, char_id)?;
    let drone_ball_id = RpcArgs::new(args, kwargs).req_positive_i64(0, "droneID")?;
    let Some((_, d_ball)) = sys_handle.get_ball_snapshot(drone_ball_id).await.ok().flatten() else {
        let msg = format!("Drone ball {drone_ball_id} is not in space");
        return Err(BridgeRpcError::with_client_error(
            "DroneNotInSpace",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        ));
    };
    if ship_ball.surface_distance(&d_ball) > 2_500.0 {
        let msg = "Drone must be within 2,500 m to scoop to drone bay".to_string();
        return Err(BridgeRpcError::with_client_error(
            "OutOfScoopRange",
            msg.clone(),
            eve_proto::ClientError::CustomInfo(msg),
        ));
    }
    if ctx.db.with_snapshot(|snap| {
        snap.combat_crimewatch_state
            .is_recently_destroyed_item_id(drone_ball_id)
    }) {
        let stale_item = sys_handle
            .remove_active_drone(drone_ball_id)
            .await
            .ok()
            .flatten()
            .map(|d| d.drone_item_id);
        return Err(reject_destroyed_drone_scoop(
            ctx,
            &sys_handle,
            drone_ball_id,
            stale_item,
            true,
            format!("Drone ball {drone_ball_id} was destroyed"),
            None,
        )
        .await);
    }

    let pre_claim = sys_handle
        .drone_scoop_claim_op(
            drone_ball_id,
            crate::system_actor::DroneScoopClaimAction::ClaimPlayerScoop { char_id },
        )
        .await;
    let active_drone = match pre_claim {
        Ok(crate::system_actor::DroneScoopClaimOutcome::Claimed(d)) => d,
        Ok(crate::system_actor::DroneScoopClaimOutcome::AlreadyClaimed) => {
            let msg = format!("Drone ball {drone_ball_id} is already being scooped");
            return Err(BridgeRpcError::with_client_error(
                "DroneNotInSpace",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            ));
        }
        Ok(crate::system_actor::DroneScoopClaimOutcome::OwnedByOther) => {
            let msg = "Cannot scoop an active drone controlled by another pilot".to_string();
            return Err(BridgeRpcError::with_client_error(
                "DroneOwnedByOther",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            ));
        }
        Ok(_) => {
            let msg = format!("Drone ball {drone_ball_id} is not in space");
            return Err(BridgeRpcError::with_client_error(
                "DroneNotInSpace",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            ));
        }
        Err(e) => {
            return Err(BridgeRpcError::with_client_error(
                "DroneNotInSpace",
                e.clone(),
                eve_proto::ClientError::CustomInfo(e),
            ));
        }
    };
    let mut scoop_guard = crate::system_actor::ScoopClaimGuard::new(&sys_handle, drone_ball_id);
    if let Some(ref drone_st) = active_drone {
        let item_id = drone_st.drone_item_id;
        if ctx
            .db
            .with_snapshot(|snap| snap.combat_crimewatch_state.is_recently_destroyed_item_id(item_id))
        {
            return Err(reject_destroyed_drone_scoop(
                ctx,
                &sys_handle,
                drone_ball_id,
                Some(item_id),
                true,
                format!("Drone item {item_id} was destroyed"),
                Some(&mut scoop_guard),
            )
            .await);
        }
    }

    enum ScoopTxResult {
        Scooped(i64),
        Destroyed {
            item_id: Option<i64>,
            purge_db: bool,
            msg: String,
        },
        CallerUnavailableAbandon(bool, String),
        ErrClear(BridgeRpcError),
        Err(BridgeRpcError),
    }
    let tx_res = ctx.db.with_mut_snapshot(|snap| -> ScoopTxResult {
        if snap
            .combat_crimewatch_state
            .is_recently_destroyed_item_id(drone_ball_id)
        {
            return ScoopTxResult::Destroyed {
                item_id: active_drone.as_ref().map(|d| d.drone_item_id),
                purge_db: true,
                msg: format!("Drone ball {drone_ball_id} was destroyed"),
            };
        }
        if let Some(ref drone_st) = active_drone {
            if snap
                .combat_crimewatch_state
                .is_recently_destroyed_item_id(drone_st.drone_item_id)
            {
                return ScoopTxResult::Destroyed {
                    item_id: Some(drone_st.drone_item_id),
                    purge_db: true,
                    msg: format!("Drone item {} was destroyed", drone_st.drone_item_id),
                };
            }
        }
            let matches_ball = |it: &eve_db::InventoryItemRecord| {
                let meta_ball = snap
                    .sp6_lifecycle
                    .space_object_meta
                    .get(&it.item_id)
                    .and_then(|m| m.drone_ball_id);
                meta_ball == Some(drone_ball_id)
                    || (meta_ball.is_none() && it.item_id == drone_ball_id)
            };
            let drone_item_id = if let Some(ref drone_st) = active_drone {
                drone_st.drone_item_id
            } else if let Some(iid) = snap
                .items
                .values()
                .find(|it| {
                    it.location_id == sys_id as i64
                        && it.flag == InventoryFlag::None
                        && matches_ball(it)
                })
                .or_else(|| snap.items.values().find(|it| matches_ball(it)))
                .map(|it| it.item_id)
            {
                iid
            } else {
                let caller_unavailable = snap.characters.get(&char_id).is_some_and(|ch| {
                    ch.station_id.is_some()
                        || ch.structure_id.is_some()
                        || ch.solar_system_id != sys_id
                        || ch.active_ship_id != ship_id
                }) || !snap.items.get(&ship_id).is_some_and(|sh| {
                    sh.location_id == sys_id as i64 && sh.flag == InventoryFlag::None
                });
                let (code, msg) = if caller_unavailable {
                    (
                        "DroneNotInSpace",
                        format!("Caller ship {ship_id} is not undocked in solar system {sys_id}"),
                    )
                } else {
                    (
                        "DroneNotFound",
                        format!("Drone ball {drone_ball_id} is not a scoopable drone"),
                    )
                };
                return ScoopTxResult::Err(BridgeRpcError::with_client_error(
                    code,
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                ));
            };

            if is_recently_destroyed_in_persisted(
                &snap.combat_crimewatch_state,
                drone_ball_id,
                drone_item_id,
            ) {
                super::purge_destroyed_drone_from_snap(snap, Some(drone_item_id), drone_ball_id);
                snap.combat_crimewatch_state
                    .record_destroyed_item_id(drone_ball_id);
                snap.combat_crimewatch_state
                    .record_destroyed_item_id(drone_item_id);
                return ScoopTxResult::Destroyed {
                    item_id: Some(drone_item_id),
                    purge_db: false,
                    msg: format!("Drone {drone_item_id} was destroyed"),
                };
            }

            let Some(it) = snap.items.get(&drone_item_id).cloned() else {
                let clear_stale = active_drone
                    .as_ref()
                    .is_some_and(|st| st.solar_system_id == sys_id);
                let msg = format!("Drone item {drone_item_id} not found");
                let err = BridgeRpcError::with_client_error(
                    "DroneNotFound",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                );
                return if clear_stale {
                    ScoopTxResult::ErrClear(err)
                } else {
                    ScoopTxResult::Err(err)
                };
            };
            if it.location_id != sys_id as i64 || it.flag != InventoryFlag::None {
                let clear_stale = active_drone
                    .as_ref()
                    .is_some_and(|st| st.solar_system_id == sys_id);
                let msg = format!("Drone item {drone_item_id} is no longer in space");
                let err = BridgeRpcError::with_client_error(
                    "DroneNotInSpace",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                );
                return if clear_stale {
                    ScoopTxResult::ErrClear(err)
                } else {
                    ScoopTxResult::Err(err)
                };
            }

            let ship_warping = crate::sp6_lifecycle::drone_return::is_ship_ball_warping(
                &sys_handle,
                Some(&ship_ball),
                ship_id,
            );
            let caller_unavailable = ship_warping
                || snap.characters.get(&char_id).is_some_and(|ch| {
                    ch.station_id.is_some()
                        || ch.structure_id.is_some()
                        || ch.solar_system_id != sys_id
                        || ch.active_ship_id != ship_id
                })
                || snap.sp6_lifecycle.disconnected_ships.contains_key(&char_id)
                || snap
                    .sp6_lifecycle
                    .disconnected_ships
                    .values()
                    .any(|d| d.ship_id == ship_id)
                || !snap.items.get(&ship_id).is_some_and(|sh| {
                    sh.location_id == sys_id as i64 && sh.flag == InventoryFlag::None
                });
            if caller_unavailable {
                let should_abandon = !ship_warping
                    && active_drone.as_ref().is_some_and(|st| {
                        !st.is_abandoned
                            && st.solar_system_id == sys_id
                            && ship_id > 0
                            && st.parent_ship_id == ship_id
                    });
                return ScoopTxResult::CallerUnavailableAbandon(
                    should_abandon,
                    format!("Caller ship {ship_id} is not undocked in solar system {sys_id}"),
                );
            }

            let Some(t) = ctx.sde.types.get(&it.type_id) else {
                let msg = format!("Item {drone_item_id} has unknown type {}", it.type_id);
                return ScoopTxResult::Err(BridgeRpcError::with_client_error(
                    "NotADrone",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                ));
            };
            if t.category_id != 18 {
                let msg = format!("Item {drone_item_id} (type {}) is not a drone", it.type_id);
                return ScoopTxResult::Err(BridgeRpcError::with_client_error(
                    "NotADrone",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                ));
            }
            let is_abandoned = active_drone.as_ref().is_some_and(|d| d.is_abandoned);
            if it.owner_id != char_id && it.owner_id != 0 && !is_abandoned {
                let msg = "Cannot scoop a drone owned by another pilot".to_string();
                return ScoopTxResult::Err(BridgeRpcError::with_client_error(
                    "DroneOwnedByOther",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                ));
            }
            let ship_drone_cap = snap
                .items
                .get(&ship_id)
                .and_then(|sh| {
                    sh.dynamic_attributes.get(&283).copied().or_else(|| {
                        ctx.sde
                            .types
                            .get(&sh.type_id)
                            .and_then(|st| st.default_attributes.get(&283).copied())
                    })
                })
                .unwrap_or(0.0)
                .max(0.0);
            let used_vol: f64 = snap
                .items
                .values()
                .filter(|i| i.location_id == ship_id && i.flag == InventoryFlag::DroneBay)
                .map(|i| {
                    ctx.sde
                        .types
                        .get(&i.type_id)
                        .map(|dt| dt.volume * (i.quantity.max(1) as f64))
                        .unwrap_or(0.0)
                })
                .sum();
            let incoming_vol = t.volume * (it.quantity.max(1) as f64);
            if ship_drone_cap <= 0.0 || used_vol + incoming_vol > ship_drone_cap + 1e-4 {
                let msg = format!(
                    "Drone bay capacity exceeded ({:.1} + {:.1} > {:.1} m3)",
                    used_vol, incoming_vol, ship_drone_cap
                );
                return ScoopTxResult::Err(BridgeRpcError::with_client_error(
                    "InsufficientDroneBayCapacity",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                ));
            }
            if !super::move_drone_item_to_bay_in_snap(
                snap,
                drone_item_id,
                drone_ball_id,
                sys_id,
                Some(char_id),
                ship_id,
                false,
            ) {
                let msg = format!("Drone item {drone_item_id} is no longer in space");
                return ScoopTxResult::Err(BridgeRpcError::with_client_error(
                    "DroneNotInSpace",
                    msg.clone(),
                    eve_proto::ClientError::CustomInfo(msg),
                ));
            }
            ScoopTxResult::Scooped(drone_item_id)
    });
    let drone_item_id = match tx_res {
        ScoopTxResult::Scooped(id) => {
            let _ = sys_handle
                .drone_scoop_claim_op(
                    drone_ball_id,
                    crate::system_actor::DroneScoopClaimAction::ConsumeScoop,
                )
                .await;
            scoop_guard.disarm();
            id
        }
        ScoopTxResult::Destroyed {
            item_id,
            purge_db,
            msg,
        } => {
            return Err(reject_destroyed_drone_scoop(
                ctx,
                &sys_handle,
                drone_ball_id,
                item_id,
                purge_db,
                msg,
                Some(&mut scoop_guard),
            )
            .await);
        }
        ScoopTxResult::CallerUnavailableAbandon(should_abandon, msg) => {
            if should_abandon {
                let claim_res = sys_handle
                    .drone_scoop_claim_op(
                        drone_ball_id,
                        crate::system_actor::DroneScoopClaimAction::ReleaseOrTakeForAbandon {
                            char_id_opt: Some(char_id),
                            ship_id,
                        },
                    )
                    .await;
                scoop_guard.disarm();
                match claim_res {
                    Ok(crate::system_actor::DroneScoopClaimOutcome::TakeForAbandon(Some(
                        live_st,
                    ))) => {
                        crate::sp6_lifecycle::drone_return::abandon_returning_drone_in_space(
                            ctx,
                            &sys_handle,
                            live_st,
                        )
                        .await;
                    }
                    Ok(crate::system_actor::DroneScoopClaimOutcome::ReleasedWithPending(
                        applied,
                    )) => {
                        crate::sp6_lifecycle::drone_return::finish_pending_scoop_abandon(
                            ctx,
                            &sys_handle,
                            drone_ball_id,
                            applied,
                        )
                        .await;
                    }
                    _ => {}
                }
            } else {
                crate::sp6_lifecycle::drone_return::release_scoop_claim_and_apply_pending(
                    ctx,
                    &sys_handle,
                    drone_ball_id,
                    Some(&mut scoop_guard),
                )
                .await;
            }
            return Err(BridgeRpcError::with_client_error(
                "DroneNotInSpace",
                msg.clone(),
                eve_proto::ClientError::CustomInfo(msg),
            ));
        }
        ScoopTxResult::ErrClear(e) => {
            let _ = sys_handle
                .drone_scoop_claim_op(
                    drone_ball_id,
                    crate::system_actor::DroneScoopClaimAction::ConsumeScoop,
                )
                .await;
            scoop_guard.disarm();
            let _ = sys_handle.clear_drone_runtime(drone_ball_id).await;
            return Err(e);
        }
        ScoopTxResult::Err(e) => {
            crate::sp6_lifecycle::drone_return::release_scoop_claim_and_apply_pending(
                ctx,
                &sys_handle,
                drone_ball_id,
                Some(&mut scoop_guard),
            )
            .await;
            return Err(e);
        }
    };
    clear_drone_web_cycle(&sys_handle, drone_ball_id).await;
    let _ = sys_handle.remove_ball(drone_ball_id).await;
    Ok(PyRep::keyval([
        ("scoopedDroneItemID", PyRep::Integer(drone_item_id)),
        ("newOwnerID", PyRep::Integer(char_id)),
    ]))
}

#[cfg(test)]
#[path = "combat_npcs_drones_cycle_ops_edges_tests.rs"]
mod combat_npcs_drones_cycle_ops_edges_tests;
