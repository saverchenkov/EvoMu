//! `pve_homefront_sites` — Homefront Operations seeding, multi-role mission runtime,
//! acceleration-gate ship restrictions, 5-pilot role-cap payout settlement, and
//! V24 `dungeonInstanceCacheMgr.GetHomefrontSiteInstances` RPC handling
//! (`feat:pve.homefront-ops`, `rpc:dungeonInstanceCacheMgr.GetHomefrontSiteInstances`).

#![forbid(unsafe_code)]

use crate::router::ServerContext;
use crate::services::pve_special_sites::{
    with_pve_special_reg, HomefrontSiteRecord, PveSpecialSitesRegistry,
};
use crate::services::pve_special_sites_ops::{
    compute_homefront_fleet_payouts, resolve_homefront_archetype_blueprint,
    resolve_sde_homefront_templates_for_system, validate_homefront_ship_eligibility,
    HomefrontArchetype, HomefrontFleetPayoutSummary, HomefrontOperationBlueprint,
    HomefrontParticipantContribution, HomefrontShipEligibility,
};
use eve_db::{IskCents, WorldRepository};
use eve_destiny::{Ball, MovementMode};
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use eve_sde::{SolarSystemId, TypeId};
use glam::DVec3;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock, Weak};

fn hf_err(code: &str, message: impl Into<String>) -> BridgeRpcError {
    BridgeRpcError {
        code: code.to_owned(),
        message: message.into(),
        client_error: None,
    }
}

/// Archetype-specific multi-role progression state for an active Homefront site instance.
#[derive(Debug, Clone, PartialEq)]
pub enum HomefrontArchetypeProgress {
    Raid {
        haulers_spawned: u8,
        haulers_destroyed: u8,
        haulers_escaped: u8,
        active_hauler_ball_id: Option<i64>,
        active_hauler_tackled: bool,
        cargo_dropped: u8,
        cargo_delivered: u8,
        required_deliveries: u8,
    },
    EmergencyAid {
        protectee_ball_id: i64,
        protectee_hp: f64,
        protectee_max_hp: f64,
        survival_elapsed_secs: u64,
        required_survival_secs: u64,
        waves_repelled: u8,
    },
    MetaliminalMeteoroid {
        meteoroid_ball_id: i64,
        mutanite_type_id: TypeId,
        remaining_units: u32,
        total_units: u32,
        total_residue_units: u32,
        ore_mined_by_fleet: u32,
    },
    DreadAssault {
        dread_ball_id: i64,
        base_ball_id: i64,
        capacitor_gj: f64,
        capacitor_target_gj: f64,
        siege_cycles_completed: u8,
        target_siege_cycles: u8,
        in_siege_mode: bool,
        siege_remaining_secs: u64,
        enemy_base_hp: f64,
        enemy_base_max_hp: f64,
    },
    AbyssalArtifactRecovery {
        current_wave: u8,
        total_waves: u8,
        mutanite_units_remaining: u32,
        wave_hostiles_remaining: u8,
        warp_bubble_active: bool,
        caches_recovered: u32,
        waves_paid: u8,
        extraction_window_secs: u64,
    },
    SuspiciousSignal {
        relay_ball_id: i64,
        array_ball_ids: Vec<i64>,
        arrays_hacked: [bool; 3],
        vulnerability_window_secs: u64,
        relay_hp: f64,
        relay_max_hp: f64,
    },
    SalvageResearch {
        wrecks_salvaged: u8,
        total_wrecks: u8,
        werpost_activated: bool,
    },
    StabilizeRift {
        arrays_cap_pct: [f64; 5],
        held_low_secs: u64,
        required_hold_secs: u64,
    },
    TrafficStop {
        smugglers_destroyed: u8,
        required_smugglers: u8,
        smugglers_escaped: u8,
        civilians_harmed: u8,
    },
}

/// Live runtime state for an active Homefront Operation instance.
#[derive(Debug, Clone)]
pub struct HomefrontRuntimeState {
    pub blueprint: HomefrontOperationBlueprint,
    pub solar_system_id: SolarSystemId,
    pub instance_id: i64,
    pub site_position: DVec3,
    pub beacon_ball_id: i64,
    pub dispatch_boss_ball_id: i64,
    pub started: bool,
    pub started_at_sec: u64,
    pub expires_at_sec: u64,
    pub completed: bool,
    pub failed: bool,
    pub failure_reason: Option<String>,
    pub spawned_ball_ids: Vec<i64>,
    pub entered_pilots: HashMap<i64, HomefrontShipEligibility>,
    pub contributions: BTreeMap<i64, HomefrontParticipantContribution>,
    pub progress: HomefrontArchetypeProgress,
}

impl HomefrontRuntimeState {
    pub fn is_objective_complete(&self) -> bool {
        if self.failed {
            return false;
        }
        match &self.progress {
            HomefrontArchetypeProgress::Raid {
                cargo_delivered,
                required_deliveries,
                ..
            } => *cargo_delivered >= *required_deliveries,
            HomefrontArchetypeProgress::EmergencyAid {
                protectee_hp,
                survival_elapsed_secs,
                required_survival_secs,
                ..
            } => *protectee_hp > 0.0 && *survival_elapsed_secs >= *required_survival_secs,
            HomefrontArchetypeProgress::MetaliminalMeteoroid {
                remaining_units, ..
            } => *remaining_units == 0,
            HomefrontArchetypeProgress::DreadAssault {
                siege_cycles_completed,
                target_siege_cycles,
                enemy_base_hp,
                ..
            } => *siege_cycles_completed >= *target_siege_cycles && *enemy_base_hp <= 0.0,
            HomefrontArchetypeProgress::AbyssalArtifactRecovery {
                current_wave,
                total_waves,
                mutanite_units_remaining,
                wave_hostiles_remaining,
                warp_bubble_active,
                ..
            } => {
                *current_wave >= *total_waves
                    && *mutanite_units_remaining == 0
                    && *wave_hostiles_remaining == 0
                    && !*warp_bubble_active
            }
            HomefrontArchetypeProgress::SuspiciousSignal { relay_hp, .. } => *relay_hp <= 0.0,
            HomefrontArchetypeProgress::SalvageResearch {
                wrecks_salvaged,
                total_wrecks,
                ..
            } => *wrecks_salvaged >= *total_wrecks,
            HomefrontArchetypeProgress::StabilizeRift {
                held_low_secs,
                required_hold_secs,
                ..
            } => *held_low_secs >= *required_hold_secs,
            HomefrontArchetypeProgress::TrafficStop {
                smugglers_destroyed,
                required_smugglers,
                civilians_harmed,
                ..
            } => *smugglers_destroyed >= *required_smugglers && *civilians_harmed <= 2,
        }
    }
}

struct WorldRuntimeBucket {
    db_weak: Weak<WorldRepository>,
    sites: HashMap<i64, HomefrontRuntimeState>,
}

fn homefront_runtime_store() -> &'static Mutex<HashMap<usize, WorldRuntimeBucket>> {
    static STORE: OnceLock<Mutex<HashMap<usize, WorldRuntimeBucket>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_homefront_runtime<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut HashMap<i64, HomefrontRuntimeState>) -> R,
) -> R {
    let key = std::sync::Arc::as_ptr(&ctx.db) as usize;
    let store = homefront_runtime_store();
    let mut guard = store
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Evict dropped WorldRepositories first so allocator pointer reuse never leaks stale state.
    guard.retain(|_, bucket| bucket.db_weak.strong_count() > 0);
    let bucket = guard.entry(key).or_insert_with(|| WorldRuntimeBucket {
        db_weak: std::sync::Arc::downgrade(&ctx.db),
        sites: HashMap::new(),
    });
    f(&mut bucket.sites)
}

fn compute_homefront_site_position(instance_id: i64, slot_index: usize) -> DVec3 {
    DVec3::new(
        10_000.0 + ((instance_id % 100) as f64) * 5_000.0 + (slot_index as f64) * 2_500.0,
        ((instance_id % 10) as f64) * 1_000.0,
        0.0,
    )
}

fn build_initial_progress(
    bp: &HomefrontOperationBlueprint,
    instance_id: i64,
) -> HomefrontArchetypeProgress {
    let base_obj = 1_897_800_000 + instance_id * 10;
    match bp.archetype {
        HomefrontArchetype::Raid => HomefrontArchetypeProgress::Raid {
            haulers_spawned: 1,
            haulers_destroyed: 0,
            haulers_escaped: 0,
            active_hauler_ball_id: Some(1_897_700_000 + instance_id),
            active_hauler_tackled: false,
            cargo_dropped: 0,
            cargo_delivered: 0,
            required_deliveries: 4,
        },
        HomefrontArchetype::EmergencyAid => HomefrontArchetypeProgress::EmergencyAid {
            protectee_ball_id: base_obj + 2,
            protectee_hp: 40_000.0,
            protectee_max_hp: 40_000.0,
            survival_elapsed_secs: 0,
            required_survival_secs: bp.time_limit_secs,
            waves_repelled: 0,
        },
        HomefrontArchetype::MetaliminalMeteoroid => {
            HomefrontArchetypeProgress::MetaliminalMeteoroid {
                meteoroid_ball_id: base_obj + 2,
                mutanite_type_id: bp.support_entity_type_id.unwrap_or(77_118),
                remaining_units: 5_000,
                total_units: 5_000,
                total_residue_units: 0,
                ore_mined_by_fleet: 0,
            }
        }
        HomefrontArchetype::DreadAssault => HomefrontArchetypeProgress::DreadAssault {
            dread_ball_id: base_obj + 3,
            base_ball_id: base_obj + 2,
            capacitor_gj: 0.0,
            capacitor_target_gj: 30_000.0,
            siege_cycles_completed: 0,
            target_siege_cycles: 3,
            in_siege_mode: false,
            siege_remaining_secs: 0,
            enemy_base_hp: 5_400_000.0,
            enemy_base_max_hp: 5_400_000.0,
        },
        HomefrontArchetype::AbyssalArtifactRecovery => {
            HomefrontArchetypeProgress::AbyssalArtifactRecovery {
                current_wave: 1,
                total_waves: 9,
                mutanite_units_remaining: 30,
                wave_hostiles_remaining: 4,
                warp_bubble_active: true,
                caches_recovered: 0,
                waves_paid: 0,
                extraction_window_secs: 90,
            }
        }
        HomefrontArchetype::SuspiciousSignal => HomefrontArchetypeProgress::SuspiciousSignal {
            relay_ball_id: base_obj + 2,
            array_ball_ids: vec![base_obj + 4, base_obj + 5, base_obj + 6],
            arrays_hacked: [false, false, false],
            vulnerability_window_secs: 0,
            relay_hp: 350_000.0,
            relay_max_hp: 350_000.0,
        },
        HomefrontArchetype::SalvageResearch => HomefrontArchetypeProgress::SalvageResearch {
            wrecks_salvaged: 0,
            total_wrecks: 30,
            werpost_activated: false,
        },
        HomefrontArchetype::StabilizeRift => HomefrontArchetypeProgress::StabilizeRift {
            arrays_cap_pct: [1.0; 5],
            held_low_secs: 0,
            required_hold_secs: 60,
        },
        HomefrontArchetype::TrafficStop => HomefrontArchetypeProgress::TrafficStop {
            smugglers_destroyed: 0,
            required_smugglers: 6,
            smugglers_escaped: 0,
            civilians_harmed: 0,
        },
    }
}

fn ensure_runtime_state_for_site(
    ctx: &ServerContext,
    site: &HomefrontSiteRecord,
    slot_index: usize,
) -> HomefrontRuntimeState {
    let bp = resolve_homefront_archetype_blueprint(
        &ctx.sde,
        site.dungeon_id,
        &site.dungeon_name,
        site.solar_system_id,
        2,
    );
    with_homefront_runtime(ctx, |map| {
        let needs_reset = map
            .get(&site.instance_id)
            .map(|st| st.blueprint.dungeon_id != site.dungeon_id || st.solar_system_id != site.solar_system_id)
            .unwrap_or(true);
        if needs_reset {
            let beacon_ball_id = 1_897_800_000 + site.instance_id * 10 + 1;
            let state = HomefrontRuntimeState {
                blueprint: bp.clone(),
                solar_system_id: site.solar_system_id,
                instance_id: site.instance_id,
                site_position: compute_homefront_site_position(site.instance_id, slot_index),
                beacon_ball_id,
                dispatch_boss_ball_id: site.boss_npc_ball_id,
                started: false,
                started_at_sec: 0,
                expires_at_sec: 0,
                completed: site.completed,
                failed: false,
                failure_reason: None,
                spawned_ball_ids: Vec::new(),
                entered_pilots: HashMap::new(),
                contributions: BTreeMap::new(),
                progress: build_initial_progress(&bp, site.instance_id),
            };
            map.insert(site.instance_id, state);
        } else if site.completed {
            if let Some(st) = map.get_mut(&site.instance_id) {
                st.completed = true;
            }
        }
        map[&site.instance_id].clone()
    })
}

fn find_site_record_with_slot(
    ctx: &ServerContext,
    instance_id: i64,
) -> Result<(HomefrontSiteRecord, usize), BridgeRpcError> {
    with_pve_special_reg(ctx, |reg| {
        for list in reg.homefront_sites.values() {
            for (idx, site) in list.iter().enumerate() {
                if site.instance_id == instance_id {
                    return Ok((site.clone(), idx));
                }
            }
        }
        Err(hf_err(
            "UnknownDungeonInstance",
            format!("Homefront instance {instance_id} does not exist"),
        ))
    })
}

/// Seeds up to 2 high-sec Homefront Operations for `sys_id` using V24 `archetype_id == 70` templates
/// and archetype/faction blueprints (no generic faction cruiser boss stub).
pub fn ensure_homefront_sites_for_system_locked(
    ctx: &ServerContext,
    reg: &mut PveSpecialSitesRegistry,
    sys_id: SolarSystemId,
) {
    if reg.homefront_sites.contains_key(&sys_id) {
        return;
    }
    let sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if sec < 0.45 || sys_id >= 31_000_000 {
        // Homefront operations only spawn in High-Security empire space
        reg.homefront_sites.insert(sys_id, Vec::new());
        return;
    }

    let templates = resolve_sde_homefront_templates_for_system(&ctx.sde, sys_id);
    if templates.is_empty() {
        reg.homefront_sites.insert(sys_id, Vec::new());
        return;
    }

    let rng = crate::rng::MechanicRng::seeded(
        (sys_id as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ ctx.now_unix(),
    );
    let idx1 = (rng.try_next_u64().unwrap_or(sys_id as u64) as usize) % templates.len();
    let idx2 = if templates.len() > 1 {
        let mut step = 1 + ((rng.try_next_u64().unwrap_or(1) as usize) % (templates.len() - 1));
        let arch1 = HomefrontArchetype::from_dungeon(
            templates[idx1].dungeon_id,
            &templates[idx1].dungeon_name,
        );
        for offset in 0..(templates.len() - 1) {
            let cand = (idx1 + ((step + offset) % (templates.len() - 1)).max(1)) % templates.len();
            if HomefrontArchetype::from_dungeon(
                templates[cand].dungeon_id,
                &templates[cand].dungeon_name,
            ) != arch1
            {
                step = (cand + templates.len() - idx1) % templates.len();
                break;
            }
        }
        (idx1 + step) % templates.len()
    } else {
        idx1
    };

    let base_inst = (sys_id as i64) * 1000;
    let t1 = &templates[idx1];
    let t2 = &templates[idx2];
    let bp1 = resolve_homefront_archetype_blueprint(
        &ctx.sde,
        t1.dungeon_id,
        &t1.dungeon_name,
        sys_id,
        t1.difficulty,
    );
    let bp2 = resolve_homefront_archetype_blueprint(
        &ctx.sde,
        t2.dungeon_id,
        &t2.dungeon_name,
        sys_id,
        t2.difficulty,
    );

    reg.homefront_sites.insert(
        sys_id,
        vec![
            HomefrontSiteRecord {
                instance_id: base_inst + 1,
                dungeon_id: t1.dungeon_id,
                dungeon_name: t1.dungeon_name.clone(),
                solar_system_id: sys_id,
                optimal_fleet_size: bp1.optimal_fleet_size,
                base_pilot_isk: bp1.base_pilot_isk,
                completed: false,
                reward_paid_isk: 0,
                boss_npc_ball_id: 1_897_700_000 + (base_inst + 1),
                boss_type_id: bp1.dispatch_commander_type_id,
            },
            HomefrontSiteRecord {
                instance_id: base_inst + 2,
                dungeon_id: t2.dungeon_id,
                dungeon_name: t2.dungeon_name.clone(),
                solar_system_id: sys_id,
                optimal_fleet_size: bp2.optimal_fleet_size,
                base_pilot_isk: bp2.base_pilot_isk,
                completed: false,
                reward_paid_isk: 0,
                boss_npc_ball_id: 1_897_700_000 + (base_inst + 2),
                boss_type_id: bp2.dispatch_commander_type_id,
            },
        ],
    );
}

pub const HOMEFRONT_SYSTEMS: &[SolarSystemId] =
    &[30_000_142_u32, 30_002_187, 30_002_659, 30_002_510];

pub fn init_homefront_sites(ctx: &ServerContext, reg: &mut PveSpecialSitesRegistry) {
    for &sys_id in HOMEFRONT_SYSTEMS {
        ensure_homefront_sites_for_system_locked(ctx, reg, sys_id);
    }
}

/// Spawns or activates a specific V24 SDE Homefront `dungeon_id` (`archetype_id == 70`) in `sys_id`
/// for live space operations, replacing or appending a fresh instance slot in `PveSpecialSitesRegistry`.
pub fn spawn_homefront_site_for_dungeon(
    ctx: &ServerContext,
    sys_id: SolarSystemId,
    dungeon_id: u32,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let sec = ctx
        .sde
        .solar_systems
        .get(&sys_id)
        .map(|s| s.security_status)
        .unwrap_or(0.0);
    if sec < 0.45 || sys_id >= 31_000_000 {
        return Err(hf_err(
            "InvalidHomefrontSecurity",
            format!("Homefront operations require High-Security empire space (sys {sys_id})"),
        ));
    }
    let dungeon = ctx.sde.dungeons.get(&dungeon_id).ok_or_else(|| {
        hf_err(
            "UnknownDungeon",
            format!("Unknown Homefront dungeon ID {dungeon_id}"),
        )
    })?;
    if dungeon.archetype_id != Some(70) {
        return Err(hf_err(
            "InvalidDungeonArchetype",
            format!("Dungeon {dungeon_id} is not an archetype 70 Homefront Operation"),
        ));
    }

    let name = ctx
        .sde
        .operations()
        .ok()
        .and_then(|m| m.get(&dungeon_id.to_string()).cloned())
        .and_then(|v| v.get("operationNameID").and_then(|id| id.as_u64()))
        .and_then(|msg_id| ctx.sde.localized_message_en_us(msg_id as u32))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| dungeon.name.clone());

    let bp = resolve_homefront_archetype_blueprint(
        &ctx.sde,
        dungeon_id,
        &name,
        sys_id,
        dungeon.difficulty.unwrap_or(2),
    );
    let (record, slot_idx) = with_pve_special_reg(ctx, |reg| {
        ensure_homefront_sites_for_system_locked(ctx, reg, sys_id);
        let list = reg.homefront_sites.entry(sys_id).or_default();
        let next_slot = list.len() + 1;
        let instance_id = (sys_id as i64) * 1000 + (next_slot as i64);
        let rec = HomefrontSiteRecord {
            instance_id,
            dungeon_id,
            dungeon_name: bp.dungeon_name.clone(),
            solar_system_id: sys_id,
            optimal_fleet_size: bp.optimal_fleet_size,
            base_pilot_isk: bp.base_pilot_isk,
            completed: false,
            reward_paid_isk: 0,
            boss_npc_ball_id: 1_897_700_000 + instance_id,
            boss_type_id: bp.dispatch_commander_type_id,
        };
        list.push(rec.clone());
        (rec, next_slot.saturating_sub(1))
    });
    Ok(ensure_runtime_state_for_site(ctx, &record, slot_idx))
}

async fn spawn_archetype_entities_into_space(
    ctx: &ServerContext,
    state: &mut HomefrontRuntimeState,
) {
    let sys_h = ctx.systems.get_or_spawn(state.solar_system_id);
    if !state.spawned_ball_ids.is_empty()
        && sys_h
            .get_ball_snapshot(state.dispatch_boss_ball_id)
            .await
            .ok()
            .flatten()
            .is_some()
    {
        return;
    }

    let pos = state.site_position;
    let bp = &state.blueprint;
    let mut spawned = Vec::new();

    // 1. Site beacon (77149)
    let _ = sys_h
        .spawn_ball(Ball {
            id: state.beacon_ball_id,
            type_id: 77_149,
            position: pos,
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1_000_000.0,
            radius: 150.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        })
        .await;
    spawned.push(state.beacon_ball_id);

    // 2. Dispatch commander / Raid hauler target with real SDE combat state
    let cmd_cs = ctx.build_type_combat_state(bp.dispatch_commander_type_id);
    let _ = sys_h
        .spawn_ball_with_combat(
            Ball {
                id: state.dispatch_boss_ball_id,
                type_id: bp.dispatch_commander_type_id,
                position: pos + DVec3::new(1_500.0, 0.0, 0.0),
                velocity: DVec3::ZERO,
                max_velocity: 220.0,
                inertia: 0.5,
                mass: 10_000_000.0,
                radius: 250.0,
                speed_fraction: 0.0,
                mode: MovementMode::Stop,
            },
            cmd_cs,
        )
        .await;
    spawned.push(state.dispatch_boss_ball_id);

    // 3. Archetype objective / support structures & warp-bubble hazards
    let base_obj = 1_897_800_000 + state.instance_id * 10;
    if let Some(obj_tid) = bp.objective_structure_type_id {
        let obj_id = base_obj + 2;
        let obj_cs = ctx.build_type_combat_state(obj_tid);
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: obj_id,
                    type_id: obj_tid,
                    position: pos + DVec3::new(0.0, 3_000.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 0.0,
                    inertia: 1.0,
                    mass: 50_000_000.0,
                    radius: 800.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                obj_cs,
            )
            .await;
        spawned.push(obj_id);
    }
    if let Some(sup_tid) = bp.support_entity_type_id {
        let sup_id = base_obj + 3;
        let sup_cs = ctx.build_type_combat_state(sup_tid);
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: sup_id,
                    type_id: sup_tid,
                    position: pos + DVec3::new(-3_500.0, 1_500.0, 0.0),
                    velocity: DVec3::ZERO,
                    max_velocity: 50.0,
                    inertia: 1.0,
                    mass: 40_000_000.0,
                    radius: 600.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                sup_cs,
            )
            .await;
        spawned.push(sup_id);
    }

    match &state.progress {
        HomefrontArchetypeProgress::SuspiciousSignal {
            array_ball_ids, ..
        } => {
            let array_tid = bp.support_entity_type_id.unwrap_or(77_121);
            for (idx, &arr_id) in array_ball_ids.iter().enumerate() {
                let offset = DVec3::new((idx as f64 - 1.0) * 12_000.0, -5_000.0, 0.0);
                let _ = sys_h
                    .spawn_ball_with_combat(
                        Ball {
                            id: arr_id,
                            type_id: array_tid,
                            position: pos + offset,
                            velocity: DVec3::ZERO,
                            max_velocity: 0.0,
                            inertia: 1.0,
                            mass: 5_000_000.0,
                            radius: 300.0,
                            speed_fraction: 0.0,
                            mode: MovementMode::Stop,
                        },
                        ctx.build_type_combat_state(array_tid),
                    )
                    .await;
                spawned.push(arr_id);
            }
        }
        HomefrontArchetypeProgress::AbyssalArtifactRecovery { .. } => {
            let _ = sys_h
                .add_warp_bubble(base_obj + 9, pos, 150_000.0)
                .await;
        }
        _ => {}
    }

    // 4. Escort wing NPCs (Logi, EWAR, Drone aggro, DPS)
    for (idx, &esc_tid) in bp.escort_wave_type_ids.iter().enumerate() {
        let esc_id = base_obj + 20 + (idx as i64);
        let offset = DVec3::new(3_000.0 + (idx as f64) * 1_500.0, -2_000.0, 0.0);
        let _ = sys_h
            .spawn_ball_with_combat(
                Ball {
                    id: esc_id,
                    type_id: esc_tid,
                    position: pos + offset,
                    velocity: DVec3::ZERO,
                    max_velocity: 260.0,
                    inertia: 0.5,
                    mass: 2_000_000.0,
                    radius: 120.0,
                    speed_fraction: 0.0,
                    mode: MovementMode::Stop,
                },
                ctx.build_type_combat_state(esc_tid),
            )
            .await;
        spawned.push(esc_id);
    }

    state.spawned_ball_ids = spawned;
}

/// Validates acceleration-gate ship restrictions and warps/admits an in-space pilot into the
/// Homefront operation pocket, spawning its SDE multi-role entities and starting the clock.
pub async fn activate_homefront_acceleration_gate(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
) -> Result<HomefrontShipEligibility, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(hf_err(
            "ShipDocked",
            "Cannot activate Homefront acceleration gate while docked",
        ));
    }
    let caller_sys = session.solar_system_id.ok_or_else(|| {
        hf_err("NotInSpace", "Pilot must be in space to activate acceleration gate")
    })?;

    let (site, slot_idx) = find_site_record_with_slot(ctx, instance_id)?;
    if site.solar_system_id != caller_sys {
        return Err(hf_err(
            "NotInSiteSystem",
            format!(
                "Pilot is in system {} but Homefront site {} is in {}",
                caller_sys, instance_id, site.solar_system_id
            ),
        ));
    }
    if site.completed {
        return Err(hf_err(
            "SiteAlreadyCompleted",
            format!("Homefront site {instance_id} is already completed"),
        ));
    }

    let ship_type_id = ctx
        .db
        .with_snapshot(|snap| {
            let ch = snap.characters.get(&char_id)?;
            let sid = session.ship_id.unwrap_or(ch.active_ship_id);
            snap.items.get(&sid).map(|it| it.type_id)
        })
        .ok_or_else(|| {
            hf_err(
                "NoActiveShip",
                "Pilot must board a valid ship before entering a Homefront gate",
            )
        })?;

    let mut state = ensure_runtime_state_for_site(ctx, &site, slot_idx);
    if state.failed {
        return Err(hf_err(
            "OperationFailed",
            state
                .failure_reason
                .clone()
                .unwrap_or_else(|| "Homefront operation failed".to_owned()),
        ));
    }

    let eligibility =
        validate_homefront_ship_eligibility(&ctx.sde, ship_type_id, state.blueprint.archetype)?;

    let now_sec = ctx.now_unix();
    if !state.started {
        state.started = true;
        state.started_at_sec = now_sec;
        state.expires_at_sec = now_sec.saturating_add(state.blueprint.time_limit_secs);
    } else if now_sec >= state.expires_at_sec {
        state.failed = true;
        state.failure_reason = Some("Operation time limit expired".to_owned());
        with_homefront_runtime(ctx, |map| {
            map.insert(instance_id, state.clone());
        });
        return Err(hf_err(
            "OperationTimedOut",
            "Homefront operation time limit has expired",
        ));
    }

    spawn_archetype_entities_into_space(ctx, &mut state).await;
    state.entered_pilots.insert(char_id, eligibility.clone());
    state
        .contributions
        .entry(char_id)
        .or_insert_with(|| HomefrontParticipantContribution {
            character_id: char_id,
            ..Default::default()
        });

    let sys_h = ctx.systems.get_or_spawn(state.solar_system_id);
    if let Some(ship_ball_id) = ctx.db.with_snapshot(|snap| {
        snap.characters
            .get(&char_id)
            .map(|c| session.ship_id.unwrap_or(c.active_ship_id))
    }) {
        if sys_h.get_ball_snapshot(ship_ball_id).await.ok().flatten().is_some() {
            let _ = sys_h.set_ball_position(ship_ball_id, state.site_position).await;
        }
    }

    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state);
    });
    Ok(eligibility)
}

fn verify_pilot_in_active_site(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
) -> Result<(i64, HomefrontRuntimeState), BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    if session.station_id.is_some() || session.structure_id.is_some() {
        return Err(hf_err(
            "ShipDocked",
            "Cannot interact with Homefront operation while docked",
        ));
    }
    let caller_sys = session
        .solar_system_id
        .ok_or_else(|| hf_err("NotInSpace", "Pilot must be in space"))?;

    let now_sec = ctx.now_unix();
    let state = with_homefront_runtime(ctx, |map| map.get(&instance_id).cloned()).ok_or_else(|| {
        hf_err(
            "GateNotEntered",
            format!("Pilot must activate the acceleration gate for Homefront {instance_id}"),
        )
    })?;

    if state.solar_system_id != caller_sys {
        return Err(hf_err(
            "NotInSiteSystem",
            format!("Must remain in system {} for Homefront {instance_id}", state.solar_system_id),
        ));
    }
    if state.completed {
        return Err(hf_err(
            "SiteAlreadyCompleted",
            format!("Homefront site {instance_id} is already completed"),
        ));
    }
    if state.failed {
        return Err(hf_err(
            "OperationFailed",
            state
                .failure_reason
                .unwrap_or_else(|| "Homefront operation failed".to_owned()),
        ));
    }
    if state.started && now_sec >= state.expires_at_sec {
        return Err(hf_err(
            "OperationTimedOut",
            "Homefront operation time limit exceeded",
        ));
    }
    if !state.entered_pilots.contains_key(&char_id) {
        return Err(hf_err(
            "GateNotEntered",
            "Pilot did not pass Homefront acceleration-gate ship restrictions",
        ));
    }
    Ok((char_id, state))
}

fn active_ship_type_for_pilot(ctx: &ServerContext, session: &SessionState, char_id: i64) -> Option<TypeId> {
    ctx.db.with_snapshot(|snap| {
        let ch = snap.characters.get(&char_id)?;
        let sid = session.ship_id.unwrap_or(ch.active_ship_id);
        snap.items.get(&sid).map(|i| i.type_id)
    })
}

/// Returns a snapshot of live Homefront multi-role runtime state for inspection/verification.
pub fn get_homefront_runtime_state(
    ctx: &ServerContext,
    instance_id: i64,
) -> Option<HomefrontRuntimeState> {
    with_homefront_runtime(ctx, |map| map.get(&instance_id).cloned())
}

/// Records fleet DPS and Logistics contribution for any Homefront archetype.
pub fn record_homefront_combat_and_logistics(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    damage_dealt: f64,
    repair_hp: f64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let (char_id, mut state) = verify_pilot_in_active_site(ctx, session, instance_id)?;
    let entry = state
        .contributions
        .entry(char_id)
        .or_insert_with(|| HomefrontParticipantContribution {
            character_id: char_id,
            ..Default::default()
        });
    entry.damage_dealt += damage_dealt.max(0.0);
    entry.repair_hp += repair_hp.max(0.0);
    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state.clone());
    });
    Ok(state)
}

/// **Raid role action 1:** Tackles and destroys an escaping Raid pirate hauler, dropping 3,500 m3 cargo.
pub fn raid_tackle_and_destroy_hauler(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    applied_tackle: bool,
    damage_dealt: f64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let (char_id, mut state) = verify_pilot_in_active_site(ctx, session, instance_id)?;
    match &mut state.progress {
        HomefrontArchetypeProgress::Raid {
            haulers_spawned,
            haulers_destroyed,
            haulers_escaped,
            active_hauler_tackled,
            cargo_dropped,
            ..
        } => {
            if !applied_tackle {
                *haulers_escaped += 1;
                *haulers_spawned += 1;
                return Err(hf_err(
                    "TargetEscaped",
                    "Enemy hauler warped off because warp disruption/scram was not applied",
                ));
            }
            *active_hauler_tackled = true;
            if damage_dealt < 1_000.0 {
                return Err(hf_err(
                    "InsufficientDamage",
                    "Tackled hauler still operational; finish destroying the hauler before recovery",
                ));
            }
            *haulers_destroyed += 1;
            *cargo_dropped += 1;
            *haulers_spawned += 1;
        }
        _ => {
            return Err(hf_err(
                "InvalidArchetypeAction",
                "Hauler intercept applies only to Raid operations",
            ));
        }
    }
    let entry = state
        .contributions
        .entry(char_id)
        .or_insert_with(|| HomefrontParticipantContribution {
            character_id: char_id,
            ..Default::default()
        });
    entry.damage_dealt += damage_dealt.max(0.0);
    entry.objective_actions += 1;
    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state.clone());
    });
    Ok(state)
}

/// **Raid role action 2:** Hauls a 3,500 m3 contraband crate to the friendly freighter (requires Hauler hold >= 3,500 m3).
pub fn raid_deliver_cargo_to_freighter(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let (char_id, mut state) = verify_pilot_in_active_site(ctx, session, instance_id)?;
    let ship_tid = active_ship_type_for_pilot(ctx, session, char_id).ok_or_else(|| {
        hf_err("NoActiveShip", "Pilot must be piloting a ship to deliver Raid cargo")
    })?;
    let cap_m3 = ctx
        .sde
        .types
        .get(&ship_tid)
        .map(|t| t.capacity)
        .unwrap_or(0.0);
    // DERIVED: Raid cargo items are 3,500 m3 each (`wiki:Raid.md`).
    if cap_m3 < 3_500.0 {
        return Err(hf_err(
            "CargoCapacityTooSmall",
            format!("Ship type {ship_tid} cargo hold ({cap_m3:.0} m3) cannot carry 3,500 m3 Raid crate"),
        ));
    }
    match &mut state.progress {
        HomefrontArchetypeProgress::Raid {
            cargo_dropped,
            cargo_delivered,
            required_deliveries,
            ..
        } => {
            if *cargo_delivered >= *cargo_dropped {
                return Err(hf_err(
                    "NoRaidCargoAvailable",
                    "No dropped contraband crates left to deliver; intercept another enemy hauler",
                ));
            }
            if *cargo_delivered < *required_deliveries {
                *cargo_delivered += 1;
            }
        }
        _ => {
            return Err(hf_err(
                "InvalidArchetypeAction",
                "Cargo delivery applies only to Raid operations",
            ));
        }
    }
    let entry = state
        .contributions
        .entry(char_id)
        .or_insert_with(|| HomefrontParticipantContribution {
            character_id: char_id,
            ..Default::default()
        });
    entry.cargo_delivered += 1;
    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state.clone());
    });
    Ok(state)
}

/// **Emergency Aid role action:** Applies fleet shield/armor logistics to keep the friendly asset alive for 10 minutes.
pub fn emergency_aid_advance_defense(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    repair_hp: f64,
    incoming_hostile_damage: f64,
    elapsed_secs: u64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let (char_id, mut state) = verify_pilot_in_active_site(ctx, session, instance_id)?;
    match &mut state.progress {
        HomefrontArchetypeProgress::EmergencyAid {
            protectee_hp,
            protectee_max_hp,
            survival_elapsed_secs,
            required_survival_secs,
            waves_repelled,
            ..
        } => {
            *protectee_hp = (*protectee_hp - incoming_hostile_damage.max(0.0) + repair_hp.max(0.0))
                .min(*protectee_max_hp);
            if *protectee_hp <= 0.0 {
                *protectee_hp = 0.0;
                state.failed = true;
                state.failure_reason = Some("Friendly asset destroyed under hostile fire".to_owned());
                with_homefront_runtime(ctx, |map| {
                    map.insert(instance_id, state.clone());
                });
                return Err(hf_err(
                    "OperationFailed",
                    "Emergency Aid failed: protected friendly vessel was destroyed",
                ));
            }
            *survival_elapsed_secs = (*survival_elapsed_secs + elapsed_secs).min(*required_survival_secs);
            if elapsed_secs > 0 {
                *waves_repelled += 1;
            }
        }
        _ => {
            return Err(hf_err(
                "InvalidArchetypeAction",
                "Emergency Aid defense applies only to Emergency Aid operations",
            ));
        }
    }
    let entry = state
        .contributions
        .entry(char_id)
        .or_insert_with(|| HomefrontParticipantContribution {
            character_id: char_id,
            ..Default::default()
        });
    entry.repair_hp += repair_hp.max(0.0);
    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state.clone());
    });
    Ok(state)
}

fn validate_pilot_mining_capability(
    ctx: &ServerContext,
    session: &SessionState,
    char_id: i64,
    ship_tid: TypeId,
) -> Result<(), BridgeRpcError> {
    if matches!(ship_tid, 32_880 | 89_648 | 89_240 | 89_647 | 91_174) {
        return Ok(());
    }
    let has_miner = ctx.db.with_snapshot(|snap| {
        let Some(ch) = snap.characters.get(&char_id) else {
            return false;
        };
        let ship_id = session.ship_id.unwrap_or(ch.active_ship_id);
        snap.items.values().any(|item| {
            item.location_id == ship_id
                && item.quantity > 0
                && ctx
                    .sde
                    .types
                    .get(&item.type_id)
                    .is_some_and(|t| t.group_id == 54)
        })
    });
    if has_miner {
        Ok(())
    } else {
        Err(hf_err(
            "MiningModuleRequired",
            format!("Ship type {ship_tid} lacks a fitted Mining Laser or mining hull bonus to mine Mutanite"),
        ))
    }
}

/// **Metaliminal Meteoroid role action:** Mines Mutanite ore (`5,000` units total, with optional T2 residue).
pub fn metaliminal_mine_mutanite(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    yield_units: u32,
    residue_units: u32,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let (char_id, mut state) = verify_pilot_in_active_site(ctx, session, instance_id)?;
    let ship_tid = active_ship_type_for_pilot(ctx, session, char_id).ok_or_else(|| {
        hf_err("NoActiveShip", "Pilot must be piloting a ship to mine Mutanite")
    })?;
    validate_pilot_mining_capability(ctx, session, char_id, ship_tid)?;
    if yield_units == 0 {
        return Err(hf_err("ZeroMiningYield", "Mining laser cycle must extract > 0 Mutanite units"));
    }
    let extracted_by_pilot;
    match &mut state.progress {
        HomefrontArchetypeProgress::MetaliminalMeteoroid {
            remaining_units,
            total_residue_units,
            ore_mined_by_fleet,
            ..
        } => {
            if *remaining_units == 0 {
                return Err(hf_err(
                    "AsteroidDepleted",
                    "Metaliminal Mutanite meteoroid is already depleted",
                ));
            }
            let mined = yield_units.min(*remaining_units);
            *remaining_units -= mined;
            let residue = residue_units.min(*remaining_units);
            *remaining_units -= residue;
            *total_residue_units += residue;
            *ore_mined_by_fleet += mined;
            extracted_by_pilot = mined;
        }
        _ => {
            return Err(hf_err(
                "InvalidArchetypeAction",
                "Mutanite mining applies only to Metaliminal Meteoroid operations",
            ));
        }
    }
    let entry = state
        .contributions
        .entry(char_id)
        .or_insert_with(|| HomefrontParticipantContribution {
            character_id: char_id,
            ..Default::default()
        });
    entry.ore_mined_units += extracted_by_pilot;
    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state.clone());
    });
    Ok(state)
}

/// **Dread Assault role action 1:** Fleet Logistics ships transfer capacitor into the allied Dreadnought
/// until reaching 30,000 GJ, triggering a 90s Siege superweapon cycle (hostile neuts subtract from cap).
pub fn dread_assault_transfer_capacitor(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    cap_transferred_gj: f64,
    hostile_neut_drain_gj: f64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let (char_id, mut state) = verify_pilot_in_active_site(ctx, session, instance_id)?;
    match &mut state.progress {
        HomefrontArchetypeProgress::DreadAssault {
            capacitor_gj,
            capacitor_target_gj,
            in_siege_mode,
            siege_remaining_secs,
            siege_cycles_completed,
            target_siege_cycles,
            ..
        } => {
            if *siege_cycles_completed >= *target_siege_cycles {
                return Err(hf_err(
                    "StructureAlreadyDestroyed",
                    "Enemy pirate structure is already destroyed",
                ));
            }
            if *in_siege_mode {
                return Err(hf_err(
                    "DreadInSiegeMode",
                    "Allied Dreadnought is in Siege Mode and cannot accept remote capacitor transfers",
                ));
            }
            *capacitor_gj = (*capacitor_gj - hostile_neut_drain_gj.max(0.0) + cap_transferred_gj.max(0.0))
                .clamp(0.0, *capacitor_target_gj);
            if *capacitor_gj >= *capacitor_target_gj {
                *in_siege_mode = true;
                *siege_remaining_secs = 90;
                *capacitor_gj = 0.0;
            }
        }
        _ => {
            return Err(hf_err(
                "InvalidArchetypeAction",
                "Capacitor transfer to allied Dreadnought applies only to Dread Assault operations",
            ));
        }
    }
    let entry = state
        .contributions
        .entry(char_id)
        .or_insert_with(|| HomefrontParticipantContribution {
            character_id: char_id,
            ..Default::default()
        });
    entry.cap_transferred += cap_transferred_gj.max(0.0);
    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state.clone());
    });
    Ok(state)
}

/// **Dread Assault role action 2:** Advances the 90s Dreadnought Siege timer and resolves its
/// superweapon volley against the enemy Homefront HQ structure.
pub fn dread_assault_advance_siege(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    elapsed_secs: u64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let (_char_id, mut state) = verify_pilot_in_active_site(ctx, session, instance_id)?;
    match &mut state.progress {
        HomefrontArchetypeProgress::DreadAssault {
            in_siege_mode,
            siege_remaining_secs,
            siege_cycles_completed,
            target_siege_cycles,
            enemy_base_hp,
            enemy_base_max_hp,
            ..
        } => {
            if !*in_siege_mode {
                return Err(hf_err(
                    "DreadNotInSiegeMode",
                    "Allied Dreadnought must reach 30,000 GJ capacitor to enter Siege Mode",
                ));
            }
            if elapsed_secs < *siege_remaining_secs {
                *siege_remaining_secs -= elapsed_secs;
            } else {
                *siege_remaining_secs = 0;
                *in_siege_mode = false;
                *siege_cycles_completed += 1;
                let volley = *enemy_base_max_hp / (*target_siege_cycles as f64);
                *enemy_base_hp = (*enemy_base_hp - volley).max(0.0);
                if *siege_cycles_completed >= *target_siege_cycles {
                    *enemy_base_hp = 0.0;
                }
            }
        }
        _ => {
            return Err(hf_err(
                "InvalidArchetypeAction",
                "Siege advancement applies only to Dread Assault operations",
            ));
        }
    }
    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state.clone());
    });
    Ok(state)
}

/// **Abyssal Artifact Recovery role action:** Mines wave Admixti Mutanite (to clear the warp-disruption
/// field), defeats wave Triglavian hostiles, optionally salvages Faulty Triglavian Cache, and advances waves 1..=9.
pub async fn abyssal_clear_wave(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    mined_units: u32,
    hostile_damage: f64,
    recover_cache: bool,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let (char_id, mut state) = verify_pilot_in_active_site(ctx, session, instance_id)?;
    if mined_units > 0 {
        let ship_tid = active_ship_type_for_pilot(ctx, session, char_id).ok_or_else(|| {
            hf_err("NoActiveShip", "Pilot must be piloting a ship to mine Admixti Mutanite")
        })?;
        validate_pilot_mining_capability(ctx, session, char_id, ship_tid)?;
    }
    let mut disperse_bubble = false;
    let mut respawn_bubble = false;
    match &mut state.progress {
        HomefrontArchetypeProgress::AbyssalArtifactRecovery {
            current_wave,
            total_waves,
            mutanite_units_remaining,
            wave_hostiles_remaining,
            warp_bubble_active,
            caches_recovered,
            waves_paid,
            ..
        } => {
            *mutanite_units_remaining = mutanite_units_remaining.saturating_sub(mined_units);
            if *mutanite_units_remaining == 0 && *warp_bubble_active {
                *warp_bubble_active = false;
                disperse_bubble = true;
            }
            if hostile_damage >= 1_000.0 {
                *wave_hostiles_remaining = 0;
            }
            if recover_cache {
                *caches_recovered += 1;
            }
            if *mutanite_units_remaining == 0 && *wave_hostiles_remaining == 0 {
                *waves_paid = (*waves_paid).max(*current_wave);
                if *current_wave < *total_waves {
                    *current_wave += 1;
                    *mutanite_units_remaining = 30;
                    *wave_hostiles_remaining = 3 + *current_wave / 2;
                    *warp_bubble_active = true;
                    respawn_bubble = true;
                }
            }
        }
        _ => {
            return Err(hf_err(
                "InvalidArchetypeAction",
                "Abyssal wave progression applies only to Abyssal Artifact Recovery",
            ));
        }
    }

    let sys_h = ctx.systems.get_or_spawn(state.solar_system_id);
    let base_obj = 1_897_800_000 + state.instance_id * 10;
    if disperse_bubble {
        let _ = sys_h.remove_warp_bubble(base_obj + 9).await;
    }
    if respawn_bubble {
        let _ = sys_h
            .add_warp_bubble(base_obj + 9, state.site_position, 150_000.0)
            .await;
    }

    let entry = state
        .contributions
        .entry(char_id)
        .or_insert_with(|| HomefrontParticipantContribution {
            character_id: char_id,
            ..Default::default()
        });
    entry.ore_mined_units += mined_units;
    entry.damage_dealt += hostile_damage.max(0.0);
    if recover_cache {
        entry.objective_actions += 1;
    }
    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state.clone());
    });
    Ok(state)
}

/// **Suspicious Signal role action 1:** Hacks one of the 3 Remote Arrays (`0..3`). All 3 must be
/// hacked before the main relay structure drops its invulnerability shield.
pub fn suspicious_signal_hack_array(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    array_index: usize,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    if array_index >= 3 {
        return Err(hf_err(
            "InvalidArrayIndex",
            format!("Suspicious Signal array index {array_index} out of range 0..3"),
        ));
    }
    let (char_id, mut state) = verify_pilot_in_active_site(ctx, session, instance_id)?;
    match &mut state.progress {
        HomefrontArchetypeProgress::SuspiciousSignal {
            arrays_hacked,
            vulnerability_window_secs,
            ..
        } => {
            arrays_hacked[array_index] = true;
            if arrays_hacked.iter().all(|&h| h) {
                // DERIVED: synchronized 3-array hack opens a timed damage window.
                *vulnerability_window_secs = 120;
            }
        }
        _ => {
            return Err(hf_err(
                "InvalidArchetypeAction",
                "Remote Array hacking applies only to Suspicious Signal operations",
            ));
        }
    }
    let entry = state
        .contributions
        .entry(char_id)
        .or_insert_with(|| HomefrontParticipantContribution {
            character_id: char_id,
            ..Default::default()
        });
    entry.arrays_hacked += 1;
    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state.clone());
    });
    Ok(state)
}

/// **Suspicious Signal role action 2:** Applies fleet DPS to the relay tower ONLY while all 3 arrays
/// are hacked and the vulnerability window remains open (`StructureInvulnerable` otherwise).
pub fn suspicious_signal_attack_relay(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    damage: f64,
    elapsed_secs: u64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let (char_id, mut state) = verify_pilot_in_active_site(ctx, session, instance_id)?;
    match &mut state.progress {
        HomefrontArchetypeProgress::SuspiciousSignal {
            arrays_hacked,
            vulnerability_window_secs,
            relay_hp,
            ..
        } => {
            if !arrays_hacked.iter().all(|&h| h) || *vulnerability_window_secs == 0 {
                return Err(hf_err(
                    "StructureInvulnerable",
                    "Relay tower is invulnerable until all 3 Remote Arrays are hacked simultaneously",
                ));
            }
            *relay_hp = (*relay_hp - damage.max(0.0)).max(0.0);
            if elapsed_secs >= *vulnerability_window_secs {
                *vulnerability_window_secs = 0;
                if *relay_hp > 0.0 {
                    *arrays_hacked = [false, false, false];
                }
            } else {
                *vulnerability_window_secs -= elapsed_secs;
            }
        }
        _ => {
            return Err(hf_err(
                "InvalidArchetypeAction",
                "Relay structure assault applies only to Suspicious Signal operations",
            ));
        }
    }
    let entry = state
        .contributions
        .entry(char_id)
        .or_insert_with(|| HomefrontParticipantContribution {
            character_id: char_id,
            ..Default::default()
        });
    entry.damage_dealt += damage.max(0.0);
    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state.clone());
    });
    Ok(state)
}

fn pilot_contrib_mut(
    contributions: &mut BTreeMap<i64, HomefrontParticipantContribution>,
    char_id: i64,
) -> &mut HomefrontParticipantContribution {
    contributions
        .entry(char_id)
        .or_insert_with(|| HomefrontParticipantContribution {
            character_id: char_id,
            ..Default::default()
        })
}

/// **Salvage Research (3-pilot) role action:** Salvages shattered wrecks (`30` total) and optionally
/// activates the malfunctioning Werpost via Data Analyzer for automated turret support.
pub fn salvage_research_process_wreck(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    wrecks_count: u8,
    activate_werpost: bool,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let (char_id, mut state) = verify_pilot_in_active_site(ctx, session, instance_id)?;
    let delta;
    match &mut state.progress {
        HomefrontArchetypeProgress::SalvageResearch {
            wrecks_salvaged,
            total_wrecks,
            werpost_activated,
        } => {
            if activate_werpost {
                *werpost_activated = true;
            }
            let remaining = total_wrecks.saturating_sub(*wrecks_salvaged);
            delta = wrecks_count.min(remaining);
            *wrecks_salvaged = wrecks_salvaged.saturating_add(delta);
        }
        _ => {
            return Err(hf_err(
                "InvalidArchetypeAction",
                "Wreck salvage applies only to Salvage Research operations",
            ));
        }
    }
    let entry = pilot_contrib_mut(&mut state.contributions, char_id);
    entry.objective_actions += u32::from(delta) + u32::from(activate_werpost);
    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state.clone());
    });
    Ok(state)
}

/// **Stabilize Rift (3-pilot) role action:** Neutralizes Sleeper Destabilizing Arrays and holds all
/// 5 below 20% capacitor for 60 seconds to stabilize the abyssal rift.
pub fn stabilize_rift_neutralize_arrays(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    array_cap_drain: [f64; 5],
    elapsed_secs: u64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let (char_id, mut state) = verify_pilot_in_active_site(ctx, session, instance_id)?;
    let mut drained_any = false;
    match &mut state.progress {
        HomefrontArchetypeProgress::StabilizeRift {
            arrays_cap_pct,
            held_low_secs,
            required_hold_secs,
        } => {
            for (slot, drain) in arrays_cap_pct.iter_mut().zip(array_cap_drain.iter()) {
                if *drain > 0.0 {
                    *slot = (*slot - *drain).clamp(0.0, 1.0);
                    drained_any = true;
                }
            }
            if arrays_cap_pct.iter().all(|&pct| pct < 0.20) {
                *held_low_secs = (*held_low_secs + elapsed_secs).min(*required_hold_secs);
            }
        }
        _ => {
            return Err(hf_err(
                "InvalidArchetypeAction",
                "Array neutralization applies only to Stabilize Rift operations",
            ));
        }
    }
    let entry = pilot_contrib_mut(&mut state.contributions, char_id);
    if drained_any {
        entry.objective_actions += 1;
    }
    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state.clone());
    });
    Ok(state)
}

/// **Traffic Stop (3-pilot) role action:** Scans incoming haulers and destroys contraband smugglers
/// without harming > 2 civilian haulers.
pub fn traffic_stop_scan_and_intercept(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
    smugglers_caught: u8,
    civilians_attacked: u8,
    damage_dealt: f64,
) -> Result<HomefrontRuntimeState, BridgeRpcError> {
    let (char_id, mut state) = verify_pilot_in_active_site(ctx, session, instance_id)?;
    match &mut state.progress {
        HomefrontArchetypeProgress::TrafficStop {
            smugglers_destroyed,
            required_smugglers,
            civilians_harmed,
            ..
        } => {
            *civilians_harmed = civilians_harmed.saturating_add(civilians_attacked);
            if *civilians_harmed > 2 {
                state.failed = true;
                state.failure_reason = Some("Attacked too many civilian haulers".to_owned());
                with_homefront_runtime(ctx, |map| {
                    map.insert(instance_id, state.clone());
                });
                return Err(hf_err(
                    "OperationFailed",
                    "Traffic Stop failed: civilian collateral limit exceeded",
                ));
            }
            *smugglers_destroyed = (*smugglers_destroyed + smugglers_caught).min(*required_smugglers);
        }
        _ => {
            return Err(hf_err(
                "InvalidArchetypeAction",
                "Contraband interception applies only to Traffic Stop operations",
            ));
        }
    }
    let entry = pilot_contrib_mut(&mut state.contributions, char_id);
    entry.damage_dealt += damage_dealt.max(0.0);
    entry.objective_actions += u32::from(smugglers_caught);
    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state.clone());
    });
    Ok(state)
}

/// Verifies all multi-role operation objectives, enforces zero fake/shortcut completion, despawns
/// site entities, and pays verified contributing pilots under the canonical 5-pilot (or 3-pilot)
/// role cap (`<= opt` full, `opt+1..=2*opt` diluted capped pool, `> 2*opt` zero ISK/LP/standing).
pub async fn complete_homefront_operation_and_pay(
    ctx: &ServerContext,
    session: &SessionState,
    instance_id: i64,
) -> Result<HomefrontFleetPayoutSummary, BridgeRpcError> {
    let (_char_id, mut state) = verify_pilot_in_active_site(ctx, session, instance_id)?;
    if !state.is_objective_complete() {
        return Err(hf_err(
            "ObjectivesIncomplete",
            format!(
                "Cannot complete Homefront {} ({instance_id}): role objectives not yet satisfied",
                state.blueprint.archetype.as_str()
            ),
        ));
    }

    let contributors: Vec<HomefrontParticipantContribution> =
        state.contributions.values().cloned().collect();
    let summary = compute_homefront_fleet_payouts(
        state.blueprint.base_pilot_isk,
        state.blueprint.base_pilot_lp,
        state.blueprint.optimal_fleet_size,
        &contributors,
    );

    if summary.eligible_pilot_count == 0 {
        return Err(hf_err(
            "NoEligibleContributors",
            "No pilot met the minimum contribution threshold for Homefront payout",
        ));
    }

    state.completed = true;
    let rep_per_pilot = summary
        .payouts
        .iter()
        .find(|p| p.eligible)
        .map(|p| p.isk)
        .unwrap_or(0);

    with_homefront_runtime(ctx, |map| {
        map.insert(instance_id, state.clone());
    });

    with_pve_special_reg(ctx, |reg| {
        if let Some(list) = reg.homefront_sites.get_mut(&state.solar_system_id) {
            if let Some(rec) = list.iter_mut().find(|s| s.instance_id == instance_id) {
                rec.completed = true;
                rec.reward_paid_isk = rep_per_pilot;
            }
        }
    });

    let sys_h = ctx.systems.get_or_spawn(state.solar_system_id);
    for bid in &state.spawned_ball_ids {
        sys_h.try_remove_ball(*bid);
    }
    let _ = sys_h.remove_warp_bubble(1_897_800_000 + state.instance_id * 10 + 9).await;

    let dname = state.blueprint.dungeon_name.clone();
    ctx.db.with_mut_snapshot(|snap| {
        for payout in &summary.payouts {
            if !payout.eligible {
                continue;
            }
            if let Some(ch) = snap.characters.get_mut(&payout.character_id) {
                if payout.isk > 0 {
                    ch.balance = ch
                        .balance
                        .checked_add(IskCents::from_isk(payout.isk))
                        .unwrap_or(ch.balance);
                    let new_bal = ch.balance;
                    snap.append_journal(
                        payout.character_id,
                        "CorporateRewardPayout",
                        IskCents::from_isk(payout.isk),
                        new_bal,
                        format!("Homefront Operation Payout ({dname})"),
                    );
                }
            }
        }
    });

    Ok(summary)
}

pub fn handle_get_homefront_site_instances(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    ServerContext::require_character(session)?;
    crate::services::pve_special_sites::ensure_special_sites_initialized(ctx);

    let sys_id = if let Some(arg0) = args.first().filter(|a| !matches!(a, PyRep::None)) {
        let raw = arg0.as_i64().ok_or_else(|| BridgeRpcError {
            code: "UnknownSolarSystem".into(),
            message: "GetHomefrontSiteInstances requires an integer solarSystemID".into(),
            client_error: None,
        })?;
        let sid = u32::try_from(raw).ok().filter(|id| ctx.sde.solar_systems.contains_key(id)).ok_or_else(|| BridgeRpcError {
            code: "UnknownSolarSystem".into(),
            message: format!("Unknown solar system ID {raw}"),
            client_error: None,
        })?;
        sid
    } else {
        session
            .solar_system_id
            .or_else(|| {
                session.character_id.and_then(|cid| {
                    ctx.db
                        .with_snapshot(|s| s.characters.get(&cid).map(|c| c.solar_system_id))
                })
            })
            .ok_or_else(|| BridgeRpcError {
                code: "NotInSpace".into(),
                message: "Action requires an active solar system session".into(),
                client_error: None,
            })?
    };

    let reply = with_pve_special_reg(ctx, |reg| {
        let mut entries: Vec<(u32, i64)> = Vec::new();
        if let Some(list) = reg.homefront_sites.get(&sys_id) {
            for s in list {
                if !s.completed {
                    entries.push((s.dungeon_id, s.instance_id));
                }
            }
        }
        entries.sort_unstable_by_key(|&(_, instance_id)| instance_id);
        let items: Vec<PyRep> = entries
            .into_iter()
            .map(|(dungeon_id, instance_id)| {
                PyRep::keyval([
                    ("dungeonID", PyRep::Integer(dungeon_id as i64)),
                    ("instanceID", PyRep::Integer(instance_id)),
                    ("solarSystemID", PyRep::Integer(sys_id as i64)),
                ])
            })
            .collect();
        PyRep::Dict(vec![(PyRep::Integer(sys_id as i64), PyRep::List(items))])
    });

    Ok(reply)
}

#[cfg(test)]
#[path = "pve_homefront_sites_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "pve_homefront_sites_edges_tests.rs"]
mod edges_tests;
