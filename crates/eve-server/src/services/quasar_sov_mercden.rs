//! `quasar_sov_mercden` — Equinox Mercenary Den and Mercenary Tactical Operation services (`Q-8`).
//!
//! Backs `eve_public.sovereignty.mercenaryden.api` (`GetAsOwnerRequest`, `GetAllOwnedRequest`,
//! `GetMaximumForCharacterRequest`), `eve_public.sovereignty.mercenaryden.api.admin`
//! (`ModifyDevelopmentLevelRequest`, `ModifyAnarchyLevelRequest`) and
//! `eve_public.sovereignty.mercenaryden.activity.api` (`GetForMercenaryDenRequest`,
//! `GetAllRequest`, `StartRequest`, `GetCapacityRequest`) with persisted
//! [`PersistedMercenaryDen`] / [`PersistedMercenaryActivity`] records in
//! `WorldStateSnapshot.sov_runtime`.
//!
//! Notices are emitted only from state transitions: `EvolutionStateChangedNotice` when the
//! lazily-simulated (or admin-modified) development / anarchy stage changes, `AddedNotice` /
//! `RemovedNotice` when the activity generator runs, and `StartedNotice` when a pilot starts one.

#![forbid(unsafe_code)]

use super::quasar_sov_hub::{push_notice, required_id, transact, PendingNotices, RpcError};
use crate::quasar_gateway::{
    require_admin_role, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_CONFLICT, STATUS_FORBIDDEN,
    STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use eve_db::{PersistedMercenaryActivity, PersistedMercenaryDen, WorldRepository, WorldStateSnapshot};
use eve_proto::pb::{self, ProtoBuilder};
use eve_proto::quasar::{AuthoritativeContext, DynamicValue, GoogleTimestamp};
use std::sync::Arc;

/// SDE skill `Mercenary Den Management` (each trained level allows one more den, up to 5).
pub const TYPE_MERCENARY_DEN_MANAGEMENT: u32 = 85_233;
/// SDE item type `Infomorph Decryption Key` (`85746`).
pub const TYPE_INFOMORPH_DECRYPTION_KEY: u32 = 85_746;

/// V24 SDE & Wiki (`Mercenary_Den_Guide.md`): absolute number of dens a character may own.
pub const MERCENARY_DEN_ABSOLUTE_MAX: u64 = 5;
/// Hourly Infomorph Decryption Key generation tick (`3,600 s`).
pub const INFOMORPH_UNIT_SECS: u64 = 3_600;
/// Wiki (`Mercenary_Tactical_Operation.md:26,39`): passive Development / Anarchy evolution tick (`+5` points per `86,400 s`).
pub const EVOLUTION_UNIT_SECS: u64 = 86_400;
/// Wiki (`Mercenary_Tactical_Operation.md:26,39`): passive Development and Anarchy gain per 24h cycle.
pub const EVOLUTION_DELTA_PER_DAY: u32 = 5;
/// Wiki (`Mercenary_Tactical_Operation.md:14`, "MTOs spawn approximately every 3 days from the time
/// they are onlined"): seconds between MTO generation passes (`72h`).
pub const ACTIVITY_GENERATION_SECS: u64 = 3 * 86_400;
/// Wiki (`Mercenary_Tactical_Operation.md:14`): lifetime of a generated (unstarted) operation (`3` days).
pub const ACTIVITY_LIFETIME_SECS: u64 = 3 * 86_400;
const MAX_LEVEL: u32 = 100;
/// V24 SDE (`type_id = 85230` Mercenary Den `capacity = 3000.0`): infomorph bay capacity.
pub const INFOMORPH_CARGO_CAPACITY: u32 = 3_000;
/// Wiki (`Mercenary_Tactical_Operation.md:28-34,41-47`): Development / Anarchy progress bounds per level (`0..=4` -> proto enum `1..=5`).
pub const STAGE_BOUNDS: [(u32, u32); 5] = [(0, 19), (20, 39), (40, 69), (70, 99), (100, 100)];

/// Infomorph bay capacity (`3,000` units from V24 SDE `type_id = 85230`).
pub fn infomorph_cargo_capacity(_dev_stage: u32) -> u32 {
    INFOMORPH_CARGO_CAPACITY
}

const EVOLUTION_NOTICE: &str = "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice";

/// Protobuf `DevelopmentStage` / `AnarchyStage` enum (`1..=5`, corresponding to UI levels `0..=4`)
/// for a progress value (`0..=100`) per `Mercenary_Tactical_Operation.md`.
pub fn stage_for_level(level: u32) -> u32 {
    match level {
        0..=19 => 1,
        20..=39 => 2,
        40..=69 => 3,
        70..=99 => 4,
        _ => 5,
    }
}

pub fn infomorphs_per_hour(dev_stage: u32) -> u32 {
    match dev_stage {
        1 => 65,
        2 => 68,
        3 => 71,
        4 => 74,
        _ => 78,
    }
}

/// V24 stage band `(min_idk_per_hr, max_idk_per_hr)` from `Mercenary_Tactical_Operation.md:28-34`.
pub fn stage_infomorph_band(dev_stage: u32) -> (u32, u32) {
    match dev_stage {
        1 => (65, 100),
        2 => (68, 105),
        3 => (71, 110),
        4 => (74, 115),
        _ => (78, 120),
    }
}

/// Planet-scaled V24 hourly Infomorph Decryption Key yield (`65..=120` IDK/hr across stages `1..=5`),
/// scaling within each stage's V24 band by the temperate planet's SDE radius (`sde.celestials_in_system`).
pub fn planet_scaled_infomorphs_per_hour(
    sde: Option<&eve_sde::SdeRegistry>,
    solar_system_id: u32,
    planet_id: i64,
    dev_stage: u32,
) -> u32 {
    let (min_r, max_r) = stage_infomorph_band(dev_stage);
    let Some(planet) = sde.and_then(|reg| {
        reg.celestials_in_system(solar_system_id)
            .into_iter()
            .find(|c| c.celestial_id == planet_id)
    }) else {
        return min_r;
    };
    let r_km = (planet.radius / 1_000.0).clamp(2_000.0, 12_000.0);
    let norm = ((r_km - 2_000.0) / 10_000.0).clamp(0.0, 1.0);
    let span = f64::from(max_r.saturating_sub(min_r));
    min_r.saturating_add((norm * span).round() as u32).min(max_r)
}

/// Wiki (`Mercenary_Tactical_Operation.md:41-47`): host Skyhook workforce disruption percentage by Anarchy stage (`1..=5`: `0%`, `0%`, `-10%`, `-20%`, `-40%`).
pub fn anarchy_workforce_disruption_pct(anarchy_stage: u32) -> u64 {
    match anarchy_stage {
        1 | 2 => 0,
        3 => 10,
        4 => 20,
        _ => 40,
    }
}

/// Concurrent Mercenary Tactical Operations offered by a den at a development stage (`3..=7` across
/// stages `1..=5`).
pub fn activity_capacity(stage: u32) -> u32 {
    2 + stage
}

fn require_character(ctx: &AuthoritativeContext) -> Result<i64, RpcError> {
    ctx.effective_character_id()
        .map(i64::from)
        .ok_or((STATUS_UNAUTHORIZED, "Authenticated character required".to_string()))
}

fn owned_den(snap: &WorldStateSnapshot, cid: i64, den_id: i64) -> Result<PersistedMercenaryDen, RpcError> {
    let den = snap
        .sov_runtime
        .mercenary_dens
        .get(&den_id)
        .ok_or((STATUS_NOT_FOUND, format!("Mercenary den {den_id} not found")))?;
    if den.owner_char_id != cid {
        return Err((STATUS_FORBIDDEN, format!("Mercenary den {den_id} is not owned by character {cid}")));
    }
    if let Some(&(_, _, shield_pct)) = snap.sov_runtime.mercenary_den_reinforcement.get(&den_id) {
        if !den.enabled || shield_pct < 50 {
            return Err((
                STATUS_CONFLICT,
                format!("Mercenary den {den_id} is reinforced and inaccessible until shields are repaired to >=50%"),
            ));
        }
    }
    Ok(den.clone())
}

/// Advances hourly infomorph generation and 24-hour development / anarchy evolution (`+5`/day),
/// tracking sub-24h remainder in `evo_rem_secs`. Returns `true` when the development or anarchy stage changed.
pub fn simulate_den_with_rem(den: &mut PersistedMercenaryDen, evo_rem_secs: &mut u64, now: u64) -> bool {
    simulate_den_for_planet(den, evo_rem_secs, None, now)
}

/// Advances hourly infomorph generation and 24-hour development / anarchy evolution (`+5`/day),
/// scaling hourly infomorph output across each V24 stage band using host planet SDE radius.
pub fn simulate_den_for_planet(
    den: &mut PersistedMercenaryDen,
    evo_rem_secs: &mut u64,
    sde: Option<&eve_sde::SdeRegistry>,
    now: u64,
) -> bool {
    if den.simulated_at_unix == 0 || !den.enabled || now <= den.simulated_at_unix {
        if den.simulated_at_unix == 0 || !den.enabled {
            den.simulated_at_unix = now;
        }
        return false;
    }
    let hours = (now - den.simulated_at_unix) / INFOMORPH_UNIT_SECS;
    if hours == 0 {
        return false;
    }
    let before = (stage_for_level(den.development_level), stage_for_level(den.anarchy_level));
    let stepped = hours.min(720);
    for _ in 0..stepped {
        let rate = planet_scaled_infomorphs_per_hour(
            sde,
            den.solar_system_id,
            den.planet_id,
            stage_for_level(den.development_level),
        );
        den.infomorphs = den.infomorphs.saturating_add(rate).min(INFOMORPH_CARGO_CAPACITY);
        *evo_rem_secs += INFOMORPH_UNIT_SECS;
        if *evo_rem_secs >= EVOLUTION_UNIT_SECS {
            *evo_rem_secs -= EVOLUTION_UNIT_SECS;
            den.development_level = (den.development_level + EVOLUTION_DELTA_PER_DAY).min(MAX_LEVEL);
            den.anarchy_level = (den.anarchy_level + EVOLUTION_DELTA_PER_DAY).min(MAX_LEVEL);
        }
    }
    let extra_hours = hours.saturating_sub(stepped);
    if extra_hours > 0 {
        let rate = planet_scaled_infomorphs_per_hour(
            sde,
            den.solar_system_id,
            den.planet_id,
            stage_for_level(den.development_level),
        );
        let added = u64::from(rate).saturating_mul(extra_hours).min(u64::from(INFOMORPH_CARGO_CAPACITY)) as u32;
        den.infomorphs = den.infomorphs.saturating_add(added).min(INFOMORPH_CARGO_CAPACITY);
        let total_rem = *evo_rem_secs + extra_hours * INFOMORPH_UNIT_SECS;
        let days = (total_rem / EVOLUTION_UNIT_SECS).min(u64::from(MAX_LEVEL)) as u32;
        *evo_rem_secs = total_rem % EVOLUTION_UNIT_SECS;
        if days > 0 {
            let delta = days.saturating_mul(EVOLUTION_DELTA_PER_DAY);
            den.development_level = (den.development_level + delta).min(MAX_LEVEL);
            den.anarchy_level = (den.anarchy_level + delta).min(MAX_LEVEL);
        }
    }
    den.simulated_at_unix += hours * INFOMORPH_UNIT_SECS;
    before != (stage_for_level(den.development_level), stage_for_level(den.anarchy_level))
}

/// Advances development, anarchy and infomorph generation for whole elapsed units.
/// Returns `true` when the development or anarchy stage changed.
pub fn simulate_den(den: &mut PersistedMercenaryDen, now: u64) -> bool {
    let mut rem = 0u64;
    simulate_den_with_rem(den, &mut rem, now)
}

/// Simulates a den in `sov_runtime` while preserving sub-24h evolution remainder.
pub fn simulate_den_in_snap(sov_rt: &mut eve_db::PersistedSovState, den_id: i64, now: u64) -> bool {
    simulate_den_in_snap_with_sde(sov_rt, None, den_id, now)
}

/// Simulates a den in `sov_runtime` using host planet SDE data.
pub fn simulate_den_in_snap_with_sde(
    sov_rt: &mut eve_db::PersistedSovState,
    sde: Option<&eve_sde::SdeRegistry>,
    den_id: i64,
    now: u64,
) -> bool {
    let Some(den) = sov_rt.mercenary_dens.get_mut(&den_id) else {
        return false;
    };
    let rem = sov_rt.mercenary_den_evo_rem.entry(den_id).or_insert(0);
    simulate_den_for_planet(den, rem, sde, now)
}

fn fill_level_sim(b: ProtoBuilder, level: u32, simulated_at: u64) -> ProtoBuilder {
    b.set("level", level)
        .set("stage", stage_for_level(level))
        .set("simulated_at", GoogleTimestamp::from_unix(simulated_at as i64, 0))
}

fn fill_evolution_simulation(b: ProtoBuilder, den: &PersistedMercenaryDen) -> ProtoBuilder {
    b.set(if den.enabled { "started" } else { "paused" }, true)
        .set_with("development", |d| {
            fill_level_sim(d, den.development_level, den.simulated_at_unix)
        })
        .set_with("anarchy", |a| fill_level_sim(a, den.anarchy_level, den.simulated_at_unix))
}

/// Encodes `eve_public.sovereignty.mercenaryden.EvolutionSimulation`.
pub fn encode_evolution_simulation(den: &PersistedMercenaryDen) -> Vec<u8> {
    fill_evolution_simulation(
        pb::message("eve_public.sovereignty.mercenaryden.EvolutionSimulation"),
        den,
    )
    .must_encode()
}

fn fill_evolution_definition(b: ProtoBuilder) -> ProtoBuilder {
    b.set_with("development", |mut dev| {
        dev = dev.set("unit_increase_time_seconds", EVOLUTION_UNIT_SECS as u32);
        for (idx, &(lower, upper)) in STAGE_BOUNDS.iter().enumerate() {
            let stage = (idx as u32) + 1;
            dev = dev.add_with("stages", |s| {
                s.set("stage", stage)
                    .set("level_lower_bound", lower)
                    .set("level_upper_bound", upper)
            });
        }
        dev
    })
    .set_with("anarchy", |mut anarchy| {
        anarchy = anarchy.set("unit_increase_time_seconds", EVOLUTION_UNIT_SECS as u32);
        for (idx, &(lower, upper)) in STAGE_BOUNDS.iter().enumerate() {
            let stage = (idx as u32) + 1;
            anarchy = anarchy.add_with("stages", |s| {
                s.set("stage", stage)
                    .set("level_lower_bound", lower)
                    .set("level_upper_bound", upper)
                    .set("workforce_consumption", anarchy_workforce_disruption_pct(stage))
            });
        }
        anarchy
    })
}

fn fill_infomorphs_gen_definition(
    mut b: ProtoBuilder,
    cap: u32,
    sde: Option<&eve_sde::SdeRegistry>,
    solar_system_id: u32,
    planet_id: i64,
) -> ProtoBuilder {
    b = b
        .set("infomorph_type.sequential", u64::from(TYPE_INFOMORPH_DECRYPTION_KEY))
        .set("generation_tick_seconds", INFOMORPH_UNIT_SECS);
    for stage in 1..=5u32 {
        let rate = planet_scaled_infomorphs_per_hour(sde, solar_system_id, planet_id, stage);
        let has_planet = sde.is_some_and(|r| {
            r.celestials_in_system(solar_system_id)
                .into_iter()
                .any(|c| c.celestial_id == planet_id)
        });
        let (band_min, band_max) = if has_planet { stage_infomorph_band(stage) } else { (rate, rate) };
        b = b.add_with("generation_bands", |band| {
            band.set("stage", stage)
                .set("lower_band", band_min)
                .set("upper_band", band_max)
        });
    }
    b.set("cargo_capacity", cap)
}

fn push_evolution_notice(pending: &PendingNotices, den: &PersistedMercenaryDen) {
    let n = pb::message(EVOLUTION_NOTICE)
        .set("mercenary_den.sequential", den.den_id as u64)
        .set_with("state", |s| fill_evolution_simulation(s, den))
        .must_encode();
    push_notice(pending, den.solar_system_id, EVOLUTION_NOTICE, n);
}

pub fn push_definitions_updated_notice(pending: &PendingNotices, den: &PersistedMercenaryDen) {
    let n = pb::message("eve_public.sovereignty.mercenaryden.api.DefinitionsUpdatedNotice")
        .set("mercenary_den.sequential", den.den_id as u64)
        .set_with("evolution", fill_evolution_definition)
        .set_with("infomorphs", |i| {
            fill_infomorphs_gen_definition(
                i,
                INFOMORPH_CARGO_CAPACITY,
                None,
                den.solar_system_id,
                den.planet_id,
            )
        })
        .must_encode();
    push_notice(pending, den.solar_system_id, "eve_public.sovereignty.mercenaryden.api.DefinitionsUpdatedNotice", n);
}

pub fn push_activity_completed_notice(
    pending: &PendingNotices,
    solar_system_id: u32,
    den_id: i64,
    uuid: &[u8],
    dev_impact: i32,
    anarchy_impact: i32,
    infomorph_bonus: u32,
) {
    let n = pb::message("eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice")
        .set("mercenary_den.sequential", den_id as u64)
        .set("id.uuid", uuid.to_vec())
        .set("development_impact", dev_impact)
        .set("anarchy_impact", anarchy_impact)
        .set("infomorph_bonus", infomorph_bonus)
        .must_encode();
    push_notice(pending, solar_system_id, "eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice", n);
}

pub fn push_activity_expiry_changed_notice(
    pending: &PendingNotices,
    solar_system_id: u32,
    activity: &PersistedMercenaryActivity,
    previous_expiry: u64,
) {
    let n = pb::message("eve_public.sovereignty.mercenaryden.activity.api.ExpiryChangedNotice")
        .set("id.uuid", activity.uuid.clone())
        .set_with("activity", |attr| fill_activity_attributes(attr, activity))
        .set("previous_expiry", GoogleTimestamp::from_unix(previous_expiry as i64, 0))
        .must_encode();
    push_notice(pending, solar_system_id, "eve_public.sovereignty.mercenaryden.activity.api.ExpiryChangedNotice", n);
}

/// Simulates a den in place using planet SDE data and emits `EvolutionStateChangedNotice` on a stage change.
fn tick_den_with_sde(
    snap: &mut WorldStateSnapshot,
    sde: Option<&eve_sde::SdeRegistry>,
    pending: &PendingNotices,
    den_id: i64,
    now: u64,
) {
    if simulate_den_in_snap_with_sde(&mut snap.sov_runtime, sde, den_id, now) {
        if let Some(den) = snap.sov_runtime.mercenary_dens.get(&den_id) {
            push_evolution_notice(pending, den);
        }
    }
}

fn tick_den(snap: &mut WorldStateSnapshot, pending: &PendingNotices, den_id: i64, now: u64) {
    tick_den_with_sde(snap, None, pending, den_id, now);
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn fill_activity_attributes(b: ProtoBuilder, a: &PersistedMercenaryActivity) -> ProtoBuilder {
    b.set_with("activitytemplate", |tmpl| {
        tmpl.when(a.dungeon_id > 0, |t| t.set("dungeon.sequential", a.dungeon_id))
            .set("development_impact", a.development_impact)
            .set("anarchy_impact", a.anarchy_impact)
            .set("infomorph_bonus", a.infomorph_bonus)
    })
    .set("mercenary_den.sequential", a.den_id as u64)
    .set("started", a.started)
    .set("expiry", GoogleTimestamp::from_unix(a.expiry_unix as i64, 0))
    .set("solar_system.sequential", a.solar_system_id)
}

/// Encodes `eve_public.sovereignty.mercenaryden.activity.Attributes`.
pub fn encode_activity_attributes(a: &PersistedMercenaryActivity) -> Vec<u8> {
    fill_activity_attributes(
        pb::message("eve_public.sovereignty.mercenaryden.activity.Attributes"),
        a,
    )
    .must_encode()
}

pub(crate) fn complete_mercenary_activity_in_snap(
    snap: &mut WorldStateSnapshot,
    pending: &PendingNotices,
    a: &PersistedMercenaryActivity,
    den_id: i64,
    solar_system_id: u32,
) {
    let mut applied_dev_impact = 0i32;
    let mut applied_infomorph_bonus = 0u32;
    let mut reward_char_id = a.started_by;
    if let Some(den_mut) = snap.sov_runtime.mercenary_dens.get_mut(&den_id) {
        if reward_char_id <= 0 {
            reward_char_id = den_mut.owner_char_id;
        }
        // Wiki (`Mercenary_Tactical_Operation.md:24`): completing an MTO increases Development
        // by +10 points (or grants +350 bonus Encrypted Infomorphs into den cargo at max Development 100)
        // and reduces Anarchy, awarding infomorph and ISK rewards.
        applied_infomorph_bonus = if den_mut.development_level >= MAX_LEVEL {
            a.infomorph_bonus.max(350)
        } else {
            a.infomorph_bonus.max(50)
        };
        let cap = infomorph_cargo_capacity(stage_for_level(den_mut.development_level));
        den_mut.infomorphs = den_mut.infomorphs.saturating_add(applied_infomorph_bonus).min(cap);

        if den_mut.development_level < MAX_LEVEL {
            applied_dev_impact = if a.development_impact != 0 { a.development_impact } else { 10 };
            den_mut.development_level = (den_mut.development_level as i32 + applied_dev_impact)
                .clamp(0, MAX_LEVEL as i32) as u32;
        }
        let applied_anarchy = if a.anarchy_impact != 0 { a.anarchy_impact } else { -10 };
        den_mut.anarchy_level =
            (den_mut.anarchy_level as i32 + applied_anarchy).clamp(0, MAX_LEVEL as i32) as u32;
        push_evolution_notice(pending, den_mut);
    }
    // Award MTO ISK reward to completing pilot
    if reward_char_id > 0 {
        let isk_reward = 7_500_000_i64;
        if let Some(ch) = snap.characters.get_mut(&reward_char_id) {
            ch.balance = ch
                .balance
                .checked_add(eve_db::IskCents::from_isk(isk_reward))
                .unwrap_or(ch.balance);
            let bal = ch.balance;
            snap.append_journal(
                reward_char_id,
                "MercenaryDenReward",
                eve_db::IskCents::from_isk(isk_reward),
                bal,
                "Mercenary Tactical Operation Completion Reward".to_string(),
            );
        }
    }
    let hex_key = hex(&a.uuid);
    snap.sov_runtime.mto_site_balls.retain(|&bid, v| {
        if v == &hex_key {
            snap.items.remove(&bid);
            snap.item_positions.remove(&bid);
            false
        } else {
            true
        }
    });
    push_activity_completed_notice(
        pending,
        solar_system_id,
        den_id,
        &a.uuid,
        applied_dev_impact,
        a.anarchy_impact,
        applied_infomorph_bonus,
    );
}

/// Expires stale operations (including timed-out started operations without completion rewards)
/// and, when the generation timer elapsed, fills the den up to its capacity — emitting
/// `RemovedNotice` / `AddedNotice` for each change.
pub(crate) fn refresh_activities(snap: &mut WorldStateSnapshot, pending: &PendingNotices, den_id: i64, now: u64) {
    tick_den(snap, pending, den_id, now);
    let Some(den) = snap.sov_runtime.mercenary_dens.get(&den_id).cloned() else { return };
    let expired: Vec<String> = snap
        .sov_runtime
        .mercenary_activities
        .iter()
        .filter(|(_, a)| a.den_id == den_id && a.expiry_unix <= now)
        .map(|(k, _)| k.clone())
        .collect();
    for key in expired {
        if let Some(a) = snap.sov_runtime.mercenary_activities.remove(&key) {
            if a.started {
                let hex_key = hex(&a.uuid);
                snap.sov_runtime.mto_site_balls.retain(|&bid, v| {
                    if v == &hex_key {
                        snap.items.remove(&bid);
                        snap.item_positions.remove(&bid);
                        false
                    } else {
                        true
                    }
                });
            }
            let n = pb::message("eve_public.sovereignty.mercenaryden.activity.api.RemovedNotice")
                .set("mercenary_den.sequential", den_id as u64)
                .set("id.uuid", a.uuid)
                .must_encode();
            push_notice(pending, den.solar_system_id, "eve_public.sovereignty.mercenaryden.activity.api.RemovedNotice", n);
        }
    }
    if !den.enabled || now < den.next_activity_generation_unix {
        return;
    }
    let stage = stage_for_level(den.development_level);
    let count = snap.sov_runtime.mercenary_activities.values().filter(|a| a.den_id == den_id).count() as u32;
    for _ in count..activity_capacity(stage) {
        snap.sov_runtime.next_mercenary_activity_seq += 1;
        let seq = snap.sov_runtime.next_mercenary_activity_seq;
        let mut uuid = (den_id as u64).to_be_bytes().to_vec();
        uuid.extend_from_slice(&seq.to_be_bytes());
        let a = PersistedMercenaryActivity {
            uuid: uuid.clone(),
            den_id,
            solar_system_id: den.solar_system_id,
            dungeon_id: 0,
            started: false,
            started_by: 0,
            expiry_unix: now + ACTIVITY_LIFETIME_SECS,
            development_impact: 10,
            // MTO completion reduces Anarchy by 10 points (`Mercenary_Tactical_Operation.md`)
            anarchy_impact: -10,
            infomorph_bonus: 350,
        };
        let n = pb::message("eve_public.sovereignty.mercenaryden.activity.api.AddedNotice")
            .set("id.uuid", uuid.clone())
            .set_with("activity", |attr| fill_activity_attributes(attr, &a))
            .set("solar_system.sequential", den.solar_system_id)
            .must_encode();
        push_notice(pending, den.solar_system_id, "eve_public.sovereignty.mercenaryden.activity.api.AddedNotice", n);
        snap.sov_runtime.mercenary_activities.insert(hex(&uuid), a);
    }
    if let Some(d) = snap.sov_runtime.mercenary_dens.get_mut(&den_id) {
        d.next_activity_generation_unix = now + ACTIVITY_GENERATION_SECS;
    }
}

fn owned_den_ids(snap: &WorldStateSnapshot, cid: i64) -> Vec<i64> {
    let mut ids: Vec<i64> = snap
        .sov_runtime
        .mercenary_dens
        .values()
        .filter(|d| d.owner_char_id == cid)
        .map(|d| d.den_id)
        .collect();
    ids.sort_unstable();
    ids
}

fn modify_level_handler(
    router: &PublicGatewayRouter,
    db: &Arc<WorldRepository>,
    req: &'static str,
    rsp: &'static str,
    development: bool,
) {
    let (db, pending) = (Arc::clone(db), Arc::clone(&router.pending_notices));
    router.register_handler(
        req,
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let den_id = required_id(dyn_msg, 1, "Mercenary den")?;
            let level = super::quasar_space_services::get_int_field(dyn_msg, 2).unwrap_or(0);
            if !(0..=i64::from(MAX_LEVEL)).contains(&level) {
                return Err((STATUS_BAD_REQUEST, format!("Level must be within 0..={MAX_LEVEL}")));
            }
            let sim = transact(&db, |snap| {
                tick_den(snap, &pending, den_id, ctx.now_unix());
                let den = snap
                    .sov_runtime
                    .mercenary_dens
                    .get_mut(&den_id)
                    .ok_or((STATUS_NOT_FOUND, format!("Mercenary den {den_id} not found")))?;
                if development {
                    den.development_level = level as u32;
                } else {
                    den.anarchy_level = level as u32;
                }
                push_evolution_notice(&pending, den);
                push_definitions_updated_notice(&pending, den);
                Ok::<_, RpcError>(encode_evolution_simulation(den))
            })?;
            Ok(pb::message(rsp).set_raw_message("simulation", sim).must_to_any())
        }),
    );
}

// V24.01 Revenant Mercenary Den evolution rates and MTO activity generation handlers.
/// Registers mercenary den, admin and activity handlers.
pub fn register_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    {
        let db = Arc::clone(&db);
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let cid = require_character(ctx)?;
                let mut resp =
                    pb::message("eve_public.sovereignty.mercenaryden.api.GetAllOwnedResponse");
                for id in db.with_snapshot(|snap| owned_den_ids(snap, cid)) {
                    resp = resp.add_with("id", |i| i.set("sequential", id as u64));
                }
                Ok(resp.must_to_any())
            }),
        );
    }
    {
        let db = Arc::clone(&db);
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let cid = require_character(ctx)?;
                let level = db.with_snapshot(|snap| {
                    snap.skills
                        .get(&cid)
                        .and_then(|s| s.get(&TYPE_MERCENARY_DEN_MANAGEMENT))
                        .map_or(0, |s| u64::from(s.level))
                });
                Ok(pb::message(
                    "eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterResponse",
                )
                .set("current_maximum", level.min(MERCENARY_DEN_ABSOLUTE_MAX) as u32)
                .set("absolute_maximum", MERCENARY_DEN_ABSOLUTE_MAX as u32)
                .must_to_any())
            }),
        );
    }
    {
        let (db, sde, pending) = (Arc::clone(&db), Arc::clone(&router.sde), Arc::clone(&router.pending_notices));
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let cid = require_character(ctx)?;
                let den_id = required_id(dyn_msg, 1, "Mercenary den")?;
                let (den, skyhook_owner) = transact(&db, |snap| {
                    owned_den(snap, cid, den_id)?;
                    tick_den_with_sde(snap, Some(&sde), &pending, den_id, ctx.now_unix());
                    let den = owned_den(snap, cid, den_id)?;
                    let owner = snap.structures.get(&den.skyhook_id).map(|s| s.owner_corp_id);
                    Ok::<_, RpcError>((den, owner))
                })?;
                let cap = infomorph_cargo_capacity(stage_for_level(den.development_level));
                Ok(pb::message("eve_public.sovereignty.mercenaryden.api.GetAsOwnerResponse")
                    .set_with("attributes", |attrs| {
                        attrs
                            .set("owner.sequential", den.owner_char_id as u64)
                            .set("skyhook.sequential", den.skyhook_id as u64)
                            .set("solar_system.sequential", den.solar_system_id)
                            .set("planet.sequential", den.planet_id as u64)
                            .set("type.sequential", den.type_id)
                    })
                    .set("enabled", den.enabled)
                    .set_with("evolution", |evo| {
                        evo.set_with("definition", fill_evolution_definition)
                            .set_with("simulation", |s| fill_evolution_simulation(s, &den))
                    })
                    .set_with("infomorphs", |info| {
                        info.set_with("definition", |d| {
                            fill_infomorphs_gen_definition(
                                d,
                                cap,
                                Some(&sde),
                                den.solar_system_id,
                                den.planet_id,
                            )
                        })
                        .set_with("contents", |c| {
                            c.set("infomorphs_count", den.infomorphs).set(
                                "last_generation_tick",
                                GoogleTimestamp::from_unix(den.simulated_at_unix as i64, 0),
                            )
                        })
                    })
                    .set("cargo_extraction_enabled", den.cargo_extraction_enabled)
                    .when_some(skyhook_owner, |b, corp| {
                        b.set("skyhook_owner.sequential", corp as u64)
                    })
                    .must_to_any())
            }),
        );
    }
    modify_level_handler(
        router,
        &db,
        "eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelRequest",
        "eve_public.sovereignty.mercenaryden.api.admin.ModifyDevelopmentLevelResponse",
        true,
    );
    modify_level_handler(
        router,
        &db,
        "eve_public.sovereignty.mercenaryden.api.admin.ModifyAnarchyLevelRequest",
        "eve_public.sovereignty.mercenaryden.api.admin.ModifyAnarchyLevelResponse",
        false,
    );
    {
        let (db, pending) = (Arc::clone(&db), Arc::clone(&router.pending_notices));
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let cid = require_character(ctx)?;
                let den_id = required_id(dyn_msg, 1, "Mercenary den")?;
                let (acts, next_gen) = transact(&db, |snap| {
                    owned_den(snap, cid, den_id)?;
                    refresh_activities(snap, &pending, den_id, ctx.now_unix());
                    let acts: Vec<PersistedMercenaryActivity> =
                        snap.sov_runtime.mercenary_activities.values().filter(|a| a.den_id == den_id).cloned().collect();
                    Ok::<_, RpcError>((acts, owned_den(snap, cid, den_id)?.next_activity_generation_unix))
                })?;
                let mut resp = pb::message(
                    "eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenResponse",
                );
                for a in &acts {
                    resp = resp.add_with("activities", |act| {
                        act.set("id.uuid", a.uuid.clone())
                            .set_with("attributes", |attr| fill_activity_attributes(attr, a))
                    });
                }
                Ok(resp
                    .set("next_generation_at", GoogleTimestamp::from_unix(next_gen as i64, 0))
                    .must_to_any())
            }),
        );
    }
    {
        let db = Arc::clone(&db);
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let cid = require_character(ctx)?;
                let now = ctx.now_unix();
                let acts = db.with_snapshot(|snap| {
                    let dens = owned_den_ids(snap, cid);
                    snap.sov_runtime
                        .mercenary_activities
                        .values()
                        .filter(|a| dens.contains(&a.den_id) && a.expiry_unix > now)
                        .cloned()
                        .collect::<Vec<_>>()
                });
                let mut resp =
                    pb::message("eve_public.sovereignty.mercenaryden.activity.api.GetAllResponse");
                for a in &acts {
                    resp = resp.add_with("activities", |act| {
                        act.set("id.uuid", a.uuid.clone())
                            .set_with("attributes", |attr| fill_activity_attributes(attr, a))
                    });
                }
                Ok(resp.must_to_any())
            }),
        );
    }
    {
        let (db, pending) = (Arc::clone(&db), Arc::clone(&router.pending_notices));
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.activity.api.StartRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let cid = require_character(ctx)?;
                let uuid = match dyn_msg.get(1) {
                    Some(DynamicValue::Message(id)) => match id.get(1) {
                        Some(DynamicValue::Bytes(b)) if !b.is_empty() => b.clone(),
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                };
                if uuid.is_empty() {
                    return Err((STATUS_BAD_REQUEST, "Activity identifier is required".into()));
                }
                let now = ctx.now_unix();
                let attrs = transact(&db, |snap| {
                    let key = hex(&uuid);
                    let a = snap
                        .sov_runtime
                        .mercenary_activities
                        .get(&key)
                        .cloned()
                        .ok_or((STATUS_NOT_FOUND, format!("Activity {key} not found")))?;
                    owned_den(snap, cid, a.den_id)?;
                    let ch = snap
                        .characters
                        .get(&cid)
                        .ok_or((STATUS_NOT_FOUND, format!("Character {cid} not found")))?;
                    if ch.solar_system_id != a.solar_system_id
                        || ch.station_id.is_some()
                        || ch.structure_id.is_some()
                    {
                        return Err((
                            STATUS_FORBIDDEN,
                            format!(
                                "Character {cid} must be undocked in solar system {} to start Mercenary Tactical Operation",
                                a.solar_system_id
                            ),
                        ));
                    }
                    if a.started {
                        return Err((STATUS_CONFLICT, format!("Activity {key} already started")));
                    }
                    if a.expiry_unix <= now {
                        return Err((STATUS_CONFLICT, format!("Activity {key} has expired")));
                    }
                    snap.next_entity_id += 1;
                    let site_ball_id = snap.next_entity_id;
                    let sys_id_u32 = a.solar_system_id;
                    snap.items.insert(
                        site_ball_id,
                        eve_db::InventoryItemRecord {
                            item_id: site_ball_id,
                            type_id: 24_690,
                            owner_id: 500_010,
                            location_id: i64::from(sys_id_u32),
                            quantity: 1,
                            singleton: true,
                            ..Default::default()
                        },
                    );
                    // Compute MTO tactical site position relative to the host den/skyhook anchor in space.
                    let den_pos = snap
                        .item_positions
                        .get(&a.den_id)
                        .copied()
                        .or_else(|| snap.structures.get(&a.den_id).map(|s| s.position))
                        .unwrap_or([250_000.0, 0.0, 0.0]);
                    let mto_pos = [den_pos[0] + 15_000.0, den_pos[1] + 5_000.0, den_pos[2]];
                    snap.item_positions.insert(site_ball_id, mto_pos);
                    snap.sov_runtime.mto_site_balls.insert(site_ball_id, key.clone());
                    let entry = snap.sov_runtime.mercenary_activities.get_mut(&key).ok_or((STATUS_NOT_FOUND, key.clone()))?;
                    let previous_expiry = entry.expiry_unix;
                    entry.started = true;
                    entry.started_by = cid;
                    entry.dungeon_id = (site_ball_id as u64 & 0x7FFF_FFFF) as u32;
                    entry.expiry_unix = now + 1_800;
                    push_activity_expiry_changed_notice(&pending, entry.solar_system_id, entry, previous_expiry);
                    let attrs = encode_activity_attributes(entry);
                    let n = pb::message("eve_public.sovereignty.mercenaryden.activity.api.StartedNotice")
                        .set("id.uuid", uuid.clone())
                        .set_raw_message("activity", &attrs)
                        .must_encode();
                    push_notice(&pending, entry.solar_system_id, "eve_public.sovereignty.mercenaryden.activity.api.StartedNotice", n);
                    Ok::<_, RpcError>(attrs)
                })?;
                Ok(pb::message("eve_public.sovereignty.mercenaryden.activity.api.StartResponse")
                    .set_raw_message("attributes", attrs)
                    .must_to_any())
            }),
        );
    }
    {
        let db = Arc::clone(&db);
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let cid = require_character(ctx)?;
                let capacity = db.with_snapshot(|snap| {
                    snap.sov_runtime
                        .mercenary_dens
                        .values()
                        .filter(|d| d.owner_char_id == cid)
                        .map(|d| activity_capacity(stage_for_level(d.development_level)))
                        .max()
                        .unwrap_or(0)
                });
                Ok(pb::message(
                    "eve_public.sovereignty.mercenaryden.activity.api.GetCapacityResponse",
                )
                .set("capacity", capacity)
                .must_to_any())
            }),
        );
    }
}

#[cfg(test)]
#[path = "quasar_sov_mercden_tests.rs"]
mod quasar_sov_mercden_tests;

#[path = "quasar_sov_mercden_cov_unit_tests.rs"]
#[cfg(test)]
mod quasar_sov_mercden_cov_unit_tests;

#[cfg(test)]
#[path = "quasar_sov_mercden_edges_tests.rs"]
mod quasar_sov_mercden_edges_tests;
