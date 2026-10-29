//! `sov_gameplay_entosis` — ADM Vulnerability, Live Entosis Capture, Command Nodes, Flex Gates & Tenebrex (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use super::flush_pending_quasar;
use super::sov_gameplay_skyhook::tick_skyhooks;
use crate::router::{NotificationTarget, ServerContext};
use crate::services::quasar_sov_hub::{compute_hub_resources, enforce_hub_power_overload, PendingNotices};
use crate::services::quasar_sov_mercden::{complete_mercenary_activity_in_snap, refresh_activities};
use crate::services::sov_services::{
    calculate_adm, claim_system_sovereignty_async, emit_sov_structures_updated, is_hub_hacked,
    TYPE_PHAROLUX_CYNO_BEACON, TYPE_SOVEREIGNTY_HUB, TYPE_TENEBREX_CYNO_JAMMER as SOV_TENEBREX_TYPE,
};
use eve_db::{CosmicAnomalyRecord, InventoryItemRecord, WorldStateSnapshot};
use eve_destiny::{Ball, MovementMode};
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::sync::{Arc, RwLock};

/// SDE type ID for Structure Command Node (`34832`).
pub const TYPE_STRUCTURE_COMMAND_NODE: u32 = 34_832;
/// SDE type ID for Ansiblex Jump Bridge (`35841`).
pub const TYPE_ANSIBLEX_JUMP_BRIDGE: u32 = 35_841;
/// SDE type ID for Tenebrex Cyno Jammer (`37534`).
pub const TYPE_TENEBREX_CYNO_JAMMER: u32 = SOV_TENEBREX_TYPE;
/// Tenebrex Cyno Jammer spool-up duration after onlining (5 minutes = `300 s`).
pub const TENEBREX_SPOOL_UP_SECS: u64 = 300;

/// SDE type IDs for Equinox Sovereignty Hub Upgrades (`sde_types` groups `4768`, `4772`).
pub const UPGRADE_CYNOSURAL_NAVIGATION: u32 = 81_615;
pub const UPGRADE_CYNOSURAL_SUPPRESSION: u32 = 81_619;
pub const UPGRADE_ADVANCED_LOGISTICS_NETWORK: u32 = 81_621;
pub const UPGRADE_MAJOR_THREAT_DETECTION: u32 = 82_496;
pub const UPGRADE_MINOR_THREAT_DETECTION: u32 = 82_499;
pub const UPGRADE_PROSPECTING_ARRAY: u32 = 82_579;

/// Base post-warmup Entosis capture time on a Sovereignty Hub (`600 s * ADM`).
pub const ENTOSIS_HUB_BASE_CAPTURE_SECS: f64 = 600.0;
/// Base post-warmup Entosis capture time on a Constellation Command Node (`240 s * ADM`).
pub const ENTOSIS_NODE_BASE_CAPTURE_SECS: f64 = 240.0;
/// Maximum Entosis Link operating range (`250,000 m`).
pub const ENTOSIS_MAX_RANGE_M: f64 = 250_000.0;

fn adm_for_system(snap: &WorldStateSnapshot, sys_id: u32) -> f64 {
    snap.system_states
        .get(&sys_id)
        .map(|s| calculate_adm(s.military_index, s.industrial_index, s.strategic_index))
        .unwrap_or(1.0)
        .clamp(1.0, 6.0)
}

/// Computes daily Sovereignty Hub vulnerability duration `18h / ADM` (`3h` at ADM 6.0 up to `18h` at ADM 1.0).
pub fn compute_adm_vulnerability_seconds(adm: f64) -> u64 {
    let clamped = adm.clamp(1.0, 6.0);
    ((18.0 * 3_600.0) / clamped).round() as u64
}

/// Applies ADM-scaled vulnerability window `(start_sec, end_sec)` around `center_hour_utc` when
/// `structureVulnerability.UpdateReinforceTimesFromClient` targets a Sovereignty Hub.
pub fn apply_adm_scaled_vulnerability_window_in_snap(
    snap: &mut WorldStateSnapshot,
    hub_id: i64,
    center_hour_utc: u32,
) -> Option<(u64, u64)> {
    let sys_id = snap.structures.get(&hub_id)?.solar_system_id;
    let adm = adm_for_system(snap, sys_id);
    let dur = compute_adm_vulnerability_seconds(adm);
    let center_sec = u64::from(center_hour_utc % 24) * 3_600;
    let half = dur / 2;
    let start_sec = (center_sec + 86_400 - (half % 86_400)) % 86_400;
    let end_sec = (start_sec + dur) % 86_400;
    snap.sov_runtime
        .adm_vulnerability_windows
        .insert(hub_id, (start_sec, end_sec));
    snap.structure_vulnerability_windows
        .insert(hub_id, (start_sec, end_sec));
    Some((start_sec, end_sec))
}

/// Checks whether a Sovereignty Hub is currently inside its configured vulnerability window,
/// properly handling daily windows that wrap across midnight UTC (`start_sec > end_sec`).
pub fn is_hub_within_vulnerability_window_in_snap(
    snap: &WorldStateSnapshot,
    hub_id: i64,
    now_unix: u64,
) -> bool {
    let Some(&(s_v, e_v)) = snap
        .sov_runtime
        .adm_vulnerability_windows
        .get(&hub_id)
        .or_else(|| snap.structure_vulnerability_windows.get(&hub_id))
    else {
        return true;
    };
    if s_v == 0 && e_v == 0 {
        return true;
    }
    is_second_in_window(now_unix, s_v, e_v)
}

fn is_second_in_window(now_unix: u64, start_v: u64, end_v: u64) -> bool {
    if start_v > 86_400 || end_v > 86_400 {
        return now_unix >= start_v && now_unix <= end_v;
    }
    let sec_of_day = now_unix % 86_400;
    if start_v <= end_v {
        sec_of_day >= start_v && sec_of_day <= end_v
    } else {
        sec_of_day >= start_v || sec_of_day <= end_v
    }
}

/// Validates live `dogmaIM.Activate` for an Entosis Link (`group_id == 1313`) when `target_id` is a
/// Sovereignty Hub or Constellation Command Node. Returns `Ok(true)` if `target_id` is a sov capture
/// target, `Ok(false)` for non-sov targets, or `Err(BridgeRpcError)` on invalid alliance/friendly/window/range.
pub async fn precheck_entosis_activation(
    ctx: &ServerContext,
    target_id: i64,
    ship_id: i64,
    char_id: i64,
) -> Result<bool, BridgeRpcError> {
    let now = ctx.now_unix();
    let info = ctx.db.with_snapshot(|snap| {
        let is_hub = snap
            .structures
            .get(&target_id)
            .is_some_and(|st| st.type_id == TYPE_SOVEREIGNTY_HUB && st.state != "Destroyed");
        let node = snap.sov_runtime.command_nodes.get(&target_id).copied();
        if !is_hub && node.is_none() {
            return Ok(None);
        }
        let ch = snap.characters.get(&char_id).ok_or_else(|| BridgeRpcError {
            code: "CharNotFound".into(),
            message: format!("Character {char_id} not found"),
        })?;
        let attacker_alliance = ch
            .alliance_id
            .or_else(|| snap.corporations.get(&ch.corporation_id).and_then(|c| c.alliance_id))
            .filter(|&a| a > 0)
            .ok_or_else(|| BridgeRpcError {
                code: "MustBeInAlliance".into(),
                message: "Pilot must belong to an alliance to Entosis sovereignty structures".into(),
            })?;
        if is_hub {
            let st = snap.structures.get(&target_id).expect("checked");
            let sys_id = st.solar_system_id;
            let holder = snap.system_states.get(&sys_id).and_then(|s| s.sov_alliance_id);
            if holder == Some(attacker_alliance) {
                return Err(BridgeRpcError {
                    code: "CannotEntosisFriendlyStructure".into(),
                    message: "Cannot activate Entosis Link on your own alliance's Sovereignty Hub".into(),
                });
            }
            if !matches!(st.state.as_str(), "ShieldVulnerable" | "Online") {
                return Err(BridgeRpcError {
                    code: "SovHubAlreadyReinforced".into(),
                    message: format!("Sovereignty Hub {target_id} is currently in state {}", st.state),
                });
            }
            if !is_hub_within_vulnerability_window_in_snap(snap, target_id, now) {
                return Err(BridgeRpcError {
                    code: "SovHubInvulnerableOutsideWindow".into(),
                    message: format!("Sovereignty Hub {target_id} is outside its daily vulnerability window"),
                });
            }
            Ok(Some((sys_id, DVec3::from_array(st.position), attacker_alliance)))
        } else {
            let (_, node_sys, _) = node.expect("checked");
            let pos = snap
                .item_positions
                .get(&target_id)
                .copied()
                .unwrap_or([120_000.0, 0.0, 0.0]);
            Ok(Some((node_sys, DVec3::from_array(pos), attacker_alliance)))
        }
    })?;
    let Some((target_sys, fallback_pos, _)) = info else {
        return Ok(false);
    };

    let sys_handle = ctx.systems.get_or_spawn(target_sys);
    let (_, ship_ball) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| BridgeRpcError {
            code: "ShipNotInSpace".into(),
            message: format!("Ship {ship_id} is not in space in system {target_sys}"),
        })?;
    let target_pos = sys_handle
        .get_ball_snapshot(target_id)
        .await
        .ok()
        .flatten()
        .map(|(_, b)| b.position)
        .unwrap_or(fallback_pos);
    let dist = ship_ball.position.distance(target_pos);
    if dist > ENTOSIS_MAX_RANGE_M {
        return Err(BridgeRpcError {
            code: "TargetOutOfEntosisRange".into(),
            message: format!("Target {target_id} is {dist:.0} m away (max 250,000 m)"),
        });
    }
    Ok(true)
}

/// Registers `target_id` in `entosis_capture_progress` after `handle_entosis_link_activation` starts
/// the warmup cycle (`warmup_done_unix = now + cycle_secs`).
pub fn register_entosis_capture_target(
    ctx: &ServerContext,
    target_id: i64,
    ship_id: i64,
    char_id: i64,
    warmup_done_unix: u64,
) {
    ctx.db.with_mut_snapshot(|snap| {
        let Some(ch) = snap.characters.get(&char_id) else { return };
        let Some(alliance_id) = ch
            .alliance_id
            .or_else(|| snap.corporations.get(&ch.corporation_id).and_then(|c| c.alliance_id))
            .filter(|&a| a > 0)
        else {
            return;
        };
        snap.sov_runtime
            .entosis_capture_progress
            .entry(target_id)
            .or_insert((char_id, alliance_id, ship_id, 0.0, warmup_done_unix));
    });
}

async fn spawn_command_nodes_for_hub(ctx: &ServerContext, hub_id: i64, hub_sys_id: u32) {
    let const_systems: Vec<u32> = if let Some(cs) = ctx.sde.solar_systems.get(&hub_sys_id) {
        let mut v: Vec<u32> = ctx
            .sde
            .solar_systems
            .values()
            .filter(|s| s.constellation_id == cs.constellation_id)
            .map(|s| s.solar_system_id)
            .collect();
        v.sort_unstable();
        if v.is_empty() { vec![hub_sys_id] } else { v }
    } else {
        vec![hub_sys_id]
    };

    let spawned: Vec<(i64, u32, [f64; 3])> = ctx.db.with_mut_snapshot(|snap| {
        let mut out = Vec::new();
        for idx in 0..5usize {
            let n_sys = const_systems[idx % const_systems.len()];
            snap.next_entity_id += 1;
            let node_id = snap.next_entity_id;
            let pos = crate::services::space_destiny::space_docking_cloaking::pick_system_celestial_position(ctx, n_sys, (idx as u32) + 10).0;
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
            out.push((node_id, n_sys, pos));
        }
        snap.sov_runtime.command_node_scores.insert(hub_id, Vec::new());
        out
    });

    for (node_id, n_sys, pos) in spawned {
        let _ = ctx
            .systems
            .get_or_spawn(n_sys)
            .spawn_ball(Ball {
                id: node_id,
                type_id: TYPE_STRUCTURE_COMMAND_NODE,
                position: DVec3::from_array(pos),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 5.0e7,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
    }
}

fn remove_ship_entosis_link(ctx: &ServerContext, ship_id: i64) {
    let db_key = ctx.instance_id as usize;
    let mut st = ctx.tactical_state.write().unwrap_or_else(|e| e.into_inner());
    if let Some(link) = st.entosis_active_links.remove(&(db_key, ship_id)) {
        st.entosis_target_cycles.remove(&(db_key, link.target_id, ship_id));
        ctx.db.with_mut_snapshot(|snap| {
            snap.module_active_states.remove(&link.module_id);
        });
    }
}

async fn tick_entosis_captures(ctx: &ServerContext, now_unix: u64) {
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
    let db_key = ctx.instance_id as usize;

    for (target_id, (char_id, alliance_id, ship_id, mut accrued_secs, last_ts)) in entries {
        let meta = ctx.db.with_snapshot(|snap| {
            if let Some(st) = snap.structures.get(&target_id) {
                if st.type_id == TYPE_SOVEREIGNTY_HUB && st.state != "Destroyed" {
                    let sys = st.solar_system_id;
                    let adm = adm_for_system(snap, sys);
                    return Some((true, sys, DVec3::from_array(st.position), ENTOSIS_HUB_BASE_CAPTURE_SECS * adm));
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
                return Some((false, n_sys, DVec3::from_array(pos), ENTOSIS_NODE_BASE_CAPTURE_SECS * adm));
            }
            None
        });
        let Some((is_hub, sys_id, fallback_pos, required_secs)) = meta else {
            ctx.db.with_mut_snapshot(|snap| {
                snap.sov_runtime.entosis_capture_progress.remove(&target_id);
            });
            continue;
        };

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let ship_opt = sys_handle.get_ball_snapshot(ship_id).await.ok().flatten();
        let target_pos = sys_handle
            .get_ball_snapshot(target_id)
            .await
            .ok()
            .flatten()
            .map(|(_, b)| b.position)
            .unwrap_or(fallback_pos);

        let alive_in_range = ship_opt
            .as_ref()
            .is_some_and(|(_, sb)| sb.position.distance(target_pos) <= ENTOSIS_MAX_RANGE_M);
        if !alive_in_range {
            remove_ship_entosis_link(ctx, ship_id);
            ctx.db.with_mut_snapshot(|snap| {
                snap.sov_runtime.entosis_capture_progress.remove(&target_id);
            });
            continue;
        }

        let (own_link, other_sides) = {
            let st = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner());
            let own = st
                .entosis_active_links
                .get(&(db_key, ship_id))
                .filter(|l| l.target_id == target_id)
                .map(|l| (l.module_id, l.cycle_duration_s, l.expires_unix));
            let others: Vec<i64> = st
                .entosis_active_links
                .iter()
                .filter(|(&(k, sid), l)| k == db_key && sid != ship_id && l.target_id == target_id)
                .map(|(_, l)| l.side_id)
                .collect();
            (own, others)
        };
        let Some((mod_id, cycle_dur_s, mut exp_u)) = own_link else {
            ctx.db.with_mut_snapshot(|snap| {
                snap.sov_runtime.entosis_capture_progress.remove(&target_id);
            });
            continue;
        };

        if now_unix <= last_ts {
            continue;
        }

        if other_sides.iter().any(|&s| s != alliance_id) {
            ctx.db.with_mut_snapshot(|snap| {
                if let Some(e) = snap.sov_runtime.entosis_capture_progress.get_mut(&target_id) {
                    e.4 = now_unix;
                }
            });
            continue;
        }

        let cycle_len = (cycle_dur_s.round() as u64).max(120);
        if now_unix >= exp_u {
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
            } else {
                let capped_dt = exp_u.saturating_sub(last_ts) as f64;
                accrued_secs += capped_dt;
                if accrued_secs + 1e-6 < required_secs {
                    remove_ship_entosis_link(ctx, ship_id);
                    ctx.db.with_mut_snapshot(|snap| {
                        snap.sov_runtime.entosis_capture_progress.remove(&target_id);
                    });
                    continue;
                }
            }
        }

        let dt = now_unix.saturating_sub(last_ts) as f64;
        accrued_secs += dt;
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
            });
            spawn_command_nodes_for_hub(ctx, target_id, sys_id).await;
            emit_sov_structures_updated(ctx, sys_id, vec![PyRep::Integer(target_id)]);
            ctx.notify(
                NotificationTarget::SolarSystem(sys_id),
                "OnSovereigntyAudioEvent",
                "solarsystemid",
                vec![PyRep::Integer(target_id), PyRep::Integer(1)],
            );
        } else {
            let _ = sys_handle.remove_ball(target_id).await;
            let campaign_win = ctx.db.with_mut_snapshot(|snap| {
                snap.sov_runtime.entosis_capture_progress.remove(&target_id);
                let (hub_id, _, _) = snap.sov_runtime.command_nodes.remove(&target_id)?;
                snap.items.remove(&target_id);
                snap.item_positions.remove(&target_id);
                let scores = snap.sov_runtime.command_node_scores.entry(hub_id).or_default();
                let new_score = if let Some(pair) = scores.iter_mut().find(|(a, _)| *a == alliance_id) {
                    pair.1 += 0.25;
                    pair.1
                } else {
                    scores.push((alliance_id, 0.25));
                    0.25
                };
                if new_score + 1e-6 < 1.0 {
                    return None;
                }
                let hub_sys = snap.structures.get(&hub_id).map(|s| s.solar_system_id)?;
                let defender = snap.system_states.get(&hub_sys).and_then(|s| s.sov_alliance_id);
                let attacker_corp = snap
                    .characters
                    .get(&char_id)
                    .map(|c| c.corporation_id)
                    .unwrap_or(0);
                let remaining_nodes: Vec<(i64, u32)> = snap
                    .sov_runtime
                    .command_nodes
                    .iter()
                    .filter(|(_, (hid, _, _))| *hid == hub_id)
                    .map(|(&nid, &(_, nsys, _))| (nid, nsys))
                    .collect();
                for (nid, _) in &remaining_nodes {
                    snap.sov_runtime.command_nodes.remove(nid);
                    snap.items.remove(nid);
                    snap.item_positions.remove(nid);
                }
                snap.sov_runtime.command_node_scores.remove(&hub_id);
                if let Some(st) = snap.structures.get_mut(&hub_id) {
                    st.state = "ShieldVulnerable".into();
                    st.reinforced_until_unix = 0;
                    if defender != Some(alliance_id) && attacker_corp > 0 {
                        st.owner_corp_id = attacker_corp;
                    }
                }
                if defender != Some(alliance_id) {
                    snap.sov_runtime.sov_claimed_since_unix.insert(hub_sys, now_unix);
                    if let Some(sys_st) = snap.system_states.get_mut(&hub_sys) {
                        sys_st.sov_alliance_id = None;
                        sys_st.strategic_index = 0;
                    }
                }
                Some((hub_id, hub_sys, defender, attacker_corp, remaining_nodes))
            });
            if let Some((hub_id, hub_sys, defender, attacker_corp, rem_nodes)) = campaign_win {
                for (nid, nsys) in rem_nodes {
                    let _ = ctx.systems.get_or_spawn(nsys).remove_ball(nid).await;
                }
                if defender != Some(alliance_id) && attacker_corp > 0 {
                    let _ = claim_system_sovereignty_async(ctx, hub_sys, alliance_id, attacker_corp, hub_id).await;
                } else {
                    emit_sov_structures_updated(ctx, hub_sys, vec![PyRep::Integer(hub_id)]);
                }
            }
        }
    }
}

fn strategic_level_for_days(days_held: u64) -> u8 {
    match days_held {
        0..=6 => 0,
        7..=20 => 1,
        21..=34 => 2,
        35..=64 => 3,
        65..=99 => 4,
        _ => 5,
    }
}

/// Credits NPC rat kills or mined ore m3 toward Military / Industrial Development Indices in claimed sov null-sec.
pub fn record_sov_system_activity(
    ctx: &ServerContext,
    solar_system_id: u32,
    npc_kills_delta: u32,
    ore_m3_mined_delta: f64,
) {
    let changed = ctx.db.with_mut_snapshot(|snap| {
        if snap
            .system_states
            .get(&solar_system_id)
            .and_then(|s| s.sov_alliance_id)
            .is_none()
        {
            return None;
        }
        let kills = {
            let k = snap.sov_runtime.sov_npc_kills.entry(solar_system_id).or_insert(0);
            *k = k.saturating_add(u64::from(npc_kills_delta));
            *k
        };
        let mined = {
            let m = snap.sov_runtime.sov_mined_units.entry(solar_system_id).or_insert(0);
            *m = m.saturating_add(ore_m3_mined_delta.max(0.0).round() as u64);
            *m
        };
        // DERIVED (synthetic): Sovereignty.md:291 notes exact NPC kill / mining volume formulas are unpublished by CCP;
        // require >= 5 NPC kills for Military Index 1 (5/15/35/70/120 kills for levels 1..=5) and >= 1,000 m3 mined ore
        // for Industrial Index 1 (1,000 / 5,000 / 15,000 / 35,000 / 75,000 m3 for levels 1..=5).
        let mil: u8 = match kills {
            0..=4 => 0,
            5..=14 => 1,
            15..=34 => 2,
            35..=69 => 3,
            70..=119 => 4,
            _ => 5,
        };
        let ind: u8 = match mined {
            0..=999 => 0,
            1_000..=4_999 => 1,
            5_000..=14_999 => 2,
            15_000..=34_999 => 3,
            35_000..=74_999 => 4,
            _ => 5,
        };
        let st = snap.system_states.entry(solar_system_id).or_default();
        let prev = (st.military_index, st.industrial_index, st.strategic_index);
        st.military_index = st.military_index.max(mil);
        st.industrial_index = st.industrial_index.max(ind);
        let next = (st.military_index, st.industrial_index, st.strategic_index);
        if next != prev {
            Some((next.0, next.1, next.2, calculate_adm(next.0, next.1, next.2)))
        } else {
            None
        }
    });
    if changed.is_some() {
        crate::services::sov_services::emit_dev_index_changed(ctx, solar_system_id);
    }
}

/// Invoked from live weapon combat (`combat_weapons_spool.rs`) whenever an entity ball is destroyed.
pub fn on_space_entity_destroyed(
    ctx: &ServerContext,
    solar_system_id: u32,
    _killer_char_id: Option<i64>,
    target_ball_id: i64,
    target_type_id: u32,
) {
    let now = ctx.now_unix();
    let pending: PendingNotices = Arc::new(RwLock::new(Vec::new()));
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(act_key) = snap.sov_runtime.mto_site_balls.remove(&target_ball_id) {
            if let Some(a) = snap.sov_runtime.mercenary_activities.remove(&act_key) {
                complete_mercenary_activity_in_snap(snap, &pending, &a, a.den_id, a.solar_system_id);
                let hub_ids: Vec<i64> = snap
                    .structures
                    .values()
                    .filter(|s| s.solar_system_id == a.solar_system_id && s.type_id == TYPE_SOVEREIGNTY_HUB)
                    .map(|s| s.structure_id)
                    .collect();
                for hid in hub_ids {
                    let _ = enforce_hub_power_overload(snap, &ctx.sde, hid);
                }
            }
        }
    });
    flush_pending_quasar(ctx, &pending, now);

    let is_npc = ctx
        .sde
        .types
        .get(&target_type_id)
        .is_some_and(|t| t.category_id == 11 || t.category_id == 6);
    if is_npc {
        record_sov_system_activity(ctx, solar_system_id, 1, 0.0);
    }
}

/// Maps Flex Upwell structure type IDs to their required online Sovereignty Hub upgrade.
pub fn required_upgrade_for_flex_structure(structure_type_id: u32) -> Option<u32> {
    match structure_type_id {
        TYPE_ANSIBLEX_JUMP_BRIDGE => Some(UPGRADE_ADVANCED_LOGISTICS_NETWORK),
        TYPE_PHAROLUX_CYNO_BEACON => Some(UPGRADE_CYNOSURAL_NAVIGATION),
        TYPE_TENEBREX_CYNO_JAMMER => Some(UPGRADE_CYNOSURAL_SUPPRESSION),
        _ => None,
    }
}

/// Verifies that the local Sovereignty Hub has the required Flex structure upgrade online and powered.
pub fn require_flex_sov_upgrade_online(
    ctx: &ServerContext,
    solar_system_id: u32,
    structure_type_id: u32,
) -> Result<(), BridgeRpcError> {
    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(1.0);
    if sys_sec > 0.0 {
        // Highsec and lowsec do not have Sovereignty Hubs
        return Ok(());
    }
    let Some(req_upgrade) = required_upgrade_for_flex_structure(structure_type_id) else {
        return Ok(());
    };
    let hub_id = ctx.db.with_snapshot(|snap| {
        snap.structures
            .values()
            .find(|st| {
                st.solar_system_id == solar_system_id
                    && st.type_id == TYPE_SOVEREIGNTY_HUB
                    && st.state != "Destroyed"
            })
            .map(|st| st.structure_id)
            .ok_or_else(|| BridgeRpcError {
                code: "MissingSovereigntyHub".into(),
                message: "A Sovereignty Hub with the required upgrade online is required".into(),
            })
    })?;
    if is_hub_hacked(ctx, hub_id) {
        return Err(BridgeRpcError {
            code: "SovHubHacked".into(),
            message: "Sovereignty Hub upgrades are temporarily disabled by a hostile hack".into(),
        });
    }
    ctx.db.with_snapshot(|snap| {
        let online = snap
            .sov_runtime
            .sov_hubs
            .get(&hub_id)
            .is_some_and(|h| {
                h.upgrades
                    .iter()
                    .any(|u| u.type_id == req_upgrade && u.online && !u.low_power)
            });
        if !online {
            return Err(BridgeRpcError {
                code: "RequiredSovUpgradeOffline".into(),
                message: format!("Required Sovereignty Hub upgrade {req_upgrade} is not online"),
            });
        }
        Ok(())
    })
}

/// Evaluates whether an anchored Tenebrex Cyno Jammer (`37534`) is actively jamming cynos in `solar_system_id`.
pub fn is_tenebrex_jammer_active_in_snap(
    snap: &WorldStateSnapshot,
    solar_system_id: u32,
    now_unix: u64,
) -> bool {
    let hub_in_sys = snap
        .structures
        .values()
        .find(|s| s.solar_system_id == solar_system_id && s.type_id == TYPE_SOVEREIGNTY_HUB && s.state != "Destroyed")
        .map(|s| s.structure_id);
    if let Some(hid) = hub_in_sys {
        if snap.sov_runtime.hacked_hubs.get(&hid).is_some_and(|&u| now_unix < u) {
            return false;
        }
        let upg_ok = snap
            .sov_runtime
            .sov_hubs
            .get(&hid)
            .is_some_and(|hub_cfg| {
                hub_cfg
                    .upgrades
                    .iter()
                    .any(|u| u.type_id == UPGRADE_CYNOSURAL_SUPPRESSION && u.online && !u.low_power)
            });
        if !upg_ok {
            return false;
        }
    }
    snap.structures.values().any(|st| {
        st.solar_system_id == solar_system_id
            && st.type_id == TYPE_TENEBREX_CYNO_JAMMER
            && st.fuel_blocks > 0
            && matches!(st.state.as_str(), "ShieldVulnerable" | "Online")
            && snap
                .sov_runtime
                .tenebrex_spool_ready_unix
                .get(&st.structure_id)
                .is_none_or(|&ready| now_unix >= ready)
    })
}

/// 1 Hz sovereignty gameplay background tick wired into `cluster_tick.rs`.
pub async fn tick_sov_gameplay(ctx: &ServerContext, now_unix: u64) {
    tick_skyhooks(ctx, now_unix).await;

    let pending: PendingNotices = Arc::new(RwLock::new(Vec::new()));
    let (mto_balls, spool_events, strat_events, upg_spawns, upg_despawns) = ctx.db.with_mut_snapshot(|snap| {
        let den_ids: Vec<i64> = snap.sov_runtime.mercenary_dens.keys().copied().collect();
        for did in den_ids {
            refresh_activities(snap, &pending, did, now_unix);
        }
        let hub_ids: Vec<(i64, u32)> = snap
            .sov_runtime
            .sov_hubs
            .keys()
            .filter_map(|&hid| snap.structures.get(&hid).map(|s| (hid, s.solar_system_id)))
            .collect();
        let mut upg_spawns: Vec<(u32, i64, &'static str, Vec<u32>, [f64; 3])> = Vec::new();
        let mut upg_despawns: Vec<(u32, i64)> = Vec::new();
        for (hid, hsys) in hub_ids {
            let _ = compute_hub_resources(snap, &ctx.sde, hid);
            let _ = enforce_hub_power_overload(snap, &ctx.sde, hid);
            let hub_hacked = snap.sov_runtime.hacked_hubs.get(&hid).is_some_and(|&u| now_unix < u);
            let upgrades = snap
                .sov_runtime
                .sov_hubs
                .get(&hid)
                .map(|h| h.upgrades.clone())
                .unwrap_or_default();
            let mut active_sigs = Vec::new();
            for u in &upgrades {
                let Some(t) = ctx.sde.types.get(&u.type_id).filter(|t| t.group_id == 4768) else {
                    continue;
                };
                let (arch, site_name, pos, ores) = if t.name.contains("Threat Detection") {
                    ("Combat", format!("Sovereign {}", t.name), [165_000.0, 25_000.0, 0.0], Vec::new())
                } else if t.name.contains("Prospecting Array") {
                    let ores = if t.name.contains("Tritanium") {
                        vec![1230, 1228]
                    } else if t.name.contains("Isogen") {
                        vec![20, 1226]
                    } else if t.name.contains("Zydrine") {
                        vec![19, 1229]
                    } else if t.name.contains("Megacyte") {
                        vec![22, 1225]
                    } else {
                        vec![1228, 1229]
                    };
                    ("Ore", format!("Sovereign {}", t.name), [160_000.0, 25_000.0, 0.0], ores)
                } else if t.name.contains("Exploration Detector") {
                    ("Data", format!("Sovereign {}", t.name), [170_000.0, 25_000.0, 0.0], Vec::new())
                } else {
                    continue;
                };
                let base_id = 1_940_000_000 + (hid.unsigned_abs() as i64 % 100_000) * 100 + i64::from(u.type_id % 100) * 2;
                if !hub_hacked && u.online && !u.low_power {
                    let sig = format!("SOV-{hid}-{}", u.type_id);
                    active_sigs.push(sig.clone());
                    let list = snap.cosmic_anomalies.entry(hsys).or_default();
                    if !list.iter().any(|a| a.anomaly_id == sig) {
                        list.push(CosmicAnomalyRecord {
                            anomaly_id: sig,
                            solar_system_id: hsys,
                            name: site_name,
                            anomaly_type: arch.into(),
                            position: pos,
                            signal_strength: 1.0,
                            ore_types: ores.clone(),
                            remaining_volume: if arch == "Ore" { 100_000.0 } else { 0.0 },
                            created_unix: now_unix,
                            lifetime_seconds: 86_400,
                            cleared: false,
                        });
                    }
                    upg_spawns.push((hsys, base_id, arch, ores, pos));
                } else {
                    upg_despawns.push((hsys, base_id));
                }
            }
            let prefix = format!("SOV-{hid}-");
            if let Some(list) = snap.cosmic_anomalies.get_mut(&hsys) {
                list.retain(|a| !a.anomaly_id.starts_with(&prefix) || active_sigs.contains(&a.anomaly_id));
            }
        }

        let mut mto_to_spawn = Vec::new();
        for (&bid, act_key) in &snap.sov_runtime.mto_site_balls {
            if let Some(a) = snap.sov_runtime.mercenary_activities.get(act_key) {
                let pos = snap.item_positions.get(&bid).copied().unwrap_or_else(|| {
                    crate::services::space_destiny::space_docking_cloaking::pick_system_celestial_position(ctx, a.solar_system_id, 23).0
                });
                mto_to_spawn.push((a.solar_system_id, bid, pos));
            }
        }

        let mut spools = Vec::new();
        let ready_ids: Vec<(i64, u64)> = snap
            .sov_runtime
            .tenebrex_spool_ready_unix
            .iter()
            .map(|(&k, &v)| (k, v))
            .collect();
        for (sid, ready_at) in ready_ids {
            if now_unix >= ready_at && !snap.sov_runtime.tenebrex_spool_notified.get(&sid).copied().unwrap_or(false) {
                snap.sov_runtime.tenebrex_spool_notified.insert(sid, true);
                if let Some(st) = snap.structures.get(&sid) {
                    if matches!(st.state.as_str(), "ShieldVulnerable" | "Online") && st.fuel_blocks > 0 {
                        spools.push(st.solar_system_id);
                    }
                }
            }
        }

        let mut strat_updates = Vec::new();
        let claimed: Vec<(u32, u64)> = snap
            .sov_runtime
            .sov_claimed_since_unix
            .iter()
            .map(|(&k, &v)| (k, v))
            .collect();
        for (sys_id, since) in claimed {
            let Some(st) = snap.system_states.get_mut(&sys_id) else { continue };
            if st.sov_alliance_id.is_none() {
                continue;
            }
            let days = now_unix.saturating_sub(since) / 86_400;
            let strat = strategic_level_for_days(days);
            if st.strategic_index != strat {
                st.strategic_index = strat;
                let adm = calculate_adm(st.military_index, st.industrial_index, st.strategic_index);
                strat_updates.push((sys_id, st.military_index, st.industrial_index, st.strategic_index, adm));
            }
        }

        (mto_to_spawn, spools, strat_updates, upg_spawns, upg_despawns)
    });

    flush_pending_quasar(ctx, &pending, now_unix);

    for (sys_id, base_id) in upg_despawns {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for off in [1i64, 2] {
            let bid = base_id + off;
            let _ = sys_handle.remove_ball(bid).await;
            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                rt.npc_entities.remove(&bid);
            });
        }
    }
    for (sys_id, base_id, arch, ores, pos) in upg_spawns {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let p0 = DVec3::from_array(pos);
        if arch == "Ore" {
            for (idx, &otid) in ores.iter().take(2).enumerate() {
                let aid = base_id + (idx as i64) + 1;
                if sys_handle.get_ball_snapshot(aid).await.ok().flatten().is_none() {
                    let apos = p0 + DVec3::new((idx as f64) * 500.0, 0.0, 0.0);
                    let uvol = ctx.sde.types.get(&otid).map(|t| t.volume).unwrap_or(0.1).max(0.01);
                    let st = crate::system_actor::AsteroidState {
                        asteroid_id: aid,
                        belt_id: 0,
                        type_id: otid,
                        initial_units: 50_000,
                        remaining_units: 50_000,
                        unit_volume: uvol,
                        depleted: false,
                        respawn_at_tick: None,
                    };
                    let ball = Ball {
                        id: aid,
                        type_id: otid,
                        position: apos,
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 1.0,
                        mass: 80_000.0,
                        radius: 120.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    };
                    let _ = sys_handle.spawn_asteroid(st, ball).await;
                }
            }
        } else if arch == "Combat" {
            for idx in 0..2i64 {
                let nid = base_id + idx + 1;
                if sys_handle.get_ball_snapshot(nid).await.ok().flatten().is_none() {
                    let npos = p0 + DVec3::new((idx as f64) * 1_500.0, 0.0, 0.0);
                    crate::services::fw_gameplay::spawn_battlefield_wave_npc(
                        ctx, sys_id, base_id, nid, 37_453, 500_010, npos,
                    )
                    .await;
                }
            }
        } else if arch == "Data" {
            let cid = base_id + 1;
            if sys_handle.get_ball_snapshot(cid).await.ok().flatten().is_none() {
                let _ = sys_handle
                    .spawn_ball(Ball {
                        id: cid,
                        type_id: 23,
                        position: p0,
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 0.0,
                        mass: 2_500.0,
                        radius: 40.0,
                        speed_fraction: 0.0,
                        mode: MovementMode::Rigid,
                    })
                    .await;
            }
        }
    }

    for (sys_id, bid, pos) in mto_balls {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let needs_spawn = sys_handle.get_ball_snapshot(bid).await.ok().flatten().is_none();
        let needs_vulnerable_cs = sys_handle
            .get_combat_state(bid)
            .await
            .ok()
            .flatten()
            .is_none_or(|cs| cs.shield_resonance == [0.0; 4]);
        if needs_spawn {
            let _ = sys_handle
                .spawn_ball(Ball {
                    id: bid,
                    type_id: 24_690,
                    position: DVec3::from_array(pos),
                    velocity: DVec3::ZERO,
                    max_velocity: 200.0,
                    inertia: 1.0,
                    mass: 1.0e6,
                    radius: 80.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                })
                .await;
        }
        if needs_spawn || needs_vulnerable_cs {
            let mut mto_cs = ctx.build_type_combat_state(24_690);
            mto_cs.shield_hp = 500.0;
            mto_cs.max_shield_hp = 500.0;
            mto_cs.armor_hp = 500.0;
            mto_cs.max_armor_hp = 500.0;
            mto_cs.structure_hp = 500.0;
            mto_cs.max_structure_hp = 500.0;
            mto_cs.shield_resonance = [1.0; 4];
            mto_cs.armor_resonance = [1.0; 4];
            mto_cs.structure_resonance = [1.0; 4];
            mto_cs.signature_radius = 40_000.0;
            let _ = sys_handle.set_combat_state(bid, mto_cs).await;
        }
    }

    for sys_id in spool_events {
        ctx.notify(
            NotificationTarget::SolarSystem(sys_id),
            "OnCynoJammerChanged",
            "solarsystemid",
            vec![PyRep::Integer(sys_id as i64), PyRep::Integer(2)],
        );
    }
    for (sys_id, _, _, _, _) in strat_events {
        crate::services::sov_services::emit_dev_index_changed(ctx, sys_id);
    }

    tick_entosis_captures(ctx, now_unix).await;
}
