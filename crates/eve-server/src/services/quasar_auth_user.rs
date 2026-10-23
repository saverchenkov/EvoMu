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
}

const QUASAR_IDENTITY_NOTE_ID: i64 = -8_500_000_000_000;
const QUASAR_IDENTITY_NOTE_OWNER_ID: i64 = -85;

fn default_identity_store() -> QuasarIdentityStore {
    let mut store = QuasarIdentityStore::default();
    store.plex_balances.insert(1, 1000);
    store.plex_balances.insert(1001, 500);
    store.omega_subscriptions.insert(1, 1_900_000_000);
    store.user_emails.insert(1, "pilot1@evomu.local".to_string());
    store
        .user_tags
        .insert(1, vec!["alpha_clone".to_string(), "verified".to_string()]);
    store
        .user_entitlements
        .insert(1, vec!["standard_pilot".to_string()]);
    store
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
    router.register_handler(
        "eve_public.auth.AuthenticateUsernameRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let mut username = "anonymous_pilot".to_string();
            if let Some(DynamicValue::Message(creds)) = dyn_msg.get(1) {
                if let Some(DynamicValue::String(u)) = creds.get(1) {
                    if !u.is_empty() {
                        username = u.clone();
                    }
                }
            }
            let _ = db_auth.find_user_id_by_username(&username);
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
        }),
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
    router.register_handler(
        "eve_public.auth.steam.AuthenticateSteamRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1);
            let mut out = Vec::new();
            encode_string_field(1, &format!("evomu-steam-token-{uid}"), &mut out);
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
    let router_pending = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve_public.user.license.api.GetRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let default_exp =
                ctx.received.as_ref().map(|t| t.seconds as u64).unwrap_or(0) + 30 * 86_400;
            let expiry = with_identity_store(&db_lic_get, |guard| {
                *guard.omega_subscriptions.entry(uid).or_insert(default_exp)
            });

            let mut notice_bytes = Vec::new();
            encode_string_field(1, "omega", &mut notice_bytes);
            router_pending
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((
                    NoticeScope::User(uid as i32),
                    GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.user.license.api.ActivatedNotice"
                        ),
                        value: notice_bytes,
                    },
                ));

            let mut lic_attr = Vec::new();
            encode_varint_field(1, 1, &mut lic_attr); // active = true
            let mut exp_ts = Vec::new();
            encode_varint_field(1, expiry, &mut exp_ts);
            encode_bytes_field(2, &exp_ts, &mut lic_attr);
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
}
