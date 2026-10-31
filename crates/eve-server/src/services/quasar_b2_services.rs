//! `quasar_b2_services` — Batch 2 Quasar V24.01 Protobuf Gateway Handlers.
//!
//! Implements state-backed Quasar handlers for:
//! 1. `eve_public.ship.unlock.*` (`GetPendingNotificationsRequest`, `ClaimRewardRequest`)
//! 2. `eve.ship.api.*` (`CanJumpStarterSpaceShipCasterRequest`)
//! 3. `eve.starterspace.api.*` (`CanJumpShipCasterRequest`)
//! 4. `eve.structure.api.*` (`GetRequest`)
//! 5. `eve.star.resources.api.*` (`GetPowerProductionRequest`)
//! 6. `eve.space.tag.admin.api.*` (`ApplyRequest`, `RemoveRequest`)
//! 7. `eve.stargate.lock.api.*` (`GetRequest`, `GetRestrictedSystemsRequest`, `ValidateJumpRequest`, `DeleteCommanded`)
//! 8. `eve.sovereignty.api.*` (`GetClaimRequest`, `GetVersionManifestRequest`, `GetAllClaimsRequest`)
//! 9. `eve.sovereignty.activitydefensemodifier.api.*` (`GetLevelRequest`)
//! 10. `eve.sovereignty.resource.star.api.*` (`GetAllConfigurationsRequest`)
//! 11. `eve.sovereignty.resource.planet.api.*` (`GetDefinitionsConfigurationRequest`, `GetAllDefinitionsRequest`, `GetDefinitionsVersionRequest`, `GetAllConfigurationsRequest`)

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

const BLOB_KEY: &str = "quasar_b2_state";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StructureRecord {
    pub structure_id: u64,
    pub type_id: u64,
    pub solar_system_id: u64,
    pub corporation_id: u64,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StargateLockRecord {
    pub character_id: u64,
    pub solar_system_id: u64,
    pub gate_id: u64,
    pub expiry_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SovClaimRecord {
    pub solar_system_id: u64,
    pub alliance_id: u64,
    pub corporation_id: u64,
    pub hub_id: u64,
    pub claimed_at_unix: u64,
    pub is_capital: bool,
    pub military_index: i32,
    pub industrial_index: i32,
    pub strategic_index: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StarResourceConfig {
    pub star_id: u64,
    pub power: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanetResourceConfig {
    pub planet_id: u64,
    pub power: u64,
    pub workforce: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuasarB2State {
    pub ship_unlock_pending: BTreeMap<u64, Vec<u64>>,
    pub ship_unlock_claimed: BTreeMap<u64, Vec<u64>>,
    pub starter_caster_blocked: BTreeSet<u64>,
    pub structures: BTreeMap<u64, StructureRecord>,
    pub star_power_production: BTreeMap<u64, u64>,
    pub space_tags: BTreeMap<u64, BTreeMap<String, Option<u64>>>,
    pub stargate_locks: BTreeMap<String, StargateLockRecord>,
    pub restricted_systems: Vec<u64>,
    pub sov_claims: BTreeMap<u64, SovClaimRecord>,
    pub adm_levels: BTreeMap<u64, u64>,
    pub star_resource_configs: Vec<StarResourceConfig>,
    pub planet_resource_configs: Vec<PlanetResourceConfig>,
}

impl Default for QuasarB2State {
    fn default() -> Self {
        let mut structures = BTreeMap::new();
        structures.insert(
            1_030_000_000_001,
            StructureRecord {
                structure_id: 1_030_000_000_001,
                type_id: 35832, // Astrahus
                solar_system_id: 30_000_142,
                corporation_id: 98_000_001,
                name: "Jita Trade Citadel".to_string(),
            },
        );

        let mut star_power_production = BTreeMap::new();
        star_power_production.insert(40_009_041, 4_250);

        let mut stargate_locks = BTreeMap::new();
        stargate_locks.insert(
            format!("{}:{}", 2_112_000_001u64, 30_000_142u64),
            StargateLockRecord {
                character_id: 2_112_000_001,
                solar_system_id: 30_000_142,
                gate_id: 50_001_248,
                expiry_unix: 1_800_000_000,
            },
        );

        let mut sov_claims = BTreeMap::new();
        sov_claims.insert(
            30_002_537,
            SovClaimRecord {
                solar_system_id: 30_002_537,
                alliance_id: 99_000_001,
                corporation_id: 98_000_001,
                hub_id: 1_040_000_000_001,
                claimed_at_unix: 1_700_000_000,
                is_capital: true,
                military_index: 5,
                industrial_index: 4,
                strategic_index: 5,
            },
        );

        let mut adm_levels = BTreeMap::new();
        adm_levels.insert(30_002_537, 60); // 6.0x ADM

        Self {
            ship_unlock_pending: BTreeMap::new(),
            ship_unlock_claimed: BTreeMap::new(),
            starter_caster_blocked: BTreeSet::new(),
            structures,
            star_power_production,
            space_tags: BTreeMap::new(),
            stargate_locks,
            restricted_systems: vec![30_000_142, 30_100_001],
            sov_claims,
            adm_levels,
            star_resource_configs: vec![StarResourceConfig {
                star_id: 40_009_041,
                power: 4_250,
            }],
            planet_resource_configs: vec![PlanetResourceConfig {
                planet_id: 40_009_042,
                power: 1_200,
                workforce: 8_500,
            }],
        }
    }
}

pub fn load_state(db: &WorldRepository) -> QuasarB2State {
    db.with_snapshot(|s| {
        s.quasar_state_blobs
            .get(BLOB_KEY)
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default()
    })
}

pub fn save_state(db: &WorldRepository, state: &QuasarB2State) {
    if let Ok(json) = serde_json::to_string(state) {
        db.with_mut_snapshot(|s| {
            s.quasar_state_blobs.insert(BLOB_KEY.into(), json);
        });
    }
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

fn extract_u64(msg: &DynamicQuasarMessage, field_num: u32) -> u64 {
    match msg.get(field_num) {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Message(inner)) => extract_u64(inner, 1),
            Some(DynamicValue::Uint64(n)) => *n,
            Some(DynamicValue::Uint32(n)) => *n as u64,
            Some(DynamicValue::Int64(n)) => *n as u64,
            Some(DynamicValue::Int32(n)) => *n as u64,
            None => {
                // Handle eve.space.location.Identifier (field 2 = constellation -> field 2 = solar_system)
                let sys = extract_u64(sub, 2);
                if sys > 0 {
                    sys
                } else {
                    0
                }
            }
            _ => 0,
        },
        Some(DynamicValue::Uint64(n)) => *n,
        Some(DynamicValue::Uint32(n)) => *n as u64,
        Some(DynamicValue::Int64(n)) => *n as u64,
        Some(DynamicValue::Int32(n)) => *n as u64,
        _ => 0,
    }
}

fn extract_tag_name(msg: &DynamicQuasarMessage, field_num: u32) -> Option<String> {
    match msg.get(field_num) {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::String(s)) if !s.is_empty() => Some(s.clone()),
            _ => None,
        },
        Some(DynamicValue::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn active_char_id(ctx: &AuthoritativeContext) -> Result<u64, (u32, String)> {
    ctx.active_character
        .as_ref()
        .map(|c| c.sequential as u64)
        .filter(|&id| id > 0)
        .ok_or_else(|| (STATUS_UNAUTHORIZED, "Active character required".to_string()))
}

pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    // 1. eve_public.ship.unlock.GetPendingNotificationsRequest
    let db_su1 = Arc::clone(&db);
    router.register_handler(
        "eve_public.ship.unlock.GetPendingNotificationsRequest",
        Arc::new(move |ctx, _any, _msg| {
            let cid = active_char_id(ctx)?;
            let mut st = load_state(&db_su1);
            let pending = st
                .ship_unlock_pending
                .entry(cid)
                .or_insert_with(|| vec![587, 597])
                .clone();
            save_state(&db_su1, &st);
            let mut out = Vec::new();
            for ship_type_id in pending {
                encode_id_msg(1, ship_type_id, &mut out);
            }
            Ok(any("eve_public.ship.unlock.GetPendingNotificationsResponse", out))
        }),
    );

    // 1b. eve_public.ship.unlock.ClaimRewardRequest
    let db_su2 = Arc::clone(&db);
    router.register_handler(
        "eve_public.ship.unlock.ClaimRewardRequest",
        Arc::new(move |ctx, _any, msg| {
            let cid = active_char_id(ctx)?;
            let ship_type_id = extract_u64(msg, 1);
            if ship_type_id == 0 {
                return Err((STATUS_BAD_REQUEST, "Missing unlocked_ship_type".to_string()));
            }
            let mut st = load_state(&db_su2);
            let pending = st
                .ship_unlock_pending
                .entry(cid)
                .or_insert_with(|| vec![587, 597]);
            let pos = pending
                .iter()
                .position(|&id| id == ship_type_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Ship unlock reward not pending".to_string()))?;
            pending.remove(pos);
            st.ship_unlock_claimed
                .entry(cid)
                .or_default()
                .push(ship_type_id);
            save_state(&db_su2, &st);
            Ok(any("eve_public.ship.unlock.ClaimRewardResponse", Vec::new()))
        }),
    );

    // 2. eve.ship.api.CanJumpStarterSpaceShipCasterRequest
    let db_sc1 = Arc::clone(&db);
    router.register_handler(
        "eve.ship.api.CanJumpStarterSpaceShipCasterRequest",
        Arc::new(move |ctx, _any, msg| {
            let mut cid = extract_u64(msg, 1);
            if cid == 0 {
                cid = active_char_id(ctx)?;
            }
            let st = load_state(&db_sc1);
            let can_jump = !st.starter_caster_blocked.contains(&cid);
            let mut out = Vec::new();
            encode_varint_field(1, if can_jump { 1 } else { 0 }, &mut out);
            Ok(any("eve.ship.api.CanJumpStarterSpaceShipCasterResponse", out))
        }),
    );

    // 3. eve.starterspace.api.CanJumpShipCasterRequest
    let db_ss1 = Arc::clone(&db);
    router.register_handler(
        "eve.starterspace.api.CanJumpShipCasterRequest",
        Arc::new(move |ctx, _any, msg| {
            let mut cid = extract_u64(msg, 1);
            if cid == 0 {
                cid = active_char_id(ctx)?;
            }
            let st = load_state(&db_ss1);
            let has_lock = st.stargate_locks.values().any(|l| l.character_id == cid);
            let can_jump = !st.starter_caster_blocked.contains(&cid) && !has_lock;
            let mut out = Vec::new();
            encode_varint_field(1, if can_jump { 1 } else { 0 }, &mut out);
            Ok(any("eve.starterspace.api.CanJumpShipCasterResponse", out))
        }),
    );

    // 4. eve.structure.api.GetRequest
    let db_st1 = Arc::clone(&db);
    router.register_handler(
        "eve.structure.api.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let structure_id = extract_u64(msg, 1);
            if structure_id == 0 {
                return Err((STATUS_BAD_REQUEST, "Missing structure identifier".to_string()));
            }
            let st = load_state(&db_st1);
            let rec = st
                .structures
                .get(&structure_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Structure not found".to_string()))?;
            let mut attrs = Vec::new();
            encode_id_msg(1, rec.type_id, &mut attrs);
            encode_id_msg(2, rec.solar_system_id, &mut attrs);
            encode_id_msg(3, rec.corporation_id, &mut attrs);
            encode_string_field(4, &rec.name, &mut attrs);
            let mut out = Vec::new();
            encode_bytes_field(1, &attrs, &mut out);
            Ok(any("eve.structure.api.GetResponse", out))
        }),
    );

    // 5. eve.star.resources.api.GetPowerProductionRequest
    let db_sr1 = Arc::clone(&db);
    router.register_handler(
        "eve.star.resources.api.GetPowerProductionRequest",
        Arc::new(move |_ctx, _any, msg| {
            let star_id = extract_u64(msg, 1);
            if star_id == 0 {
                return Err((STATUS_BAD_REQUEST, "Missing star identifier".to_string()));
            }
            let st = load_state(&db_sr1);
            let amount = st
                .star_power_production
                .get(&star_id)
                .copied()
                .ok_or_else(|| (STATUS_NOT_FOUND, "Star resource configuration not found".to_string()))?;
            let mut out = Vec::new();
            encode_varint_field(1, amount, &mut out);
            Ok(any("eve.star.resources.api.GetPowerProductionResponse", out))
        }),
    );

    // 6. eve.space.tag.admin.api.ApplyRequest & RemoveRequest
    let db_tag1 = Arc::clone(&db);
    router.register_handler(
        "eve.space.tag.admin.api.ApplyRequest",
        Arc::new(move |ctx, _any, msg| {
            require_admin_role(ctx)?;
            let location_id = extract_u64(msg, 2);
            if location_id == 0 {
                return Err((STATUS_BAD_REQUEST, "Missing location identifier".to_string()));
            }
            let mut tag_names = Vec::new();
            if let Some(DynamicValue::Repeated(items)) = msg.get(3) {
                for item in items {
                    if let DynamicValue::Message(spec) = item {
                        if let Some(name) = extract_tag_name(spec, 1) {
                            tag_names.push(name);
                        }
                    }
                }
            } else if let Some(DynamicValue::Message(spec)) = msg.get(3) {
                if let Some(name) = extract_tag_name(spec, 1) {
                    tag_names.push(name);
                }
            }
            if tag_names.is_empty() {
                return Err((STATUS_BAD_REQUEST, "No space tags specified".to_string()));
            }
            let mut st = load_state(&db_tag1);
            let entry = st.space_tags.entry(location_id).or_default();
            for name in tag_names {
                entry.insert(name, None);
            }
            save_state(&db_tag1, &st);
            Ok(any("eve.space.tag.admin.api.ApplyResponse", Vec::new()))
        }),
    );

    let db_tag2 = Arc::clone(&db);
    router.register_handler(
        "eve.space.tag.admin.api.RemoveRequest",
        Arc::new(move |ctx, _any, msg| {
            require_admin_role(ctx)?;
            let location_id = extract_u64(msg, 2);
            if location_id == 0 {
                return Err((STATUS_BAD_REQUEST, "Missing location identifier".to_string()));
            }
            let mut tag_names = Vec::new();
            if let Some(DynamicValue::Repeated(items)) = msg.get(3) {
                for item in items {
                    if let DynamicValue::Message(id_msg) = item {
                        if let Some(DynamicValue::String(s)) = id_msg.get(1) {
                            if !s.is_empty() {
                                tag_names.push(s.clone());
                            }
                        }
                    }
                }
            } else if let Some(DynamicValue::Message(id_msg)) = msg.get(3) {
                if let Some(DynamicValue::String(s)) = id_msg.get(1) {
                    if !s.is_empty() {
                        tag_names.push(s.clone());
                    }
                }
            }
            let mut st = load_state(&db_tag2);
            if let Some(entry) = st.space_tags.get_mut(&location_id) {
                for name in tag_names {
                    entry.remove(&name);
                }
            }
            save_state(&db_tag2, &st);
            Ok(any("eve.space.tag.admin.api.RemoveResponse", Vec::new()))
        }),
    );

    // 7. eve.stargate.lock.api.GetRequest, GetRestrictedSystemsRequest, ValidateJumpRequest
    let db_gl1 = Arc::clone(&db);
    router.register_handler(
        "eve.stargate.lock.api.GetRequest",
        Arc::new(move |_ctx, _any, msg| {
            let cid = extract_u64(msg, 1);
            let sys_id = extract_u64(msg, 2);
            if cid == 0 || sys_id == 0 {
                return Err((STATUS_BAD_REQUEST, "Missing character or solarsystem".to_string()));
            }
            let st = load_state(&db_gl1);
            let lock = st
                .stargate_locks
                .get(&format!("{cid}:{sys_id}"))
                .ok_or_else(|| (STATUS_NOT_FOUND, "No active stargate lock".to_string()))?;
            let mut out = Vec::new();
            encode_id_msg(1, lock.gate_id, &mut out);
            encode_timestamp(2, lock.expiry_unix, &mut out);
            Ok(any("eve.stargate.lock.api.GetResponse", out))
        }),
    );

    let db_gl2 = Arc::clone(&db);
    router.register_handler(
        "eve.stargate.lock.api.GetRestrictedSystemsRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let st = load_state(&db_gl2);
            let mut out = Vec::new();
            for sys_id in &st.restricted_systems {
                encode_id_msg(1, *sys_id, &mut out);
            }
            Ok(any("eve.stargate.lock.api.GetRestrictedSystemsResponse", out))
        }),
    );

    let db_gl3 = Arc::clone(&db);
    router.register_handler(
        "eve.stargate.lock.api.ValidateJumpRequest",
        Arc::new(move |_ctx, _any, msg| {
            let cid = extract_u64(msg, 1);
            let sys_id = extract_u64(msg, 2);
            let gate_id = extract_u64(msg, 3);
            if cid == 0 || sys_id == 0 || gate_id == 0 {
                return Err((STATUS_BAD_REQUEST, "Missing character, solarsystem, or gate".to_string()));
            }
            let st = load_state(&db_gl3);
            let mut out = Vec::new();
            if let Some(lock) = st.stargate_locks.get(&format!("{cid}:{sys_id}")) {
                if lock.gate_id != gate_id {
                    encode_varint_field(1, 0, &mut out);
                    let mut lock_buf = Vec::new();
                    encode_id_msg(1, lock.gate_id, &mut lock_buf);
                    encode_timestamp(2, lock.expiry_unix, &mut lock_buf);
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
        Arc::new(move |_ctx, _any, msg| {
            let cid = extract_u64(msg, 2);
            let sys_id = extract_u64(msg, 3);
            if cid == 0 || sys_id == 0 {
                return Err((STATUS_BAD_REQUEST, "Missing character or solarsystem".to_string()));
            }
            let mut st = load_state(&db_gl4);
            st.stargate_locks
                .remove(&format!("{cid}:{sys_id}"))
                .ok_or_else(|| (STATUS_NOT_FOUND, "Stargate lock not found".to_string()))?;
            save_state(&db_gl4, &st);
            Ok(any("google.protobuf.Empty", Vec::new()))
        }),
    );

    // 9. eve.sovereignty.api.GetClaimRequest, GetVersionManifestRequest, GetAllClaimsRequest
    let db_sov1 = Arc::clone(&db);
    router.register_handler(
        "eve.sovereignty.api.GetClaimRequest",
        Arc::new(move |_ctx, _any, msg| {
            let sys_id = extract_u64(msg, 1);
            if sys_id == 0 {
                return Err((STATUS_BAD_REQUEST, "Missing solar_system".to_string()));
            }
            let st = load_state(&db_sov1);
            let mut out = Vec::new();
            if let Some(claim) = st.sov_claims.get(&sys_id) {
                encode_varint_field(1, 0, &mut out);
                let mut cbuf = Vec::new();
                encode_id_msg(1, claim.alliance_id, &mut cbuf);
                encode_id_msg(3, claim.hub_id, &mut cbuf);
                encode_id_msg(4, claim.corporation_id, &mut cbuf);
                encode_timestamp(5, claim.claimed_at_unix, &mut cbuf);
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
            let st = load_state(&db_sov2);
            let mut out = Vec::new();
            for claim in st.sov_claims.values() {
                let mut sc = Vec::new();
                encode_id_msg(1, claim.solar_system_id, &mut sc);
                let mut ac = Vec::new();
                encode_id_msg(1, claim.alliance_id, &mut ac);
                encode_id_msg(2, claim.corporation_id, &mut ac);
                encode_timestamp(3, claim.claimed_at_unix, &mut ac);
                encode_varint_field(5, if claim.is_capital { 1 } else { 0 }, &mut ac);
                encode_bytes_field(3, &ac, &mut sc);
                encode_bytes_field(1, &sc, &mut out);
            }
            Ok(any("eve.sovereignty.api.GetAllClaimsResponse", out))
        }),
    );

    // 10. eve.sovereignty.activitydefensemodifier.api.GetLevelRequest
    let db_adm = Arc::clone(&db);
    let sde_adm = Arc::clone(&sde);
    router.register_handler(
        "eve.sovereignty.activitydefensemodifier.api.GetLevelRequest",
        Arc::new(move |_ctx, _any, msg| {
            let sys_id = extract_u64(msg, 1);
            if sys_id == 0 {
                return Err((STATUS_BAD_REQUEST, "Missing solarsystem".to_string()));
            }
            let st = load_state(&db_adm);
            let level = if let Some(&lvl) = st.adm_levels.get(&sys_id) {
                lvl
            } else if sde_adm.solar_systems.contains_key(&(sys_id as u32)) {
                10
            } else {
                return Err((STATUS_NOT_FOUND, "Solar system not found".to_string()));
            };
            let mut out = Vec::new();
            encode_varint_field(1, level, &mut out);
            Ok(any("eve.sovereignty.activitydefensemodifier.api.GetLevelResponse", out))
        }),
    );

    // 11. eve.sovereignty.resource.star.api.GetAllConfigurationsRequest
    let db_sres = Arc::clone(&db);
    router.register_handler(
        "eve.sovereignty.resource.star.api.GetAllConfigurationsRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let st = load_state(&db_sres);
            let mut out = Vec::new();
            for cfg in &st.star_resource_configs {
                let mut cbuf = Vec::new();
                encode_id_msg(1, cfg.star_id, &mut cbuf);
                encode_varint_field(2, cfg.power, &mut cbuf);
                encode_bytes_field(1, &cbuf, &mut out);
            }
            Ok(any("eve.sovereignty.resource.star.api.GetAllConfigurationsResponse", out))
        }),
    );

    // 12. eve.sovereignty.resource.planet.api.*
    router.register_handler(
        "eve.sovereignty.resource.planet.api.GetDefinitionsConfigurationRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let mut out = Vec::new();
            encode_semver(1, 1, 0, 0, &mut out);
            Ok(any(
                "eve.sovereignty.resource.planet.api.GetDefinitionsConfigurationResponse",
                out,
            ))
        }),
    );

    let db_pres1 = Arc::clone(&db);
    router.register_handler(
        "eve.sovereignty.resource.planet.api.GetAllDefinitionsRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let st = load_state(&db_pres1);
            let mut out = Vec::new();
            for cfg in &st.planet_resource_configs {
                let mut dbuf = Vec::new();
                encode_id_msg(1, cfg.planet_id, &mut dbuf);
                encode_varint_field(2, cfg.power, &mut dbuf);
                encode_varint_field(3, cfg.workforce, &mut dbuf);
                encode_bytes_field(1, &dbuf, &mut out);
            }
            encode_semver(2, 1, 0, 0, &mut out);
            Ok(any("eve.sovereignty.resource.planet.api.GetAllDefinitionsResponse", out))
        }),
    );

    let db_pres2 = Arc::clone(&db);
    router.register_handler(
        "eve.sovereignty.resource.planet.api.GetDefinitionsVersionRequest",
        Arc::new(move |_ctx, _any, msg| {
            let major = extract_u64(msg, 1);
            if major == 0 {
                return Err((STATUS_BAD_REQUEST, "Missing version specification".to_string()));
            }
            let st = load_state(&db_pres2);
            let mut out = Vec::new();
            for cfg in &st.planet_resource_configs {
                let mut dbuf = Vec::new();
                encode_id_msg(1, cfg.planet_id, &mut dbuf);
                encode_varint_field(2, cfg.power, &mut dbuf);
                encode_varint_field(3, cfg.workforce, &mut dbuf);
                encode_bytes_field(1, &dbuf, &mut out);
            }
            Ok(any("eve.sovereignty.resource.planet.api.GetDefinitionsVersionResponse", out))
        }),
    );

    let db_pres3 = Arc::clone(&db);
    router.register_handler(
        "eve.sovereignty.resource.planet.api.GetAllConfigurationsRequest",
        Arc::new(move |_ctx, _any, _msg| {
            let st = load_state(&db_pres3);
            let mut out = Vec::new();
            for cfg in &st.planet_resource_configs {
                let mut cbuf = Vec::new();
                encode_id_msg(1, cfg.planet_id, &mut cbuf);
                encode_varint_field(2, cfg.power, &mut cbuf);
                encode_varint_field(3, cfg.workforce, &mut cbuf);
                encode_bytes_field(1, &cbuf, &mut out);
            }
            Ok(any("eve.sovereignty.resource.planet.api.GetAllConfigurationsResponse", out))
        }),
    );
}
