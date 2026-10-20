//! `sov_fw_insurgency` — Domain 11: Sovereignty, Equinox Skyhooks/Mercenary Dens, Faction Warfare, & Pirate Insurgencies (`§3.8`, `§3.11`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::mining_sites;
use crate::system_actor::AsteroidState;
use eve_db::{AsteroidRecord, CosmicAnomalyRecord, CosmicSignatureRecord};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use eve_sde::TypeId;
use std::collections::HashMap;
use tracing::warn;

const AU_METERS: f64 = 149_597_870_700.0;

pub fn derive_system_site_position_from_sde(
    ctx: &ServerContext,
    sys_id: u32,
    slot: usize,
    au_scale: f64,
) -> [f64; 3] {
    let mut cels = ctx.sde.celestials_in_system(sys_id);
    cels.sort_by_key(|c| c.celestial_id);
    let base_dir = if !cels.is_empty() {
        let c = &cels[slot % cels.len()];
        let v = c.position;
        if v.length_squared() > 1.0 {
            v.normalize()
        } else {
            DVec3::new(0.6, 0.4, -0.5).normalize()
        }
    } else {
        DVec3::new(0.6, 0.4, -0.5).normalize()
    };
    let offset_dir = match slot % 4 {
        0 => DVec3::new(base_dir.x, base_dir.y, base_dir.z),
        1 => DVec3::new(-base_dir.y, base_dir.x, base_dir.z * 0.8),
        2 => DVec3::new(base_dir.z, -base_dir.x, base_dir.y),
        _ => DVec3::new(-base_dir.x, -base_dir.z, base_dir.y),
    }
    .normalize_or_zero();
    let dir = if offset_dir.length_squared() > 0.1 {
        offset_dir
    } else {
        DVec3::X
    };
    let pos = dir * (au_scale * AU_METERS);
    [pos.x, pos.y, pos.z]
}

/// Seeds the cosmic ore anomaly, hidden ore signature and gas site for `sys_id`, once per system.
///
/// Which sites spawn depends on the system's displayed security band (and, for gas sites, its region);
/// see [`mining_sites::COSMIC_ORE_SITES`] and [`mining_sites::GAS_SITES`]. Asteroid types and counts come
/// from the client's per-dungeon layout, unit quantities from the cited wiki rows. Systems with no
/// modelled site for their band (for example 0.9-1.0 high-sec ore anomalies, which the wiki limits to
/// starter systems, or wormhole space) get none. Sites only despawn when emptied
/// (`eve-wiki/articles/Asteroids_and_ore.md:72`); timed expiry isn't modelled, so `lifetime_seconds` is 0.
pub async fn seed_system_mining_sites_from_sde(ctx: &ServerContext, sys_id: u32) {
    use mining_sites::{SiteKind, COSMIC_ORE_SITES, GAS_SITES};

    let Some(true_sec) = ctx.sde.solar_systems.get(&sys_id).map(|s| s.security_status) else {
        return;
    };
    let region = mining_sites::region_name(&ctx.sde, sys_id).map(str::to_owned);

    enum Planned {
        Ore(SiteKind, mining_sites::ResolvedOreSite),
        Gas(mining_sites::ResolvedGasSite),
    }
    let mut planned: Vec<Planned> = Vec::new();
    for def in COSMIC_ORE_SITES
        .iter()
        .filter(|d| d.band.matches(sys_id, true_sec))
    {
        match mining_sites::resolve_ore_site(&ctx.sde, def) {
            Ok(site) if site.asteroids.iter().all(|a| ctx.sde.types.contains_key(&a.type_id)) => {
                planned.push(Planned::Ore(def.kind, site))
            }
            Ok(site) => warn!(
                site = site.name,
                "ore site references an asteroid type missing from sde_types; not spawned"
            ),
            Err(e) => warn!(site = def.dungeon_name, error = e, "ore site not spawned"),
        }
    }
    if let Some(def) = GAS_SITES.iter().find(|d| {
        d.band.matches(sys_id, true_sec)
            && region
                .as_deref()
                .is_some_and(|r| d.region_names.contains(&r))
    }) {
        match mining_sites::resolve_gas_site(&ctx.sde, def) {
            Ok(site) if ctx.sde.types.contains_key(&site.gas_type_id) => {
                planned.push(Planned::Gas(site))
            }
            Ok(site) => warn!(
                site = site.name,
                "gas site references a gas type missing from sde_types; not spawned"
            ),
            Err(e) => warn!(site = def.dungeon_name, error = e, "gas site not spawned"),
        }
    }
    if planned.is_empty() {
        return;
    }

    let type_vol_rad = |tid: u32| -> (f64, f64) {
        ctx.sde
            .types
            .get(&tid)
            .map(|t| (t.volume.max(0.001), t.radius.max(50.0)))
            .unwrap_or((0.001, 50.0))
    };
    // Site centres: anomaly 4 AU, signature 6.5 AU, gas 8.5 AU out along SDE celestial directions.
    let centre_for = |p: &Planned| -> [f64; 3] {
        match p {
            Planned::Ore(SiteKind::Anomaly, _) => derive_system_site_position_from_sde(ctx, sys_id, 0, 4.0),
            Planned::Ore(SiteKind::Signature, _) => derive_system_site_position_from_sde(ctx, sys_id, 1, 6.5),
            Planned::Gas(_) => derive_system_site_position_from_sde(ctx, sys_id, 2, 8.5),
        }
    };
    let centres: Vec<[f64; 3]> = planned.iter().map(centre_for).collect();

    let now = ctx.now_unix();
    let newly_spawned = ctx.db.with_mut_snapshot(|snap| {
        let already_seeded = snap
            .cosmic_anomalies
            .get(&sys_id)
            .is_some_and(|list| list.iter().any(|a| a.anomaly_type == "OreAnomaly"))
            || snap.cosmic_signatures.get(&sys_id).is_some_and(|list| {
                list.iter()
                    .any(|s| matches!(s.sig_group.as_str(), "OreAnomaly" | "Ore" | "Gas"))
            });
        if already_seeded {
            return Vec::new();
        }

        let mut spawned = Vec::new();
        for (p, centre) in planned.iter().zip(centres.iter().copied()) {
            snap.next_entity_id += 1;
            let seq = snap.next_entity_id;
            let blank_sig = |id: String, group: &str, name: String, strength: f64| CosmicSignatureRecord {
                signature_id: id,
                solar_system_id: sys_id,
                sig_group: group.into(),
                type_name: name,
                signal_strength: strength,
                position: centre,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: now,
                lifetime_seconds: 0,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            };
            let rocks: Vec<(TypeId, i64)> = match p {
                Planned::Ore(kind, site) => {
                    let site_id = format!("DNG-{}-{seq}", site.dungeon_id);
                    match kind {
                        SiteKind::Anomaly => {
                            let volume: f64 = site
                                .asteroids
                                .iter()
                                .map(|a| a.units as f64 * type_vol_rad(a.type_id).0)
                                .sum();
                            snap.cosmic_anomalies.entry(sys_id).or_default().push(
                                CosmicAnomalyRecord {
                                    anomaly_id: site_id.clone(),
                                    solar_system_id: sys_id,
                                    name: site.name.clone(),
                                    anomaly_type: "OreAnomaly".into(),
                                    position: centre,
                                    signal_strength: 1.0,
                                    ore_types: site.ore_types(),
                                    remaining_volume: volume,
                                    created_unix: now,
                                    lifetime_seconds: 0,
                                    cleared: false,
                                },
                            );
                            snap.cosmic_signatures
                                .entry(sys_id)
                                .or_default()
                                .push(blank_sig(site_id, "OreAnomaly", site.name.clone(), 1.0));
                        }
                        SiteKind::Signature => {
                            snap.cosmic_signatures
                                .entry(sys_id)
                                .or_default()
                                .push(blank_sig(site_id, "Ore", site.name.clone(), 0.0));
                        }
                    }
                    site.asteroids.iter().map(|a| (a.type_id, a.units)).collect()
                }
                Planned::Gas(site) => {
                    let site_id = format!("DNG-{}-{seq}", site.dungeon_id);
                    snap.cosmic_signatures
                        .entry(sys_id)
                        .or_default()
                        .push(blank_sig(site_id, "Gas", site.name.clone(), 0.0));
                    site.cloud_units.iter().map(|&u| (site.gas_type_id, u)).collect()
                }
            };

            for (i, (tid, units)) in rocks.into_iter().enumerate() {
                let off = mining_sites::asteroid_offset(i);
                let pos = [centre[0] + off[0], centre[1] + off[1], centre[2] + off[2]];
                let (uvol, rad) = type_vol_rad(tid);
                snap.next_entity_id += 1;
                let aid = snap.next_entity_id;
                snap.asteroids.insert(
                    aid,
                    AsteroidRecord {
                        asteroid_id: aid,
                        solar_system_id: sys_id,
                        belt_id: 0,
                        type_id: tid,
                        quantity: units,
                        initial_quantity: units,
                        position: (pos[0], pos[1], pos[2]),
                        radius: rad,
                    },
                );
                spawned.push((aid, tid, units as u64, uvol, rad, DVec3::from_array(pos)));
            }
        }
        spawned
    });

    if !newly_spawned.is_empty() {
        let handle = ctx.systems.get_or_spawn(sys_id);
        for (aid, tid, units, uvol, rad, pos) in newly_spawned {
            let ball = Ball {
                id: aid,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000.0,
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
            let _ = handle.spawn_asteroid(st, ball).await;
        }
    }
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
        ("facWarMgr", "JoinFactionAsCharacter")
        | ("fwCharacterEnlistmentMgr", "CreateMyDirectEnlistment") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(faction_id) = args.first().and_then(|a| a.as_i64()) else {
                return Some(Err(BridgeRpcError {
                    code: "InvalidWarFaction".into(),
                    message: "Joining Factional Warfare requires an explicit integer factionID".into(),
                }));
            };
            if !matches!(
                faction_id,
                500_001 | 500_002 | 500_003 | 500_004 | 500_010 | 500_011
            ) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidWarFaction".into(),
                    message: format!("Faction {faction_id} is not a valid FW/Insurgency militia faction"),
                }));
            }
            let _ = ctx.db.try_with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.war_faction_id = Some(faction_id);
                    ch.version += 1;
                }
                Ok::<_, std::convert::Infallible>(())
            });
            session.war_faction_id = Some(faction_id);
            Some(Ok(PyRep::keyval([
                ("factionID", PyRep::Integer(faction_id)),
                ("enlisted", PyRep::Bool(true)),
            ])))
        }

        ("facWarMgr", "CaptureComplex") | ("fwWarzoneSolarsystem", "CaptureComplex") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot capture FW/Insurgency complex while docked".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
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
            if !(sys_sec > 0.0 && sys_sec < 0.45) {
                return Some(Err(BridgeRpcError {
                    code: "NotLowSecWarzoneSystem".into(),
                    message: format!(
                        "Solar system {sys_id} (security {sys_sec:.2}) is not a low-sec FW warzone system (requires 0.0 < sec < 0.45)"
                    ),
                }));
            }

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let Some((current_tick, ship_ball)) =
                (match sys_handle.get_ball_snapshot(ship_id).await {
                    Ok(v) => v,
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "DestinyQueryFailed".into(),
                            message: e,
                        }))
                    }
                })
            else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: format!("Ship {ship_id} is not spawned in space in system {sys_id}"),
                }));
            };

            let effective_tick = current_tick.max(1);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let faction_id = snap
                    .characters
                    .get(&char_id)
                    .and_then(|c| c.war_faction_id)
                    .or(session.war_faction_id);
                let Some(fid) = faction_id else {
                    return Err(BridgeRpcError {
                        code: "NotEnlistedInFactionalWarfare".into(),
                        message:
                            "Character must enlist in Factional Warfare before capturing a complex"
                                .into(),
                    });
                };

                let mut outside_radius_dist = None;
                if let Some(st) = snap.system_states.get(&sys_id) {
                    if let Some(beacon_coords) = st.fw_complex_position {
                        let beacon_pos = glam::DVec3::from_array(beacon_coords);
                        let dist = ship_ball.position.distance(beacon_pos);
                        if dist > 30_000.0 {
                            outside_radius_dist = Some((dist, beacon_coords));
                        }
                    }
                }
                if let Some((dist, beacon_coords)) = outside_radius_dist {
                    if let Some(st) = snap.system_states.get_mut(&sys_id) {
                        st.last_fw_capture_tick = 0;
                    }
                    return Ok(Err((dist, beacon_coords)));
                }

                if let Some(st) = snap.system_states.get_mut(&sys_id) {
                    let Some(_beacon_coords) = st.fw_complex_position else {
                        return Err(BridgeRpcError {
                            code: "NoActiveComplexInSystem".into(),
                            message: format!("System {sys_id} has no active FW complex beacon"),
                        });
                    };
                    if st.fw_contested_pct >= 100.0 {
                        return Err(BridgeRpcError {
                            code: "ComplexAlreadyCaptured".into(),
                            message: format!("System {sys_id} is already 100% contested"),
                        });
                    };
                    if st.last_fw_capture_tick > 0 && effective_tick <= st.last_fw_capture_tick {
                        return Err(BridgeRpcError {
                            code: "ComplexCaptureTimerActive".into(),
                            message:
                                "Complex capture timer has already pulsed on the current simulation tick"
                                    .into(),
                        });
                    };
                    let duration = st.fw_capture_duration_seconds.max(600);
                    let tick_delta = if st.last_fw_capture_tick == 0 {
                        1
                    } else {
                        effective_tick
                            .saturating_sub(st.last_fw_capture_tick)
                            .clamp(1, 30)
                    };
                    st.last_fw_capture_tick = effective_tick;
                    st.fw_capture_seconds_elapsed = st
                        .fw_capture_seconds_elapsed
                        .saturating_add(tick_delta)
                        .min(duration);
                    st.fw_contested_pct =
                        ((st.fw_capture_seconds_elapsed as f64 / duration as f64) * 100.0)
                            .min(100.0);
                    st.insurgency_corruption_stage =
                        ((st.fw_contested_pct / 20.0).ceil() as u8).clamp(1, 5);
                    let lp_awarded = if st.fw_capture_seconds_elapsed >= duration {
                        15_000i64
                    } else {
                        0i64
                    };
                    let res_tuple = (
                        st.fw_contested_pct,
                        st.insurgency_corruption_stage,
                        lp_awarded,
                        st.fw_capture_seconds_elapsed,
                        duration,
                    );
                    if lp_awarded > 0 {
                        if let Some(ch) = snap.characters.get_mut(&char_id) {
                            *ch.loyalty_points.entry(fid).or_insert(0) += lp_awarded;
                        }
                    }
                    Ok(Ok(res_tuple))
                } else {
                    Err(BridgeRpcError {
                        code: "SystemStateNotFound".into(),
                        message: format!("System {sys_id} has no FW/Insurgency state"),
                    })
                }
            });

            match res {
                Ok(Ok((contested, corruption, lp_awarded, elapsed_s, duration_s))) => {
                    Some(Ok(PyRep::keyval([
                        ("solarSystemID", PyRep::Integer(sys_id as i64)),
                        ("contestedPct", PyRep::Real(contested)),
                        ("corruptionStage", PyRep::Integer(corruption as i64)),
                        ("lpAwarded", PyRep::Integer(lp_awarded)),
                        ("captureSecondsElapsed", PyRep::Integer(elapsed_s as i64)),
                        ("captureDurationSeconds", PyRep::Integer(duration_s as i64)),
                    ])))
                }
                Ok(Err((dist, beacon_coords))) => {
                    Some(Err(BridgeRpcError {
                        code: "OutsideComplexCaptureRadius".into(),
                        message: format!(
                            "Ship is {:.1} m from complex capture beacon {:?} (must be <= 30000 m)",
                            dist, beacon_coords
                        ),
                    }))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("sovMgr", "AcquireSkyhooks") => {
            let (_, char_id) = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if session.station_id.is_some() || session.structure_id.is_some() {
                return Some(Err(BridgeRpcError {
                    code: "ShipDocked".into(),
                    message: "Cannot claim sovereignty or acquire skyhooks while docked".into(),
                }));
            }
            let Some(ship_id) = session.ship_id else {
                return Some(Err(BridgeRpcError {
                    code: "NoActiveShip".into(),
                    message: "Session has no active ship".into(),
                }));
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
                        "Solar system {sys_id} (security {sys_sec:.2}) is not a claimable null-sec system"
                    ),
                }));
            }

            let sys_handle = ctx.systems.get_or_spawn(sys_id);
            let Some((current_tick, ship_ball)) =
                (match sys_handle.get_ball_snapshot(ship_id).await {
                    Ok(v) => v,
                    Err(e) => {
                        return Some(Err(BridgeRpcError {
                            code: "DestinyQueryFailed".into(),
                            message: e,
                        }))
                    }
                })
            else {
                return Some(Err(BridgeRpcError {
                    code: "ShipNotInSpace".into(),
                    message: format!("Ship {ship_id} is not spawned in space in system {sys_id}"),
                }));
            };

            let now_unix = ctx.now_unix();
            let effective_tick = current_tick.max(1);
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let (caller_corp_id, caller_alliance_id) = snap
                    .characters
                    .get(&char_id)
                    .map(|c| (c.corporation_id, c.alliance_id.or(session.alliance_id)))
                    .unwrap_or((session.corporation_id.unwrap_or(0), session.alliance_id));
                let Some(alliance_id) = caller_alliance_id else {
                    return Err(BridgeRpcError {
                        code: "NotInAlliance".into(),
                        message:
                            "Acquiring sovereignty/skyhooks requires membership in a player alliance"
                                .into(),
                    });
                };
                if !snap.alliances.contains_key(&alliance_id) {
                    return Err(BridgeRpcError {
                        code: "AllianceNotFound".into(),
                        message: format!("Alliance {alliance_id} is not registered"),
                    });
                }

                // Require an online (`ShieldVulnerable`), quantum-core-installed, fueled (`fuel_blocks > 0`) structure in sys_id owned by the caller's player corporation or alliance
                let mut friendly_structs: Vec<[f64; 3]> = Vec::new();
                let mut sov_struct_ids: Vec<i64> = Vec::new();
                let mut primary_hub_id: Option<i64> = None;
                for s in snap.structures.values() {
                    let same_player_corp =
                        caller_corp_id >= 98_000_000 && s.owner_corp_id == caller_corp_id;
                    let same_alliance = snap
                        .corporations
                        .get(&s.owner_corp_id)
                        .and_then(|c| c.alliance_id)
                        == Some(alliance_id);
                    if s.solar_system_id == sys_id
                        && (same_player_corp || same_alliance)
                        && s.state == "ShieldVulnerable"
                        && s.quantum_core_installed
                        && s.fuel_blocks > 0
                    {
                        friendly_structs.push(s.position);
                        if s.type_id == 32226 || s.type_id == 32458 {
                            sov_struct_ids.push(s.structure_id);
                            primary_hub_id = Some(s.structure_id);
                        } else if primary_hub_id.is_none() {
                            primary_hub_id = Some(s.structure_id);
                        }
                    }
                }
                if friendly_structs.is_empty() {
                    return Err(BridgeRpcError {
                        code: "NoFriendlySovStructureInSystem".into(),
                        message: format!(
                            "Claiming sovereignty in system {sys_id} requires an online structure owned by corporation {caller_corp_id} or alliance {alliance_id}"
                        ),
                    });
                }
                let min_dist = friendly_structs
                    .iter()
                    .map(|pos| ship_ball.position.distance(glam::DVec3::from_array(*pos)))
                    .fold(f64::INFINITY, f64::min);
                if min_dist > 50_000.0 {
                    return Err(BridgeRpcError {
                        code: "OutsideSovStructureProximity".into(),
                        message: format!(
                            "Ship is {:.1} m from nearest friendly sovereignty structure (must be <= 50000 m)",
                            min_dist
                        ),
                    });
                }

                let Some(st) = snap.system_states.get_mut(&sys_id) else {
                    return Err(BridgeRpcError {
                        code: "SystemStateNotFound".into(),
                        message: format!("System {sys_id} has no sovereignty state"),
                    });
                };
                if let Some(existing_alliance) = st.sov_alliance_id {
                    if existing_alliance != alliance_id {
                        return Err(BridgeRpcError {
                            code: "SovereigntyHeldByAnotherAlliance".into(),
                            message: format!(
                                "System {sys_id} sovereignty is held by alliance {existing_alliance}"
                            ),
                        });
                    }
                }
                if st.last_sov_claim_tick > 0 && effective_tick <= st.last_sov_claim_tick {
                    return Err(BridgeRpcError {
                        code: "SovereigntyClaimCycleActive".into(),
                        message: "Sovereignty index was already updated on the current simulation tick"
                            .into(),
                    });
                }
                st.last_sov_claim_tick = effective_tick;
                st.sov_alliance_id = Some(alliance_id);
                st.military_index = (st.military_index + 1).min(5);
                st.industrial_index = (st.industrial_index + 1).min(5);
                let mil_idx = st.military_index;
                let ind_idx = st.industrial_index;
                let strat_idx = st.strategic_index;

                const MIL_IND_ADM_BONUS: [f64; 6] = [0.0, 0.6, 1.2, 1.7, 2.1, 2.5];
                const STRAT_ADM_BONUS: [f64; 6] = [0.0, 0.4, 0.6, 0.8, 0.9, 1.0];
                let mil_b = MIL_IND_ADM_BONUS[usize::from(mil_idx.min(5))];
                let ind_b = MIL_IND_ADM_BONUS[usize::from(ind_idx.min(5))];
                let strat_b = STRAT_ADM_BONUS[usize::from(strat_idx.min(5))];
                let adm = ((1.0 + mil_b + ind_b + strat_b).clamp(1.0, 6.0) * 10.0).round() / 10.0;
                let vuln_start = now_unix.saturating_add(3_600);
                let vuln_duration = ((18.0 / adm) * 3_600.0).round() as u64;
                let vuln_end = vuln_start.saturating_add(vuln_duration.max(3_600));
                for sid in sov_struct_ids {
                    snap.structure_vulnerability_windows
                        .insert(sid, (vuln_start, vuln_end));
                }

                Ok((
                    alliance_id,
                    primary_hub_id,
                    mil_idx,
                    ind_idx,
                    adm,
                    vuln_start,
                    vuln_end,
                ))
            });

            match res {
                Ok((alliance_id, primary_hub_id, mil_idx, ind_idx, adm, vuln_start, vuln_end)) => {
                    Some(Ok(PyRep::keyval([
                        ("solarSystemID", PyRep::Integer(sys_id as i64)),
                        ("allianceID", PyRep::Integer(alliance_id)),
                        (
                            "infrastructureHubID",
                            primary_hub_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                        ),
                        ("militaryIndex", PyRep::Integer(mil_idx as i64)),
                        ("industrialIndex", PyRep::Integer(ind_idx as i64)),
                        ("vulnerabilityOccupancyLevel", PyRep::Real(adm)),
                        ("vulnerableStartTime", PyRep::Integer(vuln_start as i64)),
                        ("vulnerableEndTime", PyRep::Integer(vuln_end as i64)),
                        ("skyhookOnline", PyRep::Bool(true)),
                    ])))
                }
                Err(e) => Some(Err(e)),
            }
        }

        ("sovMgr", "GetSystemSovereigntyInfo") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .filter(|id| ctx.sde.solar_systems.contains_key(id))
                .or(session.solar_system_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "GetSystemSovereigntyInfo requires a valid solarSystemID argument or active session solar system".into(),
                }));
            };
            let (alliance_id, corp_id, hub_id, claim_id, contested, adm) =
                ctx.db.with_snapshot(|snap| {
                    let st = snap.system_states.get(&sys_id);
                    let aid = st.and_then(|s| s.sov_alliance_id);
                    let exec_corp = aid
                        .and_then(|a| snap.alliances.get(&a))
                        .map(|al| al.executor_corp_id);
                    let mut hub = None;
                    let mut claim = None;
                    for s in snap.structures.values() {
                        if s.solar_system_id == sys_id {
                            if s.type_id == 32458 {
                                hub = Some(s.structure_id);
                            } else if s.type_id == 32226 {
                                claim = Some(s.structure_id);
                            } else if hub.is_none() {
                                hub = Some(s.structure_id);
                            }
                        }
                    }
                    let mil = st.map(|s| s.military_index).unwrap_or(0);
                    let ind = st.map(|s| s.industrial_index).unwrap_or(0);
                    let strat = st.map(|s| s.strategic_index).unwrap_or(0);
                    let adm_val = 1.0 + (mil as f64) * 0.4 + (ind as f64) * 0.4 + (strat as f64) * 0.2;
                    let cont = st.map(|s| s.fw_contested_pct > 0.0).unwrap_or(false);
                    (aid, exec_corp, hub, claim.or(hub), cont, adm_val)
                });
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                (
                    "allianceID",
                    alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                (
                    "corporationID",
                    corp_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                (
                    "claimStructureID",
                    claim_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                (
                    "infrastructureHubID",
                    hub_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                ("contested", PyRep::Integer(if contested { 1 } else { 0 })),
                ("vulnerabilityOccupancyLevel", PyRep::Real(adm)),
            ])))
        }

        ("sovMgr", "GetInfrastructureHubInfo") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .filter(|id| ctx.sde.solar_systems.contains_key(id))
                .or(session.solar_system_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "GetInfrastructureHubInfo requires a valid solarSystemID argument or active session solar system".into(),
                }));
            };
            let (hub_id, owner_corp, alliance_id, mil_idx, ind_idx, strat_idx, prosp_lvl, online) =
                ctx.db.with_snapshot(|snap| {
                    let st = snap.system_states.get(&sys_id);
                    let aid = st.and_then(|s| s.sov_alliance_id);
                    let mil = st.map(|s| s.military_index).unwrap_or(0);
                    let ind = st.map(|s| s.industrial_index).unwrap_or(0);
                    let strat = st.map(|s| s.strategic_index).unwrap_or(0);
                    let lvl = snap.sov_prospecting_levels.get(&sys_id).copied().unwrap_or(0);
                    let struct_rec = snap
                        .structures
                        .values()
                        .find(|s| s.solar_system_id == sys_id && s.type_id == 32458)
                        .or_else(|| {
                            snap.structures
                                .values()
                                .find(|s| s.solar_system_id == sys_id)
                        });
                    let hid = struct_rec.map(|s| s.structure_id);
                    let ocorp = struct_rec.map(|s| s.owner_corp_id);
                    let is_online = struct_rec
                        .is_some_and(|s| s.state == "ShieldVulnerable" && s.fuel_blocks > 0);
                    (hid, ocorp, aid, mil, ind, strat, lvl, is_online)
                });
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("hubID", hub_id.map(PyRep::Integer).unwrap_or(PyRep::None)),
                (
                    "infrastructureHubID",
                    hub_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                (
                    "ownerID",
                    owner_corp.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                (
                    "allianceID",
                    alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                ("militaryIndex", PyRep::Integer(mil_idx as i64)),
                ("industrialIndex", PyRep::Integer(ind_idx as i64)),
                ("strategicIndex", PyRep::Integer(strat_idx as i64)),
                ("prospectingLevel", PyRep::Integer(prosp_lvl as i64)),
                ("online", PyRep::Bool(online)),
            ])))
        }

        ("sovMgr", "GetSovStructuresInfoForSolarSystem") => {
            let _ = match ServerContext::require_character(session) {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            let Some(sys_id) = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .filter(|id| ctx.sde.solar_systems.contains_key(id))
                .or(session.solar_system_id)
            else {
                return Some(Err(BridgeRpcError {
                    code: "NoSolarSystem".into(),
                    message: "GetSovStructuresInfoForSolarSystem requires a valid solarSystemID argument or active session solar system".into(),
                }));
            };
            let (alliance_id, structs_list) = ctx.db.with_snapshot(|snap| {
                let aid = snap
                    .system_states
                    .get(&sys_id)
                    .and_then(|s| s.sov_alliance_id);
                let list: Vec<PyRep> = snap
                    .structures
                    .values()
                    .filter(|s| s.solar_system_id == sys_id)
                    .map(|s| {
                        let vuln = snap
                            .structure_vulnerability_windows
                            .get(&s.structure_id)
                            .copied()
                            .unwrap_or((0, 0));
                        PyRep::keyval([
                            ("itemID", PyRep::Integer(s.structure_id)),
                            ("structureID", PyRep::Integer(s.structure_id)),
                            ("typeID", PyRep::Integer(s.type_id as i64)),
                            ("corporationID", PyRep::Integer(s.owner_corp_id)),
                            (
                                "allianceID",
                                aid.map(PyRep::Integer).unwrap_or(PyRep::None),
                            ),
                            ("solarSystemID", PyRep::Integer(sys_id as i64)),
                            ("vulnerableStartTime", PyRep::Integer(vuln.0 as i64)),
                            ("vulnerableEndTime", PyRep::Integer(vuln.1 as i64)),
                        ])
                    })
                    .collect();
                (aid, list)
            });
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                (
                    "allianceID",
                    alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                ("structures", PyRep::List(structs_list)),
            ])))
        }

        ("sovMgr", "GetSovStructuresInfoForLocalSolarSystem") => {
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
            let (alliance_id, hub_id, prosp_lvl, structs_list) = ctx.db.with_snapshot(|snap| {
                let aid = snap
                    .system_states
                    .get(&sys_id)
                    .and_then(|s| s.sov_alliance_id);
                let lvl = snap.sov_prospecting_levels.get(&sys_id).copied().unwrap_or(0);
                let mut primary_hub = None;
                let list: Vec<PyRep> = snap
                    .structures
                    .values()
                    .filter(|s| s.solar_system_id == sys_id)
                    .map(|s| {
                        if primary_hub.is_none() || s.type_id == 32458 {
                            primary_hub = Some(s.structure_id);
                        }
                        PyRep::keyval([
                            ("itemID", PyRep::Integer(s.structure_id)),
                            ("structureID", PyRep::Integer(s.structure_id)),
                            ("typeID", PyRep::Integer(s.type_id as i64)),
                            ("corporationID", PyRep::Integer(s.owner_corp_id)),
                            ("solarSystemID", PyRep::Integer(sys_id as i64)),
                        ])
                    })
                    .collect();
                (aid, primary_hub, lvl, list)
            });
            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ("local", PyRep::Bool(true)),
                (
                    "allianceID",
                    alliance_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                (
                    "infrastructureHubID",
                    hub_id.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                ("prospectingLevel", PyRep::Integer(prosp_lvl as i64)),
                ("structures", PyRep::List(structs_list)),
            ])))
        }

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

            // Resolve every modelled prospecting deposit up front (client layout + wiki quantities).
            let mut modelled: HashMap<u32, (u8, mining_sites::ResolvedOreSite)> = HashMap::new();
            for def in mining_sites::SOV_PROSPECTING_DEPOSITS {
                match mining_sites::resolve_ore_site(&ctx.sde, &def.site) {
                    Ok(site) => {
                        modelled.insert(def.upgrade_type_id, (def.level, site));
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

                // The upgrade item (SDE group 4768) must be owned by the caller or their corporation (or already
                // installed in the hub), sit in the hub itself or the caller's active ship, and be unlocked.
                let mut found_locked_local = false;
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
                        || it.location_id == hub_id;
                    let colocated =
                        it.location_id == hub_id || active_ship_id == Some(it.location_id);
                    if !owned_or_installed || !colocated {
                        continue;
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
                let Some((level, site)) = modelled.get(&up_tid) else {
                    return Err(BridgeRpcError {
                        code: "ProspectingDepositNotModelled".into(),
                        message: format!(
                            "Sovereignty Hub upgrade type {up_tid} has no modelled prospecting deposit"
                        ),
                    });
                };

                if let Some(up_it) = snap.items.get_mut(&up_iid) {
                    up_it.location_id = hub_id;
                    up_it.flag = eve_db::InventoryFlag::SubSystemSlot0;
                }
                snap.sov_prospecting_levels.insert(sys_id, u32::from(*level));
                if let Some(st) = snap.system_states.get_mut(&sys_id) {
                    st.industrial_index = st.industrial_index.max(*level);
                }

                // Idempotent across repeat calls: an uncleared SovOre deposit already in sys_id is reused.
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
                if !existing_sov_anoms.is_empty() {
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
                let handle = ctx.systems.get_or_spawn(sys_id);
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
                    let _ = handle.spawn_asteroid(st, ball).await;
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
            let arg_hub_id = args.first().and_then(|a| a.as_i64());
            let (hub_id, lvl, sov_anoms, installed) = ctx.db.with_snapshot(|snap| {
                let resolved_hub = arg_hub_id.or_else(|| {
                    snap.structures
                        .values()
                        .filter(|s| s.solar_system_id == sys_id)
                        .map(|s| s.structure_id)
                        .min()
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
            let handle = ctx.systems.get_or_spawn(sys_id);
            let live_balls = handle.list_balls().await.unwrap_or_default();

            let (anom_list, sig_list, gas_npc_ball_id) = ctx.db.with_snapshot(|snap| {
                let mut found_gas_npc_id: Option<i64> = None;

                let anoms: Vec<PyRep> = snap
                    .cosmic_anomalies
                    .get(&sys_id)
                    .map(|list| {
                        list.iter()
                            .filter(|a| !a.cleared)
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
                                let revealed = s.signal_strength >= 1.0;
                                let pos_known = s.signal_strength >= 0.25;
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
                                    ("signalStrength", PyRep::Real(s.signal_strength)),
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

        ("scanSvc", "RequestScans") => {
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
            seed_system_mining_sites_from_sde(ctx, sys_id).await;

            let mut probes: Vec<(DVec3, f64)> = Vec::new();
            if let Some(PyRep::List(probe_list)) = args.first() {
                for p in probe_list {
                    if let PyRep::Tuple(coords) | PyRep::List(coords) = p {
                        if coords.len() >= 4 {
                            let px = coords[0].as_f64().unwrap_or(0.0);
                            let py = coords[1].as_f64().unwrap_or(0.0);
                            let pz = coords[2].as_f64().unwrap_or(0.0);
                            let pr = coords[3].as_f64().unwrap_or(AU_METERS * 4.0);
                            probes.push((DVec3::new(px, py, pz), pr));
                        }
                    } else if let Some(px) = p.dict_get("x").and_then(|v| v.as_f64()) {
                        let py = p.dict_get("y").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let pz = p.dict_get("z").and_then(|v| v.as_f64()).unwrap_or(0.0);
                        let pr = p
                            .dict_get("radius")
                            .and_then(|v| v.as_f64())
                            .unwrap_or(AU_METERS * 4.0);
                        probes.push((DVec3::new(px, py, pz), pr));
                    }
                }
            }

            let rows = ctx.db.with_mut_snapshot(|snap| {
                let sigs = snap.cosmic_signatures.entry(sys_id).or_default();
                let mut out_rows = Vec::new();
                for s in sigs.iter_mut() {
                    if s.collapsed || matches!(s.sig_group.as_str(), "OreAnomaly" | "SovOre") {
                        continue;
                    }
                    let sig_pos = DVec3::from_array(s.position);
                    if !probes.is_empty() {
                        let mut covering_probes = 0usize;
                        let mut total_contrib = 0.0_f64;
                        for &(p_pos, radius) in &probes {
                            let dist = sig_pos.distance(p_pos);
                            if dist <= radius * 2.0 {
                                covering_probes += 1;
                                let dist_factor = (1.0 - (dist / (radius * 2.0))).clamp(0.05, 1.0);
                                let radius_factor = ((8.0 * AU_METERS)
                                    / radius.max(0.25 * AU_METERS))
                                .clamp(0.15, 2.5);
                                total_contrib += 0.32 * dist_factor * radius_factor;
                            }
                        }
                        let tri_mult = if covering_probes >= 4 {
                            1.0
                        } else {
                            (covering_probes as f64) * 0.18
                        };
                        let computed = (total_contrib * tri_mult).clamp(0.0, 1.0);
                        if computed > s.signal_strength {
                            s.signal_strength = computed;
                        }
                    }

                    let revealed = s.signal_strength >= 1.0;
                    out_rows.push(vec![
                        PyRep::String(s.signature_id.clone()),
                        PyRep::String(s.signature_id.clone()),
                        PyRep::String(s.sig_group.clone()),
                        if revealed {
                            PyRep::String(s.sig_group.clone())
                        } else {
                            PyRep::String("Unknown".into())
                        },
                        if revealed {
                            PyRep::String(s.type_name.clone())
                        } else {
                            PyRep::String("Unidentified Cosmic Signature".into())
                        },
                        PyRep::Real(s.signal_strength),
                        if s.signal_strength >= 0.25 {
                            PyRep::Real(s.position[0])
                        } else {
                            PyRep::None
                        },
                        if s.signal_strength >= 0.25 {
                            PyRep::Real(s.position[1])
                        } else {
                            PyRep::None
                        },
                        if s.signal_strength >= 0.25 {
                            PyRep::Real(s.position[2])
                        } else {
                            PyRep::None
                        },
                    ]);
                }
                out_rows
            });

            Some(Ok(PyRep::rowset(
                vec![
                    "id",
                    "signatureID",
                    "sigGroup",
                    "siteType",
                    "siteName",
                    "signalStrength",
                    "x",
                    "y",
                    "z",
                ],
                rows,
            )))
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
