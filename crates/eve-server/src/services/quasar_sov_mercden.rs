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

/// SDE skill `Mercenary Den Management` (each trained level allows one more den).
pub const TYPE_MERCENARY_DEN_MANAGEMENT: u32 = 85_233;

// DERIVED (synthetic — the client mercenary den evolution / infomorph / activity-template FSD
// tables are not part of the EvoMu SDE import). Every rate below is a server balance constant,
// not CCP data: den cap, 1 development + 1 anarchy level per hour while enabled, five 20-level
// stages, a 2000-infomorph cargo cap, `3 * stage` infomorphs per hour, daily activity generation,
// 3-day activity lifetime, `2 + stage` concurrent activities and `5 * stage` activity impacts.
/// DERIVED (synthetic): absolute number of dens a character may own.
pub const MERCENARY_DEN_ABSOLUTE_MAX: u64 = 5;
/// DERIVED (synthetic): seconds per development / anarchy level gained while a den is enabled.
pub const EVOLUTION_UNIT_SECS: u64 = 3_600;
/// DERIVED (synthetic): seconds between Mercenary Tactical Operation generation passes.
pub const ACTIVITY_GENERATION_SECS: u64 = 86_400;
/// DERIVED (synthetic): lifetime of a generated (unstarted) operation.
pub const ACTIVITY_LIFETIME_SECS: u64 = 3 * 86_400;
const MAX_LEVEL: u32 = 100;
/// DERIVED (synthetic): levels per development / anarchy stage.
const STAGE_WIDTH: u32 = 20;
/// DERIVED (synthetic): infomorph cargo capacity.
const INFOMORPH_CARGO_CAPACITY: u32 = 2_000;

const EVOLUTION_NOTICE: &str = "eve_public.sovereignty.mercenaryden.api.EvolutionStateChangedNotice";

/// Development / anarchy stage (1..=5) for a level (0..=100).
pub fn stage_for_level(level: u32) -> u32 {
    (level / STAGE_WIDTH + 1).min(5)
}

/// DERIVED (synthetic): concurrent operations offered by a den at a development stage.
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
    Ok(den.clone())
}

/// Advances development, anarchy and infomorph generation for whole elapsed units.
/// Returns `true` when the development or anarchy stage changed.
pub fn simulate_den(den: &mut PersistedMercenaryDen, now: u64) -> bool {
    if den.simulated_at_unix == 0 || !den.enabled || now <= den.simulated_at_unix {
        if den.simulated_at_unix == 0 || !den.enabled {
            den.simulated_at_unix = now;
        }
        return false;
    }
    let units = (now - den.simulated_at_unix) / EVOLUTION_UNIT_SECS;
    if units == 0 {
        return false;
    }
    let before = (stage_for_level(den.development_level), stage_for_level(den.anarchy_level));
    den.simulated_at_unix += units * EVOLUTION_UNIT_SECS;
    let units = units.min(u64::from(MAX_LEVEL)) as u32;
    den.development_level = (den.development_level + units).min(MAX_LEVEL);
    den.anarchy_level = (den.anarchy_level + units).min(MAX_LEVEL);
    let per_unit = 3 * stage_for_level(den.development_level);
    den.infomorphs = (den.infomorphs + per_unit * units).min(INFOMORPH_CARGO_CAPACITY);
    before != (stage_for_level(den.development_level), stage_for_level(den.anarchy_level))
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
    for stage in 1..=5u32 {
        let lower = (stage - 1) * STAGE_WIDTH;
        let upper = if stage == 5 { MAX_LEVEL } else { stage * STAGE_WIDTH - 1 };
        let mut s = Vec::new();
        encode_varint_field(1, u64::from(stage), &mut s);
        encode_varint_field(2, u64::from(lower), &mut s);
        encode_varint_field(3, u64::from(upper), &mut s);
        encode_bytes_field(2, &s, &mut dev);
        encode_varint_field(4, 500 * u64::from(stage), &mut s);
        encode_bytes_field(2, &s, &mut anarchy);
    }
    let mut b = Vec::new();
    encode_bytes_field(1, &dev, &mut b);
    encode_bytes_field(2, &anarchy, &mut b);
    b
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
    let mut info_def = Vec::new();
    encode_varint_field(2, EVOLUTION_UNIT_SECS, &mut info_def);
    encode_varint_field(4, u64::from(INFOMORPH_CARGO_CAPACITY), &mut info_def);
    encode_bytes_field(3, &info_def, &mut n);
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
    if let Some(den) = snap.sov_runtime.mercenary_dens.get_mut(&den_id) {
        if simulate_den(den, now) {
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

/// Expires stale operations and, when the generation timer elapsed, fills the den up to its
/// capacity — emitting `RemovedNotice` / `AddedNotice` for each change.
pub(crate) fn refresh_activities(snap: &mut WorldStateSnapshot, pending: &PendingNotices, den_id: i64, now: u64) {
    tick_den(snap, pending, den_id, now);
    let Some(den) = snap.sov_runtime.mercenary_dens.get(&den_id).cloned() else { return };
    let expired: Vec<String> = snap
        .sov_runtime
        .mercenary_activities
        .iter()
        .filter(|(_, a)| a.den_id == den_id && !a.started && a.expiry_unix <= now)
        .map(|(k, _)| k.clone())
        .collect();
    for key in expired {
        if let Some(a) = snap.sov_runtime.mercenary_activities.remove(&key) {
            let mut n = Vec::new();
            id_field(1, den_id as u64, &mut n);
            encode_bytes_field(2, &encode_activity_id(&a.uuid), &mut n);
            push_notice(pending, den.solar_system_id, "eve_public.sovereignty.mercenaryden.activity.api.RemovedNotice", n);
        }
    }
    let completed: Vec<String> = snap
        .sov_runtime
        .mercenary_activities
        .iter()
        .filter(|(_, a)| a.den_id == den_id && a.started && a.expiry_unix <= now)
        .map(|(k, _)| k.clone())
        .collect();
    for key in completed {
        if let Some(a) = snap.sov_runtime.mercenary_activities.remove(&key) {
            if let Some(den_mut) = snap.sov_runtime.mercenary_dens.get_mut(&den_id) {
                den_mut.development_level = (den_mut.development_level as i32 + a.development_impact).clamp(0, MAX_LEVEL as i32) as u32;
                den_mut.anarchy_level = (den_mut.anarchy_level as i32 + a.anarchy_impact).clamp(0, MAX_LEVEL as i32) as u32;
                den_mut.infomorphs = den_mut.infomorphs.saturating_add(a.infomorph_bonus);
                push_evolution_notice(pending, den_mut);
            }
            push_activity_completed_notice(
                pending,
                den.solar_system_id,
                den_id,
                &a.uuid,
                a.development_impact,
                a.anarchy_impact,
                a.infomorph_bonus,
            );
        }
    }
    if !den.enabled || now < den.next_activity_generation_unix {
        return;
    }
    let stage = stage_for_level(den.development_level);
    let mut count = snap.sov_runtime.mercenary_activities.values().filter(|a| a.den_id == den_id).count() as u32;
    while count < activity_capacity(stage) {
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
            development_impact: 5 * stage as i32,
            anarchy_impact: -(5 * stage as i32),
            infomorph_bonus: 10 * stage,
        };
        let mut n = Vec::new();
        encode_bytes_field(1, &encode_activity_id(&uuid), &mut n);
        encode_bytes_field(2, &encode_activity_attributes(&a), &mut n);
        id_field(3, u64::from(den.solar_system_id), &mut n);
        push_notice(pending, den.solar_system_id, "eve_public.sovereignty.mercenaryden.activity.api.AddedNotice", n);
        snap.sov_runtime.mercenary_activities.insert(hex(&uuid), a);
        count += 1;
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
                let (mut info_def, mut contents, mut infomorphs) = (Vec::new(), Vec::new(), Vec::new());
                encode_varint_field(2, EVOLUTION_UNIT_SECS, &mut info_def);
                encode_varint_field(4, u64::from(INFOMORPH_CARGO_CAPACITY), &mut info_def);
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
        let (db, pending) = (Arc::clone(&db), Arc::clone(&router.pending_notices));
        router.register_handler(
            "eve_public.sovereignty.mercenaryden.activity.api.GetAllRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let cid = require_character(ctx)?;
                let acts = db.with_mut_snapshot(|snap| {
                    let dens = owned_den_ids(snap, cid);
                    for den_id in &dens {
                        refresh_activities(snap, &pending, *den_id, ctx.now_unix());
                    }
                    snap.sov_runtime
                        .mercenary_activities
                        .values()
                        .filter(|a| dens.contains(&a.den_id))
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
                    if a.started {
                        return Err((STATUS_CONFLICT, format!("Activity {key} already started")));
                    }
                    if a.expiry_unix <= now {
                        return Err((STATUS_CONFLICT, format!("Activity {key} has expired")));
                    }
                    let entry = snap.sov_runtime.mercenary_activities.get_mut(&key).ok_or((STATUS_NOT_FOUND, key.clone()))?;
                    let previous_expiry = entry.expiry_unix;
                    entry.started = true;
                    entry.started_by = cid;
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
