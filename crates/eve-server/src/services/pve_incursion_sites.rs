//! `pve_incursion_sites` — Sansha Incursions (Constellation Influence, Site Tiers, Fleet ISK+LP Payouts,
//! Mothership HQ Finale).
//!
// parity: feat:pve.incursion-system, feat:pve.incursion-payout, feat:pve.incursion-mothership

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_special_sites::{with_pve_special_reg, PveSpecialSitesRegistry};
use eve_db::{IskCents, WalletJournalEntry};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::SolarSystemId;
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

type CharacterId = i64;
pub type ConstellationId = u32;

fn rpc_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.into(),
        message: message.into(),
    }
}

fn make_site_npc_ball(id: i64, type_id: u32, position: DVec3) -> Ball {
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

    let (staging_solar_system_id, vanguard_solar_system_ids, assault_solar_system_ids, hq_solar_system_id) =
        if constellation_id == 20_000_020 {
            (
                30_000_140,
                vec![30_000_142, 30_000_144],
                vec![30_000_139, 30_000_141],
                30_000_145,
            )
        } else {
            let stg = sys_list.first().copied().unwrap_or(30_000_140);
            let hq = sys_list.last().copied().unwrap_or(30_000_145);
            let vg = if sys_list.len() > 2 {
                vec![
                    sys_list[1],
                    sys_list.get(2).copied().unwrap_or(sys_list[1]),
                ]
            } else {
                vec![stg]
            };
            let as_s = if sys_list.len() > 4 {
                vec![sys_list[3], sys_list[4]]
            } else {
                vec![stg]
            };
            (stg, vg, as_s, hq)
        };

    let vg_sys = vanguard_solar_system_ids[0];
    let as_sys = assault_solar_system_ids[0];

    let sites = vec![
        make_incursion_site(
            constellation_id,
            5164,
            "Override Transfer Array (Staging)",
            staging_solar_system_id,
            "Staging",
            3,
            5,
            3_500_000,
            400,
            0.05,
            false,
        ),
        make_incursion_site(
            constellation_id,
            5165,
            "Nation Mining Colony",
            vg_sys,
            "Vanguard",
            5,
            10,
            10_395_000,
            1_400,
            0.05,
            false,
        ),
        make_incursion_site(
            constellation_id,
            5168,
            "Nation Commander Stronghold",
            as_sys,
            "Assault",
            10,
            20,
            18_200_000,
            3_500,
            0.20,
            false,
        ),
        make_incursion_site(
            constellation_id,
            5175,
            "True Creations Research Center",
            hq_solar_system_id,
            "Headquarters",
            20,
            40,
            31_500_000,
            7_000,
            0.25,
            false,
        ),
        make_incursion_site(
            constellation_id,
            5158,
            "The Kundalini Manifest",
            hq_solar_system_id,
            "Mothership",
            40,
            80,
            63_000_000,
            14_000,
            1.0,
            true,
        ),
    ];

    for s in &sites {
        let sys_handle = ctx.systems.get_or_spawn(s.solar_system_id);
        let ball = make_site_npc_ball(s.boss_npc_ball_id, 10_962, DVec3::new(10_000.0, 0.0, 0.0));
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let _ = sys_handle.spawn_ball(ball).await;
            });
        }
    }

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

pub async fn handle_incursion_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // parity: feat:pve.incursion-system, feat:pve.incursion-mothership
        ("incursion", "GetActiveIncursionData" | "GetAllIncursions" | "GetIncursionGlobalReport")
        | ("incursionSvc", "GetActiveIncursionData")
        | ("taleMgr", "GetActiveIncursionData") => {
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
        ("incursion", "GetAllIncursionSites" | "GetIncursionDungeons" | "GetIncursionState" | "GetSystemsInIncursions" | "JoinIncursionChat") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let const_id = args
                .first()
                .and_then(|a| a.as_i64())
                .map(|v| v as ConstellationId)
                .unwrap_or(20_000_020);
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

        // parity: feat:pve.incursion-payout, feat:pve.incursion-mothership
        ("incursion", "UpdateIncursionLPLog") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let site_id = args
                .first()
                .and_then(|a| a.as_str())
                .unwrap_or("INC-VG-5165")
                .to_string();
            let raw_pilots: Vec<CharacterId> = if let Some(PyRep::List(list)) = args.get(1) {
                let parsed: Vec<CharacterId> = list.iter().filter_map(|p| p.as_i64()).collect();
                if parsed.is_empty() {
                    vec![char_id]
                } else {
                    parsed
                }
            } else {
                vec![char_id]
            };

            let caller_sys = session.solar_system_id.unwrap_or(0);
            let incursion_found = with_pve_special_reg(ctx, |reg| {
                for (&cid, inc) in reg.incursions.iter() {
                    if inc.state == "Ended" {
                        continue;
                    }
                    for site in &inc.sites {
                        let matches_id = site.site_id == site_id
                            || site.site_id.ends_with(&site_id)
                            || site_id.ends_with(&format!("-{}", site.dungeon_id))
                            || site_id.ends_with(&site.dungeon_id.to_string());
                        if site.solar_system_id == caller_sys && matches_id {
                            if site.locked_until_low_influence && !inc.mothership_spawned {
                                return Err(rpc_err(
                                    "MothershipSiteLocked",
                                    format!(
                                        "Mothership HQ site {} is locked while Sansha influence ({:.2}) > 0.25",
                                        site.dungeon_name, inc.influence
                                    ),
                                ));
                            }
                            return Ok((
                                cid,
                                site.solar_system_id,
                                site.boss_npc_ball_id,
                                site.site_id.clone(),
                            ));
                        }
                    }
                }
                Err(rpc_err(
                    "IncursionSiteNotFound",
                    format!("Incursion site {site_id} not found in system {caller_sys}"),
                ))
            });
            let (target_cid, site_sys, boss_ball_id, matched_site_id) = match incursion_found {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            if session.solar_system_id != Some(site_sys) {
                return Some(Err(rpc_err(
                    "NotInIncursionSiteSystem",
                    format!("Caller must be in incursion site system {site_sys} to complete {site_id}"),
                )));
            }
            let sys_handle = ctx.systems.get_or_spawn(site_sys);
            if sys_handle
                .get_ball_snapshot(boss_ball_id)
                .await
                .ok()
                .flatten()
                .is_some()
            {
                return Some(Err(rpc_err(
                    "IncursionSiteNpcsRemaining",
                    format!("Incursion site {site_id} boss wave NPC {boss_ball_id} has not been destroyed"),
                )));
            }

            let step_res = with_pve_special_reg(ctx, |reg| {
                let inc = reg.incursions.get_mut(&target_cid).unwrap();
                let site = inc
                    .sites
                    .iter_mut()
                    .find(|s| s.site_id == matched_site_id)
                    .unwrap();
                site.completed_count += 1;
                let (b_isk, b_lp, min_pilots, opt, s_tier, delta) = (
                    site.base_isk,
                    site.base_concord_lp,
                    site.min_pilots,
                    site.max_optimal_pilots,
                    site.tier.clone(),
                    site.influence_delta,
                );
                let next_boss_ball = if s_tier == "Mothership" {
                    inc.influence = 0.0;
                    inc.mothership_defeated = true;
                    inc.state = "Ended".into();
                    None
                } else {
                    inc.influence = (inc.influence - delta).max(0.0);
                    if inc.influence <= 0.25 {
                        inc.mothership_spawned = true;
                        inc.state = "Mobilizing".into();
                    }
                    site.boss_npc_ball_id += 1_000;
                    Some(site.boss_npc_ball_id)
                };
                Ok((
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
                ))
            });
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
            ) = match step_res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            if let Some(new_bid) = next_boss_ball {
                let _ = sys_handle
                    .spawn_ball(make_site_npc_ball(
                        new_bid,
                        10_962,
                        DVec3::new(10_000.0, 0.0, 0.0),
                    ))
                    .await;
            }

            let mut unique_pilots: Vec<CharacterId> = raw_pilots;
            unique_pilots.sort_unstable();
            unique_pilots.dedup();

            let fleet_pilots: Vec<CharacterId> = ctx.db.with_snapshot(|snap| {
                unique_pilots
                    .into_iter()
                    .filter(|&pid| {
                        snap.characters.get(&pid).is_some_and(|ch| {
                            ch.solar_system_id == site_sys
                                && ch.station_id.is_none()
                                && ch.structure_id.is_none()
                        })
                    })
                    .collect()
            });

            let fleet_count = fleet_pilots.len().max(1);
            let under_scale = if fleet_count < min_pilots {
                ((fleet_count as f64) / (min_pilots as f64)).clamp(0.20, 1.0)
            } else {
                1.0
            };
            let over_pilots = fleet_count.saturating_sub(max_opt);
            let over_scale = (1.0_f64 - (over_pilots as f64) * 0.12).clamp(0.15, 1.0);
            let fleet_scale = (over_scale * under_scale).clamp(0.15, 1.0);
            let pilot_isk = ((base_isk as f64) * fleet_scale).round() as i64;
            let pilot_lp = ((base_lp as f64) * fleet_scale).round() as i64;
            let concord_corp_id: i64 = 1_000_125;

            ctx.db.with_mut_snapshot(|snap| {
                for &pid in &fleet_pilots {
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
                            reason: format!("Sansha Incursion {tier} Payout ({site_id})"),
                        });
                    }
                }
            });

            let now_ft = ctx.now_blueos_filetime_ticks();
            with_pve_special_reg(ctx, |reg| {
                for &pid in &fleet_pilots {
                    let next_log_id =
                        (reg.incursion_lp_logs.values().map(|l| l.len()).sum::<usize>() as i64) + 1;
                    reg.incursion_lp_logs
                        .entry(pid)
                        .or_default()
                        .push(IncursionLPLogEntry {
                            log_id: next_log_id,
                            character_id: pid,
                            site_id: site_id.clone(),
                            tier: tier.clone(),
                            constellation_id: target_cid,
                            solar_system_id: site_sys,
                            isk_earned: pilot_isk,
                            concord_lp_earned: pilot_lp,
                            fleet_size: fleet_count,
                            fleet_scale_factor: fleet_scale,
                            timestamp_filetime: now_ft,
                        });
                }
            });

            Some(Ok(PyRep::keyval([
                ("siteID", PyRep::String(site_id)),
                ("tier", PyRep::String(tier)),
                ("fleetSize", PyRep::Integer(fleet_count as i64)),
                ("fleetScaleFactor", PyRep::Real(fleet_scale)),
                ("iskPerPilot", PyRep::Integer(pilot_isk)),
                ("concordLpPerPilot", PyRep::Integer(pilot_lp)),
                ("influence", PyRep::Real(new_influence)),
                ("state", PyRep::String(new_state)),
                ("mothershipSpawned", PyRep::Bool(mothership_unlocked)),
                ("incursionEnded", PyRep::Bool(incursion_ended)),
            ])))
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
