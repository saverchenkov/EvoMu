//! `quasar_entitlement_membership` — Quasar Entitlement, Omega & Membership Services (`Q-3`).

use super::quasar_auth_user::{read_identity_store, with_identity_store};
use crate::quasar_gateway::{require_admin_role, NoticeScope, PublicGatewayRouter};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use std::sync::Arc;

fn extract_nested_seq_id(val: &DynamicValue) -> Option<u64> {
    match val {
        DynamicValue::Uint64(v) => Some(*v),
        DynamicValue::Int64(v) if *v >= 0 => Some(*v as u64),
        DynamicValue::Uint32(v) => Some(*v as u64),
        DynamicValue::Int32(v) if *v >= 0 => Some(*v as u64),
        DynamicValue::Message(m) => m.get(1).and_then(extract_nested_seq_id),
        _ => None,
    }
}

fn extract_logo_entitlement_target(
    ctx: &eve_proto::quasar::AuthoritativeContext,
    dyn_msg: &DynamicQuasarMessage,
) -> Result<(u64, u64), (u32, String)> {
    let opt = if let Some(DynamicValue::Message(ent)) = dyn_msg.get(1) {
        let cid = ent
            .get(1)
            .and_then(extract_nested_seq_id)
            .or_else(|| ctx.effective_character_id().map(|c| c as u64));
        let stid = ent.get(2).and_then(extract_nested_seq_id);
        cid.zip(stid)
    } else {
        let cid = dyn_msg
            .get(1)
            .and_then(extract_nested_seq_id)
            .or_else(|| ctx.effective_character_id().map(|c| c as u64));
        let stid = dyn_msg.get(2).and_then(extract_nested_seq_id);
        cid.zip(stid)
    };
    opt.ok_or((
        crate::quasar_gateway::STATUS_BAD_REQUEST,
        "Target character ID and ship type ID are required".to_string(),
    ))
}

pub fn encode_logo_identifier(char_id: u64, ship_type_id: u64) -> Vec<u8> {
    let mut char_buf = Vec::new();
    encode_varint_field(1, char_id, &mut char_buf);
    let mut ship_buf = Vec::new();
    encode_varint_field(1, ship_type_id, &mut ship_buf);
    let mut ident = Vec::new();
    encode_bytes_field(1, &char_buf, &mut ident);
    encode_bytes_field(2, &ship_buf, &mut ident);
    ident
}

pub fn register_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    // 1. eve_public.entitlement.user.api.GetRequest -> eve_public.entitlement.user.api.GetResponse
    let db_ent_get = Arc::clone(&db);
    router.register_handler(
        "eve_public.entitlement.user.api.GetRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let ents = read_identity_store(&db_ent_get, |st| {
                st.user_entitlements
                    .get(&uid)
                    .cloned()
                    .unwrap_or_else(|| vec!["standard_pilot".to_string()])
            });

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
    // parity: quasar:eveProto/generated/eve_public/entitlement/character/character_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/entitlement/character/ship/alliancelogo_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/entitlement/character/ship/corplogo_pb2.pyj
    let db_char_ent = Arc::clone(&db);
    router.register_handler(
        "eve_public.entitlement.character.GetAllRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = dyn_msg
                .get(1)
                .and_then(extract_nested_seq_id)
                .or_else(|| ctx.effective_character_id().map(|c| c as u64))
                .unwrap_or(0);
            let (corp_ships, alliance_ships) = read_identity_store(&db_char_ent, |st| {
                let c = st.char_corp_logo_ships.get(&char_id).cloned().unwrap_or_default();
                let a = st.char_alliance_logo_ships.get(&char_id).cloned().unwrap_or_default();
                (c, a)
            });
            let mut out = Vec::new();
            for ship_type_id in corp_ships {
                let ident = encode_logo_identifier(char_id, ship_type_id);
                let mut ent_msg = Vec::new();
                encode_bytes_field(1, &ident, &mut ent_msg); // field 1: corporation_logo
                encode_bytes_field(1, &ent_msg, &mut out); // field 1: repeated entitlements
            }
            for ship_type_id in alliance_ships {
                let ident = encode_logo_identifier(char_id, ship_type_id);
                let mut ent_msg = Vec::new();
                encode_bytes_field(2, &ident, &mut ent_msg); // field 2: alliance_logo
                encode_bytes_field(1, &ent_msg, &mut out); // field 1: repeated entitlements
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
    // parity: quasar:eveProto/generated/eve_public/entitlement/character/ship/alliancelogo_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/entitlement/character/ship/corplogo_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/alliancelogo_pb2.pyj
    // parity: quasar:eveProto/generated/eve_public/entitlement/character/ship/admin/corplogo_pb2.pyj
    for (req_name, rsp_name, is_alliance, is_grant) in [
        (
            "eve_public.entitlement.character.ship.admin.alliancelogo.GrantRequest",
            "eve_public.entitlement.character.ship.admin.alliancelogo.GrantResponse",
            true,
            true,
        ),
        (
            "eve_public.entitlement.character.ship.admin.alliancelogo.RevokeRequest",
            "eve_public.entitlement.character.ship.admin.alliancelogo.RevokeResponse",
            true,
            false,
        ),
        (
            "eve_public.entitlement.character.ship.admin.corplogo.GrantRequest",
            "eve_public.entitlement.character.ship.admin.corplogo.GrantResponse",
            false,
            true,
        ),
        (
            "eve_public.entitlement.character.ship.admin.corplogo.RevokeRequest",
            "eve_public.entitlement.character.ship.admin.corplogo.RevokeResponse",
            false,
            false,
        ),
    ] {
        let db_logo = Arc::clone(&db);
        let pending_logo = Arc::clone(&router.pending_notices);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, _any, dyn_msg| {
                require_admin_role(ctx)?;
                let caller_cid = ctx.effective_character_id().unwrap_or(90_000_001) as u64;
                let (target_cid, ship_type_id) = extract_logo_entitlement_target(ctx, dyn_msg)?;
                with_identity_store(&db_logo, |guard| {
                    let map = if is_alliance {
                        &mut guard.char_alliance_logo_ships
                    } else {
                        &mut guard.char_corp_logo_ships
                    };
                    let ships = map.entry(target_cid).or_default();
                    if is_grant {
                        if !ships.contains(&ship_type_id) {
                            ships.push(ship_type_id);
                        }
                    } else {
                        ships.retain(|&s| s != ship_type_id);
                    }
                });
                let ident = encode_logo_identifier(target_cid, ship_type_id);
                let pkg = if is_alliance {
                    "eve_public.entitlement.character.ship.alliancelogo"
                } else {
                    "eve_public.entitlement.character.ship.corplogo"
                };
                let (notice_type, notice_bytes) = if is_grant {
                    let mut b = Vec::new();
                    encode_bytes_field(1, &ident, &mut b);
                    (format!("{TYPE_GOOGLEAPIS_PREFIX}{pkg}.GrantedNotice"), b)
                } else {
                    let mut revoker_buf = Vec::new();
                    encode_varint_field(1, caller_cid, &mut revoker_buf);
                    let mut b = Vec::new();
                    encode_bytes_field(1, &revoker_buf, &mut b);
                    encode_bytes_field(2, &ident, &mut b);
                    (format!("{TYPE_GOOGLEAPIS_PREFIX}{pkg}.RevokedNotice"), b)
                };
                pending_logo
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((
                        NoticeScope::Character(target_cid as u32),
                        GoogleAny {
                            type_url: notice_type,
                            value: notice_bytes,
                        },
                    ));
                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rsp_name}"),
                    value: Vec::new(),
                })
            }),
        );
    }

    // 4. eve.entitlement.user.api.GrantRequest -> enqueues GrantedNotice
    let db_ent_grant = Arc::clone(&db);
    let router_pending = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve.entitlement.user.api.GrantRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
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

            with_identity_store(&db_ent_grant, |guard| {
                guard
                    .user_entitlements
                    .entry(uid)
                    .or_default()
                    .push(ent_name.clone());
            });

            let mut id_bytes = Vec::new();
            encode_string_field(1, &ent_name, &mut id_bytes);
            let mut notice_bytes = Vec::new();
            encode_bytes_field(1, &id_bytes, &mut notice_bytes);
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
