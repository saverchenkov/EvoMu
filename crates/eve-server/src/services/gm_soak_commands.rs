//! GM soak, deadwarp, session inspection, wreck spawning, and character reset commands (`#1088`).

#![forbid(unsafe_code)]

use crate::rng::MechanicRng;
use crate::router::{NotificationTarget, ServerContext};
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};
use crate::services::inventory_fitting::fitting_scanners_superweapons::{
    scanners_superweapons_state, ActiveDbuffEntry,
};
use crate::services::mobile_deployables::{
    read_deployable_state_snap, write_deployable_state_snap, GROUP_MOBILE_CYNO_BEACON,
    GROUP_MOBILE_CYNO_INHIBITOR, GROUP_MOBILE_DEPOT, GROUP_MOBILE_MICRO_JUMP_UNIT,
    GROUP_MOBILE_OBSERVATORY, GROUP_MOBILE_TRACTOR, GROUP_MOBILE_WARP_DISRUPTOR,
};
use crate::services::mobile_deployables_ext::{GROUP_MOBILE_SCAN_INHIBITOR, GROUP_MOBILE_SIPHON};
use eve_db::snapshot_ext::SkillHistoryEntry;
use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};
use eve_destiny::{unix_to_filetime, Ball, DestinyEvent, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::{BTreeSet, HashMap, HashSet};

const MAX_SOAK_LOGI_COUNT: usize = 50;
const MAX_SPAWNWRECK_COUNT: usize = 50;
const SOAK_LOGI_ORBIT_RANGE_M: f64 = 12_000.0;
const SOAK_LOGI_REP_RANGE_M: f64 = 75_000.0;
const SOAK_LOGI_SHIELD_REP_PER_CYCLE: f64 = 450.0;
const SOAK_LOGI_ARMOR_REP_PER_CYCLE: f64 = 450.0;
const SOAK_LOGI_CYCLE_INTERVAL_TICKS: u64 = 2;
const SOAK_LOGI_DBUFF_TTL_SECS: u64 = 30;
const GM_LOGI_SOAK_DBUFF_SOURCE_ID: i64 = -1_088_001;
const DEADWARP_DISTANCE_AU: f64 = 2.0;
const DEADWARP_ARRIVAL_TOLERANCE_M: f64 = 150_000.0;
const DEFAULT_SESSION_LEDGER_LIMIT: usize = 50;
const GROUP_CARGO_CONTAINER: u32 = 12;
const GROUP_WRECK: u32 = 186;
const DEFAULT_WRECK_TYPE_ID: u32 = 26_557;
const SKILL_EVENT_CHAR_CREATION: i64 = 33;

/// V24 SDE Shield/Armor Command Burst dbuffs cycled by `/soak logi` cruisers (`dbuff_id`, `value`).
const SOAK_LOGI_COMMAND_BURST_DBUFFS: [(u32, f64); 4] = [
    (10, 15.0),  // Shield Burst: Shield Harmonizing
    (11, -15.0), // Shield Burst: Active Shielding
    (13, 15.0),  // Armor Burst: Armor Energizing
    (14, -15.0), // Armor Burst: Rapid Repair
];

/// Active `/soak logi` encounter state tracked per solar system in `CombatRuntimeState`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GmLogiSoakState {
    pub solar_system_id: u32,
    pub target_char_id: i64,
    pub target_ship_id: i64,
    pub npc_type_id: u32,
    pub npc_ids: Vec<i64>,
    pub seed: u64,
    pub cycle_count: u64,
    pub cycle_interval_ticks: u64,
    pub last_cycle_tick: u64,
    pub started_unix: u64,
    pub last_cycle_unix: u64,
    pub total_shield_repaired: f64,
    pub total_armor_repaired: f64,
}

/// Phase state machine for multi-tick `/deadwarp` round trips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DeadwarpPhase {
    Outbound,
    AtAnchorDelay,
    Returning,
}

impl DeadwarpPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Outbound => "Outbound",
            Self::AtAnchorDelay => "AtAnchorDelay",
            Self::Returning => "Returning",
        }
    }
}

/// Active `/deadwarp` state tracked per ship ID in `CombatRuntimeState`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GmDeadwarpState {
    pub char_id: i64,
    pub solar_system_id: u32,
    pub ship_id: i64,
    pub origin_pos: [f64; 3],
    pub anchor_pos: [f64; 3],
    pub warp_speed_au_s: f64,
    pub phase: DeadwarpPhase,
    pub started_unix: u64,
    pub reached_anchor_unix: Option<u64>,
}

fn err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
        client_error: None,
    }
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

pub(crate) fn deterministic_logi_soak_seed(
    solar_system_id: u32,
    char_id: i64,
    ship_id: i64,
    count: usize,
) -> u64 {
    let mut s = splitmix64(u64::from(solar_system_id) ^ 0x1088_0001_u64);
    s = splitmix64(s ^ (char_id as u64));
    s = splitmix64(s ^ (ship_id as u64));
    s = splitmix64(s ^ (count as u64));
    s.max(1)
}

pub(crate) fn deterministic_deadwarp_anchor(
    origin: DVec3,
    solar_system_id: u32,
    ship_id: i64,
) -> DVec3 {
    let seed = splitmix64(
        u64::from(solar_system_id)
            ^ (ship_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ 0xDEAD_1088,
    )
    .max(1);
    let rng = MechanicRng::seeded(seed);
    let u0 = rng.try_unit_f64().unwrap_or(0.25);
    let u1 = rng.try_unit_f64().unwrap_or(0.5);
    let theta = u0 * std::f64::consts::TAU;
    let phi = (u1 - 0.5) * 0.4;
    let cos_phi = phi.cos();
    let dir = DVec3::new(theta.cos() * cos_phi, phi.sin(), theta.sin() * cos_phi).normalize_or(DVec3::X);
    origin + dir * (DEADWARP_DISTANCE_AU * crate::services::space_destiny::AU_IN_METERS)
}

pub(crate) fn pick_logi_npc_type_id(sde: &eve_sde::SdeRegistry) -> u32 {
    // Prefer category 11 NPC logistics cruisers, then logistics ships, then any category 11 entity.
    for candidate in [
        29_768_u32, // Independent Osprey
        29_754,     // Independent Augoror
        29_780,     // Independent Exequror
        29_793,     // Independent Scythe
        11_985,     // Basilisk
        11_987,     // Guardian
        11_978,     // Scimitar
        11_989,     // Oneiros
    ] {
        if sde.types.contains_key(&candidate) {
            return candidate;
        }
    }
    if let Some((&tid, _)) = sde.types.iter().find(|(_, t)| t.category_id == 11) {
        return tid;
    }
    if let Some((&tid, _)) = sde.types.iter().find(|(_, t)| t.group_id == 832) {
        return tid;
    }
    29_768
}

fn require_caller_ship_in_space(
    ctx: &ServerContext,
    session: &SessionState,
    cid: i64,
    command_label: &str,
) -> Result<(u32, i64), BridgeRpcError> {
    let info = ctx.db.with_snapshot(|snap| {
        snap.characters.get(&cid).map(|c| {
            (
                c.solar_system_id,
                c.station_id,
                c.structure_id,
                c.active_ship_id,
            )
        })
    });
    match info {
        Some((sys_id, None, None, ship_id)) if sys_id > 0 && ship_id > 0 => Ok((sys_id, ship_id)),
        Some(_) => Err(err(
            "MustBeInSpace",
            format!("{command_label} requires an active ship undocked in space"),
        )),
        None => {
            let sys_id = session.solar_system_id.unwrap_or(0);
            let ship_id = session.ship_id.unwrap_or(0);
            if session.station_id.is_none()
                && session.structure_id.is_none()
                && sys_id > 0
                && ship_id > 0
            {
                Ok((sys_id, ship_id))
            } else {
                Err(err(
                    "MustBeInSpace",
                    format!("{command_label} requires an active ship undocked in space"),
                ))
            }
        }
    }
}

fn is_deployable_group(group_id: u32) -> bool {
    matches!(
        group_id,
        GROUP_MOBILE_DEPOT
            | GROUP_MOBILE_TRACTOR
            | GROUP_MOBILE_MICRO_JUMP_UNIT
            | GROUP_MOBILE_CYNO_BEACON
            | GROUP_MOBILE_CYNO_INHIBITOR
            | GROUP_MOBILE_SCAN_INHIBITOR
            | GROUP_MOBILE_SIPHON
            | GROUP_MOBILE_OBSERVATORY
            | GROUP_MOBILE_WARP_DISRUPTOR
    )
}

pub(crate) fn ring_positions_around(
    center: DVec3,
    count: usize,
    radius_m: f64,
    seed: u64,
) -> Vec<DVec3> {
    let rng = MechanicRng::seeded(seed);
    let base_angle = rng.try_unit_f64().unwrap_or(0.0) * std::f64::consts::TAU;
    let step = std::f64::consts::TAU / (count.max(1) as f64);
    (0..count)
        .map(|i| {
            let angle = base_angle + step * (i as f64);
            let vertical = (rng.try_unit_f64().unwrap_or(0.5) - 0.5) * 1_000.0;
            center + DVec3::new(angle.cos() * radius_m, vertical, angle.sin() * radius_m)
        })
        .collect()
}

fn remove_soak_dbuffs_and_notify(
    ctx: &ServerContext,
    solar_system_id: u32,
    char_id: i64,
    ship_id: i64,
) {
    let remaining_wire: Vec<(i64, f64, i64)> = {
        let mut st = scanners_superweapons_state()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let mut rem = Vec::new();
        if let Some(entries) = st.ship_dbuffs.get_mut(&(ctx.instance_id, ship_id)) {
            entries.retain(|e| e.source_module_id != GM_LOGI_SOAK_DBUFF_SOURCE_ID);
            let now_unix = ctx.now_unix();
            entries.retain(|e| e.expires_at_unix > now_unix);
            for e in entries.iter() {
                rem.push((
                    i64::from(e.dbuff_id),
                    e.value,
                    unix_to_filetime(e.expires_at_unix),
                ));
            }
            if entries.is_empty() {
                st.ship_dbuffs.remove(&(ctx.instance_id, ship_id));
            }
        }
        rem
    };

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    if let Ok(mut bp) = sys_handle.ballpark.write() {
        bp.update_dbuffs(ship_id, remaining_wire);
    }
    ctx.invalidate_ship_fitting(ship_id);
    ctx.push_fit_projection(char_id, ship_id);
    ctx.emit_typed(
        NotificationTarget::Character(char_id),
        crate::router::notify_registry::OnDbuffUpdated { ship_id },
    );
}

async fn stop_logi_soak_in_system(
    ctx: &ServerContext,
    solar_system_id: u32,
) -> Option<GmLogiSoakState> {
    let prev = with_mut_combat_runtime(ctx, |rt| rt.gm_logi_soaks.remove(&solar_system_id))?;
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    for &npc_id in &prev.npc_ids {
        let _ = sys_handle.remove_ball(npc_id).await;
    }
    if let Ok(mut map) = ctx.npc_spawns.write() {
        if let Some(mgr) = map.get_mut(&solar_system_id) {
            for &npc_id in &prev.npc_ids {
                mgr.entities.remove(&npc_id);
            }
        }
    }
    with_mut_combat_runtime(ctx, |rt| {
        for &npc_id in &prev.npc_ids {
            rt.npc_entities.remove(&npc_id);
        }
    });
    remove_soak_dbuffs_and_notify(ctx, solar_system_id, prev.target_char_id, prev.target_ship_id);
    Some(prev)
}

fn logi_soak_status_pyrep(
    cmd: &str,
    solar_system_id: u32,
    state_opt: Option<&GmLogiSoakState>,
    stopped_count: Option<usize>,
) -> PyRep {
    let mut pairs: Vec<(&str, PyRep)> = vec![
        ("command", PyRep::String(cmd.to_string())),
        ("solarSystemID", PyRep::Integer(i64::from(solar_system_id))),
    ];
    if let Some(stopped) = stopped_count {
        pairs.push(("active", PyRep::Bool(false)));
        pairs.push(("stoppedCount", PyRep::Integer(stopped as i64)));
        if let Some(st) = state_opt {
            pairs.push(("targetShipID", PyRep::Integer(st.target_ship_id)));
            pairs.push(("cycleCount", PyRep::Integer(st.cycle_count as i64)));
            pairs.push(("seed", PyRep::Integer(st.seed as i64)));
        }
        return PyRep::keyval(pairs);
    }
    match state_opt {
        Some(st) => {
            pairs.push(("active", PyRep::Bool(true)));
            pairs.push(("targetCharID", PyRep::Integer(st.target_char_id)));
            pairs.push(("targetShipID", PyRep::Integer(st.target_ship_id)));
            pairs.push(("npcTypeID", PyRep::Integer(i64::from(st.npc_type_id))));
            pairs.push(("npcCount", PyRep::Integer(st.npc_ids.len() as i64)));
            pairs.push((
                "npcIDs",
                PyRep::List(st.npc_ids.iter().copied().map(PyRep::Integer).collect()),
            ));
            pairs.push(("seed", PyRep::Integer(st.seed as i64)));
            pairs.push(("cycleCount", PyRep::Integer(st.cycle_count as i64)));
            pairs.push(("startedUnix", PyRep::Integer(st.started_unix as i64)));
            pairs.push(("lastCycleUnix", PyRep::Integer(st.last_cycle_unix as i64)));
            pairs.push(("totalShieldRepaired", PyRep::Real(st.total_shield_repaired)));
            pairs.push(("totalArmorRepaired", PyRep::Real(st.total_armor_repaired)));
        }
        None => {
            pairs.push(("active", PyRep::Bool(false)));
            pairs.push(("npcCount", PyRep::Integer(0)));
            pairs.push(("npcIDs", PyRep::List(Vec::new())));
            pairs.push(("cycleCount", PyRep::Integer(0)));
        }
    }
    PyRep::keyval(pairs)
}

async fn handle_soak_cmd(
    ctx: &ServerContext,
    session: &SessionState,
    cid: i64,
    cmd: &str,
) -> Result<PyRep, BridgeRpcError> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    let usage = "Usage: /soak logi <count|status|stop>";
    let (subcmd, arg) = match parts.as_slice() {
        ["/soak", "logi", arg] => ("logi", *arg),
        ["/soak", "status"] => ("logi", "status"),
        ["/soak", "stop"] => ("logi", "stop"),
        _ => return Err(err("InvalidArguments", usage)),
    };
    if subcmd != "logi" {
        return Err(err("InvalidArguments", usage));
    }

    let caller_sys = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&cid).map(|c| c.solar_system_id))
        .or(session.solar_system_id)
        .unwrap_or(0);

    if arg.eq_ignore_ascii_case("status") {
        let state = with_combat_runtime(ctx, |rt| rt.gm_logi_soaks.get(&caller_sys).cloned());
        return Ok(logi_soak_status_pyrep(cmd, caller_sys, state.as_ref(), None));
    }

    if arg.eq_ignore_ascii_case("stop") {
        let prev = stop_logi_soak_in_system(ctx, caller_sys).await;
        let stopped_n = prev.as_ref().map_or(0, |s| s.npc_ids.len());
        return Ok(logi_soak_status_pyrep(
            cmd,
            caller_sys,
            prev.as_ref(),
            Some(stopped_n),
        ));
    }

    let Ok(count) = arg.parse::<usize>() else {
        return Err(err("InvalidArguments", usage));
    };
    if !(1..=MAX_SOAK_LOGI_COUNT).contains(&count) {
        return Err(err(
            "InvalidArguments",
            format!("Logi soak cruiser count must be between 1 and {MAX_SOAK_LOGI_COUNT}"),
        ));
    }

    let (sys_id, ship_id) = require_caller_ship_in_space(ctx, session, cid, "/soak logi")?;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((bp_tick, ship_ball)) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
    else {
        return Err(err(
            "ShipNotFound",
            format!("Active ship {ship_id} is not present in solar system {sys_id}"),
        ));
    };

    // Replace any existing logi soak in the system cleanly without leaving stale cruisers.
    let _ = stop_logi_soak_in_system(ctx, sys_id).await;

    let npc_type_id = pick_logi_npc_type_id(&ctx.sde);
    let seed = deterministic_logi_soak_seed(sys_id, cid, ship_id, count);
    let positions = ring_positions_around(ship_ball.position, count, SOAK_LOGI_ORBIT_RANGE_M, seed);
    let specs: Vec<(u32, &str, u32, DVec3)> = positions
        .into_iter()
        .map(|pos| (npc_type_id, "GmLogiSoak", 0_u32, pos))
        .collect();

    let npc_ids = crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_batch(
        ctx,
        sys_id,
        &specs,
        |entity| {
            entity.archetype = "GmLogiSoak".to_string();
            entity.faction_id = 500_021;
            entity.damage_vector = Default::default();
            entity.remote_shield_rep = SOAK_LOGI_SHIELD_REP_PER_CYCLE;
            entity.remote_armor_rep = SOAK_LOGI_ARMOR_REP_PER_CYCLE;
            entity.remote_rep_range = SOAK_LOGI_REP_RANGE_M;
            entity.optimal_range = SOAK_LOGI_ORBIT_RANGE_M;
            entity.orbit_range = SOAK_LOGI_ORBIT_RANGE_M;
            entity.bounty_isk = 0.0;
        },
    )
    .await;

    if let Ok(mut bp) = sys_handle.ballpark.write() {
        for &npc_id in &npc_ids {
            let _ = bp.set_mode(
                npc_id,
                MovementMode::Orbit {
                    target_id: ship_id,
                    range: SOAK_LOGI_ORBIT_RANGE_M,
                },
            );
        }
        crate::system_actor::sync_ball_positions(
            &bp,
            &sys_handle.ball_positions,
            &sys_handle.scrambled_balls,
        );
    }

    let now_unix = ctx.now_unix();
    let soak_state = GmLogiSoakState {
        solar_system_id: sys_id,
        target_char_id: cid,
        target_ship_id: ship_id,
        npc_type_id,
        npc_ids,
        seed,
        cycle_count: 0,
        cycle_interval_ticks: SOAK_LOGI_CYCLE_INTERVAL_TICKS,
        last_cycle_tick: bp_tick,
        started_unix: now_unix,
        last_cycle_unix: 0,
        total_shield_repaired: 0.0,
        total_armor_repaired: 0.0,
    };

    with_mut_combat_runtime(ctx, |rt| {
        rt.gm_logi_soaks.insert(sys_id, soak_state.clone());
    });

    Ok(logi_soak_status_pyrep(cmd, sys_id, Some(&soak_state), None))
}

fn deadwarp_status_pyrep(
    cmd: &str,
    ship_id: i64,
    solar_system_id: u32,
    state_opt: Option<&GmDeadwarpState>,
    stopped: Option<bool>,
) -> PyRep {
    let mut pairs: Vec<(&str, PyRep)> = vec![
        ("command", PyRep::String(cmd.to_string())),
        ("shipID", PyRep::Integer(ship_id)),
        ("solarSystemID", PyRep::Integer(i64::from(solar_system_id))),
        ("distanceAU", PyRep::Real(DEADWARP_DISTANCE_AU)),
    ];
    if let Some(did_stop) = stopped {
        pairs.push(("active", PyRep::Bool(false)));
        pairs.push(("stopped", PyRep::Bool(did_stop)));
        pairs.push(("phase", PyRep::String("Stopped".into())));
        return PyRep::keyval(pairs);
    }
    match state_opt {
        Some(st) => {
            pairs.push(("active", PyRep::Bool(true)));
            pairs.push(("phase", PyRep::String(st.phase.as_str().to_string())));
            pairs.push((
                "originPos",
                PyRep::List(st.origin_pos.into_iter().map(PyRep::Real).collect()),
            ));
            pairs.push((
                "anchorPos",
                PyRep::List(st.anchor_pos.into_iter().map(PyRep::Real).collect()),
            ));
            pairs.push(("warpSpeedAuS", PyRep::Real(st.warp_speed_au_s)));
            pairs.push(("startedUnix", PyRep::Integer(st.started_unix as i64)));
            pairs.push((
                "reachedAnchorUnix",
                st.reached_anchor_unix
                    .map(|u| PyRep::Integer(u as i64))
                    .unwrap_or(PyRep::None),
            ));
        }
        None => {
            pairs.push(("active", PyRep::Bool(false)));
            pairs.push(("phase", PyRep::String("Idle".into())));
        }
    }
    PyRep::keyval(pairs)
}

async fn handle_deadwarp_cmd(
    ctx: &ServerContext,
    session: &SessionState,
    cid: i64,
    cmd: &str,
) -> Result<PyRep, BridgeRpcError> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    let usage = "Usage: /deadwarp [status|stop]";
    if parts.len() > 2 {
        return Err(err("InvalidArguments", usage));
    }

    let (caller_sys, caller_ship) = ctx
        .db
        .with_snapshot(|snap| {
            snap.characters
                .get(&cid)
                .map(|c| (c.solar_system_id, c.active_ship_id))
        })
        .unwrap_or((
            session.solar_system_id.unwrap_or(0),
            session.ship_id.unwrap_or(0),
        ));

    if let Some(sub) = parts.get(1).copied() {
        if sub.eq_ignore_ascii_case("status") {
            let st = with_combat_runtime(ctx, |rt| {
                rt.gm_deadwarps
                    .get(&caller_ship)
                    .cloned()
                    .or_else(|| rt.gm_deadwarps.values().find(|d| d.char_id == cid).cloned())
            });
            return Ok(deadwarp_status_pyrep(
                cmd,
                st.as_ref().map_or(caller_ship, |s| s.ship_id),
                st.as_ref().map_or(caller_sys, |s| s.solar_system_id),
                st.as_ref(),
                None,
            ));
        }
        if sub.eq_ignore_ascii_case("stop") {
            let removed = with_mut_combat_runtime(ctx, |rt| {
                rt.gm_deadwarps.remove(&caller_ship).or_else(|| {
                    let key = rt
                        .gm_deadwarps
                        .values()
                        .find(|d| d.char_id == cid)
                        .map(|d| d.ship_id);
                    key.and_then(|k| rt.gm_deadwarps.remove(&k))
                })
            });
            let (target_sys, target_ship) = removed
                .as_ref()
                .map(|s| (s.solar_system_id, s.ship_id))
                .unwrap_or((caller_sys, caller_ship));
            if target_sys > 0 && target_ship > 0 {
                ctx.active_warps
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&target_ship);
                let sys_handle = ctx.systems.get_or_spawn(target_sys);
                if let Ok(mut bp) = sys_handle.ballpark.write() {
                    let _ = bp.set_mode(target_ship, MovementMode::Stop);
                    crate::system_actor::sync_ball_positions(
                        &bp,
                        &sys_handle.ball_positions,
                        &sys_handle.scrambled_balls,
                    );
                };
            }
            return Ok(deadwarp_status_pyrep(
                cmd,
                target_ship,
                target_sys,
                None,
                Some(removed.is_some()),
            ));
        }
        if !sub.eq_ignore_ascii_case("start") {
            return Err(err("InvalidArguments", usage));
        }
    }

    let (sys_id, ship_id) = require_caller_ship_in_space(ctx, session, cid, "/deadwarp")?;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, ship_ball)) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
    else {
        return Err(err(
            "ShipNotFound",
            format!("Active ship {ship_id} is not present in solar system {sys_id}"),
        ));
    };

    crate::services::nav_warp_interdiction::ensure_not_in_active_warp(&ship_ball.mode)?;

    let origin_pos = ship_ball.position;
    let anchor_pos = deterministic_deadwarp_anchor(origin_pos, sys_id, ship_id);
    let warp_speed =
        crate::services::space_destiny::compute_pilot_warp_speed_au_s(ctx, cid, sys_id, ship_ball.type_id)
            .max(1.5);
    let mode = MovementMode::new_warp(origin_pos, anchor_pos, warp_speed);

    if let Err(reason) = sys_handle.set_movement_mode(ship_id, mode).await {
        return Err(err(
            "DestinyCommandFailed",
            format!("/deadwarp outbound warp failed: {reason}"),
        ));
    }

    ctx.active_warps
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(ship_id, cid);

    let state = GmDeadwarpState {
        char_id: cid,
        solar_system_id: sys_id,
        ship_id,
        origin_pos: origin_pos.to_array(),
        anchor_pos: anchor_pos.to_array(),
        warp_speed_au_s: warp_speed,
        phase: DeadwarpPhase::Outbound,
        started_unix: ctx.now_unix(),
        reached_anchor_unix: None,
    };
    with_mut_combat_runtime(ctx, |rt| {
        rt.gm_deadwarps.insert(ship_id, state.clone());
    });

    Ok(deadwarp_status_pyrep(cmd, ship_id, sys_id, Some(&state), None))
}

async fn handle_sysjunkclear_cmd(
    ctx: &ServerContext,
    session: &SessionState,
    cid: i64,
    cmd: &str,
) -> Result<PyRep, BridgeRpcError> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.len() != 1 {
        return Err(err("InvalidArguments", "Usage: /sysjunkclear"));
    }

    let sys_id = ctx
        .db
        .with_snapshot(|snap| snap.characters.get(&cid).map(|c| c.solar_system_id))
        .or(session.solar_system_id)
        .unwrap_or(0);
    if sys_id == 0 {
        return Err(err(
            "SolarSystemNotFound",
            "Caller has no active solar system for /sysjunkclear",
        ));
    }

    let now_unix = ctx.now_unix();
    let sys_id_i64 = i64::from(sys_id);
    let mut wreck_ids: HashSet<i64> = HashSet::new();
    let mut jetcan_ids: HashSet<i64> = HashSet::new();
    let mut expired_dep_ids: HashSet<i64> = HashSet::new();
    let mut warp_bubble_ids: HashSet<i64> = HashSet::new();

    ctx.db.with_snapshot(|snap| {
        let dep_state = read_deployable_state_snap(snap);
        for rec in dep_state.deployables.values() {
            if rec.solar_system_id == sys_id && now_unix >= rec.expires_at_unix {
                expired_dep_ids.insert(rec.item_id);
            }
        }
        for item in snap.items.values() {
            if item.location_id != sys_id_i64 {
                continue;
            }
            let sde_type = ctx.sde.types.get(&item.type_id);
            let group_id = sde_type.map(|t| t.group_id).unwrap_or(0);
            let is_wreck = group_id == GROUP_WRECK
                || sde_type.is_some_and(|t| t.category_id == 2 && t.name.ends_with(" Wreck"));
            if is_wreck {
                wreck_ids.insert(item.item_id);
                continue;
            }
            if item.type_id == 23 || group_id == GROUP_CARGO_CONTAINER {
                jetcan_ids.insert(item.item_id);
                continue;
            }
            if group_id == GROUP_MOBILE_WARP_DISRUPTOR {
                if let Some(start_u) = snap
                    .sp6_lifecycle
                    .space_object_meta
                    .get(&item.item_id)
                    .and_then(|m| m.anchor_start_unix.or(m.created_unix))
                {
                    let decay_secs =
                        crate::sp6_lifecycle::sde_mobile_warp_disruptor_decay_secs(&ctx.sde, item.type_id);
                    if now_unix >= start_u.saturating_add(decay_secs) {
                        expired_dep_ids.insert(item.item_id);
                        warp_bubble_ids.insert(item.item_id);
                    }
                }
            }
        }
    });

    // Also include runtime-tracked wrecks/jetcans and any ballpark-only wreck/jetcan balls.
    with_combat_runtime(ctx, |rt| {
        for entry in rt.container_lifetimes.values() {
            if entry.solar_system_id != sys_id {
                continue;
            }
            if expired_dep_ids.contains(&entry.item_id) {
                continue;
            }
            let group_id = ctx
                .sde
                .types
                .get(&entry.type_id)
                .map(|t| t.group_id)
                .unwrap_or(0);
            if entry.type_id == 23 || group_id == GROUP_CARGO_CONTAINER {
                jetcan_ids.insert(entry.item_id);
            } else if group_id == GROUP_WRECK
                || (!is_deployable_group(group_id)
                    && !ctx
                        .sde
                        .types
                        .get(&entry.type_id)
                        .is_some_and(|t| t.effect_ids.contains(&649) && t.capacity > 0.0))
            {
                wreck_ids.insert(entry.item_id);
            }
        }
    });

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    if let Ok(bp) = sys_handle.ballpark.read() {
        for (&bid, b) in &bp.balls {
            let group_id = bp
                .ball_slim_meta
                .get(&bid)
                .map(|m| m.group_id)
                .or_else(|| bp.ball_group_ids.get(&bid).copied())
                .or_else(|| ctx.sde.types.get(&b.type_id).map(|t| t.group_id))
                .unwrap_or(0);
            if group_id == GROUP_WRECK {
                wreck_ids.insert(bid);
            } else if b.type_id == 23 || group_id == GROUP_CARGO_CONTAINER {
                jetcan_ids.insert(bid);
            }
        }
    }

    for &bid in &warp_bubble_ids {
        let _ = sys_handle.remove_warp_bubble(bid).await;
    }

    let mut all_purged: Vec<i64> = wreck_ids
        .iter()
        .chain(jetcan_ids.iter())
        .chain(expired_dep_ids.iter())
        .copied()
        .collect::<BTreeSet<i64>>()
        .into_iter()
        .collect();
    all_purged.sort_unstable();

    for &iid in &all_purged {
        let _ = sys_handle.remove_ball(iid).await;
    }

    if !all_purged.is_empty() {
        let purged_set: HashSet<i64> = all_purged.iter().copied().collect();
        ctx.db.with_mut_snapshot(|snap| {
            let child_ids: Vec<i64> = snap
                .items
                .values()
                .filter(|it| purged_set.contains(&it.location_id))
                .map(|it| it.item_id)
                .collect();
            for &iid in &all_purged {
                snap.items.remove(&iid);
                snap.item_positions.remove(&iid);
                snap.sp6_lifecycle.space_object_meta.remove(&iid);
                snap.sp6_lifecycle.ship_combat_hp.remove(&iid);
            }
            for cid in child_ids {
                snap.items.remove(&cid);
            }
            if !expired_dep_ids.is_empty() {
                write_deployable_state_snap(snap, |dep_state| {
                    for &did in &expired_dep_ids {
                        dep_state.deployables.remove(&did);
                        dep_state.damaged_items.remove(&did);
                        dep_state.observatory_pings_done.remove(&did);
                        dep_state.cloak_activated_unix.remove(&did);
                        dep_state.siphon_last_cycle_unix.remove(&did);
                    }
                    dep_state
                        .mmju_spools
                        .retain(|_, spool| !expired_dep_ids.contains(&spool.unit_id));
                });
            }
        });
        with_mut_combat_runtime(ctx, |rt| {
            for &iid in &all_purged {
                rt.container_lifetimes.remove(&iid);
            }
        });
    }

    let wrecks_purged = wreck_ids.len() as i64;
    let jetcans_purged = jetcan_ids.len() as i64;
    let expired_deployables_purged = expired_dep_ids.len() as i64;
    let total_purged = all_purged.len() as i64;

    Ok(PyRep::keyval([
        ("command", PyRep::String(cmd.to_string())),
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
        ("wrecksPurged", PyRep::Integer(wrecks_purged)),
        ("jetcansPurged", PyRep::Integer(jetcans_purged)),
        (
            "expiredDeployablesPurged",
            PyRep::Integer(expired_deployables_purged),
        ),
        ("totalPurged", PyRep::Integer(total_purged)),
        (
            "purgedItemIDs",
            PyRep::List(all_purged.into_iter().map(PyRep::Integer).collect()),
        ),
    ]))
}

fn opt_i64_rep(v: Option<i64>) -> PyRep {
    v.map(PyRep::Integer).unwrap_or(PyRep::None)
}

fn opt_u32_rep(v: Option<u32>) -> PyRep {
    v.map(|x| PyRep::Integer(i64::from(x))).unwrap_or(PyRep::None)
}

fn ledger_entry_to_pyrep(e: &crate::flight_recorder::OutboundLedgerEntry) -> PyRep {
    PyRep::keyval([
        ("timestampMillis", PyRep::Integer(e.timestamp_millis as i64)),
        ("disposition", PyRep::String(e.disposition.to_string())),
        ("packetKind", PyRep::String(e.packet_kind.clone())),
        ("byteSize", PyRep::Integer(e.byte_size as i64)),
        (
            "stamp",
            e.stamp.map(|s| PyRep::Integer(i64::from(s))).unwrap_or(PyRep::None),
        ),
        ("reason", PyRep::String(e.reason.clone())),
        (
            "elapsedMicros",
            e.elapsed_since_request_micros
                .map(|us| PyRep::Integer(us as i64))
                .unwrap_or(PyRep::None),
        ),
        ("bootstrap", PyRep::Bool(e.bootstrap)),
    ])
}

fn destiny_stamp_to_pyrep(s: &crate::flight_recorder::DestinyStampRecord) -> PyRep {
    PyRep::keyval([
        ("timestampMillis", PyRep::Integer(s.timestamp_millis as i64)),
        ("sessionID", PyRep::Integer(s.session_id as i64)),
        ("stamp", PyRep::Integer(i64::from(s.stamp))),
        ("action", PyRep::String(s.action.clone())),
        ("entityID", s.entity_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
        (
            "clientCurrentTick",
            s.client_current_tick
                .map(|t| PyRep::Integer(t as i64))
                .unwrap_or(PyRep::None),
        ),
    ])
}

fn handle_session_dump(
    ctx: &ServerContext,
    session: &SessionState,
    cid: i64,
    cmd: &str,
) -> PyRep {
    let sid = session.session_id;
    let now_unix = ctx.now_unix();
    let now_millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_else(|_| now_unix.saturating_mul(1_000));

    let session_dict = PyRep::keyval([
        ("sessionID", PyRep::Integer(sid as i64)),
        (
            "userid",
            session
                .user_id
                .map(|u| PyRep::Integer(u as i64))
                .unwrap_or(PyRep::None),
        ),
        ("charid", opt_i64_rep(session.character_id)),
        ("corpid", opt_i64_rep(session.corporation_id)),
        ("allianceid", opt_i64_rep(session.alliance_id)),
        ("stationid", opt_i64_rep(session.station_id)),
        ("structureid", opt_i64_rep(session.structure_id)),
        ("solarsystemid", opt_u32_rep(session.solar_system_id)),
        ("solarsystemid2", opt_u32_rep(session.solar_system_id)),
        ("constellationid", opt_u32_rep(session.constellation_id)),
        ("regionid", opt_u32_rep(session.region_id)),
        ("shipid", opt_i64_rep(session.ship_id)),
        ("raceid", opt_u32_rep(session.race_id)),
        ("corprole", PyRep::Integer(session.corp_role as i64)),
        (
            "corpAccountKey",
            PyRep::Integer(i64::from(session.corp_account_key)),
        ),
        ("fleetid", opt_i64_rep(session.fleet_id)),
        ("warfactionid", opt_i64_rep(session.war_faction_id)),
        ("role", PyRep::Integer(session.role as i64)),
        ("languageID", PyRep::String(session.language_id.clone())),
        ("address", PyRep::String(session.address.clone())),
        ("userType", PyRep::Integer(session.user_type)),
        ("maxSessionTime", opt_i64_rep(session.max_session_time)),
    ]);

    let wire_snap = ctx.flight_recorder.get_wire_state(sid);
    let active_sys = wire_snap
        .as_ref()
        .and_then(|w| w.solar_system_id)
        .or(session.solar_system_id);
    let in_space = wire_snap.as_ref().map(|w| w.in_space).unwrap_or_else(|| {
        session.station_id.is_none()
            && session.structure_id.is_none()
            && session.solar_system_id.is_some()
    });
    let (is_observer_registered, grid_ball_count, current_tick) =
        if let (true, Some(sys_id)) = (in_space, active_sys) {
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let ball_summary = if let Ok(bp) = sys_handle.ballpark.read() {
                let obs_ship = session.ship_id.unwrap_or(0);
                let registered = obs_ship > 0
                    && bp
                        .registered_observers
                        .as_ref()
                        .is_some_and(|set| set.contains(&obs_ship));
                let mut visible: HashSet<i64> = bp.static_ball_ids.iter().copied().collect();
                if let Some(gid) = (obs_ship > 0).then(|| bp.ball_grid_ids.get(&obs_ship)).flatten() {
                    if let Some(bubble) = bp.grids.get(gid) {
                        visible.extend(bubble.members.iter().copied());
                    }
                }
                (registered, visible.len(), bp.current_tick)
            } else {
                (false, 0, 0)
            };
            ball_summary
        } else {
            (false, 0, 0)
        };

    let materialized = wire_snap
        .as_ref()
        .map(|w| w.materialized)
        .unwrap_or(is_observer_registered);
    let materialized_set_size = wire_snap
        .as_ref()
        .map(|w| w.materialized_set_size)
        .unwrap_or_else(|| if materialized { grid_ball_count } else { 0 });

    let timer_opt = crate::services::space_destiny::get_session_change_timer(ctx, cid)
        .filter(|t| now_unix < t.expires_unix);
    let pending_session_change_rep = match &timer_opt {
        Some(t) => PyRep::keyval([
            ("active", PyRep::Bool(true)),
            ("expiresUnix", PyRep::Integer(t.expires_unix as i64)),
            (
                "remainingSeconds",
                PyRep::Integer(t.expires_unix.saturating_sub(now_unix) as i64),
            ),
            ("systemID", PyRep::Integer(i64::from(t.system_id))),
            ("startedTick", PyRep::Integer(t.started_tick as i64)),
            ("reason", PyRep::String(t.reason.clone())),
            (
                "locationChangeInvulUntil",
                PyRep::Integer(t.location_change_invul_until as i64),
            ),
        ]),
        None => PyRep::None,
    };

    let wire_state_rep = PyRep::keyval([
        ("hasWireSession", PyRep::Bool(wire_snap.is_some())),
        ("materialized", PyRep::Bool(materialized)),
        (
            "materializedSetSize",
            PyRep::Integer(materialized_set_size as i64),
        ),
        ("gridBallCount", PyRep::Integer(grid_ball_count as i64)),
        (
            "observerRegistered",
            PyRep::Bool(is_observer_registered),
        ),
        ("inSpace", PyRep::Bool(in_space)),
        (
            "destinyState",
            PyRep::String(
                wire_snap
                    .as_ref()
                    .map(|w| w.destiny_state.clone())
                    .unwrap_or_else(|| "Unwired".to_string()),
            ),
        ),
        (
            "epoch",
            PyRep::Integer(wire_snap.as_ref().map(|w| w.epoch as i64).unwrap_or(0)),
        ),
        (
            "setStateStamp",
            wire_snap
                .as_ref()
                .and_then(|w| w.set_state_stamp)
                .map(|s| PyRep::Integer(i64::from(s)))
                .unwrap_or(PyRep::None),
        ),
        (
            "lastStamp",
            wire_snap
                .as_ref()
                .and_then(|w| w.last_stamp)
                .map(|s| PyRep::Integer(i64::from(s)))
                .unwrap_or(PyRep::None),
        ),
        (
            "lastClientTick",
            wire_snap
                .as_ref()
                .and_then(|w| w.last_client_tick)
                .map(|t| PyRep::Integer(t as i64))
                .unwrap_or(PyRep::None),
        ),
        ("currentSystemTick", PyRep::Integer(current_tick as i64)),
        ("pendingSessionChange", pending_session_change_rep.clone()),
    ]);

    let ledger = ctx
        .flight_recorder
        .get_outbound_ledger(sid, DEFAULT_SESSION_LEDGER_LIMIT);
    let stamps = ctx
        .flight_recorder
        .get_destiny_stamps(sid, DEFAULT_SESSION_LEDGER_LIMIT);
    let rpc_hist = ctx
        .flight_recorder
        .get_history(sid, now_millis, DEFAULT_SESSION_LEDGER_LIMIT);

    let ledger_reps: Vec<PyRep> = ledger.iter().map(ledger_entry_to_pyrep).collect();
    let stamp_reps: Vec<PyRep> = stamps.iter().map(destiny_stamp_to_pyrep).collect();
    let rpc_reps: Vec<PyRep> = rpc_hist
        .iter()
        .map(|h| {
            PyRep::keyval([
                ("timestampMillis", PyRep::Integer(h.timestamp_millis as i64)),
                ("offsetMs", PyRep::Integer(h.offset_ms)),
                ("service", PyRep::String(h.service.clone())),
                ("method", PyRep::String(h.method.clone())),
                ("boundID", h.bound_id.map(|b| PyRep::Integer(b as i64)).unwrap_or(PyRep::None)),
                ("durationMicros", PyRep::Integer(h.duration_micros as i64)),
                ("status", PyRep::String(h.status.clone())),
            ])
        })
        .collect();

    PyRep::keyval([
        ("command", PyRep::String(cmd.to_string())),
        ("characterID", PyRep::Integer(cid)),
        ("sessionID", PyRep::Integer(sid as i64)),
        ("sessionDict", session_dict),
        ("wireState", wire_state_rep),
        ("materialized", PyRep::Bool(materialized)),
        (
            "hasPendingSessionChange",
            PyRep::Bool(timer_opt.is_some()),
        ),
        ("pendingSessionChange", pending_session_change_rep),
        ("ledgerCount", PyRep::Integer(ledger_reps.len() as i64)),
        ("destinyStampCount", PyRep::Integer(stamp_reps.len() as i64)),
        ("rpcHistoryCount", PyRep::Integer(rpc_reps.len() as i64)),
        ("entries", PyRep::List(ledger_reps)),
        ("destinyStamps", PyRep::List(stamp_reps)),
        ("rpcHistory", PyRep::List(rpc_reps)),
    ])
}

pub(crate) fn handle_session_cmd(
    ctx: &ServerContext,
    session: &SessionState,
    cid: i64,
    cmd: &str,
) -> Result<PyRep, BridgeRpcError> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.len() == 1 {
        return Ok(handle_session_dump(ctx, session, cid, cmd));
    }
    if parts.get(1).copied() != Some("ledger") {
        return Err(err(
            "InvalidArguments",
            "Usage: /session OR /session ledger <characterID|characterName|me> [limit]",
        ));
    }
    let Some(target_arg) = parts.get(2).copied() else {
        return Err(err(
            "InvalidArguments",
            "Usage: /session ledger <characterID|characterName|me> [limit]",
        ));
    };
    let limit = match parts.get(3) {
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) if n > 0 => n.min(crate::flight_recorder::MAX_OUTBOUND_LEDGER_PER_SESSION),
            _ => {
                return Err(err(
                    "InvalidArguments",
                    format!("Invalid ledger limit '{raw}'; expected positive integer"),
                ));
            }
        },
        None => DEFAULT_SESSION_LEDGER_LIMIT,
    };

    let resolved: Option<(i64, u64, String)> = if target_arg.eq_ignore_ascii_case("me") {
        ctx.db
            .with_snapshot(|snap| {
                snap.characters
                    .get(&cid)
                    .map(|c| (c.character_id, c.user_id, c.name.clone()))
            })
            .or_else(|| Some((cid, session.user_id.unwrap_or(0), format!("char:{cid}"))))
    } else if let Ok(target_cid) = target_arg.parse::<i64>() {
        ctx.db.with_snapshot(|snap| {
            snap.characters
                .get(&target_cid)
                .map(|c| (c.character_id, c.user_id, c.name.clone()))
        })
    } else {
        ctx.db.with_snapshot(|snap| {
            snap.characters
                .values()
                .find(|c| c.name.eq_ignore_ascii_case(target_arg))
                .map(|c| (c.character_id, c.user_id, c.name.clone()))
        })
    };

    let Some((target_cid, target_uid, target_name)) = resolved else {
        return Err(err(
            "CharacterNotFound",
            format!("Character '{target_arg}' not found"),
        ));
    };

    let target_sid = if target_cid == cid {
        Some(session.session_id)
    } else {
        ctx.db
            .get_online_session(target_uid)
            .filter(|os| os.character_id.is_none() || os.character_id == Some(target_cid))
            .map(|os| os.session_id)
    };
    let Some(target_sid) = target_sid else {
        return Err(err(
            "SessionNotFound",
            format!("Character {target_cid} ({target_name}) has no active session"),
        ));
    };

    let ledger = ctx.flight_recorder.get_outbound_ledger(target_sid, limit);
    let stamps = ctx.flight_recorder.get_destiny_stamps(target_sid, limit);
    let seq_counters = ctx.flight_recorder.session_sequencer_counters(target_sid);
    let inv_violations = ctx
        .flight_recorder
        .get_session_invariant_violations(target_sid, limit);
    let prevented_jolts = ctx
        .flight_recorder
        .get_session_prevented_jolts(target_sid, limit);

    let sorted_counts = |map: &HashMap<String, u64>| -> PyRep {
        let mut pairs: Vec<(&String, &u64)> = map.iter().collect();
        pairs.sort_by(|(a, _), (b, _)| a.cmp(b));
        PyRep::Dict(
            pairs
                .into_iter()
                .map(|(k, &v)| (PyRep::String(k.clone()), PyRep::Integer(v as i64)))
                .collect(),
        )
    };

    let entry_reps: Vec<PyRep> = ledger.iter().map(ledger_entry_to_pyrep).collect();
    let stamp_reps: Vec<PyRep> = stamps.iter().map(destiny_stamp_to_pyrep).collect();
    let violation_reps: Vec<PyRep> = inv_violations
        .iter()
        .map(|v| {
            PyRep::keyval([
                ("timestampMillis", PyRep::Integer(v.timestamp_millis as i64)),
                ("epoch", PyRep::Integer(i64::from(v.epoch))),
                ("stamp", PyRep::Integer(i64::from(v.stamp))),
                ("invariant", PyRep::String(v.invariant.clone())),
                ("detail", PyRep::String(v.detail.clone())),
            ])
        })
        .collect();
    let jolt_reps: Vec<PyRep> = prevented_jolts
        .iter()
        .map(|j| {
            PyRep::keyval([
                ("timestampMillis", PyRep::Integer(j.timestamp_millis as i64)),
                ("epoch", PyRep::Integer(i64::from(j.epoch))),
                ("batchStamp", PyRep::Integer(i64::from(j.batch_stamp))),
                ("trustedStamp", PyRep::Integer(i64::from(j.trusted_stamp))),
                ("reason", PyRep::String(j.reason.clone())),
            ])
        })
        .collect();

    Ok(PyRep::keyval([
        ("command", PyRep::String(cmd.to_string())),
        ("characterID", PyRep::Integer(target_cid)),
        ("characterName", PyRep::String(target_name)),
        ("sessionID", PyRep::Integer(target_sid as i64)),
        ("count", PyRep::Integer(entry_reps.len() as i64)),
        ("entries", PyRep::List(entry_reps)),
        ("destinyStamps", PyRep::List(stamp_reps)),
        (
            "droppedDependentNotifications",
            PyRep::Integer(seq_counters.dropped_dependent_notifications as i64),
        ),
        (
            "droppedDependentByName",
            sorted_counts(&seq_counters.dropped_dependent_by_name),
        ),
        (
            "invariantViolations",
            PyRep::Integer(seq_counters.invariant_violations as i64),
        ),
        (
            "invariantViolationsByName",
            sorted_counts(&seq_counters.invariant_violations_by_name),
        ),
        (
            "preventedJolts",
            PyRep::Integer(seq_counters.prevented_jolts as i64),
        ),
        ("invariantViolationEntries", PyRep::List(violation_reps)),
        ("preventedJoltEntries", PyRep::List(jolt_reps)),
    ]))
}

fn handle_backintime_cmd(
    ctx: &ServerContext,
    cid: i64,
    cmd: &str,
) -> Result<PyRep, BridgeRpcError> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.len() > 1 && !(parts.len() == 2 && parts[1].eq_ignore_ascii_case("me")) {
        return Err(err("InvalidArguments", "Usage: /backintime"));
    }

    let now_ft = ctx.now_blueos_filetime_ticks();
    let res = ctx.db.try_with_mut_snapshot(|snap| {
        let Some(ch) = snap.characters.get_mut(&cid) else {
            return Err(err("CharacterNotFound", format!("Character {cid} not found")));
        };
        let race_id = ch.race_id;
        let active_ship_id = ch.active_ship_id;
        let old_skill_ids: Vec<u32> = snap
            .skills
            .get(&cid)
            .map(|m| m.keys().copied().collect())
            .unwrap_or_default();

        let mut starter_skills_map: HashMap<u32, TrainedSkillRecord> = HashMap::new();
        let mut total_sp = 0.0;
        if let Some(race) = ctx.sde.races.get(&race_id) {
            for (&skill_id, &lvl) in &race.starter_skills {
                if lvl > 0 {
                    let rank = ctx
                        .sde
                        .types
                        .get(&skill_id)
                        .and_then(|t| {
                            t.default_attributes
                                .get(&eve_sde::attr_ids::SKILL_TIME_CONSTANT)
                                .copied()
                        })
                        .unwrap_or(1.0);
                    let sp = (250.0
                        * rank
                        * 2.0_f64.powf(2.5 * (i32::from(lvl.min(5)) - 1) as f64))
                    .ceil();
                    total_sp += sp;
                    starter_skills_map.insert(
                        skill_id,
                        TrainedSkillRecord {
                            type_id: skill_id,
                            level: lvl,
                            skill_points: sp,
                        },
                    );
                }
            }
        }
        if starter_skills_map.is_empty() {
            total_sp = 400_000.0;
        }

        let mut sorted_starter: Vec<TrainedSkillRecord> =
            starter_skills_map.values().cloned().collect();
        sorted_starter.sort_by_key(|s| s.type_id);
        let starter_history: Vec<SkillHistoryEntry> = sorted_starter
            .into_iter()
            .map(|s| SkillHistoryEntry {
                log_date_ft: now_ft,
                event_type_id: SKILL_EVENT_CHAR_CREATION,
                skill_type_id: s.type_id,
                skill_level: s.level,
                absolute_points: s.skill_points,
            })
            .collect();

        let mut changed_tids: BTreeSet<u32> = old_skill_ids.iter().copied().collect();
        changed_tids.extend(starter_skills_map.keys().copied());
        let removed_count = old_skill_ids
            .iter()
            .filter(|tid| !starter_skills_map.contains_key(tid))
            .count();
        let starter_count = starter_skills_map.len();

        ch.skill_points = total_sp;
        ch.free_skill_points = 0.0;
        if starter_skills_map.is_empty() {
            snap.skills.remove(&cid);
        } else {
            snap.skills.insert(cid, starter_skills_map);
        }
        snap.skill_queues.remove(&cid);
        snap.paused_skill_queues.remove(&cid);
        if starter_history.is_empty() {
            snap.skill_history.remove(&cid);
        } else {
            snap.skill_history.insert(cid, starter_history);
        }

        Ok((
            race_id,
            active_ship_id,
            starter_count,
            removed_count,
            total_sp,
            changed_tids.into_iter().collect::<Vec<u32>>(),
        ))
    })?;

    let (race_id, active_ship_id, starter_count, removed_count, total_sp, changed_tids) = res;
    crate::services::jump_clone_names::emit_server_skills_changed(
        ctx,
        cid,
        &changed_tids,
        Some("SkillReset"),
    );
    let _ = ctx.notify_new_skill_queue_saved(
        cid,
        crate::services::jump_clone_names::skill_queue_rowset(ctx, cid),
    );
    ctx.emit_typed(
        NotificationTarget::Character(cid),
        crate::router::notify_registry::OnFreeSkillPointsChanged {
            new_free_skill_points: 0.0,
        },
    );
    ctx.push_brain_update(cid);
    if active_ship_id > 0 {
        ctx.invalidate_and_push_fit(active_ship_id);
    }

    Ok(PyRep::keyval([
        ("command", PyRep::String(cmd.to_string())),
        ("characterID", PyRep::Integer(cid)),
        ("raceID", PyRep::Integer(i64::from(race_id))),
        ("starterSkillCount", PyRep::Integer(starter_count as i64)),
        ("removedSkillCount", PyRep::Integer(removed_count as i64)),
        ("totalSkillPoints", PyRep::Real(total_sp)),
        ("freeSkillPoints", PyRep::Real(0.0)),
        ("queueLength", PyRep::Integer(0)),
    ]))
}

pub(crate) fn derive_sde_wreck_loot(
    sde: &eve_sde::SdeRegistry,
    source_type_id: u32,
    wreck_type_id: u32,
    seed: u64,
) -> Vec<(u32, i32)> {
    let rng = MechanicRng::seeded(seed.max(1));
    let mut loot: Vec<(u32, i32)> = Vec::new();
    let mut seen: HashSet<u32> = HashSet::new();

    // 1. SDE salvage material yield for this wreck race/difficulty.
    for (mat_tid, qty) in sde.salvage_materials_for_wreck(wreck_type_id, true) {
        if qty > 0 && seen.insert(mat_tid) {
            loot.push((mat_tid, qty));
        }
    }

    // 2. SDE type_materials slice derived deterministically from the destroyed hull's BOM.
    if let Some(mats) = sde.type_materials.get(&source_type_id) {
        for m in mats {
            if m.quantity == 0 || !seen.insert(m.material_type_id) {
                continue;
            }
            let frac = 0.05 + rng.try_unit_f64().unwrap_or(0.5) * 0.20;
            let drop_qty = (((m.quantity as f64) * frac).round() as i32).max(1);
            loot.push((m.material_type_id, drop_qty));
            if loot.len() >= 6 {
                break;
            }
        }
    }

    // 3. Minimal-SDE safety net only when stripped unit-test SDE has empty salvage/material tables.
    if loot.is_empty() {
        for fallback_tid in [25_595_u32, 34_u32] {
            if sde.types.contains_key(&fallback_tid) {
                loot.push((fallback_tid, 1));
                break;
            }
        }
    }
    loot
}

async fn handle_spawnwreck_cmd(
    ctx: &ServerContext,
    session: &SessionState,
    cid: i64,
    cmd: &str,
) -> Result<PyRep, BridgeRpcError> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    let usage = "Usage: /spawnwreck <typeID> [count]";
    let Some(source_type_id) = parts.get(1).and_then(|s| s.parse::<u32>().ok()).filter(|&id| id > 0)
    else {
        return Err(err("InvalidArguments", usage));
    };
    if parts.len() > 3 {
        return Err(err("InvalidArguments", usage));
    }
    let count = match parts.get(2) {
        Some(raw) => match raw.parse::<usize>() {
            Ok(n) if (1..=MAX_SPAWNWRECK_COUNT).contains(&n) => n,
            _ => {
                return Err(err(
                    "InvalidArguments",
                    format!("Count for /spawnwreck must be between 1 and {MAX_SPAWNWRECK_COUNT}"),
                ));
            }
        },
        None => 1,
    };

    if !ctx.sde.types.contains_key(&source_type_id) {
        return Err(err(
            "TypeNotFound",
            format!("Type ID {source_type_id} not found in SDE"),
        ));
    }

    let (sys_id, ship_id) = require_caller_ship_in_space(ctx, session, cid, "/spawnwreck")?;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let Some((_, ship_ball)) = sys_handle
        .get_ball_snapshot(ship_id)
        .await
        .ok()
        .flatten()
    else {
        return Err(err(
            "ShipNotFound",
            format!("Active ship {ship_id} is not present in solar system {sys_id}"),
        ));
    };

    let wreck_type_id = ctx
        .sde
        .resolve_wreck_type_id(source_type_id, None)
        .unwrap_or(DEFAULT_WRECK_TYPE_ID);
    let now_unix = ctx.now_unix();
    let base_seed = splitmix64(
        u64::from(source_type_id)
            ^ (cid as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ u64::from(sys_id),
    );
    let origin = ship_ball.position;
    let wreck_sde = ctx.sde.types.get(&wreck_type_id);
    let source_name = ctx
        .sde
        .types
        .get(&source_type_id)
        .map(|t| t.name.clone())
        .unwrap_or_else(|| format!("Type {source_type_id}"));
    let wreck_name = wreck_sde
        .map(|t| t.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| format!("{source_name} Wreck"));

    let mut spawned_wrecks: Vec<(i64, DVec3, Vec<(u32, i32)>)> = Vec::with_capacity(count);
    let mut total_loot_items = 0_usize;
    let mut sample_loot_reps: Vec<PyRep> = Vec::new();

    ctx.db.with_mut_snapshot(|snap| {
        for idx in 0..count {
            snap.next_entity_id += 1;
            let wid = snap.next_entity_id;
            let pos = origin + DVec3::new(1_800.0 + (idx as f64) * 350.0, 0.0, 0.0);
            let loot = derive_sde_wreck_loot(
                &ctx.sde,
                source_type_id,
                wreck_type_id,
                splitmix64(base_seed ^ (idx as u64)),
            );

            crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
                snap, wid, now_unix, true,
            );
            snap.items.insert(
                wid,
                InventoryItemRecord {
                    item_id: wid,
                    type_id: wreck_type_id,
                    owner_id: cid,
                    location_id: i64::from(sys_id),
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            snap.item_positions.insert(wid, pos.to_array());

            for &(loot_tid, loot_qty) in &loot {
                snap.next_entity_id += 1;
                let lid = snap.next_entity_id;
                snap.items.insert(
                    lid,
                    InventoryItemRecord {
                        item_id: lid,
                        type_id: loot_tid,
                        owner_id: cid,
                        location_id: wid,
                        flag: InventoryFlag::Cargo,
                        quantity: loot_qty.max(1),
                        singleton: false,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
                total_loot_items += 1;
                if sample_loot_reps.len() < 32 {
                    sample_loot_reps.push(PyRep::keyval([
                        ("wreckID", PyRep::Integer(wid)),
                        ("itemID", PyRep::Integer(lid)),
                        ("typeID", PyRep::Integer(i64::from(loot_tid))),
                        ("quantity", PyRep::Integer(i64::from(loot_qty.max(1)))),
                    ]));
                }
            }
            spawned_wrecks.push((wid, pos, loot));
        }
    });

    let mut wreck_ids: Vec<i64> = Vec::with_capacity(spawned_wrecks.len());
    for (wid, pos, _) in spawned_wrecks {
        let _ = sys_handle
            .spawn_ball_with_meta(
                Ball {
                    id: wid,
                    type_id: wreck_type_id,
                    position: pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 50_000.0,
                    radius: 30.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(wreck_type_id),
                SlimItemMeta {
                    group_id: wreck_sde.map(|t| t.group_id).unwrap_or(GROUP_WRECK),
                    category_id: wreck_sde.map(|t| t.category_id).unwrap_or(2),
                    owner_id: cid,
                    char_id: None,
                    corp_id: -1,
                    alliance_id: -1,
                    security_status: 0.0,
                    is_cloaked: false,
                    name: Some(wreck_name.clone()),
                    ..Default::default()
                },
            )
            .await;
        crate::services::combat_crimewatch::combat_wrecks_killmails::register_container_lifetime(
            ctx,
            wid,
            sys_id,
            wreck_type_id,
            cid,
            now_unix,
            false,
        );
        wreck_ids.push(wid);
    }

    let first_wid = wreck_ids.first().copied().unwrap_or(0);
    Ok(PyRep::keyval([
        ("command", PyRep::String(cmd.to_string())),
        ("sourceTypeID", PyRep::Integer(i64::from(source_type_id))),
        ("wreckTypeID", PyRep::Integer(i64::from(wreck_type_id))),
        ("count", PyRep::Integer(wreck_ids.len() as i64)),
        ("wreckID", PyRep::Integer(first_wid)),
        (
            "wreckIDs",
            PyRep::List(wreck_ids.into_iter().map(PyRep::Integer).collect()),
        ),
        ("lootItemCount", PyRep::Integer(total_loot_items as i64)),
        ("lootItems", PyRep::List(sample_loot_reps)),
        ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
    ]))
}

/// Dispatches `#1088` GM soak and repro commands (`/soak`, `/deadwarp`, `/sysjunkclear`, `/backintime`, `/spawnwreck`).
pub(crate) async fn handle_gm_soak_slash_cmd(
    ctx: &ServerContext,
    session: &SessionState,
    cid: i64,
    cmd: &str,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if cmd == "/soak" || cmd.starts_with("/soak ") {
        return Some(handle_soak_cmd(ctx, session, cid, cmd).await);
    }
    if cmd == "/deadwarp" || cmd.starts_with("/deadwarp ") {
        return Some(handle_deadwarp_cmd(ctx, session, cid, cmd).await);
    }
    if cmd == "/sysjunkclear" || cmd.starts_with("/sysjunkclear ") {
        return Some(handle_sysjunkclear_cmd(ctx, session, cid, cmd).await);
    }
    if cmd == "/backintime" || cmd.starts_with("/backintime ") {
        return Some(handle_backintime_cmd(ctx, cid, cmd));
    }
    if cmd == "/spawnwreck" || cmd.starts_with("/spawnwreck ") {
        return Some(handle_spawnwreck_cmd(ctx, session, cid, cmd).await);
    }
    None
}

fn step_deadwarps_for_system(ctx: &ServerContext, solar_system_id: u32) {
    let entries: Vec<GmDeadwarpState> = with_combat_runtime(ctx, |rt| {
        rt.gm_deadwarps
            .values()
            .filter(|d| d.solar_system_id == solar_system_id)
            .cloned()
            .collect()
    });
    if entries.is_empty() {
        return;
    }

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let now_unix = ctx.now_unix();
    let ball_snaps: HashMap<i64, (DVec3, bool)> = if let Ok(bp) = sys_handle.ballpark.read() {
        entries
            .iter()
            .filter_map(|st| {
                bp.balls
                    .get(&st.ship_id)
                    .map(|b| (st.ship_id, (b.position, matches!(b.mode, MovementMode::Warp { .. }))))
            })
            .collect()
    } else {
        return;
    };

    let mut to_remove: Vec<i64> = Vec::new();
    let mut to_delay: Vec<(i64, u64)> = Vec::new();
    let mut launch_returns: Vec<(i64, i64, DVec3, DVec3, f64)> = Vec::new();
    let mut arrived_positions: Vec<(i64, [f64; 3])> = Vec::new();

    for st in entries {
        let Some(&(pos, in_warp)) = ball_snaps.get(&st.ship_id) else {
            to_remove.push(st.ship_id);
            continue;
        };
        let origin = DVec3::from_array(st.origin_pos);
        let anchor = DVec3::from_array(st.anchor_pos);

        match st.phase {
            DeadwarpPhase::Outbound => {
                if !in_warp {
                    if pos.distance(anchor) <= DEADWARP_ARRIVAL_TOLERANCE_M {
                        to_delay.push((st.ship_id, now_unix));
                    } else {
                        to_remove.push(st.ship_id);
                    }
                }
            }
            DeadwarpPhase::AtAnchorDelay => {
                launch_returns.push((st.ship_id, st.char_id, pos, origin, st.warp_speed_au_s));
            }
            DeadwarpPhase::Returning => {
                if !in_warp {
                    arrived_positions.push((st.ship_id, pos.to_array()));
                    to_remove.push(st.ship_id);
                }
            }
        }
    }

    let mut returned_ok: Vec<i64> = Vec::new();
    if !launch_returns.is_empty() {
        if let Ok(mut bp) = sys_handle.ballpark.write() {
            for (ship_id, char_id, curr_pos, origin, warp_speed) in launch_returns {
                let mode = MovementMode::new_warp(curr_pos, origin, warp_speed);
                if bp.set_mode(ship_id, mode).is_ok() {
                    returned_ok.push(ship_id);
                    ctx.active_warps
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(ship_id, char_id);
                } else {
                    to_remove.push(ship_id);
                }
            }
            crate::system_actor::sync_ball_positions(
                &bp,
                &sys_handle.ball_positions,
                &sys_handle.scrambled_balls,
            );
        }
    }

    if !arrived_positions.is_empty() {
        ctx.db.with_mut_snapshot(|snap| {
            for (ship_id, pos) in arrived_positions {
                snap.item_positions.insert(ship_id, pos);
            }
        });
    }

    if !to_remove.is_empty() {
        let mut warps = ctx.active_warps.lock().unwrap_or_else(|e| e.into_inner());
        for &ship_id in &to_remove {
            warps.remove(&ship_id);
        }
    }

    if !to_remove.is_empty() || !to_delay.is_empty() || !returned_ok.is_empty() {
        with_mut_combat_runtime(ctx, |rt| {
            for (ship_id, ts) in to_delay {
                if let Some(st) = rt.gm_deadwarps.get_mut(&ship_id) {
                    st.phase = DeadwarpPhase::AtAnchorDelay;
                    st.reached_anchor_unix = Some(ts);
                }
            }
            for ship_id in returned_ok {
                if let Some(st) = rt.gm_deadwarps.get_mut(&ship_id) {
                    st.phase = DeadwarpPhase::Returning;
                }
            }
            for ship_id in to_remove {
                rt.gm_deadwarps.remove(&ship_id);
            }
        });
    }
}

fn step_logi_soak_for_system(ctx: &ServerContext, solar_system_id: u32) {
    let Some(mut soak) = with_combat_runtime(ctx, |rt| rt.gm_logi_soaks.get(&solar_system_id).cloned())
    else {
        return;
    };

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let now_unix = ctx.now_unix();
    let (bp_tick, target_pos_opt, target_in_warp, live_npcs) =
        if let Ok(bp) = sys_handle.ballpark.read() {
            let target_info = bp
                .balls
                .get(&soak.target_ship_id)
                .map(|b| (b.position, matches!(b.mode, MovementMode::Warp { .. })));
            let live: Vec<(i64, u32, DVec3)> = soak
                .npc_ids
                .iter()
                .filter_map(|&nid| {
                    bp.balls
                        .get(&nid)
                        .map(|b| (nid, b.type_id, b.position))
                })
                .collect();
            (
                bp.current_tick,
                target_info.map(|(p, _)| p),
                target_info.is_some_and(|(_, w)| w),
                live,
            )
        } else {
            return;
        };

    let live_ids: Vec<i64> = live_npcs.iter().map(|&(nid, _, _)| nid).collect();
    if live_ids.is_empty() {
        remove_soak_dbuffs_and_notify(ctx, solar_system_id, soak.target_char_id, soak.target_ship_id);
        with_mut_combat_runtime(ctx, |rt| {
            rt.gm_logi_soaks.remove(&solar_system_id);
        });
        return;
    }

    let Some(target_pos) = target_pos_opt else {
        if live_ids.len() != soak.npc_ids.len() {
            with_mut_combat_runtime(ctx, |rt| {
                if let Some(st) = rt.gm_logi_soaks.get_mut(&solar_system_id) {
                    st.npc_ids = live_ids;
                }
            });
        }
        return;
    };

    if target_in_warp {
        return;
    }

    let due = soak.cycle_count == 0
        || bp_tick >= soak.last_cycle_tick.saturating_add(soak.cycle_interval_ticks);
    if !due {
        if live_ids.len() != soak.npc_ids.len() {
            with_mut_combat_runtime(ctx, |rt| {
                if let Some(st) = rt.gm_logi_soaks.get_mut(&solar_system_id) {
                    st.npc_ids = live_ids;
                }
            });
        }
        return;
    }

    let eligible_npcs: Vec<(i64, u32)> = live_npcs
        .iter()
        .filter(|(_, _, pos)| pos.distance(target_pos) <= SOAK_LOGI_REP_RANGE_M)
        .map(|&(nid, tid, _)| (nid, tid))
        .collect();
    if eligible_npcs.is_empty() {
        return;
    }

    let shield_total = SOAK_LOGI_SHIELD_REP_PER_CYCLE * (eligible_npcs.len() as f64);
    let armor_total = SOAK_LOGI_ARMOR_REP_PER_CYCLE * (eligible_npcs.len() as f64);
    let dbuff_expires_unix = now_unix.saturating_add(SOAK_LOGI_DBUFF_TTL_SECS);
    let dbuff_expires_ft = unix_to_filetime(dbuff_expires_unix);
    let wire_dbuffs: Vec<(i64, f64, i64)> = SOAK_LOGI_COMMAND_BURST_DBUFFS
        .iter()
        .map(|&(id, val)| (i64::from(id), val, dbuff_expires_ft))
        .collect();

    let mut actual_shield_repaired = 0.0;
    let mut actual_armor_repaired = 0.0;
    {
        let Ok(mut bp) = sys_handle.ballpark.write() else {
            return;
        };
        for &(nid, _) in &eligible_npcs {
            if let Some(b) = bp.balls.get(&nid) {
                if matches!(b.mode, MovementMode::Stop) {
                    let _ = bp.set_mode(
                        nid,
                        MovementMode::Orbit {
                            target_id: soak.target_ship_id,
                            range: SOAK_LOGI_ORBIT_RANGE_M,
                        },
                    );
                }
            }
        }

        let mut damage_ratios: Option<(f64, f64, f64)> = None;
        if let Some(cs) = bp.combat_states.get_mut(&soak.target_ship_id) {
            let old_s = cs.shield_hp;
            let old_a = cs.armor_hp;
            cs.shield_hp = (cs.shield_hp + shield_total).min(cs.max_shield_hp);
            cs.armor_hp = (cs.armor_hp + armor_total).min(cs.max_armor_hp);
            actual_shield_repaired = (cs.shield_hp - old_s).max(0.0);
            actual_armor_repaired = (cs.armor_hp - old_a).max(0.0);
            if actual_shield_repaired > 0.0 || actual_armor_repaired > 0.0 {
                damage_ratios = Some((
                    (cs.shield_hp / cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0),
                    (cs.armor_hp / cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0),
                    (cs.structure_hp / cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0),
                ));
            }
        }
        if let Some((sr, ar, hr)) = damage_ratios {
            bp.emit_event(
                Some(soak.target_ship_id),
                DestinyEvent::OnDamageStateChange {
                    ball_id: soak.target_ship_id,
                    shield_ratio: sr,
                    armor_ratio: ar,
                    structure_ratio: hr,
                },
            );
        }

        for &(nid, _) in &eligible_npcs {
            for guid in ["effects.ShieldTransfer", "effects.RemoteArmourRepair"] {
                bp.emit_event(
                    Some(nid),
                    DestinyEvent::OnSpecialFX {
                        ball_id: nid,
                        module_id: Some(soak.target_ship_id),
                        guid: guid.to_string(),
                        is_active: true,
                    },
                );
            }
        }
        bp.update_dbuffs(soak.target_ship_id, wire_dbuffs);
    }

    let now_ft = ctx.now_blueos_filetime_ticks();
    for &(nid, ntid) in &eligible_npcs {
        for guid in ["effects.ShieldTransfer", "effects.RemoteArmourRepair"] {
            ctx.emit_typed(
                NotificationTarget::Character(soak.target_char_id),
                crate::router::notify_registry::OnSpecialFX {
                    ship_id: nid,
                    module_id: Some(nid),
                    module_type_id: Some(i64::from(ntid)),
                    target_id: Some(soak.target_ship_id),
                    other_type_id: None,
                    guid: guid.to_string(),
                    is_offensive: false,
                    start: 1,
                    active: 1,
                    duration: (SOAK_LOGI_CYCLE_INTERVAL_TICKS as f64) * 1_000.0,
                    repeat: 1,
                    start_time: now_ft,
                    time_from_start: 0.0,
                    graphic_info: None,
                },
            );
        }
    }

    {
        let mut st = scanners_superweapons_state()
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let list = st
            .ship_dbuffs
            .entry((ctx.instance_id, soak.target_ship_id))
            .or_default();
        list.retain(|e| {
            e.source_module_id != GM_LOGI_SOAK_DBUFF_SOURCE_ID && e.expires_at_unix > now_unix
        });
        for &(dbuff_id, val) in &SOAK_LOGI_COMMAND_BURST_DBUFFS {
            list.push(ActiveDbuffEntry {
                dbuff_id,
                value: val,
                expires_at_unix: dbuff_expires_unix,
                source_module_id: GM_LOGI_SOAK_DBUFF_SOURCE_ID,
            });
        }
    }

    ctx.invalidate_ship_fitting(soak.target_ship_id);
    ctx.push_fit_projection(soak.target_char_id, soak.target_ship_id);
    ctx.emit_typed(
        NotificationTarget::Character(soak.target_char_id),
        crate::router::notify_registry::OnDbuffUpdated {
            ship_id: soak.target_ship_id,
        },
    );

    soak.npc_ids = live_ids;
    soak.cycle_count = soak.cycle_count.saturating_add(1);
    soak.last_cycle_tick = bp_tick;
    soak.last_cycle_unix = now_unix;
    soak.total_shield_repaired += actual_shield_repaired;
    soak.total_armor_repaired += actual_armor_repaired;

    with_mut_combat_runtime(ctx, |rt| {
        rt.gm_logi_soaks.insert(solar_system_id, soak);
    });
}

/// Steps active `/soak logi` and `/deadwarp` automation for `solar_system_id` once per simulation tick (`#1088`).
pub fn step_gm_soak_and_deadwarp_for_system(ctx: &ServerContext, solar_system_id: u32) {
    let has_work = {
        let guard = ctx.combat_ephemeral.read().unwrap_or_else(|e| e.into_inner());
        guard.gm_logi_soaks.contains_key(&solar_system_id)
            || guard
                .gm_deadwarps
                .values()
                .any(|d| d.solar_system_id == solar_system_id)
    };
    if !has_work {
        return;
    }
    step_deadwarps_for_system(ctx, solar_system_id);
    step_logi_soak_for_system(ctx, solar_system_id);
}

#[cfg(test)]
#[path = "gm_soak_commands_tests.rs"]
mod gm_soak_commands_tests;
