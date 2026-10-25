//! `quasar_achievements_ops` — Admin RPC handlers and notice publishers for AIR Achievements
//! (`feat:quasar.achievement`, `quasar:eveProto/generated/eve_public/achievement/api/admin/requests_pb2.pyj`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, PublicGatewayRouter, QuasarHandlerFn, STATUS_BAD_REQUEST,
    STATUS_FORBIDDEN,
};
use super::quasar_achievements::{
    emit_achievement_notices, encode_uuid_ident, AchievementManager,
};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, DynamicValue, GoogleAny, GoogleEmpty, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
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
            .and_then(|v| match v {
                DynamicValue::Uint32(u) => Some(*u),
                DynamicValue::Int32(i) => Some(*i as u32),
                DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Bytes(b) if b.len() >= 4 => {
                        let mut arr = [0u8; 4];
                        arr.copy_from_slice(&b[b.len() - 4..]);
                        Some(u32::from_be_bytes(arr))
                    }
                    _ => None,
                }),
                DynamicValue::Bytes(b) if b.len() >= 4 => {
                    let mut arr = [0u8; 4];
                    arr.copy_from_slice(&b[b.len() - 4..]);
                    Some(u32::from_be_bytes(arr))
                }
                _ => None,
            })
            .unwrap_or(101);
        let delta = dyn_msg
            .get(2)
            .and_then(|v| match v {
                DynamicValue::Uint64(u) => Some(*u),
                DynamicValue::Int64(i) => Some(*i as u64),
                _ => None,
            })
            .unwrap_or(1);

        let outcome = mgr_prog.add_progress(cid, aid, delta, ctx.now_unix());
        mgr_prog.save_to_db();

        let cat_id = mgr_prog.definitions.iter().find(|d| d.id == aid).map(|d| d.category_id).unwrap_or(1);
        let cat_score = mgr_prog.category_score(cid, cat_id);
        if let Some((ach, just_completed, new_milestones)) = outcome {
            emit_achievement_notices(cid, aid, cat_id, ach.progress, cat_score, just_completed, &new_milestones, ctx.now_unix());
        }

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
            .and_then(|v| match v {
                DynamicValue::Uint32(u) => Some(*u),
                DynamicValue::Int32(i) => Some(*i as u32),
                DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Bytes(b) if b.len() >= 4 => {
                        let mut arr = [0u8; 4];
                        arr.copy_from_slice(&b[b.len() - 4..]);
                        Some(u32::from_be_bytes(arr))
                    }
                    _ => None,
                }),
                DynamicValue::Bytes(b) if b.len() >= 4 => {
                    let mut arr = [0u8; 4];
                    arr.copy_from_slice(&b[b.len() - 4..]);
                    Some(u32::from_be_bytes(arr))
                }
                _ => None,
            })
            .unwrap_or(101);
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
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        DynamicValue::Bytes(b) if b.len() >= 4 => {
                            let mut arr = [0u8; 4];
                            arr.copy_from_slice(&b[b.len() - 4..]);
                            Some(u32::from_be_bytes(arr))
                        }
                        _ => None,
                    }),
                    _ => None,
                })
                .unwrap_or(101);
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
            let cat_id = dyn_msg.get(1).and_then(|v| match v {
                DynamicValue::Uint32(u) => Some(*u),
                DynamicValue::Int32(i) => Some(*i as u32),
                DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Bytes(b) if b.len() >= 4 => {
                        let mut arr = [0u8; 4];
                        arr.copy_from_slice(&b[b.len() - 4..]);
                        Some(u32::from_be_bytes(arr))
                    }
                    _ => None,
                }),
                _ => None,
            })
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
