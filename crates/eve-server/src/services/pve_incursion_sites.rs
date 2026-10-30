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
pub struct IncursionNpcSpawn {
    pub npc_ball_id: i64,
    pub type_id: u32,
    pub offset: [f64; 3],
    #[serde(default)]
    pub destroyed: bool,
}

fn default_boss_type_id() -> u32 {
    2933
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
    #[serde(default = "default_boss_type_id")]
    pub boss_type_id: u32,
    #[serde(default)]
    pub position: DVec3,
    #[serde(default)]
    pub escort_spawns: Vec<IncursionNpcSpawn>,
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
    /// Set once the tale `StoppedNotice` for this incursion's end has been published, so the
    /// NPC death hook and the ctx-level completion wrappers can never announce the stop twice.
    #[serde(default)]
    pub tale_stop_published: bool,
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

pub fn get_sansha_encounter_composition(
    boss_ball_id: i64,
    dungeon_id: u32,
    tier: &str,
) -> (u32, Vec<IncursionNpcSpawn>) {
    match tier {
        "Mothership" | "Boss" => {
            // The Kundalini Manifest (type_id 2192: Incursion Sansha's Nation Capital supercarrier) + multi-wave encounter
            let boss_type = 2192;
            let escorts = vec![
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 1, type_id: 2845, offset: [-3_500.0, 1_200.0, 500.0], destroyed: false }, // Outuni Mesen (BS)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 2, type_id: 2933, offset: [3_200.0, -1_500.0, -800.0], destroyed: false }, // Deltole Tegmentum (BS)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 3, type_id: 2936, offset: [-1_800.0, 2_400.0, 1_100.0], destroyed: false }, // Auga Hypophysis (Cruiser)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 4, type_id: 2859, offset: [2_100.0, 1_800.0, -600.0], destroyed: false }, // Romi Thalamus (Cruiser)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 5, type_id: 2909, offset: [-900.0, -800.0, 1_200.0], destroyed: false }, // Niarja Myelen (Frigate)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 6, type_id: 2907, offset: [850.0, 750.0, -950.0], destroyed: false }, // Schmaeel Medulla (Frigate)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 7, type_id: 2939, offset: [-600.0, 1_100.0, -700.0], destroyed: false }, // Tama Cerebellum (Frigate)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 8, type_id: 2935, offset: [4_500.0, 2_000.0, 1_500.0], destroyed: false }, // Yulai Crus Cerebi (BS)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 9, type_id: 3492, offset: [1_200.0, -2_200.0, 800.0], destroyed: false }, // Sansha's Nation Commander (Frigate)
            ];
            (boss_type, escorts)
        }
        "Headquarters" => {
            let boss_type = if dungeon_id == 5175 { 2845 } else { 2935 }; // Outuni Mesen or Yulai Crus Cerebi
            let escorts = vec![
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 1, type_id: 2933, offset: [2_500.0, 1_000.0, -500.0], destroyed: false }, // Deltole Tegmentum (BS)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 2, type_id: 2932, offset: [-2_200.0, -1_200.0, 700.0], destroyed: false }, // Ostingele Tectum (BS)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 3, type_id: 2936, offset: [1_500.0, 1_800.0, 400.0], destroyed: false }, // Auga Hypophysis (Cruiser)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 4, type_id: 2859, offset: [-1_400.0, 1_200.0, -800.0], destroyed: false }, // Romi Thalamus (Cruiser)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 5, type_id: 2909, offset: [700.0, -600.0, 500.0], destroyed: false }, // Niarja Myelen (Frigate)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 6, type_id: 2907, offset: [-800.0, 500.0, -600.0], destroyed: false }, // Schmaeel Medulla (Frigate)
            ];
            (boss_type, escorts)
        }
        "Assault" => {
            let boss_type = if dungeon_id == 5168 { 3492 } else { 2933 }; // Nation Commander or Deltole Tegmentum
            let escorts = vec![
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 1, type_id: 2845, offset: [-2_000.0, 1_500.0, 300.0], destroyed: false }, // Outuni Mesen (BS)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 2, type_id: 2936, offset: [1_800.0, -1_000.0, -400.0], destroyed: false }, // Auga Hypophysis (Cruiser)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 3, type_id: 2859, offset: [-1_200.0, -900.0, 600.0], destroyed: false }, // Romi Thalamus (Cruiser)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 4, type_id: 2909, offset: [600.0, 700.0, -500.0], destroyed: false }, // Niarja Myelen (Frigate)
            ];
            (boss_type, escorts)
        }
        "Vanguard" => {
            let boss_type = if dungeon_id == 5165 { 2933 } else { 3492 }; // Deltole Tegmentum or Nation Commander
            let escorts = vec![
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 1, type_id: 2936, offset: [1_200.0, 800.0, 200.0], destroyed: false }, // Auga Hypophysis (Cruiser)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 2, type_id: 2909, offset: [-600.0, -500.0, 400.0], destroyed: false }, // Niarja Myelen (Frigate)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 3, type_id: 2939, offset: [500.0, 600.0, -300.0], destroyed: false }, // Tama Cerebellum (Frigate)
            ];
            (boss_type, escorts)
        }
        _ => {
            let boss_type = 3492; // Sansha's Nation Commander
            let escorts = vec![
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 1, type_id: 2936, offset: [1_000.0, 600.0, 0.0], destroyed: false }, // Auga Hypophysis (Cruiser)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 2, type_id: 2859, offset: [-800.0, -600.0, 200.0], destroyed: false }, // Romi Thalamus (Cruiser)
                IncursionNpcSpawn { npc_ball_id: boss_ball_id * 10 + 3, type_id: 2190, offset: [400.0, 400.0, -200.0], destroyed: false }, // Renyn Meten (Frigate)
            ];
            (boss_type, escorts)
        }
    }
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
    let boss_npc_ball_id = 1_896_000_000 + ((const_id % 10_000) as i64) * 100 + (dungeon_id as i64);
    let (boss_type_id, escort_spawns) = get_sansha_encounter_composition(boss_npc_ball_id, dungeon_id, tier);
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
        boss_npc_ball_id,
        boss_type_id,
        position,
        escort_spawns,
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
    // DERIVED (synthetic — no tale IDs exist in the V24 client code or the SDE; live tale IDs are
    // allocated by CCP's tale service): one stable tale per constellation, 800_000 + constellation.
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
    let vg_sys2 = vanguard_solar_system_ids.get(1).copied().unwrap_or(vg_sys);
    let as_sys = assault_solar_system_ids[0];
    let as_sys2 = assault_solar_system_ids.get(1).copied().unwrap_or(as_sys);

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
        make_incursion_site(
            constellation_id, 5166, "Nation Commander Outpost", vg_sys2,
            "Vanguard", 5, 10, 10_395_000, 1_400, 0.05, false,
            derive_incursion_site_position(&ctx.sde, vg_sys2, 5),
        ),
        make_incursion_site(
            constellation_id, 5167, "Overwhelmed Civilian Facility", as_sys2,
            "Assault", 10, 20, 18_200_000, 3_500, 0.20, false,
            derive_incursion_site_position(&ctx.sde, as_sys2, 6),
        ),
        make_incursion_site(
            constellation_id, 5171, "Nation Rebirth Facility", hq_solar_system_id,
            "Headquarters", 20, 40, 31_500_000, 7_000, 0.25, false,
            derive_incursion_site_position(&ctx.sde, hq_solar_system_id, 7),
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
            tale_stop_published: false,
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

/// DERIVED (synthetic — the V24 client code and SDE contain no tale spawn-template table, so the
/// real Sansha incursion template ID is unknown): EvoMu reuses 5158, which in the client is the
/// dungeon ID of the Sansha mothership encounter (`talecommon/const.pyj` `INCURSION_INFO_BY_DUNGEONID`),
/// as the incursion tale's spawn-template identifier in `taleMgr` results and tale notices.
pub const INCURSION_TALE_TEMPLATE_ID: u64 = 5158;

/// Publishes `eve_public.tale.api.StartedNotice` / `StoppedNotice` for an incursion tale.
pub fn publish_incursion_tale_notice(
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    started: bool,
    tale_id: i64,
    staging_solar_system_id: SolarSystemId,
    now_unix: u64,
) {
    use crate::services::quasar_goal_tale_notices as notices;
    let payload = notices::tale_lifecycle(
        started,
        tale_id.max(0) as u64,
        INCURSION_TALE_TEMPLATE_ID,
        staging_solar_system_id as u64,
    );
    let _ = gateway.publish_notice(notices::tale_scope(), payload, now_unix);
}

/// When a site completion ended its incursion (mothership defeated), announces the tale stop.
pub fn publish_incursion_tale_stopped_if_ended(
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    db: &Arc<WorldRepository>,
    result: &IncursionCompletionResult,
) {
    if !result.incursion_ended {
        return;
    }
    let now = db.clock().now_unix_secs();
    let ended = with_pve_special_reg_db(db, now, |reg| {
        reg.incursions
            .values_mut()
            .find(|inc| inc.sites.iter().any(|s| s.site_id == result.site_id))
            .filter(|inc| !inc.tale_stop_published)
            .map(|inc| {
                // Claimed under the registry lock: only the first caller for this end publishes.
                inc.tale_stop_published = true;
                (inc.tale_id, inc.staging_solar_system_id)
            })
    });
    if let Some((tale_id, staging)) = ended {
        publish_incursion_tale_notice(gateway, false, tale_id, staging, now);
    }
}

pub async fn process_incursion_site_completion_and_payout_core(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    caller_char_id: Option<CharacterId>,
    caller_fleet_id: Option<i64>,
    site_sys: SolarSystemId,
    site_id: &str,
    candidate_pilots: Option<&[CharacterId]>,
) -> Result<IncursionCompletionResult, BridgeRpcError> {
    let now_unix = db.clock().now_unix_secs();
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
        let (b_isk, b_lp, min_pilots, opt, s_tier, delta, b_tid) = (
            inc.sites[site_idx].base_isk,
            inc.sites[site_idx].base_concord_lp,
            inc.sites[site_idx].min_pilots,
            inc.sites[site_idx].max_optimal_pilots,
            inc.sites[site_idx].tier.clone(),
            inc.sites[site_idx].influence_delta,
            inc.sites[site_idx].boss_type_id,
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
                inc.sites.iter().find(|s| s.tier == "Mothership").map(|s| {
                    let escorts = s.escort_spawns.iter().map(|e| (e.npc_ball_id, e.type_id, e.offset)).collect::<Vec<_>>();
                    (s.solar_system_id, s.boss_npc_ball_id, s.boss_type_id, s.position, escorts)
                })
            } else {
                None
            };
            inc.sites[site_idx].boss_npc_ball_id += 1_000;
            let next_boss_bid = inc.sites[site_idx].boss_npc_ball_id;
            let escorts = inc.sites[site_idx].escort_spawns.iter_mut().enumerate().map(|(i, esc)| {
                esc.npc_ball_id = next_boss_bid * 10 + (i as i64) + 1;
                esc.destroyed = false;
                (esc.npc_ball_id, esc.type_id, esc.offset)
            }).collect::<Vec<_>>();
            (Some((next_boss_bid, b_tid, escorts)), ms)
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

    if let Some((new_bid, new_tid, new_escorts)) = next_boss_ball {
        systems.claim_special_site_ball(target_sys, new_bid);
        let _ = sys_handle
            .spawn_ball_with_combat(
                make_site_npc_ball(new_bid, new_tid, site_pos),
                crate::services::pve_special_sites::build_special_site_npc_combat_state(sde, new_tid),
            )
            .await;
        for (esc_bid, esc_tid, esc_offset) in new_escorts {
            systems.claim_special_site_ball(target_sys, esc_bid);
            let esc_pos = site_pos + DVec3::new(esc_offset[0], esc_offset[1], esc_offset[2]);
            let _ = sys_handle
                .spawn_ball_with_combat(
                    make_site_npc_ball(esc_bid, esc_tid, esc_pos),
                    crate::services::pve_special_sites::build_special_site_npc_combat_state(sde, esc_tid),
                )
                .await;
        }
    }

    if let Some((ms_sys, ms_bid, ms_tid, ms_pos, ms_escorts)) = ms_to_spawn {
        let ms_handle = systems.get_or_spawn(ms_sys);
        systems.claim_special_site_ball(ms_sys, ms_bid);
        let _ = ms_handle
            .spawn_ball_with_combat(
                make_site_npc_ball(ms_bid, ms_tid, ms_pos),
                crate::services::pve_special_sites::build_special_site_npc_combat_state(sde, ms_tid),
            )
            .await;
        for (esc_bid, esc_tid, esc_offset) in ms_escorts {
            systems.claim_special_site_ball(ms_sys, esc_bid);
            let esc_pos = ms_pos + DVec3::new(esc_offset[0], esc_offset[1], esc_offset[2]);
            let _ = ms_handle
                .spawn_ball_with_combat(
                    make_site_npc_ball(esc_bid, esc_tid, esc_pos),
                    crate::services::pve_special_sites::build_special_site_npc_combat_state(sde, esc_tid),
                )
                .await;
        }
    }

    if incursion_ended {
        let escort_bids: Vec<i64> = with_pve_special_reg_db(db, now_unix, |reg| {
            reg.incursions.get(&target_cid).map(|inc| {
                inc.sites.iter()
                    .filter(|s| s.solar_system_id == target_sys)
                    .flat_map(|s| s.escort_spawns.iter().map(|e| e.npc_ball_id))
                    .collect()
            }).unwrap_or_default()
        });
        for bid in escort_bids {
            let _ = sys_handle.remove_ball(bid).await;
        }
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
        let f_scale = (over_scale * under_scale).clamp(0.15, 1.0);

        // Constellation influence scaling:
        // As Sansha influence decreases toward 0.0, payout scales up to 100% of base value.
        // At 1.0 influence (max Sansha control), payout penalty is up to 50% (0.50 scale).
        let influence_scale = (1.0 - (new_influence.clamp(0.0, 1.0) * 0.50)).clamp(0.50, 1.0);
        let total_scale = (f_scale * influence_scale).clamp(0.10, 1.0);

        let isk = ((base_isk as f64) * total_scale).round() as i64;
        let lp = ((base_lp as f64) * total_scale).round() as i64;
        (isk, lp, count, total_scale)
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
    let res = process_incursion_site_completion_and_payout_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        caller_char_id,
        caller_fleet_id,
        site_sys,
        site_id,
        candidate_pilots,
    )
    .await;
    if let Ok(r) = &res {
        publish_incursion_tale_stopped_if_ended(&ctx.quasar_gateway, &ctx.db, r);
        ctx.quasar_gateway.progression.extra.on_system_tale_encounter_completed(site_sys, 1, ctx.now_unix());
        if r.incursion_ended {
            let updated = ctx.quasar_gateway.progression.extra.record_tale_contribution(INCURSION_TALE_TEMPLATE_ID as u32, 1);
            if !updated.is_empty() {
                ctx.quasar_gateway.progression.extra.save_to_db();
            }
        }
    }
    res
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
    let now_sec = db.clock().now_unix_secs();
    let site_found = with_pve_special_reg_db(db, now_sec, |reg| {
        for inc in reg.incursions.values_mut() {
            if inc.state == "Ended" {
                continue;
            }
            for site in &mut inc.sites {
                if site.solar_system_id == solar_system_id && site.boss_npc_ball_id == npc_ball_id {
                    let locked = site.locked_until_low_influence && (inc.influence > 0.0 || !inc.mothership_spawned);
                    if locked {
                        return Some((site.site_id.clone(), true, false));
                    }
                    site.boss_destroyed = true;
                    site.killer_char_id = killer_char_id;
                    site.killer_fleet_id = killer_fleet_id;
                    return Some((site.site_id.clone(), false, false));
                }
                for esc in &mut site.escort_spawns {
                    if site.solar_system_id == solar_system_id && esc.npc_ball_id == npc_ball_id {
                        esc.destroyed = true;
                        return Some((site.site_id.clone(), false, true));
                    }
                }
            }
        }
        None
    });

    let (site_id, is_locked, is_escort) = site_found?;
    if is_locked {
        return Some(Err(rpc_err(
            "MothershipSiteLocked",
            "Mothership HQ site is locked while Sansha influence > 0.0",
        )));
    }
    if is_escort {
        return None;
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
    let res = on_incursion_boss_destroyed_core(
        &ctx.systems,
        &ctx.db,
        &ctx.sde,
        solar_system_id,
        npc_ball_id,
        killer_char_id,
    )
    .await;
    if let Some(Ok(r)) = &res {
        publish_incursion_tale_stopped_if_ended(&ctx.quasar_gateway, &ctx.db, r);
    }
    res
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

        // parity: feat:pve.incursion-system
        ("incursion", "UpdateIncursionLPLog") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            Some(Ok(PyRep::None))
        }

        // parity: feat:pve.incursion-system, rpc:taleMgr.GetGlobalWorldEventTales
        ("taleMgr", "GetGlobalWorldEventTales") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let list = with_pve_special_reg(ctx, |reg| {
                reg.incursions
                    .values()
                    .map(|inc| {
                        let mut scene_types_map: Vec<(PyRep, PyRep)> = Vec::new();
                        scene_types_map.push((PyRep::Integer(inc.staging_solar_system_id as i64), PyRep::String("incursionStaging".into())));
                        for &v_sys in &inc.vanguard_solar_system_ids {
                            scene_types_map.push((PyRep::Integer(v_sys as i64), PyRep::String("incursionLightInfestation".into())));
                        }
                        for &a_sys in &inc.assault_solar_system_ids {
                            scene_types_map.push((PyRep::Integer(a_sys as i64), PyRep::String("incursionMediumInfestation".into())));
                        }
                        scene_types_map.push((PyRep::Integer(inc.hq_solar_system_id as i64), PyRep::String("incursionHeavyInfestation".into())));

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
                            ("templateID", PyRep::Integer(5158)),
                            ("templateClassID", PyRep::String("worldEvents".into())),
                            ("managerSolarSystemID", PyRep::Integer(inc.staging_solar_system_id as i64)),
                            ("solar_system_id", PyRep::Integer(inc.staging_solar_system_id as i64)),
                            ("locationID", PyRep::Integer(inc.staging_solar_system_id as i64)),
                            ("constellationID", PyRep::Integer(inc.constellation_id as i64)),
                            ("constellationName", PyRep::String(inc.constellation_name.clone())),
                            ("influence", PyRep::Real(inc.influence)),
                            ("state", PyRep::String(inc.state.clone())),
                            ("aggressorFactionID", PyRep::Integer(500_019)),
                            ("templateNameID", PyRep::Integer(564035)),
                            ("templateDescriptionID", PyRep::Integer(564036)),
                            ("endTime", PyRep::Integer(inc.lockout_until_filetime.unwrap_or(0))),
                            ("hasFinalEncounter", PyRep::Bool(inc.mothership_spawned && !inc.mothership_defeated)),
                            ("mothershipSpawned", PyRep::Bool(inc.mothership_spawned)),
                            ("mothershipDefeated", PyRep::Bool(inc.mothership_defeated)),
                            ("sceneTypesByLocationID", PyRep::Dict(scene_types_map)),
                            ("sites", PyRep::List(sites_py)),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(list)))
        }

        // parity: feat:pve.incursion-system, rpc:taleMgr.get_active_tales_by_template
        ("taleMgr", "get_active_tales_by_template") => {
            if let Err(e) = ServerContext::require_character(session) {
                return Some(Err(e));
            }
            let template_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
            let now_sec = ctx.now_unix();
            let now_ft = ((now_sec + 11_644_473_600) * 10_000_000) as i64;
            if template_id != 5158 {
                let extra_tales: Vec<PyRep> = {
                    let tales = ctx.quasar_gateway.progression.extra.active_tales.read().unwrap_or_else(|e| e.into_inner());
                    tales
                        .iter()
                        .filter(|(_, (tpl_id, _, _))| (*tpl_id as i64) == template_id)
                        .map(|(&tale_id, &(tpl_id, sys_id, started))| {
                            let start_ft = ((started + 11_644_473_600) * 10_000_000) as i64;
                            PyRep::keyval([
                                ("tale_id", PyRep::Integer(tale_id as i64)),
                                ("template_id", PyRep::Integer(tpl_id as i64)),
                                ("solar_system_id", PyRep::Integer(sys_id as i64)),
                                ("start_date", PyRep::Integer(start_ft)),
                                ("end_date", PyRep::Integer(now_ft + 864_000_000_000)),
                            ])
                        })
                        .collect()
                };
                return Some(Ok(PyRep::List(extra_tales)));
            }
            let list = with_pve_special_reg(ctx, |reg| {
                reg.incursions
                    .values()
                    .filter(|inc| inc.state != "Ended")
                    .map(|inc| {
                        PyRep::keyval([
                            ("tale_id", PyRep::Integer(inc.tale_id)),
                            ("template_id", PyRep::Integer(template_id)),
                            ("solar_system_id", PyRep::Integer(inc.staging_solar_system_id as i64)),
                            ("start_date", PyRep::Integer(now_ft - 864_000_000_000)),
                            ("end_date", PyRep::Integer(inc.lockout_until_filetime.unwrap_or(now_ft + 864_000_000_000))),
                        ])
                    })
                    .collect::<Vec<_>>()
            });
            Some(Ok(PyRep::List(list)))
        }

        _ => None,
    }
}
