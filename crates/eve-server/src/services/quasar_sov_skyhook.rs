//! `quasar_sov_skyhook` — state-backed Equinox Orbital Skyhook query handlers (`Q-8`).
//!
//! Backs `eve_public.sovereignty.skyhook.api.GetRequest`, `GetAllLocalRequest`,
//! `GetAllByCorporationRequest`, `GetSolarSystemsWithTheftVulnerableSkyhooksRequest` and
//! `GetTheftVulnerableSkyhooksInSolarSystemRequest` with skyhook structures
//! (`TYPE_ORBITAL_SKYHOOK`) plus `sov_runtime.skyhook_reagents` / `skyhook_theft_windows`.
//! No skyhook, system or reagent is synthesized: unknown skyhooks are `NOT_FOUND` and empty
//! queries return empty lists.

#![forbid(unsafe_code)]

use super::quasar_sov_hub::{
    any, caller, id_field, require_caller_roles, required_id, RpcError,
};
use super::sov_services::TYPE_ORBITAL_SKYHOOK;
use crate::quasar_gateway::{
    require_admin_role, PublicGatewayRouter, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use eve_db::{corp_roles, WorldRepository, WorldStateSnapshot};
use eve_proto::quasar::{GoogleTimestamp, WireField, WireMessage, WireValue};
use eve_sde::SdeRegistry;
use std::collections::BTreeSet;
use std::sync::Arc;

fn is_active_skyhook_struct(s: &eve_db::UpwellStructureRecord) -> bool {
    s.type_id == TYPE_ORBITAL_SKYHOOK && !s.state.eq_ignore_ascii_case("destroyed")
}

fn is_skyhook(snap: &WorldStateSnapshot, id: i64) -> bool {
    snap.structures.get(&id).is_some_and(is_active_skyhook_struct)
}

fn advance_skyhook_state_in_snap(
    snap: &mut WorldStateSnapshot,
    sde: &SdeRegistry,
    id: i64,
    now: u64,
) {
    let _ = super::sov_gameplay::advance_skyhook_reagents_in_snap(snap, sde, id, now);
    let _ = super::sov_gameplay::sov_gameplay_skyhook::advance_skyhook_theft_window_in_snap(
        snap, id, now,
    );
}

/// `(start, end)` of the skyhook's scheduled theft window when active and open at `now`.
fn open_window(snap: &WorldStateSnapshot, id: i64, now: u64) -> Option<(u64, u64)> {
    if !is_skyhook(snap, id) {
        return None;
    }
    snap.sov_runtime
        .skyhook_theft_windows
        .get(&id)
        .copied()
        .filter(|(start, end)| *start <= now && now < *end)
}

/// Nominal hourly reagent harvest yield by planet `type_id` prior to radius scaling.
///
/// `// DERIVED:` Calibrated directly to V24 client `orbitalSkyhook/resourceRichness.pyj`
/// `PLANET_ICE = (36, 40)` and `PLANET_LAVA = (763, 946)` average cutoffs; non-reagent planets yield `0`.
pub fn planet_type_base_hourly_reagent_yield(type_id: u32) -> u64 {
    match type_id {
        12 => 36,
        2015 => 763,
        _ => 0,
    }
}

/// Computes radius-scaled V24 hourly reagent harvest rate directly from `(type_id, radius_m)`.
///
/// `// DERIVED:` Log-radius curve fitted to V24 client `orbitalSkyhook/const.pyj` size classes
/// (`ICE`: `1.048M`..`18.9M`; `LAVA`: `1.46M`..`4.16M`) and `resourceRichness.pyj` cutoffs
/// (`PLANET_ICE = (36, 40)`, `PLANET_LAVA = (763, 946)`), falling back to nominal averages when
/// `radius_m <= 0`. Non-reagent planets (including Oceanic `2014`) yield `0`.
pub fn planet_hourly_reagent_yield_for_planet(type_id: u32, radius_m: f64) -> u64 {
    if radius_m <= 0.0 || !radius_m.is_finite() {
        return planet_type_base_hourly_reagent_yield(type_id);
    }
    match type_id {
        12 => {
            let t = ((radius_m.ln() - 1_048_577.0f64.ln())
                / (18_900_000.0f64.ln() - 1_048_577.0f64.ln()))
            .clamp(0.0, 1.35);
            (22.0 + 18.0 * t).round() as u64
        }
        2015 => {
            let t = ((radius_m.ln() - 1_460_000.0f64.ln())
                / (4_160_000.0f64.ln() - 1_460_000.0f64.ln()))
            .clamp(0.0, 1.4);
            (520.0 + 426.0 * t).round() as u64
        }
        _ => 0,
    }
}

/// Computes V24 per-planet hourly reagent harvest yield from SDE celestial type and radius.
pub fn planet_hourly_reagent_yield(
    sde: &SdeRegistry,
    planet_id: Option<i64>,
    sys_id: Option<u32>,
    _reagent_type: u32,
) -> u64 {
    let (Some(pid), Some(sys)) = (planet_id, sys_id) else {
        return 0;
    };
    let Some(planet) = sde
        .celestials_in_system_slice(sys)
        .iter()
        .find(|c| c.celestial_id == pid)
    else {
        return 0;
    };
    planet_hourly_reagent_yield_for_planet(planet.type_id, planet.radius)
}

fn wire_submsg_u64(field_number: u32, value: u64) -> WireField {
    WireField {
        field_number,
        value: WireValue::LengthDelimited(
            WireMessage {
                fields: vec![WireField {
                    field_number: 1,
                    value: WireValue::Varint(value),
                }],
            }
            .encode(),
        ),
    }
}

#[derive(Clone, Copy)]
enum SkyhookWireMode {
    /// `skyhook.api.GetResponse`: `ReagentSimulation(1=reagent, 5=reagent_type)`, `ReagentDefinition(1=reagent, 5=reagent_type)`.
    GetResponse,
    /// `skyhook.api.GetAllLocalResponse`: `ReagentSimulation(1=reagent, 5=reagent_type)`, `ReagentDefinition(1=reagent, 3=reagent_type)`.
    GetAllLocal,
    /// `skyhook.api.GetAllByCorporationResponse`: `ReagentSimulation(1=reagent_type)`, `ReagentDefinition(1=reagent_type)`.
    GetAllByCorporation,
}

/// Encodes the shared skyhook body using `GetResponse` field numbers (`quasar_sov_skyhook` tests).
fn encode_skyhook(snap: &WorldStateSnapshot, sde: &SdeRegistry, id: i64, now: u64) -> Vec<u8> {
    encode_skyhook_with_mode(snap, sde, id, now, SkyhookWireMode::GetResponse)
}

/// Encodes a `Skyhook` proto body matching the response-specific descriptor field tags in
/// `eve_public.sovereignty.skyhook.api.requests_pb2`:
/// `skyhook(1)`, `reagent_simulations(2)`, `active(3)`, `reagent_definitions(4)`,
/// `theft_vulnerability(5)`, `workforce(7)`, and `planet_resources_definitions_version(8)`.
fn encode_skyhook_with_mode(
    snap: &WorldStateSnapshot,
    sde: &SdeRegistry,
    id: i64,
    now: u64,
    mode: SkyhookWireMode,
) -> Vec<u8> {
    let mut fields = vec![wire_submsg_u64(1, id as u64)];
    let planet_id = snap.sov_runtime.skyhook_planets.get(&id).copied();
    let sys_id = snap.structures.get(&id).map(|s| s.solar_system_id);
    if let Some((reagent_type, stock)) = snap.sov_runtime.skyhook_reagents.get(&id).copied() {
        let (sec_stock, insec_stock, sim_ts) = snap
            .sov_runtime
            .skyhook_reagent_bays
            .get(&id)
            .copied()
            .map(|(_, sec, insec, ts)| (sec, insec, if ts > 0 { ts } else { now }))
            .unwrap_or((stock, 0, now));
        let sim = WireMessage {
            fields: vec![
                WireField {
                    field_number: 1,
                    value: WireValue::Varint(sec_stock),
                },
                WireField {
                    field_number: 2,
                    value: WireValue::Varint(insec_stock),
                },
                wire_submsg_u64(3, sim_ts),
            ],
        }
        .encode();
        let maturation_or_harvest = |amt: u64| {
            WireMessage {
                fields: vec![
                    WireField {
                        field_number: 1,
                        value: WireValue::Varint(amt),
                    },
                    wire_submsg_u64(2, sim_ts),
                ],
            }
            .encode()
        };
        let mut sim_fields = vec![
            wire_submsg_u64(1, u64::from(reagent_type)),
            WireField {
                field_number: 2,
                value: WireValue::LengthDelimited(maturation_or_harvest(insec_stock)),
            },
            WireField {
                field_number: 3,
                value: WireValue::LengthDelimited(maturation_or_harvest(sec_stock)),
            },
            WireField {
                field_number: 4,
                value: WireValue::LengthDelimited(sim),
            },
        ];
        if !matches!(mode, SkyhookWireMode::GetAllByCorporation) {
            sim_fields.push(wire_submsg_u64(5, u64::from(reagent_type)));
        }
        let r = WireMessage { fields: sim_fields }.encode();
        fields.push(WireField {
            field_number: 2,
            value: WireValue::LengthDelimited(r),
        });

        let hourly_yield = planet_hourly_reagent_yield(sde, planet_id, sys_id, reagent_type);
        let amt = WireMessage {
            fields: vec![
                WireField {
                    field_number: 1,
                    value: WireValue::Varint(hourly_yield),
                },
                WireField {
                    field_number: 2,
                    value: WireValue::Varint(3_600),
                },
                WireField {
                    field_number: 3,
                    value: WireValue::Varint(super::sov_gameplay::REAGENT_SECURED_PERCENTAGE),
                },
                WireField {
                    field_number: 4,
                    value: WireValue::Varint(super::sov_gameplay::SKYHOOK_REAGENT_BAY_CAPACITY),
                },
                WireField {
                    field_number: 5,
                    value: WireValue::Varint(super::sov_gameplay::SKYHOOK_REAGENT_BAY_CAPACITY),
                },
            ],
        }
        .encode();
        let mut def_fields = vec![
            wire_submsg_u64(1, u64::from(reagent_type)),
            WireField {
                field_number: 2,
                value: WireValue::LengthDelimited(amt),
            },
        ];
        match mode {
            SkyhookWireMode::GetResponse => def_fields.push(wire_submsg_u64(5, u64::from(reagent_type))),
            SkyhookWireMode::GetAllLocal => def_fields.push(wire_submsg_u64(3, u64::from(reagent_type))),
            SkyhookWireMode::GetAllByCorporation => {}
        }
        let r_def = WireMessage { fields: def_fields }.encode();
        fields.push(WireField {
            field_number: 4,
            value: WireValue::LengthDelimited(r_def),
        });
    }
    let is_destroyed = snap
        .structures
        .get(&id)
        .is_some_and(|s| s.state.eq_ignore_ascii_case("destroyed"));
    let active = !is_destroyed && snap.sov_runtime.skyhook_active.get(&id).copied().unwrap_or(true);
    fields.push(WireField {
        field_number: 3,
        value: WireValue::Varint(u64::from(active)),
    });
    let theft_fields = match snap.sov_runtime.skyhook_theft_windows.get(&id).copied() {
        Some((start, end)) if !is_destroyed => vec![
            WireField {
                field_number: 1,
                value: WireValue::Varint(u64::from(start <= now && now < end)),
            },
            wire_submsg_u64(2, start),
            wire_submsg_u64(3, end),
        ],
        _ => vec![WireField {
            field_number: 1,
            value: WireValue::Varint(0),
        }],
    };
    fields.push(WireField {
        field_number: 5,
        value: WireValue::LengthDelimited(WireMessage { fields: theft_fields }.encode()),
    });
    let wf_amount = match (planet_id, sys_id) {
        (Some(pid), Some(sys_id)) if active => sde
            .celestials_in_system_slice(sys_id)
            .iter()
            .find(|c| c.celestial_id == pid)
            .map(|c| {
                let mut w = super::quasar_sov_hub::planet_yield_for_planet(c.type_id, c.radius).1;
                if w > 0 {
                    let siphoned: u64 = snap
                        .sov_runtime
                        .mercenary_dens
                        .values()
                        .filter(|d| {
                            d.solar_system_id == sys_id && d.enabled && d.planet_id == pid
                        })
                        .map(|d| {
                            let stage = super::quasar_sov_mercden::stage_for_level(d.anarchy_level);
                            let pct = super::quasar_sov_mercden::anarchy_workforce_disruption_pct(
                                stage,
                            );
                            (w * pct) / 100
                        })
                        .sum();
                    w = w.saturating_sub(siphoned);
                }
                w
            })
            .unwrap_or(0),
        _ => 0,
    };
    let wf = if wf_amount > 0 {
        WireField {
            field_number: 1,
            value: WireValue::Varint(wf_amount),
        }
    } else {
        WireField {
            field_number: 2,
            value: WireValue::Varint(1),
        }
    };
    fields.push(WireField {
        field_number: 7,
        value: WireValue::LengthDelimited(WireMessage { fields: vec![wf] }.encode()),
    });
    if planet_id.is_some() {
        fields.push(wire_submsg_u64(8, 1));
    }
    WireMessage { fields }.encode()
}

fn sorted_skyhooks(snap: &WorldStateSnapshot, pred: impl Fn(&eve_db::UpwellStructureRecord) -> bool) -> Vec<i64> {
    let mut ids: Vec<i64> = snap
        .structures
        .values()
        .filter(|s| is_active_skyhook_struct(s) && pred(s))
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
                    advance_skyhook_state_in_snap(snap, &sde, id, now);
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
                let now = ctx.now_unix();
                let out = db.try_with_mut_snapshot(|snap| {
                    let sys = snap
                        .characters
                        .get(&cid)
                        .map(|c| c.solar_system_id)
                        .ok_or((STATUS_NOT_FOUND, format!("Character {cid} not found")))?;
                    let mut builder = eve_proto::pb::message(
                        "eve_public.sovereignty.skyhook.api.GetAllLocalResponse",
                    )
                    .set("solar_system.sequential", u64::from(sys));
                    for id in sorted_skyhooks(snap, |s| s.solar_system_id == sys) {
                        advance_skyhook_state_in_snap(snap, &sde, id, now);
                        builder = builder.add(
                            "skyhooks",
                            eve_proto::pb::raw_message(encode_skyhook_with_mode(
                                snap,
                                &sde,
                                id,
                                now,
                                SkyhookWireMode::GetAllLocal,
                            )),
                        );
                    }
                    builder.encode_or_internal_err()
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
                let now = ctx.now_unix();
                let out = db.try_with_mut_snapshot(|snap| {
                    if require_admin_role(ctx).is_err() {
                        let c = caller(ctx, snap)?;
                        if c.corp != corp {
                            return Err::<Vec<u8>, RpcError>((
                                STATUS_FORBIDDEN,
                                format!("Skyhooks of corporation {corp} are not owned by your corporation"),
                            ));
                        }
                        require_caller_roles(
                            &c,
                            corp_roles::ROLE_DIRECTOR
                                | corp_roles::ROLE_STATION_MANAGER
                                | corp_roles::ROLE_CONFIG_EQUIPMENT,
                            "query corporation skyhooks",
                        )?;
                    }
                    let mut builder = eve_proto::pb::message(
                        "eve_public.sovereignty.skyhook.api.GetAllByCorporationResponse",
                    );
                    for id in sorted_skyhooks(snap, |s| s.owner_corp_id == corp) {
                        advance_skyhook_state_in_snap(snap, &sde, id, now);
                        builder = builder.add(
                            "skyhooks",
                            eve_proto::pb::raw_message(encode_skyhook_with_mode(
                                snap,
                                &sde,
                                id,
                                now,
                                SkyhookWireMode::GetAllByCorporation,
                            )),
                        );
                    }
                    builder.encode_or_internal_err()
                })?;
                Ok(any("eve_public.sovereignty.skyhook.api.GetAllByCorporationResponse", out))
            }),
        );
    }
    {
        let db = Arc::clone(&db);
        let sde = Arc::clone(&sde);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetSolarSystemsWithTheftVulnerableSkyhooksRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let now = ctx.now_unix();
                let systems: BTreeSet<u32> = db.with_mut_snapshot(|snap| {
                    let ids = sorted_skyhooks(snap, |_| true);
                    for &id in &ids {
                        advance_skyhook_state_in_snap(snap, &sde, id, now);
                    }
                    ids.into_iter()
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
        let sde = Arc::clone(&sde);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                let sys = required_id(dyn_msg, 1, "Solar system")? as u32;
                let now = ctx.now_unix();
                db.with_mut_snapshot(|snap| {
                    let mut builder = eve_proto::pb::message(
                        "eve_public.sovereignty.skyhook.api.GetTheftVulnerableSkyhooksInSolarSystemResponse",
                    );
                    for id in sorted_skyhooks(snap, |s| s.solar_system_id == sys) {
                        advance_skyhook_state_in_snap(snap, &sde, id, now);
                        if let Some((start, end)) = open_window(snap, id, now) {
                            builder = builder.add_with("skyhooks", |v| {
                                v.set("skyhook.sequential", id as u64)
                                    .set("expiry", GoogleTimestamp::from_unix(end as i64, 0))
                                    .set("start", GoogleTimestamp::from_unix(start as i64, 0))
                            });
                        }
                    }
                    builder.to_any_or_internal_err()
                })
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

#[cfg(test)]
#[path = "r8_shard_c_security_regression_tests.rs"]
mod r8_shard_c_security_regression_tests;


