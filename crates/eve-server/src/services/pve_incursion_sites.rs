//! `pve_incursion_sites` — Sansha Incursions (Constellation Influence, Site Tiers, Fleet ISK+LP Payouts,
//! Mothership HQ Finale).
//!
// parity: feat:pve.incursion-system, feat:pve.incursion-payout, feat:pve.incursion-mothership

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_special_sites::{with_pve_special_reg, with_pve_special_reg_db, PveSpecialSitesRegistry};
use crate::system_actor::SystemRegistry;
use eve_db::{IskCents, WalletJournalEntry, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

type CharacterId = i64;
pub type ConstellationId = u32;

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

pub fn make_site_npc_ball(id: i64, type_id: u32, position: DVec3) -> Ball {
    Ball {
        id,
        type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity: 380.0,
        inertia: 1.0,
        mass: 4_000_000.0,
        radius: 220.0,
        speed_fraction: 0.5,
        mode: MovementMode::Stop,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncursionSiteEntry {
    pub site_id: String,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub solar_system_id: SolarSystemId,
    pub tier: String,
    pub min_pilots: usize,
    pub max_optimal_pilots: usize,
    pub base_isk: i64,
    pub base_concord_lp: i64,
    pub influence_delta: f64,
    pub locked_until_low_influence: bool,
    pub completed_count: u32,
    pub boss_npc_ball_id: i64,
    #[serde(default)]
    pub position: DVec3,
    #[serde(default)]
    pub boss_destroyed: bool,
    #[serde(default)]
    pub killer_char_id: Option<CharacterId>,
    #[serde(default)]
    pub killer_fleet_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncursionTaleRecord {
    pub tale_id: i64,
    pub constellation_id: ConstellationId,
    pub constellation_name: String,
    pub staging_solar_system_id: SolarSystemId,
    pub vanguard_solar_system_ids: Vec<SolarSystemId>,
    pub assault_solar_system_ids: Vec<SolarSystemId>,
    pub hq_solar_system_id: SolarSystemId,
    pub influence: f64,
    pub state: String,
    pub mothership_spawned: bool,
    pub mothership_defeated: bool,
    pub sites: Vec<IncursionSiteEntry>,
    #[serde(default)]
    pub lockout_until_filetime: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncursionLPLogEntry {
    pub log_id: i64,
    pub character_id: CharacterId,
    pub site_id: String,
    pub tier: String,
    pub constellation_id: ConstellationId,
    pub solar_system_id: SolarSystemId,
    pub isk_earned: i64,
    pub concord_lp_earned: i64,
    pub fleet_size: usize,
    pub fleet_scale_factor: f64,
    pub timestamp_filetime: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncursionSystemEffects {
    pub constellation_id: ConstellationId,
    pub influence: f64,
    pub cyno_jammed: bool,
    pub shield_penalty: f64,
    pub armor_penalty: f64,
    pub capacitor_penalty: f64,
    pub damage_penalty: f64,
}

#[allow(clippy::too_many_arguments)]
pub fn make_incursion_site(
    const_id: ConstellationId,
    dungeon_id: u32,
    dungeon_name: &str,
    solar_system_id: SolarSystemId,
    tier: &str,
    min_pilots: usize,
    max_optimal_pilots: usize,
    base_isk: i64,
    base_concord_lp: i64,
    influence_delta: f64,
    locked_until_low_influence: bool,
    position: DVec3,
) -> IncursionSiteEntry {
    IncursionSiteEntry {
        site_id: format!("INC-{const_id}-{dungeon_id}"),
        dungeon_id,
        dungeon_name: dungeon_name.into(),
        solar_system_id,
        tier: tier.into(),
        min_pilots,
        max_optimal_pilots,
        base_isk,
        base_concord_lp,
        influence_delta,
        locked_until_low_influence,
        completed_count: 0,
        boss_npc_ball_id: 1_896_000_000 + (const_id as i64) * 100 + (dungeon_id as i64),
        position,
        boss_destroyed: false,
        killer_char_id: None,
        killer_fleet_id: None,
    }
}

pub fn derive_incursion_site_position(sde: &SdeRegistry, sys_id: SolarSystemId, site_idx: usize) -> DVec3 {
    let mut cels = sde.celestials_in_system(sys_id);
    cels.retain(|c| c.position.length_squared() > 1_000_000.0);
    cels.sort_by_key(|c| c.celestial_id);
    if !cels.is_empty() {
        let cel = &cels[site_idx % cels.len()];
        let base_radius = if cel.radius > 0.0 { cel.radius } else { 50_000.0 };
        let safe_dist = base_radius + 150_000.0;
        let dir = match site_idx % 4 {
            0 => DVec3::new(1.0, 0.1, -0.2).normalize(),
            1 => DVec3::new(-1.0, 0.2, 0.4).normalize(),
            2 => DVec3::new(0.2, -1.0, 0.6).normalize(),
            _ => DVec3::new(-0.4, 0.4, 1.0).normalize(),
        };
        cel.position + dir * safe_dist
    } else {
        DVec3::new(100_000_000.0, 50_000_000.0, -80_000_000.0)
    }
}

pub fn seed_incursion_for_constellation(
    ctx: &ServerContext,
    reg: &mut PveSpecialSitesRegistry,
    constellation_id: ConstellationId,
) {
    let tale_id = 800_000 + (constellation_id as i64);
    let constellation_name = ctx
        .sde
        .constellations
        .get(&constellation_id)
        .map(|c| c.name.clone())
        .unwrap_or_else(|| format!("Constellation-{constellation_id}"));

    let mut sys_list: Vec<SolarSystemId> = ctx
        .sde
        .solar_systems
        .values()
        .filter(|s| s.constellation_id == constellation_id)
        .map(|s| s.solar_system_id)
        .collect();
    sys_list.sort_unstable();

    let stg = sys_list.first().copied().unwrap_or(30_000_140);
    let hq = sys_list.last().copied().unwrap_or(stg);
    let vg = if sys_list.len() > 2 {
        vec![sys_list[1], sys_list.get(2).copied().unwrap_or(sys_list[1])]
    } else {
        vec![stg]
    };
    let as_s = if sys_list.len() > 4 {
        vec![sys_list[3], sys_list[4]]
    } else {
        vec![stg]
    };
    let (staging_solar_system_id, vanguard_solar_system_ids, assault_solar_system_ids, hq_solar_system_id) =
        (stg, vg, as_s, hq);

    let vg_sys = vanguard_solar_system_ids[0];
    let as_sys = assault_solar_system_ids[0];

    let sites = vec![
        make_incursion_site(
            constellation_id, 5164, "Override Transfer Array (Staging)", staging_solar_system_id,
            "Staging", 3, 5, 3_500_000, 400, 0.05, false,
            derive_incursion_site_position(&ctx.sde, staging_solar_system_id, 0),
        ),
        make_incursion_site(
            constellation_id, 5165, "Nation Mining Colony", vg_sys,
            "Vanguard", 5, 10, 10_395_000, 1_400, 0.05, false,
            derive_incursion_site_position(&ctx.sde, vg_sys, 1),
        ),
        make_incursion_site(
            constellation_id, 5168, "Nation Commander Stronghold", as_sys,
            "Assault", 10, 20, 18_200_000, 3_500, 0.20, false,
            derive_incursion_site_position(&ctx.sde, as_sys, 2),
        ),
        make_incursion_site(
            constellation_id, 5175, "True Creations Research Center", hq_solar_system_id,
            "Headquarters", 20, 40, 31_500_000, 7_000, 0.25, false,
            derive_incursion_site_position(&ctx.sde, hq_solar_system_id, 3),
        ),
        make_incursion_site(
            constellation_id, 5158, "The Kundalini Manifest", hq_solar_system_id,
            "Mothership", 40, 80, 63_000_000, 14_000, 1.0, true,
            derive_incursion_site_position(&ctx.sde, hq_solar_system_id, 4),
        ),
    ];

    reg.incursions.insert(
        constellation_id,
        IncursionTaleRecord {
            tale_id,
            constellation_id,
            constellation_name,
            staging_solar_system_id,
            vanguard_solar_system_ids,
            assault_solar_system_ids,
            hq_solar_system_id,
            influence: 1.0,
            state: "Establishing".into(),
            mothership_spawned: false,
            mothership_defeated: false,
            sites,
            lockout_until_filetime: None,
        },
    );
}

pub fn get_incursion_effects_for_system(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
) -> Option<IncursionSystemEffects> {
    let const_id = ctx.sde.solar_systems.get(&solar_system_id).map(|s| s.constellation_id)?;
    with_pve_special_reg(ctx, |reg| {
        let inc = reg.incursions.get(&const_id)?;
        if inc.influence <= 0.0 || inc.state == "Ended" {
            return None;
        }
        let inf = inc.influence;
        Some(IncursionSystemEffects {
            constellation_id: const_id,
            influence: inf,
            cyno_jammed: inf > 0.0,
            shield_penalty: 0.50 * inf,
            armor_penalty: 0.50 * inf,
            capacitor_penalty: 0.50 * inf,
            damage_penalty: 0.50 * inf,
        })
    })
}

#[derive(Debug, Clone)]
pub struct IncursionCompletionResult {
    pub site_id: String,
    pub tier: String,
    pub fleet_size: usize,
    pub fleet_scale_factor: f64,
    pub isk_per_pilot: i64,
    pub concord_lp_per_pilot: i64,
    pub influence: f64,
    pub state: String,
    pub mothership_spawned: bool,
    pub incursion_ended: bool,
}

pub async fn process_incursion_site_completion_and_payout_core(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    _sde: &Arc<SdeRegistry>,
    caller_char_id: Option<CharacterId>,
    caller_fleet_id: Option<i64>,
    site_sys: SolarSystemId,
    site_id: &str,
    candidate_pilots: Option<&[CharacterId]>,
) -> Result<IncursionCompletionResult, BridgeRpcError> {
    let now_unix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let now_ft = ((now_unix + 11_644_473_600) * 10_000_000) as i64;
    let incursion_found = with_pve_special_reg_db(db, now_unix, |reg| {
        for (&cid, inc) in reg.incursions.iter_mut() {
            if inc.state == "Ended" {
                continue;
            }
            for site in &mut inc.sites {
                let matches_id = site.site_id == site_id
                    || site.site_id.ends_with(site_id)
                    || site_id.ends_with(&format!("-{}", site.dungeon_id))
                    || site_id.ends_with(&site.dungeon_id.to_string());
                if site.solar_system_id == site_sys && matches_id {
                    if site.locked_until_low_influence {
                        if inc.influence > 0.0 || !inc.mothership_spawned {
                            return Err(rpc_err(
                                "MothershipSiteLocked",
                                format!(
                                    "Mothership HQ site {} is locked while Sansha influence ({:.2}) > 0.0",
                                    site.dungeon_name, inc.influence
                                ),
                            ));
                        }
                        if let Some(lockout) = inc.lockout_until_filetime {
                            if now_ft > lockout && !inc.mothership_defeated {
                                inc.state = "Withdrawing".into();
                                return Err(rpc_err(
                                    "MothershipSiteLocked",
                                    "Mothership HQ site is locked: lockout window has expired",
                                ));
                            }
                        }
                    }
                    if !site.boss_destroyed {
                        return Err(rpc_err(
                            "IncursionSiteNpcsRemaining",
                            format!("Incursion site {site_id} boss wave NPC has not been destroyed"),
                        ));
                    }
                    if let Some(killer_cid) = site.killer_char_id {
                        let allowed = caller_char_id == Some(killer_cid)
                            || (site.killer_fleet_id.is_some() && site.killer_fleet_id == caller_fleet_id);
                        if !allowed {
                            return Err(rpc_err(
                                "KillerAttributionMismatch",
                                format!("Character is not attributed killer or killer fleet for {site_id}"),
                            ));
                        }
                    }
                    return Ok((
                        cid,
                        site.solar_system_id,
                        site.boss_npc_ball_id,
                        site.site_id.clone(),
                        site.position,
                    ));
                }
            }
        }
        Err(rpc_err(
            "IncursionSiteNotFound",
            format!("Incursion site {site_id} not found in system {site_sys}"),
        ))
    })?;

    let (target_cid, target_sys, boss_ball_id, matched_site_id, site_pos) = incursion_found;

    let sys_handle = systems.get_or_spawn(target_sys);
    let ball_alive = sys_handle
        .get_ball_snapshot(boss_ball_id)
        .await
        .ok()
        .flatten()
        .is_some();
    if ball_alive {
        return Err(rpc_err(
            "IncursionSiteNpcsRemaining",
            format!("Incursion site {site_id} boss wave NPC {boss_ball_id} is still alive in system"),
        ));
    }

    // Determine the participating fleet ID
    let effective_fleet_id = caller_fleet_id.or_else(|| {
        caller_char_id.and_then(|cid| {
            db.with_snapshot(|s| s.characters.get(&cid).and_then(|c| c.fleet_id))
        })
    });

    // Determine raw pilots: candidate pilots or all undocked characters in site_sys
    let raw_pilots: Vec<CharacterId> = if let Some(p) = candidate_pilots {
        if p.is_empty() {
            caller_char_id.map(|c| vec![c]).unwrap_or_default()
        } else {
            p.to_vec()
        }
    } else {
        db.with_snapshot(|snap| {
            snap.characters
                .values()
                .filter(|c| c.solar_system_id == target_sys && c.station_id.is_none() && c.structure_id.is_none())
                .map(|c| c.character_id)
                .collect()
        })
    };

    let mut unique_pilots = raw_pilots;
    unique_pilots.sort_unstable();
    unique_pilots.dedup();

    // Verify each pilot: in space in site_sys, in the participating fleet, and on-grid with site
    let mut verified_pilots: Vec<CharacterId> = Vec::new();

    for pid in unique_pilots {
        let char_opt = db.with_snapshot(|snap| {
            snap.characters.get(&pid).cloned()
        });
        let Some(ch) = char_opt else { continue; };
        if ch.solar_system_id != target_sys || ch.station_id.is_some() || ch.structure_id.is_some() {
            continue;
        }

        // Fleet membership check:
        // If caller/contributor is in a fleet, rewarded pilot must share that fleet.
        // If caller/contributor is not in a fleet, only the caller themselves can be rewarded.
        // If no contributing fleet or killer is identified, unaffiliated third-parties receive no payout.
        if let Some(fid) = effective_fleet_id {
            if ch.fleet_id != Some(fid) {
                continue;
            }
        } else if let Some(cid) = caller_char_id {
            if pid != cid {
                continue;
            }
        } else {
            continue;
        }

        // On-grid check: active ship within 500 km of site_pos
        let ship_id = ch.active_ship_id;
        let mut on_grid = false;
        if ship_id > 0 {
            if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ship_id).await {
                on_grid = ball.position.distance(site_pos) <= 500_000.0;
            } else {
                let pos_opt = db.with_snapshot(|snap| {
                    snap.item_positions.get(&ship_id).map(|p| DVec3::new(p[0], p[1], p[2]))
                });
                if let Some(pos) = pos_opt {
                    on_grid = pos.distance(site_pos) <= 500_000.0;
                }
            }
        }
        if on_grid {
            verified_pilots.push(pid);
        }
    }

    // NF-INC-OFFGRID-KILL-01:
    // Even if verified_pilots is empty (e.g. off-grid killer or unattributed), advance/respawn wave
    // with 0 ISK / 0 LP payout so the site does not stall without a boss ball in space!

    let step_res = with_pve_special_reg_db(db, now_unix, |reg| {
        let inc = reg.incursions.get_mut(&target_cid).unwrap();
        let site_idx = inc
            .sites
            .iter()
            .position(|s| s.site_id == matched_site_id)
            .unwrap();
        if !inc.sites[site_idx].boss_destroyed {
            return None;
        }
        inc.sites[site_idx].boss_destroyed = false;
        inc.sites[site_idx].killer_char_id = None;
        inc.sites[site_idx].killer_fleet_id = None;
        inc.sites[site_idx].completed_count += 1;
        let (b_isk, b_lp, min_pilots, opt, s_tier, delta) = (
            inc.sites[site_idx].base_isk,
            inc.sites[site_idx].base_concord_lp,
            inc.sites[site_idx].min_pilots,
            inc.sites[site_idx].max_optimal_pilots,
            inc.sites[site_idx].tier.clone(),
            inc.sites[site_idx].influence_delta,
        );
        let (next_boss_ball, ms_to_spawn) = if s_tier == "Mothership" {
            inc.influence = 0.0;
            inc.mothership_defeated = true;
            inc.state = "Ended".into();
            (None, None)
        } else {
            inc.influence = (inc.influence - delta).max(0.0);
            let ms = if inc.influence <= 0.0 && !inc.mothership_spawned {
                inc.mothership_spawned = true;
                inc.state = "Mobilizing".into();
                inc.lockout_until_filetime = Some(now_ft + 864_000_000_000);
                inc.sites.iter().find(|s| s.tier == "Mothership").map(|s| (s.solar_system_id, s.boss_npc_ball_id, s.position))
            } else {
                None
            };
            inc.sites[site_idx].boss_npc_ball_id += 1_000;
            (Some(inc.sites[site_idx].boss_npc_ball_id), ms)
        };
        Some((
            b_isk,
            b_lp,
            min_pilots,
            opt,
            s_tier,
            inc.influence,
            inc.state.clone(),
            inc.mothership_spawned,
            inc.mothership_defeated,
            next_boss_ball,
            ms_to_spawn,
        ))
    });

    let Some(step_res) = step_res else {
        return Err(rpc_err(
            "IncursionSiteNpcsRemaining",
            format!("Incursion site {site_id} boss wave already completed or NPC still alive"),
        ));
    };

    let (
        base_isk,
        base_lp,
        min_pilots,
        max_opt,
        tier,
        new_influence,
        new_state,
        mothership_unlocked,
        incursion_ended,
        next_boss_ball,
        ms_to_spawn,
    ) = step_res;

    if let Some(new_bid) = next_boss_ball {
        let _ = sys_handle
            .spawn_ball(make_site_npc_ball(
                new_bid,
                10_962,
                site_pos,
            ))
            .await;
    }

    if let Some((ms_sys, ms_bid, ms_pos)) = ms_to_spawn {
        let ms_handle = systems.get_or_spawn(ms_sys);
        let _ = ms_handle.spawn_ball(make_site_npc_ball(ms_bid, 10_962, ms_pos)).await;
    }

    let (pilot_isk, pilot_lp, fleet_count, fleet_scale) = if verified_pilots.is_empty() {
        (0, 0, 0, 0.0)
    } else {
        let count = verified_pilots.len();
        let under_scale = if count < min_pilots {
            ((count as f64) / (min_pilots as f64)).clamp(0.20, 1.0)
        } else {
            1.0
        };
        let over_pilots = count.saturating_sub(max_opt);
        let over_scale = (1.0_f64 - (over_pilots as f64) * 0.12).clamp(0.15, 1.0);
        let scale = (over_scale * under_scale).clamp(0.15, 1.0);
        let isk = ((base_isk as f64) * scale).round() as i64;
        let lp = ((base_lp as f64) * scale).round() as i64;
        (isk, lp, count, scale)
    };
    let concord_corp_id: i64 = 1_000_125;

    let effective_site_id = if !site_id.is_empty() {
        site_id.to_string()
    } else {
        matched_site_id.clone()
    };

    db.with_mut_snapshot(|snap| {
        for &pid in &verified_pilots {
            let j_id = (snap.wallet_journal.len() as i64) + 950_000;
            if let Some(ch) = snap.characters.get_mut(&pid) {
                ch.balance = ch
                    .balance
                    .checked_add(IskCents::from_isk(pilot_isk))
                    .unwrap_or(ch.balance);
                let new_bal = ch.balance;
                *ch.loyalty_points.entry(concord_corp_id).or_insert(0) += pilot_lp;
                snap.wallet_journal.push(WalletJournalEntry {
                    entry_id: j_id,
                    character_id: pid,
                    entry_type: "CorporateRewardPayout".into(),
                    amount: IskCents::from_isk(pilot_isk),
                    balance_after: new_bal,
                    reason: format!("Sansha Incursion {tier} Payout ({effective_site_id})"),
                });
            }
        }
    });

    with_pve_special_reg_db(db, now_unix, |reg| {
        for &pid in &verified_pilots {
            let next_log_id =
                (reg.incursion_lp_logs.values().map(|l| l.len()).sum::<usize>() as i64) + 1;
            reg.incursion_lp_logs
                .entry(pid)
                .or_default()
                .push(IncursionLPLogEntry {
                    log_id: next_log_id,
                    character_id: pid,
                    site_id: effective_site_id.clone(),
                    tier: tier.clone(),
                    constellation_id: target_cid,
                    solar_system_id: target_sys,
                    isk_earned: pilot_isk,
                    concord_lp_earned: pilot_lp,
                    fleet_size: fleet_count,
                    fleet_scale_factor: fleet_scale,
                    timestamp_filetime: now_ft,
                });
        }
    });

    Ok(IncursionCompletionResult {
        site_id: effective_site_id,
        tier,
        fleet_size: fleet_count,
        fleet_scale_factor: fleet_scale,
        isk_per_pilot: pilot_isk,
        concord_lp_per_pilot: pilot_lp,
        influence: new_influence,
        state: new_state,
        mothership_spawned: mothership_unlocked,
        incursion_ended,
    })
}

pub async fn process_incursion_site_completion_and_payout(
    ctx: &ServerContext,
    caller_char_id: Option<CharacterId>,
    caller_fleet_id: Option<i64>,
    site_sys: SolarSystemId,
    site_id: &str,
    candidate_pilots: Option<&[CharacterId]>,
) -> Result<IncursionCompletionResult, BridgeRpcError> {
    process_incursion_site_completion_and_payout_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        caller_char_id,
        caller_fleet_id,
        site_sys,
        site_id,
        candidate_pilots,
    )
    .await
}

pub async fn check_and_complete_incursion_wave_server_side_core(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    solar_system_id: SolarSystemId,
    site_id: &str,
    killer_char_id: Option<CharacterId>,
) -> Result<IncursionCompletionResult, BridgeRpcError> {
    let killer_fleet_id = killer_char_id.and_then(|cid| {
        db.with_snapshot(|s| s.characters.get(&cid).and_then(|c| c.fleet_id))
    });
    process_incursion_site_completion_and_payout_core(
        systems,
        db,
        sde,
        killer_char_id,
        killer_fleet_id,
        solar_system_id,
        site_id,
        None,
    )
    .await
}

pub async fn check_and_complete_incursion_wave_server_side(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    site_id: &str,
    killer_char_id: Option<CharacterId>,
) -> Result<IncursionCompletionResult, BridgeRpcError> {
    check_and_complete_incursion_wave_server_side_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        solar_system_id,
        site_id,
        killer_char_id,
    )
    .await
}

/// Explicitly marks an incursion site boss as destroyed upon confirmed death event.
pub fn mark_incursion_boss_destroyed(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    boss_ball_id: i64,
) -> bool {
    with_pve_special_reg(ctx, |reg| {
        for inc in reg.incursions.values_mut() {
            if inc.state == "Ended" {
                continue;
            }
            for site in &mut inc.sites {
                if (solar_system_id == 0 || site.solar_system_id == solar_system_id)
                    && site.boss_npc_ball_id == boss_ball_id
                {
                    if site.locked_until_low_influence && (inc.influence > 0.0 || !inc.mothership_spawned) {
                        return false;
                    }
                    site.boss_destroyed = true;
                    site.killer_char_id = None;
                    site.killer_fleet_id = None;
                    return true;
                }
            }
        }
        false
    })
}

pub async fn on_incursion_boss_destroyed_core(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
    killer_char_id: Option<CharacterId>,
) -> Option<Result<IncursionCompletionResult, BridgeRpcError>> {
    let killer_fleet_id = killer_char_id.and_then(|cid| {
        db.with_snapshot(|s| s.characters.get(&cid).and_then(|c| c.fleet_id))
    });
    let now_sec = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
    let site_found = with_pve_special_reg_db(db, now_sec, |reg| {
        for inc in reg.incursions.values_mut() {
            if inc.state == "Ended" {
                continue;
            }
            for site in &mut inc.sites {
                if site.solar_system_id == solar_system_id && site.boss_npc_ball_id == npc_ball_id {
                    let locked = site.locked_until_low_influence && (inc.influence > 0.0 || !inc.mothership_spawned);
                    if locked {
                        return Some((site.site_id.clone(), true));
                    }
                    site.boss_destroyed = true;
                    site.killer_char_id = killer_char_id;
                    site.killer_fleet_id = killer_fleet_id;
                    return Some((site.site_id.clone(), false));
                }
            }
        }
        None
    });

    let (site_id, is_locked) = site_found?;
    if is_locked {
        return Some(Err(rpc_err(
            "MothershipSiteLocked",
            "Mothership HQ site is locked while Sansha influence > 0.0",
        )));
    }
    Some(
        check_and_complete_incursion_wave_server_side_core(
            systems,
            db,
            sde,
            solar_system_id,
            &site_id,
            killer_char_id,
        )
        .await,
    )
}

pub async fn on_incursion_boss_destroyed(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    npc_ball_id: i64,
    killer_char_id: Option<CharacterId>,
) -> Option<Result<IncursionCompletionResult, BridgeRpcError>> {
    on_incursion_boss_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        solar_system_id,
        npc_ball_id,
        killer_char_id,
    )
    .await
}

pub async fn handle_incursion_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // Non-client RPC arms return UnimplementedRPC
        ("incursion", "UpdateIncursionLPLog") => {
            Some(Err(BridgeRpcError {
                code: "UnimplementedRPC".into(),
                message: "incursion.UpdateIncursionLPLog is not implemented".into(),
            }))
        }

        // parity: feat:pve.incursion-system, feat:pve.incursion-mothership
        ("incursion", "GetActiveIncursionData" | "GetAllIncursions" | "GetIncursionGlobalReport") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let list = with_pve_special_reg(ctx, |reg| {
                reg.incursions
                    .values()
                    .map(|inc| {
                        let sites_py: Vec<PyRep> = inc
                            .sites
                            .iter()
                            .map(|s| {
                                let available = !s.locked_until_low_influence || inc.mothership_spawned;
                                PyRep::keyval([
                                    ("siteID", PyRep::String(s.site_id.clone())),
                                    ("dungeonID", PyRep::Integer(s.dungeon_id as i64)),
                                    ("dungeonName", PyRep::String(s.dungeon_name.clone())),
                                    ("solarSystemID", PyRep::Integer(s.solar_system_id as i64)),
                                    ("tier", PyRep::String(s.tier.clone())),
                                    ("minPilots", PyRep::Integer(s.min_pilots as i64)),
                                    ("maxOptimalPilots", PyRep::Integer(s.max_optimal_pilots as i64)),
                                    ("baseIsk", PyRep::Integer(s.base_isk)),
                                    ("baseConcordLP", PyRep::Integer(s.base_concord_lp)),
                                    ("bossNpcBallID", PyRep::Integer(s.boss_npc_ball_id)),
                                    ("available", PyRep::Bool(available)),
                                ])
                            })
                            .collect();
                        PyRep::keyval([
                            ("taleID", PyRep::Integer(inc.tale_id)),
                            ("constellationID", PyRep::Integer(inc.constellation_id as i64)),
                            ("constellationName", PyRep::String(inc.constellation_name.clone())),
                            ("stagingSolarSystemID", PyRep::Integer(inc.staging_solar_system_id as i64)),
                            ("hqSolarSystemID", PyRep::Integer(inc.hq_solar_system_id as i64)),
                            ("influence", PyRep::Real(inc.influence)),
                            ("state", PyRep::String(inc.state.clone())),
                            ("mothershipSpawned", PyRep::Bool(inc.mothership_spawned)),
                            ("mothershipDefeated", PyRep::Bool(inc.mothership_defeated)),
                            ("sites", PyRep::List(sites_py)),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(list)))
        }

        // parity: feat:pve.incursion-system, feat:pve.incursion-mothership
        ("incursion", "GetAllIncursionSites") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let const_id = args
                .first()
                .and_then(|a| a.as_i64())
                .map(|v| v as ConstellationId)
                .unwrap_or(20_000_001);
            let res = with_pve_special_reg(ctx, |reg| {
                let Some(inc) = reg.incursions.get(&const_id) else {
                    return Err(rpc_err(
                        "IncursionNotFound",
                        format!("No active incursion in constellation {const_id}"),
                    ));
                };
                let sites_py: Vec<PyRep> = inc
                    .sites
                    .iter()
                    .map(|s| {
                        let available = !s.locked_until_low_influence || inc.mothership_spawned;
                        PyRep::keyval([
                            ("siteID", PyRep::String(s.site_id.clone())),
                            ("dungeonID", PyRep::Integer(s.dungeon_id as i64)),
                            ("dungeonName", PyRep::String(s.dungeon_name.clone())),
                            ("solarSystemID", PyRep::Integer(s.solar_system_id as i64)),
                            ("tier", PyRep::String(s.tier.clone())),
                            ("minPilots", PyRep::Integer(s.min_pilots as i64)),
                            ("maxOptimalPilots", PyRep::Integer(s.max_optimal_pilots as i64)),
                            ("baseIsk", PyRep::Integer(s.base_isk)),
                            ("baseConcordLP", PyRep::Integer(s.base_concord_lp)),
                            ("bossNpcBallID", PyRep::Integer(s.boss_npc_ball_id)),
                            ("available", PyRep::Bool(available)),
                        ])
                    })
                    .collect();
                Ok(PyRep::keyval([
                    ("taleID", PyRep::Integer(inc.tale_id)),
                    ("constellationID", PyRep::Integer(inc.constellation_id as i64)),
                    ("influence", PyRep::Real(inc.influence)),
                    ("state", PyRep::String(inc.state.clone())),
                    ("mothershipSpawned", PyRep::Bool(inc.mothership_spawned)),
                    ("mothershipDefeated", PyRep::Bool(inc.mothership_defeated)),
                    ("sites", PyRep::List(sites_py)),
                ]))
            });
            Some(res)
        }

        // parity: feat:pve.incursion-system
        ("incursion", "GetIncursionSystemEffects" | "GetIncursionPenalties") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let sys_id = args
                .first()
                .and_then(|a| a.as_i64())
                .map(|v| v as SolarSystemId)
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);
            let effects = get_incursion_effects_for_system(ctx, sys_id);
            match effects {
                Some(eff) => Some(Ok(PyRep::keyval([
                    ("constellationID", PyRep::Integer(eff.constellation_id as i64)),
                    ("influence", PyRep::Real(eff.influence)),
                    ("cynoJammed", PyRep::Bool(eff.cyno_jammed)),
                    ("shieldPenalty", PyRep::Real(eff.shield_penalty)),
                    ("armorPenalty", PyRep::Real(eff.armor_penalty)),
                    ("capacitorPenalty", PyRep::Real(eff.capacitor_penalty)),
                    ("damagePenalty", PyRep::Real(eff.damage_penalty)),
                ]))),
                None => Some(Ok(PyRep::keyval([
                    ("constellationID", PyRep::Integer(0)),
                    ("influence", PyRep::Real(0.0)),
                    ("cynoJammed", PyRep::Bool(false)),
                    ("shieldPenalty", PyRep::Real(0.0)),
                    ("armorPenalty", PyRep::Real(0.0)),
                    ("capacitorPenalty", PyRep::Real(0.0)),
                    ("damagePenalty", PyRep::Real(0.0)),
                ]))),
            }
        }

        // parity: feat:pve.incursion-payout
        ("incursion", "GetIncursionLPLog") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let logs_py = with_pve_special_reg(ctx, |reg| {
                reg.incursion_lp_logs
                    .get(&char_id)
                    .map(|entries| {
                        entries
                            .iter()
                            .map(|e| {
                                PyRep::keyval([
                                    ("logID", PyRep::Integer(e.log_id)),
                                    ("siteID", PyRep::String(e.site_id.clone())),
                                    ("tier", PyRep::String(e.tier.clone())),
                                    ("constellationID", PyRep::Integer(e.constellation_id as i64)),
                                    ("solarSystemID", PyRep::Integer(e.solar_system_id as i64)),
                                    ("iskEarned", PyRep::Integer(e.isk_earned)),
                                    ("concordLpEarned", PyRep::Integer(e.concord_lp_earned)),
                                    ("fleetSize", PyRep::Integer(e.fleet_size as i64)),
                                    ("fleetScaleFactor", PyRep::Real(e.fleet_scale_factor)),
                                    ("timestamp", PyRep::Integer(e.timestamp_filetime)),
                                ])
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default()
            });
            Some(Ok(PyRep::List(logs_py)))
        }

        _ => None,
    }
}
