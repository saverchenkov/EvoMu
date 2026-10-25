//! `combat_npc_encounters` — Multi-Wave NPC Encounter Sites & Destruction Triggers (`feat:npc.npc-spawn-encounter`).
//!
//! Handles:
//! - Wave-based NPC encounter sites where destroying the designated trigger NPC in space
//!   spawns the next wave N+1 on the next tick with authentic NPC types from the encounter definition.
//! - Elimination of the final wave marks the encounter site complete.
//! - Real weapon/damage destruction path hook integration (`apply_damage_to_ball` / `step_combat_system`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity;
use crate::services::combat_crimewatch::{with_mut_combat_runtime, CombatRuntimeState, EncounterWaveDirector, ExtendedNpcEntity};
use crate::system_actor::SystemRegistry;
use eve_db::WorldRepository;
use eve_sde::{SdeRegistry, SolarSystemId};
use glam::DVec3;
use std::sync::{Arc, RwLock};

/// Definition of a single wave in an NPC encounter site.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EncounterWaveDefinition {
    pub wave_number: u32,
    pub npc_type_ids: Vec<u32>,
    pub trigger_index: usize,
}

/// Creates a multi-wave NPC encounter site with authentic NPC types from SDE (`feat:npc.npc-spawn-encounter`).
pub async fn create_encounter_wave_site(
    ctx: &ServerContext,
    encounter_id: u32,
    solar_system_id: SolarSystemId,
    npc_type_id: u32,
    max_waves: u32,
    pos: DVec3,
) -> Vec<i64> {
    // Generate wave definitions: wave 1 uses npc_type_id, subsequent waves derive authentic types from SDE spawn groups
    let mut waves = Vec::new();
    let num_waves = max_waves.max(1);

    // Wave 1: initial types
    waves.push(vec![npc_type_id, npc_type_id]);

    // Subsequent waves: derive authentic types from SDE spawn groups
    let candidate_types: Vec<u32> = ctx
        .sde
        .npc_spawn_groups
        .values()
        .flat_map(|sg| sg.npc_type_ids.iter().copied())
        .filter(|&tid| tid != npc_type_id)
        .take(10)
        .collect();

    for w in 2..=num_waves {
        let t1 = candidate_types.get(((w as usize) * 2) % candidate_types.len().max(1)).copied().unwrap_or(npc_type_id);
        let t2 = candidate_types.get(((w as usize) * 2 + 1) % candidate_types.len().max(1)).copied().unwrap_or(npc_type_id);
        waves.push(vec![t1, t2]);
    }

    create_custom_encounter_site(ctx, encounter_id, solar_system_id, waves, pos).await
}

/// Creates an encounter site with explicit wave definitions containing authentic NPC types.
pub async fn create_custom_encounter_site(
    ctx: &ServerContext,
    encounter_id: u32,
    solar_system_id: SolarSystemId,
    waves: Vec<Vec<u32>>,
    pos: DVec3,
) -> Vec<i64> {
    if waves.is_empty() {
        return Vec::new();
    }
    let max_waves = waves.len() as u32;
    let wave1_types = &waves[0];

    let mut wave1_balls = Vec::new();
    for (i, &tid) in wave1_types.iter().enumerate() {
        let offset = DVec3::new((i as f64) * 1_000.0, 0.0, 0.0);
        let bid = spawn_specialized_npc_entity(
            ctx,
            solar_system_id,
            tid,
            "BeltRat",
            encounter_id,
            pos + offset,
        )
        .await;
        wave1_balls.push(bid);
    }

    let trigger_ball_id = wave1_balls.first().copied().unwrap_or(0);
    let npc_type_id = wave1_types.first().copied().unwrap_or(0);

    with_mut_combat_runtime(ctx, |rt| {
        rt.encounter_waves.insert(
            encounter_id,
            EncounterWaveDirector {
                encounter_id,
                solar_system_id,
                current_wave: 1,
                max_waves,
                trigger_ball_id,
                npc_type_id,
                active_wave_ball_ids: wave1_balls.clone(),
                completed: false,
                wave_npc_types: waves,
                origin_position: pos,
                next_wave_pending: false,
            },
        );
    });

    wave1_balls
}

/// Checks if an encounter wave trigger was destroyed, spawning next wave N+1 or marking site complete (`feat:npc.npc-spawn-encounter`).
pub async fn step_encounter_wave_on_kill(
    ctx: &ServerContext,
    solar_system_id: SolarSystemId,
    killed_ball_id: i64,
    pos: DVec3,
) {
    let triggered_info: Option<(u32, u32, Vec<u32>, DVec3)> = with_mut_combat_runtime(ctx, |rt| {
        let mut spawn_req = None;
        for enc in rt.encounter_waves.values_mut() {
            if enc.solar_system_id == solar_system_id && !enc.completed {
                enc.active_wave_ball_ids.retain(|&id| id != killed_ball_id);
                let is_trigger = killed_ball_id == enc.trigger_ball_id;

                if is_trigger || enc.active_wave_ball_ids.is_empty() {
                    if enc.current_wave < enc.max_waves {
                        enc.current_wave += 1;
                        let next_idx = (enc.current_wave - 1) as usize;
                        let next_types = enc
                            .wave_npc_types
                            .get(next_idx)
                            .cloned()
                            .unwrap_or_else(|| vec![enc.npc_type_id, enc.npc_type_id]);
                        spawn_req = Some((enc.encounter_id, enc.current_wave, next_types, enc.origin_position));
                    } else if enc.active_wave_ball_ids.is_empty() {
                        enc.completed = true;
                    }
                }
            }
        }
        spawn_req
    });

    if let Some((enc_id, _wave_num, next_types, base_pos)) = triggered_info {
        let mut new_balls = Vec::new();
        for (i, &tid) in next_types.iter().enumerate() {
            let offset = DVec3::new(500.0 + (i as f64) * 1_000.0, 0.0, 0.0);
            let nb = spawn_specialized_npc_entity(
                ctx,
                solar_system_id,
                tid,
                "BeltRat",
                enc_id,
                pos.lerp(base_pos, 0.5) + offset,
            )
            .await;
            new_balls.push(nb);
        }
        with_mut_combat_runtime(ctx, |rt| {
            if let Some(enc) = rt.encounter_waves.get_mut(&enc_id) {
                enc.trigger_ball_id = new_balls.first().copied().unwrap_or(0);
                enc.active_wave_ball_ids.extend(new_balls);
            }
        });
    }
}

/// Invoked from `npc_death_hook` when any NPC ball is destroyed in space by damage (`apply_damage_to_ball` / `step_combat_system`).
pub async fn handle_npc_destroyed_in_space(
    systems: &Arc<SystemRegistry>,
    db: &Arc<WorldRepository>,
    _sde: &Arc<SdeRegistry>,
    combat_ephemeral: &Arc<RwLock<CombatRuntimeState>>,
    solar_system_id: SolarSystemId,
    killed_ball_id: i64,
    killer_char_id: Option<i64>,
) {
    let killer = killer_char_id.unwrap_or(0);

    // 1. Advance encounter waves if killed_ball_id belongs to an active encounter
    let triggered_info: Option<(u32, u32, Vec<u32>, DVec3)> = {
        let mut rt = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
        let mut spawn_req = None;
        for enc in rt.encounter_waves.values_mut() {
            if enc.solar_system_id == solar_system_id && !enc.completed {
                enc.active_wave_ball_ids.retain(|&id| id != killed_ball_id);
                let is_trigger = killed_ball_id == enc.trigger_ball_id;

                if is_trigger || enc.active_wave_ball_ids.is_empty() {
                    if enc.current_wave < enc.max_waves {
                        enc.current_wave += 1;
                        let next_idx = (enc.current_wave - 1) as usize;
                        let next_types = enc
                            .wave_npc_types
                            .get(next_idx)
                            .cloned()
                            .unwrap_or_else(|| vec![enc.npc_type_id, enc.npc_type_id]);
                        spawn_req = Some((enc.encounter_id, enc.current_wave, next_types, enc.origin_position));
                    } else if enc.active_wave_ball_ids.is_empty() {
                        enc.completed = true;
                    }
                }
            }
        }
        spawn_req
    };

    if let Some((enc_id, _wave_num, next_types, base_pos)) = triggered_info {
        let handle = systems.get_or_spawn(solar_system_id);
        let mut new_balls = Vec::new();
        for (i, &tid) in next_types.iter().enumerate() {
            let offset = DVec3::new(500.0 + (i as f64) * 1_000.0, 0.0, 0.0);
            let pos = base_pos + offset;
            let ball_id = db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                snap.next_entity_id
            });

            let ball = eve_destiny::Ball {
                id: ball_id,
                type_id: tid,
                position: pos,
                velocity: DVec3::ZERO,
                max_velocity: 250.0,
                inertia: 2.0,
                mass: 500_000.0,
                radius: 30.0,
                speed_fraction: 0.0,
                mode: eve_destiny::MovementMode::Stop,
            };

            let _ = handle.spawn_ball_with_combat(
                ball,
                eve_destiny::CombatState {
                    shield_hp: 500.0,
                    max_shield_hp: 500.0,
                    armor_hp: 500.0,
                    max_armor_hp: 500.0,
                    structure_hp: 500.0,
                    max_structure_hp: 500.0,
                    ..Default::default()
                },
            ).await;

            {
                let mut rt = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
                rt.npc_entities.insert(
                    ball_id,
                    ExtendedNpcEntity {
                        ball_id,
                        type_id: tid,
                        faction_id: 500_010,
                        archetype: "BeltRat".into(),
                        solar_system_id,
                        spawn_group_id: enc_id,
                        target_id: None,
                        aggression_threat: std::collections::HashMap::new(),
                        threat_table: std::collections::HashMap::new(),
                        bounty_isk: 50_000.0,
                        optimal_range: 15_000.0,
                        orbit_range: 5_000.0,
                        damage_vector: eve_destiny::DamageVector {
                            em: 25.0,
                            thermal: 25.0,
                            kinetic: 25.0,
                            explosive: 25.0,
                        },
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
                        spawn_position: pos,
                        sleeper_wh_class: 0,
                        custom_cargo_loot: None,
                    },
                );
            }

            new_balls.push(ball_id);
        }

        {
            let mut rt = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
            if let Some(enc) = rt.encounter_waves.get_mut(&enc_id) {
                enc.trigger_ball_id = new_balls.first().copied().unwrap_or(0);
                enc.active_wave_ball_ids.extend(new_balls);
            }
        }
    }

    // 2. Remove from npc_entities and award bounty if present
    let npc_opt = {
        let mut rt = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
        rt.npc_entities.remove(&killed_ball_id)
    };
    if let Some(npc) = npc_opt {
        if npc.bounty_isk > 0.0 && killer > 0 {
            let mut rt = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
            *rt.pending_bounty_ticks
                .entry((solar_system_id, killer))
                .or_insert(0.0) += npc.bounty_isk;
        }
    }
}
