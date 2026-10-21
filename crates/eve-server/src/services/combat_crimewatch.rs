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
use std::sync::{Arc, OnceLock, RwLock};

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
#[path = "combat_npcs_drones.rs"]
pub mod combat_npcs_drones;
#[path = "combat_weapons.rs"]
pub mod combat_weapons;
#[path = "combat_wrecks_killmails.rs"]
pub mod combat_wrecks_killmails;
#[path = "crimewatch_timers.rs"]
pub mod crimewatch_timers;

pub use combat_fighters::*;
pub(crate) use combat_fighters_abilities::*;
pub use combat_legacy_ess::*;
pub use combat_missiles_aoe::*;
pub use combat_npc_ai::*;
pub use combat_npcs_drones::*;
pub use combat_weapons::*;
pub use combat_wrecks_killmails::*;
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

#[derive(Debug, Clone)]
pub struct KillmailAttackerEntry {
    pub character_id: i64,
    pub corporation_id: i64,
    pub alliance_id: Option<i64>,
    pub ship_type_id: u32,
    pub weapon_type_id: u32,
    pub damage_done: f64,
    pub final_blow: bool,
    pub security_status: f64,
}

#[derive(Debug, Clone)]
pub struct KillmailItemEntry {
    pub type_id: u32,
    pub flag: u16,
    pub qty_dropped: i32,
    pub qty_destroyed: i32,
    pub singleton: bool,
}

#[derive(Debug, Clone)]
pub struct DetailedKillmail {
    pub kill_id: i64,
    pub kill_time_unix: u64,
    pub solar_system_id: u32,
    pub victim_character_id: i64,
    pub victim_corporation_id: i64,
    pub victim_alliance_id: Option<i64>,
    pub victim_ship_type_id: u32,
    pub victim_damage_taken: f64,
    pub victim_position: [f64; 3],
    pub attackers: Vec<KillmailAttackerEntry>,
    pub dropped_items: Vec<KillmailItemEntry>,
    pub destroyed_items: Vec<KillmailItemEntry>,
    pub total_value_isk: f64,
    pub killmail_hash: String,
}

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

#[derive(Debug, Clone)]
pub struct SecStatusTransaction {
    pub timestamp_unix: u64,
    pub old_sec: f64,
    pub new_sec: f64,
    pub delta: f64,
    pub reason: String,
    pub solar_system_id: u32,
}

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
    pub recent_session_notifications: HashMap<i64, Vec<(String, Vec<PyRep>)>>,
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
}

type CombatRuntimeEntry = (std::sync::Weak<eve_db::WorldRepository>, CombatRuntimeState);

static COMBAT_RUNTIME: OnceLock<RwLock<HashMap<usize, CombatRuntimeEntry>>> = OnceLock::new();

fn runtime_map() -> &'static RwLock<HashMap<usize, CombatRuntimeEntry>> {
    COMBAT_RUNTIME.get_or_init(|| RwLock::new(HashMap::new()))
}

fn ctx_key(ctx: &ServerContext) -> usize {
    Arc::as_ptr(&ctx.db) as usize
}

pub fn with_combat_runtime<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&CombatRuntimeState) -> R,
) -> R {
    let key = ctx_key(ctx);
    let mut guard = runtime_map().write().unwrap_or_else(|e| e.into_inner());
    guard.retain(|_, (weak, _)| weak.strong_count() > 0);
    let (_, st) = guard
        .entry(key)
        .or_insert_with(|| (Arc::downgrade(&ctx.db), CombatRuntimeState::default()));
    f(st)
}

pub fn with_mut_combat_runtime<R>(
    ctx: &ServerContext,
    f: impl FnOnce(&mut CombatRuntimeState) -> R,
) -> R {
    let key = ctx_key(ctx);
    let mut guard = runtime_map().write().unwrap_or_else(|e| e.into_inner());
    guard.retain(|_, (weak, _)| weak.strong_count() > 0);
    let (_, st) = guard
        .entry(key)
        .or_insert_with(|| (Arc::downgrade(&ctx.db), CombatRuntimeState::default()));
    f(st)
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
    if let Some(cid) = char_id_opt {
        let _ = ctx.notify_damage_state_change(
            NotificationTarget::Character(cid),
            ball_id,
            shield_ratio,
            armor_ratio,
            hull_ratio,
        );
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
    let dmg_state_args = vec![
        PyRep::Integer(target_ball_id),
        PyRep::List(vec![
            PyRep::Real(shield_ratio),
            PyRep::Real(armor_ratio),
            PyRep::Real(hull_ratio),
        ]),
    ];
    with_mut_combat_runtime(ctx, |rt| {
        let q = rt
            .recent_session_notifications
            .entry(attacker_char_id)
            .or_default();
        q.push(("OnDamageMessage".to_string(), msg_args.clone()));
        q.push((
            "OnDamageMessages".to_string(),
            vec![PyRep::List(vec![PyRep::Tuple(msg_args.clone())])],
        ));
        q.push(("OnDamageStateChange".to_string(), dmg_state_args.clone()));
    });
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
        with_mut_combat_runtime(ctx, |rt| {
            let q = rt.recent_session_notifications.entry(vic_id).or_default();
            q.push(("OnDamageMessage".to_string(), msg_args));
            q.push(("OnDamageStateChange".to_string(), dmg_state_args));
        });
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
            let recent = with_mut_combat_runtime(ctx, |rt| {
                rt.recent_session_notifications
                    .remove(&cid)
                    .unwrap_or_default()
            });
            for (ev, ev_args) in recent {
                session.push_notification(ev, ev_args);
            }
        }
        return res;
    }

    ctx.dispatch_rpc(session, service, None, method, args, kwargs, None)
        .await
}
