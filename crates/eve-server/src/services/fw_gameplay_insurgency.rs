//! `fw_gameplay_insurgency` — Live Pirate Insurgency Lifecycle, Spread, FOB Siege, Ambition & Payouts (`SO-5`, `Q-8`).
//!
//! Exposes ONLY live server tick, combat, structure-attack, and docking entrypoints:
//! - Automatic Frontline Insurgency seeding & 48h `Forecasting` -> 7d `Active` -> 48h `WindDown` lifecycle (`tick_insurgency_campaigns`).
//! - Corruption Stage 3 stargate spread (`try_spread_insurgency_from_system`, called from `insurgency_services::add_system_corruption`).
//! - Pirate FOB docking enforcement (`check_docking_allowed`, called from `station_structure.rs`).
//! - Pirate FOB armor reinforcement (`3h ± 1h` deterministic jitter) & hull destruction (`apply_pirate_fob_structure_damage`, called from `structure_lifecycle_rpc.rs`).
//! - Ambition-scaled victory evaluation, post-surrender FW I-Hub vulnerability (`25h`), and personal contribution-scaled ISK + militia-corp LP payouts (`evaluate_and_resolve_insurgency_campaign`).

#![forbid(unsafe_code)]

use crate::router::{NotificationTarget, ServerContext};
use crate::services::fw_gameplay::militia_lp_corp_id;
use crate::services::fw_services::{compute_all_frontline_statuses, set_facwar_bunker_vulnerable, STATUS_FRONTLINE};
use crate::services::insurgency_services::{
    add_system_suppression, emit_campaign_started, emit_campaign_updated, end_insurgency_campaign,
    get_campaign_phase_and_timer, init_insurgency_campaign, insurgency_stage,
    spawn_pirate_stronghold, with_insurgency_runtime, InsurgencySiteRecord, FACTION_ANGEL_CARTEL,
    FACTION_GURISTAS, PHASE_ACTIVE, PHASE_FORECASTING, PIRATE_STRONGHOLD_ANGEL_TYPE,
    PIRATE_STRONGHOLD_GURISTAS_TYPE, WINDDOWN_DURATION_SECS,
};
use eve_db::UpwellStructureRecord;
use eve_destiny::{Ball, MovementMode};
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;

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
            // DERIVED (synthetic): baseline Pirate FOB shield/armor/hull HP scaled for live Dreadnought siege artillery volleys.
            shield_hp: 2_500.0,
            armor_hp: 2_500.0,
            hull_hp: 3_500.0,
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
    let wz_systems: std::collections::HashSet<u32> = ctx.db.with_snapshot(|snap| {
        snap.fw_system_occupations.keys().copied().collect()
    });
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
        let site_id = (rt.insurgency_instances.len() as i64) + 950_001;
        rt.insurgency_instances.push(InsurgencySiteRecord {
            instance_id: site_id,
            solar_system_id: new_sys,
            site_type: "PirateMiningAmbush".into(),
            name: "Insurgency Forward Outpost".into(),
            corruption_reward: 200.0,
            completed: false,
        });
    });
    ctx.db.with_mut_snapshot(|snap| {
        let st = snap.system_states.entry(new_sys).or_default();
        if st.fw_complex_position.is_none() {
            // DERIVED (synthetic): baseline warzone complex capture beacon coordinates at grid origin.
            st.fw_complex_position = Some([0.0, 0.0, 0.0]);
        }
    });
    emit_campaign_started(ctx, new_sys, campaign_id);
    Some(new_sys)
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
    }

    let mut updated_balances: Vec<(i64, f64)> = Vec::new();
    ctx.db.with_mut_snapshot(|snap| {
        for &(cid, fid, lp_award) in &payouts {
            if lp_award > 0 {
                let lp_corp = militia_lp_corp_id(&ctx.sde, fid);
                // DERIVED (synthetic): Insurgency.md:66 specifies an ISK+LP payout at insurgency conclusion without an exact ISK formula; scale ISK at 100 ISK per LP (150M ISK max at 1.5M LP cap).
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

/// Live 1 Hz Pirate Insurgency campaign scheduler (`Forecasting` -> `Active` -> Ambition / Tie -> `WindDown` -> End),
/// automatic Frontline insurgency spawning when warzone frontlines lack a campaign, and post-surrender FW I-Hub
/// vulnerability unlocking (`24h + 1 downtime` = `25h`).
pub async fn tick_insurgency_campaigns(ctx: &ServerContext, now_unix: u64) {
    // 1. Unlock post-surrender FW I-Hub vulnerability once `surrender_ihub_vulnerable_unix` elapses.
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

    // 2. Auto-seed a Pirate Insurgency around a low-sec FW Frontline system when warzones are active
    //    and an online Pirate Militia pilot is in a warzone system with no active campaign yet.
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
        for pass in 0..3 {
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
            let _ = pass;
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
        // During the 48h Forecasting phase, non-FOB combat insurgency sites do not spawn yet.
        with_insurgency_runtime(ctx, |rt| {
            rt.insurgency_instances
                .retain(|s| !systems.contains(&s.solar_system_id) || s.site_type == "PirateStronghold");
        });
    }

    // 3. Advance existing campaigns across `Forecasting -> Active -> Ambition/Tie -> WindDown -> End`.
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
                // Ensure each covered system has an active insurgency outpost site & complex capture grid.
                for &sys in &camp.systems {
                    let st = snap.system_states.entry(sys).or_default();
                    if st.fw_complex_position.is_none() {
                        // DERIVED (synthetic): baseline warzone complex capture beacon coordinates at grid origin.
                        st.fw_complex_position = Some([0.0, 0.0, 0.0]);
                    }
                    let has_site = snap.insurgency_runtime.insurgency_instances.iter().any(|s| {
                        s.solar_system_id == sys && s.site_type != "PirateStronghold" && !s.completed
                    });
                    if !has_site {
                        let sid = (snap.insurgency_runtime.insurgency_instances.len() as i64) + 960_001;
                        snap.insurgency_runtime.insurgency_instances.push(InsurgencySiteRecord {
                            instance_id: sid,
                            solar_system_id: sys,
                            site_type: "PirateMiningAmbush".into(),
                            name: "Insurgency Ambush Point".into(),
                            corruption_reward: 200.0,
                            completed: false,
                        });
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
            // WindDown phase (`now_unix >= active_end_unix && now_unix < winddown_end_unix`): resolve Tie if unresolved.
            if !snap.insurgency_runtime.campaign_outcomes.contains_key(&cid) {
                return (Some("Resolve"), notify_active);
            }
            (None, notify_active)
        });

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
                if ctx.db.with_snapshot(|s| !s.insurgency_runtime.campaign_outcomes.contains_key(&cid)) {
                    let _ = evaluate_and_resolve_insurgency_campaign(ctx, cid, None);
                }
                end_insurgency_campaign(ctx, cid);
            }
            _ => {}
        }
    }
}
