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
use crate::services::combat_crimewatch::{with_combat_runtime, with_mut_combat_runtime};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
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
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
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

/// Snapshot-local variant of [`insurgency_system_suppression_stage`] for callers already holding `ctx.db`.
pub fn insurgency_system_suppression_stage_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    now: u64,
    sys_id: u32,
) -> u8 {
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
}

/// Returns the active Insurgency suppression stage (`0..=5`) for `sys_id`, or `0` outside active/winddown campaigns.
pub fn insurgency_system_suppression_stage(ctx: &ServerContext, sys_id: u32) -> u8 {
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| insurgency_system_suppression_stage_in_snap(snap, now, sys_id))
}

/// Emits `OnSecurityModified` when a system enters or exits Corruption Stage 5 (`LAWLESS`, `0.0` effective security)
/// unless overridden in `CombatRuntimeState.modified_system_securities`.
pub fn sync_corruption_security_status_change(
    ctx: &ServerContext,
    sys_id: u32,
    old_stage: u8,
    new_stage: u8,
) {
    let was_lawless = old_stage >= 5;
    let is_lawless = new_stage >= 5;
    if was_lawless == is_lawless {
        return;
    }
    let has_override = with_mut_combat_runtime(ctx, |rt| {
        if is_lawless {
            for ((sid, _), st) in &mut rt.sentry_gun_aggression {
                if *sid == sys_id {
                    st.active_on_grid = false;
                }
            }
        }
        rt.modified_system_securities.contains_key(&sys_id)
    });
    if has_override {
        return;
    }
    let new_sec = if is_lawless {
        0.0
    } else {
        ctx.sde
            .solar_systems
            .get(&sys_id)
            .map(|s| s.security_status)
            .unwrap_or(0.0)
    };
    crate::services::combat_crimewatch::crimewatch_timers::update_system_security_status(ctx, sys_id, new_sec);
}

/// Returns true if `char_id` is enlisted in the pirate militia leading an active/winddown insurgency in `sys_id`
/// (returning `false` if no active/winddown campaign covers `sys_id`).
pub fn is_pilot_aligned_with_insurgency_pirate_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    now: u64,
    sys_id: u32,
    char_id: i64,
) -> bool {
    let Some(wf) = snap.characters.get(&char_id).and_then(|c| c.war_faction_id) else {
        return false;
    };
    if !is_pirate_militia_faction(wf) {
        return false;
    }
    let mut any_covering = false;
    let mut matches_pirate = false;
    for c in snap.insurgency_runtime.campaigns.values() {
        if c.systems.contains(&sys_id) && now >= c.forecasting_end_unix && now < c.winddown_end_unix {
            any_covering = true;
            if c.pirate_faction_id == wf {
                matches_pirate = true;
            }
        }
    }
    if any_covering { matches_pirate } else { false }
}

/// Returns true if `char_id` is aligned with the leading pirate faction of the active insurgency in `sys_id`.
pub fn is_pilot_aligned_with_insurgency_pirate(ctx: &ServerContext, sys_id: u32, char_id: i64) -> bool {
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| is_pilot_aligned_with_insurgency_pirate_in_snap(snap, now, sys_id, char_id))
}

/// Published Corruption Stage 1–5 PvP loot drop probability (`0.50` base + `0.075 * stage` for aligned pirate militia killers).
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
    if is_self_destruct || attacker_char_id <= 0 || victim_char_id <= 0 || attacker_char_id == victim_char_id {
        return base;
    }
    let c_stage = crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, sys_id);
    if c_stage == 0 {
        return base;
    }
    if is_pilot_aligned_with_insurgency_pirate(ctx, sys_id, attacker_char_id) {
        (base + 0.075 * f64::from(c_stage.min(5))).clamp(0.0, 1.0)
    } else {
        base
    }
}

/// Published Corruption Stage 4–5 `+2.0 AU/s` warp speed bonus for aligned pirate militia members in `sys_id`.
pub fn insurgency_pirate_warp_bonus_au_s(ctx: &ServerContext, char_id: i64, sys_id: u32) -> f64 {
    if char_id <= 0 {
        return 0.0;
    }
    let c_stage = crate::services::insurgency_services::insurgency_system_corruption_stage(ctx, sys_id);
    if c_stage < 4 {
        return 0.0;
    }
    if is_pilot_aligned_with_insurgency_pirate(ctx, sys_id, char_id) { 2.0 } else { 0.0 }
}

/// Published Suppression Stage 1–5 `+5%/stage` multiplier for NPC bounties and security status gain (`1.05..=1.25`).
pub fn insurgency_suppression_bounty_and_sec_multiplier(ctx: &ServerContext, sys_id: u32) -> f64 {
    let s_stage = insurgency_system_suppression_stage(ctx, sys_id);
    1.0 + 0.05 * f64::from(s_stage.min(5))
}

/// Snapshot-local variant of [`insurgency_suppression_empire_lp_multiplier`] for callers already holding `ctx.db`.
pub fn insurgency_suppression_empire_lp_multiplier_in_snap(
    snap: &eve_db::WorldStateSnapshot,
    now_unix: u64,
    sys_id: u32,
    faction_id: i64,
) -> f64 {
    if !is_empire_militia_faction(faction_id) {
        return 1.0;
    }
    let s_stage = insurgency_system_suppression_stage_in_snap(snap, now_unix, sys_id);
    if s_stage >= 2 {
        1.0 + 0.05 * f64::from(s_stage.min(5) - 1)
    } else {
        1.0
    }
}

/// Published Suppression Stage 2–5 `+5%/stage` FW LP multiplier for Empire militia (`1.05..=1.20` at stages `2..=5`).
pub fn insurgency_suppression_empire_lp_multiplier(ctx: &ServerContext, sys_id: u32, faction_id: i64) -> f64 {
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        insurgency_suppression_empire_lp_multiplier_in_snap(snap, now, sys_id, faction_id)
    })
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

/// Returns true if `sys_id` currently has at least one live Counter-Insurgency Sentry Gun (`type_id = 79507`) ball in space.
pub fn has_active_counter_insurgency_sentries(ctx: &ServerContext, sys_id: u32) -> bool {
    let bids: Vec<i64> = with_combat_runtime(ctx, |rt| {
        rt.npc_entities
            .values()
            .filter(|n| n.solar_system_id == sys_id && n.type_id == 79_507)
            .map(|n| n.ball_id)
            .collect()
    });
    if bids.is_empty() {
        return false;
    }
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let bp = sys_handle.ballpark.read().unwrap_or_else(|e| e.into_inner());
    bids.into_iter().any(|bid| {
        bp.balls.contains_key(&bid)
            && bp.combat_states.get(&bid).is_none_or(|cs| cs.structure_hp > 0.0)
    })
}

/// Applies SDE-driven Target Painter (`250 km`, `+50%` sig) and ECM (`75 km`, `5.0 s`) from a live `79_507` sentry ball.
pub fn apply_counter_insurgency_sentry_ewar(
    sde: &eve_sde::SdeRegistry,
    bp: &mut eve_destiny::Ballpark,
    target_id: i64,
    target_radius: f64,
    dist: f64,
) {
    let ci_type = sde.types.get(&79_507);
    let attr = |aid: u16, def: f64| ci_type.and_then(|t| t.default_attributes.get(&aid).copied()).unwrap_or(def);
    let tp_range = attr(2524, 250_000.0);
    let tp_sig_pct = attr(554, 50.0);
    let ecm_range = attr(2532, 75_000.0);
    let ecm_dur_s = attr(2822, 5_000.0) / 1_000.0;
    if dist <= tp_range {
        if let Some(t_cs) = bp.combat_states.get_mut(&target_id) {
            if t_cs.base_signature_radius.is_none() {
                t_cs.base_signature_radius = Some(if t_cs.signature_radius > 0.0 {
                    t_cs.signature_radius
                } else {
                    target_radius.max(35.0)
                });
            }
            t_cs.tp_sig_multiplier = Some(1.0 + tp_sig_pct / 100.0);
        }
        bp.recompute_ball_attributes(target_id);
    }
    if dist <= ecm_range && ecm_dur_s > 0.0 {
        bp.apply_ecm_jam(target_id, ecm_dur_s);
        bp.clear_target_locks(target_id);
    }
}

/// Resolves an authentic SDE dungeon template name from `sde.dungeons` for Pirate Insurgency archetypes.
pub fn sde_insurgency_dungeon_name(
    sde: &eve_sde::SdeRegistry,
    archetype_ids: &[u32],
    faction_id: u32,
    fallback: &str,
) -> String {
    let mut matching: Vec<&eve_sde::DungeonTemplate> = sde
        .dungeons
        .values()
        .filter(|d| d.archetype_id.is_some_and(|a| archetype_ids.contains(&a)))
        .collect();
    matching.sort_by_key(|d| (if d.faction_id == Some(faction_id) { 0u8 } else { 1u8 }, d.dungeon_id));
    matching
        .first()
        .map(|d| d.name.clone())
        .unwrap_or_else(|| fallback.to_string())
}

/// Synchronizes stage-gated Pirate Insurgency dungeon & Ice Heist instances in `snap` for `sys_id`.
pub fn sync_stage_gated_insurgency_sites_in_snap(
    sde: &eve_sde::SdeRegistry,
    snap: &mut eve_db::WorldStateSnapshot,
    sys_id: u32,
    now: u64,
) {
    let covering: Vec<(i64, u32)> = snap
        .insurgency_runtime
        .campaigns
        .values()
        .filter(|c| c.systems.contains(&sys_id) && now >= c.forecasting_end_unix && now < c.winddown_end_unix)
        .map(|c| (c.pirate_faction_id, c.fob_solar_system_id))
        .collect();

    if let Some(&(pirate_fac, fob_sys)) = covering.first() {
        let beacon_pos = {
            let celestials = sde.celestials_in_system(sys_id);
            if let Some(c) = celestials.iter().find(|c| c.group_id == 8).or_else(|| celestials.last()) {
                [c.position.x + c.radius.max(0.0) + 15_000.0, c.position.y, c.position.z]
            } else {
                [250_000.0, 0.0, -150_000.0]
            }
        };
        let c_st = {
            let st = snap.system_states.entry(sys_id).or_default();
            st.solar_system_id = sys_id;
            if st.fw_complex_position.is_none() {
                st.fw_complex_position = Some(beacon_pos);
            }
            st.insurgency_corruption_stage
        };
        let p_u32 = pirate_fac as u32;
        let mut ensure_site = |stype: &str, sname: &dyn Fn() -> String, rew: f64, target_sys: u32| {
            if !snap.insurgency_runtime.insurgency_instances.iter().any(|s| {
                s.solar_system_id == target_sys && s.site_type == stype && !s.completed
            }) {
                let sid = snap
                    .insurgency_runtime
                    .insurgency_instances
                    .iter()
                    .filter(|s| s.instance_id < 1_800_000_000)
                    .map(|s| s.instance_id)
                    .max()
                    .unwrap_or(960_000)
                    .max(960_000)
                    + 1;
                snap.insurgency_runtime
                    .insurgency_instances
                    .push(eve_db::PersistedInsurgencySiteRecord {
                        instance_id: sid,
                        solar_system_id: target_sys,
                        site_type: stype.into(),
                        name: sname(),
                        // DERIVED (synthetic): baseline corruption/suppression point reward for site completion.
                        corruption_reward: rew,
                        completed: false,
                    });
            }
        };

        ensure_site(
            "CorporateOutpostRaid",
            &|| sde_insurgency_dungeon_name(sde, &[75, 76, 77, 78], p_u32, "Corporate Outpost Raid"),
            100.0,
            sys_id,
        );
        if c_st >= 1 {
            ensure_site(
                "MiningOpAmbush",
                &|| sde_insurgency_dungeon_name(sde, &[79], p_u32, "Mining Op Ambush"),
                200.0,
                sys_id,
            );
        }
        if (3..=4).contains(&c_st) {
            ensure_site(
                "IceRefineryHeist",
                &|| sde_insurgency_dungeon_name(sde, &[74], p_u32, "Ice Refinery Heist"),
                300.0,
                sys_id,
            );
            if !snap
                .insurgency_runtime
                .ice_heist_instances
                .iter()
                .any(|h| h.solar_system_id == sys_id && !h.completed)
            {
                let hid = snap
                    .insurgency_runtime
                    .ice_heist_instances
                    .iter()
                    .map(|h| h.instance_id)
                    .max()
                    .unwrap_or(860_000)
                    .max(860_000)
                    + 1;
                snap.insurgency_runtime
                    .ice_heist_instances
                    .push(eve_db::PersistedIceHeistRecord {
                        instance_id: hid,
                        solar_system_id: sys_id,
                        pirate_tankers_saved: 0,
                        empire_tankers_saved: 0,
                        completed: false,
                    });
            }
        }
        if covering.iter().any(|&(_, fsys)| fsys == sys_id) || fob_sys == sys_id {
            ensure_site(
                "PirateLaunderingFacility",
                &|| sde_insurgency_dungeon_name(sde, &[72], p_u32, "Pirate Laundering Facility"),
                150.0,
                sys_id,
            );
        }

        let war_hqs: &[(u32, u32)] = if pirate_fac == crate::services::insurgency_services::FACTION_ANGEL_CARTEL {
            &[(30_002_974, 500_003), (30_002_055, 500_002)]
        } else {
            &[(30_045_324, 500_001), (30_003_788, 500_004)]
        };
        for &(hq_sys, emp_fac) in war_hqs {
            ensure_site(
                "CounterInsurgencyOutpost",
                &|| sde_insurgency_dungeon_name(sde, &[73], emp_fac, "Counter-Insurgency Outpost"),
                100.0,
                hq_sys,
            );
        }
    }

    prune_ineligible_insurgency_sites_in_snap(snap, sys_id);
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
    if !(3..=4).contains(&c_st) {
        snap.insurgency_runtime
            .ice_heist_instances
            .retain(|h| h.solar_system_id != sys_id || h.instance_id < 860_001);
    }
}

/// Returns the nearest SDE stargate (`sde.jumps`) or NPC station (`sde.stations`) position within `500 km` sentry range.
pub fn nearest_sentry_gun_origin(ctx: &ServerContext, sys_id: u32, pos: DVec3) -> Option<DVec3> {
    let gates = ctx
        .sde
        .jumps
        .values()
        .filter(|j| j.from_solar_system_id == sys_id)
        .map(|j| (j.stargate_id, j.position));
    let stations = ctx
        .sde
        .stations
        .values()
        .filter(|s| s.solar_system_id == sys_id)
        .map(|s| (s.station_id, s.position));
    gates
        .chain(stations)
        .map(|(id, p)| (p.distance(pos), id, p))
        .filter(|&(dist, _, _)| dist <= 500_000.0)
        .min_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)))
        .map(|(_, _, p)| p)
}

#[cfg(test)]
#[path = "security_mgr_tests.rs"]
mod security_mgr_tests;

