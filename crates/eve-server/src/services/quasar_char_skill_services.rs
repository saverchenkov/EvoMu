//! `quasar_char_skill_services` — Batch 2 Quasar V24.01 Protobuf Gateway Handlers.
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
    encode_bytes_field, encode_string_field, encode_varint_field, encode_zigzag32,
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, WireMessage, WireValue,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::Arc;

pub use eve_db::QuasarAclRecord;

fn load_or_seed_acls(db: &WorldRepository) -> Vec<QuasarAclRecord> {
    db.with_mut_snapshot(|snap| {
        if let Some(records) = snap.quasar_commerce.acl_registry.as_ref() {
            return records.clone();
        }
        let defaults = vec![
            QuasarAclRecord {
                acl_id: 1,
                name: "Default Station Access".into(),
                description: "Standard corporate access control list".into(),
                version: 1,
                characters: vec![(90_000_001, 4), (90_000_002, 1)],
                corporations: vec![(98_000_001, 1)],
                alliances: vec![],
                everyone: false,
            },
            QuasarAclRecord {
                acl_id: 2,
                name: "Public Docking Facility".into(),
                description: "Open access docking ACL".into(),
                version: 1,
                characters: vec![(90_000_001, 3)],
                corporations: vec![],
                alliances: vec![],
                everyone: true,
            },
        ];
        snap.quasar_commerce.acl_registry = Some(defaults.clone());
        defaults
    })
}

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
            Some(DynamicValue::Int64(n)) | Some(DynamicValue::Sint64(n)) => {
                Ok(Some(n.unsigned_abs()))
            }
            Some(DynamicValue::Int32(n)) | Some(DynamicValue::Sint32(n)) => {
                Ok(Some(n.unsigned_abs() as u64))
            }
            None => extract_u64_checked(sub, 2),
            _ => Ok(None),
        },
        Some(DynamicValue::Uint64(n)) => Ok(Some(*n)),
        Some(DynamicValue::Uint32(n)) => Ok(Some(u64::from(*n))),
        Some(DynamicValue::Int64(n)) | Some(DynamicValue::Sint64(n)) => {
            Ok(Some(n.unsigned_abs()))
        }
        Some(DynamicValue::Int32(n)) | Some(DynamicValue::Sint32(n)) => {
            Ok(Some(n.unsigned_abs() as u64))
        }
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
        let has_caster = if let Some(misc) = snap.cluster_and_runtime.cluster_misc_state.as_ref() {
            !misc.starter_launchers.is_empty()
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

    // 13. eve.ancestry.GetAllRequest -> eve.ancestry.GetAllResponse
    let sde_anc = Arc::clone(&sde);
    router.register_handler(
        "eve.ancestry.GetAllRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let mut ancestries: Vec<_> = sde_anc.ancestries.values().collect();
            ancestries.sort_by_key(|a| a.ancestry_id);

            let mut out = Vec::new();
            for anc in ancestries {
                let mut item = Vec::new();
                encode_id_msg(1, anc.ancestry_id as u64, &mut item);

                let mut attrs = Vec::new();
                encode_string_field(1, &anc.name, &mut attrs);
                encode_string_field(2, &anc.description, &mut attrs);
                encode_string_field(3, &anc.short_description, &mut attrs);
                encode_id_msg(4, anc.bloodline_id as u64, &mut attrs);

                encode_bytes_field(2, &attrs, &mut item);
                encode_bytes_field(1, &item, &mut out);
            }
            Ok(any("eve.ancestry.GetAllResponse", out))
        }),
    );

    // 14. eve.agent.GetRequest -> eve.agent.GetResponse
    let sde_ag_get = Arc::clone(&sde);
    router.register_handler(
        "eve.agent.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let agent_id = extract_required_i64(msg, 1, "agent")?;
            let ag = sde_ag_get
                .agent(agent_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, format!("Agent {agent_id} not found")))?;

            let mut attrs = Vec::new();
            encode_string_field(1, &ag.name, &mut attrs);
            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);
            Ok(any("eve.agent.GetResponse", out))
        }),
    );

    // 15. eve.agent.SearchRequest -> eve.agent.SearchResponse
    let sde_ag_search = Arc::clone(&sde);
    router.register_handler(
        "eve.agent.SearchRequest",
        Arc::new(move |_ctx, _any, msg| {
            let search_term = match msg.get(1) {
                Some(DynamicValue::String(s)) => s.clone(),
                Some(DynamicValue::Bytes(b)) => String::from_utf8_lossy(b).to_string(),
                Some(DynamicValue::Message(sub)) => match sub.get(1) {
                    Some(DynamicValue::String(s)) => s.clone(),
                    Some(DynamicValue::Bytes(b)) => String::from_utf8_lossy(b).to_string(),
                    Some(DynamicValue::Repeated(list)) => {
                        let mut joined = String::new();
                        for item in list {
                            if let DynamicValue::String(s) = item {
                                if !joined.is_empty() { joined.push(' '); }
                                joined.push_str(s);
                            } else if let DynamicValue::Bytes(b) = item {
                                if !joined.is_empty() { joined.push(' '); }
                                joined.push_str(&String::from_utf8_lossy(b));
                            }
                        }
                        joined
                    }
                    _ => String::new(),
                },
                _ => String::new(),
            };
            let term_lower = search_term.trim().to_ascii_lowercase();
            let mut matched: Vec<_> = sde_ag_search
                .agents
                .values()
                .filter(|ag| {
                    !term_lower.is_empty() && ag.name.to_ascii_lowercase().contains(&term_lower)
                })
                .collect();
            matched.sort_by_key(|ag| ag.agent_id);

            let mut out = Vec::new();
            for ag in matched.into_iter().take(100) {
                encode_id_msg(1, ag.agent_id as u64, &mut out);
            }
            Ok(any("eve.agent.SearchResponse", out))
        }),
    );

    // 16. eve.acl.api.GetRequest -> eve.acl.api.GetResponse
    let db_acl = Arc::clone(&db);
    router.register_handler(
        "eve.acl.api.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let acl_id = extract_required_u32(msg, 1, "acl")?;
            let acls = load_or_seed_acls(&db_acl);
            let record = acls
                .into_iter()
                .find(|r| r.acl_id == acl_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, format!("ACL {acl_id} not found")))?;

            let mut membership = Vec::new();
            for (cid, ctrl) in record.characters {
                let mut char_sub = Vec::new();
                encode_id_msg(1, cid, &mut char_sub);
                encode_varint_field(2, ctrl as u64, &mut char_sub);
                encode_bytes_field(1, &char_sub, &mut membership);
            }
            for (corp_id, ctrl) in record.corporations {
                let mut corp_sub = Vec::new();
                encode_id_msg(1, corp_id, &mut corp_sub);
                encode_varint_field(2, ctrl as u64, &mut corp_sub);
                encode_bytes_field(2, &corp_sub, &mut membership);
            }
            for (all_id, ctrl) in record.alliances {
                let mut all_sub = Vec::new();
                encode_id_msg(1, all_id, &mut all_sub);
                encode_varint_field(2, ctrl as u64, &mut all_sub);
                encode_bytes_field(3, &all_sub, &mut membership);
            }
            if record.everyone {
                encode_varint_field(4, 1, &mut membership);
            }

            let mut attrs = Vec::new();
            encode_string_field(1, &record.name, &mut attrs);
            encode_string_field(2, &record.description, &mut attrs);
            encode_bytes_field(3, &membership, &mut attrs);

            let mut ver = Vec::new();
            encode_varint_field(1, record.version as u64, &mut ver);
            encode_bytes_field(4, &ver, &mut attrs);

            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);
            Ok(any("eve.acl.api.GetResponse", out))
        }),
    );

    // 17. eve.acl.api.CanManageRequest -> eve.acl.api.CanManageResponse
    let db_can_manage = Arc::clone(&db);
    router.register_handler(
        "eve.acl.api.CanManageRequest",
        Arc::new(move |_ctx, _any, msg| {
            let acl_id = extract_required_u32(msg, 1, "acl")?;
            let char_id = extract_required_u64(msg, 2, "character")?;
            let acls = load_or_seed_acls(&db_can_manage);
            let record = acls
                .into_iter()
                .find(|r| r.acl_id == acl_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, format!("ACL {acl_id} not found")))?;

            let can_manage = record
                .characters
                .iter()
                .any(|(c, ctrl)| *c == char_id && (*ctrl == 3 || *ctrl == 4));

            let mut out = Vec::new();
            encode_varint_field(1, if can_manage { 1 } else { 0 }, &mut out);
            Ok(any("eve.acl.api.CanManageResponse", out))
        }),
    );

    // 18. eve.acl.api.GetManagedRequest -> eve.acl.api.GetManagedResponse
    let db_get_managed = Arc::clone(&db);
    router.register_handler(
        "eve.acl.api.GetManagedRequest",
        Arc::new(move |_ctx, _any, msg| {
            let char_id = extract_required_u64(msg, 1, "character")?;
            let acls = load_or_seed_acls(&db_get_managed);
            let mut out = Vec::new();
            for record in acls {
                if record
                    .characters
                    .iter()
                    .any(|(c, ctrl)| *c == char_id && (*ctrl == 3 || *ctrl == 4))
                {
                    encode_id_msg(1, record.acl_id as u64, &mut out);
                }
            }
            Ok(any("eve.acl.api.GetManagedResponse", out))
        }),
    );

    // 19. eve.factionwarfare.stats.corporation.GetRequest -> eve.factionwarfare.stats.corporation.GetResponse
    router.register_handler(
        "eve.factionwarfare.stats.corporation.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let _corp_id = extract_required_u64(msg, 1, "corporation")?;
            let mut out = Vec::new();
            encode_id_msg(1, 500_001, &mut out);
            encode_timestamp(2, 1_700_000_000, &mut out);
            encode_varint_field(3, 42, &mut out);
            encode_varint_field(6, 1, &mut out);
            Ok(any("eve.factionwarfare.stats.corporation.GetResponse", out))
        }),
    );

    // 20. eve.factionwarfare.stats.factions.GetRequest -> eve.factionwarfare.stats.factions.GetResponse
    router.register_handler(
        "eve.factionwarfare.stats.factions.GetRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let mut fstat = Vec::new();
            encode_id_msg(1, 500_001, &mut fstat);
            encode_varint_field(2, 150, &mut fstat);
            encode_varint_field(3, 24, &mut fstat);
            let mut kills = Vec::new();
            encode_varint_field(1, 12, &mut kills);
            encode_varint_field(2, 85, &mut kills);
            encode_varint_field(3, 8500, &mut kills);
            encode_bytes_field(4, &kills, &mut fstat);
            let mut out = Vec::new();
            encode_bytes_field(1, &fstat, &mut out);
            Ok(any("eve.factionwarfare.stats.factions.GetResponse", out))
        }),
    );

    // 33. eve.fulfillment.offer.api.GetRequest -> eve.fulfillment.offer.api.GetResponse
    router.register_handler(
        "eve.fulfillment.offer.api.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let offer_id = extract_required_u64(msg, 1, "offer")?;
            let mut attrs = Vec::new();
            encode_string_field(1, "Standard PLEX Offer", &mut attrs);
            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);
            encode_id_msg(2, offer_id, &mut out);
            Ok(any("eve.fulfillment.offer.api.GetResponse", out))
        }),
    );

    // 34. eve.fulfillment.RewardDeliveryRequest -> eve.fulfillment.RewardDeliveryResponse
    router.register_handler(
        "eve.fulfillment.RewardDeliveryRequest",
        Arc::new(move |ctx, _any, msg| {
            let user_id = extract_u64_checked(msg, 1)?.unwrap_or_else(|| ctx.effective_user_id().unwrap_or(1001) as u64);
            let offer_id = extract_u64_checked(msg, 2)?.unwrap_or(501);
            let mut out = Vec::new();
            encode_id_msg(1, user_id, &mut out);
            encode_id_msg(2, offer_id, &mut out);
            encode_id_msg(3, 777_001, &mut out);
            encode_varint_field(4, 0, &mut out);
            Ok(any("eve.fulfillment.RewardDeliveryResponse", out))
        }),
    );

    // 35. eve.mission.api.GetCompletedCountRequest -> eve.mission.api.GetCompletedCountResponse
    let db_mission = Arc::clone(&db);
    router.register_handler(
        "eve.mission.api.GetCompletedCountRequest",
        Arc::new(move |ctx, _any, msg| {
            let cid = match extract_u64_checked(msg, 1)?
                .map(|v| v as i64)
                .or_else(|| ctx.effective_character_id().map(|c| c as i64))
            {
                Some(id) if id > 0 => id,
                _ => {
                    return Err((
                        STATUS_UNAUTHORIZED,
                        "Character ID required to query completed mission count".into(),
                    ))
                }
            };
            let completed_count = db_mission.with_snapshot(|snap| {
                snap.characters.get(&cid).map(|_| 15u64).unwrap_or(0)
            });
            let mut out = Vec::new();
            encode_varint_field(1, completed_count, &mut out);
            Ok(any("eve.mission.api.GetCompletedCountResponse", out))
        }),
    );

    // 36. eve.monolith.GetClusterInfoRequest -> eve.monolith.GetClusterInfoResponse
    router.register_handler(
        "eve.monolith.GetClusterInfoRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let mut cluster = Vec::new();
            encode_string_field(1, "macho.tranquility", &mut cluster);
            encode_string_field(2, "release/v24.01", &mut cluster);
            encode_varint_field(3, 1024, &mut cluster);
            encode_varint_field(5, 1, &mut cluster);
            encode_string_field(6, "24.01", &mut cluster);
            let mut node = Vec::new();
            for (fnr, val) in [(1, 1), (3, 1), (12, 1), (13, 1), (14, 0x7FFF), (15, 1)] {
                encode_varint_field(fnr, val, &mut node);
            }
            encode_string_field(2, "esp://127.0.0.1:26001", &mut node);
            let mut out = Vec::new();
            encode_bytes_field(1, &cluster, &mut out);
            encode_bytes_field(2, &node, &mut out);
            Ok(any("eve.monolith.GetClusterInfoResponse", out))
        }),
    );

    // 37. eve.monolith.character.location.GetRequest -> eve.monolith.character.location.GetResponse
    let db_mono_loc = Arc::clone(&db);
    router.register_handler(
        "eve.monolith.character.location.GetRequest",
        Arc::new(move |ctx, _any, msg| {
            let cid = extract_u64_checked(msg, 1)?
                .map(|v| v as i64)
                .or_else(|| ctx.effective_character_id().map(|c| c as i64))
                .unwrap_or(0);
            let (sys_id, station_id) = db_mono_loc.with_snapshot(|snap| {
                if let Some(ch) = snap.characters.get(&cid) {
                    (ch.solar_system_id as u64, ch.station_id.or(ch.structure_id).unwrap_or(0) as u64)
                } else {
                    (0, 0)
                }
            });
            let mut out = Vec::new();
            encode_id_msg(1, sys_id, &mut out);
            encode_varint_field(2, 0, &mut out);
            encode_id_msg(3, station_id, &mut out);
            Ok(any("eve.monolith.character.location.GetResponse", out))
        }),
    );

    // 38. eve.monolith.character.logonstatus.GetRequest -> eve.monolith.character.logonstatus.GetResponse
    router.register_handler(
        "eve.monolith.character.logonstatus.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_varint_field(1, 1, &mut out);
            Ok(any("eve.monolith.character.logonstatus.GetResponse", out))
        }),
    );

    // 39. eve.monolith.legacy.json.Request -> eve.monolith.legacy.json.Response
    router.register_handler(
        "eve.monolith.legacy.json.Request",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let mut headers = Vec::new();
            encode_string_field(3, "corr-legacy-dispatch", &mut headers);
            encode_string_field(13, "monolith.dispatch", &mut headers);
            encode_varint_field(17, 0, &mut headers);
            encode_string_field(18, "success", &mut headers);
            let body_str = match dyn_msg.get(3) {
                Some(DynamicValue::String(s)) => format!(r#"{{"echo":"{}", "status":"ok", "dispatched":true}}"#, s),
                _ => r#"{"status":"ok", "dispatched":true}"#.to_string(),
            };
            let mut out = Vec::new();
            encode_bytes_field(1, &headers, &mut out);
            encode_string_field(3, &body_str, &mut out);
            Ok(any("eve.monolith.legacy.json.Response", out))
        }),
    );
}

/// Helper to publish `eve.academy.ships.events.Borrowed` notice to a target session or broadcast.
pub fn publish_academy_borrowed_notice(
    gateway: &PublicGatewayRouter,
    scope: crate::quasar_gateway::NoticeScope,
    char_id: u64,
    ship_type_id: u64,
    ship_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_msg(1, char_id, &mut payload);
    encode_id_msg(2, ship_type_id, &mut payload);
    encode_id_msg(3, ship_id, &mut payload);
    let any_msg = any("eve.academy.ships.events.Borrowed", payload);
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

/// Helper to publish `eve.acl.api.MembershipUpdated` notice to a target session or broadcast.
pub fn publish_acl_membership_updated_notice(
    gateway: &PublicGatewayRouter,
    scope: crate::quasar_gateway::NoticeScope,
    acl_id: u32,
    version: u32,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_msg(1, acl_id as u64, &mut payload);
    let mut ver = Vec::new();
    encode_varint_field(1, version as u64, &mut ver);
    encode_bytes_field(3, &ver, &mut payload);
    let any_msg = any("eve.acl.api.MembershipUpdated", payload);
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

/// Helper to publish `eve.dogma.modifier.system_wide.api.Applied` notice.
pub fn publish_system_wide_dbuff_applied(
    gateway: &PublicGatewayRouter,
    scope: crate::quasar_gateway::NoticeScope,
    system_id: u64,
    dbuff_id: u64,
    units: i64,
    nanos: i32,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_msg(1, system_id, &mut payload);
    encode_id_msg(2, dbuff_id, &mut payload);
    let mut mag = Vec::new();
    encode_varint_field(1, encode_zigzag32(units as i32) as u64, &mut mag);
    encode_varint_field(2, encode_zigzag32(nanos) as u64, &mut mag);
    encode_bytes_field(3, &mag, &mut payload);
    let any_msg = any("eve.dogma.modifier.system_wide.api.Applied", payload);
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

/// Helper to publish `eve.dogma.modifier.system_wide.api.Removed` notice.
pub fn publish_system_wide_dbuff_removed(
    gateway: &PublicGatewayRouter,
    scope: crate::quasar_gateway::NoticeScope,
    system_id: u64,
    dbuff_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_msg(1, system_id, &mut payload);
    encode_id_msg(2, dbuff_id, &mut payload);
    let any_msg = any("eve.dogma.modifier.system_wide.api.Removed", payload);
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

/// Helper to publish `eve.duel.ChallengeIssued` notice.
pub fn publish_duel_challenge_issued_notice(
    gateway: &PublicGatewayRouter, scope: crate::quasar_gateway::NoticeScope,
    challenger_id: u64, recipient_id: u64, now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    gateway.publish_notice(scope, any("eve.duel.ChallengeIssued", encode_duel_challenge_issued(challenger_id, recipient_id)), now_unix).map(|(_, d)| d)
}

/// Helper to publish `eve.duel.ChallengeAccepted` notice.
pub fn publish_duel_challenge_accepted_notice(
    gateway: &PublicGatewayRouter, scope: crate::quasar_gateway::NoticeScope,
    challenger_id: u64, recipient_id: u64, now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    gateway.publish_notice(scope, any("eve.duel.ChallengeAccepted", encode_duel_challenge_accepted(challenger_id, recipient_id)), now_unix).map(|(_, d)| d)
}

/// Helper to publish `eve.effect.combat.electronic.pvp.WarpScramblingApplied` notice.
pub fn publish_warp_scrambling_applied_notice(
    gateway: &PublicGatewayRouter, scope: crate::quasar_gateway::NoticeScope,
    scrambler_id: u64, scrambled_id: u64, module_type_id: u64, now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_msg(1, scrambler_id, &mut payload);
    encode_id_msg(2, scrambled_id, &mut payload);
    let mut dur = Vec::new();
    encode_varint_field(1, 10, &mut dur);
    encode_bytes_field(3, &dur, &mut payload);
    encode_id_msg(4, module_type_id, &mut payload);
    gateway.publish_notice(scope, any("eve.effect.combat.electronic.pvp.WarpScramblingApplied", payload), now_unix).map(|(_, d)| d)
}

/// Helper to publish `eve.mission.epicarc.event.CharacterCompleted` notice.
pub fn publish_epicarc_character_completed_notice(
    gateway: &PublicGatewayRouter, scope: crate::quasar_gateway::NoticeScope,
    char_id: u64, faction_id: u64, now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_msg(1, char_id, &mut payload);
    encode_id_msg(2, faction_id, &mut payload);
    gateway.publish_notice(scope, any("eve.mission.epicarc.event.CharacterCompleted", payload), now_unix).map(|(_, d)| d)
}

/// Helper to publish `eve.fleet.api.AdvertCreated` notice.
pub fn publish_fleet_advert_created_notice(
    gateway: &PublicGatewayRouter, scope: crate::quasar_gateway::NoticeScope,
    fleet_id: u64, char_id: u64, solar_system_id: u64, now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_msg(1, fleet_id, &mut payload);
    encode_id_msg(2, char_id, &mut payload);
    encode_id_msg(3, solar_system_id, &mut payload);
    gateway.publish_notice(scope, any("eve.fleet.api.AdvertCreated", payload), now_unix).map(|(_, d)| d)
}

/// Helper to publish `eve.fleet.member.Joined` notice.
pub fn publish_fleet_member_joined_notice(
    gateway: &PublicGatewayRouter, scope: crate::quasar_gateway::NoticeScope,
    fleet_id: u64, member_id: u64, now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_msg(1, fleet_id, &mut payload);
    encode_id_msg(2, member_id, &mut payload);
    gateway.publish_notice(scope, any("eve.fleet.member.Joined", payload), now_unix).map(|(_, d)| d)
}

/// Helper to encode duel ChallengeIssued message payload.
pub fn encode_duel_challenge_issued(challenger_id: u64, recipient_id: u64) -> Vec<u8> {
    let mut payload = Vec::new();
    encode_id_msg(1, challenger_id, &mut payload);
    encode_id_msg(2, recipient_id, &mut payload);
    payload
}

/// Helper to encode duel ChallengeAccepted message payload.
pub fn encode_duel_challenge_accepted(challenger_id: u64, recipient_id: u64) -> Vec<u8> {
    let mut payload = Vec::new();
    encode_id_msg(1, challenger_id, &mut payload);
    encode_id_msg(2, recipient_id, &mut payload);
    payload
}

/// Helper to encode DBS SettingValue.
pub fn encode_dbs_setting_value(units: i64, nanos: i32) -> Vec<u8> {
    let mut out = Vec::new();
    encode_varint_field(1, units as u64, &mut out);
    encode_varint_field(2, nanos as u64, &mut out);
    out
}

/// Helper to encode DBS Settings with min, max, and equilibrium multiplier fields.
pub fn encode_dbs_settings(
    min_units: i64, min_nanos: i32, max_units: i64, max_nanos: i32, eq_units: i64, eq_nanos: i32,
) -> Vec<u8> {
    let mut out = Vec::new();
    encode_bytes_field(6, &encode_dbs_setting_value(min_units, min_nanos), &mut out);
    encode_bytes_field(7, &encode_dbs_setting_value(max_units, max_nanos), &mut out);
    encode_bytes_field(9, &encode_dbs_setting_value(eq_units, eq_nanos), &mut out);
    out
}

/// Helper to encode DBS SolarSystemDetails.
pub fn encode_dbs_solar_system_details(solar_system_id: u64, sec_units: i64, sec_nanos: i32) -> Vec<u8> {
    let mut out = Vec::new();
    encode_id_msg(1, solar_system_id, &mut out);
    encode_bytes_field(2, &encode_dbs_setting_value(sec_units, sec_nanos), &mut out);
    out
}

/// Helper to encode fighter type identifier.
pub fn encode_fighter_type_identifier(type_id: u32) -> Vec<u8> {
    let mut out = Vec::new();
    encode_varint_field(1, type_id as u64, &mut out);
    out
}

/// Helper to encode fighter squad state with squad ID and ability slots.
pub fn encode_fighter_squad_state(
    squad_id: u32,
    fighter_type_id: u32,
    unit_count: u32,
    ability_slots: &[u32],
) -> Vec<u8> {
    let mut out = Vec::new();
    encode_varint_field(1, squad_id as u64, &mut out);
    let mut ft = Vec::new();
    encode_varint_field(1, fighter_type_id as u64, &mut ft);
    encode_bytes_field(2, &ft, &mut out);
    encode_varint_field(3, unit_count as u64, &mut out);
    for &slot in ability_slots {
        encode_varint_field(4, slot as u64, &mut out);
    }
    out
}

/// Helper to encode epic arc progress stages.
pub fn encode_epicarc_progress(arc_id: u32, branch_stage: u32, completed: bool) -> Vec<u8> {
    let mut out = Vec::new();
    encode_id_msg(1, arc_id as u64, &mut out);
    encode_varint_field(2, branch_stage as u64, &mut out);
    encode_varint_field(3, if completed { 1 } else { 0 }, &mut out);
    out
}

/// Fallback wire decoder for registered request handlers whose proto schemas are not in the SQLite catalog.
pub fn decode_wire_dynamic(full_name: &str, buf: &[u8]) -> DynamicQuasarMessage {
    decode_wire_dynamic_depth(full_name, buf, 0)
}

fn decode_wire_dynamic_depth(full_name: &str, buf: &[u8], depth: usize) -> DynamicQuasarMessage {
    let mut msg = DynamicQuasarMessage::new(full_name);
    if let Ok(wm) = WireMessage::decode(buf) {
        for f in wm.fields {
            let val = match f.value {
                WireValue::Varint(v) => DynamicValue::Uint64(v),
                WireValue::Fixed64(v) => DynamicValue::Fixed64(v),
                WireValue::Fixed32(v) => DynamicValue::Fixed32(v),
                WireValue::LengthDelimited(b) => {
                    let is_printable_ascii = !b.is_empty() && b.iter().all(|&c| (0x20..=0x7E).contains(&c));
                    if depth < 4 && !is_printable_ascii {
                        match WireMessage::decode(&b) {
                            Ok(sub) if !sub.fields.is_empty() => {
                                DynamicValue::Message(Box::new(decode_wire_dynamic_depth("", &b, depth + 1)))
                            }
                            _ => DynamicValue::Bytes(b),
                        }
                    } else {
                        DynamicValue::Bytes(b)
                    }
                }
            };
            msg.set(f.field_number, val);
        }
    }
    msg
}

#[cfg(test)] #[path = "quasar_char_skill_services_tests.rs"]
mod quasar_char_skill_services_tests;



