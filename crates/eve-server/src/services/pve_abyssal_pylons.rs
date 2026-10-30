//! `pve_abyssal_pylons` — Abyssal automated pylons (V24 SDE Suppressors & Multibody Tracking Pylons),
//! per-ship spherical 75 km room boundary enforcement, full capsule destruction, and fitted ship kinematics.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_abyssal_hazards::{
    cloud_modifiers_for_type, is_spatial_hazard_cloud_type, resolve_character_home_station_and_system,
    spatial_cloud_radius_m, ABYSSAL_ROOM_BOUNDARY_RADIUS_M, AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID,
    AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID, AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID,
    MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID, MULTIBODY_TRACKING_PYLON_TYPE_ID,
};
use crate::services::pve_special_sites::with_pve_special_reg;
use eve_sde::{attr_ids, SolarSystemId, TypeId};
use glam::DVec3;
use std::collections::{HashMap, HashSet};

type CharacterId = i64;
type ItemId = i64;

/// Builds a space `Ball` for `ship_id` using fitted Dogma kinematics (`maxVelocity` `37`, `agility` `70`,
/// `mass` `4`, `radius` `162`) with V24 SDE hull fallbacks.
pub fn build_fitted_ship_space_ball(
    ctx: &ServerContext,
    char_id: CharacterId,
    ship_id: ItemId,
    ship_type_id: TypeId,
    position: DVec3,
) -> eve_destiny::Ball {
    let sde_type = ctx.sde.types.get(&ship_type_id);
    let fit_opt = ctx.get_or_rebuild_ship_fitting(char_id, ship_id).ok();
    let max_velocity = fit_opt
        .as_ref()
        .map(|f| f.ship.get_attribute(attr_ids::MAX_VELOCITY, &ctx.sde))
        .filter(|&v| v > 0.0)
        .or_else(|| {
            sde_type
                .and_then(|t| t.default_attributes.get(&attr_ids::MAX_VELOCITY).copied())
                .filter(|&v| v > 0.0)
        })
        .unwrap_or(250.0);
    let inertia = fit_opt
        .as_ref()
        .map(|f| f.ship.get_attribute(attr_ids::AGILITY, &ctx.sde))
        .filter(|&v| v > 0.0)
        .or_else(|| {
            sde_type
                .and_then(|t| t.default_attributes.get(&attr_ids::AGILITY).copied())
                .filter(|&v| v > 0.0)
        })
        .unwrap_or(0.5);
    let mass = fit_opt
        .as_ref()
        .map(|f| f.ship.get_attribute(attr_ids::MASS, &ctx.sde))
        .filter(|&v| v > 0.0)
        .or_else(|| sde_type.map(|t| t.mass).filter(|&m| m > 0.0))
        .unwrap_or(1_000_000.0);
    let radius = fit_opt
        .as_ref()
        .map(|f| f.ship.get_attribute(162, &ctx.sde))
        .filter(|&v| v > 0.0)
        .or_else(|| sde_type.map(|t| t.radius).filter(|&r| r > 0.0))
        .unwrap_or(50.0);
    eve_destiny::Ball {
        id: ship_id,
        type_id: ship_type_id,
        position,
        velocity: DVec3::ZERO,
        max_velocity,
        inertia,
        mass,
        radius,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    }
}

/// Resolves V24 SDE `maxRange` (`attr 54`) for Deviant Automata Suppressors (`47437` = 15 km, `47438` = 40 km, `48254` = 40 km).
pub fn suppressor_radius_m(
    sde_opt: Option<&eve_sde::SdeRegistry>,
    suppressor_tid: TypeId,
    fallback_radius: f64,
) -> f64 {
    if let Some(r) = sde_opt
        .and_then(|s| s.types.get(&suppressor_tid))
        .and_then(|t| t.default_attributes.get(&54).copied())
        .filter(|&r| r > 0.0)
    {
        return r;
    }
    match suppressor_tid {
        AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID => 15_000.0,
        AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID | AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID => 40_000.0,
        _ => fallback_radius.max(15_000.0),
    }
}

/// Resolves V24 SDE pulse damage (`attr 114/116/117/118`) for Deviant Automata Suppressors.
pub fn suppressor_pulse_damage(
    sde_opt: Option<&eve_sde::SdeRegistry>,
    suppressor_tid: TypeId,
) -> eve_destiny::DamageVector {
    if let Some(t) = sde_opt.and_then(|s| s.types.get(&suppressor_tid)) {
        let em = t.default_attributes.get(&114).copied().unwrap_or(0.0);
        let ex = t.default_attributes.get(&116).copied().unwrap_or(0.0);
        let ki = t.default_attributes.get(&117).copied().unwrap_or(0.0);
        let th = t.default_attributes.get(&118).copied().unwrap_or(0.0);
        if em + ex + ki + th > 0.0 {
            return eve_destiny::DamageVector {
                em,
                thermal: th,
                kinetic: ki,
                explosive: ex,
            };
        }
    }
    eve_destiny::DamageVector {
        em: 0.0,
        thermal: 45.0,
        kinetic: 0.0,
        explosive: 45.0,
    }
}

/// Resolves V24 SDE `maxRange` (`attr 54`) and `warfareBuff1Value` (`attr 2469`) tracking multiplier
/// (`47469` Short-Range = `15,000` m `1.8x`, `47470` Medium-Range = `40,000` m `1.6x`).
pub fn tracking_pylon_radius_and_mult(
    sde_opt: Option<&eve_sde::SdeRegistry>,
    pylon_tid: TypeId,
    fallback_radius: f64,
) -> (f64, f64) {
    let sde_t = sde_opt.and_then(|s| s.types.get(&pylon_tid));
    let radius = sde_t
        .and_then(|t| t.default_attributes.get(&54).copied())
        .filter(|&r| r > 0.0)
        .unwrap_or_else(|| match pylon_tid {
            MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID => 40_000.0,
            MULTIBODY_TRACKING_PYLON_TYPE_ID => 15_000.0,
            _ => fallback_radius.max(15_000.0),
        });
    let bonus_pct = sde_t
        .and_then(|t| t.default_attributes.get(&2469).copied())
        .filter(|&b| b > 0.0)
        .unwrap_or(if pylon_tid == MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID {
            60.0
        } else {
            80.0
        });
    (radius, 1.0 + bonus_pct / 100.0)
}

/// V24 SDE `typeList 46` (`AoE Point Defense Targets`) with stationary Sentry Drone (`attr 37 < 1.0`)
/// and Tessera cruiser (`48262..=48265`) exclusions (`BF-04`).
pub fn is_suppressor_target_type(t: &eve_sde::InvType) -> bool {
    let is_sentry = t.category_id == 18
        && (t.default_attributes.get(&37).is_some_and(|&v| v < 1.0) || t.type_id == 60_481);
    if is_sentry || matches!(t.type_id, 48_262..=48_265) {
        return false;
    }
    matches!(t.category_id, 8 | 18 | 87) || matches!(t.group_id, 1997 | 4037 | 297)
}

/// Player ships (`category 6`), Sentry Drones (`group 100`), and NPC combat ships/entities (`category 11`,
/// excluding clouds/pylons/traces/gates/caches) receive the Multibody Tracking Pylon aura (`BF-04`).
pub fn is_tracking_pylon_beneficiary(t: &eve_sde::InvType) -> bool {
    if t.category_id == 6 || t.group_id == 100 {
        return true;
    }
    t.category_id == 11
        && !matches!(t.group_id, 1971 | 1981 | 1991 | 4081)
        && !matches!(t.type_id, 47_685 | 47_686 | 47_951 | 49_661 | 49_662)
}

/// Destroys a pilot's capsule (`670`), removes all plugged-in `InventoryFlag::Implant` items, clears
/// `active_implants`, spawns a `Corpse` (`type_id = 25`) in `death_sys`, records `KillmailRecord` +
/// `DetailedKillmail`, and re-homes the pilot into their race rookie corvette (`601 / 588 / 596 / 606`).
pub fn destroy_pod_and_respawn_faction_corvette_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    sde_opt: Option<&eve_sde::SdeRegistry>,
    cid: CharacterId,
    death_sys: SolarSystemId,
    death_pos: [f64; 3],
    now_unix: u64,
) -> (i64, SolarSystemId, ItemId, ItemId, i64) {
    let mut imp_tids = snap.active_implants.remove(&cid).unwrap_or_default();
    let imp_items: Vec<(ItemId, TypeId)> = snap
        .items
        .values()
        .filter(|i| (i.location_id == cid || i.owner_id == cid) && i.flag == eve_db::InventoryFlag::Implant)
        .map(|i| (i.item_id, i.type_id))
        .collect();
    for (iid, tid) in imp_items {
        snap.items.remove(&iid);
        if !imp_tids.contains(&tid) {
            imp_tids.push(tid);
        }
    }
    let mut items_isk = 0.0;
    let mut destroyed_entries = Vec::new();
    for imp_tid in imp_tids {
        items_isk += sde_opt
            .and_then(|sde| sde.types.get(&imp_tid))
            .map(|t| t.base_price)
            .filter(|&p| p > 0.0)
            .unwrap_or(1_000.0);
        destroyed_entries.push(eve_db::KillmailItemEntry {
            type_id: imp_tid,
            flag: eve_db::InventoryFlag::Implant as u16,
            qty_dropped: 0,
            qty_destroyed: 1,
            singleton: true,
        });
    }

    snap.next_entity_id += 1;
    let corpse_id = snap.next_entity_id;
    snap.items.insert(
        corpse_id,
        eve_db::InventoryItemRecord {
            item_id: corpse_id,
            type_id: 25,
            owner_id: cid,
            location_id: death_sys as i64,
            flag: eve_db::InventoryFlag::None,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        },
    );
    snap.item_positions.insert(corpse_id, death_pos);

    let (home_station_id, home_sys_id, const_id, reg_id) = sde_opt
        .map(|sde| resolve_character_home_station_and_system(snap, sde, cid))
        .unwrap_or((60_003_760, 30_000_142, 20_000_020, 10_000_002));
    let race_id = snap.characters.get(&cid).map(|c| c.race_id).unwrap_or(8);
    let corvette_type_id = sde_opt
        .and_then(|sde| sde.race(race_id))
        .and_then(|r| r.corvette_type_id)
        .unwrap_or(match race_id {
            1 => 601,
            2 => 588,
            4 => 596,
            _ => 606,
        });

    snap.next_entity_id += 1;
    let corvette_id = snap.next_entity_id;
    snap.items.insert(
        corvette_id,
        eve_db::InventoryItemRecord {
            item_id: corvette_id,
            type_id: corvette_type_id,
            owner_id: cid,
            location_id: home_station_id,
            flag: eve_db::InventoryFlag::Hangar,
            quantity: 1,
            singleton: true,
            version: 1,
            ..Default::default()
        },
    );
    let (vic_corp, vic_all) = snap
        .characters
        .get_mut(&cid)
        .map(|ch| {
            ch.active_ship_id = corvette_id;
            ch.station_id = Some(home_station_id);
            ch.structure_id = None;
            ch.solar_system_id = home_sys_id;
            ch.constellation_id = const_id;
            ch.region_id = reg_id;
            ch.version += 1;
            ch.system_epoch += 1;
            (ch.corporation_id, ch.alliance_id)
        })
        .unwrap_or((1_000_009, None));

    snap.next_entity_id += 1;
    let pod_kid = snap.next_entity_id;
    let pod_total_isk = 1_000.0 + items_isk;
    snap.record_killmail(
        eve_db::KillmailRecord {
            kill_id: pod_kid,
            victim_character_id: cid,
            attacker_character_id: 0,
            attacker_corp_id: 1_000_298,
            solar_system_id: death_sys,
            ship_type_id: 670,
            total_value_isk: pod_total_isk,
            ..Default::default()
        },
        now_unix,
    );
    let km_hash = crate::services::combat_crimewatch::compute_killmail_esi_hash(cid, 0, 670, now_unix);
    snap.combat_crimewatch_state.detailed_killmails.insert(
        pod_kid,
        eve_db::DetailedKillmail {
            kill_id: pod_kid,
            kill_time_unix: now_unix,
            solar_system_id: death_sys,
            victim_character_id: cid,
            victim_corporation_id: vic_corp,
            victim_alliance_id: vic_all,
            victim_ship_type_id: 670,
            victim_damage_taken: 100.0,
            victim_position: death_pos,
            attackers: vec![eve_db::KillmailAttackerEntry {
                character_id: 0,
                corporation_id: 1_000_298,
                alliance_id: None,
                ship_type_id: 0,
                weapon_type_id: 0,
                damage_done: 100.0,
                final_blow: true,
                security_status: 0.0,
            }],
            dropped_items: Vec::new(),
            destroyed_items: destroyed_entries,
            total_value_isk: pod_total_isk,
            killmail_hash: km_hash,
        },
    );
    (home_station_id, home_sys_id, corvette_id, corpse_id, pod_kid)
}

/// Pulses automated pylons in `sys_id`, applying V24 SDE Automata Suppressor AoE damage and Multibody Tracking Pylon aura.
pub async fn pulse_abyssal_automated_pylons(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
) -> (usize, usize) {
    let pocket_by_ship: HashMap<i64, i64> = with_pve_special_reg(ctx, |reg| {
        let mut map = HashMap::new();
        for p in reg.abyssal_pockets.values().filter(|p| !p.completed && !p.collapsed_destroyed) {
            map.insert(p.ship_id, p.pocket_id);
            for &sid in &p.fleet_ship_ids {
                map.insert(sid, p.pocket_id);
            }
            for r in &p.rooms {
                for &nid in &r.npc_ball_ids {
                    map.insert(nid, p.pocket_id);
                }
            }
        }
        map
    });
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let balls = sys_handle.list_balls().await.unwrap_or_default();
    let mut suppressors: Vec<(Option<i64>, DVec3, f64, eve_destiny::DamageVector)> = Vec::new();
    let mut tracking_pylons: Vec<(Option<i64>, DVec3, f64, f64)> = Vec::new();
    for b in &balls {
        let is_alive = sys_handle
            .get_combat_state(b.id)
            .await
            .ok()
            .flatten()
            .map(|cs| cs.structure_hp > 0.0)
            .unwrap_or(true);
        if !is_alive {
            continue;
        }
        let py_pocket = (b.id >= 1_899_000_000).then_some((b.id - 1_899_000_000) / 100);
        if matches!(
            b.type_id,
            AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID | AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID | AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID
        ) {
            let s_range = suppressor_radius_m(Some(&ctx.sde), b.type_id, b.radius);
            let s_dmg = suppressor_pulse_damage(Some(&ctx.sde), b.type_id);
            suppressors.push((py_pocket, b.position, s_range, s_dmg));
        } else if matches!(
            b.type_id,
            MULTIBODY_TRACKING_PYLON_TYPE_ID | MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID
        ) {
            let (p_range, p_mult) = tracking_pylon_radius_and_mult(Some(&ctx.sde), b.type_id, b.radius);
            tracking_pylons.push((py_pocket, b.position, p_range, p_mult));
        }
    }

    let mut damaged_count = 0usize;
    let mut tracking_buffed_count = 0usize;
    let mut killed_targets = Vec::new();
    for b in &balls {
        let Some(t) = ctx.sde.types.get(&b.type_id) else { continue };
        let b_pocket = pocket_by_ship
            .get(&b.id)
            .copied()
            .or_else(|| (60_000_000..100_000_000).contains(&b.id).then_some(b.id / 100));
        if is_suppressor_target_type(t) {
            for &(spocket, spos, srange, sdmg) in &suppressors {
                if (spocket.is_none() || b_pocket.is_none() || spocket == b_pocket) && b.position.distance(spos) <= srange {
                    if let Ok(Some((_, destroyed, _))) = sys_handle.apply_damage(b.id, sdmg).await {
                        if destroyed {
                            killed_targets.push(b.id);
                        }
                    }
                    damaged_count += 1;
                    if killed_targets.last() == Some(&b.id) {
                        break;
                    }
                }
            }
        }

        if is_tracking_pylon_beneficiary(t) && !killed_targets.contains(&b.id) {
            let mut desired_mult = 1.0_f64;
            for &(ppocket, ppos, prange, pmult) in &tracking_pylons {
                if (ppocket.is_none() || b_pocket.is_none() || ppocket == b_pocket)
                    && b.position.distance(ppos) <= prange
                    && pmult > desired_mult
                {
                    desired_mult = pmult;
                }
            }
            if let Ok(Some(mut cs)) = sys_handle.get_combat_state(b.id).await {
                if (cs.tracking_speed_multiplier - desired_mult).abs() > 1e-6 {
                    cs.tracking_speed_multiplier = desired_mult;
                    let _ = sys_handle.set_combat_state(b.id, cs).await;
                }
            }
            if desired_mult > 1.0 {
                tracking_buffed_count += 1;
            }
        }
    }
    if !killed_targets.is_empty() {
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            for &kid in &killed_targets {
                rt.npc_entities.remove(&kid);
                rt.active_drones_in_space.remove(&kid);
            }
        });
        for kid in killed_targets {
            let _ = crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_core(
                &ctx.systems,
                &ctx.db,
                sys_id,
                kid,
            )
            .await;
        }
    }
    (damaged_count, tracking_buffed_count)
}

/// Removes active scanner probes that have strayed beyond their owner's 75 km Abyssal room boundary.
pub fn prune_out_of_bounds_abyssal_probes(ctx: &ServerContext, sys_id: SolarSystemId) {
    if !(32_000_001..=32_999_999).contains(&sys_id) {
        return;
    }
    let room_centers: HashMap<CharacterId, DVec3> = with_pve_special_reg(ctx, |reg| {
        reg.abyssal_pockets
            .values()
            .filter(|p| !p.completed && !p.collapsed_destroyed)
            .map(|p| {
                let z = (p.current_room.saturating_sub(1) as f64) * 50_000.0;
                (p.character_id, DVec3::from_array(p.grid_offset) + DVec3::new(0.0, 0.0, z))
            })
            .collect()
    });
    let mut removed_probes = Vec::new();
    if let Ok(mut probes) = ctx.active_scanner_probes.write() {
        probes.retain(|&pid, pr| {
            if pr.solar_system_id != sys_id {
                return true;
            }
            let center = room_centers.get(&pr.owner_char_id).copied().unwrap_or(DVec3::ZERO);
            let outside = DVec3::from_array(pr.position).distance(center) > ABYSSAL_ROOM_BOUNDARY_RADIUS_M;
            if outside {
                removed_probes.push(pid);
            }
            !outside
        });
    }
    if !removed_probes.is_empty() {
        let handle = ctx.systems.get_or_spawn(sys_id);
        for pid in removed_probes {
            handle.try_remove_ball(pid);
        }
    }
}

/// Real-time 1 Hz tick evaluation of Abyssal hazard clouds, automated pylons, force-decloak, and per-ship 75 km boundary damage.
/// Returns any ball IDs destroyed by Deviant Automata Suppressors during this tick.
pub fn step_abyssal_hazards_and_pylons_in_ballpark(
    ballpark: &mut eve_destiny::Ballpark,
    sde_opt: Option<&eve_sde::SdeRegistry>,
    db_opt: Option<&std::sync::Arc<eve_db::WorldRepository>>,
    solar_system_id: eve_sde::SolarSystemId,
) -> Vec<i64> {
    let mut suppressor_destroyed = Vec::new();
    if !(32_000_001..=32_999_999).contains(&solar_system_id) {
        return suppressor_destroyed;
    }
    let mut pocket_by_ship: HashMap<i64, i64> = HashMap::new();
    let mut room_center_by_ship: HashMap<i64, DVec3> = HashMap::new();
    let mut room_center_by_char: HashMap<i64, DVec3> = HashMap::new();
    let mut pocket_npc_ids: HashSet<i64> = HashSet::new();
    let mut arena_ships: HashSet<i64> = HashSet::new();
    let mut active_clouds: HashMap<i64, TypeId> = HashMap::new();
    let mut boundary_ticks: HashMap<i64, u32> = HashMap::new();
    if let Some(db) = db_opt {
        let _ = db.try_with_mut_snapshot(|snap| -> Result<(), ()> {
            if let Some(blob) = snap.service_state_blobs.get("incursions_state") {
                if let Ok(reg) = serde_json::from_str::<crate::services::pve_special_sites::PveSpecialSitesRegistry>(blob) {
                    let active_pockets: Vec<_> = reg
                        .abyssal_pockets
                        .values()
                        .filter(|p| !p.completed && !p.collapsed_destroyed)
                        .collect();
                    // First pass: bind each active pilot's own ship & character to their authoritative `current_room`
                    for p in &active_pockets {
                        let z = (p.current_room.saturating_sub(1) as f64) * 50_000.0;
                        let center = DVec3::from_array(p.grid_offset) + DVec3::new(0.0, 0.0, z);
                        pocket_by_ship.insert(p.ship_id, p.pocket_id);
                        room_center_by_ship.insert(p.ship_id, center);
                        room_center_by_char.insert(p.character_id, center);
                    }
                    // Second pass: fill fallback fleet ships via per-ship `ship_room` without overwriting any pilot's own room
                    for p in &active_pockets {
                        for &sid in &p.fleet_ship_ids {
                            pocket_by_ship.entry(sid).or_insert(p.pocket_id);
                            room_center_by_ship.entry(sid).or_insert_with(|| {
                                let sr = p.ship_room.get(&sid).copied().unwrap_or(p.current_room);
                                let z = (sr.saturating_sub(1) as f64) * 50_000.0;
                                DVec3::from_array(p.grid_offset) + DVec3::new(0.0, 0.0, z)
                            });
                        }
                        for r in &p.rooms {
                            for &nid in &r.npc_ball_ids {
                                pocket_npc_ids.insert(nid);
                                pocket_by_ship.entry(nid).or_insert(p.pocket_id);
                            }
                        }
                    }
                }
            }
            let pvp_store = crate::services::pvp_filament_mgr::load_pvp_store(snap);
            for m in pvp_store.active_matches.values().filter(|m| m.active) {
                for &cid in m.team_a.iter().chain(m.team_b.iter()) {
                    if let Some(ch) = snap.characters.get(&cid) {
                        arena_ships.insert(ch.active_ship_id);
                    }
                }
            }
            if let Some(cblob) = snap.service_state_blobs.get("abyssal_active_clouds") {
                active_clouds = serde_json::from_str(cblob).unwrap_or_default();
            }
            if let Some(bblob) = snap.service_state_blobs.get("abyssal_boundary_ticks") {
                boundary_ticks = serde_json::from_str(bblob).unwrap_or_default();
            }
            Ok(())
        });
    }

    let mut suppressors: Vec<(Option<i64>, DVec3, f64, eve_destiny::DamageVector)> = Vec::new();
    let mut tracking_pylons: Vec<(Option<i64>, DVec3, f64, f64)> = Vec::new();
    for (&bid, b) in &ballpark.balls {
        if !ballpark.combat_states.get(&bid).map(|cs| cs.structure_hp > 0.0).unwrap_or(true) {
            continue;
        }
        let py_pocket = (bid >= 1_899_000_000).then_some((bid - 1_899_000_000) / 100);
        if matches!(
            b.type_id,
            AUTOMATA_SUPPRESSOR_SMALL_TYPE_ID | AUTOMATA_SUPPRESSOR_MEDIUM_TYPE_ID | AUTOMATA_SUPPRESSOR_LARGE_TYPE_ID
        ) {
            let s_range = suppressor_radius_m(sde_opt, b.type_id, b.radius);
            let s_dmg = suppressor_pulse_damage(sde_opt, b.type_id);
            suppressors.push((py_pocket, b.position, s_range, s_dmg));
        } else if matches!(b.type_id, MULTIBODY_TRACKING_PYLON_TYPE_ID | MULTIBODY_TRACKING_PYLON_MEDIUM_TYPE_ID) {
            let (p_range, p_mult) = tracking_pylon_radius_and_mult(sde_opt, b.type_id, b.radius);
            tracking_pylons.push((py_pocket, b.position, p_range, p_mult));
        }
    }

    if !suppressors.is_empty() {
        let mut damages: Vec<(i64, eve_destiny::DamageVector)> = Vec::new();
        for (&bid, b) in &ballpark.balls {
            if arena_ships.contains(&bid) {
                continue;
            }
            let b_pocket = pocket_by_ship
                .get(&bid)
                .copied()
                .or_else(|| (60_000_000..100_000_000).contains(&bid).then_some(bid / 100));
            let sde_type = sde_opt.and_then(|sde| sde.types.get(&b.type_id));
            if sde_type.is_some_and(is_suppressor_target_type) {
                for &(spocket, spos, srange, sdmg) in &suppressors {
                    if (spocket.is_none() || b_pocket.is_none() || spocket == b_pocket) && b.position.distance(spos) <= srange {
                        damages.push((bid, sdmg));
                        break;
                    }
                }
            }
        }
        for (target_id, dmg) in damages {
            if let Some((_, true, _)) = ballpark.apply_damage_to_ball(target_id, dmg) {
                suppressor_destroyed.push(target_id);
                if let Some(db) = db_opt {
                    crate::services::pve_special_sites_abyssal::on_abyssal_room_npc_destroyed_in_ballpark(
                        ballpark,
                        db,
                        solar_system_id,
                        target_id,
                    );
                }
            }
        }
    }

    let ship_ids: Vec<i64> = ballpark
        .balls
        .keys()
        .copied()
        .filter(|&id| ballpark.combat_states.contains_key(&id) && !ballpark.is_static_ball(id) && !arena_ships.contains(&id))
        .collect();

    for &sid in &ship_ids {
        let Some(b) = ballpark.balls.get(&sid) else { continue };
        if let Some(cs) = ballpark.combat_states.get_mut(&sid) {
            if cs.is_cloaked {
                cs.is_cloaked = false;
            }
        }
        if let Some(sm) = ballpark.ball_slim_meta.get_mut(&sid) {
            if sm.is_cloaked {
                sm.is_cloaked = false;
            }
        }
        let is_beneficiary = sde_opt
            .and_then(|s| s.types.get(&b.type_id))
            .map(is_tracking_pylon_beneficiary)
            .unwrap_or(true);
        if !is_beneficiary {
            continue;
        }
        let s_pocket = pocket_by_ship
            .get(&sid)
            .copied()
            .or_else(|| (60_000_000..100_000_000).contains(&sid).then_some(sid / 100));
        let mut desired = 1.0_f64;
        for &(ppocket, ppos, prange, pmult) in &tracking_pylons {
            if (ppocket.is_none() || s_pocket.is_none() || ppocket == s_pocket)
                && b.position.distance(ppos) <= prange
                && pmult > desired
            {
                desired = pmult;
            }
        }
        if let Some(cs) = ballpark.combat_states.get_mut(&sid) {
            cs.tracking_speed_multiplier = desired;
        }
    }

    let boundary_candidates: Vec<(i64, TypeId, DVec3, bool, bool)> = ballpark
        .balls
        .iter()
        .filter_map(|(&bid, b)| {
            if ballpark.is_static_ball(bid)
                || arena_ships.contains(&bid)
                || bid >= 1_895_000_000
                || pocket_npc_ids.contains(&bid)
                || (60_000_000..100_000_000).contains(&bid)
            {
                return None;
            }
            let is_pod = b.type_id == 670;
            let sde_t = sde_opt.and_then(|s| s.types.get(&b.type_id));
            let is_probe = sde_t.is_some_and(|t| t.group_id == 479);
            let is_ship = !is_pod
                && sde_t
                    .map(|t| t.category_id == 6)
                    .unwrap_or_else(|| {
                        pocket_by_ship.contains_key(&bid)
                            || ballpark.ball_slim_meta.get(&bid).is_some_and(|m| m.char_id.is_some())
                            || ballpark.combat_states.contains_key(&bid)
                    });
            (is_pod || is_probe || is_ship).then_some((bid, b.type_id, b.position, is_probe, is_pod))
        })
        .collect();

    let mut boundary_destroyed: Vec<(i64, TypeId, DVec3, bool, bool, u32)> = Vec::new();
    for (bid, b_type, b_pos, is_probe, is_pod) in boundary_candidates {
        let center = room_center_by_ship
            .get(&bid)
            .copied()
            .or_else(|| {
                ballpark
                    .ball_slim_meta
                    .get(&bid)
                    .and_then(|m| room_center_by_char.get(&m.owner_id).copied())
            })
            .unwrap_or(DVec3::ZERO);
        if b_pos.distance(center) <= ABYSSAL_ROOM_BOUNDARY_RADIUS_M {
            boundary_ticks.remove(&bid);
            continue;
        }
        let ticks = boundary_ticks.entry(bid).or_insert(0);
        *ticks += 1;
        let t_count = *ticks;
        let per_type = 250.0 * (1.0 + 0.5 * ((t_count as f64) - 1.0));
        if let Some(cs) = ballpark.combat_states.get_mut(&bid) {
            let raw_hull_dmg = per_type
                * (cs.structure_resonance[0]
                    + cs.structure_resonance[1]
                    + cs.structure_resonance[2]
                    + cs.structure_resonance[3]);
            cs.structure_hp = (cs.structure_hp - raw_hull_dmg).max(0.0);
            if cs.structure_hp <= 0.0 {
                boundary_ticks.remove(&bid);
                boundary_destroyed.push((bid, b_type, b_pos, is_probe, is_pod, t_count));
            }
        } else if is_probe {
            boundary_ticks.remove(&bid);
            boundary_destroyed.push((bid, b_type, b_pos, true, false, t_count));
        }
    }

    for (bid, b_type, b_pos, is_probe, is_pod, t_count) in boundary_destroyed {
        let owner_hint = ballpark
            .ball_slim_meta
            .get(&bid)
            .map(|m| m.owner_id)
            .unwrap_or(0);
        ballpark.remove_ball(bid);
        if is_probe {
            if let Some(db) = db_opt {
                db.with_mut_snapshot(|snap| {
                    snap.items.remove(&bid);
                    snap.item_positions.remove(&bid);
                });
            }
            continue;
        }
        if is_pod {
            let mut spawned_corpse: Option<(ItemId, CharacterId)> = None;
            if let Some(db) = db_opt {
                let now_u = db.clock().now_unix_secs();
                crate::sp6_lifecycle::with_mut_sp6_state(db, |st, snap| {
                    let cid = snap
                        .items
                        .get(&bid)
                        .map(|i| i.owner_id)
                        .filter(|&o| o > 0)
                        .or_else(|| {
                            snap.characters
                                .values()
                                .find(|c| c.active_ship_id == bid)
                                .map(|c| c.character_id)
                        })
                        .unwrap_or(owner_hint);
                    snap.items.remove(&bid);
                    snap.item_positions.remove(&bid);
                    st.ship_combat_hp.remove(&bid);
                    if cid > 0 {
                        st.abyssal_sessions.remove(&cid);
                        if let Some(blob) = snap.service_state_blobs.get("incursions_state").cloned() {
                            if let Ok(mut reg) = serde_json::from_str::<crate::services::pve_special_sites::PveSpecialSitesRegistry>(&blob) {
                                if let Some(p) = reg.abyssal_pockets.get_mut(&cid) {
                                    p.collapsed_destroyed = true;
                                    p.completed = true;
                                }
                                if let Ok(js) = serde_json::to_string(&reg) {
                                    snap.service_state_blobs.insert("incursions_state".into(), js);
                                }
                            }
                        }
                        let (_, _, _, corpse_id, _) = destroy_pod_and_respawn_faction_corvette_in_snap(
                            snap,
                            sde_opt,
                            cid,
                            solar_system_id,
                            [b_pos.x, b_pos.y, b_pos.z],
                            now_u,
                        );
                        spawned_corpse = Some((corpse_id, cid));
                    }
                });
            }
            if let Some((corpse_id, cid)) = spawned_corpse {
                let (c_grp, c_cat) = sde_opt
                    .and_then(|sde| sde.types.get(&25))
                    .map(|t| (t.group_id, t.category_id))
                    .unwrap_or((14, 2));
                ballpark.add_ball(eve_destiny::Ball {
                    id: corpse_id,
                    type_id: 25,
                    position: b_pos,
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 80.0,
                    radius: 2.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                });
                ballpark.set_ball_slim_meta(
                    corpse_id,
                    eve_destiny::SlimItemMeta {
                        group_id: c_grp,
                        category_id: c_cat,
                        owner_id: cid,
                        char_id: None,
                        corp_id: -1,
                        alliance_id: -1,
                        security_status: 0.0,
                        is_cloaked: false,
                    },
                );
            }
            continue;
        }
        let (cap_id, owner_cid) = if let Some(db) = db_opt {
            let now_u = db.clock().now_unix_secs();
            crate::sp6_lifecycle::with_mut_sp6_state(db, |st, snap| {
                let cid = snap
                    .items
                    .get(&bid)
                    .map(|i| i.owner_id)
                    .filter(|&o| o > 0)
                    .or_else(|| {
                        snap.characters
                            .values()
                            .find(|c| c.active_ship_id == bid)
                            .map(|c| c.character_id)
                    })
                    .unwrap_or(owner_hint);
                let base_price = sde_opt
                    .and_then(|sde| sde.types.get(&b_type))
                    .map(|t| t.base_price)
                    .unwrap_or(250_000.0);
                snap.items.remove(&bid);
                snap.item_positions.remove(&bid);
                snap.module_online_states.remove(&bid);
                st.ship_combat_hp.remove(&bid);
                snap.items.retain(|_, it| it.location_id != bid);
                snap.next_entity_id += 1;
                let cap_id = snap.next_entity_id;
                snap.items.insert(
                    cap_id,
                    eve_db::InventoryItemRecord {
                        item_id: cap_id,
                        type_id: 670,
                        owner_id: cid,
                        location_id: solar_system_id as i64,
                        flag: eve_db::InventoryFlag::None,
                        quantity: 1,
                        singleton: true,
                        version: 1,
                        ..Default::default()
                    },
                );
                snap.item_positions.insert(cap_id, [b_pos.x, b_pos.y, b_pos.z]);
                if cid > 0 {
                    if let Some(ch) = snap.characters.get_mut(&cid) {
                        ch.active_ship_id = cap_id;
                        ch.version += 1;
                        ch.system_epoch += 1;
                    }
                    if let Some(blob) = snap.service_state_blobs.get("incursions_state").cloned() {
                        if let Ok(mut reg) = serde_json::from_str::<crate::services::pve_special_sites::PveSpecialSitesRegistry>(&blob) {
                            let room_opt = reg.abyssal_pockets.get(&cid).map(|p| (p.pocket_id, p.current_room));
                            if let Some(p) = reg.abyssal_pockets.get_mut(&cid) {
                                p.ship_id = cap_id;
                                if !p.fleet_ship_ids.contains(&cap_id) {
                                    p.fleet_ship_ids.push(cap_id);
                                }
                            }
                            if let Some((pid, rnum)) = room_opt {
                                for other in reg.abyssal_pockets.values_mut().filter(|o| o.pocket_id == pid) {
                                    other.ship_room.remove(&bid);
                                    other.ship_room.insert(cap_id, rnum);
                                }
                            }
                            if let Ok(js) = serde_json::to_string(&reg) {
                                snap.service_state_blobs.insert("incursions_state".into(), js);
                            }
                        }
                    }
                    snap.next_entity_id += 1;
                    let kid = snap.next_entity_id;
                    snap.record_killmail(
                        eve_db::KillmailRecord {
                            kill_id: kid,
                            victim_character_id: cid,
                            attacker_character_id: 0,
                            attacker_corp_id: 1_000_298,
                            solar_system_id,
                            ship_type_id: b_type,
                            total_value_isk: base_price,
                            ..Default::default()
                        },
                        now_u,
                    );
                }
                (cap_id, cid)
            })
        } else {
            (bid.saturating_add(900_000_000), owner_hint)
        };
        if let Some(&center) = room_center_by_ship.get(&bid) {
            room_center_by_ship.insert(cap_id, center);
        }
        boundary_ticks.insert(cap_id, t_count);
        let cap_cs = eve_destiny::CombatState {
            shield_hp: 0.0,
            max_shield_hp: 0.0,
            armor_hp: 0.0,
            max_armor_hp: 0.0,
            structure_hp: 100.0,
            max_structure_hp: 100.0,
            ..Default::default()
        };
        ballpark.add_ball(eve_destiny::Ball {
            id: cap_id,
            type_id: 670,
            position: b_pos,
            velocity: DVec3::ZERO,
            max_velocity: 200.0,
            inertia: 0.2,
            mass: 1_000.0,
            radius: 15.0,
            speed_fraction: 0.0,
            mode: eve_destiny::MovementMode::Stop,
        });
        ballpark.combat_states.insert(cap_id, cap_cs);
        ballpark.set_ball_slim_meta(
            cap_id,
            eve_destiny::SlimItemMeta {
                group_id: 29,
                category_id: 6,
                owner_id: owner_cid,
                char_id: (owner_cid > 0).then_some(owner_cid),
                corp_id: -1,
                alliance_id: -1,
                security_status: 0.0,
                is_cloaked: false,
            },
        );
    }

    boundary_ticks.retain(|bid, _| ballpark.balls.contains_key(bid));
    if let Some(db) = db_opt {
        if let Ok(json) = serde_json::to_string(&boundary_ticks) {
            let _ = db.try_with_mut_snapshot(|snap| -> Result<(), ()> {
                snap.service_state_blobs.insert("abyssal_boundary_ticks".into(), json);
                Ok(())
            });
        }
    }

    let mut clouds: Vec<(Option<i64>, TypeId, DVec3, f64)> = Vec::new();
    for (&bid, b) in &ballpark.balls {
        if is_spatial_hazard_cloud_type(b.type_id) {
            let c_pocket = (bid >= 1_898_000_000).then_some((bid - 1_898_000_000) / 100);
            let c_range = spatial_cloud_radius_m(sde_opt, b.type_id, b.radius);
            clouds.push((c_pocket, b.type_id, b.position, c_range));
        }
    }

    if clouds.is_empty() && active_clouds.is_empty() {
        return suppressor_destroyed;
    }

    active_clouds.retain(|bid, _| ballpark.balls.contains_key(bid));

    for sid in ship_ids {
        let Some(b) = ballpark.balls.get_mut(&sid) else { continue };
        let pos = b.position;
        let s_pocket = pocket_by_ship
            .get(&sid)
            .copied()
            .or_else(|| (60_000_000..100_000_000).contains(&sid).then_some(sid / 100));
        let current_cloud = clouds
            .iter()
            .find(|(cpocket, _, cpos, cradius)| {
                (cpocket.is_none() || s_pocket.is_none() || *cpocket == s_pocket) && pos.distance(*cpos) < *cradius
            })
            .map(|(_, tid, _, _)| *tid);

        let prev_cloud = active_clouds.get(&sid).copied();
        if prev_cloud == current_cloud {
            continue;
        }

        let (prev_v, prev_sig, prev_in, _prev_b, prev_cap, _) = cloud_modifiers_for_type(prev_cloud);
        b.max_velocity /= prev_v;
        b.inertia /= prev_in;
        if let Some(cs) = ballpark.combat_states.get_mut(&sid) {
            cs.signature_radius /= prev_sig;
            cs.shield_boost_multiplier = 1.0;
            cs.capacitor_recharge_time_ms = (cs.capacitor_recharge_time_ms / prev_cap).max(1_000.0);
        }

        let (cur_v, cur_sig, cur_in, cur_b, cur_cap, _) = cloud_modifiers_for_type(current_cloud);
        b.max_velocity *= cur_v;
        b.inertia *= cur_in;
        if let Some(cs) = ballpark.combat_states.get_mut(&sid) {
            cs.signature_radius *= cur_sig;
            cs.shield_boost_multiplier = cur_b;
            cs.capacitor_recharge_time_ms = (cs.capacitor_recharge_time_ms * cur_cap).max(1_000.0);
        }
        if let Some(cid) = current_cloud {
            active_clouds.insert(sid, cid);
        } else {
            active_clouds.remove(&sid);
        }
    }

    if let Some(db) = db_opt {
        if let Ok(json) = serde_json::to_string(&active_clouds) {
            let _ = db.try_with_mut_snapshot(|snap| -> Result<(), ()> {
                snap.service_state_blobs.insert("abyssal_active_clouds".into(), json);
                Ok(())
            });
        }
    }
    suppressor_destroyed
}
