//! `fw_gameplay_insurgency` — Live Pirate Insurgency Lifecycle, Spread, FOB Siege, Ambition & Payouts (`SO-5`, `Q-8`).
//!
//! Exposes ONLY live server tick, combat, structure-attack, and docking entrypoints:
//! - Automatic Frontline Insurgency seeding & 48h `Forecasting` -> 7d `Active` -> 48h `WindDown` lifecycle (`tick_insurgency_campaigns`).
//! - Corruption Stage 3 stargate spread (`try_spread_insurgency_from_system`, called from `insurgency_services::add_system_corruption`).
//! - Pirate FOB docking enforcement (`check_docking_allowed`, called from `station_structure.rs`).
//! - Pirate FOB armor reinforcement (`3h ± 1h` deterministic jitter) & hull destruction (`apply_pirate_fob_structure_damage`, called from `structure_lifecycle_rpc.rs`).
//! - Ambition-scaled victory evaluation, post-surrender FW I-Hub vulnerability (`25h`), and personal contribution-scaled ISK + militia-corp LP payouts (`evaluate_and_resolve_insurgency_campaign`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::insurgency_services::{
    add_system_suppression, get_campaign_phase_and_timer, insurgency_stage, with_insurgency_runtime,
    FACTION_ANGEL_CARTEL, FACTION_GURISTAS, PHASE_ACTIVE, PHASE_WINDDOWN,
    PIRATE_STRONGHOLD_ANGEL_TYPE, PIRATE_STRONGHOLD_GURISTAS_TYPE,
};
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

#[path = "fw_gameplay_insurgency_lifecycle.rs"]
mod fw_gameplay_insurgency_lifecycle;
pub use fw_gameplay_insurgency_lifecycle::*;

/// Pirate FOB base armor reinforcement duration (3 hours = `10,800 s`, `± 3,600 s` jitter).
pub const FOB_ARMOR_REINFORCEMENT_BASE_SECS: u64 = 3 * 3_600;
/// Post-insurgency surrender I-Hub vulnerability delay (`24h + 1 downtime` = `25 hours` = `90,000 s`).
pub const SURRENDER_IHUB_VULNERABILITY_DELAY_SECS: u64 = 25 * 3_600;
/// Maximum LP payout per pilot at the end of a Pirate Insurgency campaign (reached at `>= 45%` personal share).
pub const INSURGENCY_MAX_PAYOUT_LP: i64 = 1_500_000;
/// Deterministic structure ID base for campaign Pirate FOBs (`1_897_000_000 + campaign_id`).
pub const PIRATE_FOB_STRUCTURE_BASE_ID: i64 = 1_897_000_000;

fn splitmix64_fob(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Deterministic `[-3600, +3600]` second jitter for Pirate FOB 3h (`± 1h`) armor reinforcement.
pub fn fob_reinforcement_jitter_secs(campaign_id: i64, now_unix: u64) -> i64 {
    let h = splitmix64_fob((campaign_id as u64) ^ (now_unix / 3_600));
    ((h % 7_201) as i64) - 3_600
}

/// Records a pilot's personal Corruption or Suppression contribution points toward an Insurgency campaign.
/// Invoked automatically from live complex pulses (`insurgency_services.rs`) and FOB combat.
pub fn record_insurgency_contribution(
    ctx: &ServerContext,
    campaign_id: i64,
    char_id: i64,
    faction_id: i64,
    points: i64,
) {
    if char_id <= 0 || points <= 0 {
        return;
    }
    with_insurgency_runtime(ctx, |rt| {
        let list = rt.campaign_contributions.entry(campaign_id).or_default();
        if let Some(entry) = list.iter_mut().find(|(cid, fid, _)| *cid == char_id && *fid == faction_id) {
            entry.2 = entry.2.saturating_add(points);
        } else {
            list.push((char_id, faction_id, points));
        }
    });
}

/// Enforces Pirate FOB docking access on `beyonce.Dock` / `structureDocking.Dock`:
/// only pilots enlisted in the FOB's exact pirate militia (`500010` Guristas for `79172`,
/// `500011` Angel Cartel for `78260`, or the local campaign's `pirate_faction_id`) may dock.
pub fn check_docking_allowed(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: u32,
    dock_id: i64,
    _is_structure: bool,
) -> Result<(), BridgeRpcError> {
    // The Fulcrum Alignment Services (`feat:structures.fulcrum-alignment-services`):
    // Refuses docking to characters enlisted in an Empire militia (`500_001..=500_004`, whether via
    // direct enlistment, character war faction, or corporation enlistment) or carrying an active
    // 60s weapons timer (`is_weapon_timer_active`) or Zarzakh retribution timer (NOT the 15m PvP logoff timer).
    if dock_id == 60_015_187 {
        let eff_war_fac = ctx.db.with_snapshot(|s| {
            let ch = s.characters.get(&char_id);
            s.fw_runtime
                .direct_enlisted_chars
                .get(&char_id)
                .copied()
                .or_else(|| ch.and_then(|c| c.war_faction_id))
                .or_else(|| ch.and_then(|c| s.fw_runtime.corp_war_factions.get(&c.corporation_id).copied()))
        });
        if matches!(eff_war_fac, Some(500_001..=500_004)) {
            return Err(BridgeRpcError::user_error("FulcrumRefusesEmpireMilitia"));
        }
        if crate::services::cluster_macho_misc_services::is_zarzakh_horizon_siege_active(ctx)
            && !matches!(eff_war_fac, Some(500_010 | 500_011))
        {
            return Err(BridgeRpcError::user_error("HorizonSiegeFulcrumLockdown"));
        }
        let now_unix = ctx.now_unix();
        let cw = ctx.crimewatch.read().unwrap_or_else(|e| e.into_inner());
        let weapons_active = cw
            .states
            .get(&char_id)
            .is_some_and(|s| s.is_weapon_timer_active(now_unix));
        let retribution_active = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.disapproval_flags
                .get(&char_id)
                .copied()
                .is_some_and(|exp| now_unix < exp)
        });
        if weapons_active || retribution_active {
            return Err(BridgeRpcError::user_error("FulcrumRefusesTimerActive"));
        }
    }

    let check = ctx.db.with_snapshot(|snap| {
        let st = snap.structures.get(&dock_id)?;
        let is_fob = matches!(
            st.type_id,
            PIRATE_STRONGHOLD_ANGEL_TYPE | PIRATE_STRONGHOLD_GURISTAS_TYPE
        ) || ctx
            .sde
            .types
            .get(&st.type_id)
            .is_some_and(|t| t.group_id == 4644);
        if !is_fob {
            return None;
        }
        let req_pirate_fac = if st.type_id == PIRATE_STRONGHOLD_ANGEL_TYPE {
            FACTION_ANGEL_CARTEL
        } else if st.type_id == PIRATE_STRONGHOLD_GURISTAS_TYPE {
            FACTION_GURISTAS
        } else {
            snap.insurgency_runtime
                .campaigns
                .values()
                .find(|c| c.fob_solar_system_id == sys_id || c.systems.contains(&sys_id))
                .map(|c| c.pirate_faction_id)
                .unwrap_or(FACTION_GURISTAS)
        };
        let war_fac = snap.characters.get(&char_id).and_then(|c| c.war_faction_id);
        Some((req_pirate_fac, war_fac))
    });
    if let Some((req_fac, war_fac)) = check {
        if war_fac != Some(req_fac) {
            return Err(BridgeRpcError {
                code: "FobDockingRestrictedToPirateMilitia".into(),
                message: format!(
                    "Docking at Pirate FOB {dock_id} requires enlistment in pirate militia {req_fac} (have {war_fac:?})"
                ),
            });
        }
    }
    Ok(())
}

/// Applies live weapon damage (`dogmaIM.Activate`) to a Pirate FOB (`79172` / `78260`):
/// - Invulnerable during `Forecasting` / `WindDown` (`FobInvulnerableInPhase`).
/// - Invulnerable until the Empire side reaches its Ambition-scaled Suppression Stage-5 goal (`SuppressionGoalNotReached`).
/// - Depleting shield+armor triggers a single `3h ± 1h` `ArmorReinforced` timer (`fob_armor_reinforced_until`).
/// - Depleting hull after timer expiry destroys the FOB, raises Suppression by `+1` stage (`+200` pts, max Stage 5)
///   across all covered campaign systems, and resolves the Insurgency campaign as an `EmpireWin`!
pub async fn apply_pirate_fob_structure_damage(
    ctx: &ServerContext,
    attacker_char_id: i64,
    structure_id: i64,
    damage: f64,
) -> Result<PyRep, BridgeRpcError> {
    let now = ctx.now_unix();
    enum FobStep {
        Damaged {
            state: String,
            shield_hp: f64,
            armor_hp: f64,
            hull_hp: f64,
            reinf_until: u64,
            camp_id: i64,
            attacker_fac: i64,
            contrib_pts: i64,
        },
        Destroyed {
            sys_id: u32,
            camp_id: i64,
            systems: Vec<u32>,
            attacker_fac: i64,
            contrib_pts: i64,
        },
    }

    let step = ctx.db.try_with_mut_snapshot(|snap| {
        let st_sys = snap
            .structures
            .get(&structure_id)
            .map(|s| s.solar_system_id)
            .ok_or_else(|| BridgeRpcError {
                code: "StructureNotFound".into(),
                message: format!("Pirate FOB {structure_id} not found"),
            })?;
        let attacker_fac = snap
            .characters
            .get(&attacker_char_id)
            .and_then(|c| c.war_faction_id)
            .unwrap_or(500_001);
        let camp_id = if structure_id > PIRATE_FOB_STRUCTURE_BASE_ID
            && snap
                .insurgency_runtime
                .campaigns
                .contains_key(&(structure_id - PIRATE_FOB_STRUCTURE_BASE_ID))
        {
            structure_id - PIRATE_FOB_STRUCTURE_BASE_ID
        } else {
            snap.insurgency_runtime
                .campaigns
                .values()
                .find(|c| c.fob_solar_system_id == st_sys)
                .map(|c| c.campaign_id)
                .ok_or_else(|| BridgeRpcError {
                    code: "CampaignNotFound".into(),
                    message: format!("No active Insurgency campaign found for FOB {structure_id}"),
                })?
        };
        let camp = snap.insurgency_runtime.campaigns.get(&camp_id).expect("checked above");
        if attacker_fac == camp.pirate_faction_id {
            return Err(BridgeRpcError {
                code: "CannotAttackFriendlyStructure".into(),
                message: "Pirate militia pilots cannot attack their own Pirate FOB".into(),
            });
        }
        let (phase, _) = get_campaign_phase_and_timer(camp, now);
        if phase != PHASE_ACTIVE {
            return Err(BridgeRpcError {
                code: "FobInvulnerableInPhase".into(),
                message: format!("Pirate FOB is invulnerable during {phase} phase"),
            });
        }
        let emp_mod = snap
            .insurgency_runtime
            .empire_ambition_modifier
            .get(&camp.pirate_faction_id)
            .copied()
            .unwrap_or(0)
            .min(5);
        let supp_goal = usize::from(5 + emp_mod);
        let suppressed_systems = camp
            .suppression_points
            .values()
            .filter(|&&pts| insurgency_stage(pts) >= 5)
            .count();
        let existing_reinf = snap
            .insurgency_runtime
            .fob_armor_reinforced_until
            .get(&camp_id)
            .copied();
        if existing_reinf.is_none() && suppressed_systems < supp_goal {
            return Err(BridgeRpcError {
                code: "SuppressionGoalNotReached".into(),
                message: format!(
                    "Pirate FOB requires {supp_goal} Stage-5 suppressed systems before attack (currently {suppressed_systems})"
                ),
            });
        }
        if let Some(until) = existing_reinf {
            if now < until {
                return Err(BridgeRpcError {
                    code: "FobArmorReinforced".into(),
                    message: format!("Pirate FOB is in armor reinforcement until {until} (now {now})"),
                });
            }
        }
        let systems = camp.systems.clone();
        let st = snap.structures.get_mut(&structure_id).expect("checked above");
        if st.state == "Destroyed" {
            return Err(BridgeRpcError {
                code: "StructureDestroyed".into(),
                message: format!("Pirate FOB {structure_id} is already destroyed"),
            });
        }
        st.last_damage_unix = now;
        let mut rem_dmg = damage.max(0.0);
        let mut dealt = 0.0;
        // DERIVED (synthetic): scale FOB combat contribution points from fraction of total FOB HP (8,500 HP) dealt relative to 1,000-point stage threshold.
        let pts_for_dealt = |d: f64| -> i64 { ((d / 8_500.0) * 1_000.0).round().max(1.0) as i64 };

        if existing_reinf.is_none() {
            if st.shield_hp > 0.0 {
                let take = rem_dmg.min(st.shield_hp);
                st.shield_hp -= take;
                rem_dmg -= take;
                dealt += take;
            }
            if rem_dmg > 0.0 && st.armor_hp > 0.0 {
                let take = rem_dmg.min(st.armor_hp);
                st.armor_hp -= take;
                dealt += take;
            }
            let contrib_pts = pts_for_dealt(dealt);
            if st.armor_hp <= 0.0 {
                let jitter = fob_reinforcement_jitter_secs(camp_id, now);
                let timer_secs = ((FOB_ARMOR_REINFORCEMENT_BASE_SECS as i64) + jitter).max(7_200) as u64;
                let until = now + timer_secs;
                st.armor_hp = 0.0;
                st.state = "ArmorReinforced".into();
                st.reinforced_until_unix = until;
                let s_hp = st.shield_hp;
                let a_hp = st.armor_hp;
                let h_hp = st.hull_hp;
                snap.insurgency_runtime.fob_armor_reinforced_until.insert(camp_id, until);
                return Ok(FobStep::Damaged {
                    state: "ArmorReinforced".into(),
                    shield_hp: s_hp,
                    armor_hp: a_hp,
                    hull_hp: h_hp,
                    reinf_until: until,
                    camp_id,
                    attacker_fac,
                    contrib_pts,
                });
            }
            return Ok(FobStep::Damaged {
                state: st.state.clone(),
                shield_hp: st.shield_hp,
                armor_hp: st.armor_hp,
                hull_hp: st.hull_hp,
                reinf_until: 0,
                camp_id,
                attacker_fac,
                contrib_pts,
            });
        }

        st.state = "HullVulnerable".into();
        let take_hull = rem_dmg.min(st.hull_hp.max(0.0));
        st.hull_hp = (st.hull_hp - take_hull).max(0.0);
        let contrib_pts = pts_for_dealt(take_hull);
        if st.hull_hp > 0.0 {
            return Ok(FobStep::Damaged {
                state: st.state.clone(),
                shield_hp: st.shield_hp,
                armor_hp: st.armor_hp,
                hull_hp: st.hull_hp,
                reinf_until: existing_reinf.unwrap_or(0),
                camp_id,
                attacker_fac,
                contrib_pts,
            });
        }

        st.state = "Destroyed".into();
        Ok(FobStep::Destroyed {
            sys_id: st_sys,
            camp_id,
            systems,
            attacker_fac,
            contrib_pts,
        })
    })?;

    match step {
        FobStep::Damaged {
            state,
            shield_hp,
            armor_hp,
            hull_hp,
            reinf_until,
            camp_id,
            attacker_fac,
            contrib_pts,
        } => {
            if state == "ArmorReinforced" && reinf_until > 0 {
                let _ = crate::services::cluster_macho_misc_services::schedule_zarzakh_horizon_siege(
                    ctx,
                    Some(camp_id),
                );
            }
            record_insurgency_contribution(ctx, camp_id, attacker_char_id, attacker_fac, contrib_pts);
            Ok(PyRep::keyval([
                ("structureID", PyRep::Integer(structure_id)),
                ("state", PyRep::String(state)),
                ("shieldHP", PyRep::Real(shield_hp)),
                ("armorHP", PyRep::Real(armor_hp)),
                ("hullHP", PyRep::Real(hull_hp)),
                ("reinforcedUntilUnix", PyRep::Integer(reinf_until as i64)),
            ]))
        }
        FobStep::Destroyed {
            sys_id,
            camp_id,
            systems,
            attacker_fac,
            contrib_pts,
        } => {
            let _ = ctx.systems.get_or_spawn(sys_id).remove_ball(structure_id).await;
            record_insurgency_contribution(ctx, camp_id, attacker_char_id, attacker_fac, contrib_pts);
            for s in &systems {
                let _ = add_system_suppression(ctx, camp_id, *s, 200);
            }
            let _ = evaluate_and_resolve_insurgency_campaign(ctx, camp_id, Some("EmpireWin"));
            Ok(PyRep::keyval([
                ("structureID", PyRep::Integer(structure_id)),
                ("state", PyRep::String("Destroyed".into())),
                ("shieldHP", PyRep::Real(0.0)),
                ("armorHP", PyRep::Real(0.0)),
                ("hullHP", PyRep::Real(0.0)),
                ("reinforcedUntilUnix", PyRep::Integer(0)),
            ]))
        }
    }
}

fn select_insurgency_roamer_type(
    ctx: &ServerContext,
    group_id: u32,
    faction_filter: Option<u32>,
    tier: usize,
    total_tiers: usize,
) -> Option<u32> {
    static ROAMER_POOL: std::sync::OnceLock<Vec<(u32, u32, u64, u64, u32)>> = std::sync::OnceLock::new();
    let pool = ROAMER_POOL.get_or_init(|| {
        ctx.sde
            .types
            .values()
            .filter(|t| {
                matches!(t.group_id, 4637 | 4638)
                    && t.faction_id.is_some()
                    && !crate::services::insurgency_complex_ops::is_named_insurgency_roamer(t.type_id)
            })
            .map(|t| {
                let hp = t.default_attributes.get(&263).copied().unwrap_or(0.0)
                    + t.default_attributes.get(&265).copied().unwrap_or(0.0)
                    + t.default_attributes.get(&9).copied().unwrap_or(0.0);
                let bounty = t.default_attributes.get(&481).copied().unwrap_or(0.0);
                (t.group_id, t.faction_id.unwrap_or(0), hp.round() as u64, bounty.round() as u64, t.type_id)
            })
            .collect()
    });
    let mut candidates: Vec<(u64, u64, u32)> = pool
        .iter()
        .filter(|&&(gid, fid, _, _, _)| gid == group_id && faction_filter.is_none_or(|f| fid == f))
        .map(|&(_, _, hp, bounty, tid)| (hp, bounty, tid))
        .collect();
    candidates.sort_unstable();
    if candidates.is_empty() {
        return None;
    }
    let idx = (tier.min(total_tiers) * (candidates.len() - 1)) / total_tiers.max(1);
    Some(candidates[idx].2)
}

fn system_roamer_waypoints(ctx: &ServerContext, sys_id: u32) -> Vec<DVec3> {
    let mut celestials = ctx.sde.celestials_in_system(sys_id);
    celestials.sort_by_key(|c| c.celestial_id);
    let mut wps: Vec<DVec3> = celestials
        .into_iter()
        .filter(|c| matches!(c.group_id, 7 | 8 | 9 | 10 | 15))
        .take(6)
        .map(|c| {
            let safe_r = if matches!(c.group_id, 6 | 7 | 8) {
                c.radius.max(0.0) + 50_000.0
            } else {
                0.0
            };
            DVec3::new(c.position.x + safe_r, c.position.y, c.position.z)
        })
        .collect();
    if wps.len() < 2 {
        // DERIVED (synthetic): fallback patrol grid waypoints around system origin when SDE celestials < 2.
        wps.push(DVec3::new(12_000.0, 0.0, 4_000.0));
        wps.push(DVec3::new(-12_000.0, 0.0, 4_000.0));
        wps.push(DVec3::new(0.0, 0.0, -14_000.0));
    }
    wps
}

async fn sync_insurgency_system_npcs(
    ctx: &ServerContext,
    pirate_fac: i64,
    empire_fac: i64,
    sys_id: u32,
    c_st: u8,
    s_st: u8,
) {
    use crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity;
    use crate::services::combat_crimewatch::with_mut_combat_ephemeral;

    let desired_pirate: Option<u32> =
        crate::services::insurgency_complex_ops::select_named_or_regular_roamer_type(
            ctx, sys_id, 4637, pirate_fac, c_st, s_st,
        )
        .or_else(|| {
            (c_st >= 2).then(|| {
                select_insurgency_roamer_type(ctx, 4637, Some(pirate_fac as u32), usize::from(c_st - 2), 3)
            }).flatten()
        });

    let desired_enforcer: Option<u32> =
        crate::services::insurgency_complex_ops::select_named_or_regular_roamer_type(
            ctx, sys_id, 4638, pirate_fac, c_st, s_st,
        )
        .or_else(|| {
            (s_st >= 3).then(|| select_insurgency_roamer_type(ctx, 4638, None, usize::from(s_st - 3), 2)).flatten()
        });

    let (stale_balls, has_pirate, has_enforcer, has_ci_sentry, idle_roamers) =
        with_mut_combat_ephemeral(ctx, |rt| {
            let mut stale = Vec::new();
            let mut hp = false;
            let mut he = false;
            let mut hs = false;
            let mut idle = Vec::new();
            for (&bid, npc) in &rt.npc_entities {
                if npc.solar_system_id != sys_id {
                    continue;
                }
                if npc.type_id == 79_507 {
                    hs = true;
                    continue;
                }
                let gid = ctx.sde.types.get(&npc.type_id).map(|t| t.group_id).unwrap_or(0);
                if gid == 4637 {
                    if desired_pirate == Some(npc.type_id) {
                        hp = true;
                        if npc.target_id.is_none() {
                            idle.push((bid, true, npc.spawn_position));
                        }
                    } else {
                        stale.push(bid);
                    }
                } else if gid == 4638 {
                    if desired_enforcer == Some(npc.type_id) {
                        he = true;
                        if npc.target_id.is_none() {
                            idle.push((bid, false, npc.spawn_position));
                        }
                    } else {
                        stale.push(bid);
                    }
                }
            }
            for &bid in &stale {
                rt.npc_entities.remove(&bid);
            }
            (stale, hp, he, hs, idle)
        });

    let need_pirate = desired_pirate.is_some() && !has_pirate;
    let need_enforcer = desired_enforcer.is_some() && !has_enforcer;
    if stale_balls.is_empty() && idle_roamers.is_empty() && !need_pirate && !need_enforcer && (s_st < 5 || has_ci_sentry) {
        if s_st < 5 {
            with_mut_combat_ephemeral(ctx, |rt| {
                rt.ci_sentry_deployed_systems.remove(&sys_id);
            });
        }
        return;
    }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    for bid in stale_balls {
        let _ = sys_handle.remove_ball(bid).await;
    }

    let waypoints = system_roamer_waypoints(ctx, sys_id);
    // DERIVED (synthetic): patrol offsets near celestial/grid anchors so Pirate and Enforcer roamers do not overlap at exact origin.
    let pirate_offset = DVec3::new(12_000.0, 0.0, 4_000.0);
    let enforcer_offset = DVec3::new(-12_000.0, 0.0, 4_000.0);

    if !idle_roamers.is_empty() {
        let mut arrived_positions = Vec::new();
        {
            let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
            for (bid, is_pir, cur_anchor) in idle_roamers {
                let offset = if is_pir { pirate_offset } else { enforcer_offset };
                let Some((cur_pos, is_warping)) = bp
                    .balls
                    .get(&bid)
                    .map(|b| (b.position, matches!(b.mode, eve_destiny::MovementMode::Warp { .. })))
                else {
                    continue;
                };
                if is_warping {
                    continue;
                }
                let anchor_pos = if cur_pos.distance(cur_anchor) > 15_000.0 {
                    arrived_positions.push((bid, cur_pos));
                    cur_pos
                } else {
                    cur_anchor
                };
                let cur_idx = waypoints
                    .iter()
                    .position(|w| (*w + offset).distance(anchor_pos) < 15_000.0)
                    .unwrap_or(0);
                let next_idx = (cur_idx + 1) % waypoints.len();
                let next_pos = waypoints[next_idx] + offset;
                if cur_pos.distance(next_pos) >= 150_000.0 {
                    let _ = bp.set_mode(bid, eve_destiny::MovementMode::new_warp(cur_pos, next_pos, 3.0));
                } else {
                    let _ = bp.goto_point(bid, next_pos);
                }
            }
            crate::system_actor::sync_ball_positions(
                &bp,
                &sys_handle.ball_positions,
                &sys_handle.scrambled_balls,
            );
        }
        if !arrived_positions.is_empty() {
            with_mut_combat_ephemeral(ctx, |rt| {
                for (bid, pos) in arrived_positions {
                    if let Some(npc) = rt.npc_entities.get_mut(&bid) {
                        npc.spawn_position = pos;
                    }
                }
            });
        }
    }

    if let Some(tid) = desired_pirate {
        if !has_pirate {
            let spawn_pos = waypoints[0] + pirate_offset;
            let next_pos = waypoints[1 % waypoints.len()] + pirate_offset;
            let bid = spawn_specialized_npc_entity(
                ctx,
                sys_id,
                tid,
                "InsurgencyPirateRoamer",
                pirate_fac as u32,
                spawn_pos,
            )
            .await;
            with_mut_combat_ephemeral(ctx, |rt| {
                if let Some(npc) = rt.npc_entities.get_mut(&bid) {
                    npc.custom_cargo_loot = crate::services::insurgency_complex_ops::named_insurgency_roamer_loot_drop(tid);
                }
            });
            let _ = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner()).goto_point(bid, next_pos);
        }
    }
    if let Some(tid) = desired_enforcer {
        if !has_enforcer {
            let spawn_pos = waypoints[0] + enforcer_offset;
            let next_pos = waypoints[1 % waypoints.len()] + enforcer_offset;
            let bid = spawn_specialized_npc_entity(
                ctx,
                sys_id,
                tid,
                "InsurgencyEnforcer",
                empire_fac as u32,
                spawn_pos,
            )
            .await;
            with_mut_combat_ephemeral(ctx, |rt| {
                if let Some(npc) = rt.npc_entities.get_mut(&bid) {
                    npc.custom_cargo_loot = crate::services::insurgency_complex_ops::named_insurgency_roamer_loot_drop(tid);
                }
            });
            let _ = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner()).goto_point(bid, next_pos);
        }
    }
    let should_deploy_ci = with_mut_combat_ephemeral(ctx, |rt| {
        if s_st < 5 {
            rt.ci_sentry_deployed_systems.remove(&sys_id);
            false
        } else if has_ci_sentry {
            rt.ci_sentry_deployed_systems.insert(sys_id);
            false
        } else {
            rt.ci_sentry_deployed_systems.insert(sys_id)
        }
    });
    if should_deploy_ci {
        // DERIVED (synthetic): Counter-Insurgency Sentry Gun deployment offset near grid anchor.
        let _ = spawn_specialized_npc_entity(ctx, sys_id, 79_507, "CounterInsurgencySentry", empire_fac as u32, DVec3::new(0.0, 0.0, 5_000.0)).await;
    }
}

pub(crate) async fn sync_campaign_system_npcs(ctx: &ServerContext, campaign_id: i64, sys_id: u32) {
    let now_unix = ctx.now_unix();
    let info = ctx.db.with_snapshot(|snap| {
        let camp = snap.insurgency_runtime.campaigns.get(&campaign_id)?;
        let (phase, _) = get_campaign_phase_and_timer(camp, now_unix);
        if !matches!(phase, PHASE_ACTIVE | PHASE_WINDDOWN) || !camp.systems.contains(&sys_id) {
            return None;
        }
        let st = snap.system_states.get(&sys_id);
        Some((
            camp.pirate_faction_id,
            camp.target_faction_id,
            st.map_or(0, |s| s.insurgency_corruption_stage),
            st.map_or(0, |s| s.insurgency_suppression_stage),
        ))
    });
    if let Some((p_fac, e_fac, c_st, s_st)) = info {
        sync_insurgency_system_npcs(ctx, p_fac, e_fac, sys_id, c_st, s_st).await;
    }
}

async fn cleanup_ended_campaign_sites_and_roamers(
    ctx: &ServerContext,
    pirate_fac: i64,
    ended_systems: &[u32],
) {
    use crate::services::combat_crimewatch::with_mut_combat_ephemeral;

    let orphan_systems: Vec<u32> = ctx.db.with_mut_snapshot(|snap| {
        let still_covered: std::collections::HashSet<u32> = snap.insurgency_runtime.campaigns.values().flat_map(|c| c.systems.iter().copied()).collect();
        let same_fac_active = snap.insurgency_runtime.campaigns.values().any(|c| c.pirate_faction_id == pirate_fac);
        let war_hqs: &[u32] = if pirate_fac == FACTION_ANGEL_CARTEL { &[30_002_974, 30_002_055] } else { &[30_045_324, 30_003_788] };
        let orphans: Vec<u32> = ended_systems.iter().copied().filter(|s| !still_covered.contains(s)).collect();
        snap.insurgency_runtime.insurgency_instances.retain(|site| {
            if site.site_type == "PirateStronghold" { return true; }
            if site.site_type == "CounterInsurgencyOutpost" && war_hqs.contains(&site.solar_system_id) { return same_fac_active; }
            !orphans.contains(&site.solar_system_id)
        });
        snap.insurgency_runtime
            .ice_heist_instances
            .retain(|h| !orphans.contains(&h.solar_system_id) || h.instance_id < 860_001);
        orphans
    });

    // Despawn roaming Pirate & Enforcer NPCs in ended systems, while keeping Counter-Insurgency Sentry Guns (`79507`) until destroyed.
    let to_remove: Vec<(u32, i64)> = with_mut_combat_ephemeral(ctx, |rt| {
        for &osys in &orphan_systems {
            rt.ci_sentry_deployed_systems.remove(&osys);
        }
        let mut out = Vec::new();
        rt.npc_entities.retain(|&bid, npc| {
            if !orphan_systems.contains(&npc.solar_system_id) || npc.type_id == 79_507 {
                return true;
            }
            let gid = ctx.sde.types.get(&npc.type_id).map(|t| t.group_id).unwrap_or(0);
            if gid == 4637 || gid == 4638 {
                out.push((npc.solar_system_id, bid));
                false
            } else {
                true
            }
        });
        out
    });
    for (sys_id, bid) in to_remove {
        let _ = ctx.systems.get_or_spawn(sys_id).remove_ball(bid).await;
    }
    crate::services::insurgency_complex_ops::cleanup_orphan_insurgency_complex_ops(ctx, &orphan_systems).await;
}

#[cfg(test)] #[path = "fw_gameplay_insurgency_tests.rs"]
mod fw_gameplay_insurgency_tests;

#[path = "fw_gameplay_insurgency_cov_unit_tests.rs"]
#[cfg(test)]
mod fw_gameplay_insurgency_cov_unit_tests;

