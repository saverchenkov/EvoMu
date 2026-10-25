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
    compute_insurance_quote, compute_ship_insurance_reference_value, concord_response_delay_sec,
    entropic_spool_multiplier, missile_damage_factor, turret_chance_to_hit,
    turret_chance_to_hit_with_res, turret_shot_damage, vorton_arc_targets_with_cap,
};
use crate::router::{NotificationTarget, ServerContext};
use eve_db::{
    InventoryFlag, InventoryItemRecord, IskCents, KillRightRecord, KillmailRecord,
    SystemDynamicState, WalletJournalEntry,
};
use eve_destiny::DamageVector;
use eve_machonet::SessionState;
use eve_proto::{BridgeRpcError, PyRep};
use glam::DVec3;
use std::collections::HashMap;

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
pub use crimewatch_timers::*;

/// Wreck lifetime in seconds (2 hours = 7,200s per EVE wiki `Wrecks.md`).
pub const WRECK_DECAY_LIFETIME_SECS: u64 = 7_200;

/// Jettisoned cargo container (jetcan) lifetime in seconds (1 hour = 3,600s).
pub const JETCAN_DECAY_LIFETIME_SECS: u64 = 3_600;

/// Killmail assist window in seconds (15 minutes = 900s).
pub const KILLMAIL_ASSIST_WINDOW_SECS: u64 = 900;

/// Self-destruct countdown duration in seconds (2 minutes = 120s).
pub const SELF_DESTRUCT_COUNTDOWN_SECS: u64 = 120;

#[derive(Debug, Clone)]
pub struct EntropicSpoolState {
    pub target_id: i64,
    pub cycles: u32,
    pub elapsed_in_cycle: f64,
    pub per_cycle_bonus: f64,
    pub max_bonus: f64,
    pub current_multiplier: f64,
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct ContainerLifetimeEntry {
    pub item_id: i64,
    pub solar_system_id: u32,
    pub type_id: u32,
    pub owner_id: i64,
    pub created_unix: u64,
    pub lifetime_secs: u64,
    pub is_abandoned: bool,
}

#[derive(Debug, Clone)]
pub struct SelfDestructCountdown {
    pub ship_id: i64,
    pub char_id: i64,
    pub solar_system_id: u32,
    pub started_unix: u64,
    pub detonate_at_unix: u64,
}

pub use eve_db::PersistedSecStatusTransaction as SecStatusTransaction;

#[derive(Debug, Clone)]
pub struct SentryAggressionState {
    pub solar_system_id: u32,
    pub offender_char_id: i64,
    pub offender_ship_id: i64,
    pub sentry_origin: DVec3,
    pub active_on_grid: bool,
    pub total_sentry_damage_dealt: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SafetyLevel {
    Red = 0,
    Yellow = 1,
    #[default]
    Green = 2,
}

#[derive(Debug, Clone)]
pub struct DroneBehaviorSettings {
    pub aggressive: bool,
    pub focus_fire: bool,
}

impl Default for DroneBehaviorSettings {
    fn default() -> Self {
        Self {
            aggressive: true,
            focus_fire: true,
        }
    }
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct InsuranceContractState {
    pub ship_id: i64,
    pub ship_type_id: u32,
    pub owner_id: i64,
    pub level_name: String,
    pub level_fraction: f64,
    pub cost_isk: f64,
    pub payout_isk: f64,
    pub start_unix: u64,
    pub end_unix: u64,
}

#[derive(Debug, Default)]
pub struct CombatRuntimeState {
    pub entropic_spools: HashMap<(i64, i64), EntropicSpoolState>,
    pub in_flight_missiles: HashMap<i64, InFlightMissileState>,
    pub active_breacher_dots: HashMap<i64, ActiveBreacherDot>,
    pub aggression_assist_log: HashMap<i64, Vec<AssistLogEntry>>,
    pub detailed_killmails: HashMap<i64, DetailedKillmail>,
    pub container_lifetimes: HashMap<i64, ContainerLifetimeEntry>,
    pub self_destruct_countdowns: HashMap<i64, SelfDestructCountdown>,
    pub sec_status_transactions: HashMap<i64, Vec<SecStatusTransaction>>,
    pub duel_challenges: HashMap<(i64, i64), u64>,
    pub corp_aggression_settings: HashMap<i64, bool>,
    pub modified_system_securities: HashMap<u32, f64>,
    pub disapproval_flags: HashMap<i64, u64>,
    pub sentry_gun_aggression: HashMap<(u32, i64), SentryAggressionState>,
    pub safety_levels: HashMap<i64, SafetyLevel>,
    pub limited_engagements: HashMap<i64, HashMap<i64, u64>>,
    pub npc_timers: HashMap<i64, u64>,
    pub crimewatch_timer_states: HashMap<i64, (i64, i64, i64, i64)>,
    pub active_drones_in_space: HashMap<i64, DroneSpaceState>,
    pub drone_settings: HashMap<i64, DroneBehaviorSettings>,
    pub npc_entities: HashMap<i64, ExtendedNpcEntity>,
    pub pending_bounty_ticks: HashMap<(u32, i64), f64>,
    pub encounter_waves: HashMap<u32, EncounterWaveDirector>,
    pub insurance_contracts: HashMap<i64, InsuranceContractState>,
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
}

#[derive(Debug, Clone)]
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

impl CombatRuntimeState {
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

    pub fn sync_from_persisted(&mut self, p: &eve_db::PersistedCombatCrimewatchState) {
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
    }

    pub fn to_persisted(&self) -> eve_db::PersistedCombatCrimewatchState {
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
        }
    }
}

pub fn with_combat_runtime<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&CombatRuntimeState) -> R,
) -> R {
    ctx.db.with_snapshot(|s| {
        let mut guard = ctx
            .combat_ephemeral
            .write()
            .unwrap_or_else(|e| e.into_inner());
        guard.sync_from_persisted(&s.combat_crimewatch_state);
        f(&guard)
    })
}

pub fn with_mut_combat_runtime<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut CombatRuntimeState) -> R,
) -> R {
    ctx.db.with_mut_snapshot(|s| {
        let mut guard = ctx
            .combat_ephemeral
            .write()
            .unwrap_or_else(|e| e.into_inner());
        guard.sync_from_persisted(&s.combat_crimewatch_state);
        let out = f(&mut guard);
        s.combat_crimewatch_state = guard.to_persisted();
        out
    })
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
    let mut base_sig = if let Ok(fit) = ctx.get_or_rebuild_ship_fitting(char_id, ship_id) {
        let eval_ctx = fit.evaluated_context(&ctx.sde);
        let sig = eval_ctx.ship.get_attribute(eve_sde::attr_ids::SIGNATURE_RADIUS, &ctx.sde);
        if sig > 0.0 {
            sig
        } else {
            fit.ship
                .get_attribute(eve_sde::attr_ids::SIGNATURE_RADIUS, &ctx.sde)
                .max(35.0)
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
    if mwd_bloom_mult > 1.0 && base_sig < 150.0 {
        base_sig *= mwd_bloom_mult;
    }

    let sys_handle = ctx.systems.get_or_spawn(solar_system_id);
    if let Ok(Some((_, ball))) = sys_handle.get_ball_snapshot(ship_id).await {
        if (ball.radius - base_sig).abs() > 1e-3 {
            if let Ok(Some(mut cs)) = sys_handle.get_combat_state(ship_id).await {
                cs.signature_radius = base_sig;
                let _ = sys_handle.set_combat_state(ship_id, cs).await;
            }
        }
    }
    base_sig
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
    let after = eve_dogma::simulate_capacitor(
        seed_hp,
        cs.max_shield_hp,
        recharge_time_s.max(1.0),
        dt,
    );
    let delta = (after - before).max(0.0);
    cs.shield_hp = after;
    let shield_ratio = (cs.shield_hp / cs.max_shield_hp.max(1.0)).clamp(0.0, 1.0);
    let armor_ratio = (cs.armor_hp / cs.max_armor_hp.max(1.0)).clamp(0.0, 1.0);
    let hull_ratio = (cs.structure_hp / cs.max_structure_hp.max(1.0)).clamp(0.0, 1.0);
    let _ = sys_handle.set_combat_state(ball_id, cs).await;
    if delta > 0.0 {
        if let Some(cid) = char_id_opt {
            let _ = ctx.notify_damage_state_change(
                NotificationTarget::Character(cid),
                ball_id,
                shield_ratio,
                armor_ratio,
                hull_ratio,
            );
            crate::services::fleet_social::stream_fleet_watchlist_damage(
                ctx,
                cid,
                shield_ratio,
                armor_ratio,
                hull_ratio,
            );
        }
    }
    Some((after, delta))
}

/// Instantaneous passive shield recharge rate in HP/s at `current_shield` out of `max_shield`
/// with time constant `recharge_time_s` (`feat:dogma.shield-recharge`).
pub fn passive_shield_recharge_rate_hps(
    current_shield: f64,
    max_shield: f64,
    recharge_time_s: f64,
) -> f64 {
    if max_shield <= 0.0 || recharge_time_s <= 0.0 {
        return 0.0;
    }
    let ratio = (current_shield / max_shield).clamp(0.0, 1.0);
    (10.0 * max_shield / recharge_time_s) * (ratio.sqrt() - ratio)
}

/// Emits `OnDamageMessage`, `OnDamageMessages`, and `OnDamageStateChange` notifications to both attacker and victim (`feat:combat.damage-log`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn emit_combat_log_and_damage_state(
    ctx: &ServerContext,
    attacker_char_id: i64,
    victim_char_opt: Option<i64>,
    attacker_ship_id: i64,
    target_ball_id: i64,
    weapon_type_id: u32,
    applied_dmg: f64,
    is_wrecking: bool,
    shield_ratio: f64,
    armor_ratio: f64,
    hull_ratio: f64,
) {
    let hit_quality = if applied_dmg <= 0.0 {
        "miss"
    } else if is_wrecking {
        "wrecking"
    } else {
        "hit"
    };
    let msg_args = vec![
        PyRep::String(hit_quality.into()),
        PyRep::Dict(vec![
            (PyRep::String("damage".into()), PyRep::Real(applied_dmg)),
            (PyRep::String("source".into()), PyRep::Integer(attacker_ship_id)),
            (PyRep::String("target".into()), PyRep::Integer(target_ball_id)),
            (
                PyRep::String("weapon".into()),
                PyRep::Integer(weapon_type_id as i64),
            ),
            (PyRep::String("isWrecking".into()), PyRep::Bool(is_wrecking)),
        ]),
    ];
    ctx.notify(
        NotificationTarget::Character(attacker_char_id),
        "OnDamageMessage",
        "charid",
        msg_args.clone(),
    );
    ctx.notify(
        NotificationTarget::Character(attacker_char_id),
        "OnDamageMessages",
        "charid",
        vec![PyRep::List(vec![PyRep::Tuple(msg_args.clone())])],
    );
    let _ = ctx.notify_damage_state_change(
        NotificationTarget::Character(attacker_char_id),
        target_ball_id,
        shield_ratio,
        armor_ratio,
        hull_ratio,
    );


    let resolved_victim = victim_char_opt.or_else(|| {
        ctx.db.with_snapshot(|snap| {
            snap.items
                .get(&target_ball_id)
                .map(|i| i.owner_id)
                .filter(|id| snap.characters.contains_key(id))
                .or_else(|| {
                    snap.characters
                        .values()
                        .find(|c| c.active_ship_id == target_ball_id)
                        .map(|c| c.character_id)
                })
        })
    });
    if let Some(vic_id) = resolved_victim {
        crate::services::fleet_social::stream_fleet_watchlist_damage(
            ctx,
            vic_id,
            shield_ratio,
            armor_ratio,
            hull_ratio,
        );
    }
    if let Some(vic_id) = victim_char_opt.filter(|&v| v != attacker_char_id) {
        ctx.notify(
            NotificationTarget::Character(vic_id),
            "OnDamageMessage",
            "charid",
            msg_args.clone(),
        );
        ctx.notify(
            NotificationTarget::Character(vic_id),
            "OnDamageMessages",
            "charid",
            vec![PyRep::List(vec![PyRep::Tuple(msg_args.clone())])],
        );
        let _ = ctx.notify_damage_state_change(
            NotificationTarget::Character(vic_id),
            target_ball_id,
            shield_ratio,
            armor_ratio,
            hull_ratio,
        );

    }
}

pub(crate) fn is_legal_war_or_suspect_target(
    snap: &eve_db::WorldStateSnapshot,
    attacker_char_id: i64,
    victim_char_id: i64,
    now_unix: u64,
) -> bool {
    let has_suspect = snap
        .kill_rights
        .values()
        .any(|kr| kr.target_id == victim_char_id && kr.is_active_suspect);
    if has_suspect {
        return true;
    }
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
    let att_entity = att.alliance_id.unwrap_or(att.corporation_id);
    let vic_entity = vic.alliance_id.unwrap_or(vic.corporation_id);
    snap.wars.values().any(|w| {
        w.active
            && now_unix >= w.started_unix.saturating_add(86_400)
            && ((w.aggressor_corp_id == att_entity && w.defender_corp_id == vic_entity)
                || (w.aggressor_corp_id == vic_entity && w.defender_corp_id == att_entity)
                || (w.aggressor_corp_id == att.corporation_id
                    && w.defender_corp_id == vic.corporation_id)
                || (w.aggressor_corp_id == vic.corporation_id
                    && w.defender_corp_id == att.corporation_id))
    })
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
        PyRep::List(items) | PyRep::Tuple(items) => {
            items.iter().filter_map(|v| v.as_i64()).collect()
        }
        PyRep::ObjectEx { header, list, .. } => {
            let mut out: Vec<i64> = list.iter().filter_map(|v| v.as_i64()).collect();
            if let PyRep::Tuple(h) = header.as_ref() {
                if let Some(PyRep::Tuple(ctor_args)) = h.get(1) {
                    if let Some(PyRep::List(items) | PyRep::Tuple(items)) = ctor_args.first() {
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
            for tid in &bounty_ids {
                if let Some(&amt) = rt.player_bounty_pool.get(tid) {
                    if amt > 0.0 {
                        list.push(PyRep::Tuple(vec![
                            PyRep::Integer(*tid),
                            PyRep::keyval([
                                ("targetID", PyRep::Integer(*tid)),
                                ("bountyISK", PyRep::Real(amt)),
                                ("bounty", PyRep::Real(amt)),
                            ]),
                        ]));
                    }
                }
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
                let restr = rt.kill_right_restricted_to.get(&kr.kill_right_id).copied();
                let restr_rep = match restr {
                    Some(id) if id > 0 => PyRep::Integer(id),
                    _ => PyRep::None,
                };
                PyRep::keyval([
                    ("killRightID", PyRep::Integer(kr.kill_right_id)),
                    ("fromID", PyRep::Integer(kr.owner_id)),
                    ("toID", PyRep::Integer(kr.target_id)),
                    ("price", PyRep::Real(kr.activation_fee.to_wire_f64())),
                    ("activeSuspect", PyRep::Bool(kr.is_active_suspect)),
                    ("expiryTime", PyRep::Integer(exp as i64)),
                    ("restrictedTo", restr_rep),
                ])
            })
            .collect::<Vec<_>>()
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
    let initial_len = session
        .character_id
        .and_then(|cid| {
            ctx.pending_notifications
                .read()
                .ok()
                .and_then(|q| q.get(&cid).map(|v| v.len()))
        })
        .unwrap_or(0);

    if let Some(res) = handle(ctx, session, service, method, args, kwargs).await {
        if let Some(cid) = session.character_id {
            if let Some(notifs) = ctx
                .pending_notifications
                .read()
                .ok()
                .and_then(|q| q.get(&cid).cloned())
            {
                for notif in notifs.into_iter().skip(initial_len) {
                    session.push_notification(notif.event, notif.args);
                }
            }

        }
        return res;
    }

    ctx.dispatch_rpc(session, service, None, method, args, kwargs, None)
        .await
}

/// Claims up to 20% of the destroyed ship's reference value from the victim's (and corp/alliance)
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
    let (vic_corp, vic_all, kil_corp, kil_all) = ctx.db.with_snapshot(|snap| {
        let vc = snap.characters.get(&victim_char_id).map(|c| c.corporation_id).unwrap_or(0);
        let va = snap.corporations.get(&vc).and_then(|co| co.alliance_id).or_else(|| snap.characters.get(&victim_char_id).and_then(|c| c.alliance_id)).unwrap_or(0);
        let kc = snap.characters.get(&killer_char_id).map(|c| c.corporation_id).unwrap_or(0);
        let ka = snap.corporations.get(&kc).and_then(|co| co.alliance_id).or_else(|| snap.characters.get(&killer_char_id).and_then(|c| c.alliance_id)).unwrap_or(0);
        (vc, va, kc, ka)
    });
    let ref_val = compute_ship_insurance_reference_value(&ctx.sde, victim_ship_type_id).max(250_000.0);
    let max_claim = 0.20 * ref_val;
    let (claimed_isk, new_bal) = ctx.db.with_mut_snapshot(|snap| {
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
        let mut guard = ctx.combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
        guard.sync_from_persisted(&snap.combat_crimewatch_state);
        (claimed, bal)
    });
    if claimed_isk > 0.0 {
        if let Some(bal) = new_bal {
            let _ = ctx.notify_account_change(killer_char_id, "cash", killer_char_id, bal);
        }
        let now_ft = ctx.now_blueos_filetime_ticks();
        ctx.notify(
            NotificationTarget::Character(killer_char_id),
            "OnBountyAddedToPayout",
            "charid",
            vec![PyRep::Dict(vec![
                (PyRep::String("amount".into()), PyRep::Real(claimed_isk)),
                (PyRep::String("payoutTime".into()), PyRep::Integer(now_ft)),
                (
                    PyRep::String("enemyTypeID".into()),
                    PyRep::Integer(i64::from(victim_ship_type_id)),
                ),
                (PyRep::String("isModified".into()), PyRep::Bool(false)),
            ])],
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
        });
    }
    match action {
        "set" => {
            if !amount_isk.is_finite() || amount_isk < 0.0 {
                return Err(BridgeRpcError {
                    code: "InvalidBountyAmount".into(),
                    message: "Bounty amount must be non-negative finite number".into(),
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
            // Refunds are capped at the remaining bounty pool and scaled pro-rata across contributors
            // so that bounty ISK is strictly conserved without duplication.
            let total_reimbursed = ctx.db.with_mut_snapshot(|snap| {
                let (remaining_pool, refunds) = if target_id > 0 {
                    let pool = snap.combat_crimewatch_state.player_bounty_pool.remove(&target_id).unwrap_or(0.0);
                    let contribs = snap.combat_crimewatch_state.bounty_contributions.remove(&target_id).unwrap_or_default();
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
                            snap.append_journal(cid, "bounty_reimbursement", add, bal, "bounty_reimbursement");
                        }
                    }
                }
                let mut guard = ctx.combat_ephemeral.write().unwrap_or_else(|e| e.into_inner());
                guard.sync_from_persisted(&snap.combat_crimewatch_state);
                sum
            });
            Ok(total_reimbursed)
        }
        _ => Err(BridgeRpcError {
            code: "InvalidGmBountyAction".into(),
            message: format!("Unknown GM bounty action '{action}'"),
        }),
    }
}

/// Creates a kill right from `owner_id` against `target_id` and emits `OnKillRightCreated`.
pub fn create_kill_right(ctx: &ServerContext, kr_id: i64, owner_id: i64, target_id: i64, fee_isk: f64) {
    let expiry_ft = ctx.now_blueos_filetime_ticks() + (30 * 86_400 * 10_000_000);
    let expires_unix = ctx.now_unix().saturating_add(30 * 86_400);
    with_mut_combat_runtime(ctx, |rt| {
        rt.kill_right_expires_unix.insert(kr_id, expires_unix);
    });
    ctx.db.with_mut_snapshot(|snap| {
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
    });
    let args = vec![
        PyRep::Integer(kr_id),
        PyRep::Integer(owner_id),
        PyRep::Integer(target_id),
        PyRep::Integer(expiry_ft),
    ];
    ctx.notify(
        NotificationTarget::Character(owner_id),
        "OnKillRightCreated",
        "charid",
        args.clone(),
    );
    ctx.notify(
        NotificationTarget::Character(target_id),
        "OnKillRightCreated",
        "charid",
        args,
    );
}

pub(crate) fn validate_kill_right_live_preconditions(
    ctx: &ServerContext,
    snap: &eve_db::WorldStateSnapshot,
    kr_id: i64,
    caller_id: i64,
    target_id: i64,
) -> Result<(), BridgeRpcError> {
    let exp_opt = snap.combat_crimewatch_state.kill_right_expires_unix.get(&kr_id).copied();
    if let Some(exp) = exp_opt {
        if ctx.now_unix() >= exp {
            return Err(BridgeRpcError {
                code: "KillRightExpired".into(),
                message: format!("Kill right {kr_id} has expired (30-day lifetime)"),
            });
        }
    }
    let Some(caller) = snap.characters.get(&caller_id) else {
        return Err(BridgeRpcError {
            code: "CharacterNotFound".into(),
            message: format!("Character {caller_id} not found"),
        });
    };
    if caller.station_id.is_some() || caller.structure_id.is_some() {
        return Err(BridgeRpcError {
            code: "KillRightCannotActivateDocked".into(),
            message: "Cannot activate or buy a kill right while docked".into(),
        });
    }
    let Some(target) = snap.characters.get(&target_id) else {
        return Err(BridgeRpcError {
            code: "TargetNotFound".into(),
            message: format!("Kill right target {target_id} not found"),
        });
    };
    if target.station_id.is_some()
        || target.structure_id.is_some()
        || caller.solar_system_id != target.solar_system_id
    {
        return Err(BridgeRpcError {
            code: "KillRightTargetNotInSystem".into(),
            message: "Target pilot must be undocked in the same solar system".into(),
        });
    }
    let sys_id = caller.solar_system_id;
    let c_pos = ctx.systems.get_ball_position_sync(sys_id, caller.active_ship_id);
    let t_pos = ctx.systems.get_ball_position_sync(sys_id, target.active_ship_id);
    if let (Some(cp), Some(tp)) = (c_pos, t_pos) {
        if (cp - tp).length() > 8_000_000.0 {
            return Err(BridgeRpcError {
                code: "KillRightTargetNotOnGrid".into(),
                message: "Target pilot is not on grid with caller".into(),
            });
        }
    }
    Ok(())
}

/// Emits limited engagement timeout start/stop and end notifications to `char_id`.
pub fn update_crimewatch_engagement_timers(ctx: &ServerContext, char_id: i64, other_char_id: i64) {
    let timeout_ft = ctx.now_blueos_filetime_ticks() + 3_000_000_000;
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnCrimewatchEngagementStartTimeout",
        "charid",
        vec![PyRep::Integer(other_char_id), PyRep::Integer(timeout_ft)],
    );
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnCrimewatchEngagementStopTimeout",
        "charid",
        vec![PyRep::Integer(other_char_id)],
    );
    with_mut_combat_runtime(ctx, |rt| {
        if let Some(m) = rt.limited_engagements.get_mut(&char_id) {
            m.remove(&other_char_id);
        }
    });
    ctx.notify(
        NotificationTarget::Character(char_id),
        "OnCrimewatchEngagementEnded",
        "charid",
        vec![PyRep::Integer(other_char_id)],
    );
}
