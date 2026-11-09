//! `sov_gameplay_entosis` — ADM Vulnerability, Live Entosis Capture, Command Nodes, Flex Gates & Tenebrex (`SO-5`, `Q-8`).

#![forbid(unsafe_code)]

use super::flush_pending_quasar;
use super::sov_gameplay_skyhook::tick_skyhooks;
use crate::router::{NotificationTarget, ServerContext};
use crate::services::quasar_sov_hub::{compute_hub_resources, enforce_hub_power_overload, PendingNotices};
use crate::services::quasar_sov_mercden::{complete_mercenary_activity_in_snap, refresh_activities};
use crate::services::sov_services::{
    calculate_adm, is_hub_hacked,
    TYPE_PHAROLUX_CYNO_BEACON, TYPE_SOVEREIGNTY_HUB, TYPE_TENEBREX_CYNO_JAMMER as SOV_TENEBREX_TYPE,
};
use eve_db::{CosmicAnomalyRecord, WorldStateSnapshot};
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

/// DERIVED (synthetic): EVE Sovereignty Command Node initial campaign progress (`eve-wiki/articles/Sovereignty.md:224-228`).
pub const SOV_COMMAND_NODE_INITIAL_PROGRESS: f64 = 0.60;
/// DERIVED (synthetic — exact server-side Threat Detection anomaly respawn delay is not in `.fsdbinary`;
/// `Sovereignty.md:65` states completed sites respawn after a set delay): 15-minute respawn cooldown.
pub const SOV_COMBAT_ANOMALY_RESPAWN_SECS: u64 = 900;
/// DERIVED (synthetic): EVE Sovereignty Command Node score change per captured node (`eve-wiki/articles/Sovereignty.md:224-228`).
pub const SOV_COMMAND_NODE_SCORE_DELTA: f64 = 0.07;

fn adm_for_system(snap: &WorldStateSnapshot, sys_id: u32) -> f64 {
    snap.system_states
        .get(&sys_id)
        .map(|s| calculate_adm(s.military_index, s.industrial_index, s.strategic_index))
        .unwrap_or(1.0)
        .clamp(1.0, 6.0)
}

#[path = "sov_gameplay_entosis_capture.rs"]
mod sov_gameplay_entosis_capture;
pub use sov_gameplay_entosis_capture::register_entosis_capture_target;
use sov_gameplay_entosis_capture::tick_entosis_captures;

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
    if !snap.sov_runtime.adm_vulnerability_windows.contains_key(&hub_id) && s_v <= 23 && e_v <= 6 {
        let sys_id = snap.structures.get(&hub_id).map(|s| s.solar_system_id).unwrap_or(0);
        let dur = compute_adm_vulnerability_seconds(adm_for_system(snap, sys_id));
        let center_sec = (s_v % 24) * 3_600;
        let half = dur / 2;
        let start_sec = (center_sec + 86_400 - (half % 86_400)) % 86_400;
        let end_sec = (start_sec + dur) % 86_400;
        return is_second_in_window(now_unix, start_sec, end_sec);
    }
    is_second_in_window(now_unix, s_v, e_v)
}

pub(crate) fn expire_hacked_hubs_in_snap(
    snap: &mut WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
    now_unix: u64,
) {
    let expired: Vec<i64> = snap
        .sov_runtime
        .hacked_hubs
        .iter()
        .filter_map(|(&hid, &until)| (now_unix >= until).then_some(hid))
        .collect();
    for hid in expired {
        snap.sov_runtime.hacked_hubs.remove(&hid);
        if let Some(hub) = snap.sov_runtime.sov_hubs.get_mut(&hid) {
            let fuel = hub.fuel.clone();
            for u in &mut hub.upgrades {
                if u.online
                    && crate::services::quasar_sov_hub::upgrade_definition(sde, u.type_id)
                        .is_none_or(|d| {
                            d.fuel_type == 0
                                || d.fuel_per_hour == 0
                                || fuel.get(&d.fuel_type).copied().unwrap_or(0) >= d.fuel_per_hour
                        })
                {
                    u.low_power = false;
                }
            }
        }
        if let Some(st) = snap.structures.get_mut(&hid) {
            if st.state == "ArmorReinforced"
                && (st.reinforced_until_unix == 0 || now_unix >= st.reinforced_until_unix)
            {
                st.state = "ShieldVulnerable".into();
                st.reinforced_until_unix = 0;
            }
        }
    }
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
    let has_expired_hack = ctx.db.with_snapshot(|snap| {
        snap.sov_runtime.hacked_hubs.get(&target_id).is_some_and(|&u| now >= u)
    });
    if has_expired_hack {
        ctx.db.with_mut_snapshot(|snap| expire_hacked_hubs_in_snap(snap, &ctx.sde, now));
    }
    let sov_types = crate::esi::sov_fw_routes::sov_structure_type_ids(&ctx.sde);
    let info = ctx.db.with_snapshot(|snap| {
        let is_hub = snap
            .structures
            .get(&target_id)
            .is_some_and(|st| (st.type_id == TYPE_SOVEREIGNTY_HUB || sov_types.contains(&st.type_id)) && st.state != "Destroyed");
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
                let db_key = crate::services::tactical_modules::db_ptr_key(&ctx.db);
                let has_attacker_link = {
                    let tst = ctx.tactical_state.read().unwrap_or_else(|e| e.into_inner());
                    tst.entosis_active_links
                        .iter()
                        .any(|(&(k, _), l)| k == db_key && l.target_id == target_id && l.side_id != attacker_alliance)
                };
                let has_progress = snap
                    .sov_runtime
                    .entosis_capture_progress
                    .get(&target_id)
                    .is_some_and(|e| e.3 > 1e-6);
                if !has_attacker_link && !has_progress {
                    return Err(BridgeRpcError {
                        code: "CannotEntosisFriendlyStructure".into(),
                        message: "Cannot activate Entosis Link on your own uncontested Sovereignty Hub".into(),
                    });
                }
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
    let now_unix = ctx.now_unix();
    let changed = ctx.db.with_mut_snapshot(|snap| {
        if snap
            .system_states
            .get(&solar_system_id)
            .and_then(|s| s.sov_alliance_id)
            .is_none()
        {
            return None;
        }
        snap.sov_runtime
            .sov_dev_decay_last_unix
            .entry(solar_system_id)
            .or_insert(now_unix);
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
        let mil = crate::services::sov_warzone_dynamic::military_index_for_kills(kills);
        let ind = crate::services::sov_warzone_dynamic::industrial_index_for_mined(mined);
        let st = snap.system_states.entry(solar_system_id).or_default();
        st.solar_system_id = solar_system_id;
        let prev = (st.military_index, st.industrial_index, st.strategic_index);
        st.military_index = mil;
        st.industrial_index = ind;
        let next = (st.military_index, st.industrial_index, st.strategic_index);
        if next != prev {
            crate::services::sov_warzone_dynamic::refresh_system_adm_vulnerability_windows_in_snap(snap, solar_system_id);
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
    killer_char_id: Option<i64>,
    target_ball_id: i64,
    target_type_id: u32,
) {
    on_space_entity_destroyed_ext(
        ctx,
        solar_system_id,
        killer_char_id,
        target_ball_id,
        target_type_id,
        false,
    );
}

/// Extended destruction hook distinguishing pre-removed specialized NPCs from unpiloted hulls.
pub fn on_space_entity_destroyed_ext(
    ctx: &ServerContext,
    solar_system_id: u32,
    _killer_char_id: Option<i64>,
    target_ball_id: i64,
    target_type_id: u32,
    was_tracked_npc: bool,
) {
    let now = ctx.now_unix();
    let pending: PendingNotices = Arc::new(RwLock::new(Vec::new()));
    let mut was_mto_npc = false;
    let mut was_upg_npc = false;
    ctx.db.with_mut_snapshot(|snap| {
        if snap.sov_runtime.sov_upgrade_combat_npcs.remove(&target_ball_id) {
            was_upg_npc = true;
            let cleared_sigs: Vec<String> = snap
                .structures
                .values()
                .filter(|s| s.solar_system_id == solar_system_id && s.type_id == TYPE_SOVEREIGNTY_HUB)
                .flat_map(|s| {
                    let hid = s.structure_id;
                    snap.sov_runtime
                        .sov_hubs
                        .get(&hid)
                        .into_iter()
                        .flat_map(move |h| h.upgrades.iter().map(move |u| (hid, u.type_id)))
                })
                .flat_map(|(hid, tid)| {
                    // Same ID layout as the upgrade-site spawner: `base + s_idx * 10 + 1..=9` per site.
                    let base = 1_940_000_000 + (hid.unsigned_abs() as i64 % 100_000) * 100 + i64::from(tid % 100) * 10;
                    let npcs = &snap.sov_runtime.sov_upgrade_combat_npcs;
                    (0..8i64).filter_map(move |s_idx| {
                        let site = base + s_idx * 10;
                        let matched = (site + 1..=site + 9).contains(&target_ball_id);
                        let any_left = (site + 1..=site + 9).any(|n| npcs.contains(&n));
                        (matched && !any_left).then(|| {
                            if s_idx == 0 { format!("SOV-{hid}-{tid}") } else { format!("SOV-{hid}-{tid}-{s_idx}") }
                        })
                    })
                })
                .collect();
            if let Some(list) = snap.cosmic_anomalies.get_mut(&solar_system_id) {
                for anom in list.iter_mut().filter(|a| cleared_sigs.contains(&a.anomaly_id)) {
                    anom.cleared = true;
                    anom.created_unix = now + SOV_COMBAT_ANOMALY_RESPAWN_SECS;
                }
            }
        }
        if let Some(act_key) = snap.sov_runtime.mto_site_balls.remove(&target_ball_id) {
            was_mto_npc = true;
            if let Some(a) = snap.sov_runtime.mercenary_activities.remove(&act_key) {
                complete_mercenary_activity_in_snap(
                    snap,
                    &pending,
                    &a,
                    a.den_id,
                    a.solar_system_id,
                );
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
    record_rat_kill_military_index(
        ctx,
        solar_system_id,
        target_ball_id,
        target_type_id,
        was_mto_npc || was_tracked_npc || was_upg_npc,
    );
}

/// Credits Sovereignty Military Index (`record_sov_system_activity`) only for genuine NPC entities
/// (`category_id == 11` or tracked in server NPC/site registries), never player-owned/unpiloted `category_id == 6` ships.
pub fn record_rat_kill_military_index(
    ctx: &ServerContext,
    solar_system_id: u32,
    target_ball_id: i64,
    target_type_id: u32,
    was_site_or_tracked_npc: bool,
) {
    if target_type_id == 670 {
        return;
    }
    let category_id = ctx
        .sde
        .types
        .get(&target_type_id)
        .map(|t| t.category_id)
        .unwrap_or(0);
    let is_player_item_or_structure = ctx.db.with_snapshot(|snap| {
        snap.structures.contains_key(&target_ball_id)
            || snap.items.get(&target_ball_id).is_some_and(|it| {
                ctx.sde
                    .types
                    .get(&it.type_id)
                    .is_some_and(|t| t.category_id == 6)
            })
    });
    if is_player_item_or_structure {
        return;
    }
    let in_npc_spawns = ctx
        .npc_spawns
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(&solar_system_id)
        .is_some_and(|mgr| mgr.entities.contains_key(&target_ball_id));
    let in_combat_npc = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
        rt.npc_entities.contains_key(&target_ball_id)
            || rt.destroyed_npc_cache.contains_key(&target_ball_id)
    });
    let in_fw_or_convoy = ctx.living_world.get_convoy(target_ball_id).is_some()
        || ctx.db.with_snapshot(|snap| {
            snap.fw_runtime
                .battlefield_npc_waves
                .values()
                .any(|w| w.contains(&target_ball_id))
        });
    if category_id == 11 || was_site_or_tracked_npc || in_npc_spawns || in_combat_npc || in_fw_or_convoy {
        let now_unix = ctx.now_unix();
        ctx.db.with_mut_snapshot(|snap| snap.record_system_kill(solar_system_id, 0, 1, 0, now_unix));
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
    if sys_sec >= 0.0 || solar_system_id >= 31_000_000 {
        return Err(BridgeRpcError {
            code: "CannotAnchorFlexOutsideSovNull".into(),
            message: format!(
                "FLEX structure type {structure_type_id} can only be anchored in sovereign K-space null-security space (system {solar_system_id})"
            ),
        });
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
    let Some(hid) = snap
        .structures
        .values()
        .find(|s| s.solar_system_id == solar_system_id && s.type_id == TYPE_SOVEREIGNTY_HUB && s.state != "Destroyed")
        .map(|s| s.structure_id)
    else {
        return false;
    };
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
    snap.structures.values().any(|st| {
        st.solar_system_id == solar_system_id
            && st.type_id == TYPE_TENEBREX_CYNO_JAMMER
            && st.fuel_blocks > 0
            && matches!(st.state.as_str(), "ShieldVulnerable" | "Online")
            && snap
                .sov_runtime
                .tenebrex_spool_ready_unix
                .get(&st.structure_id)
                .is_some_and(|&ready| now_unix >= ready)
    })
}

/// 1 Hz sovereignty gameplay background tick wired into `cluster_tick.rs`.
pub async fn tick_sov_gameplay(ctx: &ServerContext, now_unix: u64) {
    tick_skyhooks(ctx, now_unix).await;

    // Pre-check read-only snapshot state so steady-state idle 1 Hz ticks with zero pending
    // sovereignty mutations skip `with_mut_snapshot` (0 SQLite WAL writes), while still
    // advancing Strategic Index day thresholds, Military/Industrial index daily decay,
    // Tenebrex spool-up completion, hacked hub expiry, Command Node spawns, unattended
    // Command Node regeneration, and live Entosis Link captures.
    let has_sov_snapshot_work = ctx.db.with_snapshot(|snap| {
        !snap.sov_runtime.mercenary_dens.is_empty()
            || !snap.sov_runtime.mercenary_den_reinforcement.is_empty()
            || !snap.sov_runtime.mto_site_balls.is_empty()
            || snap
                .sov_runtime
                .hacked_hubs
                .values()
                .any(|&until| now_unix >= until)
            || !snap.sov_runtime.sov_hubs.is_empty()
            || snap.structures.values().any(|s| {
                s.type_id == super::TYPE_MERCENARY_DEN
                    && matches!(s.state.as_str(), "HullReinforced" | "HullVulnerable")
                    && s.reinforced_until_unix > 0
            })
            || snap
                .sov_runtime
                .tenebrex_spool_ready_unix
                .iter()
                .any(|(sid, &ready_at)| {
                    now_unix >= ready_at
                        && !snap
                            .sov_runtime
                            .tenebrex_spool_notified
                            .get(sid)
                            .copied()
                            .unwrap_or(false)
                })
            || snap
                .sov_runtime
                .sov_claimed_since_unix
                .iter()
                .any(|(&sys_id, &since)| {
                    snap.system_states.get(&sys_id).is_some_and(|st| {
                        st.sov_alliance_id.is_some()
                            && st.strategic_index
                                != strategic_level_for_days(now_unix.saturating_sub(since) / 86_400)
                    })
                })
            || snap.system_states.iter().any(|(&sys_id, st)| {
                st.sov_alliance_id.is_some()
                    && match snap.sov_runtime.sov_dev_decay_last_unix.get(&sys_id) {
                        None => true,
                        Some(&last) => now_unix.saturating_sub(last) >= 86_400,
                    }
            })
    });

    let (mto_balls, mto_despawns, spool_events, strat_events, upg_spawns, upg_despawns) =
        if has_sov_snapshot_work {
            let pending: PendingNotices = Arc::new(RwLock::new(Vec::new()));
            let res = ctx.db.with_mut_snapshot(|snap| {
                super::tick_mercenary_den_reinforcements_in_snap(snap, &pending, now_unix);
        let mto_before: std::collections::HashMap<i64, u32> = snap
            .sov_runtime
            .mto_site_balls
            .iter()
            .filter_map(|(&bid, act_key)| {
                let sys = snap
                    .sov_runtime
                    .mercenary_activities
                    .get(act_key)
                    .map(|a| a.solar_system_id)
                    .or_else(|| snap.items.get(&bid).and_then(|it| u32::try_from(it.location_id).ok()));
                sys.map(|s| (bid, s))
            })
            .collect();
        let den_ids: Vec<i64> = snap.sov_runtime.mercenary_dens.keys().copied().collect();
        for did in den_ids {
            refresh_activities(snap, &pending, did, now_unix);
        }
        let mto_despawns: Vec<(u32, i64)> = mto_before
            .into_iter()
            .filter(|(bid, _)| !snap.sov_runtime.mto_site_balls.contains_key(bid))
            .map(|(bid, sys)| (sys, bid))
            .collect();
        expire_hacked_hubs_in_snap(snap, &ctx.sde, now_unix);
        let hub_ids: Vec<(i64, u32)> = snap
            .sov_runtime
            .sov_hubs
            .keys()
            .filter_map(|&hid| snap.structures.get(&hid).map(|s| (hid, s.solar_system_id)))
            .collect();
        let mut upg_spawns: Vec<(u32, i64, &'static str, Vec<u32>, [f64; 3], usize, Vec<i64>)> = Vec::new();
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
            let sec_status = ctx
                .sde
                .solar_systems
                .get(&hsys)
                .map(|s| s.security_status)
                .unwrap_or(-0.5);
            let mut active_sigs = Vec::new();
            for u in &upgrades {
                let Some(t) = ctx.sde.types.get(&u.type_id).filter(|t| t.group_id == 4768) else {
                    continue;
                };
                let specs = crate::services::sov_warzone_dynamic::sov_upgrade_site_specs(&t.name, sec_status);
                if specs.is_empty() {
                    continue;
                }
                let base_id = 1_940_000_000 + (hid.unsigned_abs() as i64 % 100_000) * 100 + i64::from(u.type_id % 100) * 10;
                if !hub_hacked && u.online && !u.low_power {
                    for (s_idx, (arch, site_name, pos, ores, unit_cnt)) in specs.into_iter().enumerate() {
                        let sig = if s_idx == 0 {
                            format!("SOV-{hid}-{}", u.type_id)
                        } else {
                            format!("SOV-{hid}-{}-{s_idx}", u.type_id)
                        };
                        active_sigs.push(sig.clone());
                        let site_base_id = base_id + (s_idx as i64) * 10;
                        let mut seed_combat = false;
                        {
                            let list = snap.cosmic_anomalies.entry(hsys).or_default();
                            if let Some(anom) = list.iter_mut().find(|a| a.anomaly_id == sig) {
                                if arch == "Combat" && anom.cleared && now_unix >= anom.created_unix {
                                    anom.cleared = false;
                                    anom.created_unix = now_unix;
                                    seed_combat = true;
                                }
                            } else {
                                list.push(CosmicAnomalyRecord {
                                    anomaly_id: sig,
                                    solar_system_id: hsys,
                                    name: site_name,
                                    anomaly_type: arch.into(),
                                    position: pos,
                                    signal_strength: 1.0,
                                    ore_types: ores.clone(),
                                    remaining_volume: if arch == "Ore" { (unit_cnt as f64) * 50_000.0 } else { 0.0 },
                                    created_unix: now_unix,
                                    lifetime_seconds: 86_400,
                                    cleared: false,
                                });
                                seed_combat = arch == "Combat";
                            }
                        }
                        if seed_combat {
                            for off in 1..=(unit_cnt as i64) {
                                snap.sov_runtime.sov_upgrade_combat_npcs.insert(site_base_id + off);
                            }
                        }
                        let combat_nids: Vec<i64> = if arch == "Combat" {
                            (1..=(unit_cnt as i64))
                                .map(|off| site_base_id + off)
                                .filter(|nid| snap.sov_runtime.sov_upgrade_combat_npcs.contains(nid))
                                .collect()
                        } else {
                            Vec::new()
                        };
                        upg_spawns.push((hsys, site_base_id, arch, ores, pos, unit_cnt, combat_nids));
                    }
                } else {
                    for off in 0i64..40 {
                        snap.sov_runtime.sov_upgrade_combat_npcs.remove(&(base_id + off));
                    }
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

        let decayed_sys = crate::services::sov_warzone_dynamic::decay_sov_dev_indices_in_snap(snap, now_unix);
        for dsys in decayed_sys {
            if let Some(st) = snap.system_states.get(&dsys) {
                let adm = calculate_adm(st.military_index, st.industrial_index, st.strategic_index);
                strat_updates.push((dsys, st.military_index, st.industrial_index, st.strategic_index, adm));
            }
        }

        for &(usys, _, _, _, _) in &strat_updates {
            crate::services::sov_warzone_dynamic::refresh_system_adm_vulnerability_windows_in_snap(snap, usys);
        }

        (mto_to_spawn, mto_despawns, spools, strat_updates, upg_spawns, upg_despawns)
    });
            flush_pending_quasar(ctx, &pending, now_unix);
            res
        } else {
            (
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        };

    // Spawn Constellation Command Nodes for any reinforced Sovereignty Hub or TCU whose
    // reinforcement timer has elapsed inside its ADM-scaled vulnerability window, even when
    // no Equinox upgrades (`sov_hubs`) are installed.
    let hubs_ready_for_nodes: Vec<(i64, u32)> = ctx.db.with_snapshot(|snap| {
        if !snap
            .structures
            .values()
            .any(|st| st.state == "HullReinforced" && now_unix >= st.reinforced_until_unix)
        {
            return Vec::new();
        }
        let sov_types = crate::esi::sov_fw_routes::sov_structure_type_ids(&ctx.sde);
        snap.structures
            .values()
            .filter(|st| {
                (st.type_id == TYPE_SOVEREIGNTY_HUB || sov_types.contains(&st.type_id))
                    && st.state == "HullReinforced"
                    && now_unix >= st.reinforced_until_unix
                    && is_hub_within_vulnerability_window_in_snap(snap, st.structure_id, now_unix)
                    && !snap
                        .sov_runtime
                        .command_nodes
                        .values()
                        .any(|&(hid, _, _)| hid == st.structure_id)
            })
            .map(|st| (st.structure_id, st.solar_system_id))
            .collect()
    });

    for (hub_id, hsys) in hubs_ready_for_nodes {
        crate::services::sov_warzone_dynamic::spawn_command_nodes_for_hub(ctx, hub_id, hsys).await;
    }

    for (sys_id, bid) in mto_despawns {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let _ = sys_handle.remove_ball(bid).await;
    }

    for (sys_id, base_id) in upg_despawns {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        for off in 0i64..40 {
            let bid = base_id + off;
            let _ = sys_handle.remove_ball(bid).await;
            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                rt.npc_entities.remove(&bid);
            });
        }
    }
    for (sys_id, base_id, arch, ores, pos, unit_cnt, combat_nids) in upg_spawns {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let p0 = DVec3::from_array(pos);
        if arch == "Ore" && !ores.is_empty() {
            for idx in 0..unit_cnt {
                let otid = ores[idx % ores.len()];
                let aid = base_id + (idx as i64) + 1;
                if sys_handle.get_ball_snapshot(aid).await.ok().flatten().is_none() {
                    let apos = p0 + DVec3::new((idx as f64) * 500.0, ((idx % 2) as f64) * 250.0, 0.0);
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
            for nid in combat_nids {
                if sys_handle.get_ball_snapshot(nid).await.ok().flatten().is_none() {
                    let idx = (nid - base_id - 1).max(0);
                    let npos = p0 + DVec3::new((idx as f64) * 1_500.0, ((idx % 2) as f64) * 500.0, 0.0);
                    crate::services::fw_gameplay::spawn_battlefield_wave_npc(
                        ctx, sys_id, base_id, nid, 37_453, 500_010, npos,
                    )
                    .await;
                }
            }
        } else if matches!(arch, "Data" | "Relic") {
            let cid = base_id + 1;
            let ctype = if arch == "Relic" { 28_729 } else { 28_730 };
            if sys_handle.get_ball_snapshot(cid).await.ok().flatten().is_none() {
                let _ = sys_handle
                    .spawn_ball(Ball {
                        id: cid,
                        type_id: ctype,
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
            let mto_cs = ctx.build_type_combat_state(24_690);
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

    crate::services::sov_warzone_dynamic::regenerate_unattended_command_nodes(ctx, now_unix).await;
    tick_entosis_captures(ctx, now_unix).await;
}

#[cfg(test)] #[path = "sov_gameplay_entosis_tests.rs"]
mod sov_gameplay_entosis_tests;
