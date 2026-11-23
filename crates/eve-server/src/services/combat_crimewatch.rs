//! `combat_crimewatch` — Domain 6: Weapons, EWAR, Drones/Fighters, NPC AI, CrimeWatch 2.0, CONCORD, Killmails, & DBS/ESS (`§3.6`).
//!
// parity: feat:dogma.signature-radius, feat:dogma.shield-recharge, feat:dogma.resistances, feat:dogma.weapon-timer
// parity: feat:modules.turret-hybrid, feat:modules.turret-projectile, feat:modules.turret-laser, feat:modules.turret-hit-chance
// parity: feat:modules.turret-precursor, feat:modules.vorton-projector, feat:modules.missile-launchers, feat:modules.missile-application
// parity: feat:modules.missile-defender, feat:modules.missile-fof, feat:modules.smartbomb, feat:modules.bomb-launcher
// parity: feat:modules.capital-weapons, feat:modules.breacher-pods, feat:combat.missile-flight-physics-destruction
// parity: feat:combat.bomb-damage-void-lockbreaker, feat:combat.damage-pipeline, feat:combat.damage-log
// parity: feat:combat.ship-destruction, feat:combat.pod-destruction, feat:combat.wreck-loot, feat:combat.loot-all
// parity: feat:combat.wreck-lifetime, feat:combat.killmail-generate, feat:combat.killmail-view, feat:combat.killmail-esi-hash
// parity: feat:combat.killmail-assist-timeout-ewar, feat:combat.capsule-ejection-items, feat:combat.self-destruct-pod
// parity: feat:combat.aggression-tracking, feat:crimewatch.security-status, feat:crimewatch.sec-status-loss
// parity: feat:crimewatch.sec-status-gain, feat:crimewatch.sec-status-tags, feat:crimewatch.sec-status-transactions
// parity: feat:crimewatch.suspect-flag, feat:crimewatch.criminal-flag, feat:crimewatch.concord-response
// parity: feat:crimewatch.faction-police, feat:crimewatch.safety-settings, feat:crimewatch.limited-engagements
// parity: feat:crimewatch.logi-timers, feat:crimewatch.pod-timer, feat:crimewatch.npc-timers, feat:crimewatch.legal-timers
// parity: feat:crimewatch.kill-rights-gain, feat:crimewatch.kill-rights-sell, feat:crimewatch.duels
// parity: feat:crimewatch.crimewatch-states, feat:crimewatch.disapproval-flag, feat:crimewatch.suspects-list
// parity: feat:crimewatch.highsec-restrictions, feat:crimewatch.security-bands, feat:crimewatch.weapon-timer-scope
// parity: feat:crimewatch.pvp-logoff-timer-renewal, feat:crimewatch.highsec-criminal-restrictions
// parity: feat:crimewatch.third-party-assistance-legality, feat:crimewatch.criminal-wreck-auto-abandon
// parity: feat:crimewatch.lowsec-suspect-vs-criminal, feat:crimewatch.faction-police-thresholds
// parity: feat:crimewatch.outlaw-status, feat:crimewatch.sec-status-gain-caps, feat:crimewatch.sec-tag-tier-bands
// parity: feat:crimewatch.weapon-timer-tether-filament-block, feat:npc.sentry-gun-grid-aggression
// parity: feat:modules.salvager, feat:combat.wreck-size-salvage-difficulty, feat:modules.tractor-beam
// parity: feat:drones.launch-drones, feat:drones.recall-drones, feat:drones.return-orbit, feat:drones.engage-target
// parity: feat:drones.assist-guard, feat:drones.aggressive-passive, feat:drones.abandon-scoop, feat:drones.reconnect-drones
// parity: feat:drones.drone-bandwidth, feat:drones.drone-control-range, feat:drones.salvage-drones, feat:drones.logistic-drones
// parity: feat:drones.ewar-drones, feat:drones.sentry-drones, feat:drones.drone-hp-loss, feat:drones.sentry-drone-scoop-distance
// parity: feat:drones.gecko-augmented-mutated-drones, feat:drones.gecko-subverted-bandwidth
// parity: feat:combat.bounty-payout-npc, feat:npc.belt-rats, feat:npc.npc-ai-targeting, feat:npc.npc-damage-types
// parity: feat:npc.npc-loot, feat:npc.sentry-guns, feat:npc.drifters, feat:npc.triglavians, feat:npc.sleepers
// parity: feat:npc.edencom-npc, feat:npc.rogue-drones, feat:npc.incursion-npc, feat:npc.navy-npc, feat:npc.npc-hauler
// parity: feat:npc.npc-spawn-encounter, feat:npc.npc-standings-aggression, feat:npc.officer-npc, feat:npc.npc-ewar-application
// parity: feat:npc.npc-remote-repair, feat:npc.npc-target-switching, feat:npc.clone-soldier-spawns
// parity: feat:drones.fighter-load-tubes, feat:drones.fighter-launch-recall, feat:drones.fighter-movement
// parity: feat:drones.fighter-abilities, feat:drones.fighter-abandon, feat:drones.fighter-squadron-losses
// parity: feat:combat.insurance-quote, feat:combat.insurance-buy, feat:combat.insurance-payout
// parity: feat:combat.insurance-contracts, feat:ships.ship-insurance-link
// parity: feat:combat.player-bounties, feat:combat.bounty-leaderboards, feat:admin.gm-bounty-tools
// parity: rpc:bountyProxy.GetBountiesAndKillRights
// parity: feat:pve.ess, feat:pve.ess-reserve, feat:pve.dynamic-bounty

#![forbid(unsafe_code)]

use crate::formulas::{
    breacher_pod_tick_damage, compute_dbs_and_ess_bounty_split, compute_insurance_levels,
    compute_insurance_quote, compute_ship_insurance_reference_value,
    entropic_spool_multiplier, missile_damage_factor, turret_chance_to_hit,
    turret_chance_to_hit_with_res, turret_shot_damage, vorton_arc_targets_with_cap,
};
use crate::router::{NotificationTarget, ServerContext};
pub use crate::router::RpcArgs;
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, KillRightRecord, KillmailRecord,
    SystemDynamicState,
};
use eve_destiny::DamageVector;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::{HashMap, VecDeque};

#[path = "combat_fighters.rs"]
pub mod combat_fighters;
#[path = "combat_fighters_abilities.rs"]
pub mod combat_fighters_abilities;
#[path = "combat_legacy_ess.rs"]
pub mod combat_legacy_ess;
#[path = "combat_missiles_aoe.rs"]
pub mod combat_missiles_aoe;
#[path = "combat_npc_ai.rs"]
pub mod combat_npc_ai;
#[path = "combat_npc_encounters.rs"]
pub mod combat_npc_encounters;
#[path = "combat_npc_police.rs"]
pub mod combat_npc_police;
#[path = "combat_npcs_drones.rs"]
pub mod combat_npcs_drones;
#[path = "combat_weapons.rs"]
pub mod combat_weapons;
#[path = "combat_weapons_spool.rs"]
pub mod combat_weapons_spool;
#[path = "combat_wrecks_killmails.rs"]
pub mod combat_wrecks_killmails;
#[path = "crimewatch_bounty.rs"]
pub mod crimewatch_bounty;
#[path = "crimewatch_scene_notify.rs"]
mod crimewatch_scene_notify;
#[path = "crimewatch_timers.rs"]
pub mod crimewatch_timers;

pub use combat_fighters::*;
pub(crate) use combat_fighters_abilities::*;
pub use combat_legacy_ess::*;
pub use combat_missiles_aoe::*;
pub use combat_npc_ai::*;
pub use combat_npc_encounters::*;
pub use combat_npc_police::*;
pub use combat_npcs_drones::*;
pub use combat_weapons::*;
pub use combat_weapons_spool::*;
pub use combat_wrecks_killmails::*;
pub use crimewatch_bounty::*;
pub(crate) use crimewatch_scene_notify::emit_combat_log_and_damage_state;
pub use crimewatch_timers::*;

/// Wreck lifetime in seconds (2 hours = 7,200s per EVE wiki `Wrecks.md`).
pub const WRECK_DECAY_LIFETIME_SECS: u64 = 7_200;

/// Jettisoned cargo container (jetcan) lifetime in seconds (1 hour = 3,600s).
pub const JETCAN_DECAY_LIFETIME_SECS: u64 = 3_600;

/// Killmail assist window in seconds (15 minutes = 900s).
pub const KILLMAIL_ASSIST_WINDOW_SECS: u64 = 900;

/// Self-destruct countdown duration in seconds (2 minutes = 120s).
pub const SELF_DESTRUCT_COUNTDOWN_SECS: u64 = 120;

/// Schedules a high-security CONCORD response if `char_id` enters high-sec space (`>= 0.45`)
/// while flagged Criminal in a non-capsule ship and CONCORD is not suppressed in `sys_id` (#559).
pub fn schedule_highsec_criminal_concord_if_needed(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    sys_id: u32,
    now_unix: u64,
) {
    let sys_sec = with_combat_runtime(ctx, |rt| rt.modified_system_securities.get(&sys_id).copied())
        .unwrap_or_else(|| {
            ctx.sde
                .solar_systems
                .get(&sys_id)
                .map(|s| s.security_status)
                .unwrap_or(0.0)
        });
    if sys_sec < 0.45 || is_concord_suppressed_in_system(ctx, sys_id) {
        return;
    }
    let is_capsule = ctx.db.with_snapshot(|s| {
        s.items.get(&ship_id).is_some_and(|i| {
            i.type_id == 670
                || ctx.sde.types.get(&i.type_id).is_some_and(|t| t.group_id == 29)
        })
    });
    if is_capsule {
        return;
    }
    let mut cw = ctx.crimewatch.write().unwrap_or_else(|e| e.into_inner());
    let st = cw.get_or_create(char_id);
    if st.is_criminal(now_unix) {
        let needs_schedule = match st.pending_concord_arrival {
            None => true,
            Some((prev_sys, _, prev_ship, _)) => prev_sys != sys_id || prev_ship != ship_id,
        };
        if needs_schedule {
            let delay_sec = crate::combat::crimewatch::concord_delay_for_security(sys_sec);
            st.pending_concord_arrival = Some((
                sys_id,
                now_unix.saturating_add(delay_sec.ceil() as u64),
                ship_id,
                0,
            ));
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EntropicSpoolState {
    pub target_id: i64,
    pub solar_system_id: u32,
    pub attacker_char_id: i64,
    pub attacker_ship_id: i64,
    pub module_id: i64,
    pub module_type_id: u32,
    pub charge_item_id: Option<i64>,
    pub base_damage: eve_destiny::DamageVector,
    pub cap_need: f64,
    pub duration_s: f64,
    pub cycles: u32,
    pub elapsed_in_cycle: f64,
    pub per_cycle_bonus: f64,
    pub max_bonus: f64,
    pub current_multiplier: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct InFlightMissileState {
    pub missile_ball_id: i64,
    pub solar_system_id: u32,
    pub attacker_char_id: i64,
    pub attacker_ship_id: i64,
    pub launcher_module_id: i64,
    pub launcher_type_id: u32,
    pub missile_type_id: u32,
    pub target_ball_id: i64,
    pub max_velocity: f64,
    pub flight_time_s: f64,
    pub remaining_flight_s: f64,
    pub hp: f64,
    pub max_hp: f64,
    pub base_damage: DamageVector,
    pub explosion_radius: f64,
    pub explosion_velocity: f64,
    pub drf: f64,
    pub is_fof: bool,
    pub is_breacher_pod: bool,
    pub breacher_dot_duration_s: f64,
    pub breacher_flat_cap: f64,
    pub breacher_pct_cap: f64,
    pub destroyed: bool,
    pub expired: bool,
    pub impacted: bool,
    pub applied_damage: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ActiveBreacherDot {
    pub target_ball_id: i64,
    pub source_char_id: i64,
    pub source_ship_id: i64,
    pub weapon_type_id: u32,
    pub charge_type_id: u32,
    pub flat_cap_dps: f64,
    pub pct_cap_fraction: f64,
    pub remaining_s: f64,
    pub tick_damage: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AssistLogEntry {
    pub attacker_char_id: i64,
    pub corporation_id: i64,
    pub alliance_id: Option<i64>,
    pub ship_type_id: u32,
    pub weapon_type_id: u32,
    pub damage_done: f64,
    pub is_ewar: bool,
    pub last_action_unix: u64,
}

pub use eve_db::{DetailedKillmail, KillmailAttackerEntry, KillmailItemEntry};

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContainerLifetimeEntry {
    pub item_id: i64,
    pub solar_system_id: u32,
    pub type_id: u32,
    pub owner_id: i64,
    pub created_unix: u64,
    pub lifetime_secs: u64,
    pub is_abandoned: bool,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SelfDestructCountdown {
    pub ship_id: i64,
    pub char_id: i64,
    pub solar_system_id: u32,
    pub started_unix: u64,
    pub detonate_at_unix: u64,
}

pub use eve_db::PersistedSecStatusTransaction as SecStatusTransaction;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SentryAggressionState {
    pub solar_system_id: u32,
    pub offender_char_id: i64,
    pub offender_ship_id: i64,
    pub sentry_origin: DVec3,
    pub active_on_grid: bool,
    pub total_sentry_damage_dealt: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum SafetyLevel {
    Red = 0,
    Yellow = 1,
    #[default]
    Green = 2,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DroneBehaviorSettings {
    pub aggressive: bool,
    pub focus_fire: bool,
}

impl Default for DroneBehaviorSettings {
    fn default() -> Self {
        Self { aggressive: true, focus_fire: true }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DroneSpaceState {
    pub ball_id: i64,
    pub drone_item_id: i64,
    pub type_id: u32,
    pub group_id: u32,
    pub owner_char_id: i64,
    pub original_owner_char_id: i64,
    pub parent_ship_id: i64,
    pub solar_system_id: u32,
    pub target_id: Option<i64>,
    pub activity_state: i64,
    pub assist_char_id: Option<i64>,
    pub guard_char_id: Option<i64>,
    pub is_abandoned: bool,
    pub is_sentry: bool,
    pub bandwidth_used: f64,
    pub max_velocity: f64,
    pub optimal_range: f64,
    pub damage_vector: DamageVector,
    pub shield_repair_amount: f64,
    pub armor_repair_amount: f64,
    pub hull_repair_amount: f64,
    pub web_speed_factor: f64,
    pub neut_amount: f64,
    pub ecm_strength: f64,
    pub tp_sig_bonus_pct: f64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ExtendedNpcEntity {
    pub ball_id: i64,
    pub type_id: u32,
    pub faction_id: u32,
    pub archetype: String,
    pub solar_system_id: u32,
    pub spawn_group_id: u32,
    pub target_id: Option<i64>,
    pub aggression_threat: HashMap<i64, f64>,
    pub threat_table: HashMap<i64, f64>,
    pub bounty_isk: f64,
    pub optimal_range: f64,
    pub orbit_range: f64,
    pub damage_vector: DamageVector,
    pub entropic_cycles: u32,
    pub drifter_overshield_hp: f64,
    pub drifter_doomsday_fired: bool,
    pub web_speed_factor: f64,
    pub web_range: f64,
    pub scram_strength: f64,
    pub scram_range: f64,
    pub neut_amount: f64,
    pub ecm_jammed: bool,
    pub remote_shield_rep: f64,
    pub remote_armor_rep: f64,
    pub remote_rep_range: f64,
    pub tag_drop_type_id: Option<u32>,
    pub spawn_position: DVec3,
    pub sleeper_wh_class: u8,
    pub custom_cargo_loot: Option<(u32, i64)>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EncounterWaveDirector {
    pub encounter_id: u32,
    pub solar_system_id: u32,
    pub current_wave: u32,
    pub max_waves: u32,
    pub trigger_ball_id: i64,
    pub npc_type_id: u32,
    pub active_wave_ball_ids: Vec<i64>,
    pub completed: bool,
    pub wave_npc_types: Vec<Vec<u32>>,
    pub origin_position: DVec3,
    pub next_wave_pending: bool,
}

#[derive(Debug, Default, Clone)]
pub struct CombatRuntimeState {
    #[doc(hidden)]
    pub synced_db_epoch: u64,
    #[doc(hidden)]
    pub synced_runtime_epoch: u64,
    #[doc(hidden)]
    pub synced_ext_hash: u64,
    #[doc(hidden)]
    pub synced_baseline: Option<std::sync::Arc<eve_db::PersistedCombatCrimewatchState>>,
    pub entropic_spools: HashMap<(i64, i64), EntropicSpoolState>,
    pub in_flight_missiles: HashMap<i64, InFlightMissileState>,
    pub active_breacher_dots: HashMap<i64, ActiveBreacherDot>,
    pub aggression_assist_log: HashMap<i64, Vec<AssistLogEntry>>,
    pub detailed_killmails: HashMap<i64, DetailedKillmail>,
    pub container_lifetimes: HashMap<i64, ContainerLifetimeEntry>,
    pub self_destruct_countdowns: HashMap<i64, SelfDestructCountdown>,
    pub self_destruct_generations: HashMap<i64, u64>,
    pub sec_status_transactions: HashMap<i64, Vec<SecStatusTransaction>>,
    pub duel_challenges: HashMap<(i64, i64), u64>,
    pub corp_aggression_settings: HashMap<i64, bool>,
    pub modified_system_securities: HashMap<u32, f64>,
    pub disapproval_flags: HashMap<i64, u64>,
    pub sentry_gun_aggression: HashMap<(u32, i64), SentryAggressionState>,
    pub safety_levels: HashMap<i64, SafetyLevel>,
    pub limited_engagements: HashMap<i64, HashMap<i64, u64>>,
    pub npc_timers: HashMap<i64, u64>,
    pub crimewatch_timer_states: HashMap<i64, (i64, i64, i64, i64, i64)>,
    pub crimewatch_timer_triggered_at: HashMap<i64, [u64; 5]>,
    pub active_drones_in_space: HashMap<i64, DroneSpaceState>,
    pub drone_next_ready_unix: HashMap<i64, u64>,
    pub drone_settings: HashMap<i64, DroneBehaviorSettings>,
    pub npc_entities: HashMap<i64, ExtendedNpcEntity>,
    pub pending_bounty_ticks: HashMap<(u32, i64), f64>,
    pub bounty_tick_next_flush_unix: HashMap<(u32, i64), u64>,
    pub pending_bounty_tick_started_unix: HashMap<(u32, i64), u64>,
    pub encounter_waves: HashMap<u32, EncounterWaveDirector>,
    pub player_bounty_pool: HashMap<i64, f64>,
    pub bounty_contributions: HashMap<i64, Vec<(i64, f64)>>,
    pub bounty_hunter_claimed: HashMap<i64, f64>,
    pub fighter_unit_damage_accum: HashMap<i64, f64>,
    pub fighter_tube_rearm_until: HashMap<(i64, usize), u64>,
    pub kill_right_expires_unix: HashMap<i64, u64>,
    pub kill_right_restricted_to: HashMap<i64, i64>,
    pub behavior_debugging_active: std::collections::HashSet<i64>,
    pub authorized_shipping_storage: std::collections::HashSet<(i64, i64)>,
    pub active_drone_miners: HashMap<i64, DroneMiningState>,
    pub ess_main_bank_thefts: HashMap<u32, Vec<(i64, f64, u64)>>,
    pub ess_reserve_bank_thefts: HashMap<u32, Vec<(i64, f64, u64)>>,
    pub ess_reserve_unlocked_until: HashMap<u32, u64>,
    pub clone_soldier_cooldown_until: HashMap<u32, u64>,
    pub bounty_credited_balls: std::collections::HashSet<i64>,
    pub destroyed_npc_cache: HashMap<i64, ExtendedNpcEntity>,
    pub ci_sentry_deployed_systems: std::collections::HashSet<u32>,
    pub npc_scrambled_ships: HashMap<u32, std::collections::HashSet<i64>>,
    pub npc_sec_gain_intervals: HashMap<(i64, i32), (u64, f64)>,
    pub recently_destroyed_item_ids: VecDeque<i64>,
    pub self_destruct_sim_remaining_secs: HashMap<i64, f64>,
    pub gm_logi_soaks: HashMap<u32, crate::services::gm_soak_commands::GmLogiSoakState>,
    pub gm_deadwarps: HashMap<i64, crate::services::gm_soak_commands::GmDeadwarpState>,
    pub(crate) extended_npc_weapon_runtime: crate::combat::npc_ai_weapons_loot::SharedExtendedNpcWeaponRuntime,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DroneMiningState {
    pub drone_id: i64,
    pub asteroid_id: i64,
    pub ship_id: i64,
    pub owner_char_id: i64,
    pub solar_system_id: u32,
    pub yield_units: u64,
    pub duration_secs: u64,
    pub next_cycle_unix: u64,
    pub waste_prob: f64,
    pub ore_type_id: u32,
    pub destination_flag: eve_db::InventoryFlag,
}

#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct PersistedCombatExtendedPayload {
    entropic_spools: Vec<((i64, i64), EntropicSpoolState)>,
    in_flight_missiles: HashMap<i64, InFlightMissileState>,
    active_breacher_dots: HashMap<i64, ActiveBreacherDot>,
    aggression_assist_log: HashMap<i64, Vec<AssistLogEntry>>,
    container_lifetimes: HashMap<i64, ContainerLifetimeEntry>,
    self_destruct_countdowns: HashMap<i64, SelfDestructCountdown>,
    #[serde(default)]
    self_destruct_generations: HashMap<i64, u64>,
    modified_system_securities: HashMap<u32, f64>,
    sentry_gun_aggression: Vec<((u32, i64), SentryAggressionState)>,
    crimewatch_timer_states: HashMap<i64, (i64, i64, i64, i64, i64)>,
    crimewatch_timer_triggered_at: HashMap<i64, [u64; 5]>,
    active_drones_in_space: HashMap<i64, DroneSpaceState>,
    drone_next_ready_unix: HashMap<i64, u64>,
    npc_entities: HashMap<i64, ExtendedNpcEntity>,
    pending_bounty_ticks: Vec<((u32, i64), f64)>,
    bounty_tick_next_flush_unix: Vec<((u32, i64), u64)>,
    pending_bounty_tick_started_unix: Vec<((u32, i64), u64)>,
    encounter_waves: HashMap<u32, EncounterWaveDirector>,
    fighter_unit_damage_accum: HashMap<i64, f64>,
    fighter_tube_rearm_until: Vec<((i64, usize), u64)>,
    behavior_debugging_active: std::collections::HashSet<i64>,
    authorized_shipping_storage: Vec<(i64, i64)>,
    active_drone_miners: HashMap<i64, DroneMiningState>,
    ess_main_bank_thefts: HashMap<u32, Vec<(i64, f64, u64)>>,
    ess_reserve_bank_thefts: HashMap<u32, Vec<(i64, f64, u64)>>,
    ess_reserve_unlocked_until: HashMap<u32, u64>,
    clone_soldier_cooldown_until: HashMap<u32, u64>,
    bounty_credited_balls: std::collections::HashSet<i64>,
    destroyed_npc_cache: HashMap<i64, ExtendedNpcEntity>,
    ci_sentry_deployed_systems: std::collections::HashSet<u32>,
    npc_scrambled_ships: HashMap<u32, std::collections::HashSet<i64>>,
    npc_sec_gain_intervals: Vec<((i64, i32), (u64, f64))>,
    recently_destroyed_item_ids: VecDeque<i64>,
    self_destruct_sim_remaining_secs: HashMap<i64, f64>,
}

impl CombatRuntimeState {
    pub(crate) fn ext_runtime_hash(p: &eve_db::PersistedCombatCrimewatchState) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for &b in p.extended_runtime_json.as_bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        if h == 0 {
            1
        } else {
            h
        }
    }

    #[cfg(test)]
    pub(crate) fn rehydrate_from_persisted_epochs(
        &mut self,
        p: &eve_db::PersistedCombatCrimewatchState,
        combat_epoch: u64,
        force_full: bool,
    ) {
        let ext_hash = Self::ext_runtime_hash(p);
        if force_full || self.synced_db_epoch == 0 || self.synced_ext_hash != ext_hash {
            self.sync_from_persisted(p);
        } else {
            self.sync_core_from_persisted(p);
        }
        self.synced_db_epoch = combat_epoch;
        self.synced_runtime_epoch = combat_epoch;
        self.synced_ext_hash = ext_hash;
        self.synced_baseline = Some(std::sync::Arc::new(p.clone()));
        crate::combat::npc_ai_weapons_loot::prune_wrt_snap(
            crate::combat::npc_ai_weapons_loot::capture_wrt_prune(&self.extended_npc_weapon_runtime, &self.npc_entities),
        );
    }

    #[inline]
    pub(crate) fn is_synced_with_db(&self, combat_epoch: u64, runtime_epoch: u64) -> bool {
        self.synced_db_epoch != 0
            && if self.synced_runtime_epoch != 0 {
                self.synced_runtime_epoch == runtime_epoch
            } else {
                self.synced_db_epoch == combat_epoch
            }
    }

    fn mark_synced_arc(
        &mut self,
        arc: &std::sync::Arc<eve_db::PersistedCombatCrimewatchState>,
        combat_epoch: u64,
        runtime_epoch: u64,
    ) {
        self.synced_db_epoch = combat_epoch;
        self.synced_runtime_epoch = runtime_epoch;
        self.synced_ext_hash = Self::ext_runtime_hash(arc);
        self.synced_baseline = Some(std::sync::Arc::clone(arc));
    }

    pub(crate) fn reconcile_with_db_persisted(
        &mut self,
        db_arc: &std::sync::Arc<eve_db::PersistedCombatCrimewatchState>,
        combat_epoch: u64,
        runtime_epoch: u64,
        force_full: bool,
    ) {
        if force_full || self.synced_db_epoch == 0 {
            self.sync_from_persisted(db_arc);
            self.mark_synced_arc(db_arc, combat_epoch, runtime_epoch);
            return;
        }
        if self.synced_runtime_epoch != 0 && self.synced_runtime_epoch == runtime_epoch {
            self.synced_db_epoch = combat_epoch;
            return;
        }
        if let Some(base_arc) = self.synced_baseline.as_ref().map(std::sync::Arc::clone) {
            if std::sync::Arc::ptr_eq(&base_arc, db_arc) || *base_arc == **db_arc {
                self.synced_db_epoch = combat_epoch;
                self.synced_runtime_epoch = runtime_epoch;
                return;
            }
            let ram = self.to_persisted();
            if ram == *base_arc {
                self.sync_from_persisted(db_arc);
                self.mark_synced_arc(db_arc, combat_epoch, runtime_epoch);
                return;
            }
            let merged = merge_persisted_3way(&base_arc, db_arc, &ram);
            self.sync_from_persisted(&merged);
            self.mark_synced_arc(db_arc, combat_epoch, runtime_epoch);
        } else {
            let db_ext_hash = Self::ext_runtime_hash(db_arc);
            if self.synced_ext_hash != 0 && self.synced_ext_hash == db_ext_hash {
                self.sync_core_from_persisted(db_arc);
                self.synced_db_epoch = combat_epoch;
                self.synced_runtime_epoch = runtime_epoch;
                self.synced_ext_hash = db_ext_hash;
            } else {
                let default_base = eve_db::PersistedCombatCrimewatchState::default();
                let ram = self.to_persisted();
                let merged = merge_persisted_3way(&default_base, db_arc, &ram);
                self.sync_from_persisted(&merged);
                self.mark_synced_arc(db_arc, combat_epoch, runtime_epoch);
            }
        }
    }

    pub fn has_limited_engagement_with(&self, char_id: i64, target_id: i64, now_unix: u64) -> bool {
        self.limited_engagements
            .get(&char_id)
            .and_then(|m| m.get(&target_id))
            .is_some_and(|&exp| now_unix < exp)
    }

    pub fn add_limited_engagement(&mut self, char_id: i64, target_id: i64, now_unix: u64) {
        let exp = now_unix.saturating_add(300);
        self.limited_engagements
            .entry(char_id)
            .or_default()
            .insert(target_id, exp);
    }

    pub fn is_in_any_limited_engagement(&self, char_id: i64, now_unix: u64) -> bool {
        self.limited_engagements
            .get(&char_id)
            .is_some_and(|m| m.values().any(|&exp| now_unix < exp))
    }

    pub fn trigger_npc_timer(&mut self, char_id: i64, now_unix: u64) {
        self.npc_timers.insert(char_id, now_unix.saturating_add(300));
    }

    pub fn is_npc_timer_active(&self, char_id: i64, now_unix: u64) -> bool {
        self.npc_timers
            .get(&char_id)
            .is_some_and(|&exp| now_unix < exp)
    }

    /// Returns the monotonic self-destruct generation counter for `ship_id` (`0` if never set) (`#1174`).
    pub fn self_destruct_generation(&self, ship_id: i64) -> u64 {
        self.self_destruct_generations.get(&ship_id).copied().unwrap_or(0)
    }

    /// Increments and returns the monotonic self-destruct generation counter for `ship_id` (`#1174`).
    pub fn bump_self_destruct_generation(&mut self, ship_id: i64) -> u64 {
        let next = self
            .self_destruct_generations
            .get(&ship_id)
            .copied()
            .unwrap_or(0)
            .wrapping_add(1)
            .max(1);
        self.self_destruct_generations.insert(ship_id, next);
        next
    }

    /// Records an item or ball ID whose inventory/space state was destroyed so late
    /// client `dogmaIM.ItemGetInfo` queries return `None` instead of `TypeNotFound` (`#1032`).
    pub fn record_destroyed_item_id(&mut self, item_id: i64) {
        const MAX_RECENTLY_DESTROYED_ITEM_IDS: usize = 1_024;
        if item_id <= 0 {
            return;
        }
        if let Some(pos) = self
            .recently_destroyed_item_ids
            .iter()
            .position(|&existing| existing == item_id)
        {
            self.recently_destroyed_item_ids.remove(pos);
        } else if self.recently_destroyed_item_ids.len() >= MAX_RECENTLY_DESTROYED_ITEM_IDS {
            self.recently_destroyed_item_ids.pop_front();
        }
        self.recently_destroyed_item_ids.push_back(item_id);
    }

    /// Returns `true` if `item_id` was recently destroyed in combat or space cleanup (`#1032`).
    pub fn is_recently_destroyed_item_id(&self, item_id: i64) -> bool {
        item_id > 0 && self.recently_destroyed_item_ids.contains(&item_id)
    }

    pub fn sync_core_from_persisted(&mut self, p: &eve_db::PersistedCombatCrimewatchState) {
        self.sec_status_transactions = p.sec_status_transactions.clone();
        self.safety_levels = p
            .safety_levels
            .iter()
            .map(|(&cid, &lvl)| {
                let s = match lvl {
                    0 => SafetyLevel::Red,
                    1 => SafetyLevel::Yellow,
                    _ => SafetyLevel::Green,
                };
                (cid, s)
            })
            .collect();
        self.drone_settings = p
            .drone_settings
            .iter()
            .map(|(&cid, &(aggressive, focus_fire))| {
                (
                    cid,
                    DroneBehaviorSettings {
                        aggressive,
                        focus_fire,
                    },
                )
            })
            .collect();
        self.limited_engagements = p.limited_engagements.clone();
        self.npc_timers = p.npc_timers.clone();
        self.disapproval_flags = p.disapproval_flags.clone();
        self.duel_challenges = p.duel_challenges.clone();
        self.corp_aggression_settings = p.corp_aggression_settings.clone();
        self.player_bounty_pool = p.player_bounty_pool.clone();
        self.bounty_contributions = p.bounty_contributions.clone();
        self.bounty_hunter_claimed = p.bounty_hunter_claimed.clone();
        self.kill_right_expires_unix = p.kill_right_expires_unix.clone();
        self.kill_right_restricted_to = p.kill_right_restricted_to.clone();
        self.detailed_killmails = p.detailed_killmails.clone();
        self.pending_bounty_ticks = p.pending_bounty_ticks.clone();
        self.bounty_tick_next_flush_unix = p.bounty_tick_next_flush_unix.clone();
        self.pending_bounty_tick_started_unix = p.pending_bounty_tick_started_unix.clone();
    }

    pub fn sync_from_persisted(&mut self, p: &eve_db::PersistedCombatCrimewatchState) {
        self.sync_core_from_persisted(p);

        if p.extended_runtime_json.is_empty() {
            self.entropic_spools.clear();
            self.in_flight_missiles.clear();
            self.active_breacher_dots.clear();
            self.aggression_assist_log.clear();
            self.container_lifetimes.clear();
            self.self_destruct_countdowns.clear();
            self.self_destruct_generations.clear();
            self.modified_system_securities.clear();
            self.sentry_gun_aggression.clear();
            self.crimewatch_timer_states.clear();
            self.crimewatch_timer_triggered_at.clear();
            self.active_drones_in_space.clear();
            self.drone_next_ready_unix.clear();
            self.npc_entities.clear();
            self.encounter_waves.clear();
            self.fighter_unit_damage_accum.clear();
            self.fighter_tube_rearm_until.clear();
            self.behavior_debugging_active.clear();
            self.authorized_shipping_storage.clear();
            self.active_drone_miners.clear();
            self.ess_main_bank_thefts.clear();
            self.ess_reserve_bank_thefts.clear();
            self.ess_reserve_unlocked_until.clear();
            self.clone_soldier_cooldown_until.clear();
            self.bounty_credited_balls.clear();
            self.destroyed_npc_cache.clear();
            self.ci_sentry_deployed_systems.clear();
            self.npc_scrambled_ships.clear();
            self.npc_sec_gain_intervals.clear();
            self.recently_destroyed_item_ids.clear();
            self.self_destruct_sim_remaining_secs.clear();
        } else if let Ok(ext) =
            serde_json::from_str::<PersistedCombatExtendedPayload>(&p.extended_runtime_json)
        {
            self.entropic_spools = ext.entropic_spools.into_iter().collect();
            self.in_flight_missiles = ext.in_flight_missiles;
            self.active_breacher_dots = ext.active_breacher_dots;
            self.aggression_assist_log = ext.aggression_assist_log;
            self.container_lifetimes = ext.container_lifetimes;
            self.self_destruct_countdowns = ext.self_destruct_countdowns;
            self.self_destruct_generations = ext.self_destruct_generations;
            self.modified_system_securities = ext.modified_system_securities;
            self.sentry_gun_aggression = ext.sentry_gun_aggression.into_iter().collect();
            self.crimewatch_timer_states = ext.crimewatch_timer_states;
            self.crimewatch_timer_triggered_at = ext.crimewatch_timer_triggered_at;
            self.active_drones_in_space = ext.active_drones_in_space;
            self.drone_next_ready_unix = ext.drone_next_ready_unix;
            self.npc_entities = ext.npc_entities;
            if !ext.pending_bounty_ticks.is_empty() || p.pending_bounty_ticks.is_empty() {
                self.pending_bounty_ticks = ext.pending_bounty_ticks.into_iter().collect();
            }
            if !ext.bounty_tick_next_flush_unix.is_empty()
                || p.bounty_tick_next_flush_unix.is_empty()
            {
                self.bounty_tick_next_flush_unix =
                    ext.bounty_tick_next_flush_unix.into_iter().collect();
            }
            if !ext.pending_bounty_tick_started_unix.is_empty()
                || p.pending_bounty_tick_started_unix.is_empty()
            {
                self.pending_bounty_tick_started_unix =
                    ext.pending_bounty_tick_started_unix.into_iter().collect();
            }
            self.encounter_waves = ext.encounter_waves;
            self.fighter_unit_damage_accum = ext.fighter_unit_damage_accum;
            self.fighter_tube_rearm_until = ext.fighter_tube_rearm_until.into_iter().collect();
            self.behavior_debugging_active = ext.behavior_debugging_active;
            self.authorized_shipping_storage =
                ext.authorized_shipping_storage.into_iter().collect();
            self.active_drone_miners = ext.active_drone_miners;
            self.ess_main_bank_thefts = ext.ess_main_bank_thefts;
            self.ess_reserve_bank_thefts = ext.ess_reserve_bank_thefts;
            self.ess_reserve_unlocked_until = ext.ess_reserve_unlocked_until;
            self.clone_soldier_cooldown_until = ext.clone_soldier_cooldown_until;
            self.bounty_credited_balls = ext.bounty_credited_balls;
            self.destroyed_npc_cache = ext.destroyed_npc_cache;
            self.ci_sentry_deployed_systems = ext.ci_sentry_deployed_systems;
            self.npc_scrambled_ships = ext.npc_scrambled_ships;
            self.npc_sec_gain_intervals = ext.npc_sec_gain_intervals.into_iter().collect();
            self.recently_destroyed_item_ids = ext.recently_destroyed_item_ids;
            self.self_destruct_sim_remaining_secs = ext.self_destruct_sim_remaining_secs;
        }
    }

    fn has_extended_runtime_state(&self) -> bool {
        !self.entropic_spools.is_empty()
            || !self.in_flight_missiles.is_empty()
            || !self.active_breacher_dots.is_empty()
            || !self.aggression_assist_log.is_empty()
            || !self.container_lifetimes.is_empty()
            || !self.self_destruct_countdowns.is_empty()
            || !self.self_destruct_generations.is_empty()
            || !self.self_destruct_sim_remaining_secs.is_empty()
            || !self.modified_system_securities.is_empty()
            || !self.sentry_gun_aggression.is_empty()
            || !self.crimewatch_timer_states.is_empty()
            || !self.crimewatch_timer_triggered_at.is_empty()
            || !self.active_drones_in_space.is_empty()
            || !self.drone_next_ready_unix.is_empty()
            || !self.npc_entities.is_empty()
            || !self.pending_bounty_ticks.is_empty()
            || !self.bounty_tick_next_flush_unix.is_empty()
            || !self.pending_bounty_tick_started_unix.is_empty()
            || !self.encounter_waves.is_empty()
            || !self.fighter_unit_damage_accum.is_empty()
            || !self.fighter_tube_rearm_until.is_empty()
            || !self.behavior_debugging_active.is_empty()
            || !self.authorized_shipping_storage.is_empty()
            || !self.active_drone_miners.is_empty()
            || !self.ess_main_bank_thefts.is_empty()
            || !self.ess_reserve_bank_thefts.is_empty()
            || !self.ess_reserve_unlocked_until.is_empty()
            || !self.clone_soldier_cooldown_until.is_empty()
            || !self.bounty_credited_balls.is_empty()
            || !self.destroyed_npc_cache.is_empty()
            || !self.ci_sentry_deployed_systems.is_empty()
            || !self.npc_scrambled_ships.is_empty()
            || !self.npc_sec_gain_intervals.is_empty()
            || !self.recently_destroyed_item_ids.is_empty()
    }

    pub fn to_persisted(&self) -> eve_db::PersistedCombatCrimewatchState {
        let extended_runtime_json = if self.has_extended_runtime_state() {
            let mut payload = PersistedCombatExtendedPayload {
                entropic_spools: self
                    .entropic_spools
                    .iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect(),
                in_flight_missiles: self.in_flight_missiles.clone(),
                active_breacher_dots: self.active_breacher_dots.clone(),
                aggression_assist_log: self.aggression_assist_log.clone(),
                container_lifetimes: self.container_lifetimes.clone(),
                self_destruct_countdowns: self.self_destruct_countdowns.clone(),
                self_destruct_generations: self.self_destruct_generations.clone(),
                modified_system_securities: self.modified_system_securities.clone(),
                sentry_gun_aggression: self
                    .sentry_gun_aggression
                    .iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect(),
                crimewatch_timer_states: self.crimewatch_timer_states.clone(),
                crimewatch_timer_triggered_at: self.crimewatch_timer_triggered_at.clone(),
                active_drones_in_space: self.active_drones_in_space.clone(),
                drone_next_ready_unix: self.drone_next_ready_unix.clone(),
                npc_entities: self.npc_entities.clone(),
                pending_bounty_ticks: self
                    .pending_bounty_ticks
                    .iter()
                    .map(|(k, v)| (*k, *v))
                    .collect(),
                bounty_tick_next_flush_unix: self
                    .bounty_tick_next_flush_unix
                    .iter()
                    .map(|(k, v)| (*k, *v))
                    .collect(),
                pending_bounty_tick_started_unix: self
                    .pending_bounty_tick_started_unix
                    .iter()
                    .map(|(k, v)| (*k, *v))
                    .collect(),
                encounter_waves: self.encounter_waves.clone(),
                fighter_unit_damage_accum: self.fighter_unit_damage_accum.clone(),
                fighter_tube_rearm_until: self
                    .fighter_tube_rearm_until
                    .iter()
                    .map(|(k, v)| (*k, *v))
                    .collect(),
                behavior_debugging_active: self.behavior_debugging_active.clone(),
                authorized_shipping_storage: self
                    .authorized_shipping_storage
                    .iter()
                    .copied()
                    .collect(),
                active_drone_miners: self.active_drone_miners.clone(),
                ess_main_bank_thefts: self.ess_main_bank_thefts.clone(),
                ess_reserve_bank_thefts: self.ess_reserve_bank_thefts.clone(),
                ess_reserve_unlocked_until: self.ess_reserve_unlocked_until.clone(),
                clone_soldier_cooldown_until: self.clone_soldier_cooldown_until.clone(),
                bounty_credited_balls: self.bounty_credited_balls.clone(),
                destroyed_npc_cache: self.destroyed_npc_cache.clone(),
                ci_sentry_deployed_systems: self.ci_sentry_deployed_systems.clone(),
                npc_scrambled_ships: self.npc_scrambled_ships.clone(),
                npc_sec_gain_intervals: self
                    .npc_sec_gain_intervals
                    .iter()
                    .map(|(k, v)| (*k, *v))
                    .collect(),
                recently_destroyed_item_ids: self.recently_destroyed_item_ids.clone(),
                self_destruct_sim_remaining_secs: self.self_destruct_sim_remaining_secs.clone(),
            };
            payload.entropic_spools.sort_by_key(|(k, _)| *k);
            payload.sentry_gun_aggression.sort_by_key(|(k, _)| *k);
            payload.pending_bounty_ticks.sort_by_key(|(k, _)| *k);
            payload.bounty_tick_next_flush_unix.sort_by_key(|(k, _)| *k);
            payload.pending_bounty_tick_started_unix.sort_by_key(|(k, _)| *k);
            payload.fighter_tube_rearm_until.sort_by_key(|(k, _)| *k);
            payload.authorized_shipping_storage.sort_unstable();
            payload.npc_sec_gain_intervals.sort_by_key(|(k, _)| *k);
            serde_json::to_string(&payload).unwrap_or_default()
        } else {
            String::new()
        };
        eve_db::PersistedCombatCrimewatchState {
            sec_status_transactions: self.sec_status_transactions.clone(),
            safety_levels: self
                .safety_levels
                .iter()
                .map(|(&cid, &s)| (cid, s as i32))
                .collect(),
            drone_settings: self
                .drone_settings
                .iter()
                .map(|(&cid, d)| (cid, (d.aggressive, d.focus_fire)))
                .collect(),
            limited_engagements: self.limited_engagements.clone(),
            npc_timers: self.npc_timers.clone(),
            disapproval_flags: self.disapproval_flags.clone(),
            duel_challenges: self.duel_challenges.clone(),
            corp_aggression_settings: self.corp_aggression_settings.clone(),
            player_bounty_pool: self.player_bounty_pool.clone(),
            bounty_contributions: self.bounty_contributions.clone(),
            bounty_hunter_claimed: self.bounty_hunter_claimed.clone(),
            kill_right_expires_unix: self.kill_right_expires_unix.clone(),
            kill_right_restricted_to: self.kill_right_restricted_to.clone(),
            detailed_killmails: self.detailed_killmails.clone(),
            pending_bounty_ticks: self.pending_bounty_ticks.clone(),
            bounty_tick_next_flush_unix: self.bounty_tick_next_flush_unix.clone(),
            pending_bounty_tick_started_unix: self.pending_bounty_tick_started_unix.clone(),
            extended_runtime_json,
        }
    }
}

fn merge_map_3way<K, V>(
    base: &HashMap<K, V>,
    db: &HashMap<K, V>,
    ram: &HashMap<K, V>,
) -> HashMap<K, V>
where
    K: Eq + std::hash::Hash + Clone,
    V: PartialEq + Clone,
{
    if db == base {
        return ram.clone();
    }
    if ram == base {
        return db.clone();
    }
    let mut out = db.clone();
    for (k, ram_v) in ram {
        if db.get(k) == base.get(k) {
            out.insert(k.clone(), ram_v.clone());
        }
    }
    for k in base.keys() {
        if !ram.contains_key(k) && db.get(k) == base.get(k) {
            out.remove(k);
        }
    }
    out
}

fn merge_set_3way<T>(
    base: &std::collections::HashSet<T>,
    db: &std::collections::HashSet<T>,
    ram: &std::collections::HashSet<T>,
) -> std::collections::HashSet<T>
where
    T: Eq + std::hash::Hash + Clone,
{
    if db == base {
        return ram.clone();
    }
    if ram == base {
        return db.clone();
    }
    let mut out = db.clone();
    for item in ram {
        if !base.contains(item) {
            out.insert(item.clone());
        }
    }
    for item in base {
        if !ram.contains(item) && db.contains(item) {
            out.remove(item);
        }
    }
    out
}

fn merge_pair_vec_3way<K, V>(
    base: &[(K, V)],
    db: &[(K, V)],
    ram: &[(K, V)],
) -> Vec<(K, V)>
where
    K: Eq + Ord + std::hash::Hash + Clone,
    V: PartialEq + Clone,
{
    let base_map: HashMap<K, V> = base.iter().cloned().collect();
    let db_map: HashMap<K, V> = db.iter().cloned().collect();
    let ram_map: HashMap<K, V> = ram.iter().cloned().collect();
    let mut out: Vec<(K, V)> = merge_map_3way(&base_map, &db_map, &ram_map)
        .into_iter()
        .collect();
    out.sort_by(|(a, _), (b, _)| a.cmp(b));
    out
}

fn merge_pair_set_vec_3way<T>(base: &[T], db: &[T], ram: &[T]) -> Vec<T>
where
    T: Eq + Ord + std::hash::Hash + Clone,
{
    let base_set: std::collections::HashSet<T> = base.iter().cloned().collect();
    let db_set: std::collections::HashSet<T> = db.iter().cloned().collect();
    let ram_set: std::collections::HashSet<T> = ram.iter().cloned().collect();
    let mut out: Vec<T> = merge_set_3way(&base_set, &db_set, &ram_set)
        .into_iter()
        .collect();
    out.sort_unstable();
    out
}

fn merge_recent_ids_3way(
    base: &VecDeque<i64>,
    db: &VecDeque<i64>,
    ram: &VecDeque<i64>,
) -> VecDeque<i64> {
    const MAX_RECENTLY_DESTROYED_ITEM_IDS: usize = 1_024;
    if db == base {
        return ram.clone();
    }
    if ram == base {
        return db.clone();
    }
    let mut out = db.clone();
    for &id in ram {
        if !base.contains(&id) && !out.contains(&id) {
            if out.len() >= MAX_RECENTLY_DESTROYED_ITEM_IDS {
                out.pop_front();
            }
            out.push_back(id);
        }
    }
    out
}

fn merge_monotonic_u64_map_3way(
    base: &HashMap<i64, u64>,
    db: &HashMap<i64, u64>,
    ram: &HashMap<i64, u64>,
) -> HashMap<i64, u64> {
    let mut out = HashMap::new();
    for map in [base, db, ram] {
        for (&k, &v) in map {
            out.entry(k)
                .and_modify(|cur: &mut u64| {
                    if v > *cur {
                        *cur = v;
                    }
                })
                .or_insert(v);
        }
    }
    out
}

fn merge_self_destruct_map_by_gen_3way<V: PartialEq + Clone>(
    base_gen: &HashMap<i64, u64>,
    db_gen: &HashMap<i64, u64>,
    ram_gen: &HashMap<i64, u64>,
    base: &HashMap<i64, V>,
    db: &HashMap<i64, V>,
    ram: &HashMap<i64, V>,
) -> HashMap<i64, V> {
    let mut out = merge_map_3way(base, db, ram);
    let mut keys: std::collections::HashSet<i64> = db_gen.keys().copied().collect();
    keys.extend(ram_gen.keys().copied());
    keys.extend(base_gen.keys().copied());
    for k in keys {
        let dg = db_gen.get(&k).copied().unwrap_or(0);
        let rg = ram_gen.get(&k).copied().unwrap_or(0);
        if dg > rg {
            match db.get(&k) {
                Some(v) => {
                    out.insert(k, v.clone());
                }
                None => {
                    out.remove(&k);
                }
            }
        } else if rg > dg {
            match ram.get(&k) {
                Some(v) => {
                    out.insert(k, v.clone());
                }
                None => {
                    out.remove(&k);
                }
            }
        }
    }
    out
}

fn merge_extended_payload_3way(
    base: &PersistedCombatExtendedPayload,
    db: &PersistedCombatExtendedPayload,
    ram: &PersistedCombatExtendedPayload,
) -> PersistedCombatExtendedPayload {
    let self_destruct_generations = merge_monotonic_u64_map_3way(
        &base.self_destruct_generations,
        &db.self_destruct_generations,
        &ram.self_destruct_generations,
    );
    let self_destruct_countdowns = merge_self_destruct_map_by_gen_3way(
        &base.self_destruct_generations,
        &db.self_destruct_generations,
        &ram.self_destruct_generations,
        &base.self_destruct_countdowns,
        &db.self_destruct_countdowns,
        &ram.self_destruct_countdowns,
    );
    let self_destruct_sim_remaining_secs = merge_self_destruct_map_by_gen_3way(
        &base.self_destruct_generations,
        &db.self_destruct_generations,
        &ram.self_destruct_generations,
        &base.self_destruct_sim_remaining_secs,
        &db.self_destruct_sim_remaining_secs,
        &ram.self_destruct_sim_remaining_secs,
    );
    PersistedCombatExtendedPayload {
        entropic_spools: merge_pair_vec_3way(&base.entropic_spools, &db.entropic_spools, &ram.entropic_spools),
        in_flight_missiles: merge_map_3way(&base.in_flight_missiles, &db.in_flight_missiles, &ram.in_flight_missiles),
        active_breacher_dots: merge_map_3way(&base.active_breacher_dots, &db.active_breacher_dots, &ram.active_breacher_dots),
        aggression_assist_log: merge_map_3way(&base.aggression_assist_log, &db.aggression_assist_log, &ram.aggression_assist_log),
        container_lifetimes: merge_map_3way(&base.container_lifetimes, &db.container_lifetimes, &ram.container_lifetimes),
        self_destruct_countdowns,
        self_destruct_generations,
        modified_system_securities: merge_map_3way(&base.modified_system_securities, &db.modified_system_securities, &ram.modified_system_securities),
        sentry_gun_aggression: merge_pair_vec_3way(&base.sentry_gun_aggression, &db.sentry_gun_aggression, &ram.sentry_gun_aggression),
        crimewatch_timer_states: merge_map_3way(&base.crimewatch_timer_states, &db.crimewatch_timer_states, &ram.crimewatch_timer_states),
        crimewatch_timer_triggered_at: merge_map_3way(&base.crimewatch_timer_triggered_at, &db.crimewatch_timer_triggered_at, &ram.crimewatch_timer_triggered_at),
        active_drones_in_space: merge_map_3way(&base.active_drones_in_space, &db.active_drones_in_space, &ram.active_drones_in_space),
        drone_next_ready_unix: merge_map_3way(&base.drone_next_ready_unix, &db.drone_next_ready_unix, &ram.drone_next_ready_unix),
        npc_entities: merge_map_3way(&base.npc_entities, &db.npc_entities, &ram.npc_entities),
        pending_bounty_ticks: merge_pair_vec_3way(&base.pending_bounty_ticks, &db.pending_bounty_ticks, &ram.pending_bounty_ticks),
        bounty_tick_next_flush_unix: merge_pair_vec_3way(&base.bounty_tick_next_flush_unix, &db.bounty_tick_next_flush_unix, &ram.bounty_tick_next_flush_unix),
        pending_bounty_tick_started_unix: merge_pair_vec_3way(&base.pending_bounty_tick_started_unix, &db.pending_bounty_tick_started_unix, &ram.pending_bounty_tick_started_unix),
        encounter_waves: merge_map_3way(&base.encounter_waves, &db.encounter_waves, &ram.encounter_waves),
        fighter_unit_damage_accum: merge_map_3way(&base.fighter_unit_damage_accum, &db.fighter_unit_damage_accum, &ram.fighter_unit_damage_accum),
        fighter_tube_rearm_until: merge_pair_vec_3way(&base.fighter_tube_rearm_until, &db.fighter_tube_rearm_until, &ram.fighter_tube_rearm_until),
        behavior_debugging_active: merge_set_3way(&base.behavior_debugging_active, &db.behavior_debugging_active, &ram.behavior_debugging_active),
        authorized_shipping_storage: merge_pair_set_vec_3way(&base.authorized_shipping_storage, &db.authorized_shipping_storage, &ram.authorized_shipping_storage),
        active_drone_miners: merge_map_3way(&base.active_drone_miners, &db.active_drone_miners, &ram.active_drone_miners),
        ess_main_bank_thefts: merge_map_3way(&base.ess_main_bank_thefts, &db.ess_main_bank_thefts, &ram.ess_main_bank_thefts),
        ess_reserve_bank_thefts: merge_map_3way(&base.ess_reserve_bank_thefts, &db.ess_reserve_bank_thefts, &ram.ess_reserve_bank_thefts),
        ess_reserve_unlocked_until: merge_map_3way(&base.ess_reserve_unlocked_until, &db.ess_reserve_unlocked_until, &ram.ess_reserve_unlocked_until),
        clone_soldier_cooldown_until: merge_map_3way(&base.clone_soldier_cooldown_until, &db.clone_soldier_cooldown_until, &ram.clone_soldier_cooldown_until),
        bounty_credited_balls: merge_set_3way(&base.bounty_credited_balls, &db.bounty_credited_balls, &ram.bounty_credited_balls),
        destroyed_npc_cache: merge_map_3way(&base.destroyed_npc_cache, &db.destroyed_npc_cache, &ram.destroyed_npc_cache),
        ci_sentry_deployed_systems: merge_set_3way(&base.ci_sentry_deployed_systems, &db.ci_sentry_deployed_systems, &ram.ci_sentry_deployed_systems),
        npc_scrambled_ships: merge_map_3way(&base.npc_scrambled_ships, &db.npc_scrambled_ships, &ram.npc_scrambled_ships),
        npc_sec_gain_intervals: merge_pair_vec_3way(&base.npc_sec_gain_intervals, &db.npc_sec_gain_intervals, &ram.npc_sec_gain_intervals),
        recently_destroyed_item_ids: merge_recent_ids_3way(&base.recently_destroyed_item_ids, &db.recently_destroyed_item_ids, &ram.recently_destroyed_item_ids),
        self_destruct_sim_remaining_secs,
    }
}

fn merge_persisted_3way(
    base: &eve_db::PersistedCombatCrimewatchState,
    db: &eve_db::PersistedCombatCrimewatchState,
    ram: &eve_db::PersistedCombatCrimewatchState,
) -> eve_db::PersistedCombatCrimewatchState {
    let pending_bounty_ticks = merge_map_3way(&base.pending_bounty_ticks, &db.pending_bounty_ticks, &ram.pending_bounty_ticks);
    let bounty_tick_next_flush_unix = merge_map_3way(&base.bounty_tick_next_flush_unix, &db.bounty_tick_next_flush_unix, &ram.bounty_tick_next_flush_unix);
    let pending_bounty_tick_started_unix = merge_map_3way(&base.pending_bounty_tick_started_unix, &db.pending_bounty_tick_started_unix, &ram.pending_bounty_tick_started_unix);
    let bounty_maps_differ_from_ram = pending_bounty_ticks != ram.pending_bounty_ticks
        || bounty_tick_next_flush_unix != ram.bounty_tick_next_flush_unix
        || pending_bounty_tick_started_unix != ram.pending_bounty_tick_started_unix;

    let extended_runtime_json = if db.extended_runtime_json == base.extended_runtime_json
        && (!bounty_maps_differ_from_ram || ram.extended_runtime_json.is_empty())
    {
        ram.extended_runtime_json.clone()
    } else if ram.extended_runtime_json == base.extended_runtime_json && !bounty_maps_differ_from_ram {
        db.extended_runtime_json.clone()
    } else {
        let parse_ext = |s: &str| -> PersistedCombatExtendedPayload {
            if s.is_empty() {
                PersistedCombatExtendedPayload::default()
            } else {
                serde_json::from_str(s).unwrap_or_default()
            }
        };
        let mut merged_ext = merge_extended_payload_3way(
            &parse_ext(&base.extended_runtime_json),
            &parse_ext(&db.extended_runtime_json),
            &parse_ext(&ram.extended_runtime_json),
        );
        merged_ext.pending_bounty_ticks = pending_bounty_ticks.iter().map(|(k, v)| (*k, *v)).collect();
        merged_ext.bounty_tick_next_flush_unix = bounty_tick_next_flush_unix.iter().map(|(k, v)| (*k, *v)).collect();
        merged_ext.pending_bounty_tick_started_unix = pending_bounty_tick_started_unix.iter().map(|(k, v)| (*k, *v)).collect();
        merged_ext.pending_bounty_ticks.sort_by_key(|(k, _)| *k);
        merged_ext.bounty_tick_next_flush_unix.sort_by_key(|(k, _)| *k);
        merged_ext.pending_bounty_tick_started_unix.sort_by_key(|(k, _)| *k);
        if merged_ext == PersistedCombatExtendedPayload::default() {
            String::new()
        } else {
            serde_json::to_string(&merged_ext).unwrap_or_default()
        }
    };

    eve_db::PersistedCombatCrimewatchState {
        sec_status_transactions: merge_map_3way(&base.sec_status_transactions, &db.sec_status_transactions, &ram.sec_status_transactions),
        safety_levels: merge_map_3way(&base.safety_levels, &db.safety_levels, &ram.safety_levels),
        drone_settings: merge_map_3way(&base.drone_settings, &db.drone_settings, &ram.drone_settings),
        limited_engagements: merge_map_3way(&base.limited_engagements, &db.limited_engagements, &ram.limited_engagements),
        npc_timers: merge_map_3way(&base.npc_timers, &db.npc_timers, &ram.npc_timers),
        disapproval_flags: merge_map_3way(&base.disapproval_flags, &db.disapproval_flags, &ram.disapproval_flags),
        duel_challenges: merge_map_3way(&base.duel_challenges, &db.duel_challenges, &ram.duel_challenges),
        corp_aggression_settings: merge_map_3way(&base.corp_aggression_settings, &db.corp_aggression_settings, &ram.corp_aggression_settings),
        player_bounty_pool: merge_map_3way(&base.player_bounty_pool, &db.player_bounty_pool, &ram.player_bounty_pool),
        bounty_contributions: merge_map_3way(&base.bounty_contributions, &db.bounty_contributions, &ram.bounty_contributions),
        bounty_hunter_claimed: merge_map_3way(&base.bounty_hunter_claimed, &db.bounty_hunter_claimed, &ram.bounty_hunter_claimed),
        kill_right_expires_unix: merge_map_3way(&base.kill_right_expires_unix, &db.kill_right_expires_unix, &ram.kill_right_expires_unix),
        kill_right_restricted_to: merge_map_3way(&base.kill_right_restricted_to, &db.kill_right_restricted_to, &ram.kill_right_restricted_to),
        detailed_killmails: merge_map_3way(&base.detailed_killmails, &db.detailed_killmails, &ram.detailed_killmails),
        pending_bounty_ticks,
        bounty_tick_next_flush_unix,
        pending_bounty_tick_started_unix,
        extended_runtime_json,
    }
}

/// Rehydrates [`CombatRuntimeState`] from `ctx.db` whenever `combat_crimewatch_state` changed
/// (e.g. on startup, database reload, or snapshot replacement) (`#592`, `#1238`).
pub fn sync_combat_runtime_from_db(ctx: &ServerContext) {
    let combat_epoch = ctx.db.combat_state_epoch();
    let runtime_epoch = ctx.db.combat_runtime_epoch();
    {
        let guard_read = ctx
            .combat_ephemeral
            .read()
            .unwrap_or_else(|e| e.into_inner());
        if guard_read.is_synced_with_db(combat_epoch, runtime_epoch) {
            return;
        }
    }
    rehydrate_combat_runtime_with_mode(ctx, false);
}

/// Initializes a fresh [`CombatRuntimeState`] aligned atomically with `db`'s current epochs (`#1238`).
pub fn init_combat_runtime_state(db: &eve_db::WorldRepository) -> CombatRuntimeState {
    loop {
        let cw_before = db.combat_state_epoch();
        let rt_before = db.combat_runtime_epoch();
        let persisted_arc = db.with_snapshot(|snap| {
            std::sync::Arc::clone(snap.combat_crimewatch_state.as_arc())
        });
        if db.combat_state_epoch() == cw_before && db.combat_runtime_epoch() == rt_before {
            let mut rt = CombatRuntimeState::default();
            rt.sync_from_persisted(&persisted_arc);
            rt.mark_synced_arc(&persisted_arc, cw_before, rt_before);
            return rt;
        }
    }
}

/// Unconditionally rehydrates [`CombatRuntimeState`] from the currently published `ctx.db` snapshot.
pub fn rehydrate_combat_runtime_from_db_forced(ctx: &ServerContext) {
    rehydrate_combat_runtime_with_mode(ctx, true);
}

fn rehydrate_combat_runtime_with_mode(ctx: &ServerContext, force_full: bool) {
    rehydrate_combat_runtime_with_hook(ctx, force_full, || {});
}

pub(super) fn rehydrate_combat_runtime_with_hook(
    ctx: &ServerContext,
    force_full: bool,
    after_snapshot_hook: impl FnMut(),
) {
    rehydrate_combat_runtime_locks_with_hook(&ctx.db, &*ctx.combat_ephemeral, force_full, after_snapshot_hook);
}

pub(crate) fn rehydrate_combat_runtime_locks_with_hook(
    db: &eve_db::WorldRepository,
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<CombatRuntimeState>,
    force_full: bool,
    mut after_snapshot_hook: impl FnMut(),
) {
    loop {
        let cw_before = db.combat_state_epoch();
        let rt_before = db.combat_runtime_epoch();
        let persisted_arc = db.with_snapshot(|s| std::sync::Arc::clone(s.combat_crimewatch_state.as_arc()));
        after_snapshot_hook();
        if db.combat_state_epoch() != cw_before || db.combat_runtime_epoch() != rt_before {
            continue;
        }
        let prune_snap = {
            let mut guard = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
            if db.combat_state_epoch() != cw_before || db.combat_runtime_epoch() != rt_before {
                continue;
            }
            if force_full || !guard.is_synced_with_db(cw_before, rt_before) {
                guard.reconcile_with_db_persisted(&persisted_arc, cw_before, rt_before, force_full);
                Some(crate::combat::npc_ai_weapons_loot::capture_wrt_prune(&guard.extended_npc_weapon_runtime, &guard.npc_entities))
            } else { None }
        };
        if let Some(snap) = prune_snap { crate::combat::npc_ai_weapons_loot::prune_wrt_snap(snap); }
        break;
    }
}

/// Persists the current in-memory [`CombatRuntimeState`] mirror into `ctx.db` (`#592`).
pub fn flush_combat_runtime_to_db(ctx: &ServerContext) {
    flush_combat_runtime_to_db_with_hooks(ctx, || {}, || {});
}

pub(super) fn flush_combat_runtime_to_db_with_hooks(
    ctx: &ServerContext,
    after_read_hook: impl FnMut(),
    before_stamp_hook: impl FnMut(),
) {
    flush_combat_runtime_to_db_locks_with_hooks(
        &ctx.db,
        &*ctx.combat_ephemeral,
        after_read_hook,
        before_stamp_hook,
    );
}

pub(crate) fn flush_combat_runtime_to_db_locks_with_hooks(
    db: &eve_db::WorldRepository,
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<CombatRuntimeState>,
    mut after_read_hook: impl FnMut(),
    mut before_stamp_hook: impl FnMut(),
) {
    let mut read_hook_ran = false;
    let mut fast_clean = None;
    loop {
        let cw_before = db.combat_state_epoch();
        let rt_before = db.combat_runtime_epoch();
        let clean_base = {
            let guard = combat_ephemeral
                .read()
                .unwrap_or_else(|e| e.into_inner());
            if guard.is_synced_with_db(cw_before, rt_before) {
                guard
                    .synced_baseline
                    .as_ref()
                    .filter(|b| guard.to_persisted() == ***b)
                    .map(std::sync::Arc::clone)
            } else {
                None
            }
        };
        let Some(base_arc) = clean_base else {
            break;
        };
        let db_matches = db.with_snapshot(|s| {
            std::sync::Arc::ptr_eq(s.combat_crimewatch_state.as_arc(), &base_arc)
                || *s.combat_crimewatch_state == *base_arc
        });
        read_hook_ran = true;
        after_read_hook();
        if db.combat_state_epoch() != cw_before || db.combat_runtime_epoch() != rt_before {
            break;
        }
        if db_matches {
            fast_clean = Some((cw_before, rt_before));
            break;
        }
        break;
    }
    if let Some((cw_expected, rt_expected)) = fast_clean {
        before_stamp_hook();
        if db.combat_state_epoch() == cw_expected && db.combat_runtime_epoch() == rt_expected {
            return;
        }
        rehydrate_combat_runtime_locks_with_hook(db, combat_ephemeral, false, || {});
        return;
    }
    if !read_hook_ran {
        after_read_hook();
    }

    let (staged_next, target_cw, target_rt, pre_ram_p) = db.with_mut_snapshot(|s| {
        let pre_cw = db.combat_state_epoch();
        let pre_rt = db.combat_runtime_epoch();
        let mut guard_copy = {
            let guard = combat_ephemeral
                .write()
                .unwrap_or_else(|e| e.into_inner());
            guard.clone()
        };
        let pre_ram_p = guard_copy.to_persisted();
        guard_copy.reconcile_with_db_persisted(
            s.combat_crimewatch_state.as_arc(),
            pre_cw,
            pre_rt,
            false,
        );
        let next_p = guard_copy.to_persisted();
        let changed = *s.combat_crimewatch_state != next_p;
        let (target_cw, target_rt, next_arc) = if changed {
            let arc = std::sync::Arc::new(next_p);
            s.combat_crimewatch_state = eve_db::CowCell::from_arc(std::sync::Arc::clone(&arc));
            (pre_cw.wrapping_add(1), pre_rt.wrapping_add(1), arc)
        } else {
            (
                pre_cw,
                pre_rt,
                std::sync::Arc::clone(s.combat_crimewatch_state.as_arc()),
            )
        };
        guard_copy.mark_synced_arc(&next_arc, target_cw, target_rt);
        (guard_copy, target_cw, target_rt, pre_ram_p)
    });

    before_stamp_hook();
    if db.combat_state_epoch() == target_cw && db.combat_runtime_epoch() == target_rt {
        let mut guard = combat_ephemeral
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if db.combat_state_epoch() == target_cw
            && db.combat_runtime_epoch() == target_rt
            && guard.to_persisted() == pre_ram_p
        {
            *guard = staged_next;
            return;
        }
    }
    rehydrate_combat_runtime_locks_with_hook(db, combat_ephemeral, false, || {});
}

pub fn with_combat_runtime<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&CombatRuntimeState) -> R,
) -> R {
    with_combat_runtime_with_locks(&ctx.db, &*ctx.combat_ephemeral, f)
}

pub fn with_combat_runtime_with_locks<R>(
    db: &eve_db::WorldRepository,
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<CombatRuntimeState>,
    f: impl FnOnce(&CombatRuntimeState) -> R,
) -> R {
    let combat_epoch = db.combat_state_epoch();
    let runtime_epoch = db.combat_runtime_epoch();
    {
        let guard = combat_ephemeral
            .read()
            .unwrap_or_else(|e| e.into_inner());
        if guard.is_synced_with_db(combat_epoch, runtime_epoch) {
            return f(&guard);
        }
    }
    rehydrate_combat_runtime_locks_with_hook(db, combat_ephemeral, false, || {});
    let guard = combat_ephemeral
        .read()
        .unwrap_or_else(|e| e.into_inner());
    f(&guard)
}

/// Mutates persisted combat crimewatch state directly inside an open DB snapshot transaction
/// (`#1238` EP-12), keeping `ctx.combat_ephemeral` untouched until post-commit rehydration so
/// aborted or rolled-back transactions never leak partial mutations into runtime memory.
pub fn with_mut_combat_runtime_in_snap<R>(
    snap: &mut eve_db::WorldStateSnapshot,
    f: impl FnOnce(&mut CombatRuntimeState) -> R,
) -> R {
    let mut rt = CombatRuntimeState::default();
    rt.sync_from_persisted(&snap.combat_crimewatch_state);
    let out = f(&mut rt);
    snap.combat_crimewatch_state = rt.to_persisted().into();
    out
}

pub fn with_mut_combat_runtime<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut CombatRuntimeState) -> R,
) -> R {
    with_mut_combat_runtime_with_locks(&ctx.db, &*ctx.combat_ephemeral, f)
}

pub fn with_mut_combat_runtime_with_locks<R>(
    db: &eve_db::WorldRepository,
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<CombatRuntimeState>,
    f: impl FnOnce(&mut CombatRuntimeState) -> R,
) -> R {
    with_mut_combat_runtime_tx_locks(db, combat_ephemeral, |_, rt| f(rt))
}

pub fn with_mut_combat_runtime_tx_locks<R>(
    db: &eve_db::WorldRepository,
    combat_ephemeral: &dyn eve_db::lock_rank::AnyRwLock<CombatRuntimeState>,
    f: impl FnOnce(&mut eve_db::WorldStateSnapshot, &mut CombatRuntimeState) -> R,
) -> R {
    let (out, staged_next, expected_rt, pre_ram_p) = db.with_mut_snapshot(|s| {
        let pre_cw = db.combat_state_epoch();
        let pre_rt = db.combat_runtime_epoch();
        let mut guard_copy = {
            let guard = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
            guard.clone()
        };
        let pre_ram_p = guard_copy.to_persisted();
        guard_copy.reconcile_with_db_persisted(s.combat_crimewatch_state.as_arc(), pre_cw, pre_rt, false);
        let out = f(s, &mut guard_copy);
        let persisted = guard_copy.to_persisted();
        let changed = *s.combat_crimewatch_state != persisted;
        let expected_cw = if changed { pre_cw.wrapping_add(1) } else { pre_cw };
        let (expected_rt, next_arc) = if changed {
            let arc = std::sync::Arc::new(persisted);
            s.combat_crimewatch_state = eve_db::CowCell::from_arc(std::sync::Arc::clone(&arc));
            (pre_rt.wrapping_add(1), arc)
        } else {
            (pre_rt, std::sync::Arc::clone(s.combat_crimewatch_state.as_arc()))
        };
        guard_copy.mark_synced_arc(&next_arc, expected_cw, expected_rt);
        (out, guard_copy, expected_rt, pre_ram_p)
    });
    if db.combat_runtime_epoch() == expected_rt {
        let maybe_prune = {
            let mut guard = combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
            if db.combat_runtime_epoch() == expected_rt && guard.to_persisted() == pre_ram_p {
                let mut next = staged_next;
                next.synced_db_epoch = db.combat_state_epoch();
                let snap = crate::combat::npc_ai_weapons_loot::capture_wrt_prune_if_changed(guard.synced_db_epoch == 0, &guard.npc_entities, &next.npc_entities, &next.extended_npc_weapon_runtime);
                *guard = next;
                Some(snap)
            } else { None }
        };
        if let Some(snap_opt) = maybe_prune {
            if let Some(snap) = snap_opt { crate::combat::npc_ai_weapons_loot::prune_wrt_snap(snap); }
            return out;
        }
    }
    rehydrate_combat_runtime_locks_with_hook(db, combat_ephemeral, false, || {});
    out
}

pub fn with_combat_ephemeral<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&CombatRuntimeState) -> R,
) -> R {
    with_combat_runtime(ctx, f)
}

pub fn with_mut_combat_ephemeral<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut CombatRuntimeState) -> R,
) -> R {
    with_mut_combat_runtime(ctx, f)
}

/// Evaluates the effective signature radius of a ship in space (`feat:dogma.signature-radius`),
/// combining base hull signature radius (`attr 552`), active Microwarpdrive bloom (`attr 554`),
/// shield extender bonuses, and projected Target Painter effects (`group_id == 379`).
pub async fn evaluate_effective_signature_radius(
    ctx: &ServerContext,
    char_id: i64,
    ship_id: i64,
    solar_system_id: u32,
) -> f64 {
    let tp_mult = ctx.effective_target_painter_multiplier(ship_id);
    let base_sig = if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
        let eval_ctx = fit.evaluated_context(&ctx.sde);
        let sig = eval_ctx.ship.get_attribute(eve_sde::attr_ids::SIGNATURE_RADIUS, &ctx.sde);
        let fitted_sig = if sig > 0.0 {
            sig
        } else {
            fit.ship
                .get_attribute(eve_sde::attr_ids::SIGNATURE_RADIUS, &ctx.sde)
                .max(35.0)
        };
        if tp_mult > 1.0 + 1e-6 {
            fitted_sig / tp_mult
        } else {
            fitted_sig
        }
    } else {
        ctx.db
            .with_snapshot(|snap| {
                snap.items
                    .get(&ship_id)
                    .and_then(|it| ctx.sde.types.get(&it.type_id))
                    .and_then(|t| {
                        t.default_attributes
                            .get(&eve_sde::attr_ids::SIGNATURE_RADIUS)
                            .copied()
                    })
            })
            .unwrap_or(35.0)
    };

    let mwd_bloom_mult = ctx.db.with_snapshot(|snap| {
        let mut mult = 1.0;
        for it in snap.items.values() {
            if it.location_id == ship_id && snap.module_active_states.contains(&it.item_id) {
                if let Some(t) = ctx.sde.types.get(&it.type_id) {
                    if t.group_id == 46 {
                        let bonus = t.default_attributes.get(&554).copied().unwrap_or(0.0);
                        if bonus > 0.0 {
                            mult *= 1.0 + bonus / 100.0;
                        }
                    }
                }
            }
        }
        mult
    });
    let unbloomed_sig = base_sig;
    let bloom_mult = if mwd_bloom_mult > 1.0 && unbloomed_sig < 150.0 {
        mwd_bloom_mult
    } else {
        1.0
    };
    let effective_sig = unbloomed_sig * bloom_mult * tp_mult;
    let tp_sig_mult = (tp_mult > 1.0 + 1e-6).then_some(tp_mult);
    let ext_sig_mult = (bloom_mult > 1.0).then_some(bloom_mult);

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ship_id).await {
        if let Ok(Some(cs)) = sys_handle.get_combat_state(ship_id).await {
            let cs_mismatch = (cs.tp_sig_multiplier.unwrap_or(1.0) - tp_mult).abs() > 1e-6
                || (cs.external_sig_multiplier.unwrap_or(1.0) - bloom_mult).abs() > 1e-6
                || cs
                    .base_signature_radius
                    .is_some_and(|b| (b - unbloomed_sig).abs() > 1e-3);
            if (ball.radius - effective_sig).abs() > 1e-3 || cs_mismatch {
                let _ = sys_handle
                    .modify_combat_state(ship_id, move |live_cs| {
                        live_cs.base_signature_radius = Some(unbloomed_sig);
                        live_cs.tp_sig_multiplier = tp_sig_mult;
                        live_cs.external_sig_multiplier = ext_sig_mult;
                        live_cs.signature_radius = effective_sig;
                    })
                    .await;
            }
        }
    }
    effective_sig
}

/// Computes combat probe scan signal strength (`0.0..=1.0`) as a function of target signature radius,
/// probe sensor strength, and scan range (`feat:dogma.signature-radius`).
pub fn compute_probe_scan_signal_strength(
    probe_sensor_strength: f64,
    target_signature_radius: f64,
    scan_range_au: f64,
) -> f64 {
    let base_range_factor = (0.25 / scan_range_au.max(0.25)).clamp(0.01, 4.0);
    let sig_factor = (target_signature_radius.max(1.0) / 40.0).clamp(0.05, 20.0);
    ((probe_sensor_strength / 40.0) * sig_factor * base_range_factor).clamp(0.0, 1.0)
}

/// Steps passive shield recharge on `ball_id` over `dt` seconds (`feat:dogma.shield-recharge`),
/// peaking at ~25% shield capacity per EVE's non-linear recharge differential equation,
/// and emits `OnDamageStateChange`.
pub async fn step_passive_shield_recharge(
    ctx: &ServerContext,
    solar_system_id: u32,
    char_id_opt: Option<i64>,
    ball_id: i64,
    recharge_time_s: f64,
    dt: f64,
) -> Option<(f64, f64)> {
    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    let mut cs = sys_handle.get_combat_state(ball_id).await.ok().flatten()?;
    if cs.max_shield_hp <= 0.0 {
        return None;
    }
    let before = cs.shield_hp;
    let seed_hp = if before <= 0.0 && dt > 0.0 {
        (cs.max_shield_hp * 1e-4).max(0.01)
    } else {
        before
    };
    let rate = eve_dogma::compute_passive_recharge_rate(
        seed_hp,
        cs.max_shield_hp,
        recharge_time_s.max(1.0),
    );
    let after = (seed_hp + rate * dt).min(cs.max_shield_hp);
    let delta = (after - before).max(0.0);
    cs.shield_hp = after;
    let shield_ratio = (cs.shield_hp / cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0);
    let armor_ratio = (cs.armor_hp / cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0);
    let hull_ratio = (cs.structure_hp / cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0);
    let _ = sys_handle
        .modify_combat_state(ball_id, move |live_cs| {
            live_cs.shield_hp = after;
        })
        .await;
    if delta > 0.0 {
        if let Some(cid) = char_id_opt {
            let (in_combat_space, hit_piloted_ship) = ctx.db.with_snapshot(|snap| {
                let in_sys = ctx.space_char_sys_in_snapshot(snap, cid) == Some(solar_system_id);
                let piloted = in_sys
                    && snap
                        .characters
                        .get(&cid)
                        .is_some_and(|ch| ball_id == ch.active_ship_id || ball_id == ch.character_id);
                (in_sys, piloted)
            });
            if in_combat_space {
                if let Some(scene) =
                    crate::router::notify_registry::BallparkScene::try_from_outbound_scope(
                        ctx.character_space_scene_scope(cid, Some(solar_system_id)),
                        solar_system_id,
                    )
                {
                    crate::services::notify_emitters_combat_ess_progression::emit_damage_state_change_in_scene(
                        ctx,
                        cid,
                        ball_id,
                        shield_ratio,
                        armor_ratio,
                        hull_ratio,
                        scene,
                    );
                }
                if hit_piloted_ship {
                    crate::services::fleet_social::stream_fleet_watchlist_damage(
                        ctx,
                        cid,
                        shield_ratio,
                        armor_ratio,
                        hull_ratio,
                    );
                }
            }
        }
    }
    Some((after, delta))
}

/// Instantaneous passive shield recharge rate in HP/s at `current_shield` out of `max_shield`
/// with time constant `recharge_time_s` (`feat:dogma.shield-recharge`).
pub fn passive_shield_recharge_rate_hps(current_shield: f64, max_shield: f64, recharge_time_s: f64) -> f64 {
    eve_dogma::compute_passive_recharge_rate(current_shield, max_shield, recharge_time_s)
}

pub fn is_legal_war_or_suspect_target(
    snap: &eve_db::WorldStateSnapshot,
    attacker_char_id: i64,
    victim_char_id: i64,
    now_unix: u64,
) -> bool {
    if let Some(vic) = snap.characters.get(&victim_char_id) {
        if vic.security_status <= -4.95 {
            return true;
        }
    }
    let (Some(att), Some(vic)) = (
        snap.characters.get(&attacker_char_id),
        snap.characters.get(&victim_char_id),
    ) else {
        return false;
    };
    if let (Some(af), Some(vf)) = (att.war_faction_id, vic.war_faction_id) {
        if af != vf && (snap.fw_wars.iter().any(|&(f1, f2)| (f1 == af && f2 == vf) || (f1 == vf && f2 == af))
            || crate::services::fw_services::fw_opposing_faction(af) == Some(vf)) {
            return true;
        }
    }
    crate::domain::wars::active_war_between_with_hints(
        snap,
        att.corporation_id,
        att.alliance_id,
        vic.corporation_id,
        vic.alliance_id,
        now_unix,
    )
}

pub async fn handle(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Option<Result<PyRep, BridgeRpcError>> {
    if (service, method) == ("dynamicBountyMgr", "GetOutputForClientSolarSystem") {
        // `dynamicResourceSvc.GetDynamicBountySettingForCurrentSystem`
        // (`eve/client/script/ui/shared/dynamicResource/dynamicResourceSvc.pyj:105-115`)
        // unpacks `output, dbsIsActive = sm.RemoteSvc('dynamicBountyMgr').GetOutputForClientSolarSystem()`.
        let _ = match ServerContext::require_character(session) {
            Ok(v) => v,
            Err(e) => return Some(Err(e)),
        };
        let Some(sys_id) = session.solar_system_id else {
            return Some(Err(BridgeRpcError {
                code: "NoSolarSystem".into(),
                message: "Session has no active solar system".into(),
                client_error: None,
            }));
        };
        let sec = ctx
            .sde
            .solar_systems
            .get(&sys_id)
            .map(|s| s.security_status)
            .unwrap_or(1.0);
        let dbs_active = sec <= 0.0 && sys_id < 31_000_000;
        let output = ctx.db.with_snapshot(|snap| {
            snap.system_states
                .get(&sys_id)
                .map(|s| s.dbs_multiplier)
                .unwrap_or(if dbs_active { 1.35 } else { 1.0 })
        });
        return Some(Ok(PyRep::Tuple(vec![
            PyRep::Real(output),
            PyRep::Bool(dbs_active),
        ])));
    }
    if let Some(res) = handle_legacy_and_ess(ctx, session, service, method, args, kwargs).await {
        return Some(res);
    }
    if let Some(res) = handle_fighters(ctx, session, service, method, args, kwargs).await {
        return Some(res);
    }
    if let Some(res) = handle_weapons(ctx, session, service, method, args, kwargs).await {
        return Some(res);
    }
    if let Some(res) = handle_wrecks_killmails(ctx, session, service, method, args, kwargs).await {
        return Some(res);
    }
    if let Some(res) = handle_crimewatch(ctx, session, service, method, args, kwargs).await {
        return Some(res);
    }
    if let Some(res) = handle_npcs_drones(ctx, session, service, method, args, kwargs).await {
        return Some(res);
    }
    None
}

pub(crate) fn extract_id_collection(rep: Option<&PyRep>) -> Vec<i64> {
    let Some(rep) = rep else {
        return Vec::new();
    };
    match rep {
        PyRep::List(items) | PyRep::Tuple(items) | PyRep::PySet(items) | PyRep::Set(items) => {
            items.iter().filter_map(|v| v.as_i64()).collect()
        }
        PyRep::ObjectEx { header, list, .. } => {
            let mut out: Vec<i64> = list.iter().filter_map(|v| v.as_i64()).collect();
            if let PyRep::Tuple(h) = header.as_ref() {
                if let Some(PyRep::Tuple(ctor_args)) = h.get(1) {
                    if let Some(
                        PyRep::List(items)
                        | PyRep::Tuple(items)
                        | PyRep::PySet(items)
                        | PyRep::Set(items),
                    ) = ctor_args.first()
                    {
                        out.extend(items.iter().filter_map(|v| v.as_i64()));
                    }
                }
            }
            out
        }
        _ => Vec::new(),
    }
}

pub(crate) fn handle_get_bounties_and_kill_rights(
    ctx: &ServerContext,
    session: &SessionState,
    args: &[PyRep],
) -> Result<PyRep, BridgeRpcError> {
    let (_, char_id) = ServerContext::require_character(session)?;
    let bounty_ids = extract_id_collection(args.first());
    let target_ids = extract_id_collection(args.get(1));
    let now_unix = ctx.now_unix();
    let bounties = with_combat_runtime(ctx, |rt| {
        let mut list = Vec::new();
        if !bounty_ids.is_empty() {
            for &tid in &bounty_ids {
                let amt = rt.player_bounty_pool.get(&tid).copied().unwrap_or(0.0);
                list.push(PyRep::Tuple(vec![
                    PyRep::Integer(tid),
                    PyRep::keyval([
                        ("targetID", PyRep::Integer(tid)),
                        ("bountyISK", PyRep::Real(amt)),
                        ("bounty", PyRep::Real(amt)),
                    ]),
                ]));
            }
        } else {
            for (&tid, &amt) in &rt.player_bounty_pool {
                if amt > 0.0 {
                    list.push(PyRep::Tuple(vec![
                        PyRep::Integer(tid),
                        PyRep::keyval([
                            ("targetID", PyRep::Integer(tid)),
                            ("bountyISK", PyRep::Real(amt)),
                            ("bounty", PyRep::Real(amt)),
                        ]),
                    ]));
                }
            }
        }
        list
    });
    let kill_rights = ctx.db.with_snapshot(|snap| {
        let rt = &snap.combat_crimewatch_state;
        snap.kill_rights
            .values()
            .filter(|kr| {
                let exp = rt.kill_right_expires_unix.get(&kr.kill_right_id).copied().unwrap_or(0);
                if exp > 0 && now_unix >= exp {
                    return false;
                }
                if target_ids.is_empty() {
                    kr.owner_id == char_id || kr.target_id == char_id
                } else {
                    target_ids.contains(&kr.target_id)
                }
            })
            .map(|kr| {
                let exp = rt.kill_right_expires_unix.get(&kr.kill_right_id).copied().unwrap_or(0);
                let restr_rep = match rt.kill_right_restricted_to.get(&kr.kill_right_id).copied() {
                    Some(id) if id > 0 => PyRep::Integer(id),
                    _ => PyRep::None,
                };
                PyRep::keyval([
                    ("killRightID", PyRep::Integer(kr.kill_right_id)), ("fromID", PyRep::Integer(kr.owner_id)),
                    ("toID", PyRep::Integer(kr.target_id)), ("price", PyRep::Real(kr.activation_fee.to_wire_f64())),
                    ("activeSuspect", PyRep::Bool(kr.is_active_suspect)), ("expiryTime", PyRep::Integer(exp as i64)),
                    ("restrictedTo", restr_rep),
                ])
            }).collect::<Vec<_>>()
    });
    Ok(PyRep::Tuple(vec![PyRep::List(bounties), PyRep::List(kill_rights)]))
}

/// Dispatches a combat/crimewatch RPC directly to `combat_crimewatch::handle` (or falls back to `ctx.dispatch_rpc`)
/// and flushes any newly queued character notifications onto `session`.
pub async fn dispatch_combat_rpc(
    ctx: &ServerContext,
    session: &mut SessionState,
    service: &str,
    method: &str,
    args: &[PyRep],
    kwargs: &HashMap<String, PyRep>,
) -> Result<PyRep, BridgeRpcError> {
    let pre_seq = session
        .character_id
        .map(|cid| ctx.latest_notification_seq_for_char(cid))
        .unwrap_or(0);

    if let Some(res) = handle(ctx, session, service, method, args, kwargs).await {
        if !session.live_notify_listener {
            if let Some(cid) = session.character_id {
                if let Some(notifs) = ctx.pending_notifications.read().ok().and_then(|q| q.get(&cid).cloned()) {
                    for notif in notifs.into_iter().filter(|n| n.seq_id > pre_seq) {
                        crate::router::notify_registry::push_pending_session_notification(
                            session,
                            notif,
                        );
                    }
                }
            }
        }
        return res;
    }

    ctx.dispatch_rpc(session, service, None, method, args, kwargs, None)
        .await
}

/// Claims up to 20% of the destroyed ship's reference value from the victim's (and corp/alliance)
/// Claims up to 20% of `victim_ship_type_id`'s insurance reference value directly inside an open
/// DB snapshot transaction (`#1238` EP-12).
pub fn claim_player_bounty_on_kill_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    sde: &eve_sde::SdeRegistry,
    killer_char_id: i64,
    victim_char_id: i64,
    victim_ship_type_id: u32,
) -> (f64, Option<f64>) {
    if killer_char_id <= 0 || victim_char_id <= 0 || killer_char_id == victim_char_id {
        return (0.0, None);
    }
    let vic_corp = snap
        .characters
        .get(&victim_char_id)
        .map(|c| c.corporation_id)
        .unwrap_or(0);
    let vic_all = snap
        .corporations
        .get(&vic_corp)
        .and_then(|co| co.alliance_id)
        .or_else(|| snap.characters.get(&victim_char_id).and_then(|c| c.alliance_id))
        .unwrap_or(0);
    let kil_corp = snap
        .characters
        .get(&killer_char_id)
        .map(|c| c.corporation_id)
        .unwrap_or(0);
    let kil_all = snap
        .corporations
        .get(&kil_corp)
        .and_then(|co| co.alliance_id)
        .or_else(|| snap.characters.get(&killer_char_id).and_then(|c| c.alliance_id))
        .unwrap_or(0);
    let ref_val = compute_ship_insurance_reference_value(sde, victim_ship_type_id).max(250_000.0);
    let max_claim = 0.20 * ref_val;
    let mut remaining_cap = max_claim;
    let mut claimed = 0.0;
    let rt = &mut snap.combat_crimewatch_state;
    for tid in [victim_char_id, vic_corp, vic_all] {
        if tid <= 0 || remaining_cap <= 0.0 {
            continue;
        }
        if let Some(pool) = rt.player_bounty_pool.get_mut(&tid) {
            let take = (*pool).min(remaining_cap);
            if take > 0.0 {
                *pool -= take;
                remaining_cap -= take;
                claimed += take;
            }
            if *pool <= 1e-6 {
                rt.player_bounty_pool.remove(&tid);
            }
        }
    }
    if claimed > 0.0 {
        *rt.bounty_hunter_claimed.entry(killer_char_id).or_insert(0.0) += claimed;
        if kil_corp > 0 {
            *rt.bounty_hunter_claimed.entry(kil_corp).or_insert(0.0) += claimed;
        }
        if kil_all > 0 {
            *rt.bounty_hunter_claimed.entry(kil_all).or_insert(0.0) += claimed;
        }
    }
    let bal = if claimed > 0.0 {
        if let Some(ch) = snap.characters.get_mut(&killer_char_id) {
            let add = IskCents::from_f64_isk(claimed);
            ch.balance = ch.balance.checked_add(add).unwrap_or(ch.balance);
            Some(ch.balance.to_wire_f64())
        } else {
            None
        }
    } else {
        None
    };
    (claimed, bal)
}

/// bounty pool and credits the final-blow killer (`feat:combat.player-bounties`, `feat:combat.bounty-leaderboards`).
pub fn claim_player_bounty_on_kill(
    ctx: &ServerContext,
    killer_char_id: i64,
    victim_char_id: i64,
    victim_ship_type_id: u32,
) -> f64 {
    if killer_char_id <= 0 || victim_char_id <= 0 || killer_char_id == victim_char_id {
        return 0.0;
    }
    let (claimed_isk, _new_bal) = ctx.db.with_mut_snapshot(|snap| {
        claim_player_bounty_on_kill_in_snap(snap, &ctx.sde, killer_char_id, victim_char_id, victim_ship_type_id)
    });
    if claimed_isk > 0.0 {
        sync_combat_runtime_from_db(ctx);
        let now_ft = ctx.now_blueos_filetime_ticks();
        ctx.emit_typed(
            NotificationTarget::Character(killer_char_id),
            crate::router::notify_registry::OnBountyAddedToPayout {
                data_dict: PyRep::Dict(vec![
                    (PyRep::String("amount".into()), PyRep::Real(claimed_isk)),
                    (PyRep::String("payoutTime".into()), PyRep::Integer(now_ft)),
                    (PyRep::String("enemyTypeID".into()), PyRep::Integer(i64::from(victim_ship_type_id))),
                    (PyRep::String("isModified".into()), PyRep::Bool(false)),
                ]),
            },
        );
    }
    claimed_isk
}

/// GM/Admin bounty manipulation tool (`set`, `clear`, `reimburse`) (`feat:admin.gm-bounty-tools`).
pub fn gm_admin_bounty_action(
    ctx: &ServerContext,
    session: &SessionState,
    action: &str,
    target_id: i64,
    amount_isk: f64,
) -> Result<f64, BridgeRpcError> {
    if !session.has_gm_role() && (session.role & eve_machonet::service_roles::ROLE_QA) == 0 {
        return Err(BridgeRpcError {
            code: "InsufficientRole".into(),
            message: "GM bounty tools require a GM/Admin/QA role".into(),
            client_error: None,
        });
    }
    match action {
        "set" => {
            if !amount_isk.is_finite() || amount_isk < 0.0 {
                return Err(BridgeRpcError {
                    code: "InvalidBountyAmount".into(),
                    message: "Bounty amount must be non-negative finite number".into(),
                    client_error: None,
                });
            }
            let val = amount_isk;
            with_mut_combat_runtime(ctx, |rt| {
                if val > 0.0 {
                    rt.player_bounty_pool.insert(target_id, val);
                } else {
                    rt.player_bounty_pool.remove(&target_id);
                }
            });
            Ok(val)
        }
        "clear" => {
            let prev = with_mut_combat_runtime(ctx, |rt| {
                if target_id > 0 {
                    rt.bounty_contributions.remove(&target_id);
                    rt.player_bounty_pool.remove(&target_id).unwrap_or(0.0)
                } else {
                    let sum: f64 = rt.player_bounty_pool.values().sum();
                    rt.player_bounty_pool.clear();
                    rt.bounty_contributions.clear();
                    sum
                }
            });
            Ok(prev)
        }
        "reimburse" => {
            // Atomize GM bounty pool clearing and contributor wallet refunds under a single database
            // snapshot commit so a crash or persistence failure never leaves money movement half-applied.
            let total_reimbursed = ctx.db.with_mut_snapshot(|snap| {
                gm_admin_bounty_reimburse_in_snap(snap, target_id)
            });
            sync_combat_runtime_from_db(ctx);
            Ok(total_reimbursed)
        }
        _ => Err(BridgeRpcError {
            code: "InvalidGmBountyAction".into(),
            message: format!("Unknown GM bounty action '{action}'"),
            client_error: None,
        }),
    }
}

/// Atomically reimburses `target_id` (or all targets when `target_id <= 0`) pro-rata directly
/// inside an open DB snapshot transaction (`#1238` EP-12).
pub fn gm_admin_bounty_reimburse_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    target_id: i64,
) -> f64 {
    let (remaining_pool, refunds) = if target_id > 0 {
        let pool = snap
            .combat_crimewatch_state
            .player_bounty_pool
            .remove(&target_id)
            .unwrap_or(0.0);
        let contribs = snap
            .combat_crimewatch_state
            .bounty_contributions
            .remove(&target_id)
            .unwrap_or_default();
        (pool, contribs)
    } else {
        let pool: f64 = snap.combat_crimewatch_state.player_bounty_pool.values().sum();
        snap.combat_crimewatch_state.player_bounty_pool.clear();
        let all = std::mem::take(&mut snap.combat_crimewatch_state.bounty_contributions);
        (pool, all.into_values().flatten().collect())
    };
    let total_contrib: f64 = refunds.iter().map(|(_, a)| *a).sum();
    let scale = if total_contrib > 0.0 && remaining_pool < total_contrib {
        (remaining_pool / total_contrib).max(0.0)
    } else {
        1.0
    };
    let mut sum = 0.0;
    for (cid, amt) in refunds {
        let refund_amt = amt * scale;
        if refund_amt > 0.0 {
            let mut entry = None;
            if let Some(ch) = snap.characters.get_mut(&cid) {
                let add = IskCents::from_f64_isk(refund_amt);
                ch.balance = ch.balance.checked_add(add).unwrap_or(ch.balance);
                entry = Some((add, ch.balance));
                sum += refund_amt;
            }
            if let Some((add, bal)) = entry {
                snap.append_journal(
                    cid,
                    "bounty_reimbursement",
                    add,
                    bal,
                    "bounty_reimbursement",
                );
            }
        }
    }
    sum
}

/// Creates a kill right directly inside an open DB snapshot transaction (`#1238` EP-12).
pub fn create_kill_right_in_snap(
    snap: &mut eve_db::WorldStateSnapshot,
    kr_id: i64,
    owner_id: i64,
    target_id: i64,
    fee_isk: f64,
    expires_unix: u64,
) {
    snap.combat_crimewatch_state
        .kill_right_expires_unix
        .insert(kr_id, expires_unix);
    snap.kill_rights.insert(
        kr_id,
        KillRightRecord {
            kill_right_id: kr_id,
            owner_id,
            target_id,
            activation_fee: IskCents::from_f64_isk(fee_isk.max(0.0)),
            is_active_suspect: false,
        },
    );
}

/// Creates a kill right from `owner_id` against `target_id` and emits `OnKillRightCreated`.
pub fn create_kill_right(ctx: &ServerContext, kr_id: i64, owner_id: i64, target_id: i64, fee_isk: f64) {
    let expiry_ft = ctx.now_blueos_filetime_ticks() + (30 * 86_400 * 10_000_000);
    let expires_unix = ctx.now_unix().saturating_add(30 * 86_400);
    ctx.db.with_mut_snapshot(|snap| {
        create_kill_right_in_snap(snap, kr_id, owner_id, target_id, fee_isk, expires_unix);
    });
    sync_combat_runtime_from_db(ctx);
    let kr_notif = crate::router::notify_registry::OnKillRightCreated {
        kill_right_id: kr_id,
        from_id: owner_id,
        to_id: target_id,
        expiry_time: expiry_ft,
    };
    ctx.emit_typed(NotificationTarget::Character(owner_id), kr_notif.clone());
    ctx.emit_typed(NotificationTarget::Character(target_id), kr_notif);
}

pub(crate) fn validate_kill_right_live_preconditions(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    kr_id: i64,
    caller_id: i64,
    target_id: i64,
) -> Result<(), BridgeRpcError> {
    let expired = match snap.combat_crimewatch_state.kill_right_expires_unix.get(&kr_id).copied() {
        Some(exp) => ctx.now_unix() >= exp,
        None => !snap.kill_rights.contains_key(&kr_id),
    };
    if expired {
        return Err(BridgeRpcError { code: "KillRightExpired".into(), message: format!("Kill right {kr_id} has expired (30-day lifetime)"), client_error: None });
    }
    let Some(caller) = snap.characters.get(&caller_id) else {
        return Err(BridgeRpcError { code: "CharacterNotFound".into(), message: format!("Character {caller_id} not found"), client_error: None });
    };
    if caller.station_id.is_some() || caller.structure_id.is_some() {
        return Err(BridgeRpcError { code: "KillRightCannotActivateDocked".into(), message: "Cannot activate or buy a kill right while docked".into(), client_error: None });
    }
    let Some(target) = snap.characters.get(&target_id) else {
        return Err(BridgeRpcError { code: "TargetNotFound".into(), message: format!("Kill right target {target_id} not found"), client_error: None });
    };
    if target.station_id.is_some() || target.structure_id.is_some() || caller.solar_system_id != target.solar_system_id {
        return Err(BridgeRpcError { code: "KillRightTargetNotInSystem".into(), message: "Target pilot must be undocked in the same solar system".into(), client_error: None });
    }
    let sys_id = caller.solar_system_id;
    let c_pos = ctx.systems.get_ball_position_sync(sys_id, caller.active_ship_id);
    let t_pos = ctx.systems.get_ball_position_sync(sys_id, target.active_ship_id);
    if let (Some(cp), Some(tp)) = (c_pos, t_pos) {
        if (cp - tp).length() > 8_000_000.0 {
            return Err(BridgeRpcError { code: "KillRightTargetNotOnGrid".into(), message: "Target pilot is not on grid with caller".into(), client_error: None });
        }
    }
    Ok(())
}

/// Emits limited engagement timeout start/stop and end notifications to `char_id`.
pub fn update_crimewatch_engagement_timers(ctx: &ServerContext, char_id: i64, other_char_id: i64) {
    let timeout_ft = ctx.now_blueos_filetime_ticks() + 3_000_000_000;
    let t = NotificationTarget::Character(char_id);
    ctx.emit_typed(t.clone(), crate::router::notify_registry::OnCrimewatchEngagementStartTimeout { other_char_id, timeout: timeout_ft });
    ctx.emit_typed(t.clone(), crate::router::notify_registry::OnCrimewatchEngagementStopTimeout { other_char_id });
    with_mut_combat_runtime(ctx, |rt| {
        if let Some(m) = rt.limited_engagements.get_mut(&char_id) { m.remove(&other_char_id); }
    });
    ctx.emit_typed(t, crate::router::notify_registry::OnCrimewatchEngagementEnded { other_char_id });
}

#[cfg(test)]
#[path = "combat_crimewatch_tests.rs"] mod combat_crimewatch_tests;
#[cfg(test)]
#[path = "combat_crimewatch_edges_tests.rs"] mod combat_crimewatch_edges_tests;

