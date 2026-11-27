//! `pve_homefront_runtime` — Homefront Operations sim-clock timer stepping (`R14`) and
//! non-destructive restart pocket reconciliation (`RT-36`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::{with_combat_ephemeral, with_mut_combat_ephemeral};
use crate::services::pve_homefront_sites::{
    advance_next_raid_hauler, apply_homefront_direct_damage_in_space,
    despawn_homefront_balls_sync, ensure_contribution_fields, insert_scene_item_in_snap,
    raid_hauler_ball, register_homefront_npc_entity, runtime_record_objective_complete,
    set_homefront_relay_tethered, spawn_aar_basileus_in_space, spawn_aar_cache_in_space,
    spawn_ball_sync_with_claim, spawn_next_aar_wave_in_space,
    spawn_or_reconcile_pocket_entities_with_handle, try_settle_completed_homefront_live,
    HOMEFRONT_RAID_CRATE_TYPE_ID, HOMEFRONT_TRAFFIC_CONTRABAND_TYPE_ID,
    HOMEFRONT_TRAFFIC_TIMER_SECS,
};
use crate::system_actor::{SystemHandle, SystemView};
use eve_db::{
    HomefrontProgressRecord, HomefrontRuntimeRecord, InventoryFlag, InventoryItemRecord,
};
use eve_destiny::{Ball, CombatState, MovementMode};
use eve_proto::BridgeRpcError;
use eve_sde::{SolarSystemId, TypeId};
use glam::DVec3;
use std::collections::BTreeMap;

/// Returns `true` when `item_id` belongs to an active (non-completed, non-failed) Homefront
/// operation's space containers/wrecks/objectives so downtime and jetcan decay sweeps do not
/// destroy live mission state across server restarts or under TiDi (`#1196` RT-36).
pub(crate) fn is_active_homefront_space_object_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    item_id: i64,
) -> bool {
    snap.dungeon_runtime.homefront_runtimes.values().any(|r| {
        !r.completed
            && !r.failed
            && (r.spawned_ball_ids.contains(&item_id)
                || match &r.progress {
                    HomefrontProgressRecord::Raid {
                        delivery_depot_ball_id,
                        dropped_crate_ball_ids,
                        ..
                    } => {
                        *delivery_depot_ball_id == item_id
                            || dropped_crate_ball_ids.contains(&item_id)
                    }
                    HomefrontProgressRecord::SalvageResearch { wreck_ball_ids, .. } => {
                        wreck_ball_ids.contains(&item_id)
                    }
                    _ => false,
                })
    })
}

/// Updates `expires_at_sec` to `now + remaining_secs` for UI/status reads without altering
/// simulation-clock elapsed tracking.
pub(super) fn update_homefront_expires_at_sec(rec: &mut HomefrontRuntimeRecord, now: u64) {
    if !rec.started {
        return;
    }
    let remaining_secs = (rec.blueprint.time_limit_secs as f64 - rec.elapsed_sim_secs)
        .max(0.0)
        .ceil() as u64;
    rec.expires_at_sec = now.saturating_add(remaining_secs);
}

/// Returns `true` when a started Homefront site has reached or exceeded its simulation-clock time limit.
pub(super) fn is_homefront_sim_timer_expired(rec: &HomefrontRuntimeRecord) -> bool {
    rec.started
        && rec.blueprint.time_limit_secs > 0
        && (rec.elapsed_sim_secs + 1e-9) >= rec.blueprint.time_limit_secs as f64
}

/// Computes effective elapsed simulation seconds (`rec.elapsed_sim_secs + pending_dt_sim`) without
/// mutating `rec.last_sim_elapsed_secs`, for read-mostly timeout guards on active pilot RPCs.
pub(super) fn effective_elapsed_sim_secs(
    rec: &HomefrontRuntimeRecord,
    cur_sim_opt: Option<f64>,
) -> f64 {
    let Some(cur_sim) = cur_sim_opt else {
        return rec.elapsed_sim_secs;
    };
    let dt_sim = match rec.last_sim_elapsed_secs {
        Some(prev_sim) if cur_sim >= prev_sim => (cur_sim - prev_sim).max(0.0),
        _ => 0.0,
    };
    rec.elapsed_sim_secs + dt_sim
}

/// Advances `rec.elapsed_sim_secs` against the solar system's live `sim_elapsed_secs` clock (`R14`).
///
/// Cold systems (`cur_sim_opt == None`) do not advance `elapsed_sim_secs`. On initial anchor
/// (`last_sim_elapsed_secs == None`) or server/actor restart (`cur_sim < prev_sim`), `last_sim_elapsed_secs`
/// re-anchors to `Some(cur_sim)` with `dt_sim = 0.0` so zero downtime is charged.
pub(super) fn advance_homefront_sim_clock(
    rec: &mut HomefrontRuntimeRecord,
    cur_sim_opt: Option<f64>,
    now: u64,
) -> (f64, u64) {
    let Some(cur_sim) = cur_sim_opt else {
        update_homefront_expires_at_sec(rec, now);
        return (0.0, 0);
    };
    let dt_sim = match rec.last_sim_elapsed_secs {
        Some(prev_sim) if cur_sim >= prev_sim => (cur_sim - prev_sim).max(0.0),
        _ => 0.0,
    };
    rec.last_sim_elapsed_secs = Some(cur_sim);
    let prev_whole = (rec.elapsed_sim_secs + 1e-9).floor() as u64;
    rec.elapsed_sim_secs += dt_sim;
    let next_whole = (rec.elapsed_sim_secs + 1e-9).floor() as u64;
    let dt_whole_secs = next_whole.saturating_sub(prev_whole);
    update_homefront_expires_at_sec(rec, now);
    (dt_sim, dt_whole_secs)
}

/// Returns `true` when both the outer entry structures and active archetype pocket anchor balls
/// are present in `view` (`RT-36`).
pub(super) fn pocket_anchor_balls_present_in_view(
    rec: &HomefrontRuntimeRecord,
    view: &SystemView,
) -> bool {
    if !view.balls.contains_key(&rec.outer_beacon_ball_id)
        || !view.balls.contains_key(&rec.outer_gate_ball_id)
    {
        return false;
    }
    if rec.spawned_ball_ids.contains(&rec.dispatch_boss_ball_id)
        && !view.balls.contains_key(&rec.dispatch_boss_ball_id)
    {
        return false;
    }
    let base_obj = 1_897_800_000 + rec.instance_id.saturating_mul(100);
    for offset in 20_i64..30 {
        let esc_id = base_obj + offset;
        if rec.spawned_ball_ids.contains(&esc_id) && !view.balls.contains_key(&esc_id) {
            return false;
        }
    }
    match &rec.progress {
        HomefrontProgressRecord::Raid {
            freighter_ball_id,
            delivery_depot_ball_id,
            active_hauler_ball_id,
            ..
        } => {
            view.balls.contains_key(freighter_ball_id)
                && view.balls.contains_key(delivery_depot_ball_id)
                && active_hauler_ball_id.is_none_or(|hid| view.balls.contains_key(&hid))
        }
        HomefrontProgressRecord::EmergencyAid {
            protectee_ball_id, ..
        } => view.balls.contains_key(protectee_ball_id),
        HomefrontProgressRecord::MetaliminalMeteoroid {
            meteoroid_ball_id,
            remaining_units,
            ..
        } => *remaining_units == 0 || view.balls.contains_key(meteoroid_ball_id),
        HomefrontProgressRecord::DreadAssault {
            dread_ball_id,
            base_ball_id,
            enemy_base_hp,
            ..
        } => {
            view.balls.contains_key(dread_ball_id)
                && (*enemy_base_hp <= 0.0 || view.balls.contains_key(base_ball_id))
        }
        HomefrontProgressRecord::AbyssalArtifactRecovery {
            rock_ball_ids,
            wave_hostile_ball_ids,
            basileus_ball_id,
            cache_ball_id,
            ..
        } => {
            rock_ball_ids.iter().all(|id| view.balls.contains_key(id))
                && wave_hostile_ball_ids.iter().all(|id| view.balls.contains_key(id))
                && basileus_ball_id.is_none_or(|bid| view.balls.contains_key(&bid))
                && cache_ball_id.is_none_or(|cid| view.balls.contains_key(&cid))
        }
        HomefrontProgressRecord::SuspiciousSignal {
            relay_ball_id,
            array_ball_ids,
            relay_hp,
            ..
        } => {
            (*relay_hp <= 0.0 || view.balls.contains_key(relay_ball_id))
                && array_ball_ids.iter().all(|id| view.balls.contains_key(id))
        }
        HomefrontProgressRecord::SalvageResearch {
            werpost_ball_id, ..
        } => view.balls.contains_key(werpost_ball_id),
        HomefrontProgressRecord::StabilizeRift { array_ball_ids, .. } => {
            array_ball_ids.iter().all(|id| view.balls.contains_key(id))
        }
        HomefrontProgressRecord::TrafficStop {
            station_ball_id,
            active_hauler_ball_ids,
            ..
        } => {
            view.balls.contains_key(station_ball_id)
                && active_hauler_ball_ids.iter().all(|id| view.balls.contains_key(id))
        }
    }
}

/// Non-destructively rebuilds missing outer beacon/gate and inner pocket entities for a started
/// Homefront site after ballpark restart (`RT-36`), preserving `progress`, `contributions`,
/// `entered_pilots`, and `elapsed_sim_secs`.
pub(super) fn reconcile_homefront_pocket_after_restart_sync(
    ctx: &ServerContext,
    sys_h: &SystemHandle,
    instance_id: i64,
) {
    spawn_or_reconcile_pocket_entities_with_handle(ctx, sys_h, instance_id, true);
}

/// Synchronous 1 Hz / `advance_pve_special_clock` Homefront tick across all systems.
pub fn tick_homefront_operations_sync(ctx: &ServerContext) {
    let now = ctx.now_unix();
    let active_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime
            .homefront_runtimes
            .values()
            .filter(|r| r.started && !r.completed && !r.failed)
            .map(|r| r.instance_id)
            .collect()
    });
    if active_ids.is_empty() {
        return;
    }

    for inst_id in active_ids {
        tick_single_homefront_instance_sync(ctx, inst_id, now);
    }
}

/// Synchronous 1 Hz per-system Homefront tick called from `combat_npc_ai::step_extended_npc_ai_sync`.
pub fn tick_homefront_operations_for_system_sync(ctx: &ServerContext, sys_id: SolarSystemId) {
    let now = ctx.now_unix();
    let active_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime
            .homefront_runtimes
            .values()
            .filter(|r| r.solar_system_id == sys_id && r.started && !r.completed && !r.failed)
            .map(|r| r.instance_id)
            .collect()
    });
    if active_ids.is_empty() {
        return;
    }

    for inst_id in active_ids {
        tick_single_homefront_instance_sync(ctx, inst_id, now);
    }
}

fn tick_single_homefront_instance_sync(ctx: &ServerContext, inst_id: i64, now: u64) {
    #[derive(Default)]
    struct SideEffects {
        settle_complete: bool,
        despawn_fail: Option<(SolarSystemId, Vec<i64>)>,
        despawn_escaped_balls: Option<(SolarSystemId, Vec<i64>)>,
        spawn_traffic_smugglers: Option<(SolarSystemId, Vec<(i64, DVec3)>)>,
        retether_relay: Option<(SolarSystemId, i64)>,
        spawn_aar_wave: Option<u8>,
        spawn_basileus: Option<(SolarSystemId, DVec3)>,
        werpost_attack: Option<(SolarSystemId, i64, f64)>,
        dread_siege_dmg: Option<(SolarSystemId, i64, i64, f64)>,
        respawn_raid_hauler: Option<(SolarSystemId, i64, TypeId, DVec3, i64)>,
    }

    let snapshot_pre = ctx.db.with_snapshot(|snap| {
        snap.dungeon_runtime.homefront_runtimes.get(&inst_id).cloned()
    });
    let Some(mut pre) = snapshot_pre else {
        return;
    };
    let sys_h_opt = ctx.systems.get_handle(pre.solar_system_id);

    // RT-36: Reconcile missing outer/inner pocket entities when ballpark restarted on an active site.
    if pre.started && !pre.completed && !pre.failed && pre.pocket_spawned {
        if let Some(sys_h) = sys_h_opt.as_ref() {
            let view = sys_h.view();
            if !pocket_anchor_balls_present_in_view(&pre, &view) {
                reconcile_homefront_pocket_after_restart_sync(ctx, sys_h, inst_id);
                if let Some(refreshed) = ctx.db.with_snapshot(|snap| {
                    snap.dungeon_runtime.homefront_runtimes.get(&inst_id).cloned()
                }) {
                    pre = refreshed;
                }
            }
        }
    }

    let cur_sim_opt = sys_h_opt.as_ref().map(|h| h.view().sim_elapsed_secs);

    // Sample live ballpark CombatState outside DB write lock (strict lock order!)
    let mut live_dread_cap: Option<f64> = None;
    let mut live_rift_caps: Option<[f64; 5]> = None;
    let mut live_protectee_hp: Option<f64> = None;
    let mut live_raid_scrambled = false;
    let mut live_hostile_target: Option<i64> = None;
    let mut scrambled_traffic_smugglers: Vec<i64> = Vec::new();

    if let Some(sys_h) = sys_h_opt.as_ref() {
        let view = sys_h.view();
        match &pre.progress {
            HomefrontProgressRecord::DreadAssault { dread_ball_id, .. } => {
                if let Some(cs) = view.combat_states.get(dread_ball_id) {
                    live_dread_cap = Some(cs.capacitor);
                }
            }
            HomefrontProgressRecord::StabilizeRift { array_ball_ids, .. } => {
                let mut caps = [1.0_f64; 5];
                let mut any_ball = false;
                for (i, aid) in array_ball_ids.iter().enumerate().take(5) {
                    if let Some(cs) = view.combat_states.get(aid) {
                        caps[i] = if cs.max_capacitor > 0.0 {
                            (cs.capacitor / cs.max_capacitor).clamp(0.0, 1.0)
                        } else {
                            1.0
                        };
                        any_ball = true;
                    }
                }
                if any_ball {
                    live_rift_caps = Some(caps);
                }
            }
            HomefrontProgressRecord::EmergencyAid {
                protectee_ball_id, ..
            } => {
                if let Some(cs) = view.combat_states.get(protectee_ball_id) {
                    live_protectee_hp = Some(cs.shield_hp + cs.armor_hp + cs.structure_hp);
                }
            }
            HomefrontProgressRecord::Raid {
                active_hauler_ball_id: Some(hid),
                ..
            } => {
                live_raid_scrambled = sys_h.is_ball_scrambled_sync(*hid);
            }
            HomefrontProgressRecord::TrafficStop { smuggler_ball_ids, .. } => {
                for &sid in smuggler_ball_ids {
                    if sys_h.is_ball_scrambled_sync(sid) {
                        scrambled_traffic_smugglers.push(sid);
                    }
                }
            }
            HomefrontProgressRecord::SalvageResearch {
                werpost_activated: true,
                ..
            } => {
                live_hostile_target = pre
                    .spawned_ball_ids
                    .iter()
                    .copied()
                    .find(|bid| *bid == pre.dispatch_boss_ball_id && view.balls.contains_key(bid));
            }
            _ => {}
        }
    }

    let fx_res = ctx.db.try_with_mut_snapshot(|snap| -> Result<SideEffects, BridgeRpcError> {
        let mut fx = SideEffects::default();
        let (raid_depot_count, raid_per_owner) = snap
            .dungeon_runtime
            .homefront_runtimes
            .get(&inst_id)
            .and_then(|rec| match &rec.progress {
                HomefrontProgressRecord::Raid {
                    delivery_depot_ball_id,
                    ..
                } => {
                    let loot_tid = rec
                        .blueprint
                        .loot_commodity_type_id
                        .unwrap_or(HOMEFRONT_RAID_CRATE_TYPE_ID);
                    let mut total: i64 = 0;
                    let mut per_owner: BTreeMap<i64, u32> = BTreeMap::new();
                    for it in snap.items.values() {
                        if it.location_id == *delivery_depot_ball_id
                            && it.type_id == loot_tid
                            && it.quantity > 0
                            && rec.entered_pilots.contains_key(&it.owner_id)
                        {
                            let qty = u32::try_from(it.quantity).unwrap_or(0);
                            total += i64::from(it.quantity);
                            *per_owner.entry(it.owner_id).or_insert(0) += qty;
                        }
                    }
                    Some((
                        u8::try_from(total.max(0)).unwrap_or(u8::MAX),
                        per_owner,
                    ))
                }
                _ => None,
            })
            .unwrap_or_else(|| (0, BTreeMap::new()));
        let mut traffic_replacement_req: Option<(SolarSystemId, DVec3, u32, u8)> = None;
        let mut traffic_failed = false;
        let mut traffic_escaped_balls: Vec<i64> = Vec::new();
        let (sys_id, hostile_faction_id) = {
            let next_entity_id = &mut snap.next_entity_id;
            let Some(rec) = snap.dungeon_runtime.homefront_runtimes.get_mut(&inst_id) else {
                return Ok(fx);
            };
            if !rec.started || rec.completed || rec.failed {
                return Ok(fx);
            }

            let prev_last_tick_sec = rec.last_tick_sec.max(rec.started_at_sec);
            let (_dt_sim, dt) = advance_homefront_sim_clock(rec, cur_sim_opt, now);
            rec.last_tick_sec = now;

            if is_homefront_sim_timer_expired(rec) {
                rec.failed = true;
                rec.failure_reason = Some(format!(
                    "Homefront {} exceeded {}-second operation timer",
                    rec.blueprint.dungeon_name, rec.blueprint.time_limit_secs
                ));
                fx.despawn_fail = Some((rec.solar_system_id, rec.spawned_ball_ids.clone()));
                return Ok(fx);
            }

            if let HomefrontProgressRecord::Raid {
                cargo_delivered,
                required_deliveries,
                ..
            } = &mut rec.progress
            {
                if raid_depot_count > *cargo_delivered {
                    let mut rem = u32::from(raid_depot_count - *cargo_delivered);
                    *cargo_delivered = raid_depot_count;
                    for (&pid, &owned_qty) in &raid_per_owner {
                        if rem == 0 {
                            break;
                        }
                        let already = rec.contributions.get(&pid).map_or(0, |c| c.cargo_delivered);
                        let credit = owned_qty.saturating_sub(already).min(rem);
                        if credit > 0 {
                            ensure_contribution_fields(
                                &mut rec.contributions,
                                &mut rec.next_entry_sequence,
                                pid,
                            )
                            .cargo_delivered += credit;
                            rem -= credit;
                        }
                    }
                    if rem > 0 {
                        if let Some((&fallback_pid, _)) = raid_per_owner.iter().next() {
                            ensure_contribution_fields(
                                &mut rec.contributions,
                                &mut rec.next_entry_sequence,
                                fallback_pid,
                            )
                            .cargo_delivered += rem;
                        }
                    }
                    if *cargo_delivered >= *required_deliveries {
                        fx.settle_complete = true;
                    }
                }
            }

            // Keep Raid hauler escape deadline invariant under TiDi, cold ticks, and server restarts.
            if let HomefrontProgressRecord::Raid {
                active_hauler_ball_id: Some(_),
                active_hauler_escape_at_sec,
                ..
            } = &mut rec.progress
            {
                if *active_hauler_escape_at_sec > 0 {
                    let remaining_before = (*active_hauler_escape_at_sec)
                        .saturating_sub(prev_last_tick_sec)
                        .min(90);
                    if dt >= remaining_before {
                        *active_hauler_escape_at_sec = now;
                    } else {
                        *active_hauler_escape_at_sec =
                            now.saturating_add(remaining_before - dt);
                    }
                }
            }

            if dt == 0 {
                return Ok(fx);
            }

            let sys_id = rec.solar_system_id;
            let pocket_pos = DVec3::from_array(rec.pocket_position);
            let hostile_faction_id = rec.blueprint.hostile_faction_id;
            let hauler_tid = rec
                .blueprint
                .objective_structure_type_id
                .unwrap_or(77_192);

            match &mut rec.progress {
                HomefrontProgressRecord::Raid {
                    haulers_spawned,
                    haulers_destroyed,
                    haulers_escaped,
                    active_hauler_ball_id,
                    active_hauler_tackled,
                    active_hauler_escape_at_sec,
                    required_deliveries,
                    ..
                } => {
                    if live_raid_scrambled {
                        *active_hauler_tackled = true;
                    }
                    if let Some(old_hid) = *active_hauler_ball_id {
                        if !*active_hauler_tackled
                            && *active_hauler_escape_at_sec > 0
                            && now >= *active_hauler_escape_at_sec
                        {
                            *haulers_escaped = haulers_escaped.saturating_add(1);
                            rec.spawned_ball_ids.retain(|&id| id != old_hid);
                            if let Some((new_hid, h_tid, h_pos)) = advance_next_raid_hauler(
                                next_entity_id,
                                now,
                                hauler_tid,
                                pocket_pos,
                                haulers_spawned,
                                *haulers_destroyed,
                                *required_deliveries,
                                active_hauler_ball_id,
                                active_hauler_tackled,
                                active_hauler_escape_at_sec,
                                &mut rec.spawned_ball_ids,
                            ) {
                                fx.respawn_raid_hauler =
                                    Some((sys_id, old_hid, h_tid, h_pos, new_hid));
                            }
                        }
                    }
                }
                HomefrontProgressRecord::EmergencyAid {
                    protectee_hp,
                    survival_elapsed_secs,
                    required_survival_secs,
                    waves_repelled,
                    ..
                } => {
                    if let Some(lhp) = live_protectee_hp {
                        *protectee_hp = lhp;
                    }
                    if *protectee_hp <= 0.0 {
                        rec.failed = true;
                        rec.failure_reason =
                            Some("Emergency Aid allied vessel was destroyed".into());
                        fx.despawn_fail = Some((sys_id, rec.spawned_ball_ids.clone()));
                        return Ok(fx);
                    }
                    *survival_elapsed_secs =
                        survival_elapsed_secs.saturating_add(dt).min(*required_survival_secs);
                    *waves_repelled =
                        (*waves_repelled).max((*survival_elapsed_secs / 200).min(3) as u8);
                    if *survival_elapsed_secs >= *required_survival_secs {
                        fx.settle_complete = true;
                    }
                }
                HomefrontProgressRecord::DreadAssault {
                    dread_ball_id,
                    base_ball_id,
                    capacitor_gj,
                    capacitor_target_gj,
                    siege_cycles_completed,
                    target_siege_cycles,
                    in_siege_mode,
                    siege_remaining_secs,
                    enemy_base_hp,
                    enemy_base_max_hp,
                    ..
                } => {
                    if !*in_siege_mode {
                        if let Some(dcap) = live_dread_cap {
                            if dcap > *capacitor_gj {
                                *capacitor_gj = dcap.clamp(0.0, *capacitor_target_gj);
                            }
                        }
                        if *capacitor_gj >= *capacitor_target_gj {
                            *in_siege_mode = true;
                            *siege_remaining_secs = 90;
                        }
                    }
                    if *in_siege_mode {
                        if dt >= *siege_remaining_secs {
                            *siege_remaining_secs = 0;
                            *in_siege_mode = false;
                            *capacitor_gj = 0.0;
                            *siege_cycles_completed = siege_cycles_completed.saturating_add(1);
                            let chunk =
                                *enemy_base_max_hp / f64::from((*target_siege_cycles).max(1));
                            *enemy_base_hp = (*enemy_base_hp - chunk).max(0.0);
                            fx.dread_siege_dmg = Some((sys_id, *dread_ball_id, *base_ball_id, chunk));
                            if *siege_cycles_completed >= *target_siege_cycles
                                && *enemy_base_hp <= 0.0
                            {
                                fx.settle_complete = true;
                            }
                        } else {
                            *siege_remaining_secs -= dt;
                        }
                    }
                }
                HomefrontProgressRecord::AbyssalArtifactRecovery {
                    current_wave,
                    total_waves,
                    setup_remaining_secs,
                    wave_time_remaining_secs,
                    evacuation_active,
                    basileus_spawned,
                    extraction_window_secs,
                    mutanite_units_remaining,
                    ..
                } => {
                    if *setup_remaining_secs > 0 {
                        *setup_remaining_secs = setup_remaining_secs.saturating_sub(dt);
                    } else if *evacuation_active {
                        if dt >= *extraction_window_secs {
                            *extraction_window_secs = 0;
                            if !*basileus_spawned {
                                *basileus_spawned = true;
                                fx.spawn_basileus = Some((sys_id, pocket_pos));
                            }
                        } else {
                            *extraction_window_secs -= dt;
                        }
                    } else if *mutanite_units_remaining > 0 {
                        if dt >= *wave_time_remaining_secs {
                            *wave_time_remaining_secs = 0;
                            rec.failed = true;
                            rec.failure_reason = Some(format!(
                                "Abyssal Artifact Recovery wave {} timed out with {} Mutanite unmined",
                                *current_wave, *mutanite_units_remaining
                            ));
                            fx.despawn_fail = Some((sys_id, rec.spawned_ball_ids.clone()));
                            return Ok(fx);
                        } else {
                            *wave_time_remaining_secs -= dt;
                        }
                    } else if *current_wave < *total_waves {
                        fx.spawn_aar_wave = Some(*current_wave + 1);
                    }
                }
                HomefrontProgressRecord::SuspiciousSignal {
                    relay_ball_id,
                    arrays_hacked,
                    vulnerability_window_secs,
                    relay_hp,
                    ..
                } => {
                    if *vulnerability_window_secs > 0 && *relay_hp > 0.0 {
                        if dt >= *vulnerability_window_secs {
                            *vulnerability_window_secs = 0;
                            *arrays_hacked = [false; 3];
                            fx.retether_relay = Some((sys_id, *relay_ball_id));
                        } else {
                            *vulnerability_window_secs -= dt;
                        }
                    }
                }
                HomefrontProgressRecord::SalvageResearch {
                    werpost_activated: true,
                    ..
                } => {
                    if let Some(target_id) = live_hostile_target {
                        fx.werpost_attack = Some((sys_id, target_id, (dt as f64) * 350.0));
                    }
                }
                HomefrontProgressRecord::StabilizeRift {
                    arrays_cap_pct,
                    held_low_secs,
                    required_hold_secs,
                    ..
                } => {
                    if let Some(caps) = live_rift_caps {
                        *arrays_cap_pct = caps;
                    }
                    if arrays_cap_pct.iter().all(|&p| p < 0.20) {
                        *held_low_secs =
                            held_low_secs.saturating_add(dt).min(*required_hold_secs);
                        if *held_low_secs >= *required_hold_secs {
                            fx.settle_complete = true;
                        }
                    } else {
                        *held_low_secs = 0;
                    }
                }
                HomefrontProgressRecord::TrafficStop {
                    active_hauler_ball_ids,
                    smuggler_ball_ids,
                    survival_elapsed_secs,
                    required_survival_secs,
                    smugglers_destroyed,
                    required_smugglers,
                    smugglers_escaped,
                    civilians_harmed,
                    ..
                } => {
                    const ESCAPE_CHECKPOINTS: [u64; 6] = [90, 165, 240, 315, 390, 465];
                    let prev_elapsed = *survival_elapsed_secs;
                    let req_secs = (*required_survival_secs).max(HOMEFRONT_TRAFFIC_TIMER_SECS);
                    let next_elapsed = prev_elapsed.saturating_add(dt).min(req_secs);
                    *survival_elapsed_secs = next_elapsed;

                    for cp in ESCAPE_CHECKPOINTS {
                        if cp > prev_elapsed && cp <= next_elapsed {
                            if let Some(pos_idx) = smuggler_ball_ids
                                .iter()
                                .position(|id| !scrambled_traffic_smugglers.contains(id))
                            {
                                let escaped_id = smuggler_ball_ids.remove(pos_idx);
                                active_hauler_ball_ids.retain(|&id| id != escaped_id);
                                traffic_escaped_balls.push(escaped_id);
                                *smugglers_escaped = smugglers_escaped.saturating_add(1);
                            }
                        }
                    }

                    if *smugglers_escaped >= 5 || *civilians_harmed >= 3 {
                        traffic_failed = true;
                    } else if !traffic_escaped_balls.is_empty() {
                        let alive_plus_killed =
                            smugglers_destroyed.saturating_add(smuggler_ball_ids.len() as u8);
                        let need_rep = required_smugglers.saturating_sub(alive_plus_killed);
                        if need_rep > 0 {
                            traffic_replacement_req =
                                Some((sys_id, pocket_pos, hostile_faction_id, need_rep));
                        }
                    }
                }
                _ => {}
            }

            if !traffic_escaped_balls.is_empty() {
                rec.spawned_ball_ids
                    .retain(|id| !traffic_escaped_balls.contains(id));
                fx.despawn_escaped_balls = Some((sys_id, traffic_escaped_balls));
            }
            if traffic_failed {
                rec.failed = true;
                rec.failure_reason = Some("Traffic Stop objectives failed".into());
                fx.despawn_escaped_balls = None;
                fx.despawn_fail = Some((sys_id, rec.spawned_ball_ids.clone()));
                return Ok(fx);
            }
            if matches!(rec.progress, HomefrontProgressRecord::TrafficStop { .. })
                && runtime_record_objective_complete(rec)
            {
                fx.settle_complete = true;
            }
            let _ = rec;
            (sys_id, hostile_faction_id)
        };

        if let Some((rep_sys, rep_pos, rep_faction, rep_count)) = traffic_replacement_req {
            let mut rep_allocs = Vec::with_capacity(usize::from(rep_count));
            for i in 0..rep_count {
                snap.next_entity_id += 1;
                let hid = snap.next_entity_id;
                snap.next_entity_id += 1;
                let cargo_id = snap.next_entity_id;
                insert_scene_item_in_snap(
                    snap,
                    InventoryItemRecord {
                        item_id: cargo_id,
                        type_id: HOMEFRONT_TRAFFIC_CONTRABAND_TYPE_ID,
                        owner_id: i64::from(rep_faction),
                        location_id: hid,
                        flag: InventoryFlag::Cargo,
                        quantity: 1,
                        singleton: false,
                        version: 1,
                        ..Default::default()
                    },
                );
                let spawn_pos =
                    rep_pos + DVec3::new((f64::from(i) + 1.0) * 2_500.0, 2_000.0, -3_000.0);
                rep_allocs.push((hid, spawn_pos));
            }
            if let Some(rec) = snap.dungeon_runtime.homefront_runtimes.get_mut(&inst_id) {
                if let HomefrontProgressRecord::TrafficStop {
                    active_hauler_ball_ids,
                    smuggler_ball_ids,
                    ..
                } = &mut rec.progress
                {
                    for &(hid, _) in &rep_allocs {
                        active_hauler_ball_ids.push(hid);
                        smuggler_ball_ids.push(hid);
                        rec.spawned_ball_ids.push(hid);
                    }
                }
            }
            fx.spawn_traffic_smugglers = Some((rep_sys, rep_allocs));
        }
        let _ = (sys_id, hostile_faction_id);
        Ok(fx)
    });

    let Ok(fx) = fx_res else {
        return;
    };
    if let Some((sys_id, balls)) = fx.despawn_escaped_balls {
        despawn_homefront_balls_sync(ctx, sys_id, inst_id, &balls);
    }
    if let Some((sys_id, new_smugglers)) = fx.spawn_traffic_smugglers {
        if let Some(sys_h) = ctx.systems.get_handle(sys_id) {
            for (hid, h_pos) in new_smugglers {
                spawn_ball_sync_with_claim(
                    ctx,
                    &sys_h,
                    sys_id,
                    Ball {
                        id: hid,
                        type_id: 649_u32,
                        position: h_pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 140.0,
                        inertia: 0.9,
                        mass: 12_000_000.0,
                        radius: 220.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    },
                    CombatState {
                        shield_hp: 10_000.0,
                        max_shield_hp: 10_000.0,
                        armor_hp: 8_000.0,
                        max_armor_hp: 8_000.0,
                        structure_hp: 8_000.0,
                        max_structure_hp: 8_000.0,
                        is_tethered: false,
                        ..CombatState::default()
                    },
                );
            }
        }
    }
    if let Some((sys_id, old_hid, h_tid, pos, new_hid)) = fx.respawn_raid_hauler {
        if let Some(sys_h) = ctx.systems.get_handle(sys_id) {
            sys_h.try_remove_ball(old_hid);
            spawn_ball_sync_with_claim(
                ctx,
                &sys_h,
                sys_id,
                raid_hauler_ball(new_hid, h_tid, pos),
                ctx.build_type_combat_state(h_tid),
            );
        }
    }
    if let Some((sys_id, relay_id)) = fx.retether_relay {
        set_homefront_relay_tethered(ctx, sys_id, relay_id, true);
    }
    if let Some((sys_id, dread_id, base_id, dmg)) = fx.dread_siege_dmg {
        let _ = apply_homefront_direct_damage_in_space(ctx, sys_id, base_id, dmg);
        if let Some(sys_h) = ctx.systems.get_handle(sys_id) {
            let _ = sys_h.try_update_combat_state(dread_id, |d_cs| {
                d_cs.capacitor = 0.0;
            });
        }
    }
    if let Some((sys_id, target_id, dmg)) = fx.werpost_attack {
        if apply_homefront_direct_damage_in_space(ctx, sys_id, target_id, dmg) {
            with_mut_combat_ephemeral(ctx, |rt| {
                rt.npc_entities.remove(&target_id);
            });
            let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
                if let Some(rec) = snap.dungeon_runtime.homefront_runtimes.get_mut(&inst_id) {
                    rec.spawned_ball_ids.retain(|&id| id != target_id);
                }
                Ok(())
            });
        }
    }
    if let Some(next_w) = fx.spawn_aar_wave {
        spawn_next_aar_wave_in_space(ctx, inst_id, next_w);
    }
    if let Some((sys_id, pocket_pos)) = fx.spawn_basileus {
        let mut basileus_ids = None;
        let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), BridgeRpcError> {
            snap.next_entity_id += 1;
            let bid = snap.next_entity_id;
            snap.next_entity_id += 1;
            let cid = snap.next_entity_id;
            if let Some(rec) = snap.dungeon_runtime.homefront_runtimes.get_mut(&inst_id) {
                rec.spawned_ball_ids.push(bid);
                rec.spawned_ball_ids.push(cid);
                if let HomefrontProgressRecord::AbyssalArtifactRecovery {
                    basileus_ball_id,
                    cache_ball_id,
                    ..
                } = &mut rec.progress
                {
                    *basileus_ball_id = Some(bid);
                    *cache_ball_id = Some(cid);
                }
                basileus_ids = Some((bid, cid));
            }
            Ok(())
        });
        if let Some((bid, cid)) = basileus_ids {
            if let Some(sys_h) = ctx.systems.get_handle(sys_id) {
                spawn_aar_basileus_in_space(ctx, &sys_h, sys_id, bid, pocket_pos);
                spawn_aar_cache_in_space(ctx, &sys_h, sys_id, cid, pocket_pos);
            }
        }
    }
    if let Some((sys_id, balls)) = fx.despawn_fail {
        despawn_homefront_balls_sync(ctx, sys_id, inst_id, &balls);
    }
    if fx.settle_complete {
        try_settle_completed_homefront_live(ctx, inst_id);
        return;
    }

    // Ensure AAR wave escorts spawned inside DB-only destruction hooks also have ExtendedNpcEntity AI.
    if let HomefrontProgressRecord::AbyssalArtifactRecovery {
        wave_hostile_ball_ids,
        ..
    } = &pre.progress
    {
        if !wave_hostile_ball_ids.is_empty() {
            let missing_ids: Vec<i64> = with_combat_ephemeral(ctx, |rt| {
                wave_hostile_ball_ids
                    .iter()
                    .copied()
                    .filter(|id| !rt.npc_entities.contains_key(id))
                    .collect()
            });
            if !missing_ids.is_empty() {
                if let Some(sys_h) = ctx.systems.get_handle(pre.solar_system_id) {
                    let view = sys_h.view();
                    for (idx, eid) in missing_ids.into_iter().enumerate() {
                        if let Some(ball) = view.balls.get(&eid) {
                            register_homefront_npc_entity(
                                ctx,
                                pre.solar_system_id,
                                eid,
                                ball.type_id,
                                pre.blueprint.hostile_faction_id,
                                "HomefrontHostile",
                                ball.position,
                                [0.25, 0.25, 0.25, 0.25],
                                100.0 + (idx as f64) * 5.0,
                                3_500.0,
                                20_000.0,
                            );
                        }
                    }
                }
            }
        }
    }
}

/// Async entry point for server clock tick (`pve_special_sites` / `combat_npc_ai`).
pub async fn tick_homefront_operations_on_clock(ctx: &ServerContext) {
    tick_homefront_operations_sync(ctx);
}
