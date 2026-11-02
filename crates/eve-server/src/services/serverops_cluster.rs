//! `serverops_cluster` — Cluster Time Dilation (`DoSimClockRebase`), worker-node load balancing
//! & fleet-fight node reinforcement, 11:00 UTC daily downtime countdown/shutdown/restart lifecycle,
//! and daily world-state maintenance jobs.

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use super::wallet_services_ext::{save_json_note, sweep_expired_raffles};
use super::wallet_serverops::process_due_automatic_bills;
use eve_db::{MarketHistoryDayRecord, WorldStateSnapshot};
use eve_proto::PyRep;
use eve_sde::SolarSystemId;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Countdown warning thresholds in seconds prior to `downtime_utc_secs_of_day` (`60m`, `15m`, `5m`).
pub const DOWNTIME_WARNING_THRESHOLDS_SECS: [u64; 3] = [3_600, 900, 300];

/// Standard EVE daily downtime maintenance window duration in minutes (`15` minutes).
pub const DOWNTIME_DURATION_MINUTES: i64 = 15;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedDowntimeState {
    pub emitted_warnings_by_day: BTreeMap<u64, Vec<u64>>,
    pub last_shutdown_epoch_day: Option<u64>,
    pub cluster_restart_epoch: u64,
    pub last_maintenance_unix: u64,
    #[serde(default)]
    pub downtime_until_unix: Option<u64>,
}

fn load_downtime_state(snap: &WorldStateSnapshot) -> PersistedDowntimeState {
    snap.service_state_blobs
        .get("cluster_downtime_state")
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default()
}

fn save_downtime_state(snap: &mut WorldStateSnapshot, st: &PersistedDowntimeState) {
    if let Ok(json) = serde_json::to_string(st) {
        snap.service_state_blobs
            .insert("cluster_downtime_state".into(), json);
    }
}

fn unix_to_filetime(unix_secs: u64) -> i64 {
    (unix_secs as i64 + 11_644_473_600).saturating_mul(10_000_000)
}

/// DERIVED: Heuristic constellation-to-worker-node mapping (`workers[(constellation_id as usize) % pool_len]`).
fn default_constellation_worker_node(ctx: &ServerContext, solar_system_id: SolarSystemId, workers: &[u32]) -> u32 {
    let pool_len = workers.len().max(1);
    let key = ctx
        .sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.constellation_id)
        .unwrap_or(solar_system_id);
    workers[(key as usize) % pool_len]
}

fn emit_sim_clock_rebase(ctx: &ServerContext, solar_system_id: SolarSystemId, _old_factor: f64, _new_factor: f64) {
    let now_unix = ctx.now_unix();
    let new_sim_ft = ctx.now_blueos_filetime_ticks();
    let (old_sim_ft, new_sim_ft) = {
        let mut cn = ctx
            .systems
            .cluster_nodes
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let prev_sim_ft = cn
            .sim_clock_anchoring
            .get(&solar_system_id)
            .map(|&(_, ft)| ft)
            .unwrap_or(new_sim_ft);
        let old_ft = prev_sim_ft.min(new_sim_ft);
        cn.sim_clock_anchoring.insert(solar_system_id, (now_unix, new_sim_ft));
        (old_ft, new_sim_ft)
    };
    // parity: notify:DoSimClockRebase
    ctx.notify(
        NotificationTarget::SolarSystem(solar_system_id),
        "DoSimClockRebase",
        "solarsystemid2",
        vec![PyRep::Tuple(vec![
            PyRep::Integer(old_sim_ft),
            PyRep::Integer(new_sim_ft),
        ])],
    );
}

fn persist_system_tidi_note(ctx: &ServerContext, solar_system_id: SolarSystemId, factor: f64) {
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let nid = -(7_500_000_000_000_i64.saturating_add(solar_system_id as i64));
        save_json_note(
            snap,
            nid,
            -75,
            solar_system_id as i64,
            format!("tidi:{solar_system_id}"),
            &factor,
            now,
        );
    });
}

/// Sets an explicit Time Dilation factor (`0.10..=1.0`) for `solar_system_id`, updates the live
/// `SystemActor` simulation tick scaler, and broadcasts `DoSimClockRebase((oldSimTime, newSimTime))`
/// to sessions in that solar system (`feat:serverops.time-dilation`, `notify:DoSimClockRebase`).
pub fn set_system_tidi_factor(ctx: &ServerContext, solar_system_id: u32, requested_factor: f64) -> f64 {
    let factor = if requested_factor.is_finite() {
        requested_factor.clamp(0.10, 1.0)
    } else {
        1.0
    };
    {
        let mut cn = ctx
            .systems
            .cluster_nodes
            .write()
            .unwrap_or_else(|e| e.into_inner());
        cn.manual_tidi_overrides.insert(solar_system_id, factor);
    }
    let handle = ctx.systems.get_or_spawn(solar_system_id);
    let old_factor = handle.set_tidi_factor_raw(factor);
    persist_system_tidi_note(ctx, solar_system_id, factor);
    emit_sim_clock_rebase(ctx, solar_system_id, old_factor, factor);
    factor
}

/// Clears any manual Time Dilation override on `solar_system_id` and recomputes its worker-node load TiDi.
pub fn clear_manual_system_tidi_override(ctx: &ServerContext, solar_system_id: u32) -> f64 {
    {
        let mut cn = ctx
            .systems
            .cluster_nodes
            .write()
            .unwrap_or_else(|e| e.into_inner());
        cn.manual_tidi_overrides.remove(&solar_system_id);
    }
    let updated = recompute_cluster_node_tidi(ctx);
    updated
        .get(&solar_system_id)
        .copied()
        .unwrap_or_else(|| get_system_tidi_factor(ctx, solar_system_id))
}

/// Reads the live Time Dilation factor (`0.10..=1.0`, default `1.0`) for `solar_system_id`.
pub fn get_system_tidi_factor(ctx: &ServerContext, solar_system_id: u32) -> f64 {
    if let Some((_, h)) = ctx
        .systems
        .active_system_handles()
        .into_iter()
        .find(|(id, _)| *id == solar_system_id)
    {
        return h.tidi_factor();
    }
    if let Some(m) = ctx
        .systems
        .cluster_nodes
        .read()
        .ok()
        .and_then(|cn| cn.manual_tidi_overrides.get(&solar_system_id).copied())
    {
        return m.clamp(0.10, 1.0);
    }
    ctx.db.with_snapshot(|snap| {
        let nid = -(7_500_000_000_000_i64.saturating_add(solar_system_id as i64));
        snap.owner_notes
            .get(&nid)
            .and_then(|n| serde_json::from_str::<f64>(&n.note).ok())
            .unwrap_or(1.0)
            .clamp(0.10, 1.0)
    })
}

/// Enables or disables automatic background load-induced Time Dilation (`auto_tidi_enabled`, default `false`).
pub fn set_auto_cluster_tidi_enabled(ctx: &ServerContext, enabled: bool) {
    let mut cn = ctx
        .systems
        .cluster_nodes
        .write()
        .unwrap_or_else(|e| e.into_inner());
    cn.auto_tidi_enabled = enabled;
}

/// Returns whether automatic background load-induced Time Dilation is enabled.
pub fn is_auto_cluster_tidi_enabled(ctx: &ServerContext) -> bool {
    ctx.systems
        .cluster_nodes
        .read()
        .map(|cn| cn.auto_tidi_enabled)
        .unwrap_or(false)
}

/// Configures the per-worker-node active load threshold above which automatic Time Dilation engages.
pub fn set_cluster_node_load_threshold(ctx: &ServerContext, threshold: usize) {
    let mut cn = ctx
        .systems
        .cluster_nodes
        .write()
        .unwrap_or_else(|e| e.into_inner());
    cn.node_load_tidi_threshold = threshold.max(1);
}

/// DERIVED: Computes Time Dilation factor (`0.10..=1.0`) from aggregate worker-node load units.
pub fn compute_tidi_from_node_load(load_units: usize, threshold: usize) -> f64 {
    let cap = threshold.max(1) as f64;
    if (load_units as f64) <= cap {
        1.0
    } else {
        (((cap / (load_units as f64)).clamp(0.10, 1.0)) * 100.0).round() / 100.0
    }
}

/// Resolves the worker node currently hosting `solar_system_id` (`feat:serverops.node-balancing`).
pub fn get_system_worker_node(ctx: &ServerContext, solar_system_id: u32) -> u32 {
    let cn = ctx
        .systems
        .cluster_nodes
        .read()
        .unwrap_or_else(|e| e.into_inner());
    if let Some(&override_node) = cn.system_node_overrides.get(&solar_system_id) {
        return override_node;
    }
    if let Some(persisted) = ctx.db.with_snapshot(|snap| {
        let nid = -(7_600_000_000_000_i64.saturating_add(solar_system_id as i64));
        snap.owner_notes
            .get(&nid)
            .and_then(|n| serde_json::from_str::<u32>(&n.note).ok())
    }) {
        if cn.reinforced_nodes.get(&persisted).copied().unwrap_or(solar_system_id) == solar_system_id {
            return persisted;
        }
    }
    let candidate = default_constellation_worker_node(ctx, solar_system_id, &cn.worker_nodes);
    if cn.reinforced_nodes.get(&candidate).is_some_and(|&owner| owner != solar_system_id) {
        cn.worker_nodes
            .iter()
            .copied()
            .find(|n| !cn.reinforced_nodes.contains_key(n))
            .unwrap_or(1)
    } else {
        candidate
    }
}

/// Computes the aggregate live simulation load across all active solar systems hosted on `node_id`.
pub fn get_worker_node_load(ctx: &ServerContext, node_id: u32) -> usize {
    ctx.systems
        .active_system_handles()
        .into_iter()
        .filter(|(sys_id, _)| get_system_worker_node(ctx, *sys_id) == node_id)
        .map(|(_, h)| h.active_entity_and_cycle_load())
        .sum()
}

/// Recomputes load-induced Time Dilation across all active worker nodes, updating each hosted
/// `SystemHandle`'s tick rate and emitting `DoSimClockRebase` whenever a system's TiDi changes.
pub fn recompute_cluster_node_tidi(ctx: &ServerContext) -> HashMap<u32, f64> {
    let active = ctx.systems.active_system_handles();
    if active.is_empty() {
        return HashMap::new();
    }
    let (threshold, manual_overrides) = {
        let cn = ctx
            .systems
            .cluster_nodes
            .read()
            .unwrap_or_else(|e| e.into_inner());
        (cn.node_load_tidi_threshold.max(1), cn.manual_tidi_overrides.clone())
    };
    let mut node_loads: HashMap<u32, usize> = HashMap::new();
    let mut sys_nodes: HashMap<u32, u32> = HashMap::new();
    for (sys_id, handle) in &active {
        let node_id = get_system_worker_node(ctx, *sys_id);
        sys_nodes.insert(*sys_id, node_id);
        *node_loads.entry(node_id).or_insert(0) += handle.active_entity_and_cycle_load();
    }

    let mut result = HashMap::with_capacity(active.len());
    for (sys_id, handle) in active {
        let target_factor = if let Some(&manual) = manual_overrides.get(&sys_id) {
            manual.clamp(0.10, 1.0)
        } else {
            let load = node_loads.get(&sys_nodes[&sys_id]).copied().unwrap_or(0);
            compute_tidi_from_node_load(load, threshold)
        };
        let old_factor = handle.tidi_factor();
        if (target_factor - old_factor).abs() >= 0.005 {
            handle.set_tidi_factor_raw(target_factor);
            persist_system_tidi_note(ctx, sys_id, target_factor);
            emit_sim_clock_rebase(ctx, sys_id, old_factor, target_factor);
        }
        result.insert(sys_id, target_factor);
    }
    result
}

/// Assigns or reinforces `solar_system_id` onto an isolated cluster node (`feat:serverops.node-balancing`).
/// When `dedicated_node_id` is `Some(node_id)`, marks `node_id` as a reinforced fleet-fight node
/// dedicated exclusively to `solar_system_id`, migrates any sibling systems off `node_id` onto the
/// least-loaded general worker node, registers the node address in `MachoClusterState`, and recomputes TiDi.
pub fn assign_or_reinforce_system_node(
    ctx: &ServerContext,
    solar_system_id: u32,
    dedicated_node_id: Option<u32>,
) -> u32 {
    let Some(node_id) = dedicated_node_id else {
        return get_system_worker_node(ctx, solar_system_id);
    };
    let now = ctx.now_unix();
    let active_ids: Vec<u32> = ctx
        .systems
        .active_system_handles()
        .into_iter()
        .map(|(id, _)| id)
        .collect();

    let mut migrated_siblings: Vec<(u32, u32)> = Vec::new();
    {
        let mut cn = ctx
            .systems
            .cluster_nodes
            .write()
            .unwrap_or_else(|e| e.into_inner());
        cn.reinforced_nodes.retain(|_, &mut sid| sid != solar_system_id);
        cn.reinforced_nodes.insert(node_id, solar_system_id);
        cn.system_node_overrides.insert(solar_system_id, node_id);
        if !cn.worker_nodes.contains(&node_id) && node_id <= 16 {
            cn.worker_nodes.push(node_id);
        }
        let non_reinforced: Vec<u32> = cn
            .worker_nodes
            .iter()
            .copied()
            .filter(|n| *n != node_id && !cn.reinforced_nodes.contains_key(n))
            .collect();
        let fallback_pool = if non_reinforced.is_empty() {
            vec![1_u32]
        } else {
            non_reinforced
        };

        for (idx, sib_id) in active_ids.into_iter().filter(|&id| id != solar_system_id).enumerate() {
            let cur_override = cn.system_node_overrides.get(&sib_id).copied();
            let cur_default = default_constellation_worker_node(ctx, sib_id, &cn.worker_nodes);
            if cur_override == Some(node_id) || (cur_override.is_none() && cur_default == node_id) {
                let target_worker = fallback_pool[idx % fallback_pool.len()];
                cn.system_node_overrides.insert(sib_id, target_worker);
                migrated_siblings.push((sib_id, target_worker));
            }
        }
    }

    if let Ok(mut cluster) = ctx.cluster_state.write() {
        let port = 26_000_u32.saturating_add(node_id);
        cluster
            .node_addresses
            .insert(format!("127.0.0.1:{port}"), node_id as i64);
    }

    ctx.db.with_mut_snapshot(|snap| {
        let nid = -(7_600_000_000_000_i64.saturating_add(solar_system_id as i64));
        save_json_note(
            snap,
            nid,
            -76,
            solar_system_id as i64,
            format!("node:{solar_system_id}"),
            &node_id,
            now,
        );
        for &(sib_id, target_worker) in &migrated_siblings {
            let sib_nid = -(7_600_000_000_000_i64.saturating_add(sib_id as i64));
            save_json_note(
                snap,
                sib_nid,
                -76,
                sib_id as i64,
                format!("node:{sib_id}"),
                &target_worker,
                now,
            );
        }
    });

    let _ = recompute_cluster_node_tidi(ctx);
    node_id
}

/// Allocates a dedicated reinforced cluster node for a heavy fleet-fight solar system, isolating
/// its simulation load from neighboring systems in the same constellation (`feat:serverops.node-balancing`).
pub fn reinforce_solar_system_for_fleet_fight(ctx: &ServerContext, solar_system_id: u32) -> u32 {
    let dedicated_id = {
        let mut cn = ctx
            .systems
            .cluster_nodes
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let id = cn.next_dedicated_node_id.max(100);
        cn.next_dedicated_node_id = id + 1;
        id
    };
    assign_or_reinforce_system_node(ctx, solar_system_id, Some(dedicated_id))
}

/// Rebalances multi-system overloaded general worker nodes by migrating secondary active systems
/// onto the least-loaded non-reinforced worker node (`feat:serverops.node-balancing`).
pub fn rebalance_overloaded_worker_nodes(ctx: &ServerContext) -> usize {
    let active = ctx.systems.active_system_handles();
    if active.len() < 2 {
        return 0;
    }
    let (threshold, workers, reinforced) = {
        let cn = ctx
            .systems
            .cluster_nodes
            .read()
            .unwrap_or_else(|e| e.into_inner());
        (
            cn.node_load_tidi_threshold.max(1),
            cn.worker_nodes.clone(),
            cn.reinforced_nodes.clone(),
        )
    };
    let general_workers: Vec<u32> = workers
        .into_iter()
        .filter(|n| !reinforced.contains_key(n))
        .collect();
    if general_workers.len() < 2 {
        return 0;
    }

    let mut per_node_systems: HashMap<u32, Vec<(u32, usize)>> = HashMap::new();
    let mut node_totals: HashMap<u32, usize> = general_workers.iter().map(|&n| (n, 0)).collect();
    for (sys_id, h) in &active {
        let node = get_system_worker_node(ctx, *sys_id);
        if reinforced.contains_key(&node) {
            continue;
        }
        let load = h.active_entity_and_cycle_load();
        *node_totals.entry(node).or_insert(0) += load;
        per_node_systems.entry(node).or_default().push((*sys_id, load));
    }

    let mut migrations: Vec<(u32, u32)> = Vec::new();
    for (&node, systems) in &mut per_node_systems {
        let total = node_totals.get(&node).copied().unwrap_or(0);
        if total <= threshold || systems.len() < 2 {
            continue;
        }
        systems.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        // Keep the heaviest system on `node` and migrate lighter sibling systems onto least-loaded workers
        for &(sib_sys, sib_load) in systems.iter().skip(1) {
            if sib_load == 0 {
                continue;
            }
            if let Some((&best_node, best_load)) = node_totals
                .iter()
                .filter(|(&n, _)| n != node)
                .min_by_key(|(&n, &l)| (l, n))
            {
                if *best_load + sib_load < total {
                    migrations.push((sib_sys, best_node));
                    *node_totals.entry(best_node).or_insert(0) += sib_load;
                    if let Some(cur) = node_totals.get_mut(&node) {
                        *cur = cur.saturating_sub(sib_load);
                    }
                }
            }
        }
    }

    if !migrations.is_empty() {
        let now = ctx.now_unix();
        {
            let mut cn = ctx
                .systems
                .cluster_nodes
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for &(sys_id, dst_node) in &migrations {
                cn.system_node_overrides.insert(sys_id, dst_node);
            }
        }
        ctx.db.with_mut_snapshot(|snap| {
            for &(sys_id, dst_node) in &migrations {
                let nid = -(7_600_000_000_000_i64.saturating_add(sys_id as i64));
                save_json_note(
                    snap,
                    nid,
                    -76,
                    sys_id as i64,
                    format!("node:{sys_id}"),
                    &dst_node,
                    now,
                );
            }
        });
        let _ = recompute_cluster_node_tidi(ctx);
    }
    migrations.len()
}

fn emit_cluster_shutdown_initiated(ctx: &ServerContext, shutdown_when_unix: u64, duration_minutes: i64) {
    // V24 client `OnClusterShutdownInitiated(explanationLabel, when, duration)` takes 3 arguments.
    ctx.notify(
        NotificationTarget::All,
        "OnClusterShutdownInitiated",
        "clientID",
        vec![
            PyRep::String("UI/Shared/ClusterShutdownInitiated".into()),
            PyRep::Integer(unix_to_filetime(shutdown_when_unix)),
            PyRep::Integer(duration_minutes),
        ],
    );
}

/// Checks countdown thresholds (`60m`, `15m`, `5m` before daily 11:00 UTC downtime) and broadcasts
/// 3-argument `OnClusterShutdownInitiated(explanationLabel, whenFiletime, durationMinutes)` once per
/// threshold per day (`feat:serverops.downtime`).
pub fn step_downtime_countdown_warnings(ctx: &ServerContext, now_unix: u64) -> Vec<u64> {
    let dt_sec = ctx.systems.downtime_utc_secs_of_day();
    let upcoming_dt_unix = crate::system_actor::next_downtime_unix_secs(now_unix, 86_400, dt_sec);
    let secs_until_dt = upcoming_dt_unix.saturating_sub(now_unix);
    if secs_until_dt == 0 || secs_until_dt > DOWNTIME_WARNING_THRESHOLDS_SECS[0] {
        return Vec::new();
    }
    let dt_day_key = upcoming_dt_unix / 86_400;
    let newly_crossed: Vec<u64> = ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_downtime_state(snap);
        let emitted = st.emitted_warnings_by_day.entry(dt_day_key).or_default();
        let mut fired = Vec::new();
        for &threshold in &DOWNTIME_WARNING_THRESHOLDS_SECS {
            if secs_until_dt <= threshold && !emitted.contains(&threshold) {
                emitted.push(threshold);
                fired.push(threshold);
            }
        }
        if !fired.is_empty() {
            // Keep bounded history
            for _ in 0..st.emitted_warnings_by_day.len().saturating_sub(8) {
                if let Some(&oldest) = st.emitted_warnings_by_day.keys().next() {
                    st.emitted_warnings_by_day.remove(&oldest);
                } else {
                    break;
                }
            }
            save_downtime_state(snap, &st);
        }
        fired
    });

    for _ in &newly_crossed {
        emit_cluster_shutdown_initiated(ctx, upcoming_dt_unix, DOWNTIME_DURATION_MINUTES);
    }
    newly_crossed
}

/// Returns true if a downtime countdown warning was emitted for the UTC day containing `now_unix`.
pub fn has_downtime_countdown_armed_for_day(ctx: &ServerContext, now_unix: u64) -> bool {
    let day_key = now_unix / 86_400;
    ctx.db.with_snapshot(|snap| {
        load_downtime_state(snap)
            .emitted_warnings_by_day
            .get(&day_key)
            .is_some_and(|v| !v.is_empty())
    })
}

/// Enters active cluster downtime shutdown: emits `OnClusterShutdownInitiated`, marks
/// `shutdown_in_progress = true` (rejecting non-GM `authentication.Login` with `ClusterShuttingDown`),
/// disconnects active non-GM transport sessions, and releases non-GM online session locks (`feat:serverops.downtime`).
pub fn initiate_cluster_downtime_shutdown(ctx: &ServerContext) -> usize {
    let now = ctx.now_unix();
    emit_cluster_shutdown_initiated(ctx, now, DOWNTIME_DURATION_MINUTES);

    if let Ok(mut cluster) = ctx.cluster_state.write() {
        cluster.shutdown_in_progress = true;
        cluster.clients_terminated = true;
        cluster.shutdown_graceful = Some(true);
        cluster.shutdown_reason = Some("DailyClusterDowntime".into());
        cluster.termination_reason = Some("Daily cluster downtime".into());
    }

    let gm_uids: BTreeSet<u64> = ctx.db.with_snapshot(|snap| {
        snap.accounts
            .values()
            .filter(|a| (a.role & eve_db::service_roles::ROLE_GM_MASK) != 0)
            .map(|a| a.user_id)
            .collect()
    });
    let mut gm_session_ids = BTreeSet::new();
    let mut non_gm_users = Vec::new();
    let mut non_gm_session_ids = BTreeSet::new();
    for (uid, sid) in ctx.db.online_sessions_snapshot() {
        if gm_uids.contains(&uid) {
            gm_session_ids.insert(sid);
        } else {
            non_gm_users.push(uid);
            non_gm_session_ids.insert(sid);
        }
    }

    let closer_sids: Vec<u64> = ctx
        .session_closers
        .read()
        .map(|g| g.keys().copied().collect())
        .unwrap_or_default();

    let mut disconnected = BTreeSet::new();
    for sid in closer_sids {
        if gm_session_ids.contains(&sid) {
            continue;
        }
        ctx.disconnect_session(sid, "ClusterShutdown", "Daily cluster downtime");
        disconnected.insert(sid);
    }
    for sid in &non_gm_session_ids {
        ctx.disconnect_session(*sid, "ClusterShutdown", "Daily cluster downtime");
        ctx.quasar_gateway.unsubscribe_session(*sid);
        disconnected.insert(*sid);
    }
    for uid in non_gm_users {
        let _ = ctx.db.release_online_lock(uid, 0);
    }
    disconnected.len()
}

/// Completes cluster restart following daily downtime maintenance, clearing `shutdown_in_progress`
/// so pilots can authenticate and enter space on the restarted cluster (`feat:serverops.downtime`).
pub fn complete_cluster_downtime_restart(ctx: &ServerContext) -> u64 {
    let now = ctx.now_unix();
    if let Ok(mut cluster) = ctx.cluster_state.write() {
        cluster.shutdown_in_progress = false;
        cluster.clients_terminated = false;
        cluster.shutdown_reason = None;
        cluster.termination_reason = None;
    }
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_downtime_state(snap);
        st.cluster_restart_epoch = st.cluster_restart_epoch.saturating_add(1);
        st.last_maintenance_unix = now;
        st.last_shutdown_epoch_day = Some(now / 86_400);
        st.downtime_until_unix = None;
        let epoch = st.cluster_restart_epoch;
        save_downtime_state(snap, &st);
        epoch
    })
}

fn resolve_tx_region_id(ctx: &ServerContext, snap: &WorldStateSnapshot, location_id: i64) -> Option<u32> {
    if let Some(station) = ctx.sde.stations.get(&location_id) {
        if station.region_id > 0 {
            return Some(station.region_id);
        }
        return ctx.sde.solar_systems.get(&station.solar_system_id).map(|s| s.region_id);
    }
    if let Ok(sys_u32) = u32::try_from(location_id) {
        if (30_000_000..=34_000_000).contains(&sys_u32) {
            return ctx.sde.solar_systems.get(&sys_u32).map(|s| s.region_id);
        }
    }
    if let Some(st) = snap.structures.get(&location_id) {
        return ctx
            .sde
            .solar_systems
            .get(&st.solar_system_id)
            .map(|s| s.region_id);
    }
    for sys in ctx.sde.solar_systems.values() {
        if sys.stations.iter().any(|st| st.station_id == location_id) {
            return Some(sys.region_id);
        }
    }
    None
}

fn rollup_24h_market_history(ctx: &ServerContext, snap: &mut WorldStateSnapshot, now: u64) -> i64 {
    let window_start = now.saturating_sub(86_400);
    let date_str = eve_db::snapshot_ext::unix_secs_to_date_string(now);
    // Group fills in (window_start..=now] by (region_id, type_id)
    #[derive(Default)]
    struct DayAgg {
        earliest_ts: u64,
        open_price: f64,
        latest_ts: u64,
        close_price: f64,
        high: f64,
        low: f64,
        total_value: f64,
        total_vol: i64,
        orders: i64,
    }
    let mut aggs: HashMap<(u32, u32), DayAgg> = HashMap::new();
    for tx in &snap.market_transactions {
        if tx.quantity <= 0 || tx.date_unix <= window_start || tx.date_unix > now {
            continue;
        }
        let Some(region_id) = resolve_tx_region_id(ctx, snap, tx.location_id) else {
            continue;
        };
        let px = tx.price.to_f64_isk();
        let entry = aggs.entry((region_id, tx.type_id)).or_insert_with(|| DayAgg {
            earliest_ts: tx.date_unix,
            open_price: px,
            latest_ts: tx.date_unix,
            close_price: px,
            high: px,
            low: px,
            total_value: 0.0,
            total_vol: 0,
            orders: 0,
        });
        if tx.date_unix < entry.earliest_ts {
            entry.earliest_ts = tx.date_unix;
            entry.open_price = px;
        }
        if tx.date_unix >= entry.latest_ts {
            entry.latest_ts = tx.date_unix;
            entry.close_price = px;
        }
        if px > entry.high {
            entry.high = px;
        }
        if px < entry.low {
            entry.low = px;
        }
        entry.total_value += px * (tx.quantity as f64);
        entry.total_vol = entry.total_vol.saturating_add(tx.quantity);
        entry.orders = entry.orders.saturating_add(1);
    }

    let count = aggs.len() as i64;
    for ((region_id, type_id), agg) in aggs {
        if agg.total_vol <= 0 {
            continue;
        }
        let avg = ((agg.total_value / (agg.total_vol as f64)) * 100.0).round() / 100.0;
        let rec = MarketHistoryDayRecord {
            date: date_str.clone(),
            average: avg,
            highest: agg.high,
            lowest: agg.low,
            order_count: agg.orders,
            volume: agg.total_vol,
            open_price: Some(agg.open_price),
            close_price: Some(agg.close_price),
        };
        let series = snap.market_history.entry((region_id, type_id)).or_default();
        if let Some(existing) = series.iter_mut().find(|d| d.date == date_str) {
            *existing = rec;
        } else {
            series.push(rec);
            series.sort_by(|a, b| a.date.cmp(&b.date));
            if series.len() > 400 {
                let drop_n = series.len() - 400;
                series.drain(0..drop_n);
            }
        }
    }
    count
}

/// Runs all daily maintenance jobs on the world state without changing cluster shutdown flags (`feat:serverops.daily-jobs`).
pub fn execute_daily_maintenance_jobs(ctx: &ServerContext) -> HashMap<String, i64> {
    let now = ctx.now_unix();
    let seconds_in_day = now % 86_400;
    let dt_sec = ctx.systems.downtime_utc_secs_of_day();
    let is_1100_utc_window =
        seconds_in_day >= dt_sec && seconds_in_day < dt_sec.saturating_add(900);

    let expired_raffles = sweep_expired_raffles(ctx) as i64;
    let auto_paid_bills = process_due_automatic_bills(ctx) as i64;
    crate::services::fleet_social::fleet_social_notify::prune_offline_fleets_and_channels(ctx);
    let evacuated_abyssal = if is_1100_utc_window {
        crate::services::pve_special_sites::evacuate_abyssal_pockets_for_downtime(ctx)
    } else {
        0
    };
    let (npc_wrecks_cleaned, mission_npcs_respawned) =
        crate::sp6_lifecycle::cleanup_npc_wrecks_and_reset_missions_on_downtime(ctx);
    let wormhole_sites_despawned =
        super::wormhole_pve_escalations::process_wormhole_sites_on_downtime(ctx);

    // 1. Respawn fully depleted SDE ore belts and depleted ice belts in all active solar system actors
    let mut respawned_belt_asteroids = 0_i64;
    for (_, handle) in ctx.systems.active_system_handles() {
        respawned_belt_asteroids += handle.respawn_depleted_belts_on_downtime() as i64;
    }

    let (
        replenished_asteroids,
        expired_order_list,
        expired_contracts,
        sov_decayed,
        rolled_up_market_pairs,
        recomputed_sci_systems,
        mission_declines_reset,
        ess_rolled_over,
        changed_sov_notifications,
    ) = ctx.db.with_mut_snapshot(|snap| {
        let mut ast_count = respawned_belt_asteroids;
        // Replenish persisted `snap.asteroids` for unloaded systems: fully depleted belts (`Asteroids_and_ore.md:69`) + ice (`group_id == 465`)
        let active_sys_set: BTreeSet<u32> = ctx
            .systems
            .active_system_handles()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        let mut unloaded_belts: HashMap<(u32, i64), Vec<i64>> = HashMap::new();
        for (&aid, arec) in &snap.asteroids {
            if !active_sys_set.contains(&arec.solar_system_id) && arec.belt_id > 0 {
                unloaded_belts
                    .entry((arec.solar_system_id, arec.belt_id))
                    .or_default()
                    .push(aid);
            }
        }
        let mut aids_to_restore: BTreeSet<i64> = BTreeSet::new();
        for (_, aids) in unloaded_belts {
            let all_empty = aids
                .iter()
                .all(|id| snap.asteroids.get(id).is_some_and(|a| a.quantity <= 0));
            if all_empty {
                aids_to_restore.extend(aids);
            }
        }
        for (&aid, arec) in &snap.asteroids {
            if !active_sys_set.contains(&arec.solar_system_id) {
                let is_ice = ctx
                    .sde
                    .types
                    .get(&arec.type_id)
                    .is_some_and(|t| t.group_id == 465);
                if is_ice && arec.quantity < arec.initial_quantity {
                    aids_to_restore.insert(aid);
                }
            }
        }
        for aid in aids_to_restore {
            if let Some(arec) = snap.asteroids.get_mut(&aid) {
                if arec.initial_quantity > 0 && arec.quantity < arec.initial_quantity {
                    arec.quantity = arec.initial_quantity;
                    ast_count += 1;
                }
            }
        }

        // Also replenish in-space ore & ice inventory items (`category_id == 25`, including Ice `group_id == 465`)
        for item in snap.items.values_mut() {
            if (30_000_000..=50_000_000).contains(&item.location_id)
                && ctx
                    .sde
                    .types
                    .get(&item.type_id)
                    .is_some_and(|t| t.category_id == 25)
            {
                let target_qty = item
                    .dynamic_attributes
                    .get(&1805)
                    .copied()
                    .unwrap_or(25_000.0) as i32;
                if item.quantity < target_qty {
                    item.quantity = target_qty;
                    ast_count += 1;
                }
            }
        }

        // 2. 24h Market History OHLCV Roll-Up
        let rolled_up_market_pairs = rollup_24h_market_history(ctx, snap, now);

        // 3. 28d Industry System Cost Indices Recompute
        let sci_rows =
            crate::esi::economy_industry_routes::compute_industry_system_cost_indices(&ctx.sde, snap, now);
        let active_sci_count = sci_rows
            .iter()
            .filter(|v| {
                v["cost_indices"].as_array().is_some_and(|arr| {
                    arr.iter().any(|e| {
                        let ci = e["cost_index"]
                            .as_f64()
                            .unwrap_or(crate::esi::economy_industry_routes::BASELINE_SYSTEM_COST_INDEX);
                        (ci - crate::esi::economy_industry_routes::BASELINE_SYSTEM_COST_INDEX).abs() > 1e-9
                    })
                })
            })
            .count() as i64;
        if let Ok(meta_json) = serde_json::to_string(&serde_json::json!({
            "recomputed_at_unix": now,
            "total_systems": sci_rows.len(),
            "active_systems": active_sci_count,
        })) {
            snap.service_state_blobs
                .insert("industry_system_cost_indices_daily".into(), meta_json);
        }

        // 4. Expired Market Orders & Contracts
        let expired_order_list = super::market_contracts::sweep_expired_orders(snap, now);
        let expired_cids: Vec<i64> = snap
            .contracts
            .iter()
            .filter(|(_, c)| {
                (c.status == "Outstanding" || c.status == "InProgress")
                    && c.expiration_unix > 0
                    && c.expiration_unix <= now
            })
            .map(|(&cid, _)| cid)
            .collect();
        let mut exp_contracts = 0_i64;
        for cid in expired_cids {
            if snap.resolve_expired_contract(cid, now).is_some() {
                exp_contracts += 1;
            }
        }

        // 5. Sovereignty Index Decay across military & industrial indices + ADM vulnerability rollover + stale ESS link cleanup
        let mut sov_hubs_by_sys: HashMap<u32, Vec<(i64, u32)>> = HashMap::new();
        for s in snap.structures.values() {
            if s.type_id == super::sov_services::TYPE_SOVEREIGNTY_HUB && s.state != "Destroyed" {
                if let Some((start_s, end_s)) = snap
                    .sov_runtime
                    .adm_vulnerability_windows
                    .get(&s.structure_id)
                    .or_else(|| snap.structure_vulnerability_windows.get(&s.structure_id))
                    .copied()
                {
                    let dur_s = (end_s + 86_400 - (start_s % 86_400)) % 86_400;
                    let center_hour = (((start_s + dur_s / 2) % 86_400) / 3_600) as u32;
                    sov_hubs_by_sys
                        .entry(s.solar_system_id)
                        .or_default()
                        .push((s.structure_id, center_hour));
                }
            }
        }
        let mut sov_cnt = 0_i64;
        let ess_cnt = 0_i64;
        let mut decayed_hubs: Vec<(i64, u32)> = Vec::new();
        let mut changed_sov: Vec<(u32, HashMap<String, i64>)> = Vec::new();
        for (&sys_id, sov) in snap.system_states.iter_mut() {
            let mut decayed_here = false;
            if sov.sov_alliance_id.is_some() {
                if sov.military_index > 1 {
                    sov.military_index -= 1;
                    decayed_here = true;
                }
                if sov.industrial_index > 1 {
                    sov.industrial_index -= 1;
                    decayed_here = true;
                }
            } else {
                if sov.military_index > 0 {
                    sov.military_index -= 1;
                    decayed_here = true;
                }
                if sov.industrial_index > 0 {
                    sov.industrial_index -= 1;
                    decayed_here = true;
                }
            }
            if decayed_here {
                sov_cnt += 1;
                if let Some(hubs) = sov_hubs_by_sys.get(&sys_id) {
                    decayed_hubs.extend(hubs.iter().copied());
                }
                changed_sov.push((
                    sys_id,
                    HashMap::from([
                        ("militaryIndex".into(), i64::from(sov.military_index)),
                        ("industrialIndex".into(), i64::from(sov.industrial_index)),
                        ("strategicIndex".into(), i64::from(sov.strategic_index)),
                    ]),
                ));
            }
        }

        for (hub_id, center_hour) in decayed_hubs {
            let _ = crate::services::sov_gameplay::sov_gameplay_entosis::apply_adm_scaled_vulnerability_window_in_snap(
                snap,
                hub_id,
                center_hour,
            );
        }

        // 6. Daily Mission Decline Cooldown Reset & LP Log Retention Sweep
        let before_declines = snap.last_mission_decline_times.len();
        snap.last_mission_decline_times
            .retain(|_, &mut ts| now.saturating_sub(ts) < 4 * 3_600);
        let declines_cleared = before_declines.saturating_sub(snap.last_mission_decline_times.len()) as i64;
        for logs in snap.char_lp_logs.values_mut() {
            if logs.len() > 500 {
                let drop_n = logs.len() - 500;
                logs.drain(0..drop_n);
            }
        }

        (
            ast_count,
            expired_order_list,
            exp_contracts,
            sov_cnt,
            rolled_up_market_pairs,
            sci_rows.len() as i64,
            declines_cleared,
            ess_cnt,
            changed_sov,
        )
    });

    let expired_orders = expired_order_list.len() as i64;
    super::market_contracts::emit_expired_order_notifications(ctx, &expired_order_list);
    for (sys_id, _dev_indices) in changed_sov_notifications {
        super::sov_services::emit_dev_index_changed(ctx, sys_id);
    }

    HashMap::from([
        ("is1100UtcWindow".into(), i64::from(is_1100_utc_window)),
        ("replenishedAsteroids".into(), replenished_asteroids),
        ("respawnedBeltAsteroids".into(), respawned_belt_asteroids),
        ("rolledUpMarketHistoryPairs".into(), rolled_up_market_pairs),
        ("recomputedIndustrySystems".into(), recomputed_sci_systems),
        ("expiredMarketOrders".into(), expired_orders),
        ("expiredContracts".into(), expired_contracts),
        ("sovSystemsDecayed".into(), sov_decayed),
        ("missionDeclinesReset".into(), mission_declines_reset),
        ("essBanksRolledOver".into(), ess_rolled_over),
        ("autoPaidBills".into(), auto_paid_bills),
        ("expiredRaffles".into(), expired_raffles),
        ("evacuatedAbyssalPilots".into(), evacuated_abyssal),
        ("npcWrecksCleaned".into(), npc_wrecks_cleaned as i64),
        ("missionNpcsRespawned".into(), mission_npcs_respawned as i64),
        ("wormholeSitesDespawned".into(), wormhole_sites_despawned as i64),
    ])
}

/// Begins the scheduled `11:00–11:15 UTC` daily downtime window (`tick_background_cluster`):
/// disconnects non-GM sessions, runs daily maintenance jobs, and keeps `shutdown_in_progress = true`
/// until the 15-minute maintenance window completes (`feat:serverops.downtime`).
pub fn start_daily_downtime_window(ctx: &ServerContext) -> HashMap<String, i64> {
    let now = ctx.now_unix();
    let disconnected = initiate_cluster_downtime_shutdown(ctx) as i64;
    let mut report = execute_daily_maintenance_jobs(ctx);
    let until = now.saturating_add((DOWNTIME_DURATION_MINUTES as u64) * 60);
    ctx.db.with_mut_snapshot(|snap| {
        let mut st = load_downtime_state(snap);
        st.downtime_until_unix = Some(until);
        save_downtime_state(snap, &st);
    });
    report.insert("disconnectedSessions".into(), disconnected);
    report
}

/// Completes a scheduled daily downtime window once `now_unix` reaches `downtime_until_unix`.
pub fn maybe_complete_scheduled_downtime_restart(ctx: &ServerContext, now_unix: u64) -> Option<u64> {
    if !ctx.is_shutting_down() {
        return None;
    }
    let due = ctx.db.with_snapshot(|snap| {
        load_downtime_state(snap)
            .downtime_until_unix
            .is_some_and(|until| now_unix >= until)
    });
    if due {
        Some(complete_cluster_downtime_restart(ctx))
    } else {
        None
    }
}

/// Executes an atomic 11:00 UTC cluster downtime & restart rollover (`run_daily_downtime_and_jobs`):
/// disconnects non-GM sessions, runs all daily world-state maintenance jobs, and completes cluster
/// restart (`feat:serverops.downtime`, `feat:serverops.daily-jobs`).
pub fn run_daily_downtime_and_jobs(ctx: &ServerContext) -> HashMap<String, i64> {
    let disconnected = initiate_cluster_downtime_shutdown(ctx) as i64;
    let mut report = execute_daily_maintenance_jobs(ctx);
    let restart_epoch = complete_cluster_downtime_restart(ctx) as i64;
    report.insert("disconnectedSessions".into(), disconnected);
    report.insert("clusterRestartEpoch".into(), restart_epoch);
    report
}
