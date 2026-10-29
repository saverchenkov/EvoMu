//! `security_mgr` — System security status modifications and queries (`securityMgr.get_modified_systems`).
//!
//! Provides authoritative calculation of effective and modified system security statuses
//! reflecting live Pirate Insurgency Corruption (stages 1–5) and Suppression (stages 1–5),
//! as well as manual runtime overrides.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::with_combat_runtime;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// Computes the mapping of solar system IDs to their modified security status (`f64`).
///
/// Published system effects:
/// - Manual overrides from `CombatRuntimeState.modified_system_securities` take highest precedence.
/// - Pirate Insurgency Corruption:
///   - Stage 1: security degradation (-0.02)
///   - Stage 2: security degradation (-0.05) & Faction Police suppressed
///   - Stage 3: security degradation (-0.10) & Sentry Guns offline
///   - Stage 4: security degradation (-0.15) & Pirate warp bonus (+2 AU/s)
///   - Stage 5: lawless nullsec security override (`0.0`), CONCORD withdrawal in highsec, bubbles/bombs in lowsec.
/// - Pirate Insurgency Suppression:
///   - Stage 1: security reinforcement (+0.02)
///   - Stage 2: security reinforcement (+0.04) & enhanced sentry tracking
///   - Stage 3: security reinforcement (+0.06) & corruption gain reduced by 30%
///   - Stage 4: security reinforcement (+0.08) & +10% scram/web range for empire militia
///   - Stage 5: security reinforcement (+0.10, capped at 1.0), counter-insurgency sentry guns deployed, corruption capped.
///
/// Effects are dynamically removed when the stage drops to 0.
pub fn compute_modified_systems(ctx: &ServerContext) -> HashMap<u32, f64> {
    let mut sec_map: HashMap<u32, f64> = with_combat_runtime(ctx, |rt| {
        rt.modified_system_securities.clone()
    });

    ctx.db.with_snapshot(|snap| {
        for (&sid, st) in &snap.system_states {
            if sec_map.contains_key(&sid) {
                continue;
            }
            let c_stage = st.insurgency_corruption_stage;
            let s_stage = st.insurgency_suppression_stage;
            if c_stage == 0 && s_stage == 0 {
                continue;
            }
            if c_stage >= 5 {
                sec_map.insert(sid, 0.0);
                continue;
            }
            let base_sec = ctx
                .sde
                .solar_systems
                .get(&sid)
                .map(|s| s.security_status)
                .unwrap_or(0.0);
            let mut mod_sec = base_sec;
            match c_stage {
                1 => mod_sec -= 0.02,
                2 => mod_sec -= 0.05,
                3 => mod_sec -= 0.10,
                4 => mod_sec -= 0.15,
                _ => {}
            }
            match s_stage {
                1 => mod_sec += 0.02,
                2 => mod_sec += 0.04,
                3 => mod_sec += 0.06,
                4 => mod_sec += 0.08,
                5 => mod_sec += 0.10,
                _ => {}
            }
            let rounded = ((mod_sec.clamp(0.0, 1.0) * 10_000.0).round()) / 10_000.0;
            sec_map.insert(sid, rounded);
        }
    });

    sec_map
}

/// Returns the effective security status for a solar system, accounting for dynamic modifications.
pub fn get_effective_system_security(ctx: &ServerContext, solar_system_id: u32) -> f64 {
    let mods = compute_modified_systems(ctx);
    if let Some(&sec) = mods.get(&solar_system_id) {
        return sec;
    }
    ctx.sde
        .solar_systems
        .get(&solar_system_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0)
}

/// Handles `securityMgr` RPC calls from the game client.
pub fn handle_security_mgr_call(
    ctx: &ServerContext,
    session: &SessionState,
    method: &str,
    _args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    match ("securityMgr", method) {
        ("securityMgr", "get_modified_systems") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let sec_map = compute_modified_systems(ctx);
            let entries = sec_map
                .into_iter()
                .map(|(sid, sec)| (PyRep::Integer(sid as i64), PyRep::Real(sec)))
                .collect();
            Some(Ok(PyRep::Dict(entries)))
        }
        _ => None,
    }
}

/// Returns the active Insurgency suppression stage (`0..=5`) for `sys_id`, or `0` outside active/winddown campaigns.
pub fn insurgency_system_suppression_stage(ctx: &ServerContext, sys_id: u32) -> u8 {
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        let mut stage = 0u8;
        let mut covered = false;
        for c in snap.insurgency_runtime.campaigns.values() {
            if c.systems.contains(&sys_id) && now >= c.forecasting_end_unix && now < c.winddown_end_unix {
                covered = true;
                stage = stage.max(crate::services::insurgency_services::insurgency_stage(
                    c.suppression_points.get(&sys_id).copied().unwrap_or(0),
                ));
            }
        }
        if covered {
            stage.max(snap.system_states.get(&sys_id).map_or(0, |st| st.insurgency_suppression_stage))
        } else {
            0
        }
    })
}

/// Prunes stage-gated insurgency sites in `snap` when system corruption or suppression stages drop.
pub fn prune_ineligible_insurgency_sites_in_snap(snap: &mut eve_db::WorldStateSnapshot, sys_id: u32) {
    let c_st = snap.system_states.get(&sys_id).map_or(0, |st| st.insurgency_corruption_stage);
    let s_st = snap.system_states.get(&sys_id).map_or(0, |st| st.insurgency_suppression_stage);
    snap.insurgency_runtime.insurgency_instances.retain(|site| {
        if site.solar_system_id != sys_id || site.site_type == "PirateStronghold" {
            return true;
        }
        match site.site_type.as_str() {
            "PirateMiningAmbush" => c_st >= 1,
            "IceHeist" => c_st >= 2,
            "LawlessOutpost" => c_st >= 3,
            "PirateRoamer" => c_st >= 4,
            "LawEnforcementPatrol" => s_st >= 1,
            "CounterInsurgencyOutpost" => s_st >= 2,
            "AntiPiratePatrol" => s_st >= 3,
            "HeavyLawPatrol" => s_st >= 4,
            "CounterInsurgencySentry" => s_st >= 5,
            _ => true,
        }
    });
}
