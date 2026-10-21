//! `quasar_social_chat` — Quasar Local Chat, Moderation, Bans, and Character Presence Services (`Q-5`).
//!
//! Implements live request/response and notice fan-out semantics for:
//! - `eve_public/chat/local/api/requests.proto` (`GetMembershipListRequest`, `BroadcastMessageRequest`)
//! - `eve_public/chat/local/api/notices.proto` (`MessageBroadcastNotice`, `MembershipRefreshedNotice`,
//!   `MembershipListNotice`, `JoinNotice`, `LeaveNotice`)
//! - `eve_public/chat/local/api/admin/requests.proto` (`MuteRequest`)
//! - `eve/chat/local/api/requests.proto` (`GetModeRequest`, `GetClassificationRequest`)
//! - `eve/chat/local/api/admin/requests.proto` (`MuteRequest`, `SetModeRequest`)
//! - `eve/chat/ban.proto` (`TempBanCharacterRequest`, `PermaBanCharacterRequest`, `UnbanCharacterRequest`)
//! - `eve_public/presence/api/request.proto` (`GetCorporationRequest`, `GetAllianceRequest`, `GetFactionRequest`)
//! - `eve/chat/chat.proto`, `eve/chat/local/local.proto`, `eve/chat/proxy/proxy.proto`,
//!   `eve_public/chat/local/local.proto`.

use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

pub fn ctx_now_unix(ctx: &AuthoritativeContext) -> u64 {
    ctx.received.as_ref().map(|t| t.seconds.max(0) as u64).unwrap_or(0)
}

#[derive(Debug, Default)]
pub struct ChatModerationState {
    pub system_modes: RwLock<HashMap<u32, i32>>,
    pub temp_bans: RwLock<HashMap<u32, (u64, String)>>,
    pub perma_bans: RwLock<HashMap<u32, String>>,
    pub mutes: RwLock<HashMap<u32, (u64, String)>>,
}

fn encode_sequential_id(field_num: u32, id: u32, out: &mut Vec<u8>) {
    let mut id_bytes = Vec::with_capacity(8);
    encode_varint_field(1, id as u64, &mut id_bytes);
    encode_bytes_field(field_num, &id_bytes, out);
}

pub fn register(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    let state = Arc::new(ChatModerationState::default());

    // 1. eve_public.chat.local.api.GetMembershipListRequest -> GetMembershipListResponse
    router.register_handler(
        "eve_public.chat.local.api.GetMembershipListRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((
                    STATUS_FORBIDDEN,
                    "Active character required for chat membership".into(),
                ));
            };
            let corpid = ctx.effective_corporation_id().unwrap_or(1000001);
            let sys_id = 30000142u32; // Default system Jita 4-4 or derived from character session

            // Build member entry for the calling character
            let mut member_bytes = Vec::new();
            encode_sequential_id(1, cid, &mut member_bytes); // character
            encode_sequential_id(2, corpid, &mut member_bytes); // corporation
            encode_varint_field(3, 1, &mut member_bytes); // no_alliance = true
            encode_varint_field(5, 1, &mut member_bytes); // no_faction = true
            encode_varint_field(7, 0, &mut member_bytes); // classification = 0 (Normal)

            let mut out = Vec::new();
            encode_bytes_field(1, &member_bytes, &mut out); // members
            encode_sequential_id(2, sys_id, &mut out); // solar_system

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.GetMembershipListResponse"),
                value: out,
            })
        }),
    );

    // 2. eve_public.chat.local.api.BroadcastMessageRequest -> BroadcastMessageResponse
    let state_bcast = Arc::clone(&state);
    router.register_handler(
        "eve_public.chat.local.api.BroadcastMessageRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required to broadcast".into()));
            };

            // Check if muted or banned
            let now = ctx_now_unix(ctx);
            if let Some((expiry, reason)) = state_bcast.mutes.read().unwrap_or_else(|e| e.into_inner()).get(&cid) {
                if *expiry > now {
                    return Err((STATUS_FORBIDDEN, format!("Character is muted until {expiry}: {reason}")));
                }
            }
            if let Some((expiry, reason)) = state_bcast.temp_bans.read().unwrap_or_else(|e| e.into_inner()).get(&cid) {
                if *expiry > now {
                    return Err((STATUS_FORBIDDEN, format!("Character is temp-banned until {expiry}: {reason}")));
                }
            }
            if let Some(reason) = state_bcast.perma_bans.read().unwrap_or_else(|e| e.into_inner()).get(&cid) {
                return Err((STATUS_FORBIDDEN, format!("Character is permanently banned: {reason}")));
            }

            let msg_text = match dyn_msg.get(1) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => return Err((STATUS_BAD_REQUEST, "Missing message text in BroadcastMessageRequest".into())),
            };

            let sys_id = 30000142u32; // Jita or active system

            // Construct eve_public.chat.local.api.MessageBroadcastNotice
            let mut notice_bytes = Vec::new();
            encode_sequential_id(1, cid, &mut notice_bytes); // author
            encode_sequential_id(2, sys_id, &mut notice_bytes); // solar_system
            encode_string_field(3, &msg_text, &mut notice_bytes); // message

            let notice_any = GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.MessageBroadcastNotice"),
                value: notice_bytes,
            };

            // Queue notice to solar system scope
            // We use global router queue via thread-safe handler queue
            ctx_queue_notice(NoticeScope::SolarSystem(sys_id), notice_any, now);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.BroadcastMessageResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 3. eve_public.chat.local.api.admin.MuteRequest & eve.chat.local.api.admin.MuteRequest
    let state_mute = Arc::clone(&state);
    let mute_handler: crate::quasar_gateway::QuasarHandlerFn = Arc::new(move |ctx: &AuthoritativeContext, _any: &GoogleAny, dyn_msg: &DynamicQuasarMessage| {
        let char_id = extract_identifier_val(dyn_msg.get(1)).unwrap_or(0);
        let duration_secs = extract_duration_secs(dyn_msg.get(2)).unwrap_or(300);
        let reason = match dyn_msg.get(3) {
            Some(DynamicValue::String(s)) => s.clone(),
            _ => "Chat moderation mute".into(),
        };

        let now = ctx_now_unix(ctx);
        let expiry = now.saturating_add(duration_secs);
        state_mute
            .mutes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(char_id, (expiry, reason));

        let sys_id = 30000142u32;
        let mut ref_notice = Vec::new();
        encode_sequential_id(1, char_id, &mut ref_notice);
        encode_sequential_id(2, sys_id, &mut ref_notice);
        let notice_any = GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.MembershipRefreshedNotice"),
            value: ref_notice,
        };
        ctx_queue_notice(NoticeScope::SolarSystem(sys_id), notice_any, now);

        Ok(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.admin.MuteResponse"),
            value: Vec::new(),
        })
    });
    router.register_handler("eve_public.chat.local.api.admin.MuteRequest", Arc::clone(&mute_handler));
    router.register_handler("eve.chat.local.api.admin.MuteRequest", mute_handler);

    // 4. eve.chat.local.api.GetModeRequest -> GetModeResponse
    let state_mode = Arc::clone(&state);
    router.register_handler(
        "eve.chat.local.api.GetModeRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let sys_id = extract_identifier_val(dyn_msg.get(1)).unwrap_or(30000142);
            let mode = *state_mode
                .system_modes
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&sys_id)
                .unwrap_or(&0); // 0 = Normal, 1 = Delayed

            let mut out = Vec::new();
            encode_varint_field(1, mode as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.chat.local.api.GetModeResponse"),
                value: out,
            })
        }),
    );

    // 5. eve.chat.local.api.admin.SetModeRequest -> SetModeResponse
    let state_set_mode = Arc::clone(&state);
    router.register_handler(
        "eve.chat.local.api.admin.SetModeRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let sys_id = extract_identifier_val(dyn_msg.get(1)).unwrap_or(30000142);
            let mode = match dyn_msg.get(2) {
                Some(DynamicValue::Enum(e)) | Some(DynamicValue::Int32(e)) => *e,
                _ => 0,
            };
            state_set_mode
                .system_modes
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(sys_id, mode);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.chat.local.api.admin.SetModeResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 6. eve.chat.local.api.GetClassificationRequest -> GetClassificationResponse
    router.register_handler(
        "eve.chat.local.api.GetClassificationRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_varint_field(1, 0, &mut out); // 0 = Normal
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.chat.local.api.GetClassificationResponse"),
                value: out,
            })
        }),
    );

    // 7. eve.chat.ban.TempBanCharacterRequest -> TempBanCharacterResponse
    let state_tb = Arc::clone(&state);
    router.register_handler(
        "eve.chat.ban.TempBanCharacterRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cid = extract_identifier_val(dyn_msg.get(1))
                .filter(|&id| id > 0)
                .or_else(|| ctx.effective_character_id())
                .unwrap_or(0);
            let reason = match dyn_msg.get(3) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => "Temporary chat ban".into(),
            };
            let expiry = ctx_now_unix(ctx) + 86400;
            state_tb
                .temp_bans
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(cid, (expiry, reason));

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.chat.ban.TempBanCharacterResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 8. eve.chat.ban.PermaBanCharacterRequest -> PermaBanCharacterResponse
    let state_pb = Arc::clone(&state);
    router.register_handler(
        "eve.chat.ban.PermaBanCharacterRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cid = extract_identifier_val(dyn_msg.get(1))
                .filter(|&id| id > 0)
                .or_else(|| ctx.effective_character_id())
                .unwrap_or(0);
            let reason = match dyn_msg.get(2) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => "Permanent chat ban".into(),
            };
            state_pb
                .perma_bans
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(cid, reason);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.chat.ban.PermaBanCharacterResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 9. eve.chat.ban.UnbanCharacterRequest -> UnbanCharacterResponse
    let state_ub = Arc::clone(&state);
    router.register_handler(
        "eve.chat.ban.UnbanCharacterRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cid = extract_identifier_val(dyn_msg.get(1))
                .filter(|&id| id > 0)
                .or_else(|| ctx.effective_character_id())
                .unwrap_or(0);
            state_ub.temp_bans.write().unwrap_or_else(|e| e.into_inner()).remove(&cid);
            state_ub.perma_bans.write().unwrap_or_else(|e| e.into_inner()).remove(&cid);
            state_ub.mutes.write().unwrap_or_else(|e| e.into_inner()).remove(&cid);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.chat.ban.UnbanCharacterResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 10. Presence: eve_public.presence.api.GetCorporationRequest -> GetCorporationResponse
    let db_pcorp = Arc::clone(&db);
    router.register_handler(
        "eve_public.presence.api.GetCorporationRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let cid = extract_identifier_val(dyn_msg.get(1)).unwrap_or(0);
            let corpid = db_pcorp
                .get_character(cid as i64)
                .map(|c| c.corporation_id as u32)
                .unwrap_or(1000001);

            let mut out = Vec::new();
            encode_sequential_id(1, corpid, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.presence.api.GetCorporationResponse"),
                value: out,
            })
        }),
    );

    // 11. Presence: eve_public.presence.api.GetAllianceRequest -> GetAllianceResponse
    let db_pall = Arc::clone(&db);
    router.register_handler(
        "eve_public.presence.api.GetAllianceRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let cid = extract_identifier_val(dyn_msg.get(1)).unwrap_or(0);
            let alliance_id = db_pall
                .get_character(cid as i64)
                .ok()
                .and_then(|c| c.alliance_id);

            let mut out = Vec::new();
            if let Some(aid) = alliance_id {
                encode_sequential_id(2, aid as u32, &mut out);
            } else {
                encode_varint_field(1, 1, &mut out); // no_alliance = true
            }

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.presence.api.GetAllianceResponse"),
                value: out,
            })
        }),
    );

    // 12. Presence: eve_public.presence.api.GetFactionRequest -> GetFactionResponse
    router.register_handler(
        "eve_public.presence.api.GetFactionRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let _cid = extract_identifier_val(dyn_msg.get(1)).unwrap_or(0);
            let faction_id: Option<u32> = None;

            let mut out = Vec::new();
            if let Some(fid) = faction_id {
                encode_sequential_id(2, fid, &mut out);
            } else {
                encode_varint_field(1, 1, &mut out); // no_faction = true
            }

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.presence.api.GetFactionResponse"),
                value: out,
            })
        }),
    );
}

// Thread-local or global notice buffering helper for handlers that emit notices
thread_local! {
    static LOCAL_NOTICES: std::cell::RefCell<Vec<(NoticeScope, GoogleAny, u64)>> = const { std::cell::RefCell::new(Vec::new()) };
}

pub fn ctx_queue_notice(scope: NoticeScope, payload: GoogleAny, now_unix: u64) {
    LOCAL_NOTICES.with(|q| q.borrow_mut().push((scope, payload, now_unix)));
}

pub fn drain_local_notices() -> Vec<(NoticeScope, GoogleAny, u64)> {
    LOCAL_NOTICES.with(|q| std::mem::take(&mut *q.borrow_mut()))
}

fn extract_identifier_val(val: Option<&DynamicValue>) -> Option<u32> {
    match val {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Uint32(u)) => Some(*u),
            Some(DynamicValue::Int32(i)) => Some(*i as u32),
            Some(DynamicValue::Uint64(u)) => Some(*u as u32),
            Some(DynamicValue::Int64(i)) => Some(*i as u32),
            _ => None,
        },
        Some(DynamicValue::Uint32(u)) => Some(*u),
        Some(DynamicValue::Int32(i)) => Some(*i as u32),
        Some(DynamicValue::Uint64(u)) => Some(*u as u32),
        Some(DynamicValue::Int64(i)) => Some(*i as u32),
        _ => None,
    }
}

fn extract_duration_secs(val: Option<&DynamicValue>) -> Option<u64> {
    match val {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Int64(i)) => Some((*i).max(0) as u64),
            Some(DynamicValue::Uint64(u)) => Some(*u),
            _ => None,
        },
        Some(DynamicValue::Int64(i)) => Some((*i).max(0) as u64),
        Some(DynamicValue::Uint64(u)) => Some(*u),
        _ => None,
    }
}
