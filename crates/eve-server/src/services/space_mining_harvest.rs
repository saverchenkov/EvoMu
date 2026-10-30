//! `space_mining_harvest` — Gameplay progression event dispatch on mining harvest / yield.

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::quasar_campaign_state::CampaignEvent;

/// Concrete authentic contribution method for harvesting a type of SDE group `group_id`:
/// Ice (465) -> `MineIce`, Harvestable Cloud (711) -> `MineGas`, moon ore groups (1884, 1920-1923)
/// -> `MineMoon`, any other ore -> `MineAsteroid`.
pub fn mining_contribution_method(group_id: u32) -> &'static str {
    match group_id {
        465 => "MineIce",
        711 => "MineGas",
        1884 | 1920..=1923 => "MineMoon",
        _ => "MineAsteroid",
    }
}

pub fn dispatch_mining_gameplay_events(ctx: &ServerContext, char_id: i64, units: u64, sys_id: u32, ore_type_id: u32) {
    if units > 0 {
        ctx.quasar_gateway
            .progression
            .daily_goals
            .on_gameplay_event(char_id as u32, 2, units, &ctx.quasar_gateway);
        ctx.quasar_gateway
            .progression
            .career_goals
            .on_gameplay_event(char_id as u32, "mining", units, &ctx.quasar_gateway);
        ctx.quasar_gateway
            .progression
            .extra
            .on_campaign_gameplay_event(
                char_id as u32,
                CampaignEvent {
                    method: mining_contribution_method(ctx.sde.types.get(&ore_type_id).map(|t| t.group_id).unwrap_or(0)),
                    type_id: Some(ore_type_id),
                    solar_system_id: Some(sys_id),
                    faction_id: None,
                },
                units,
                ctx.now_unix(),
                &ctx.quasar_gateway,
            );
        crate::services::social_corp_fleet::social_corp_projects::record_server_project_activity(
            ctx,
            char_id,
            &["MineMaterials"],
            units,
            Some(ore_type_id as i64),
        );
        let ore_vol = ctx
            .sde
            .types
            .get(&ore_type_id)
            .map(|t| t.volume.max(0.1))
            .unwrap_or(1.0)
            * (units as f64);
        crate::services::sov_gameplay::record_sov_system_activity(ctx, sys_id, 0, ore_vol);
    }
}

pub fn deposit_mined_ore_and_record_ledger(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    target_flag: eve_db::InventoryFlag,
    ore_type_id: u32,
    mined_units: u64,
) {
    deposit_mined_ore_and_record_ledger_with_waste(
        ctx,
        char_id,
        ship_id,
        sys_id,
        target_flag,
        ore_type_id,
        mined_units,
        0,
    );
}

pub fn deposit_mined_ore_and_record_ledger_with_waste(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    target_flag: eve_db::InventoryFlag,
    ore_type_id: u32,
    mined_units: u64,
    wasted_units: u64,
) {
    if mined_units > 0
        && ctx
            .db
            .add_or_stack_item(char_id, ship_id, target_flag, ore_type_id, mined_units as i64)
            .is_ok()
    {
        let now_unix = ctx.now_unix_secs();
        let _ = ctx.db.record_mining_ledger_entry_with_waste(
            char_id,
            sys_id,
            ore_type_id,
            mined_units as i64,
            wasted_units as i64,
            now_unix,
        );
        ctx.db.with_mut_snapshot(|snap| {
            let belt_obs = snap
                .asteroids
                .values()
                .find(|a| a.solar_system_id == sys_id && a.type_id == ore_type_id && a.belt_id > 0)
                .map(|a| a.belt_id)
                .filter(|id| !snap.structures.get(id).is_some_and(|s| s.solar_system_id == sys_id));
            if let Some(obs_id) = belt_obs {
                let corp_id = snap.characters.get(&char_id).map(|c| c.corporation_id).unwrap_or(0);
                snap.observer_mining_ledger.entry(obs_id).or_default().push(eve_db::ObserverMiningRecord {
                    observer_id: obs_id,
                    character_id: char_id,
                    recorded_corporation_id: corp_id,
                    type_id: ore_type_id,
                    quantity: mined_units as i64,
                    last_updated_date: eve_db::unix_secs_to_date_string(now_unix),
                    timestamp_unix: now_unix,
                });
            }
        });
        dispatch_mining_gameplay_events(ctx, char_id, mined_units, sys_id, ore_type_id);
    }
}

/// Seeds or respawns the regional `"IceBelt"` cosmic anomaly and its SDE ice asteroids in a
/// designated ice solar system (`plan:MN-08`, 4-hour = `14,400`s respawn after depletion).
pub fn tick_ice_belt_respawn_sync(ctx: &ServerContext, sys_id: u32) {
    if !crate::services::mining_sites::is_designated_ice_system(sys_id) {
        return;
    }
    let Some(Ok(site)) = crate::services::mining_sites::resolve_regional_ice_belt(&ctx.sde, sys_id)
    else {
        return;
    };
    if !site
        .asteroids
        .iter()
        .all(|a| ctx.sde.types.contains_key(&a.type_id))
    {
        return;
    }
    let type_vol_rad = |tid: u32| -> (f64, f64) {
        ctx.sde
            .types
            .get(&tid)
            .map(|t| (t.volume.max(0.001), t.radius.max(50.0)))
            .unwrap_or((0.001, 50.0))
    };
    let ice_pos =
        crate::services::mining_sites::derive_system_site_position_from_sde(ctx, sys_id, 4, 10.5);
    let ice_pos_vec = glam::DVec3::from_array(ice_pos);
    let now = ctx.now_unix_secs();

    let (old_ids, newly_spawned) = ctx.db.with_mut_snapshot(|snap| {
        let mut newly_cleared_ids = Vec::new();
        if let Some(anoms) = snap.cosmic_anomalies.get_mut(&sys_id) {
            for anom in anoms.iter_mut().filter(|a| a.anomaly_type == "IceBelt" && !a.cleared) {
                let anom_pos = glam::DVec3::from_array(anom.position);
                let any_remaining = snap.asteroids.values().any(|a| {
                    a.solar_system_id == sys_id
                        && a.quantity > 0
                        && glam::DVec3::new(a.position.0, a.position.1, a.position.2)
                            .distance(anom_pos)
                            <= 50_000.0
                });
                if !any_remaining {
                    anom.cleared = true;
                    anom.remaining_volume = 0.0;
                    anom.created_unix = now;
                    anom.lifetime_seconds =
                        crate::services::mining_sites::ICE_BELT_RESPAWN_DELAY_SECS;
                    newly_cleared_ids.push(anom.anomaly_id.clone());
                }
            }
        }
        if !newly_cleared_ids.is_empty() {
            if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
                for s in sigs.iter_mut().filter(|s| newly_cleared_ids.contains(&s.signature_id)) {
                    s.collapsed = true;
                    s.signal_strength = 0.0;
                }
            }
        }

        let has_any_ice = snap
            .cosmic_anomalies
            .get(&sys_id)
            .is_some_and(|list| list.iter().any(|a| a.anomaly_type == "IceBelt"));
        let has_uncleared = snap
            .cosmic_anomalies
            .get(&sys_id)
            .is_some_and(|list| list.iter().any(|a| a.anomaly_type == "IceBelt" && !a.cleared));
        let ready_to_respawn = !has_uncleared
            && snap.cosmic_anomalies.get(&sys_id).is_some_and(|list| {
                list.iter().any(|a| {
                    a.anomaly_type == "IceBelt"
                        && a.cleared
                        && crate::services::mining_sites::is_ice_belt_ready_to_respawn(
                            now.saturating_sub(a.created_unix),
                        )
                })
            });
        if has_any_ice && !ready_to_respawn {
            return (Vec::new(), Vec::new());
        }

        let mut old_ids = Vec::new();
        snap.asteroids.retain(|&aid, a| {
            let remove = a.solar_system_id == sys_id
                && a.belt_id == 0
                && glam::DVec3::new(a.position.0, a.position.1, a.position.2)
                    .distance(ice_pos_vec)
                    <= 50_000.0;
            if remove {
                old_ids.push(aid);
            }
            !remove
        });
        if let Some(anoms) = snap.cosmic_anomalies.get_mut(&sys_id) {
            anoms.retain(|a| a.anomaly_type != "IceBelt");
        }
        if let Some(sigs) = snap.cosmic_signatures.get_mut(&sys_id) {
            sigs.retain(|s| s.sig_group != "IceBelt");
        }

        snap.next_entity_id += 1;
        let anom_id = format!("DNG-{}-{}", site.dungeon_id, snap.next_entity_id);
        let mut volume = 0.0_f64;
        let mut spawned = Vec::with_capacity(site.asteroids.len());
        for (i, a) in site.asteroids.iter().enumerate() {
            let off = crate::services::mining_sites::asteroid_offset(i);
            let pos = [ice_pos[0] + off[0], ice_pos[1] + off[1], ice_pos[2] + off[2]];
            let (uvol, rad) = type_vol_rad(a.type_id);
            volume += a.units as f64 * uvol;
            snap.next_entity_id += 1;
            let aid = snap.next_entity_id;
            snap.asteroids.insert(
                aid,
                eve_db::AsteroidRecord {
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
            spawned.push((
                aid,
                a.type_id,
                a.units as u64,
                uvol,
                rad,
                glam::DVec3::from_array(pos),
            ));
        }
        snap.cosmic_anomalies
            .entry(sys_id)
            .or_default()
            .push(eve_db::CosmicAnomalyRecord {
                anomaly_id: anom_id.clone(),
                solar_system_id: sys_id,
                name: site.name.clone(),
                anomaly_type: "IceBelt".into(),
                position: ice_pos,
                signal_strength: 1.0,
                ore_types: site.ore_types(),
                remaining_volume: volume,
                created_unix: now,
                lifetime_seconds: crate::services::mining_sites::ICE_BELT_RESPAWN_DELAY_SECS,
                cleared: false,
            });
        snap.cosmic_signatures
            .entry(sys_id)
            .or_default()
            .push(eve_db::CosmicSignatureRecord {
                signature_id: anom_id,
                solar_system_id: sys_id,
                sig_group: "IceBelt".into(),
                type_name: site.name.clone(),
                signal_strength: 1.0,
                position: ice_pos,
                wormhole_type_id: None,
                destination_system_id: None,
                created_unix: now,
                lifetime_seconds: crate::services::mining_sites::ICE_BELT_RESPAWN_DELAY_SECS,
                max_mass_kg: 0.0,
                remaining_mass_kg: 0.0,
                max_jump_mass_kg: 0.0,
                collapsed: false,
            });
        (old_ids, spawned)
    });

    if !old_ids.is_empty() || !newly_spawned.is_empty() {
        let handle = ctx.systems.get_or_spawn(sys_id);
        for old_id in old_ids {
            handle.try_remove_ball(old_id);
        }
        for (aid, tid, units, uvol, rad, pos) in newly_spawned {
            let ball = eve_destiny::Ball {
                id: aid,
                type_id: tid,
                position: pos,
                velocity: glam::DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 80_000.0,
                radius: rad,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            };
            let st = crate::system_actor::AsteroidState {
                asteroid_id: aid,
                belt_id: 0,
                type_id: tid,
                initial_units: units,
                remaining_units: units,
                unit_volume: uvol,
                depleted: false,
                respawn_at_tick: None,
            };
            handle.try_spawn_asteroid(st, ball);
        }
    }
}

/// Evaluates 4-hour ice belt respawn timers across all systems with a depleted `"IceBelt"` anomaly.
pub fn tick_all_ice_belt_respawns_sync(ctx: &ServerContext) {
    let sys_ids: Vec<u32> = ctx.db.with_snapshot(|snap| {
        snap.cosmic_anomalies
            .iter()
            .filter_map(|(&sys_id, anoms)| {
                if anoms.iter().any(|a| a.anomaly_type == "IceBelt" && a.cleared) {
                    Some(sys_id)
                } else {
                    None
                }
            })
            .collect()
    });
    for sys_id in sys_ids {
        tick_ice_belt_respawn_sync(ctx, sys_id);
    }
}

pub const WORMHOLE_GUARD_TYPE_IDS: &[eve_sde::TypeId] = &[30209, 30210, 30211, 30200, 30203, 30196];

pub fn build_wormhole_guard_wave(
    next_entity_id: &mut i64,
    centre: [f64; 3],
    guards: &[(eve_sde::TypeId, u32)],
    out: &mut Vec<(i64, eve_sde::TypeId, glam::DVec3)>,
) {
    let total: u32 = guards.iter().map(|&(_, c)| c).sum();
    let mut idx: u32 = 0;
    for &(npc_tid, count) in guards {
        for _ in 0..count {
            *next_entity_id += 1;
            let angle = (idx as f64) * std::f64::consts::TAU / (total.max(1) as f64);
            idx += 1;
            out.push((
                *next_entity_id,
                npc_tid,
                glam::DVec3::new(centre[0] + angle.cos() * 4_500.0, centre[1] + 500.0, centre[2] + angle.sin() * 4_500.0),
            ));
        }
    }
}

pub async fn spawn_wormhole_ore_guards(
    ctx: &ServerContext,
    sys_id: u32,
    guard_waves: Vec<(i64, eve_sde::TypeId, glam::DVec3)>,
) {
    if guard_waves.is_empty() {
        return;
    }
    let handle = ctx.systems.get_or_spawn(sys_id);
    for (gid, npc_tid, gpos) in guard_waves {
        let npc_rec = ctx.sde.types.get(&npc_tid);
        let npc_rad = npc_rec.map(|t| t.radius.max(25.0)).unwrap_or(45.0);
        let npc_mass = npc_rec.map(|t| t.mass.max(10_000.0)).unwrap_or(1_200_000.0);
        let max_vel = npc_rec
            .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::MAX_VELOCITY))
            .copied()
            .unwrap_or(320.0);
        // DERIVED: Sleeper NPC types lack dogma attr 70 (agility) in SDE; default 1.0 matches space_destiny.rs.
        let inertia = npc_rec
            .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::AGILITY))
            .copied()
            .unwrap_or(1.0);
        let ball = eve_destiny::Ball {
            id: gid, type_id: npc_tid, position: gpos, velocity: glam::DVec3::ZERO,
            max_velocity: max_vel, inertia, mass: npc_mass, radius: npc_rad,
            speed_fraction: 0.0, mode: eve_destiny::MovementMode::Stop,
        };
        let _ = handle.spawn_ball_with_combat(ball, ctx.build_type_combat_state(npc_tid)).await;
    }
}

pub async fn respawn_wormhole_ore_guards_if_missing(
    ctx: &ServerContext,
    sys_id: u32,
    sites: &[([f64; 3], &'static crate::services::mining_sites::WormholeOreSiteDef)],
) {
    if sites.is_empty() {
        return;
    }
    let handle = ctx.systems.get_or_spawn(sys_id);
    let existing = handle.list_balls().await.unwrap_or_default();
    if existing.iter().any(|b| WORMHOLE_GUARD_TYPE_IDS.contains(&b.type_id)) {
        return;
    }
    let waves = ctx.db.with_mut_snapshot(|snap| {
        let mut out = Vec::new();
        let anoms = snap.cosmic_anomalies.get(&sys_id).cloned().unwrap_or_default();
        for &(fallback_centre, def) in sites {
            let centre = anoms
                .iter()
                .find(|a| a.name == def.dungeon_name)
                .map(|a| a.position)
                .unwrap_or(fallback_centre);
            build_wormhole_guard_wave(&mut snap.next_entity_id, centre, def.guards, &mut out);
        }
        out
    });
    spawn_wormhole_ore_guards(ctx, sys_id, waves).await;
}

pub async fn spawn_seeded_site_entities(
    ctx: &ServerContext,
    sys_id: u32,
    newly_spawned: Vec<(i64, u32, u64, f64, f64, glam::DVec3)>,
    guard_waves: Vec<(i64, u32, glam::DVec3)>,
) {
    if !newly_spawned.is_empty() {
        let handle = ctx.systems.get_or_spawn(sys_id);
        for (aid, tid, units, uvol, rad, pos) in newly_spawned {
            let ball = eve_destiny::Ball {
                id: aid,
                type_id: tid,
                position: pos,
                velocity: glam::DVec3::ZERO,
                max_velocity: 0.0,
                inertia: 1.0,
                mass: 50_000.0,
                radius: rad,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            };
            let st = crate::system_actor::AsteroidState {
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
    spawn_wormhole_ore_guards(ctx, sys_id, guard_waves).await;
}

/// Phased Asteroid Field site lifecycle (`feat:mining.phased-asteroid-field-lifecycle`, `feat:mining.mobile-phase-anchor`).
#[derive(Debug, Clone)]
pub struct PhasedAsteroidFieldState {
    pub signature_id: String,
    pub solar_system_id: u32,
    pub progress: f64,
    pub beacon_spawned: bool,
    pub collapsed: bool,
    pub phased_in: bool,
    pub mobile_phase_anchor_energy: u32,
}

impl PhasedAsteroidFieldState {
    pub fn new(signature_id: String, solar_system_id: u32) -> Self {
        Self {
            signature_id,
            solar_system_id,
            progress: 0.1,
            beacon_spawned: false,
            collapsed: false,
            phased_in: false,
            mobile_phase_anchor_energy: 0,
        }
    }

    pub fn advance_fracture(&mut self, delta_progress: f64) {
        self.progress = (self.progress + delta_progress).min(1.0);
        if self.progress >= 0.5 {
            self.beacon_spawned = true;
        }
        if self.progress >= 1.0 {
            self.collapsed = true;
            self.phased_in = false;
        }
    }

    pub fn connect_industrial_command_ship(&mut self, ship_type_id: eve_sde::TypeId) -> bool {
        let energy = match ship_type_id {
            28352 => 100, // Rorqual
            28606 => 50,  // Orca
            42244 => 35,  // Porpoise
            _ => 0,
        };
        self.mobile_phase_anchor_energy += energy;
        if self.mobile_phase_anchor_energy >= 100 && !self.collapsed {
            self.phased_in = true;
        }
        self.phased_in
    }

    pub fn mining_efficiency(&self) -> f64 {
        if self.collapsed {
            0.0
        } else if self.phased_in {
            1.0
        } else {
            0.10
        }
    }
}
