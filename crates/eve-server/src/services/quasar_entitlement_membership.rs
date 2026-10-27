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
        DynamicValue::Uint64(v) if *v > 0 => Some(*v),
        DynamicValue::Int64(v) if *v > 0 => Some(*v as u64),
        DynamicValue::Sint64(v) if *v > 0 => Some(*v as u64),
        DynamicValue::Uint32(v) if *v > 0 => Some(*v as u64),
        DynamicValue::Int32(v) if *v > 0 => Some(*v as u64),
        DynamicValue::Sint32(v) if *v > 0 => Some(*v as u64),
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
            let caller_uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let (uid, ent_name) = if let Some(DynamicValue::Message(ent_msg)) = dyn_msg.get(1) {
                let target_uid = ent_msg
                    .get(2)
                    .and_then(extract_nested_seq_id)
                    .filter(|&id| id > 0)
                    .unwrap_or(caller_uid);
                let name = if let Some(DynamicValue::String(s)) = ent_msg.get(3) {
                    s.clone()
                } else if let Some(DynamicValue::String(s)) = ent_msg.get(1) {
                    s.clone()
                } else {
                    "special_access".to_string()
                };
                (target_uid, name)
            } else if let Some(DynamicValue::Message(ent_msg)) = dyn_msg.get(2) {
                let target_uid = dyn_msg
                    .get(1)
                    .and_then(extract_nested_seq_id)
                    .filter(|&id| id > 0)
                    .unwrap_or(caller_uid);
                let name = if let Some(DynamicValue::String(s)) = ent_msg.get(1) {
                    s.clone()
                } else {
                    "special_access".to_string()
                };
                (target_uid, name)
            } else {
                (caller_uid, "special_access".to_string())
            };

            with_identity_store(&db_ent_grant, |guard| {
                let list = guard.user_entitlements.entry(uid).or_default();
                if !list.contains(&ent_name) {
                    list.push(ent_name.clone());
                }
            });
            if ent_name.eq_ignore_ascii_case("omega") {
                db_ent_grant.with_mut_snapshot(|snap| {
                    for ch in snap.characters.values_mut() {
                        if ch.user_id == uid {
                            ch.clone_grade = 2;
                        }
                    }
                });
            }

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
    let db_mem = Arc::clone(&db);
    router.register_handler(
        "eve.membership.GetCharacterAllRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let has_omega = read_identity_store(&db_mem, |st| {
                st.user_entitlements
                    .get(&uid)
                    .is_some_and(|ents| ents.iter().any(|e| e.eq_ignore_ascii_case("omega")))
            });
            let mut mem_attr = Vec::new();
            encode_varint_field(1, u64::from(has_omega), &mut mem_attr); // active = 1 if omega else 0
            encode_string_field(2, if has_omega { "omega" } else { "alpha" }, &mut mem_attr);
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

    // 6. eve.character.skill.PurchaseSkillsRequest -> eve.character.skill.PurchaseSkillsResponse (`feat:skills.direct-purchase-inject`)
    let db_skill_buy = Arc::clone(&db);
    let sde_skill_buy = Arc::clone(&router.sde);
    router.register_handler(
        "eve.character.skill.PurchaseSkillsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let char_id = ctx
                .effective_character_id()
                .ok_or_else(|| {
                    (
                        crate::quasar_gateway::STATUS_UNAUTHORIZED,
                        "Character authentication required".to_string(),
                    )
                })? as i64;
            let mut skill_ids = Vec::new();
            for field_num in [1u32, 2u32] {
                if let Some(val) = dyn_msg.get(field_num) {
                    match val {
                        DynamicValue::Repeated(list) => {
                            for item in list {
                                if let Some(id) = extract_nested_seq_id(item) {
                                    if id > 0 && !skill_ids.contains(&(id as u32)) {
                                        skill_ids.push(id as u32);
                                    }
                                }
                            }
                        }
                        single => {
                            if let Some(id) = extract_nested_seq_id(single) {
                                if id > 0 && !skill_ids.contains(&(id as u32)) {
                                    skill_ids.push(id as u32);
                                }
                            }
                        }
                    }
                }
            }
            if skill_ids.is_empty() {
                return Err((
                    crate::quasar_gateway::STATUS_BAD_REQUEST,
                    "PurchaseSkillsRequest requires at least one skill type ID".into(),
                ));
            }
            let now_unix = ctx.now_unix();
            let now_ft = (now_unix as i64 + 11_644_473_600) * 10_000_000;
            let res: Result<(Vec<u32>, i64), (u32, String)> =
                db_skill_buy.try_with_mut_snapshot(|snap| {
                    let mut total_cost: i64 = 0;
                    let mut costs = Vec::new();
                    for &sk_id in &skill_ids {
                        let Some(st) = sde_skill_buy.types.get(&sk_id) else {
                            return Err((
                                crate::quasar_gateway::STATUS_NOT_FOUND,
                                format!("Skill type {sk_id} not found"),
                            ));
                        };
                        if st.category_id != 16 || !st.published {
                            return Err((
                                crate::quasar_gateway::STATUS_BAD_REQUEST,
                                format!("Type {sk_id} is not a published skill"),
                            ));
                        }
                        if snap
                            .skills
                            .get(&char_id)
                            .is_some_and(|m| m.contains_key(&sk_id))
                        {
                            return Err((
                                crate::quasar_gateway::STATUS_CONFLICT,
                                format!("Skill {sk_id} is already injected"),
                            ));
                        }
                        let npc_price_cents = snap
                            .market_orders
                            .values()
                            .filter(|o| !o.is_bid && o.is_npc && o.vol_remaining > 0 && o.type_id == sk_id)
                            .map(|o| o.price.0)
                            .min()
                            .ok_or_else(|| {
                                (
                                    crate::quasar_gateway::STATUS_PRECONDITION_FAILED,
                                    format!("Skill {sk_id} is not available from NPC market orders"),
                                )
                            })?;
                        let markup_cents = ((npc_price_cents as f64) * 1.30).round() as i64;
                        total_cost = total_cost.saturating_add(markup_cents);
                        costs.push((sk_id, markup_cents));
                    }
                    let ch = snap.characters.get_mut(&char_id).ok_or_else(|| {
                        (
                            crate::quasar_gateway::STATUS_NOT_FOUND,
                            format!("Character {char_id} not found"),
                        )
                    })?;
                    if ch.balance.0 < total_cost {
                        return Err((
                            crate::quasar_gateway::STATUS_PRECONDITION_FAILED,
                            "Insufficient ISK for direct skill purchase".into(),
                        ));
                    }
                    ch.balance = eve_db::IskCents(ch.balance.0 - total_cost);
                    let new_bal = ch.balance.0;
                    let skills_map = snap.skills.entry(char_id).or_default();
                    let hist = snap.skill_history.entry(char_id).or_default();
                    for (sk_id, cost_c) in costs {
                        skills_map.insert(
                            sk_id,
                            eve_db::TrainedSkillRecord {
                                type_id: sk_id,
                                level: 0,
                                skill_points: 0.0,
                            },
                        );
                        hist.push(eve_db::SkillHistoryEntry {
                            log_date_ft: now_ft,
                            event_type_id: 36,
                            skill_type_id: sk_id,
                            skill_level: 0,
                            absolute_points: 0.0,
                        });
                        snap.next_entity_id += 1;
                        let jid = snap.next_entity_id;
                        snap.wallet_journal.push(eve_db::WalletJournalEntry {
                            entry_id: jid,
                            character_id: char_id,
                            entry_type: "skill_purchase".into(),
                            amount: eve_db::IskCents(-cost_c),
                            balance_after: eve_db::IskCents(new_bal),
                            reason: format!("Direct skill purchase and injection (type {sk_id})"),
                        });
                        snap.wallet_journal_timestamps.insert(jid, now_unix);
                    }
                    Ok((skill_ids, total_cost))
                });
            let (bought, total_cost) = res?;
            let mut out = Vec::new();
            for sk in bought {
                encode_varint_field(1, sk as u64, &mut out);
            }
            encode_varint_field(2, total_cost as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.character.skill.PurchaseSkillsResponse"
                ),
                value: out,
            })
        }),
    );
}
