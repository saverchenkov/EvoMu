//! `pve_dungeon_cache` — `dungeonInstanceCacheMgr` per-system dungeon instance lists (`SO-5`).
//!
//! The V24 client (`evedungeons/client/anomalyTracker.pyj`) calls
//! `sm.RemoteSvc('dungeonInstanceCacheMgr').GetCombatAnomalyInstances()` /
//! `GetOreAnomalyInstances()` / `GetIceBeltInstances()` and the agency/map signature providers call
//! `GetSignatureInstances()`. The tracker unpacks the reply as
//! `{solar_system_id: {dungeon_id: [instance_id, ...]}}` and filters each dungeon by its own
//! `dungeons.fsdbinary` archetype, so every dungeon ID returned here is a real SDE dungeon ID.
//!
//! Instances come from the live per-system state, never from a fixed list:
//! * combat anomalies / combat-data-relic-ghost-sleeper signatures: the PvE exploration registry
//!   ([`super::pve_exploration`]), uncleared sites only;
//! * ore anomalies and ore/gas signatures: `cosmic_anomalies` / `cosmic_signatures` seeded by
//!   [`super::mining_sites::seed_system_mining_sites_from_sde`], uncleared only;
//! * ice belts: seeded here from SDE archetype-28 dungeons (see [`seed_system_ice_belt_from_sde`]).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::mining_sites::{
    resolve_gas_site, resolve_ore_site, seed_system_mining_sites_from_sde, COSMIC_ORE_SITES,
    GAS_SITES,
};
use crate::services::pve_exploration::{
    anomaly_dungeons_for_system, ded_complex_for_rating, ensure_system_pve_sites_seeded,
    pirate_faction_for_system, read_pve_exploration_reg, wormhole_class_for_system, PirateFaction,
};
use crate::services::pve_exploration_seeding::{
    ghost_site_dungeon_for_system, sleeper_cache_dungeon_for_system,
};
use crate::services::pve_dungeon_editor::{
    ensure_dungeon_rooms_seeded, handle_gm_dungeon_editor_op,
};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Client `appConst.dunArchetypeIceBelt`. Every `* Ice Belt` / `* Ice Field` dungeon in the SDE's
/// `sde_dungeons` table carries archetype 28, which is how ice-belt dungeons are discovered here.
pub const DUNGEON_ARCHETYPE_ICE_BELT: u32 = 28;

/// `cosmic_anomalies.anomaly_type` used for seeded ice belts.
pub const ICE_BELT_ANOMALY_TYPE: &str = "IceBelt";

fn rpc_msg_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    let msg = message.into();
    BridgeRpcError::with_client_error(
        code,
        msg.clone(),
        eve_proto::ClientError::CustomInfo(msg),
    )
}

/// Parses the security ceiling an ice type advertises in its SDE description
/// ("Available in <color=…>0.4</color> security status solar systems or lower.").
///
/// DERIVED: the per-type security band of ice is only published as this description text in the
/// client SDE (no dogma attribute carries it), so it is parsed from there.
pub fn ice_type_security_ceiling(sde: &SdeRegistry, type_id: TypeId) -> Option<f64> {
    let desc = &sde.types.get(&type_id)?.description;
    let tail = &desc[desc.find("Available in")?..];
    let end = tail.find("security status")?;
    let segment = &tail[..end];
    // Strip `<color=…>` / `</color>` markup and keep the first number.
    let mut plain = String::new();
    let mut in_tag = false;
    for ch in segment.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            c if !in_tag => plain.push(c),
            _ => {}
        }
    }
    plain
        .split_whitespace()
        .find_map(|tok| tok.parse::<f64>().ok())
}

/// Ice-belt dungeons (archetype 28) whose every ice type is available at `displayed_sec`, paired with
/// the belt's security ceiling (the minimum ceiling over its ice types). Belts containing an ice type
/// without a published ceiling (e.g. metaliminal volatile ice, which only appears in storms) are
/// never eligible.
pub fn eligible_ice_belts(sde: &SdeRegistry, displayed_sec: f64) -> Vec<(u32, f64)> {
    let mut out = Vec::new();
    for d in sde.dungeons.values() {
        if d.archetype_id != Some(DUNGEON_ARCHETYPE_ICE_BELT) {
            continue;
        }
        let Some(rows) = sde.dungeon_asteroids.get(&d.dungeon_id) else {
            continue;
        };
        if rows.is_empty() {
            continue;
        }
        let ceiling = rows
            .iter()
            .map(|r| ice_type_security_ceiling(sde, r.type_id))
            .try_fold(f64::INFINITY, |acc, c| c.map(|c| acc.min(c)));
        if let Some(c) = ceiling {
            if displayed_sec <= c + 1e-9 {
                out.push((d.dungeon_id, c));
            }
        }
    }
    out.sort_by_key(|(id, _)| *id);
    out
}

/// Picks the ice-belt dungeon for `sys_id` when `sys_id` is a designated ice system with a regional
/// SDE ice-belt layout (`resolve_regional_ice_belt`).
///
/// Only designated systems whose regional layout resolves (and can be spawned with real asteroids by
/// `tick_ice_belt_respawn_sync`) expose an ice belt; non-designated systems (including Jita) and
/// wormhole/abyssal/unbacked systems receive none.
pub fn ice_belt_dungeon_for_system(ctx: &ServerContext, sys_id: SolarSystemId) -> Option<u32> {
    if sys_id >= 31_000_000 || !crate::services::mining_sites::is_designated_ice_system(sys_id) {
        return None;
    }
    let Ok(site) = crate::services::mining_sites::resolve_regional_ice_belt(&ctx.sde, sys_id)?
    else {
        return None;
    };
    site.asteroids
        .iter()
        .all(|a| ctx.sde.types.contains_key(&a.type_id))
        .then_some(site.dungeon_id)
}

/// Seeds or respawns the system's regional ice-belt anomaly and its asteroid balls when `sys_id`
/// is a designated ice system and ready to spawn/respawn. Non-designated systems receive none and no
/// asteroid-less placeholder anomaly is inserted.
pub fn seed_system_ice_belt_from_sde(ctx: &ServerContext, sys_id: SolarSystemId) {
    crate::services::space_mining_harvest::tick_ice_belt_respawn_sync(ctx, sys_id);
}

/// Parses `DNG-<dungeonID>-<instanceID>` (or lowercase `dng-`) site IDs used by SDE mining/ice/seasonal sites.
pub fn parse_dng_site_id(site_id: &str) -> Option<(u32, i64)> {
    let rest = site_id
        .strip_prefix("DNG-")
        .or_else(|| site_id.strip_prefix("dng-"))?;
    let (d, i) = rest.split_once('-')?;
    Some((d.parse().ok()?, i.parse().ok()?))
}

/// Stable positive instance ID for exploration-registry sites, whose IDs are strings.
///
/// DERIVED: FNV-1a 64-bit hash of the site ID string, masked to 53 bits so it survives a client
/// float round-trip. The registry keys sites by string; the client only needs a stable integer.
pub fn registry_instance_id(site_id: &str) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in site_id.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    (h & 0x001f_ffff_ffff_ffff) as i64
}

fn instances_reply(sys_id: SolarSystemId, by_dungeon: BTreeMap<u32, Vec<i64>>) -> PyRep {
    let inner: Vec<(PyRep, PyRep)> = by_dungeon
        .into_iter()
        .map(|(did, mut ids)| {
            ids.sort_unstable();
            (
                PyRep::Integer(did as i64),
                PyRep::List(ids.into_iter().map(PyRep::Integer).collect()),
            )
        })
        .collect();
    PyRep::Dict(vec![(PyRep::Integer(sys_id as i64), PyRep::Dict(inner))])
}

fn anomaly_instances(ctx: &ServerContext, sys_id: SolarSystemId, kind: &str) -> BTreeMap<u32, Vec<i64>> {
    let mut out: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        for a in snap.cosmic_anomalies.get(&sys_id).into_iter().flatten() {
            if a.anomaly_type != kind
                || a.cleared
                || (a.lifetime_seconds > 0 && now >= a.created_unix.saturating_add(a.lifetime_seconds))
            {
                continue;
            }
            let (did, iid) = parse_dng_site_id(&a.anomaly_id)
                .unwrap_or_else(|| (0, registry_instance_id(&a.anomaly_id)));
            out.entry(did).or_default().push(iid);
        }
    });
    out
}

fn is_site_scanned_by_character(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    site_id: &str,
) -> bool {
    ctx.db.with_snapshot(|snap| {
        snap.cosmic_signatures
            .get(&sys_id)
            .into_iter()
            .flatten()
            .any(|sig| {
                sig.signature_id == site_id
                    && !sig.collapsed
                    && ctx.effective_signature_signal_strength(char_id, sys_id, sig) >= 1.0
            })
    })
}

fn registry_instances(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    probe_required: bool,
) -> BTreeMap<u32, Vec<i64>> {
    read_pve_exploration_reg(ctx, |reg| {
        let now = crate::services::pve_exploration::now_sec(reg);
        let mut out: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
        for s in reg.sites_by_system.get(&sys_id).into_iter().flatten() {
            if s.probe_required == probe_required
                && !s.cleared
                && (s.lifetime_seconds == 0 || now < s.created_unix.saturating_add(s.lifetime_seconds))
            {
                out.entry(s.dungeon_id)
                    .or_default()
                    .push(registry_instance_id(&s.site_id));
            }
        }
        out
    })
}

pub(crate) struct UnseededCombatAnomalySpec {
    pub site_id: String,
    pub dungeon_id: u32,
    pub dungeon_name: String,
    pub site_category: &'static str,
    pub wormhole_class: Option<u8>,
    pub current_wave: u8,
    pub total_waves: u8,
}

fn combat_dungeon_info_for_wh_class(wh_class: u8, variant: usize) -> (u32, &'static str, u8) {
    match wh_class {
        1 => (3707, "Perimeter Ambush Point", 3),
        2 => (3708, "Perimeter Camp", 3),
        3 => (3710, "Fortification Frontier Stronghold", 3),
        4 => (3713, "Outpost Frontier Stronghold", 3),
        5 => {
            if variant % 2 == 0 {
                (3704, "Core Citadel", 4)
            } else {
                (3716, "Core Garrison", 4)
            }
        }
        _ => (3716, "Core Garrison", 4),
    }
}

/// Computes unseeded deterministic combat-anomaly site metadata read-only
/// so `custom_agency_provider.get_content_data` and remote `scanSvc.GetCombatAnomalyInstances`
/// remain read-pure without mutating DB state or spawning NPCs.
pub(crate) fn unseeded_combat_anomaly_specs(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> Vec<UnseededCombatAnomalySpec> {
    let cleared_ids: std::collections::HashSet<String> = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&sys_id)
            .into_iter()
            .flatten()
            .filter(|a| a.cleared)
            .map(|a| a.anomaly_id.clone())
            .collect()
    });
    let mut specs = Vec::new();
    let (wh_class, _, is_shattered, _) =
        crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde(&ctx.sde, sys_id);
    if (14..=18).contains(&wh_class) {
        return specs;
    }
    let sde_dungeon_name = |did: u32, fallback: &str| {
        ctx.sde
            .dungeons
            .get(&did)
            .map(|d| d.name.trim().to_string())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| fallback.to_string())
    };
    if wh_class == 13 || ((1..=6).contains(&wh_class) && is_shattered) {
        let is_c13 = wh_class == 13;
        let combat_classes: Vec<u8> = if is_c13 {
            vec![1, 2, 3, 2, 3]
        } else {
            let min_c = wh_class.saturating_sub(1).max(1);
            let max_c = (wh_class + 1).min(6);
            (min_c..=max_c).collect()
        };
        for (idx, &site_c) in combat_classes.iter().enumerate() {
            let (did, default_name, waves) = combat_dungeon_info_for_wh_class(site_c, idx);
            let site_id = format!("PVE-ANOM-{sys_id}-{}", idx + 1);
            if !cleared_ids.contains(&site_id) {
                specs.push(UnseededCombatAnomalySpec {
                    site_id,
                    dungeon_id: did,
                    dungeon_name: sde_dungeon_name(did, default_name),
                    site_category: "WormholeCombat",
                    wormhole_class: Some(site_c),
                    current_wave: 1,
                    total_waves: waves,
                });
            }
        }
        if !is_c13 && is_shattered {
            for (site_id, did, name, cat) in [
                (
                    format!("SHD-{sys_id}"),
                    3790_u32,
                    "Shattered Debris Field",
                    "ShatteredDebris",
                ),
                (
                    format!("SHI-{sys_id}"),
                    3791_u32,
                    "Shattered Ice Field",
                    "ShatteredIce",
                ),
            ] {
                if !cleared_ids.contains(&site_id) {
                    specs.push(UnseededCombatAnomalySpec {
                        site_id,
                        dungeon_id: did,
                        dungeon_name: name.to_string(),
                        site_category: cat,
                        wormhole_class: Some(wh_class),
                        current_wave: 0,
                        total_waves: 1,
                    });
                }
            }
        }
        return specs;
    }
    if ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .is_some_and(|s| s.region_id == 10_000_070)
    {
        let site_id = format!("PVE-POCHVEN-HIVE-{sys_id}");
        if !cleared_ids.contains(&site_id) {
            specs.push(UnseededCombatAnomalySpec {
                site_id,
                dungeon_id: 9006,
                dungeon_name: sde_dungeon_name(9006, "Torpid Sleeper Hive"),
                site_category: "Combat",
                wormhole_class: None,
                current_wave: 1,
                total_waves: 1,
            });
        }
        return specs;
    }
    let true_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.5);
    for (idx, (did, default_name, cat, waves, wh_c)) in
        anomaly_dungeons_for_system(&ctx.sde, sys_id, true_sec)
            .into_iter()
            .enumerate()
    {
        let site_id = format!("PVE-ANOM-{sys_id}-{}", idx + 1);
        if !cleared_ids.contains(&site_id) {
            specs.push(UnseededCombatAnomalySpec {
                site_id,
                dungeon_id: did,
                dungeon_name: sde_dungeon_name(did, default_name),
                site_category: cat,
                wormhole_class: wh_c,
                current_wave: 1,
                total_waves: waves,
            });
        }
    }
    specs
}

/// Computes unseeded deterministic combat-anomaly `(dungeon_id, instance_id)` entries read-only
/// so `custom_agency_provider.get_content_data` stays pure without mutating DB state.
fn unseeded_agency_anomaly_instances(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> BTreeMap<u32, Vec<i64>> {
    let mut out: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
    for spec in unseeded_combat_anomaly_specs(ctx, sys_id) {
        out.entry(spec.dungeon_id)
            .or_default()
            .push(registry_instance_id(&spec.site_id));
    }
    out
}

/// Reads combat anomaly instances for `sys_id` read-only, falling back to deterministic
/// SDE-derived combat anomalies when the system has not yet been materialized in DB.
fn combat_anomaly_instances_readonly(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    include_seasonal: bool,
) -> BTreeMap<u32, Vec<i64>> {
    let pve_seeded = read_pve_exploration_reg(ctx, |reg| {
        reg.seeded_systems.get(&sys_id).copied().unwrap_or(false)
    });
    let mut by_dungeon = registry_instances(ctx, sys_id, false);
    if !pve_seeded && by_dungeon.is_empty() {
        by_dungeon = unseeded_agency_anomaly_instances(ctx, sys_id);
    }
    if include_seasonal {
        for (did, iids) in
            crate::services::pve_special_sites_combat::active_seasonal_anomaly_instances_readonly(
                ctx, sys_id,
            )
        {
            by_dungeon.entry(did).or_default().extend(iids);
        }
    }
    by_dungeon
}

fn deterministic_mining_instance_id(did: u32, sys_id: SolarSystemId) -> i64 {
    registry_instance_id(&format!("DNG-{did}-{sys_id}"))
}

fn mining_site_matches_target(
    did: u32,
    sys_id: SolarSystemId,
    target_iid: Option<i64>,
    target_did: Option<u32>,
) -> bool {
    let synthetic_iid = deterministic_mining_instance_id(did, sys_id);
    if let Some(tid_did) = target_did {
        tid_did == did && (target_iid.is_none() || target_iid == Some(synthetic_iid))
    } else {
        target_iid == Some(synthetic_iid)
    }
}

fn is_system_mining_seeded(snap: &eve_db::WorldStateSnapshot, sys_id: SolarSystemId) -> bool {
    snap.cosmic_anomalies
        .get(&sys_id)
        .is_some_and(|list| list.iter().any(|a| a.anomaly_type == "OreAnomaly"))
        || snap.cosmic_signatures.get(&sys_id).is_some_and(|list| {
            list.iter()
                .any(|s| matches!(s.sig_group.as_str(), "OreAnomaly" | "Ore" | "Gas"))
        })
}

fn system_wormhole_class_opt(sde: &SdeRegistry, sys: &eve_sde::SolarSystem) -> Option<u8> {
    sys.wormhole_class_id
        .or_else(|| {
            sde.constellations
                .get(&sys.constellation_id)
                .and_then(|c| c.wormhole_class_id)
        })
        .or_else(|| {
            sde.regions
                .get(&sys.region_id)
                .and_then(|r| r.wormhole_class_id)
        })
        .map(|c| c as u8)
}

fn is_wormhole_mining_system(
    sde: &SdeRegistry,
    sys_id: SolarSystemId,
    sys: &eve_sde::SolarSystem,
) -> (bool, Option<u8>) {
    let wh_class_opt = system_wormhole_class_opt(sde, sys);
    let is_wh = (31_000_000..32_000_000).contains(&sys_id)
        || wh_class_opt.is_some_and(|c| matches!(c, 1..=6 | 12..=18));
    (is_wh, wh_class_opt)
}

/// Returns deterministic ore anomaly dungeon IDs guaranteed by `seed_system_mining_sites_from_sde`
/// in `sys_id` (known-space cosmic ore, Pochven ore, C1-C4 WH ore, and `Shattered Debris Field`
/// in shattered WHs; excludes random C5/C6 4-of-10 pools so unseeded reads stay accurate).
fn deterministic_unseeded_ore_anomaly_dungeons(
    sde: &SdeRegistry,
    sys_id: SolarSystemId,
) -> Vec<u32> {
    use crate::services::mining_sites::{
        resolve_wormhole_ore_site_with_dungeon, wormhole_ore_sites_for_class, SiteKind,
        WORMHOLE_ORE_SITES,
    };
    let Some(sys) = sde.solar_systems.get(&sys_id) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for def in COSMIC_ORE_SITES
        .iter()
        .filter(|d| d.kind == SiteKind::Anomaly && d.band.matches_system(sys))
    {
        if let Ok(site) = resolve_ore_site(sde, def) {
            if site.asteroids.iter().all(|a| sde.types.contains_key(&a.type_id)) {
                out.push(site.dungeon_id);
            }
        }
    }
    if sys.region_id == 10_000_070 {
        const POCHVEN_SPECIAL_ORE_DUNGEONS: [u32; 5] = [8987, 8988, 8989, 8990, 8991];
        let primary_did =
            POCHVEN_SPECIAL_ORE_DUNGEONS[(sys_id as usize) % POCHVEN_SPECIAL_ORE_DUNGEONS.len()];
        for did in [primary_did, 8974] {
            if sde.dungeons.contains_key(&did)
                && sde.dungeon_asteroids.get(&did).is_some_and(|v| {
                    !v.is_empty() && v.iter().all(|e| sde.types.contains_key(&e.type_id))
                })
            {
                out.push(did);
            }
        }
    }
    let (is_wh, wh_class_opt) = is_wormhole_mining_system(sde, sys_id, sys);
    if is_wh {
        let wh_c = wh_class_opt.unwrap_or(3);
        let is_shattered = matches!(wh_c, 12..=18)
            || sde
                .celestials_in_system_slice(sys_id)
                .iter()
                .any(|c| c.type_id == 30889);
        let wh_candidates: Vec<_> = if (1..=6).contains(&wh_c) && is_shattered {
            let lo = wh_c.saturating_sub(1).max(1);
            let hi = (wh_c + 1).min(6);
            WORMHOLE_ORE_SITES
                .iter()
                .filter(|d| {
                    (d.wh_class_min <= hi && d.wh_class_max >= lo)
                        || d.dungeon_name == "Shattered Debris Field"
                })
                .collect()
        } else {
            wormhole_ore_sites_for_class(wh_c)
        };
        let deterministic_candidates: Vec<_> = if wh_candidates.len() <= 4 {
            wh_candidates
        } else if is_shattered {
            wh_candidates
                .into_iter()
                .filter(|d| d.dungeon_name == "Shattered Debris Field")
                .collect()
        } else {
            Vec::new()
        };
        for def in deterministic_candidates {
            if let Ok((wh_did, _)) = resolve_wormhole_ore_site_with_dungeon(sde, def) {
                out.push(wh_did);
            }
        }
    }
    out
}

fn unseeded_ice_belt_dungeon_for_system(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> Option<u32> {
    ice_belt_dungeon_for_system(ctx, sys_id)
}

/// Reads SDE ore anomaly instances for `sys_id` strictly read-only without mutating DB state.
fn ore_anomaly_instances_readonly(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> BTreeMap<u32, Vec<i64>> {
    let already_seeded = ctx
        .db
        .with_snapshot(|snap| is_system_mining_seeded(snap, sys_id));
    if already_seeded {
        return anomaly_instances(ctx, sys_id, "OreAnomaly");
    }
    let mut out: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
    for did in deterministic_unseeded_ore_anomaly_dungeons(&ctx.sde, sys_id) {
        out.entry(did)
            .or_default()
            .push(deterministic_mining_instance_id(did, sys_id));
    }
    out
}

/// Reads ice-belt instances for `sys_id` strictly read-only without mutating DB state.
fn ice_belt_instances_readonly(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> BTreeMap<u32, Vec<i64>> {
    let has_any_ice_row = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .get(&sys_id)
            .is_some_and(|list| list.iter().any(|a| a.anomaly_type == ICE_BELT_ANOMALY_TYPE))
    });
    if has_any_ice_row {
        return anomaly_instances(ctx, sys_id, ICE_BELT_ANOMALY_TYPE);
    }
    let mut out: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
    if let Some(did) = unseeded_ice_belt_dungeon_for_system(ctx, sys_id) {
        out.entry(did)
            .or_default()
            .push(deterministic_mining_instance_id(did, sys_id));
    }
    out
}

/// Validates ship-level warpability constraints (pre-warp checks, active warp, movement/tackle/cloak
/// locks, and minimum 150 km warp capacitor) before lazy site seeding can mutate the database.
async fn validate_pre_agency_warp(
    ctx: &ServerContext,
    sys_handle: &crate::system_actor::SystemHandle,
    char_id: i64,
    ship_id: i64,
    sys_id: SolarSystemId,
    pre_ball: &eve_destiny::Ball,
) -> Result<(), BridgeRpcError> {
    crate::services::nav_warp_interdiction::validate_pre_warp_common(
        ctx, char_id, ship_id, sys_id, sys_handle,
    )
    .await?;
    crate::services::nav_warp_interdiction::ensure_not_in_active_warp(&pre_ball.mode)?;
    ctx.validate_ship_movement(
        char_id,
        ship_id,
        crate::services::space_nav_movement_guard::MovementIntent::AgencyWarp { sys_id },
    )
    .await?;

    let (base_warp_cap_need, sde_max_cap) = ctx
        .sde
        .types
        .get(&pre_ball.type_id)
        .map(|t| {
            let wcn = t.default_attributes.get(&153).copied().unwrap_or(1.24e-6);
            let cap = t.default_attributes.get(&482).copied().unwrap_or(250.0);
            (wcn, cap)
        })
        .unwrap_or((1.24e-6, 250.0));
    let wdo_lvl = ctx.db.with_snapshot(|snap| {
        snap.skills
            .get(&char_id)
            .and_then(|m| m.get(&3455))
            .map(|s| s.level.min(5))
            .unwrap_or(0)
    });
    let eff_warp_cap_need = base_warp_cap_need * (1.0 - 0.10 * (wdo_lvl as f64));
    let cap_per_m = (eff_warp_cap_need * pre_ball.mass) / 149_597_870_700.0;
    if cap_per_m > 0.0 {
        if let Ok(Some(mut cs)) = sys_handle.get_combat_state(ship_id).await {
            if (cs.max_capacitor - 250.0).abs() < 1e-6
                && (cs.capacitor - 250.0).abs() < 1e-6
                && sde_max_cap > 250.0
            {
                cs.capacitor = sde_max_cap;
            }
            if (cs.capacitor / cap_per_m) < 150_000.0 {
                return Err(BridgeRpcError {
                    code: "NotEnoughCapacitor".into(),
                    message: format!(
                        "Insufficient capacitor ({:.3} GJ) to initiate minimum 150 km warp",
                        cs.capacitor
                    ),
                    client_error: None,
                });
            }
        }
    }
    Ok(())
}

/// Resolves an already-populated content warp target read-only.
/// Returns `(resolved_pos, unscanned_blocked, pve_seeded, mining_seeded)`.
fn resolve_warp_target_readonly(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    target_iid: Option<i64>,
    target_did: Option<u32>,
) -> (Option<glam::DVec3>, bool, bool, bool) {
    let mut unscanned_blocked = false;
    let (mut resolved_pos, pve_seeded) = read_pve_exploration_reg(ctx, |reg| {
        let seeded = reg.seeded_systems.get(&sys_id).copied().unwrap_or(false);
        let now = crate::services::pve_exploration::now_sec(reg);
        for s in reg.sites_by_system.get(&sys_id).into_iter().flatten() {
            if s.cleared
                || (s.lifetime_seconds > 0
                    && now >= s.created_unix.saturating_add(s.lifetime_seconds))
            {
                continue;
            }
            let iid = registry_instance_id(&s.site_id);
            if Some(iid) == target_iid || (target_iid.is_none() && Some(s.dungeon_id) == target_did) {
                if s.probe_required
                    && !is_site_scanned_by_character(ctx, char_id, sys_id, &s.site_id)
                {
                    unscanned_blocked = true;
                    return (None, seeded);
                }
                return (Some(glam::DVec3::from_array(s.position)), seeded);
            }
        }
        (None, seeded)
    });
    if unscanned_blocked {
        return (None, true, pve_seeded, false);
    }
    let mut mining_seeded = false;
    if resolved_pos.is_none() {
        resolved_pos = ctx.db.with_snapshot(|snap| {
            mining_seeded = is_system_mining_seeded(snap, sys_id);
            for a in snap.cosmic_anomalies.get(&sys_id).into_iter().flatten() {
                if a.cleared {
                    continue;
                }
                if let Some((did, iid)) = parse_dng_site_id(&a.anomaly_id) {
                    let is_docked_mining =
                        matches!(a.anomaly_type.as_str(), "OreAnomaly" | ICE_BELT_ANOMALY_TYPE);
                    let matches_synthetic_iid = is_docked_mining
                        && target_iid == Some(deterministic_mining_instance_id(did, sys_id))
                        && (target_did.is_none() || target_did == Some(did));
                    if Some(iid) == target_iid
                        || matches_synthetic_iid
                        || (target_iid.is_none() && Some(did) == target_did)
                    {
                        return Some(glam::DVec3::from_array(a.position));
                    }
                }
            }
            for s in snap.cosmic_signatures.get(&sys_id).into_iter().flatten() {
                if s.collapsed
                    || snap.cosmic_anomalies.get(&sys_id).is_some_and(|anoms| {
                        anoms
                            .iter()
                            .any(|a| a.cleared && a.anomaly_id == s.signature_id)
                    })
                {
                    continue;
                }
                if let Some((did, iid)) = parse_dng_site_id(&s.signature_id) {
                    if Some(iid) == target_iid || (target_iid.is_none() && Some(did) == target_did) {
                        if ctx.effective_signature_signal_strength(char_id, sys_id, s) < 1.0 {
                            unscanned_blocked = true;
                            return None;
                        }
                        return Some(glam::DVec3::from_array(s.position));
                    }
                }
            }
            None
        });
    }
    (resolved_pos, unscanned_blocked, pve_seeded, mining_seeded)
}

pub(crate) fn unseeded_pve_signature_specs(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> Vec<(u32, String)> {
    let Some(sys) = ctx.sde.solar_systems.get(&sys_id) else {
        return Vec::new();
    };
    let (wh_class, _, is_shattered, _) =
        crate::services::wormhole_lifecycle_ops::system_wormhole_profile_sde(&ctx.sde, sys_id);
    if (14..=18).contains(&wh_class) || sys.region_id == 10_000_070 {
        return Vec::new();
    }
    if wh_class == 13 || ((1..=6).contains(&wh_class) && is_shattered) {
        let count = if wh_class == 13 { 4 } else { 2 };
        return (0..count)
            .map(|s_idx| {
                let is_relic = s_idx % 2 == 1;
                let did = if is_relic { 3722 } else { 3729 };
                let sig_id = format!(
                    "{}-{sys_id}-{}",
                    if is_relic { "REL" } else { "DAT" },
                    s_idx + 1
                );
                (did, sig_id)
            })
            .collect();
    }
    let true_sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.5);
    let wh_c = wormhole_class_for_system(sys_id);
    let ded_r = if true_sec >= 0.5 { 3 } else if true_sec > 0.0 { 5 } else { 10 };
    let ded_did = ded_complex_for_rating(&ctx.sde, sys_id, ded_r).0;
    let dsg_did = match pirate_faction_for_system(&ctx.sde, sys_id) {
        PirateFaction::Guristas => 98,
        PirateFaction::BloodRaiders => 106,
        PirateFaction::Sansha => 110,
        PirateFaction::Serpentis => 119,
        PirateFaction::AngelCartel => 59,
    };
    let guarded = wh_c.is_some() || true_sec <= 0.0;
    let (gho_did, _) = ghost_site_dungeon_for_system(&ctx.sde, sys_id, true_sec, wh_c);
    let (slc_did, _) = sleeper_cache_dungeon_for_system(&ctx.sde, true_sec, wh_c);
    vec![
        (ded_did, format!("DED-{sys_id}-{ded_r}")),
        (dsg_did, format!("DSG-{sys_id}")),
        (if guarded { 3729 } else { 1133 }, format!("DAT-{sys_id}")),
        (if guarded { 3722 } else { 1286 }, format!("REL-{sys_id}")),
        (gho_did, format!("GHO-{sys_id}")),
        (slc_did, format!("SLC-{sys_id}")),
    ]
}

struct UnseededSeedNeeds {
    need_pve: bool,
    need_mining: bool,
    need_ice: bool,
    unscanned_sig: bool,
}

/// Returns seeding requirements if `(target_iid, target_did)` matches unseeded combat/ore/ice
/// anomalies (requiring seeding before warp) or unseeded probe-required signatures (which fail
/// with `SignatureNotScanned` without seeding DB).
fn matching_unseeded_seed_needs(
    ctx: &ServerContext,
    char_id: i64,
    sys_id: SolarSystemId,
    target_iid: Option<i64>,
    target_did: Option<u32>,
    pve_seeded: bool,
    mining_seeded: bool,
) -> UnseededSeedNeeds {
    let mut need_pve = false;
    let mut unscanned_sig = false;
    if !pve_seeded {
        for (did, iids) in unseeded_agency_anomaly_instances(ctx, sys_id) {
            if target_iid.is_some_and(|iid| iids.contains(&iid))
                || (target_iid.is_none() && target_did == Some(did))
            {
                need_pve = true;
                break;
            }
        }
        if !need_pve {
            for (sig_did, sig_id) in unseeded_pve_signature_specs(ctx, sys_id) {
                let sig_iid = registry_instance_id(&sig_id);
                if target_iid == Some(sig_iid)
                    || (target_iid.is_none() && target_did == Some(sig_did))
                {
                    if is_site_scanned_by_character(ctx, char_id, sys_id, &sig_id) {
                        need_pve = true;
                    } else {
                        unscanned_sig = true;
                    }
                    break;
                }
            }
        }
    }
    let mut need_mining = false;
    let mut need_ice = false;
    if !unscanned_sig {
        if let Some(sys) = ctx.sde.solar_systems.get(&sys_id) {
            if !mining_seeded {
                need_mining = deterministic_unseeded_ore_anomaly_dungeons(&ctx.sde, sys_id)
                    .into_iter()
                    .any(|did| mining_site_matches_target(did, sys_id, target_iid, target_did));

                if !need_mining && !need_pve && target_iid.is_none() {
                    if let Some(did) = target_did {
                        use crate::services::mining_sites::{
                            resolve_wormhole_gas_site, SiteKind, WORMHOLE_GAS_SITES,
                        };
                        let region = crate::services::mining_sites::region_name(&ctx.sde, sys_id);
                        let (is_wh, _) = is_wormhole_mining_system(&ctx.sde, sys_id, sys);
                        let matches_mining_sig = COSMIC_ORE_SITES
                            .iter()
                            .filter(|d| d.kind == SiteKind::Signature && d.band.matches_system(sys))
                            .filter_map(|d| resolve_ore_site(&ctx.sde, d).ok())
                            .any(|s| s.dungeon_id == did)
                            || GAS_SITES
                                .iter()
                                .find(|d| {
                                    d.band.matches_system(sys)
                                        && region.is_some_and(|r| d.region_names.contains(&r))
                                })
                                .and_then(|d| resolve_gas_site(&ctx.sde, d).ok())
                                .is_some_and(|s| s.dungeon_id == did)
                            || (is_wh
                                && WORMHOLE_GAS_SITES
                                    .first()
                                    .and_then(|wh_def| resolve_wormhole_gas_site(&ctx.sde, wh_def).ok())
                                    .is_some_and(|(wh_gas_did, _)| wh_gas_did == did));
                        if matches_mining_sig {
                            unscanned_sig = true;
                        }
                    }
                }
            }

            if !need_mining && !unscanned_sig {
                if let Some(ice_did) = unseeded_ice_belt_dungeon_for_system(ctx, sys_id) {
                    if mining_site_matches_target(ice_did, sys_id, target_iid, target_did) {
                        let now = ctx.now_unix_secs();
                        need_ice = ctx.db.with_snapshot(|snap| {
                            let list = snap
                                .cosmic_anomalies
                                .get(&sys_id)
                                .map(Vec::as_slice)
                                .unwrap_or(&[]);
                            let has_any_ice =
                                list.iter().any(|a| a.anomaly_type == ICE_BELT_ANOMALY_TYPE);
                            let has_uncleared = list
                                .iter()
                                .any(|a| a.anomaly_type == ICE_BELT_ANOMALY_TYPE && !a.cleared);
                            let ready_to_respawn = !has_uncleared
                                && crate::services::mining_sites::is_designated_ice_system(sys_id)
                                && list.iter().any(|a| {
                                    a.anomaly_type == ICE_BELT_ANOMALY_TYPE
                                        && a.cleared
                                        && crate::services::mining_sites::is_ice_belt_ready_to_respawn(
                                            now.saturating_sub(a.created_unix),
                                        )
                                });
                            !has_any_ice || ready_to_respawn
                        });
                    }
                }
            }

            if !need_mining && !need_ice && !unscanned_sig && target_iid.is_none() {
                if let Some(did) = target_did {
                    let matching_sov_defs: Vec<(u32, u64)> =
                        crate::services::mining_sites::SOV_PROSPECTING_DEPOSITS
                            .iter()
                            .filter_map(|def| {
                                let site = resolve_ore_site(&ctx.sde, &def.site).ok()?;
                                if site.dungeon_id == did
                                    && site
                                        .asteroids
                                        .iter()
                                        .all(|a| ctx.sde.types.contains_key(&a.type_id))
                                {
                                    Some((def.upgrade_type_id, def.respawn_seconds))
                                } else {
                                    None
                                }
                            })
                            .collect();
                    if !matching_sov_defs.is_empty() {
                        let now = ctx.now_unix();
                        need_mining = ctx.db.with_snapshot(|snap| {
                            let list = snap
                                .cosmic_anomalies
                                .get(&sys_id)
                                .map(Vec::as_slice)
                                .unwrap_or(&[]);
                            let has_uncleared =
                                list.iter().any(|a| a.anomaly_type == "SovOre" && !a.cleared);
                            if has_uncleared {
                                return false;
                            }
                            let matching_installed_respawn = snap
                                .structures
                                .values()
                                .filter(|s| s.solar_system_id == sys_id)
                                .find_map(|s| {
                                    snap.items.values().find_map(|it| {
                                        if it.location_id == s.structure_id
                                            && it.flag == eve_db::InventoryFlag::SubSystemSlot0
                                        {
                                            matching_sov_defs
                                                .iter()
                                                .find(|(up_tid, _)| *up_tid == it.type_id)
                                                .map(|(_, respawn_secs)| *respawn_secs)
                                        } else {
                                            None
                                        }
                                    })
                                });
                            matching_installed_respawn.is_some_and(|respawn_secs| {
                                list.iter().any(|a| {
                                    a.anomaly_type == "SovOre"
                                        && a.cleared
                                        && now
                                            >= a.created_unix
                                                .saturating_add(a.lifetime_seconds.max(respawn_secs))
                                })
                            })
                        });
                    }
                }
            }
        }
    }
    UnseededSeedNeeds {
        need_pve,
        need_mining,
        need_ice,
        unscanned_sig,
    }
}

// DERIVED: V24 client SDE `dungeons.fsdbinary` stores numeric `archetypeID` on each dungeon but does not
// ship the server-side `dungeon.archetypes` table; map SDE `archetypeID`s to editor labels.
fn canonical_archetype_name(aid: u32, sample_dungeon_name: Option<&str>) -> String {
    match aid {
        0 => "Test / Event Dungeon".into(),
        1 => "Constellation Gate / Landmark".into(),
        2 => "Monument / Outpost".into(),
        3 => "Deadspace Complex".into(),
        4 => "Escalation Dungeon".into(),
        5 => "Exploration Site".into(),
        6 => "Unrated Outpost / Scout Site".into(),
        8 => "DED Rated Complex".into(),
        12 => "Capital Security Facility".into(),
        14 => "Penal / Retention Complex".into(),
        16 => "Troop Camp / Complex".into(),
        19 => "Trigger Test Dungeon".into(),
        20 => "Agent Mission Dungeon".into(),
        21 => "Faction Stronghold".into(),
        22 => "Agent Site".into(),
        24 => "Military / Merchant Depot".into(),
        25 => "Triglavian Spawner".into(),
        26 => "Contested Mining / Compound".into(),
        27 => "Ore Anomaly / Gravimetric Deposit".into(),
        28 => "Ice Belt".into(),
        29 => "Booster Gas Distribution Base".into(),
        30 => "Gas Cloud Nebula".into(),
        31 => "Storyline Mission Dungeon".into(),
        32 => "Faction Warfare Complex".into(),
        33 => "Faction Warfare Novice Complex".into(),
        34 => "Faction Warfare Small Complex".into(),
        35 => "Faction Warfare Medium Complex".into(),
        36 => "Faction Warfare Large / Open Complex".into(),
        37 => "Proving Ground Arena".into(),
        38 => "Wormhole".into(),
        39 => "Covert Research Facility (Ghost Site)".into(),
        43 => "Combat Anomaly".into(),
        44 => "Relic Site".into(),
        45 => "Data Site".into(),
        46 => "Empire Border Encampment".into(),
        47 => "Resource Wars Mining Expedition".into(),
        48 => "Reconnaissance / Research Outpost".into(),
        49 => "Sleeper / Wormhole Anomaly".into(),
        50 => "Station Harbor / Event Site".into(),
        51 => "Abyssal Deadspace".into(),
        60..=63 => format!("Sansha Incursion Site ({aid})"),
        64 => "Drifter Hive / Conflux".into(),
        65 => "Triglavian Conduit".into(),
        66 => "Encounter Surveillance System".into(),
        67 => "Faction Warfare Supply Depot / Rendezvous".into(),
        68 => "Faction Warfare Battlefield".into(),
        70 => "Homefront Operation".into(),
        71 => "NPE Anomaly Training Site".into(),
        72 => "Pirate Laundering Facility".into(),
        73 => "Counter-Insurgency Outpost".into(),
        74 => "Pirate Insurgency Ice Refinery Heist".into(),
        75..=78 => format!("Pirate Insurgency Outpost Raid ({aid})"),
        79 => "Pirate Insurgency Mining Ambush".into(),
        80 => "Roaming Mercenary / Drifter Muster".into(),
        81 => "Bulwark / Phased Field Mining Site".into(),
        _ => sample_dungeon_name
            .map(|s| format!("Archetype {aid} ({s})"))
            .unwrap_or_else(|| format!("Archetype {aid}")),
    }
}

fn reachable_solar_systems_within_jumps(
    sde: &SdeRegistry,
    start_sys_id: SolarSystemId,
    max_jumps: usize,
) -> Vec<SolarSystemId> {
    let mut adj: HashMap<SolarSystemId, Vec<SolarSystemId>> = HashMap::new();
    for j in sde.jumps.values() {
        adj.entry(j.from_solar_system_id)
            .or_default()
            .push(j.to_solar_system_id);
    }
    let mut visited: BTreeMap<SolarSystemId, usize> = BTreeMap::new();
    let mut queue = std::collections::VecDeque::new();
    visited.insert(start_sys_id, 0);
    queue.push_back((start_sys_id, 0usize));
    while let Some((curr, dist)) = queue.pop_front() {
        if dist >= max_jumps {
            continue;
        }
        if let Some(neighbors) = adj.get(&curr) {
            for &nbr in neighbors {
                if !visited.contains_key(&nbr) {
                    visited.insert(nbr, dist + 1);
                    queue.push_back((nbr, dist + 1));
                }
            }
        }
    }
    visited.into_keys().collect()
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("dungeon", "DEGetArchetypes") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "dungeon.DEGetArchetypes requires GM/Content role".into(),
                    client_error: None,
                }));
            }
            let mut by_aid: BTreeMap<u32, (u32, &str)> = BTreeMap::new();
            for d in ctx.sde.dungeons.values() {
                if let Some(aid) = d.archetype_id {
                    by_aid
                        .entry(aid)
                        .and_modify(|(min_id, name)| {
                            if d.dungeon_id < *min_id {
                                *min_id = d.dungeon_id;
                                *name = d.name.as_str();
                            }
                        })
                        .or_insert((d.dungeon_id, d.name.as_str()));
                }
            }
            let rows: Vec<Vec<PyRep>> = by_aid
                .into_iter()
                .map(|(aid, (_, sample))| {
                    vec![
                        PyRep::Integer(aid as i64),
                        PyRep::String(canonical_archetype_name(aid, Some(sample))),
                    ]
                })
                .collect();
            return Some(Ok(PyRep::rowset(vec!["archetypeID", "archetypeName"], rows)));
        }

        ("dungeon", "DEGetDungeons") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "dungeon.DEGetDungeons requires GM/Content role".into(),
                    client_error: None,
                }));
            }
            let req_dungeon_id = kwargs
                .get("dungeonID")
                .or_else(|| kwargs.get("dungeonVID"))
                .and_then(|v| v.as_i64())
                .or_else(|| args.first().and_then(|a| a.as_i64()));
            let req_archetype_id = kwargs
                .get("archetypeID")
                .and_then(|v| v.as_i64())
                .map(|v| v as u32);
            let req_faction_id = kwargs
                .get("factionID")
                .and_then(|v| v.as_i64())
                .map(|v| v as u32);

            let mut dungeons: Vec<_> = ctx
                .sde
                .dungeons
                .values()
                .filter(|d| {
                    if let Some(did) = req_dungeon_id {
                        if d.dungeon_id as i64 != did {
                            return false;
                        }
                    }
                    if let Some(aid) = req_archetype_id {
                        if d.archetype_id != Some(aid) {
                            return false;
                        }
                    }
                    if let Some(fid) = req_faction_id {
                        if d.faction_id != Some(fid) {
                            return false;
                        }
                    }
                    true
                })
                .take(128)
                .collect();
            dungeons.sort_by_key(|d| d.dungeon_id);

            let rows: Vec<Vec<PyRep>> = dungeons
                .into_iter()
                .map(|d| {
                    vec![
                        PyRep::Integer(d.dungeon_id as i64),
                        PyRep::String(d.name.clone()),
                        PyRep::Integer(1), // dungeonStatus: 1 = RELEASE
                        ctx.sde
                            .dungeon_name_id(d.dungeon_id)
                            .map_or(PyRep::None, |id| PyRep::Integer(i64::from(id))),
                        PyRep::Integer(d.faction_id.unwrap_or(0) as i64),
                        PyRep::Integer(d.archetype_id.unwrap_or(0) as i64),
                    ]
                })
                .collect();
            return Some(Ok(PyRep::rowset(
                vec![
                    "dungeonID",
                    "dungeonName",
                    "dungeonStatus",
                    "dungeonNameID",
                    "factionID",
                    "archetypeID",
                ],
                rows,
            )));
        }

        ("dungeon", "DEGetFactions") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "dungeon.DEGetFactions requires GM/Content role".into(),
                    client_error: None,
                }));
            }
            let mut factions: Vec<_> = ctx.sde.factions.values().collect();
            factions.sort_by_key(|f| f.faction_id);
            let rows: Vec<Vec<PyRep>> = factions
                .into_iter()
                .map(|f| {
                    vec![
                        PyRep::Integer(f.faction_id as i64),
                        PyRep::String(f.name.clone()),
                    ]
                })
                .collect();
            return Some(Ok(PyRep::rowset(vec!["factionID", "factionName"], rows)));
        }

        ("dungeon", "DEGetRoomObjectPaletteData") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "dungeon.DEGetRoomObjectPaletteData requires GM/Content role".into(),
                    client_error: None,
                }));
            }
            // Populate object palette dynamically across all SDE dungeon-placeable categories:
            // celestials/gates/beacons/LCOs, asteroids/ice/gas clouds, deployables, structures, and NPC entities.
            let mut dungeon_tids: BTreeSet<u32> = BTreeSet::new();
            for list in ctx.sde.dungeon_asteroids.values() {
                for entry in list {
                    dungeon_tids.insert(entry.type_id);
                }
            }
            for list in ctx.sde.dungeon_gas_types.values() {
                for &tid in list {
                    dungeon_tids.insert(tid);
                }
            }
            for grp in ctx.sde.npc_spawn_groups.values() {
                if let Some(&tid) = grp.npc_type_ids.first() {
                    dungeon_tids.insert(tid);
                }
            }
            let mut palette_types: Vec<(u32, &str)> = ctx
                .sde
                .types
                .values()
                .filter(|t| {
                    matches!(t.group_id, 10 | 310 | 311 | 366 | 417 | 473 | 711)
                        || dungeon_tids.contains(&t.type_id)
                        || (matches!(t.category_id, 22 | 23 | 25 | 65) && t.published)
                })
                .map(|t| (t.type_id, t.name.as_str()))
                .collect();
            palette_types.sort_by_key(|(tid, _)| *tid);
            let list: Vec<PyRep> = palette_types
                .into_iter()
                .map(|(tid, name)| {
                    PyRep::Tuple(vec![
                        PyRep::Integer(tid as i64),
                        PyRep::String(name.to_string()),
                    ])
                })
                .collect();
            return Some(Ok(PyRep::List(list)));
        }

        ("dungeon", "DEGetRooms") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "dungeon.DEGetRooms requires GM/Content role".into(),
                    client_error: None,
                }));
            }
            let Some(dungeon_id) = kwargs
                .get("dungeonID")
                .or_else(|| kwargs.get("dungeonVID"))
                .and_then(|v| v.as_i64())
                .or_else(|| args.first().and_then(|a| a.as_i64()))
                .filter(|&id| id > 0)
            else {
                return Some(Err(BridgeRpcError::user_error("DungeonIDRequired")));
            };
            let Some(d) = u32::try_from(dungeon_id)
                .ok()
                .and_then(|did| ctx.sde.dungeons.get(&did))
            else {
                return Some(Err(BridgeRpcError::user_error("DungeonNotFound")));
            };
            let rooms = ensure_dungeon_rooms_seeded(ctx, d);
            let rows: Vec<Vec<PyRep>> = rooms
                .into_iter()
                .map(|r| {
                    vec![
                        PyRep::Integer(r.room_id),
                        PyRep::String(r.room_name),
                        PyRep::Integer(r.dungeon_id),
                    ]
                })
                .collect();
            return Some(Ok(PyRep::rowset(vec!["roomID", "roomName", "dungeonID"], rows)));
        }

        (
            "dungeon",
            "EditRoom"
            | "SaveRoom"
            | "DeleteGroup"
            | "GetDunObjects"
            | "DropIntoDungeonRoom"
            | "SetObjectPosition"
            | "SetObjectRotation"
            | "SetObjectRadius"
            | "SetObjectQuantity"
            | "GetObjectPosition"
            | "GetObjectRotation"
            | "GetObjectRadius"
            | "GetObjectQuantity"
            | "IsObjectLocked"
            | "DeleteSelected"
            | "DeleteGroupObjects"
            | "SaveAllChanges"
            | "ResetDungeon",
        )
        // parity: rpc:keeper.GotoRoom
        // parity: rpc:keeper.PlayDungeon
        | (
            "keeper",
            "Bind"
            | "EditDungeon"
            | "EditRoom"
            | "PlayDungeon"
            | "GotoRoom"
            | "GetCurrentlyEditedRoomID"
            | "GetCurrentRoomID"
            | "GetEditingDungeonID"
            | "GetEditingRoomID"
            | "GetDunObjects"
            | "ObjectSelection"
            | "Reset"
            | "ResetDungeon",
        ) => {
            return Some(handle_gm_dungeon_editor_op(ctx, session, method, args, kwargs).await);
        }

        ("keeper", "GetCurrentDungeonForCharacter") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let abyssal_dungeon =
                crate::services::pve_special_sites_abyssal::read_abyssal_state(ctx, |st| {
                    let p = st.pockets.get(&char_id)?;
                    if p.completed || p.collapsed_destroyed {
                        return None;
                    }
                    let room_idx = (p.current_room.saturating_sub(1) as usize).min(2);
                    let r = &p.rooms[room_idx];
                    let room_id = ctx
                        .sde
                        .dungeon_asteroids
                        .get(&r.dungeon_id)
                        .and_then(|rows| rows.first().map(|e| e.room_id))
                        .unwrap_or(
                            r.dungeon_id
                                .saturating_mul(100)
                                .saturating_add(u32::from(p.current_room)),
                        );
                    Some((r.dungeon_id, room_id, p.pocket_id, r.dungeon_name.clone()))
                });
            let active_dungeon = abyssal_dungeon.or_else(|| {
                crate::services::operations_manager::active_operation_dungeon_for_character(
                    ctx, char_id,
                )
            });
            let rep = match active_dungeon {
                Some((dungeon_id, room_id, instance_id, dname)) => PyRep::Tuple(vec![
                    PyRep::Integer(dungeon_id as i64),
                    PyRep::Integer(room_id as i64),
                    PyRep::Integer(instance_id),
                    PyRep::keyval([
                        ("dungeonID", PyRep::Integer(dungeon_id as i64)),
                        ("dungeonName", PyRep::String(dname)),
                        ("roomID", PyRep::Integer(room_id as i64)),
                        ("instanceID", PyRep::Integer(instance_id)),
                    ]),
                ]),
                None => PyRep::None,
            };
            return Some(Ok(rep));
        }

        ("keeper", "GetLevelEditor") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "keeper.GetLevelEditor requires GM role".into(),
                    client_error: None,
                }));
            }
            let target_oid = session.solar_system_id.map(i64::from).unwrap_or(char_id);
            let bound_id = match ctx.bind_object(session, "keeper", target_oid) {
                Ok(id) => id,
                Err(e) => return Some(Err(e)),
            };
            return Some(Ok(PyRep::keyval([
                ("boundOID", PyRep::Integer(bound_id as i64)),
                ("isEditorActive", PyRep::Bool(true)),
                ("sessionID", PyRep::Integer(session.session_id as i64)),
            ])));
        }

        ("keeper", "GetOperationSpawnpointSolarSystem") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_char_id = args
                .first()
                .and_then(|a| a.as_i64())
                .filter(|&id| id >= 90_000_000)
                .unwrap_or(char_id);
            if target_char_id != char_id && !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "UserError".into(),
                    message: "cannot query another character".into(),
                    client_error: None,
                }));
            }
            let operation_id = if args.len() >= 2 {
                args.get(1).and_then(|a| a.as_i64())
            } else {
                args.first()
                    .and_then(|a| a.as_i64())
                    .filter(|&id| id > 0 && id < 30_000_000)
            };
            let sys_opt =
                crate::services::operations_manager::resolve_operation_spawnpoint_solar_system(
                    ctx,
                    target_char_id,
                    operation_id,
                );
            return Some(Ok(sys_opt
                .map(|s| PyRep::Integer(s as i64))
                .unwrap_or(PyRep::None)));
        }

        ("keeper", "IsOperationSite") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_id = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| session.solar_system_id.map(i64::from))
                .unwrap_or(0);
            let is_op = crate::services::operations_manager::is_operation_site_or_system(
                ctx, char_id, target_id,
            );
            return Some(Ok(PyRep::Bool(is_op)));
        }

        _ => {}
    }
    // parity: feat:pve.agency-content
    // parity: rpc:custom_agency_provider.get_content_data
    if service == "custom_agency_provider" && method == "get_content_data" {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(sys_id) = session.solar_system_id else {
            return Some(Err(BridgeRpcError {
                code: "UnknownSolarSystem".into(),
                message: "custom_agency_provider.get_content_data requires a session solar system".into(),
                client_error: None,
            }));
        };
        let Some(sys_info) = ctx.sde.solar_systems.get(&sys_id) else {
            return Some(Err(BridgeRpcError {
                code: "UnknownSolarSystem".into(),
                message: format!("Unknown solar system {sys_id}"),
                client_error: None,
            }));
        };
        let mut content_pieces = Vec::new();
        let sec = sys_info.security_status;
        let is_seeded = read_pve_exploration_reg(ctx, |reg| {
            reg.seeded_systems.get(&sys_id).copied().unwrap_or(false)
        });
        let mut anomalies = registry_instances(ctx, sys_id, false);
        if !is_seeded && anomalies.is_empty() {
            anomalies = unseeded_agency_anomaly_instances(ctx, sys_id);
        }
        for (did, instances) in anomalies {
            for iid in instances {
                content_pieces.push(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                    ("contentType", PyRep::String("combatAnomaly".into())),
                    ("content_type", PyRep::Integer(1)),
                    ("dungeonID", PyRep::Integer(i64::from(did))),
                    ("instanceID", PyRep::Integer(iid)),
                    ("security", PyRep::Real(sec)),
                    ("jumps", PyRep::Integer(0)),
                ]));
            }
        }
        // Only expose `probe_required` cosmic signatures that `char_id` has already scanned to 100% (`#520`).
        let scanned_sigs: Vec<(u32, i64)> = read_pve_exploration_reg(ctx, |reg| {
            let now = crate::services::pve_exploration::now_sec(reg);
            reg.sites_by_system
                .get(&sys_id)
                .into_iter()
                .flatten()
                .filter(|s| {
                    s.probe_required
                        && !s.cleared
                        && (s.lifetime_seconds == 0
                            || now < s.created_unix.saturating_add(s.lifetime_seconds))
                        && is_site_scanned_by_character(ctx, char_id, sys_id, &s.site_id)
                })
                .map(|s| (s.dungeon_id, registry_instance_id(&s.site_id)))
                .collect()
        });
        for (did, iid) in scanned_sigs {
            content_pieces.push(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                ("contentType", PyRep::String("signature".into())),
                ("content_type", PyRep::Integer(2)),
                ("dungeonID", PyRep::Integer(i64::from(did))),
                ("instanceID", PyRep::Integer(iid)),
                ("security", PyRep::Real(sec)),
                ("jumps", PyRep::Integer(0)),
            ]));
        }
        return Some(Ok(PyRep::List(content_pieces)));
    }

    // parity: rpc:custom_agency_provider.warp_to_content
    if service == "custom_agency_provider" && method == "warp_to_content" {
        let (_, char_id) = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(sys_id) = session.solar_system_id else {
            return Some(Err(BridgeRpcError {
                code: "UnknownSolarSystem".into(),
                message: "custom_agency_provider.warp_to_content requires a session solar system".into(),
                client_error: None,
            }));
        };
        if session.station_id.is_some() || session.structure_id.is_some() {
            return Some(Err(BridgeRpcError {
                code: "NotInSpace".into(),
                message: "Cannot warp while docked in station or structure".into(),
                client_error: None,
            }));
        }
        let Some(ship_id) = session.ship_id else {
            return Some(Err(BridgeRpcError {
                code: "NoActiveShip".into(),
                message: "Session has no active ship".into(),
                client_error: None,
            }));
        };
        if let Err(e) = crate::services::structure_control_ops::reject_if_controlling_structure(ctx, ship_id, "warp to content") {
            return Some(Err(e));
        }

        // Extract content identifier from argument (no client raw coordinates or arbitrary celestials accepted)
        let (target_iid, target_did) = match args.first() {
            Some(PyRep::Integer(id)) => (Some(*id), None),
            Some(rep) if matches!(rep, PyRep::Dict(_) | PyRep::KeyVal { .. }) => {
                let iid = rep.dict_get("instanceID").and_then(|v| v.as_i64());
                let did = rep
                    .dict_get("dungeonID")
                    .and_then(|v| v.as_i64())
                    .and_then(|d| u32::try_from(d).ok());
                (iid, did)
            }
            _ => {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "custom_agency_provider.warp_to_content requires contentPiece or contentID".into(),
                    client_error: None,
                }));
            }
        };

        let (mut resolved_pos, unscanned_blocked, pve_seeded, mining_seeded) =
            resolve_warp_target_readonly(ctx, char_id, sys_id, target_iid, target_did);
        if unscanned_blocked {
            return Some(Err(BridgeRpcError {
                code: "SignatureNotScanned".into(),
                message: "Cosmic signature must be probe-scanned to 100% before warping".into(),
                client_error: None,
            }));
        }

        if resolved_pos.is_none() {
            let needs = matching_unseeded_seed_needs(
                ctx,
                char_id,
                sys_id,
                target_iid,
                target_did,
                pve_seeded,
                mining_seeded,
            );
            if needs.unscanned_sig {
                return Some(Err(BridgeRpcError {
                    code: "SignatureNotScanned".into(),
                    message: "Cosmic signature must be probe-scanned to 100% before warping".into(),
                    client_error: None,
                }));
            }
            if !needs.need_pve && !needs.need_mining && !needs.need_ice {
                return Some(Err(rpc_msg_err(
                    "SiteNotFound",
                    format!("Target content site ({target_iid:?}, {target_did:?}) not found in system {sys_id}"),
                )));
            }
            let Some(pre_handle) = ctx.systems.get_handle(sys_id) else {
                return Some(Err(rpc_msg_err(
                    "ShipNotInSpace",
                    "Active ship is not spawned in Ballpark",
                )));
            };
            let pre_ball = match pre_handle.get_ball_snapshot(ship_id).await {
                Ok(Some((_, ball))) => ball,
                Ok(None) => {
                    return Some(Err(rpc_msg_err(
                        "ShipNotInSpace",
                        "Active ship is not spawned in Ballpark",
                    )));
                }
                Err(e) => {
                    return Some(Err(rpc_msg_err("DestinyCommandFailed", e)));
                }
            };
            if let Err(e) = validate_pre_agency_warp(
                ctx,
                &pre_handle,
                char_id,
                ship_id,
                sys_id,
                &pre_ball,
            )
            .await
            {
                return Some(Err(e));
            }
            if needs.need_pve {
                Box::pin(ensure_system_pve_sites_seeded(ctx, sys_id)).await;
            }
            if needs.need_mining {
                Box::pin(seed_system_mining_sites_from_sde(ctx, sys_id)).await;
            }
            if needs.need_ice {
                seed_system_ice_belt_from_sde(ctx, sys_id);
            }
            let (re_pos, re_unscanned, _, _) =
                resolve_warp_target_readonly(ctx, char_id, sys_id, target_iid, target_did);
            if re_unscanned {
                return Some(Err(BridgeRpcError {
                    code: "SignatureNotScanned".into(),
                    message: "Cosmic signature must be probe-scanned to 100% before warping".into(),
                    client_error: None,
                }));
            }
            resolved_pos = re_pos;
        }

        let dest_pos = match resolved_pos {
            Some(p) if p.is_finite() => p,
            Some(_) => {
                return Some(Err(BridgeRpcError {
                    code: "InvalidCoordinates".into(),
                    message: "Resolved site coordinates are not finite".into(),
                    client_error: None,
                }));
            }
            None => {
                return Some(Err(BridgeRpcError {
                    code: "SiteNotFound".into(),
                    message: format!("Target content site ({target_iid:?}, {target_did:?}) not found in system {sys_id}"),
                    client_error: None,
                }));
            }
        };

        let Some(handle) = ctx.systems.get_handle(sys_id) else {
            return Some(Err(BridgeRpcError {
                code: "ShipNotInSpace".into(),
                message: "Active ship is not spawned in Ballpark".into(),
                client_error: None,
            }));
        };
        let Some((_, current_ball)) = (match handle.get_ball_snapshot(ship_id).await {
            Ok(v) => v,
            Err(e) => return Some(Err(BridgeRpcError { code: "DestinyCommandFailed".into(), message: e, client_error: None })),
        }) else {
            return Some(Err(BridgeRpcError {
                code: "ShipNotInSpace".into(),
                message: "Active ship is not spawned in Ballpark".into(),
                client_error: None,
            }));
        };

        let warp_dist = current_ball.position.distance(dest_pos);
        if warp_dist < 150_000.0 {
            return Some(Err(BridgeRpcError::warp_distance_too_short(format!(
                "Cannot warp {:.1} m (minimum warp distance is 150,000 m)",
                warp_dist
            ))));
        }

        if let Err(e) = crate::services::nav_warp_interdiction::execute_agency_content_warp(
            ctx,
            &handle,
            char_id,
            ship_id,
            sys_id,
            &current_ball,
            dest_pos,
        )
        .await
        {
            return Some(Err(e));
        }

        return Some(Ok(PyRep::None));
    }

    if service != "dungeonInstanceCacheMgr" {
        return None;
    }

    if method == "GetFactionWarfareBattlefieldInstances" {
        if let Err(e) = ServerContext::require_character(session) {
            return Some(Err(e));
        }
        if let Some(sys_id) = args
            .first()
            .and_then(|a| a.as_i64())
            .and_then(|v| u32::try_from(v).ok())
            .or(session.solar_system_id)
        {
            if session.solar_system_id == Some(sys_id)
                && session.station_id.is_none()
                && session.structure_id.is_none()
            {
                crate::services::fw_gameplay::ensure_warzone_sites_spawned(ctx, sys_id).await;
            }
        }
        return None;
    }
    if !matches!(
        method,
        "GetCombatAnomalyInstances"
            | "GetOreAnomalyInstances"
            | "GetIceBeltInstances"
            | "GetSignatureInstances"
            | "GetCombatAnomaliesCount"
            | "GetOreAnomaliesCount"
            | "GetOreAnomaliesCountInRange"
            | "GetIceBeltsCount"
            | "GetBulwarkMiningSiteCounts"
            | "GetFactionWarfareInstances"
            | "GetHomefrontSiteInstances"
    ) {
        return None;
    }
    if let Err(e) = ServerContext::require_character(session) {
        return Some(Err(e));
    }
    let Some(sys_id) = session.solar_system_id else {
        return Some(Err(BridgeRpcError {
            code: "UnknownSolarSystem".into(),
            message: format!("dungeonInstanceCacheMgr.{method} requires a session solar system"),
            client_error: None,
        }));
    };
    let in_system_undocked = session.station_id.is_none() && session.structure_id.is_none();

    match method {
        "GetCombatAnomalyInstances" => {
            let by_dungeon = if in_system_undocked {
                ensure_system_pve_sites_seeded(ctx, sys_id).await;
                crate::services::pve_special_sites::ensure_special_sites_spawned_for_system(
                    ctx, sys_id,
                )
                .await;
                let _ = crate::services::pve_special_sites::ensure_seasonal_event_sites_for_system(
                    ctx, sys_id,
                )
                .await;
                let mut out = registry_instances(ctx, sys_id, false);
                for (did, iids) in
                    crate::services::pve_special_sites_combat::active_seasonal_anomaly_instances_readonly(
                        ctx, sys_id,
                    )
                {
                    out.entry(did).or_default().extend(iids);
                }
                out
            } else {
                combat_anomaly_instances_readonly(ctx, sys_id, true)
            };
            Some(Ok(instances_reply(sys_id, by_dungeon)))
        }
        "GetCombatAnomaliesCount" => {
            let instances = if in_system_undocked {
                ensure_system_pve_sites_seeded(ctx, sys_id).await;
                registry_instances(ctx, sys_id, false)
            } else {
                combat_anomaly_instances_readonly(ctx, sys_id, false)
            };
            let count: usize = instances.values().map(|v| v.len()).sum();
            Some(Ok(PyRep::Dict(vec![(
                PyRep::Integer(sys_id as i64),
                PyRep::Integer(count as i64),
            )])))
        }
        "GetOreAnomalyInstances" => {
            let by_dungeon = if in_system_undocked {
                seed_system_mining_sites_from_sde(ctx, sys_id).await;
                anomaly_instances(ctx, sys_id, "OreAnomaly")
            } else {
                ore_anomaly_instances_readonly(ctx, sys_id)
            };
            Some(Ok(instances_reply(sys_id, by_dungeon)))
        }
        "GetOreAnomaliesCount" => {
            let instances = if in_system_undocked {
                seed_system_mining_sites_from_sde(ctx, sys_id).await;
                anomaly_instances(ctx, sys_id, "OreAnomaly")
            } else {
                ore_anomaly_instances_readonly(ctx, sys_id)
            };
            let count: usize = instances.values().map(|v| v.len()).sum();
            Some(Ok(PyRep::Dict(vec![(
                PyRep::Integer(sys_id as i64),
                PyRep::Integer(count as i64),
            )])))
        }
        "GetOreAnomaliesCountInRange" => {
            let max_jumps = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| {
                    kwargs
                        .get("maxJumps")
                        .or_else(|| kwargs.get("range"))
                        .and_then(|v| v.as_i64())
                })
                .unwrap_or(2)
                .clamp(0, 10) as usize;
            let systems = reachable_solar_systems_within_jumps(&ctx.sde, sys_id, max_jumps);
            let mut entries = Vec::with_capacity(systems.len());
            for s in systems {
                let instances = ore_anomaly_instances_readonly(ctx, s);
                let count: usize = instances.values().map(|v| v.len()).sum();
                entries.push((PyRep::Integer(s as i64), PyRep::Integer(count as i64)));
            }
            Some(Ok(PyRep::Dict(entries)))
        }
        "GetIceBeltInstances" => {
            let by_dungeon = if in_system_undocked {
                seed_system_ice_belt_from_sde(ctx, sys_id);
                anomaly_instances(ctx, sys_id, ICE_BELT_ANOMALY_TYPE)
            } else {
                ice_belt_instances_readonly(ctx, sys_id)
            };
            Some(Ok(instances_reply(sys_id, by_dungeon)))
        }
        "GetIceBeltsCount" => {
            let instances = if in_system_undocked {
                seed_system_ice_belt_from_sde(ctx, sys_id);
                anomaly_instances(ctx, sys_id, ICE_BELT_ANOMALY_TYPE)
            } else {
                ice_belt_instances_readonly(ctx, sys_id)
            };
            let count: usize = instances.values().map(|v| v.len()).sum();
            Some(Ok(PyRep::Dict(vec![(
                PyRep::Integer(sys_id as i64),
                PyRep::Integer(count as i64),
            )])))
        }
        "GetBulwarkMiningSiteCounts" => {
            let now = ctx.now_unix();
            let counts: BTreeMap<SolarSystemId, i64> = ctx.db.with_snapshot(|snap| {
                let mut map: BTreeMap<SolarSystemId, i64> = BTreeMap::new();
                map.insert(sys_id, 0);
                for (&s, list) in &snap.cosmic_anomalies {
                    for a in list {
                        if a.cleared
                            || (a.lifetime_seconds > 0
                                && now >= a.created_unix.saturating_add(a.lifetime_seconds))
                        {
                            continue;
                        }
                        let is_bulwark = a.anomaly_type == "BulwarkMiningSite"
                            || parse_dng_site_id(&a.anomaly_id)
                                .and_then(|(did, _)| ctx.sde.dungeons.get(&did))
                                .is_some_and(|d| d.archetype_id == Some(81));
                        if is_bulwark {
                            *map.entry(s).or_insert(0) += 1;
                        }
                    }
                }
                map
            });
            let entries: Vec<(PyRep, PyRep)> = counts
                .into_iter()
                .map(|(s, c)| (PyRep::Integer(s as i64), PyRep::Integer(c)))
                .collect();
            Some(Ok(PyRep::Dict(entries)))
        }
        "GetFactionWarfareInstances" => {
            if in_system_undocked {
                crate::services::fw_gameplay::ensure_warzone_sites_spawned(ctx, sys_id).await;
            }
            let by_dng: BTreeMap<u32, Vec<i64>> = ctx.db.with_snapshot(|snap| {
                let mut out: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
                for (&cid, (csys, tier, fac, _, completed)) in &snap.fw_runtime.fw_complexes {
                    if *csys == sys_id && !*completed {
                        let arch_id: u32 = match tier.as_str() {
                            "Novice" => 32,
                            "Small" => 33,
                            "Medium" => 34,
                            "Large" => 35,
                            _ => 36,
                        };
                        let did = ctx
                            .sde
                            .dungeons
                            .values()
                            .filter(|d| {
                                d.archetype_id == Some(arch_id)
                                    && (d.faction_id == Some(*fac as u32) || d.faction_id.is_none())
                            })
                            .map(|d| d.dungeon_id)
                            .min()
                            .unwrap_or(arch_id);
                        out.entry(did).or_default().push(cid);
                    }
                }
                for bf in &snap.fw_runtime.battlefield_instances {
                    if bf.solar_system_id == sys_id && !bf.completed {
                        let bf_did = ctx
                            .sde
                            .dungeons
                            .values()
                            .filter(|d| {
                                d.archetype_id == Some(68)
                                    && (d.faction_id == Some(bf.faction_id as u32)
                                        || d.faction_id.is_none())
                            })
                            .map(|d| d.dungeon_id)
                            .min()
                            .or_else(|| {
                                ctx.sde
                                    .dungeons
                                    .values()
                                    .filter(|d| matches!(d.archetype_id, Some(33..=36 | 68)))
                                    .map(|d| d.dungeon_id)
                                    .min()
                            });
                        if let Some(did) = bf_did {
                            out.entry(did).or_default().push(bf.instance_id);
                        }
                    }
                }
                for a in snap.cosmic_anomalies.get(&sys_id).into_iter().flatten() {
                    if !a.cleared {
                        if let Some((did, iid)) = parse_dng_site_id(&a.anomaly_id) {
                            if ctx
                                .sde
                                .dungeons
                                .get(&did)
                                .is_some_and(|d| matches!(d.archetype_id, Some(32..=36 | 67 | 68)))
                            {
                                out.entry(did).or_default().push(iid);
                            }
                        }
                    }
                }
                out
            });
            Some(Ok(instances_reply(sys_id, by_dng)))
        }
        "GetHomefrontSiteInstances" => {
            let res = crate::services::pve_homefront_sites::handle_get_homefront_site_instances(ctx, session, args);
            let target_sys = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|raw| u32::try_from(raw).ok())
                .unwrap_or(sys_id);
            if in_system_undocked && session.solar_system_id == Some(target_sys) {
                crate::services::pve_special_sites::ensure_special_sites_spawned_for_system(ctx, target_sys).await;
            }
            Some(res)
        }
        _ => {
            // GetSignatureInstances: probe-only exploration sites plus SDE-seeded ore/gas signatures.
            if in_system_undocked {
                ensure_system_pve_sites_seeded(ctx, sys_id).await;
                seed_system_mining_sites_from_sde(ctx, sys_id).await;
            }
            let mut out = registry_instances(ctx, sys_id, true);
            let now = ctx.now_unix();
            ctx.db.with_snapshot(|snap| {
                for s in snap.cosmic_signatures.get(&sys_id).into_iter().flatten() {
                    if s.collapsed
                        || !matches!(s.sig_group.as_str(), "Ore" | "Gas")
                        || (s.lifetime_seconds > 0 && now >= s.created_unix.saturating_add(s.lifetime_seconds))
                    {
                        continue;
                    }
                    if let Some((did, iid)) = parse_dng_site_id(&s.signature_id) {
                        out.entry(did).or_default().push(iid);
                    }
                }
            });
            Some(Ok(instances_reply(sys_id, out)))
        }
    }
}

#[cfg(test)]
#[path = "pve_dungeon_cache_tests.rs"]
mod pve_dungeon_cache_tests;
