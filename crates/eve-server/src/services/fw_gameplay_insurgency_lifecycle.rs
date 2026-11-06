//! Insurgency campaign spawning, stargate spread, Ambition victory resolution, and tick lifecycle.

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::fw_gameplay::militia_lp_corp_id;
use super::{
    FACTION_ANGEL_CARTEL, FACTION_GURISTAS, INSURGENCY_MAX_PAYOUT_LP,
    PIRATE_FOB_STRUCTURE_BASE_ID, SURRENDER_IHUB_VULNERABILITY_DELAY_SECS,
};
use crate::services::fw_services::{
    compute_all_frontline_statuses, set_facwar_bunker_vulnerable, STATUS_FRONTLINE,
};
use crate::services::insurgency_services::{
    emit_campaign_started, emit_campaign_updated, end_insurgency_campaign,
    get_campaign_phase_and_timer, init_insurgency_campaign, insurgency_stage,
    spawn_pirate_stronghold, with_insurgency_runtime, PHASE_ACTIVE, PHASE_FORECASTING,
    PHASE_WINDDOWN, PIRATE_STRONGHOLD_ANGEL_TYPE, PIRATE_STRONGHOLD_GURISTAS_TYPE,
    WINDDOWN_DURATION_SECS,
};
use eve_db::UpwellStructureRecord;
use eve_destiny::{Ball, MovementMode};
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

/// Ensures the Pirate FOB `UpwellStructureRecord` (`79_172` Guristas / `78_260` Angel Cartel) and
/// space ball exist in `fob_solar_system_id` for every non-ended Insurgency campaign.
pub async fn ensure_campaign_pirate_fob(ctx: &ServerContext, campaign_id: i64) -> Option<i64> {
    let (fob_sys, pirate_fac, fob_id, fob_type, pos) = ctx.db.with_mut_snapshot(|snap| {
        let camp = snap.insurgency_runtime.campaigns.get(&campaign_id)?;
        let fob_sys = camp.fob_solar_system_id;
        let pirate_fac = camp.pirate_faction_id;
        let fob_id = PIRATE_FOB_STRUCTURE_BASE_ID + campaign_id;
        let fob_type = if pirate_fac == FACTION_ANGEL_CARTEL {
            PIRATE_STRONGHOLD_ANGEL_TYPE
        } else {
            PIRATE_STRONGHOLD_GURISTAS_TYPE
        };
        let owner_corp = militia_lp_corp_id(&ctx.sde, pirate_fac);
        let base_hp = |attr_id: u16, fallback: f64| -> f64 {
            ctx.sde
                .types
                .get(&fob_type)
                .and_then(|t| t.default_attributes.get(&attr_id).copied())
                .filter(|&v| v > 0.0)
                .map_or(fallback, |_| fallback)
        };
        // DERIVED (synthetic): baseline pirate FOB orbital anchor coordinates.
        let pos = [95_000.0, 0.0, 45_000.0];
        let st = snap.structures.entry(fob_id).or_insert_with(|| UpwellStructureRecord {
            structure_id: fob_id,
            type_id: fob_type,
            owner_corp_id: owner_corp,
            solar_system_id: fob_sys,
            name: if pirate_fac == FACTION_ANGEL_CARTEL {
                "Angel Cartel Forward Operating Base".into()
            } else {
                "Guristas Forward Operating Base".into()
            },
            state: "ShieldVulnerable".into(),
            position: pos,
            quantum_core_installed: true,
            fuel_blocks: 10_000,
            shield_hp: base_hp(263, 2_500.0),
            armor_hp: base_hp(265, 2_500.0),
            hull_hp: base_hp(9, 3_500.0),
            reinforced_until_unix: 0,
            last_damage_unix: 0,
            last_damage_tick: 0,
            self_destruct_at_unix: None,
        });
        if st.state == "Destroyed" {
            return None;
        }
        Some((fob_sys, pirate_fac, fob_id, fob_type, pos))
    })?;
    let _ = pirate_fac;
    let sys_handle = ctx.systems.get_or_spawn(fob_sys);
    if sys_handle.get_ball_snapshot(fob_id).await.ok().flatten().is_none() {
        let _ = sys_handle
            .spawn_ball(Ball {
                id: fob_id,
                type_id: fob_type,
                position: DVec3::from_array(pos),
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 0.0,
                mass: 5.0e8,
                radius: 1_200.0,
                speed_fraction: 0.0,
                mode: MovementMode::Rigid,
            })
            .await;
    }
    Some(fob_id)
}

/// Spreads an active Pirate Insurgency when `source_sys_id` reaches Corruption Stage 3 (`>= 3`)
/// (unless `source_sys_id` is Stage 5 Suppressed), adding a stargate-adjacent `0.0..=0.8` Empire system.
pub fn try_spread_insurgency_from_system(
    ctx: &ServerContext,
    campaign_id: i64,
    source_sys_id: u32,
) -> Option<u32> {
    let (covered, suppressed5) = with_insurgency_runtime(ctx, |rt| {
        rt.campaigns.get(&campaign_id).map(|c| {
            let s5: std::collections::HashSet<u32> = c
                .suppression_points
                .iter()
                .filter(|(_, &pts)| insurgency_stage(pts) >= 5)
                .map(|(&sid, _)| sid)
                .collect();
            (c.systems.clone(), s5)
        })
    })?;
    let source_stage5_suppressed = suppressed5.contains(&source_sys_id)
        || ctx.db.with_snapshot(|snap| {
            snap.system_states
                .get(&source_sys_id)
                .is_some_and(|st| st.insurgency_suppression_stage >= 5)
        });
    if source_stage5_suppressed {
        return None;
    }

    let valid_sec = |sys_id: u32| -> bool {
        ctx.sde.solar_systems.get(&sys_id).is_some_and(|s| {
            s.solar_system_id < 30_100_000
                && matches!(s.wormhole_class_id, None | Some(7..=9))
                && s.security_status >= 0.0
                && s.security_status < 0.85
                && ctx.sde.regions.get(&s.region_id).is_some_and(|r| {
                    matches!(r.faction_id, Some(500_001..=500_004 | 500_007 | 500_008))
                })
        })
    };
    let has_hs_covered = covered.iter().any(|&s| {
        ctx.sde
            .solar_systems
            .get(&s)
            .is_some_and(|sys| sys.security_status >= 0.5)
    });
    let wz_systems: std::collections::HashSet<u32> = ctx
        .db
        .with_snapshot(|snap| snap.fw_system_occupations.keys().copied().collect());
    let rank = |sys_id: u32| -> (u8, u8, u32) {
        let is_hs = ctx
            .sde
            .solar_systems
            .get(&sys_id)
            .is_some_and(|s| s.security_status >= 0.5);
        let hs_prio = if !has_hs_covered && is_hs { 0 } else { 1 };
        let wz_prio = if wz_systems.contains(&sys_id) { 0 } else { 1 };
        (hs_prio, wz_prio, sys_id)
    };

    let mut candidate: Option<u32> = ctx
        .sde
        .jumps
        .values()
        .filter(|j| j.from_solar_system_id == source_sys_id)
        .map(|j| j.to_solar_system_id)
        .filter(|s| !covered.contains(s) && valid_sec(*s))
        .min_by_key(|&s| rank(s));

    if candidate.is_none() {
        let state_s5: std::collections::HashSet<u32> = ctx.db.with_snapshot(|snap| {
            covered
                .iter()
                .copied()
                .filter(|s| {
                    suppressed5.contains(s)
                        || snap
                            .system_states
                            .get(s)
                            .is_some_and(|st| st.insurgency_suppression_stage >= 5)
                })
                .collect()
        });
        candidate = ctx
            .sde
            .jumps
            .values()
            .filter(|j| {
                covered.contains(&j.from_solar_system_id)
                    && !state_s5.contains(&j.from_solar_system_id)
            })
            .map(|j| j.to_solar_system_id)
            .filter(|s| !covered.contains(s) && valid_sec(*s))
            .min_by_key(|&s| rank(s));
    }

    let new_sys = candidate?;
    with_insurgency_runtime(ctx, |rt| {
        if let Some(c) = rt.campaigns.get_mut(&campaign_id) {
            if !c.systems.contains(&new_sys) {
                c.systems.push(new_sys);
                c.corruption_points.entry(new_sys).or_insert(0);
                c.suppression_points.entry(new_sys).or_insert(0);
            }
        }
    });
    let now_spread = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        crate::services::security_mgr::sync_stage_gated_insurgency_sites_in_snap(
            &ctx.sde,
            snap,
            new_sys,
            now_spread,
        );
    });
    emit_campaign_started(ctx, new_sys, campaign_id);
    Some(new_sys)
}

/// Evaluates an Insurgency campaign's Ambition-scaled victory condition (`5 + ambition_modifier` Stage-5 systems),
/// updates Pirate/Empire Ambition modifiers (`0..=5`, mutually exclusive; underdog win reduces leader's modifier by 1),
/// schedules post-surrender FW I-Hub vulnerability (`25h`) on a Pirate victory, and pays personal contribution-scaled
/// ISK + militia-corp LP (`1.0x` winner, `0.4x` loser, max at `45%` share, capped at `1,500,000 LP`, `0` on tie).
pub fn evaluate_and_resolve_insurgency_campaign(
    ctx: &ServerContext,
    campaign_id: i64,
    forced_outcome: Option<&str>,
) -> Result<(String, u8, u8, Vec<(i64, i64, i64)>), BridgeRpcError> {
    let now = ctx.now_unix();
    let (outcome, new_p_mod, new_e_mod, fob_sys, sys_list, pirate_fac, payouts) =
        with_insurgency_runtime(ctx, |rt| {
            let camp = rt.campaigns.get_mut(&campaign_id).ok_or_else(|| BridgeRpcError {
                code: "CampaignNotFound".into(),
                message: format!("Insurgency campaign {campaign_id} not found"),
            })?;
            let pirate_fac = camp.pirate_faction_id;
            let fob_sys = camp.fob_solar_system_id;
            let sys_list = camp.systems.clone();
            if let Some(existing) = rt.campaign_outcomes.get(&campaign_id) {
                let p = rt.pirate_ambition_modifier.get(&pirate_fac).copied().unwrap_or(0);
                let e = rt.empire_ambition_modifier.get(&pirate_fac).copied().unwrap_or(0);
                return Ok((existing.clone(), p, e, fob_sys, sys_list, pirate_fac, Vec::new()));
            }
            let mut p_mod = rt.pirate_ambition_modifier.get(&pirate_fac).copied().unwrap_or(0).min(5);
            let mut e_mod = rt.empire_ambition_modifier.get(&pirate_fac).copied().unwrap_or(0).min(5);
            let p_target = usize::from(5 + p_mod);
            let e_target = usize::from(5 + e_mod);
            let corrupted_5 = camp
                .corruption_points
                .values()
                .filter(|&&pts| insurgency_stage(pts) >= 5)
                .count();
            let suppressed_5 = camp
                .suppression_points
                .values()
                .filter(|&&pts| insurgency_stage(pts) >= 5)
                .count();

            let outcome = if let Some(forced) = forced_outcome {
                forced.to_string()
            } else if corrupted_5 >= p_target {
                "PirateWin".to_string()
            } else if suppressed_5 >= e_target {
                "EmpireWin".to_string()
            } else {
                "Tie".to_string()
            };

            match outcome.as_str() {
                "PirateWin" => {
                    if e_mod > 0 {
                        e_mod -= 1;
                    } else {
                        p_mod = (p_mod + 1).min(5);
                    }
                }
                "EmpireWin" => {
                    if p_mod > 0 {
                        p_mod -= 1;
                    } else {
                        e_mod = (e_mod + 1).min(5);
                    }
                }
                _ => {}
            }

            if camp.active_end_unix > now {
                camp.forecasting_end_unix = camp.forecasting_end_unix.min(now);
                camp.active_end_unix = now;
                camp.winddown_end_unix = now + WINDDOWN_DURATION_SECS;
            }

            rt.pirate_ambition_modifier.insert(pirate_fac, p_mod);
            rt.empire_ambition_modifier.insert(pirate_fac, e_mod);
            rt.campaign_outcomes.insert(campaign_id, outcome.clone());
            if outcome == "PirateWin" {
                rt.surrender_ihub_vulnerable_unix
                    .insert(fob_sys, now + SURRENDER_IHUB_VULNERABILITY_DELAY_SECS);
            }

            let contribs = rt.campaign_contributions.get(&campaign_id).cloned().unwrap_or_default();
            let pirate_total: i64 = contribs
                .iter()
                .filter(|(_, fid, _)| *fid == pirate_fac || *fid == FACTION_GURISTAS || *fid == FACTION_ANGEL_CARTEL)
                .map(|(_, _, pts)| *pts)
                .sum();
            let empire_total: i64 = contribs
                .iter()
                .filter(|(_, fid, _)| *fid != pirate_fac && *fid != FACTION_GURISTAS && *fid != FACTION_ANGEL_CARTEL)
                .map(|(_, _, pts)| *pts)
                .sum();

            let mut payouts = Vec::with_capacity(contribs.len());
            for (cid, fid, pts) in contribs {
                let is_pirate = fid == pirate_fac || fid == FACTION_GURISTAS || fid == FACTION_ANGEL_CARTEL;
                let side_total = if is_pirate { pirate_total } else { empire_total }.max(1);
                let share = (pts as f64) / (side_total as f64);
                let scaled_ratio = (share / 0.45).clamp(0.0, 1.0);
                let side_mult = match (outcome.as_str(), is_pirate) {
                    ("PirateWin", true) | ("EmpireWin", false) => 1.0,
                    ("PirateWin", false) | ("EmpireWin", true) => 0.4,
                    _ => 0.0,
                };
                let lp_award = ((INSURGENCY_MAX_PAYOUT_LP as f64) * scaled_ratio * side_mult)
                    .round()
                    .min(INSURGENCY_MAX_PAYOUT_LP as f64) as i64;
                payouts.push((cid, fid, lp_award));
            }
            Ok((outcome, p_mod, e_mod, fob_sys, sys_list, pirate_fac, payouts))
        })?;

    if outcome == "PirateWin" {
        let _ = spawn_pirate_stronghold(ctx, campaign_id);
    } else if outcome == "EmpireWin" || outcome == "AntiPirateWin" {
        let _ = crate::services::cluster_macho_misc_services::trigger_zarzakh_horizon_siege(
            ctx,
            Some(campaign_id),
            crate::services::cluster_macho_misc_services::HORIZON_SIEGE_ACTIVE_SECS,
        );
    }

    let mut updated_balances: Vec<(i64, f64)> = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        if outcome == "PirateWin" {
            if let Some(occ) = snap.fw_system_occupations.get_mut(&fob_sys) {
                occ.occupier_faction_id = pirate_fac;
            }
            snap.fw_runtime.ihub_vulnerable.insert(fob_sys, false);
        }
        for &(cid, fid, lp_award) in &payouts {
            if lp_award > 0 {
                let lp_corp = militia_lp_corp_id(&ctx.sde, fid);
                // DERIVED (synthetic): Insurgency.md:66 ISK+LP payout scaled at 100 ISK per LP (150M ISK at 1.5M LP cap).
                let isk_award = lp_award.saturating_mul(100);
                if let Some(ch) = snap.characters.get_mut(&cid) {
                    *ch.loyalty_points.entry(lp_corp).or_insert(0) += lp_award;
                    let isk_cents = eve_db::IskCents::from_isk(isk_award);
                    ch.balance = ch
                        .balance
                        .checked_add(isk_cents)
                        .unwrap_or(ch.balance);
                    let new_bal = ch.balance;
                    snap.append_journal(
                        cid,
                        "InsurgencyContributionPayout",
                        isk_cents,
                        new_bal,
                        format!("Insurgency campaign {campaign_id} contribution reward"),
                    );
                    updated_balances.push((cid, new_bal.to_wire_f64()));
                }
            }
        }
    });

    for (cid, new_bal) in updated_balances {
        let _ = ctx.notify_account_change(cid, "cash", cid, new_bal);
    }

    for sys in sys_list {
        emit_campaign_updated(ctx, sys, campaign_id, 5);
        let outcome_state = match outcome.as_str() {
            "PirateWin" => crate::services::insurgency_services::CAMPAIGN_STATE_PIRATE_WIN,
            "EmpireWin" | "AntiPirateWin" => crate::services::insurgency_services::CAMPAIGN_STATE_ANTIPIRATE_WIN,
            _ => crate::services::insurgency_services::CAMPAIGN_STATE_NO_WINNER,
        };
        ctx.notify(
            NotificationTarget::SolarSystem(sys),
            "OnInsurgencyStateChangedForFaction",
            "solarsystemid",
            vec![
                PyRep::Integer(outcome_state),
                PyRep::Integer(pirate_fac),
                PyRep::Integer(i64::from(fob_sys)),
            ],
        );
    }

    Ok((outcome, new_p_mod, new_e_mod, payouts))
}

/// Live 1 Hz Pirate Insurgency campaign scheduler (`Forecasting` -> `Active` -> Ambition / Tie -> `WindDown` -> End).
pub async fn tick_insurgency_campaigns(ctx: &ServerContext, now_unix: u64) {
    let _ = crate::services::cluster_macho_misc_services::tick_zarzakh_horizon_siege(ctx, now_unix);
    let unlocked_surrender_sys: Vec<(u32, i64)> = ctx.db.with_mut_snapshot(|snap| {
        let due: Vec<u32> = snap
            .insurgency_runtime
            .surrender_ihub_vulnerable_unix
            .iter()
            .filter(|(&sys, &ts)| now_unix >= ts && !snap.fw_runtime.ihub_vulnerable.get(&sys).copied().unwrap_or(false))
            .map(|(&sys, _)| sys)
            .collect();
        let mut out = Vec::new();
        for sys in due {
            if let Some(occ) = snap.fw_system_occupations.get_mut(&sys) {
                if !(500_001..=500_004).contains(&occ.occupier_faction_id) {
                    occ.occupier_faction_id = occ.owner_faction_id;
                }
                occ.victory_points = occ.victory_points_threshold;
                let occ_fac = occ.occupier_faction_id;
                snap.fw_runtime
                    .system_victory_points
                    .insert(sys, occ.victory_points_threshold);
                if let Some(st) = snap.system_states.get_mut(&sys) {
                    st.fw_contested_pct = 100.0;
                }
                out.push((sys, occ_fac));
            }
        }
        out
    });
    for (sys, occ_fac) in unlocked_surrender_sys {
        set_facwar_bunker_vulnerable(ctx, sys, occ_fac, true);
    }

    let auto_frontline_seed: Option<(i64, i64, i64, u32, Vec<u32>)> = ctx.db.with_snapshot(|snap| {
        if !snap.insurgency_runtime.campaigns.is_empty() || snap.fw_system_occupations.is_empty() {
            return None;
        }
        let mut pirate_pilots: Vec<_> = snap
            .characters
            .values()
            .filter(|c| {
                matches!(c.war_faction_id, Some(FACTION_GURISTAS) | Some(FACTION_ANGEL_CARTEL))
                    && snap.fw_system_occupations.contains_key(&c.solar_system_id)
            })
            .collect();
        pirate_pilots.sort_by_key(|c| c.character_id);
        let pirate_pilot = pirate_pilots.first()?;
        let pirate_fac = pirate_pilot.war_faction_id?;
        let frontlines = compute_all_frontline_statuses(ctx, snap);
        let frontline_sys = if frontlines.get(&pirate_pilot.solar_system_id) == Some(&STATUS_FRONTLINE) {
            pirate_pilot.solar_system_id
        } else {
            *frontlines.iter().find(|(_, &st)| st == STATUS_FRONTLINE)?.0
        };
        let target_fac = snap
            .fw_system_occupations
            .get(&frontline_sys)
            .map(|o| o.occupier_faction_id)
            .unwrap_or(500_001);
        let mut starting = vec![frontline_sys];
        for _pass in 0..3 {
            if starting.len() >= 4 {
                break;
            }
            let frontier = starting.clone();
            let mut adj: Vec<u32> = ctx
                .sde
                .jumps
                .values()
                .filter(|j| frontier.contains(&j.from_solar_system_id))
                .map(|j| j.to_solar_system_id)
                .filter(|s| {
                    !starting.contains(s)
                        && ctx
                            .sde
                            .solar_systems
                            .get(s)
                            .is_some_and(|sys| sys.security_status > 0.0 && sys.security_status <= 0.8 && sys.solar_system_id < 31_000_000)
                })
                .collect();
            adj.sort_by_key(|s| (if snap.fw_system_occupations.contains_key(s) { 0u8 } else { 1u8 }, *s));
            adj.dedup();
            for s in adj {
                if starting.len() >= 4 {
                    break;
                }
                if !starting.contains(&s) {
                    starting.push(s);
                }
            }
        }
        let next_cid = (now_unix % 100_000) as i64 + 900_001;
        Some((next_cid, pirate_fac, target_fac, frontline_sys, starting))
    });
    if let Some((cid, p_fac, t_fac, fl_sys, systems)) = auto_frontline_seed {
        init_insurgency_campaign(ctx, cid, p_fac, t_fac, fl_sys, systems.clone());
        with_insurgency_runtime(ctx, |rt| {
            rt.insurgency_instances
                .retain(|s| !systems.contains(&s.solar_system_id) || s.site_type == "PirateStronghold");
        });
    }

    let camp_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.insurgency_runtime.campaigns.keys().copied().collect()
    });
    for cid in camp_ids {
        let _ = ensure_campaign_pirate_fob(ctx, cid).await;

        let (action, notify_active) = ctx.db.with_mut_snapshot(|snap| {
            let Some(camp) = snap.insurgency_runtime.campaigns.get(&cid).cloned() else {
                return (None, None);
            };
            let (phase, _) = get_campaign_phase_and_timer(&camp, now_unix);
            if now_unix >= camp.winddown_end_unix {
                return (Some("End"), None);
            }
            if phase == PHASE_FORECASTING {
                return (None, None);
            }
            let notify_active = if phase == PHASE_ACTIVE && snap.insurgency_runtime.active_state_notified.insert(cid) {
                Some((camp.systems.clone(), camp.pirate_faction_id, camp.fob_solar_system_id))
            } else {
                None
            };
            if phase == PHASE_ACTIVE {
                if notify_active.is_some() {
                    for &sys in &camp.systems {
                        crate::services::security_mgr::sync_stage_gated_insurgency_sites_in_snap(
                            &ctx.sde,
                            snap,
                            sys,
                            now_unix,
                        );
                    }
                }
                if !snap.insurgency_runtime.campaign_outcomes.contains_key(&cid) {
                    let p_mod = snap
                        .insurgency_runtime
                        .pirate_ambition_modifier
                        .get(&camp.pirate_faction_id)
                        .copied()
                        .unwrap_or(0)
                        .min(5);
                    let e_mod = snap
                        .insurgency_runtime
                        .empire_ambition_modifier
                        .get(&camp.pirate_faction_id)
                        .copied()
                        .unwrap_or(0)
                        .min(5);
                    let c5 = camp
                        .corruption_points
                        .values()
                        .filter(|&&pts| insurgency_stage(pts) >= 5)
                        .count();
                    let s5 = camp
                        .suppression_points
                        .values()
                        .filter(|&&pts| insurgency_stage(pts) >= 5)
                        .count();
                    let fob_destroyed = snap
                        .structures
                        .get(&(PIRATE_FOB_STRUCTURE_BASE_ID + cid))
                        .is_some_and(|st| st.state == "Destroyed");
                    if c5 >= usize::from(5 + p_mod) || (s5 >= usize::from(5 + e_mod) && fob_destroyed) {
                        return (Some("Resolve"), notify_active);
                    }
                }
                return (None, notify_active);
            }
            if !snap.insurgency_runtime.campaign_outcomes.contains_key(&cid) {
                return (Some("Resolve"), notify_active);
            }
            (None, notify_active)
        });

        let active_sys_info: Option<(i64, i64, Vec<(u32, u8, u8)>)> = ctx.db.with_snapshot(|snap| {
            let camp = snap.insurgency_runtime.campaigns.get(&cid)?;
            let (phase, _) = get_campaign_phase_and_timer(camp, now_unix);
            if !matches!(phase, PHASE_ACTIVE | PHASE_WINDDOWN) {
                return None;
            }
            let list = camp
                .systems
                .iter()
                .map(|&sys| {
                    let st = snap.system_states.get(&sys);
                    (
                        sys,
                        st.map_or(0, |s| s.insurgency_corruption_stage),
                        st.map_or(0, |s| s.insurgency_suppression_stage),
                    )
                })
                .collect();
            Some((camp.pirate_faction_id, camp.target_faction_id, list))
        });
        if let Some((p_fac, e_fac, sys_stages)) = active_sys_info {
            for (sys, c_st, s_st) in sys_stages {
                super::sync_insurgency_system_npcs(ctx, p_fac, e_fac, sys, c_st, s_st).await;
            }
            crate::services::insurgency_complex_ops::sync_insurgency_complex_sites_and_facilities(ctx, now_unix).await;
        }

        if let Some((systems, p_fac, fob_sys)) = notify_active {
            for sys in systems {
                ctx.notify(
                    NotificationTarget::SolarSystem(sys),
                    "OnInsurgencyStateChangedForFaction",
                    "solarsystemid",
                    vec![
                        PyRep::Integer(crate::services::insurgency_services::CAMPAIGN_STATE_ACTIVE),
                        PyRep::Integer(p_fac),
                        PyRep::Integer(i64::from(fob_sys)),
                    ],
                );
            }
        }

        match action {
            Some("Resolve") => {
                let _ = evaluate_and_resolve_insurgency_campaign(ctx, cid, None);
            }
            Some("End") => {
                let end_info = ctx.db.with_snapshot(|s| {
                    s.insurgency_runtime
                        .campaigns
                        .get(&cid)
                        .map(|c| (c.pirate_faction_id, c.systems.clone()))
                });
                if ctx.db.with_snapshot(|s| !s.insurgency_runtime.campaign_outcomes.contains_key(&cid)) {
                    let _ = evaluate_and_resolve_insurgency_campaign(ctx, cid, None);
                }
                end_insurgency_campaign(ctx, cid);
                if let Some((p_fac, ended_systems)) = end_info {
                    super::cleanup_ended_campaign_sites_and_roamers(ctx, p_fac, &ended_systems).await;
                }
            }
            _ => {}
        }
    }
}

#[path = "fw_gameplay_insurgency_lifecycle_cov_unit_tests.rs"]
#[cfg(test)]
mod fw_gameplay_insurgency_lifecycle_cov_unit_tests;

