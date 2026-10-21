//! `quasar_entitlement_membership` — Quasar Entitlement, Omega & Membership Services (`Q-3`).

use super::quasar_auth_user::get_identity_store;
use crate::quasar_gateway::{require_admin_role, NoticeScope, PublicGatewayRouter};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicValue, GoogleAny,
    TYPE_GOOGLEAPIS_PREFIX,
};
use std::sync::Arc;

pub fn register_handlers(router: &PublicGatewayRouter, _db: Arc<WorldRepository>) {
    let store = get_identity_store();

    // 1. eve_public.entitlement.user.api.GetRequest -> eve_public.entitlement.user.api.GetResponse
    let store_ent_get = Arc::clone(&store);
    router.register_handler(
        "eve_public.entitlement.user.api.GetRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let ents = store_ent_get
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .user_entitlements
                .get(&uid)
                .cloned()
                .unwrap_or_else(|| vec!["standard_pilot".to_string()]);

            let mut out = Vec::new();
            for ent_name in ents {
                let mut ent_bytes = Vec::new();
                encode_string_field(1, &ent_name, &mut ent_bytes);
                encode_bytes_field(1, &ent_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.entitlement.user.api.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 2. eve_public.entitlement.character.GetAllRequest -> eve_public.entitlement.character.GetAllResponse
    let store_char_ent = Arc::clone(&store);
    router.register_handler(
        "eve_public.entitlement.character.GetAllRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let ents = store_char_ent
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .user_entitlements
                .get(&uid)
                .cloned()
                .unwrap_or_else(|| vec!["standard_pilot".to_string()]);
            let mut out = Vec::new();
            for ent_name in ents {
                let mut ent_bytes = Vec::new();
                encode_string_field(1, &ent_name, &mut ent_bytes);
                encode_bytes_field(1, &ent_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.entitlement.character.GetAllResponse"
                ),
                value: out,
            })
        }),
    );

    // 3. eve_public.entitlement.character.ship.admin.{alliancelogo,corplogo}.{Grant,Revoke}Request
    for (req_name, rsp_name) in [
        (
            "eve_public.entitlement.character.ship.admin.alliancelogo.GrantRequest",
            "eve_public.entitlement.character.ship.admin.alliancelogo.GrantResponse",
        ),
        (
            "eve_public.entitlement.character.ship.admin.alliancelogo.RevokeRequest",
            "eve_public.entitlement.character.ship.admin.alliancelogo.RevokeResponse",
        ),
        (
            "eve_public.entitlement.character.ship.admin.corplogo.GrantRequest",
            "eve_public.entitlement.character.ship.admin.corplogo.GrantResponse",
        ),
        (
            "eve_public.entitlement.character.ship.admin.corplogo.RevokeRequest",
            "eve_public.entitlement.character.ship.admin.corplogo.RevokeResponse",
        ),
    ] {
        let store_logo = Arc::clone(&store);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, _any, _dyn_msg| {
                require_admin_role(ctx)?;
                let uid = ctx.effective_user_id().unwrap_or(1) as u64;
                let mut guard = store_logo.write().unwrap_or_else(|e| e.into_inner());
                let entry = guard.user_entitlements.entry(uid).or_default();
                if req_name.ends_with("GrantRequest") {
                    entry.push(req_name.to_string());
                } else {
                    entry.retain(|x| !x.contains("logo"));
                }
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rsp_name}"),
                    value: Vec::new(),
                })
            }),
        );
    }

    // 4. eve.entitlement.user.api.GrantRequest -> enqueues GrantedNotice
    let store_ent_grant = Arc::clone(&store);
    let router_pending = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve.entitlement.user.api.GrantRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let ent_name = if let Some(DynamicValue::Message(ent_msg)) = dyn_msg.get(1) {
                if let Some(DynamicValue::String(s)) = ent_msg.get(3) {
                    s.clone()
                } else if let Some(DynamicValue::String(s)) = ent_msg.get(1) {
                    s.clone()
                } else {
                    "special_access".to_string()
                }
            } else if let Some(DynamicValue::Message(ent_msg)) = dyn_msg.get(2) {
                if let Some(DynamicValue::String(s)) = ent_msg.get(1) {
                    s.clone()
                } else {
                    "special_access".to_string()
                }
            } else {
                "special_access".to_string()
            };

            store_ent_grant
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .user_entitlements
                .entry(uid)
                .or_default()
                .push(ent_name.clone());

            let mut notice_bytes = Vec::new();
            encode_string_field(1, &ent_name, &mut notice_bytes);
            router_pending
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((
                    NoticeScope::User(uid as i32),
                    GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.entitlement.api.GrantedNotice"
                        ),
                        value: notice_bytes,
                    },
                ));

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.entitlement.user.api.GrantResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 5. eve.membership.GetCharacterAllRequest -> eve.membership.GetCharacterAllResponse
    router.register_handler(
        "eve.membership.GetCharacterAllRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let mut mem_attr = Vec::new();
            encode_varint_field(1, 1, &mut mem_attr); // active = true
            encode_string_field(2, "omega", &mut mem_attr); // tier = omega
            let now = ctx.received.as_ref().map(|t| t.seconds as u64).unwrap_or(0);
            encode_varint_field(3, now.saturating_sub(86400 * 30), &mut mem_attr); // join date

            let mut out = Vec::new();
            encode_bytes_field(1, &mem_attr, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.membership.GetCharacterAllResponse"
                ),
                value: out,
            })
        }),
    );
}
