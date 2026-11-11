//! `sov_gameplay_entosis_capture` — Live Entosis Link Target Registration, Contested Pause & De-Capture (`#512`).

#![forbid(unsafe_code)]

use super::{
    adm_for_system, ENTOSIS_HUB_BASE_CAPTURE_SECS, ENTOSIS_MAX_RANGE_M,
    ENTOSIS_NODE_BASE_CAPTURE_SECS, SOV_COMMAND_NODE_INITIAL_PROGRESS,
    SOV_COMMAND_NODE_SCORE_DELTA, TYPE_STRUCTURE_COMMAND_NODE,
};
use crate::router::{NotificationTarget, ServerContext};
use crate::services::sov_services::{emit_sov_structures_updated, TYPE_SOVEREIGNTY_HUB};
use eve_db::InventoryItemRecord;
use glam::DVec3;
use std::collections::HashSet;

/// Registers `target_id` in `entosis_capture_progress` after `handle_entosis_link_activation` starts
/// the warmup cycle (`warmup_done_unix = now + cycle_secs`), preserving any existing `controlling_side`
/// (`existing.1`) when opposing alliances contest or de-capture partial progress.
pub fn register_entosis_capture_target(
    ctx: &ServerContext,
    target_id: i64,
    ship_id: i64,
    char_id: i64,
    warmup_done_unix: u64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let Some(ch) = snap.characters.get(&char_id) else {
            return;
        };
        let Some(alliance_id) = ch
            .alliance_id
            .or_else(|| snap.corporations.get(&ch.corporation_id).and_then(|c| c.alliance_id))
            .filter(|&a| a > 0)
        else {
            return;
        };
        let holder = snap
            .structures
            .get(&target_id)
            .and_then(|st| snap.system_states.get(&st.solar_system_id).and_then(|s| s.sov_alliance_id));
        if holder == Some(alliance_id) {
            return;
        }
        if let Some(existing) = snap.sov_runtime.entosis_capture_progress.get_mut(&target_id) {
            if existing.3 <= 1e-6 && existing.2 == 0 {
                existing.0 = char_id;
                existing.1 = alliance_id;
                existing.2 = ship_id;
                existing.4 = warmup_done_unix;
            } else if existing.1 == alliance_id && existing.2 == 0 {
                existing.0 = char_id;
                existing.2 = ship_id;
                existing.4 = existing.4.max(warmup_done_unix);
            }
            return;
        }
        snap.sov_runtime
            .entosis_capture_progress
            .insert(target_id, (char_id, alliance_id, ship_id, 0.0, warmup_done_unix));
    });
}

pub(super) fn remove_ship_entosis_link(ctx: &ServerContext, ship_id: i64) {
    let db_key = crate::services::tactical_modules::db_ptr_key(&ctx.db);
    let removed = {
        let mut st = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        let link = st.entosis_active_links.remove(&(db_key, ship_id));
        if let Some(ref l) = link {
            st.entosis_target_cycles.remove(&(db_key, l.target_id, ship_id));
        }
        link
    };
    if let Some(link) = removed {
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.remove(&link.module_id);
        });
    }
}

fn renew_or_expire_entosis_cycle(
    ctx: &ServerContext,
    db_key: usize,
    now_unix: u64,
    last_ts: u64,
    ship_id: i64,
    mod_id: i64,
    cycle_dur_s: f64,
    mut exp_u: u64,
) -> (bool, f64) {
    let dt = now_unix.saturating_sub(last_ts) as f64;
    if now_unix < exp_u {
        return (true, dt);
    }
    let cycle_len = (cycle_dur_s.round() as u64).max(120);
    let extra_cycles = ((now_unix - exp_u) / cycle_len) + 1;
    let stront_ok = ctx.db.with_mut_snapshot(|snap| {
        let needed = extra_cycles as i32;
        let stront_id = snap
            .items
            .values()
            .find(|i| i.location_id == ship_id && i.type_id == 16_275 && i.quantity >= needed)
            .map(|i| i.item_id);
        if let Some(sid_item) = stront_id {
            if let Some(it) = snap.items.get_mut(&sid_item) {
                it.quantity -= needed;
                if it.quantity <= 0 {
                    snap.items.remove(&sid_item);
                }
            }
            true
        } else {
            false
        }
    });
    if stront_ok {
        exp_u = exp_u.saturating_add(extra_cycles * cycle_len);
        let mut st = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
        if let Some(l) = st.entosis_active_links.get_mut(&(db_key, ship_id)) {
            if l.module_id == mod_id {
                l.started_unix = exp_u - cycle_len;
                l.expires_unix = exp_u;
                l.cycle_index = l.cycle_index.saturating_add(extra_cycles as u32);
            }
        }
        (true, dt)
    } else {
        (false, exp_u.saturating_sub(last_ts) as f64)
    }
}

pub(super) async fn tick_entosis_captures(ctx: &ServerContext, now_unix: u64) {
    let entries: Vec<(i64, (i64, i64, i64, f64, u64))> = ctx.db.with_snapshot(|snap| {
        snap.sov_runtime
            .entosis_capture_progress
            .iter()
            .map(|(&t, &v)| (t, v))
            .collect()
    });
    if entries.is_empty() {
        return;
    }
    let db_key = crate::services::tactical_modules::db_ptr_key(&ctx.db);
    let sov_types = crate::esi::sov_fw_routes::sov_structure_type_ids(&ctx.sde);

    for (target_id, (mut char_id, mut alliance_id, mut ship_id, mut accrued_secs, last_ts)) in entries {
        let meta = ctx.db.with_snapshot(|snap| {
            if let Some(st) = snap.structures.get(&target_id) {
                if (st.type_id == TYPE_SOVEREIGNTY_HUB || sov_types.contains(&st.type_id))
                    && st.state != "Destroyed"
                {
                    let sys = st.solar_system_id;
                    let adm = adm_for_system(snap, sys);
                    let holder = snap.system_states.get(&sys).and_then(|s| s.sov_alliance_id);
                    return Some((
                        true,
                        sys,
                        DVec3::from_array(st.position),
                        ENTOSIS_HUB_BASE_CAPTURE_SECS * adm,
                        holder,
                    ));
                }
            }
            if let Some(&(hub_id, n_sys, _)) = snap.sov_runtime.command_nodes.get(&target_id) {
                let hub_sys = snap.structures.get(&hub_id).map(|s| s.solar_system_id).unwrap_or(n_sys);
                let adm = adm_for_system(snap, hub_sys);
                let pos = snap
                    .item_positions
                    .get(&target_id)
                    .copied()
                    .unwrap_or([120_000.0, 0.0, 0.0]);
                return Some((false, n_sys, DVec3::from_array(pos), ENTOSIS_NODE_BASE_CAPTURE_SECS * adm, None));
            }
            None
        });
        let Some((is_hub, sys_id, fallback_pos, required_secs, holder)) = meta else {
            ctx.db.with_mut_snapshot(|snap| {
                snap.sov_runtime.entosis_capture_progress.remove(&target_id);
            });
            continue;
        };

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let target_pos = sys_handle
            .get_ball_snapshot(target_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position)
            .unwrap_or(fallback_pos);

        let all_links: Vec<(i64, i64, i64, i64, f64, u64)> = {
            let st = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner());
            st.entosis_active_links
                .iter()
                .filter(|(&(k, _), l)| k == db_key && l.target_id == target_id)
                .map(|(&(_, sid), l)| {
                    (sid, l.char_id, l.side_id, l.module_id, l.cycle_duration_s, l.expires_unix)
                })
                .collect()
        };
        let mut valid_links = Vec::new();
        for (sid, cid, side, mid, cdur, exp) in all_links {
            let in_range = sys_handle
                .get_ball_snapshot(sid)
                .await
                .ok()
                .flatten()
                .is_some_and(|(_, sb)| sb.position.distance(target_pos) <= ENTOSIS_MAX_RANGE_M);
            if !in_range {
                remove_ship_entosis_link(ctx, sid);
                if sid == ship_id {
                    ship_id = 0;
                }
                continue;
            }
            valid_links.push((sid, cid, side, mid, cdur, exp));
        }

        if now_unix <= last_ts {
            continue;
        }
        let dt = now_unix.saturating_sub(last_ts) as f64;
        let distinct_sides: HashSet<i64> = valid_links.iter().map(|l| l.2).collect();

        // 1. Multiple distinct alliances/sides linking -> progress paused on both Hubs and Command Nodes.
        if distinct_sides.len() > 1 {
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(e) = snap.sov_runtime.entosis_capture_progress.get_mut(&target_id) {
                    e.4 = now_unix;
                }
            });
            continue;
        }

        // 2. Unattended partial capture -> slowly regenerate back to 0 (up to 50 min = 3,000s).
        if valid_links.is_empty() {
            let regen_rate = required_secs / 3_000.0;
            accrued_secs = (accrued_secs - regen_rate * dt).max(0.0);
            ctx.db.with_mut_snapshot(|snap| {
                if accrued_secs <= 1e-6 {
                    snap.sov_runtime.entosis_capture_progress.remove(&target_id);
                } else {
                    snap.sov_runtime
                        .entosis_capture_progress
                        .insert(target_id, (char_id, alliance_id, 0, accrued_secs, now_unix));
                }
            });
            continue;
        }

        let sole_side = *distinct_sides.iter().next().expect("non-empty");

        // 3. Hub defender counter-entosising uncontested -> count capture progress back down to 0.
        if is_hub && holder == Some(sole_side) {
            accrued_secs = (accrued_secs - dt).max(0.0);
            if accrued_secs <= 1e-6 {
                for (def_sid, _, _, _, _, _) in valid_links {
                    remove_ship_entosis_link(ctx, def_sid);
                }
                ctx.db.with_mut_snapshot(|snap| {
                    snap.sov_runtime.entosis_capture_progress.remove(&target_id);
                });
            } else {
                ctx.db.with_mut_snapshot(|snap| {
                    snap.sov_runtime
                        .entosis_capture_progress
                        .insert(target_id, (char_id, alliance_id, 0, accrued_secs, now_unix));
                });
            }
            continue;
        }

        let active_link = valid_links
            .iter()
            .find(|l| l.0 == ship_id)
            .copied()
            .unwrap_or(valid_links[0]);
        let (stront_alive, eff_dt) = renew_or_expire_entosis_cycle(
            ctx,
            db_key,
            now_unix,
            last_ts,
            active_link.0,
            active_link.3,
            active_link.4,
            active_link.5,
        );

        // 4. Opposing side linking against existing progress -> must de-capture accrued_secs to 0 first.
        if alliance_id != 0 && sole_side != alliance_id && accrued_secs > 1e-6 {
            if eff_dt + 1e-6 < accrued_secs {
                accrued_secs = (accrued_secs - eff_dt).max(0.0);
                if !stront_alive {
                    remove_ship_entosis_link(ctx, active_link.0);
                }
                ctx.db.with_mut_snapshot(|snap| {
                    snap.sov_runtime
                        .entosis_capture_progress
                        .insert(target_id, (char_id, alliance_id, 0, accrued_secs, now_unix));
                });
                continue;
            }
            let rem_dt = (eff_dt - accrued_secs).max(0.0);
            ship_id = active_link.0;
            char_id = active_link.1;
            alliance_id = sole_side;
            accrued_secs = rem_dt;
        } else {
            ship_id = active_link.0;
            char_id = active_link.1;
            alliance_id = sole_side;
            accrued_secs += eff_dt;
        }

        if !stront_alive && accrued_secs + 1e-6 < required_secs {
            remove_ship_entosis_link(ctx, ship_id);
            ctx.db.with_mut_snapshot(|snap| {
                if accrued_secs <= 1e-6 {
                    snap.sov_runtime.entosis_capture_progress.remove(&target_id);
                } else {
                    snap.sov_runtime
                        .entosis_capture_progress
                        .insert(target_id, (char_id, alliance_id, 0, accrued_secs, now_unix));
                }
            });
            continue;
        }

        if accrued_secs + 1e-6 < required_secs {
            ctx.db.with_mut_snapshot(|snap| {
                snap.sov_runtime
                    .entosis_capture_progress
                    .insert(target_id, (char_id, alliance_id, ship_id, accrued_secs, now_unix));
            });
            continue;
        }

        remove_ship_entosis_link(ctx, ship_id);

        if is_hub {
            ctx.db.with_mut_snapshot(|snap| {
                snap.sov_runtime.entosis_capture_progress.remove(&target_id);
                if let Some(st) = snap.structures.get_mut(&target_id) {
                    st.state = "HullReinforced".into();
                    st.reinforced_until_unix = now_unix + 48 * 3_600;
                }
                crate::services::sov_warzone_dynamic::init_hub_campaign_scores_in_snap(snap, target_id, sys_id);
            });
            emit_sov_structures_updated(ctx, sys_id, &[target_id]);
            ctx.notify(
                NotificationTarget::SolarSystem(sys_id),
                "OnSovereigntyAudioEvent",
                "solarsystemid",
                vec![eve_proto::PyRep::Integer(target_id), eve_proto::PyRep::Integer(1)],
            );
        } else {
            let _ = sys_handle.remove_ball(target_id).await;
            complete_command_node_capture(ctx, target_id, char_id, alliance_id).await;
        }
    }
}

async fn complete_command_node_capture(
    ctx: &ServerContext,
    target_id: i64,
    char_id: i64,
    alliance_id: i64,
) {
    enum NodeOutcome {
        Respawned(Vec<(i64, u32, [f64; 3])>),
        Won {
            hub_id: i64,
            hub_sys: u32,
            defender_won: bool,
            remaining_nodes: Vec<(i64, u32)>,
        },
    }
    let outcome = ctx.db.with_mut_snapshot(|snap| -> Option<NodeOutcome> {
        snap.sov_runtime.entosis_capture_progress.remove(&target_id);
        let (hub_id, _, _) = snap.sov_runtime.command_nodes.remove(&target_id)?;
        snap.items.remove(&target_id);
        snap.item_positions.remove(&target_id);
        let (hub_sys, hub_owner_corp, hub_pos) = snap
            .structures
            .get(&hub_id)
            .map(|s| (s.solar_system_id, s.owner_corp_id, s.position))?;
        let defender = snap.system_states.get(&hub_sys).and_then(|s| s.sov_alliance_id);
        let capturer_corp = snap
            .characters
            .get(&char_id)
            .map(|c| c.corporation_id)
            .unwrap_or(0);
        let is_defender = (defender.is_some() && defender == Some(alliance_id))
            || (hub_owner_corp > 0 && capturer_corp == hub_owner_corp);
        let def_key = defender.unwrap_or(-hub_owner_corp.max(1));
        let scores = snap.sov_runtime.command_node_scores.entry(hub_id).or_default();
        if scores.is_empty() {
            scores.push((def_key, SOV_COMMAND_NODE_INITIAL_PROGRESS));
        }
        let mut progress = scores[0].1;
        if is_defender {
            progress = (progress + SOV_COMMAND_NODE_SCORE_DELTA).min(1.0);
        } else {
            progress = (progress - SOV_COMMAND_NODE_SCORE_DELTA).max(0.0);
        }
        scores[0].0 = def_key;
        scores[0].1 = progress;
        let def_rounded = (progress.clamp(0.0, 1.0) * 100.0).round() / 100.0;
        let att_rounded = ((1.0 - def_rounded).clamp(0.0, 1.0) * 100.0).round() / 100.0;
        snap.sov_campaign_scores.insert(
            hub_id,
            eve_db::SovCampaignScoreRecord {
                structure_id: hub_id,
                attackers_score: att_rounded,
                defender_score: def_rounded,
            },
        );

        if progress > 1e-6 && progress + 1e-6 < 1.0 {
            let const_systems =
                crate::services::sov_warzone_dynamic::constellation_systems_for_hub(ctx, hub_sys);
            let active_cnt = snap
                .sov_runtime
                .command_nodes
                .values()
                .filter(|&&(hid, _, _)| hid == hub_id)
                .count();
            let need = 4usize.saturating_sub(active_cnt);
            let mut respawned = Vec::with_capacity(need);
            for off in 0..need {
                snap.next_entity_id += 1;
                let node_id = snap.next_entity_id;
                let seed = (node_id.unsigned_abs() as usize).wrapping_add(off);
                let n_sys = const_systems[seed % const_systems.len()];
                let pos = crate::services::space_destiny::space_docking_cloaking::pick_system_celestial_position(
                    ctx,
                    n_sys,
                    ((seed % 20) as u32) + 15,
                )
                .0;
                snap.items.insert(
                    node_id,
                    InventoryItemRecord {
                        item_id: node_id,
                        type_id: TYPE_STRUCTURE_COMMAND_NODE,
                        owner_id: 1,
                        location_id: i64::from(n_sys),
                        quantity: 1,
                        singleton: true,
                        ..Default::default()
                    },
                );
                snap.item_positions.insert(node_id, pos);
                snap.sov_runtime.command_nodes.insert(node_id, (hub_id, n_sys, 0));
                respawned.push((node_id, n_sys, pos));
            }
            return Some(NodeOutcome::Respawned(respawned));
        }

        let defender_won = progress >= 1.0 - 1e-6;
        let remaining_nodes: Vec<(i64, u32)> = snap
            .sov_runtime
            .command_nodes
            .iter()
            .filter(|(_, (hid, _, _))| *hid == hub_id)
            .map(|(&nid, &(_, nsys, _))| (nid, nsys))
            .collect();
        for (nid, _) in &remaining_nodes {
            snap.sov_runtime.command_nodes.remove(nid);
            snap.sov_runtime.entosis_capture_progress.remove(nid);
            snap.items.remove(nid);
            snap.item_positions.remove(nid);
        }
        snap.sov_runtime.command_node_scores.remove(&hub_id);
        snap.sov_campaign_scores.remove(&hub_id);
        if let Some(st) = snap.structures.get_mut(&hub_id) {
            st.state = if defender_won { "Online" } else { "Destroyed" }.into();
            st.reinforced_until_unix = 0;
        }
        if !defender_won {
            snap.sov_runtime.sov_claimed_since_unix.remove(&hub_sys);
            if let Some(sys_st) = snap.system_states.get_mut(&hub_sys) {
                sys_st.sov_alliance_id = None;
                sys_st.strategic_index = 0;
            }
            if let Some(hub_data) = snap.sov_runtime.sov_hubs.remove(&hub_id) {
                for (idx, upg) in hub_data.upgrades.into_iter().enumerate() {
                    if idx % 2 == 0 {
                        snap.next_entity_id += 1;
                        let drop_id = snap.next_entity_id;
                        snap.items.insert(
                            drop_id,
                            InventoryItemRecord {
                                item_id: drop_id,
                                type_id: upg.type_id,
                                owner_id: 1,
                                location_id: i64::from(hub_sys),
                                quantity: 1,
                                singleton: true,
                                ..Default::default()
                            },
                        );
                        snap.item_positions.insert(drop_id, hub_pos);
                    }
                }
                for (fuel_type, qty) in hub_data.fuel {
                    let drop_qty = (qty + 1) / 2;
                    if drop_qty > 0 {
                        snap.next_entity_id += 1;
                        let drop_id = snap.next_entity_id;
                        snap.items.insert(
                            drop_id,
                            InventoryItemRecord {
                                item_id: drop_id,
                                type_id: fuel_type,
                                owner_id: 1,
                                location_id: i64::from(hub_sys),
                                quantity: drop_qty as i32,
                                ..Default::default()
                            },
                        );
                        snap.item_positions.insert(drop_id, hub_pos);
                    }
                }
            }
        }
        Some(NodeOutcome::Won {
            hub_id,
            hub_sys,
            defender_won,
            remaining_nodes,
        })
    });
    match outcome {
        Some(NodeOutcome::Respawned(respawned)) => {
            crate::services::sov_warzone_dynamic::spawn_command_node_balls(ctx, respawned).await;
        }
        Some(NodeOutcome::Won {
            hub_id,
            hub_sys,
            defender_won,
            remaining_nodes,
        }) => {
            for (nid, nsys) in remaining_nodes {
                let _ = ctx.systems.get_or_spawn(nsys).remove_ball(nid).await;
            }
            if !defender_won {
                let _ = ctx.systems.get_or_spawn(hub_sys).remove_ball(hub_id).await;
                crate::services::sov_services::emit_sovereignty_changed(ctx, hub_sys, None, None);
            }
            emit_sov_structures_updated(ctx, hub_sys, &[hub_id]);
        }
        None => {}
    }
}

#[cfg(test)]
#[path = "sov_gameplay_entosis_capture_edges_tests.rs"]
mod sov_gameplay_entosis_capture_edges_tests;
