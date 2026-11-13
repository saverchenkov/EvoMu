//! Daily downtime wreck/mission maintenance (`feat:lifecycle.wreck-downtime`, `feat:lifecycle.mission-downtime-respawn`).

use crate::router::ServerContext;
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use super::with_mut_sp6_state;
use eve_destiny::{Ball, MovementMode, SlimItemMeta};
use glam::DVec3;
use std::collections::HashMap;

/// Daily downtime maintenance: purges stale NPC wrecks (`feat:lifecycle.wreck-downtime`)
/// and resets uncompleted ("Accepted") missions by despawning remaining NPCs and restoring
/// initial room/NPC state (`feat:lifecycle.mission-downtime-respawn`).
// parity: feat:lifecycle.wreck-downtime, feat:lifecycle.mission-downtime-respawn
pub fn cleanup_npc_wrecks_and_reset_missions_on_downtime(ctx: &ServerContext) -> (usize, usize) {
    let mut removed_wreck_balls: Vec<(u32, i64)> = Vec::new();
    let mut removed_mission_npcs: Vec<(u32, i64)> = Vec::new();
    let mut missions_reset_count: usize = 0;

    with_mut_sp6_state(&ctx.db, |st, snap| {
        let npc_wreck_ids: Vec<(i64, u32)> = snap
            .items
            .values()
            .filter(|i| {
                let is_wreck = i.type_id == 26557
                    || ctx
                        .sde
                        .types
                        .get(&i.type_id)
                        .is_some_and(|t| t.group_id == 186);
                let is_npc_wreck = st
                    .space_object_meta
                    .get(&i.item_id)
                    .is_some_and(|m| m.is_npc_wreck);
                is_wreck && is_npc_wreck && (30_000_000..=34_000_000).contains(&i.location_id)
            })
            .map(|i| (i.item_id, i.location_id as u32))
            .collect();

        for (wid, sys_id) in npc_wreck_ids {
            snap.items.remove(&wid);
            snap.item_positions.remove(&wid);
            st.space_object_meta.remove(&wid);
            st.ship_combat_hp.remove(&wid);
            snap.items.retain(|_, child| child.location_id != wid);
            removed_wreck_balls.push((sys_id, wid));
        }

        // Despawn surviving mission NPCs and reset uncompleted ("Accepted") combat missions (`feat:lifecycle.mission-downtime-respawn`)
        for (&map_key, mission) in snap.active_missions.iter_mut() {
            if mission.state == "Accepted" {
                let mkey = format!("{}:{}", mission.character_id, mission.mission_id);
                let initial_count = st
                    .mission_initial_npc_counts
                    .get(&mkey)
                    .copied()
                    .unwrap_or(0);
                let old_spawned_len = mission.spawned_npc_ball_ids.len() as u32;
                let has_combat_wave =
                    initial_count > 0 || old_spawned_len > 0 || mission.npcs_remaining > 0;
                for &old_bid in &mission.spawned_npc_ball_ids {
                    removed_mission_npcs.push((mission.solar_system_id, old_bid));
                }
                mission.spawned_npc_ball_ids.clear();
                mission.current_room_id = 1;
                if has_combat_wave {
                    let target_count = if initial_count > 0 {
                        initial_count
                    } else {
                        old_spawned_len.max(mission.npcs_remaining).max(4)
                    };
                    mission.npcs_remaining = target_count;
                    if !st.missions_awaiting_downtime_respawn.contains(&map_key) {
                        st.missions_awaiting_downtime_respawn.push(map_key);
                    }
                    missions_reset_count += 1;
                } else {
                    mission.npcs_remaining = 0;
                }
            }
        }
        let active_accepted_keys: std::collections::HashSet<String> = snap
            .active_missions
            .values()
            .filter(|m| m.state == "Accepted")
            .map(|m| format!("{}:{}", m.character_id, m.mission_id))
            .collect();
        st.mission_initial_npc_counts
            .retain(|k, _| active_accepted_keys.contains(k));
        st.mission_initial_npc_types
            .retain(|k, _| active_accepted_keys.contains(k));
        st.missions_awaiting_downtime_respawn.retain(|k| {
            snap.active_missions
                .get(k)
                .is_some_and(|m| m.state == "Accepted")
        });
    });

    let wrecks_removed_count = removed_wreck_balls.len();
    if !removed_wreck_balls.is_empty() || !removed_mission_npcs.is_empty() {
        with_mut_combat_runtime(ctx, |rt| {
            for &(_, wid) in &removed_wreck_balls {
                rt.container_lifetimes.remove(&wid);
                rt.npc_entities.remove(&wid);
            }
            for &(_, bid) in &removed_mission_npcs {
                rt.npc_entities.remove(&bid);
            }
        });
    }
    for (sys_id, ball_id) in removed_wreck_balls.into_iter().chain(removed_mission_npcs) {
        let handle = ctx.systems.get_or_spawn(sys_id);
        handle.try_remove_ball(ball_id);
    }

    crate::services::fleet_social::execute_pending_auto_kicks_on_downtime(ctx);
    let _bounty_flushed = flush_pending_bounty_ticks_on_downtime(ctx);
    let _probes_recovered = recover_scanner_probes_on_downtime(ctx);
    ctx.sync_runtime_to_db();

    (wrecks_removed_count, missions_reset_count)
}

/// Returns non-expired scanner probes to their ship launcher clip or cargo bay during daily
/// downtime and purges already-expired probes instead of resurrecting them (`#543`).
/// Returns `(returned_count, purged_expired_count)`.
pub fn recover_scanner_probes_on_downtime(ctx: &ServerContext) -> (usize, usize) {
    let now_unix = ctx.now_unix();
    let mut candidates: Vec<crate::router::ActiveScannerProbe> = Vec::new();
    let mut purged_expired: Vec<crate::router::ActiveScannerProbe> = Vec::new();

    if let Ok(mut map) = ctx.active_scanner_probes.write() {
        let mut all_ids: Vec<i64> = map.keys().copied().collect();
        all_ids.sort_unstable();
        for pid in all_ids {
            if let Some(p) = map.remove(&pid) {
                let is_expired = p.state < 0
                    || now_unix
                        >= p.launched_at_unix
                            .saturating_add(p.max_flight_time_s.max(0.0) as u64);
                if is_expired {
                    purged_expired.push(p);
                } else {
                    candidates.push(p);
                }
            }
        }
    }

    let (recovered, unrecovered) = if !candidates.is_empty() {
        ctx.db.with_mut_snapshot(|snap| {
            let mut rec = Vec::new();
            let mut unrec = Vec::new();
            let add_stack = |items: &mut eve_db::CowMap<i64, eve_db::InventoryItemRecord>,
                             next_id: &mut i64,
                             owner_id: i64,
                             ship_id: i64,
                             flag: eve_db::InventoryFlag,
                             p_type_id: u32,
                             qty: i32,
                             exclude_id: i64| {
                let existing_id = items
                    .values()
                    .find(|i| {
                        i.owner_id == owner_id
                            && i.location_id == ship_id
                            && i.flag == flag
                            && i.item_id != exclude_id
                            && i.type_id == p_type_id
                    })
                    .map(|i| i.item_id);
                if let Some(st) = existing_id.and_then(|id| items.get_mut(&id)) {
                    st.quantity += qty;
                    st.version += 1;
                } else {
                    *next_id += 1;
                    let new_id = *next_id;
                    items.insert(
                        new_id,
                        eve_db::InventoryItemRecord {
                            item_id: new_id,
                            type_id: p_type_id,
                            owner_id,
                            location_id: ship_id,
                            flag,
                            quantity: qty,
                            singleton: false,
                            version: 1,
                            ..Default::default()
                        },
                    );
                }
            };

            for mut p in candidates {
                let (char_id, ship_id, launcher_id, p_type_id) = (
                    p.owner_char_id,
                    p.ship_id,
                    p.launcher_module_id,
                    p.probe_type_id,
                );
                if !snap.items.contains_key(&ship_id) {
                    p.state = 0;
                    p.warp_end_ft = None;
                    unrec.push(p);
                    continue;
                }

                let mut placed_in_launcher = false;
                if let Some(li) = snap
                    .items
                    .get(&launcher_id)
                    .cloned()
                    .filter(|i| i.location_id == ship_id)
                {
                    let cur_clip: i32 = snap
                        .items
                        .values()
                        .filter(|i| {
                            i.owner_id == char_id
                                && i.location_id == ship_id
                                && i.flag == li.flag
                                && i.item_id != launcher_id
                        })
                        .map(|i| i.quantity)
                        .sum();
                    let matching_clip = snap.items.values().any(|i| {
                        i.owner_id == char_id
                            && i.location_id == ship_id
                            && i.flag == li.flag
                            && i.item_id != launcher_id
                            && i.type_id == p_type_id
                    });
                    if cur_clip == 0 || (cur_clip < 8 && matching_clip) {
                        add_stack(
                            &mut snap.items,
                            &mut snap.next_entity_id,
                            char_id,
                            ship_id,
                            li.flag,
                            p_type_id,
                            1,
                            launcher_id,
                        );
                        placed_in_launcher = true;
                    }
                }

                if placed_in_launcher {
                    rec.push(p);
                    continue;
                }

                let ship_cargo_cap = snap
                    .items
                    .get(&ship_id)
                    .and_then(|si| {
                        si.dynamic_attributes
                            .get(&eve_sde::attr_ids::CAPACITY)
                            .copied()
                            .or_else(|| {
                                ctx.sde.types.get(&si.type_id).map(|t| {
                                    t.default_attributes
                                        .get(&eve_sde::attr_ids::CAPACITY)
                                        .copied()
                                        .unwrap_or(t.capacity)
                                })
                            })
                    })
                    .unwrap_or(0.0);
                let used_cargo_vol: f64 = snap
                    .items
                    .values()
                    .filter(|i| {
                        i.location_id == ship_id && i.flag == eve_db::InventoryFlag::Cargo
                    })
                    .map(|i| {
                        ctx.sde
                            .types
                            .get(&i.type_id)
                            .map(|t| t.volume)
                            .unwrap_or(0.1)
                            * (i.quantity.max(1) as f64)
                    })
                    .sum();
                let probe_vol = ctx
                    .sde
                    .types
                    .get(&p_type_id)
                    .map(|t| t.volume)
                    .filter(|&v| v > 0.0)
                    .unwrap_or(0.1);
                if ship_cargo_cap > 0.0 && used_cargo_vol + probe_vol <= ship_cargo_cap + 1e-6 {
                    add_stack(
                        &mut snap.items,
                        &mut snap.next_entity_id,
                        char_id,
                        ship_id,
                        eve_db::InventoryFlag::Cargo,
                        p_type_id,
                        1,
                        0,
                    );
                    rec.push(p);
                } else {
                    p.state = 0;
                    p.warp_end_ft = None;
                    unrec.push(p);
                }
            }
            (rec, unrec)
        })
    } else {
        (Vec::new(), Vec::new())
    };

    if !unrecovered.is_empty() {
        if let Ok(mut map) = ctx.active_scanner_probes.write() {
            for p in unrecovered {
                map.insert(p.probe_id, p);
            }
        }
    }
    if !recovered.is_empty() || !purged_expired.is_empty() || ctx.db.with_snapshot(|s| !crate::sp6_lifecycle::load_sp6_state_from_snap(s).active_scanner_probes.is_empty()) {
        crate::services::space_probe_scanning::sync_scanner_probes_to_db(ctx);
    }

    for p in recovered.iter().chain(purged_expired.iter()) {
        let handle = ctx.systems.get_or_spawn(p.solar_system_id);
        handle.try_remove_ball(p.probe_id);
    }

    (recovered.len(), purged_expired.len())
}

/// Flushes all pending 20-minute batched NPC bounty ticks during daily cluster downtime
/// so ratting payouts (including corp tax and null-sec ESS splits) are never stranded across DT.
pub fn flush_pending_bounty_ticks_on_downtime(ctx: &ServerContext) -> usize {
    let mut pending_keys: Vec<(u32, i64)> =
        crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.pending_bounty_ticks
                .iter()
                .filter(|&(_, &amount)| amount > 0.0)
                .map(|(&key, _)| key)
                .collect()
        });
    pending_keys.sort_unstable();
    let mut flushed = 0usize;
    for (sys_id, char_id) in pending_keys {
        let payout =
            crate::services::combat_npc_encounters::flush_npc_bounty_tick(ctx, sys_id, char_id, 0.0);
        if payout.pilot_isk > 0.0 || payout.main_ess_isk > 0.0 || payout.reserve_ess_isk > 0.0 {
            flushed += 1;
        }
    }
    flushed
}

/// Spawns the initial wave of SDE-backed pirate NPCs when a pilot warps to their accepted mission pocket
/// after daily downtime reset (`feat:lifecycle.mission-downtime-respawn`).
pub fn spawn_mission_wave_on_pocket_entry_if_needed(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: u32,
    pocket_origin: DVec3,
) -> usize {
    let default_npc_tid =
        crate::services::agents_missions::agents_missions_helpers::resolve_mission_npc_type(ctx)
            .max(10_019);
    let mut spawned_npcs: Vec<(u32, i64, u32, DVec3)> = Vec::new();

    with_mut_sp6_state(&ctx.db, |st, snap| {
        let mut keys: Vec<(i64, u32)> = snap.active_missions.keys().copied().collect();
        keys.sort_unstable();
        for (cid, aid) in keys {
            let Some((mission_id, cur_npcs)) =
                snap.active_missions.get(&(cid, aid)).and_then(|mission| {
                    let awaiting = st.missions_awaiting_downtime_respawn.contains(&(cid, aid));
                    let needs_respawn = awaiting
                        || (mission.spawned_npc_ball_ids.is_empty() && mission.npcs_remaining > 0);
                    (cid == char_id
                        && mission.state == "Accepted"
                        && mission.solar_system_id == solar_system_id
                        && needs_respawn)
                        .then_some((mission.mission_id, mission.npcs_remaining))
                })
            else {
                continue;
            };
            st.missions_awaiting_downtime_respawn
                .retain(|&k| k != (cid, aid));
            let mkey = format!("{cid}:{mission_id}");
            let initial_count = st
                .mission_initial_npc_counts
                .get(&mkey)
                .copied()
                .unwrap_or(0);
            if initial_count == 0 && cur_npcs == 0 {
                continue;
            }
            let wave_count = if initial_count > 0 {
                initial_count
            } else {
                cur_npcs.max(4)
            };
            let wave_types = st.mission_initial_npc_types.get(&mkey).cloned();
            let new_bids: Vec<i64> = (0..wave_count).map(|_| snap.alloc_entity_id()).collect();
            if let Some(mission) = snap.active_missions.get_mut(&(cid, aid)) {
                mission.npcs_remaining = wave_count;
                mission.current_room_id = 1;
                mission.spawned_npc_ball_ids.clear();
                for (idx, npc_bid) in new_bids.into_iter().enumerate() {
                    let npc_tid = wave_types
                        .as_ref()
                        .filter(|v| !v.is_empty())
                        .map(|v| v[idx % v.len()])
                        .unwrap_or(default_npc_tid);
                    mission.spawned_npc_ball_ids.push(npc_bid);
                    let angle = (idx as f64) * std::f64::consts::TAU / (wave_count as f64);
                    let pos = pocket_origin
                        + DVec3::new(angle.cos() * 4_500.0, 0.0, angle.sin() * 4_500.0);
                    spawned_npcs.push((solar_system_id, npc_bid, npc_tid, pos));
                }
            }
        }
    });

    let count = spawned_npcs.len();
    for (sys_id, npc_bid, npc_type_id, pos) in spawned_npcs {
        let handle = ctx.systems.get_or_spawn(sys_id);
        let sde_type = ctx.sde.types.get(&npc_type_id);
        let (group_id, category_id, faction_id, max_vel, mass, radius, inertia, opt_range, bounty, dmg_vec) =
            sde_type
                .map(|t| {
                    let mv = t.default_attributes.get(&37).copied().unwrap_or(320.0);
                    let agi = t.default_attributes.get(&70).copied().unwrap_or(2.0);
                    let opt = t.default_attributes.get(&54).copied().unwrap_or(10_000.0);
                    let bty = t.default_attributes.get(&481).copied().unwrap_or(25_000.0);
                    let mult = t.default_attributes.get(&64).copied().unwrap_or(1.0).max(0.5);
                    let em = t.default_attributes.get(&114).copied().unwrap_or(12.0) * mult;
                    let ex = t.default_attributes.get(&116).copied().unwrap_or(12.0) * mult;
                    let ki = t.default_attributes.get(&117).copied().unwrap_or(15.0) * mult;
                    let th = t.default_attributes.get(&118).copied().unwrap_or(15.0) * mult;
                    (
                        t.group_id,
                        t.category_id,
                        t.faction_id.unwrap_or(500_010),
                        mv.max(150.0),
                        t.mass.max(100_000.0),
                        t.radius.max(30.0),
                        agi.max(0.5),
                        opt.max(5_000.0),
                        bty.max(10_000.0),
                        eve_destiny::DamageVector::new(em, th, ki, ex),
                    )
                })
                .unwrap_or((
                    546,
                    11,
                    500_010,
                    320.0,
                    1_200_000.0,
                    45.0,
                    2.0,
                    10_000.0,
                    25_000.0,
                    eve_destiny::DamageVector::new(12.0, 15.0, 15.0, 12.0),
                ));
        let npc_corp_id = ctx
            .sde
            .npc_corporations
            .values()
            .find(|c| c.faction_id == Some(faction_id))
            .map(|c| c.corporation_id as i64)
            .unwrap_or(1_000_127);
        let cs = ctx.build_type_combat_state(npc_type_id);
        handle.try_spawn_ball_with_meta(
            Ball {
                id: npc_bid,
                type_id: npc_type_id,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: max_vel,
                inertia,
                mass,
                radius,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cs,
            SlimItemMeta {
                group_id,
                category_id,
                owner_id: faction_id as i64,
                char_id: None,
                corp_id: npc_corp_id,
                alliance_id: -1,
                security_status: -5.0,
                is_cloaked: false, ..Default::default()
            },
        );
        with_mut_combat_runtime(ctx, |rt| {
            rt.npc_entities.insert(
                npc_bid,
                crate::services::combat_crimewatch::ExtendedNpcEntity {
                    ball_id: npc_bid,
                    type_id: npc_type_id,
                    faction_id,
                    archetype: "Pirate".into(),
                    solar_system_id: sys_id,
                    spawn_group_id: 1,
                    target_id: None,
                    aggression_threat: HashMap::new(),
                    threat_table: HashMap::new(),
                    bounty_isk: bounty,
                    optimal_range: opt_range,
                    orbit_range: (opt_range * 0.6).clamp(2_500.0, 15_000.0),
                    damage_vector: dmg_vec,
                    entropic_cycles: 0,
                    drifter_overshield_hp: 0.0,
                    drifter_doomsday_fired: false,
                    web_speed_factor: 0.0,
                    web_range: 0.0,
                    scram_strength: 0.0,
                    scram_range: 0.0,
                    neut_amount: 0.0,
                    ecm_jammed: false,
                    remote_shield_rep: 0.0,
                    remote_armor_rep: 0.0,
                    remote_rep_range: 0.0,
                    tag_drop_type_id: None,
                    custom_cargo_loot: None,
                    spawn_position: pos,
                    sleeper_wh_class: 0,
                },
            );
        });
    }
    count
}

/// Clears any stored safe-logoff state for `char_id` on login, undock, ship board, or action.
pub fn clear_safe_logoff(db: &std::sync::Arc<eve_db::WorldRepository>, char_id: i64) {
    super::with_mut_sp6_state(db, |st, _snap| {
        st.safe_logoffs.remove(&char_id);
    });
}

/// Disconnects active client sessions and releases online locks for `char_id` after safe logoff.
pub fn disconnect_safe_logoff_sessions(ctx: &crate::router::ServerContext, char_id: i64) {
    crate::services::campaigns_and_milestones::playtime::pause_milestone_playtime(
        &ctx.db,
        char_id,
        ctx.now_unix(),
    );
    let mut session_ids = std::collections::BTreeSet::new();
    let user_id = ctx.db.with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id));
    if let Some(uid) = user_id {
        if let Some(rec) = ctx.db.get_online_session(uid) {
            if rec.character_id.is_none_or(|cid| cid == char_id) {
                session_ids.insert(rec.session_id);
                ctx.db.release_online_lock(uid, rec.session_id);
            }
        }
    }
    if let Ok(bound) = ctx.bound_objects.read() {
        for entry in bound.values() {
            if entry.character_id == char_id {
                session_ids.insert(entry.session_id);
            }
        }
    }
    for sid in session_ids {
        ctx.disconnect_session(sid, "SafeLogoff", "Safe logoff completed");
        ctx.release_session_bindings(sid);
    }
    crate::services::fleet_social::fleet_social_notify::on_character_logout(ctx, char_id);
    crate::services::fleet_social::fleet_social_notify::handle_character_disconnect(ctx, char_id);
}

#[cfg(test)]
#[path = "downtime_cleanup_tests.rs"]
mod downtime_cleanup_tests;

#[cfg(test)]
#[path = "cluster_tick_downtime_extra_tests.rs"]
mod cluster_tick_downtime_extra_tests;
