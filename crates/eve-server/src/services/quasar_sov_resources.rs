//! `quasar_sov_resources` — Quasar Planetary/Stellar Resources & Skyhook Admin Services (`Q-8`).

#![forbid(unsafe_code)]

use super::quasar_sov_hub::{required_id, star_power, transact, PLANET_RESOURCE_TABLE};
use super::quasar_sov_services::authorize_skyhook;
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::pb::{self, ProtoBuilder};
use eve_proto::quasar::{
    DynamicQuasarMessage, DynamicValue, GoogleAny, GoogleDuration, GoogleTimestamp,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

/// Longest skyhook theft vulnerability window an owner may schedule.
const MAX_THEFT_WINDOW_SECS: u64 = 86_400;

/// Reads `google.protobuf.Timestamp.seconds` from the sub-message at `field`.
fn timestamp_secs(msg: &DynamicQuasarMessage, field: u32) -> Option<u64> {
    match msg.get(field) {
        Some(DynamicValue::Message(ts)) => match ts.get(1) {
            Some(DynamicValue::Int64(s)) if *s > 0 => Some(*s as u64),
            Some(DynamicValue::Uint64(s)) if *s > 0 => Some(*s),
            _ => None,
        },
        _ => None,
    }
}

fn add_planet_definitions(mut b: ProtoBuilder) -> ProtoBuilder {
    // V24.01 Equinox planetary Power/Workforce & Colony Reagent definitions.
    for &(type_id, _name, power, workforce) in PLANET_RESOURCE_TABLE.iter() {
        b = b.add_with("definitions", |def| {
            let def = def
                .set("planet.sequential", type_id)
                .set("power", power)
                .set("workforce", workforce);
            if let Some(reagent_type) = super::sov_gameplay::planet_reagent_type(type_id) {
                let hourly_reagent =
                    super::quasar_sov_skyhook::planet_type_base_hourly_reagent_yield(type_id);
                def.add_with("reagent_definitions", |r_def| {
                    r_def
                        .set("type.sequential", reagent_type)
                        .set_with("definition", |amt| {
                            amt.set("amount_per_cycle", hourly_reagent)
                                .set("cycle_period", GoogleDuration { seconds: 3_600, nanos: 0 })
                                .set("secured_percentage", hourly_reagent)
                                .set("secured_capacity", 25_000u32)
                                .set("unsecured_capacity", 25_000u32)
                        })
                        .set("reagent_type.sequential", reagent_type)
                })
            } else {
                def
            }
        });
    }
    b
}

#[cfg(test)]
fn encode_planet_definitions(out: &mut Vec<u8>) {
    out.extend(
        add_planet_definitions(pb::message(
            "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionResponse",
        ))
        .must_encode(),
    );
}

/// Registers planetary/stellar resources and skyhook admin handlers on the gateway router.
pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    let _desc = Arc::clone(&router.descriptors);

    // 1. eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest
    {
        router.register_handler(
            "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                let no_known = dyn_msg.get(2).is_some_and(|v| matches!(v, DynamicValue::Bool(true)));
                let known_matches = !no_known
                    && match dyn_msg.get(1) {
                        Some(DynamicValue::Message(ver)) => {
                            let maj = match ver.get(1) {
                                Some(DynamicValue::Uint32(n)) => u64::from(*n),
                                Some(DynamicValue::Uint64(n)) => *n,
                                Some(DynamicValue::Int32(n)) => *n as u64,
                                Some(DynamicValue::Int64(n)) => *n as u64,
                                _ => 0,
                            };
                            let min = match ver.get(2) {
                                Some(DynamicValue::Uint32(n)) => u64::from(*n),
                                Some(DynamicValue::Uint64(n)) => *n,
                                _ => 0,
                            };
                            let pat = match ver.get(3) {
                                Some(DynamicValue::Uint32(n)) => u64::from(*n),
                                Some(DynamicValue::Uint64(n)) => *n,
                                _ => 0,
                            };
                            maj == 1 && min == 0 && pat == 0
                        }
                        _ => false,
                    };
                let mut resp = pb::message(
                    "eve_public.sovereignty.resource.planet.api.GetAllDefinitionsResponse",
                );
                if !known_matches {
                    resp = add_planet_definitions(resp);
                }
                Ok(resp
                    .set("version.major", 1u32)
                    .set("version.minor", 0u32)
                    .set("version.patch", 0u32)
                    .must_to_any())
            }),
        );
    }

    // 2. eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest
    //    Schema: `GetDefinitionsVersionRequest { Specification version = 1; }`
    //            `GetDefinitionsVersionResponse { repeated Definition definitions = 1; }`
    {
        router.register_handler(
            "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
            Arc::new(move |_ctx, _any, dyn_msg| {
                if let Some(DynamicValue::Message(ver)) = dyn_msg.get(1) {
                    let maj = match ver.get(1) {
                        Some(DynamicValue::Uint32(n)) => u64::from(*n),
                        Some(DynamicValue::Uint64(n)) => *n,
                        Some(DynamicValue::Int32(n)) => *n as u64,
                        Some(DynamicValue::Int64(n)) => *n as u64,
                        _ => 0,
                    };
                    if maj != 0 && maj != 1 {
                        return Err((
                            STATUS_NOT_FOUND,
                            format!("Unknown planet resource definitions major version {maj}"),
                        ));
                    }
                }
                Ok(add_planet_definitions(pb::message(
                    "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionResponse",
                ))
                .must_to_any())
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
                for cel in sde_cfg.celestials_in_system(sys_id as u32) {
                    if cel.group_id == 7 {
                        let (power, workforce) = super::quasar_sov_hub::planet_yield(cel.type_id);
                        resp = resp.add_with("configurations", |cfg| {
                            let cfg = cfg
                                .set("planet.sequential", cel.celestial_id as u64)
                                .set("power", power)
                                .set("workforce", workforce);
                            if let Some(reagent_type) =
                                super::sov_gameplay::planet_reagent_type(cel.type_id)
                            {
                                // SDE radius- and security-scaled planetary Colony Reagent harvest rate.
                                let hourly_reagent =
                                    super::quasar_sov_skyhook::planet_hourly_reagent_yield(
                                        &sde_cfg,
                                        Some(cel.celestial_id),
                                        Some(sys_id as u32),
                                        reagent_type,
                                    );
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
                                                .set("max_stock", 25_000u64)
                                        })
                                        .set_with("maturation", |m| {
                                            m.set("percentage", 100u32)
                                                .set(
                                                    "cycle_period",
                                                    GoogleDuration { seconds: 86_400, nanos: 0 },
                                                )
                                                .set("min", 0u64)
                                                .set("max", 25_000u64)
                                        })
                                })
                            } else {
                                cfg
                            }
                        });
                    }
                }

                Ok(resp.must_to_any())
            }),
        );
    }

    // 4. eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest
    //    All star celestials (`group_id == 6`) from SDE `sde_celestials` (cached per router in `OnceLock`).
    {
        let sde_star = Arc::clone(&sde);
        let star_configs: Arc<OnceLock<Vec<u8>>> = Arc::new(OnceLock::new());
        router.register_handler(
            "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let _cid = ctx
                    .effective_character_id()
                    .ok_or((STATUS_UNAUTHORIZED, "Authenticated character required".to_string()))?;
                let out = star_configs
                    .get_or_init(|| {
                        let mut resp = pb::message(
                            "eve_public.sovereignty.resource.star.api.GetAllConfigurationsResponse",
                        );
                        for (star_id, type_id) in sde_star.all_stars() {
                            let power = star_power(&sde_star, type_id);
                            resp = resp.add_with("configurations", |cfg| {
                                cfg.set("star.sequential", star_id as u64).set("power", power)
                            });
                        }
                        resp.must_encode()
                    })
                    .clone();

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
        let pending = Arc::clone(&router.pending_notices);
        router.register_handler(
            "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsRequest",
            Arc::new(move |ctx, _any, dyn_msg| {
                crate::quasar_gateway::require_admin_role(ctx)?;
                let skyhook_id = required_id(dyn_msg, 1, "Skyhook")?;

                let (reag_type, reag_qty) = dyn_msg.get(2).and_then(|v| match v {
                    eve_proto::quasar::DynamicValue::Repeated(list) => list.first().and_then(|item| match item {
                        eve_proto::quasar::DynamicValue::Message(sub) => {
                            let qty = sub.get(2).and_then(|q| match q {
                                eve_proto::quasar::DynamicValue::Uint32(n) => Some(*n as u64),
                                eve_proto::quasar::DynamicValue::Uint64(n) => Some(*n),
                                eve_proto::quasar::DynamicValue::Int32(n) => Some(*n as u64),
                                eve_proto::quasar::DynamicValue::Int64(n) => Some(*n as u64),
                                _ => None,
                            }).unwrap_or(1200);
                            let tid = sub.get(1).and_then(|r| match r {
                                eve_proto::quasar::DynamicValue::Message(rid) => rid.get(1).and_then(|id_v| match id_v {
                                    eve_proto::quasar::DynamicValue::Uint32(n) => Some(*n as u32),
                                    eve_proto::quasar::DynamicValue::Uint64(n) => Some(*n as u32),
                                    eve_proto::quasar::DynamicValue::Int32(n) => Some(*n as u32),
                                    eve_proto::quasar::DynamicValue::Int64(n) => Some(*n as u32),
                                    _ => None,
                                }),
                                _ => None,
                            }).unwrap_or(81143);
                            Some((tid, qty))
                        },
                        _ => None,
                    }),
                    _ => None,
                }).unwrap_or((81143, 1200));

                let sys_id = transact(&db_mod, |snap| {
                    let sys = authorize_skyhook(ctx, snap, skyhook_id, "modify skyhook reagents")?;
                    snap.sov_runtime.skyhook_reagents.insert(skyhook_id, (reag_type, reag_qty));
                    Ok::<_, (u32, String)>(sys)
                })?;

                // Emit ReagentSimulationsNotice
                let notice = pb::message(
                    "eve_public.sovereignty.skyhook.api.ReagentSimulationsNotice",
                )
                .set("skyhook.sequential", skyhook_id as u64)
                .add_with("simulations", |s| {
                    s.set("reagent.sequential", reag_type)
                        .set("simulation.secured_stock", reag_qty as u32)
                })
                .must_to_any();

                pending.write().unwrap_or_else(|e| e.into_inner()).push((
                    NoticeScope::SolarSystem(sys_id),
                    notice,
                ));

                Ok(pb::message(
                    "eve_public.sovereignty.skyhook.api.admin.ModifyReagentsResponse",
                )
                .add_with("reagent_simulations", |s| {
                    s.set("reagent.sequential", reag_type)
                        .set("simulation.secured_stock", reag_qty as u32)
                })
                .must_to_any())
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
                let notice = pb::message(
                    "eve_public.sovereignty.skyhook.api.TheftVulnerabilityWindowScheduledNotice",
                )
                .set("skyhook.sequential", skyhook_id as u64)
                .set("start_time", GoogleTimestamp::from_unix(start as i64, 0))
                .set("end_time", GoogleTimestamp::from_unix(end as i64, 0))
                .must_to_any();

                pending.write().unwrap_or_else(|e| e.into_inner()).push((
                    NoticeScope::SolarSystem(sys_id),
                    notice,
                ));

                Ok(pb::message(
                    "eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowResponse",
                )
                .must_to_any())
            }),
        );
    }
}

#[cfg(test)]
#[path = "quasar_sov_resources_tests.rs"]
mod quasar_sov_resources_tests;

