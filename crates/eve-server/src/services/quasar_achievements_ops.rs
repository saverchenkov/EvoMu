//! `quasar_achievements_ops` — Admin RPC handlers and notice publishers for AIR Achievements
//! (`feat:quasar.achievement`, `quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, PublicGatewayRouter, QuasarHandlerFn, STATUS_BAD_REQUEST,
    STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_PRECONDITION_FAILED, STATUS_SERVICE_UNAVAILABLE,
};
use super::quasar_achievements::{
    emit_achievement_notices, encode_uuid_ident, extract_u32_ident, extract_u64_val,
    AchievementManager, RewardClaimFilter,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, sha256_hex, DynamicValue,
    GoogleAny, GoogleEmpty, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use std::sync::Arc;

pub fn publish_achievement_milestone_reached_notice(
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    cid: u32,
    aid: u32,
    milestone_id: u32,
) {
    let mut ms_bytes = Vec::new();
    encode_uuid_ident(1, aid, &mut ms_bytes);
    encode_uuid_ident(2, milestone_id, &mut ms_bytes);
    gateway
        .pending_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((
            crate::quasar_gateway::NoticeScope::Character(cid),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.milestone.api.ReachedNotice"),
                value: ms_bytes,
            },
        ));
}

pub fn publish_achievement_version_check_notice(
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    major: u64,
    minor: u64,
    patch: u64,
) {
    let mut ver_data = Vec::new();
    let mut spec_data = Vec::new();
    encode_varint_field(1, major, &mut spec_data);
    encode_varint_field(2, minor, &mut spec_data);
    encode_varint_field(3, patch, &mut spec_data);
    encode_bytes_field(1, &spec_data, &mut ver_data);

    gateway
        .pending_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((
            crate::quasar_gateway::NoticeScope::Broadcast,
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.definition.version.api.CheckVersionNotice"),
                value: ver_data,
            },
        ));
}

pub fn publish_achievement_category_progress_notice(
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    cid: u32,
    category_id: u32,
    score: u32,
) {
    let mut cat_prog = Vec::new();
    encode_uuid_ident(1, category_id, &mut cat_prog);
    encode_varint_field(2, score as u64, &mut cat_prog);
    gateway
        .pending_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((
            crate::quasar_gateway::NoticeScope::Character(cid),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.category.api.ProgressedNotice"),
                value: cat_prog,
            },
        ));
}

pub fn publish_achievement_category_milestone_notice(
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    cid: u32,
    category_id: u32,
    milestone_id: u32,
) {
    let mut ms_data = Vec::new();
    encode_uuid_ident(1, category_id, &mut ms_data);
    encode_uuid_ident(2, milestone_id, &mut ms_data);
    gateway
        .pending_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((
            crate::quasar_gateway::NoticeScope::Character(cid),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.category.milestone.api.ReachedNotice"),
                value: ms_data,
            },
        ));
}

/// Registers all admin handlers for achievement progress, completion, reset, and version control.
pub fn register_achievement_admin_handlers(
    router: &PublicGatewayRouter,
    manager: Arc<AchievementManager>,
) {
    // 1. eve_public.achievement.api.admin.AddProgressRequest -> AddProgressResponse
    let mgr_prog = Arc::clone(&manager);
    let progress_handler: QuasarHandlerFn = Arc::new(move |ctx, _any, dyn_msg| {
        require_admin_role(ctx)?;
        let Some(cid) = ctx.effective_character_id() else {
            return Err((STATUS_FORBIDDEN, "Active character required".into()));
        };
        let aid = dyn_msg
            .get(1)
            .and_then(extract_u32_ident)
            .filter(|&id| id > 0)
            .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or invalid achievement ID".into()))?;
        let delta = dyn_msg
            .get(2)
            .and_then(extract_u64_val)
            .or_else(|| dyn_msg.get(4).and_then(extract_u64_val))
            .filter(|&d| d > 0)
            .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or invalid progress delta".into()))?;

        let Some((ach, just_completed, new_milestones)) =
            mgr_prog.add_progress(cid, aid, delta, ctx.now_unix())
        else {
            return Err((STATUS_NOT_FOUND, "Achievement not found".into()));
        };
        mgr_prog.save_to_db();

        let cat_id = ach.def.category_id;
        let cat_score = mgr_prog.category_score(cid, cat_id);
        emit_achievement_notices(
            cid,
            aid,
            cat_id,
            ach.progress,
            cat_score,
            just_completed,
            &new_milestones,
            ctx.now_unix(),
        );

        Ok(GoogleEmpty.to_any())
    });
    router.register_handler(
        "eve_public.achievement.api.admin.AddProgressRequest",
        Arc::clone(&progress_handler),
    );
    router.register_handler(
        "eve_public.achievement.api.admin.ProgressAchievementRequest",
        progress_handler,
    );

    // 2. eve_public.achievement.api.admin.CompleteRequest -> CompleteResponse
    let mgr_comp = Arc::clone(&manager);
    let complete_handler: QuasarHandlerFn = Arc::new(move |ctx, _any, dyn_msg| {
        require_admin_role(ctx)?;
        let Some(cid) = ctx.effective_character_id() else {
            return Err((STATUS_FORBIDDEN, "Active character required".into()));
        };
        let aid = dyn_msg
            .get(1)
            .and_then(extract_u32_ident)
            .filter(|&id| id > 0)
            .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or invalid achievement ID".into()))?;
        let ach = mgr_comp.mark_completed(cid, aid, ctx.now_unix())?;
        mgr_comp.save_to_db();

        let cat_id = ach.def.category_id;
        let cat_score = mgr_comp.category_score(cid, cat_id);
        let ms: Vec<u32> = (1..=ach.def.milestone_targets.len() as u32).collect();
        emit_achievement_notices(cid, aid, cat_id, ach.progress, cat_score, true, &ms, ctx.now_unix());

        Ok(GoogleEmpty.to_any())
    });
    router.register_handler(
        "eve_public.achievement.api.admin.CompleteRequest",
        Arc::clone(&complete_handler),
    );
    router.register_handler(
        "eve_public.achievement.api.admin.CompleteAchievementRequest",
        Arc::clone(&complete_handler),
    );
    router.register_handler(
        "eve_public.achievement.api.admin.UnlockAchievementRequest",
        complete_handler,
    );

    // 3. eve_public.achievement.api.admin.ResetRequest -> ResetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.api.admin.ResetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let aid = dyn_msg
                .get(1)
                .and_then(extract_u32_ident)
                .filter(|&id| id > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing or invalid achievement ID".into()))?;
            let _ = mgr.reset_achievement(cid, aid)?;
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 4. eve_public.achievement.api.admin.ResetAllRequest -> ResetAllResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.api.admin.ResetAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            require_admin_role(ctx)?;
            if let Some(cid) = ctx.effective_character_id() {
                mgr.reset_all(cid);
                mgr.save_to_db();
            }
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 5. eve_public.achievement.category.api.admin.ResetRequest -> ResetResponse
    let mgr_cat_reset = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.category.api.admin.ResetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let cat_id = dyn_msg
                .get(1)
                .and_then(extract_u32_ident)
                .filter(|&id| id > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing category identifier".into()))?;
            mgr_cat_reset.reset_category(cid, cat_id);

            let mut cat_prog = Vec::new();
            encode_uuid_ident(1, cat_id, &mut cat_prog);
            encode_varint_field(2, 0, &mut cat_prog);
            super::quasar_social_chat::ctx_queue_notice(
                crate::quasar_gateway::NoticeScope::Character(cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.category.api.ProgressedNotice"),
                    value: cat_prog,
                },
                ctx.now_unix(),
            );

            Ok(GoogleEmpty.to_any())
        }),
    );

    // 6. eve.achievement.definition.version.api.admin.SetDefaultVersionRequest
    router.register_handler(
        "eve.achievement.definition.version.api.admin.SetDefaultVersionRequest",
        Arc::new(move |ctx, _any, _dyn| {
            require_admin_role(ctx)?;
            let mut ver_data = Vec::new();
            let mut spec_data = Vec::new();
            encode_varint_field(1, 24, &mut spec_data);
            encode_varint_field(2, 1, &mut spec_data);
            encode_varint_field(3, 0, &mut spec_data);
            encode_bytes_field(1, &spec_data, &mut ver_data);

            super::quasar_social_chat::ctx_queue_notice(
                crate::quasar_gateway::NoticeScope::Broadcast,
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.definition.version.api.CheckVersionNotice"),
                    value: ver_data,
                },
                ctx.now_unix(),
            );
            Ok(GoogleEmpty.to_any())
        }),
    );
}

pub fn register_achievement_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    manager: Arc<AchievementManager>,
) {
    // 1. eve_public.achievement.api.GetRequest -> GetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let aid_opt = dyn_msg.get(1).and_then(extract_u32_ident);
            let aid = match aid_opt {
                Some(id) if id > 0 => {
                    if !mgr.definitions.iter().any(|d| d.id == id) {
                        return Err((STATUS_NOT_FOUND, "Achievement not found".into()));
                    }
                    id
                }
                Some(_) => return Err((STATUS_BAD_REQUEST, "Invalid achievement ID".into())),
                None => return Err((STATUS_BAD_REQUEST, "Missing achievement ID".into())),
            };

            let achievements = mgr.get_or_init_achievements(cid);
            let a = achievements
                .iter()
                .find(|x| x.def.id == aid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Achievement not found".into()))?;

            let mut out = Vec::new();
            if a.completed {
                let ts_proto = GoogleTimestamp {
                    seconds: a.completed_date.unwrap_or(1_700_000_000) as i64,
                    nanos: 0,
                };
                encode_bytes_field(1, &ts_proto.encode_proto(), &mut out);
            } else {
                encode_varint_field(2, 1, &mut out); // not_completed = true (mutually exclusive oneof with completed_date)
            }
            encode_varint_field(3, a.progress, &mut out); // progress_value

            let ts_proto = GoogleTimestamp {
                seconds: a.completed_date.unwrap_or(1_700_000_000) as i64,
                nanos: 0,
            };
            let ts_bytes = ts_proto.encode_proto();
            let pts_per_ms = a.def.score / (a.def.milestone_targets.len().max(1) as u32);
            for m_id in &a.reached_milestones {
                let mut rm = Vec::new();
                encode_uuid_ident(1, *m_id, &mut rm);
                encode_bytes_field(2, &ts_bytes, &mut rm);
                encode_varint_field(3, pts_per_ms as u64, &mut rm);
                encode_bytes_field(5, &rm, &mut out);
            }

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.api.GetResponse"),
                value: out,
            })
        }),
    );

    // 2. eve_public.achievement.category.api.GetRequest -> GetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.category.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let cat_id = dyn_msg
                .get(1)
                .and_then(extract_u32_ident)
                .filter(|&id| id > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing category identifier".into()))?;
            let score = mgr.category_score(cid, cat_id);
            let mut out = Vec::new();
            encode_varint_field(1, score as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.category.api.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 3. eve_public.achievement.showcase.api.GetCharacterScoreRequest -> GetCharacterScoreResponse
    let mgr_sc = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_cid = match dyn_msg.get(1) {
                None => cid,
                Some(DynamicValue::Message(m)) if m.get(1).is_none() => cid,
                Some(v) => extract_u32_ident(v)
                    .filter(|&id| id > 0)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Invalid character identifier".into()))?,
            };

            let score = mgr_sc.total_character_score(target_cid);
            let mut out = Vec::new();
            encode_varint_field(1, score as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.showcase.api.GetCharacterScoreResponse"
                ),
                value: out,
            })
        }),
    );

    // 4. eve_public.achievement.reward.api.GetUnclaimedRequest -> GetUnclaimedResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.reward.api.GetUnclaimedRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let achievements = mgr.get_or_init_achievements(cid);
            let mut out = Vec::new();
            for a in &achievements {
                if a.unclaimed_reward_count > 0 {
                    let ms_ids: Vec<u32> = if !a.reached_milestones.is_empty() {
                        a.reached_milestones.iter().rev().take(a.unclaimed_reward_count as usize).copied().rev().collect()
                    } else {
                        (1..=a.unclaimed_reward_count).collect()
                    };
                    for m_id in ms_ids {
                        let mut src = Vec::new();
                        encode_uuid_ident(1, a.def.id, &mut src);
                        encode_uuid_ident(2, m_id, &mut src);

                        let rew_uuid = super::quasar_achievements::achievement_reward_uuid(a.def.id, m_id);
                        let mut rew_ident = Vec::new();
                        encode_bytes_field(1, &rew_uuid, &mut rew_ident);

                        let mut rew = Vec::new();
                        encode_bytes_field(1, &src, &mut rew);
                        encode_bytes_field(3, &rew_ident, &mut rew);
                        encode_bytes_field(1, &rew, &mut out);
                    }
                }
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.reward.api.GetUnclaimedResponse"
                ),
                value: out,
            })
        }),
    );

    // 5. eve_public.achievement.reward.api.ClaimRequest -> ClaimResponse
    let mgr = Arc::clone(&manager);
    let db_claim = Arc::clone(&db);
    router.register_handler(
        "eve_public.achievement.reward.api.ClaimRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(_cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let mut filter = RewardClaimFilter::default();
            if let Some(DynamicValue::Message(src)) = dyn_msg.get(1) {
                if let Some(v) = src.get(1) {
                    filter.achievement_id = Some(
                        extract_u32_ident(v)
                            .filter(|&id| id > 0)
                            .ok_or_else(|| (STATUS_BAD_REQUEST, "Invalid achievement identifier".into()))?,
                    );
                }
                if let Some(v) = src.get(2) {
                    filter.milestone_id = Some(
                        extract_u32_ident(v)
                            .filter(|&id| id > 0)
                            .ok_or_else(|| (STATUS_BAD_REQUEST, "Invalid milestone identifier".into()))?,
                    );
                }
            } else if let Some(DynamicValue::Message(src)) = dyn_msg.get(2) {
                if let Some(v) = src.get(1) {
                    filter.category_id = Some(
                        extract_u32_ident(v)
                            .filter(|&id| id > 0)
                            .ok_or_else(|| (STATUS_BAD_REQUEST, "Invalid category identifier".into()))?,
                    );
                }
                if let Some(v) = src.get(2) {
                    filter.milestone_id = Some(
                        extract_u32_ident(v)
                            .filter(|&id| id > 0)
                            .ok_or_else(|| (STATUS_BAD_REQUEST, "Invalid milestone identifier".into()))?,
                    );
                }
            }
            // DATA GAP: Achievement reward payouts require server CDN checkpoint definitions
            // (not shipped in local client FSD `achievements.static`). Fail closed without
            // minting ISK, free SP, or wallet journal entries.
            let _ = (&mgr, &db_claim, &filter);
            Err((
                STATUS_PRECONDITION_FAILED,
                "DATA GAP: Achievement reward payouts require server CDN checkpoint definitions"
                    .into(),
            ))
        }),
    );

    // 6. eve_public.achievement.definition.api.cdn.GetLatestRequest -> GetLatestResponse
    let mgr_cdn = Arc::clone(&manager);
    router.register_handler(
        "eve_public.achievement.definition.api.cdn.GetLatestRequest",
        Arc::new(move |_ctx, any, dyn_msg| {
            let no_local = dyn_msg.get(2).and_then(|v| match v {
                DynamicValue::Bool(b) => Some(*b),
                DynamicValue::Uint64(u) => Some(*u != 0),
                DynamicValue::Uint32(u) => Some(*u != 0),
                DynamicValue::Int64(i) => Some(*i != 0),
                DynamicValue::Int32(i) => Some(*i != 0),
                _ => None,
            }).unwrap_or(false);

            if !no_local
                && any.type_url.starts_with("type.evetech.net/")
                && std::env::var("EVOMU_CDN_BASE_URL")
                    .ok()
                    .filter(|s| !s.trim().is_empty())
                    .is_none()
            {
                let fb = super::quasar_achievements::empty_achievement_flatbuffer();
                super::quasar_achievements::ensure_client_flatbuffer_storage_seeded("achievements", "24.1.60", &fb);
                return Ok(GoogleAny {
                    type_url: String::new(),
                    value: Vec::new(),
                });
            }

            if dyn_msg.get(1).is_some() && !no_local {
                return Err((
                    STATUS_SERVICE_UNAVAILABLE,
                    "External TLS CDN not configured; use local client achievement definitions"
                        .to_string(),
                ));
            }
            let count = mgr_cdn.sde_achievement_count;
            let hash = &mgr_cdn.sde_achievement_hash;
            let cdn_base = std::env::var("EVOMU_CDN_BASE_URL")
                .ok()
                .map(|s| s.trim_end_matches('/').to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "http://127.0.0.1:26004/cdn".to_string());
            let url = format!("{cdn_base}/achievements/v24.1.{count}/{hash}.json");

            let mut checkpoint = Vec::new();
            encode_string_field(1, &url, &mut checkpoint);

            let spec = if let Some(DynamicValue::Message(client_ver)) = dyn_msg.get(1) {
                let mut s = Vec::new();
                for fno in 1..=3 {
                    if let Some(v) = client_ver.get(fno).and_then(extract_u64_val) {
                        encode_varint_field(fno, v, &mut s);
                    }
                }
                for fno in 4..=5 {
                    match client_ver.get(fno) {
                        Some(DynamicValue::String(tag)) => encode_string_field(fno, tag, &mut s),
                        Some(DynamicValue::Repeated(tags)) => {
                            for t in tags {
                                if let DynamicValue::String(tag) = t {
                                    encode_string_field(fno, tag, &mut s);
                                }
                            }
                        }
                        _ => {}
                    }
                }
                s
            } else {
                let mut s = Vec::new();
                encode_varint_field(1, 24, &mut s);
                encode_varint_field(2, 1, &mut s);
                encode_varint_field(3, count as u64, &mut s);
                s
            };
            encode_bytes_field(2, &spec, &mut checkpoint);

            let crc_val =
                crate::router::cache_notifications::crc32_ieee(&mgr_cdn.sde_definitions_json) as u32;
            let mut crc_msg = Vec::new();
            encode_varint_field(1, crc_val as u64, &mut crc_msg);
            encode_bytes_field(3, &crc_msg, &mut checkpoint);

            let mut out = Vec::new();
            encode_bytes_field(1, &checkpoint, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.definition.api.cdn.GetLatestResponse"
                ),
                value: out,
            })
        }),
    );

    // 7-11. Admin handlers (AddProgress, Complete, Reset, ResetAll, Category Reset)
    register_achievement_admin_handlers(router, Arc::clone(&manager));

    // 12. eve.achievement.api.GetRequest -> GetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.achievement.api.GetRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let score = ctx
                .effective_character_id()
                .map(|cid| mgr.total_character_score(cid))
                .unwrap_or(0);
            let mut out = Vec::new();
            encode_varint_field(1, score as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.achievement.api.GetResponse"),
                value: out,
            })
        }),
    );

    // 13. eve.achievement.category.api.GetRequest -> GetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.achievement.category.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cat_id = dyn_msg
                .get(1)
                .and_then(extract_u32_ident)
                .filter(|&id| id > 0)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing category identifier".into()))?;
            let score = ctx
                .effective_character_id()
                .map(|cid| mgr.category_score(cid, cat_id))
                .unwrap_or(0);
            let mut out = Vec::new();
            encode_varint_field(1, score as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.achievement.category.api.GetResponse"),
                value: out,
            })
        }),
    );

    // 14. eve.achievement.definition.api.GetLatestRequest -> GetLatestResponse
    let mgr_cdn_eve = Arc::clone(&manager);
    router.register_handler(
        "eve.achievement.definition.api.GetLatestRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let json_bytes = serde_json::to_vec(&mgr_cdn_eve.definitions).unwrap_or_default();
            let hash = sha256_hex(&json_bytes);
            let mut out = Vec::new();
            encode_string_field(1, &hash, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.achievement.definition.api.GetLatestResponse"
                ),
                value: out,
            })
        }),
    );

    // 15. eve.achievement.definition.version.api.ListRequest -> ListResponse
    router.register_handler(
        "eve.achievement.definition.version.api.ListRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            encode_string_field(1, "v24.01.01-air-achievements", &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.achievement.definition.version.api.ListResponse"
                ),
                value: out,
            })
        }),
    );

    // 16. eve.achievement.definition.version.api.GetDefaultVersionRequest -> GetDefaultVersionResponse
    router.register_handler(
        "eve.achievement.definition.version.api.GetDefaultVersionRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            encode_string_field(1, "v24.01.01-air-achievements", &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.achievement.definition.version.api.GetDefaultVersionResponse"
                ),
                value: out,
            })
        }),
    );

    // 17. eve.achievement.reward.api.GetUnclaimedRequest -> GetUnclaimedResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.achievement.reward.api.GetUnclaimedRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let achievements = mgr.get_or_init_achievements(cid);
            let mut out = Vec::new();
            for a in &achievements {
                if a.unclaimed_reward_count > 0 {
                    let ms_ids: Vec<u32> = if !a.reached_milestones.is_empty() {
                        a.reached_milestones.iter().rev().take(a.unclaimed_reward_count as usize).copied().rev().collect()
                    } else {
                        (1..=a.unclaimed_reward_count).collect()
                    };
                    for m_id in ms_ids {
                        let mut src = Vec::new();
                        encode_uuid_ident(1, a.def.id, &mut src);
                        encode_uuid_ident(2, m_id, &mut src);
                        let mut rew_ident = Vec::new();
                        encode_bytes_field(1, &super::quasar_achievements::achievement_reward_uuid(a.def.id, m_id), &mut rew_ident);
                        let mut rew = Vec::new();
                        encode_bytes_field(1, &src, &mut rew);
                        encode_bytes_field(3, &rew_ident, &mut rew);
                        encode_bytes_field(1, &rew, &mut out);
                    }
                }
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.achievement.reward.api.GetUnclaimedResponse"
                ),
                value: out,
            })
        }),
    );

    // 18. eve.achievement.showcase.api.GetCharacterScoreRequest -> GetCharacterScoreResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.achievement.showcase.api.GetCharacterScoreRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_cid = match dyn_msg.get(1) {
                None => cid,
                Some(DynamicValue::Message(m)) if m.get(1).is_none() => cid,
                Some(v) => extract_u32_ident(v)
                    .filter(|&id| id > 0)
                    .ok_or_else(|| (STATUS_BAD_REQUEST, "Invalid character identifier".into()))?,
            };
            let score = mgr.total_character_score(target_cid);
            let mut out = Vec::new();
            encode_varint_field(1, score as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.achievement.showcase.api.GetCharacterScoreResponse"
                ),
                value: out,
            })
        }),
    );
}

#[cfg(test)]
#[path = "quasar_achievements_ops_tests.rs"]
mod quasar_achievements_ops_tests;

