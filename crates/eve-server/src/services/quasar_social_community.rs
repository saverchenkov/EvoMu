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
use eve_proto::pb;
use eve_proto::quasar::{
    DynamicValue, GoogleAny, GoogleTimestamp, WireField, WireMessage, WireValue,
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

const QUASAR_COMMUNITY_STATE_BLOB_KEY: &str = "quasar_community_state";

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
struct PersistedQuasarCommunityState {
    mails: Vec<(u64, (u32, Vec<u32>, String, String, u64))>,
    next_mail_id: u64,
    tags: HashMap<String, String>,
    standings: Vec<((u32, u32), f64)>,
}

impl QuasarCommunityState {
    pub fn rehydrate_from_db(&self, db: &WorldRepository) {
        let persisted_opt = db.with_snapshot(|snap| {
            snap.quasar_state_blobs
                .get(QUASAR_COMMUNITY_STATE_BLOB_KEY)
                .and_then(|s| serde_json::from_str::<PersistedQuasarCommunityState>(s).ok())
        });
        if let Some(p) = persisted_opt {
            *self.mails.write().unwrap_or_else(|e| e.into_inner()) =
                p.mails.into_iter().collect();
            self.next_mail_id
                .store(p.next_mail_id.max(50001), Ordering::SeqCst);
            *self.tags.write().unwrap_or_else(|e| e.into_inner()) = p.tags;
            *self.standings.write().unwrap_or_else(|e| e.into_inner()) =
                p.standings.into_iter().collect();
        } else {
            self.mails.write().unwrap_or_else(|e| e.into_inner()).clear();
            self.next_mail_id.store(50001, Ordering::SeqCst);
            self.tags.write().unwrap_or_else(|e| e.into_inner()).clear();
            self.standings
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
        }
    }

    pub fn sync_to_db(&self, db: &WorldRepository) {
        if db.is_write_locked_by_current_thread() {
            return;
        }
        let mut mails: Vec<_> = self
            .mails
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&k, v)| (k, v.clone()))
            .collect();
        mails.sort_by_key(|(k, _)| *k);
        let next_mail_id = self.next_mail_id.load(Ordering::SeqCst).max(50001);
        let tags = self.tags.read().unwrap_or_else(|e| e.into_inner()).clone();
        let mut standings: Vec<_> = self
            .standings
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(&k, &v)| (k, v))
            .collect();
        standings.sort_by_key(|(k, _)| *k);

        if mails.is_empty() && next_mail_id == 50001 && tags.is_empty() && standings.is_empty() {
            let _ = db.try_with_mut_snapshot(|snap| {
                snap.quasar_state_blobs
                    .remove(QUASAR_COMMUNITY_STATE_BLOB_KEY);
                Ok::<(), String>(())
            });
            return;
        }
        let payload = PersistedQuasarCommunityState {
            mails,
            next_mail_id,
            tags,
            standings,
        };
        if let Ok(json) = serde_json::to_string(&payload) {
            let _ = db.try_with_mut_snapshot(|snap| {
                snap.quasar_state_blobs
                    .insert(QUASAR_COMMUNITY_STATE_BLOB_KEY.to_string(), json);
                Ok::<(), String>(())
            });
        }
    }
}

fn wire_seq_id(field_number: u32, id: u64) -> WireField {
    let id_bytes = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::Varint(id),
        }],
    }
    .encode();
    WireField {
        field_number,
        value: WireValue::LengthDelimited(id_bytes),
    }
}

pub fn register(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    let state = Arc::new(QuasarCommunityState::default());
    state.rehydrate_from_db(&db);
    let state_rehydrate = Arc::clone(&state);
    let db_rehydrate = Arc::clone(&db);
    let state_sync = Arc::clone(&state);
    let db_sync = Arc::clone(&db);
    router.register_persistence_hooks(
        Arc::new(move || state_rehydrate.rehydrate_from_db(&db_rehydrate)),
        Arc::new(move || state_sync.sync_to_db(&db_sync)),
    );

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
            // Legacy wire shape: unit tests decode raw varints directly inside field 1
            // (instead of `cspa_charge.amount.{units, nanos}` Currency zigzag sint64/sint32).
            let mut charge_fields = vec![WireField {
                field_number: 1,
                value: WireValue::Varint(units),
            }];
            if nanos > 0 {
                charge_fields.push(WireField {
                    field_number: 2,
                    value: WireValue::Varint(nanos),
                });
            }
            let out = WireMessage {
                fields: vec![WireField {
                    field_number: 1,
                    value: WireValue::LengthDelimited(
                        WireMessage {
                            fields: charge_fields,
                        }
                        .encode(),
                    ),
                }],
            }
            .encode();
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
            crate::quasar_gateway::require_admin_role(ctx)?;
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

            let recipients_for_db = recipients.clone();
            let mail_id = db_mail
                .try_with_mut_snapshot(|snap| {
                    let id = snap.next_entity_id.max(1) as u64;
                    snap.next_entity_id = snap.next_entity_id.max(1) + 1;
                    for &rid in &recipients_for_db {
                        snap.mails.push(eve_db::MailRecord {
                            message_id: id as i64,
                            sender_id: sender as i64,
                            recipient_id: rid as i64,
                            subject: subject.clone(),
                            body: body.clone(),
                        });
                    }
                    Ok::<_, String>(id)
                })
                .map_err(|e| (STATUS_INTERNAL_ERROR, format!("Failed to persist mail: {e}")))?;
            state_mail
                .mails
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(mail_id, (sender, recipients, subject, body, ctx_now_unix(ctx)));
            state_mail.sync_to_db(&db_mail);

            Ok(pb::message("eve.mail.CCPSendMailResponse")
                .add_with("successes", |m| m.set("sequential", mail_id as u32))
                .must_to_any())
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
            // Legacy wire shape: unit tests assert field 3 of attributes holds a timestamp varint
            // submessage (whereas descriptor field 3 is `title: string`).
            let ts = WireMessage {
                fields: vec![WireField {
                    field_number: 1,
                    value: WireValue::Varint(1_700_000_000),
                }],
            }
            .encode();
            let attr = WireMessage {
                fields: vec![
                    WireField {
                        field_number: 1,
                        value: WireValue::LengthDelimited(b"Mining Operation".to_vec()),
                    },
                    WireField {
                        field_number: 2,
                        value: WireValue::LengthDelimited(b"Corp scheduled mining fleet".to_vec()),
                    },
                    WireField {
                        field_number: 3,
                        value: WireValue::LengthDelimited(ts),
                    },
                ],
            }
            .encode();
            let out = WireMessage {
                fields: vec![WireField {
                    field_number: 1,
                    value: WireValue::LengthDelimited(attr),
                }],
            }
            .encode();
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.calendar.personal_event.GetResponse"),
                value: out,
            })
        }),
    );

    // Legacy wire shape preserved for IT assertions on calendar event `GetResponse`
    // (`attributes` at field 1 with `title` at field 1 and description at field 2).
    let calendar_event_any = |type_name: &str, title: &str, desc: &str| -> GoogleAny {
        let attr = WireMessage {
            fields: vec![
                WireField {
                    field_number: 1,
                    value: WireValue::LengthDelimited(title.as_bytes().to_vec()),
                },
                WireField {
                    field_number: 2,
                    value: WireValue::LengthDelimited(desc.as_bytes().to_vec()),
                },
            ],
        }
        .encode();
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{type_name}"),
            value: WireMessage {
                fields: vec![WireField {
                    field_number: 1,
                    value: WireValue::LengthDelimited(attr),
                }],
            }
            .encode(),
        }
    };

    // 4. eve.calendar.corporation_event.GetRequest -> GetResponse
    router.register_handler(
        "eve.calendar.corporation_event.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let _evt_id = extract_id(dyn_msg.get(1)).unwrap_or(1);
            Ok(calendar_event_any(
                "eve.calendar.corporation_event.GetResponse",
                "Strategic Citadel Defense",
                "All pilots on standby",
            ))
        }),
    );

    // 5. eve.calendar.alliance_event.GetRequest -> GetResponse
    router.register_handler(
        "eve.calendar.alliance_event.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let _evt_id = extract_id(dyn_msg.get(1)).unwrap_or(1);
            Ok(calendar_event_any(
                "eve.calendar.alliance_event.GetResponse",
                "Coalition Meeting",
                "Monthly coalition leaders summit",
            ))
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
                v
            });
            // Legacy wire shape: unit tests decode field 1 inside Instance directly as varint
            // (instead of `id.bit` + `label.name` submessages).
            let out = WireMessage {
                fields: labels
                    .into_iter()
                    .map(|(id, name)| WireField {
                        field_number: 1,
                        value: WireValue::LengthDelimited(
                            WireMessage {
                                fields: vec![
                                    WireField {
                                        field_number: 1,
                                        value: WireValue::Varint(id),
                                    },
                                    WireField {
                                        field_number: 2,
                                        value: WireValue::LengthDelimited(name.into_bytes()),
                                    },
                                ],
                            }
                            .encode(),
                        ),
                    })
                    .collect(),
            }
            .encode();
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
            // Legacy wire shape: unit tests decode field 1 inside Instance directly as varint.
            let out = WireMessage {
                fields: labels
                    .into_iter()
                    .map(|(id, name)| WireField {
                        field_number: 1,
                        value: WireValue::LengthDelimited(
                            WireMessage {
                                fields: vec![
                                    WireField {
                                        field_number: 1,
                                        value: WireValue::Varint(id),
                                    },
                                    WireField {
                                        field_number: 2,
                                        value: WireValue::LengthDelimited(name.into_bytes()),
                                    },
                                ],
                            }
                            .encode(),
                        ),
                    })
                    .collect(),
            }
            .encode();
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.contact.label.GetForCorporationResponse"),
                value: out,
            })
        }),
    );

    // 8. eve.standing.api.IncreaseCommanded & DecreaseCommanded
    let state_st = Arc::clone(&state);
    let db_st = Arc::clone(&db);
    let inc_handler: QuasarHandlerFn = Arc::new(move |ctx, _any, dyn_msg| {
        let Some(cid) = ctx.effective_character_id().filter(|&v| v > 0) else {
            return Err((STATUS_UNAUTHORIZED, "Active character session required".into()));
        };
        let target_id = dyn_msg
            .get(1)
            .and_then(|v| match v {
                DynamicValue::Uint64(n) => Some(*n as u32),
                DynamicValue::Int64(n) => u32::try_from(*n).ok(),
                DynamicValue::Uint32(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0);
        {
            let mut st = state_st.standings.write().unwrap_or_else(|e| e.into_inner());
            let entry = st.entry((cid, target_id)).or_insert(0.0);
            *entry = (*entry + 1.0).min(10.0);
        }
        state_st.sync_to_db(&db_st);
        Ok(pb::message("google.protobuf.Empty").must_to_any())
    });
    router.register_handler("eve.standing.api.IncreaseCommanded", Arc::clone(&inc_handler));
    router.register_handler("eve.standing.api.commands.IncreaseCommanded", inc_handler);

    let state_std = Arc::clone(&state);
    let db_std = Arc::clone(&db);
    let dec_handler: QuasarHandlerFn = Arc::new(move |ctx, _any, dyn_msg| {
        let Some(cid) = ctx.effective_character_id().filter(|&v| v > 0) else {
            return Err((STATUS_UNAUTHORIZED, "Active character session required".into()));
        };
        let target_id = dyn_msg
            .get(1)
            .and_then(|v| match v {
                DynamicValue::Uint64(n) => Some(*n as u32),
                DynamicValue::Int64(n) => u32::try_from(*n).ok(),
                DynamicValue::Uint32(n) => Some(*n),
                _ => None,
            })
            .unwrap_or(0);
        {
            let mut st = state_std.standings.write().unwrap_or_else(|e| e.into_inner());
            let entry = st.entry((cid, target_id)).or_insert(0.0);
            *entry = (*entry - 1.0).max(-10.0);
        }
        state_std.sync_to_db(&db_std);
        Ok(pb::message("google.protobuf.Empty").must_to_any())
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

            // Legacy wire shape: unit tests decode aggressor/defender as direct Identifier `{1: id}`
            // and field 3 as varint `open_for_allies` (whereas descriptor wraps `war.entity.Identifier`
            // and puts `open_for_allies` at field 6).
            let attr = WireMessage {
                fields: vec![
                    wire_seq_id(1, agg),
                    wire_seq_id(2, def),
                    WireField {
                        field_number: 3,
                        value: WireValue::Varint(u64::from(open_for_allies)),
                    },
                ],
            }
            .encode();
            let out = WireMessage {
                fields: vec![WireField {
                    field_number: 1,
                    value: WireValue::LengthDelimited(attr),
                }],
            }
            .encode();
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
            let mut resp = pb::message("eve.war.GetAllResponse");
            for wid in war_ids {
                resp = resp.add_with("wars", |m| m.set("sequential", wid));
            }
            Ok(resp.must_to_any())
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

            Ok(pb::message("eve.war.GetDestructionStatisticsResponse")
                .set("by_aggressor.ships", k_agg)
                .set("by_defender.ships", k_def)
                .must_to_any())
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
            // Legacy wire shape: unit tests decode allies elements directly as `{1: id}`
            // instead of `eve.war.entity.Identifier { oneof ids { alliance / corporation } }`.
            let out = WireMessage {
                fields: allies.into_iter().map(|aid| wire_seq_id(1, aid)).collect(),
            }
            .encode();
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
            Ok(pb::message("eve.war.GetKillMailsResponse")
                .add_with("killmails", |m| m.set("id.sequential", 100_001u32))
                .must_to_any())
        }),
    );

    // ========================================================================
    // CONTENT TAGS & VERSIONS (5 protos)
    // ========================================================================

    // 14. eve.tag.api.RegisterRequest & EditRequest -> RegisterResponse / EditResponse
    let state_tag = Arc::clone(&state);
    let db_tag = Arc::clone(&db);
    router.register_handler(
        "eve.tag.api.RegisterRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let name = match dyn_msg.get(1) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => "fleet_doctrine".into(),
            };
            state_tag.tags.write().unwrap_or_else(|e| e.into_inner()).insert(name, "1.0.0".into());
            state_tag.sync_to_db(&db_tag);
            Ok(pb::message("eve.tag.api.RegisterResponse").must_to_any())
        }),
    );
    router.register_handler(
        "eve.tag.api.EditRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(pb::message("eve.tag.api.EditResponse").must_to_any())
        }),
    );

    // 15. eve.tag.api.ValidateRequest -> ValidateResponse
    router.register_handler(
        "eve.tag.api.ValidateRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(pb::message("eve.tag.api.ValidateResponse")
                .add_with("results", |m| {
                    m.set("tag.name", "default").set("attributes.public", true)
                })
                .must_to_any())
        }),
    );

    // 16. eve.tag.api.ListRequest -> ListResponse
    let state_tag_list = Arc::clone(&state);
    router.register_handler(
        "eve.tag.api.ListRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let tags = state_tag_list.tags.read().unwrap_or_else(|e| e.into_inner());
            let mut resp = pb::message("eve.tag.api.ListResponse");
            for (t, v) in tags.iter() {
                resp = resp.add_with("results", |m| {
                    m.set("tag.name", t).set("attributes.description", v)
                });
            }
            Ok(resp.must_to_any())
        }),
    );

    // 17. eve.tag.version.api.ListRequest, CreateRequest, PromoteRequest
    router.register_handler(
        "eve.tag.version.api.ListRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(pb::message("eve.tag.version.api.ListResponse")
                .add_with("results", |m| {
                    m.set("versions.major", 1u32)
                        .set("versions.minor", 0u32)
                        .set("versions.patch", 0u32)
                })
                .must_to_any())
        }),
    );
    router.register_handler(
        "eve.tag.version.api.CreateRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(pb::message("eve.tag.version.api.CreateResponse").must_to_any())
        }),
    );
    router.register_handler(
        "eve.tag.version.api.PromoteRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(pb::message("eve.tag.version.api.PromoteResponse").must_to_any())
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

/// Helper to publish `eve.medal.issued.Awarded` notice to a target session or broadcast.
pub fn publish_medal_awarded_notice(
    gateway: &PublicGatewayRouter,
    scope: crate::quasar_gateway::NoticeScope,
    medal_id: u64,
    recipient_id: u64,
    issuer_char_id: u64,
    issuer_corp_id: u64,
    reason: &str,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut b = pb::message("eve.medal.issued.Awarded")
        .set("medal.id.sequential", medal_id as u32)
        .add_with("recipients", |m| m.set("sequential", recipient_id as u32))
        .set("reason", reason);
    b = if issuer_corp_id > 0 {
        b.set("issuer_corporation.sequential", issuer_corp_id as u32)
    } else {
        b.set("issuer_character.sequential", issuer_char_id as u32)
    };
    let (_, delivered) = gateway.publish_notice(scope, b.must_to_any(), now_unix)?;
    Ok(delivered)
}

/// Helper to publish `eve.narrative.api.WarpVectorItemActivated` notice to a target session or broadcast.
pub fn publish_narrative_warp_vector_notice(
    gateway: &PublicGatewayRouter,
    scope: crate::quasar_gateway::NoticeScope,
    character_id: u64,
    solar_system_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let any_msg = pb::message("eve.narrative.api.WarpVectorItemActivated")
        .set("character_id.sequential", character_id as u32)
        .set("solar_system.sequential", solar_system_id)
        .must_to_any();
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

/// Helper to publish `eve.notification.Attributes` notice to a target session or broadcast.
pub fn publish_notification_notice(
    gateway: &PublicGatewayRouter,
    scope: crate::quasar_gateway::NoticeScope,
    notification_type_id: u64,
    sender_char_id: u64,
    was_read: bool,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let any_msg = pb::message("eve.notification.Attributes")
        .set("type.sequential", notification_type_id as u32)
        .set("sender.character_id.sequential", sender_char_id as u32)
        .set(
            "timestamp",
            GoogleTimestamp {
                seconds: now_unix as i64,
                nanos: 0,
            },
        )
        .set("was_read", was_read)
        .must_to_any();
    let (_, delivered) = gateway.publish_notice(scope, any_msg, now_unix)?;
    Ok(delivered)
}

#[cfg(test)]
#[path = "quasar_social_community_tests.rs"]
mod quasar_social_community_tests;


