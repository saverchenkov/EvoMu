//! `fw_insurgency` — Sovereignty Hub Upgrades, Cosmic Anomalies, & Probe/Directional Scanning (`§3.8`, `§3.11`).

#![forbid(unsafe_code)]

use super::{derive_system_site_position_from_sde, tick_sov_prospecting_deposit_respawn, AU_METERS};
use crate::router::ServerContext;
use crate::services::mining_sites;
use crate::system_actor::AsteroidState;
use eve_db::{AsteroidRecord, CosmicAnomalyRecord, CosmicSignatureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use tracing::warn;
use glam::DVec3;
use std::collections::HashMap;

pub async fn handle_hub_and_scan(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    match (service, method) {
        ("sovHubSvc", "SaveUpgradeConfiguration") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let sys_sec = ctx
                .sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(1.0);
            if sys_sec > 0.0 {
                return Some(Err(BridgeRpcError {
                    code: "NotNullSecSystem".into(),
                    message: format!(
                        "Sovereignty Hub prospecting upgrades can only be installed in null-sec systems (system {sys_id} has sec {sys_sec:.2})"
                    ),
                }));
            }

            // Requested upgrade type: kwargs `typeID`/`upgradeTypeID`, an `upgrades`/`configuration` list of
            // `{typeID: ...}` dicts or `(typeID, ...)` tuples, or a positional integer. Only SDE group-4768
            // Sovereignty Hub upgrades are accepted. The deposit level comes from the upgrade type itself.
            let is_sov_upgrade_type = |tid: u32| -> bool {
                ctx.sde.types.get(&tid).is_some_and(|t| t.group_id == 4768)
            };
            let type_from_rep = |v: &PyRep| -> Option<u32> {
                v.dict_get("typeID")
                    .or_else(|| v.dict_get("upgradeTypeID"))
                    .and_then(|x| x.as_i64())
                    .or_else(|| match v {
                        PyRep::Tuple(t) => t.first().and_then(|x| x.as_i64()),
                        _ => v.as_i64(),
                    })
                    .and_then(|x| u32::try_from(x).ok())
                    .filter(|&tid| is_sov_upgrade_type(tid))
            };
            let mut requested_upgrade_type: Option<u32> = kwargs
                .get("typeID")
                .or_else(|| kwargs.get("upgradeTypeID"))
                .and_then(|v| v.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .filter(|&tid| is_sov_upgrade_type(tid));
            if requested_upgrade_type.is_none() {
                if let Some(PyRep::List(items) | PyRep::Tuple(items)) =
                    kwargs.get("upgrades").or_else(|| kwargs.get("configuration"))
                {
                    requested_upgrade_type = items.iter().find_map(type_from_rep);
                }
            }
            if requested_upgrade_type.is_none() {
                requested_upgrade_type = args.iter().find_map(|a| match a {
                    PyRep::List(items) | PyRep::Tuple(items) => items.iter().find_map(type_from_rep),
                    other => type_from_rep(other),
                });
            }

            // Resolve every modelled prospecting deposit up front (client layout + wiki quantities + respawn timer).
            let mut modelled: HashMap<u32, (u8, u64, mining_sites::ResolvedOreSite)> = HashMap::new();
            for def in mining_sites::SOV_PROSPECTING_DEPOSITS {
                match mining_sites::resolve_ore_site(&ctx.sde, &def.site) {
                    Ok(site) => {
                        modelled.insert(def.upgrade_type_id, (def.level, def.respawn_seconds, site));
                    }
                    Err(e) => warn!(
                        upgrade_type_id = def.upgrade_type_id,
                        error = e,
                        "prospecting deposit not resolvable"
                    ),
                }
            }

            let sov_pos = derive_system_site_position_from_sde(ctx, sys_id, 3, 5.0);
            let type_vol_rad = |tid: u32| -> (f64, f64) {
                ctx.sde
                    .types
                    .get(&tid)
                    .map(|t| (t.volume.max(0.001), t.radius.max(60.0)))
                    .unwrap_or((0.001, 60.0))
            };
            let now = ctx.now_unix();
            let active_ship_id = session.ship_id;
            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let active_ship_pos: Option<DVec3> = if let Some(sid) = active_ship_id {
                sys_handle
                    .get_ball_snapshot(sid)
                    .await
                    .ok()
                    .flatten()
                    .map(|(_, b)| b.position)
                    .or_else(|| {
                        ctx.db
                            .with_snapshot(|s| s.item_positions.get(&sid).copied())
                            .map(DVec3::from_array)
                    })
            } else {
                None
            };
            let sys_ast_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
                snap.asteroids
                    .values()
                    .filter(|a| a.solar_system_id == sys_id && a.belt_id == 0)
                    .map(|a| a.asteroid_id)
                    .collect()
            });
            let mut live_ast_states = Vec::with_capacity(sys_ast_ids.len());
            for aid in sys_ast_ids {
                if let Ok(Some(st)) = sys_handle.get_asteroid_state(aid).await {
                    live_ast_states.push((aid, st.remaining_units as i64));
                }
            }

            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (caller_corp_id, caller_alliance_id, caller_roles) = snap
                    .characters
                    .get(&char_id)
                    .map(|c| {
                        (
                            c.corporation_id,
                            c.alliance_id.or(session.alliance_id),
                            session.corp_role,
                        )
                    })
                    .unwrap_or((
                        session.corporation_id.unwrap_or(0),
                        session.alliance_id,
                        session.corp_role,
                    ));
                let held_alliance = snap
                    .system_states
                    .get(&sys_id)
                    .and_then(|st| st.sov_alliance_id);
                let Some(caller_aid) = caller_alliance_id else {
                    return Err(BridgeRpcError {
                        code: "SovereigntyNotClaimed".into(),
                        message: format!(
                            "Caller must belong to the sovereignty-holding alliance in system {sys_id}"
                        ),
                    });
                };
                if held_alliance != Some(caller_aid) {
                    return Err(BridgeRpcError {
                        code: "SovereigntyNotClaimed".into(),
                        message: format!(
                            "System {sys_id} sovereignty is not held by caller's alliance {caller_aid}"
                        ),
                    });
                }

                // Verify an online friendly sovereignty structure exists in sys_id and caller has director/station-manager/CEO role
                let corp_rec = snap.corporations.get(&caller_corp_id);
                let is_ceo = corp_rec.is_some_and(|c| c.ceo_id == char_id);
                let member_role = corp_rec
                    .and_then(|c| c.member_roles.get(&char_id).copied())
                    .unwrap_or(caller_roles);
                if !is_ceo && (member_role & (1 | 128 | 1024)) == 0 {
                    return Err(BridgeRpcError {
                        code: "InsufficientCorporationRole".into(),
                        message: "Configuring Sovereignty Hub upgrades requires Director or Station Manager role".into(),
                    });
                }

                // Lowest qualifying structure ID, so the hub is deterministic when several qualify.
                let hub_struct = snap
                    .structures
                    .values()
                    .filter(|s| {
                        let same_corp = s.owner_corp_id == caller_corp_id;
                        let same_alliance = snap
                            .corporations
                            .get(&s.owner_corp_id)
                            .and_then(|c| c.alliance_id)
                            == Some(caller_aid);
                        s.solar_system_id == sys_id
                            && (same_corp || same_alliance)
                            && s.state == "ShieldVulnerable"
                            && s.quantum_core_installed
                            && s.fuel_blocks > 0
                    })
                    .map(|s| s.structure_id)
                    .min();
                let Some(hub_id) = hub_struct else {
                    return Err(BridgeRpcError {
                        code: "NoFriendlySovStructureInSystem".into(),
                        message: format!(
                            "Configuring Sovereignty Hub upgrades in system {sys_id} requires an online structure owned by alliance {caller_aid}"
                        ),
                    });
                };
                let hub_pos = snap
                    .structures
                    .get(&hub_id)
                    .map(|s| DVec3::from_array(s.position));

                // The upgrade item (SDE group 4768) must be owned by the caller or their corporation (or already
                // installed in the hub), sit in the hub itself or the caller's active ship (within 50 km of the hub),
                // and be unlocked.
                let mut found_locked_local = false;
                let mut found_out_of_range_ship = false;
                let mut upgrade_item_id: Option<i64> = None;
                for it in snap.items.values() {
                    if it.quantity <= 0 || !is_sov_upgrade_type(it.type_id) {
                        continue;
                    }
                    if !requested_upgrade_type.map_or(true, |rt| it.type_id == rt) {
                        continue;
                    }
                    let owned_or_installed = it.owner_id == char_id
                        || it.owner_id == caller_corp_id
                        || (it.location_id == hub_id
                            && it.flag == eve_db::InventoryFlag::SubSystemSlot0);
                    let in_hub = it.location_id == hub_id;
                    let in_ship = active_ship_id == Some(it.location_id);
                    if !owned_or_installed || (!in_hub && !in_ship) {
                        continue;
                    }
                    if in_ship && !in_hub {
                        if let (Some(s_pos), Some(h_pos)) = (active_ship_pos, hub_pos) {
                            if s_pos.length_squared() > 0.0
                                && h_pos.length_squared() > 0.0
                                && s_pos.distance(h_pos) > 50_000.0
                            {
                                found_out_of_range_ship = true;
                                continue;
                            }
                        }
                    }
                    if it.flag == eve_db::InventoryFlag::Locked
                        || snap.is_item_locked_in_contract_or_job(it.item_id)
                    {
                        found_locked_local = true;
                        continue;
                    }
                    upgrade_item_id =
                        Some(upgrade_item_id.map_or(it.item_id, |cur| cur.min(it.item_id)));
                }

                let Some(up_iid) = upgrade_item_id else {
                    if found_out_of_range_ship {
                        return Err(BridgeRpcError {
                            code: "OutsideSovStructureProximity".into(),
                            message: "Installing a Sovereignty Hub upgrade from your ship requires being within 50 km of the hub".into(),
                        });
                    }
                    if found_locked_local {
                        return Err(BridgeRpcError {
                            code: "UpgradeItemLocked".into(),
                            message: "Sovereignty Hub upgrade item is locked in a contract or escrow".into(),
                        });
                    }
                    return Err(BridgeRpcError {
                        code: "UpgradeItemNotFound".into(),
                        message: "SaveUpgradeConfiguration requires an unlocked Sovereignty Hub upgrade item (group 4768) in the hub structure or your active ship".into(),
                    });
                };
                let up_tid = snap.items.get(&up_iid).map(|i| i.type_id).unwrap_or(0);
                let Some((level, respawn_secs, site)) = modelled.get(&up_tid) else {
                    return Err(BridgeRpcError {
                        code: "ProspectingDepositNotModelled".into(),
                        message: format!(
                            "Sovereignty Hub upgrade type {up_tid} has no modelled prospecting deposit"
                        ),
                    });
                };

                let was_already_installed = snap.items.get(&up_iid).is_some_and(|it| {
                    it.location_id == hub_id && it.flag == eve_db::InventoryFlag::SubSystemSlot0
                });
                if let Some(up_it) = snap.items.get_mut(&up_iid) {
                    up_it.location_id = hub_id;
                    up_it.flag = eve_db::InventoryFlag::SubSystemSlot0;
                }
                snap.sov_prospecting_levels.insert(sys_id, u32::from(*level));
                if let Some(st) = snap.system_states.get_mut(&sys_id) {
                    st.industrial_index = st.industrial_index.max(*level);
                }

                // Sync live SystemActor asteroid quantities into snapshot and mark cleared SovOre deposits
                // with their wiki respawn timer (`respawn_seconds`, e.g. 36,000s = 10h for Large Veldspar Deposit).
                for &(aid, rem) in &live_ast_states {
                    if let Some(ast) = snap.asteroids.get_mut(&aid) {
                        ast.quantity = rem;
                    }
                }
                let mut newly_cleared_anom_ids = Vec::new();
                if let Some(anoms) = snap.cosmic_anomalies.get_mut(&sys_id) {
                    for anom in anoms.iter_mut().filter(|a| a.anomaly_type == "SovOre") {
                        let anom_pos = DVec3::from_array(anom.position);
                        let any_remaining = snap.asteroids.values().any(|a| {
                            a.solar_system_id == sys_id
                                && a.quantity > 0
                                && DVec3::new(a.position.0, a.position.1, a.position.2)
                                    .distance(anom_pos)
                                    <= 50_000.0
                        });
                        if !any_remaining {
                            if !anom.cleared {
                                anom.cleared = true;
                                anom.remaining_volume = 0.0;
                                anom.created_unix = now;
                                anom.lifetime_seconds = *respawn_secs;
                                newly_cleared_anom_ids.push(anom.anomaly_id.clone());
                            } else if anom.lifetime_seconds == 0 {
                                anom.created_unix = now;
                                anom.lifetime_seconds = *respawn_secs;
                            }
                        }
                    }
                }
                if !newly_cleared_anom_ids.is_empty() {
                    if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
                        for s in sigs.iter_mut() {
                            if newly_cleared_anom_ids.contains(&s.signature_id) {
                                s.collapsed = true;
                                s.signal_strength = 0.0;
                            }
                        }
                    }
                }

                // Installing an upgrade is a one-time configuration change (`R74-P1-1`):
                // an already-installed upgrade never triggers a deposit spawn in SaveUpgradeConfiguration.
                // Subsequent respawns after deposit depletion are driven by the server-clock timer in
                // `tick_sov_prospecting_deposit_respawn` (`seed_system_mining_sites_from_sde`).
                let existing_sov_anoms: Vec<String> = snap
                    .cosmic_anomalies
                    .get(&sys_id)
                    .map(|list| {
                        list.iter()
                            .filter(|a| a.anomaly_type == "SovOre" && !a.cleared)
                            .map(|a| a.anomaly_id.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                if was_already_installed || !existing_sov_anoms.is_empty() {
                    return Ok((hub_id, *level, i64::from(up_tid), site.clone(), existing_sov_anoms, Vec::new()));
                }

                snap.next_entity_id += 1;
                let anom_id = format!("DNG-{}-{}", site.dungeon_id, snap.next_entity_id);
                let mut volume = 0.0_f64;
                let mut newly_spawned = Vec::with_capacity(site.asteroids.len());
                for (i, a) in site.asteroids.iter().enumerate() {
                    let off = mining_sites::asteroid_offset(i);
                    let pos = [sov_pos[0] + off[0], sov_pos[1] + off[1], sov_pos[2] + off[2]];
                    let (uvol, rad) = type_vol_rad(a.type_id);
                    volume += a.units as f64 * uvol;
                    snap.next_entity_id += 1;
                    let aid = snap.next_entity_id;
                    snap.asteroids.insert(
                        aid,
                        AsteroidRecord {
                            asteroid_id: aid,
                            solar_system_id: sys_id,
                            belt_id: 0,
                            type_id: a.type_id,
                            quantity: a.units,
                            initial_quantity: a.units,
                            position: (pos[0], pos[1], pos[2]),
                            radius: rad,
                        },
                    );
                    newly_spawned.push((aid, a.type_id, a.units as u64, uvol, rad, DVec3::from_array(pos)));
                }
                snap.cosmic_anomalies.entry(sys_id).or_default().push(CosmicAnomalyRecord {
                    anomaly_id: anom_id.clone(),
                    solar_system_id: sys_id,
                    name: site.name.clone(),
                    anomaly_type: "SovOre".into(),
                    position: sov_pos,
                    signal_strength: 1.0,
                    ore_types: site.ore_types(),
                    remaining_volume: volume,
                    created_unix: now,
                    lifetime_seconds: 0,
                    cleared: false,
                });
                snap.cosmic_signatures.entry(sys_id).or_default().push(CosmicSignatureRecord {
                    signature_id: anom_id.clone(),
                    solar_system_id: sys_id,
                    sig_group: "SovOre".into(),
                    type_name: site.name.clone(),
                    position: sov_pos,
                    signal_strength: 1.0,
                    wormhole_type_id: None,
                    destination_system_id: None,
                    created_unix: now,
                    lifetime_seconds: 0,
                    max_mass_kg: 0.0,
                    remaining_mass_kg: 0.0,
                    max_jump_mass_kg: 0.0,
                    collapsed: false,
                });

                Ok((hub_id, *level, i64::from(up_tid), site.clone(), vec![anom_id], newly_spawned))
            });

            let (hub_id, level, upgrade_type_id, site, anom_ids, newly_spawned) = match res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };

            if !newly_spawned.is_empty() {
                for (aid, tid, units, uvol, rad, pos) in newly_spawned {
                    let ball = Ball {
                        id: aid,
                        type_id: tid,
                        position: pos,
                        velocity: DVec3::ZERO,
                        max_velocity: 0.0,
                        inertia: 1.0,
                        mass: 80_000.0,
                        radius: rad,
                        speed_fraction: 0.0,
                        mode: MovementMode::Stop,
                    };
                    let st = AsteroidState {
                        asteroid_id: aid,
                        belt_id: 0,
                        type_id: tid,
                        initial_units: units,
                        remaining_units: units,
                        unit_volume: uvol,
                        depleted: false,
                        respawn_at_tick: None,
                    };
                    let _ = sys_handle.spawn_asteroid(st, ball).await;
                }
            }

            Some(Ok(PyRep::keyval([
                ("hubID", PyRep::Integer(hub_id)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("prospectingLevel", PyRep::Integer(i64::from(level))),
                ("upgradeTypeID", PyRep::Integer(upgrade_type_id)),
                ("online", PyRep::Bool(true)),
                (
                    "spawnedAnomalies",
                    PyRep::List(anom_ids.into_iter().map(PyRep::String).collect()),
                ),
                (
                    "oreTypes",
                    PyRep::List(
                        site.ore_types()
                            .into_iter()
                            .map(|tid| PyRep::Integer(i64::from(tid)))
                            .collect(),
                    ),
                ),
            ])))
        }

        ("sovHubSvc", "GetInstalledUpgradesLocal")
        | ("sovHubSvc", "GetInstalledUpgradesForHub") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            tick_sov_prospecting_deposit_respawn(ctx, sys_id).await;
            let arg_hub_id = args.first().and_then(|a| a.as_i64());
            let (hub_id, lvl, sov_anoms, installed) = ctx.db.with_snapshot(|snap| {
                let (caller_corp_id, caller_alliance_id) = snap
                    .characters
                    .get(&char_id)
                    .map(|c| (c.corporation_id, c.alliance_id.or(session.alliance_id)))
                    .unwrap_or((session.corporation_id.unwrap_or(0), session.alliance_id));
                let resolved_hub = arg_hub_id.or_else(|| {
                    snap.structures
                        .values()
                        .filter(|s| {
                            let same_corp = s.owner_corp_id == caller_corp_id;
                            let same_alliance = caller_alliance_id.is_some()
                                && snap
                                    .corporations
                                    .get(&s.owner_corp_id)
                                    .and_then(|c| c.alliance_id)
                                    == caller_alliance_id;
                            s.solar_system_id == sys_id && (same_corp || same_alliance)
                        })
                        .map(|s| s.structure_id)
                        .min()
                        .or_else(|| {
                            snap.structures
                                .values()
                                .filter(|s| s.solar_system_id == sys_id)
                                .map(|s| s.structure_id)
                                .min()
                        })
                });
                // Installed upgrades: SDE group-4768 items moved into the hub by SaveUpgradeConfiguration.
                let mut installed: Vec<u32> = resolved_hub
                    .map(|hid| {
                        snap.items
                            .values()
                            .filter(|it| {
                                it.location_id == hid
                                    && it.flag == eve_db::InventoryFlag::SubSystemSlot0
                                    && ctx.sde.types.get(&it.type_id).is_some_and(|t| t.group_id == 4768)
                            })
                            .map(|it| it.type_id)
                            .collect()
                    })
                    .unwrap_or_default();
                installed.sort_unstable();
                let l = snap.sov_prospecting_levels.get(&sys_id).copied().unwrap_or(0);
                let list: Vec<PyRep> = snap
                    .cosmic_anomalies
                    .get(&sys_id)
                    .map(|anoms| {
                        anoms
                            .iter()
                            .filter(|a| a.anomaly_type == "SovOre" && !a.cleared)
                            .map(|a| {
                                PyRep::keyval([
                                    ("anomalyID", PyRep::String(a.anomaly_id.clone())),
                                    ("siteName", PyRep::String(a.name.clone())),
                                    ("siteType", PyRep::String(a.anomaly_type.clone())),
                                    ("signalStrength", PyRep::Real(1.0)),
                                    ("x", PyRep::Real(a.position[0])),
                                    ("y", PyRep::Real(a.position[1])),
                                    ("z", PyRep::Real(a.position[2])),
                                    (
                                        "oreTypes",
                                        PyRep::List(
                                            a.ore_types
                                                .iter()
                                                .map(|&tid| PyRep::Integer(tid as i64))
                                                .collect(),
                                        ),
                                    ),
                                ])
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                (resolved_hub, l, list, installed)
            });
            let upgrades: Vec<PyRep> = installed
                .into_iter()
                .map(|tid| {
                    PyRep::keyval([
                        ("typeID", PyRep::Integer(i64::from(tid))),
                        ("online", PyRep::Bool(true)),
                    ])
                })
                .collect();
            Some(Ok(PyRep::keyval([
                ("hubID", hub_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("prospectingLevel", PyRep::Integer(lvl as i64)),
                ("upgrades", PyRep::List(upgrades)),
                ("anomalies", PyRep::List(sov_anoms)),
            ])))
        }

        ("scanSvc", "GetSystemScanMgr")
        | ("scanSvc", "GetScanResults")
        | ("scanMgr", "GetScanResults") => {
            let (_user_id, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let handle = ctx.systems.get_or_spawn(sys_id);
            let now = ctx.now_unix();
            if sys_id >= 31_000_000 {
                let existing = handle.list_balls().await.unwrap_or_default();
                let wh_gas_pos_opt = ctx.db.with_snapshot(|snap| {
                    snap.cosmic_signatures.get(&sys_id).and_then(|list| {
                        list.iter()
                            .find(|s| {
                                !s.collapsed
                                    && s.sig_group == "Gas"
                                    && ctx.effective_signature_signal_strength(char_id, sys_id, s) >= 1.0
                                    && now >= s.created_unix.saturating_add(900)
                            })
                            .map(|s| DVec3::from_array(s.position))
                    })
                });
                if let Some(gas_pos) = wh_gas_pos_opt {
                    let has_sleeper = existing.iter().any(|b| {
                        ctx.sde.types.get(&b.type_id).is_some_and(|t| t.category_id == 11)
                            && b.position.distance(gas_pos) <= 50_000.0
                    });
                    if !has_sleeper {
                        if let Some(wh_def) = crate::services::mining_sites::WORMHOLE_GAS_SITES.first() {
                            for i in 0..wh_def.guard_npc_count {
                                let npc_id = ctx.db.with_mut_snapshot(|snap| {
                                    snap.next_entity_id += 1;
                                    snap.next_entity_id
                                });
                                let _ = handle
                                    .spawn_ball_with_combat(
                                        Ball {
                                            id: npc_id,
                                            type_id: wh_def.guard_npc_type_id,
                                            position: gas_pos + DVec3::new(3_000.0 + (i as f64) * 500.0, 0.0, 0.0),
                                            velocity: DVec3::ZERO,
                                            max_velocity: 320.0,
                                            inertia: 2.0,
                                            mass: 1_200_000.0,
                                            radius: 40.0,
                                            speed_fraction: 0.0,
                                            mode: MovementMode::Stop,
                                        },
                                        ctx.build_type_combat_state(wh_def.guard_npc_type_id),
                                    )
                                    .await;
                            }
                        }
                    }
                }
            }
            let live_balls = handle.list_balls().await.unwrap_or_default();

            let (anom_list, sig_list, gas_npc_ball_id) = ctx.db.with_snapshot(|snap| {
                let mut found_gas_npc_id: Option<i64> = None;

                let anoms: Vec<PyRep> = snap
                    .cosmic_anomalies
                    .get(&sys_id)
                    .map(|list| {
                        list.iter()
                            .filter(|a| {
                                !a.cleared
                                    && !(a.anomaly_type == "OreAnomaly"
                                        && a.created_unix > 0
                                        && crate::services::mining_sites::is_ore_anomaly_expired(
                                            now.saturating_sub(a.created_unix),
                                        ))
                            })
                            .map(|a| {
                                let anom_pos = DVec3::from_array(a.position);
                                let mut site_asts: Vec<&AsteroidRecord> = snap
                                    .asteroids
                                    .values()
                                    .filter(|ast| {
                                        ast.solar_system_id == sys_id
                                            && ast.quantity > 0
                                            && DVec3::new(
                                                ast.position.0,
                                                ast.position.1,
                                                ast.position.2,
                                            )
                                            .distance(anom_pos)
                                                <= 50_000.0
                                    })
                                    .collect();
                                site_asts.sort_by_key(|ast| ast.asteroid_id);
                                let asteroid_ids: Vec<PyRep> = site_asts
                                    .iter()
                                    .map(|ast| PyRep::Integer(ast.asteroid_id))
                                    .collect();
                                let ore_types: Vec<PyRep> = a
                                    .ore_types
                                    .iter()
                                    .map(|&tid| PyRep::Integer(tid as i64))
                                    .collect();
                                PyRep::keyval([
                                    ("id", PyRep::String(a.anomaly_id.clone())),
                                    ("anomalyID", PyRep::String(a.anomaly_id.clone())),
                                    ("siteType", PyRep::String(a.anomaly_type.clone())),
                                    ("siteName", PyRep::String(a.name.clone())),
                                    ("signalStrength", PyRep::Real(1.0)),
                                    ("probeRequired", PyRep::Bool(false)),
                                    ("cleared", PyRep::Bool(a.cleared)),
                                    ("x", PyRep::Real(a.position[0])),
                                    ("y", PyRep::Real(a.position[1])),
                                    ("z", PyRep::Real(a.position[2])),
                                    ("oreTypes", PyRep::List(ore_types)),
                                    ("asteroidBallIDs", PyRep::List(asteroid_ids)),
                                ])
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                let sigs: Vec<PyRep> = snap
                    .cosmic_signatures
                    .get(&sys_id)
                    .map(|list| {
                        list.iter()
                            .filter(|s| {
                                !s.collapsed
                                    && !matches!(s.sig_group.as_str(), "OreAnomaly" | "SovOre")
                            })
                            .map(|s| {
                                let eff_strength = ctx.effective_signature_signal_strength(char_id, sys_id, s);
                                let revealed = eff_strength >= 1.0;
                                let pos_known = eff_strength >= 0.25;
                                let sig_pos = DVec3::from_array(s.position);

                                if s.sig_group == "Gas" {
                                    if let Some(npc_ball) = live_balls.iter().find(|b| {
                                        ctx.sde
                                            .types
                                            .get(&b.type_id)
                                            .is_some_and(|t| t.category_id == 11)
                                            && b.position.distance(sig_pos) <= 50_000.0
                                    }) {
                                        found_gas_npc_id = Some(npc_ball.id);
                                    }
                                }

                                let (ore_types, ball_ids) = if revealed
                                    && matches!(s.sig_group.as_str(), "Ore" | "Gas")
                                {
                                    let mut site_asts: Vec<&AsteroidRecord> = snap
                                        .asteroids
                                        .values()
                                        .filter(|ast| {
                                            ast.solar_system_id == sys_id
                                                && ast.quantity > 0
                                                && DVec3::new(
                                                    ast.position.0,
                                                    ast.position.1,
                                                    ast.position.2,
                                                )
                                                .distance(sig_pos)
                                                    <= 50_000.0
                                        })
                                        .collect();
                                    site_asts.sort_by_key(|ast| ast.asteroid_id);
                                    let tids: Vec<PyRep> = site_asts
                                        .iter()
                                        .map(|ast| PyRep::Integer(ast.type_id as i64))
                                        .collect();
                                    let bids: Vec<PyRep> = site_asts
                                        .iter()
                                        .map(|ast| PyRep::Integer(ast.asteroid_id))
                                        .collect();
                                    (tids, bids)
                                } else {
                                    (vec![], vec![])
                                };
                                PyRep::keyval([
                                    ("id", PyRep::String(s.signature_id.clone())),
                                    ("signatureID", PyRep::String(s.signature_id.clone())),
                                    ("sigGroup", PyRep::String(s.sig_group.clone())),
                                    (
                                        "siteType",
                                        if revealed {
                                            PyRep::String(s.sig_group.clone())
                                        } else {
                                            PyRep::String("Unknown".into())
                                        },
                                    ),
                                    (
                                        "siteName",
                                        if revealed {
                                            PyRep::String(s.type_name.clone())
                                        } else {
                                            PyRep::String("Unidentified Cosmic Signature".into())
                                        },
                                    ),
                                    ("signalStrength", PyRep::Real(eff_strength)),
                                    ("warpable", PyRep::Bool(revealed)),
                                    (
                                        "x",
                                        if pos_known {
                                            PyRep::Real(s.position[0])
                                        } else {
                                            PyRep::None
                                        },
                                    ),
                                    (
                                        "y",
                                        if pos_known {
                                            PyRep::Real(s.position[1])
                                        } else {
                                            PyRep::None
                                        },
                                    ),
                                    (
                                        "z",
                                        if pos_known {
                                            PyRep::Real(s.position[2])
                                        } else {
                                            PyRep::None
                                        },
                                    ),
                                    ("oreTypes", PyRep::List(ore_types)),
                                    ("siteBallIDs", PyRep::List(ball_ids)),
                                    (
                                        "gasSiteNpcBallID",
                                        if s.sig_group == "Gas" {
                                            found_gas_npc_id
                                                .map(PyRep::Integer)
                                                .unwrap_or(PyRep::None)
                                        } else {
                                            PyRep::None
                                        },
                                    ),
                                ])
                            })
                            .collect()
                    })
                    .unwrap_or_default();

                (anoms, sigs, found_gas_npc_id)
            });

            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("anomalies", PyRep::List(anom_list)),
                ("signatures", PyRep::List(sig_list)),
                ("gasSiteNpcSpawned", PyRep::Bool(gas_npc_ball_id.is_some())),
                (
                    "gasSiteNpcBallID",
                    gas_npc_ball_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
            ])))
        }

        ("scanSvc", "ConeScan") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = session.solar_system_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "Session has no active solar system".into(),
                }));
            };
            let max_range = args
                .get(1)
                .and_then(|a| a.as_f64())
                .unwrap_or(14.4 * AU_METERS);
            let handle = ctx.systems.get_or_spawn(sys_id);
            let ship_pos = if let Some(ship_id) = session.ship_id {
                handle
                    .get_ball_snapshot(ship_id)
                    .await
                    .ok()
                    .flatten()
                    .map(|(_, b)| b.position)
                    .unwrap_or(DVec3::ZERO)
            } else {
                DVec3::ZERO
            };

            let all_balls = handle.list_balls().await.unwrap_or_default();
            let mut results = Vec::new();
            for b in all_balls {
                if Some(b.id) == session.ship_id {
                    continue;
                }
                let dist = ship_pos.distance(b.position);
                if dist <= max_range {
                    let group_id = ctx
                        .sde
                        .types
                        .get(&b.type_id)
                        .map(|t| t.group_id as i64)
                        .unwrap_or(0);
                    results.push(PyRep::keyval([
                        ("itemID", PyRep::Integer(b.id)),
                        ("typeID", PyRep::Integer(b.type_id as i64)),
                        ("groupID", PyRep::Integer(group_id)),
                        ("distance", PyRep::Real(dist)),
                    ]));
                }
            }
            Some(Ok(PyRep::List(results)))
        }

        _ => None,
    }
}
