//! Daily downtime wreck/mission maintenance (`feat:lifecycle.wreck-downtime`, `feat:lifecycle.mission-downtime-respawn`).

use crate::router::ServerContext;
use crate::services::combat_crimewatch::with_mut_combat_runtime;
use eve_destiny::SlimItemMeta;
use glam::DVec3;

/// Daily downtime maintenance: purges stale NPC wrecks (`feat:lifecycle.wreck-downtime`)
/// and resets uncompleted (`MissionState::Accepted`) missions by despawning remaining NPCs and restoring
/// initial room/NPC state (`feat:lifecycle.mission-downtime-respawn`).
// parity: feat:lifecycle.wreck-downtime, feat:lifecycle.mission-downtime-respawn
pub fn cleanup_npc_wrecks_and_reset_missions_on_downtime(ctx: &ServerContext) -> (usize, usize) {
    let mut removed_wreck_balls: Vec<(u32, i64)> = Vec::new();
    let mut removed_mission_npcs: Vec<(u32, i64)> = Vec::new();
    let mut missions_reset_count: usize = 0;

    ctx.db.with_mut_snapshot(|snap| {
        {
            let st = snap.sp6_lifecycle.as_inner_mut();
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
        }

        // Despawn surviving mission NPCs, reset uncompleted accepted combat missions
        // (`feat:lifecycle.mission-downtime-respawn`), and clear stale wave metadata from
        // non-accepted missions.
        let downtime_ids: Vec<eve_db::MissionInstanceId> = snap
            .missions
            .iter()
            .filter(|m| {
                m.state == eve_db::MissionState::Accepted
                    || m.encounter().is_some_and(|enc| {
                        enc.initial_wave_count.is_some()
                            || !enc.initial_wave_types.is_empty()
                            || enc.awaiting_downtime_respawn
                    })
            })
            .map(|m| m.instance_id)
            .collect();
        for iid in downtime_ids {
            let _ = snap.missions.update(iid, |mission| {
                if mission.state != eve_db::MissionState::Accepted {
                    if let Some(enc) = mission.encounter_mut() {
                        enc.initial_wave_count = None;
                        enc.initial_wave_types.clear();
                        enc.awaiting_downtime_respawn = false;
                    }
                    return;
                }
                let sys_id = mission.solar_system_id;
                mission.current_room_id = 1;
                let Some(enc) = mission.encounter_mut() else {
                    return;
                };
                let initial_count = enc.initial_wave_count.unwrap_or(0);
                let old_spawned_len = enc.spawned_npc_ball_ids.len() as u32;
                let has_combat_wave =
                    initial_count > 0 || old_spawned_len > 0 || enc.npcs_remaining > 0;
                for &old_bid in &enc.spawned_npc_ball_ids {
                    removed_mission_npcs.push((sys_id, old_bid));
                }
                enc.spawned_npc_ball_ids.clear();
                if has_combat_wave {
                    let target_count = if initial_count > 0 {
                        initial_count
                    } else {
                        old_spawned_len.max(enc.npcs_remaining).max(4)
                    };
                    enc.npcs_remaining = target_count;
                    enc.awaiting_downtime_respawn = true;
                    missions_reset_count += 1;
                } else {
                    enc.npcs_remaining = 0;
                }
            });
        }
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
    let mut pending_keys: Vec<(u32, i64)> = ctx.db.with_snapshot(|snap| {
        snap.combat_crimewatch_state
            .pending_bounty_ticks
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
    spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
        ctx,
        char_id,
        solar_system_id,
        pocket_origin,
        None,
    )
}

/// Mission-scoped variant of [`spawn_mission_wave_on_pocket_entry_if_needed`]: when
/// `target_mission_id` is `Some(mid)`, only that accepted mission's wave spawns at `pocket_origin`,
/// preventing cross-mission wave spawns when a pilot holds multiple accepted missions in the same
/// solar system.
pub fn spawn_mission_wave_on_pocket_entry_for_mission_if_needed(
    ctx: &ServerContext,
    char_id: i64,
    solar_system_id: u32,
    pocket_origin: DVec3,
    target_mission_id: Option<u32>,
) -> usize {
    if !pocket_origin.is_finite() {
        return 0;
    }
    let now_unix = ctx.now_unix();
    let default_npc_tid =
        crate::services::agents_missions::agents_missions_helpers::resolve_mission_npc_type(ctx)
            .max(10_019);
    let mut spawned_npcs: Vec<(u32, i64, u32, DVec3)> = Vec::new();

    ctx.db.with_mut_snapshot(|snap| {
        let candidate_ids: Vec<eve_db::MissionInstanceId> = snap
            .missions
            .iter_by_character(char_id)
            .map(|m| m.instance_id)
            .collect();
        for iid in candidate_ids {
            let Some((wave_count, wave_types)) = snap.missions.get(iid).and_then(|mission| {
                let enc = mission.encounter()?;
                let matches_target = match target_mission_id {
                    Some(mid) => {
                        mission.template_id == mid
                            && crate::services::agents_missions::agents_missions_warp::resolve_mission_pocket_origin_in_snap(
                                ctx,
                                snap,
                                None,
                                mission,
                                solar_system_id,
                                now_unix,
                                false,
                            )
                            .is_ok()
                    }
                    None => {
                        let Ok(known_origin) =
                            crate::services::agents_missions::agents_missions_warp::resolve_mission_pocket_origin_in_snap(
                                ctx,
                                snap,
                                None,
                                mission,
                                solar_system_id,
                                now_unix,
                                mission.site_origin_m.is_some() || mission.bookmark_id.is_some(),
                            )
                        else {
                            return None;
                        };
                        known_origin
                            .is_none_or(|pos| pos.is_finite() && pocket_origin.distance(pos) <= 200_000.0)
                    }
                };
                let needs_respawn = enc.awaiting_downtime_respawn
                    || (enc.spawned_npc_ball_ids.is_empty() && enc.npcs_remaining > 0);
                if mission.state != eve_db::MissionState::Accepted
                    || mission.solar_system_id != solar_system_id
                    || !matches_target
                    || !needs_respawn
                {
                    return None;
                }
                let initial_count = enc.initial_wave_count.unwrap_or(0);
                let wave_count = if initial_count > 0 {
                    initial_count
                } else {
                    enc.npcs_remaining
                };
                Some((wave_count, enc.initial_wave_types.clone()))
            }) else {
                continue;
            };
            if wave_count == 0 {
                let _ = snap.missions.update(iid, |mission| {
                    if let Some(enc) = mission.encounter_mut() {
                        enc.awaiting_downtime_respawn = false;
                    }
                });
                continue;
            }
            let new_bids: Vec<i64> = (0..wave_count).map(|_| snap.alloc_entity_id()).collect();
            let _ = snap.missions.update(iid, |mission| {
                mission.current_room_id = 1;
                if let Some(enc) = mission.encounter_mut() {
                    enc.npcs_remaining = wave_count;
                    enc.awaiting_downtime_respawn = false;
                    enc.spawned_npc_ball_ids.clear();
                    for (idx, &npc_bid) in new_bids.iter().enumerate() {
                        let npc_tid = wave_types
                            .get(idx % wave_types.len().max(1))
                            .copied()
                            .unwrap_or(default_npc_tid);
                        enc.spawned_npc_ball_ids.push(npc_bid);
                        let angle = (idx as f64) * std::f64::consts::TAU / (wave_count as f64);
                        let pos = pocket_origin
                            + DVec3::new(angle.cos() * 4_500.0, 0.0, angle.sin() * 4_500.0);
                        spawned_npcs.push((solar_system_id, npc_bid, npc_tid, pos));
                    }
                }
            });
        }
    });

    let count = spawned_npcs.len();
    for (sys_id, npc_bid, npc_type_id, pos) in spawned_npcs {
        let handle = ctx.systems.get_or_spawn(sys_id);
        let sde_type = ctx.sde.types.get(&npc_type_id);
        let (mut ball, cs, mut entity) =
            crate::services::wormhole_pve_seeding::build_specialized_npc_components(
                ctx,
                sys_id,
                npc_bid,
                npc_type_id,
                "Pirate",
                1,
                pos,
            );
        if sde_type.is_none() {
            ball.max_velocity = 320.0;
            ball.mass = 1_200_000.0;
            ball.radius = 45.0;
            ball.inertia = 2.0;
            entity.faction_id = 500_010;
            entity.bounty_isk = 25_000.0;
            entity.optimal_range = 10_000.0;
            entity.orbit_range = 6_000.0;
            entity.damage_vector = eve_destiny::DamageVector::new(12.0, 15.0, 15.0, 12.0);
        } else {
            entity.bounty_isk = entity.bounty_isk.max(10_000.0);
        }
        let owner_faction_id = if entity.faction_id > 0 {
            entity.faction_id
        } else {
            500_010
        };
        let owner_id = i64::from(owner_faction_id);
        let npc_corp_id =
            crate::services::combat_crimewatch::combat_npc_ai::resolve_npc_corp_id_for_faction(
                ctx, owner_id,
            );
        let (group_id, category_id, name) = sde_type
            .map(|t| {
                (
                    t.group_id,
                    t.category_id,
                    (!t.name.is_empty()).then(|| t.name.clone()),
                )
            })
            .unwrap_or((546, 11, None));
        handle.try_spawn_ball_with_meta(
            ball,
            cs,
            SlimItemMeta {
                group_id,
                category_id,
                owner_id,
                char_id: None,
                corp_id: npc_corp_id,
                alliance_id: -1,
                security_status: -5.0,
                is_cloaked: false,
                name,
                ..Default::default()
            },
        );
        with_mut_combat_runtime(ctx, |rt| {
            rt.npc_entities.insert(npc_bid, entity);
        });
    }
    count
}

/// Clears any stored safe-logoff state for `char_id` on login, undock, ship board, or action.
pub fn clear_safe_logoff(db: &std::sync::Arc<eve_db::WorldRepository>, char_id: i64) {
    super::with_mut_sp6_state(db, |st| {
        st.safe_logoffs.remove(&char_id);
    });
}

/// Transitions live client sessions to character selection (or disconnects headless/stale
/// sessions) and releases character bindings/locks for `char_id` after safe logoff (#1360).
pub fn disconnect_safe_logoff_sessions(ctx: &crate::router::ServerContext, char_id: i64) {
    finalize_safe_logoff_sessions_inner(ctx, char_id, |_| {});
}

/// Clears `online_lock.character_id` and executes character-logoff side effects BEFORE
/// running `before_charsel_signal(had_online_char)` (removing the ship ball and emitting
/// `OnSafeLogoffActivated`), flushes any notifications staged inside an enclosing `dispatch_rpc`
/// scope (e.g. `timeControl.AdvanceClock`), and then delivers `"SafeLogoffCharsel"` on the live
/// closer channel.
pub(super) fn finalize_safe_logoff_sessions_with_hook<F>(
    ctx: &crate::router::ServerContext,
    char_id: i64,
    before_charsel_signal: F,
) where
    F: FnOnce(bool),
{
    finalize_safe_logoff_sessions_inner(ctx, char_id, before_charsel_signal);
}

fn finalize_safe_logoff_sessions_inner<F>(
    ctx: &crate::router::ServerContext,
    char_id: i64,
    before_charsel_signal: F,
) where
    F: FnOnce(bool),
{
    crate::services::campaigns_and_milestones::playtime::pause_milestone_playtime(
        &ctx.db,
        char_id,
        ctx.now_unix(),
    );
    let mut session_ids = std::collections::BTreeSet::new();
    let user_id = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&char_id).map(|c| c.user_id));
    let mut online_sid_for_char = None;
    let mut had_online_char = false;
    if let Some(uid) = user_id {
        if let Some(rec) = ctx.db.get_online_session(uid) {
            if rec.character_id.is_none_or(|cid| cid == char_id) {
                session_ids.insert(rec.session_id);
                online_sid_for_char = Some((uid, rec.session_id));
                had_online_char = rec.character_id == Some(char_id);
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
    let mut live_charsel: Option<(
        u64,
        u64,
        tokio::sync::mpsc::UnboundedSender<(String, String)>,
    )> = None;
    if let Some((uid, rec_sid)) = online_sid_for_char {
        let live_tx = ctx
            .session_closers
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&rec_sid)
            .filter(|tx| !tx.is_closed())
            .cloned();
        // Clear character_id while preserving session_id, computer_hash, and peer IP
        // BEFORE removing the ship ball or notifying the listener so notify_rx / sys_rx /
        // frame_rx deterministically observe charsel state.
        let lock_reset = live_tx.is_some()
            && ctx
                .db
                .acquire_online_lock(uid, rec_sid, None, false, ctx.now_unix())
                .is_ok();
        if lock_reset {
            if let Some(tx) = live_tx {
                live_charsel = Some((uid, rec_sid, tx));
                ctx.release_session_bindings(rec_sid);
            }
        } else {
            ctx.db.release_online_lock(uid, rec_sid);
        }
    }
    let live_charsel_sid = live_charsel.as_ref().map(|&(_, sid, _)| sid);
    for sid in session_ids {
        if Some(sid) == live_charsel_sid {
            continue;
        }
        ctx.disconnect_session(sid, "SafeLogoff", "Safe logoff completed");
        ctx.release_session_bindings(sid);
    }
    crate::services::character_skills::notify_watchlist_presence(ctx, char_id, false);
    crate::services::fleet_social::fleet_social_notify::on_character_logout(ctx, char_id);
    crate::services::fleet_social::fleet_social_notify::handle_character_disconnect(ctx, char_id);
    crate::services::fleet_social::revoke_xmpp_auth_token(ctx, char_id);
    if let Ok(mut cn) = ctx.systems.cluster_nodes.write() {
        cn.char_rebased_system_gen.remove(&char_id);
    }

    before_charsel_signal(had_online_char);

    // Flush notifications staged inside an enclosing `dispatch_rpc` scope (e.g.
    // `timeControl.AdvanceClock`) BEFORE sending `"SafeLogoffCharsel"` on `session_closers` so
    // the player's `macho_listener` observes `OnSafeLogoffActivated` in `pending_notifications`
    // and `notify_tx` before clearing `session.character_id = None` (#1360, #1386).
    crate::router::cache_notifications::flush_staged_dispatch_notifications(
        &*ctx.pending_notifications,
        &ctx.notify_tx,
    );

    if let Some((uid, rec_sid, tx)) = live_charsel {
        if tx
            .send(("SafeLogoffCharsel".to_string(), char_id.to_string()))
            .is_err()
        {
            ctx.db.release_online_lock(uid, rec_sid);
            ctx.disconnect_session(rec_sid, "SafeLogoff", "Safe logoff completed");
        }
    }
}

pub(super) fn warn_sync_deadline_overrun(
    kind: &'static str,
    sys_id: Option<u32>,
    tick_secs: Option<u64>,
    elapsed: std::time::Duration,
    budget: std::time::Duration,
) {
    if elapsed > budget {
        tracing::warn!(
            solar_system_id = ?sys_id,
            tick_secs = ?tick_secs,
            elapsed_ms = elapsed.as_millis() as u64,
            budget_ms = budget.as_millis() as u64,
            "{kind} exceeded deadline budget"
        );
    }
}

/// Runs synchronous per-system NPC AI/spawns off the Tokio worker pool under a bounded timeout (`#1170`, `#1208`).
pub(super) async fn step_system_ai_and_combat_bounded(
    ctx: &ServerContext,
    handle: &crate::system_actor::SystemHandle,
    sys_id: eve_sde::SolarSystemId,
    deadline: std::time::Duration,
) {
    let ctx_ai = ctx.clone();
    let handle_ai = handle.clone();
    let npc_ai_start = std::time::Instant::now();
    match tokio::time::timeout(
        deadline,
        tokio::task::spawn_blocking(move || {
            crate::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(
                &ctx_ai, sys_id,
            );
            let has_spawns = ctx_ai
                .npc_spawns
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&sys_id)
                .is_some_and(|mgr| !mgr.entities.is_empty());
            if has_spawns {
                let tick = handle_ai.view().current_tick;
                let tidi =
                    crate::services::serverops_cluster::get_system_tidi_factor(&ctx_ai, sys_id);
                crate::combat::npc_ai::step_system_npc_spawns_ai_sync(
                    &ctx_ai,
                    sys_id,
                    tick,
                    1.0 * tidi,
                );
            }
        }),
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(join_err)) => {
            tracing::error!(
                solar_system_id = sys_id,
                error = %join_err,
                "per-system NPC AI blocking step panicked"
            );
        }
        Err(_) => {
            tracing::warn!(
                solar_system_id = sys_id,
                elapsed_ms = npc_ai_start.elapsed().as_millis() as u64,
                timeout_ms = deadline.as_millis() as u64,
                "per-system NPC AI / spawns step exceeded deadline timeout"
            );
        }
    }
}

#[cfg(test)]
#[path = "downtime_cleanup_tests.rs"]
mod downtime_cleanup_tests;

#[cfg(test)]
#[path = "cluster_tick_downtime_extra_tests.rs"]
mod cluster_tick_downtime_extra_tests;
