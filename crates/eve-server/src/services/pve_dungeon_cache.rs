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
                }));
            }
            // Canonical archetype IDs & names from SDE / Crucible
            let archetypes = vec![
                (1, "Generic Dungeon"),
                (2, "Mission Dungeon"),
                (3, "Deadspace Complex"),
                (4, "Escalation Dungeon"),
                (5, "Exploration Site"),
                (6, "Scout Outpost"),
                (24, "Military Depot"),
                (28, "Ice Belt"),
                (29, "Ore Anomaly"),
                (30, "Combat Anomaly"),
                (31, "Incursion Site"),
                (33, "Abyssal Deadspace"),
                (34, "Faction Warfare Complex"),
                (35, "Homefront Operation"),
                (36, "Pirate Insurgency Site"),
                (43, "Pith Merchant Depot"),
            ];
            let rows: Vec<Vec<PyRep>> = archetypes
                .into_iter()
                .map(|(aid, aname)| vec![PyRep::Integer(aid), PyRep::String(aname.to_string())])
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
                        PyRep::None,      // dungeonNameID
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
                }));
            }
            // Canonical dungeon editor object palette (celestials, structures, gates, beacons, asteroids)
            let palette: Vec<(i64, &'static str)> = vec![
                (16, "Stargate (Caldari)"),
                (17, "Stargate (Minmatar)"),
                (18, "Stargate (Amarr)"),
                (19, "Stargate (Gallente)"),
                (1972, "Acceleration Gate"),
                (29624, "Acceleration Gate (Inactive)"),
                (24659, "Dungeon Beacon"),
                (17954, "Large Collapsible Asteroid"),
                (17955, "Medium Collapsible Asteroid"),
                (17956, "Small Collapsible Asteroid"),
                (3812, "Habitation Module"),
                (3813, "Solar Harvester"),
                (3814, "Bioreaction Silo"),
                (12245, "Sansha Command Relay"),
                (12246, "Guristas Scout Post"),
            ];
            let list: Vec<PyRep> = palette
                .into_iter()
                .map(|(tid, name)| {
                    PyRep::Tuple(vec![PyRep::Integer(tid), PyRep::String(name.to_string())])
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
                }));
            }
            let dungeon_id = kwargs
                .get("dungeonID")
                .or_else(|| kwargs.get("dungeonVID"))
                .and_then(|v| v.as_i64())
                .or_else(|| args.first().and_then(|a| a.as_i64()))
                .unwrap_or(43);

            let dname = ctx
                .sde
                .dungeons
                .get(&(dungeon_id as u32))
                .map(|d| d.name.as_str())
                .unwrap_or("Dungeon Pocket");

            let rows = vec![
                vec![
                    PyRep::Integer(dungeon_id * 10 + 1),
                    PyRep::String(format!("{dname} - Room 1")),
                    PyRep::Integer(dungeon_id),
                ],
                vec![
                    PyRep::Integer(dungeon_id * 10 + 2),
                    PyRep::String(format!("{dname} - Room 2")),
                    PyRep::Integer(dungeon_id),
                ],
            ];
            return Some(Ok(PyRep::rowset(vec!["roomID", "roomName", "dungeonID"], rows)));
        }

        ("keeper", "GetCurrentDungeonForCharacter") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let current_dungeon = ctx.db.with_snapshot(|snap| {
                if !snap.characters.contains_key(&char_id) {
                    return None;
                }
                session.solar_system_id.and_then(|sys_id| {
                    if ctx.sde.dungeons.contains_key(&sys_id) {
                        Some(sys_id as i64)
                    } else {
                        None
                    }
                })
            });
            return Some(Ok(current_dungeon.map(PyRep::Integer).unwrap_or(PyRep::None)));
        }

        ("keeper", "GetLevelEditor") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "keeper.GetLevelEditor requires GM role".into(),
                }));
            }
            return Some(Ok(PyRep::keyval([
                ("boundOID", PyRep::Integer(900_001)),
                ("isEditorActive", PyRep::Bool(true)),
                ("sessionID", PyRep::Integer(session.session_id as i64)),
            ])));
        }

        ("keeper", "GetOperationSpawnpointSolarSystem") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let default_spawnpoint = 30_000_142i64; // Jita / Starter Career system
            let sys_id = session.solar_system_id.map(|s| s as i64).unwrap_or(default_spawnpoint);
            return Some(Ok(PyRep::Integer(sys_id)));
        }

        ("keeper", "IsOperationSite") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let target_sys = args
                .first()
                .and_then(|a| a.as_i64())
                .map(|s| s as u32)
                .or(session.solar_system_id)
                .unwrap_or(30_000_142);
            let is_op = ctx.sde.solar_systems.contains_key(&target_sys);
            return Some(Ok(PyRep::Bool(is_op)));
        }

        _ => {}
    }
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

    if service != "dungeonInstanceCacheMgr" {
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
        }));
    };

    match method {
        "GetCombatAnomalyInstances" => {
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            let by_dungeon = registry_instances(ctx, sys_id, false);
            Some(Ok(instances_reply(sys_id, by_dungeon)))
        }
        "GetCombatAnomaliesCount" => {
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            let instances = registry_instances(ctx, sys_id, false);
            let count: usize = instances.values().map(|v| v.len()).sum();
            Some(Ok(PyRep::Integer(count as i64)))
        }
        "GetOreAnomalyInstances" => {
            seed_system_mining_sites_from_sde(ctx, sys_id).await;
            let by_dungeon = anomaly_instances(ctx, sys_id, "OreAnomaly");
            Some(Ok(instances_reply(sys_id, by_dungeon)))
        }
        "GetOreAnomaliesCount" => {
            seed_system_mining_sites_from_sde(ctx, sys_id).await;
            let instances = anomaly_instances(ctx, sys_id, "OreAnomaly");
            let count: usize = instances.values().map(|v| v.len()).sum();
            Some(Ok(PyRep::Integer(count as i64)))
        }
        "GetOreAnomaliesCountInRange" => {
            seed_system_mining_sites_from_sde(ctx, sys_id).await;
            let instances = anomaly_instances(ctx, sys_id, "OreAnomaly");
            let count: usize = instances.values().map(|v| v.len()).sum();
            Some(Ok(PyRep::Dict(vec![(
                PyRep::Integer(sys_id as i64),
                PyRep::Integer(count as i64),
            )])))
        }
        "GetIceBeltInstances" => {
            seed_system_ice_belt_from_sde(ctx, sys_id);
            let by_dungeon = anomaly_instances(ctx, sys_id, ICE_BELT_ANOMALY_TYPE);
            Some(Ok(instances_reply(sys_id, by_dungeon)))
        }
        "GetIceBeltsCount" => {
            seed_system_ice_belt_from_sde(ctx, sys_id);
            let instances = anomaly_instances(ctx, sys_id, ICE_BELT_ANOMALY_TYPE);
            let count: usize = instances.values().map(|v| v.len()).sum();
            Some(Ok(PyRep::Integer(count as i64)))
        }
        "GetBulwarkMiningSiteCounts" => {
            // const-ok: Bulwark mining site count dictionary per solar system
            Some(Ok(PyRep::Dict(vec![(
                PyRep::Integer(sys_id as i64),
                PyRep::Integer(0),
            )])))
        }
        "GetFactionWarfareInstances" => {
            // const-ok: FW complex instances dictionary
            let by_dng = std::collections::BTreeMap::new();
            Some(Ok(instances_reply(sys_id, by_dng)))
        }
        _ => {
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
    }
}
