//! `npc_ai` — Belt/gate NPC spawns from SDE spawn groups, NPC AI state machine (Idle -> Chase -> Orbit -> Attack -> Reps), and dynamic bounty payout (`C-4`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, KillmailRecord};
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
    if sde.types.len() > 1_000 {
        static CLONE_SOLDIER_CATALOG_BY_LEN: std::sync::OnceLock<
            std::sync::Mutex<std::collections::HashMap<(usize, usize), Vec<(u32, u32, String)>>>,
        > = std::sync::OnceLock::new();
        let key = (
            sde.types.len(),
            (33_130u32..=33_250)
                .filter(|id| sde.types.contains_key(id))
                .count(),
        );
        let map = CLONE_SOLDIER_CATALOG_BY_LEN
            .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()));
        let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
        let catalog = guard.entry(key).or_insert_with(|| {
            let mut rows: Vec<(u32, u32, String)> = sde
                .types
                .values()
                .filter(|t| {
                    matches!(t.group_id, 551 | 552 | 556 | 566 | 574)
                        && t.name.contains("Clone Soldier")
                })
                .map(|t| (t.type_id, t.group_id, t.name.clone()))
                .collect();
            rows.sort_unstable_by_key(|r| r.0);
            rows
        });
        if let Some(&(tid, _, _)) = catalog
            .iter()
            .find(|&&(_, gid, ref name)| gid == target_group_id && name.contains(tier))
        {
            return Some(tid);
        }
        return clone_soldier_type_for_security(sys_sec);
    }
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
    let scale = (bounty / 500_000.0).clamp(0.01, 10.0);
    (headroom * 0.01 * scale).clamp(0.001, 0.5).min(headroom)
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
        let count = spawn_group.npc_type_ids.len() as i64;
        let start_ball_id = ctx.db.with_mut_snapshot(|snap| {
            let start = snap.next_entity_id + 1;
            snap.next_entity_id += count;
            start
        });
        Ok(self
            .spawn_group_with_start_id(ctx, sys_id, spawn_group_id, &spawn_group, spawn_pos, start_ball_id)
            .await)
    }

    async fn spawn_group_with_start_id(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        spawn_group_id: u32,
        spawn_group: &eve_sde::NpcSpawnGroupInfo,
        spawn_pos: DVec3,
        start_ball_id: i64,
    ) -> Vec<i64> {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut spawned_ball_ids = Vec::with_capacity(spawn_group.npc_type_ids.len());

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
            let faction_id = spawn_group.faction_id;
            let fac_owner = if faction_id > 0 {
                faction_id as i64
            } else {
                500_001
            };
            let sde_t = ctx.sde.types.get(&npc_type_id);
            let npc_meta = eve_destiny::SlimItemMeta {
                group_id: sde_t.map(|t| t.group_id).unwrap_or(546),
                category_id: sde_t.map(|t| t.category_id).unwrap_or(11),
                owner_id: fac_owner,
                char_id: None,
                corp_id: crate::services::combat_crimewatch::combat_npc_ai::resolve_npc_corp_id_for_faction(ctx, fac_owner),
                alliance_id: -1,
                security_status: -1.0,
                is_cloaked: false,
                name: sde_t.map(|t| t.name.clone()).filter(|n| !n.is_empty()),
                ..Default::default()
            };
            let _ = sys_handle
                .spawn_ball_with_meta(ball, combat_state, npc_meta)
                .await;

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

        spawned_ball_ids
    }

    pub(crate) fn plan_belt_rats_for_system(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        drifter_alive: bool,
        rt_until_opt: Option<u64>,
        cs_alive: bool,
        mordu_alive: bool,
        officer_alive: bool,
    ) -> (Vec<(u32, eve_sde::NpcSpawnGroupInfo, DVec3)>, Vec<(u32, &'static str, u32, DVec3)>) {
        let now_unix = ctx.now_unix();
        // Wormhole & Drifter Hive systems (31_000_000..32_000_000): spawn Drifter Cruiser + Battleship (`feat:npc.drifters`)
        if (31_000_000..32_000_000).contains(&sys_id) {
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
                return (
                    Vec::new(),
                    vec![
                        (cruiser_tid, "Drifter", 500_024, anchor_pos),
                        (
                            bs_tid,
                            "Drifter",
                            500_024,
                            anchor_pos + DVec3::new(2_500.0, 0.0, 0.0),
                        ),
                    ],
                );
            }
            return (Vec::new(), Vec::new());
        }

        let belt_positions: Vec<DVec3> = ctx
            .sde
            .celestials_in_system(sys_id)
            .into_iter()
            .filter(|c| c.group_id == 9) // Asteroid Belt
            .map(|c| c.position)
            .collect();

        if belt_positions.is_empty() {
            return (Vec::new(), Vec::new());
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

        let mut planned_groups: Vec<(u32, eve_sde::NpcSpawnGroupInfo, DVec3)> = Vec::new();
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
                if let Some(sg) = ctx.sde.npc_spawn_groups.get(&group_id) {
                    planned_groups.push((group_id, sg.clone(), belt_pos));
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
                        if let Some(sg) = ctx.sde.npc_spawn_groups.get(&gate_group_id) {
                            planned_groups.push((gate_group_id, sg.clone(), gate_pos));
                        }
                    }
                }
            }
        }

        let need_cs = clone_soldier_tier_for_security(sys_sec).is_some();
        let need_officer = sys_sec < 0.0 && sys_id < 31_000_000;
        let mut special_specs: Vec<(u32, &'static str, u32, DVec3)> = Vec::new();

        // In low-sec (0.1..=0.4), spawn a Clone Soldier at the first asteroid belt if not on respawn cooldown (`feat:npc.clone-soldier-spawns`).
        if need_cs {
            if let Some(rt_until) = rt_until_opt {
                let entry = self.clone_soldier_respawn_timers.entry(sys_id).or_insert(rt_until);
                if rt_until > *entry {
                    *entry = rt_until;
                }
            }
            let on_cooldown = self
                .clone_soldier_respawn_timers
                .get(&sys_id)
                .is_some_and(|&until| now_unix < until)
                || cs_alive;
            if !on_cooldown {
                if let Some(cs_type_id) =
                    clone_soldier_type_for_security_and_faction(&ctx.sde, sys_sec, reg_faction)
                {
                    special_specs.push((
                        cs_type_id,
                        "CloneSoldier",
                        reg_faction,
                        belt_positions[0],
                    ));
                }
            }

            // Low-sec Mordu's Legion Special Warfare Unit belt spawn (SDE groups 1285..=1287, faction 500018)
            let mordu_cd = self
                .respawn_timers
                .get(&(sys_id, 500_018))
                .is_some_and(|&until| now_unix < until)
                || mordu_alive;
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
                        special_specs.push((mordu_tid, "MorduCommander", 500_018, m_pos));
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
                    special_specs.push((
                        cmd_tids[pick],
                        "Officer",
                        reg_faction,
                        belt_positions[0] + DVec3::new(1_800.0, 0.0, -1_200.0),
                    ));
                }
            }
        }

        // Rare Named Officer spawn in true null-sec asteroid belts (`sys_sec < 0.0`, `feat:npc.officer-npc`)
        if need_officer {
            let off_cd = self
                .respawn_timers
                .get(&(sys_id, 900_099))
                .is_some_and(|&until| now_unix < until)
                || officer_alive;
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
                        special_specs.push((
                            off_tids[pick],
                            "Officer",
                            reg_faction,
                            belt_positions[0] + DVec3::new(-2_500.0, 0.0, 1_500.0),
                        ));
                    }
                }
            }
        }

        (planned_groups, special_specs)
    }

    pub(crate) async fn execute_belt_rat_plan_with_start_id(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        planned_groups: Vec<(u32, eve_sde::NpcSpawnGroupInfo, DVec3)>,
        special_specs: Vec<(u32, &'static str, u32, DVec3)>,
        mut next_start_id: i64,
        out_entities: &mut Vec<(i64, crate::services::combat_crimewatch::ExtendedNpcEntity)>,
    ) -> Vec<i64> {
        let mut all_spawned = Vec::new();
        for (group_id, sg, spawn_pos) in planned_groups {
            let count = sg.npc_type_ids.len() as i64;
            let spawned = self
                .spawn_group_with_start_id(ctx, sys_id, group_id, &sg, spawn_pos, next_start_id)
                .await;
            all_spawned.extend(spawned);
            next_start_id += count;
        }
        if !special_specs.is_empty() {
            let special_ids = crate::services::combat_crimewatch::combat_npc_ai::spawn_specialized_npc_batch_with_start_id(
                ctx,
                sys_id,
                &special_specs,
                next_start_id,
                |_| {},
                out_entities,
            )
            .await;
            all_spawned.extend(special_ids);
        }
        all_spawned
    }

    /// Spawns belt rats at asteroid belts in a solar system based on system security and region (`feat:npc.belt-rats`, `feat:npc.clone-soldier-spawns`, `feat:npc.officer-npc`, `feat:npc.drifters`).
    pub async fn spawn_belt_rats_for_system(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
    ) -> Result<Vec<i64>, String> {
        let (drifter_alive, rt_until_opt, cs_alive, mordu_alive, officer_alive) =
            if (31_000_000..32_000_000).contains(&sys_id) {
                let da = crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    rt.npc_entities
                        .values()
                        .any(|n| n.solar_system_id == sys_id && n.archetype == "Drifter")
                });
                (da, None, false, false, false)
            } else {
                let sys_sec = ctx
                    .sde
                    .solar_systems
                    .get(&sys_id)
                    .map(|s| s.security_status as f64)
                    .unwrap_or(0.5);
                let need_cs = clone_soldier_tier_for_security(sys_sec).is_some();
                let need_officer = sys_sec < 0.0 && sys_id < 31_000_000;
                if need_cs || need_officer {
                    let (rt_until_opt, cs_alive, mordu_alive, officer_alive) =
                        crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                            let mut cs_alive = false;
                            let mut mordu_alive = false;
                            let mut officer_alive = false;
                            for n in rt.npc_entities.values() {
                                if n.solar_system_id != sys_id {
                                    continue;
                                }
                                if n.archetype == "CloneSoldier" {
                                    cs_alive = true;
                                } else if n.archetype == "MorduCommander" {
                                    mordu_alive = true;
                                } else if need_officer
                                    && ctx
                                        .sde
                                        .types
                                        .get(&n.type_id)
                                        .is_some_and(|t| matches!(t.group_id, 553 | 559 | 564 | 569 | 574))
                                {
                                    officer_alive = true;
                                }
                            }
                            (
                                rt.clone_soldier_cooldown_until.get(&sys_id).copied(),
                                cs_alive,
                                mordu_alive,
                                officer_alive,
                            )
                        });
                    (false, rt_until_opt, cs_alive, mordu_alive, officer_alive)
                } else {
                    (false, None, false, false, false)
                }
            };

        let (planned_groups, special_specs) = self.plan_belt_rats_for_system(
            ctx,
            sys_id,
            drifter_alive,
            rt_until_opt,
            cs_alive,
            mordu_alive,
            officer_alive,
        );
        let group_npcs: i64 = planned_groups
            .iter()
            .map(|(_, sg, _)| sg.npc_type_ids.len() as i64)
            .sum();
        let total_npcs = group_npcs + special_specs.len() as i64;
        if total_npcs == 0 {
            return Ok(Vec::new());
        }
        let start_id = ctx.db.with_mut_snapshot(|snap| {
            let start = snap.next_entity_id + 1;
            snap.next_entity_id += total_npcs;
            start
        });
        let mut built_special = Vec::with_capacity(special_specs.len());
        let all_spawned = self
            .execute_belt_rat_plan_with_start_id(
                ctx,
                sys_id,
                planned_groups,
                special_specs,
                start_id,
                &mut built_special,
            )
            .await;
        if !built_special.is_empty() {
            crate::services::combat_crimewatch::with_mut_combat_ephemeral(ctx, |rt| {
                for (ball_id, entity) in built_special {
                    rt.npc_entities.insert(ball_id, entity);
                }
            });
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
        if self.entities.is_empty() {
            return;
        }
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let (ball_positions, mut cand_targets): (
            HashMap<i64, DVec3>,
            Vec<(i64, u32, DVec3)>,
        ) = {
            let bp = sys_handle
                .ballpark
                .read()
                .unwrap_or_else(|e| e.into_inner());
            let mut npc_pos = HashMap::with_capacity(self.entities.len());
            for &nid in self.entities.keys() {
                if let Some(b) = bp.balls.get(&nid) {
                    npc_pos.insert(nid, b.position);
                }
            }
            let mut cands = Vec::new();
            for (&id, b) in &bp.balls {
                if self.entities.contains_key(&id)
                    || bp.static_ball_ids.contains(&id)
                    || bp.non_free_ball_ids.contains(&id)
                    || b.type_id == 670
                    || b.max_velocity <= 0.0
                    || matches!(b.mode, MovementMode::Warp { .. } | MovementMode::Rigid)
                    || bp
                        .combat_states
                        .get(&id)
                        .is_some_and(|cs| cs.is_cloaked || cs.structure_hp <= 0.0)
                {
                    continue;
                }
                let cat_id = bp
                    .ball_slim_meta
                    .get(&id)
                    .map(|m| m.category_id)
                    .or_else(|| ctx.sde.types.get(&b.type_id).map(|t| t.category_id))
                    .unwrap_or(6);
                if matches!(cat_id, 6 | 18 | 65) {
                    cands.push((id, b.type_id, b.position));
                }
            }
            (npc_pos, cands)
        };

        if !cand_targets.is_empty() {
            crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                cand_targets.retain(|(id, _, _)| !rt.npc_entities.contains_key(id));
            });
        }

        // Find potential player targets (ships/drones/structures that are not NPCs, not asteroids/celestials, not capsules, not cloaked, not warping, and alive)
        let player_targets: Vec<(i64, DVec3, f64)> = if cand_targets.is_empty() {
            Vec::new()
        } else {
            ctx.db.with_snapshot(|snap| {
                let mut ewar_set: std::collections::HashSet<i64> = std::collections::HashSet::new();
                let mut logi_set: std::collections::HashSet<i64> = std::collections::HashSet::new();
                if !snap.module_active_states.is_empty() {
                    for &aid in &snap.module_active_states {
                        if let Some(it) = snap.items.get(&aid) {
                            if let Some(mt) = ctx.sde.types.get(&it.type_id) {
                                if matches!(mt.group_id, 52 | 65 | 201 | 208 | 291 | 357) {
                                    ewar_set.insert(it.location_id);
                                }
                                if matches!(mt.group_id, 41 | 325 | 585) {
                                    logi_set.insert(it.location_id);
                                }
                            }
                        }
                    }
                }
                cand_targets
                    .into_iter()
                    .map(|(bid, type_id, pos)| {
                        let sig = ctx
                            .sde
                            .types
                            .get(&type_id)
                            .and_then(|t| t.default_attributes.get(&552).copied())
                            .unwrap_or(40.0);
                        let ewar = ewar_set.contains(&bid);
                        let logi = logi_set.contains(&bid);
                        // DERIVED: EWAR 1.5x, logistics 1.3x, and signature radius threat weight.
                        let mult = if ewar { 1.5 } else if logi { 1.3 } else { 1.0 };
                        let weight = (100.0 + (sig / 40.0).clamp(0.5, 5.0) * 10.0) * mult;
                        (bid, pos, weight)
                    })
                    .collect()
            })
        };

        let mut to_remove = Vec::new();
        let mut mode_updates: Vec<(i64, MovementMode)> = Vec::new();
        let mut dmg_updates: Vec<(i64, u32, i64, DamageVector)> = Vec::new();

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
                                dmg_updates.push((ball_id, entity.type_id, target_id, dmg));
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

        let damaged_target_ids: Vec<i64> = dmg_updates.iter().map(|&(_, _, tid, _)| tid).collect();
        let mut destroyed_balls: Vec<(i64, i64, Ball)> = Vec::new();
        let mut applied_hits: Vec<(i64, u32, i64, f64, f64, f64, f64)> = Vec::new();
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
            for (attacker_id, npc_type_id, tid, dmg) in dmg_updates {
                bp.emit_event(
                    Some(attacker_id),
                    eve_destiny::DestinyEvent::OnSpecialFX {
                        ball_id: attacker_id,
                        module_id: Some(tid),
                        guid: "effects.Laser".to_string(),
                        is_active: true,
                    },
                );
                if let Some((applied, destroyed, dead_ball_opt)) = bp.apply_damage_to_ball(tid, dmg)
                {
                    let (s_r, a_r, h_r) = bp
                        .combat_states
                        .get(&tid)
                        .map(|cs| {
                            (
                                (cs.shield_hp / cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0),
                                (cs.armor_hp / cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0),
                                (cs.structure_hp / cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0),
                            )
                        })
                        .unwrap_or((0.0, 0.0, 0.0));
                    applied_hits.push((attacker_id, npc_type_id, tid, applied, s_r, a_r, h_r));
                    if destroyed {
                        if let Some(dead_ball) = dead_ball_opt {
                            destroyed_balls.push((attacker_id, tid, dead_ball));
                        }
                    }
                }
            }
            let deact_guard = sys_handle
                .module_deactivation_hook
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            crate::system_actor::reconcile_tackle_and_bubbles(
                &mut bp,
                &mut st.active_module_cycles,
                &st.base_scramble_strengths,
                &st.base_core_strengths,
                &st.db_opt,
                deact_guard.as_ref(),
            );
            crate::system_actor::sync_ball_positions(
                &bp,
                &sys_handle.ball_positions,
                &sys_handle.scrambled_balls,
            );
        }

        if !applied_hits.is_empty() {
            for (attacker_id, npc_type_id, tid, applied, s_r, a_r, h_r) in applied_hits {
                let victim_char_opt = ctx.db.with_snapshot(|snap| {
                    snap.characters
                        .get(&tid)
                        .map(|c| c.character_id)
                        .or_else(|| {
                            snap.characters
                                .values()
                                .find(|c| c.active_ship_id == tid)
                                .map(|c| c.character_id)
                        })
                        .or_else(|| {
                            snap.items
                                .get(&tid)
                                .map(|i| i.owner_id)
                                .filter(|id| snap.characters.contains_key(id))
                        })
                });
                crate::services::combat_crimewatch::emit_combat_log_and_damage_state(
                    ctx,
                    attacker_id,
                    victim_char_opt,
                    attacker_id,
                    tid,
                    npc_type_id,
                    applied,
                    false,
                    s_r,
                    a_r,
                    h_r,
                );
            }
        }

        let now_unix = ctx.now_unix();
        if !damaged_target_ids.is_empty() {
            let hit_chars: Vec<i64> = ctx.db.with_snapshot(|snap| {
                damaged_target_ids
                    .iter()
                    .filter_map(|&tid| {
                        snap.characters
                            .get(&tid)
                            .map(|c| c.character_id)
                            .or_else(|| {
                                snap.characters
                                    .values()
                                    .find(|c| c.active_ship_id == tid)
                                    .map(|c| c.character_id)
                            })
                    })
                    .collect()
            });
            for cid in hit_chars {
                crate::sp6_lifecycle::record_npc_aggression_timer(ctx, cid, now_unix);
            }
        }
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

        if !destroyed_balls.is_empty() {
            let mut remaining = Vec::with_capacity(destroyed_balls.len());
            for (attacker_npc_id, target_id, dead_ball) in destroyed_balls {
                if self.entities.contains_key(&target_id) {
                    let _ = self.handle_npc_destroyed(
                        ctx,
                        sys_id,
                        target_id,
                        attacker_npc_id,
                        now_unix,
                    );
                } else {
                    remaining.push((attacker_npc_id, target_id, dead_ball));
                }
            }
            handle_npc_ai_ball_destructions(ctx, sys_id, remaining);
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
            crate::services::pve_ess_reserve_banks::credit_ballpark_npc_bounty_with_ess_split(
                &ctx.sde,
                snap,
                sys_id,
                killer_char_id,
                bounty,
                entity.bounty_isk,
                supp_mult,
                entity.type_id,
                now_unix,
            );

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
            crate::services::social_corp_fleet::social_corp_projects::record_server_project_activity(
                ctx,
                killer_char_id,
                &["DamageNPCShips", "DestroyNPCShips"],
                1,
                Some(sys_id as i64),
            );
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
            let npc_gid = ctx
                .sde
                .types
                .get(&entity.type_id)
                .map(|t| t.group_id)
                .unwrap_or(0);
            crate::services::operations_manager::record_server_operation_event(
                ctx,
                killer_char_id,
                "somethingExplode",
                &[
                    ("typeID", entity.type_id.to_string()),
                    ("groupID", npc_gid.to_string()),
                ],
                1,
            );
        }
        res
    }
}

/// Processes ball destructions returned by `bp.apply_damage_to_ball` during NPC AI ticks:
/// invokes `execute_death_pipeline_sync` for player ships/capsules, cleans up destroyed player drones,
/// and removes destroyed extended NPC entities.
pub(crate) fn handle_npc_ai_ball_destructions(
    ctx: &ServerContext,
    sys_id: u32,
    destructions: Vec<(i64, i64, Ball)>,
) {
    if destructions.is_empty() {
        return;
    }
    let now_unix = ctx.now_unix();
    for (attacker_npc_id, target_id, dead_ball) in destructions {
        let removed_drone = if crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            rt.active_drones_in_space.contains_key(&target_id)
        }) {
            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                let d = rt.active_drones_in_space.remove(&target_id);
                if d.is_some() {
                    rt.active_drone_miners.remove(&target_id);
                    rt.drone_next_ready_unix.remove(&target_id);
                }
                d
            })
        } else {
            None
        };
        if let Some(d) = removed_drone {
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.remove(&d.drone_item_id);
                snap.item_positions.remove(&d.ball_id);
                snap.sp6_lifecycle.space_object_meta.remove(&d.ball_id);
            });
            continue;
        }

        let victim_char_opt = ctx.db.with_snapshot(|snap| {
            snap.characters
                .values()
                .find(|c| c.active_ship_id == target_id)
                .map(|c| c.character_id)
                .or_else(|| {
                    snap.sp6_lifecycle
                        .disconnected_ships
                        .values()
                        .find(|d| d.ship_id == target_id)
                        .map(|d| d.char_id)
                })
                .or_else(|| {
                    snap.items
                        .get(&target_id)
                        .filter(|it| {
                            it.owner_id >= 90_000_000
                                && ctx
                                    .sde
                                    .types
                                    .get(&it.type_id)
                                    .is_some_and(|t| t.category_id == 6)
                        })
                        .map(|it| it.owner_id)
                })
        });
        if let Some(victim_char_id) = victim_char_opt {
            let final_blow_id =
                crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
                    let mut latest: Option<(u64, i64)> = None;
                    for key in [target_id, victim_char_id] {
                        if let Some(list) = rt.aggression_assist_log.get(&key) {
                            for e in list {
                                if now_unix.saturating_sub(e.last_action_unix)
                                    <= crate::services::combat_crimewatch::KILLMAIL_ASSIST_WINDOW_SECS
                                    && e.attacker_char_id != victim_char_id
                                    && latest.is_none_or(|(ts, _)| e.last_action_unix > ts)
                                {
                                    latest = Some((e.last_action_unix, e.attacker_char_id));
                                }
                            }
                        }
                    }
                    latest.map(|(_, cid)| cid)
                })
                .unwrap_or(attacker_npc_id);
            let _ = crate::combat::death_pipeline::execute_death_pipeline_sync(
                ctx,
                sys_id,
                victim_char_id,
                target_id,
                final_blow_id,
                dead_ball.position,
                now_unix,
            );
            continue;
        }

        let removed_ext_npc =
            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                rt.npc_entities.remove(&target_id).is_some()
            });
        if removed_ext_npc {
            ctx.db.with_mut_snapshot(|snap| {
                snap.items.remove(&target_id);
                snap.item_positions.remove(&target_id);
                snap.sp6_lifecycle.space_object_meta.remove(&target_id);
            });
        }
    }
}

pub(crate) fn compute_sleeper_sentry_interval_s(ctx: &ServerContext, npc_type_id: u32) -> f64 {
    ctx.sde
        .types
        .get(&npc_type_id)
        .and_then(|t| t.default_attributes.get(&51).copied())
        .unwrap_or(15_000.0)
        / 1000.0
}

#[cfg(test)] #[path = "npc_ai_unit_tests.rs"]
mod npc_ai_unit_tests;

#[cfg(test)]
#[path = "npc_ai_edges_tests.rs"]
mod npc_ai_edges_tests;

