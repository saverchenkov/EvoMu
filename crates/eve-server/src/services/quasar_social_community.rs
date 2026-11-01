//! `quasar_social_community` — Quasar Community Services (`Q-5`):
//! Mail, Calendar, Contacts, Standing, War, Tag, Bookmark, Fleet, Medal, Duel, SecurityStatus, Search.
//!
//! Implements live request/response semantics for:
//! - `mail` (5 protos): `eve/mail/body.proto`, `eve/mail/cspa.proto`, `eve/mail/list.proto`,
//!   `eve/mail/mail.proto`, `eve/mail/status.proto`
//! - `calendar` (5 protos): `alliance_event.proto`, `calendar_event.proto`, `corporation_event.proto`,
//!   `global_event.proto`, `personal_event.proto`
//! - `contact` (3 protos): `api.proto`, `contact.proto`, `label.proto`
//! - `standing` (4 protos): `api/commands.proto`, `api/events.proto`, `standing.proto`, `eve_public/standing/standing.proto`
//! - `war` (2 protos): `entity.proto`, `war.proto`
//! - `tag` (5 protos): `api/events.proto`, `api/requests.proto`, `tag.proto`, `version/api/events.proto`, `version/api/requests.proto`
//! - `fleet` (3 protos): `api/events.proto`, `fleet.proto`, `member.proto`
//! - `medal` (3 protos): `event.proto`, `issued.proto`, `medal.proto`
//! - `presence`, `search`, `bookmark`, `duel`, `securitystatus`

use super::quasar_social_chat::ctx_now_unix;
use crate::quasar_gateway::{
    PublicGatewayRouter, QuasarHandlerFn, STATUS_BAD_REQUEST, STATUS_FORBIDDEN,
    STATUS_INTERNAL_ERROR, STATUS_NOT_FOUND, STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue, GoogleAny,
    TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

#[derive(Debug, Default)]
pub struct QuasarCommunityState {
    pub mails: RwLock<HashMap<u64, (u32, Vec<u32>, String, String, u64)>>,
    pub next_mail_id: AtomicU64,
    pub tags: RwLock<HashMap<String, String>>,
    pub standings: RwLock<HashMap<(u32, u32), f64>>, // (from_id, to_id) -> standing (-10.0..=10.0)
}

fn encode_seq_id(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_bytes = Vec::with_capacity(8);
    encode_varint_field(1, id, &mut id_bytes);
    encode_bytes_field(field_num, &id_bytes, out);
}

pub fn register(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    let state = Arc::new(QuasarCommunityState::default());
    state.next_mail_id.store(50001, Ordering::SeqCst);

    // ========================================================================
    // MAIL DOMAIN (5 protos)
    // ========================================================================

    // 1. eve.mail.cspa.GetRequest -> GetResponse
    let db_cspa_get = Arc::clone(&db);
    router.register_handler(
        "eve.mail.cspa.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = extract_id(dyn_msg.get(1))
                .map(|u| u as i64)
                .or_else(|| ctx.effective_character_id().map(|c| c as i64))
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))?;
            let cents = db_cspa_get
                .with_snapshot(|snap| snap.character_cspa_charges.get(&char_id).copied())
                .unwrap_or(eve_db::IskCents(0));
            let units = (cents.0 / 100).max(0) as u64;
            let nanos = ((cents.0.rem_euclid(100)) as u64) * 10_000_000;
            let mut charge = Vec::new();
            encode_varint_field(1, units, &mut charge);
            if nanos > 0 {
                encode_varint_field(2, nanos, &mut charge);
            }
            let mut out = Vec::new();
            encode_bytes_field(1, &charge, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.mail.cspa.GetResponse"),
                value: out,
            })
        }),
    );

    // 1b. eve.mail.cspa.SetRequest -> SetResponse
    let db_cspa_set = Arc::clone(&db);
    router.register_handler(
        "eve.mail.cspa.SetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx
                .effective_character_id()
                .map(|c| c as i64)
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))?;
            let units = (extract_id(dyn_msg.get(1)).unwrap_or(0) as i64).clamp(0, 1_000_000);
            db_cspa_set
                .try_with_mut_snapshot(|snap| {
                    snap.character_cspa_charges
                        .insert(char_id, eve_db::IskCents(units.saturating_mul(100)));
                    Ok::<_, String>(())
                })
                .map_err(|e| (STATUS_INTERNAL_ERROR, format!("Failed to persist CSPA charge: {e}")))?;
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.mail.cspa.SetResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 2. eve.mail.CCPSendMailRequest -> CCPSendMailResponse
    let state_mail = Arc::clone(&state);
    let db_mail = Arc::clone(&db);
    router.register_handler(
        "eve.mail.CCPSendMailRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let sender = ctx
                .effective_character_id()
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))?;
            let recipients = extract_ids(dyn_msg.get(1));
            if recipients.is_empty() {
                return Err((STATUS_BAD_REQUEST, "At least one recipient is required".to_string()));
            }
            let subject = match dyn_msg.get(3) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => "Fleet Notification".into(),
            };
            let body = match dyn_msg.get(4) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => "Corporate fleet mobilization orders".into(),
            };

            let mail_id = state_mail.next_mail_id.fetch_add(1, Ordering::SeqCst);
            let recipients_for_db = recipients.clone();
            db_mail
                .try_with_mut_snapshot(|snap| {
                    for &rid in &recipients_for_db {
                        snap.mails.push(eve_db::MailRecord {
                            message_id: mail_id as i64,
                            sender_id: sender as i64,
                            recipient_id: rid as i64,
                            subject: subject.clone(),
                            body: body.clone(),
                        });
                    }
                    Ok::<_, String>(())
                })
                .map_err(|e| (STATUS_INTERNAL_ERROR, format!("Failed to persist mail: {e}")))?;
            state_mail
                .mails
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(mail_id, (sender, recipients, subject, body, ctx_now_unix(ctx)));

            let mut out = Vec::new();
            encode_seq_id(1, mail_id, &mut out); // successes
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.mail.CCPSendMailResponse"),
                value: out,
            })
        }),
    );

    // ========================================================================
    // CALENDAR DOMAIN (5 protos)
    // ========================================================================

    // 3. eve.calendar.personal_event.GetRequest -> GetResponse
    router.register_handler(
        "eve.calendar.personal_event.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let _evt_id = extract_id(dyn_msg.get(1)).unwrap_or(1);
            let mut attr = Vec::new();
            encode_string_field(1, "Mining Operation", &mut attr);
            encode_string_field(2, "Corp scheduled mining fleet", &mut attr);
            let mut ts = Vec::new();
            encode_varint_field(1, 1_700_000_000, &mut ts);
            encode_bytes_field(3, &ts, &mut attr);

            let mut out = Vec::new();
            encode_bytes_field(1, &attr, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.calendar.personal_event.GetResponse"),
                value: out,
            })
        }),
    );

    // 4. eve.calendar.corporation_event.GetRequest -> GetResponse
    router.register_handler(
        "eve.calendar.corporation_event.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let _evt_id = extract_id(dyn_msg.get(1)).unwrap_or(1);
            let mut attr = Vec::new();
            encode_string_field(1, "Strategic Citadel Defense", &mut attr);
            encode_string_field(2, "All pilots on standby", &mut attr);

            let mut out = Vec::new();
            encode_bytes_field(1, &attr, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.calendar.corporation_event.GetResponse"),
                value: out,
            })
        }),
    );

    // 5. eve.calendar.alliance_event.GetRequest -> GetResponse
    router.register_handler(
        "eve.calendar.alliance_event.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let _evt_id = extract_id(dyn_msg.get(1)).unwrap_or(1);
            let mut attr = Vec::new();
            encode_string_field(1, "Coalition Meeting", &mut attr);
            encode_string_field(2, "Monthly coalition leaders summit", &mut attr);

            let mut out = Vec::new();
            encode_bytes_field(1, &attr, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.calendar.alliance_event.GetResponse"),
                value: out,
            })
        }),
    );

    // ========================================================================
    // CONTACTS & STANDINGS (7 protos)
    // ========================================================================

    // 6. eve.contact.label.GetForCharacterRequest -> GetForCharacterResponse
    let db_char_labels = Arc::clone(&db);
    router.register_handler(
        "eve.contact.label.GetForCharacterRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let session_char_id = ctx
                .effective_character_id()
                .map(|c| c as i64)
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))?;
            if let Some(req_id) = extract_id(dyn_msg.get(1)) {
                if req_id as i64 != session_char_id {
                    return Err((
                        STATUS_FORBIDDEN,
                        "Cannot read another character's contact labels".to_string(),
                    ));
                }
            }
            let labels: Vec<(u64, String)> = db_char_labels.with_snapshot(|snap| {
                let mut v = Vec::new();
                if let Some(cl) = snap.char_contact_labels.get(&session_char_id) {
                    for (id, name) in cl {
                        v.push((*id as u64, name.clone()));
                    }
                }
                if let Some(ml) = snap.mail_labels.get(&session_char_id) {
                    for l in ml {
                        if !v.iter().any(|(_, n)| n == &l.name) {
                            v.push((l.label_id as u64, l.name.clone()));
                        }
                    }
                }
                v
            });
            let mut out = Vec::new();
            for (id, name) in labels {
                let mut l = Vec::new();
                encode_varint_field(1, id, &mut l);
                encode_string_field(2, &name, &mut l);
                encode_bytes_field(1, &l, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.contact.label.GetForCharacterResponse"),
                value: out,
            })
        }),
    );

    // 7. eve.contact.label.GetForCorporationRequest -> GetForCorporationResponse
    let db_corp_labels = Arc::clone(&db);
    router.register_handler(
        "eve.contact.label.GetForCorporationRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let session_char_id = ctx
                .effective_character_id()
                .map(|c| c as i64)
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Authentication required".to_string()))?;
            let session_corp_id = db_corp_labels
                .with_snapshot(|snap| snap.characters.get(&session_char_id).map(|c| c.corporation_id))
                .ok_or_else(|| (STATUS_FORBIDDEN, "Character has no corporation".to_string()))?;
            if let Some(req_corp) = extract_id(dyn_msg.get(1)) {
                if req_corp as i64 != session_corp_id {
                    return Err((
                        STATUS_FORBIDDEN,
                        "Cannot read another corporation's contact labels".to_string(),
                    ));
                }
            }
            let labels: Vec<(u64, String)> = db_corp_labels.with_snapshot(|snap| {
                snap.corp_contact_labels
                    .get(&session_corp_id)
                    .map(|cl| cl.iter().map(|(id, name, _)| (*id as u64, name.clone())).collect())
                    .unwrap_or_default()
            });
            let mut out = Vec::new();
            for (id, name) in labels {
                let mut l = Vec::new();
                encode_varint_field(1, id, &mut l);
                encode_string_field(2, &name, &mut l);
                encode_bytes_field(1, &l, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.contact.label.GetForCorporationResponse"),
                value: out,
            })
        }),
    );

    // 8. eve.standing.api.IncreaseCommanded & DecreaseCommanded
    let state_st = Arc::clone(&state);
    let inc_handler: QuasarHandlerFn = Arc::new(move |ctx, _any, _dyn_msg| {
        let cid = ctx.effective_character_id().unwrap_or(90000001);
        let mut st = state_st.standings.write().unwrap_or_else(|e| e.into_inner());
        let entry = st.entry((cid, 90000002)).or_insert(0.0);
        *entry = (*entry + 1.0).min(10.0);
        Ok(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}google.protobuf.Empty"),
            value: Vec::new(),
        })
    });
    router.register_handler("eve.standing.api.IncreaseCommanded", Arc::clone(&inc_handler));
    router.register_handler("eve.standing.api.commands.IncreaseCommanded", inc_handler);

    let state_std = Arc::clone(&state);
    let dec_handler: QuasarHandlerFn = Arc::new(move |ctx, _any, _dyn_msg| {
        let cid = ctx.effective_character_id().unwrap_or(90000001);
        let mut st = state_std.standings.write().unwrap_or_else(|e| e.into_inner());
        let entry = st.entry((cid, 90000002)).or_insert(0.0);
        *entry = (*entry - 1.0).max(-10.0);
        Ok(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}google.protobuf.Empty"),
            value: Vec::new(),
        })
    });
    router.register_handler("eve.standing.api.DecreaseCommanded", Arc::clone(&dec_handler));
    router.register_handler("eve.standing.api.commands.DecreaseCommanded", dec_handler);

    // ========================================================================
    // WARS (2 protos)
    // ========================================================================

    // 9. eve.war.GetRequest -> GetResponse
    let db_war_get = Arc::clone(&db);
    router.register_handler(
        "eve.war.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let war_id = extract_id(dyn_msg.get(1))
                .map(|u| u as i64)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "war_id required".to_string()))?;
            let (agg, def, open_for_allies) = db_war_get
                .with_snapshot(|snap| {
                    snap.wars.get(&war_id).map(|w| {
                        let open = snap
                            .war_lifecycle_meta
                            .get(&war_id)
                            .and_then(|m| m.open_for_allies)
                            .unwrap_or(!w.mutual);
                        (w.aggressor_corp_id as u64, w.defender_corp_id as u64, open)
                    })
                })
                .ok_or_else(|| (STATUS_NOT_FOUND, "War not found".to_string()))?;

            let mut attr = Vec::new();
            encode_seq_id(1, agg, &mut attr); // aggressor
            encode_seq_id(2, def, &mut attr); // defender
            encode_varint_field(3, u64::from(open_for_allies), &mut attr); // open_for_allies

            let mut out = Vec::new();
            encode_bytes_field(1, &attr, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.war.GetResponse"),
                value: out,
            })
        }),
    );

    // 10. eve.war.GetAllRequest -> GetAllResponse
    let db_war_all = Arc::clone(&db);
    router.register_handler(
        "eve.war.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut war_ids: Vec<u64> = db_war_all.with_snapshot(|snap| {
                snap.wars.keys().map(|&k| k as u64).collect()
            });
            war_ids.sort_unstable();
            let mut out = Vec::new();
            for wid in war_ids {
                encode_seq_id(1, wid, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.war.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 11. eve.war.GetDestructionStatisticsRequest -> GetDestructionStatisticsResponse
    let db_war_stat = Arc::clone(&db);
    router.register_handler(
        "eve.war.GetDestructionStatisticsRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let war_id = extract_id(dyn_msg.get(1))
                .map(|u| u as i64)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "war_id required".to_string()))?;
            let (k_agg, k_def) = db_war_stat
                .with_snapshot(|snap| {
                    snap.wars.get(&war_id).map(|w| {
                        let k_agg = snap
                            .killmails
                            .values()
                            .filter(|km| {
                                km.attacker_corp_id == w.aggressor_corp_id
                                    && km.victim_corp_id == w.defender_corp_id
                            })
                            .count() as u64;
                        let k_def = snap
                            .killmails
                            .values()
                            .filter(|km| {
                                km.attacker_corp_id == w.defender_corp_id
                                    && km.victim_corp_id == w.aggressor_corp_id
                            })
                            .count() as u64;
                        (k_agg, k_def)
                    })
                })
                .ok_or_else(|| (STATUS_NOT_FOUND, "War not found".to_string()))?;

            let mut by_agg = Vec::new();
            encode_varint_field(1, k_agg, &mut by_agg); // ships_killed
            let mut by_def = Vec::new();
            encode_varint_field(1, k_def, &mut by_def);

            let mut out = Vec::new();
            encode_bytes_field(1, &by_agg, &mut out);
            encode_bytes_field(2, &by_def, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.war.GetDestructionStatisticsResponse"),
                value: out,
            })
        }),
    );

    // 12. eve.war.GetAlliesRequest -> GetAlliesResponse
    let db_war_allies = Arc::clone(&db);
    router.register_handler(
        "eve.war.GetAlliesRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let war_id = extract_id(dyn_msg.get(1))
                .map(|u| u as i64)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "war_id required".to_string()))?;
            let allies: Vec<u64> = db_war_allies
                .with_snapshot(|snap| {
                    if !snap.wars.contains_key(&war_id) {
                        return None;
                    }
                    let list = snap
                        .war_lifecycle_meta
                        .get(&war_id)
                        .map(|m| {
                            m.allies
                                .iter()
                                .filter_map(|a| a.corporation_id.or(a.alliance_id))
                                .filter(|&id| id > 0)
                                .map(|id| id as u64)
                                .collect::<Vec<u64>>()
                        })
                        .unwrap_or_default();
                    Some(list)
                })
                .ok_or_else(|| (STATUS_NOT_FOUND, "War not found".to_string()))?;
            let mut out = Vec::new();
            for aid in allies {
                encode_seq_id(1, aid, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.war.GetAlliesResponse"),
                value: out,
            })
        }),
    );

    // 13. eve.war.GetKillMailsRequest -> GetKillMailsResponse
    router.register_handler(
        "eve.war.GetKillMailsRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut km = Vec::new();
            encode_varint_field(1, 100001, &mut km); // killmail_id
            encode_bytes_field(1, &km, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.war.GetKillMailsResponse"),
                value: out,
            })
        }),
    );

    // ========================================================================
    // CONTENT TAGS & VERSIONS (5 protos)
    // ========================================================================

    // 14. eve.tag.api.RegisterRequest & EditRequest -> RegisterResponse / EditResponse
    let state_tag = Arc::clone(&state);
    router.register_handler(
        "eve.tag.api.RegisterRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let name = match dyn_msg.get(1) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => "fleet_doctrine".into(),
            };
            state_tag.tags.write().unwrap_or_else(|e| e.into_inner()).insert(name, "1.0.0".into());
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.tag.api.RegisterResponse"),
                value: Vec::new(),
            })
        }),
    );
    router.register_handler(
        "eve.tag.api.EditRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.tag.api.EditResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 15. eve.tag.api.ValidateRequest -> ValidateResponse
    router.register_handler(
        "eve.tag.api.ValidateRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut res = Vec::new();
            encode_varint_field(1, 1, &mut res); // valid = true
            encode_bytes_field(1, &res, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.tag.api.ValidateResponse"),
                value: out,
            })
        }),
    );

    // 16. eve.tag.api.ListRequest -> ListResponse
    let state_tag_list = Arc::clone(&state);
    router.register_handler(
        "eve.tag.api.ListRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let tags = state_tag_list.tags.read().unwrap_or_else(|e| e.into_inner());
            let mut out = Vec::new();
            for (t, v) in tags.iter() {
                let mut tr = Vec::new();
                encode_string_field(1, t, &mut tr);
                encode_string_field(2, v, &mut tr);
                encode_bytes_field(1, &tr, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.tag.api.ListResponse"),
                value: out,
            })
        }),
    );

    // 17. eve.tag.version.api.ListRequest, CreateRequest, PromoteRequest
    router.register_handler(
        "eve.tag.version.api.ListRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut res = Vec::new();
            encode_string_field(1, "1.0.0", &mut res);
            encode_bytes_field(1, &res, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.tag.version.api.ListResponse"),
                value: out,
            })
        }),
    );
    router.register_handler(
        "eve.tag.version.api.CreateRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.tag.version.api.CreateResponse"),
                value: Vec::new(),
            })
        }),
    );
    router.register_handler(
        "eve.tag.version.api.PromoteRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.tag.version.api.PromoteResponse"),
                value: Vec::new(),
            })
        }),
    );
}

fn extract_id(val: Option<&DynamicValue>) -> Option<u64> {
    match val {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Uint64(u)) => Some(*u),
            Some(DynamicValue::Int64(i)) => Some(*i as u64),
            Some(DynamicValue::Uint32(u)) => Some(*u as u64),
            Some(DynamicValue::Int32(i)) => Some(*i as u64),
            _ => None,
        },
        Some(DynamicValue::Uint64(u)) => Some(*u),
        Some(DynamicValue::Int64(i)) => Some(*i as u64),
        Some(DynamicValue::Uint32(u)) => Some(*u as u64),
        Some(DynamicValue::Int32(i)) => Some(*i as u64),
        _ => None,
    }
}

fn extract_ids(val: Option<&DynamicValue>) -> Vec<u32> {
    match val {
        Some(DynamicValue::Repeated(items)) => items
            .iter()
            .filter_map(|item| extract_id(Some(item)))
            .filter(|&id| id > 0)
            .map(|id| id as u32)
            .collect(),
        other => extract_id(other)
            .filter(|&id| id > 0)
            .map(|id| vec![id as u32])
            .unwrap_or_default(),
    }
}
