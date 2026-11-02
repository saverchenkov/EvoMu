//! `quasar_b2_services` — Batch 2 Quasar V24.01 Protobuf Gateway Handlers.
//!
//! Wired directly to `WorldSnapshot` (`structures`, `zarzakh_gate_locks`,
//! `system_states`, `sov_runtime`, `service_state_blobs["cluster_misc_state"]`)
//! and `SdeRegistry`. Equinox star/planet resource handlers return
//! `STATUS_NOT_IMPLEMENTED` (`DATA GAP` in `docs/BLOCKERS.md`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_NOT_FOUND, STATUS_NOT_IMPLEMENTED, STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::Arc;

const MISC_STATE_BLOB_KEY: &str = "cluster_misc_state";

fn any(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value,
    }
}

fn encode_id_msg(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, id, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

fn encode_semver(field_num: u32, major: u32, minor: u32, patch: u32, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, major as u64, &mut sub);
    encode_varint_field(2, minor as u64, &mut sub);
    encode_varint_field(3, patch as u64, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

fn encode_timestamp(field_num: u32, seconds: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, seconds, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

fn extract_u64_checked(msg: &DynamicQuasarMessage, field_num: u32) -> Result<Option<u64>, (u32, String)> {
    match msg.get(field_num) {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Message(inner)) => extract_u64_checked(inner, 1),
            Some(DynamicValue::Uint64(n)) => Ok(Some(*n)),
            Some(DynamicValue::Uint32(n)) => Ok(Some(u64::from(*n))),
            Some(DynamicValue::Int64(n)) => u64::try_from(*n)
                .map(Some)
                .map_err(|_| (STATUS_BAD_REQUEST, "Negative integer identifier".to_string())),
            Some(DynamicValue::Int32(n)) => u64::try_from(*n)
                .map(Some)
                .map_err(|_| (STATUS_BAD_REQUEST, "Negative integer identifier".to_string())),
            None => extract_u64_checked(sub, 2),
            _ => Ok(None),
        },
        Some(DynamicValue::Uint64(n)) => Ok(Some(*n)),
        Some(DynamicValue::Uint32(n)) => Ok(Some(u64::from(*n))),
        Some(DynamicValue::Int64(n)) => u64::try_from(*n)
            .map(Some)
            .map_err(|_| (STATUS_BAD_REQUEST, "Negative integer identifier".to_string())),
        Some(DynamicValue::Int32(n)) => u64::try_from(*n)
            .map(Some)
            .map_err(|_| (STATUS_BAD_REQUEST, "Negative integer identifier".to_string())),
        _ => Ok(None),
    }
}

fn extract_required_u64(msg: &DynamicQuasarMessage, field_num: u32, what: &str) -> Result<u64, (u32, String)> {
    match extract_u64_checked(msg, field_num)? {
        Some(v) if v > 0 => Ok(v),
        _ => Err((STATUS_BAD_REQUEST, format!("Missing {what}"))),
    }
}

fn extract_required_u32(msg: &DynamicQuasarMessage, field_num: u32, what: &str) -> Result<u32, (u32, String)> {
    let v = extract_required_u64(msg, field_num, what)?;
    u32::try_from(v).map_err(|_| (STATUS_BAD_REQUEST, format!("Invalid {what}")))
}

fn extract_required_i64(msg: &DynamicQuasarMessage, field_num: u32, what: &str) -> Result<i64, (u32, String)> {
    let v = extract_required_u64(msg, field_num, what)?;
    i64::try_from(v).map_err(|_| (STATUS_BAD_REQUEST, format!("Invalid {what}")))
}

fn active_char_id(ctx: &AuthoritativeContext) -> Result<u64, (u32, String)> {
    ctx.active_character
        .as_ref()
        .map(|c| c.sequential as u64)
        .filter(|&id| id > 0)
        .ok_or_else(|| (STATUS_UNAUTHORIZED, "Active character required".to_string()))
}

fn can_jump_shipcaster(
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
    db: &WorldRepository,
    sde: &SdeRegistry,
    require_starter_system: bool,
) -> Result<bool, (u32, String)> {
    let caller_cid = i64::try_from(active_char_id(ctx)?)
        .map_err(|_| (STATUS_UNAUTHORIZED, "Invalid character".to_string()))?;
    let cid = match extract_u64_checked(msg, 1)? {
        Some(rcid) if rcid > 0 => {
            let rcid_i64 = i64::try_from(rcid)
                .map_err(|_| (STATUS_BAD_REQUEST, "Invalid character identifier".to_string()))?;
            if rcid_i64 != caller_cid && require_admin_role(ctx).is_err() {
                return Err((STATUS_FORBIDDEN, "Cannot query another character".to_string()));
            }
            rcid_i64
        }
        _ => caller_cid,
    };
    let now = ctx.now_unix();
    let eligible = db.with_snapshot(|snap| {
        let has_caster = if let Some(json) = snap.service_state_blobs.get(MISC_STATE_BLOB_KEY) {
            serde_json::from_str::<serde_json::Value>(json)
                .ok()
                .and_then(|val| val.get("starter_launchers")?.as_array().map(|a| !a.is_empty()))
                .unwrap_or(false)
        } else {
            snap.structures.values().any(|st| {
                st.state != "Destroyed"
                    && sde
                        .types
                        .get(&st.type_id)
                        .is_some_and(|t| t.name.to_ascii_lowercase().contains("shipcaster"))
            })
        };
        if !has_caster {
            return false;
        }
        let has_lock = snap
            .zarzakh_gate_locks
            .get(&cid)
            .is_some_and(|&(_, exp)| exp > now);
        if has_lock {
            return false;
        }
        let Some(ch) = snap.characters.get(&cid) else {
            return false;
        };
        if ch.jump_cooldown_until_unix > now || ch.active_ship_id <= 0 {
            return false;
        }
        if require_starter_system
            && !sde
                .solar_systems
                .get(&ch.solar_system_id)
                .is_some_and(|sys| sys.security_status >= 0.45)
        {
            return false;
        }
        snap.items
            .get(&ch.active_ship_id)
            .is_some_and(|sh| sh.location_id == i64::from(ch.solar_system_id))
    });
    Ok(eligible)
}

pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    // 1. eve_public.ship.unlock.* (DATA GAP -> STATUS_NOT_IMPLEMENTED)
    router.register_handler(
        "eve_public.ship.unlock.GetPendingNotificationsRequest",
        Arc::new(move |_ctx, _any, _msg| {
            Err((STATUS_NOT_IMPLEMENTED, "Not implemented".to_string()))
        }),
    );

    router.register_handler(
        "eve_public.ship.unlock.ClaimRewardRequest",
        Arc::new(move |_ctx, _any, _msg| {
            Err((STATUS_NOT_IMPLEMENTED, "Not implemented".to_string()))
        }),
    );

    // 2. eve.ship.api.CanJumpStarterSpaceShipCasterRequest
    let db_sc1 = Arc::clone(&db);
    let sde_sc1 = Arc::clone(&sde);
    router.register_handler(
        "eve.ship.api.CanJumpStarterSpaceShipCasterRequest",
        Arc::new(move |ctx, _any, msg| {
            let can_jump = can_jump_shipcaster(ctx, msg, &db_sc1, &sde_sc1, false)?;
            let mut out = Vec::new();
            encode_varint_field(1, if can_jump { 1 } else { 0 }, &mut out);
            Ok(any("eve.ship.api.CanJumpStarterSpaceShipCasterResponse", out))
        }),
    );

    // 3. eve.starterspace.api.CanJumpShipCasterRequest
    let db_ss1 = Arc::clone(&db);
    let sde_ss1 = Arc::clone(&sde);
    router.register_handler(
        "eve.starterspace.api.CanJumpShipCasterRequest",
        Arc::new(move |ctx, _any, msg| {
            let can_jump = can_jump_shipcaster(ctx, msg, &db_ss1, &sde_ss1, true)?;
            let mut out = Vec::new();
            encode_varint_field(1, if can_jump { 1 } else { 0 }, &mut out);
            Ok(any("eve.starterspace.api.CanJumpShipCasterResponse", out))
        }),
    );

    // 4. eve.structure.api.GetRequest -> snap.structures
    let db_st1 = Arc::clone(&db);
    router.register_handler(
        "eve.structure.api.GetRequest",
        Arc::new(move |ctx, _any, msg| {
            let is_admin = require_admin_role(ctx).is_ok();
            let cid = if is_admin {
                ctx.active_character.as_ref().map(|c| i64::from(c.sequential)).unwrap_or(0)
            } else {
                i64::try_from(active_char_id(ctx)?)
                    .map_err(|_| (STATUS_UNAUTHORIZED, "Invalid character".to_string()))?
            };
            let structure_id = extract_required_i64(msg, 1, "structure identifier")?;
            let rec = db_st1
                .with_snapshot(|snap| {
                    let st = snap.structures.get(&structure_id)?;
                    if st.state == "Destroyed" {
                        return None;
                    }
                    if !is_admin
                        && !crate::services::station_structure::structure_upwell_ops::evaluate_character_structure_access_in_snap(
                            snap,
                            st.structure_id,
                            cid,
                        )
                    {
                        return None;
                    }
                    Some(st.clone())
                })
                .ok_or_else(|| (STATUS_NOT_FOUND, "Structure not found".to_string()))?;
            let mut attrs = Vec::new();
            encode_id_msg(1, u64::from(rec.type_id), &mut attrs);
            encode_id_msg(2, u64::from(rec.solar_system_id), &mut attrs);
            encode_id_msg(3, rec.owner_corp_id as u64, &mut attrs);
            encode_string_field(4, &rec.name, &mut attrs);
            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);
            Ok(any("eve.structure.api.GetResponse", out))
        }),
    );

    // 5. eve.star.resources.api.GetPowerProductionRequest (DATA GAP -> STATUS_NOT_IMPLEMENTED)
    router.register_handler(
        "eve.star.resources.api.GetPowerProductionRequest",
        Arc::new(move |_ctx, _any, _msg| {
            Err((
                STATUS_NOT_IMPLEMENTED,
                "Star power production not implemented".to_string(),
            ))
        }),
    );

    // 7. eve.stargate.lock.api.* -> snap.zarzakh_gate_locks (Zarzakh = 30_100_000)
    const ZARZAKH_SOLAR_SYSTEM_ID: u32 = 30_100_000;

    let db_gl1 = Arc::clone(&db);
    router.register_handler(
        "eve.stargate.lock.api.GetRequest",
        Arc::new(move |ctx, _any, msg| {
            let cid = active_char_id(ctx)?;
            if let Some(req_cid) = extract_u64_checked(msg, 1)? {
                if req_cid != 0 && req_cid != cid {
                    return Err((STATUS_FORBIDDEN, "Character mismatch".to_string()));
                }
            }
            let sys_id = extract_required_u32(msg, 2, "solarsystem")?;
            if sys_id != ZARZAKH_SOLAR_SYSTEM_ID {
                return Err((STATUS_NOT_FOUND, "No active stargate lock".to_string()));
            }
            let now = ctx.now_unix();
            let (gate_id, expiry_unix) = db_gl1
                .with_snapshot(|snap| {
                    snap.zarzakh_gate_locks
                        .get(&(cid as i64))
                        .copied()
                        .filter(|&(_, exp)| exp > now)
                })
                .ok_or_else(|| (STATUS_NOT_FOUND, "No active stargate lock".to_string()))?;
            let mut out = Vec::new();
            encode_id_msg(1, gate_id as u64, &mut out);
            encode_timestamp(2, expiry_unix, &mut out);
            Ok(any("eve.stargate.lock.api.GetResponse", out))
        }),
    );

    router.register_handler(
        "eve.stargate.lock.api.GetRestrictedSystemsRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let mut out = Vec::new();
            encode_id_msg(1, u64::from(ZARZAKH_SOLAR_SYSTEM_ID), &mut out);
            Ok(any("eve.stargate.lock.api.GetRestrictedSystemsResponse", out))
        }),
    );

    let db_gl3 = Arc::clone(&db);
    router.register_handler(
        "eve.stargate.lock.api.ValidateJumpRequest",
        Arc::new(move |ctx, _any, msg| {
            let cid = active_char_id(ctx)?;
            if let Some(req_cid) = extract_u64_checked(msg, 1)? {
                if req_cid != 0 && req_cid != cid {
                    return Err((STATUS_FORBIDDEN, "Character mismatch".to_string()));
                }
            }
            let sys_id = extract_required_u32(msg, 2, "solarsystem")?;
            let gate_id = extract_required_u64(msg, 3, "gate")?;
            let mut out = Vec::new();
            if sys_id != ZARZAKH_SOLAR_SYSTEM_ID {
                encode_varint_field(1, 1, &mut out);
                return Ok(any("eve.stargate.lock.api.ValidateJumpResponse", out));
            }
            let now = ctx.now_unix();
            let active_lock = db_gl3.with_snapshot(|snap| {
                snap.zarzakh_gate_locks
                    .get(&(cid as i64))
                    .copied()
                    .filter(|&(_, exp)| exp > now)
            });
            if let Some((locked_gate_id, expiry_unix)) = active_lock {
                if locked_gate_id as u64 != gate_id {
                    encode_varint_field(1, 0, &mut out);
                    let mut lock_buf = Vec::new();
                    encode_id_msg(1, locked_gate_id as u64, &mut lock_buf);
                    encode_timestamp(2, expiry_unix, &mut lock_buf);
                    encode_bytes_field(2, &lock_buf, &mut out);
                    return Ok(any("eve.stargate.lock.api.ValidateJumpResponse", out));
                }
            }
            encode_varint_field(1, 1, &mut out);
            Ok(any("eve.stargate.lock.api.ValidateJumpResponse", out))
        }),
    );

    // 8. eve.stargate.lock.api.DeleteCommanded
    let db_gl4 = Arc::clone(&db);
    router.register_handler(
        "eve.stargate.lock.api.DeleteCommanded",
        Arc::new(move |ctx, _any, msg| {
            require_admin_role(ctx)?;
            let cid = extract_required_i64(msg, 2, "character")?;
            let sys_id = extract_required_u32(msg, 3, "solarsystem")?;
            if sys_id != ZARZAKH_SOLAR_SYSTEM_ID {
                return Err((STATUS_NOT_FOUND, "Stargate lock not found".to_string()));
            }
            let removed = db_gl4.with_mut_snapshot(|snap| {
                snap.zarzakh_gate_locks.remove(&cid).is_some()
            });
            if !removed {
                return Err((STATUS_NOT_FOUND, "Stargate lock not found".to_string()));
            }
            Ok(any("google.protobuf.Empty", Vec::new()))
        }),
    );

    // 9. eve.sovereignty.api.* -> snap.system_states / snap.sov_runtime
    let db_sov1 = Arc::clone(&db);
    router.register_handler(
        "eve.sovereignty.api.GetClaimRequest",
        Arc::new(move |_ctx, _any, msg| {
            let sys_id = extract_required_u32(msg, 1, "solar_system")?;
            let claim = db_sov1.with_snapshot(|snap| {
                snap.system_states
                    .get(&sys_id)
                    .and_then(|st| st.sov_alliance_id)
                    .map(|alliance_id| {
                        let hub = snap.structures.values().find(|s| {
                            s.solar_system_id == sys_id
                                && snap.sov_runtime.sov_hubs.contains_key(&s.structure_id)
                        });
                        let hub_id = hub.map(|s| s.structure_id as u64).unwrap_or(0);
                        let corp_id = hub.map(|s| s.owner_corp_id as u64).unwrap_or(0);
                        let claimed_since = snap
                            .sov_runtime
                            .sov_claimed_since_unix
                            .get(&sys_id)
                            .copied()
                            .unwrap_or(0);
                        (alliance_id as u64, hub_id, corp_id, claimed_since)
                    })
            });
            let mut out = Vec::new();
            if let Some((alliance_id, hub_id, corp_id, claimed_since)) = claim {
                encode_varint_field(1, 0, &mut out);
                let mut cbuf = Vec::new();
                encode_id_msg(1, alliance_id, &mut cbuf);
                encode_id_msg(3, hub_id, &mut cbuf);
                encode_id_msg(4, corp_id, &mut cbuf);
                encode_timestamp(5, claimed_since, &mut cbuf);
                encode_bytes_field(2, &cbuf, &mut out);
            } else {
                encode_varint_field(1, 1, &mut out);
            }
            Ok(any("eve.sovereignty.api.GetClaimResponse", out))
        }),
    );

    router.register_handler(
        "eve.sovereignty.api.GetVersionManifestRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let mut manifest = Vec::new();
            for field_num in 1..=6 {
                encode_semver(field_num, 1, 0, 0, &mut manifest);
            }
            let mut out = Vec::new();
            encode_bytes_field(1, &manifest, &mut out);
            Ok(any("eve.sovereignty.api.GetVersionManifestResponse", out))
        }),
    );

    let db_sov2 = Arc::clone(&db);
    router.register_handler(
        "eve.sovereignty.api.GetAllClaimsRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let claims: Vec<(u64, u64, u64, u64)> = db_sov2.with_snapshot(|snap| {
                snap.system_states
                    .values()
                    .filter_map(|st| {
                        st.sov_alliance_id.map(|alliance_id| {
                            let corp_id = snap
                                .structures
                                .values()
                                .find(|s| {
                                    s.solar_system_id == st.solar_system_id
                                        && snap.sov_runtime.sov_hubs.contains_key(&s.structure_id)
                                })
                                .map(|s| s.owner_corp_id as u64)
                                .unwrap_or(0);
                            let claimed_since = snap
                                .sov_runtime
                                .sov_claimed_since_unix
                                .get(&st.solar_system_id)
                                .copied()
                                .unwrap_or(0);
                            (u64::from(st.solar_system_id), alliance_id as u64, corp_id, claimed_since)
                        })
                    })
                    .collect()
            });
            let mut out = Vec::new();
            for (sys_id, alliance_id, corp_id, claimed_since) in claims {
                let mut sc = Vec::new();
                encode_id_msg(1, sys_id, &mut sc);
                let mut ac = Vec::new();
                encode_id_msg(1, alliance_id, &mut ac);
                encode_id_msg(2, corp_id, &mut ac);
                encode_timestamp(3, claimed_since, &mut ac);
                encode_varint_field(5, 1, &mut ac);
                encode_bytes_field(3, &ac, &mut sc);
                encode_bytes_field(1, &sc, &mut out);
            }
            Ok(any("eve.sovereignty.api.GetAllClaimsResponse", out))
        }),
    );

    // 10. eve.sovereignty.activitydefensemodifier.api.GetLevelRequest -> snap.system_states
    let db_adm = Arc::clone(&db);
    let sde_adm = Arc::clone(&sde);
    router.register_handler(
        "eve.sovereignty.activitydefensemodifier.api.GetLevelRequest",
        Arc::new(move |_ctx, _any, msg| {
            let sys_id = extract_required_u32(msg, 1, "solarsystem")?;
            if !sde_adm.solar_systems.contains_key(&sys_id) {
                return Err((STATUS_NOT_FOUND, "Solar system not found".to_string()));
            }
            let level = db_adm.with_snapshot(|snap| {
                snap.system_states
                    .get(&sys_id)
                    .map(|st| {
                        let adm = crate::services::sov_services::calculate_adm(
                            st.military_index,
                            st.industrial_index,
                            st.strategic_index,
                        );
                        (adm * 10.0).round() as u64
                    })
                    .unwrap_or(10)
            });
            let mut out = Vec::new();
            encode_varint_field(1, level, &mut out);
            Ok(any(
                "eve.sovereignty.activitydefensemodifier.api.GetLevelResponse",
                out,
            ))
        }),
    );

    // 11-12. Equinox star/planet resource handlers (DATA GAP -> STATUS_NOT_IMPLEMENTED)
    for req_type in [
        "eve.sovereignty.resource.star.api.GetAllConfigurationsRequest",
        "eve.sovereignty.resource.planet.api.GetDefinitionsConfigurationRequest",
        "eve.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
        "eve.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
        "eve.sovereignty.resource.planet.api.GetAllConfigurationsRequest",
    ] {
        router.register_handler(
            req_type,
            Arc::new(move |_ctx, _any, _msg| {
                Err((
                    STATUS_NOT_IMPLEMENTED,
                    "Equinox resource configuration not implemented".to_string(),
                ))
            }),
        );
    }
}
