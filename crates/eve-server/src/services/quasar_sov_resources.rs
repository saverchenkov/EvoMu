//! `quasar_sov_resources` — Quasar Planetary/Stellar Resources & Skyhook Admin Services (`Q-8`).

#![forbid(unsafe_code)]

use super::quasar_sov_hub::{
    planet_yield_for_planet, required_id, star_power, star_power_for_info, transact,
};
use super::quasar_sov_services::authorize_skyhook;
use super::quasar_sov_skyhook::planet_hourly_reagent_yield_for_planet;
use super::sov_gameplay::{
    planet_reagent_type, REAGENT_SECURED_PERCENTAGE, SKYHOOK_REAGENT_BAY_CAPACITY,
};
use super::sov_services::{TYPE_MAGMATIC_GAS, TYPE_SUPERIONIC_ICE};
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_NOT_MODIFIED,
    STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::pb;
use eve_proto::quasar::{
    DynamicQuasarMessage, DynamicValue, GoogleAny, GoogleDuration, GoogleTimestamp, WireField,
    WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::{CelestialInfo, SdeRegistry};
use std::sync::{Arc, OnceLock};

/// Longest skyhook theft vulnerability window an owner may schedule.
const MAX_THEFT_WINDOW_SECS: u64 = 86_400;

fn dyn_u64(v: &DynamicValue) -> Option<u64> {
    match v {
        DynamicValue::Uint32(n) => Some(u64::from(*n)),
        DynamicValue::Uint64(n) => Some(*n),
        DynamicValue::Int32(n) if *n >= 0 => Some(*n as u64),
        DynamicValue::Int64(n) if *n >= 0 => Some(*n as u64),
        _ => None,
    }
}

/// Reads `google.protobuf.Timestamp.seconds` from the sub-message at `field`.
fn timestamp_secs(msg: &DynamicQuasarMessage, field: u32) -> Option<u64> {
    match msg.get(field) {
        Some(DynamicValue::Message(ts)) => ts.get(1).and_then(dyn_u64).filter(|&s| s > 0),
        _ => None,
    }
}

fn parse_reagent_state_entry(
    item: &DynamicValue,
    now: u64,
) -> Result<(u32, u64, u64, u64), (u32, String)> {
    let DynamicValue::Message(sub) = item else {
        return Err((STATUS_BAD_REQUEST, "Malformed reagent state entry".to_string()));
    };
    let tid = sub
        .get(1)
        .or_else(|| sub.get(6))
        .and_then(|r| match r {
            DynamicValue::Message(rid) => rid.get(1).and_then(dyn_u64),
            _ => None,
        })
        .map(|n| n as u32)
        .filter(|&t| t == TYPE_SUPERIONIC_ICE || t == TYPE_MAGMATIC_GAS)
        .ok_or((
            STATUS_BAD_REQUEST,
            "Valid Colony Reagent type_id (81143 or 81144) is required".to_string(),
        ))?;
    let sec_qty = sub.get(2).and_then(dyn_u64).unwrap_or(0);
    let unsec_qty = sub.get(3).and_then(dyn_u64).unwrap_or(0);
    if sec_qty > SKYHOOK_REAGENT_BAY_CAPACITY || unsec_qty > SKYHOOK_REAGENT_BAY_CAPACITY {
        return Err((
            STATUS_BAD_REQUEST,
            format!(
                "Reagent stock ({sec_qty}/{unsec_qty}) exceeds bay capacity {SKYHOOK_REAGENT_BAY_CAPACITY}"
            ),
        ));
    }
    let ts = timestamp_secs(sub, 5).unwrap_or(now);
    Ok((tid, sec_qty, unsec_qty, ts))
}

/// Populates common `ReagentSimulation` descriptor fields shared between `ModifyReagentsResponse`
/// and `ReagentSimulationsNotice`.
pub(crate) fn build_reagent_simulation_entry(
    b: pb::ProtoBuilder,
    reag_type: u32,
    sec_qty: u64,
    unsec_qty: u64,
    cycle_ts: u64,
) -> pb::ProtoBuilder {
    b.set("reagent.sequential", reag_type)
        .set("reagent_type.sequential", u64::from(reag_type))
        .set("simulation.secured_stock", sec_qty as u32)
        .set("simulation.unsecured_stock", unsec_qty as u32)
        .set(
            "simulation.last_cycle",
            GoogleTimestamp::from_unix(cycle_ts as i64, 0),
        )
}

/// Builds a descriptor-validated `eve_public.sovereignty.skyhook.api.ReagentSimulationsNotice`.
pub(crate) fn build_reagent_simulations_notice(
    label: &str,
    skyhook_id: i64,
    planet_id: Option<i64>,
    reag_type: u32,
    sec_qty: u64,
    unsec_qty: u64,
    cycle_ts: u64,
) -> Option<GoogleAny> {
    crate::quasar_gateway::pb_to_any_or_log(
        label,
        pb::message("eve_public.sovereignty.skyhook.api.ReagentSimulationsNotice")
            .set("skyhook.sequential", skyhook_id as u64)
            .with(|b| match planet_id {
                Some(pid) => b.set("planet.sequential", pid as u64),
                None => b,
            })
            .add_with("simulations", |s| {
                build_reagent_simulation_entry(s, reag_type, sec_qty, unsec_qty, cycle_ts)
                    .set("maturation.stock", unsec_qty as u32)
                    .set(
                        "maturation.last_cycle",
                        GoogleTimestamp::from_unix(cycle_ts as i64, 0),
                    )
                    .set("harvest.stock", sec_qty as u32)
                    .set(
                        "harvest.last_cycle",
                        GoogleTimestamp::from_unix(cycle_ts as i64, 0),
                    )
            }),
    )
}

fn wire_id_msg(field_number: u32, sequential: u64) -> WireField {
    WireField {
        field_number,
        value: WireValue::LengthDelimited(
            WireMessage {
                fields: vec![WireField {
                    field_number: 1,
                    value: WireValue::Varint(sequential),
                }],
            }
            .encode(),
        ),
    }
}

fn sovereign_planet_definitions<'a>(sde: &'a SdeRegistry) -> Vec<&'a CelestialInfo> {
    let mut planets: Vec<&'a CelestialInfo> = sde
        .solar_systems
        .values()
        .filter(|s| s.security_status <= 0.0 && s.solar_system_id < 31_000_000)
        .flat_map(|s| sde.celestials_in_system_slice(s.solar_system_id).iter())
        .filter(|c| c.group_id == 7)
        .collect();
    if planets.is_empty() {
        // Minimal synthetic unit registries without sovereign null-sec systems fall back to registered planets.
        planets = sde
            .celestials_by_system
            .values()
            .flat_map(|v| v.iter())
            .filter(|c| c.group_id == 7)
            .collect();
    }
    planets.sort_by_key(|c| c.celestial_id);
    planets
}

fn encode_planet_definition_entry(cel: &CelestialInfo) -> Vec<u8> {
    let (power, workforce) = planet_yield_for_planet(cel.type_id, cel.radius);
    let mut fields = vec![
        wire_id_msg(1, cel.celestial_id as u64),
        WireField {
            field_number: 2,
            value: WireValue::Varint(power),
        },
        WireField {
            field_number: 3,
            value: WireValue::Varint(workforce),
        },
    ];
    if let Some(reagent_type) = planet_reagent_type(cel.type_id) {
        let hourly_reagent = planet_hourly_reagent_yield_for_planet(cel.type_id, cel.radius);
        let dur_bytes = WireMessage {
            fields: vec![WireField {
                field_number: 1,
                value: WireValue::Varint(3_600),
            }],
        }
        .encode();
        let def_bytes = WireMessage {
            fields: vec![
                WireField {
                    field_number: 1,
                    value: WireValue::Varint(hourly_reagent),
                },
                WireField {
                    field_number: 2,
                    value: WireValue::LengthDelimited(dur_bytes),
                },
                WireField {
                    field_number: 3,
                    value: WireValue::Varint(REAGENT_SECURED_PERCENTAGE),
                },
                WireField {
                    field_number: 4,
                    value: WireValue::Varint(SKYHOOK_REAGENT_BAY_CAPACITY),
                },
                WireField {
                    field_number: 5,
                    value: WireValue::Varint(SKYHOOK_REAGENT_BAY_CAPACITY),
                },
            ],
        }
        .encode();
        let wrapper = WireMessage {
            fields: vec![
                wire_id_msg(1, u64::from(reagent_type)),
                WireField {
                    field_number: 2,
                    value: WireValue::LengthDelimited(def_bytes),
                },
                wire_id_msg(3, u64::from(reagent_type)),
            ],
        }
        .encode();
        fields.push(WireField {
            field_number: 4,
            value: WireValue::LengthDelimited(wrapper),
        });
    }
    WireMessage { fields }.encode()
}

fn encode_planet_definitions_payload(sde: &SdeRegistry, include_version_spec: bool) -> Vec<u8> {
    let planets = sovereign_planet_definitions(sde);
    let mut fields: Vec<WireField> = Vec::with_capacity(planets.len() + usize::from(include_version_spec));
    for cel in planets {
        fields.push(WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(encode_planet_definition_entry(cel)),
        });
    }
    if include_version_spec {
        let spec = WireMessage {
            fields: vec![
                WireField {
                    field_number: 1,
                    value: WireValue::Varint(1),
                },
                WireField {
                    field_number: 2,
                    value: WireValue::Varint(0),
                },
                WireField {
                    field_number: 3,
                    value: WireValue::Varint(0),
                },
            ],
        }
        .encode();
        fields.push(WireField {
            field_number: 2,
            value: WireValue::LengthDelimited(spec),
        });
    }
    WireMessage { fields }.encode()
}

#[cfg(test)]
fn encode_planet_definitions(sde: &SdeRegistry, out: &mut Vec<u8>) {
    out.extend(encode_planet_definitions_payload(sde, false));
}

/// Registers planetary/stellar resources and skyhook admin handlers on the gateway router.
pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    let _desc = Arc::clone(&router.descriptors);
    let cached_all_defs: Arc<OnceLock<Vec<u8>>> = Arc::new(OnceLock::new());
    let cached_ver_defs: Arc<OnceLock<Vec<u8>>> = Arc::new(OnceLock::new());

    // 1. eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest
    {
        let sde_defs = Arc::clone(&sde);
        let cache = Arc::clone(&cached_all_defs);
        router.register_handler(
            "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let no_known = dyn_msg.get(2).is_some_and(|v| matches!(v, DynamicValue::Bool(true)));
                let known_matches = !no_known
                    && match dyn_msg.get(1) {
                        Some(DynamicValue::Message(ver)) => {
                            let maj = ver.get(1).and_then(dyn_u64).unwrap_or(0);
                            let min = ver.get(2).and_then(dyn_u64).unwrap_or(0);
                            let pat = ver.get(3).and_then(dyn_u64).unwrap_or(0);
                            maj == 1 && min == 0 && pat == 0
                        }
                        _ => false,
                    };
                if known_matches {
                    return Err((
                        STATUS_NOT_MODIFIED,
                        "Planet resource definitions version 1.0.0 is current".to_string(),
                    ));
                }
                let out = cache
                    .get_or_init(|| encode_planet_definitions_payload(&sde_defs, true))
                    .clone();
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.resource.planet.api.GetAllDefinitionsResponse"),
                    value: out,
                })
            }),
        );
    }

    // 2. eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest
    //    Schema: `GetDefinitionsVersionRequest { Specification version = 1; }`
    //            `GetDefinitionsVersionResponse { repeated Definition definitions = 1; }`
    {
        let sde_ver = Arc::clone(&sde);
        let cache = Arc::clone(&cached_ver_defs);
        router.register_handler(
            "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                if let Some(DynamicValue::Message(ver)) = dyn_msg.get(1) {
                    let maj = ver.get(1).and_then(dyn_u64).unwrap_or(0);
                    if maj != 0 && maj != 1 {
                        return Err((
                            STATUS_NOT_FOUND,
                            format!("Unknown planet resource definitions major version {maj}"),
                        ));
                    }
                }
                let out = cache
                    .get_or_init(|| encode_planet_definitions_payload(&sde_ver, false))
                    .clone();
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionResponse"),
                    value: out,
                })
            }),
        );
    }

    // 3. eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest
    {
        let sde_cfg = Arc::clone(&sde);
        let db_cfg = Arc::clone(&db);
        router.register_handler(
            "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let cid = ctx
                    .effective_character_id()
                    .ok_or((STATUS_UNAUTHORIZED, "Authenticated character required".to_string()))? as i64;
                let sys_id = db_cfg
                    .with_snapshot(|s| s.characters.get(&cid).map(|c| c.solar_system_id))
                    .ok_or((STATUS_NOT_FOUND, format!("Character {cid} not found")))?;
                let mut resp = pb::message(
                    "eve_public.sovereignty.resource.planet.api.GetAllConfigurationsResponse",
                );
                for cel in sde_cfg.celestials_in_system_slice(sys_id as u32) {
                    if cel.group_id == 7 {
                        let (power, workforce) = planet_yield_for_planet(cel.type_id, cel.radius);
                        resp = resp.add_with("configurations", |cfg| {
                            let cfg = cfg
                                .set("planet.sequential", cel.celestial_id as u64)
                                .set("power", power)
                                .set("workforce", workforce);
                            if let Some(reagent_type) = planet_reagent_type(cel.type_id) {
                                let hourly_reagent =
                                    planet_hourly_reagent_yield_for_planet(cel.type_id, cel.radius);
                                cfg.add_with("reagent_configurations", |r_cfg| {
                                    r_cfg
                                        .set("type.sequential", reagent_type)
                                        .set_with("harvest", |h| {
                                            h.set("amount_per_cycle", hourly_reagent)
                                                .set(
                                                    "cycle_period",
                                                    GoogleDuration { seconds: 3_600, nanos: 0 },
                                                )
                                                .set("min_stock", 0u64)
                                                .set("max_stock", SKYHOOK_REAGENT_BAY_CAPACITY)
                                        })
                                        .set_with("maturation", |m| {
                                            m.set("percentage", 100u32)
                                                .set(
                                                    "cycle_period",
                                                    GoogleDuration { seconds: 86_400, nanos: 0 },
                                                )
                                                .set("min", 0u64)
                                                .set("max", SKYHOOK_REAGENT_BAY_CAPACITY)
                                        })
                                })
                            } else {
                                cfg
                            }
                        });
                    }
                }

                resp.to_any_or_internal_err()
            }),
        );
    }

    // 4. eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest
    //    All star celestials (`group_id == 6`) from SDE `sde_stars.luminosity` (cached per router in `OnceLock`).
    {
        let sde_star = Arc::clone(&sde);
        let star_configs: Arc<OnceLock<Vec<u8>>> = Arc::new(OnceLock::new());
        router.register_handler(
            "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let _cid = ctx
                    .effective_character_id()
                    .ok_or((STATUS_UNAUTHORIZED, "Authenticated character required".to_string()))?;
                let out = if let Some(cached) = star_configs.get() {
                    cached.clone()
                } else {
                    let mut resp = pb::message(
                        "eve_public.sovereignty.resource.star.api.GetAllConfigurationsResponse",
                    );
                    for (star_id, type_id) in sde_star.all_stars() {
                        let power = sde_star
                            .star(star_id)
                            .map_or_else(|| star_power(&sde_star, type_id), |st| star_power_for_info(&sde_star, st));
                        resp = resp.add_with("configurations", |cfg| {
                            cfg.set("star.sequential", star_id as u64).set("power", power)
                        });
                    }
                    let encoded = resp.encode_or_internal_err()?;
                    let _ = star_configs.set(encoded.clone());
                    encoded
                };

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.resource.star.api.GetAllConfigurationsResponse"),
                    value: out,
                })
            }),
        );
    }

    // 5. eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest
    {
        let db_mod = Arc::clone(&db);
        let sde_mod = Arc::clone(&sde);
        let pending = Arc::clone(&router.pending_notices);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                crate::quasar_gateway::require_admin_role(ctx)?;
                let skyhook_id = required_id(dyn_msg, 1, "Skyhook")?;
                let now = ctx.now_unix();

                let explicit_list: Option<Vec<(u32, u64, u64, u64)>> = match dyn_msg.get(2) {
                    Some(DynamicValue::Repeated(list)) => {
                        let parsed = list
                            .iter()
                            .map(|item| parse_reagent_state_entry(item, now))
                            .collect::<Result<Vec<_>, _>>()?;
                        if parsed.is_empty() {
                            return Err((
                                STATUS_BAD_REQUEST,
                                "Reagents state list must not be empty".to_string(),
                            ));
                        }
                        Some(parsed)
                    }
                    Some(_) => {
                        return Err((STATUS_BAD_REQUEST, "Malformed reagents field".to_string()));
                    }
                    None => None,
                };

                let (sys_id, planet_id, reag_type, sec_qty, unsec_qty, cycle_ts) = transact(&db_mod, |snap| {
                    let sys = authorize_skyhook(ctx, snap, skyhook_id, "modify skyhook reagents")?;
                    let pid = snap.sov_runtime.skyhook_planets.get(&skyhook_id).copied();
                    let planet_reagent = pid
                        .and_then(|p| sde_mod.celestial(p))
                        .map(|c| planet_reagent_type(c.type_id));
                    let (tid, sec, unsec, ts) = match (explicit_list, planet_reagent) {
                        (_, Some(None)) => {
                            return Err((
                                STATUS_BAD_REQUEST,
                                format!("Skyhook {skyhook_id} planet has no harvestable Colony Reagent"),
                            ));
                        }
                        (Some(mut list), Some(Some(expected))) => {
                            for &(tid, ..) in &list {
                                if tid != expected {
                                    return Err((
                                        STATUS_BAD_REQUEST,
                                        format!(
                                            "Skyhook {skyhook_id} planet requires Colony Reagent type {expected}, got {tid}"
                                        ),
                                    ));
                                }
                            }
                            list.pop().expect("non-empty validated reagents list")
                        }
                        (Some(mut list), None) => list.pop().expect("non-empty validated reagents list"),
                        (None, Some(Some(expected))) => (expected, 1_200, 0, now),
                        (None, None) => {
                            let existing = snap
                                .sov_runtime
                                .skyhook_reagents
                                .get(&skyhook_id)
                                .map(|r| r.0);
                            let tid = match existing {
                                Some(t) => t,
                                None if pid.is_some() => {
                                    return Err((
                                        STATUS_BAD_REQUEST,
                                        format!(
                                            "Skyhook {skyhook_id} planet has no harvestable Colony Reagent"
                                        ),
                                    ));
                                }
                                // Test-seeded skyhook without linked planet defaults to Magmatic Gas.
                                None => TYPE_MAGMATIC_GAS,
                            };
                            (tid, 1_200, 0, now)
                        }
                    };
                    snap.sov_runtime
                        .skyhook_reagent_bays
                        .insert(skyhook_id, (tid, sec, unsec, ts));
                    snap.sov_runtime
                        .skyhook_reagents
                        .insert(skyhook_id, (tid, sec.saturating_add(unsec)));
                    snap.sov_runtime
                        .skyhook_last_harvest_unix
                        .insert(skyhook_id, ts);
                    snap.sov_runtime
                        .skyhook_maturity_rem
                        .insert(skyhook_id, (unsec, 0));
                    Ok::<_, (u32, String)>((sys, pid, tid, sec, unsec, ts))
                })?;

                // Emit ReagentSimulationsNotice
                if let Some(notice) = build_reagent_simulations_notice(
                    "quasar_sov_resources.modify_reagents",
                    skyhook_id,
                    planet_id,
                    reag_type,
                    sec_qty,
                    unsec_qty,
                    cycle_ts,
                ) {
                    pending.write().unwrap_or_else(|e| e.into_inner()).push((
                        NoticeScope::SolarSystem(sys_id),
                        notice,
                    ));
                }

                pb::message(
                    "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsResponse",
                )
                .add_with("reagent_simulations", |s| {
                    build_reagent_simulation_entry(s, reag_type, sec_qty, unsec_qty, cycle_ts)
                })
                .to_any_or_internal_err()
            }),
        );
    }

    // 6. eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest
    {
        let db_sched = Arc::clone(&db);
        let pending = Arc::clone(&router.pending_notices);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                crate::quasar_gateway::require_admin_role(ctx)?;
                let skyhook_id = required_id(dyn_msg, 1, "Skyhook")?;
                let now = ctx.now_unix();
                let start = timestamp_secs(dyn_msg, 2)
                    .ok_or((STATUS_BAD_REQUEST, "vulnerability_start is required".to_string()))?;
                let end = timestamp_secs(dyn_msg, 3)
                    .ok_or((STATUS_BAD_REQUEST, "vulnerability_end is required".to_string()))?;
                if start < now {
                    return Err((STATUS_BAD_REQUEST, format!("Vulnerability window start {start} is in the past (server time {now})")));
                }
                if end <= start || end - start > MAX_THEFT_WINDOW_SECS {
                    return Err((STATUS_BAD_REQUEST, format!("Invalid vulnerability window {start}..{end}")));
                }

                let sys_id = transact(&db_sched, |snap| {
                    let sys = authorize_skyhook(ctx, snap, skyhook_id, "schedule skyhook theft vulnerability")?;
                    snap.sov_runtime.skyhook_theft_windows.insert(skyhook_id, (start, end));
                    Ok::<_, (u32, String)>(sys)
                })?;

                // Emit TheftVulnerabilityWindowScheduledNotice
                if let Some(notice) = crate::quasar_gateway::pb_to_any_or_log(
                    "quasar_sov_resources.schedule_theft_vulnerability_window",
                    pb::message(
                        "eve_public.sovereignty.skyhook.api.TheftVulnerabilityWindowScheduledNotice",
                    )
                    .set("skyhook.sequential", skyhook_id as u64)
                    .set("start_time", GoogleTimestamp::from_unix(start as i64, 0))
                    .set("end_time", GoogleTimestamp::from_unix(end as i64, 0)),
                ) {
                    pending.write().unwrap_or_else(|e| e.into_inner()).push((
                        NoticeScope::SolarSystem(sys_id),
                        notice,
                    ));
                }

                pb::message(
                    "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowResponse",
                )
                .to_any_or_internal_err()
            }),
        );
    }
}

#[cfg(test)]
#[path = "quasar_sov_resources_tests.rs"]
mod quasar_sov_resources_tests;

