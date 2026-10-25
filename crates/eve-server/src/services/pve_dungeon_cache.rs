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
    let now = ctx.now_unix();
    ctx.db.with_snapshot(|snap| {
        for a in snap.cosmic_anomalies.get(&sys_id).into_iter().flatten() {
            if a.anomaly_type != kind
                || a.cleared
                || (a.lifetime_seconds > 0 && now >= a.created_unix.saturating_add(a.lifetime_seconds))
            {
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
            // Populate object palette dynamically from SDE celestial/dungeon object groups:
            // 10 = Stargate, 310 = Beacon, 366 = Warp Gate (Acceleration Gate)
            let mut palette_types: Vec<(u32, &str)> = ctx
                .sde
                .types
                .values()
                .filter(|t| matches!(t.group_id, 10 | 310 | 366))
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

            let sde_room_ids: Vec<u32> = ctx
                .sde
                .dungeon_asteroids
                .get(&d.dungeon_id)
                .map(|entries| {
                    let set: std::collections::BTreeSet<u32> =
                        entries.iter().map(|e| e.room_id).collect();
                    set.into_iter().collect()
                })
                .unwrap_or_default();

            let rows: Vec<Vec<PyRep>> = sde_room_ids
                .into_iter()
                .enumerate()
                .map(|(idx, room_id)| {
                    vec![
                        PyRep::Integer(room_id as i64),
                        PyRep::String(format!("{} - Room {}", d.name, idx + 1)),
                        PyRep::Integer(dungeon_id),
                    ]
                })
                .collect();
            return Some(Ok(PyRep::rowset(vec!["roomID", "roomName", "dungeonID"], rows)));
        }

        ("keeper", "GetCurrentDungeonForCharacter") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let abyssal_dungeon =
                crate::services::pve_special_sites::with_pve_special_reg(ctx, |reg| {
                    let p = reg.abyssal_pockets.get(&char_id)?;
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
                seed_system_mining_sites_from_sde(ctx, s).await;
                let instances = anomaly_instances(ctx, s, "OreAnomaly");
                let count: usize = instances.values().map(|v| v.len()).sum();
                entries.push((PyRep::Integer(s as i64), PyRep::Integer(count as i64)));
            }
            Some(Ok(PyRep::Dict(entries)))
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
            let by_dng: BTreeMap<u32, Vec<i64>> = ctx.db.with_snapshot(|snap| {
                let mut out: BTreeMap<u32, Vec<i64>> = BTreeMap::new();
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
            Some(crate::services::pve_homefront_sites::handle_get_homefront_site_instances(ctx, session, args))
        }
        _ => {
            // GetSignatureInstances: probe-only exploration sites plus SDE-seeded ore/gas signatures.
            ensure_system_pve_sites_seeded(ctx, sys_id).await;
            seed_system_mining_sites_from_sde(ctx, sys_id).await;
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
