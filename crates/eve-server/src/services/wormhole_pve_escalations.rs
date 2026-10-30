//! C5/C6 capital escalations (`Upgraded Avenger`), Drifter response escalations
//! (`Decloaked Transmission Relay` -> `Arithmos Tyrannos` / `Hyleus Tyrannos`),
//! delayed Shattered Debris/Ice waves, and W-space site downtime despawn.
//!
// parity: feat:pve.wormhole-capital-escalations, feat:pve.wormhole-drifter-response-escalation
// parity: feat:pve.wormhole-site-downtime-despawn, feat:pve.sleeper-sentry-towers, feat:pve.wormhole-site-capital-escalation-cap

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::{CombatRuntimeState, ExtendedNpcEntity};
use crate::services::pve_exploration::{
    with_pve_exploration_reg, with_pve_exploration_reg_db, ItemId,
};
use crate::services::pve_exploration_seeding::build_type_combat_state_from_sde;
pub use crate::services::wormhole_pve_seeding::*;
use crate::system_actor::SystemRegistry;
use eve_db::WorldRepository;
use eve_destiny::{Ball, CombatState, DamageVector, MovementMode, SlimItemMeta};
use eve_sde::{SdeRegistry, SolarSystemId, TypeId};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ActiveCapitalEscalationEntry {
    pub ship_id: i64,
    pub character_id: i64,
    pub is_dreadnought: bool,
    pub warp_in_pos: [f64; 3],
    pub avenger_ball_ids: Vec<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DecloakedRelayInfo {
    pub relay_ball_id: i64,
    pub solar_system_id: SolarSystemId,
    pub site_id: String,
    pub position: [f64; 3],
    pub capital_was_on_grid: bool,
    pub triggered: bool,
    pub spawned_drifter_ball_id: Option<i64>,
    pub spawned_drifter_type_id: Option<u32>,
    pub last_warp_unix: u64,
    pub last_warp_sim_s: f64,
    pub warp_hop_count: u32,
}

/// Spawns `Upgraded Avenger` (`37472`) capital escalation battleships (`3` in C5, `4` in C6)
/// with `30,000 m` signature radius, web, scram, and neut (`feat:pve.wormhole-capital-escalations`).
pub async fn spawn_upgraded_avengers_core(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    combat_ephemeral: &Arc<RwLock<CombatRuntimeState>>,
    solar_system_id: SolarSystemId,
    wh_class: u8,
    center: DVec3,
    batch_offset: usize,
) -> Vec<i64> {
    let count = if wh_class >= 6 { 4 } else { 3 };
    let tid: TypeId = 37_472; // Upgraded Avenger
    let sde_type = sde.types.get(&tid);
    let attr = |aid: u16| -> Option<f64> {
        sde_type.and_then(|t| t.default_attributes.get(&aid).copied())
    };
    let sys_handle = systems.get_or_spawn(solar_system_id);
    let mut ids = Vec::with_capacity(count);
    for i in 0..count {
        let bid = db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            snap.next_entity_id
        });
        let pos = center
            + DVec3::new(
                ((i as f64) - 1.5) * 3_500.0,
                ((batch_offset as f64) + 1.0) * 2_000.0,
                ((i % 2) as f64) * 2_500.0,
            );
        let mut cs = build_type_combat_state_from_sde(sde, tid);
        cs.signature_radius = attr(552).unwrap_or(30_000.0);
        if cs.max_armor_hp <= 50.0 {
            cs.max_armor_hp = 250_000.0;
            cs.armor_hp = 250_000.0;
            cs.max_structure_hp = 150_000.0;
            cs.structure_hp = 150_000.0;
        }
        let ball = Ball {
            id: bid,
            type_id: tid,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: attr(37).unwrap_or(3_000.0),
            inertia: 1.0,
            mass: sde_type.map(|t| t.mass).unwrap_or(19_000_000.0),
            radius: sde_type.map(|t| t.radius).unwrap_or(350.0),
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        };
        let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;
        let dmg_mult = attr(64).unwrap_or(40.0);
        let entity = ExtendedNpcEntity {
            ball_id: bid,
            type_id: tid,
            faction_id: 500_005,
            archetype: "Sleeper".into(),
            solar_system_id,
            spawn_group_id: 0,
            target_id: None,
            aggression_threat: HashMap::new(),
            threat_table: HashMap::new(),
            bounty_isk: 0.0,
            optimal_range: attr(54).unwrap_or(320_000.0),
            orbit_range: 15_000.0,
            damage_vector: DamageVector {
                em: attr(114).unwrap_or(40.0) * dmg_mult,
                thermal: attr(118).unwrap_or(40.0) * dmg_mult,
                kinetic: 0.0,
                explosive: 0.0,
            },
            entropic_cycles: 0,
            drifter_overshield_hp: 0.0,
            drifter_doomsday_fired: false,
            web_speed_factor: attr(20).unwrap_or(-50.0),
            web_range: attr(514).or_else(|| attr(580)).unwrap_or(100_000.0),
            scram_strength: attr(105).unwrap_or(15.0),
            scram_range: attr(103).or_else(|| attr(582)).unwrap_or(100_000.0),
            neut_amount: attr(97).unwrap_or(1_500.0),
            ecm_jammed: false,
            remote_shield_rep: 0.0,
            remote_armor_rep: 0.0,
            remote_rep_range: 0.0,
            tag_drop_type_id: None,
            spawn_position: pos,
            sleeper_wh_class: wh_class,
            custom_cargo_loot: None,
        };
        combat_ephemeral
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .npc_entities
            .insert(bid, entity);
        ids.push(bid);
    }
    ids
}

/// Spawns a `Decloaked Transmission Relay` (`41589`) when a C5/C6 combat or Sleeper data/relic site
/// finishes (`feat:pve.wormhole-drifter-response-escalation`).
#[allow(clippy::too_many_arguments)]
pub async fn spawn_decloaked_transmission_relay_core(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    sde: &Arc<SdeRegistry>,
    combat_ephemeral: &Arc<RwLock<CombatRuntimeState>>,
    solar_system_id: SolarSystemId,
    site_id: &str,
    pos: [f64; 3],
) -> Option<i64> {
    let now_unix = db.clock().now_unix_secs();
    let should_spawn = with_pve_exploration_reg_db(db, now_unix, |reg| {
        let Some(site) = reg
            .sites_by_system
            .get_mut(&solar_system_id)
            .and_then(|l| l.iter_mut().find(|s| s.site_id == site_id))
        else {
            return false;
        };
        if site.relay_spawned {
            return false;
        }
        let qualifies = matches!(site.wormhole_class, Some(5 | 6))
            && matches!(site.site_category.as_str(), "WormholeCombat" | "Data" | "Relic");
        if !qualifies {
            return false;
        }
        site.relay_spawned = true;
        true
    });
    if !should_spawn {
        return None;
    }

    let sys_handle = systems.get_or_spawn(solar_system_id);
    let site_vec = DVec3::from_array(pos);
    let on_grid_ship_types: Vec<u32> = {
        let bp = sys_handle.ballpark.read().unwrap_or_else(|e| e.into_inner());
        bp.balls
            .values()
            .filter(|b| {
                !matches!(b.mode, MovementMode::Warp { .. })
                    && b.position.distance(site_vec) <= 100_000.0
            })
            .map(|b| b.type_id)
            .collect()
    };
    let capital_was_on_grid = on_grid_ship_types.iter().any(|tid| {
        sde.types
            .get(tid)
            .is_some_and(|t| is_any_capital_group(t.group_id))
    });

    let relay_tid: TypeId = 41_589; // Decloaked Transmission Relay
    let relay_bid = db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        snap.next_entity_id
    });
    let mut cs = build_type_combat_state_from_sde(sde, relay_tid);
    if cs.max_structure_hp <= 50.0 {
        cs.max_shield_hp = 1_000_000.0;
        cs.shield_hp = 1_000_000.0;
        cs.max_armor_hp = 500_000.0;
        cs.armor_hp = 500_000.0;
        cs.max_structure_hp = 500_000.0;
        cs.structure_hp = 500_000.0;
    }
    let ball = Ball {
        id: relay_bid,
        type_id: relay_tid,
        position: site_vec,
        velocity: DVec3::ZERO,
        max_velocity: 0.0,
        inertia: 1.0,
        mass: 100_000.0,
        radius: 500.0,
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let _ = sys_handle.spawn_ball_with_combat(ball, cs).await;
    let entity = ExtendedNpcEntity {
        ball_id: relay_bid,
        type_id: relay_tid,
        faction_id: 500_024,
        archetype: "TransmissionRelay".into(),
        solar_system_id,
        spawn_group_id: 0,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk: 0.0,
        optimal_range: 0.0,
        orbit_range: 0.0,
        damage_vector: DamageVector::default(),
        entropic_cycles: 0,
        drifter_overshield_hp: 0.0,
        drifter_doomsday_fired: false,
        web_speed_factor: 0.0,
        web_range: 0.0,
        scram_strength: 0.0,
        scram_range: 0.0,
        neut_amount: 0.0,
        ecm_jammed: false,
        remote_shield_rep: 0.0,
        remote_armor_rep: 0.0,
        remote_rep_range: 0.0,
        tag_drop_type_id: None,
        spawn_position: site_vec,
        sleeper_wh_class: 6,
        custom_cargo_loot: None,
    };
    combat_ephemeral
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .npc_entities
        .insert(relay_bid, entity);

    with_pve_exploration_reg_db(db, now_unix, |reg| {
        reg.active_relays.insert(
            relay_bid,
            DecloakedRelayInfo {
                relay_ball_id: relay_bid,
                solar_system_id,
                site_id: site_id.to_string(),
                position: pos,
                capital_was_on_grid,
                triggered: false,
                spawned_drifter_ball_id: None,
                spawned_drifter_type_id: None,
                last_warp_unix: now_unix,
                last_warp_sim_s: sys_handle.sim_elapsed_secs(),
                warp_hop_count: 0,
            },
        );
    });
    Some(relay_bid)
}

/// Attacking a `Decloaked Transmission Relay` (`41589`) spawns `Arithmos Tyrannos` (`37473`) if a capital
/// was on grid when the relay spawned, otherwise `Hyleus Tyrannos` (`86498`).
pub fn on_decloaked_relay_attacked_sync(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    relay_ball_id: i64,
) -> Option<(i64, TypeId)> {
    let relay_info = with_pve_exploration_reg(ctx, |reg| {
        let info = reg.active_relays.get_mut(&relay_ball_id)?;
        if info.triggered {
            return None;
        }
        info.triggered = true;
        Some(info.clone())
    })?;

    let is_arithmos = relay_info.capital_was_on_grid;
    let drifter_tid: TypeId = if is_arithmos { 37_473 } else { 86_498 };
    let sde_type = ctx.sde.types.get(&drifter_tid);
    let attr = |aid: u16| -> Option<f64> {
        sde_type.and_then(|t| t.default_attributes.get(&aid).copied())
    };
    let drifter_bid = ctx.db.with_mut_snapshot(|snap| {
        snap.next_entity_id += 1;
        snap.next_entity_id
    });
    let spawn_pos = DVec3::from_array(relay_info.position) + DVec3::new(3_500.0, 0.0, 0.0);
    let mut cs = build_type_combat_state_from_sde(&ctx.sde, drifter_tid);
    cs.warp_core_strength = 3;
    if cs.max_shield_hp <= 50.0 {
        cs.max_shield_hp = if is_arithmos { 800_000.0 } else { 270_000.0 };
        cs.shield_hp = cs.max_shield_hp;
        cs.max_armor_hp = if is_arithmos { 1_200_000.0 } else { 400_000.0 };
        cs.armor_hp = cs.max_armor_hp;
        cs.max_structure_hp = if is_arithmos { 150_000.0 } else { 50_000.0 };
        cs.structure_hp = cs.max_structure_hp;
    }
    let ball = Ball {
        id: drifter_bid,
        type_id: drifter_tid,
        position: spawn_pos,
        velocity: DVec3::ZERO,
        max_velocity: attr(37).unwrap_or(3_500.0),
        inertia: 1.5,
        mass: sde_type.map(|t| t.mass).unwrap_or(10_900_000.0),
        radius: sde_type.map(|t| t.radius).unwrap_or(350.0),
        speed_fraction: 0.0,
        mode: MovementMode::Stop,
    };
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    sys_handle.try_spawn_ball_with_meta(
        ball,
        cs,
        SlimItemMeta {
            group_id: sde_type.map(|t| t.group_id).unwrap_or(1530),
            category_id: sde_type.map(|t| t.category_id).unwrap_or(11),
            ..Default::default()
        },
    );
    sys_handle
        .actor_state
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .base_core_strengths
        .insert(drifter_bid, 3);

    let dmg_mult = attr(64).unwrap_or(if is_arithmos { 15.0 } else { 10.0 });
    let entity = ExtendedNpcEntity {
        ball_id: drifter_bid,
        type_id: drifter_tid,
        faction_id: 500_024,
        archetype: "Drifter".into(),
        solar_system_id,
        spawn_group_id: 0,
        target_id: None,
        aggression_threat: HashMap::new(),
        threat_table: HashMap::new(),
        bounty_isk: 0.0,
        optimal_range: attr(54).unwrap_or(250_000.0),
        orbit_range: 18_000.0,
        damage_vector: DamageVector {
            em: attr(114).unwrap_or(80.0) * dmg_mult,
            thermal: attr(118).unwrap_or(80.0) * dmg_mult,
            kinetic: attr(117).unwrap_or(80.0) * dmg_mult,
            explosive: attr(116).unwrap_or(80.0) * dmg_mult,
        },
        entropic_cycles: 0,
        drifter_overshield_hp: if is_arithmos { 150_000.0 } else { 50_000.0 },
        drifter_doomsday_fired: false,
        web_speed_factor: attr(20).unwrap_or(if is_arithmos { -60.0 } else { -50.0 }),
        web_range: attr(514).or_else(|| attr(580)).unwrap_or(100_000.0),
        scram_strength: attr(105).unwrap_or(20.0),
        scram_range: attr(103).or_else(|| attr(582)).unwrap_or(100_000.0),
        neut_amount: attr(97).unwrap_or(if is_arithmos { 2_000.0 } else { 1_000.0 }),
        ecm_jammed: false,
        remote_shield_rep: 0.0,
        remote_armor_rep: 0.0,
        remote_rep_range: 0.0,
        tag_drop_type_id: None,
        spawn_position: spawn_pos,
        sleeper_wh_class: 6,
        custom_cargo_loot: if is_arithmos {
            Some((30_747, 5))
        } else {
            None
        },
    };
    ctx.combat_ephemeral
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .npc_entities
        .insert(drifter_bid, entity);

    let now_unix = ctx.now_unix_secs();
    let now_sim = sys_handle.sim_elapsed_secs();
    with_pve_exploration_reg(ctx, |reg| {
        if let Some(info) = reg.active_relays.get_mut(&relay_ball_id) {
            info.spawned_drifter_ball_id = Some(drifter_bid);
            info.spawned_drifter_type_id = Some(drifter_tid);
            info.last_warp_unix = now_unix;
            info.last_warp_sim_s = now_sim;
        }
    });
    Some((drifter_bid, drifter_tid))
}

fn pick_drifter_roam_destination(
    sde: &SdeRegistry,
    solar_system_id: SolarSystemId,
    cur_pos: DVec3,
    hop_index: u32,
) -> DVec3 {
    let celestials = sde.celestials_in_system(solar_system_id);
    if !celestials.is_empty() {
        for offset in 0..celestials.len() {
            let idx = ((hop_index as usize) + offset) % celestials.len();
            let candidate = celestials[idx].position + DVec3::new(35_000.0, 15_000.0, -20_000.0);
            if candidate.distance(cur_pos) >= 1_000_000.0 {
                return candidate;
            }
        }
    }
    let angle = ((hop_index.wrapping_mul(73) % 360) as f64).to_radians();
    let r = 6.0 * crate::services::space_destiny::AU_IN_METERS;
    DVec3::new(r * angle.cos(), 1.2 * crate::services::space_destiny::AU_IN_METERS, r * angle.sin())
}

/// Steps wormhole PvE state per system tick:
/// - Tracks first warp-in activation (`activated_at_downtime`) for W-space sites.
/// - Triggers delayed Sleeper waves on Shattered Debris/Ice Fields.
/// - Triggers C5/C6 capital escalations (`Upgraded Avenger`: 3 in C5, 4 in C6, capped at 2 per
///   capital class / 4 total) and despawns escalation NPCs when the capital leaves or moves > 100 km.
/// - Completes C5/C6 Data/Relic sites when all guards are dead and all cans hacked, spawning a
///   `Decloaked Transmission Relay`.
/// - Warps `Arithmos Tyrannos` / `Hyleus Tyrannos` around the system if alive and not warp-scrambled.
pub async fn step_wormhole_pve_system(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    _dt: f64,
) {
    crate::services::space_destiny::space_docking_cloaking::collapse_expired_wormholes_in_system(
        ctx,
        solar_system_id,
        ctx.now_unix_secs(),
    )
    .await;
    if !(31_000_000..32_000_000).contains(&solar_system_id) {
        return;
    }
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let (ball_map, combat_states): (BTreeMap<i64, Ball>, BTreeMap<i64, CombatState>) = {
        let bp = sys_handle.ballpark.read().unwrap_or_else(|e| e.into_inner());
        (bp.balls.clone(), bp.combat_states.clone())
    };
    let active_ships: HashMap<i64, (i64, u32)> = ctx.db.with_snapshot(|snap| {
        snap.characters
            .values()
            .filter(|c| {
                c.solar_system_id == solar_system_id
                    && c.station_id.is_none()
                    && c.structure_id.is_none()
                    && c.active_ship_id > 0
            })
            .filter_map(|c| {
                let tid = snap.items.get(&c.active_ship_id)?.type_id;
                let gid = ctx.sde.types.get(&tid).map(|t| t.group_id).unwrap_or(0);
                Some((c.active_ship_id, (c.character_id, gid)))
            })
            .collect()
    });

    let now_unix = ctx.now_unix_secs();
    let sim_elapsed_s = sys_handle.sim_elapsed_secs();

    let (
        avengers_to_despawn,
        avengers_to_spawn,
        delayed_waves_to_spawn,
        relays_to_spawn,
        drifter_warps,
        damaged_relays,
    ) = with_pve_exploration_reg(ctx, |reg| {
        let mut despawn_ids: Vec<i64> = Vec::new();
        let mut spawn_avenger_jobs: Vec<(String, i64, u8, [f64; 3], usize)> = Vec::new();
        let mut spawn_delayed_jobs: Vec<(String, u8, [f64; 3])> = Vec::new();
        let mut spawn_relay_jobs: Vec<(String, [f64; 3])> = Vec::new();
        let mut warp_jobs: Vec<(i64, DVec3, DVec3)> = Vec::new();

        let hacked = reg.hacked_containers.clone();
        let dt_counter = reg.downtime_counter;

        if let Some(sites) = reg.sites_by_system.get_mut(&solar_system_id) {
            for site in sites.iter_mut() {
                if site.wormhole_class.is_none() {
                    continue;
                }
                let site_vec = DVec3::from_array(site.position);

                // Despawn escalation Avengers if triggering capital left grid/system or moved > 100 km
                site.active_escalation_capitals.retain(|entry| {
                    let still_on_grid = active_ships.contains_key(&entry.ship_id)
                        && ball_map.get(&entry.ship_id).is_some_and(|b| {
                            !matches!(b.mode, MovementMode::Warp { .. })
                                && b.position.distance(site_vec) <= 100_000.0
                                && b.position.distance(DVec3::from_array(entry.warp_in_pos))
                                    <= 100_000.0
                        });
                    if !still_on_grid {
                        despawn_ids.extend(entry.avenger_ball_ids.iter().copied());
                    }
                    still_on_grid
                });

                if site.cleared {
                    if !site.relay_spawned
                        && matches!(site.wormhole_class, Some(5 | 6))
                        && matches!(
                            site.site_category.as_str(),
                            "WormholeCombat" | "Data" | "Relic"
                        )
                    {
                        spawn_relay_jobs.push((site.site_id.clone(), site.position));
                    }
                    continue;
                }

                // Auto-complete Sleeper Data/Relic site when all guards dead and all cans hacked
                if matches!(site.site_category.as_str(), "Data" | "Relic")
                    && site.active_npc_ball_ids.is_empty()
                    && !site.hackable_can_ids.is_empty()
                    && site
                        .hackable_can_ids
                        .iter()
                        .all(|cid| hacked.contains(&(solar_system_id, *cid)))
                {
                    site.cleared = true;
                    if matches!(site.wormhole_class, Some(5 | 6)) && !site.relay_spawned {
                        spawn_relay_jobs.push((site.site_id.clone(), site.position));
                    }
                    continue;
                }

                // Check ships currently on grid (within 100 km and not warping)
                let mut on_grid_ships: Vec<(i64, i64, u32, [f64; 3])> = Vec::new();
                for (&ship_id, &(char_id, gid)) in &active_ships {
                    if let Some(sb) = ball_map.get(&ship_id) {
                        if !matches!(sb.mode, MovementMode::Warp { .. })
                            && sb.position.distance(site_vec) <= 100_000.0
                        {
                            on_grid_ships.push((
                                ship_id,
                                char_id,
                                gid,
                                [sb.position.x, sb.position.y, sb.position.z],
                            ));
                        }
                    }
                }
                if on_grid_ships.is_empty() {
                    continue;
                }

                // First warp-in activates the W-space site for downtime despawn tracking
                site.activated_at_downtime.get_or_insert(dt_counter);

                // Delayed Sleeper wave for Shattered Debris / Ice Fields
                if site.is_shattered_resource_site && !site.delayed_wave_triggered {
                    site.delayed_wave_triggered = true;
                    site.current_wave = 1;
                    spawn_delayed_jobs.push((
                        site.site_id.clone(),
                        site.wormhole_class.unwrap_or(3),
                        site.position,
                    ));
                }

                // C5/C6 Capital Escalations (max 2 per class: Dreadnought vs Carrier/FAX, 4 total)
                if let Some(wh_c @ (5 | 6)) = site.wormhole_class {
                    if matches!(
                        site.site_category.as_str(),
                        "WormholeCombat" | "Data" | "Relic"
                    ) {
                        for (ship_id, char_id, gid, ship_pos) in on_grid_ships {
                            let Some(is_dread) = capital_escalation_class_for_group(gid) else {
                                continue;
                            };
                            if site
                                .active_escalation_capitals
                                .iter()
                                .any(|e| e.ship_id == ship_id)
                            {
                                continue;
                            }
                            let allowed = if is_dread {
                                if site.dread_escalation_triggers < 2 {
                                    site.dread_escalation_triggers += 1;
                                    true
                                } else {
                                    false
                                }
                            } else if site.carrier_escalation_triggers < 2 {
                                site.carrier_escalation_triggers += 1;
                                true
                            } else {
                                false
                            };
                            if allowed {
                                let batch = (site.dread_escalation_triggers
                                    + site.carrier_escalation_triggers)
                                    as usize;
                                site.active_escalation_capitals.push(
                                    ActiveCapitalEscalationEntry {
                                        ship_id,
                                        character_id: char_id,
                                        is_dreadnought: is_dread,
                                        warp_in_pos: ship_pos,
                                        avenger_ball_ids: Vec::new(),
                                    },
                                );
                                spawn_avenger_jobs.push((
                                    site.site_id.clone(),
                                    ship_id,
                                    wh_c,
                                    site.position,
                                    batch,
                                ));
                            }
                        }
                    }
                }
            }
        }

        let mut damaged_relays: Vec<i64> = Vec::new();
        for relay in reg.active_relays.values() {
            if relay.solar_system_id == solar_system_id && !relay.triggered {
                let was_attacked = match combat_states.get(&relay.relay_ball_id) {
                    Some(rcs) => {
                        rcs.shield_hp < rcs.max_shield_hp
                            || rcs.armor_hp < rcs.max_armor_hp
                            || rcs.structure_hp < rcs.max_structure_hp
                    }
                    None => !ball_map.contains_key(&relay.relay_ball_id),
                };
                if was_attacked {
                    damaged_relays.push(relay.relay_ball_id);
                }
            }
        }

        // Check Arithmos Tyrannos / Hyleus Tyrannos roaming warp-around if alive and not warp-scrambled
        for relay in reg.active_relays.values_mut() {
            if relay.solar_system_id != solar_system_id || !relay.triggered {
                continue;
            }
            let Some(drifter_bid) = relay.spawned_drifter_ball_id else {
                continue;
            };
            let Some(dball) = ball_map.get(&drifter_bid) else {
                continue;
            };
            let is_scrambled = combat_states
                .get(&drifter_bid)
                .is_some_and(|cs| cs.is_warp_scrambled());
            if is_scrambled {
                continue;
            }
            // DERIVED: 240s per-grid Drifter loiter interval before warping to next celestial.
            let elapsed_u = now_unix.saturating_sub(relay.last_warp_unix);
            let elapsed_s = (sim_elapsed_s - relay.last_warp_sim_s).max(0.0);
            if (elapsed_u >= 240 || elapsed_s >= 240.0)
                && !matches!(dball.mode, MovementMode::Warp { .. })
            {
                let next_pos = pick_drifter_roam_destination(
                    &ctx.sde,
                    solar_system_id,
                    dball.position,
                    relay.warp_hop_count + 1,
                );
                relay.last_warp_unix = now_unix;
                relay.last_warp_sim_s = sim_elapsed_s;
                relay.warp_hop_count = relay.warp_hop_count.saturating_add(1);
                warp_jobs.push((drifter_bid, dball.position, next_pos));
            }
        }

        (
            despawn_ids,
            spawn_avenger_jobs,
            spawn_delayed_jobs,
            spawn_relay_jobs,
            warp_jobs,
            damaged_relays,
        )
    });

    for relay_bid in damaged_relays {
        let _ = on_decloaked_relay_attacked_sync(ctx, solar_system_id, relay_bid);
    }

    for &bid in &avengers_to_despawn {
        let _ = sys_handle.remove_ball(bid).await;
        ctx.combat_ephemeral
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .npc_entities
            .remove(&bid);
    }

    for (site_id, ship_id, wh_c, site_pos, batch) in avengers_to_spawn {
        let ids = spawn_upgraded_avengers_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            &ctx.combat_ephemeral,
            solar_system_id,
            wh_c,
            DVec3::from_array(site_pos),
            batch,
        )
        .await;
        with_pve_exploration_reg(ctx, |reg| {
            if let Some(site) = reg
                .sites_by_system
                .get_mut(&solar_system_id)
                .and_then(|l| l.iter_mut().find(|s| s.site_id == site_id))
            {
                if let Some(entry) = site
                    .active_escalation_capitals
                    .iter_mut()
                    .find(|e| e.ship_id == ship_id)
                {
                    entry.avenger_ball_ids = ids;
                }
            }
        });
    }

    for (site_id, wh_c, site_pos) in delayed_waves_to_spawn {
        let wave_ids = crate::services::pve_exploration_sites::spawn_sleeper_wave_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            &ctx.combat_ephemeral,
            solar_system_id,
            wh_c,
            1,
            DVec3::from_array(site_pos),
        )
        .await;
        with_pve_exploration_reg(ctx, |reg| {
            if let Some(site) = reg
                .sites_by_system
                .get_mut(&solar_system_id)
                .and_then(|l| l.iter_mut().find(|s| s.site_id == site_id))
            {
                site.active_npc_ball_ids = wave_ids;
                site.npc_guards_spawned = true;
            }
        });
    }

    for (site_id, site_pos) in relays_to_spawn {
        let _ = spawn_decloaked_transmission_relay_core(
            &ctx.systems,
            &ctx.db,
            &ctx.sde,
            &ctx.combat_ephemeral,
            solar_system_id,
            &site_id,
            site_pos,
        )
        .await;
    }

    let had_drifter_warp_or_despawn = !drifter_warps.is_empty() || !avengers_to_despawn.is_empty();
    for (drifter_bid, cur_pos, next_pos) in drifter_warps {
        let _ = sys_handle
            .set_movement_mode(drifter_bid, MovementMode::new_warp(cur_pos, next_pos, 3.0))
            .await;
        if let Some(npc) = ctx
            .combat_ephemeral
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .npc_entities
            .get_mut(&drifter_bid)
        {
            npc.spawn_position = next_pos;
        }
    }
    if had_drifter_warp_or_despawn {
        crate::services::combat_crimewatch::combat_npc_ai::step_extended_npc_ai_sync(ctx, solar_system_id);
    }
}

/// Triggers the single delayed Sleeper wave if a pilot mines an asteroid in a Shattered Debris Field
/// or Shattered Ice Field (`feat:navigation.standard-shattered-wormholes`).
pub fn on_asteroid_mined_in_system(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    _char_id: i64,
) {
    if !(31_000_000..32_000_000).contains(&solar_system_id) {
        return;
    }
    let to_trigger: Vec<(String, u8, [f64; 3])> = with_pve_exploration_reg(ctx, |reg| {
        let dt_counter = reg.downtime_counter;
        let Some(sites) = reg.sites_by_system.get_mut(&solar_system_id) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for site in sites.iter_mut() {
            if site.is_shattered_resource_site && !site.delayed_wave_triggered && !site.cleared {
                site.activated_at_downtime.get_or_insert(dt_counter);
                site.delayed_wave_triggered = true;
                site.current_wave = 1;
                out.push((
                    site.site_id.clone(),
                    site.wormhole_class.unwrap_or(3),
                    site.position,
                ));
            }
        }
        out
    });
    if to_trigger.is_empty() {
        return;
    }
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    for (site_id, wh_c, site_pos) in to_trigger {
        let (trigger_tid, other_tids) =
            crate::services::pve_exploration_sites::sleeper_npc_types_for_class_and_wave(wh_c, 1);
        let sentry_tid = sleeper_sentry_type_for_class(wh_c);
        let center = DVec3::from_array(site_pos);
        let mut spawned_ids = Vec::new();
        for (idx, tid) in std::iter::once(trigger_tid)
            .chain(other_tids)
            .enumerate()
        {
            let bid = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                snap.next_entity_id
            });
            let pos = center
                + DVec3::new(
                    ((idx as f64) - 1.0) * 2_500.0,
                    ((idx % 2) as f64) * 1_500.0,
                    0.0,
                );
            let ball = Ball {
                id: bid,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 350.0,
                inertia: 0.8,
                mass: 5_000_000.0,
                radius: 45.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            };
            let sde_t = ctx.sde.types.get(&tid);
            sys_handle.try_spawn_ball_with_meta(
                ball,
                build_type_combat_state_from_sde(&ctx.sde, tid),
                SlimItemMeta {
                    group_id: sde_t.map(|t| t.group_id).unwrap_or(0),
                    category_id: sde_t.map(|t| t.category_id).unwrap_or(11),
                    ..Default::default()
                },
            );
            ctx.combat_ephemeral
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .npc_entities
                .insert(
                    bid,
                    ExtendedNpcEntity {
                        ball_id: bid,
                        type_id: tid,
                        faction_id: 500_005,
                        archetype: "Sleeper".into(),
                        solar_system_id,
                        spawn_group_id: 0,
                        target_id: None,
                        aggression_threat: HashMap::new(),
                        threat_table: HashMap::new(),
                        bounty_isk: 0.0,
                        optimal_range: 20_000.0,
                        orbit_range: 15_000.0,
                        damage_vector: DamageVector {
                            em: 80.0,
                            thermal: 80.0,
                            kinetic: 80.0,
                            explosive: 80.0,
                        },
                        entropic_cycles: 0,
                        drifter_overshield_hp: 0.0,
                        drifter_doomsday_fired: false,
                        web_speed_factor: ctx
                            .sde
                            .types
                            .get(&tid)
                            .and_then(|t| t.default_attributes.get(&20).copied())
                            .filter(|&v| v < 0.0)
                            .unwrap_or(-50.0),
                        web_range: 15_000.0,
                        scram_strength: 2.0,
                        scram_range: 12_000.0,
                        neut_amount: 120.0,
                        ecm_jammed: false,
                        remote_shield_rep: 0.0,
                        remote_armor_rep: 0.0,
                        remote_rep_range: 0.0,
                        tag_drop_type_id: None,
                        spawn_position: pos,
                        sleeper_wh_class: wh_c,
                        custom_cargo_loot: None,
                    },
                );
            spawned_ids.push(bid);
        }
        let sbid = ctx.db.with_mut_snapshot(|snap| {
            snap.next_entity_id += 1;
            snap.next_entity_id
        });
        let (sball, scs, sentity) = build_sleeper_sentry_entity(
            &ctx.sde,
            solar_system_id,
            sbid,
            sentry_tid,
            center + DVec3::new(3_000.0, 0.0, 2_500.0),
            wh_c,
        );
        let sentry_sde = ctx.sde.types.get(&sentry_tid);
        sys_handle.try_spawn_ball_with_meta(
            sball,
            scs,
            SlimItemMeta {
                group_id: sentry_sde.map(|t| t.group_id).unwrap_or(383),
                category_id: sentry_sde.map(|t| t.category_id).unwrap_or(11),
                ..Default::default()
            },
        );
        ctx.combat_ephemeral
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .npc_entities
            .insert(sbid, sentity);
        spawned_ids.push(sbid);

        with_pve_exploration_reg(ctx, |reg| {
            if let Some(site) = reg
                .sites_by_system
                .get_mut(&solar_system_id)
                .and_then(|l| l.iter_mut().find(|s| s.site_id == site_id))
            {
                site.active_npc_ball_ids = spawned_ids;
                site.npc_guards_spawned = true;
            }
        });
    }
}

/// Processes downtime maintenance for W-space PvE sites:
/// - Resets per-site capital escalation counters (`dread_escalation_triggers`, `carrier_escalation_triggers`)
///   so escalation waves regenerate after downtime (`feat:pve.wormhole-capital-escalations`,
///   `feat:pve.wormhole-site-capital-escalation-cap`).
/// - Despawns activated W-space sites at the 3rd downtime after activation if completed, and at the
///   4th downtime after activation if left unfinished (`feat:pve.wormhole-site-downtime-despawn`).
pub fn process_wormhole_sites_on_downtime(ctx: &ServerContext) -> usize {
    let despawned_sites: Vec<(SolarSystemId, String, [f64; 3], Vec<i64>, Vec<ItemId>)> =
        with_pve_exploration_reg(ctx, |reg| {
            reg.downtime_counter = reg.downtime_counter.saturating_add(1);
            let dt = reg.downtime_counter;
            let mut removed = Vec::new();
            for (&sys_id, sites) in reg.sites_by_system.iter_mut() {
                sites.retain_mut(|site| {
                    if site.wormhole_class.is_none() {
                        return true;
                    }
                    // Regenerate capital escalation triggers at downtime
                    site.dread_escalation_triggers = 0;
                    site.carrier_escalation_triggers = 0;

                    let Some(act_dt) = site.activated_at_downtime else {
                        return true;
                    };
                    let elapsed_dt = dt.saturating_sub(act_dt);
                    let should_despawn = if site.cleared {
                        elapsed_dt >= 3
                    } else {
                        elapsed_dt >= 4
                    };
                    if should_despawn {
                        let mut ball_ids = std::mem::take(&mut site.active_npc_ball_ids);
                        for cap_entry in &site.active_escalation_capitals {
                            ball_ids.extend(cap_entry.avenger_ball_ids.iter().copied());
                        }
                        removed.push((
                            sys_id,
                            site.site_id.clone(),
                            site.position,
                            ball_ids,
                            std::mem::take(&mut site.hackable_can_ids),
                        ));
                        false
                    } else {
                        true
                    }
                });
            }
            removed
        });

    if despawned_sites.is_empty() {
        return 0;
    }

    let mut per_sys_removed_ids: HashMap<SolarSystemId, Vec<String>> = HashMap::new();
    let mut all_removed_balls: Vec<(SolarSystemId, i64)> = Vec::new();
    let mut removed_ast_balls: Vec<(SolarSystemId, i64)> = Vec::new();

    ctx.db.with_mut_snapshot(|snap| {
        for (sys_id, site_id, pos, ball_ids, can_ids) in &despawned_sites {
            if let Some(anoms) = snap.cosmic_anomalies.get_mut(sys_id) {
                anoms.retain(|a| &a.anomaly_id != site_id);
            }
            if let Some(sigs) = snap.cosmic_signatures.get_mut(sys_id) {
                sigs.retain(|s| &s.signature_id != site_id);
            }
            for &cid in can_ids {
                snap.items.remove(&cid);
                snap.item_positions.remove(&cid);
                all_removed_balls.push((*sys_id, cid));
            }
            for &bid in ball_ids {
                all_removed_balls.push((*sys_id, bid));
            }
            let site_vec = DVec3::from_array(*pos);
            snap.asteroids.retain(|&aid, a| {
                let is_site_ast = a.solar_system_id == *sys_id
                    && a.belt_id == 0
                    && DVec3::new(a.position.0, a.position.1, a.position.2).distance(site_vec)
                        <= 50_000.0;
                if is_site_ast {
                    removed_ast_balls.push((*sys_id, aid));
                }
                !is_site_ast
            });
            per_sys_removed_ids
                .entry(*sys_id)
                .or_default()
                .push(site_id.clone());
        }
    });

    {
        let mut rt = ctx
            .combat_ephemeral
            .write()
            .unwrap_or_else(|e| e.into_inner());
        for (sys_id, bid) in all_removed_balls.iter().chain(removed_ast_balls.iter()) {
            rt.npc_entities.remove(bid);
            let sys_handle = ctx.systems.get_or_spawn(*sys_id);
            let mut bp = sys_handle.ballpark.write().unwrap_or_else(|e| e.into_inner());
            let _ = bp.remove_ball(*bid);
        }
    }

    for (sys_id, ids) in per_sys_removed_ids {
        crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_anomaly_delta(
            ctx,
            sys_id,
            &[],
            &ids,
        );
        crate::services::fleet_social::fleet_social_scanning::emit_signal_tracker_signature_delta(
            ctx,
            sys_id,
            &[],
            &ids,
        );
    }

    despawned_sites.len()
}
