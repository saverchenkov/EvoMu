//! `quasar_achievements_ops` — Admin RPC handlers and notice publishers for AIR Achievements
//! (`feat:quasar.achievement`, `quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, PublicGatewayRouter, QuasarHandlerFn, STATUS_BAD_REQUEST,
    STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_PRECONDITION_FAILED, STATUS_SERVICE_UNAVAILABLE,
};
use super::quasar_achievements::{
    emit_achievement_notices, extract_u32_ident, extract_u64_val, AchievementManager,
    RewardClaimFilter,
};
use eve_db::WorldRepository;
use eve_proto::pb;
use eve_proto::quasar::{
    sha256_hex, DynamicValue, GoogleAny, GoogleEmpty, GoogleTimestamp, QuasarMessage,
};
use std::sync::Arc;

fn id_to_uuid(id: u32) -> [u8; 16] {
    let mut uuid = [0u8; 16];
    uuid[12..16].copy_from_slice(&id.to_be_bytes());
    uuid
}

pub fn publish_achievement_milestone_reached_notice(
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    cid: u32,
    aid: u32,
    milestone_id: u32,
) {
    let notice = pb::message("eve_public.achievement.milestone.api.ReachedNotice")
        .set("achievement.uuid", id_to_uuid(aid))
        .set("milestone.uuid", id_to_uuid(milestone_id))
        .must_to_any();
    gateway
        .pending_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((crate::quasar_gateway::NoticeScope::Character(cid), notice));
}

pub fn publish_achievement_version_check_notice(
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    major: u64,
    minor: u64,
    patch: u64,
) {
    let notice = pb::message("eve_public.achievement.definition.version.api.CheckVersionNotice")
        .set("version.major", major as u32)
        .set("version.minor", minor as u32)
        .set("version.patch", patch as u32)
        .must_to_any();

    gateway
        .pending_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((crate::quasar_gateway::NoticeScope::Broadcast, notice));
}

pub fn publish_achievement_category_progress_notice(
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    cid: u32,
    category_id: u32,
    score: u32,
) {
    let notice = pb::message("eve_public.achievement.category.api.ProgressedNotice")
        .set("category.uuid", id_to_uuid(category_id))
        .set("score", score)
        .must_to_any();
    gateway
        .pending_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((crate::quasar_gateway::NoticeScope::Character(cid), notice));
}

pub fn publish_achievement_category_milestone_notice(
    gateway: &crate::quasar_gateway::PublicGatewayRouter,
    cid: u32,
    category_id: u32,
    milestone_id: u32,
) {
    let notice = pb::message("eve_public.achievement.category.milestone.api.ReachedNotice")
        .set("category.uuid", id_to_uuid(category_id))
        .set("milestone.uuid", id_to_uuid(milestone_id))
        .must_to_any();
    gateway
        .pending_notices
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .push((crate::quasar_gateway::NoticeScope::Character(cid), notice));
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

            let cat_prog = pb::message("eve_public.achievement.category.api.ProgressedNotice")
                .set("category.uuid", id_to_uuid(cat_id))
                .set("score", 0u32)
                .must_to_any();
            super::quasar_social_chat::ctx_queue_notice(
                crate::quasar_gateway::NoticeScope::Character(cid),
                cat_prog,
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
            let ver_notice =
                pb::message("eve_public.achievement.definition.version.api.CheckVersionNotice")
                    .set("version.major", 24u32)
                    .set("version.minor", 1u32)
                    .set("version.patch", 0u32)
                    .must_to_any();

            super::quasar_social_chat::ctx_queue_notice(
                crate::quasar_gateway::NoticeScope::Broadcast,
                ver_notice,
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

            let ts_proto = GoogleTimestamp {
                seconds: a.completed_date.unwrap_or(1_700_000_000) as i64,
                nanos: 0,
            };
            let pts_per_ms = a.def.score / (a.def.milestone_targets.len().max(1) as u32);
            let mut msg = pb::message("eve_public.achievement.api.GetResponse");
            if a.completed {
                msg = msg.set("completed_date", ts_proto.clone());
            } else {
                msg = msg.set("not_completed", true);
            }
            msg = msg.set("progress_value", a.progress);
            for m_id in &a.reached_milestones {
                msg = msg.add_with("reached_milestones", |rm| {
                    rm.set("milestone.uuid", id_to_uuid(*m_id))
                        .set("reached_date", ts_proto.clone())
                        .set("category_points_awarded", pts_per_ms)
                });
            }

            Ok(msg.must_to_any())
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
            Ok(pb::message("eve_public.achievement.category.api.GetResponse")
                .set("score", score)
                .must_to_any())
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
            Ok(pb::message("eve_public.achievement.showcase.api.GetCharacterScoreResponse")
                .set("score", score)
                .must_to_any())
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
            let mut msg = pb::message("eve_public.achievement.reward.api.GetUnclaimedResponse");
            for a in &achievements {
                if a.unclaimed_reward_count > 0 {
                    let ms_ids: Vec<u32> = if !a.reached_milestones.is_empty() {
                        a.reached_milestones.iter().rev().take(a.unclaimed_reward_count as usize).copied().rev().collect()
                    } else {
                        (1..=a.unclaimed_reward_count).collect()
                    };
                    for m_id in ms_ids {
                        let rew_uuid = super::quasar_achievements::achievement_reward_uuid(a.def.id, m_id);
                        msg = msg.add_with("rewards", |rew| {
                            rew.set("achievement_source.achievement.uuid", id_to_uuid(a.def.id))
                                .set("achievement_source.milestone.uuid", id_to_uuid(m_id))
                                .add_with("unclaimed_rewards", |ident| ident.set("uuid", rew_uuid))
                        });
                    }
                }
            }
            Ok(msg.must_to_any())
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

            let crc_val =
                crate::router::cache_notifications::crc32_ieee(&mgr_cdn.sde_definitions_json) as u32;
            let msg = pb::message("eve_public.achievement.definition.api.cdn.GetLatestResponse")
                .set("checkpoint.url", &url)
                .set("checkpoint.crc.crc", crc_val)
                .set_with("checkpoint.version", |ver| {
                    if let Some(DynamicValue::Message(client_ver)) = dyn_msg.get(1) {
                        let mut v = ver
                            .set_opt(
                                "major",
                                client_ver
                                    .get(1)
                                    .and_then(extract_u64_val)
                                    .and_then(|n| u32::try_from(n).ok()),
                            )
                            .set_opt(
                                "minor",
                                client_ver
                                    .get(2)
                                    .and_then(extract_u64_val)
                                    .and_then(|n| u32::try_from(n).ok()),
                            )
                            .set_opt(
                                "patch",
                                client_ver
                                    .get(3)
                                    .and_then(extract_u64_val)
                                    .and_then(|n| u32::try_from(n).ok()),
                            );
                        for (fno, field_name) in [(4, "prerelease_tags"), (5, "build_tags")] {
                            match client_ver.get(fno) {
                                Some(DynamicValue::String(tag)) => {
                                    v = v.add(field_name, tag.as_str());
                                }
                                Some(DynamicValue::Repeated(tags)) => {
                                    for t in tags {
                                        if let DynamicValue::String(tag) = t {
                                            v = v.add(field_name, tag.as_str());
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                        v
                    } else {
                        ver.set("major", 24u32)
                            .set("minor", 1u32)
                            .set("patch", count as u32)
                    }
                });

            Ok(msg.must_to_any())
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
            Ok(pb::message("eve.achievement.api.GetResponse")
                .set("not_completed", true)
                .set("progress_value", score as u64)
                .must_to_any())
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
            Ok(pb::message("eve.achievement.category.api.GetResponse")
                .set("score", score)
                .must_to_any())
        }),
    );

    // 14. eve.achievement.definition.api.GetLatestRequest -> GetLatestResponse
    let mgr_cdn_eve = Arc::clone(&manager);
    router.register_handler(
        "eve.achievement.definition.api.GetLatestRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let json_bytes = serde_json::to_vec(&mgr_cdn_eve.definitions).unwrap_or_default();
            let hash = sha256_hex(&json_bytes);
            Ok(pb::message("eve.achievement.definition.api.GetLatestResponse")
                .set("checkpoint.url", &hash)
                .must_to_any())
        }),
    );

    // 15. eve.achievement.definition.version.api.ListRequest -> ListResponse
    router.register_handler(
        "eve.achievement.definition.version.api.ListRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            Ok(pb::message("eve.achievement.definition.version.api.ListResponse")
                .add_with("versions", |v| {
                    v.set("major", 24u32)
                        .set("minor", 1u32)
                        .set("patch", 1u32)
                        .add("build_tags", "air-achievements")
                })
                .must_to_any())
        }),
    );

    // 16. eve.achievement.definition.version.api.GetDefaultVersionRequest -> GetDefaultVersionResponse
    router.register_handler(
        "eve.achievement.definition.version.api.GetDefaultVersionRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            Ok(
                pb::message("eve.achievement.definition.version.api.GetDefaultVersionResponse")
                    .set("version.major", 24u32)
                    .set("version.minor", 1u32)
                    .set("version.patch", 1u32)
                    .add("version.build_tags", "air-achievements")
                    .must_to_any(),
            )
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
            let mut msg = pb::message("eve.achievement.reward.api.GetUnclaimedResponse");
            for a in &achievements {
                if a.unclaimed_reward_count > 0 {
                    let ms_ids: Vec<u32> = if !a.reached_milestones.is_empty() {
                        a.reached_milestones.iter().rev().take(a.unclaimed_reward_count as usize).copied().rev().collect()
                    } else {
                        (1..=a.unclaimed_reward_count).collect()
                    };
                    for m_id in ms_ids {
                        let rew_uuid =
                            super::quasar_achievements::achievement_reward_uuid(a.def.id, m_id);
                        msg = msg.add_with("rewards", |rew| {
                            rew.set("achievement_source.achievement.uuid", id_to_uuid(a.def.id))
                                .set("achievement_source.milestone.uuid", id_to_uuid(m_id))
                                .add_with("unclaimed_rewards", |ident| ident.set("uuid", rew_uuid))
                        });
                    }
                }
            }
            Ok(msg.must_to_any())
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
            Ok(pb::message("eve.achievement.showcase.api.GetCharacterScoreResponse")
                .set("score", score)
                .must_to_any())
        }),
    );
}

#[cfg(test)]
#[path = "quasar_achievements_ops_tests.rs"]
mod quasar_achievements_ops_tests;

