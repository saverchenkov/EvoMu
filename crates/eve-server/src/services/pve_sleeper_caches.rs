//! `pve_sleeper_caches` — Limited, Standard, and Superior Sleeper Caches (`feat:pve.sleeper-caches`, `feat:pve.ghost-sites`).
//!
//! Handles room hazard mechanics (corrosive gas clouds, plasma chambers, automated defense sentries),
//! defense puzzle triggers, and spatial rift/gate passage based on puzzle solution.
//! Also handles Ghost Site real server countdown timers, detonation damage, and covert pirate response spawns.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

use crate::router::ServerContext;
use crate::services::pve_exploration::{
    now_sec, PveExplorationRegistry, PveSiteInstance,
};
use eve_machonet::SessionState;
use eve_proto::BridgeRpcError;
use eve_sde::SolarSystemId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SleeperCacheTier {
    Limited,
    Standard,
    Superior,
}

pub fn sleeper_cache_tier_for_dungeon(dungeon_id: u32) -> Option<SleeperCacheTier> {
    match dungeon_id {
        5604 | 3546 | 2355 => Some(SleeperCacheTier::Limited),
        5609 | 3547 | 2356 => Some(SleeperCacheTier::Standard),
        5632 | 3548 | 2357 => Some(SleeperCacheTier::Superior),
        _ => None,
    }
}

pub fn sleeper_cache_ship_restrictions(tier: SleeperCacheTier) -> (f64, &'static str, &'static [u32]) {
    match tier {
        SleeperCacheTier::Limited => (
            5_000_000.0,
            "Limited Sleeper Cache: Frigate and Expedition Frigate hulls only (max mass 5,000,000 kg)",
            &[25, 420, 830, 831, 893, 1283, 1534], // Frigates, Covert Ops, Stealth Bombers, Interceptors, Assault Frigates, Electronic Attack Ships, Expedition Frigates
        ),
        SleeperCacheTier::Standard => (
            20_000_000.0,
            "Standard Sleeper Cache: Cruiser, Destroyer, and Frigate hulls only (max mass 20,000,000 kg)",
            &[25, 26, 420, 830, 831, 893, 1283, 1534, 419, 540, 541, 832, 833, 898, 906, 963],
        ),
        SleeperCacheTier::Superior => (
            120_000_000.0,
            "Superior Sleeper Cache: Battleship and sub-capital hulls only (max mass 120,000,000 kg)",
            &[25, 26, 27, 419, 420, 540, 541, 830, 831, 832, 833, 893, 898, 906, 963, 1283, 1534, 381, 894, 900],
        ),
    }
}

pub fn validate_sleeper_cache_passage(
    tier: SleeperCacheTier,
    rift_stabilized: bool,
    ship_group_id: u32,
    ship_mass: f64,
) -> Result<(), BridgeRpcError> {
    if !rift_stabilized {
        return Err(BridgeRpcError {
            code: "RiftPassageUnstable".into(),
            message: "Must hack the Spatial Rift Generator Control before traversing the Sleeper Cache passage".into(),
        });
    }
    let (max_mass, label, allowed_groups) = sleeper_cache_ship_restrictions(tier);
    if !allowed_groups.contains(&ship_group_id) || ship_mass > max_mass {
        return Err(BridgeRpcError {
            code: "ShipSizeRestricted".into(),
            message: format!("Acceleration gate rejects hull group {ship_group_id} (mass {ship_mass:.0} kg): {label}"),
        });
    }
    Ok(())
}

/// Applies environmental hazard damage if entering an active hazard chamber without solving puzzle.
pub async fn apply_sleeper_hazard_damage_if_active(
    ctx: &ServerContext,
    session: &SessionState,
    solar_system_id: SolarSystemId,
    site: &PveSiteInstance,
) {
    if !site.sleeper_gas_cloud_active && site.sleeper_sentry_disabled {
        return;
    }
    let Some(ship_id) = session.ship_id else {
        return;
    };
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let thermal = if site.sleeper_gas_cloud_active { 200.0 } else { 0.0 };
    let kinetic = if !site.sleeper_sentry_disabled { 200.0 } else { 0.0 };
    if thermal > 0.0 || kinetic > 0.0 {
        let dmg_vec = eve_destiny::DamageVector {
            thermal,
            kinetic,
            ..Default::default()
        };
        let _ = sys_handle.apply_damage(ship_id, dmg_vec).await;
    }
}

/// Ghost Site timer management: checks if the timer has expired for a site.
pub fn is_ghost_site_timer_expired(
    reg: &PveExplorationRegistry,
    solar_system_id: SolarSystemId,
    site_id: &str,
) -> bool {
    let now = now_sec(reg);
    if let Some(sites) = reg.sites_by_system.get(&solar_system_id) {
        if let Some(s) = sites.iter().find(|x| x.site_id == site_id) {
            if s.site_category == "Ghost" {
                if let Some(started) = s.ghost_timer_started_at {
                    return now >= started.saturating_add(s.ghost_detonate_after_sec);
                }
            }
        }
    }
    false
}

/// Activates countdown timer on a Ghost site when player decloaks or initiates hacking.
pub fn activate_ghost_site_timer(
    reg: &mut PveExplorationRegistry,
    solar_system_id: SolarSystemId,
    site_id: &str,
    timer_duration_sec: u64,
) -> bool {
    let now = now_sec(reg);
    if let Some(sites) = reg.sites_by_system.get_mut(&solar_system_id) {
        if let Some(s) = sites.iter_mut().find(|x| x.site_id == site_id) {
            if s.site_category == "Ghost" && s.ghost_timer_started_at.is_none() {
                s.ghost_timer_started_at = Some(now);
                if timer_duration_sec > 0 {
                    s.ghost_detonate_after_sec = timer_duration_sec;
                }
                return true;
            }
        }
    }
    false
}
