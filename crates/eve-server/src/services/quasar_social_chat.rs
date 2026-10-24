//! `quasar_social_chat` — Quasar Local Chat, Moderation, and Character Presence Services (`Q-5`).
//!
//! Implements live request/response and notice fan-out semantics for:
//! - `eve_public/chat/local/api/requests.proto` (`GetMembershipListRequest`, `BroadcastMessageRequest`)
//! - `eve_public/chat/local/api/notices.proto` (`MessageBroadcastNotice`, `MembershipRefreshedNotice`,
//!   `MembershipListNotice`, `JoinNotice`, `LeaveNotice`)
//! - `eve_public/chat/local/api/admin/requests.proto` (`MuteRequest`)
//! - `eve_public/presence/api/request.proto` (`GetCorporationRequest`, `GetAllianceRequest`, `GetFactionRequest`)
//! - `eve_public/chat/local/local.proto`.

use crate::quasar_gateway::{
    require_admin_role, GatewaySubscriber, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST,
    STATUS_FORBIDDEN, STATUS_NOT_FOUND,
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
    pub mutes: RwLock<HashMap<u32, (u64, String)>>,
    pub wormhole_speakers: RwLock<std::collections::HashSet<(u32, u32)>>,
}

impl ChatModerationState {
    pub fn new(db: &WorldRepository) -> Self {
        let initial_mutes = load_mutes_from_db(db);
        Self {
            mutes: RwLock::new(initial_mutes),
            wormhole_speakers: RwLock::new(std::collections::HashSet::new()),
        }
    }
}

pub fn load_mutes_from_db(db: &WorldRepository) -> HashMap<u32, (u64, String)> {
    db.with_snapshot(|snap| {
        snap.quasar_state_blobs
            .get("quasar_chat_mutes")
            .and_then(|s| serde_json::from_str(s).ok())
            .unwrap_or_default()
    })
}

pub fn save_mutes_to_db(db: &WorldRepository, mutes: &HashMap<u32, (u64, String)>) {
    if let Ok(json) = serde_json::to_string(mutes) {
        let _ = db.try_with_mut_snapshot(|snap| {
            snap.quasar_state_blobs
                .insert("quasar_chat_mutes".into(), json);
            Ok::<(), String>(())
        });
    }
}

pub fn encode_chat_member(
    cid: u32,
    corpid: u32,
    alliance_id: Option<u32>,
    faction_id: Option<u32>,
) -> Vec<u8> {
    let mut member_bytes = Vec::new();
    encode_sequential_id(1, cid, &mut member_bytes); // character
    encode_sequential_id(2, corpid, &mut member_bytes); // corporation
    if let Some(aid) = alliance_id {
        encode_sequential_id(4, aid, &mut member_bytes); // alliance
    } else {
        encode_varint_field(3, 1, &mut member_bytes); // no_alliance = true
    }
    if let Some(fid) = faction_id {
        encode_sequential_id(6, fid, &mut member_bytes); // faction
    } else {
        encode_varint_field(5, 1, &mut member_bytes); // no_faction = true
    }
    encode_varint_field(7, 0, &mut member_bytes); // classification = 0 (CLASSIFICATION_UNSPECIFIED)
    member_bytes
}

pub fn is_wormhole_system(sys_id: u32) -> bool {
    (31_000_000..32_000_000).contains(&sys_id)
}

pub fn get_solar_system_members(
    db: &WorldRepository,
    subscribers: Option<&Arc<RwLock<HashMap<u64, GatewaySubscriber>>>>,
    sys_id: u32,
    caller_cid: Option<u32>,
    _caller_corpid: Option<u32>,
    moderation: Option<&ChatModerationState>,
) -> (Vec<Vec<u8>>, Vec<u32>) {
    let is_wh = is_wormhole_system(sys_id);
    let wh_speakers: Option<std::collections::HashSet<u32>> = if is_wh {
        moderation.map(|m| {
            m.wormhole_speakers
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .filter(|(s, _)| *s == sys_id)
                .map(|(_, c)| *c)
                .collect()
        })
    } else {
        None
    };

    let active_sub_chars: std::collections::HashSet<u32> = subscribers
        .map(|s| {
            s.read()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .filter(|sub| sub.solar_system_id == Some(sys_id))
                .filter_map(|sub| sub.character_id)
                .collect()
        })
        .unwrap_or_default();

    db.with_snapshot(|snap| {
        let online_chars: std::collections::HashSet<i64> = snap
            .sessions
            .values()
            .filter_map(|s| s.character_id)
            .collect();

        let mut seen = std::collections::HashSet::new();
        let mut member_bytes_list = Vec::new();
        let mut member_ids = Vec::new();

        for c in snap.characters.values() {
            if c.solar_system_id as u32 != sys_id {
                continue;
            }
            let cid = c.character_id as u32;
            let is_online = online_chars.contains(&c.character_id)
                || active_sub_chars.contains(&cid);
            let is_speaker = !is_wh || wh_speakers.as_ref().map(|s| s.contains(&cid)).unwrap_or(false);
            if is_online && is_speaker && seen.insert(cid) {
                member_ids.push(cid);
                member_bytes_list.push(encode_chat_member(
                    cid,
                    c.corporation_id as u32,
                    c.alliance_id.map(|a| a as u32),
                    c.war_faction_id.map(|f| f as u32),
                ));
            }
        }

        if let Some(cid) = caller_cid {
            let caller_speaker = !is_wh || wh_speakers.as_ref().map(|s| s.contains(&cid)).unwrap_or(false);
            if caller_speaker {
                let is_online = online_chars.contains(&(cid as i64))
                    || active_sub_chars.contains(&cid);
                if is_online {
                    if let Some(c) = snap.characters.get(&(cid as i64)) {
                        if c.solar_system_id as u32 == sys_id && seen.insert(cid) {
                            member_ids.push(cid);
                            member_bytes_list.push(encode_chat_member(
                                cid,
                                c.corporation_id as u32,
                                c.alliance_id.map(|a| a as u32),
                                c.war_faction_id.map(|f| f as u32),
                            ));
                        }
                    }
                }
            }
        }

        (member_bytes_list, member_ids)
    })
}

fn encode_sequential_id(field_num: u32, id: u32, out: &mut Vec<u8>) {
    let mut id_bytes = Vec::with_capacity(8);
    encode_varint_field(1, id as u64, &mut id_bytes);
    encode_bytes_field(field_num, &id_bytes, out);
}

pub fn register(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    let state = Arc::clone(&router.chat_moderation);

    // 1. eve_public.chat.local.api.GetMembershipListRequest -> GetMembershipListResponse
    let db_mem = Arc::clone(&db);
    let subs_mem = Arc::clone(&router.subscribers);
    let state_mem = Arc::clone(&state);
    router.register_handler(
        "eve_public.chat.local.api.GetMembershipListRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((
                    STATUS_FORBIDDEN,
                    "Active character required for chat membership".into(),
                ));
            };
            let caller_corpid = ctx.effective_corporation_id();
            let sys_id = match db_mem.with_snapshot(|snap| {
                snap.characters
                    .get(&(cid as i64))
                    .map(|c| c.solar_system_id as u32)
            }) {
                Some(sid) => sid,
                None => {
                    return Err((
                        STATUS_NOT_FOUND,
                        "Character not found or has no solar system".into(),
                    ));
                }
            };

            let (member_bytes_list, _) = get_solar_system_members(
                &db_mem,
                Some(&subs_mem),
                sys_id,
                Some(cid),
                caller_corpid,
                Some(&state_mem),
            );

            let mut out = Vec::new();
            for m in &member_bytes_list {
                encode_bytes_field(1, m, &mut out);
            }
            encode_sequential_id(2, sys_id, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.GetMembershipListResponse"),
                value: out,
            })
        }),
    );

    // 2. eve_public.chat.local.api.BroadcastMessageRequest -> BroadcastMessageResponse
    let db_bcast = Arc::clone(&db);
    let state_bcast = Arc::clone(&state);
    let subs_bcast = Arc::clone(&router.subscribers);
    router.register_handler(
        "eve_public.chat.local.api.BroadcastMessageRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((
                    STATUS_FORBIDDEN,
                    "Active character required to broadcast message".into(),
                ));
            };
            let now = ctx_now_unix(ctx);

            if let Some((expiry, reason)) = state_bcast
                .mutes
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .get(&cid)
                .cloned()
            {
                if expiry > now {
                    return Err((STATUS_FORBIDDEN, format!("Character is muted until {expiry}: {reason}")));
                }
            }

            let msg_text = match dyn_msg.get(1) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => return Err((STATUS_BAD_REQUEST, "Missing message text in BroadcastMessageRequest".into())),
            };

            let (caller_sys, corpid, aid, fid) = match db_bcast.with_snapshot(|snap| {
                snap.characters
                    .get(&(cid as i64))
                    .map(|c| {
                        (
                            c.solar_system_id as u32,
                            c.corporation_id as u32,
                            c.alliance_id.map(|a| a as u32),
                            c.war_faction_id.map(|f| f as u32),
                        )
                    })
            }) {
                Some(tuple) => tuple,
                None => {
                    return Err((
                        STATUS_NOT_FOUND,
                        "Character not found or has no solar system".into(),
                    ));
                }
            };

            let sys_id = caller_sys;

            let is_wh = is_wormhole_system(sys_id);
            if is_wh {
                let is_new = state_bcast
                    .wormhole_speakers
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .insert((sys_id, cid));
                if is_new {
                    let member_bytes = encode_chat_member(cid, corpid, aid, fid);
                    let mut join_payload = Vec::new();
                    encode_bytes_field(1, &member_bytes, &mut join_payload);
                    encode_sequential_id(2, sys_id, &mut join_payload);
                    let join_any = GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.JoinNotice"),
                        value: join_payload,
                    };
                    ctx_queue_notice(NoticeScope::SolarSystem(sys_id), join_any, now);
                }
            } else {
                let (_, member_ids) = get_solar_system_members(
                    &db_bcast,
                    Some(&subs_bcast),
                    sys_id,
                    Some(cid),
                    None,
                    Some(&state_bcast),
                );
                if !member_ids.contains(&cid) {
                    return Err((
                        STATUS_FORBIDDEN,
                        "Caller is not a member of local chat in this solar system".into(),
                    ));
                }
            }

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
            ctx_queue_notice(NoticeScope::SolarSystem(sys_id), notice_any, now);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.BroadcastMessageResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 3. eve_public.chat.local.api.admin.MuteRequest
    let state_mute = Arc::clone(&state);
    let db_mute = Arc::clone(&db);
    router.register_handler(
        "eve_public.chat.local.api.admin.MuteRequest",
        Arc::new(move |ctx: &AuthoritativeContext, _any: &GoogleAny, dyn_msg: &DynamicQuasarMessage| {
            require_admin_role(ctx)?;

            let char_id = extract_identifier_val(dyn_msg.get(1))
                .filter(|&c| c > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or invalid character in MuteRequest".into()))?;
            let duration_secs = extract_duration_secs(dyn_msg.get(2))
                .filter(|&d| d > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or invalid duration in MuteRequest".into()))?;
            let reason = match dyn_msg.get(3) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => "Chat moderation mute".into(),
            };

            let (sys_id, corpid, aid, fid) = match db_mute.with_snapshot(|snap| {
                snap.characters
                    .get(&(char_id as i64))
                    .map(|c| {
                        (
                            c.solar_system_id as u32,
                            c.corporation_id as u32,
                            c.alliance_id.map(|a| a as u32),
                            c.war_faction_id.map(|f| f as u32),
                        )
                    })
            }) {
                Some(quad) => quad,
                None => return Err((STATUS_NOT_FOUND, "Target character not found".into())),
            };

            let now = ctx_now_unix(ctx);
            let expiry = now.saturating_add(duration_secs);
            let mutes_snapshot = {
                let mut mutes = state_mute
                    .mutes
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                mutes.retain(|_, (exp, _)| *exp > now);
                mutes.insert(char_id, (expiry, reason));
                mutes.clone()
            };
            save_mutes_to_db(&db_mute, &mutes_snapshot);

            let member_bytes = encode_chat_member(char_id, corpid, aid, fid);
            let mut ref_notice = Vec::new();
            encode_bytes_field(1, &member_bytes, &mut ref_notice);
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
        }),
    );

    // 4. Presence: eve_public.presence.api.GetCorporationRequest -> GetCorporationResponse
    let db_pcorp = Arc::clone(&db);
    router.register_handler(
        "eve_public.presence.api.GetCorporationRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let cid = extract_identifier_val(dyn_msg.get(1))
                .filter(|&id| id > 0)
                .or_else(|| _ctx.effective_character_id())
                .unwrap_or(0);
            let corpid = match db_pcorp.get_character(cid as i64) {
                Ok(c) => c.corporation_id as u32,
                Err(_) => return Err((STATUS_NOT_FOUND, "Character not found".into())),
            };

            let mut out = Vec::new();
            encode_sequential_id(1, corpid, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.presence.api.GetCorporationResponse"),
                value: out,
            })
        }),
    );

    // 5. Presence: eve_public.presence.api.GetAllianceRequest -> GetAllianceResponse
    let db_pall = Arc::clone(&db);
    router.register_handler(
        "eve_public.presence.api.GetAllianceRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let cid = extract_identifier_val(dyn_msg.get(1))
                .filter(|&id| id > 0)
                .or_else(|| _ctx.effective_character_id())
                .unwrap_or(0);
            let alliance_id = match db_pall.get_character(cid as i64) {
                Ok(c) => c.alliance_id,
                Err(_) => return Err((STATUS_NOT_FOUND, "Character not found".into())),
            };

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

    // 6. Presence: eve_public.presence.api.GetFactionRequest -> GetFactionResponse
    let db_faction = Arc::clone(&db);
    router.register_handler(
        "eve_public.presence.api.GetFactionRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let cid = extract_identifier_val(dyn_msg.get(1))
                .filter(|&id| id > 0)
                .or_else(|| _ctx.effective_character_id())
                .unwrap_or(0);
            let faction_id: Option<u32> = match db_faction.with_snapshot(|snap| {
                snap.characters
                    .get(&(cid as i64))
                    .map(|c| c.war_faction_id.map(|f| f as u32))
            }) {
                Some(f) => f,
                None => return Err((STATUS_NOT_FOUND, "Character not found".into())),
            };

            let mut out = Vec::new();
            if let Some(fid) = faction_id {
                encode_sequential_id(2, fid as u32, &mut out);
            } else {
                encode_varint_field(1, 1, &mut out); // unaligned = true
            }

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.presence.api.GetFactionResponse"),
                value: out,
            })
        }),
    );
}

// Thread-local notice buffering helper for handlers that emit notices
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
            Some(DynamicValue::Int32(i)) => u32::try_from(*i).ok(),
            Some(DynamicValue::Uint64(u)) => u32::try_from(*u).ok(),
            Some(DynamicValue::Int64(i)) => u32::try_from(*i).ok(),
            _ => None,
        },
        Some(DynamicValue::Uint32(u)) => Some(*u),
        Some(DynamicValue::Int32(i)) => u32::try_from(*i).ok(),
        Some(DynamicValue::Uint64(u)) => u32::try_from(*u).ok(),
        Some(DynamicValue::Int64(i)) => u32::try_from(*i).ok(),
        _ => None,
    }
}

fn extract_duration_secs(val: Option<&DynamicValue>) -> Option<u64> {
    match val {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Int64(i)) if *i > 0 => Some(*i as u64),
            Some(DynamicValue::Uint64(u)) if *u > 0 => Some(*u),
            _ => None,
        },
        Some(DynamicValue::Int64(i)) if *i > 0 => Some(*i as u64),
        Some(DynamicValue::Uint64(u)) if *u > 0 => Some(*u),
        _ => None,
    }
}

pub fn on_character_enter_local_chat(
    gateway: &PublicGatewayRouter,
    db: &WorldRepository,
    char_id: u32,
    solar_system_id: u32,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let (corpid, aid, fid) = match db.with_snapshot(|snap| {
        snap.characters
            .get(&(char_id as i64))
            .map(|c| {
                (
                    c.corporation_id as u32,
                    c.alliance_id.map(|a| a as u32),
                    c.war_faction_id.map(|f| f as u32),
                )
            })
    }) {
        Some(triple) => triple,
        None => return Ok(0),
    };

    let is_wh = is_wormhole_system(solar_system_id);
    let d1 = if !is_wh {
        let member_bytes = encode_chat_member(char_id, corpid, aid, fid);
        let mut join_payload = Vec::new();
        encode_bytes_field(1, &member_bytes, &mut join_payload);
        encode_sequential_id(2, solar_system_id, &mut join_payload);
        let join_any = GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.JoinNotice"),
            value: join_payload,
        };
        let (_, count) = gateway.publish_notice(
            NoticeScope::SolarSystem(solar_system_id),
            join_any,
            now_unix,
        )?;
        count
    } else {
        0
    };

    // 2. MembershipListNotice published to Character scope ONLY
    let (members, _) = get_solar_system_members(
        db,
        Some(&gateway.subscribers),
        solar_system_id,
        Some(char_id),
        Some(corpid),
        Some(&gateway.chat_moderation),
    );

    let mut list_payload = Vec::new();
    for m in &members {
        encode_bytes_field(1, m, &mut list_payload);
    }
    encode_sequential_id(2, solar_system_id, &mut list_payload);
    let list_any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.MembershipListNotice"),
        value: list_payload,
    };
    let (_, d2) = gateway.publish_notice(
        NoticeScope::Character(char_id),
        list_any,
        now_unix,
    )?;

    Ok(d1 + d2)
}

pub fn on_character_leave_local_chat(
    gateway: &PublicGatewayRouter,
    _db: &WorldRepository,
    char_id: u32,
    solar_system_id: u32,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let is_wh = is_wormhole_system(solar_system_id);
    if is_wh {
        let was_revealed = gateway
            .chat_moderation
            .wormhole_speakers
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&(solar_system_id, char_id));
        if !was_revealed {
            return Ok(0);
        }
    }
    let mut leave_payload = Vec::new();
    encode_sequential_id(1, char_id, &mut leave_payload);
    encode_sequential_id(2, solar_system_id, &mut leave_payload);
    let leave_any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.LeaveNotice"),
        value: leave_payload,
    };
    let (_, delivered) = gateway.publish_notice(
        NoticeScope::SolarSystem(solar_system_id),
        leave_any,
        now_unix,
    )?;
    Ok(delivered)
}

pub fn on_character_local_chat_membership_refreshed(
    gateway: &PublicGatewayRouter,
    db: &WorldRepository,
    char_id: u32,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let (sys_id, corpid, aid, fid) = match db.with_snapshot(|snap| {
        snap.characters
            .get(&(char_id as i64))
            .map(|c| {
                (
                    c.solar_system_id as u32,
                    c.corporation_id as u32,
                    c.alliance_id.map(|a| a as u32),
                    c.war_faction_id.map(|f| f as u32),
                )
            })
    }) {
        Some(quad) => quad,
        None => return Ok(0),
    };

    let member_bytes = encode_chat_member(char_id, corpid, aid, fid);
    let mut ref_notice = Vec::new();
    encode_bytes_field(1, &member_bytes, &mut ref_notice);
    encode_sequential_id(2, sys_id, &mut ref_notice);
    let notice_any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.MembershipRefreshedNotice"),
        value: ref_notice,
    };
    let (_, delivered) = gateway.publish_notice(
        NoticeScope::SolarSystem(sys_id),
        notice_any,
        now_unix,
    )?;
    Ok(delivered)
}
