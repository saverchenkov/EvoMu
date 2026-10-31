//! `quasar_b33_services` — Batch 33 Quasar Public Gateway Service Handlers.
//!
//! Implements state-backed, SDE-validated Quasar protobuf handlers for:
//! - `eve_public.faction.activity.admin` (`IncrementContributionScoreRequest`, `SetContributionScoreRequest`)
//! - `eve_public.payment.schedule.api` (`GetRequest`, `GetActiveByTypeRequest`)
//! - `eve_public.achievement.api.admin` (`AddProgressRequest`, `CompleteRequest`, `ResetRequest`, `ResetAllRequest`)
//! - `eve_public.career.goal.api.admin` (`ProgressRequest`, `CompleteRequest`)
//! - `eve_public.dailygoal.api.admin` (`ProgressRequest`)

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_NOT_FOUND,
    STATUS_UNAUTHORIZED,
};
use crate::services::quasar_achievements::emit_achievement_notices;
use crate::services::quasar_payment_store_types::PaymentScheduleRecord;
use crate::services::quasar_progression::ProgressionManagers;
use crate::services::quasar_space_services::{get_identifier_or_int, get_int_field};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, GoogleAny, GoogleEmpty, QuasarMessage, WireMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::SdeRegistry;
use std::sync::Arc;

fn encode_u64_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut sub = Vec::new();
    encode_varint_field(1, id, &mut sub);
    encode_bytes_field(field_num, &sub, out);
}

/// Encodes an achievement/category UUID identifier field for Quasar requests.
pub fn encode_ach_uuid_ident(field_no: u32, id: u32, out: &mut Vec<u8>) {
    super::quasar_achievements::encode_uuid_ident(field_no, id, out);
}

fn extract_uuid_16(dyn_msg: &DynamicQuasarMessage, field_num: u32) -> Option<[u8; 16]> {
    let bytes = match dyn_msg.get(field_num)? {
        DynamicValue::Message(sub) => match sub.get(1)? {
            DynamicValue::Bytes(b) if b.len() == 16 => b.as_slice(),
            _ => return None,
        },
        DynamicValue::Bytes(b) if b.len() == 16 => b.as_slice(),
        _ => return None,
    };
    let mut arr = [0u8; 16];
    arr.copy_from_slice(bytes);
    Some(arr)
}

fn extract_u64_field(dyn_msg: &DynamicQuasarMessage, field_num: u32) -> Option<u64> {
    match dyn_msg.get(field_num)? {
        DynamicValue::Uint64(v) => Some(*v),
        DynamicValue::Int64(v) if *v >= 0 => Some(*v as u64),
        DynamicValue::Uint32(v) => Some(u64::from(*v)),
        DynamicValue::Int32(v) if *v >= 0 => Some(*v as u64),
        _ => None,
    }
}

/// Persists a new recurring payment schedule record in `WorldStateSnapshot` for `user_id`.
pub fn create_user_payment_schedule(
    db: &WorldRepository,
    user_id: u64,
    schedule_type: u32,
    offer_id: &str,
    interval_days: u32,
    now_unix: u64,
) -> PaymentScheduleRecord {
    db.with_mut_snapshot(|snap| {
        let mut schedules: Vec<PaymentScheduleRecord> = snap
            .service_state_blobs
            .get("payment_schedules")
            .and_then(|json| serde_json::from_str(json).ok())
            .unwrap_or_default();
        let next_id = schedules.iter().map(|s| s.id).max().unwrap_or(1000) + 1;
        let rec = PaymentScheduleRecord {
            id: next_id,
            user_id,
            schedule_type,
            offer_id: offer_id.to_string(),
            interval_days,
            next_payment_unix: now_unix + u64::from(interval_days) * 86_400,
            created_unix: now_unix,
        };
        schedules.push(rec.clone());
        if let Ok(json) = serde_json::to_string(&schedules) {
            snap.service_state_blobs
                .insert("payment_schedules".to_string(), json);
        }
        rec
    })
}

/// Registers Batch 33 Quasar service handlers on `router`.
pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
    progression: Arc<ProgressionManagers>,
) {
    register_faction_activity_admin_handlers(router, Arc::clone(&db), Arc::clone(&sde));
    register_payment_schedule_handlers(router, Arc::clone(&db));
    register_achievement_admin_handlers(router, Arc::clone(&progression));
    register_career_goal_admin_handlers(router, Arc::clone(&progression));
    register_daily_goal_admin_handlers(router, Arc::clone(&progression));
}

fn register_faction_activity_admin_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    sde: Arc<SdeRegistry>,
) {
    for (req_name, rsp_name, is_increment) in [
        (
            "eve_public.faction.activity.admin.IncrementContributionScoreRequest",
            "eve_public.faction.activity.admin.IncrementContributionScoreResponse",
            true,
        ),
        (
            "eve_public.faction.activity.admin.SetContributionScoreRequest",
            "eve_public.faction.activity.admin.SetContributionScoreResponse",
            false,
        ),
    ] {
        let db_fw = Arc::clone(&db);
        let sde_fw = Arc::clone(&sde);
        let pending_fw = Arc::clone(&router.pending_notices);
        router.register_handler(
            req_name,
            Arc::new(move |ctx, _any, dyn_msg| {
                let caller_cid = ctx
                    .effective_character_id()
                    .ok_or((STATUS_UNAUTHORIZED, "Authenticated character required".to_string()))?
                    as u64;
                require_admin_role(ctx)?;

                let sys_id = get_identifier_or_int(dyn_msg, 1)
                    .map(|v| v as u32)
                    .filter(|&v| v > 0)
                    .ok_or((STATUS_BAD_REQUEST, "target_system is required".to_string()))?;
                if !sde_fw.solar_systems.contains_key(&sys_id) {
                    return Err((
                        STATUS_NOT_FOUND,
                        format!("Solar system {sys_id} not found in SDE"),
                    ));
                }

                let opt_fac = get_identifier_or_int(dyn_msg, 2).map(|v| v as u32);
                if let Some(fac_id) = opt_fac {
                    if !sde_fw.factions.contains_key(&fac_id) {
                        return Err((
                            STATUS_NOT_FOUND,
                            format!("Faction {fac_id} not found in SDE"),
                        ));
                    }
                }

                let delta_or_val = get_int_field(dyn_msg, 3).unwrap_or(0);
                let mut resolved_fac_id = 0u64;
                let mutate_res = db_fw.with_mut_snapshot(|snap| {
                    let occ = snap.fw_system_occupations.get_mut(&sys_id)?;
                    resolved_fac_id = opt_fac
                        .map(u64::from)
                        .unwrap_or(occ.occupier_faction_id as u64);
                    let threshold = (occ.victory_points_threshold as f64).max(1.0);
                    let entry = snap
                        .fw_runtime
                        .system_victory_points
                        .entry(sys_id)
                        .or_insert(occ.victory_points);
                    if is_increment {
                        *entry = entry.saturating_add(delta_or_val).max(0);
                    } else {
                        *entry = delta_or_val.max(0);
                        occ.occupier_faction_id = resolved_fac_id as i64;
                    }
                    let vp = *entry as u64;
                    let contested = ((vp as f64 / threshold) * 100.0).clamp(0.0, 100.0);
                    if let Some(st) = snap.system_states.get_mut(&sys_id) {
                        st.fw_contested_pct = contested;
                    }
                    occ.victory_points = *entry;
                    let floor = snap
                        .fw_runtime
                        .system_advantage
                        .get(&sys_id)
                        .map(|(occ_adv, _)| ((*occ_adv).max(0.0) * 100.0) as u64)
                        .unwrap_or(0);
                    Some((vp, floor))
                });

                let (new_vp, adv_floor) = mutate_res.ok_or_else(|| {
                    (
                        STATUS_BAD_REQUEST,
                        format!("Solar system {sys_id} is not part of Faction Warfare warzone"),
                    )
                })?;

                if is_increment {
                    let mut contrib_notice = Vec::new();
                    encode_u64_id_field(1, u64::from(sys_id), &mut contrib_notice);
                    encode_u64_id_field(2, caller_cid, &mut contrib_notice);
                    encode_u64_id_field(3, resolved_fac_id, &mut contrib_notice);
                    encode_varint_field(4, delta_or_val.max(0) as u64, &mut contrib_notice);
                    pending_fw
                        .write()
                        .unwrap_or_else(|e| e.into_inner())
                        .push((
                            NoticeScope::SolarSystem(sys_id),
                            GoogleAny {
                                type_url: format!(
                                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.faction.activity.ScoreContributionNotice"
                                ),
                                value: contrib_notice,
                            },
                        ));
                }

                let mut scores_notice = Vec::new();
                encode_u64_id_field(1, u64::from(sys_id), &mut scores_notice);
                let mut score_entry = Vec::new();
                encode_u64_id_field(1, resolved_fac_id, &mut score_entry);
                encode_varint_field(2, new_vp, &mut score_entry);
                encode_varint_field(3, adv_floor, &mut score_entry);
                encode_bytes_field(2, &score_entry, &mut scores_notice);
                pending_fw
                    .write()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((
                        NoticeScope::SolarSystem(sys_id),
                        GoogleAny {
                            type_url: format!(
                                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.faction.activity.SolarSystemScoresNotice"
                            ),
                            value: scores_notice,
                        },
                    ));

                Ok(GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{rsp_name}"),
                    value: Vec::new(),
                })
            }),
        );
    }
}

fn register_payment_schedule_handlers(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    let db_get = Arc::clone(&db);
    router.register_handler(
        "eve_public.payment.schedule.api.GetRequest",
        Arc::new(move |ctx, any, dyn_msg| {
            let uid = ctx
                .effective_user_id()
                .ok_or((STATUS_UNAUTHORIZED, "Authenticated user required".to_string()))?
                as u64;
            let sched_id = get_identifier_or_int(dyn_msg, 1)
                .map(|v| v as u64)
                .or_else(|| {
                    WireMessage::decode(&any.value).ok().and_then(|wm| {
                        wm.fields
                            .iter()
                            .find(|f| f.field_number == 1)
                            .and_then(|f| f.value.as_varint())
                    })
                })
                .filter(|&id| id > 0)
                .ok_or((STATUS_BAD_REQUEST, "schedule Identifier is required".to_string()))?;

            let sched = db_get
                .with_snapshot(|snap| {
                    snap.service_state_blobs
                        .get("payment_schedules")
                        .and_then(|json| {
                            serde_json::from_str::<Vec<PaymentScheduleRecord>>(json).ok()
                        })
                        .and_then(|list| {
                            list.into_iter()
                                .find(|s| s.id == sched_id && s.user_id == uid)
                        })
                })
                .ok_or((STATUS_NOT_FOUND, format!("Payment schedule {sched_id} not found")))?;

            let mut attr = Vec::new();
            encode_varint_field(1, u64::from(sched.schedule_type), &mut attr);
            let mut ts_bytes = Vec::new();
            encode_varint_field(1, sched.next_payment_unix, &mut ts_bytes);
            encode_bytes_field(2, &ts_bytes, &mut attr);
            let mut off_ident = Vec::new();
            encode_string_field(1, &sched.offer_id, &mut off_ident);
            encode_bytes_field(3, &off_ident, &mut attr);
            encode_varint_field(4, u64::from(sched.interval_days), &mut attr);

            let mut out = Vec::new();
            encode_bytes_field(1, &attr, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.schedule.api.GetResponse"
                ),
                value: out,
            })
        }),
    );

    let db_type = Arc::clone(&db);
    router.register_handler(
        "eve_public.payment.schedule.api.GetActiveByTypeRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let uid = ctx
                .effective_user_id()
                .ok_or((STATUS_UNAUTHORIZED, "Authenticated user required".to_string()))?
                as u64;
            let stype = match dyn_msg.get(1) {
                Some(DynamicValue::Enum(e)) if *e > 0 => *e as u32,
                Some(DynamicValue::Uint32(u)) if *u > 0 => *u,
                Some(DynamicValue::Int32(i)) if *i > 0 => *i as u32,
                Some(DynamicValue::Uint64(u)) if *u > 0 => *u as u32,
                Some(DynamicValue::Int64(i)) if *i > 0 => *i as u32,
                _ => return Err((STATUS_BAD_REQUEST, "schedule type is required".to_string())),
            };

            let sched = db_type
                .with_snapshot(|snap| {
                    snap.service_state_blobs
                        .get("payment_schedules")
                        .and_then(|json| {
                            serde_json::from_str::<Vec<PaymentScheduleRecord>>(json).ok()
                        })
                        .and_then(|list| {
                            list.into_iter()
                                .find(|s| s.user_id == uid && s.schedule_type == stype)
                        })
                })
                .ok_or((
                    STATUS_NOT_FOUND,
                    format!("No active payment schedule for type {stype}"),
                ))?;

            let mut ident = Vec::new();
            encode_varint_field(1, sched.id, &mut ident);
            let mut out = Vec::new();
            encode_bytes_field(1, &ident, &mut out);
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.payment.schedule.api.GetActiveByTypeResponse"
                ),
                value: out,
            })
        }),
    );
}

fn register_achievement_admin_handlers(
    router: &PublicGatewayRouter,
    progression: Arc<ProgressionManagers>,
) {
    let mgr_prog = Arc::clone(&progression.achievements);
    router.register_handler(
        "eve_public.achievement.api.admin.AddProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cid = ctx
                .effective_character_id()
                .ok_or((STATUS_UNAUTHORIZED, "Active character required".to_string()))?;
            require_admin_role(ctx)?;

            let aid = super::quasar_achievements::extract_u32_ident(
                dyn_msg.get(1).ok_or((
                    STATUS_BAD_REQUEST,
                    "achievement Identifier is required".to_string(),
                ))?,
            )
            .filter(|&id| id > 0)
            .ok_or((STATUS_BAD_REQUEST, "achievement Identifier is required".to_string()))?;
            let def = mgr_prog
                .definitions
                .iter()
                .find(|d| d.id == aid)
                .ok_or((STATUS_NOT_FOUND, format!("Achievement {aid} not found")))?;
            let cat_id = def.category_id;

            let delta = extract_u64_field(dyn_msg, 2)
                .filter(|&d| d > 0)
                .ok_or((STATUS_BAD_REQUEST, "progress_delta must be > 0".to_string()))?;

            let outcome = mgr_prog.add_progress(cid, aid, delta, ctx.now_unix());
            mgr_prog.save_to_db();

            let cat_score = mgr_prog.category_score(cid, cat_id);
            if let Some((ach, just_completed, new_milestones)) = outcome {
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
            }
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr_comp = Arc::clone(&progression.achievements);
    router.register_handler(
        "eve_public.achievement.api.admin.CompleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cid = ctx
                .effective_character_id()
                .ok_or((STATUS_UNAUTHORIZED, "Active character required".to_string()))?;
            require_admin_role(ctx)?;

            let aid = super::quasar_achievements::extract_u32_ident(
                dyn_msg.get(1).ok_or((
                    STATUS_BAD_REQUEST,
                    "achievement Identifier is required".to_string(),
                ))?,
            )
            .filter(|&id| id > 0)
            .ok_or((STATUS_BAD_REQUEST, "achievement Identifier is required".to_string()))?;
            let def = mgr_comp
                .definitions
                .iter()
                .find(|d| d.id == aid)
                .ok_or((STATUS_NOT_FOUND, format!("Achievement {aid} not found")))?;
            let cat_id = def.category_id;

            let ach = mgr_comp.mark_completed(cid, aid, ctx.now_unix())?;
            mgr_comp.save_to_db();

            let cat_score = mgr_comp.category_score(cid, cat_id);
            emit_achievement_notices(
                cid,
                aid,
                cat_id,
                ach.progress,
                cat_score,
                true,
                &[],
                ctx.now_unix(),
            );
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr_reset = Arc::clone(&progression.achievements);
    router.register_handler(
        "eve_public.achievement.api.admin.ResetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cid = ctx
                .effective_character_id()
                .ok_or((STATUS_UNAUTHORIZED, "Active character required".to_string()))?;
            require_admin_role(ctx)?;

            let aid = super::quasar_achievements::extract_u32_ident(
                dyn_msg.get(1).ok_or((
                    STATUS_BAD_REQUEST,
                    "achievement Identifier is required".to_string(),
                ))?,
            )
            .filter(|&id| id > 0)
            .ok_or((STATUS_BAD_REQUEST, "achievement Identifier is required".to_string()))?;
            if !mgr_reset.definitions.iter().any(|d| d.id == aid) {
                return Err((STATUS_NOT_FOUND, format!("Achievement {aid} not found")));
            }
            let _ = mgr_reset.reset_achievement(cid, aid)?;
            mgr_reset.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr_reset_all = Arc::clone(&progression.achievements);
    router.register_handler(
        "eve_public.achievement.api.admin.ResetAllRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let cid = ctx
                .effective_character_id()
                .ok_or((STATUS_UNAUTHORIZED, "Active character required".to_string()))?;
            require_admin_role(ctx)?;

            mgr_reset_all.reset_all(cid);
            mgr_reset_all.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );
}

fn register_career_goal_admin_handlers(
    router: &PublicGatewayRouter,
    progression: Arc<ProgressionManagers>,
) {
    let mgr_prog = Arc::clone(&progression.career_goals);
    router.register_handler(
        "eve_public.career.goal.api.admin.ProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cid = ctx
                .effective_character_id()
                .ok_or((STATUS_UNAUTHORIZED, "Active character required".to_string()))?;
            require_admin_role(ctx)?;

            let goal_uuid = extract_uuid_16(dyn_msg, 1)
                .ok_or((STATUS_BAD_REQUEST, "goal Identifier.uuid is required".to_string()))?;
            let delta = extract_u64_field(dyn_msg, 2)
                .filter(|&d| d > 0)
                .ok_or((STATUS_BAD_REQUEST, "progress must be > 0".to_string()))?;

            let updated = mgr_prog
                .add_progress(cid, &goal_uuid, delta, ctx.now_unix())
                .ok_or((STATUS_NOT_FOUND, "Career goal not found".to_string()))?;
            mgr_prog.save_to_db();
            let _ = updated;
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr_comp = Arc::clone(&progression.career_goals);
    router.register_handler(
        "eve_public.career.goal.api.admin.CompleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cid = ctx
                .effective_character_id()
                .ok_or((STATUS_UNAUTHORIZED, "Active character required".to_string()))?;
            require_admin_role(ctx)?;

            let goal_uuid = extract_uuid_16(dyn_msg, 1)
                .ok_or((STATUS_BAD_REQUEST, "goal Identifier.uuid is required".to_string()))?;
            let updated = mgr_comp.mark_completed(cid, &goal_uuid, ctx.now_unix())?;
            mgr_comp.save_to_db();
            let _ = updated;
            Ok(GoogleEmpty.to_any())
        }),
    );
}

fn register_daily_goal_admin_handlers(
    router: &PublicGatewayRouter,
    progression: Arc<ProgressionManagers>,
) {
    let mgr = Arc::clone(&progression.daily_goals);
    router.register_handler(
        "eve_public.dailygoal.api.admin.ProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let goal_uuid = extract_uuid_16(dyn_msg, 1)
                .ok_or((STATUS_BAD_REQUEST, "goal Identifier.uuid is required".to_string()))?;
            let target_cid = get_identifier_or_int(dyn_msg, 2)
                .map(|v| v as u32)
                .filter(|&id| id > 0)
                .or_else(|| ctx.effective_character_id())
                .ok_or((STATUS_UNAUTHORIZED, "Target character is required".to_string()))?;
            let delta = extract_u64_field(dyn_msg, 3)
                .filter(|&d| d > 0)
                .ok_or((STATUS_BAD_REQUEST, "progress must be > 0".to_string()))?;

            let updated = mgr
                .add_progress(target_cid, &goal_uuid, delta, ctx.now_unix())
                .ok_or((STATUS_NOT_FOUND, "Daily goal not found".to_string()))?;
            mgr.save_to_db();
            let _ = updated;
            Ok(GoogleEmpty.to_any())
        }),
    );
}
