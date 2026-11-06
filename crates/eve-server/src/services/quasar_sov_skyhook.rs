//! `quasar_sov_skyhook` — state-backed Equinox Orbital Skyhook query handlers (`Q-8`).
//!
//! Backs `eve_public.sovereignty.skyhook.api.GetRequest`, `GetAllLocalRequest`,
//! `GetAllByCorporationRequest`, `GetSolarSystemsWithTheftVulnerableSkyhooksRequest` and
//! `GetTheftVulnerableSkyhooksInSolarSystemRequest` with skyhook structures
//! (`TYPE_ORBITAL_SKYHOOK`) plus `sov_runtime.skyhook_reagents` / `skyhook_theft_windows`.
//! No skyhook, system or reagent is synthesized: unknown skyhooks are `NOT_FOUND` and empty
//! queries return empty lists.

#![forbid(unsafe_code)]

use super::quasar_sov_hub::{any, id_field, required_id, ts_field, RpcError};
use super::sov_services::TYPE_ORBITAL_SKYHOOK;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_NOT_FOUND, STATUS_UNAUTHORIZED};
use eve_db::{WorldRepository, WorldStateSnapshot};
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};
use eve_sde::SdeRegistry;
use std::collections::BTreeSet;
use std::sync::Arc;

fn is_skyhook(snap: &WorldStateSnapshot, id: i64) -> bool {
    snap.structures.get(&id).is_some_and(|s| s.type_id == TYPE_ORBITAL_SKYHOOK)
}

/// `(start, end)` of the skyhook's scheduled theft window when it is open at `now`.
fn open_window(snap: &WorldStateSnapshot, id: i64, now: u64) -> Option<(u64, u64)> {
    snap.sov_runtime
        .skyhook_theft_windows
        .get(&id)
        .copied()
        .filter(|(start, end)| *start <= now && now < *end)
}

/// Computes V24 per-planet-type hourly reagent harvest yield from SDE celestial type and radius.
pub fn planet_hourly_reagent_yield(
    sde: &SdeRegistry,
    planet_id: Option<i64>,
    sys_id: Option<u32>,
    _reagent_type: u32,
) -> u64 {
    let fallback = super::sov_gameplay::SKYHOOK_HOURLY_REAGENT_YIELD;
    let (Some(pid), Some(sys)) = (planet_id, sys_id) else {
        return fallback;
    };
    let Some(planet) = sde.celestials_in_system(sys).into_iter().find(|c| c.celestial_id == pid) else {
        return fallback;
    };
    let (base, min_y, max_y) = match planet.type_id {
        12 => (1_200.0, 900, 1_800),     // Ice -> Superionic Ice
        2015 => (1_000.0, 750, 1_500),   // Lava -> Magmatic Gas
        2014 | 2063 => (600.0, 450, 900),
        2017 | 13 => (400.0, 300, 600),
        _ => return fallback,
    };
    if planet.radius > 0.0 {
        let factor = (planet.radius / 5_000_000.0).clamp(0.75, 1.5);
        ((base * factor).round() as u64).clamp(min_y, max_y)
    } else {
        base as u64
    }
}

/// Encodes the shared skyhook body: `skyhook(1)`, `reagent_simulations(2)`, `active(3)`,
/// `theft_vulnerability(5)` and `workforce(7)`.
fn encode_skyhook(snap: &WorldStateSnapshot, sde: &SdeRegistry, id: i64, now: u64) -> Vec<u8> {
    let mut b = Vec::new();
    id_field(1, id as u64, &mut b);
    let planet_id = snap.sov_runtime.skyhook_planets.get(&id).copied();
    let sys_id = snap.structures.get(&id).map(|s| s.solar_system_id);
    if let Some((reagent_type, stock)) = snap.sov_runtime.skyhook_reagents.get(&id).copied() {
        let (sec_stock, insec_stock, sim_ts) = snap
            .sov_runtime
            .skyhook_reagent_bays
            .get(&id)
            .copied()
            .map(|(_, sec, insec, ts)| (sec, insec, if ts > 0 { ts } else { now }))
            .unwrap_or((stock, stock / 2, now));
        let mut sim = Vec::new();
        encode_varint_field(1, sec_stock, &mut sim);
        encode_varint_field(2, insec_stock, &mut sim);
        ts_field(3, sim_ts, &mut sim);
        let mut r = Vec::new();
        id_field(1, u64::from(reagent_type), &mut r);
        encode_bytes_field(2, &sim, &mut r);
        encode_bytes_field(4, &sim, &mut r);
        encode_bytes_field(2, &r, &mut b);

        let hourly_yield = planet_hourly_reagent_yield(sde, planet_id, sys_id, reagent_type);
        let mut r_def = Vec::new();
        id_field(1, u64::from(reagent_type), &mut r_def);
        let mut amt = Vec::new();
        encode_varint_field(1, hourly_yield, &mut amt);
        encode_varint_field(2, 3_600, &mut amt);
        encode_bytes_field(2, &amt, &mut r_def);
        encode_bytes_field(4, &r_def, &mut b);
    }
    let active = snap.sov_runtime.skyhook_active.get(&id).copied().unwrap_or(true);
    encode_varint_field(3, u64::from(active), &mut b);
    let mut theft = Vec::new();
    match snap.sov_runtime.skyhook_theft_windows.get(&id).copied() {
        Some((start, end)) => {
            encode_varint_field(1, u64::from(start <= now && now < end), &mut theft);
            ts_field(2, start, &mut theft);
            ts_field(3, end, &mut theft);
        }
        None => encode_varint_field(1, 0, &mut theft),
    }
    encode_bytes_field(5, &theft, &mut b);
    if let Some(pid) = planet_id {
        id_field(6, pid as u64, &mut b);
    }
    let wf_amount = match (planet_id, sys_id) {
        (Some(pid), Some(sys_id)) if active => sde
            .celestials_in_system(sys_id)
            .into_iter()
            .find(|c| c.celestial_id == pid)
            .map(|c| super::quasar_sov_hub::planet_yield(c.type_id).1)
            .unwrap_or(0),
        _ => 0,
    };
    let mut wf = Vec::new();
    if wf_amount > 0 {
        encode_varint_field(1, wf_amount, &mut wf);
    } else {
        encode_varint_field(2, 1, &mut wf);
    }
    encode_bytes_field(7, &wf, &mut b);
    b
}

fn sorted_skyhooks(snap: &WorldStateSnapshot, pred: impl Fn(&eve_db::UpwellStructureRecord) -> bool) -> Vec<i64> {
    let mut ids: Vec<i64> = snap
        .structures
        .values()
        .filter(|s| s.type_id == TYPE_ORBITAL_SKYHOOK && pred(s))
        .map(|s| s.structure_id)
        .collect();
    ids.sort_unstable();
    ids
}

/// Registers skyhook query handlers.
pub fn register_query_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>, sde: Arc<SdeRegistry>) {
    {
        let db = Arc::clone(&db);
        let sde = Arc::clone(&sde);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let id = required_id(dyn_msg, 1, "Skyhook")?;
                let now = ctx.now_unix();
                let body = db.try_with_mut_snapshot(|snap| {
                    if !is_skyhook(snap, id) {
                        return Err::<Vec<u8>, RpcError>((STATUS_NOT_FOUND, format!("Skyhook {id} not found")));
                    }
                    let _ = super::sov_gameplay::advance_skyhook_reagents_in_snap(snap, id, now);
                    Ok(encode_skyhook(snap, &sde, id, now))
                })?;
                Ok(any("eve_public.sovereignty.skyhook.api.GetResponse", body))
            }),
        );
    }
    {
        let db = Arc::clone(&db);
        let sde = Arc::clone(&sde);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetAllLocalRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let cid = ctx
                    .effective_character_id()
                    .ok_or((STATUS_UNAUTHORIZED, "Authenticated character required".to_string()))? as i64;
                let out = db.with_snapshot(|snap| {
                    let sys = snap
                        .characters
                        .get(&cid)
                        .map(|c| c.solar_system_id)
                        .ok_or((STATUS_NOT_FOUND, format!("Character {cid} not found")))?;
                    let mut out = Vec::new();
                    id_field(1, u64::from(sys), &mut out);
                    for id in sorted_skyhooks(snap, |s| s.solar_system_id == sys) {
                        encode_bytes_field(2, &encode_skyhook(snap, &sde, id, ctx.now_unix()), &mut out);
                    }
                    Ok::<_, RpcError>(out)
                })?;
                Ok(any("eve_public.sovereignty.skyhook.api.GetAllLocalResponse", out))
            }),
        );
    }
    {
        let db = Arc::clone(&db);
        let sde = Arc::clone(&sde);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetAllByCorporationRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let corp = required_id(dyn_msg, 1, "Corporation")?;
                let out = db.with_snapshot(|snap| {
                    let mut out = Vec::new();
                    for id in sorted_skyhooks(snap, |s| s.owner_corp_id == corp) {
                        encode_bytes_field(1, &encode_skyhook(snap, &sde, id, ctx.now_unix()), &mut out);
                    }
                    out
                });
                Ok(any("eve_public.sovereignty.skyhook.api.GetAllByCorporationResponse", out))
            }),
        );
    }
    {
        let db = Arc::clone(&db);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let now = ctx.now_unix();
                let systems: BTreeSet<u32> = db.with_snapshot(|snap| {
                    sorted_skyhooks(snap, |_| true)
                        .into_iter()
                        .filter(|id| open_window(snap, *id, now).is_some())
                        .filter_map(|id| snap.structures.get(&id).map(|s| s.solar_system_id))
                        .collect()
                });
                let mut out = Vec::new();
                for s in systems {
                    id_field(1, u64::from(s), &mut out);
                }
                Ok(any("eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksResponse", out))
            }),
        );
    }
    {
        let db = Arc::clone(&db);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let sys = required_id(dyn_msg, 1, "Solar system")? as u32;
                let now = ctx.now_unix();
                let out = db.with_snapshot(|snap| {
                    let mut out = Vec::new();
                    for id in sorted_skyhooks(snap, |s| s.solar_system_id == sys) {
                        if let Some((start, end)) = open_window(snap, id, now) {
                            let mut v = Vec::new();
                            id_field(1, id as u64, &mut v);
                            ts_field(3, end, &mut v);
                            ts_field(4, start, &mut v);
                            encode_bytes_field(1, &v, &mut out);
                        }
                    }
                    out
                });
                Ok(any("eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemResponse", out))
            }),
        );
    }
}

#[cfg(test)]
#[path = "quasar_sov_skyhook_tests.rs"]
mod quasar_sov_skyhook_tests;

#[path = "quasar_sov_skyhook_cov_unit_tests.rs"]
#[cfg(test)]
mod quasar_sov_skyhook_cov_unit_tests;


