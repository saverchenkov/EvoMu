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

/// Resolves the regional pirate faction ID for a solar system (`feat:npc.belt-rats`, `feat:npc.clone-soldier-spawns`).
// DERIVED: Region-to-pirate-faction table mapped from EVE regional pirate rat distribution
// (Guristas, Blood Raiders, Sansha's Nation, Serpentis, Angel Cartel); client FSD static data
// does not ship server-side regional pirate spawn tables.
pub fn regional_pirate_faction_id(ctx: &ServerContext, solar_system_id: u32) -> u32 {
    let Some(sys) = ctx.sde.solar_systems.get(&solar_system_id) else {
        return 500_010;
    };
    let sde_faction = ctx
        .sde
        .constellations
        .get(&sys.constellation_id)
        .and_then(|c| c.faction_id)
        .or_else(|| ctx.sde.regions.get(&sys.region_id).and_then(|r| r.faction_id));
    if let Some(fid) = sde_faction {
        match fid {
            500_010 | 500_001 => return 500_010, // Guristas / Caldari State
            500_011 | 500_002 | 500_015 => return 500_011, // Angel Cartel / Minmatar / Thukker
            500_012 | 500_003 | 500_008 => return 500_012, // Blood Raiders / Amarr / Khanid
            500_019 | 500_007 => return 500_019, // Sansha's Nation / Ammatar Mandate
            500_020 | 500_004 | 500_009 | 500_014 => return 500_020, // Serpentis / Gallente / Syndicate / ORE
            _ => {}
        }
    }
    match sys.region_id {
        10_000_002 | 10_000_003 | 10_000_010 | 10_000_015 | 10_000_016
        | 10_000_023 | 10_000_033 | 10_000_051 | 10_000_069 => 500_010, // Guristas
        10_000_020 | 10_000_036 | 10_000_038 | 10_000_043 | 10_000_052 | 10_000_054
        | 10_000_065 | 10_000_067 => 500_012, // Blood Raiders
        10_000_001 | 10_000_031 | 10_000_037 | 10_000_039 | 10_000_047 | 10_000_050
        | 10_000_056 | 10_000_063 => 500_019, // Sansha's Nation
        10_000_027 | 10_000_032 | 10_000_035 | 10_000_041 | 10_000_044 | 10_000_046
        | 10_000_048 | 10_000_055 | 10_000_057 | 10_000_058 | 10_000_059 | 10_000_060
        | 10_000_064 | 10_000_068 => 500_020, // Serpentis
        10_000_005 | 10_000_006 | 10_000_007 | 10_000_008 | 10_000_009 | 10_000_011
        | 10_000_012 | 10_000_014 | 10_000_025 | 10_000_028 | 10_000_030 | 10_000_042 => 500_011, // Angel Cartel
        _ => 500_010,
    }
}

/// Returns the Clone Soldier tier name for a low-sec system security status (`0.05..0.45`), or `None` outside low-sec (`feat:npc.clone-soldier-spawns`).
pub fn clone_soldier_tier_for_security(sys_sec: f64) -> Option<&'static str> {
    if !(0.05..0.45).contains(&sys_sec) {
        return None;
    }
    if sys_sec >= 0.35 {
        Some("Trainer")
    } else if sys_sec >= 0.25 {
        Some("Recruiter")
    } else if sys_sec >= 0.15 {
        Some("Transporter")
    } else {
        Some("Negotiator")
    }
}

/// Returns the Clone Soldier NPC `type_id` for a low-sec system security status (`0.05..0.45`), or `None` outside low-sec (`feat:npc.clone-soldier-spawns`).
pub fn clone_soldier_type_for_security(sys_sec: f64) -> Option<u32> {
    match clone_soldier_tier_for_security(sys_sec)? {
        "Trainer" => Some(33132),     // Clone Soldier Trainer (0.4) -> Tag 33138
        "Recruiter" => Some(33142),   // Clone Soldier Recruiter (0.3) -> Tag 33139
        "Transporter" => Some(33163), // Clone Soldier Transporter (0.2) -> Tag 33140
        _ => Some(33175),             // Clone Soldier Negotiator (0.1) -> Tag 33141
    }
}

/// Resolves the regional pirate faction's Clone Soldier NPC `type_id` from `sde.types`
/// matching the system's low-sec security band (`0.1`..=`0.4`) and pirate faction (`feat:npc.clone-soldier-spawns`).
pub fn clone_soldier_type_for_security_and_faction(
    sde: &eve_sde::SdeRegistry,
    sys_sec: f64,
    faction_id: u32,
) -> Option<u32> {
    let tier = clone_soldier_tier_for_security(sys_sec)?;
    let target_group_id: u32 = match faction_id {
        500_011 => 551, // Asteroid Angel Cartel BattleCruiser / Clone Soldiers
        500_012 => 552, // Asteroid Blood Raiders BattleCruiser / Clone Soldiers
        500_010 => 556, // Asteroid Guristas BattleCruiser / Clone Soldiers
        500_019 => 566, // Asteroid Sansha's Nation BattleCruiser / Clone Soldiers
        500_020 => 574, // Asteroid Serpentis BattleCruiser / Clone Soldiers
        _ => 556,
    };
    let mut matches: Vec<u32> = sde
        .types
        .values()
        .filter(|t| {
            t.group_id == target_group_id
                && t.name.contains("Clone Soldier")
                && t.name.contains(tier)
        })
        .map(|t| t.type_id)
        .collect();
    matches.sort_unstable();
    matches
        .into_iter()
        .next()
        .or_else(|| clone_soldier_type_for_security(sys_sec))
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
    /// Tracks Clone Soldier respawn cooldown per low-sec solar system: `system_id -> respawn_after_unix`.
    pub clone_soldier_respawn_timers: HashMap<u32, u64>,
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

        let count = spawn_group.npc_type_ids.len() as i64;
        let start_ball_id = ctx.db.with_mut_snapshot(|snap| {
            let start = snap.next_entity_id + 1;
            snap.next_entity_id += count;
            start
        });

        for (idx, &npc_type_id) in spawn_group.npc_type_ids.iter().enumerate() {
            let ball_id = start_ball_id + idx as i64;
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

    /// Spawns belt rats at asteroid belts in a solar system based on system security and region (`feat:npc.belt-rats`, `feat:npc.clone-soldier-spawns`, `feat:npc.officer-npc`, `feat:npc.drifters`).
    pub async fn spawn_belt_rats_for_system(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
    ) -> Result<Vec<i64>, String> {
        let now_unix = ctx.now_unix();
        // Wormhole & Drifter Hive systems (31_000_000..32_000_000): spawn Drifter Cruiser + Battleship (`feat:npc.drifters`)
        if (31_000_000..32_000_000).contains(&sys_id) {
            let drifter_alive = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.npc_entities
                    .values()
                    .any(|n| n.solar_system_id == sys_id && n.archetype == "Drifter")
            });
            let on_cd = self
                .respawn_timers
                .get(&(sys_id, 500_024))
                .is_some_and(|&until| now_unix < until);
            if !drifter_alive && !on_cd {
                self.respawn_timers
                    .insert((sys_id, 500_024), now_unix.saturating_add(600));
                let anchor_pos = ctx
                    .sde
                    .celestials_in_system(sys_id)
                    .first()
                    .map(|c| c.position + DVec3::new(15_000.0, 0.0, 0.0))
                    .unwrap_or(DVec3::new(15_000.0, 0.0, 0.0));
                let cruiser_tid = ctx
                    .sde
                    .types
                    .values()
                    .filter(|t| t.group_id == 1956)
                    .map(|t| t.type_id)
                    .min()
                    .unwrap_or(47153);
                let bs_group = if (31_000_001..=31_000_006).contains(&sys_id) {
                    1530
                } else {
                    1310
                };
                let bs_tid = ctx
                    .sde
                    .types
                    .values()
                    .filter(|t| t.group_id == bs_group)
                    .map(|t| t.type_id)
                    .min()
                    .unwrap_or(34495);
                let c_bid = crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
                    ctx,
                    sys_id,
                    cruiser_tid,
                    "Drifter",
                    500_024,
                    anchor_pos,
                )
                .await;
                let b_bid = crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
                    ctx,
                    sys_id,
                    bs_tid,
                    "Drifter",
                    500_024,
                    anchor_pos + DVec3::new(2_500.0, 0.0, 0.0),
                )
                .await;
                return Ok(vec![c_bid, b_bid]);
            }
            return Ok(Vec::new());
        }

        let belt_positions: Vec<DVec3> = ctx
            .sde
            .celestials_in_system(sys_id)
            .into_iter()
            .filter(|c| c.group_id == 9) // Asteroid Belt
            .map(|c| c.position)
            .collect();

        if belt_positions.is_empty() {
            return Ok(Vec::new());
        }

        let sys_sec = ctx
            .sde
            .solar_systems
            .get(&sys_id)
            .map(|s| s.security_status as f64)
            .unwrap_or(0.5);
        let reg_faction = regional_pirate_faction_id(ctx, sys_id);

        let max_ship_class: u32 = if sys_sec >= 0.45 {
            2 // High-sec: Frigate / Destroyer
        } else if sys_sec >= 0.05 {
            4 // Low-sec: up to Cruiser / Battlecruiser
        } else {
            6 // Null-sec / W-space: up to Battleship
        };
        let mut candidate_groups: Vec<u32> = ctx
            .sde
            .npc_spawn_groups
            .iter()
            .filter(|(_, g)| {
                (g.faction_id == reg_faction || g.faction_id == 0)
                    && g.ship_class <= max_ship_class
            })
            .map(|(&gid, _)| gid)
            .collect();
        if candidate_groups.is_empty() {
            candidate_groups = ctx
                .sde
                .npc_spawn_groups
                .iter()
                .filter(|(_, g)| g.faction_id == reg_faction || g.faction_id == 0)
                .map(|(&gid, _)| gid)
                .collect();
        }
        if candidate_groups.is_empty() {
            candidate_groups = ctx.sde.npc_spawn_groups.keys().copied().collect();
        }
        candidate_groups.sort_unstable();

        let mut all_spawned = Vec::new();
        if !candidate_groups.is_empty() {
            for (idx, &belt_pos) in belt_positions.iter().take(3).enumerate() {
                let group_id = candidate_groups[idx % candidate_groups.len()];
                let belt_alive = self
                    .entities
                    .values()
                    .any(|e| e.spawn_pos.distance(belt_pos) < 25_000.0);
                if belt_alive
                    || self
                        .respawn_timers
                        .get(&(sys_id, group_id))
                        .is_some_and(|&until| now_unix < until)
                {
                    continue;
                }
                if let Ok(spawned) = self.spawn_group(ctx, sys_id, group_id, belt_pos).await {
                    all_spawned.extend(spawned);
                }
            }
            if sys_sec < 0.45 {
                let gate_positions: Vec<DVec3> = ctx
                    .sde
                    .celestials_in_system(sys_id)
                    .into_iter()
                    .filter(|c| c.group_id == 10) // Stargate
                    .map(|c| c.position)
                    .collect();
                if let Some(&gate_pos) = gate_positions.first() {
                    let gate_group_id = candidate_groups[belt_positions.len() % candidate_groups.len()];
                    let gate_alive = self
                        .entities
                        .values()
                        .any(|e| e.spawn_pos.distance(gate_pos) < 25_000.0);
                    if !gate_alive
                        && !self
                            .respawn_timers
                            .get(&(sys_id, gate_group_id))
                            .is_some_and(|&until| now_unix < until)
                    {
                        if let Ok(spawned) =
                            self.spawn_group(ctx, sys_id, gate_group_id, gate_pos).await
                        {
                            all_spawned.extend(spawned);
                        }
                    }
                }
            }
        }

        // In low-sec (0.1..=0.4), spawn a Clone Soldier at the first asteroid belt if not on respawn cooldown (`feat:npc.clone-soldier-spawns`).
        if let Some(cs_type_id) =
            clone_soldier_type_for_security_and_faction(&ctx.sde, sys_sec, reg_faction)
        {
            if let Some(rt_until) = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                rt.clone_soldier_cooldown_until.get(&sys_id).copied()
            }) {
                let entry = self.clone_soldier_respawn_timers.entry(sys_id).or_insert(rt_until);
                if rt_until > *entry {
                    *entry = rt_until;
                }
            }
            let on_cooldown = self
                .clone_soldier_respawn_timers
                .get(&sys_id)
                .is_some_and(|&until| now_unix < until)
                || crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    rt.npc_entities.values().any(|n| {
                        n.solar_system_id == sys_id && n.archetype == "CloneSoldier"
                    })
                });
            if !on_cooldown {
                let cs_ball_id = crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
                    ctx,
                    sys_id,
                    cs_type_id,
                    "CloneSoldier",
                    reg_faction,
                    belt_positions[0],
                )
                .await;
                all_spawned.push(cs_ball_id);
            }

            // Low-sec Mordu's Legion Special Warfare Unit belt spawn (SDE groups 1285..=1287, faction 500018)
            let mordu_cd = self
                .respawn_timers
                .get(&(sys_id, 500_018))
                .is_some_and(|&until| now_unix < until)
                || crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    rt.npc_entities
                        .values()
                        .any(|n| n.solar_system_id == sys_id && n.archetype == "MorduCommander")
                });
            if !mordu_cd {
                self.respawn_timers
                    .insert((sys_id, 500_018), now_unix.saturating_add(300));
                if ctx.mechanic_rng.roll(0.25) {
                    let mordu_grp: u32 = if sys_sec >= 0.30 {
                        1285 // Frigate Operative (Garmur)
                    } else if sys_sec >= 0.15 {
                        1286 // Cruiser Specialist (Orthrus)
                    } else {
                        1287 // Battleship Commander (Barghest)
                    };
                    if let Some(mordu_tid) = ctx
                        .sde
                        .types
                        .values()
                        .filter(|t| t.group_id == mordu_grp)
                        .map(|t| t.type_id)
                        .min()
                    {
                        let m_pos = belt_positions
                            .get(1)
                            .copied()
                            .unwrap_or(belt_positions[0])
                            + DVec3::new(2_500.0, 0.0, 0.0);
                        let m_bid = crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
                            ctx,
                            sys_id,
                            mordu_tid,
                            "MorduCommander",
                            500_018,
                            m_pos,
                        )
                        .await;
                        all_spawned.push(m_bid);
                    }
                }
            }
        }

        // Rare Faction Commander spawn at asteroid belts (`feat:npc.belt-rats`)
        let cmd_cd = self
            .respawn_timers
            .get(&(sys_id, 900_050))
            .is_some_and(|&until| now_unix < until);
        if !cmd_cd {
            self.respawn_timers
                .insert((sys_id, 900_050), now_unix.saturating_add(300));
            let cmd_prob = if sys_sec >= 0.45 {
                0.08
            } else if sys_sec >= 0.05 {
                0.15
            } else {
                0.20
            };
            if ctx.mechanic_rng.roll(cmd_prob) {
                let cmd_grp: u32 = match (reg_faction, sys_sec >= 0.45, sys_sec >= 0.05) {
                    (500_011, true, _) => 789,
                    (500_011, false, true) => 790,
                    (500_011, false, false) => 848,
                    (500_012, true, _) => 792,
                    (500_012, false, true) => 791,
                    (500_012, false, false) => 849,
                    (500_019, true, _) => 810,
                    (500_019, false, true) => 808,
                    (500_019, false, false) => 851,
                    (500_020, true, _) => 814,
                    (500_020, false, true) => 812,
                    (500_020, false, false) => 852,
                    (_, true, _) => 800,
                    (_, false, true) => 798,
                    (_, false, false) => 850,
                };
                let mut cmd_tids: Vec<u32> = ctx
                    .sde
                    .types
                    .values()
                    .filter(|t| t.group_id == cmd_grp)
                    .map(|t| t.type_id)
                    .collect();
                cmd_tids.sort_unstable();
                if !cmd_tids.is_empty() {
                    let pick = (ctx.mechanic_rng.try_next_u64().unwrap_or(0) as usize) % cmd_tids.len();
                    let cmd_bid = crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
                        ctx,
                        sys_id,
                        cmd_tids[pick],
                        "Officer",
                        reg_faction,
                        belt_positions[0] + DVec3::new(1_800.0, 0.0, -1_200.0),
                    )
                    .await;
                    all_spawned.push(cmd_bid);
                }
            }
        }

        // Rare Named Officer spawn in true null-sec asteroid belts (`sys_sec < 0.0`, `feat:npc.officer-npc`)
        if sys_sec < 0.0 && sys_id < 31_000_000 {
            let off_cd = self
                .respawn_timers
                .get(&(sys_id, 900_099))
                .is_some_and(|&until| now_unix < until)
                || crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    rt.npc_entities.values().any(|n| {
                        n.solar_system_id == sys_id
                            && ctx
                                .sde
                                .types
                                .get(&n.type_id)
                                .is_some_and(|t| matches!(t.group_id, 553 | 559 | 564 | 569 | 574))
                    })
                });
            if !off_cd {
                self.respawn_timers
                    .insert((sys_id, 900_099), now_unix.saturating_add(600));
                if ctx.mechanic_rng.roll(0.10) {
                    let off_grp: u32 = match reg_faction {
                        500_011 => 553, // Angel Officer
                        500_012 => 559, // Blood Raiders Officer
                        500_019 => 569, // Sansha Officer
                        500_020 => 574, // Serpentis Officer
                        _ => 564,       // Guristas Officer
                    };
                    let mut off_tids: Vec<u32> = ctx
                        .sde
                        .types
                        .values()
                        .filter(|t| t.group_id == off_grp)
                        .map(|t| t.type_id)
                        .collect();
                    off_tids.sort_unstable();
                    if !off_tids.is_empty() {
                        let pick =
                            (ctx.mechanic_rng.try_next_u64().unwrap_or(0) as usize) % off_tids.len();
                        let off_bid = crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_entity(
                            ctx,
                            sys_id,
                            off_tids[pick],
                            "Officer",
                            reg_faction,
                            belt_positions[0] + DVec3::new(-2_500.0, 0.0, 1_500.0),
                        )
                        .await;
                        all_spawned.push(off_bid);
                    }
                }
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
        let (all_balls, cloaked_or_dead): (Vec<Ball>, std::collections::HashSet<i64>) = {
            let bp = sys_handle
                .ballpark
                .read()
                .unwrap_or_else(|e| e.into_inner());
            let balls = bp.balls.values().cloned().collect();
            let invalid = bp
                .combat_states
                .iter()
                .filter(|(_, cs)| cs.is_cloaked || cs.structure_hp <= 0.0)
                .map(|(&id, _)| id)
                .collect();
            (balls, invalid)
        };
        let ball_positions: HashMap<i64, DVec3> =
            all_balls.iter().map(|b| (b.id, b.position)).collect();

        // Find potential player targets (ships that are not NPCs, not capsules, not cloaked, not warping, and alive)
        let player_targets: Vec<(i64, DVec3, f64)> = all_balls
            .into_iter()
            .filter(|b| {
                !self.entities.contains_key(&b.id)
                    && b.type_id != 670
                    && b.max_velocity > 0.0
                    && !matches!(b.mode, MovementMode::Warp { .. })
                    && !cloaked_or_dead.contains(&b.id)
            })
            .map(|b| {
                let weight = ctx.db.with_snapshot(|snap| {
                    let sig = ctx.sde.types.get(&b.type_id).and_then(|t| t.default_attributes.get(&552).copied()).unwrap_or(40.0);
                    let mut ewar = false;
                    let mut logi = false;
                    for &aid in &snap.module_active_states {
                        if let Some(it) = snap.items.get(&aid).filter(|i| i.location_id == b.id) {
                            if let Some(mt) = ctx.sde.types.get(&it.type_id) {
                                if matches!(mt.group_id, 52 | 65 | 201 | 208 | 291 | 357) { ewar = true; }
                                if matches!(mt.group_id, 41 | 325 | 585) { logi = true; }
                            }
                        }
                    }
                    // DERIVED: EWAR 1.5x, logistics 1.3x, and signature radius threat weight.
                    let mult = if ewar { 1.5 } else if logi { 1.3 } else { 1.0 };
                    (100.0 + (sig / 40.0).clamp(0.5, 5.0) * 10.0) * mult
                });
                (b.id, b.position, weight)
            })
            .collect();

        let mut to_remove = Vec::new();
        let mut mode_updates: Vec<(i64, MovementMode)> = Vec::new();
        let mut dmg_updates: Vec<(i64, DamageVector)> = Vec::new();

        for (&ball_id, entity) in self.entities.iter_mut() {
            let Some(&npc_pos) = ball_positions.get(&ball_id) else {
                to_remove.push(ball_id);
                continue;
            };

            // Pick highest-threat candidate within 50 km (breaking ties by closest distance)
            let best_in_range = player_targets
                .iter()
                .map(|&(pid, pos, weight)| (pid, pos, (pos - npc_pos).length(), weight))
                .filter(|&(_, _, dist, _)| dist <= 50_000.0)
                .max_by(|a, b| {
                    a.3.partial_cmp(&b.3)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal))
                });

            match best_in_range {
                Some((target_id, _target_pos, dist, _)) => {
                    let target_changed = entity.target_id != Some(target_id);
                    entity.target_id = Some(target_id);

                    if dist > entity.orbit_range * 1.4 {
                        // 1. Chase target
                        if entity.state != NpcAiState::Chase || target_changed {
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
                        if (entity.state != NpcAiState::Orbit && entity.state != NpcAiState::Attack)
                            || target_changed
                        {
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

        let now_unix = ctx.now_unix();
        for dead_id in to_remove {
            if let Some(dead_ent) = self.entities.remove(&dead_id) {
                let until = now_unix.saturating_add(1_200);
                if crate::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype(
                    ctx,
                    dead_ent.type_id,
                ) == "CloneSoldier"
                {
                    self.clone_soldier_respawn_timers.insert(sys_id, until);
                    crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                        rt.clone_soldier_cooldown_until.insert(sys_id, until);
                    });
                } else {
                    self.respawn_timers
                        .insert((sys_id, dead_ent.spawn_group_id), until);
                }
            }
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

        let supp_mult = crate::services::security_mgr::insurgency_suppression_bounty_and_sec_multiplier(ctx, sys_id);
        let bounty = entity.bounty_isk * supp_mult;

        // Record respawn cooldown: 20 minutes (1,200 seconds)
        let until = now_unix.saturating_add(1_200);
        if crate::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype(
            ctx,
            entity.type_id,
        ) == "CloneSoldier"
        {
            self.clone_soldier_respawn_timers.insert(sys_id, until);
            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                rt.clone_soldier_cooldown_until.insert(sys_id, until);
            });
        } else {
            self.respawn_timers
                .insert((sys_id, entity.spawn_group_id), until);
        }

        let res = ctx.db.try_with_mut_snapshot(|snap| {
            let bounty_cents = IskCents::from_f64_isk(bounty);

            // Credit dynamic bounty to killer's wallet and increase security status (feat:crimewatch.sec-status-gain)
            if let Some(ch) = snap.characters.get_mut(&killer_char_id) {
                ch.balance = ch.balance.checked_add(bounty_cents).unwrap_or(ch.balance);
                let gain = compute_sec_status_gain(ch.security_status, entity.bounty_isk) * supp_mult;
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

            if !(32_000_000..=32_999_999).contains(&sys_id) {
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
            ctx.quasar_gateway.progression.achievements.on_gameplay_event_by_name(
                killer_char_id as u32,
                &["destroyHostileNPC", "entityGroupDestroyed"],
                1,
                ctx.now_unix(),
                &ctx.quasar_gateway,
            );
        }
        res
    }
}
