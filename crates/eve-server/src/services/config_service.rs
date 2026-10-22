//! `config_service` — `config` (`P-4`), `objectCaching` (`P-3`), and `notificationMgr` (`P-5`) RPC handlers.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, DBRowDescriptor, PyRep};
use std::collections::{HashMap, HashSet};

fn parse_id_list(
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
    kw_name: &str,
    method: &str,
) -> Result<Vec<i64>, BridgeRpcError> {
    let container = args.first().or_else(|| kwargs.get(kw_name)).ok_or_else(|| {
        BridgeRpcError {
            code: "InvalidArgument".into(),
            message: format!("config.{method} requires an integer ID list argument ({kw_name})"),
        }
    })?;
    match container {
        PyRep::List(items) | PyRep::Tuple(items) => {
            let mut out = Vec::with_capacity(items.len());
            for (idx, item) in items.iter().enumerate() {
                let Some(val) = item.as_i64() else {
                    return Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: format!(
                            "config.{method}: element {idx} in {kw_name} is not an integer"
                        ),
                    });
                };
                out.push(val);
            }
            Ok(out)
        }
        PyRep::Integer(single) if args.len() == 1 => Ok(vec![*single]),
        _ => Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: format!("config.{method} expects a List or Tuple of integer IDs"),
        }),
    }
}

fn query_celestials_batch(
    ctx: &ServerContext,
    celestial_ids: &[i64],
) -> HashMap<i64, eve_sde::CelestialInfo> {
    let mut out = HashMap::new();
    if celestial_ids.is_empty() {
        return out;
    }
    let Some(p) = ctx.sde.db_path.as_ref() else {
        return out;
    };
    let Ok(conn) =
        rusqlite::Connection::open_with_flags(p, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
    else {
        return out;
    };
    let Ok(mut stmt) = conn.prepare(
        "SELECT celestial_id, system_id, type_id, group_id, name, radius, x, y, z, orbit_id, orbit_index, celestial_index FROM sde_celestials WHERE celestial_id = ?1",
    ) else {
        return out;
    };
    for &celestial_id in celestial_ids {
        if out.contains_key(&celestial_id) {
            continue;
        }
        if let Ok(cel) = stmt.query_row([celestial_id], |r| {
            Ok(eve_sde::CelestialInfo {
                celestial_id: r.get(0)?,
                solar_system_id: r.get::<_, i64>(1)? as eve_sde::SolarSystemId,
                type_id: r.get::<_, i64>(2)? as eve_sde::TypeId,
                group_id: r.get::<_, i64>(3)? as eve_sde::GroupId,
                name: r.get(4)?,
                radius: r.get(5)?,
                position: glam::DVec3::new(r.get(6)?, r.get(7)?, r.get(8)?),
                orbit_id: r.get(9)?,
                orbit_index: r.get(10)?,
                celestial_index: r.get(11)?,
            })
        }) {
            out.insert(celestial_id, cel);
        }
    }
    out
}

pub fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        // ====================================================================
        // P-3: objectCaching service (`ObjCacheService.cpp:247-315`)
        // ====================================================================
        ("objectCaching", "GetCachableObject") | ("objectCaching", "GetCachedObject") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let (shared, object_id) = match (args.first(), args.get(1)) {
                (Some(PyRep::Bool(b)), Some(oid)) => (*b, oid),
                (Some(PyRep::Integer(i)), Some(oid)) => (*i != 0, oid),
                (Some(oid), None) => (true, oid),
                _ => {
                    return Some(Err(BridgeRpcError {
                        code: "InvalidArgument".into(),
                        message: "objectCaching.GetCachableObject requires (shared, objectID, (timestamp, version), nodeID) or (objectID,)".into(),
                    }));
                }
            };
            let Some(rec) = ctx.get_or_populate_cached_object(object_id, shared) else {
                return Some(Err(BridgeRpcError {
                    code: "CachedObjectNotFound".into(),
                    message: format!("Cached object {:?} is not registered", object_id),
                }));
            };
            Some(Ok(rec.to_cached_object_pyrep()))
        }

        ("objectCaching", "InvalidateCachedObjects") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "objectCaching.InvalidateCachedObjects requires GM/Admin role".into(),
                }));
            }
            let mut invalidated = 0i64;
            match args.first() {
                Some(PyRep::List(items) | PyRep::Tuple(items)) => {
                    for oid in items {
                        if ctx.invalidate_cached_object(oid) {
                            invalidated += 1;
                        }
                    }
                }
                Some(oid) => {
                    if ctx.invalidate_cached_object(oid) {
                        invalidated += 1;
                    }
                }
                None => {}
            }
            Some(Ok(PyRep::Integer(invalidated)))
        }

        // ====================================================================
        // P-4: config service (`ConfigService.cpp` / `ConfigDB.cpp`)
        // ====================================================================
        ("config", "GetMultiOwnersEx") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let owner_ids = match parse_id_list(args, kwargs, "ownerIDs", "GetMultiOwnersEx") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut seen = HashSet::new();
            let mut rows = Vec::new();
            ctx.db.with_snapshot(|snap| {
                for id in owner_ids {
                    if !seen.insert(id) {
                        continue;
                    }
                    if let Some(ch) = snap.characters.get(&id) {
                        rows.push(vec![
                            PyRep::Integer(ch.character_id),
                            PyRep::String(ch.name.clone()),
                            PyRep::Integer(1373),
                            PyRep::None,
                            PyRep::None,
                        ]);
                    } else if let Some(corp) = snap.corporations.get(&id) {
                        rows.push(vec![
                            PyRep::Integer(corp.corporation_id),
                            PyRep::String(corp.name.clone()),
                            PyRep::Integer(2),
                            PyRep::Bool(false),
                            PyRep::None,
                        ]);
                    } else if let Some(ally) = snap.alliances.get(&id) {
                        rows.push(vec![
                            PyRep::Integer(ally.alliance_id),
                            PyRep::String(ally.name.clone()),
                            PyRep::Integer(16159),
                            PyRep::Bool(false),
                            PyRep::None,
                        ]);
                    } else if let Some(npc_corp) = ctx.sde.npc_corporations.get(&id) {
                        rows.push(vec![
                            PyRep::Integer(npc_corp.corporation_id),
                            PyRep::String(npc_corp.name.clone()),
                            PyRep::Integer(2),
                            PyRep::Bool(false),
                            PyRep::None,
                        ]);
                    } else if let Some(fac) =
                        u32::try_from(id).ok().and_then(|fid| ctx.sde.factions.get(&fid))
                    {
                        rows.push(vec![
                            PyRep::Integer(id),
                            PyRep::String(fac.name.clone()),
                            PyRep::Integer(30),
                            PyRep::Bool(false),
                            PyRep::None,
                        ]);
                    } else if let Some(ag) = ctx.sde.agents.get(&id) {
                        rows.push(vec![
                            PyRep::Integer(ag.agent_id),
                            PyRep::String(ag.name.clone()),
                            PyRep::Integer(1373),
                            PyRep::None,
                            PyRep::None,
                        ]);
                    } else if id == 1 {
                        rows.push(vec![
                            PyRep::Integer(1),
                            PyRep::String("EVE System".into()),
                            PyRep::Integer(1),
                            PyRep::Bool(false),
                            PyRep::None,
                        ]);
                    }
                }
            });
            Some(Ok(PyRep::tuple_set(
                vec!["ownerID", "ownerName", "typeID", "gender", "ownerNameID"],
                rows,
            )))
        }

        ("config", "GetMultiLocationsEx") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let loc_ids = match parse_id_list(args, kwargs, "locationIDs", "GetMultiLocationsEx") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let celestials_map = query_celestials_batch(ctx, &loc_ids);
            let mut seen = HashSet::new();
            let mut rows = Vec::new();
            ctx.db.with_snapshot(|snap| {
                for id in loc_ids {
                    if !seen.insert(id) {
                        continue;
                    }
                    if let Some(reg) =
                        u32::try_from(id).ok().and_then(|rid| ctx.sde.regions.get(&rid))
                    {
                        rows.push(vec![
                            PyRep::Integer(id),
                            PyRep::String(reg.name.clone()),
                            PyRep::Real(reg.center[0]),
                            PyRep::Real(reg.center[1]),
                            PyRep::Real(reg.center[2]),
                            PyRep::None,
                        ]);
                    } else if let Some(con) = u32::try_from(id)
                        .ok()
                        .and_then(|cid| ctx.sde.constellations.get(&cid))
                    {
                        rows.push(vec![
                            PyRep::Integer(id),
                            PyRep::String(con.name.clone()),
                            PyRep::Real(con.center[0]),
                            PyRep::Real(con.center[1]),
                            PyRep::Real(con.center[2]),
                            PyRep::None,
                        ]);
                    } else if let Some(sys) = u32::try_from(id)
                        .ok()
                        .and_then(|sid| ctx.sde.solar_systems.get(&sid))
                    {
                        rows.push(vec![
                            PyRep::Integer(id),
                            PyRep::String(sys.name.clone()),
                            PyRep::Real(sys.galactic_pos_ly[0]),
                            PyRep::Real(sys.galactic_pos_ly[1]),
                            PyRep::Real(sys.galactic_pos_ly[2]),
                            PyRep::None,
                        ]);
                    } else if let Some(st) = ctx
                        .sde
                        .solar_systems
                        .values()
                        .flat_map(|s| &s.stations)
                        .find(|st| st.station_id == id)
                    {
                        rows.push(vec![
                            PyRep::Integer(id),
                            PyRep::String(st.name.clone()),
                            PyRep::Real(st.position.x),
                            PyRep::Real(st.position.y),
                            PyRep::Real(st.position.z),
                            PyRep::None,
                        ]);
                    } else if let Some(jmp) = ctx.sde.jumps.get(&id) {
                        let dest_name = ctx
                            .sde
                            .solar_systems
                            .get(&jmp.to_solar_system_id)
                            .map(|s| format!("Stargate ({})", s.name))
                            .unwrap_or_else(|| format!("Stargate {id}"));
                        rows.push(vec![
                            PyRep::Integer(id),
                            PyRep::String(dest_name),
                            PyRep::Real(jmp.position.x),
                            PyRep::Real(jmp.position.y),
                            PyRep::Real(jmp.position.z),
                            PyRep::None,
                        ]);
                    } else if let Some(st) = snap.structures.get(&id) {
                        rows.push(vec![
                            PyRep::Integer(id),
                            PyRep::String(st.name.clone()),
                            PyRep::Real(st.position[0]),
                            PyRep::Real(st.position[1]),
                            PyRep::Real(st.position[2]),
                            PyRep::None,
                        ]);
                    } else if let Some(cel) = celestials_map.get(&id) {
                        rows.push(vec![
                            PyRep::Integer(id),
                            PyRep::String(cel.name.clone()),
                            PyRep::Real(cel.position.x),
                            PyRep::Real(cel.position.y),
                            PyRep::Real(cel.position.z),
                            PyRep::None,
                        ]);
                    } else if let Some(item) = snap.items.get(&id) {
                        let visible = session.character_id == Some(item.owner_id)
                            || session.corporation_id == Some(item.owner_id)
                            || session
                                .solar_system_id
                                .is_some_and(|sys_id| item.location_id == i64::from(sys_id));
                        if visible {
                            let name = ctx
                                .sde
                                .types
                                .get(&item.type_id)
                                .map(|t| t.name.clone())
                                .unwrap_or_else(|| format!("Item {id}"));
                            let (ix, iy, iz) = if let Some(st) = ctx
                                .sde
                                .solar_systems
                                .values()
                                .flat_map(|s| &s.stations)
                                .find(|st| st.station_id == item.location_id)
                            {
                                (st.position.x, st.position.y, st.position.z)
                            } else if let Some(st) = snap.structures.get(&item.location_id) {
                                (st.position[0], st.position[1], st.position[2])
                            } else if let Some(sys) = u32::try_from(item.location_id)
                                .ok()
                                .and_then(|sid| ctx.sde.solar_systems.get(&sid))
                            {
                                (
                                    sys.galactic_pos_ly[0],
                                    sys.galactic_pos_ly[1],
                                    sys.galactic_pos_ly[2],
                                )
                            } else {
                                (0.0, 0.0, 0.0)
                            };
                            rows.push(vec![
                                PyRep::Integer(id),
                                PyRep::String(name),
                                PyRep::Real(ix),
                                PyRep::Real(iy),
                                PyRep::Real(iz),
                                PyRep::None,
                            ]);
                        }
                    }
                }
            });
            Some(Ok(PyRep::tuple_set(
                vec![
                    "locationID",
                    "locationName",
                    "x",
                    "y",
                    "z",
                    "locationNameID",
                ],
                rows,
            )))
        }

        ("config", "GetMultiAllianceShortNamesEx") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let alliance_ids = match parse_id_list(
                args,
                kwargs,
                "allianceIDs",
                "GetMultiAllianceShortNamesEx",
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut seen = HashSet::new();
            let mut rows = Vec::new();
            ctx.db.with_snapshot(|snap| {
                for id in alliance_ids {
                    if !seen.insert(id) {
                        continue;
                    }
                    if let Some(ally) = snap.alliances.get(&id) {
                        rows.push(vec![
                            PyRep::Integer(ally.alliance_id),
                            PyRep::String(ally.short_name.clone()),
                        ]);
                    }
                }
            });
            Some(Ok(PyRep::tuple_set(vec!["allianceID", "shortName"], rows)))
        }

        ("config", "GetMultiCorpTickerNamesEx") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let corp_ids = match parse_id_list(
                args,
                kwargs,
                "corporationIDs",
                "GetMultiCorpTickerNamesEx",
            ) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut seen = HashSet::new();
            let mut rows = Vec::new();
            ctx.db.with_snapshot(|snap| {
                for id in corp_ids {
                    if !seen.insert(id) {
                        continue;
                    }
                    let ticker_opt = snap
                        .corporations
                        .get(&id)
                        .map(|c| c.ticker.clone())
                        .or_else(|| {
                            ctx.sde
                                .npc_corporations
                                .get(&id)
                                .map(|nc| nc.ticker_name.clone())
                        });
                    if let Some(ticker) = ticker_opt {
                        rows.push(vec![
                            PyRep::Integer(id),
                            PyRep::String(ticker),
                            PyRep::None,
                            PyRep::None,
                            PyRep::None,
                            PyRep::None,
                            PyRep::None,
                            PyRep::None,
                        ]);
                    }
                }
            });
            Some(Ok(
                DBRowDescriptor::corp_ticker_header().build_packed_row_list(rows),
            ))
        }

        ("config", "GetStationSolarSystemsByOwner") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let Some(owner_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| kwargs.get("ownerID").and_then(|v| v.as_i64()))
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "config.GetStationSolarSystemsByOwner requires ownerID".into(),
                }));
            };
            let mut rows: Vec<(i64, u32)> = Vec::new();
            for sys in ctx.sde.solar_systems.values() {
                for st in &sys.stations {
                    if st.owner_corporation_id == owner_id {
                        rows.push((st.station_id, sys.solar_system_id));
                    }
                }
            }
            ctx.db.with_snapshot(|snap| {
                for st in snap.structures.values() {
                    if st.owner_corp_id == owner_id {
                        rows.push((st.structure_id, st.solar_system_id));
                    }
                }
            });
            rows.sort_by_key(|(stid, _)| *stid);
            rows.dedup_by_key(|(stid, _)| *stid);
            let py_rows = rows
                .into_iter()
                .map(|(stid, sys_id)| {
                    vec![PyRep::Integer(stid), PyRep::Integer(sys_id as i64)]
                })
                .collect();
            Some(Ok(PyRep::rowset(
                vec!["stationID", "solarSystemID"],
                py_rows,
            )))
        }

        ("config", "GetMap") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let Some(sys_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .or_else(|| session.solar_system_id.map(|s| s as i64))
                .and_then(|v| u32::try_from(v).ok())
            else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "config.GetMap requires solarSystemID".into(),
                }));
            };
            let Some(sys) = ctx.sde.solar_systems.get(&sys_id) else {
                return Some(Err(BridgeRpcError {
                    code: "SolarSystemNotFound".into(),
                    message: format!("Solar system {sys_id} not found"),
                }));
            };
            let cels = ctx.sde.celestials_in_system(sys_id);
            let mut rows = Vec::new();
            for c in cels {
                let dest = ctx
                    .sde
                    .jumps
                    .get(&c.celestial_id)
                    .map(|j| PyRep::Integer(j.destination_stargate_id))
                    .unwrap_or(PyRep::None);
                rows.push(vec![
                    PyRep::Integer(sys_id as i64),
                    PyRep::Real(-sys.radius),
                    PyRep::Real(sys.radius),
                    PyRep::Real(-sys.radius),
                    PyRep::Real(sys.radius),
                    PyRep::Real(-sys.radius),
                    PyRep::Real(sys.radius),
                    PyRep::Real(0.5),
                    PyRep::Real(c.position.x),
                    PyRep::Real(c.position.y),
                    PyRep::Real(c.position.z),
                    PyRep::Integer(c.celestial_id),
                    PyRep::String(c.name),
                    PyRep::Integer(c.type_id as i64),
                    c.orbit_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                    dest,
                ]);
            }
            for st in &sys.stations {
                rows.push(vec![
                    PyRep::Integer(sys_id as i64),
                    PyRep::Real(-sys.radius),
                    PyRep::Real(sys.radius),
                    PyRep::Real(-sys.radius),
                    PyRep::Real(sys.radius),
                    PyRep::Real(-sys.radius),
                    PyRep::Real(sys.radius),
                    PyRep::Real(0.5),
                    PyRep::Real(st.position.x),
                    PyRep::Real(st.position.y),
                    PyRep::Real(st.position.z),
                    PyRep::Integer(st.station_id),
                    PyRep::String(st.name.clone()),
                    PyRep::Integer(st.station_type_id as i64),
                    PyRep::None,
                    PyRep::None,
                ]);
            }
            Some(Ok(PyRep::rowset(
                vec![
                    "locationID",
                    "xMin",
                    "xMax",
                    "yMin",
                    "yMax",
                    "zMin",
                    "zMax",
                    "luminosity",
                    "x",
                    "y",
                    "z",
                    "itemID",
                    "itemName",
                    "typeID",
                    "orbitID",
                    "destinations",
                ],
                rows,
            )))
        }

        // ====================================================================
        // P-5: notificationMgr service (`NotificationMgrService.cpp:33-132`)
        // ====================================================================
        ("notificationMgr", "GetAllNotifications") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx
                .notification_records
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|n| n.receiver_id == char_id && !n.deleted)
                .map(|n| {
                    vec![
                        PyRep::Integer(n.notification_id),
                        PyRep::Integer(n.type_id as i64),
                        PyRep::Integer(n.sender_id),
                        PyRep::Integer(n.receiver_id),
                        PyRep::Bool(n.processed),
                        PyRep::Integer(n.created),
                        PyRep::String(n.data.clone()),
                        PyRep::Bool(n.deleted),
                    ]
                })
                .collect();
            Some(Ok(DBRowDescriptor::notification_header().build_crowset(rows)))
        }

        ("notificationMgr", "GetByGroupID") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(group_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "notificationMgr.GetByGroupID requires groupID".into(),
                }));
            };
            let rows = ctx
                .notification_records
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|n| {
                    n.receiver_id == char_id && !n.deleted && n.group_id as i64 == group_id
                })
                .map(|n| {
                    vec![
                        PyRep::Integer(n.notification_id),
                        PyRep::Integer(n.type_id as i64),
                        PyRep::Integer(n.sender_id),
                        PyRep::Integer(n.receiver_id),
                        PyRep::Bool(n.processed),
                        PyRep::Integer(n.created),
                        PyRep::String(n.data.clone()),
                        PyRep::Bool(n.deleted),
                    ]
                })
                .collect();
            Some(Ok(DBRowDescriptor::notification_header().build_crowset(rows)))
        }

        ("notificationMgr", "GetUnprocessed") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let rows = ctx
                .notification_records
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|n| n.receiver_id == char_id && !n.deleted && !n.processed)
                .map(|n| {
                    vec![
                        PyRep::Integer(n.notification_id),
                        PyRep::Integer(n.type_id as i64),
                        PyRep::Integer(n.sender_id),
                        PyRep::Integer(n.receiver_id),
                        PyRep::Bool(n.processed),
                        PyRep::Integer(n.created),
                        PyRep::String(n.data.clone()),
                        PyRep::Bool(n.deleted),
                    ]
                })
                .collect();
            Some(Ok(DBRowDescriptor::notification_header().build_crowset(rows)))
        }

        ("notificationMgr", "MarkAsProcessed") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ids = match parse_id_list(args, kwargs, "notificationIDs", "MarkAsProcessed") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let id_set: HashSet<i64> = ids.into_iter().collect();
            let mut updated = 0i64;
            let mut records = ctx
                .notification_records
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for n in records.iter_mut() {
                if n.receiver_id == char_id && id_set.contains(&n.notification_id) && !n.processed {
                    n.processed = true;
                    updated += 1;
                }
            }
            if updated > 0 {
                let _ = ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnNotificationChange",
                    "charid",
                    vec![PyRep::Integer(char_id)],
                );
            }
            Some(Ok(PyRep::Integer(updated)))
        }

        ("notificationMgr", "MarkAllAsProcessed") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut updated = 0i64;
            let mut records = ctx
                .notification_records
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for n in records.iter_mut() {
                if n.receiver_id == char_id && !n.deleted && !n.processed {
                    n.processed = true;
                    updated += 1;
                }
            }
            if updated > 0 {
                let _ = ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnNotificationChange",
                    "charid",
                    vec![PyRep::Integer(char_id)],
                );
            }
            Some(Ok(PyRep::Integer(updated)))
        }

        ("notificationMgr", "MarkGroupAsProcessed") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(group_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "notificationMgr.MarkGroupAsProcessed requires groupID".into(),
                }));
            };
            let mut updated = 0i64;
            let mut records = ctx
                .notification_records
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for n in records.iter_mut() {
                if n.receiver_id == char_id
                    && !n.deleted
                    && n.group_id as i64 == group_id
                    && !n.processed
                {
                    n.processed = true;
                    updated += 1;
                }
            }
            if updated > 0 {
                let _ = ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnNotificationChange",
                    "charid",
                    vec![PyRep::Integer(char_id)],
                );
            }
            Some(Ok(PyRep::Integer(updated)))
        }

        ("notificationMgr", "DeleteNotifications") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let ids = match parse_id_list(args, kwargs, "notificationIDs", "DeleteNotifications") {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let id_set: HashSet<i64> = ids.into_iter().collect();
            let mut deleted = 0i64;
            let mut records = ctx
                .notification_records
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for n in records.iter_mut() {
                if n.receiver_id == char_id && id_set.contains(&n.notification_id) && !n.deleted {
                    n.deleted = true;
                    deleted += 1;
                }
            }
            if deleted > 0 {
                let _ = ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnNotificationDeleted",
                    "charid",
                    vec![PyRep::Integer(char_id)],
                );
            }
            Some(Ok(PyRep::Integer(deleted)))
        }

        ("notificationMgr", "DeleteGroupNotifications") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(group_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidArgument".into(),
                    message: "notificationMgr.DeleteGroupNotifications requires groupID".into(),
                }));
            };
            let mut deleted = 0i64;
            let mut records = ctx
                .notification_records
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for n in records.iter_mut() {
                if n.receiver_id == char_id && !n.deleted && n.group_id as i64 == group_id {
                    n.deleted = true;
                    deleted += 1;
                }
            }
            if deleted > 0 {
                let _ = ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnNotificationDeleted",
                    "charid",
                    vec![PyRep::Integer(char_id)],
                );
            }
            Some(Ok(PyRep::Integer(deleted)))
        }

        ("notificationMgr", "DeleteAllNotifications") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let mut deleted = 0i64;
            let mut records = ctx
                .notification_records
                .write()
                .unwrap_or_else(|e| e.into_inner());
            for n in records.iter_mut() {
                if n.receiver_id == char_id && !n.deleted {
                    n.deleted = true;
                    deleted += 1;
                }
            }
            if deleted > 0 {
                let _ = ctx.notify(
                    crate::router::NotificationTarget::Character(char_id),
                    "OnNotificationDeleted",
                    "charid",
                    vec![PyRep::Integer(char_id)],
                );
            }
            Some(Ok(PyRep::Integer(deleted)))
        }

        ("notificationMgr", "LogNotificationInteraction") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let notif_id = args.first().and_then(|a| a.as_i64());
            if let Some(nid) = notif_id {
                let mut recs = ctx
                    .notification_records
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(r) = recs.iter_mut().find(|r| r.notification_id == nid && r.receiver_id == char_id) {
                    r.interacted = true;
                }
            }
            Some(Ok(PyRep::Bool(true)))
        }

        ("notificationMgr", "iterkeys") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let keys = ctx
                .notification_records
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|n| n.receiver_id == char_id && !n.deleted)
                .map(|n| PyRep::Integer(n.notification_id))
                .collect();
            Some(Ok(PyRep::List(keys)))
        }

        ("loadService", "GetTotalStats") => {
            if let Err(e) = ServerContext::require_auth(session) {
                return Some(Err(e));
            }
            let active_sessions = ctx.db.with_snapshot(|snap| {
                snap.characters
                    .values()
                    .filter(|ch| ctx.is_character_online(ch.character_id, ch.user_id))
                    .count() as i64
            });
            Some(Ok(PyRep::keyval([
                ("cpuPercentage", PyRep::Real(1.5)),
                ("memoryUsageMB", PyRep::Integer(256)),
                ("activeSessions", PyRep::Integer(active_sessions)),
                ("activeBallparks", PyRep::Integer(1)),
                ("nodeID", PyRep::Integer(1001)),
                ("isOverloaded", PyRep::Bool(false)),
            ])))
        }

        ("loadService", "SetConfig") => {
            if !session.has_gm_role() {
                return Some(Err(BridgeRpcError {
                    code: "InsufficientRole".into(),
                    message: "loadService.SetConfig requires GM role".into(),
                }));
            }
            Some(Ok(PyRep::Bool(true)))
        }

        _ => None,
    }
}

pub fn get_multi_station_ex(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;
    let station_ids = parse_id_list(args, kwargs, "stationIDs", "GetMultiStationEx")?;
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for id in station_ids {
        if !seen.insert(id) {
            continue;
        }
        if let Some((sys_id, st)) = ctx
            .sde
            .solar_systems
            .values()
            .flat_map(|s| s.stations.iter().map(move |st| (s.solar_system_id, st)))
            .find(|(_, st)| st.station_id == id)
        {
            rows.push(vec![
                PyRep::Integer(st.station_id),
                PyRep::String(st.name.clone()),
                PyRep::Integer(st.station_type_id as i64),
                PyRep::Integer(sys_id as i64),
                PyRep::Real(st.position.x),
                PyRep::Real(st.position.y),
                PyRep::Real(st.position.z),
            ]);
        }
    }
    Ok(PyRep::tuple_set(
        vec![
            "stationID",
            "stationName",
            "stationTypeID",
            "solarSystemID",
            "x",
            "y",
            "z",
        ],
        rows,
    ))
}

pub fn get_multi_inv_types_ex(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;
    let type_ids = parse_id_list(args, kwargs, "typeIDs", "GetMultiInvTypesEx")?;
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for id in type_ids {
        if !seen.insert(id) {
            continue;
        }
        if let Some(t) = u32::try_from(id).ok().and_then(|tid| ctx.sde.types.get(&tid)) {
            rows.push(vec![
                PyRep::Integer(t.type_id as i64),
                PyRep::Integer(t.group_id as i64),
                PyRep::String(t.name.clone()),
                PyRep::String(t.name.clone()),
                PyRep::Integer(0),
                PyRep::Real(t.radius),
                PyRep::Real(t.mass),
                PyRep::Real(t.volume),
                PyRep::Real(t.capacity),
                PyRep::Integer(t.portion_size as i64),
                t.race_id
                    .map(|r| PyRep::Integer(r as i64))
                    .unwrap_or(PyRep::None),
                PyRep::Real(t.base_price),
                PyRep::Bool(t.published),
                t.market_group_id
                    .map(|mg| PyRep::Integer(mg as i64))
                    .unwrap_or(PyRep::None),
                PyRep::Real(0.0),
            ]);
        }
    }
    Ok(DBRowDescriptor::inv_type_header().build_packed_row_list(rows))
}

pub fn get_multi_graphics_ex(
    _ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;
    Err(BridgeRpcError {
        code: "UnimplementedRPC".into(),
        message: "GetMultiGraphicsEx requires sde_graphics table import".into(),
    })
}

pub fn get_celestial_statistic(
    _ctx: &ServerContext,
    session: &SessionState,
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;
    Err(BridgeRpcError {
        code: "UnimplementedRPC".into(),
        message: "GetCelestialStatistic requires sde_celestial_statistics table import".into(),
    })
}

pub fn get_map_objects(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;
    let Some(entity_id) = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| session.solar_system_id.map(|s| s as i64))
    else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "GetMapObjects requires entityID".into(),
        });
    };
    let explicit_flags = args.len() > 1;
    let want_regions = if explicit_flags {
        args.get(1).and_then(|v| v.as_i64()).unwrap_or(0) != 0
    } else {
        entity_id == 9
    };
    let want_constellations = if explicit_flags {
        args.get(2).and_then(|v| v.as_i64()).unwrap_or(0) != 0
    } else {
        u32::try_from(entity_id)
            .ok()
            .is_some_and(|rid| ctx.sde.regions.contains_key(&rid))
    };
    let want_systems = if explicit_flags {
        args.get(3).and_then(|v| v.as_i64()).unwrap_or(0) != 0
    } else {
        u32::try_from(entity_id)
            .ok()
            .is_some_and(|cid| ctx.sde.constellations.contains_key(&cid))
    };
    let want_stations = if explicit_flags {
        args.get(4).and_then(|v| v.as_i64()).unwrap_or(0) != 0
    } else {
        u32::try_from(entity_id)
            .ok()
            .is_some_and(|sid| ctx.sde.solar_systems.contains_key(&sid))
    };

    let mut rows = Vec::new();
    if want_regions {
        let mut regs: Vec<&eve_sde::RegionInfo> = ctx.sde.regions.values().collect();
        regs.sort_by_key(|r| r.region_id);
        for r in regs {
            if entity_id == 9 || i64::from(r.region_id) == entity_id {
                rows.push(vec![
                    PyRep::Integer(3),
                    PyRep::Integer(3),
                    PyRep::Integer(r.region_id as i64),
                    PyRep::String(r.name.clone()),
                    PyRep::Integer(9),
                    PyRep::Integer(0),
                    PyRep::Real(r.center[0]),
                    PyRep::Real(r.center[1]),
                    PyRep::Real(r.center[2]),
                ]);
            }
        }
    }
    if want_constellations {
        let mut cons: Vec<&eve_sde::ConstellationInfo> = ctx.sde.constellations.values().collect();
        cons.sort_by_key(|c| c.constellation_id);
        for c in cons {
            if entity_id == 9
                || i64::from(c.region_id) == entity_id
                || i64::from(c.constellation_id) == entity_id
            {
                rows.push(vec![
                    PyRep::Integer(4),
                    PyRep::Integer(4),
                    PyRep::Integer(c.constellation_id as i64),
                    PyRep::String(c.name.clone()),
                    PyRep::Integer(c.region_id as i64),
                    PyRep::Integer(0),
                    PyRep::Real(c.center[0]),
                    PyRep::Real(c.center[1]),
                    PyRep::Real(c.center[2]),
                ]);
            }
        }
    }
    if want_systems {
        let mut syss: Vec<&eve_sde::SolarSystem> = ctx.sde.solar_systems.values().collect();
        syss.sort_by_key(|s| s.solar_system_id);
        for s in syss {
            if entity_id == 9
                || i64::from(s.region_id) == entity_id
                || i64::from(s.constellation_id) == entity_id
                || i64::from(s.solar_system_id) == entity_id
            {
                rows.push(vec![
                    PyRep::Integer(5),
                    PyRep::Integer(5),
                    PyRep::Integer(s.solar_system_id as i64),
                    PyRep::String(s.name.clone()),
                    PyRep::Integer(s.constellation_id as i64),
                    PyRep::Integer(0),
                    PyRep::Real(s.galactic_pos_ly[0]),
                    PyRep::Real(s.galactic_pos_ly[1]),
                    PyRep::Real(s.galactic_pos_ly[2]),
                ]);
            }
        }
    }
    if let Some(sys_id) = u32::try_from(entity_id).ok() {
        if let Some(sys) = ctx.sde.solar_systems.get(&sys_id) {
            if want_stations {
                for st in &sys.stations {
                    rows.push(vec![
                        PyRep::Integer(15),
                        PyRep::Integer(st.station_type_id as i64),
                        PyRep::Integer(st.station_id),
                        PyRep::String(st.name.clone()),
                        PyRep::Integer(sys_id as i64),
                        PyRep::Integer(0),
                        PyRep::Real(st.position.x),
                        PyRep::Real(st.position.y),
                        PyRep::Real(st.position.z),
                    ]);
                }
            }
            if !explicit_flags || want_stations {
                for c in ctx.sde.celestials_in_system(sys_id) {
                    rows.push(vec![
                        PyRep::Integer(c.group_id as i64),
                        PyRep::Integer(c.type_id as i64),
                        PyRep::Integer(c.celestial_id),
                        PyRep::String(c.name),
                        PyRep::Integer(sys_id as i64),
                        PyRep::Integer(c.orbit_id.unwrap_or(0)),
                        PyRep::Real(c.position.x),
                        PyRep::Real(c.position.y),
                        PyRep::Real(c.position.z),
                    ]);
                }
            }
        }
    }
    Ok(DBRowDescriptor::map_objects_header().build_crowset(rows))
}

pub fn get_map_connections(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_auth(session)?;
    let Some(item_id) = args
        .first()
        .and_then(|a| a.as_i64())
        .or_else(|| session.solar_system_id.map(|s| s as i64))
    else {
        return Err(BridgeRpcError {
            code: "InvalidArgument".into(),
            message: "GetMapConnections requires itemID".into(),
        });
    };
    let explicit_flags = args.len() > 1;
    let want_reg = if explicit_flags {
        args.get(1).and_then(|v| v.as_i64()).unwrap_or(0) != 0
    } else {
        true
    };
    let want_con = if explicit_flags {
        args.get(2).and_then(|v| v.as_i64()).unwrap_or(0) != 0
    } else {
        true
    };
    let want_sol = if explicit_flags {
        args.get(3).and_then(|v| v.as_i64()).unwrap_or(0) != 0
    } else {
        true
    };

    let item_u32 = u32::try_from(item_id).ok();
    let filter_sys = item_u32.filter(|s| ctx.sde.solar_systems.contains_key(s));
    let filter_con = item_u32.filter(|c| ctx.sde.constellations.contains_key(c));
    let filter_reg = item_u32.filter(|r| ctx.sde.regions.contains_key(r));

    let mut gate_ids: Vec<i64> = ctx.sde.jumps.keys().copied().collect();
    gate_ids.sort_unstable();
    let mut rows = Vec::new();
    for gid in gate_ids {
        let jmp = &ctx.sde.jumps[&gid];
        let (from_reg, from_con) = ctx
            .sde
            .solar_systems
            .get(&jmp.from_solar_system_id)
            .map(|s| (s.region_id, s.constellation_id))
            .unwrap_or((0, 0));
        let (to_reg, to_con) = ctx
            .sde
            .solar_systems
            .get(&jmp.to_solar_system_id)
            .map(|s| (s.region_id, s.constellation_id))
            .unwrap_or((0, 0));

        if let Some(fs) = filter_sys {
            if jmp.from_solar_system_id != fs && jmp.to_solar_system_id != fs {
                continue;
            }
        } else if let Some(fc) = filter_con {
            if from_con != fc && to_con != fc {
                continue;
            }
        } else if let Some(fr) = filter_reg {
            if from_reg != fr && to_reg != fr {
                continue;
            }
        } else if item_id != 9 {
            continue;
        }

        let is_inter_reg = from_reg != to_reg;
        let is_inter_con = from_con != to_con;
        let level_match = if is_inter_reg {
            want_reg || want_sol
        } else if is_inter_con {
            want_con || want_sol
        } else {
            want_sol
        };
        if !level_match {
            continue;
        }

        rows.push(vec![
            PyRep::String("stargate".into()),
            PyRep::Integer(from_reg as i64),
            PyRep::Integer(from_con as i64),
            PyRep::Integer(jmp.from_solar_system_id as i64),
            PyRep::Integer(jmp.stargate_id),
            PyRep::Integer(jmp.destination_stargate_id),
            PyRep::Integer(jmp.to_solar_system_id as i64),
            PyRep::Integer(to_con as i64),
            PyRep::Integer(to_reg as i64),
        ]);
    }
    Ok(PyRep::rowset(
        vec![
            "ctype",
            "fromreg",
            "fromcon",
            "fromsol",
            "stargateID",
            "celestialID",
            "tosol",
            "tocon",
            "toreg",
        ],
        rows,
    ))
}
