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

pub use eve_db::{
    IncursionLPLogEntry, IncursionNpcSpawn, IncursionSiteEntry, IncursionTaleRecord,
};

pub use crate::services::pve_incursion_lifecycle::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncursionSystemEffects {
    pub constellation_id: ConstellationId,
    pub effect_type_id: u32,
    pub influence: f64,
    pub cyno_jammed: bool,
    pub shield_penalty: f64,
    pub armor_penalty: f64,
    pub capacitor_penalty: f64,
    pub damage_penalty: f64,
    pub resistance_penalty: f64,
    pub bounty_penalty: f64,
    pub lp_penalty: f64,
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
/// `(min_pilots, max_optimal_pilots, max ISK, max CONCORD LP)` per wiki:Incursion_sites for the site tier and
/// the system's security band (high-sec `>= 0.45` vs low/null-sec). Staging sites pay the top five contributors.
pub fn incursion_site_payout_band(sde: &eve_sde::SdeRegistry, sys: SolarSystemId, tier: &str) -> (usize, usize, i64, i64) {
    let highsec = sde.solar_systems.get(&sys).is_some_and(|s| s.security_status >= 0.45);
    match (tier, highsec) {
        ("Staging", _) => (1, 5, 3_500_000, 400),
        ("Vanguard", true) => (5, 10, 10_395_000, 1_400),
        ("Vanguard", false) => (5, 15, 15_000_000, 2_000),
        ("Assault", true) => (1, 20, 18_200_000, 3_500),
        ("Assault", false) => (1, 30, 26_000_000, 5_000),
        ("Headquarters", true) => (20, 40, 31_500_000, 7_000),
        ("Headquarters", false) => (20, 60, 45_000_000, 10_000),
        (_, true) => (40, 80, 63_000_000, 14_000),
        (_, false) => (40, 120, 90_000_000, 20_000),
    }
}

/// Per-pilot payout ratio for `over` pilots above the site's maximum, from the wiki:Incursion_sites tables
/// (e.g. high-sec Vanguard 10,395,000 -> 9,615,375 / 8,783,775 / 7,900,200 ISK for 11 / 12 / 13 pilots).
/// Larger overstacks are not listed and pay nothing ("you end up not getting anything").
pub fn incursion_overstack_ratio(over: usize) -> f64 {
    match over {
        0 => 1.0,
        1 => 0.925,
        2 => 0.845,
        3 => 0.76,
        _ => 0.0,
    }
}

pub fn make_incursion_site(
    const_id: ConstellationId,
    dungeon_id: u32,
    dungeon_name: &str,
    solar_system_id: SolarSystemId,
    tier: &str,
    (min_pilots, max_optimal_pilots, base_isk, base_concord_lp): (usize, usize, i64, i64),
    influence_delta: f64,
    locked_until_low_influence: bool,
    position: DVec3,
) -> IncursionSiteEntry {
    let boss_npc_ball_id =
        1_896_000_000 + ((const_id % 10_000) as i64) * 100_000_000 + ((dungeon_id % 10_000) as i64) * 10_000;
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
        position: position.to_array(),
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
    let tale_id = crate::services::pve_incursion_sites_ops::allocate_persistent_incursion_tale_id(
        reg,
        constellation_id,
    );
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
        crate::services::pve_incursion_lifecycle::assign_constellation_system_roles(
            &ctx.sde,
            constellation_id,
            &sys_list,
        );

    let vg_sys = vanguard_solar_system_ids[0];
    let vg_sys2 = vanguard_solar_system_ids.get(1).copied().unwrap_or(vg_sys);
    let as_sys = assault_solar_system_ids[0];
    let as_sys2 = assault_solar_system_ids.get(1).copied().unwrap_or(as_sys);

    // Site payouts and pilot bands per wiki:Incursion_sites (high-sec vs low/null-sec); influence deltas DERIVED.
    let band = |sys: SolarSystemId, tier: &str| incursion_site_payout_band(&ctx.sde, sys, tier);
    let mut sites = vec![
        make_incursion_site(
            constellation_id, 5164, "Override Transfer Array (Staging)", staging_solar_system_id,
            "Staging", band(staging_solar_system_id, "Staging"), 0.05, false,
            derive_incursion_site_position(&ctx.sde, staging_solar_system_id, 0),
        ),
        make_incursion_site(
            constellation_id, 5165, "Nation Mining Colony", vg_sys,
            "Vanguard", band(vg_sys, "Vanguard"), 0.05, false,
            derive_incursion_site_position(&ctx.sde, vg_sys, 1),
        ),
        make_incursion_site(
            constellation_id, 5168, "Nation Commander Stronghold", as_sys,
            "Assault", band(as_sys, "Assault"), 0.20, false,
            derive_incursion_site_position(&ctx.sde, as_sys, 2),
        ),
        make_incursion_site(
            constellation_id, 5175, "True Creations Research Center", hq_solar_system_id,
            "Headquarters", band(hq_solar_system_id, "Headquarters"), 0.25, false,
            derive_incursion_site_position(&ctx.sde, hq_solar_system_id, 3),
        ),
        make_incursion_site(
            constellation_id, 5158, "The Kundalini Manifest", hq_solar_system_id,
            "Mothership", band(hq_solar_system_id, "Mothership"), 1.0, true,
            derive_incursion_site_position(&ctx.sde, hq_solar_system_id, 4),
        ),
        make_incursion_site(
            constellation_id, 5166, "Nation Commander Outpost", vg_sys2,
            "Vanguard", band(vg_sys2, "Vanguard"), 0.05, false,
            derive_incursion_site_position(&ctx.sde, vg_sys2, 5),
        ),
        make_incursion_site(
            constellation_id, 5167, "Overwhelmed Civilian Facility", as_sys2,
            "Assault", band(as_sys2, "Assault"), 0.20, false,
            derive_incursion_site_position(&ctx.sde, as_sys2, 6),
        ),
        make_incursion_site(
            constellation_id, 5171, "Nation Rebirth Facility", hq_solar_system_id,
            "Headquarters", band(hq_solar_system_id, "Headquarters"), 0.25, false,
            derive_incursion_site_position(&ctx.sde, hq_solar_system_id, 7),
        ),
    ];
    for s in &mut sites {
        if let Some((btid, esc)) = crate::services::pve_incursion_sites_ops::resolve_sansha_encounter_from_sde(
            &ctx.sde,
            s.boss_npc_ball_id,
            s.dungeon_id,
            &s.tier,
        ) {
            s.boss_type_id = btid;
            s.escort_spawns = esc;
        }
    }

    let now_sec = crate::services::pve_special_sites::spec_now_sec(reg);
    reg.incursion_lifecycle.insert(
        constellation_id,
        crate::services::pve_incursion_lifecycle::IncursionLifecycleMeta {
            spawned_at_sec: now_sec,
            last_site_completed_sec: now_sec,
            last_regen_sec: now_sec,
            withdraw_until_sec: None,
            ended_at_sec: None,
        },
    );
    for s in &sites {
        reg.incursion_site_waves.insert(
            s.site_id.clone(),
            crate::services::pve_incursion_lifecycle::IncursionSiteWaveMeta {
                current_wave: 1,
                total_waves: if s.tier == "Mothership" {
                    1
                } else if s.tier == "Staging" {
                    2
                } else {
                    3
                },
            },
        );
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
            lockout_until_filetime: None,
            tale_stop_published: false,
        },
    );
}

pub fn get_incursion_effects_for_system_in_snap(
    sde: &eve_sde::SdeRegistry,
    snap: &eve_db::WorldStateSnapshot,
    solar_system_id: SolarSystemId,
) -> Option<IncursionSystemEffects> {
    crate::services::pve_incursion_lifecycle::compute_incursion_system_effects_in_snap(
        sde,
        snap,
        solar_system_id,
    )
}

pub fn get_incursion_effects_for_system(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
) -> Option<IncursionSystemEffects> {
    crate::services::pve_incursion_lifecycle::compute_incursion_system_effects(ctx, solar_system_id)
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

pub use crate::services::pve_incursion_lifecycle::{
    notify_incursion_site_completed, publish_incursion_tale_notice,
    publish_incursion_tale_stopped_if_ended,
};

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
    let site_id = site_id.trim();
    if site_id.is_empty() {
        return Err(rpc_err(
            "IncursionSiteNotFound",
            "Incursion site_id cannot be empty",
        ));
    }
    let now_unix = db.clock().now_unix_secs();
    let now_ft = ((now_unix + 11_644_473_600) * 10_000_000) as i64;
    let incursion_found = with_pve_special_reg_db(db, now_unix, |reg| {
        for (&cid, inc) in reg.incursions.iter_mut() {
            if inc.state == "Ended" || inc.state == "Withdrawing" {
                continue;
            }
            for site in &mut inc.sites {
                let matches_id = site.site_id == site_id
                    || (!site_id.is_empty() && site.site_id.ends_with(site_id))
                    || site_id.ends_with(&format!("-{}", site.dungeon_id))
                    || site_id == site.dungeon_id.to_string();
                if site.solar_system_id == site_sys && matches_id {
                    if site.locked_until_low_influence {
                        if !inc.mothership_spawned || inc.influence > 0.10 {
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
                    if let Some(wm) = reg.incursion_site_waves.get(&site.site_id) {
                        if wm.current_wave < wm.total_waves {
                            return Err(rpc_err(
                                "IncursionSiteNpcsRemaining",
                                format!(
                                    "Incursion site {} is on wave {}/{}; clear all waves before site completion",
                                    site.site_id, wm.current_wave, wm.total_waves
                                ),
                            ));
                        }
                    }
                    if site.escort_spawns.iter().any(|e| !e.destroyed) {
                        return Err(rpc_err(
                            "IncursionSiteNpcsRemaining",
                            format!("Incursion site {} escort NPCs are still alive", site.site_id),
                        ));
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
                        DVec3::from_array(site.position),
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
        let sim_now = crate::services::pve_special_sites::spec_now_sec(reg);
        let inc = reg.incursions.get_mut(&target_cid).unwrap();
        let site_idx = inc
            .sites
            .iter()
            .position(|s| s.site_id == matched_site_id)
            .unwrap();
        if !inc.sites[site_idx].boss_destroyed
            || reg
                .incursion_site_waves
                .get(&inc.sites[site_idx].site_id)
                .is_some_and(|w| w.current_wave < w.total_waves)
            || inc.sites[site_idx].escort_spawns.iter().any(|e| !e.destroyed)
        {
            return None;
        }
        inc.sites[site_idx].boss_destroyed = false;
        inc.sites[site_idx].killer_char_id = None;
        inc.sites[site_idx].killer_fleet_id = None;
        inc.sites[site_idx].completed_count += 1;
        let stale_escorts: Vec<i64> = inc.sites[site_idx]
            .escort_spawns
            .iter()
            .filter(|e| !e.destroyed)
            .map(|e| e.npc_ball_id)
            .collect();
        let (b_isk, b_lp, min_pilots, opt, s_tier, delta, s_did) = (
            inc.sites[site_idx].base_isk,
            inc.sites[site_idx].base_concord_lp,
            inc.sites[site_idx].min_pilots,
            inc.sites[site_idx].max_optimal_pilots,
            inc.sites[site_idx].tier.clone(),
            inc.sites[site_idx].influence_delta,
            inc.sites[site_idx].dungeon_id,
        );
        let meta = reg
            .incursion_lifecycle
            .entry(target_cid)
            .or_insert_with(|| crate::services::pve_incursion_lifecycle::IncursionLifecycleMeta {
                spawned_at_sec: sim_now,
                last_site_completed_sec: sim_now,
                last_regen_sec: sim_now,
                withdraw_until_sec: None,
                ended_at_sec: None,
            });
        meta.last_site_completed_sec = sim_now;
        meta.last_regen_sec = sim_now;

        let pre_state = inc.state.clone();
        let (next_boss_ball, ms_to_spawn) = if s_tier == "Mothership" {
            inc.influence = 0.0;
            inc.mothership_defeated = true;
            inc.state = "Withdrawing".into();
            inc.lockout_until_filetime = Some(
                now_ft
                    + (crate::services::pve_incursion_lifecycle::INCURSION_WITHDRAWAL_DURATION_SEC
                        as i64)
                        * 10_000_000,
            );
            meta.withdraw_until_sec = Some(
                sim_now
                    + crate::services::pve_incursion_lifecycle::INCURSION_WITHDRAWAL_DURATION_SEC,
            );
            (None, None)
        } else {
            let next_inf = (inc.influence - delta).max(0.0);
            inc.influence = if next_inf <= 1e-6 { 0.0 } else { next_inf };
            let ms = if inc.influence <= 0.0 && !inc.mothership_spawned {
                inc.mothership_spawned = true;
                inc.state = "Mobilizing".into();
                inc.lockout_until_filetime = Some(now_ft + 864_000_000_000);
                inc.sites.iter().find(|s| s.tier == "Mothership").map(|s| {
                    let escorts = s.escort_spawns.iter().map(|e| (e.npc_ball_id, e.type_id, e.offset)).collect::<Vec<_>>();
                    (s.solar_system_id, s.boss_npc_ball_id, s.boss_type_id, DVec3::from_array(s.position), escorts)
                })
            } else {
                None
            };
            inc.sites[site_idx].boss_npc_ball_id += 1_000;
            let next_boss_bid = inc.sites[site_idx].boss_npc_ball_id;
            let (next_b_tid, fresh_escorts) =
                crate::services::pve_incursion_sites_ops::resolve_sansha_encounter_from_sde(
                    sde,
                    next_boss_bid,
                    s_did,
                    &s_tier,
                )
                .unwrap_or_else(|| get_sansha_encounter_composition(next_boss_bid, s_did, &s_tier));
            inc.sites[site_idx].boss_type_id = next_b_tid;
            let escorts: Vec<(i64, u32, [f64; 3])> = fresh_escorts
                .iter()
                .map(|e| (e.npc_ball_id, e.type_id, e.offset))
                .collect();
            inc.sites[site_idx].escort_spawns = fresh_escorts;
            let has_wave_tracker = reg.incursion_site_waves.contains_key(&inc.sites[site_idx].site_id);
            if has_wave_tracker {
                reg.incursion_site_waves.insert(
                    inc.sites[site_idx].site_id.clone(),
                    crate::services::pve_incursion_lifecycle::IncursionSiteWaveMeta {
                        current_wave: 1,
                        total_waves: if s_tier == "Staging" { 2 } else { 3 },
                    },
                );
            }
            (Some((next_boss_bid, next_b_tid, escorts, !has_wave_tracker)), ms)
        };
        Some((
            b_isk,
            b_lp,
            min_pilots,
            opt,
            s_tier,
            inc.influence,
            inc.state.clone(),
            pre_state,
            inc.mothership_spawned,
            inc.mothership_defeated,
            next_boss_ball,
            ms_to_spawn,
            stale_escorts,
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
        pre_state,
        mothership_unlocked,
        incursion_ended,
        next_boss_ball,
        ms_to_spawn,
        stale_escorts,
    ) = step_res;

    for bid in stale_escorts {
        let _ = sys_handle.remove_ball(bid).await;
    }

    if let Some((new_bid, new_tid, new_escorts, spawn_boss_now)) = next_boss_ball {
        if spawn_boss_now {
            systems.claim_special_site_ball(target_sys, new_bid);
            let _ = sys_handle
                .spawn_ball_with_combat(
                    make_site_npc_ball(new_bid, new_tid, site_pos),
                    super::pve_incursion_lifecycle::build_incursion_npc_combat_state(sde, new_tid),
                )
                .await;
        }
        for (esc_bid, esc_tid, esc_offset) in new_escorts {
            systems.claim_special_site_ball(target_sys, esc_bid);
            let esc_pos = site_pos + DVec3::new(esc_offset[0], esc_offset[1], esc_offset[2]);
            let _ = sys_handle
                .spawn_ball_with_combat(
                    make_site_npc_ball(esc_bid, esc_tid, esc_pos),
                    super::pve_incursion_lifecycle::build_incursion_npc_combat_state(sde, esc_tid),
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
                super::pve_incursion_lifecycle::build_incursion_npc_combat_state(sde, ms_tid),
            )
            .await;
        for (esc_bid, esc_tid, esc_offset) in ms_escorts {
            systems.claim_special_site_ball(ms_sys, esc_bid);
            let esc_pos = ms_pos + DVec3::new(esc_offset[0], esc_offset[1], esc_offset[2]);
            let _ = ms_handle
                .spawn_ball_with_combat(
                    make_site_npc_ball(esc_bid, esc_tid, esc_pos),
                    super::pve_incursion_lifecycle::build_incursion_npc_combat_state(sde, esc_tid),
                )
                .await;
        }
    }

    if incursion_ended {
        let (escort_bids, const_systems): (Vec<i64>, Vec<SolarSystemId>) =
            with_pve_special_reg_db(db, now_unix, |reg| {
                reg.incursions
                    .get(&target_cid)
                    .map(|inc| {
                        let bids = inc
                            .sites
                            .iter()
                            .filter(|s| s.solar_system_id == target_sys)
                            .flat_map(|s| s.escort_spawns.iter().map(|e| e.npc_ball_id))
                            .collect();
                        let mut sys_ids = vec![inc.staging_solar_system_id, inc.hq_solar_system_id];
                        sys_ids.extend_from_slice(&inc.vanguard_solar_system_ids);
                        sys_ids.extend_from_slice(&inc.assault_solar_system_ids);
                        (bids, sys_ids)
                    })
                    .unwrap_or_default()
            });
        for bid in escort_bids {
            let _ = sys_handle.remove_ball(bid).await;
        }
        db.with_mut_snapshot(|snap| {
            let stale: Vec<i64> = snap
                .items
                .iter()
                .filter_map(|(&iid, item)| {
                    if matches!(
                        item.type_id,
                        crate::services::pve_incursion_lifecycle::INCURSION_EFFECT_BEACON_VANGUARD
                            | crate::services::pve_incursion_lifecycle::INCURSION_EFFECT_BEACON_ASSAULT
                            | crate::services::pve_incursion_lifecycle::INCURSION_EFFECT_BEACON_HQ
                    ) && u32::try_from(item.location_id)
                        .is_ok_and(|sid| const_systems.contains(&sid))
                    {
                        Some(iid)
                    } else {
                        None
                    }
                })
                .collect();
            for iid in stale {
                snap.items.remove(&iid);
                snap.item_positions.remove(&iid);
            }
        });
    }

    // Payout scales only with on-grid fleet size (wiki:Incursion_sites); influence does not change payouts.
    let (pilot_isk, pilot_lp, fleet_count, fleet_scale) = if verified_pilots.is_empty() {
        (0, 0, 0, 0.0)
    } else {
        let count = verified_pilots.len();
        let eff_state = if tier == "Mothership" { "Establishing" } else { pre_state.as_str() };
        let scale =
            crate::services::pve_incursion_sites_ops::compute_incursion_fleet_payout_scale(
                count,
                min_pilots,
                max_opt,
                eff_state,
            );
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

    if pilot_isk > 0 || pilot_lp > 0 {
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
    }

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
        notify_incursion_site_completed(&ctx.quasar_gateway, &ctx.db, site_sys, r);
        crate::services::pve_incursion_lifecycle::emit_incursion_completion_macho_notifications(ctx, r);
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
                    if site.locked_until_low_influence && (!inc.mothership_spawned || inc.influence > 0.10) {
                        return false;
                    }
                    if reg
                        .incursion_site_waves
                        .get(&site.site_id)
                        .is_some_and(|w| w.current_wave < w.total_waves)
                        || site.escort_spawns.iter().any(|e| !e.destroyed)
                    {
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
                    let locked = site.locked_until_low_influence && (!inc.mothership_spawned || inc.influence > 0.10);
                    if locked {
                        return Some((site.site_id.clone(), true, false, false, None, None));
                    }
                    let earlier_waves_pending = reg
                        .incursion_site_waves
                        .get(&site.site_id)
                        .is_some_and(|w| w.current_wave < w.total_waves);
                    if earlier_waves_pending {
                        return Some((site.site_id.clone(), false, false, true, None, None));
                    }
                    let killed_tid = (!site.boss_destroyed).then_some(site.boss_type_id);
                    site.boss_destroyed = true;
                    site.killer_char_id = killer_char_id;
                    site.killer_fleet_id = killer_fleet_id;
                    if site.escort_spawns.iter().any(|e| !e.destroyed) {
                        return Some((site.site_id.clone(), false, false, true, None, killed_tid));
                    }
                    return Some((site.site_id.clone(), false, false, false, killer_char_id, killed_tid));
                }
                for esc in &mut site.escort_spawns {
                    if site.solar_system_id == solar_system_id && esc.npc_ball_id == npc_ball_id {
                        let killed_tid = (!esc.destroyed).then_some(esc.type_id);
                        esc.destroyed = true;
                        let waves_done = reg
                            .incursion_site_waves
                            .get(&site.site_id)
                            .is_none_or(|w| w.current_wave >= w.total_waves);
                        let all_escorts_dead = site.escort_spawns.iter().all(|e| e.destroyed);
                        if site.boss_destroyed && waves_done && all_escorts_dead {
                            return Some((
                                site.site_id.clone(),
                                false,
                                false,
                                false,
                                site.killer_char_id.or(killer_char_id),
                                killed_tid,
                            ));
                        }
                        return Some((site.site_id.clone(), false, true, false, None, killed_tid));
                    }
                }
            }
        }
        None
    });

    let (site_id, is_locked, is_escort, waves_pending, completion_killer_opt, killed_npc_tid) = site_found?;
    if let (Some(killer), Some(tid)) = (killer_char_id.filter(|&k| k > 0), killed_npc_tid) {
        let (bonus_pct, max_gain) = sde
            .types
            .get(&tid)
            .map(|t| {
                let b = t.default_attributes.get(&252).copied().unwrap_or(0.10);
                let cap = t.default_attributes.get(&563).copied().unwrap_or(5.0).min(5.0);
                (b, cap)
            })
            .unwrap_or((0.10, 5.0));
        db.with_mut_snapshot(|snap| {
            let Some(old_sec) = snap.characters.get(&killer).map(|c| c.security_status) else {
                return;
            };
            if old_sec < max_gain && bonus_pct > 0.0 {
                let gain = ((10.0 - old_sec) * bonus_pct / 100.0).max(0.0);
                if gain > 0.0 {
                    let new_sec = (old_sec + gain).min(max_gain).clamp(-10.0, 10.0);
                    if let Some(ch) = snap.characters.get_mut(&killer) {
                        ch.security_status = new_sec;
                    }
                    snap.combat_crimewatch_state.sec_status_transactions.entry(killer).or_default().push(
                        eve_db::PersistedSecStatusTransaction {
                            timestamp_unix: now_sec, old_sec, new_sec, delta: new_sec - old_sec,
                            reason: "NPC Pirate Bounty".into(), solar_system_id,
                            event_type_id: 378, other_owner_id: None, other_type_id: Some(tid),
                        },
                    );
                }
            }
        });
    }
    if is_locked {
        return Some(Err(rpc_err(
            "MothershipSiteLocked",
            "Mothership HQ site is locked while Sansha influence > 0.0",
        )));
    }
    if waves_pending {
        return Some(Err(rpc_err(
            "IncursionSiteNpcsRemaining",
            format!("Incursion site {site_id} escort waves must be cleared before boss kill"),
        )));
    }
    if is_escort {
        crate::services::pve_incursion_lifecycle::advance_incursion_wave_on_escort_kill(
            systems,
            db,
            sde,
            solar_system_id,
            &site_id,
        )
        .await;
        return None;
    }
    Some(
        check_and_complete_incursion_wave_server_side_core(
            systems,
            db,
            sde,
            solar_system_id,
            &site_id,
            completion_killer_opt.or(killer_char_id),
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
        notify_incursion_site_completed(&ctx.quasar_gateway, &ctx.db, solar_system_id, r);
        crate::services::pve_incursion_lifecycle::emit_incursion_completion_macho_notifications(ctx, r);
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
        ("incursion", "GetIncursionLPLog") => handle_get_incursion_lp_log(ctx, session),

        // parity: feat:pve.incursion-system, rpc:incursion.UpdateIncursionLPLog
        ("incursion", "UpdateIncursionLPLog") => handle_update_incursion_lp_log(ctx, session, args),

        // parity: feat:pve.incursion-system, feat:pve.tales, rpc:taleMgr.GetGlobalWorldEventTales
        ("taleMgr", "GetGlobalWorldEventTales") => handle_get_global_world_event_tales(ctx, session),

        // parity: feat:pve.incursion-system, feat:pve.tales, rpc:taleMgr.get_active_tales_by_template
        ("taleMgr", "get_active_tales_by_template") => {
            handle_get_active_tales_by_template(ctx, session, args)
        }

        _ => None,
    }
}

fn handle_get_incursion_lp_log(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
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
                    .map(crate::services::pve_incursion_sites_ops::encode_incursion_lp_log_row)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    });
    Some(Ok(PyRep::List(logs_py)))
}

fn handle_update_incursion_lp_log(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    let (_, char_id) = match ServerContext::require_character(session) {
        Ok(v) => v,
        Err(e) => return Some(Err(e)),
    };
    let now_ft = ctx.now_blueos_filetime_ticks();
    let synced_rows = with_pve_special_reg(ctx, |reg| {
        crate::services::pve_incursion_sites_ops::sync_incursion_lp_log_rows(
            reg, char_id, args, now_ft,
        )
    });
    if synced_rows.is_empty() {
        Some(Ok(PyRep::None))
    } else {
        Some(Ok(PyRep::List(synced_rows)))
    }
}

fn handle_get_global_world_event_tales(
    ctx: &ServerContext,
    session: &SessionState,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Err(e) = ServerContext::require_character(session) {
        return Some(Err(e));
    }
    crate::services::pve_special_sites::ensure_special_sites_initialized(ctx);
    let list = crate::services::pve_incursion_lifecycle::collect_all_world_event_tales(ctx, None);
    Some(Ok(PyRep::List(list)))
}

fn handle_get_active_tales_by_template(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Option<Result<PyRep, BridgeRpcError>> {
    if let Err(e) = ServerContext::require_character(session) {
        return Some(Err(e));
    }
    crate::services::pve_special_sites::ensure_special_sites_initialized(ctx);
    let template_id = args.first().and_then(|a| a.as_i64()).unwrap_or(0);
    let list = crate::services::pve_incursion_lifecycle::collect_all_world_event_tales(
        ctx,
        Some(template_id),
    );
    Some(Ok(PyRep::List(list)))
}

#[cfg(test)]
#[path = "pve_incursion_sites_tests.rs"]
mod pve_incursion_sites_tests;

