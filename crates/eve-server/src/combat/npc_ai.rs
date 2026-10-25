//! `npc_ai` — Belt/gate NPC spawns from SDE spawn groups, NPC AI state machine (Idle -> Chase -> Orbit -> Attack -> Reps), and dynamic bounty payout (`C-4`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, IskCents, KillmailRecord, WalletJournalEntry};
use eve_destiny::{Ball, DamageVector, MovementMode};
use glam::DVec3;
use std::collections::HashMap;

/// AI state machine for spawned NPC combat ships.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NpcAiState {
    Idle,
    Chase,
    Orbit,
    Attack,
}

/// Computes faction-specific damage profile (`feat:npc.npc-damage-types`).
pub fn faction_damage_vector(faction_id: u32, total_dps: f64) -> DamageVector {
    match faction_id {
        500010 | 500001 => {
            // Guristas / Caldari: 70% Kinetic, 30% Thermal
            DamageVector::new(0.0, total_dps * 0.30, total_dps * 0.70, 0.0)
        }
        500012 | 500019 | 500003 => {
            // Blood Raiders / Sansha's Nation / Amarr: 60% EM, 40% Thermal
            DamageVector::new(total_dps * 0.60, total_dps * 0.40, 0.0, 0.0)
        }
        500020 | 500004 => {
            // Serpentis / Gallente: 60% Thermal, 40% Kinetic
            DamageVector::new(0.0, total_dps * 0.60, total_dps * 0.40, 0.0)
        }
        500011 | 500002 => {
            // Angel Cartel / Minmatar: 60% Explosive, 30% Kinetic, 10% Thermal
            DamageVector::new(0.0, total_dps * 0.10, total_dps * 0.30, total_dps * 0.60)
        }
        _ => {
            let q = total_dps * 0.25;
            DamageVector::new(q, q, q, q)
        }
    }
}

/// Computes character security status gain from destroying an NPC pirate (`feat:crimewatch.sec-status-gain`).
pub fn compute_sec_status_gain(current_sec: f64, bounty: f64) -> f64 {
    if current_sec >= 5.0 || bounty <= 0.0 {
        return 0.0;
    }
    let headroom = (5.0 - current_sec).max(0.0);
    let scale = (bounty / 500_000.0).clamp(0.01, 0.10);
    (headroom * 0.01 * scale).clamp(0.001, 0.5)
}

/// In-memory runtime tracking for an active NPC combat entity in a solar system.
#[derive(Debug, Clone)]
pub struct NpcCombatEntity {
    pub ball_id: i64,
    pub type_id: u32,
    pub faction_id: u32,
    pub state: NpcAiState,
    pub spawn_group_id: u32,
    pub spawn_pos: DVec3,
    pub target_id: Option<i64>,
    pub bounty_isk: f64,
    pub optimal_range: f64,
    pub orbit_range: f64,
    pub base_dps: f64,
    pub damage_vector: DamageVector,
    pub last_attack_tick: u64,
    pub has_shield_rep: bool,
    pub has_armor_rep: bool,
}

/// System-level NPC spawner and AI director.
#[derive(Debug, Default)]
pub struct NpcSpawnManager {
    pub entities: HashMap<i64, NpcCombatEntity>,
    /// Tracks cleared spawn groups and their respawn cooldown timestamp: `(system_id, spawn_group_id) -> respawn_after_unix`.
    pub respawn_timers: HashMap<(u32, u32), u64>,
}

impl NpcSpawnManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawns an SDE NPC spawn group (e.g. Guristas / Blood Raider / Serpentis pirate patrol)
    /// at the given belt or gate position.
    pub async fn spawn_group(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        spawn_group_id: u32,
        spawn_pos: DVec3,
    ) -> Result<Vec<i64>, String> {
        let spawn_group = ctx
            .sde
            .npc_spawn_groups
            .get(&spawn_group_id)
            .cloned()
            .ok_or_else(|| format!("SDE NPC spawn group {spawn_group_id} not found"))?;

        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut spawned_ball_ids = Vec::new();

        for (idx, &npc_type_id) in spawn_group.npc_type_ids.iter().enumerate() {
            let (bounty, max_vel, opt_range, orb_range, base_dps) = ctx
                .sde
                .types
                .get(&npc_type_id)
                .map(|t| {
                    let a = &t.default_attributes;
                    let b = a
                        .get(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY)
                        .copied()
                        .unwrap_or(25_000.0);
                    let v = a.get(&eve_sde::attr_ids::MAX_VELOCITY).copied().unwrap_or(320.0);
                    let opt = a.get(&54).copied().unwrap_or(8_000.0);
                    let orb = (opt * 0.75).clamp(2_500.0, 15_000.0);
                    let dps = (b / 500.0).clamp(20.0, 250.0);
                    (b, v, opt, orb, dps)
                })
                .unwrap_or((25_000.0, 320.0, 8_000.0, 5_000.0, 40.0));

            let ball_id = ctx.db.with_mut_snapshot(|snap| {
                snap.next_entity_id += 1;
                snap.next_entity_id
            });

            // Disperse ships slightly around spawn center
            let offset = DVec3::new(
                ((idx as f64) * 500.0).sin() * 1_200.0,
                0.0,
                ((idx as f64) * 500.0).cos() * 1_200.0,
            );
            let entity_pos = spawn_pos + offset;

            let ball = Ball {
                id: ball_id,
                type_id: npc_type_id,
                position: entity_pos,
                velocity: DVec3::ZERO,
                max_velocity: max_vel,
                inertia: 2.5,
                mass: 1_200_000.0,
                radius: 45.0,
                speed_fraction: 1.0,
                mode: MovementMode::Stop,
            };

            let combat_state = ctx.build_type_combat_state(npc_type_id);
            let _ = sys_handle.spawn_ball_with_combat(ball, combat_state).await;

            let faction_id = spawn_group.faction_id;
            let damage_vector = faction_damage_vector(faction_id, base_dps);

            let entity = NpcCombatEntity {
                ball_id,
                type_id: npc_type_id,
                faction_id,
                state: NpcAiState::Idle,
                spawn_group_id,
                spawn_pos: entity_pos,
                target_id: None,
                bounty_isk: bounty,
                optimal_range: opt_range,
                orbit_range: orb_range,
                base_dps,
                damage_vector,
                last_attack_tick: 0,
                has_shield_rep: true,
                has_armor_rep: true,
            };

            self.entities.insert(ball_id, entity);
            spawned_ball_ids.push(ball_id);
        }

        Ok(spawned_ball_ids)
    }

    /// Spawns belt rats at asteroid belts in a solar system based on system security and region (`feat:npc.belt-rats`).
    pub async fn spawn_belt_rats_for_system(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
    ) -> Result<Vec<i64>, String> {
        let mut belt_positions: Vec<DVec3> = ctx
            .sde
            .celestials_in_system(sys_id)
            .into_iter()
            .filter(|c| c.group_id == 9) // Asteroid Belt
            .map(|c| c.position)
            .collect();

        if belt_positions.is_empty() {
            belt_positions.push(DVec3::new(100_000.0, 0.0, 50_000.0));
        }

        let mut candidate_groups: Vec<u32> = ctx
            .sde
            .npc_spawn_groups
            .keys()
            .copied()
            .collect();
        candidate_groups.sort_unstable();

        if candidate_groups.is_empty() {
            return Ok(Vec::new());
        }

        let mut all_spawned = Vec::new();
        for (idx, &belt_pos) in belt_positions.iter().enumerate() {
            let group_id = candidate_groups[idx % candidate_groups.len()];
            if let Ok(spawned) = self.spawn_group(ctx, sys_id, group_id, belt_pos).await {
                all_spawned.extend(spawned);
            }
        }

        Ok(all_spawned)
    }

    /// Evaluates the NPC AI state machine for all active NPC entities in the solar system:
    /// - `Idle`: No hostile within 50 km -> stand down / stop.
    /// - `Chase`: Hostile detected within 50 km -> pursue using `MovementMode::Follow`.
    /// - `Orbit`: Distance <= `orbit_range * 1.3` -> switch to `MovementMode::Orbit`.
    /// - `Attack`: Hostile within weapon range -> apply cycle damage, trigger shield/armor reps if HP low.
    pub fn step_ai_sync(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        current_tick: u64,
        _dt: f64,
    ) {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let all_balls: Vec<Ball> = sys_handle
            .ballpark
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .balls
            .values()
            .cloned()
            .collect();
        let ball_positions: HashMap<i64, DVec3> =
            all_balls.iter().map(|b| (b.id, b.position)).collect();

        // Find potential player targets (ships that are not NPCs)
        let player_targets: Vec<(i64, DVec3)> = all_balls
            .into_iter()
            .filter(|b| !self.entities.contains_key(&b.id) && b.max_velocity > 0.0)
            .map(|b| (b.id, b.position))
            .collect();

        let mut to_remove = Vec::new();
        let mut mode_updates: Vec<(i64, MovementMode)> = Vec::new();
        let mut dmg_updates: Vec<(i64, DamageVector)> = Vec::new();

        for (&ball_id, entity) in self.entities.iter_mut() {
            let Some(&npc_pos) = ball_positions.get(&ball_id) else {
                to_remove.push(ball_id);
                continue;
            };

            // Find closest candidate player target
            let closest = player_targets
                .iter()
                .map(|&(pid, pos)| (pid, pos, (pos - npc_pos).length()))
                .min_by(|a, b| a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal));

            match closest {
                Some((target_id, _target_pos, dist)) if dist <= 50_000.0 => {
                    entity.target_id = Some(target_id);

                    if dist > entity.orbit_range * 1.4 {
                        // 1. Chase target
                        if entity.state != NpcAiState::Chase {
                            entity.state = NpcAiState::Chase;
                            mode_updates.push((
                                ball_id,
                                MovementMode::Follow {
                                    target_id,
                                    range: entity.orbit_range,
                                },
                            ));
                        }
                    } else {
                        // 2. Orbit target
                        if entity.state != NpcAiState::Orbit && entity.state != NpcAiState::Attack {
                            entity.state = NpcAiState::Orbit;
                            mode_updates.push((
                                ball_id,
                                MovementMode::Orbit {
                                    target_id,
                                    range: entity.orbit_range,
                                },
                            ));
                        }

                        // 3. Attack target
                        if dist <= entity.optimal_range * 1.25 {
                            entity.state = NpcAiState::Attack;
                            if current_tick >= entity.last_attack_tick.saturating_add(2) {
                                entity.last_attack_tick = current_tick;
                                let dmg = entity.damage_vector.scaled(0.5);
                                dmg_updates.push((target_id, dmg));
                            }
                        }
                    }
                }
                _ => {
                    // No target within 50km: transition to Idle
                    if entity.state != NpcAiState::Idle {
                        entity.state = NpcAiState::Idle;
                        entity.target_id = None;
                        mode_updates.push((ball_id, MovementMode::Stop));
                    }
                }
            }
        }

        if !mode_updates.is_empty() || !dmg_updates.is_empty() {
            let mut bp = sys_handle
                .ballpark
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let mut st_guard = sys_handle
                .actor_state
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let st = &mut *st_guard;
            for (bid, mode) in mode_updates {
                let _ = bp.set_mode(bid, mode);
            }
            for (tid, dmg) in dmg_updates {
                bp.apply_damage_to_ball(tid, dmg);
            }
            crate::system_actor::reconcile_tackle_and_bubbles(
                &mut bp,
                &mut st.active_module_cycles,
                &st.base_scramble_strengths,
                &st.base_core_strengths,
            );
            crate::system_actor::sync_ball_positions(
                &bp,
                &sys_handle.ball_positions,
                &sys_handle.scrambled_balls,
            );
        }

        for dead_id in to_remove {
            self.entities.remove(&dead_id);
        }
    }

    pub async fn step_ai(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        current_tick: u64,
        dt: f64,
    ) {
        self.step_ai_sync(ctx, sys_id, current_tick, dt);
    }

    /// Handles death of an NPC: credits dynamic bounty based on `ENTITY_KILL_BOUNTY`,
    /// spawns a wreck with loot drops, raises killer's security status, records a killmail with the exact bounty value, and updates respawn timers.
    pub fn handle_npc_destroyed(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        npc_ball_id: i64,
        killer_char_id: i64,
        now_unix: u64,
    ) -> Result<f64, String> {
        let entity = self
            .entities
            .remove(&npc_ball_id)
            .ok_or_else(|| format!("NPC ball {npc_ball_id} not tracked by spawn manager"))?;

        let bounty = entity.bounty_isk;

        // Record respawn cooldown: 20 minutes (1,200 seconds)
        self.respawn_timers.insert(
            (sys_id, entity.spawn_group_id),
            now_unix.saturating_add(1_200),
        );

        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let bounty_cents = IskCents::from_f64_isk(bounty);

            // Credit dynamic bounty to killer's wallet and increase security status (feat:crimewatch.sec-status-gain)
            if let Some(ch) = snap.characters.get_mut(&killer_char_id) {
                ch.balance = ch.balance.checked_add(bounty_cents).unwrap_or(ch.balance);
                let gain = compute_sec_status_gain(ch.security_status, bounty);
                ch.security_status = (ch.security_status + gain).min(5.0);
            }

            // Wallet journal entry
            snap.next_entity_id += 1;
            let jid = snap.next_entity_id;
            snap.wallet_journal.push(WalletJournalEntry {
                entry_id: jid,
                character_id: killer_char_id,
                entry_type: "bounty_payout".to_string(),
                amount: bounty_cents,
                balance_after: snap
                    .characters
                    .get(&killer_char_id)
                    .map(|c| c.balance)
                    .unwrap_or(bounty_cents),
                reason: format!("NPC bounty award for destruction of {}", entity.type_id),
            });

            // Persist Killmail with dynamic bounty value (NOT flat 100k)
            snap.next_entity_id += 1;
            let kid = snap.next_entity_id;
            snap.record_killmail(
                KillmailRecord {
                    kill_id: kid,
                    victim_character_id: npc_ball_id,
                    attacker_character_id: killer_char_id,
                    solar_system_id: sys_id,
                    ship_type_id: entity.type_id,
                    total_value_isk: bounty,
                    ..Default::default()
                },
                now_unix,
            );

            // Spawn physical NPC wreck item (26557)
            snap.next_entity_id += 1;
            let wreck_id = snap.next_entity_id;
            crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(
                snap, wreck_id, now_unix, true,
            );
            snap.items.insert(
                wreck_id,
                InventoryItemRecord {
                    item_id: wreck_id,
                    type_id: 26557,
                    owner_id: killer_char_id,
                    location_id: sys_id as i64,
                    flag: InventoryFlag::Cargo,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    dynamic_attributes: HashMap::new(),
                    source_mutaplasmid_id: None,
                    insured_payout_isk: 0.0,
                    is_blueprint_copy: false,
                    blueprint_runs_remaining: None,
                },
            );
            snap.item_positions.insert(
                wreck_id,
                [entity.spawn_pos.x, entity.spawn_pos.y, entity.spawn_pos.z],
            );

            // Populate NPC loot table drops inside the wreck item (feat:npc.npc-loot)
            let ammo_type_id = match entity.faction_id {
                500010 | 500001 => 204, // Scourge Heavy Missile
                500012 | 500019 | 500003 => 178, // Radio S
                500020 | 500004 => 230, // Antimatter Charge S
                500011 | 500002 => 213, // Phased Plasma S
                _ => 212, // Iron Charge S
            };
            let loot_drops = [(15331, 10), (ammo_type_id, 100)];
            for &(lid_type, lid_qty) in &loot_drops {
                snap.next_entity_id += 1;
                let lid = snap.next_entity_id;
                snap.items.insert(
                    lid,
                    InventoryItemRecord {
                        item_id: lid,
                        type_id: lid_type,
                        owner_id: killer_char_id,
                        location_id: wreck_id,
                        flag: InventoryFlag::Cargo,
                        quantity: lid_qty,
                        singleton: false,
                        version: 1,
                        dynamic_attributes: HashMap::new(),
                        source_mutaplasmid_id: None,
                        insured_payout_isk: 0.0,
                        is_blueprint_copy: false,
                        blueprint_runs_remaining: None,
                    },
                );
            }

            Ok::<_, String>(bounty)
        });
        if res.is_ok() && killer_char_id > 0 {
            crate::services::agents_missions::on_mission_npc_destroyed_core(
                &ctx.db,
                sys_id,
                npc_ball_id,
                Some(killer_char_id),
            );
            ctx.quasar_gateway.progression.daily_goals.on_gameplay_event(
                killer_char_id as u32,
                1,
                1,
                &ctx.quasar_gateway,
            );
            ctx.quasar_gateway.progression.career_goals.on_gameplay_event(
                killer_char_id as u32,
                "combat",
                1,
                &ctx.quasar_gateway,
            );
        }
        res
    }
}
