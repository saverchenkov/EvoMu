//! `quasar_sov_resources` — Quasar Planetary/Stellar Resources & Skyhook Admin Services (`Q-8`).

#![forbid(unsafe_code)]

use super::quasar_sov_hub::{required_id, system_yield, transact, PLANET_RESOURCE_TABLE};
use super::quasar_sov_services::authorize_skyhook;
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::Arc;

/// Longest skyhook theft vulnerability window an owner may schedule.
const MAX_THEFT_WINDOW_SECS: u64 = 86_400;

fn encode_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut buf = Vec::new();
    encode_varint_field(1, id, &mut buf);
    encode_bytes_field(field_num, &buf, out);
}

fn encode_timestamp_field(field_num: u32, seconds: u64, out: &mut Vec<u8>) {
    let mut buf = Vec::new();
    encode_varint_field(1, seconds, &mut buf);
    encode_bytes_field(field_num, &buf, out);
}

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
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let mut out = Vec::new();
                for &(type_id, _name, power, workforce) in PLANET_RESOURCE_TABLE.iter() {
                    let mut def_buf = Vec::new();
                    encode_id_field(1, u64::from(type_id), &mut def_buf);
                    encode_varint_field(2, power, &mut def_buf);
                    encode_varint_field(3, workforce, &mut def_buf);
                    if type_id == 12 || type_id == 2015 {
                        let reagent_type = if type_id == 12 {
                            super::sov_services::TYPE_SUPERIONIC_ICE
                        } else {
                            super::sov_services::TYPE_MAGMATIC_GAS
                        };
                        let mut r_def = Vec::new();
                        encode_id_field(1, u64::from(reagent_type), &mut r_def);
                        let mut amt = Vec::new();
                        encode_varint_field(1, 50, &mut amt);
                        encode_varint_field(2, 3_600, &mut amt);
                        encode_bytes_field(2, &amt, &mut r_def);
                        encode_bytes_field(4, &r_def, &mut def_buf);
                    }
                    encode_bytes_field(1, &def_buf, &mut out);
                }
                let mut ver_buf = Vec::new();
                encode_varint_field(1, 1, &mut ver_buf);
                encode_varint_field(2, 0, &mut ver_buf);
                encode_varint_field(3, 0, &mut ver_buf);
                encode_bytes_field(2, &ver_buf, &mut out);

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.resource.planet.api.GetAllDefinitionsResponse"),
                    value: out,
                })
            }),
        );
    }

    // 2. eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest
    {
        router.register_handler(
            "eve_public.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
            Arc::new(move |_ctx, _any, _dyn_msg| {
                let mut out = Vec::new();
                let mut ver_buf = Vec::new();
                encode_varint_field(1, 1, &mut ver_buf);
                encode_varint_field(2, 0, &mut ver_buf);
                encode_varint_field(3, 0, &mut ver_buf);
                encode_bytes_field(1, &ver_buf, &mut out);

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
                let mut out = Vec::new();
                let mut count = 0;
                for cel in sde_cfg.celestials_in_system(sys_id as u32) {
                    if cel.group_id == 7 {
                        let (power, workforce) = super::quasar_sov_hub::planet_yield(cel.type_id);
                        let mut cfg_buf = Vec::new();
                        encode_id_field(1, cel.celestial_id as u64, &mut cfg_buf);
                        encode_varint_field(2, power, &mut cfg_buf);
                        encode_varint_field(3, workforce, &mut cfg_buf);
                        if cel.type_id == 12 || cel.type_id == 2015 {
                            let reagent_type = if cel.type_id == 12 {
                                super::sov_services::TYPE_SUPERIONIC_ICE
                            } else {
                                super::sov_services::TYPE_MAGMATIC_GAS
                            };
                            let mut r_def = Vec::new();
                            encode_id_field(1, u64::from(reagent_type), &mut r_def);
                            let mut amt = Vec::new();
                            encode_varint_field(1, 50, &mut amt);
                            encode_varint_field(2, 3_600, &mut amt);
                            encode_bytes_field(2, &amt, &mut r_def);
                            encode_bytes_field(4, &r_def, &mut cfg_buf);
                        }
                        encode_bytes_field(1, &cfg_buf, &mut out);
                        count += 1;
                        if count >= 8 { break; }
                    }
                }

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.resource.planet.api.GetAllConfigurationsResponse"),
                    value: out,
                })
            }),
        );
    }

    // 4. eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest
    //    Star celestials (`group_id == 6`) of the caller's current system from SDE `mapDenormalize`.
    {
        let db_star = Arc::clone(&db);
        let sde_star = Arc::clone(&sde);
        router.register_handler(
            "eve_public.sovereignty.resource.star.api.GetAllConfigurationsRequest",
            Arc::new(move |ctx, _any, _dyn_msg| {
                let cid = ctx
                    .effective_character_id()
                    .ok_or((STATUS_UNAUTHORIZED, "Authenticated character required".to_string()))? as i64;
                let sys_id = db_star
                    .with_snapshot(|s| s.characters.get(&cid).map(|c| c.solar_system_id))
                    .ok_or((STATUS_NOT_FOUND, format!("Character {cid} not found")))?;
                let mut out = Vec::new();
                for (star_id, power) in system_yield(&sde_star, sys_id).stars {
                    let mut cfg_buf = Vec::new();
                    encode_id_field(1, star_id as u64, &mut cfg_buf);
                    encode_varint_field(2, power, &mut cfg_buf);
                    encode_bytes_field(1, &cfg_buf, &mut out);
                }

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
                let mut sim_payload = Vec::new();
                encode_id_field(1, skyhook_id as u64, &mut sim_payload);
                let mut sim_item = Vec::new();
                encode_id_field(1, reag_type as u64, &mut sim_item);
                encode_varint_field(2, reag_qty, &mut sim_item);
                encode_bytes_field(3, &sim_item, &mut sim_payload);

                pending.write().unwrap_or_else(|e| e.into_inner()).push((
                    NoticeScope::SolarSystem(sys_id),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.skyhook.api.ReagentSimulationsNotice"),
                        value: sim_payload,
                    },
                ));

                let mut out = Vec::new();
                let mut resp_sim = Vec::new();
                encode_id_field(1, reag_type as u64, &mut resp_sim);
                let mut sim_body = Vec::new();
                encode_varint_field(1, reag_qty, &mut sim_body);
                encode_bytes_field(2, &sim_body, &mut resp_sim);
                encode_bytes_field(1, &resp_sim, &mut out);

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.skyhook.api.admin.ModifyReagentsResponse"),
                    value: out,
                })
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
                let mut sched_payload = Vec::new();
                encode_id_field(1, skyhook_id as u64, &mut sched_payload);
                encode_timestamp_field(2, start, &mut sched_payload);
                encode_timestamp_field(3, end, &mut sched_payload);

                pending.write().unwrap_or_else(|e| e.into_inner()).push((
                    NoticeScope::SolarSystem(sys_id),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.skyhook.api.TheftVulnerabilityWindowScheduledNotice"),
                        value: sched_payload,
                    },
                ));

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.sovereignty.skyhook.api.admin.ScheduleTheftVulnerabilityWindowResponse"),
                    value: Vec::new(),
                })
            }),
        );
    }
}
