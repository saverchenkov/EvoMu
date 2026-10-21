//! `quasar_entitlement_membership` — Quasar Entitlement, Omega & Membership Services (`Q-3`).

use super::quasar_auth_user::{build_generic_response, get_identity_store};
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field,
    DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use std::sync::Arc;

pub const GROUP_2_REQUEST_PAIRS: &[(&str, &str)] = &[
    ("eve.entitlement.character.admin.GetAllRequest", "eve.entitlement.character.admin.GetAllResponse"),
    ("eve.entitlement.character.api.IsEntitledRequest", "eve.entitlement.character.api.IsEntitledResponse"),
    ("eve.entitlement.character.ship.admin.alliancelogo.GrantRequest", "eve.entitlement.character.ship.admin.alliancelogo.GrantResponse"),
    ("eve.entitlement.character.ship.admin.alliancelogo.RevokeRequest", "eve.entitlement.character.ship.admin.alliancelogo.RevokeResponse"),
    ("eve.entitlement.character.ship.admin.corplogo.GrantRequest", "eve.entitlement.character.ship.admin.corplogo.GrantResponse"),
    ("eve.entitlement.character.ship.admin.corplogo.RevokeRequest", "eve.entitlement.character.ship.admin.corplogo.RevokeResponse"),
    ("eve.entitlement.user.api.GrantRequest", "eve.entitlement.user.api.GrantResponse"),
    ("eve.entitlement.user.subscription.api.GrantRequest", "eve.entitlement.user.subscription.api.GrantResponse"),
    ("eve.membership.GetCharacterAllRequest", "eve.membership.GetCharacterAllResponse"),
    ("eve.membership.GetCharactersAllRequest", "eve.membership.GetCharactersAllResponse"),
    ("eve.membership.GetCorporationAllRequest", "eve.membership.GetCorporationAllResponse"),
    ("eve.membership.alliance.GetByCharacterRequest", "eve.membership.alliance.GetByCharacterResponse"),
    ("eve.membership.alliance.GetByCorporationRequest", "eve.membership.alliance.GetByCorporationResponse"),
    ("eve.membership.alliance.GetCharacterRequest", "eve.membership.alliance.GetCharacterResponse"),
    ("eve.membership.alliance.GetMultipleCharactersRequest", "eve.membership.alliance.GetMultipleCharactersResponse"),
    ("eve.membership.corporation.CheckIsMemberOfNPCCorporationRequest", "eve.membership.corporation.CheckIsMemberOfNPCCorporationResponse"),
    ("eve.membership.corporation.GetCharacterRequest", "eve.membership.corporation.GetCharacterResponse"),
    ("eve.membership.corporation.GetMultipleCharactersRequest", "eve.membership.corporation.GetMultipleCharactersResponse"),
    ("eve.membership.faction.GetByAllianceRequest", "eve.membership.faction.GetByAllianceResponse"),
    ("eve.membership.faction.GetByCorporationRequest", "eve.membership.faction.GetByCorporationResponse"),
    ("eve.membership.faction.GetCharacterRequest", "eve.membership.faction.GetCharacterResponse"),
    ("eve.membership.faction.GetMultipleCharactersRequest", "eve.membership.faction.GetMultipleCharactersResponse"),
    ("eve_public.entitlement.character.GetAllRequest", "eve_public.entitlement.character.GetAllResponse"),
    ("eve_public.entitlement.character.ship.admin.alliancelogo.GrantRequest", "eve_public.entitlement.character.ship.admin.alliancelogo.GrantResponse"),
    ("eve_public.entitlement.character.ship.admin.alliancelogo.RevokeRequest", "eve_public.entitlement.character.ship.admin.alliancelogo.RevokeResponse"),
    ("eve_public.entitlement.character.ship.admin.corplogo.GrantRequest", "eve_public.entitlement.character.ship.admin.corplogo.GrantResponse"),
    ("eve_public.entitlement.character.ship.admin.corplogo.RevokeRequest", "eve_public.entitlement.character.ship.admin.corplogo.RevokeResponse"),
    ("eve_public.entitlement.user.api.GetRequest", "eve_public.entitlement.user.api.GetResponse"),
];

pub fn register_handlers(router: &PublicGatewayRouter, _db: Arc<WorldRepository>) {
    let descriptors = Arc::clone(&router.descriptors);
    let store = get_identity_store();

    // 1. Specific custom handlers with live entitlement/membership semantics

    // eve_public.entitlement.user.api.GetRequest -> eve_public.entitlement.user.api.GetResponse
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
                .unwrap_or_default();

            let mut out = Vec::new();
            for ent_name in ents {
                let mut ent_bytes = Vec::new();
                encode_string_field(1, &ent_name, &mut ent_bytes);
                encode_bytes_field(1, &ent_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.entitlement.user.api.GetResponse"),
                value: out,
            })
        }),
    );

    // eve.entitlement.user.api.GetListRequest -> eve.entitlement.user.api.GetListResponse
    let store_ent_int = Arc::clone(&store);
    router.register_handler(
        "eve.entitlement.user.api.GetListRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let ents = store_ent_int
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .user_entitlements
                .get(&uid)
                .cloned()
                .unwrap_or_default();

            let mut out = Vec::new();
            for ent_name in ents {
                let mut ent_bytes = Vec::new();
                encode_string_field(1, &ent_name, &mut ent_bytes);
                encode_bytes_field(1, &ent_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.entitlement.user.api.GetListResponse"),
                value: out,
            })
        }),
    );

    // eve.entitlement.user.api.GrantRequest -> enqueues GrantedNotice
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
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.assetholding.entitlement.api.GrantedNotice"),
                        value: notice_bytes,
                    },
                ));

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.entitlement.user.api.GrantResponse"),
                value: Vec::new(),
            })
        }),
    );

    // eve.membership.GetCharacterAllRequest -> eve.membership.GetCharacterAllResponse
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
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.membership.GetCharacterAllResponse"),
                value: out,
            })
        }),
    );

    // 2. Register all other Group 2 request pairs using descriptors
    for &(req_type, rsp_type) in GROUP_2_REQUEST_PAIRS {
        let req_norm = QuasarDescriptorRegistry::normalize_type_name(req_type).to_string();
        let rsp_norm = QuasarDescriptorRegistry::normalize_type_name(rsp_type).to_string();
        let desc_arc = Arc::clone(&descriptors);
        let rsp_name_clone = rsp_norm.clone();

        if matches!(
            req_norm.as_str(),
            "eve_public.entitlement.user.api.GetRequest"
                | "eve.entitlement.user.api.GetListRequest"
                | "eve.entitlement.user.api.GrantRequest"
                | "eve.membership.GetCharacterAllRequest"
        ) {
            continue;
        }

        router.register_handler(
            &req_norm,
            Arc::new(move |_ctx, _any, _dyn_msg| {
                build_generic_response(&desc_arc, &rsp_name_clone, 0x2001)
            }),
        );
    }
}
