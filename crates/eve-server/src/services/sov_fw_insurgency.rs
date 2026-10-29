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

pub(crate) const AU_METERS: f64 = 149_597_870_700.0;


#[path = "fw_insurgency.rs"]
pub mod fw_insurgency;
pub use fw_insurgency::*;

pub fn derive_system_site_position_from_sde(ctx: &ServerContext, sys_id: u32, slot: usize, au_scale: f64) -> [f64; 3] {
    let mut cels = ctx.sde.celestials_in_system(sys_id);
    cels.sort_by_key(|c| c.celestial_id);
    let base_dir = if !cels.is_empty() {
        let v = cels[slot % cels.len()].position;
        if v.length_squared() > 1.0 { v.normalize() } else { DVec3::new(0.6, 0.4, -0.5).normalize() }
    } else {
        DVec3::new(0.6, 0.4, -0.5).normalize()
    };
    let offset_dir = match slot % 4 {
        0 => DVec3::new(base_dir.x, base_dir.y, base_dir.z),
        1 => DVec3::new(-base_dir.y, base_dir.x, base_dir.z * 0.8),
        2 => DVec3::new(base_dir.z, -base_dir.x, base_dir.y),
        _ => DVec3::new(-base_dir.x, -base_dir.z, base_dir.y),
    }.normalize_or_zero();
    let dir = if offset_dir.length_squared() > 0.1 { offset_dir } else { DVec3::X };
    let pos = dir * (au_scale * AU_METERS);
    [pos.x, pos.y, pos.z]
}

/// Evaluates server-clock respawn timers for depleted Sovereignty Hub prospecting deposits in `sys_id`.
pub async fn tick_sov_prospecting_deposit_respawn(ctx: &ServerContext, sys_id: u32) {
    let has_sov_anom = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies.get(&sys_id).is_some_and(|list| list.iter().any(|a| a.anomaly_type == "SovOre"))
    });
    if !has_sov_anom { return; }

    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let sov_asteroid_ids: Vec<i64> = ctx.db.with_snapshot(|snap| {
        snap.asteroids.values().filter(|a| a.solar_system_id == sys_id && a.belt_id == 0).map(|a| a.asteroid_id).collect()
    });
    let mut live_ast_states = Vec::with_capacity(sov_asteroid_ids.len());
    for aid in sov_asteroid_ids {
        if let Ok(Some(st)) = sys_handle.get_asteroid_state(aid).await {
            live_ast_states.push((aid, if st.depleted { 0_i64 } else { st.remaining_units as i64 }));
        }
    }

    let mut modelled: HashMap<TypeId, (u64, mining_sites::ResolvedOreSite)> = HashMap::new();
    for def in mining_sites::SOV_PROSPECTING_DEPOSITS {
        if let Ok(site) = mining_sites::resolve_ore_site(&ctx.sde, &def.site) {
            if site.asteroids.iter().all(|a| ctx.sde.types.contains_key(&a.type_id)) {
                modelled.insert(def.upgrade_type_id, (def.respawn_seconds, site));
            }
        }
    }
    let type_vol_rad = |tid: u32| -> (f64, f64) {
        ctx.sde.types.get(&tid).map(|t| (t.volume.max(0.001), t.radius.max(50.0))).unwrap_or((0.001, 50.0))
    };
    let sov_pos = derive_system_site_position_from_sde(ctx, sys_id, 3, 5.0);
    let now = ctx.now_unix();

    let (newly_spawned, cleared_anom_ids, added_anom_opt, added_sig_opt) = ctx.db.with_mut_snapshot(|snap| {
        for &(aid, rem) in &live_ast_states {
            if let Some(ast) = snap.asteroids.get_mut(&aid) { ast.quantity = rem; }
        }
        let installed_upgrade = snap.structures.values().filter(|s| s.solar_system_id == sys_id).find_map(|s| {
            snap.items.values().find_map(|it| {
                if it.location_id == s.structure_id && it.flag == eve_db::InventoryFlag::SubSystemSlot0 {
                    modelled.get(&it.type_id)
                } else {
                    None
                }
            })
        });
        let default_respawn_secs = installed_upgrade.map(|(r, _)| *r).unwrap_or(36_000);

        let mut newly_cleared_anom_ids = Vec::new();
        if let Some(anoms) = snap.cosmic_anomalies.get_mut(&sys_id) {
            for anom in anoms.iter_mut().filter(|a| a.anomaly_type == "SovOre") {
                let anom_pos = DVec3::from_array(anom.position);
                let any_remaining = snap.asteroids.values().any(|a| {
                    a.solar_system_id == sys_id
                        && a.quantity > 0
                        && DVec3::new(a.position.0, a.position.1, a.position.2).distance(anom_pos)
                            <= 50_000.0
                });
                if !any_remaining {
                    if !anom.cleared {
                        anom.cleared = true;
                        anom.remaining_volume = 0.0;
                        anom.created_unix = now;
                        anom.lifetime_seconds = default_respawn_secs;
                        newly_cleared_anom_ids.push(anom.anomaly_id.clone());
                    } else if anom.lifetime_seconds == 0 {
                        anom.created_unix = now;
                        anom.lifetime_seconds = default_respawn_secs;
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

        let Some((respawn_secs, site)) = installed_upgrade else {
            return (Vec::new(), newly_cleared_anom_ids, None, None);
        };
        let has_uncleared = snap
            .cosmic_anomalies
            .get(&sys_id)
            .is_some_and(|list| list.iter().any(|a| a.anomaly_type == "SovOre" && !a.cleared));
        let ready_to_respawn = !has_uncleared
            && snap.cosmic_anomalies.get(&sys_id).is_some_and(|list| {
                list.iter().any(|a| {
                    a.anomaly_type == "SovOre"
                        && a.cleared
                        && now >= a.created_unix.saturating_add(a.lifetime_seconds.max(*respawn_secs))
                })
            });
        if !ready_to_respawn {
            return (Vec::new(), newly_cleared_anom_ids, None, None);
        }

        let sov_pos_vec = DVec3::from_array(sov_pos);
        snap.asteroids.retain(|_, a| {
            !(a.solar_system_id == sys_id
                && a.belt_id == 0
                && DVec3::new(a.position.0, a.position.1, a.position.2).distance(sov_pos_vec)
                    <= 50_000.0)
        });
        if let Some(anoms) = snap.cosmic_anomalies.get_mut(&sys_id) {
            anoms.retain(|a| a.anomaly_type != "SovOre");
        }
        if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
            sigs.retain(|s| s.sig_group != "SovOre");
        }

        snap.next_entity_id += 1;
        let anom_id = format!("DNG-{}-{}", site.dungeon_id, snap.next_entity_id);
        let mut volume = 0.0_f64;
        let mut spawned = Vec::with_capacity(site.asteroids.len());
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
            spawned.push((aid, a.type_id, a.units as u64, uvol, rad, DVec3::from_array(pos)));
        }
        let new_anom = CosmicAnomalyRecord {
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
        };
        let new_sig = CosmicSignatureRecord {
            signature_id: anom_id,
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
        };
        snap.cosmic_anomalies.entry(sys_id).or_default().push(new_anom.clone());
        snap.cosmic_signatures.entry(sys_id).or_default().push(new_sig.clone());
        (spawned, newly_cleared_anom_ids, Some(new_anom), Some(new_sig))
    });
    let added_anoms: Vec<CosmicAnomalyRecord> = added_anom_opt.into_iter().collect();
    let added_sigs: Vec<CosmicSignatureRecord> = added_sig_opt.into_iter().collect();
    crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_anomaly_delta(
        ctx, sys_id, &added_anoms, &cleared_anom_ids,
    );
    crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(
        ctx, sys_id, &added_sigs, &cleared_anom_ids,
    );

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

    tick_sov_prospecting_deposit_respawn(ctx, sys_id).await;

    let Some(sys) = ctx.sde.solar_systems.get(&sys_id) else {
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
        .filter(|d| d.band.matches_system(sys))
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
        d.band.matches_system(sys)
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
    if let Some(res) = crate::services::fw_enlistment::handle(ctx, session, service, method, args) {
        return Some(res);
    }
    if let Some(res) = crate::services::sov_services::handle(ctx, session, service, method, args, kwargs).await {
        return Some(res);
    }
    if let Some(res) = crate::services::fw_services::handle(ctx, session, service, method, args, kwargs).await {
        return Some(res);
    }
    if let Some(res) = crate::services::insurgency_services::handle(ctx, session, service, method, args, kwargs).await {
        return Some(res);
    }
    if let Some(res) = crate::services::sov_warzone_dynamic::handle(ctx, session, service, method, args, kwargs).await {
        return Some(res);
    }
    match (service, method) {
        ("facWarMgr", "JoinFactionAsCharacter") => {
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
            // Militia factions are derived from the SDE (factions with a militia corporation).
            if !crate::services::fw_enlistment::is_militia_faction(ctx, faction_id) {
                return Some(Err(BridgeRpcError {
                    code: "InvalidWarFaction".into(),
                    message: format!("Faction {faction_id} is not a valid FW/Insurgency militia faction"),
                }));
            }
            let militia_corp_opt = ctx
                .sde
                .factions
                .get(&(faction_id as u32))
                .and_then(|f| f.militia_corporation_id)
                .map(i64::from);
            let now_u = ctx.now_unix();
            let res = ctx.db.try_with_mut_snapshot(|snap| {
                let store = crate::services::fw_enlistment::load_store(snap);
                if let Some(&until) = store.cooldown_until.get(&char_id) {
                    if now_u < until {
                        return Err(BridgeRpcError {
                            code: "FwEnlistmentCooldown".into(),
                            message: "Direct enlistment cooldown is still active".into(),
                        });
                    }
                }
                let Some(ch) = snap.characters.get(&char_id) else {
                    return Err(BridgeRpcError {
                        code: "CharacterNotFound".into(),
                        message: "Character not found".into(),
                    });
                };
                let old_corp_id = ch.corporation_id;
                let in_player_corp = snap.corporations.contains_key(&old_corp_id);
                if ch.war_faction_id.is_some() {
                    return Err(BridgeRpcError {
                        code: "FwAlreadyEnlisted".into(),
                        message: "Character is already enlisted in Factional Warfare".into(),
                    });
                }
                if ch.standings.get(&faction_id).copied().unwrap_or(0.0) < 0.0 {
                    return Err(BridgeRpcError {
                        code: "FwStandingTooLow".into(),
                        message: "Standing with militia faction is below 0.0".into(),
                    });
                }
                if let Some(allowed) = store.corp_allowed.get(&old_corp_id) {
                    if !allowed.contains(&faction_id) {
                        return Err(BridgeRpcError {
                            code: "FwFactionNotAllowedByCorp".into(),
                            message: "Corporation does not allow direct enlistment into this faction".into(),
                        });
                    }
                }
                let mut new_corp_opt = None;
                if let Some(ch) = snap.characters.get_mut(&char_id) {
                    ch.war_faction_id = Some(faction_id);
                    if !in_player_corp {
                        if let Some(militia_corp_id) = militia_corp_opt {
                            if militia_corp_id != old_corp_id {
                                ch.corporation_id = militia_corp_id;
                                new_corp_opt = Some((old_corp_id, militia_corp_id));
                            }
                        }
                    }
                    ch.version += 1;
                }
                if let Some((old_cid, new_cid)) = new_corp_opt {
                    crate::services::fleet_social::fleet_social_corps::record_corp_join_in_snap_pub(
                        snap,
                        char_id,
                        old_cid,
                        new_cid,
                        now_u,
                    );
                }
                let rec = snap.fw_character_stats.entry(char_id).or_insert_with(|| {
                    eve_db::snapshot_ext::FwCharacterStatsRecord {
                        character_id: char_id,
                        faction_id: Some(faction_id),
                        enlisted_on_unix: Some(now_u),
                        current_rank: 1,
                        highest_rank: 1,
                        kills: Default::default(),
                        victory_points: Default::default(),
                    }
                });
                rec.faction_id = Some(faction_id);
                rec.enlisted_on_unix = Some(now_u);
                Ok(new_corp_opt)
            });
            let new_corp_opt = match res {
                Ok(v) => v,
                Err(e) => return Some(Err(e)),
            };
            if let Some((_, new_cid)) = new_corp_opt {
                session.corporation_id = Some(new_cid);
            }
            session.war_faction_id = Some(faction_id);
            Some(Ok(PyRep::keyval([
                ("factionID", PyRep::Integer(faction_id)),
                ("enlisted", PyRep::Bool(true)),
            ])))
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
            let sys_id = args
                .first()
                .and_then(|a| a.as_i64())
                .and_then(|v| u32::try_from(v).ok())
                .filter(|id| ctx.sde.solar_systems.contains_key(id))
                .or(session.solar_system_id);
            let Some(sys_id) = sys_id else {
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

            // Proximity check for compatibility with space proximity test if in space
            if let Some(ship_id) = session.ship_id {
                let sys_handle = ctx.systems.get_or_spawn(sys_id);
                if let Ok(Some((_, ship_ball))) = sys_handle.get_ball_snapshot(ship_id).await {
                    let friendly_structs: Vec<[f64; 3]> = ctx.db.with_snapshot(|snap| {
                        let caller_corp_id = snap
                            .characters
                            .get(&char_id)
                            .map(|c| c.corporation_id)
                            .unwrap_or(0);
                        let caller_alliance_id = snap
                            .characters
                            .get(&char_id)
                            .and_then(|c| c.alliance_id)
                            .or(session.alliance_id);
                        snap.structures
                            .values()
                            .filter(|s| {
                                s.solar_system_id == sys_id
                                    && (s.owner_corp_id == caller_corp_id
                                        || (caller_alliance_id.is_some()
                                            && snap
                                                .corporations
                                                .get(&s.owner_corp_id)
                                                .and_then(|c| c.alliance_id)
                                                == caller_alliance_id))
                                    && s.state == "ShieldVulnerable"
                            })
                            .map(|s| s.position)
                            .collect()
                    });
                    if !friendly_structs.is_empty() {
                        let min_dist = friendly_structs
                            .iter()
                            .map(|pos| ship_ball.position.distance(glam::DVec3::from_array(*pos)))
                            .fold(f64::INFINITY, f64::min);
                        if min_dist > 50_000.0 {
                            return Some(Err(BridgeRpcError {
                                code: "OutsideSovStructureProximity".into(),
                                message: format!(
                                    "Ship is {:.1} m from nearest friendly sovereignty structure (must be <= 50000 m)",
                                    min_dist
                                ),
                            }));
                        }
                    }
                }
            }

            // Strictly read-only: return skyhook records for system, zero mutation of sovereignty owner or indices
            let (aid, skyhooks_list) = ctx.db.with_snapshot(|snap| {
                let aid = snap
                    .system_states
                    .get(&sys_id)
                    .and_then(|s| s.sov_alliance_id);
                let planet_ids = ctx
                    .sde
                    .solar_systems
                    .get(&sys_id)
                    .map(|info| info.planet_ids.clone())
                    .unwrap_or_default();
                let default_planet_id = planet_ids.first().copied().unwrap_or(0);

                let list: Vec<PyRep> = snap
                    .structures
                    .values()
                    .filter(|s| s.solar_system_id == sys_id && (s.type_id == 81080 || s.type_id == 32458 || s.type_id == 32226))
                    .map(|s| {
                        let reagents = snap
                            .sov_runtime
                            .skyhook_reagents
                            .get(&s.structure_id)
                            .copied()
                            .unwrap_or((0, 0));
                        let struct_ally = snap
                            .corporations
                            .get(&s.owner_corp_id)
                            .and_then(|c| c.alliance_id)
                            .or(aid);
                        PyRep::keyval([
                            ("skyhookID", PyRep::Integer(s.structure_id)),
                            ("structureID", PyRep::Integer(s.structure_id)),
                            ("planetID", PyRep::Integer(default_planet_id)),
                            ("solarSystemID", PyRep::Integer(sys_id as i64)),
                            ("typeID", PyRep::Integer(s.type_id as i64)),
                            ("ownerID", PyRep::Integer(s.owner_corp_id)),
                            ("corporationID", PyRep::Integer(s.owner_corp_id)),
                            (
                                "allianceID",
                                struct_ally.map(PyRep::Integer).unwrap_or(PyRep::None),
                            ),
                            ("state", PyRep::String(s.state.clone())),
                            ("reagentTypeID", PyRep::Integer(i64::from(reagents.0))),
                            ("reagentQuantity", PyRep::Integer(reagents.1 as i64)),
                        ])
                    })
                    .collect();
                (aid, list)
            });

            let first_skyhook = skyhooks_list.first();

            Some(Ok(PyRep::keyval([
                ("solarSystemID", PyRep::Integer(sys_id as i64)),
                (
                    "allianceID",
                    aid.map(PyRep::Integer).unwrap_or(PyRep::None),
                ),
                ("skyhooks", PyRep::List(skyhooks_list.clone())),
                (
                    "skyhookID",
                    first_skyhook
                        .and_then(|k| k.dict_get("skyhookID"))
                        .cloned()
                        .unwrap_or(PyRep::None),
                ),
                (
                    "planetID",
                    first_skyhook
                        .and_then(|k| k.dict_get("planetID"))
                        .cloned()
                        .unwrap_or(PyRep::None),
                ),
                (
                    "typeID",
                    first_skyhook
                        .and_then(|k| k.dict_get("typeID"))
                        .cloned()
                        .unwrap_or(PyRep::None),
                ),
                (
                    "ownerID",
                    first_skyhook
                        .and_then(|k| k.dict_get("ownerID"))
                        .cloned()
                        .unwrap_or(PyRep::None),
                ),
                (
                    "reagentTypeID",
                    first_skyhook
                        .and_then(|k| k.dict_get("reagentTypeID"))
                        .cloned()
                        .unwrap_or(PyRep::Integer(0)),
                ),
                (
                    "reagentQuantity",
                    first_skyhook
                        .and_then(|k| k.dict_get("reagentQuantity"))
                        .cloned()
                        .unwrap_or(PyRep::Integer(0)),
                ),
                ("skyhookOnline", PyRep::Bool(!skyhooks_list.is_empty())),
            ])))
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
                    .filter(|s| {
                        s.solar_system_id == sys_id
                            && (s.type_id == 32226 || s.type_id == 32458)
                    })
                    .map(|s| {
                        if s.type_id == 32458 {
                            primary_hub = Some(s.structure_id);
                        }
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
                            ("campaignState", PyRep::None),
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

        _ => fw_insurgency::handle_hub_and_scan(ctx, session, service, method, args, kwargs).await,
    }
}
