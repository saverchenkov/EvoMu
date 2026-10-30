//! `security_mgr` — System security status modifications and queries (`securityMgr.get_modified_systems`),
//! plus authoritative helpers for published Pirate Insurgency Corruption & Suppression effects.
//!
//! Ground truth (`docs/eve-wiki/Corruption_and_Suppression.md`, `docs/eve-wiki/Insurgency.md`):
//! - `securityMgr.get_modified_systems` reports manual runtime security overrides and Corruption Stage 5
//!   (`LAWLESS`, `0.0` security override in highsec/lowsec).
//! - Per-stage Corruption (1–5) & Suppression (1–5) effects are applied directly to their real subsystems:
//!   - Corruption 1–5: `+7.5%/stage` PvP loot drop bonus for pirate militia (`+7.5%..+37.5%`).
//!   - Corruption 2+: Faction Police suppressed; roaming Pirate NPCs spawn.
//!   - Corruption 3+: Insurgency spreads across stargates; `IceRefineryHeist` spawns at Corruption `3..=4`.
//!   - Corruption 4+: `+2 AU/s` warp speed for pirate militia in-system.
//!   - Corruption 5: Highsec CONCORD ignores ship aggression (still responds to pods & criminal assists);
//!     lowsec gate/station sentries stop firing and interdiction bubbles/bombs are permitted.
//!   - Suppression 1–5: `+5%/stage` NPC bounty & security status gain (`+5%..+25%`).
//!   - Suppression 2–5: `+5%/stage` FW LP payout for Empire militia (`+5%..+20%`).
//!   - Suppression 3+: `-30%` corruption gain; roaming Enforcer NPCs spawn.
//!   - Suppression 4+: `+10%` Stasis Webifier & Warp Scrambler optimal range for Empire militia.
//!   - Suppression 5: Blocks stargate spread out of system, caps corruption at `< 800` pts (Stage 4),
//!     and deploys Counter-Insurgency Sentry Guns (`type_id = 79507`) that persist until destroyed.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::with_combat_runtime;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use std::collections::HashMap;

/// Returns true if `faction_id` is Guristas (`500010`) or Angel Cartel (`500011`).
pub fn is_pirate_militia_faction(faction_id: i64) -> bool {
    matches!(faction_id, 500_010 | 500_011)
}

/// Returns true if `faction_id` is one of the 4 Empire FW militias (`500001..=500004`).
pub fn is_empire_militia_faction(faction_id: i64) -> bool {
    matches!(faction_id, 500_001..=500_004)
}

/// Returns true if `sys_id` is one of the 4 Empire FW War HQ solar systems
/// (`Mehatoor`, `Onnamon`, `Intaki`, `Amo`).
pub fn is_empire_war_hq_system(sys_id: u32) -> bool {
    matches!(sys_id, 30_002_974 | 30_045_324 | 30_003_788 | 30_002_055)
}

/// Computes the mapping of solar system IDs to their modified security status (`f64`).
///
/// Only manual overrides (`CombatRuntimeState.modified_system_securities`) and Corruption Stage 5
/// (`LAWLESS`, `0.0` effective security) modify system security status.
pub fn compute_modified_systems(ctx: &ServerContext) -> HashMap<u32, f64> {
    let mut sec_map: HashMap<u32, f64> = with_combat_runtime(ctx, |rt| {
        rt.modified_system_securities.clone()
    });

    ctx.db.with_snapshot(|snap| {
        for (&sid, st) in &snap.system_states {
            if sec_map.contains_key(&sid) {
                continue;
            }
            if st.insurgency_corruption_stage >= 5 {
                sec_map.insert(sid, 0.0);
            }
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

/// Published Corruption Stage 1–5 PvP loot drop probability (`0.50` base + `0.075 * stage` for pirate militia killers).
pub fn insurgency_pvp_loot_drop_threshold(
    ctx: &ServerContext,
    sys_id: u32,
    attacker_char_id: i64,
    victim_char_id: i64,
    is_self_destruct: bool,
) -> f64 {
    let is_pochven = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .is_some_and(|s| s.region_id == 10_000_070);
    let base = if is_pochven { 0.60 } else { 0.50 };
    if is_self_destruct || attacker_char_id <= 0 || attacker_char_id == victim_char_id {
        return base;
    }
    let c_stage = crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, sys_id);
    if c_stage == 0 {
        return base;
    }
    let attacker_pirate = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&attacker_char_id)
            .and_then(|c| c.war_faction_id)
            .is_some_and(is_pirate_militia_faction)
    });
    if attacker_pirate {
        (base + 0.075 * f64::from(c_stage.min(5))).clamp(0.0, 1.0)
    } else {
        base
    }
}

/// Published Corruption Stage 4–5 `+2.0 AU/s` warp speed bonus for pirate militia members in `sys_id`.
pub fn insurgency_pirate_warp_bonus_au_s(ctx: &ServerContext, char_id: i64, sys_id: u32) -> f64 {
    if char_id <= 0 {
        return 0.0;
    }
    let c_stage = crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, sys_id);
    if c_stage < 4 {
        return 0.0;
    }
    let is_pirate = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.war_faction_id)
            .is_some_and(is_pirate_militia_faction)
    });
    if is_pirate { 2.0 } else { 0.0 }
}

/// Published Suppression Stage 1–5 `+5%/stage` multiplier for NPC bounties and security status gain (`1.05..=1.25`).
pub fn insurgency_suppression_bounty_and_sec_multiplier(ctx: &ServerContext, sys_id: u32) -> f64 {
    let s_stage = insurgency_system_suppression_stage(ctx, sys_id);
    1.0 + 0.05 * f64::from(s_stage.min(5))
}

/// Published Suppression Stage 2–5 `+5%/stage` FW LP multiplier for Empire militia (`1.05..=1.20` at stages `2..=5`).
pub fn insurgency_suppression_empire_lp_multiplier(ctx: &ServerContext, sys_id: u32, faction_id: i64) -> f64 {
    if !is_empire_militia_faction(faction_id) {
        return 1.0;
    }
    let s_stage = insurgency_system_suppression_stage(ctx, sys_id);
    if s_stage >= 2 {
        1.0 + 0.05 * f64::from(s_stage.min(5) - 1)
    } else {
        1.0
    }
}

/// Published Suppression Stage 4–5 `+10%` Stasis Webifier & Warp Scrambler optimal range multiplier for Empire militia.
pub fn insurgency_suppression_web_scram_range_multiplier(ctx: &ServerContext, char_id: i64, sys_id: u32) -> f64 {
    if char_id <= 0 {
        return 1.0;
    }
    let s_stage = insurgency_system_suppression_stage(ctx, sys_id);
    if s_stage < 4 {
        return 1.0;
    }
    let is_empire = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .and_then(|c| c.war_faction_id)
            .is_some_and(is_empire_militia_faction)
    });
    if is_empire { 1.10 } else { 1.0 }
}

/// Returns true if `sys_id` is at Suppression Stage 5 or has live Counter-Insurgency Sentry Guns (`79507`) in space.
pub fn has_active_counter_insurgency_sentries(ctx: &ServerContext, sys_id: u32) -> bool {
    insurgency_system_suppression_stage(ctx, sys_id) >= 5
        || with_combat_runtime(ctx, |rt| {
            rt.npc_entities
                .values()
                .any(|n| n.solar_system_id == sys_id && n.type_id == 79_507)
        })
}

/// Prunes stage-gated insurgency sites in `snap` when system corruption stages drop (`docs/eve-wiki/Insurgency.md:77-83`).
pub fn prune_ineligible_insurgency_sites_in_snap(snap: &mut eve_db::WorldStateSnapshot, sys_id: u32) {
    let c_st = snap.system_states.get(&sys_id).map_or(0, |st| st.insurgency_corruption_stage);
    let is_fob_sys = snap
        .insurgency_runtime
        .campaigns
        .values()
        .any(|c| c.fob_solar_system_id == sys_id);
    snap.insurgency_runtime.insurgency_instances.retain(|site| {
        if site.solar_system_id != sys_id || site.site_type == "PirateStronghold" {
            return true;
        }
        match site.site_type.as_str() {
            "CorporateOutpostRaid" => true,
            "MiningOpAmbush" => c_st >= 1,
            "IceRefineryHeist" => (3..=4).contains(&c_st),
            "PirateLaunderingFacility" => is_fob_sys,
            "CounterInsurgencyOutpost" => is_empire_war_hq_system(sys_id),
            _ => true,
        }
    });
}
