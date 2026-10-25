//! `sp6_lifecycle` — Logoff, disconnect & login in space (`plan:SP-6`) and background cluster
//! scheduler, unobserved systems & ballpark reload (`plan:SP-7`).
//!
// parity: plan:SP-6, plan:SP-7
// parity: feat:crimewatch.pvp-logoff-timer-renewal
// parity: feat:lifecycle.world-unobserved-system-sim
// parity: feat:lifecycle.world-restart-space-objects
// parity: feat:lifecycle.wreck-restart
// parity: feat:lifecycle.structure-restart
// parity: feat:lifecycle.mobile_warp_disruptor-lifecycle
// parity: feat:lifecycle.drones-in-space-persist-restart
// parity: feat:lifecycle.self-destruct-server-timer
// parity: feat:lifecycle.abyssal-disconnect
// parity: feat:lifecycle.wreck-downtime
// parity: feat:lifecycle.mission-downtime-respawn

#![forbid(unsafe_code)]

mod cluster_tick;

pub use cluster_tick::{
    cleanup_npc_wrecks_and_reset_missions_on_downtime, on_advance_secs,
    populate_persisted_space_objects, seed_persisted_abandoned_drones,
    spawn_mission_wave_on_pocket_entry_if_needed, tick_background_cluster,
};

use crate::router::{NotificationTarget, ServerContext};
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};
pub use eve_db::{
    AbyssalPersistedSession, DisconnectedShipEntry, InventoryFlag, PersistedCombatHp,
    PersistedSpaceObjectMeta, SafeLogoffEntry, SelfDestructPersistedEntry, Sp6PersistedState,
    WorldRepository,
};
use eve_destiny::{Ball, CombatState, MovementMode, SlimItemMeta};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::sync::Arc;

pub const SAFE_LOGOFF_COUNTDOWN_SECS: u64 = 30;
pub const UNAGGRESSED_LOGOFF_REMOVE_SECS: u64 = 60;
pub const WEAPONS_TIMER_SECS: u64 = 60;
pub const NPC_TIMER_SECS: u64 = 300;
pub const PVP_TIMER_SECS: u64 = 900;
pub const CRIMINAL_TIMER_SECS: u64 = 900;
pub const EMERGENCY_WARP_DISTANCE_M: f64 = 1_000_000_000.0; // 1,000,000 km
pub const MOBILE_WARP_DISRUPTOR_DECAY_SECS: u64 = 2 * 86_400; // 48h (172,800s)
// DERIVED: EVE Online Upwell and legacy container decay rules
pub const SECURE_CONTAINER_DECAY_SECS: u64 = 30 * 86_400; // 30d (2,592,000s)
pub const WRECK_DECAY_LIFETIME_SECS: u64 = 7_200; // 2h
pub const JETCAN_DECAY_LIFETIME_SECS: u64 = 3_600; // 1h
pub const ABYSSAL_COLLAPSE_TIMEOUT_SECS: u64 = 1_200; // 20m
pub const SELF_DESTRUCT_COUNTDOWN_SECS: u64 = 120; // 2m

/// Converts Unix seconds into Windows FILETIME (`100ns` ticks since 1601-01-01 UTC).
pub fn unix_to_filetime(unix_secs: u64) -> i64 {
    (unix_secs as i64)
        .saturating_add(11_644_473_600)
        .saturating_mul(10_000_000)
}

pub fn load_sp6_state_from_snap(snap: &eve_db::WorldStateSnapshot) -> Sp6PersistedState {
    snap.sp6_lifecycle.clone()
}

pub fn save_sp6_state_to_snap(snap: &mut eve_db::WorldStateSnapshot, state: &Sp6PersistedState) {
    snap.sp6_lifecycle = state.clone();
}

pub fn with_sp6_state<R>(
    db: &Arc<WorldRepository>,
    f: impl FnOnce(&Sp6PersistedState) -> R,
) -> R {
    let st = db.with_snapshot(|snap| snap.sp6_lifecycle.clone());
    f(&st)
}

pub fn try_with_mut_sp6_state<R>(
    db: &Arc<WorldRepository>,
    f: impl FnOnce(&mut Sp6PersistedState, &mut eve_db::WorldStateSnapshot) -> Result<R, String>,
) -> Result<R, String> {
    db.try_with_mut_snapshot(|snap| {
        let mut st = std::mem::take(&mut snap.sp6_lifecycle);
        let res = f(&mut st, snap);
        snap.sp6_lifecycle = st;
        res
    })
}

pub fn with_mut_sp6_state<R>(
    db: &Arc<WorldRepository>,
    f: impl FnOnce(&mut Sp6PersistedState, &mut eve_db::WorldStateSnapshot) -> R,
) -> R {
    try_with_mut_sp6_state(db, |st, snap| Ok(f(st, snap))).expect("sp6_lifecycle snapshot commit")
}

/// Records wreck, jetcan, or corpse creation metadata in `snap.sp6_lifecycle.space_object_meta`.
pub fn record_wreck_or_container_meta_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    item_id: i64,
    created_unix: u64,
    is_npc_wreck: bool,
) {
    let meta = snap
        .sp6_lifecycle
        .space_object_meta
        .entry(item_id)
        .or_default();
    if meta.created_unix.is_none() {
        meta.created_unix = Some(created_unix);
    }
    if is_npc_wreck {
        meta.is_npc_wreck = true;
    }
}

/// Records a launched drone's in-space ball ID in `snap.sp6_lifecycle.space_object_meta`.
pub fn record_drone_ball_meta_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    item_id: i64,
    ball_id: i64,
) {
    snap.sp6_lifecycle
        .space_object_meta
        .entry(item_id)
        .or_default()
        .drone_ball_id = Some(ball_id);
}

/// Records anchoring metadata for a deployable or Mobile Warp Disruptor in `snap.sp6_lifecycle.space_object_meta`.
pub fn record_anchor_meta_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    item_id: i64,
    anchor_start_unix: u64,
    anchoring_delay_secs: f64,
    bubble_radius_m: Option<f64>,
) {
    let meta = snap
        .sp6_lifecycle
        .space_object_meta
        .entry(item_id)
        .or_default();
    meta.anchor_start_unix = Some(anchor_start_unix);
    meta.anchoring_delay_secs = anchoring_delay_secs;
    if let Some(r) = bubble_radius_m {
        meta.bubble_radius_m = Some(r);
    }
}

/// Queries the SDE uncollected decay lifetime in seconds for a Mobile Warp Disruptor type (`group_id == 361`),
/// defaulting to 48 hours (`172,800s`) per EVE mechanics (`feat:lifecycle.mobile_warp_disruptor-lifecycle`).
pub fn sde_mobile_warp_disruptor_decay_secs(sde: &eve_sde::SdeRegistry, type_id: u32) -> u64 {
    if let Some(t) = sde.types.get(&type_id) {
        if let Some(&val) = t
            .default_attributes
            .get(&1237)
            .or_else(|| t.default_attributes.get(&1034))
        {
            if val > 0.0 {
                let secs = if val > 1_000_000.0 {
                    (val / 1_000.0).round() as u64
                } else {
                    val.round() as u64
                };
                if secs >= 3_600 {
                    return secs;
                }
            }
        }
    }
    MOBILE_WARP_DISRUPTOR_DECAY_SECS
}

/// Extracts SDE-derived hull physics `(group_id, category_id, mass, radius, max_velocity, inertia, warp_speed_au_s)` for `ship_type_id`.
pub fn sde_ship_physics(
    sde: &eve_sde::SdeRegistry,
    ship_type_id: u32,
) -> (u32, u32, f64, f64, f64, f64, f64) {
    sde.types
        .get(&ship_type_id)
        .map(|t| {
            let max_vel = t.default_attributes.get(&37).copied().unwrap_or(300.0).max(50.0);
            let inertia = t.default_attributes.get(&70).copied().unwrap_or(2.5).max(0.5);
            let base_warp = t.default_attributes.get(&1281).copied().unwrap_or(1.0).max(0.5);
            let warp_mult = t.default_attributes.get(&600).copied().unwrap_or(3.0).max(1.0);
            let warp_speed_au_s = (base_warp * warp_mult).clamp(1.5, 12.0);
            (
                t.group_id,
                t.category_id,
                t.mass.max(1_000.0),
                t.radius.max(20.0),
                max_vel,
                inertia,
                warp_speed_au_s,
            )
        })
        .unwrap_or((25, 6, 1_200_000.0, 40.0, 300.0, 2.5, 3.0))
}

/// Persists a ship ball's shield/armor/hull HP into `snap.sp6_lifecycle.ship_combat_hp` so damage
/// taken while disconnected or before a server restart is preserved upon reconnect (`plan:SP-6`).
pub fn persist_ship_combat_hp_in_db(
    db: &Arc<WorldRepository>,
    ball_id: i64,
    cs: &CombatState,
) {
    let _ = db.try_with_mut_snapshot(|snap| -> Result<(), String> {
        snap.sp6_lifecycle.ship_combat_hp.insert(
            ball_id,
            PersistedCombatHp {
                shield_hp: cs.shield_hp.max(0.0),
                armor_hp: cs.armor_hp.max(0.0),
                hull_hp: cs.structure_hp.max(0.0),
            },
        );
        Ok(())
    });
}

/// Restores persisted shield/armor/hull HP from `snap.sp6_lifecycle.ship_combat_hp` onto `cs`.
pub fn apply_persisted_hp_to_combat_state(
    ctx: &ServerContext,
    ship_id: i64,
    cs: &mut CombatState,
) {
    ctx.db.with_snapshot(|snap| {
        if let Some(hp) = snap.sp6_lifecycle.ship_combat_hp.get(&ship_id) {
            cs.shield_hp = hp.shield_hp.clamp(0.0, cs.max_shield_hp);
            cs.armor_hp = hp.armor_hp.clamp(0.0, cs.max_armor_hp);
            if hp.hull_hp > 0.0 {
                cs.structure_hp = hp.hull_hp.clamp(0.0, cs.max_structure_hp);
            }
        }
    });
}

/// Evaluates active Crimewatch / combat timers `(weapons_until, npc_until, pvp_until, criminal_until)` for `char_id`.
pub fn active_timers_for_char(
    ctx: &ServerContext,
    char_id: i64,
    now_unix: u64,
) -> (u64, u64, u64, u64) {
    let (cw_weap, cw_pvp, cw_crim) = {
        let guard = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner());
        if let Some(st) = guard.states.get(&char_id) {
            (
                st.weapon_timer_until_unix,
                st.pvp_timer_until_unix,
                st.criminal_flag_until_unix,
            )
        } else {
            (0, 0, 0)
        }
    };
    let rt_npc = with_combat_runtime(ctx, |rt| {
        rt.npc_timers.get(&char_id).copied().unwrap_or(0)
    });
    let (sp_weap, sp_npc, sp_pvp, sp_crim) = with_sp6_state(&ctx.db, |st| {
        (
            st.weapon_timers_until.get(&char_id).copied().unwrap_or(0),
            st.npc_timers_until.get(&char_id).copied().unwrap_or(0),
            st.pvp_timers_until.get(&char_id).copied().unwrap_or(0),
            st.criminal_timers_until.get(&char_id).copied().unwrap_or(0),
        )
    });
    let weap = cw_weap.max(sp_weap);
    let npc = rt_npc.max(sp_npc);
    let pvp = cw_pvp.max(sp_pvp);
    let crim = cw_crim.max(sp_crim);
    (
        if weap > now_unix { weap } else { 0 },
        if npc > now_unix { npc } else { 0 },
        if pvp > now_unix { pvp } else { 0 },
        if crim > now_unix { crim } else { 0 },
    )
}

pub fn has_active_pvp_timer(ctx: &ServerContext, char_id: i64, now_unix: u64) -> bool {
    let (_, _, pvp, crim) = active_timers_for_char(ctx, char_id, now_unix);
    pvp > now_unix || crim > now_unix
}

/// Blocks Abyssal, Noise/Signal, and Proving Grounds filaments while under an active 15m PvP timer
/// (`feat:crimewatch.pvp-logoff-timer-renewal`).
pub fn require_no_pvp_timer_for_filament(
    ctx: &ServerContext,
    char_id: i64,
) -> Result<(), BridgeRpcError> {
    let now_unix = ctx.now_unix();
    if has_active_pvp_timer(ctx, char_id, now_unix) {
        return Err(BridgeRpcError {
            code: "PvpTimerActive".into(),
            message: "Cannot activate filament while a 15-minute Capsuleer Log-Off (PvP) timer is active"
                .into(),
        });
    }
    Ok(())
}

/// Records capsuleer-vs-capsuleer aggression, starting/refreshing the 15-minute (`900s`) PvP log-off timer
/// on both attacker and victim, aborting any active safe-logoff countdown, and renewing the victim's
/// in-space removal timer to `now + 900s` if the victim is already disconnected (`feat:crimewatch.pvp-logoff-timer-renewal`).
pub fn record_pvp_aggression_and_renew_logoff_timer(
    ctx: &ServerContext,
    attacker_char_id: i64,
    victim_char_id: i64,
    now_unix: u64,
) {
    let pvp_until = now_unix.saturating_add(PVP_TIMER_SECS);
    let weap_until = now_unix.saturating_add(WEAPONS_TIMER_SECS);
    {
        let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
        let att = cw.get_or_create(attacker_char_id);
        att.weapon_timer_until_unix = att.weapon_timer_until_unix.max(weap_until);
        att.pvp_timer_until_unix = att.pvp_timer_until_unix.max(pvp_until);
        if victim_char_id > 0 {
            let vic = cw.get_or_create(victim_char_id);
            vic.pvp_timer_until_unix = vic.pvp_timer_until_unix.max(pvp_until);
        }
    }
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.weapon_timers_until
            .insert(attacker_char_id, weap_until);
        st.pvp_timers_until.insert(attacker_char_id, pvp_until);
        if victim_char_id > 0 {
            st.pvp_timers_until.insert(victim_char_id, pvp_until);
            if let Some(disc) = st.disconnected_ships.get_mut(&victim_char_id) {
                if !disc.removed_from_space {
                    disc.remove_at_unix = disc.remove_at_unix.max(pvp_until);
                }
            }
        }
    });
    let _ = maybe_abort_safe_logoff_on_action(ctx, attacker_char_id, "SAFE_LOGOFF_ABORT_AGGRESSION");
    if victim_char_id > 0 {
        let _ =
            maybe_abort_safe_logoff_on_action(ctx, victim_char_id, "SAFE_LOGOFF_ABORT_AGGRESSION");
    }
}

/// Records NPC aggression (`300s` = 5m timer) for an online character.
/// Per `feat:crimewatch.pvp-logoff-timer-renewal`, NPC aggression does **not** renew the log-off
/// timer of a ship whose pilot has already logged off / disconnected.
pub fn record_npc_aggression_timer(ctx: &ServerContext, char_id: i64, now_unix: u64) {
    let is_disconnected = with_sp6_state(&ctx.db, |st| {
        st.disconnected_ships
            .get(&char_id)
            .is_some_and(|d| !d.removed_from_space)
    });
    if is_disconnected {
        return;
    }
    let npc_until = now_unix.saturating_add(NPC_TIMER_SECS);
    with_mut_combat_runtime(ctx, |rt| {
        rt.npc_timers.insert(char_id, npc_until);
    });
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.npc_timers_until.insert(char_id, npc_until);
    });
    let _ = maybe_abort_safe_logoff_on_action(ctx, char_id, "SAFE_LOGOFF_ABORT_AGGRESSION");
}

/// Checks whether the pilot has active cycling modules, cloak/bastion/siege state, deployed drones/fighters,
/// or deployed scanner/moon probes in space that block `ship.SafeLogoff` (`BF-SP6-SAFE-LOGOFF-CONDITIONS`).
pub fn safe_logoff_blocking_space_conditions(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
) -> Vec<&'static str> {
    let mut conds = Vec::new();
    let has_active_mods = ctx.systems.has_active_modules_or_cloak_sync(sys_id, ship_id)
        || ctx.mining_cycles.read().is_ok_and(|mc| {
            mc.values()
                .any(|c| c.ship_id == ship_id || c.char_id == char_id)
        })
        || ctx.ship_fittings.read().is_ok_and(|sf| {
            sf.get(&ship_id).is_some_and(|eng| {
                eng.modules.values().any(|m| {
                    matches!(
                        m.state,
                        eve_dogma::ModuleState::Active | eve_dogma::ModuleState::Overloaded
                    )
                })
            })
        });
    if has_active_mods {
        conds.push("SAFE_LOGOFF_ACTIVE_MODULES");
    }

    let has_drones_or_fighters = with_combat_runtime(ctx, |rt| {
        rt.active_drones_in_space
            .values()
            .any(|d| !d.is_abandoned && (d.owner_char_id == char_id || d.parent_ship_id == ship_id))
    }) || ctx.active_fighters.read().is_ok_and(|af| {
        af.values()
            .any(|f| !f.is_abandoned && (f.owner_char_id == char_id || f.parent_ship_id == ship_id))
    });
    if has_drones_or_fighters {
        conds.push("SAFE_LOGOFF_DRONES_IN_SPACE");
    }

    let has_probes = ctx.active_scanner_probes.read().is_ok_and(|sp| {
        sp.values()
            .any(|p| p.owner_char_id == char_id || p.ship_id == ship_id)
    }) || ctx
        .active_moon_probes
        .read()
        .is_ok_and(|mp| mp.iter().any(|p| p.owner_char_id == char_id));
    if has_probes {
        conds.push("SAFE_LOGOFF_PROBES_IN_SPACE");
    }

    conds
}

/// Handles `ship.SafeLogoff` (0 args) (`plan:SP-6`).
/// - Refuses if docked, warp-scrambled, self-destructing, running active modules/cloak, controlling drones/fighters/probes,
///   or under an active Weapons, NPC, PvP, or Criminal timer:
///   emits `OnSafeLogoffFailed` with `vec![PyRep::List(failed_conditions)]` (`arity = 1`) and returns `PyRep::List(failed_conditions)`.
/// - Otherwise starts a 30s countdown (`now_unix + 30`), emits `OnSafeLogoffTimerStarted` with
///   `vec![PyRep::Integer(unix_to_filetime(complete_at_unix))]` (`arity = 1`), and returns `PyRep::List(vec![])`.
pub async fn handle_safe_logoff(
    ctx: &ServerContext,
    session: &mut SessionState,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let now_unix = ctx.now_unix();
    let mut failed_conditions: Vec<PyRep> = Vec::new();

    if session.station_id.is_some() || session.structure_id.is_some() {
        failed_conditions.push(PyRep::String("SAFE_LOGOFF_DOCKED".into()));
    }
    let ship_id = session.ship_id.unwrap_or_else(|| {
        ctx.db
            .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.active_ship_id))
            .unwrap_or(0)
    });
    let sys_id = session.solar_system_id.unwrap_or_else(|| {
        ctx.db
            .with_snapshot(|s| s.characters.get(&char_id).map(|c| c.solar_system_id))
            .unwrap_or(30_000_142)
    });

    if ship_id > 0 && session.station_id.is_none() && session.structure_id.is_none() {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        if sys_handle.is_ball_scrambled(ship_id).await.unwrap_or(false) {
            failed_conditions.push(PyRep::String("SAFE_LOGOFF_WARP_SCRAMBLED".into()));
        }
        for code in safe_logoff_blocking_space_conditions(ctx, char_id, ship_id, sys_id) {
            failed_conditions.push(PyRep::String(code.into()));
        }
    }

    let (weap_until, npc_until, pvp_until, crim_until) =
        active_timers_for_char(ctx, char_id, now_unix);
    if weap_until > now_unix {
        failed_conditions.push(PyRep::String("SAFE_LOGOFF_WEAPONS_TIMER".into()));
    }
    if npc_until > now_unix {
        failed_conditions.push(PyRep::String("SAFE_LOGOFF_NPC_TIMER".into()));
    }
    if pvp_until > now_unix {
        failed_conditions.push(PyRep::String("SAFE_LOGOFF_PVP_TIMER".into()));
    }
    if crim_until > now_unix {
        failed_conditions.push(PyRep::String("SAFE_LOGOFF_CRIMINAL_TIMER".into()));
    }

    let self_destruct_active = with_combat_runtime(ctx, |rt| {
        rt.self_destruct_countdowns
            .get(&ship_id)
            .is_some_and(|cd| now_unix < cd.detonate_at_unix)
    }) || with_sp6_state(&ctx.db, |st| {
        st.self_destructs
            .get(&ship_id)
            .is_some_and(|cd| now_unix < cd.detonate_at_unix)
    });
    if self_destruct_active {
        failed_conditions.push(PyRep::String("SAFE_LOGOFF_SELF_DESTRUCT_ACTIVE".into()));
    }

    if !failed_conditions.is_empty() {
        let list_rep = PyRep::List(failed_conditions);
        let notif_args = vec![list_rep.clone()];
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnSafeLogoffFailed",
            "charid",
            notif_args.clone(),
        );
        session.push_notification("OnSafeLogoffFailed", notif_args);
        return Ok(list_rep);
    }

    let complete_at_unix = now_unix.saturating_add(SAFE_LOGOFF_COUNTDOWN_SECS);
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.disconnected_ships.remove(&char_id);
        st.safe_logoffs.insert(
            char_id,
            SafeLogoffEntry {
                char_id,
                ship_id,
                solar_system_id: sys_id,
                started_unix: now_unix,
                complete_at_unix,
                completed: false,
            },
        );
    });

    let safe_logoff_ft = unix_to_filetime(complete_at_unix);
    let started_args = vec![PyRep::Integer(safe_logoff_ft)];
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSafeLogoffTimerStarted",
        "charid",
        started_args.clone(),
    );
    session.push_notification("OnSafeLogoffTimerStarted", started_args);

    Ok(PyRep::List(vec![]))
}

/// Aborts an active safe-logoff countdown on explicit request (`ship.AbortSafeLogoff`)
/// and returns `PyRep::Bool(true)` (`plan:SP-6`).
pub fn handle_abort_safe_logoff(
    ctx: &ServerContext,
    session: &mut SessionState,
    reason_code: &str,
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.safe_logoffs.remove(&char_id);
    });
    let abort_args = vec![PyRep::String(reason_code.to_string())];
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnSafeLogoffAborted",
        "charid",
        abort_args.clone(),
    );
    session.push_notification("OnSafeLogoffAborted", abort_args);
    Ok(PyRep::Bool(true))
}

/// Aborts an active safe-logoff countdown when the pilot moves, warps, activates a module, or takes/deals aggression (`plan:SP-6`).
pub fn maybe_abort_safe_logoff_on_action(
    ctx: &ServerContext,
    char_id: i64,
    reason_code: &str,
) -> bool {
    let aborted = with_mut_sp6_state(&ctx.db, |st, _snap| {
        if let Some(entry) = st.safe_logoffs.get(&char_id) {
            if !entry.completed {
                st.safe_logoffs.remove(&char_id);
                return true;
            }
        }
        false
    });
    if aborted {
        ctx.notify(
            NotificationTarget::Character(char_id),
            "OnSafeLogoffAborted",
            "charid",
            vec![PyRep::String(reason_code.to_string())],
        );
    }
    aborted
}

/// Computes a deterministic 1,000,000 km (`1e9 m`) emergency-warp destination vector in deep space.
fn compute_emergency_warp_destination(current_pos: DVec3, char_id: i64, ship_id: i64) -> DVec3 {
    let seed = (char_id as u64)
        .wrapping_mul(6364136223846793005)
        .wrapping_add(ship_id as u64)
        .wrapping_add(1442695040888963407);
    let theta = ((seed & 0xFFFF) as f64 / 65535.0) * std::f64::consts::TAU;
    let z_frac = (((seed >> 16) & 0xFFFF) as f64 / 65535.0) * 0.4 - 0.2;
    let r_xy = (1.0 - z_frac * z_frac).sqrt();
    let dir = DVec3::new(r_xy * theta.cos(), z_frac, r_xy * theta.sin()).normalize_or_zero();
    let unit = if dir.length_squared() > 0.5 {
        dir
    } else {
        DVec3::X
    };
    current_pos + unit * EMERGENCY_WARP_DISTANCE_M
}

/// Abandons all active in-space drones and fighters controlled by `char_id` or `ship_id` (`feat:lifecycle.drones-leave-grid-abandon`, `feat:lifecycle.fighters-left-in-space`).
pub fn abandon_drones_and_fighters_for_ship_sync(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
) {
    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
        for d in rt.active_drones_in_space.values_mut() {
            if (d.owner_char_id == char_id || d.parent_ship_id == ship_id) && !d.is_abandoned {
                d.is_abandoned = true;
                d.owner_char_id = 0;
                d.parent_ship_id = 0;
                d.target_id = None;
            }
        }
    });
    let mut abandoned_fighter_items = Vec::new();
    if let Ok(mut fighters) = ctx.active_fighters.write() {
        for f in fighters.values_mut() {
            if (f.owner_char_id == char_id || f.parent_ship_id == ship_id) && !f.is_abandoned {
                f.is_abandoned = true;
                f.target_id = None;
                abandoned_fighter_items.push(f.item_id);
            }
        }
    }
    if !abandoned_fighter_items.is_empty() {
        let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
            for iid in abandoned_fighter_items {
                if let Some(it) = snap.items.get_mut(&iid) {
                    it.location_id = sys_id as i64;
                    it.flag = InventoryFlag::None;
                    it.version += 1;
                }
            }
            Ok(())
        });
    }
}

/// Handles ungraceful logoff (`authentication.Logout`, `userSvc.UserLogOffCharacter`) or dropped socket
/// connection while in space (`plan:SP-6`):
/// - Initiates a single emergency warp to ~1,000,000 km unless the ship is warp-scrambled or criminal in high-sec.
/// - Keeps the ship ball in the `Ballpark` for the active log-off timer (`60s` unaggressed/weapons, `300s` NPC, `900s` PvP/Criminal).
/// - Persists `DisconnectedShipEntry` in SQLite so the ship remains vulnerable until `remove_at_unix` and
///   logs back into space at its final position on next login.
pub async fn handle_character_logoff_or_disconnect(
    ctx: &ServerContext,
    char_id: i64,
) -> Option<DisconnectedShipEntry> {
    let now_unix = ctx.now_unix();

    // If the pilot already completed a 30s safe logoff, their ship was already cleanly removed.
    let already_safely_logged_off = with_mut_sp6_state(&ctx.db, |st, _snap| {
        if let Some(entry) = st.safe_logoffs.get(&char_id) {
            if entry.completed {
                return true;
            }
            st.safe_logoffs.remove(&char_id);
        }
        false
    });
    if already_safely_logged_off {
        return None;
    }

    let char_info = ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id)?;
        if ch.station_id.is_some() || ch.structure_id.is_some() {
            return None;
        }
        let ship_id = ch.active_ship_id;
        let ship_type_id = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(606);
        let saved_pos = snap
            .item_positions
            .get(&ship_id)
            .copied()
            .unwrap_or([0.0, 0.0, 0.0]);
        Some((ch.solar_system_id, ship_id, ship_type_id, saved_pos))
    })?;

    let (sys_id, ship_id, ship_type_id, saved_pos) = char_info;
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let (group_id, category_id, mass, radius, max_vel, inertia, warp_speed_au_s) =
        sde_ship_physics(&ctx.sde, ship_type_id);

    // Ensure the ship ball exists in the ballpark if it wasn't spawned yet.
    let current_pos = if let Some(pos) = sys_handle.get_ball_position_sync(ship_id) {
        pos
    } else {
        let pos = DVec3::from_array(saved_pos);
        let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
        let (corp_id, alliance_id, sec_status) = ctx.db.with_snapshot(|snap| {
            snap.characters
                .get(&char_id)
                .map(|c| {
                    (
                        c.corporation_id,
                        c.alliance_id.unwrap_or(-1),
                        c.security_status,
                    )
                })
                .unwrap_or((1_000_009, -1, 0.0))
        });
        let _ = sys_handle
            .spawn_ball_with_meta(
                Ball {
                    id: ship_id,
                    type_id: ship_type_id,
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
                    owner_id: char_id,
                    char_id: Some(char_id),
                    corp_id,
                    alliance_id,
                    security_status: sec_status,
                    is_cloaked: false,
                },
            )
            .await;
        pos
    };

    if let Ok(Some(cs)) = sys_handle.get_combat_state(ship_id).await {
        persist_ship_combat_hp_in_db(&ctx.db, ship_id, &cs);
    }
    abandon_drones_and_fighters_for_ship_sync(ctx, char_id, ship_id, sys_id);

    let (weap_until, npc_until, pvp_until, crim_until) =
        active_timers_for_char(ctx, char_id, now_unix);
    let remove_at_unix = pvp_until
        .max(crim_until)
        .max(npc_until)
        .max(weap_until)
        .max(now_unix.saturating_add(UNAGGRESSED_LOGOFF_REMOVE_SECS));

    let sys_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.5);
    let in_abyssal = (32_000_000..=32_999_999).contains(&sys_id);
    let is_scrambled = in_abyssal
        || sys_handle.is_ball_scrambled(ship_id).await.unwrap_or(false)
        || (sys_sec >= 0.45 && crim_until > now_unix);

    // Emergency warp is prohibited while warp-scrambled, in Abyssal space, as a highsec criminal,
    // or under an active Weapons timer (`feat:account.logout`, `plan:SP-6`).
    let emergency_warp_dest = if !is_scrambled && weap_until <= now_unix {
        let dest = compute_emergency_warp_destination(current_pos, char_id, ship_id);
        let _ = sys_handle
            .set_movement_mode(ship_id, MovementMode::new_warp(current_pos, dest, warp_speed_au_s))
            .await;
        Some([dest.x, dest.y, dest.z])
    } else {
        None
    };

    let entry = DisconnectedShipEntry {
        char_id,
        ship_id,
        ship_type_id,
        solar_system_id: sys_id,
        disconnected_at_unix: now_unix,
        remove_at_unix,
        emergency_warp_dest,
        last_position: [current_pos.x, current_pos.y, current_pos.z],
        removed_from_space: false,
    };

    with_mut_sp6_state(&ctx.db, |st, snap| {
        snap.item_positions
            .insert(ship_id, [current_pos.x, current_pos.y, current_pos.z]);
        if let Some(ch) = snap.characters.get_mut(&char_id) {
            ch.station_id = None;
            ch.structure_id = None;
            ch.solar_system_id = sys_id;
        }
        st.disconnected_ships.insert(char_id, entry.clone());
    });

    Some(entry)
}

/// Synchronously reconnects a character who logged off or disconnected in space (`plan:SP-6`).
pub fn reconnect_character_in_space_sync(
    ctx: &ServerContext,
    char_id: i64,
) -> Option<(u32, i64, DVec3)> {
    let now_unix = ctx.now_unix();
    cluster_tick::on_advance_secs(ctx, now_unix, 0);
    cluster_tick::collapse_stranded_abyssal_if_needed_sync(ctx, char_id, now_unix);

    let (sys_id, ship_id, ship_type_id, pos_arr, had_live_disc, corp_id, alliance_id, sec_status) =
        with_mut_sp6_state(&ctx.db, |st, snap| {
            let ch = snap.characters.get(&char_id)?;
            if ch.station_id.is_some() || ch.structure_id.is_some() {
                st.disconnected_ships.remove(&char_id);
                st.safe_logoffs.remove(&char_id);
                return None;
            }
            let sys_id = ch.solar_system_id;
            let ship_id = ch.active_ship_id;
            let ship_type_id = snap.items.get(&ship_id).map(|i| i.type_id).unwrap_or(606);
            let corp_id = ch.corporation_id;
            let alliance_id = ch.alliance_id.unwrap_or(-1);
            let sec_status = ch.security_status;

            let disc_entry = st
                .disconnected_ships
                .remove(&char_id)
                .filter(|d| d.ship_id == ship_id && snap.items.contains_key(&d.ship_id));
            let disc_pos = disc_entry.map(|d| d.last_position);
            st.safe_logoffs.remove(&char_id);

            let pos_arr = disc_pos
                .or_else(|| snap.item_positions.get(&ship_id).copied())
                .unwrap_or([0.0, 0.0, 0.0]);
            snap.item_positions.insert(ship_id, pos_arr);
            if let Some(ship_item) = snap.items.get_mut(&ship_id) {
                ship_item.location_id = sys_id as i64;
                ship_item.flag = InventoryFlag::None;
            }
            Some((
                sys_id,
                ship_id,
                ship_type_id,
                pos_arr,
                disc_pos.is_some(),
                corp_id,
                alliance_id,
                sec_status,
            ))
        })?;

    let sys_handle = ctx.systems.get_or_spawn(sys_id);

    // If the character's ship is already active in space, preserve the live ball position,
    // capacitor, tackle warp scrambles, and combat status instead of resetting to origin or defaults.
    if let Some(existing_pos) = sys_handle.get_ball_position_sync(ship_id) {
        let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
            snap.item_positions
                .insert(ship_id, [existing_pos.x, existing_pos.y, existing_pos.z]);
            Ok(())
        });
        return Some((sys_id, ship_id, existing_pos));
    }

    let final_pos = if had_live_disc {
        sys_handle
            .get_ball_position_sync(ship_id)
            .unwrap_or_else(|| DVec3::from_array(pos_arr))
    } else {
        DVec3::from_array(pos_arr)
    };
    let _ = ctx.db.try_with_mut_snapshot(|snap| -> Result<(), String> {
        snap.item_positions
            .insert(ship_id, [final_pos.x, final_pos.y, final_pos.z]);
        Ok(())
    });

    // Remove any existing ball for `ship_id` so there are never duplicate ghost balls.
    sys_handle.try_remove_ball(ship_id);

    let cs = ctx.build_ship_combat_state(char_id, ship_id, ship_type_id);
    let (group_id, category_id, mass, radius, max_vel, inertia, _warp_speed) =
        sde_ship_physics(&ctx.sde, ship_type_id);

    sys_handle.try_spawn_ball_with_meta(
        Ball {
            id: ship_id,
            type_id: ship_type_id,
            position: final_pos,
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
            owner_id: char_id,
            char_id: Some(char_id),
            corp_id,
            alliance_id,
            security_status: sec_status,
            is_cloaked: false,
        },
    );

    Some((sys_id, ship_id, final_pos))
}

/// Reconnects a character who logged off or disconnected in space (`plan:SP-6`).
pub async fn reconnect_character_in_space(
    ctx: &ServerContext,
    char_id: i64,
) -> Option<(u32, i64, DVec3)> {
    reconnect_character_in_space_sync(ctx, char_id)
}

/// Persists a started 120s self-destruct countdown so it detonates via server timer at `T+120s`
/// even if the client disconnects or never calls again (`feat:lifecycle.self-destruct-server-timer`).
pub fn register_self_destruct_countdown(
    ctx: &ServerContext,
    ship_id: i64,
    char_id: i64,
    solar_system_id: u32,
    started_unix: u64,
    detonate_at_unix: u64,
) {
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.self_destructs.insert(
            ship_id,
            SelfDestructPersistedEntry {
                ship_id,
                char_id,
                solar_system_id,
                started_unix,
                detonate_at_unix,
            },
        );
    });
}

pub fn abort_self_destruct_countdown(ctx: &ServerContext, ship_id: i64) -> bool {
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.self_destructs.remove(&ship_id).is_some()
    })
}

/// Persists an active Abyssal Deadspace pocket session (`20m` = `1200s` collapse timer)
/// so the server collapses the pocket and destroys the ship + pod even if the pilot disconnects
/// (`feat:lifecycle.abyssal-disconnect`).
pub fn register_abyssal_session(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    pocket_system_id: u32,
    origin_system_id: u32,
    entered_unix: u64,
    collapse_at_unix: u64,
) {
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.abyssal_sessions.insert(
            char_id,
            AbyssalPersistedSession {
                char_id,
                ship_id,
                pocket_system_id,
                origin_system_id,
                entered_unix,
                collapse_at_unix,
            },
        );
    });
}

pub fn clear_abyssal_session(ctx: &ServerContext, char_id: i64) {
    with_mut_sp6_state(&ctx.db, |st, _snap| {
        st.abyssal_sessions.remove(&char_id);
    });
}
