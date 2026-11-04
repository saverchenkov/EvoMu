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

use super::quasar_sov_hub::{any, id_field, push_notice, required_id, transact, ts_field, PendingNotices, RpcError};
use crate::quasar_gateway::{
    require_admin_role, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_CONFLICT, STATUS_FORBIDDEN,
    STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use eve_db::{PersistedMercenaryActivity, PersistedMercenaryDen, WorldRepository, WorldStateSnapshot};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field, AuthoritativeContext, DynamicValue};
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
/// DERIVED (synthetic — deterministic 72h cadence matching `Mercenary_Tactical_Operation.md:14`
/// "MTOs spawn approximately every 3 days from the time they are onlined"): seconds between MTO generation passes.
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

/// DERIVED (synthetic deterministic lower bound of each stage's `Mercenary_Tactical_Operation.md:28-34`
/// range: `65-100`, `68-105`, `71-110`, `74-115`, `78-120` IDK/hr across stages `1..=5`; exact server
/// per-planet formula is not in `.fsdbinary`).
pub fn infomorphs_per_hour(dev_stage: u32) -> u32 {
    match dev_stage {
        1 => 65,
        2 => 68,
        3 => 71,
        4 => 74,
        _ => 78,
    }
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

/// DERIVED (synthetic — server-only MTO slot capacity formula is not in `.fsdbinary`):
/// concurrent operations offered by a den at a development stage (`3..=7` across stages `1..=5`).
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
        let rate = infomorphs_per_hour(stage_for_level(den.development_level));
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
        let rate = infomorphs_per_hour(stage_for_level(den.development_level));
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
    let Some(den) = sov_rt.mercenary_dens.get_mut(&den_id) else {
        return false;
    };
    let rem = sov_rt.mercenary_den_evo_rem.entry(den_id).or_insert(0);
    simulate_den_with_rem(den, rem, now)
}

fn encode_level_sim(level: u32, simulated_at: u64) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, u64::from(level), &mut b);
    encode_varint_field(2, u64::from(stage_for_level(level)), &mut b);
    ts_field(4, simulated_at, &mut b);
    b
}

/// Encodes `eve_public.sovereignty.mercenaryden.EvolutionSimulation`.
pub fn encode_evolution_simulation(den: &PersistedMercenaryDen) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(if den.enabled { 2 } else { 1 }, 1, &mut b);
    encode_bytes_field(3, &encode_level_sim(den.development_level, den.simulated_at_unix), &mut b);
    encode_bytes_field(4, &encode_level_sim(den.anarchy_level, den.simulated_at_unix), &mut b);
    b
}

fn encode_evolution_definition() -> Vec<u8> {
    let (mut dev, mut anarchy) = (Vec::new(), Vec::new());
    encode_varint_field(1, EVOLUTION_UNIT_SECS, &mut dev);
    encode_varint_field(1, EVOLUTION_UNIT_SECS, &mut anarchy);
    for (idx, &(lower, upper)) in STAGE_BOUNDS.iter().enumerate() {
        let stage = (idx as u32) + 1;
        let mut s = Vec::new();
        encode_varint_field(1, u64::from(stage), &mut s);
        encode_varint_field(2, u64::from(lower), &mut s);
        encode_varint_field(3, u64::from(upper), &mut s);
        encode_bytes_field(2, &s, &mut dev);
        encode_varint_field(4, anarchy_workforce_disruption_pct(stage), &mut s);
        encode_bytes_field(2, &s, &mut anarchy);
    }
    let mut b = Vec::new();
    encode_bytes_field(1, &dev, &mut b);
    encode_bytes_field(2, &anarchy, &mut b);
    b
}

fn encode_infomorphs_gen_definition(cap: u32) -> Vec<u8> {
    let mut info_def = Vec::new();
    id_field(1, u64::from(TYPE_INFOMORPH_DECRYPTION_KEY), &mut info_def);
    encode_varint_field(2, INFOMORPH_UNIT_SECS, &mut info_def);
    for stage in 1..=5u32 {
        let rate = infomorphs_per_hour(stage);
        let mut band = Vec::new();
        encode_varint_field(1, u64::from(stage), &mut band);
        encode_varint_field(2, u64::from(rate), &mut band);
        encode_varint_field(3, u64::from(rate), &mut band);
        encode_bytes_field(3, &band, &mut info_def);
    }
    encode_varint_field(4, u64::from(cap), &mut info_def);
    info_def
}

fn push_evolution_notice(pending: &PendingNotices, den: &PersistedMercenaryDen) {
    let mut n = Vec::new();
    id_field(1, den.den_id as u64, &mut n);
    encode_bytes_field(2, &encode_evolution_simulation(den), &mut n);
    push_notice(pending, den.solar_system_id, EVOLUTION_NOTICE, n);
}

pub fn push_definitions_updated_notice(pending: &PendingNotices, den: &PersistedMercenaryDen) {
    let mut n = Vec::new();
    id_field(1, den.den_id as u64, &mut n);
    encode_bytes_field(2, &encode_evolution_definition(), &mut n);
    encode_bytes_field(3, &encode_infomorphs_gen_definition(INFOMORPH_CARGO_CAPACITY), &mut n);
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
    let mut n = Vec::new();
    id_field(1, den_id as u64, &mut n);
    encode_bytes_field(2, &encode_activity_id(uuid), &mut n);
    encode_varint_field(3, dev_impact as i64 as u64, &mut n);
    encode_varint_field(4, anarchy_impact as i64 as u64, &mut n);
    encode_varint_field(5, u64::from(infomorph_bonus), &mut n);
    push_notice(pending, solar_system_id, "eve_public.sovereignty.mercenaryden.activity.api.CompletedNotice", n);
}

pub fn push_activity_expiry_changed_notice(
    pending: &PendingNotices,
    solar_system_id: u32,
    activity: &PersistedMercenaryActivity,
    previous_expiry: u64,
) {
    let mut n = Vec::new();
    encode_bytes_field(1, &encode_activity_id(&activity.uuid), &mut n);
    encode_bytes_field(2, &encode_activity_attributes(activity), &mut n);
    ts_field(3, previous_expiry, &mut n);
    push_notice(pending, solar_system_id, "eve_public.sovereignty.mercenaryden.activity.api.ExpiryChangedNotice", n);
}

/// Simulates a den in place and emits `EvolutionStateChangedNotice` on a stage change.
fn tick_den(snap: &mut WorldStateSnapshot, pending: &PendingNotices, den_id: i64, now: u64) {
    if simulate_den_in_snap(&mut snap.sov_runtime, den_id, now) {
        if let Some(den) = snap.sov_runtime.mercenary_dens.get(&den_id) {
            push_evolution_notice(pending, den);
        }
    }
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Encodes `eve_public.sovereignty.mercenaryden.activity.Attributes`.
pub fn encode_activity_attributes(a: &PersistedMercenaryActivity) -> Vec<u8> {
    let mut tmpl = Vec::new();
    if a.dungeon_id > 0 {
        id_field(3, u64::from(a.dungeon_id), &mut tmpl);
    }
    encode_varint_field(4, i64::from(a.development_impact) as u64, &mut tmpl);
    encode_varint_field(5, i64::from(a.anarchy_impact) as u64, &mut tmpl);
    encode_varint_field(6, u64::from(a.infomorph_bonus), &mut tmpl);
    let mut b = Vec::new();
    encode_bytes_field(1, &tmpl, &mut b);
    id_field(2, a.den_id as u64, &mut b);
    encode_varint_field(3, u64::from(a.started), &mut b);
    ts_field(4, a.expiry_unix, &mut b);
    id_field(5, u64::from(a.solar_system_id), &mut b);
    b
}

fn encode_activity_id(uuid: &[u8]) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(1, uuid, &mut b);
    b
}

fn encode_activity_entry(a: &PersistedMercenaryActivity) -> Vec<u8> {
    let mut b = Vec::new();
    encode_bytes_field(1, &encode_activity_id(&a.uuid), &mut b);
    encode_bytes_field(2, &encode_activity_attributes(a), &mut b);
    b
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
            let mut n = Vec::new();
            id_field(1, den_id as u64, &mut n);
            encode_bytes_field(2, &encode_activity_id(&a.uuid), &mut n);
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
            // DERIVED: synthetic MTO anarchy delta (-10) pending V24.01 client/SDE MTO tables
            anarchy_impact: -10,
            infomorph_bonus: 350,
        };
        let mut n = Vec::new();
        encode_bytes_field(1, &encode_activity_id(&uuid), &mut n);
        encode_bytes_field(2, &encode_activity_attributes(&a), &mut n);
        id_field(3, u64::from(den.solar_system_id), &mut n);
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
            let mut out = Vec::new();
            encode_bytes_field(1, &sim, &mut out);
            Ok(any(rsp, out))
        }),
    );
}

// DERIVED (synthetic): DATA GAP — Mercenary Den evolution rates and activity generation parameters are server-only tables not shipped in V24 client FSD.
/// Registers mercenary den, admin and activity handlers.
pub fn register_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    {
        let db = Arc::clone(&db);
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.api.GetAllOwnedRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let cid = require_character(ctx)?;
                let mut out = Vec::new();
                for id in db.with_snapshot(|snap| owned_den_ids(snap, cid)) {
                    id_field(1, id as u64, &mut out);
                }
                Ok(any("eve_public.sovereignty.mercenaryden.api.GetAllOwnedResponse", out))
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
                let mut out = Vec::new();
                encode_varint_field(1, level.min(MERCENARY_DEN_ABSOLUTE_MAX), &mut out);
                encode_varint_field(2, MERCENARY_DEN_ABSOLUTE_MAX, &mut out);
                Ok(any("eve_public.sovereignty.mercenaryden.api.GetMaximumForCharacterResponse", out))
            }),
        );
    }
    {
        let (db, pending) = (Arc::clone(&db), Arc::clone(&router.pending_notices));
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.api.GetAsOwnerRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let cid = require_character(ctx)?;
                let den_id = required_id(dyn_msg, 1, "Mercenary den")?;
                let (den, skyhook_owner) = transact(&db, |snap| {
                    owned_den(snap, cid, den_id)?;
                    tick_den(snap, &pending, den_id, ctx.now_unix());
                    let den = owned_den(snap, cid, den_id)?;
                    let owner = snap.structures.get(&den.skyhook_id).map(|s| s.owner_corp_id);
                    Ok::<_, RpcError>((den, owner))
                })?;
                let mut attrs = Vec::new();
                id_field(1, den.owner_char_id as u64, &mut attrs);
                id_field(2, den.skyhook_id as u64, &mut attrs);
                id_field(3, u64::from(den.solar_system_id), &mut attrs);
                id_field(4, den.planet_id as u64, &mut attrs);
                id_field(5, u64::from(den.type_id), &mut attrs);
                let mut evo = Vec::new();
                encode_bytes_field(1, &encode_evolution_definition(), &mut evo);
                encode_bytes_field(2, &encode_evolution_simulation(&den), &mut evo);
                let (mut contents, mut infomorphs) = (Vec::new(), Vec::new());
                let cap = infomorph_cargo_capacity(stage_for_level(den.development_level));
                let info_def = encode_infomorphs_gen_definition(cap);
                encode_varint_field(1, u64::from(den.infomorphs), &mut contents);
                ts_field(2, den.simulated_at_unix, &mut contents);
                encode_bytes_field(1, &info_def, &mut infomorphs);
                encode_bytes_field(2, &contents, &mut infomorphs);
                let mut out = Vec::new();
                encode_bytes_field(1, &attrs, &mut out);
                encode_varint_field(2, u64::from(den.enabled), &mut out);
                encode_bytes_field(3, &evo, &mut out);
                encode_bytes_field(4, &infomorphs, &mut out);
                encode_varint_field(5, u64::from(den.cargo_extraction_enabled), &mut out);
                if let Some(corp) = skyhook_owner {
                    id_field(6, corp as u64, &mut out);
                }
                Ok(any("eve_public.sovereignty.mercenaryden.api.GetAsOwnerResponse", out))
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
                let mut out = Vec::new();
                for a in &acts {
                    encode_bytes_field(1, &encode_activity_entry(a), &mut out);
                }
                ts_field(2, next_gen, &mut out);
                Ok(any("eve_public.sovereignty.mercenaryden.activity.api.GetForMercenaryDenResponse", out))
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
                let mut out = Vec::new();
                for a in &acts {
                    encode_bytes_field(1, &encode_activity_entry(a), &mut out);
                }
                Ok(any("eve_public.sovereignty.mercenaryden.activity.api.GetAllResponse", out))
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
                    // DERIVED (synthetic): derive MTO tactical site position relative to the host den/skyhook anchor in space.
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
                    let mut n = Vec::new();
                    encode_bytes_field(1, &encode_activity_id(&uuid), &mut n);
                    encode_bytes_field(2, &attrs, &mut n);
                    push_notice(&pending, entry.solar_system_id, "eve_public.sovereignty.mercenaryden.activity.api.StartedNotice", n);
                    Ok::<_, RpcError>(attrs)
                })?;
                let mut out = Vec::new();
                encode_bytes_field(1, &attrs, &mut out);
                Ok(any("eve_public.sovereignty.mercenaryden.activity.api.StartResponse", out))
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
                let mut out = Vec::new();
                encode_varint_field(1, u64::from(capacity), &mut out);
                Ok(any("eve_public.sovereignty.mercenaryden.activity.api.GetCapacityResponse", out))
            }),
        );
    }
}
