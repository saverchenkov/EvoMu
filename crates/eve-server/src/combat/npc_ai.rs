//! `npc_ai` — Belt/gate NPC spawns from SDE spawn groups, NPC AI state machine (Idle -> Chase -> Orbit -> Attack -> Reps), and dynamic bounty payout (`C-4`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use eve_db::{InventoryFlag, InventoryItemRecord, KillmailRecord, WorldStateSnapshot};
use eve_destiny::{Ball, DamageVector, MovementMode};
use eve_sde::SdeRegistry;
use glam::DVec3;
use std::collections::{HashMap, HashSet};

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
/// Resolves the regional pirate faction ID directly from an [`eve_sde::SdeRegistry`]
/// without requiring a full [`ServerContext`].
pub fn regional_pirate_faction_id_for_sde(
    sde: &eve_sde::SdeRegistry,
    solar_system_id: u32,
) -> u32 {
    let Some(sys) = sde.solar_systems.get(&solar_system_id) else {
        return 500_010;
    };
    let sde_faction = sde
        .constellations
        .get(&sys.constellation_id)
        .and_then(|c| c.faction_id)
        .or_else(|| sde.regions.get(&sys.region_id).and_then(|r| r.faction_id));
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

pub fn regional_pirate_faction_id(ctx: &ServerContext, solar_system_id: u32) -> u32 {
    regional_pirate_faction_id_for_sde(&ctx.sde, solar_system_id)
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
        struct CachedCloneSoldierCatalog(
            std::sync::Mutex<std::collections::HashMap<(usize, usize), Vec<(u32, u32, String)>>>,
        );
        let key = (
            sde.types.len(),
            (33_130u32..=33_250)
                .filter(|id| sde.types.contains_key(id))
                .count(),
        );
        let cache = sde.get_or_init_derived::<CachedCloneSoldierCatalog>(|| {
            CachedCloneSoldierCatalog(std::sync::Mutex::new(std::collections::HashMap::new()))
        });
        let mut guard = cache.0.lock().unwrap_or_else(|e| e.into_inner());
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
    /// Per-anchor respawn cooldown timestamps (`(system_id, location_slot, spawn_group_id) -> until`),
    /// keeping Belt 1/2/3 and stargate cooldowns decoupled even when locations share a group ID.
    belt_slot_respawn_timers: HashMap<(u32, u8, u32), u64>,
    /// Per-entity SDE combat profile & sub-tick weapon cooldown state (`#1396`).
    runtime: HashMap<i64, NpcRuntimeCombatState>,
}

#[cfg(test)]
pub(crate) use super::npc_ai_weapons_loot::fallback_log_registry;
pub use super::npc_ai_weapons_loot::{
    EdencomHostilityMeta, ExtendedNpcWeaponRuntime, NpcCombatProfile, NpcMissileProfile,
    NpcTurretProfile, NpcWeaponKind,
};
pub(crate) use super::npc_ai_weapons_loot::{
    build_npc_ball_from_sde, damage_vector_approx_eq, emit_npc_special_fx,
    evaluate_npc_missile_or_vorton_hit, evaluate_npc_turret_shot_with_rng, inspect_npc_sde_profile,
    resolve_npc_combat_profile, resolve_npc_weapon_ammo_loot, resolve_npc_wreck_type_id,
    NpcSizeClass,
};

#[derive(Debug, Clone)]
struct NpcRuntimeCombatState {
    profile: NpcCombatProfile,
    uses_synthetic_legacy: bool,
    baseline_damage_vector: DamageVector,
    baseline_optimal_range: f64,
    turret_cooldown_s: f64,
    missile_cooldown_s: f64,
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

    async fn spawn_group_detached_with_start_id(
        ctx: &ServerContext,
        sys_id: u32,
        spawn_group_id: u32,
        spawn_group: &eve_sde::NpcSpawnGroupInfo,
        spawn_pos: DVec3,
        start_ball_id: i64,
    ) -> (Vec<i64>, Vec<(i64, NpcCombatEntity)>) {
        let sys_handle = ctx.systems.get_or_spawn(sys_id);
        let mut spawned_ball_ids = Vec::with_capacity(spawn_group.npc_type_ids.len());
        let mut spawned_entities = Vec::with_capacity(spawn_group.npc_type_ids.len());

        for (idx, &npc_type_id) in spawn_group.npc_type_ids.iter().enumerate() {
            let ball_id = start_ball_id + idx as i64;
            let faction_id = spawn_group.faction_id;
            let profile = resolve_npc_combat_profile(&ctx.sde, npc_type_id, None, faction_id);

            // Disperse ships slightly around spawn center
            let offset = DVec3::new(
                ((idx as f64) * 500.0).sin() * 1_200.0,
                0.0,
                ((idx as f64) * 500.0).cos() * 1_200.0,
            );
            let entity_pos = spawn_pos + offset;

            let ball = build_npc_ball_from_sde(
                &ctx.sde,
                ball_id,
                npc_type_id,
                entity_pos,
                profile.max_velocity,
                if profile.max_velocity > 0.0 { 1.0 } else { 0.0 },
            );

            let combat_state = ctx.build_type_combat_state(npc_type_id);
            let fac_owner = if faction_id > 0 {
                faction_id as i64
            } else {
                500_001
            };
            let sde_t = ctx.sde.get_type(npc_type_id);
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

            let entity = NpcCombatEntity {
                ball_id,
                type_id: npc_type_id,
                faction_id,
                state: NpcAiState::Idle,
                spawn_group_id,
                spawn_pos: entity_pos,
                target_id: None,
                bounty_isk: profile.bounty_isk,
                optimal_range: profile.engagement_optimal_range,
                orbit_range: profile.orbit_range,
                base_dps: profile.total_nominal_dps,
                damage_vector: profile.nominal_damage_vector,
                last_attack_tick: 0,
                has_shield_rep: false,
                has_armor_rep: false,
            };

            spawned_entities.push((ball_id, entity));
            spawned_ball_ids.push(ball_id);
        }

        (spawned_ball_ids, spawned_entities)
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
        let (spawned_ball_ids, spawned_entities) = Self::spawn_group_detached_with_start_id(
            ctx,
            sys_id,
            spawn_group_id,
            spawn_group,
            spawn_pos,
            start_ball_id,
        )
        .await;
        for (ball_id, entity) in spawned_entities {
            self.insert_spawned_entity(&ctx.sde, ball_id, entity);
        }
        spawned_ball_ids
    }

    fn insert_spawned_entity(&mut self, sde: &SdeRegistry, ball_id: i64, entity: NpcCombatEntity) {
        let profile = resolve_npc_combat_profile(
            sde,
            entity.type_id,
            Some(entity.bounty_isk),
            entity.faction_id,
        );
        self.runtime.insert(
            ball_id,
            NpcRuntimeCombatState {
                uses_synthetic_legacy: profile.is_synthetic_fallback,
                baseline_damage_vector: profile.nominal_damage_vector,
                baseline_optimal_range: profile.engagement_optimal_range,
                profile,
                turret_cooldown_s: 0.0,
                missile_cooldown_s: 0.0,
            },
        );
        self.entities.insert(ball_id, entity);
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
        let min_type_in_group = |gid: u32, fallback: u32| {
            ctx.sde
                .types
                .values()
                .filter(|t| t.group_id == gid)
                .map(|t| t.type_id)
                .min()
                .unwrap_or(fallback)
        };
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
                let bs_group = if (31_000_001..=31_000_006).contains(&sys_id) { 1530 } else { 1310 };
                let cruiser_tid = min_type_in_group(1956, 47153);
                let bs_tid = min_type_in_group(bs_group, 34495);
                return (
                    Vec::new(),
                    vec![
                        (cruiser_tid, "Drifter", 500_024, anchor_pos),
                        (bs_tid, "Drifter", 500_024, anchor_pos + DVec3::new(2_500.0, 0.0, 0.0)),
                    ],
                );
            }
            return (Vec::new(), Vec::new());
        }

        let (belt_positions, gate_pos) = Self::belt_and_gate_anchors(&ctx.sde, sys_id);
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
            2 // High-sec: Frigate
        } else if sys_sec >= 0.05 {
            4 // Low-sec: up to Cruiser / Battlecruiser
        } else {
            6 // Null-sec / W-space: up to Battleship
        };
        let hull_rank = |g: &eve_sde::NpcSpawnGroupInfo| -> u8 {
            const RANKS: &[(&str, u8)] = &[
                ("Frigate", 0), ("Destroyer", 1), ("BattleCruiser", 3),
                ("Battlecruiser", 3), ("Cruiser", 2), ("Battleship", 4),
            ];
            RANKS.iter().find_map(|&(kw, r)| g.group_name.contains(kw).then_some(r)).unwrap_or(5)
        };
        let is_non_standard_belt_group = |g: &eve_sde::NpcSpawnGroupInfo| -> bool {
            const EXCLUDED: &[&str] = &[
                "Officer", "Commander", "Hauler", "Dreadnought", "Titan", "Carrier",
                "Supercarrier", "Mothership", "Industrial", "Deadspace", "Ghost",
                "Mission", "Incursion", "Roaming", "Irregular", "Insurgency",
            ];
            EXCLUDED.iter().any(|kw| g.group_name.contains(kw))
        };
        let max_hull_rank: u8 = if sys_sec >= 0.45 { 0 } else if sys_sec >= 0.05 { 3 } else { 4 };
        let fits_sec_tier = |g: &eve_sde::NpcSpawnGroupInfo| -> bool {
            let hr = hull_rank(g);
            g.ship_class <= max_ship_class
                && !is_non_standard_belt_group(g)
                && (hr <= max_hull_rank || hr == 5)
        };
        let is_eligible_belt_group = |g: &eve_sde::NpcSpawnGroupInfo| -> bool {
            !g.npc_type_ids.is_empty()
                && !crate::services::combat_crimewatch::combat_legacy_ess::is_non_combat_npc_prop_group(g.group_id)
                && !crate::services::combat_crimewatch::combat_legacy_ess::is_invulnerable_npc_authority_group(g.group_id)
                && !crate::services::combat_crimewatch::combat_legacy_ess::is_sleeper_or_drifter_npc_group(g.group_id)
                // Exclude LCO Drone, Minor Threat, Convoy, Sentry, and Storyline NPC groups.
                && !matches!(g.group_id, 279 | 286 | 297 | 298 | 383 | 495 | 520 | 522 | 523 | 527 | 533 | 534)
        };
        let regional_keyword = match reg_faction {
            500_011 => "Angel",
            500_012 => "Blood",
            500_019 => "Sansha",
            500_020 => "Serpentis",
            _ => "Guristas",
        };
        let filter_groups = |pred: &dyn Fn(&eve_sde::NpcSpawnGroupInfo) -> bool| -> Vec<u32> {
            ctx.sde.npc_spawn_groups.iter().filter(|(_, g)| pred(g)).map(|(&gid, _)| gid).collect()
        };
        let mut candidate_groups = filter_groups(&|g| {
            is_eligible_belt_group(g)
                && fits_sec_tier(g)
                && (g.faction_id == reg_faction
                    || (g.faction_id == 0 && g.group_name.contains(regional_keyword)))
        });
        if candidate_groups.is_empty() {
            candidate_groups = filter_groups(&|g| {
                is_eligible_belt_group(g)
                    && (g.faction_id == reg_faction || g.faction_id == 0)
                    && fits_sec_tier(g)
            });
        }
        if candidate_groups.is_empty() {
            candidate_groups = filter_groups(&|g| {
                is_eligible_belt_group(g) && (g.faction_id == reg_faction || g.faction_id == 0)
            });
        }
        if candidate_groups.is_empty() {
            candidate_groups = ctx.sde.npc_spawn_groups.keys().copied().collect();
        }
        candidate_groups.sort_by_key(|&gid| {
            ctx.sde.npc_spawn_groups.get(&gid).map_or(
                (true, true, u32::MAX, u8::MAX, gid),
                |g| (is_non_standard_belt_group(g), !g.group_name.starts_with("Asteroid "), g.ship_class, hull_rank(g), gid),
            )
        });

        let squad_limit = |sg: &eve_sde::NpcSpawnGroupInfo| -> usize {
            if sys_sec >= 0.45 || hull_rank(sg) >= 2 { 1 } else { 2 }
        };
        let cap_belt_squad = |sg: &eve_sde::NpcSpawnGroupInfo, limit: usize| -> eve_sde::NpcSpawnGroupInfo {
            let mut trimmed = sg.clone();
            let is_cs = |tid: u32| ctx.sde.types.get(&tid).is_some_and(|x| x.name.contains("Clone Soldier"));
            // Strip Clone Soldier types before sorting/truncating so they never spawn as ordinary belt rats.
            if trimmed.npc_type_ids.iter().any(|&tid| !is_cs(tid)) {
                trimmed.npc_type_ids.retain(|&tid| !is_cs(tid));
            }
            if trimmed.npc_type_ids.len() > limit {
                trimmed.npc_type_ids.sort_by_key(|&tid| {
                    let bounty = ctx.sde.types.get(&tid)
                        .and_then(|x| x.default_attributes.get(&eve_sde::attr_ids::ENTITY_KILL_BOUNTY).copied())
                        .unwrap_or(f64::MAX);
                    (bounty.max(0.0) as u64, tid)
                });
                trimmed.npc_type_ids.truncate(limit);
            }
            trimmed
        };
        let mut planned_groups: Vec<(u32, eve_sde::NpcSpawnGroupInfo, DVec3)> = Vec::new();
        if !candidate_groups.is_empty() {
            let belts_taken = belt_positions.len().min(3);
            for (idx, &belt_pos) in belt_positions.iter().take(3).enumerate() {
                // Keep Belt 0 on the lightest hull while spanning Cruiser/Battlecruiser hulls in null-sec.
                let group_idx = if sys_sec < 0.05 && candidate_groups.len() >= 5 {
                    [0, 2, 3].get(idx).copied().unwrap_or(idx % candidate_groups.len())
                } else {
                    idx % candidate_groups.len()
                };
                let group_id = candidate_groups[group_idx];
                let belt_alive = self.entities.values().any(|e| e.spawn_pos.distance(belt_pos) < 25_000.0);
                if belt_alive || self.is_belt_slot_on_cooldown(sys_id, idx as u8, group_id, now_unix) {
                    continue;
                }
                if let Some(sg) = ctx.sde.npc_spawn_groups.get(&group_id) {
                    planned_groups.push((group_id, cap_belt_squad(sg, squad_limit(sg)), belt_pos));
                }
            }
            if sys_sec < 0.45 {
                if let Some(gate_pos) = gate_pos {
                    let gate_idx = if candidate_groups.len() > belts_taken {
                        candidate_groups.len() - 1
                    } else {
                        belts_taken % candidate_groups.len()
                    };
                    let gate_group_id = candidate_groups[gate_idx];
                    let gate_alive = self.entities.values().any(|e| e.spawn_pos.distance(gate_pos) < 25_000.0);
                    if !gate_alive && !self.is_belt_slot_on_cooldown(sys_id, 3, gate_group_id, now_unix) {
                        if let Some(sg) = ctx.sde.npc_spawn_groups.get(&gate_group_id) {
                            planned_groups.push((gate_group_id, cap_belt_squad(sg, 1), gate_pos));
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

    pub(crate) async fn execute_belt_rat_plan_detached_with_start_id(
        ctx: &ServerContext,
        sys_id: u32,
        planned_groups: Vec<(u32, eve_sde::NpcSpawnGroupInfo, DVec3)>,
        special_specs: Vec<(u32, &'static str, u32, DVec3)>,
        mut next_start_id: i64,
        out_entities: &mut Vec<(i64, crate::services::combat_crimewatch::ExtendedNpcEntity)>,
    ) -> (Vec<i64>, Vec<(i64, NpcCombatEntity)>) {
        let mut all_spawned = Vec::new();
        let mut all_entities = Vec::new();
        for (group_id, sg, spawn_pos) in planned_groups {
            let count = sg.npc_type_ids.len() as i64;
            let (spawned, entities) = Self::spawn_group_detached_with_start_id(
                ctx,
                sys_id,
                group_id,
                &sg,
                spawn_pos,
                next_start_id,
            )
            .await;
            all_spawned.extend(spawned);
            all_entities.extend(entities);
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
        (all_spawned, all_entities)
    }

    pub(crate) async fn execute_belt_rat_plan_with_start_id(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        planned_groups: Vec<(u32, eve_sde::NpcSpawnGroupInfo, DVec3)>,
        special_specs: Vec<(u32, &'static str, u32, DVec3)>,
        next_start_id: i64,
        out_entities: &mut Vec<(i64, crate::services::combat_crimewatch::ExtendedNpcEntity)>,
    ) -> Vec<i64> {
        let (all_spawned, all_entities) = Self::execute_belt_rat_plan_detached_with_start_id(
            ctx,
            sys_id,
            planned_groups,
            special_specs,
            next_start_id,
            out_entities,
        )
        .await;
        for (ball_id, entity) in all_entities {
            self.insert_spawned_entity(&ctx.sde, ball_id, entity);
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
        dt: f64,
    ) {
        if self.entities.is_empty() {
            return;
        }
        let npc_ids: HashSet<i64> = self.entities.keys().copied().collect();
        let (ball_kinematics, player_targets) = collect_ai_step_inputs(ctx, sys_id, &npc_ids);
        let plan = self.plan_ai_step_for_ids(
            None,
            &ctx.sde,
            &ball_kinematics,
            &player_targets,
            current_tick,
            dt,
        );
        let destroyed_balls = apply_ai_step_world_effects(
            ctx,
            sys_id,
            plan.mode_updates,
            plan.dmg_actions,
            &ball_kinematics,
        );
        let now_unix = ctx.now_unix();
        let actions =
            self.apply_ai_step_cleanup(ctx, sys_id, plan.to_remove, destroyed_balls, now_unix);
        finalize_ai_step_cleanup(ctx, sys_id, actions, now_unix);
    }

    fn plan_ai_step_for_ids(
        &mut self,
        tracked_ids: Option<&HashSet<i64>>,
        sde: &SdeRegistry,
        ball_kinematics: &HashMap<i64, AiBallKinematics>,
        player_targets: &[(i64, DVec3, f64)],
        current_tick: u64,
        dt: f64,
    ) -> AiStepPlan {
        let step_dt = if dt.is_finite() && dt > 0.0 { dt } else { 1.0 };
        self.runtime.retain(|id, _| self.entities.contains_key(id));

        let mut to_remove = Vec::new();
        let mut mode_updates: Vec<ModeUpdate> = Vec::new();
        let mut dmg_actions: Vec<AiDamageAction> = Vec::new();

        for (&ball_id, entity) in self.entities.iter_mut() {
            if let Some(ids) = tracked_ids {
                if !ids.contains(&ball_id) {
                    continue;
                }
            }
            let Some(&npc_kin) = ball_kinematics.get(&ball_id) else {
                to_remove.push(ball_id);
                continue;
            };
            let rt = self.runtime.entry(ball_id).or_insert_with(|| {
                let profile = resolve_npc_combat_profile(
                    sde,
                    entity.type_id,
                    Some(entity.bounty_isk),
                    entity.faction_id,
                );
                let overridden = !damage_vector_approx_eq(entity.damage_vector, profile.nominal_damage_vector)
                    || (entity.optimal_range - profile.engagement_optimal_range).abs() > 1e-3;
                NpcRuntimeCombatState {
                    uses_synthetic_legacy: profile.is_synthetic_fallback || overridden,
                    baseline_damage_vector: profile.nominal_damage_vector,
                    baseline_optimal_range: profile.engagement_optimal_range,
                    profile,
                    turret_cooldown_s: 0.0,
                    missile_cooldown_s: 0.0,
                }
            });
            if !rt.uses_synthetic_legacy
                && (!damage_vector_approx_eq(entity.damage_vector, rt.baseline_damage_vector)
                    || (entity.optimal_range - rt.baseline_optimal_range).abs() > 1e-3)
            {
                rt.uses_synthetic_legacy = true;
            }
            let uses_synthetic_legacy = rt.uses_synthetic_legacy;
            let max_velocity = rt.profile.max_velocity;
            let orbit_speed_fraction = if max_velocity > 0.0 {
                (rt.profile.cruise_velocity / max_velocity).clamp(0.05, 1.0)
            } else {
                0.0
            };
            let chase_speed_fraction = if max_velocity > 0.0 { 1.0 } else { 0.0 };

            // Pick highest-threat candidate within 50 km (breaking ties by closest distance)
            let best_in_range = player_targets
                .iter()
                .map(|&(pid, pos, weight)| (pid, pos, (pos - npc_kin.position).length(), weight))
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
                    let target_radius = ball_kinematics
                        .get(&target_id)
                        .map(|tk| tk.radius)
                        .unwrap_or(0.0);
                    let surface_dist = (dist - npc_kin.radius.max(0.0) - target_radius.max(0.0)).max(0.0);
                    let is_stationary = max_velocity <= 0.0;
                    let in_orbit_band = !is_stationary && dist <= entity.orbit_range * 1.4;
                    let prev_state = entity.state;

                    if !is_stationary {
                        if !in_orbit_band {
                            // 1. Chase target at maxVelocity
                            if prev_state != NpcAiState::Chase || target_changed {
                                entity.state = NpcAiState::Chase;
                                mode_updates.push(ModeUpdate {
                                    ball_id,
                                    mode: MovementMode::Follow {
                                        target_id,
                                        range: entity.orbit_range,
                                    },
                                    speed_fraction: chase_speed_fraction,
                                });
                            }
                        } else {
                            // 2. Orbit target at entityCruiseSpeed
                            if !matches!(prev_state, NpcAiState::Orbit | NpcAiState::Attack)
                                || target_changed
                            {
                                entity.state = NpcAiState::Orbit;
                                mode_updates.push(ModeUpdate {
                                    ball_id,
                                    mode: MovementMode::Orbit {
                                        target_id,
                                        range: entity.orbit_range,
                                    },
                                    speed_fraction: orbit_speed_fraction,
                                });
                            }
                        }
                    }

                    // 3. Weapon engagement & ROF accumulator
                    if uses_synthetic_legacy {
                        rt.turret_cooldown_s = (rt.turret_cooldown_s - step_dt).max(0.0);
                        rt.missile_cooldown_s = (rt.missile_cooldown_s - step_dt).max(0.0);
                        if (is_stationary || in_orbit_band) && dist <= entity.optimal_range * 1.25 {
                            entity.state = NpcAiState::Attack;
                            if current_tick >= entity.last_attack_tick.saturating_add(2) {
                                entity.last_attack_tick = current_tick;
                                dmg_actions.push(AiDamageAction::Synthetic {
                                    attacker_id: ball_id,
                                    npc_type_id: entity.type_id,
                                    target_id,
                                    dmg: entity.damage_vector.scaled(0.5),
                                });
                            }
                        } else if is_stationary {
                            entity.state = NpcAiState::Idle;
                        }
                    } else {
                        let mut fired_or_in_range = false;

                        if let Some(turret) = &rt.profile.turret {
                            let max_r = turret.max_effective_range();
                            if surface_dist <= max_r && turret.shot_damage.total() > 0.0 {
                                fired_or_in_range = true;
                                let mut shots = 0_u32;
                                while rt.turret_cooldown_s <= 1e-9 && shots < 16 {
                                    shots += 1;
                                    rt.turret_cooldown_s += turret.cycle_duration_s;
                                    entity.last_attack_tick = current_tick;
                                    if turret.kind == NpcWeaponKind::Vorton {
                                        dmg_actions.push(AiDamageAction::Vorton {
                                            attacker_id: ball_id,
                                            npc_type_id: entity.type_id,
                                            target_id,
                                            module_type_id: turret.gfx_type_id,
                                            fx_guid: turret.fx_guid.clone(),
                                            cycle_duration_s: turret.cycle_duration_s,
                                            shot_damage: turret.shot_damage,
                                            optimal_range: turret.optimal_range,
                                            explosion_radius: turret.vorton_explosion_radius,
                                            explosion_velocity: turret.vorton_explosion_velocity,
                                            drf: turret.vorton_drf,
                                        });
                                    } else {
                                        dmg_actions.push(AiDamageAction::Turret {
                                            attacker_id: ball_id,
                                            npc_type_id: entity.type_id,
                                            target_id,
                                            module_type_id: turret.gfx_type_id,
                                            fx_guid: turret.fx_guid.clone(),
                                            cycle_duration_s: turret.cycle_duration_s,
                                            shot_damage: turret.shot_damage,
                                            optimal_range: turret.optimal_range,
                                            falloff_range: turret.falloff_range,
                                            tracking_speed: turret.tracking_speed,
                                            sig_resolution: turret.signature_resolution,
                                            max_effective_range: max_r,
                                        });
                                    }
                                }
                                rt.turret_cooldown_s -= step_dt;
                            } else {
                                rt.turret_cooldown_s = (rt.turret_cooldown_s - step_dt).max(0.0);
                            }
                        }

                        if let Some(missile) = &rt.profile.missile {
                            if surface_dist <= missile.max_flight_range
                                && missile.volley_damage.total() > 0.0
                            {
                                fired_or_in_range = true;
                                let mut volleys = 0_u32;
                                while rt.missile_cooldown_s <= 1e-9 && volleys < 16 {
                                    volleys += 1;
                                    rt.missile_cooldown_s += missile.cycle_duration_s;
                                    entity.last_attack_tick = current_tick;
                                    dmg_actions.push(AiDamageAction::Missile {
                                        attacker_id: ball_id,
                                        npc_type_id: entity.type_id,
                                        target_id,
                                        missile_type_id: missile.missile_type_id,
                                        fx_guid: missile.fx_guid.clone(),
                                        cycle_duration_s: missile.cycle_duration_s,
                                        volley_damage: missile.volley_damage,
                                        max_flight_range: missile.max_flight_range,
                                        explosion_radius: missile.explosion_radius,
                                        explosion_velocity: missile.explosion_velocity,
                                        drf: missile.damage_reduction_factor,
                                    });
                                }
                                rt.missile_cooldown_s -= step_dt;
                            } else {
                                rt.missile_cooldown_s = (rt.missile_cooldown_s - step_dt).max(0.0);
                            }
                        }

                        if is_stationary {
                            entity.state = if fired_or_in_range {
                                NpcAiState::Attack
                            } else {
                                NpcAiState::Idle
                            };
                        } else if in_orbit_band {
                            entity.state = if fired_or_in_range {
                                NpcAiState::Attack
                            } else {
                                NpcAiState::Orbit
                            };
                        }
                    }
                }
                _ => {
                    // No target within 50km: transition to Idle
                    rt.turret_cooldown_s = (rt.turret_cooldown_s - step_dt).max(0.0);
                    rt.missile_cooldown_s = (rt.missile_cooldown_s - step_dt).max(0.0);
                    if entity.state != NpcAiState::Idle {
                        entity.state = NpcAiState::Idle;
                        entity.target_id = None;
                        if max_velocity > 0.0 {
                            mode_updates.push(ModeUpdate {
                                ball_id,
                                mode: MovementMode::Stop,
                                speed_fraction: 0.0,
                            });
                        }
                    } else if entity.target_id.is_some() {
                        entity.target_id = None;
                    }
                }
            }
        }

        AiStepPlan {
            to_remove,
            mode_updates,
            dmg_actions,
        }
    }

    fn belt_and_gate_anchors(sde: &eve_sde::SdeRegistry, sys_id: u32) -> (Vec<DVec3>, Option<DVec3>) {
        let cels = sde.celestials_in_system(sys_id);
        let belts = cels.iter().filter(|c| c.group_id == 9).map(|c| c.position).collect();
        let gate = cels.iter().find(|c| c.group_id == 10).map(|c| c.position);
        (belts, gate)
    }

    fn is_belt_slot_on_cooldown(&self, sys_id: u32, slot: u8, group_id: u32, now_unix: u64) -> bool {
        if self.belt_slot_respawn_timers.get(&(sys_id, slot, group_id)).is_some_and(|&u| now_unix < u) {
            return true;
        }
        let managed = self
            .belt_slot_respawn_timers
            .iter()
            .any(|(&(s, _, gid), &u)| s == sys_id && gid == group_id && now_unix < u);
        !managed && self.respawn_timers.get(&(sys_id, group_id)).is_some_and(|&u| now_unix < u)
    }

    fn belt_rat_location_slot(sde: &eve_sde::SdeRegistry, sys_id: u32, pos: DVec3) -> Option<u8> {
        let (belts, gate) = Self::belt_and_gate_anchors(sde, sys_id);
        belts
            .iter()
            .take(3)
            .enumerate()
            .map(|(i, p)| (i as u8, *p))
            .chain(gate.into_iter().map(|p| (3u8, p)))
            .filter_map(|(slot, p)| {
                let d = pos.distance(p);
                (d < 25_000.0).then_some((slot, d))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(slot, _)| slot)
    }

    fn remove_npc_and_record_cooldown(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        npc_ball_id: i64,
        now_unix: u64,
    ) -> Option<(NpcCombatEntity, Option<u64>)> {
        self.runtime.remove(&npc_ball_id);
        let entity = self.entities.remove(&npc_ball_id)?;
        let until = now_unix.saturating_add(1_200);
        let clone_soldier_until = if crate::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype(
            ctx,
            entity.type_id,
        ) == "CloneSoldier"
        {
            self.clone_soldier_respawn_timers.insert(sys_id, until);
            Some(until)
        } else {
            if let Some(slot) = Self::belt_rat_location_slot(&ctx.sde, sys_id, entity.spawn_pos) {
                self.belt_slot_respawn_timers
                    .insert((sys_id, slot, entity.spawn_group_id), until);
            }
            self.respawn_timers
                .insert((sys_id, entity.spawn_group_id), until);
            None
        };
        Some((entity, clone_soldier_until))
    }

    fn apply_ai_step_cleanup(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        to_remove: Vec<i64>,
        destroyed_balls: Vec<(i64, i64, Ball)>,
        now_unix: u64,
    ) -> AiCleanupActions {
        let mut clone_soldier_cooldowns = Vec::new();
        for dead_id in to_remove {
            if let Some((_dead_ent, cs_until)) =
                self.remove_npc_and_record_cooldown(ctx, sys_id, dead_id, now_unix)
            {
                if let Some(until) = cs_until {
                    clone_soldier_cooldowns.push(until);
                }
            }
        }

        let mut destroyed_tracked_npcs = Vec::new();
        let mut remaining_destructions = Vec::with_capacity(destroyed_balls.len());
        for (attacker_npc_id, target_id, dead_ball) in destroyed_balls {
            if let Some((entity, cs_until)) =
                self.remove_npc_and_record_cooldown(ctx, sys_id, target_id, now_unix)
            {
                if let Some(until) = cs_until {
                    clone_soldier_cooldowns.push(until);
                }
                destroyed_tracked_npcs.push((attacker_npc_id, entity));
            } else {
                remaining_destructions.push((attacker_npc_id, target_id, dead_ball));
            }
        }

        AiCleanupActions {
            clone_soldier_cooldowns,
            destroyed_tracked_npcs,
            remaining_destructions,
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

    /// Handles death of an NPC and returns full wreck/killmail/bounty outcome details (`#1193`).
    pub(crate) fn handle_npc_destroyed_detailed(
        &mut self,
        ctx: &ServerContext,
        sys_id: u32,
        npc_ball_id: i64,
        killer_char_id: i64,
        now_unix: u64,
    ) -> Result<DestroyedNpcOutcome, String> {
        let Some((entity, cs_until)) =
            self.remove_npc_and_record_cooldown(ctx, sys_id, npc_ball_id, now_unix)
        else {
            return Err(format!(
                "NPC ball {npc_ball_id} not tracked by spawn manager"
            ));
        };
        if let Some(until) = cs_until {
            crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
                rt.clone_soldier_cooldown_until.insert(sys_id, until);
            });
        }
        finalize_destroyed_npc_side_effects(ctx, sys_id, &entity, killer_char_id, now_unix)
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
        self.handle_npc_destroyed_detailed(ctx, sys_id, npc_ball_id, killer_char_id, now_unix)
            .map(|outcome| outcome.bounty_isk)
    }
}

/// Result of finalizing a tracked NPC's destruction side effects (`#1193`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct DestroyedNpcOutcome {
    pub bounty_isk: f64,
    pub wreck_id: i64,
    pub wreck_type_id: u32,
    pub wreck_pos: DVec3,
    pub killmail_id: i64,
}

#[derive(Debug, Clone, Copy)]
struct AiBallKinematics {
    position: DVec3,
    radius: f64,
    target_sig_radius: f64,
}

#[derive(Debug, Clone)]
pub struct ModeUpdate {
    pub ball_id: i64,
    pub mode: MovementMode,
    pub speed_fraction: f64,
}

#[derive(Debug, Clone)]
pub enum AiDamageAction {
    Turret {
        attacker_id: i64,
        npc_type_id: u32,
        target_id: i64,
        module_type_id: Option<u32>,
        fx_guid: String,
        cycle_duration_s: f64,
        shot_damage: DamageVector,
        optimal_range: f64,
        falloff_range: f64,
        tracking_speed: f64,
        sig_resolution: f64,
        max_effective_range: f64,
    },
    Vorton {
        attacker_id: i64,
        npc_type_id: u32,
        target_id: i64,
        module_type_id: Option<u32>,
        fx_guid: String,
        cycle_duration_s: f64,
        shot_damage: DamageVector,
        optimal_range: f64,
        explosion_radius: f64,
        explosion_velocity: f64,
        drf: f64,
    },
    Missile {
        attacker_id: i64,
        npc_type_id: u32,
        target_id: i64,
        missile_type_id: u32,
        fx_guid: String,
        cycle_duration_s: f64,
        volley_damage: DamageVector,
        max_flight_range: f64,
        explosion_radius: f64,
        explosion_velocity: f64,
        drf: f64,
    },
    Synthetic {
        attacker_id: i64,
        npc_type_id: u32,
        target_id: i64,
        dmg: DamageVector,
    },
}

#[derive(Debug, Clone, Default)]
pub struct NpcAiWorldEffectsOutput {
    pub applied_hits: Vec<(i64, u32, i64, f64, bool, f64, f64, f64)>,
    pub destroyed_balls: Vec<(i64, i64, Ball)>,
    pub damaged_target_ids: Vec<i64>,
}

struct AiStepPlan {
    to_remove: Vec<i64>,
    mode_updates: Vec<ModeUpdate>,
    dmg_actions: Vec<AiDamageAction>,
}

struct AiCleanupActions {
    clone_soldier_cooldowns: Vec<u64>,
    destroyed_tracked_npcs: Vec<(i64, NpcCombatEntity)>,
    remaining_destructions: Vec<(i64, i64, Ball)>,
}

/// Steps the system's `ServerContext::npc_spawns` AI without holding `RANK_NPC_SPAWNS` across
/// `ballpark`, `db`, `combat_ephemeral`, or `death_pipeline` (`cmd_sender` / `try_send_fit_projection`).
pub(crate) fn step_system_npc_spawns_ai_sync(
    ctx: &ServerContext,
    sys_id: u32,
    current_tick: u64,
    dt: f64,
) {
    let npc_ids: HashSet<i64> = {
        let spawns = ctx.npc_spawns.read().unwrap_or_else(|e| e.into_inner());
        let Some(mgr) = spawns.get(&sys_id) else {
            return;
        };
        if mgr.entities.is_empty() {
            return;
        }
        mgr.entities.keys().copied().collect()
    };
    let (ball_kinematics, player_targets) = collect_ai_step_inputs(ctx, sys_id, &npc_ids);
    let plan = {
        let mut spawns = ctx.npc_spawns.write().unwrap_or_else(|e| e.into_inner());
        let Some(mgr) = spawns.get_mut(&sys_id) else {
            return;
        };
        mgr.plan_ai_step_for_ids(
            Some(&npc_ids),
            &ctx.sde,
            &ball_kinematics,
            &player_targets,
            current_tick,
            dt,
        )
    };
    let destroyed_balls = apply_ai_step_world_effects(
        ctx,
        sys_id,
        plan.mode_updates,
        plan.dmg_actions,
        &ball_kinematics,
    );
    let now_unix = ctx.now_unix();
    let actions = {
        let mut spawns = ctx.npc_spawns.write().unwrap_or_else(|e| e.into_inner());
        if let Some(mgr) = spawns.get_mut(&sys_id) {
            mgr.apply_ai_step_cleanup(ctx, sys_id, plan.to_remove, destroyed_balls, now_unix)
        } else {
            AiCleanupActions {
                clone_soldier_cooldowns: Vec::new(),
                destroyed_tracked_npcs: Vec::new(),
                remaining_destructions: destroyed_balls,
            }
        }
    };
    finalize_ai_step_cleanup(ctx, sys_id, actions, now_unix);
}

fn collect_ai_step_inputs(
    ctx: &ServerContext,
    sys_id: u32,
    npc_ids: &HashSet<i64>,
) -> (HashMap<i64, AiBallKinematics>, Vec<(i64, DVec3, f64)>) {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let view = sys_handle.view();
    let (mut ball_kinematics, mut cand_targets): (
        HashMap<i64, AiBallKinematics>,
        Vec<(i64, u32, DVec3, f64, Option<f64>, Option<f64>)>,
    ) = {
        let mut kin_map = HashMap::with_capacity(npc_ids.len());
        for &nid in npc_ids {
            if let Some(b) = view.balls.get(&nid) {
                kin_map.insert(
                    nid,
                    AiBallKinematics {
                        position: b.position,
                        radius: b.radius,
                        target_sig_radius: 0.0,
                    },
                );
            }
        }
        let mut cands = Vec::new();
        for (&id, b) in &view.balls {
            if npc_ids.contains(&id)
                || view.static_ball_ids.contains(&id)
                || view.non_free_ball_ids.contains(&id)
                || b.type_id == 670
                || b.max_velocity <= 0.0
                || matches!(b.mode, MovementMode::Warp { .. } | MovementMode::Rigid)
                || view
                    .combat_states
                    .get(&id)
                    .is_some_and(|cs| cs.blocks_targeted_effect() || cs.structure_hp <= 0.0)
            {
                continue;
            }
            let cat_id = view
                .ball_slim_meta
                .get(&id)
                .map(|m| m.category_id)
                .or_else(|| ctx.sde.types.get(&b.type_id).map(|t| t.category_id))
                .unwrap_or(6);
            if matches!(cat_id, 6 | 18 | 65) {
                let cs_opt = view.combat_states.get(&id);
                let cs_sig = cs_opt.map(|cs| cs.signature_radius).filter(|&s| s > 0.0);
                let cs_base_sig = cs_opt.and_then(|cs| cs.base_signature_radius).filter(|&s| s > 0.0);
                cands.push((
                    id,
                    b.type_id,
                    b.position,
                    b.radius,
                    cs_sig,
                    cs_base_sig,
                ));
            }
        }
        (kin_map, cands)
    };

    if !cand_targets.is_empty() {
        crate::services::combat_crimewatch::with_combat_runtime(ctx, |rt| {
            cand_targets.retain(|(id, _, _, _, _, _)| !rt.npc_entities.contains_key(id));
        });
    }

    // Resolve effective target signature radius (with fit / MWD / TP bloom) outside ballpark & npc_spawns locks.
    let mut target_sigs: HashMap<i64, f64> = HashMap::with_capacity(cand_targets.len());
    for &(bid, type_id, pos, radius, cs_sig_opt, cs_base_sig_opt) in &cand_targets {
        let raw_sde_sig = ctx
            .sde
            .types
            .get(&type_id)
            .and_then(|t| t.default_attributes.get(&eve_sde::attr_ids::SIGNATURE_RADIUS).copied())
            .filter(|&v| v > 0.0)
            .unwrap_or_else(|| radius.max(25.0));
        let eff_sig = if let Some(cs_sig) = cs_sig_opt.filter(|_| cs_base_sig_opt.is_some()) {
            cs_sig
        } else {
            let db_sig = ctx.effective_target_signature_radius(
                bid,
                cs_sig_opt.unwrap_or(raw_sde_sig),
            );
            if let Some(cs_sig) = cs_sig_opt {
                if (cs_sig - raw_sde_sig).abs() > 1e-3 && (db_sig - raw_sde_sig).abs() <= 1e-3 {
                    cs_sig
                } else {
                    db_sig
                }
            } else {
                db_sig
            }
        };
        target_sigs.insert(bid, eff_sig.max(1.0));
        ball_kinematics.entry(bid).or_insert(AiBallKinematics {
            position: pos,
            radius,
            target_sig_radius: eff_sig.max(1.0),
        });
    }

    // Find potential player targets (ships/drones/structures that are not NPCs, not asteroids/celestials, not capsules, not cloaked, not warping, and alive)
    let player_targets: Vec<(i64, DVec3, f64)> = if cand_targets.is_empty() {
        Vec::new()
    } else {
        ctx.db.with_snapshot(|snap| {
            let mut ewar_set: HashSet<i64> = HashSet::new();
            let mut logi_set: HashSet<i64> = HashSet::new();
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
                .map(|(bid, _type_id, pos, _, _, _)| {
                    let sig = target_sigs.get(&bid).copied().unwrap_or(40.0);
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

    (ball_kinematics, player_targets)
}

fn combat_state_ratios(bp: &eve_destiny::Ballpark, tid: i64) -> (f64, f64, f64) {
    bp.combat_states
        .get(&tid)
        .map(|cs| {
            (
                (cs.shield_hp / cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0),
                (cs.armor_hp / cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0),
                (cs.structure_hp / cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0),
            )
        })
        .unwrap_or((0.0, 0.0, 0.0))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_npc_ai_world_effects_in_actor(
    bp: &mut eve_destiny::Ballpark,
    mechanic_rng: &crate::rng::MechanicRng,
    target_sigs: &HashMap<i64, f64>,
    mode_updates: Vec<ModeUpdate>,
    dmg_actions: Vec<AiDamageAction>,
) -> NpcAiWorldEffectsOutput {
    let mut damaged_target_ids: Vec<i64> = Vec::with_capacity(dmg_actions.len());
    let mut destroyed_balls: Vec<(i64, i64, Ball)> = Vec::new();
    let mut applied_hits: Vec<(i64, u32, i64, f64, bool, f64, f64, f64)> = Vec::new();

    for upd in mode_updates {
        if matches!(upd.mode, MovementMode::Follow { .. })
            && upd.speed_fraction > 0.0
            && !bp.is_immobile_structure_or_static(upd.ball_id)
        {
            if let Some(b) = bp.balls.get_mut(&upd.ball_id) {
                b.speed_fraction = upd.speed_fraction.clamp(0.0, 1.0);
            }
        }
        let _ = bp.set_mode(upd.ball_id, upd.mode);
        let needs_speed_update = bp
            .balls
            .get(&upd.ball_id)
            .is_some_and(|b| (b.speed_fraction - upd.speed_fraction).abs() >= 1e-6);
        if needs_speed_update {
            let _ = bp.set_speed_fraction(upd.ball_id, upd.speed_fraction);
        }
    }
    for action in dmg_actions {
        let (
            attacker_id,
            npc_type_id,
            tid,
            module_type_id,
            other_type_id,
            fx_guid,
            cycle_duration_s,
            resolved_dmg,
            is_wrecking,
        ) = match action {
            AiDamageAction::Synthetic {
                attacker_id,
                npc_type_id,
                target_id,
                dmg,
            } => {
                if !bp.balls.contains_key(&attacker_id) || !bp.balls.contains_key(&target_id) {
                    continue;
                }
                (
                    attacker_id,
                    npc_type_id,
                    target_id,
                    None,
                    None,
                    NpcWeaponKind::Laser.fx_guid().to_string(),
                    2.0,
                    dmg,
                    false,
                )
            }
            AiDamageAction::Turret {
                attacker_id,
                npc_type_id,
                target_id,
                module_type_id,
                fx_guid,
                cycle_duration_s,
                shot_damage,
                optimal_range,
                falloff_range,
                tracking_speed,
                sig_resolution,
                max_effective_range,
            } => {
                let Some((dmg, wreck)) = evaluate_npc_turret_shot_with_rng(
                    mechanic_rng,
                    bp,
                    target_sigs,
                    attacker_id,
                    target_id,
                    shot_damage,
                    optimal_range,
                    falloff_range,
                    tracking_speed,
                    sig_resolution,
                    max_effective_range,
                ) else {
                    continue;
                };
                (
                    attacker_id,
                    npc_type_id,
                    target_id,
                    module_type_id,
                    None,
                    fx_guid,
                    cycle_duration_s,
                    dmg,
                    wreck,
                )
            }
            AiDamageAction::Vorton {
                attacker_id,
                npc_type_id,
                target_id,
                module_type_id,
                fx_guid,
                cycle_duration_s,
                shot_damage,
                optimal_range,
                explosion_radius,
                explosion_velocity,
                drf,
            } => {
                let Some(dmg) = evaluate_npc_missile_or_vorton_hit(
                    bp,
                    target_sigs,
                    attacker_id,
                    target_id,
                    shot_damage,
                    optimal_range,
                    explosion_radius,
                    explosion_velocity,
                    drf,
                ) else {
                    continue;
                };
                (
                    attacker_id,
                    npc_type_id,
                    target_id,
                    module_type_id,
                    None,
                    fx_guid,
                    cycle_duration_s,
                    dmg,
                    false,
                )
            }
            AiDamageAction::Missile {
                attacker_id,
                npc_type_id,
                target_id,
                missile_type_id,
                fx_guid,
                cycle_duration_s,
                volley_damage,
                max_flight_range,
                explosion_radius,
                explosion_velocity,
                drf,
            } => {
                let Some(dmg) = evaluate_npc_missile_or_vorton_hit(
                    bp,
                    target_sigs,
                    attacker_id,
                    target_id,
                    volley_damage,
                    max_flight_range,
                    explosion_radius,
                    explosion_velocity,
                    drf,
                ) else {
                    continue;
                };
                (
                    attacker_id,
                    npc_type_id,
                    target_id,
                    None,
                    Some(missile_type_id),
                    fx_guid,
                    cycle_duration_s,
                    dmg,
                    false,
                )
            }
        };

        damaged_target_ids.push(tid);
        emit_npc_special_fx(
            bp,
            attacker_id,
            tid,
            module_type_id,
            other_type_id,
            fx_guid,
            cycle_duration_s,
        );
        if resolved_dmg.total() > 0.0 {
            if let Some((applied, destroyed, dead_ball_opt)) = bp.apply_damage_to_ball(tid, resolved_dmg) {
                let (s_r, a_r, h_r) = combat_state_ratios(bp, tid);
                applied_hits.push((
                    attacker_id,
                    npc_type_id,
                    tid,
                    applied,
                    is_wrecking,
                    s_r,
                    a_r,
                    h_r,
                ));
                if destroyed {
                    if let Some(dead_ball) = dead_ball_opt {
                        destroyed_balls.push((attacker_id, tid, dead_ball));
                    }
                }
            }
        } else {
            let (s_r, a_r, h_r) = combat_state_ratios(bp, tid);
            applied_hits.push((attacker_id, npc_type_id, tid, 0.0, false, s_r, a_r, h_r));
        }
    }

    NpcAiWorldEffectsOutput {
        applied_hits,
        destroyed_balls,
        damaged_target_ids,
    }
}

fn apply_ai_step_world_effects(
    ctx: &ServerContext,
    sys_id: u32,
    mode_updates: Vec<ModeUpdate>,
    dmg_actions: Vec<AiDamageAction>,
    ball_kinematics: &HashMap<i64, AiBallKinematics>,
) -> Vec<(i64, i64, Ball)> {
    let sys_handle = ctx.systems.get_or_spawn(sys_id);
    let target_sigs: HashMap<i64, f64> = ball_kinematics
        .iter()
        .map(|(&id, k)| (id, k.target_sig_radius))
        .collect();
    let NpcAiWorldEffectsOutput {
        applied_hits,
        destroyed_balls,
        damaged_target_ids,
    } = if !mode_updates.is_empty() || !dmg_actions.is_empty() {
        match sys_handle.apply_npc_ai_step_effects_sync(
            std::sync::Arc::clone(&ctx.mechanic_rng),
            target_sigs,
            mode_updates,
            dmg_actions,
        ) {
            Ok(out) => out,
            Err(err) => {
                tracing::warn!(
                    solar_system_id = sys_id,
                    error = %err,
                    "CMB-51 apply_ai_step_world_effects actor command failed"
                );
                return Vec::new();
            }
        }
    } else {
        NpcAiWorldEffectsOutput::default()
    };

    if !applied_hits.is_empty() {
        // Resolve the active controller/pilot for hit targets (`#1565`): abandoned drones/fighters
        // must not resolve to former owners, while docked/out-of-system suppression is enforced
        // downstream by outbound scene scoping (`#1190`).
        let active_craft_owners: HashMap<i64, Option<i64>> = {
            let sys_view_opt = ctx.systems.get_handle(sys_id).map(|h| h.hydrated_view());
            let mut map = applied_hits
                .iter()
                .filter_map(|&(_, _, tid, ..)| {
                    sys_view_opt
                        .as_ref()
                        .and_then(|v| v.active_drones_in_space.get(&tid))
                        .map(|d| {
                            let owner = (!d.is_abandoned
                                && d.owner_char_id > 0
                                && d.solar_system_id == sys_id)
                                .then_some(d.owner_char_id);
                            (tid, owner)
                        })
                })
                .collect::<HashMap<i64, Option<i64>>>();
            if let Some(sys_h) = ctx.systems.get_handle(sys_id) {
                ctx.systems.ensure_live_fighters_hydrated_if_unlocked_sync(sys_id, &sys_h);
                let view = sys_h.view();
                for &(_, _, tid, ..) in &applied_hits {
                    if let Some(sq) = view.fighters.active_fighters.get(&tid) {
                        let owner = (!sq.is_abandoned && sq.owner_char_id > 0)
                            .then_some(sq.owner_char_id);
                        map.entry(tid).or_insert(owner);
                    }
                }
            }
            map
        };
        for (attacker_id, npc_type_id, tid, applied, is_wrecking, s_r, a_r, h_r) in applied_hits {
            let victim_char_opt = ctx.db.with_snapshot(|snap| {
                let ch = match active_craft_owners.get(&tid) {
                    Some(Some(cid)) => snap.characters.get(cid),
                    Some(None) => None,
                    None => snap
                        .characters
                        .get(&tid)
                        .or_else(|| snap.characters.values().find(|c| c.active_ship_id == tid)),
                }?;
                Some(ch.character_id)
            });
            if victim_char_opt.is_some() {
                crate::services::combat_crimewatch::emit_combat_log_and_damage_state(
                    ctx,
                    attacker_id,
                    victim_char_opt,
                    attacker_id,
                    tid,
                    npc_type_id,
                    applied,
                    is_wrecking,
                    s_r,
                    a_r,
                    h_r,
                    Some(sys_id),
                );
            }
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
                        .or_else(|| snap.characters.values().find(|c| c.active_ship_id == tid))
                        .filter(|c| {
                            c.solar_system_id == sys_id
                                && c.station_id.is_none()
                                && c.structure_id.is_none_or(|st_id| c.active_ship_id == st_id)
                        })
                        .map(|c| c.character_id)
                })
                .collect()
        });
        for cid in hit_chars {
            crate::sp6_lifecycle::record_npc_aggression_timer(ctx, cid, now_unix);
        }
    }

    destroyed_balls
}

fn finalize_ai_step_cleanup(
    ctx: &ServerContext,
    sys_id: u32,
    actions: AiCleanupActions,
    now_unix: u64,
) {
    if !actions.clone_soldier_cooldowns.is_empty() {
        crate::services::combat_crimewatch::with_mut_combat_runtime(ctx, |rt| {
            for until in actions.clone_soldier_cooldowns {
                rt.clone_soldier_cooldown_until.insert(sys_id, until);
            }
        });
    }
    for (attacker_npc_id, entity) in actions.destroyed_tracked_npcs {
        let _ = finalize_destroyed_npc_side_effects(
            ctx,
            sys_id,
            &entity,
            attacker_npc_id,
            now_unix,
        );
    }
    if !actions.remaining_destructions.is_empty() {
        handle_npc_ai_ball_destructions(ctx, sys_id, actions.remaining_destructions);
    }
}

/// Atomically inserts a physical NPC wreck item and all contained loot items inside a single
/// [`WorldStateSnapshot`] transaction.
pub(crate) fn spawn_npc_wreck_and_loot_in_snap(
    snap: &mut WorldStateSnapshot,
    solar_system_id: u32,
    owner_char_id: i64,
    pos: DVec3,
    now_unix: u64,
    wreck_type_id: u32,
    loot_entries: &[(u32, i32, bool)],
) -> i64 {
    let make_item = |item_id, type_id, location_id, quantity, singleton, is_bpc| InventoryItemRecord {
        item_id,
        type_id,
        owner_id: owner_char_id,
        location_id,
        flag: InventoryFlag::Cargo,
        quantity,
        singleton,
        version: 1,
        dynamic_attributes: HashMap::new(),
        source_mutaplasmid_id: None,
        insured_payout_isk: 0.0,
        is_blueprint_copy: is_bpc,
        blueprint_runs_remaining: is_bpc.then_some(1),
    };

    let wreck_id = snap.alloc_entity_id();
    crate::sp6_lifecycle::record_wreck_or_container_meta_in_snap(snap, wreck_id, now_unix, true);
    snap.items.insert(
        wreck_id,
        make_item(wreck_id, wreck_type_id, solar_system_id as i64, 1, true, false),
    );
    snap.item_positions
        .insert(wreck_id, [pos.x, pos.y, pos.z]);

    for &(loot_type_id, raw_qty, is_bpc) in loot_entries {
        let lid = snap.alloc_entity_id();
        let qty = if is_bpc { 1 } else { raw_qty.max(1) };
        snap.items.insert(
            lid,
            make_item(lid, loot_type_id, wreck_id, qty, is_bpc, is_bpc),
        );
    }

    wreck_id
}

fn finalize_destroyed_npc_side_effects(
    ctx: &ServerContext,
    sys_id: u32,
    entity: &NpcCombatEntity,
    killer_char_id: i64,
    now_unix: u64,
) -> Result<DestroyedNpcOutcome, String> {
    let supp_mult = crate::services::security_mgr::insurgency_suppression_bounty_and_sec_multiplier(ctx, sys_id);
    let bounty = entity.bounty_isk * supp_mult;
    let npc_ball_id = entity.ball_id;

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
            let kid = snap.alloc_entity_id();
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

            let (wreck_id, wreck_type_id) = if !(32_000_000..=32_999_999).contains(&sys_id) {
                let wreck_type_id =
                    resolve_npc_wreck_type_id(&ctx.sde, Some(entity.type_id), None);
                let size_class = NpcSizeClass::from_sde(&ctx.sde, Some(entity.type_id));
                let mut loot_entries = vec![(15331_u32, size_class.scrap_quantity(), false)];
                let (profile, _) = inspect_npc_sde_profile(
                    &ctx.sde,
                    entity.type_id,
                    Some(entity.bounty_isk),
                    entity.faction_id,
                );
                if let Some((ammo_tid, ammo_qty)) = resolve_npc_weapon_ammo_loot(
                    &ctx.sde,
                    Some(entity.type_id),
                    &profile,
                    Some(entity.faction_id),
                ) {
                    loot_entries.push((ammo_tid, ammo_qty, false));
                }
                let wreck_id = spawn_npc_wreck_and_loot_in_snap(
                    snap,
                    sys_id,
                    killer_char_id,
                    entity.spawn_pos,
                    now_unix,
                    wreck_type_id,
                    &loot_entries,
                );
                (wreck_id, wreck_type_id)
            } else {
                (0, 0)
            };

            Ok::<_, String>(DestroyedNpcOutcome {
                bounty_isk: bounty,
                wreck_id,
                wreck_type_id,
                wreck_pos: entity.spawn_pos,
                killmail_id: kid,
            })
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
        crate::services::agents_missions::agents_standings_arcs::apply_npc_kill_faction_standing_loss(
            ctx,
            sys_id,
            killer_char_id,
            entity.type_id,
            (entity.faction_id > 0).then_some(entity.faction_id),
            None,
            now_unix,
        );
        crate::quasar_gateway::isolate_side_effect("combat.npc_ai.npc_destroyed_progression", || {
            ctx.quasar_gateway.progression.daily_goals.on_gameplay_event(
                killer_char_id as u32,
                1,
                1,
                &ctx.quasar_gateway,
            );
            let ship_group_id = crate::services::quasar_career_goals::character_active_ship_group_id(
                &ctx.db,
                &ctx.sde,
                killer_char_id,
            );
            let mut combat_ev = crate::services::quasar_career_goals::solar_system_career_event(
                &ctx.sde,
                "combat",
                sys_id,
            );
            combat_ev.ship_group_id = ship_group_id;
            ctx.quasar_gateway.progression.career_goals.on_career_gameplay_event(
                killer_char_id as u32,
                combat_ev,
                1,
                &ctx.quasar_gateway,
            );
            if crate::services::combat_crimewatch::combat_npc_ai::derive_npc_archetype(
                ctx,
                entity.type_id,
            ) == "Triglavian"
            {
                crate::services::quasar_career_goals::record_career_sub_kind(
                    ctx,
                    killer_char_id,
                    "abyssal",
                    Some("kill_triglavians"),
                    1,
                );
            }
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
        });
    }
    res
}

/// Processes ball destructions returned by `bp.apply_damage_to_ball` during NPC AI ticks:
/// routes all destroyed balls through the unified [`crate::combat::death_pipeline::on_ball_destroyed_sync`]
/// pipeline (`#918`, `#835`, `#858`).
pub(crate) fn handle_npc_ai_ball_destructions(
    ctx: &ServerContext,
    sys_id: u32,
    destructions: Vec<(i64, i64, Ball)>,
) {
    for (attacker_npc_id, target_id, dead_ball) in destructions {
        let _ = crate::combat::death_pipeline::on_ball_destroyed_sync(
            ctx,
            sys_id,
            target_id,
            Some(attacker_npc_id),
            dead_ball.position,
        );
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

#[cfg(any(test, feature = "testkit"))]
pub mod npc_test_helpers {
    //! Test-only façade over runtime NPC entity tracking (`#1541` Track C2b/C2c).
    //!
    //! Compiled under `#[cfg(any(test, feature = "testkit"))]` so both unit tests (`--lib`) and
    //! integration tests (`--test it`) access NPC entity state without reaching directly into
    //! `CombatRuntimeState::npc_entities`.

    use crate::router::ServerContext;
    use crate::services::combat_crimewatch::{
        with_combat_ephemeral, with_mut_combat_ephemeral, ExtendedNpcEntity,
    };

    /// Inserts or replaces an [`ExtendedNpcEntity`] keyed by `entity.ball_id`.
    pub fn seed_npc_entity(ctx: &ServerContext, entity: ExtendedNpcEntity) {
        with_mut_combat_ephemeral(ctx, |rt| {
            rt.npc_entities.insert(entity.ball_id, entity);
        });
    }

    /// Returns a cloned [`ExtendedNpcEntity`] for `ball_id`, if present.
    pub fn get_npc_entity(ctx: &ServerContext, ball_id: i64) -> Option<ExtendedNpcEntity> {
        with_combat_ephemeral(ctx, |rt| rt.npc_entities.get(&ball_id).cloned())
    }

    /// Returns `true` if an [`ExtendedNpcEntity`] exists for `ball_id`.
    pub fn has_npc_entity(ctx: &ServerContext, ball_id: i64) -> bool {
        with_combat_ephemeral(ctx, |rt| rt.npc_entities.contains_key(&ball_id))
    }

    /// Mutates the [`ExtendedNpcEntity`] for `ball_id` in place if present, returning `f`'s output.
    pub fn update_npc_entity<R>(
        ctx: &ServerContext,
        ball_id: i64,
        f: impl FnOnce(&mut ExtendedNpcEntity) -> R,
    ) -> Option<R> {
        with_mut_combat_ephemeral(ctx, |rt| rt.npc_entities.get_mut(&ball_id).map(f))
    }

    /// Removes and returns the [`ExtendedNpcEntity`] for `ball_id`, if present.
    pub fn remove_npc_entity(ctx: &ServerContext, ball_id: i64) -> Option<ExtendedNpcEntity> {
        with_mut_combat_ephemeral(ctx, |rt| rt.npc_entities.remove(&ball_id))
    }

    /// Removes all [`ExtendedNpcEntity`] entries in `solar_system_id` matching `pred`, returning their ball IDs.
    pub fn remove_npc_entities_in_system_where(
        ctx: &ServerContext,
        solar_system_id: u32,
        mut pred: impl FnMut(&ExtendedNpcEntity) -> bool,
    ) -> Vec<i64> {
        with_mut_combat_ephemeral(ctx, |rt| {
            let ids: Vec<i64> = rt
                .npc_entities
                .iter()
                .filter(|(_, n)| n.solar_system_id == solar_system_id && pred(n))
                .map(|(&id, _)| id)
                .collect();
            for id in &ids {
                rt.npc_entities.remove(id);
            }
            ids
        })
    }

    /// Clears all tracked NPC entities across all systems.
    pub fn clear_npc_entities(ctx: &ServerContext) {
        with_mut_combat_ephemeral(ctx, |rt| {
            rt.npc_entities.clear();
        });
    }

    /// Returns the total number of tracked NPC entities across all systems.
    pub fn npc_entity_count(ctx: &ServerContext) -> usize {
        with_combat_ephemeral(ctx, |rt| rt.npc_entities.len())
    }

    /// Returns the ball IDs of all tracked NPC entities across all systems.
    pub fn npc_entity_ids(ctx: &ServerContext) -> Vec<i64> {
        with_combat_ephemeral(ctx, |rt| rt.npc_entities.keys().copied().collect())
    }

    /// Returns cloned [`ExtendedNpcEntity`] records belonging to `solar_system_id`.
    pub fn list_npc_entities_in_system(
        ctx: &ServerContext,
        solar_system_id: u32,
    ) -> Vec<ExtendedNpcEntity> {
        with_combat_ephemeral(ctx, |rt| {
            rt.npc_entities
                .values()
                .filter(|n| n.solar_system_id == solar_system_id)
                .cloned()
                .collect()
        })
    }

    /// Returns the first tracked [`ExtendedNpcEntity`] matching `pred`.
    pub fn find_npc_entity(
        ctx: &ServerContext,
        mut pred: impl FnMut(&ExtendedNpcEntity) -> bool,
    ) -> Option<ExtendedNpcEntity> {
        with_combat_ephemeral(ctx, |rt| rt.npc_entities.values().find(|n| pred(n)).cloned())
    }

    /// Returns the first tracked [`ExtendedNpcEntity`] in `solar_system_id` matching `pred`.
    pub fn find_npc_entity_in_system(
        ctx: &ServerContext,
        solar_system_id: u32,
        mut pred: impl FnMut(&ExtendedNpcEntity) -> bool,
    ) -> Option<ExtendedNpcEntity> {
        with_combat_ephemeral(ctx, |rt| {
            rt.npc_entities
                .values()
                .find(|n| n.solar_system_id == solar_system_id && pred(n))
                .cloned()
        })
    }

    /// Returns `true` if any tracked [`ExtendedNpcEntity`] matches `pred`.
    pub fn any_npc_entity(
        ctx: &ServerContext,
        mut pred: impl FnMut(&ExtendedNpcEntity) -> bool,
    ) -> bool {
        with_combat_ephemeral(ctx, |rt| rt.npc_entities.values().any(|n| pred(n)))
    }

    /// Returns `true` if any tracked [`ExtendedNpcEntity`] in `solar_system_id` matches `pred`.
    pub fn any_npc_entity_in_system(
        ctx: &ServerContext,
        solar_system_id: u32,
        mut pred: impl FnMut(&ExtendedNpcEntity) -> bool,
    ) -> bool {
        with_combat_ephemeral(ctx, |rt| {
            rt.npc_entities
                .values()
                .any(|n| n.solar_system_id == solar_system_id && pred(n))
        })
    }
}

#[cfg(test)] #[path = "npc_ai_unit_tests.rs"]
mod npc_ai_unit_tests;

#[cfg(test)]
#[path = "npc_ai_edges_tests.rs"]
mod npc_ai_edges_tests;

