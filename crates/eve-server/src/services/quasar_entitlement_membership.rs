//! `quasar_entitlement_membership` — Quasar Entitlement, Omega & Membership Services (`Q-3`).

use super::quasar_auth_user::{read_identity_store, with_identity_store};
use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::pb;
use eve_proto::quasar::{
    DynamicQuasarMessage, DynamicValue, GoogleAny, WireField, WireMessage, WireValue,
    TYPE_GOOGLEAPIS_PREFIX,
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
    pb::message("eve_public.entitlement.character.ship.corplogo.Identifier")
        .set("character.sequential", char_id as u32)
        .set("ship_type.sequential", ship_type_id)
        .must_encode()
}

pub fn register_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    // 1. eve_public.entitlement.user.api.GetRequest -> eve_public.entitlement.user.api.GetResponse
    let db_ent_get = Arc::clone(&db);
    router.register_handler(
        "eve_public.entitlement.user.api.GetRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let Some(uid) = ctx.effective_user_id().filter(|&u| u > 0).map(|u| u as u64) else {
                return Err((
                    crate::quasar_gateway::STATUS_FORBIDDEN,
                    "Authenticated user session required".to_string(),
                ));
            };
            let now_unix = ctx.now_unix();
            let active_ents: Vec<(String, Option<u64>)> = read_identity_store(&db_ent_get, |st| {
                let expiries = st.user_entitlement_expiries.get(&uid);
                let omega_exp = st.omega_subscriptions.get(&uid).copied().filter(|&e| e > now_unix);
                let mut out = Vec::new();
                if let Some(names) = st.user_entitlements.get(&uid) {
                    for name in names {
                        let exp = expiries
                            .and_then(|m| m.get(name).copied())
                            .or_else(|| if name.eq_ignore_ascii_case("omega") { omega_exp } else { None });
                        if exp.is_some_and(|e| e <= now_unix) {
                            continue;
                        }
                        out.push((name.clone(), exp));
                    }
                }
                if let Some(exp) = omega_exp {
                    if !out.iter().any(|(n, _)| n.eq_ignore_ascii_case("omega")) {
                        out.push(("omega".to_string(), Some(exp)));
                    }
                }
                out
            });

            // Descriptor declares `entitlement` (field 1) singular, while gateway callers expect all active entries repeated on field 1.
            let mut fields = Vec::with_capacity(active_ents.len());
            for (ent_name, exp_opt) in active_ents {
                let mut ent = pb::message("eve_public.entitlement.user.entitlement.Entitlement")
                    .set("name", ent_name);
                if let Some(exp_secs) = exp_opt {
                    ent = ent.set("expiry_date.seconds", exp_secs as i64);
                }
                fields.push(WireField {
                    field_number: 1,
                    value: WireValue::LengthDelimited(ent.must_encode()),
                });
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.entitlement.user.api.GetResponse"
                ),
                value: WireMessage { fields }.encode(),
            })
        }),
    );

    // 2. eve_public.entitlement.character.GetAllRequest & eve.entitlement.character.admin.GetAllRequest
    for (req_name, rsp_name, require_admin) in [
        (
            "eve_public.entitlement.character.GetAllRequest",
            "eve_public.entitlement.character.GetAllResponse",
            false,
        ),
        (
            "eve.entitlement.character.admin.GetAllRequest",
            "eve.entitlement.character.admin.GetAllResponse",
            true,
        ),
    ] {
        let db_char_ent = Arc::clone(&db);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, _any, dyn_msg| {
                if require_admin {
                    require_admin_role(ctx)?;
                }
                let char_id = dyn_msg
                    .get(1)
                    .and_then(extract_nested_seq_id)
                    .or_else(|| {
                        if require_admin {
                            None
                        } else {
                            ctx.effective_character_id().filter(|&c| c > 0).map(|c| c as u64)
                        }
                    })
                    .unwrap_or(0);
                if char_id == 0 {
                    let status = if require_admin {
                        crate::quasar_gateway::STATUS_BAD_REQUEST
                    } else {
                        crate::quasar_gateway::STATUS_FORBIDDEN
                    };
                    return Err((status, "Character identifier required".to_string()));
                }
                let (corp_ships, alliance_ships) = read_identity_store(&db_char_ent, |st| {
                    let c = st.char_corp_logo_ships.get(&char_id).cloned().unwrap_or_default();
                    let a = st.char_alliance_logo_ships.get(&char_id).cloned().unwrap_or_default();
                    (c, a)
                });
                let mut resp = pb::message(rsp_name);
                for ship_type_id in corp_ships {
                    resp = resp.add_with("entitlements", |e| {
                        e.set_with("corporation_logo", |l| {
                            l.set("character.sequential", char_id as u32)
                                .set("ship_type.sequential", ship_type_id)
                        })
                    });
                }
                for ship_type_id in alliance_ships {
                    resp = resp.add_with("entitlements", |e| {
                        e.set_with("alliance_logo", |l| {
                            l.set("character.sequential", char_id as u32)
                                .set("ship_type.sequential", ship_type_id)
                        })
                    });
                }
                Ok(resp.must_to_any())
            }),
        );
    }

    // 2b. eve.entitlement.character.api.IsEntitledRequest -> IsEntitledResponse
    let db_is_entitled = Arc::clone(&db);
    router.register_handler(
        "eve.entitlement.character.api.IsEntitledRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let (is_alliance, target_cid, ship_type_id) =
                if let Some(DynamicValue::Message(ident)) = dyn_msg.get(1) {
                    let cid = ident.get(1).and_then(extract_nested_seq_id).unwrap_or(0);
                    let stid = ident.get(2).and_then(extract_nested_seq_id).unwrap_or(0);
                    (false, cid, stid)
                } else if let Some(DynamicValue::Message(ident)) = dyn_msg.get(2) {
                    let cid = ident.get(1).and_then(extract_nested_seq_id).unwrap_or(0);
                    let stid = ident.get(2).and_then(extract_nested_seq_id).unwrap_or(0);
                    (true, cid, stid)
                } else {
                    (false, 0, 0)
                };
            if target_cid == 0 || ship_type_id == 0 {
                return Err((
                    crate::quasar_gateway::STATUS_BAD_REQUEST,
                    "IsEntitledRequest requires ship logo identifier with character and ship_type".to_string(),
                ));
            }
            let caller_cid = ctx.effective_character_id().filter(|&c| c > 0).map(|c| c as u64);
            if caller_cid != Some(target_cid) && require_admin_role(ctx).is_err() {
                return Err((
                    crate::quasar_gateway::STATUS_FORBIDDEN,
                    "Cannot query another character's entitlements without admin role".to_string(),
                ));
            }
            let entitled = read_identity_store(&db_is_entitled, |st| {
                let map = if is_alliance {
                    &st.char_alliance_logo_ships
                } else {
                    &st.char_corp_logo_ships
                };
                map.get(&target_cid)
                    .is_some_and(|ships| ships.contains(&ship_type_id))
            });
            if !entitled {
                return Err((
                    crate::quasar_gateway::STATUS_NOT_FOUND,
                    "Character does not hold the requested ship logo entitlement".to_string(),
                ));
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.entitlement.character.api.IsEntitledResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 3. {eve,eve_public}.entitlement.character.ship.admin.{alliancelogo,corplogo}.{Grant,Revoke}Request
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
        (
            "eve.entitlement.character.ship.admin.alliancelogo.GrantRequest",
            "eve.entitlement.character.ship.admin.alliancelogo.GrantResponse",
            true,
            true,
        ),
        (
            "eve.entitlement.character.ship.admin.alliancelogo.RevokeRequest",
            "eve.entitlement.character.ship.admin.alliancelogo.RevokeResponse",
            true,
            false,
        ),
        (
            "eve.entitlement.character.ship.admin.corplogo.GrantRequest",
            "eve.entitlement.character.ship.admin.corplogo.GrantResponse",
            false,
            true,
        ),
        (
            "eve.entitlement.character.ship.admin.corplogo.RevokeRequest",
            "eve.entitlement.character.ship.admin.corplogo.RevokeResponse",
            false,
            false,
        ),
    ] {
        let db_logo = Arc::clone(&db);
        let pending_logo = Arc::clone(&router.pending_notices);
        let sink_logo = Arc::clone(&router.macho_notify_sink);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, _any, dyn_msg| {
                require_admin_role(ctx)?;
                let Some(caller_cid) = ctx.effective_character_id().filter(|&v| v > 0).map(u64::from) else {
                    return Err((STATUS_UNAUTHORIZED, "Active character session required".into()));
                };
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
                if !is_grant {
                    super::quasar_cosmetics_live::prune_revoked_logo_entitlement(
                        &db_logo,
                        &sink_logo,
                        target_cid,
                        ship_type_id as u32,
                        is_alliance,
                    );
                }
                let pkg = if is_alliance {
                    "eve_public.entitlement.character.ship.alliancelogo"
                } else {
                    "eve_public.entitlement.character.ship.corplogo"
                };
                let notice = if is_grant {
                    pb::message(&format!("{pkg}.GrantedNotice"))
                        .set("entitlement.character.sequential", target_cid as u32)
                        .set("entitlement.ship_type.sequential", ship_type_id)
                        .must_to_any()
                } else {
                    pb::message(&format!("{pkg}.RevokedNotice"))
                        .set("revoker.sequential", caller_cid as u32)
                        .set("entitlement.character.sequential", target_cid as u32)
                        .set("entitlement.ship_type.sequential", ship_type_id)
                        .must_to_any()
                };
                pending_logo
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((NoticeScope::Character(target_cid as u32), notice));
                Ok(pb::message(rsp_name).must_to_any())
            }),
        );
    }

    // 4. eve.entitlement.user.api.GrantRequest -> enqueues GrantedNotice (+ ActivatedNotice if omega)
    let db_ent_grant = Arc::clone(&db);
    let router_pending = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve.entitlement.user.api.GrantRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let now_unix = ctx.now_unix();
            let caller_uid = ctx.effective_user_id().unwrap_or(1) as u64;
            let (uid, ent_name, duration_opt, expiry_opt) =
                if let Some(DynamicValue::Message(ent_msg)) = dyn_msg.get(1) {
                    let target_uid = ent_msg
                        .get(2)
                        .and_then(extract_nested_seq_id)
                        .filter(|&id| id > 0)
                        .unwrap_or(caller_uid);
                    let name = if let Some(DynamicValue::String(s)) = ent_msg.get(3) {
                        s.trim().to_string()
                    } else if let Some(DynamicValue::String(s)) = ent_msg.get(1) {
                        s.trim().to_string()
                    } else {
                        String::new()
                    };
                    let dur = ent_msg.get(4).and_then(extract_nested_seq_id);
                    let exp = ent_msg.get(5).and_then(extract_nested_seq_id);
                    (target_uid, name, dur, exp)
                } else if let Some(DynamicValue::Message(ent_msg)) = dyn_msg.get(2) {
                    let target_uid = dyn_msg
                        .get(1)
                        .and_then(extract_nested_seq_id)
                        .filter(|&id| id > 0)
                        .unwrap_or(caller_uid);
                    let name = if let Some(DynamicValue::String(s)) = ent_msg.get(1) {
                        s.trim().to_string()
                    } else {
                        String::new()
                    };
                    (target_uid, name, None, None)
                } else {
                    (caller_uid, String::new(), None, None)
                };
            if ent_name.is_empty() {
                return Err((
                    crate::quasar_gateway::STATUS_BAD_REQUEST,
                    "Entitlement name is required".to_string(),
                ));
            }

            let effective_exp = expiry_opt
                .filter(|&e| e > now_unix)
                .or_else(|| duration_opt.map(|d| now_unix.saturating_add(d)));
            if ent_name.eq_ignore_ascii_case("omega") {
                let dur_secs = duration_opt
                    .or_else(|| expiry_opt.map(|e| e.saturating_sub(now_unix)))
                    .unwrap_or(30 * 86_400)
                    .max(1);
                let (_next_exp, activated_notice) = db_ent_grant.with_mut_snapshot(|snap| {
                    super::quasar_auth_user::activate_user_omega_license_in_snapshot(
                        snap, uid, dur_secs, now_unix,
                    )
                });
                router_pending
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((NoticeScope::User(uid as i32), activated_notice));
            } else {
                with_identity_store(&db_ent_grant, |guard| {
                    let list = guard.user_entitlements.entry(uid).or_default();
                    if !list.contains(&ent_name) {
                        list.push(ent_name.clone());
                    }
                    if let Some(exp) = effective_exp {
                        guard
                            .user_entitlement_expiries
                            .entry(uid)
                            .or_default()
                            .insert(ent_name.clone(), exp);
                    }
                });
            }

            let granted_notice = pb::message("eve_public.assetholding.entitlement.api.GrantedNotice")
                .set("entitlement.group_by.name", &ent_name)
                .must_to_any();
            router_pending
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .push((NoticeScope::User(uid as i32), granted_notice));

            Ok(pb::message("eve.entitlement.user.api.GrantResponse").must_to_any())
        }),
    );

    // 4b. eve.entitlement.user.subscription.api.GrantRequest -> GrantResponse + ActivatedNotice + GrantedNotice
    let db_sub_grant = Arc::clone(&db);
    let pending_sub = Arc::clone(&router.pending_notices);
    router.register_handler(
        "eve.entitlement.user.subscription.api.GrantRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let now_unix = ctx.now_unix();
            let Some(DynamicValue::Message(sub_msg)) = dyn_msg.get(1) else {
                return Err((
                    crate::quasar_gateway::STATUS_BAD_REQUEST,
                    "subscription_fulfillment is required".to_string(),
                ));
            };
            let uid = sub_msg
                .get(1)
                .and_then(extract_nested_seq_id)
                .or_else(|| ctx.effective_user_id().filter(|&u| u > 0).map(|u| u as u64))
                .unwrap_or(0);
            if uid == 0 {
                return Err((
                    crate::quasar_gateway::STATUS_BAD_REQUEST,
                    "Target user identifier is required".to_string(),
                ));
            }
            let days = sub_msg.get(3).and_then(extract_nested_seq_id).unwrap_or(30).max(1);
            let sub_name = match sub_msg.get(6) {
                Some(DynamicValue::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                _ => "omega".to_string(),
            };
            let (_next_exp, activated_notice) = db_sub_grant.with_mut_snapshot(|snap| {
                super::quasar_auth_user::activate_user_omega_license_in_snapshot(
                    snap,
                    uid,
                    days.saturating_mul(86_400),
                    now_unix,
                )
            });
            let granted_notice = pb::message("eve_public.assetholding.entitlement.api.GrantedNotice")
                .set("entitlement.group_by.name", &sub_name)
                .must_to_any();
            let mut q = pending_sub.write().unwrap_or_else(|e| e.into_inner());
            q.push((NoticeScope::User(uid as i32), activated_notice));
            q.push((NoticeScope::User(uid as i32), granted_notice));
            Ok(pb::message("eve.entitlement.user.subscription.api.GrantResponse").must_to_any())
        }),
    );

    // 5. eve.membership.GetCharacterAllRequest -> eve.membership.GetCharacterAllResponse
    let db_mem = Arc::clone(&db);
    router.register_handler(
        "eve.membership.GetCharacterAllRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let Some(cid) = ctx.effective_character_id().filter(|&v| v > 0).map(i64::from) else {
                return Err((STATUS_UNAUTHORIZED, "Active character session required".into()));
            };
            let (corp_id, alliance_id) = db_mem.with_snapshot(|snap| {
                if let Some(ch) = snap.characters.get(&cid) {
                    (ch.corporation_id, ch.alliance_id)
                } else {
                    (1000170, None)
                }
            });

            Ok(pb::message("eve.membership.GetCharacterAllResponse")
                .set_with("membership", |m| {
                    m.set("corporation_id.sequential", corp_id as u32)
                        .set_with("alliance_membership", |am| {
                            if let Some(aid) = alliance_id {
                                am.set("alliance_id.sequential", aid as i32)
                            } else {
                                am.set("unaffiliated", true)
                            }
                        })
                        .set("faction_membership.unaffiliated", true)
                })
                .must_to_any())
        }),
    );

    // 5b. eve.membership.corporation.GetCharacterRequest -> eve.membership.corporation.GetCharacterResponse
    let db_corp_mem = Arc::clone(&db);
    router.register_handler(
        "eve.membership.corporation.GetCharacterRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Message(sub) => sub.get(1).and_then(|u| match u {
                        DynamicValue::Uint64(n) => Some(*n as i64),
                        DynamicValue::Int64(n) => Some(*n),
                        DynamicValue::Uint32(n) => Some(*n as i64),
                        _ => None,
                    }),
                    _ => None,
                })
                .filter(|&v| v > 0)
                .or_else(|| ctx.effective_character_id().filter(|&v| v > 0).map(|c| c as i64))
            else {
                return Err((
                    STATUS_UNAUTHORIZED,
                    "Character ID or active character session required".into(),
                ));
            };

            let (corp_id, title) = db_corp_mem.with_snapshot(|snap| {
                if let Some(ch) = snap.characters.get(&cid) {
                    let corp_title = if snap.corporations.get(&ch.corporation_id).map(|c| c.ceo_id) == Some(cid) {
                        "Chief Executive Officer".to_string()
                    } else if (ctx.corp_role_mask & eve_db::corp_roles::ROLE_DIRECTOR) != 0
                        || snap
                            .corporations
                            .get(&ch.corporation_id)
                            .and_then(|c| c.member_roles.get(&cid))
                            .is_some_and(|&r| (r & eve_db::corp_roles::ROLE_DIRECTOR) != 0)
                    {
                        "Director".to_string()
                    } else {
                        "Member".to_string()
                    };
                    (ch.corporation_id, corp_title)
                } else {
                    (1000170, "Member".to_string())
                }
            });

            Ok(pb::message("eve.membership.corporation.GetCharacterResponse")
                .set("corporation_id.sequential", corp_id as u32)
                .set("attributes.title", title)
                .must_to_any())
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
                        if st.base_price <= 0.0 {
                            return Err((
                                crate::quasar_gateway::STATUS_PRECONDITION_FAILED,
                                format!("Skill {sk_id} is drop-only and not available for direct purchase"),
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
            // Synthetic response (`eve.character.skill.PurchaseSkillsResponse` has no proto descriptor).
            let mut fields: Vec<WireField> = bought
                .into_iter()
                .map(|sk| WireField {
                    field_number: 1,
                    value: WireValue::Varint(u64::from(sk)),
                })
                .collect();
            fields.push(WireField {
                field_number: 2,
                value: WireValue::Varint(total_cost as u64),
            });
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.character.skill.PurchaseSkillsResponse"
                ),
                value: WireMessage { fields }.encode(),
            })
        }),
    );
}

#[cfg(test)]
#[path = "quasar_entitlement_membership_tests.rs"]
mod quasar_entitlement_membership_tests;

#[cfg(test)]
#[path = "quasar_entitlement_membership_edges_tests.rs"]
mod quasar_entitlement_membership_edges_tests;
