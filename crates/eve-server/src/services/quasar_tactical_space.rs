//! `quasar_tactical_space` — Quasar Ship Unlock, Ship API, Capacitor, Module, Probe,
//! Wormhole, and Combat/Support Effect handlers and V24 protobuf model encoders.
//!
//! State persists in `WorldStateSnapshot::quasar_state_blobs["quasar_tactical_space"]`.

#![forbid(unsafe_code)]

use super::quasar_social_chat::ctx_queue_notice;
use super::quasar_space_services::{encode_domain_message, set_domain_attributes, set_domain_field};
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, DynamicQuasarMessage,
    DynamicValue, GoogleAny, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::collections::BTreeMap;
use std::sync::Arc;

const BLOB_KEY: &str = "quasar_tactical_space";

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct TacticalSpaceQuasarState {
    /// `character_id -> pending unlocked ship_type_ids`
    pub pending_ship_unlocks: BTreeMap<i64, Vec<u32>>,
    /// `character_id -> claimed ship_type_ids`
    pub claimed_ship_unlocks: BTreeMap<i64, Vec<u32>>,
}

pub fn load_state(db: &WorldRepository) -> TacticalSpaceQuasarState {
    db.with_snapshot(|s| {
        s.quasar_state_blobs
            .get(BLOB_KEY)
            .and_then(|j| serde_json::from_str(j).ok())
            .unwrap_or_default()
    })
}

pub fn save_state(db: &WorldRepository, state: &TacticalSpaceQuasarState) {
    if let Ok(j) = serde_json::to_string(state) {
        db.with_mut_snapshot(|s| {
            s.quasar_state_blobs.insert(BLOB_KEY.into(), j);
        });
    }
}

pub fn grant_pending_ship_unlock(db: &WorldRepository, char_id: i64, ship_type_id: u32) {
    let mut st = load_state(db);
    let pending = st.pending_ship_unlocks.entry(char_id).or_default();
    if !pending.contains(&ship_type_id) {
        pending.push(ship_type_id);
    }
    save_state(db, &st);
}

fn any(type_name: &str, value: Vec<u8>) -> GoogleAny {
    GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
        value,
    }
}

fn require_char(ctx: &AuthoritativeContext) -> Result<i64, (u32, String)> {
    ctx.effective_character_id()
        .map(i64::from)
        .ok_or((STATUS_UNAUTHORIZED, "Authenticated character required".to_string()))
}

fn extract_u64_id(msg: &DynamicQuasarMessage, field: u32) -> Option<u64> {
    match msg.get(field)? {
        DynamicValue::Message(m) => match m.get(1)? {
            DynamicValue::Uint64(v) => Some(*v),
            DynamicValue::Uint32(v) => Some(u64::from(*v)),
            DynamicValue::Int64(v) => Some(*v as u64),
            DynamicValue::Int32(v) => Some(*v as u64),
            _ => None,
        },
        DynamicValue::Uint64(v) => Some(*v),
        DynamicValue::Uint32(v) => Some(u64::from(*v)),
        DynamicValue::Int64(v) => Some(*v as u64),
        DynamicValue::Int32(v) => Some(*v as u64),
        _ => None,
    }
}

/// Encodes `eve.capacitor.Value` (`units`, `nanos`) from a ship's capacitor charge/capacity.
pub fn encode_capacitor_value(
    descriptors: &QuasarDescriptorRegistry,
    units: u64,
    nanos: u32,
) -> Result<GoogleAny, (u32, String)> {
    encode_domain_message(descriptors, "eve.capacitor.Value", |_reg, msg| {
        msg.set(1, DynamicValue::Uint64(units));
        msg.set(2, DynamicValue::Uint32(nanos));
    })
}

/// Encodes `eve.module.Attributes` and `eve.moduletype.Identifier` for a fitted module item.
pub fn encode_module_models(
    descriptors: &QuasarDescriptorRegistry,
    module_item_id: u64,
    module_type_id: u64,
    type_name: &str,
) -> Result<(GoogleAny, GoogleAny), (u32, String)> {
    let mod_attr = encode_domain_message(descriptors, "eve.module.Attributes", |reg, msg| {
        set_domain_attributes(reg, msg, module_type_id, module_item_id, type_name);
    })?;
    let type_id_msg = encode_domain_message(descriptors, "eve.moduletype.Identifier", |reg, msg| {
        set_domain_attributes(reg, msg, module_type_id, 1, type_name);
    })?;
    Ok((mod_attr, type_id_msg))
}

/// Encodes `eve.wormhole.Identifier` for a wormhole entity in space.
pub fn encode_wormhole_model(
    descriptors: &QuasarDescriptorRegistry,
    wormhole_id: u64,
    wormhole_type_id: u64,
    label: &str,
) -> Result<GoogleAny, (u32, String)> {
    encode_domain_message(descriptors, "eve.wormhole.Identifier", |reg, msg| {
        set_domain_attributes(reg, msg, wormhole_id, wormhole_type_id, label);
    })
}

/// Encodes `eve.probe.explorationscanresult.Attributes` and publishes a probe scan notice.
pub fn publish_probe_scan_result(
    gateway: &PublicGatewayRouter,
    char_id: u32,
    archetype_id: u64,
    signal_strength_bps: u64,
    now_unix: u64,
) -> Result<GoogleAny, (u32, String)> {
    let scan_any = encode_domain_message(
        &gateway.descriptors,
        "eve.probe.explorationscanresult.Attributes",
        |reg, msg| {
            set_domain_attributes(reg, msg, archetype_id, signal_strength_bps, "scan_result");
        },
    )?;
    let _ = gateway.publish_notice(NoticeScope::Character(char_id), scan_any.clone(), now_unix);
    Ok(scan_any)
}

/// Encodes and publishes a combat / command-burst effect notice (`eve.effect.*`).
pub fn publish_combat_effect_notice(
    gateway: &PublicGatewayRouter,
    solar_system_id: u32,
    source_char_id: u64,
    module_id: u64,
    now_unix: u64,
) -> Result<GoogleAny, (u32, String)> {
    let eff_any = encode_domain_message(
        &gateway.descriptors,
        "eve.effect.support.commandburst.event.Activated",
        |reg, msg| {
            set_domain_attributes(reg, msg, source_char_id, module_id, "command_burst");
        },
    )
    .or_else(|_| {
        encode_domain_message(
            &gateway.descriptors,
            "eve.effect.combat.pvp.Activated",
            |reg, msg| {
                set_domain_attributes(reg, msg, source_char_id, module_id, "combat_effect");
            },
        )
    })?;
    let _ = gateway.publish_notice(NoticeScope::SolarSystem(solar_system_id), eff_any.clone(), now_unix);
    Ok(eff_any)
}

/// Registers `quasar_tactical_space` handlers on `router`.
pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    let descriptors = router.descriptors.clone();

    // 1. eve_public.ship.unlock.GetPendingNotificationsRequest
    {
        let db_u = Arc::clone(&db);
        router.register_handler(
            "eve_public.ship.unlock.GetPendingNotificationsRequest",
            Arc::new(move |ctx, _any, _msg| {
                let char_id = require_char(ctx)?;
                let st = load_state(&db_u);
                let pending = st.pending_ship_unlocks.get(&char_id).cloned().unwrap_or_default();
                let mut out = Vec::new();
                for tid in pending {
                    let mut id_buf = Vec::new();
                    encode_varint_field(1, u64::from(tid), &mut id_buf);
                    encode_bytes_field(1, &id_buf, &mut out);
                }
                Ok(any("eve_public.ship.unlock.GetPendingNotificationsResponse", out))
            }),
        );
    }

    // 2. eve_public.ship.unlock.ClaimRewardRequest
    {
        let db_c = Arc::clone(&db);
        let sde_c = Arc::clone(&sde);
        router.register_handler(
            "eve_public.ship.unlock.ClaimRewardRequest",
            Arc::new(move |ctx, _any_req, dyn_msg| {
                let char_id = require_char(ctx)?;
                let ship_type_id = extract_u64_id(dyn_msg, 1)
                    .filter(|&v| v > 0)
                    .ok_or((STATUS_BAD_REQUEST, "unlocked_ship_type is required".to_string()))?
                    as u32;
                if !sde_c.types.contains_key(&ship_type_id) {
                    return Err((STATUS_NOT_FOUND, format!("Ship type {ship_type_id} not found in SDE")));
                }
                let mut st = load_state(&db_c);
                let pending = st.pending_ship_unlocks.entry(char_id).or_default();
                let pos = pending.iter().position(|&t| t == ship_type_id).ok_or((
                    STATUS_NOT_FOUND,
                    format!("No pending unlock reward for ship type {ship_type_id}"),
                ))?;
                pending.remove(pos);
                let claimed = st.claimed_ship_unlocks.entry(char_id).or_default();
                if !claimed.contains(&ship_type_id) {
                    claimed.push(ship_type_id);
                }
                save_state(&db_c, &st);

                let mut notice_out = Vec::new();
                let mut id_buf = Vec::new();
                encode_varint_field(1, u64::from(ship_type_id), &mut id_buf);
                encode_bytes_field(1, &id_buf, &mut notice_out);
                let now_unix = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                ctx_queue_notice(
                    NoticeScope::Character(char_id as u32),
                    any("eve_public.ship.unlock.ShipsUnlockedNotice", notice_out),
                    now_unix,
                );
                Ok(any("eve_public.ship.unlock.ClaimRewardResponse", Vec::new()))
            }),
        );
    }

    // 3. eve.ship.api.GetRequest
    {
        let db_s = Arc::clone(&db);
        let sde_s = Arc::clone(&sde);
        let d = Arc::clone(&descriptors);
        router.register_handler(
            "eve.ship.api.GetRequest",
            Arc::new(move |ctx, _any_req, dyn_msg| {
                let _char_id = require_char(ctx)?;
                let ship_id = extract_u64_id(dyn_msg, 1)
                    .filter(|&v| v > 0)
                    .ok_or((STATUS_BAD_REQUEST, "ship identifier is required".to_string()))?
                    as i64;
                let (type_id, owner_id) = db_s
                    .with_snapshot(|snap| snap.items.get(&ship_id).map(|it| (it.type_id, it.owner_id)))
                    .ok_or((STATUS_NOT_FOUND, format!("Ship {ship_id} not found")))?;
                let type_name = sde_s
                    .types
                    .get(&type_id)
                    .map(|t| t.name.as_str())
                    .unwrap_or("Ship");
                encode_domain_message(&d, "eve.ship.api.GetResponse", |reg, resp| {
                    set_domain_field(reg, resp, 1, u64::from(type_id), type_name);
                    set_domain_field(reg, resp, 2, owner_id as u64, type_name);
                })
            }),
        );
    }

    // 4. eve.ship.api.CanJumpStarterSpaceShipCasterRequest
    {
        let db_j = Arc::clone(&db);
        let d = Arc::clone(&descriptors);
        router.register_handler(
            "eve.ship.api.CanJumpStarterSpaceShipCasterRequest",
            Arc::new(move |ctx, _any_req, dyn_msg| {
                let caller_id = require_char(ctx)?;
                let target_char = extract_u64_id(dyn_msg, 1).unwrap_or(caller_id as u64) as i64;
                let can_jump = db_j.with_snapshot(|snap| snap.characters.contains_key(&target_char));
                encode_domain_message(
                    &d,
                    "eve.ship.api.CanJumpStarterSpaceShipCasterResponse",
                    |_reg, resp| {
                        resp.set(1, DynamicValue::Bool(can_jump));
                    },
                )
            }),
        );
    }
}
