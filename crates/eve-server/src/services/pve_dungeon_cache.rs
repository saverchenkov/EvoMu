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
    derive_system_site_position_from_sde, displayed_security, seed_system_mining_sites_from_sde,
};
use crate::services::pve_exploration::{
    ensure_system_pve_sites_seeded, with_pve_exploration_reg,
};
use eve_db::CosmicAnomalyRecord;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use std::collections::{BTreeMap, HashMap};

/// Client `appConst.dunArchetypeIceBelt`. Every `* Ice Belt` / `* Ice Field` dungeon in the SDE's
/// `sde_dungeons` table carries archetype 28, which is how ice-belt dungeons are discovered here.
pub const DUNGEON_ARCHETYPE_ICE_BELT: u32 = 28;

/// `cosmic_anomalies.anomaly_type` used for seeded ice belts.
pub const ICE_BELT_ANOMALY_TYPE: &str = "IceBelt";

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

/// Picks the ice-belt dungeon seeded in `sys_id`, if any.
///
/// DERIVED: CCP's server-side ice-belt distribution is not part of the client SDE. EvoMu gives every
/// known-space system one belt: among the eligible belts it keeps those with the tightest security
/// ceiling (the richest ice the system's band allows) and picks one by `sys_id` modulo their count.
/// Wormhole and Abyssal systems (IDs >= 31,000,000) get none.
pub fn ice_belt_dungeon_for_system(ctx: &ServerContext, sys_id: SolarSystemId) -> Option<u32> {
    if sys_id >= 31_000_000 {
        return None;
    }
    let true_sec = ctx.sde.solar_systems.get(&sys_id)?.security_status;
    let eligible = eligible_ice_belts(&ctx.sde, displayed_security(true_sec));
    let tightest = eligible
        .iter()
        .map(|(_, c)| *c)
        .fold(f64::INFINITY, f64::min);
    let best: Vec<u32> = eligible
        .iter()
        .filter(|(_, c)| (*c - tightest).abs() < 1e-9)
        .map(|(id, _)| *id)
        .collect();
    if best.is_empty() {
        return None;
    }
    Some(best[(sys_id as usize) % best.len()])
}

/// Seeds the system's ice-belt anomaly once (idempotent). The record's `anomaly_id` is
/// `DNG-<dungeonID>-<instanceID>` like the ore anomalies, and `ore_types` lists the belt's ice types
/// from the SDE dungeon layout. Ice asteroid balls are not materialised here.
pub fn seed_system_ice_belt_from_sde(ctx: &ServerContext, sys_id: SolarSystemId) {
    let Some(dungeon_id) = ice_belt_dungeon_for_system(ctx, sys_id) else {
        return;
    };
    let name = ctx
        .sde
        .dungeons
        .get(&dungeon_id)
        .map(|d| d.name.trim().to_string())
        .unwrap_or_default();
    let mut ice_types: Vec<TypeId> = ctx
        .sde
        .dungeon_asteroids
        .get(&dungeon_id)
        .map(|rows| rows.iter().map(|r| r.type_id).collect())
        .unwrap_or_default();
    ice_types.sort_unstable();
    ice_types.dedup();
    let position = derive_system_site_position_from_sde(ctx, sys_id, 3, 5.0);
    let now = ctx.now_unix();
    ctx.db.with_mut_snapshot(|snap| {
        let list = snap.cosmic_anomalies.entry(sys_id).or_default();
        if list.iter().any(|a| a.anomaly_type == ICE_BELT_ANOMALY_TYPE) {
            return;
        }
        snap.next_entity_id += 1;
        let seq = snap.next_entity_id;
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(CosmicAnomalyRecord {
                anomaly_id: format!("DNG-{dungeon_id}-{seq}"),
                solar_system_id: sys_id,
                name,
                anomaly_type: ICE_BELT_ANOMALY_TYPE.into(),
                position,
                signal_strength: 1.0,
                ore_types: ice_types,
                remaining_volume: 0.0,
                created_unix: now,
                lifetime_seconds: 0,
                cleared: false,
            });
    });
}

/// Parses `DNG-<dungeonID>-<instanceID>` site IDs used by the SDE-seeded mining/ice sites.
pub fn parse_dng_site_id(site_id: &str) -> Option<(u32, i64)> {
    let rest = site_id.strip_prefix("DNG-")?;
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
    ctx.db.with_snapshot(|snap| {
        for a in snap.cosmic_anomalies.get(&sys_id).into_iter().flatten() {
            if a.anomaly_type != kind || a.cleared {
                continue;
            }
            if let Some((did, iid)) = parse_dng_site_id(&a.anomaly_id) {
                out.entry(did).or_default().push(iid);
            }
        }
    });
    out
}

fn registry_instances(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    probe_required: bool,
) -> BTreeMap<u32, Vec<i64>> {
    with_pve_exploration_reg(ctx, |reg| {
        let mut out: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
        for s in reg.sites_by_system.get(&sys_id).into_iter().flatten() {
            if s.probe_required == probe_required && !s.cleared {
                out.entry(s.dungeon_id)
                    .or_default()
                    .push(registry_instance_id(&s.site_id));
            }
        }
        out
    })
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    _args: &[PyRep],
    _kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    // parity: feat:pve.agency-content
    // parity: rpc:custom_agency_provider.get_content_data
    if service == "custom_agency_provider" && method == "get_content_data" {
        if let Err(e) = ServerContext::require_character(session) {
            return Some(Err(e));
        }
        let Some(sys_id) = session.solar_system_id else {
            return Some(Err(BridgeRpcError {
                code: "UnknownSolarSystem".into(),
                message: "custom_agency_provider.get_content_data requires a session solar system".into(),
            }));
        };
        ensure_system_pve_sites_seeded(ctx, sys_id).await;
        seed_system_mining_sites_from_sde(ctx, sys_id).await;
        let mut content_pieces = Vec::new();
        let sec = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status).unwrap_or(1.0);
        let anomalies = registry_instances(ctx, sys_id, false);
        for (did, instances) in anomalies {
            for iid in instances {
                content_pieces.push(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                    ("contentType", PyRep::String("combatAnomaly".into())),
                    ("dungeonID", PyRep::Integer(i64::from(did))),
                    ("instanceID", PyRep::Integer(iid)),
                    ("security", PyRep::Real(sec)),
                    ("jumps", PyRep::Integer(0)),
                ]));
            }
        }
        let sigs = registry_instances(ctx, sys_id, true);
        for (did, instances) in sigs {
            for iid in instances {
                content_pieces.push(PyRep::keyval([
                    ("solarSystemID", PyRep::Integer(i64::from(sys_id))),
                    ("contentType", PyRep::String("signature".into())),
                    ("dungeonID", PyRep::Integer(i64::from(did))),
                    ("instanceID", PyRep::Integer(iid)),
                    ("security", PyRep::Real(sec)),
                    ("jumps", PyRep::Integer(0)),
                ]));
            }
        }
        return Some(Ok(PyRep::List(content_pieces)));
    }

    if !service.ends_with("InstanceCacheMgr") {
        return None;
    }

    if let Err(e) = ServerContext::require_character(session) {
        return Some(Err(e));
    }
    let Some(sys_id) = session.solar_system_id else {
        return Some(Err(BridgeRpcError {
            code: "UnknownSolarSystem".into(),
            message: format!("{service}.{method} requires a session solar system"),
        }));
    };

    match (service, method) {
        ("dungeonInstanceCacheMgr", "GetCombatAnomaliesCount") => {
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            let count: usize = registry_instances(ctx, sys_id, false).values().map(|v| v.len()).sum();
            Some(Ok(PyRep::Integer(count as i64)))
        }
        ("dungeonInstanceCacheMgr", "GetOreAnomaliesCount") => {
            seed_system_mining_sites_from_sde(ctx, sys_id).await;
            let count: usize = anomaly_instances(ctx, sys_id, "OreAnomaly").values().map(|v| v.len()).sum();
            Some(Ok(PyRep::Integer(count as i64)))
        }
        ("dungeonInstanceCacheMgr", "GetOreAnomaliesCountInRange") => {
            seed_system_mining_sites_from_sde(ctx, sys_id).await;
            let count: usize = anomaly_instances(ctx, sys_id, "OreAnomaly").values().map(|v| v.len()).sum();
            Some(Ok(PyRep::Dict(vec![(
                PyRep::Integer(sys_id as i64),
                PyRep::Integer(count as i64),
            )])))
        }
        ("dungeonInstanceCacheMgr", "GetIceBeltsCount") => {
            seed_system_ice_belt_from_sde(ctx, sys_id);
            let count: usize = anomaly_instances(ctx, sys_id, ICE_BELT_ANOMALY_TYPE).values().map(|v| v.len()).sum();
            Some(Ok(PyRep::Integer(count as i64)))
        }
        ("dungeonInstanceCacheMgr", "GetBulwarkMiningSiteCounts") => {
            if _args.is_empty() {
                return None;
            }
            seed_system_mining_sites_from_sde(ctx, sys_id).await;
            let count: usize = anomaly_instances(ctx, sys_id, "BulwarkMiningSite").values().map(|v| v.len()).sum();
            Some(Ok(PyRep::Dict(vec![(
                PyRep::Integer(sys_id as i64),
                PyRep::Integer(count as i64),
            )])))
        }
        ("dungeonInstanceCacheMgr", "GetFactionWarfareInstances")
        | ("dungeonInstanceCacheMgr", "GetFactionWarfareBattlefieldInstances")
        | ("dungeonInstanceCacheMgr", "GetHomefrontSiteInstances")
        | ("dungeonInstanceCacheMgr", "GetPirateInsurgencyInstances")
        | ("dungeonInstanceCacheMgr", "GetPirateInsurgencyIceHeistInstances")
        | ("dungeonInstanceCacheMgr", "GetTriglavianSiteInstances")
        | ("dungeonInstanceCacheMgr", "GetCombatAnomalyInstances") => {
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            let by_dungeon = registry_instances(ctx, sys_id, false);
            Some(Ok(instances_reply(sys_id, by_dungeon)))
        }
        ("dungeonInstanceCacheMgr", "GetOreAnomalyInstances") => {
            seed_system_mining_sites_from_sde(ctx, sys_id).await;
            let by_dungeon = anomaly_instances(ctx, sys_id, "OreAnomaly");
            Some(Ok(instances_reply(sys_id, by_dungeon)))
        }
        ("dungeonInstanceCacheMgr", "GetIceBeltInstances") => {
            seed_system_ice_belt_from_sde(ctx, sys_id);
            let by_dungeon = anomaly_instances(ctx, sys_id, ICE_BELT_ANOMALY_TYPE);
            Some(Ok(instances_reply(sys_id, by_dungeon)))
        }
        ("dungeonInstanceCacheMgr", "GetSignatureInstances") => {
            // GetSignatureInstances: probe-only exploration sites plus SDE-seeded ore/gas signatures.
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            seed_system_mining_sites_from_sde(ctx, sys_id).await;
            let mut out = registry_instances(ctx, sys_id, true);
            ctx.db.with_snapshot(|snap| {
                for s in snap.cosmic_signatures.get(&sys_id).into_iter().flatten() {
                    if s.collapsed || !matches!(s.sig_group.as_str(), "Ore" | "Gas") {
                        continue;
                    }
                    if let Some((did, iid)) = parse_dng_site_id(&s.signature_id) {
                        out.entry(did).or_default().push(iid);
                    }
                }
            });
            Some(Ok(instances_reply(sys_id, out)))
        }
        _ => None,
    }
}
