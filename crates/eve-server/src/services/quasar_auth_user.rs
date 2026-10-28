//! `quasar_auth_user` — Quasar Public Identity, Auth & User License Services (`Q-3`).

use crate::quasar_gateway::{NoticeScope, PublicGatewayRouter};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue, GoogleAny,
    TYPE_GOOGLEAPIS_PREFIX,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QuasarIdentityStore {
    pub plex_balances: HashMap<u64, i64>,
    pub omega_subscriptions: HashMap<u64, u64>,
    pub user_emails: HashMap<u64, String>,
    pub user_tags: HashMap<u64, Vec<String>>,
    pub accepted_agreements: HashMap<u64, Vec<String>>,
    pub redeemed_codes: HashSet<String>,
    pub user_entitlements: HashMap<u64, Vec<String>>,
    pub payment_tokens: HashMap<u64, Vec<String>>,
    pub billing_schedules: HashMap<u64, Vec<String>>,
    #[serde(default)]
    pub char_alliance_logo_ships: HashMap<u64, Vec<u64>>,
    #[serde(default)]
    pub char_corp_logo_ships: HashMap<u64, Vec<u64>>,
    #[serde(default)]
    pub steam_links: HashMap<String, u64>,
    #[serde(default)]
    pub epic_links: HashMap<String, u64>,
    #[serde(default)]
    pub twitch_links: HashMap<String, u64>,
    #[serde(default)]
    pub delivered_external_grants: HashSet<String>,
}

const QUASAR_IDENTITY_NOTE_ID: i64 = -8_500_000_000_000;
const QUASAR_IDENTITY_NOTE_OWNER_ID: i64 = -85;

fn default_identity_store() -> QuasarIdentityStore {
    QuasarIdentityStore::default()
}

pub fn activate_user_omega_license_in_snapshot(
    snap: &mut eve_db::WorldStateSnapshot,
    user_id: u64,
    duration_seconds: u64,
    now_unix: u64,
) -> (u64, GoogleAny) {
    let mut st = snap
        .owner_notes
        .get(&QUASAR_IDENTITY_NOTE_ID)
        .and_then(|n| serde_json::from_str::<QuasarIdentityStore>(&n.note).ok())
        .unwrap_or_else(default_identity_store);

    let cur = st.omega_subscriptions.get(&user_id).copied().unwrap_or(now_unix);
    let base = if cur > now_unix { cur } else { now_unix };
    let next = base.saturating_add(duration_seconds);
    st.omega_subscriptions.insert(user_id, next);

    if let Ok(json) = serde_json::to_string(&st) {
        snap.owner_notes.insert(
            QUASAR_IDENTITY_NOTE_ID,
            eve_db::OwnerNoteRecord {
                note_id: QUASAR_IDENTITY_NOTE_ID,
                character_id: QUASAR_IDENTITY_NOTE_OWNER_ID,
                item_id: 0,
                label: "S:QUASAR_IDENTITY_STORE".to_string(),
                note: json,
                timestamp_unix: 0,
            },
        );
    }

    for ch in snap.characters.values_mut() {
        if ch.user_id == user_id {
            ch.clone_grade = 2; // Omega
        }
    }

    let mut notice_bytes = Vec::new();
    let mut ident = Vec::new();
    encode_string_field(1, "omega", &mut ident);
    encode_bytes_field(1, &ident, &mut notice_bytes);

    let mut lic_attr = Vec::new();
    encode_varint_field(1, 0, &mut lic_attr); // no_expiry_date = false
    let mut exp_ts = Vec::new();
    encode_varint_field(1, next, &mut exp_ts);
    encode_bytes_field(2, &exp_ts, &mut lic_attr);
    encode_bytes_field(2, &lic_attr, &mut notice_bytes);

    let notice = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.user.license.api.ActivatedNotice"),
        value: notice_bytes,
    };
    (next, notice)
}

pub fn activate_user_omega_license(
    db: &WorldRepository,
    router_notices: &std::sync::RwLock<Vec<(NoticeScope, GoogleAny)>>,
    user_id: u64,
    duration_seconds: u64,
    now_unix: u64,
) -> u64 {
    let (new_expiry, notice) = db.with_mut_snapshot(|snap| {
        activate_user_omega_license_in_snapshot(snap, user_id, duration_seconds, now_unix)
    });

    router_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((NoticeScope::User(user_id as i32), notice));
    new_expiry
}

pub fn expire_user_omega_license(
    db: &WorldRepository,
    router_notices: &std::sync::RwLock<Vec<(NoticeScope, GoogleAny)>>,
    user_id: u64,
) {
    db.with_mut_snapshot(|snap| {
        let mut st = snap
            .owner_notes
            .get(&QUASAR_IDENTITY_NOTE_ID)
            .and_then(|n| serde_json::from_str::<QuasarIdentityStore>(&n.note).ok())
            .unwrap_or_else(default_identity_store);

        st.omega_subscriptions.insert(user_id, 0);

        if let Ok(json) = serde_json::to_string(&st) {
            snap.owner_notes.insert(
                QUASAR_IDENTITY_NOTE_ID,
                eve_db::OwnerNoteRecord {
                    note_id: QUASAR_IDENTITY_NOTE_ID,
                    character_id: QUASAR_IDENTITY_NOTE_OWNER_ID,
                    item_id: 0,
                    label: "S:QUASAR_IDENTITY_STORE".to_string(),
                    note: json,
                    timestamp_unix: 0,
                },
            );
        }

        for ch in snap.characters.values_mut() {
            if ch.user_id == user_id {
                ch.clone_grade = 1; // Alpha
            }
        }
    });

    let mut notice_bytes = Vec::new();
    let mut ident = Vec::new();
    encode_string_field(1, "omega", &mut ident);
    encode_bytes_field(1, &ident, &mut notice_bytes);

    let mut lic_attr = Vec::new();
    encode_varint_field(1, 0, &mut lic_attr);
    let mut exp_ts = Vec::new();
    encode_varint_field(1, 0, &mut exp_ts);
    encode_bytes_field(2, &exp_ts, &mut lic_attr);
    encode_bytes_field(2, &lic_attr, &mut notice_bytes);

    router_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((
            NoticeScope::User(user_id as i32),
            GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.user.license.api.ExpiredNotice"
                ),
                value: notice_bytes,
            },
        ));
}

pub fn load_identity_store_from_db(db: &WorldRepository) -> Option<QuasarIdentityStore> {
    db.with_snapshot(|snap| {
        snap.owner_notes
            .get(&QUASAR_IDENTITY_NOTE_ID)
            .and_then(|n| serde_json::from_str::<QuasarIdentityStore>(&n.note).ok())
    })
}

pub fn save_identity_store_to_db(db: &WorldRepository, store: &QuasarIdentityStore) {
    if let Ok(json) = serde_json::to_string(store) {
        db.with_mut_snapshot(|snap| {
            snap.owner_notes.insert(
                QUASAR_IDENTITY_NOTE_ID,
                eve_db::OwnerNoteRecord {
                    note_id: QUASAR_IDENTITY_NOTE_ID,
                    character_id: QUASAR_IDENTITY_NOTE_OWNER_ID,
                    item_id: 0,
                    label: "S:QUASAR_IDENTITY_STORE".to_string(),
                    note: json,
                    timestamp_unix: 0,
                },
            );
        });
    }
}

pub fn read_identity_store<R>(
    db: &WorldRepository,
    f: impl FnOnce(&QuasarIdentityStore) -> R,
) -> R {
    let state = load_identity_store_from_db(db).unwrap_or_else(default_identity_store);
    f(&state)
}

pub fn with_identity_store<R>(
    db: &WorldRepository,
    f: impl FnOnce(&mut QuasarIdentityStore) -> R,
) -> R {
    let mut state = load_identity_store_from_db(db).unwrap_or_else(default_identity_store);
    let out = f(&mut state);
    save_identity_store_to_db(db, &state);
    out
}

pub fn register_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {

    // 1. eve_public.auth.AuthenticateUsernameRequest -> eve_public.auth.AuthenticateUsernameResponse
    let db_auth = Arc::clone(&db);
    let auth_handler: crate::quasar_gateway::QuasarHandlerFn = Arc::new(move |_ctx, _any, dyn_msg| {
        let mut username = String::new();
        let mut password = String::new();
        if let Some(DynamicValue::Message(creds)) = dyn_msg.get(1) {
            if let Some(DynamicValue::String(u)) = creds.get(1) {
                username = u.clone();
            }
            match creds.get(2) {
                Some(DynamicValue::Message(enc)) => {
                    if let Some(DynamicValue::Bytes(b)) = enc.get(2) {
                        password = String::from_utf8_lossy(b).into_owned();
                    } else if let Some(DynamicValue::String(s)) = enc.get(2) {
                        password = s.clone();
                    }
                }
                Some(DynamicValue::String(p)) => {
                    password = p.clone();
                }
                Some(DynamicValue::Bytes(b)) => {
                    password = String::from_utf8_lossy(b).into_owned();
                }
                _ => {}
            }
        } else {
            if let Some(DynamicValue::String(u)) = dyn_msg.get(1) {
                username = u.clone();
            }
            if let Some(DynamicValue::String(p)) = dyn_msg.get(2) {
                password = p.clone();
            }
        }
        if username.is_empty() || password.is_empty() {
            return Err((
                crate::quasar_gateway::STATUS_UNAUTHORIZED,
                "Invalid username or password".into(),
            ));
        }
        if db_auth.authenticate(&username, &password).is_err() {
            return Err((
                crate::quasar_gateway::STATUS_UNAUTHORIZED,
                "Invalid username or password".into(),
            ));
        }
        let mut token_result = Vec::new();
        encode_string_field(
            1,
            &format!("evomu-auth-token-{username}"),
            &mut token_result,
        );
        encode_varint_field(2, 0, &mut token_result); // is_pwned_password = false

        let mut out = Vec::new();
        encode_bytes_field(1, &token_result, &mut out); // field 1: login_successful
        Ok(GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.auth.AuthenticateUsernameResponse"
            ),
            value: out,
        })
    });
    router.register_handler(
        "eve_public.auth.AuthenticateUsernameRequest",
        auth_handler,
    );

    // 2. eve_public.auth.crypto.rsa.GetPublicKeyRequest -> eve_public.auth.crypto.rsa.GetPublicKeyResponse
    router.register_handler(
        "eve_public.auth.crypto.rsa.GetPublicKeyRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_string_field(1, "evomu-rsa-pub-v24", &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.auth.crypto.rsa.GetPublicKeyResponse"
                ),
                value: out,
            })
        }),
    );

    // 3. eve_public.auth.steam.AuthenticateSteamRequest -> eve_public.auth.steam.AuthenticateSteamResponse
    let db_steam_auth = Arc::clone(&db);
    router.register_handler(
        "eve_public.auth.steam.AuthenticateSteamRequest",
        Arc::new(move |ctx, any, dyn_msg| {
            let caller_uid = ctx
                .effective_user_id()
                .ok_or_else(|| {
                    (
                        crate::quasar_gateway::STATUS_UNAUTHORIZED,
                        "Authentication required for Steam auth".to_string(),
                    )
                })? as u64;
            let steam_id = extract_dyn_string(dyn_msg, &any.value, 1).unwrap_or_default();
            let uid = if !steam_id.is_empty() {
                let linked = read_identity_store(&db_steam_auth, |st| {
                    st.steam_links.get(&steam_id).copied()
                })
                .ok_or_else(|| {
                    (
                        crate::quasar_gateway::STATUS_UNAUTHORIZED,
                        format!("Steam account '{steam_id}' is not linked"),
                    )
                })?;
                if linked != caller_uid && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                    return Err((
                        crate::quasar_gateway::STATUS_FORBIDDEN,
                        "Steam account is linked to a different user".to_string(),
                    ));
                }
                linked
            } else {
                caller_uid
            };
            let mut out = Vec::new();
            encode_string_field(1, &format!("evomu-steam-token-{uid}"), &mut out);
            encode_varint_field(2, uid, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.auth.steam.AuthenticateSteamResponse"
                ),
                value: out,
            })
        }),
    );

    // 4. eve_public.user.license.api.GetRequest -> eve_public.user.license.api.GetResponse
    let db_lic_get = Arc::clone(&db);
    router.register_handler(
        "eve_public.user.license.api.GetRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let now = ctx.now_unix();
            let expiry = read_identity_store(&db_lic_get, |guard| {
                guard.omega_subscriptions.get(&uid).copied().unwrap_or(0)
            });
            let is_active = expiry > now;

            let mut lic_attr = Vec::new();
            encode_varint_field(1, 0, &mut lic_attr); // no_expiry_date = false
            if is_active && expiry > 0 {
                let mut exp_ts = Vec::new();
                encode_varint_field(1, expiry, &mut exp_ts);
                encode_bytes_field(2, &exp_ts, &mut lic_attr);
            }
            let mut out = Vec::new();
            encode_bytes_field(1, &lic_attr, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.user.license.api.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 5. eve.legal.agreement.api.GetLatestRequest -> GetLatestResponse
    let db_latest = Arc::clone(&db);
    router.register_handler(
        "eve.legal.agreement.api.GetLatestRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let accepted = read_identity_store(&db_latest, |st| {
                st.accepted_agreements.get(&uid).cloned().unwrap_or_default()
            });
            let mut doc = Vec::new();
            encode_string_field(1, "EVE Online End User License Agreement V24.01", &mut doc);
            for ag in &accepted {
                encode_string_field(2, ag, &mut doc);
            }
            let mut out = Vec::new();
            encode_bytes_field(1, &doc, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.legal.agreement.api.GetLatestResponse"
                ),
                value: out,
            })
        }),
    );

    // 6. eve.user.legal.agreement.api.AcceptRequest -> AcceptResponse
    let db_accept = Arc::clone(&db);
    router.register_handler(
        "eve.user.legal.agreement.api.AcceptRequest",
        Arc::new(move |ctx, any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let agreement_id = if !any.value.is_empty() {
                String::from_utf8_lossy(&any.value).into_owned()
            } else {
                "eula_v24".to_string()
            };
            with_identity_store(&db_accept, |guard| {
                guard
                    .accepted_agreements
                    .entry(uid)
                    .or_default()
                    .push(agreement_id);
            });
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.user.legal.agreement.api.AcceptResponse"
                ),
                value: vec![],
            })
        }),
    );

    // 7. eve.localization.message.api.GetLocalizedTextRequest -> GetLocalizedTextResponse
    router.register_handler(
        "eve.localization.message.api.GetLocalizedTextRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_string_field(1, "Welcome to EVE Online Tranquility", &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.localization.message.api.GetLocalizedTextResponse"
                ),
                value: out,
            })
        }),
    );

    register_federated_and_transfer_handlers(router, db);
}

fn extract_dyn_u64(
    msg: &eve_proto::quasar::DynamicQuasarMessage,
    raw_bytes: &[u8],
    field_num: u32,
) -> Option<u64> {
    fn from_val(v: &DynamicValue) -> Option<u64> {
        match v {
            DynamicValue::Uint64(x) if *x > 0 => Some(*x),
            DynamicValue::Int64(x) if *x > 0 => Some(*x as u64),
            DynamicValue::Sint64(x) if *x > 0 => Some(*x as u64),
            DynamicValue::Uint32(x) if *x > 0 => Some(*x as u64),
            DynamicValue::Int32(x) if *x > 0 => Some(*x as u64),
            DynamicValue::Sint32(x) if *x > 0 => Some(*x as u64),
            DynamicValue::Message(m) => m.get(1).and_then(from_val),
            _ => None,
        }
    }
    msg.get(field_num)
        .or_else(|| msg.get(90 + field_num))
        .and_then(from_val)
        .or_else(|| {
            let wm = eve_proto::quasar::WireMessage::decode(raw_bytes).ok()?;
            wm.fields
                .iter()
                .find(|f| f.field_number == field_num || f.field_number == 90 + field_num)
                .and_then(|f| f.value.as_varint())
                .filter(|&v| v > 0)
        })
}

fn extract_dyn_string(
    msg: &eve_proto::quasar::DynamicQuasarMessage,
    raw_bytes: &[u8],
    field_num: u32,
) -> Option<String> {
    fn from_val(v: &DynamicValue) -> Option<String> {
        match v {
            DynamicValue::String(s) if !s.is_empty() => Some(s.clone()),
            DynamicValue::Bytes(b) if !b.is_empty() => {
                String::from_utf8(b.clone()).ok().filter(|s| !s.is_empty())
            }
            DynamicValue::Uint64(x) if *x > 0 => Some(x.to_string()),
            DynamicValue::Int64(x) if *x > 0 => Some(x.to_string()),
            DynamicValue::Message(m) => m.get(1).and_then(from_val),
            _ => None,
        }
    }
    msg.get(field_num)
        .or_else(|| msg.get(90 + field_num))
        .and_then(from_val)
        .or_else(|| {
            let wm = eve_proto::quasar::WireMessage::decode(raw_bytes).ok()?;
            wm.fields
                .iter()
                .find(|f| f.field_number == field_num || f.field_number == 90 + field_num)
                .and_then(|f| f.value.as_str().ok().flatten().map(str::to_string))
                .filter(|s| !s.is_empty())
        })
}

fn register_federated_and_transfer_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
) {
    // Steam / Epic federated account linking & entitlement delivery (`feat:account.steam-epic-accounts`)
    let db_steam_link = Arc::clone(&db);
    router.register_handler(
        "eve.vendors.steam.user.api.admin.LinkRequest",
        Arc::new(move |ctx, any, dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let steam_id = extract_dyn_string(dyn_msg, &any.value, 1).unwrap_or_else(|| "steam_default".into());
            let uid = extract_dyn_u64(dyn_msg, &any.value, 2)
                .or_else(|| ctx.effective_user_id().map(|u| u as u64))
                .unwrap_or(1);
            with_identity_store(&db_steam_link, |st| {
                st.steam_links.insert(steam_id.clone(), uid);
            });
            let mut out = Vec::new();
            encode_string_field(1, &steam_id, &mut out);
            encode_varint_field(2, uid, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.vendors.steam.user.api.admin.LinkResponse"
                ),
                value: out,
            })
        }),
    );

    let db_epic_user = Arc::clone(&db);
    router.register_handler(
        "eve.vendors.epic.api.GetUserByEpicUserIDRequest",
        Arc::new(move |ctx, any, dyn_msg| {
            let caller_uid = ctx
                .effective_user_id()
                .ok_or_else(|| {
                    (
                        crate::quasar_gateway::STATUS_UNAUTHORIZED,
                        "Authentication required".to_string(),
                    )
                })? as u64;
            let epic_id = extract_dyn_string(dyn_msg, &any.value, 1).unwrap_or_else(|| "epic_default".into());
            let explicit_uid = extract_dyn_u64(dyn_msg, &any.value, 2);
            if let Some(u) = explicit_uid {
                if u != caller_uid && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                    return Err((
                        crate::quasar_gateway::STATUS_FORBIDDEN,
                        "Cannot bind Epic identity to another user account".into(),
                    ));
                }
            }
            let uid = with_identity_store(&db_epic_user, |st| {
                if let Some(u) = explicit_uid {
                    st.epic_links.insert(epic_id.clone(), u);
                    u
                } else {
                    *st.epic_links.entry(epic_id.clone()).or_insert(caller_uid)
                }
            });
            let mut out = Vec::new();
            encode_varint_field(1, uid, &mut out);
            encode_string_field(2, &epic_id, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.vendors.epic.api.GetUserByEpicUserIDResponse"
                ),
                value: out,
            })
        }),
    );

    let db_epic_deliv = Arc::clone(&db);
    router.register_handler(
        "eve.vendors.epic.api.DeliverEntitlementRequest",
        Arc::new(move |ctx, any, dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let epic_id = extract_dyn_string(dyn_msg, &any.value, 1).unwrap_or_else(|| "epic_default".into());
            let grant_id = extract_dyn_string(dyn_msg, &any.value, 2).unwrap_or_else(|| format!("epic_grant:{epic_id}"));
            let type_id = extract_dyn_u64(dyn_msg, &any.value, 3).unwrap_or(44992) as u32;
            let qty = extract_dyn_u64(dyn_msg, &any.value, 4).unwrap_or(1) as i32;
            let (uid, is_new) = with_identity_store(&db_epic_deliv, |st| {
                let u = st
                    .epic_links
                    .get(&epic_id)
                    .copied()
                    .or_else(|| st.steam_links.get(&epic_id).copied())
                    .or_else(|| ctx.effective_user_id().map(|x| x as u64))
                    .unwrap_or(1);
                let inserted = st.delivered_external_grants.insert(grant_id.clone());
                (u, inserted)
            });
            if !is_new {
                return Err((
                    crate::quasar_gateway::STATUS_CONFLICT,
                    format!("External entitlement {grant_id} has already been delivered"),
                ));
            }
            let token_id = db_epic_deliv.with_mut_snapshot(|snap| {
                super::account_services::enqueue_redeem_token_in_snapshot(
                    snap,
                    uid,
                    type_id,
                    qty,
                    format!("External store entitlement ({grant_id})"),
                    None,
                    None,
                )
            });
            let mut out = Vec::new();
            encode_varint_field(1, token_id as u64, &mut out);
            encode_varint_field(2, uid, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.vendors.epic.api.DeliverEntitlementResponse"
                ),
                value: out,
            })
        }),
    );

    let db_epic_ents = Arc::clone(&db);
    router.register_handler(
        "eve.vendors.epic.api.GetEntitlementsRequest",
        Arc::new(move |ctx, any, dyn_msg| {
            let caller_uid = ctx
                .effective_user_id()
                .ok_or_else(|| {
                    (
                        crate::quasar_gateway::STATUS_UNAUTHORIZED,
                        "Authentication required".to_string(),
                    )
                })? as u64;
            let epic_id = extract_dyn_string(dyn_msg, &any.value, 1).unwrap_or_default();
            let uid = read_identity_store(&db_epic_ents, |st| {
                st.epic_links
                    .get(&epic_id)
                    .copied()
                    .filter(|&u| u == caller_uid || crate::quasar_gateway::require_admin_role(ctx).is_ok())
                    .unwrap_or(caller_uid)
            });
            let tokens = db_epic_ents.with_snapshot(|snap| {
                super::account_services::load_store(snap)
                    .redeem_tokens
                    .get(&uid)
                    .cloned()
                    .unwrap_or_default()
            });
            let mut out = Vec::new();
            for t in tokens {
                encode_varint_field(1, t.token_id as u64, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.vendors.epic.api.GetEntitlementsResponse"
                ),
                value: out,
            })
        }),
    );

    // Twitch account linking & Twitch Drops delivery (`feat:account.twitch-drops`)
    let db_twitch_link = Arc::clone(&db);
    router.register_handler(
        "eve.id.federate.twitch.api.CompleteLinkingRequest",
        Arc::new(move |ctx, any, dyn_msg| {
            let caller_uid = ctx
                .effective_user_id()
                .ok_or_else(|| {
                    (
                        crate::quasar_gateway::STATUS_UNAUTHORIZED,
                        "Authentication required".to_string(),
                    )
                })? as u64;
            let twitch_id = extract_dyn_string(dyn_msg, &any.value, 1).unwrap_or_else(|| "twitch_default".into());
            let uid = extract_dyn_u64(dyn_msg, &any.value, 2).unwrap_or(caller_uid);
            if uid != caller_uid && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                return Err((
                    crate::quasar_gateway::STATUS_FORBIDDEN,
                    "Cannot link Twitch identity for another account".into(),
                ));
            }
            with_identity_store(&db_twitch_link, |st| {
                st.twitch_links.insert(twitch_id.clone(), uid);
            });
            let mut out = Vec::new();
            encode_string_field(1, &twitch_id, &mut out);
            encode_varint_field(2, uid, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.id.federate.twitch.api.CompleteLinkingResponse"
                ),
                value: out,
            })
        }),
    );

    for (req_name, rsp_name) in [
        (
            "eve.id.federate.twitch.api.GetRequest",
            "eve.id.federate.twitch.api.GetResponse",
        ),
        (
            "eve.id.federate.twitch.api.GetEveIDRequest",
            "eve.id.federate.twitch.api.GetEveIDResponse",
        ),
    ] {
        let db_tw_get = Arc::clone(&db);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, any, dyn_msg| {
                let twitch_id = extract_dyn_string(dyn_msg, &any.value, 1).unwrap_or_default();
                let uid = read_identity_store(&db_tw_get, |st| {
                    if !twitch_id.is_empty() {
                        st.twitch_links.get(&twitch_id).copied()
                    } else {
                        let caller = ctx.effective_user_id().unwrap_or(1) as u64;
                        st.twitch_links
                            .iter()
                            .find(|(_, &u)| u == caller)
                            .map(|(_, &u)| u)
                    }
                })
                .ok_or_else(|| {
                    (
                        crate::quasar_gateway::STATUS_NOT_FOUND,
                        "Twitch account is not linked to an EVE account".to_string(),
                    )
                })?;
                let mut out = Vec::new();
                encode_varint_field(1, uid, &mut out);
                encode_string_field(2, &twitch_id, &mut out);
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rsp_name}"),
                    value: out,
                })
            }),
        );
    }

    for (req_name, rsp_name) in [
        (
            "eve.vendors.twitch.entitlement.api.DropEventReceived",
            "eve.vendors.twitch.entitlement.api.DropEventReceivedResponse",
        ),
        (
            "eve.vendors.twitch.entitlement.api.DropReceived",
            "eve.vendors.twitch.entitlement.api.DropReceivedResponse",
        ),
        (
            "eve.id.federate.twitch.api.VerifyExternalGrantRequest",
            "eve.id.federate.twitch.api.VerifyExternalGrantResponse",
        ),
    ] {
        let db_tw_drop = Arc::clone(&db);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, any, dyn_msg| {
                crate::quasar_gateway::require_admin_role(ctx)?;
                let twitch_id = extract_dyn_string(dyn_msg, &any.value, 1).unwrap_or_default();
                let drop_id = extract_dyn_string(dyn_msg, &any.value, 2)
                    .unwrap_or_else(|| format!("twitch_drop:{twitch_id}"));
                let type_id = extract_dyn_u64(dyn_msg, &any.value, 3).unwrap_or(44992) as u32;
                let qty = extract_dyn_u64(dyn_msg, &any.value, 4).unwrap_or(1) as i32;
                let grant_key = format!("twitch:{drop_id}");
                let (uid_opt, is_new) = with_identity_store(&db_tw_drop, |st| {
                    let Some(&u) = st.twitch_links.get(&twitch_id) else {
                        return (None, false);
                    };
                    let inserted = st.delivered_external_grants.insert(grant_key.clone());
                    (Some(u), inserted)
                });
                let Some(uid) = uid_opt else {
                    return Err((
                        crate::quasar_gateway::STATUS_PRECONDITION_FAILED,
                        format!("Twitch account '{twitch_id}' is not linked to an EVE account"),
                    ));
                };
                if !is_new {
                    return Err((
                        crate::quasar_gateway::STATUS_CONFLICT,
                        format!("Twitch drop '{drop_id}' has already been delivered"),
                    ));
                }
                let token_id = db_tw_drop.with_mut_snapshot(|snap| {
                    super::account_services::enqueue_redeem_token_in_snapshot(
                        snap,
                        uid,
                        type_id,
                        qty,
                        format!("Twitch Drop reward ({drop_id})"),
                        None,
                        None,
                    )
                });
                let mut out = Vec::new();
                encode_varint_field(1, token_id as u64, &mut out);
                encode_varint_field(2, uid, &mut out);
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rsp_name}"),
                    value: out,
                })
            }),
        );
    }

    // Character Transfer (`feat:account.character-transfer`)
    for (req_name, rsp_name, is_execute) in [
        (
            "eve.user.character.transfer.GetAvailabilityRequest",
            "eve.user.character.transfer.GetAvailabilityResponse",
            false,
        ),
        (
            "eve.user.character.transfer.ValidateRequest",
            "eve.user.character.transfer.ValidateResponse",
            false,
        ),
        (
            "eve.user.character.transfer.QueueRequest",
            "eve.user.character.transfer.QueueResponse",
            true,
        ),
    ] {
        let db_xfer = Arc::clone(&db);
        let pending_xfer = Arc::clone(&router.pending_notices);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, any, dyn_msg| {
                let caller_uid = ctx
                    .effective_user_id()
                    .ok_or_else(|| {
                        (
                            crate::quasar_gateway::STATUS_UNAUTHORIZED,
                            "Authentication required for character transfer".to_string(),
                        )
                    })? as u64;
                let is_admin = crate::quasar_gateway::require_admin_role(ctx).is_ok();
                let char_id = extract_dyn_u64(dyn_msg, &any.value, 1)
                    .map(|v| v as i64)
                    .or_else(|| ctx.effective_character_id().map(|c| c as i64))
                    .ok_or_else(|| {
                        (
                            crate::quasar_gateway::STATUS_BAD_REQUEST,
                            "Character ID required for character transfer".to_string(),
                        )
                    })?;
                let dest_uid = extract_dyn_u64(dyn_msg, &any.value, 2).unwrap_or(caller_uid);
                if dest_uid == 0 {
                    return Err((
                        crate::quasar_gateway::STATUS_BAD_REQUEST,
                        "Valid destination user ID required for character transfer".to_string(),
                    ));
                }
                let dest_consent_token = extract_dyn_string(dyn_msg, &any.value, 3);
                let now_unix = ctx.now_unix();
                let cooldown_key = format!("char_transfer_last_unix:{char_id}");

                let xfer_res: Result<(), (u32, String)> = db_xfer.try_with_mut_snapshot(|snap| {
                    let Some(ch) = snap.characters.get(&char_id).cloned() else {
                        return Err((
                            crate::quasar_gateway::STATUS_NOT_FOUND,
                            format!("Character {char_id} not found"),
                        ));
                    };
                    if ch.user_id != caller_uid && !is_admin {
                        return Err((
                            crate::quasar_gateway::STATUS_FORBIDDEN,
                            format!("Character {char_id} is not owned by user {caller_uid}"),
                        ));
                    }
                    if is_execute && dest_uid == ch.user_id {
                        return Err((
                            crate::quasar_gateway::STATUS_BAD_REQUEST,
                            "Destination account must differ from source account".into(),
                        ));
                    }
                    let Some(dest_acc) = snap.accounts.values().find(|a| a.user_id == dest_uid) else {
                        return Err((
                            crate::quasar_gateway::STATUS_NOT_FOUND,
                            format!("Destination account {dest_uid} not found"),
                        ));
                    };
                    if dest_acc.banned_until_unix.is_some_and(|u| u > now_unix) && !is_admin {
                        return Err((
                            crate::quasar_gateway::STATUS_FORBIDDEN,
                            format!("Destination account {dest_uid} is banned"),
                        ));
                    }
                    let consent_key = format!("char_transfer_consent_token:{dest_uid}");
                    if dest_uid != caller_uid && !is_admin {
                        if let Some(expected_tok) = snap.service_state_blobs.get(&consent_key) {
                            if expected_tok.trim().is_empty()
                                || dest_consent_token.as_deref() != Some(expected_tok.as_str())
                            {
                                return Err((
                                    crate::quasar_gateway::STATUS_FORBIDDEN,
                                    "Destination account transfer consent token mismatch".into(),
                                ));
                            }
                        } else if dest_consent_token.is_some() {
                            return Err((
                                crate::quasar_gateway::STATUS_FORBIDDEN,
                                "Destination account has no active transfer consent token".into(),
                            ));
                        }
                    }
                    // Must be in an NPC corporation (1_000_000..2_000_000) and not a player corp CEO
                    let is_npc_corp = (1_000_000..2_000_000).contains(&ch.corporation_id);
                    let is_ceo = snap.corporations.get(&ch.corporation_id).is_some_and(|c| c.ceo_id == char_id);
                    if !is_npc_corp || is_ceo {
                        return Err((
                            crate::quasar_gateway::STATUS_PRECONDITION_FAILED,
                            "Character must be in an NPC corporation and not a CEO to transfer".into(),
                        ));
                    }
                    // Must have at least 1,000 ISK balance
                    if ch.balance.0 < 100_000 {
                        return Err((
                            crate::quasar_gateway::STATUS_PRECONDITION_FAILED,
                            "Character requires at least 1,000 ISK wallet balance to transfer".into(),
                        ));
                    }
                    // Must have no active market orders (personal or corporate issued by char_id), contracts, or industry jobs
                    if snap.market_orders.values().any(|o| {
                        o.vol_remaining > 0
                            && (o.owner_id == char_id
                                || snap
                                    .market_order_meta
                                    .get(&o.order_id)
                                    .and_then(|m| m.issued_by)
                                    == Some(char_id))
                    }) {
                        return Err((
                            crate::quasar_gateway::STATUS_PRECONDITION_FAILED,
                            "Character has active market orders".into(),
                        ));
                    }
                    if snap.contracts.values().any(|c| {
                        (c.issuer_id == char_id || c.acceptor_id == Some(char_id) || c.assignee_id == char_id)
                            && (c.status.eq_ignore_ascii_case("outstanding")
                                || c.status.eq_ignore_ascii_case("inprogress")
                                || c.status.eq_ignore_ascii_case("in_progress"))
                    }) {
                        return Err((
                            crate::quasar_gateway::STATUS_PRECONDITION_FAILED,
                            "Character has active contracts".into(),
                        ));
                    }
                    if snap.industry_jobs.values().any(|j| {
                        j.installer_id == char_id
                            && (j.status.eq_ignore_ascii_case("active")
                                || j.status.eq_ignore_ascii_case("paused")
                                || j.status.eq_ignore_ascii_case("ready"))
                    }) {
                        return Err((
                            crate::quasar_gateway::STATUS_PRECONDITION_FAILED,
                            "Character has active industry jobs".into(),
                        ));
                    }
                    if let Some(last_u) = snap
                        .service_state_blobs
                        .get(&cooldown_key)
                        .and_then(|s| s.parse::<u64>().ok())
                    {
                        if now_unix < last_u.saturating_add(10 * 86_400) {
                            return Err((
                                crate::quasar_gateway::STATUS_PRECONDITION_FAILED,
                                "Character transfer 10-day cooldown is still active".into(),
                            ));
                        }
                    }
                    if is_execute || dest_uid != caller_uid {
                        let dest_char_count = snap
                            .characters
                            .values()
                            .filter(|c| c.user_id == dest_uid)
                            .count();
                        if dest_char_count >= 3 {
                            return Err((
                                crate::quasar_gateway::STATUS_PRECONDITION_FAILED,
                                format!("Destination account {dest_uid} already has the maximum 3 characters"),
                            ));
                        }
                    }
                    if is_execute {
                        // Paid transfer requires 1,000 PLEX in account PLEX vault or character PLEX balance
                        let vault_plex = snap.account_plex_vault.get(&ch.user_id).copied().unwrap_or(0);
                        if vault_plex >= 1_000 {
                            snap.account_plex_vault.insert(ch.user_id, vault_plex - 1_000);
                        } else if ch.plex_balance >= 1_000 {
                            if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                                ch_mut.plex_balance -= 1_000;
                            }
                        } else {
                            return Err((
                                crate::quasar_gateway::STATUS_PRECONDITION_FAILED,
                                "Paid character transfer requires 1,000 PLEX transfer fee".into(),
                            ));
                        }
                        let dest_omega_expiry = snap
                            .owner_notes
                            .get(&QUASAR_IDENTITY_NOTE_ID)
                            .and_then(|n| serde_json::from_str::<QuasarIdentityStore>(&n.note).ok())
                            .and_then(|st| st.omega_subscriptions.get(&dest_uid).copied())
                            .unwrap_or(0);
                        if let Some(ch_mut) = snap.characters.get_mut(&char_id) {
                            ch_mut.user_id = dest_uid;
                            ch_mut.clone_grade = if dest_omega_expiry > now_unix { 2 } else { 1 };
                            ch_mut.version = ch_mut.version.wrapping_add(1);
                        }
                        snap.service_state_blobs.remove(&consent_key);
                        snap.service_state_blobs.insert(
                            format!("char_transfer_evict_session:{char_id}"),
                            now_unix.to_string(),
                        );
                        snap.employment_history
                            .entry(char_id)
                            .or_default()
                            .push((ch.corporation_id, now_unix, None));
                        snap.next_entity_id += 1;
                        let jid = snap.next_entity_id;
                        snap.wallet_journal.push(eve_db::WalletJournalEntry {
                            entry_id: jid,
                            character_id: char_id,
                            entry_type: "character_transfer".into(),
                            amount: eve_db::IskCents(0),
                            balance_after: ch.balance,
                            reason: format!("Paid character transfer from account {} to {}", ch.user_id, dest_uid),
                        });
                        snap.wallet_journal_timestamps.insert(jid, now_unix);
                        snap.service_state_blobs
                            .insert(cooldown_key, now_unix.to_string());
                    }
                    Ok(())
                });
                xfer_res?;

                if is_execute {
                    let mut notice_bytes = Vec::new();
                    encode_varint_field(1, char_id as u64, &mut notice_bytes);
                    encode_varint_field(2, dest_uid, &mut notice_bytes);
                    pending_xfer
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((
                            NoticeScope::User(dest_uid as i32),
                            GoogleAny {
                                type_url: format!(
                                    "{TYPE_GOOGLEAPIS_PREFIX}eve.user.character.transfer.api.Completed"
                                ),
                                value: notice_bytes,
                            },
                        ));
                }

                let mut out = Vec::new();
                encode_varint_field(1, 1, &mut out); // available / valid / queued = true
                encode_varint_field(2, char_id as u64, &mut out);
                encode_varint_field(3, dest_uid, &mut out);
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rsp_name}"),
                    value: out,
                })
            }),
        );
    }

    router.register_handler(
        "eve.user.character.transfer.GetPendingByUserRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.user.character.transfer.GetPendingByUserResponse"
                ),
                value: Vec::new(),
            })
        }),
    );
}
