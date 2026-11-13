//! Batch 34 Quasar Gateway Services:
//! - `eve.dailygoal.api` & `eve.dailygoal.api.admin` (`requests_pb2`, `api_pb2`, `events_pb2`)
//! - `eve.loyaltypoints.wallet` & `eve.loyaltypoints.assetholding.operation.api` (`wallet_pb2`, `api_pb2`, `operation_pb2`, `requests_pb2`, `events_pb2`)
//! - `eve.solarsystem.bounty.npc` & `eve.solarsystem.bounty.bank.*` (`npc_pb2`, `bank_pb2`, `mainbank_pb2`, `reservebank_pb2`)

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST,
    STATUS_NOT_FOUND, STATUS_NOT_IMPLEMENTED, STATUS_PRECONDITION_FAILED,
};
use crate::services::quasar_daily_goals::{
    daily_goal_day_bucket, encode_goal_attributes_for_day_bucket, encode_goal_earnings,
    encode_goal_identifier, extract_uuid, CharacterDailyGoal, DailyGoalManager,
};
use crate::services::quasar_social_chat::ctx_queue_notice;
use eve_db::{IskCents, WorldRepository};
use eve_proto::pb::{self, ProtoBuilder};
use eve_proto::quasar::{
    DynamicQuasarMessage, DynamicValue, GoogleAny, GoogleTimestamp, WireMessage, WireValue,
    TYPE_GOOGLEAPIS_PREFIX,
};
use std::sync::Arc;

// ============================================================================
// Wire Encoding & Decoding Helpers
// ============================================================================

fn build_isk_currency(b: ProtoBuilder, cents: IskCents) -> ProtoBuilder {
    let units = cents.0 / 100;
    let nanos = ((cents.0 % 100) * 10_000_000) as i32;
    b.when(units != 0, |b| b.set("units", units))
        .when(nanos != 0, |b| b.set("nanos", nanos))
}

fn build_u32_id(b: ProtoBuilder, id: u32) -> ProtoBuilder {
    b.when(id != 0, |b| b.set("sequential", id))
}

fn build_u64_id(b: ProtoBuilder, id: u32) -> ProtoBuilder {
    b.when(id != 0, |b| b.set("sequential", u64::from(id)))
}

fn extract_u32_field(msg: &DynamicQuasarMessage, field_num: u32) -> Option<u32> {
    match msg.get(field_num) {
        Some(DynamicValue::Uint32(v)) => Some(*v),
        Some(DynamicValue::Int32(v)) => u32::try_from(*v).ok(),
        Some(DynamicValue::Uint64(v)) => u32::try_from(*v).ok(),
        Some(DynamicValue::Int64(v)) => u32::try_from(*v).ok(),
        _ => None,
    }
}

fn extract_u64_field(msg: &DynamicQuasarMessage, field_num: u32) -> Option<u64> {
    match msg.get(field_num) {
        Some(DynamicValue::Uint64(v)) => Some(*v),
        Some(DynamicValue::Int64(v)) => u64::try_from(*v).ok(),
        Some(DynamicValue::Uint32(v)) => Some(u64::from(*v)),
        Some(DynamicValue::Int32(v)) => u64::try_from(*v).ok(),
        _ => None,
    }
}

fn extract_nested_u32(msg: &DynamicQuasarMessage, field_num: u32, sub_field: u32) -> Option<u32> {
    if let Some(DynamicValue::Message(sub)) = msg.get(field_num) {
        extract_u32_field(sub, sub_field)
    } else {
        None
    }
}

fn goal_id_and_attrs(goal: &CharacterDailyGoal) -> (Vec<u8>, Vec<u8>) {
    let mut id_bytes = Vec::new();
    encode_goal_identifier(&goal.uuid, &mut id_bytes);
    let mut attr_bytes = Vec::new();
    encode_goal_attributes_for_day_bucket(&goal.template, goal.day_bucket, &mut attr_bytes);
    (id_bytes, attr_bytes)
}

fn goal_earning_items(goal: &CharacterDailyGoal) -> Vec<Vec<u8>> {
    let mut raw = Vec::new();
    encode_goal_earnings(&goal.template, 1, &mut raw);
    WireMessage::decode(&raw)
        .map(|m| {
            m.fields
                .into_iter()
                .filter_map(|f| match f.value {
                    WireValue::LengthDelimited(bytes) => Some(bytes),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

// ============================================================================
// Daily Goal Event Encoders (`eve.dailygoal.api.*`)
// ============================================================================

fn encode_daily_goal_created(goal: &CharacterDailyGoal) -> Vec<u8> {
    let (id_bytes, attr_bytes) = goal_id_and_attrs(goal);
    pb::message("eve.dailygoal.api.Created")
        .set_raw_message("id", &id_bytes)
        .set_raw_message("goal", &attr_bytes)
        .must_encode()
}

fn encode_daily_goal_prematurely_deactivated(uuid: &[u8; 16], now_unix: u64) -> Vec<u8> {
    let mut id_bytes = Vec::new();
    encode_goal_identifier(uuid, &mut id_bytes);
    pb::message("eve.dailygoal.api.PrematurelyDeactivated")
        .set_raw_message("id", &id_bytes)
        .set("deactivated", GoogleTimestamp { seconds: now_unix as i64, nanos: 0 })
        .must_encode()
}

fn encode_daily_goal_removed(uuid: &[u8; 16]) -> Vec<u8> {
    let mut id_bytes = Vec::new();
    encode_goal_identifier(uuid, &mut id_bytes);
    pb::message("eve.dailygoal.api.Removed")
        .set_raw_message("id", &id_bytes)
        .must_encode()
}

fn encode_daily_goal_progressed(
    cid: u32,
    uuid: &[u8; 16],
    prev: u64,
    curr: u64,
    now_unix: u64,
) -> Vec<u8> {
    let mut id_bytes = Vec::new();
    encode_goal_identifier(uuid, &mut id_bytes);
    pb::message("eve.dailygoal.api.Progressed")
        .set("timestamp", GoogleTimestamp { seconds: now_unix as i64, nanos: 0 })
        .set_with("character", |b| build_u32_id(b, cid))
        .set_raw_message("goal", &id_bytes)
        .set("previous_progress", prev)
        .set("current_progress", curr)
        .must_encode()
}

fn encode_daily_goal_completed(cid: u32, goal: &CharacterDailyGoal, paid: bool) -> Vec<u8> {
    let mut id_bytes = Vec::new();
    encode_goal_identifier(&goal.uuid, &mut id_bytes);
    let mut b = pb::message("eve.dailygoal.api.Completed")
        .set_with("character", |b| build_u32_id(b, cid))
        .set_raw_message("goal", &id_bytes)
        .set("category", 1i32) // CATEGORY_DAILY = 1
        .when(paid, |b| b.set("paid_completion", true));
    for item in goal_earning_items(goal) {
        b = b.add("earnings", pb::raw_message(item));
    }
    b.must_encode()
}

fn encode_daily_goal_redeemed(cid: u32, goal: &CharacterDailyGoal, autoredeemed: bool) -> Vec<u8> {
    let mut id_bytes = Vec::new();
    encode_goal_identifier(&goal.uuid, &mut id_bytes);
    let mut b = pb::message("eve.dailygoal.api.Redeemed")
        .set_with("character", |b| build_u32_id(b, cid))
        .set_raw_message("goal", &id_bytes)
        .when(autoredeemed, |b| b.set("autoredeemed", true));
    for item in goal_earning_items(goal) {
        b = b.add("earnings", pb::raw_message(item));
    }
    b.must_encode()
}

fn encode_period_regenerated(category: u64, now_unix: u64) -> Vec<u8> {
    let duration = if category == 2 { 30 * 86_400 } else { 86_400 };
    pb::message("eve.dailygoal.api.PeriodRegenerated")
        .set("period.start", GoogleTimestamp { seconds: now_unix as i64, nanos: 0 })
        .set(
            "period.end",
            GoogleTimestamp {
                seconds: now_unix.saturating_add(duration) as i64,
                nanos: 0,
            },
        )
        .set("period.category", category as i32)
        .set("timestamp", GoogleTimestamp { seconds: now_unix as i64, nanos: 0 })
        .must_encode()
}

// ============================================================================
// Loyalty Points Wallet & AssetHolding Helpers
// ============================================================================

#[derive(Clone, Copy, Debug)]
enum LpWalletOwner {
    Character(u32),
    Corporation(u32),
}

struct ParsedLpOperation {
    op_uuid: [u8; 16],
    owner: LpWalletOwner,
    issuer_corp_id: u32,
    amount: i64,
    context_bytes: Vec<u8>,
}

fn parse_lp_operation_request(
    dyn_msg: &DynamicQuasarMessage,
    default_cid: Option<u32>,
) -> Result<ParsedLpOperation, (u32, String)> {
    let op_uuid = extract_uuid(dyn_msg, 1)
        .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing operation Identifier.uuid".into()))?;
    let Some(DynamicValue::Message(op_attr)) = dyn_msg.get(2) else {
        return Err((STATUS_BAD_REQUEST, "Missing operation Attributes".into()));
    };
    let Some(DynamicValue::Message(wallet_id)) = op_attr.get(1) else {
        return Err((STATUS_BAD_REQUEST, "Missing wallet Identifier".into()));
    };
    let owner = if let Some(cid) = extract_nested_u32(wallet_id, 1, 1) {
        if cid == 0 {
            return Err((STATUS_BAD_REQUEST, "Invalid character_id 0".into()));
        }
        LpWalletOwner::Character(cid)
    } else if let Some(corp_id) = extract_nested_u32(wallet_id, 2, 1) {
        if corp_id == 0 {
            return Err((STATUS_BAD_REQUEST, "Invalid corporation_id 0".into()));
        }
        LpWalletOwner::Corporation(corp_id)
    } else if let Some(cid) = default_cid {
        LpWalletOwner::Character(cid)
    } else {
        return Err((STATUS_BAD_REQUEST, "Missing wallet owner".into()));
    };

    let issuer_corp_id = extract_nested_u32(wallet_id, 3, 1).unwrap_or(0);
    if issuer_corp_id == 0 {
        return Err((STATUS_BAD_REQUEST, "Missing issuer corporation".into()));
    }

    let raw_amount = if let Some(DynamicValue::Message(curr)) = op_attr.get(2) {
        extract_u64_field(curr, 1).unwrap_or(0)
    } else {
        0
    };
    if raw_amount == 0 {
        return Err((STATUS_BAD_REQUEST, "LP operation amount must be > 0".into()));
    }
    let amount = i64::try_from(raw_amount)
        .ok()
        .filter(|&v| v > 0)
        .ok_or_else(|| (STATUS_BAD_REQUEST, "LP amount out of range".into()))?;

    let context_bytes = match dyn_msg.get(3) {
        Some(DynamicValue::Message(m)) => {
            static CTX_REG: std::sync::OnceLock<Arc<eve_proto::quasar::QuasarDescriptorRegistry>> =
                std::sync::OnceLock::new();
            let reg = CTX_REG.get_or_init(eve_proto::quasar::QuasarDescriptorRegistry::open_default_or_empty);
            let mut ctx_msg = (**m).clone();
            if ctx_msg.full_name.is_empty() {
                ctx_msg.full_name = "eve.assetholding.Context".to_string();
            }
            reg.encode_dynamic(&ctx_msg)
                .map_err(|e| (STATUS_BAD_REQUEST, format!("Invalid LP operation context: {e}")))?
        }
        Some(DynamicValue::Bytes(b)) => b.clone(),
        _ => Vec::new(),
    };
    if !context_bytes.is_empty() {
        pb::message("eve.loyaltypoints.assetholding.operation.api.Deposited")
            .set_raw_message("context", &context_bytes)
            .encode()
            .map_err(|e| (STATUS_BAD_REQUEST, format!("Invalid LP operation context: {e}")))?;
    }

    Ok(ParsedLpOperation {
        op_uuid,
        owner,
        issuer_corp_id,
        amount,
        context_bytes,
    })
}

fn build_lp_wallet_id(b: ProtoBuilder, owner: LpWalletOwner, issuer_corp_id: u32) -> ProtoBuilder {
    let b = match owner {
        LpWalletOwner::Character(cid) => b.set_with("character", |c| build_u32_id(c, cid)),
        LpWalletOwner::Corporation(corp_id) => b.set_with("corporation", |c| build_u32_id(c, corp_id)),
    };
    b.set_with("issuer", |c| build_u32_id(c, issuer_corp_id))
}

fn encode_lp_op_event(type_name: &str, op: &ParsedLpOperation) -> Vec<u8> {
    pb::message(type_name)
        .set("identifier.uuid", op.op_uuid.as_slice())
        .set_with("operation", |b| {
            b.set_with("wallet", |w| build_lp_wallet_id(w, op.owner, op.issuer_corp_id))
                .set_with("amount", |a| {
                    a.when(op.amount != 0, |a| a.set("amount", op.amount as u64))
                })
        })
        .when(!op.context_bytes.is_empty(), |b| {
            b.set_raw_message("context", &op.context_bytes)
        })
        .must_encode()
}

fn encode_lp_op_ended(op_uuid: &[u8; 16]) -> Vec<u8> {
    pb::message("eve.loyaltypoints.assetholding.operation.api.Ended")
        .set("operation.uuid", op_uuid.as_slice())
        .must_encode()
}

fn op_uuid_hex(uuid: &[u8; 16]) -> String {
    uuid.iter().map(|b| format!("{b:02x}")).collect()
}

// ============================================================================
// ESS & SolarSystem Bounty Notice Emitters (`eve.solarsystem.bounty.*`)
// ============================================================================

pub fn emit_ess_main_bank_primed(
    router: &PublicGatewayRouter,
    sys_id: u32,
    main_cents: IskCents,
    now_unix: u64,
) {
    let any_msg = pb::message("eve.solarsystem.bounty.bank.main.MainBankPrimed")
        .set_with("solar_system", |b| build_u64_id(b, sys_id))
        .set_with("ess_main_value", |b| build_isk_currency(b, main_cents))
        .must_to_any();
    let _ = router.publish_notice(NoticeScope::SolarSystem(sys_id), any_msg, now_unix);
}

pub fn emit_ess_reserve_bank_primed(
    router: &PublicGatewayRouter,
    sys_id: u32,
    reserve_cents: IskCents,
    now_unix: u64,
) {
    let any_msg = pb::message("eve.solarsystem.bounty.bank.reserve.ReserveBankPrimed")
        .set_with("solar_system", |b| build_u64_id(b, sys_id))
        .set_with("ess_reserve_value", |b| build_isk_currency(b, reserve_cents))
        .must_to_any();
    let _ = router.publish_notice(NoticeScope::SolarSystem(sys_id), any_msg, now_unix);
}

pub fn emit_ess_main_bank_link_established(
    router: &PublicGatewayRouter,
    char_id: u32,
    sys_id: u32,
    ship_id: u64,
    main_cents: IskCents,
    reserve_cents: IskCents,
    now_unix: u64,
) {
    let any_msg = pb::message("eve.solarsystem.bounty.bank.main.MainBankLinkEstablished")
        .set_with("linked_character", |b| build_u32_id(b, char_id))
        .set_with("solar_system", |b| build_u64_id(b, sys_id))
        .set_with("ess_main_value", |b| build_isk_currency(b, main_cents))
        .set_with("ess_reserve_value", |b| build_isk_currency(b, reserve_cents))
        .when(ship_id != 0, |b| b.set("ship.sequential", ship_id))
        .must_to_any();
    let _ = router.publish_notice(NoticeScope::SolarSystem(sys_id), any_msg, now_unix);
}

pub fn emit_ess_main_bank_theft_completed(
    router: &PublicGatewayRouter,
    char_id: u32,
    sys_id: u32,
    reserve_cents: IskCents,
    stolen_cents: IskCents,
    destroyed_cents: IskCents,
    now_unix: u64,
) {
    let any_msg = pb::message("eve.solarsystem.bounty.bank.main.MainBankTheftCompleted")
        .set_with("paid_character", |b| build_u32_id(b, char_id))
        .set_with("solar_system", |b| build_u64_id(b, sys_id))
        .set_with("ess_reserve_value", |b| build_isk_currency(b, reserve_cents))
        .set_with("amount_stolen", |b| build_isk_currency(b, stolen_cents))
        .set_with("amount_destroyed", |b| build_isk_currency(b, destroyed_cents))
        .must_to_any();
    let _ = router.publish_notice(NoticeScope::SolarSystem(sys_id), any_msg, now_unix);
}

pub fn emit_ess_solar_system_persisted(
    router: &PublicGatewayRouter,
    sys_id: u32,
    main_cents: IskCents,
    reserve_cents: IskCents,
    now_unix: u64,
) {
    let any_msg = pb::message("eve.solarsystem.bounty.bank.SolarSystemPersisted")
        .set_with("solar_system", |b| build_u64_id(b, sys_id))
        .set_with("ess_main_value", |b| build_isk_currency(b, main_cents))
        .set_with("ess_reserve_value", |b| build_isk_currency(b, reserve_cents))
        .must_to_any();
    let _ = router.publish_notice(NoticeScope::SolarSystem(sys_id), any_msg, now_unix);
}

// ============================================================================
// Handler Registration
// ============================================================================

pub fn register_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    _sde: Arc<eve_sde::SdeRegistry>,
    daily_goals: Arc<DailyGoalManager>,
) {
    // 1. eve.dailygoal.api.admin.RegenerateCurrentGoalsRequest -> RegenerateCurrentGoalsResponse
    let mgr = Arc::clone(&daily_goals);
    router.register_handler(
        "eve.dailygoal.api.admin.RegenerateCurrentGoalsRequest",
        Arc::new(move |ctx, _any, _dyn| {
            require_admin_role(ctx)?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_BAD_REQUEST, "Missing character Identifier".into()));
            };
            let now_unix = ctx.now_unix();
            let day_bucket = daily_goal_day_bucket(now_unix);
            let templates = mgr.get_templates();
            let new_goals: Vec<CharacterDailyGoal> = templates
                .iter()
                .map(|tpl| CharacterDailyGoal {
                    uuid: tpl.uuid,
                    template: tpl.clone(),
                    current_progress: 0,
                    completed: false,
                    claimed: false,
                    completed_at_unix: None,
                    claimed_at_unix: None,
                    paid_completion: false,
                    day_bucket,
                    expired: false,
                })
                .collect();
            {
                let mut map = mgr.character_goals.write().unwrap_or_else(|e| e.into_inner());
                map.insert(cid, new_goals.clone());
            }
            mgr.save_to_db();

            let regen_bytes = encode_period_regenerated(1, now_unix);
            ctx_queue_notice(
                NoticeScope::Character(cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.PeriodRegenerated"),
                    value: regen_bytes,
                },
                now_unix,
            );

            let mut resp = pb::message("eve.dailygoal.api.admin.RegenerateCurrentGoalsResponse");
            for g in &new_goals {
                let (id_bytes, attr_bytes) = goal_id_and_attrs(g);
                let created_bytes = encode_daily_goal_created(g);
                ctx_queue_notice(
                    NoticeScope::Character(cid),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.Created"),
                        value: created_bytes,
                    },
                    now_unix,
                );
                resp = resp.add_with("goals", |b| {
                    b.set_raw_message("id", &id_bytes)
                        .set_raw_message("goal", &attr_bytes)
                });
            }

            Ok(resp.must_to_any())
        }),
    );

    // 2. eve.dailygoal.api.admin.RegenerateCurrentMonthlyGoalsRequest -> STATUS_NOT_IMPLEMENTED
    router.register_handler(
        "eve.dailygoal.api.admin.RegenerateCurrentMonthlyGoalsRequest",
        Arc::new(move |ctx, _any, _dyn| {
            require_admin_role(ctx)?;
            Err((
                STATUS_NOT_IMPLEMENTED,
                "Monthly daily-goal regeneration is not implemented".into(),
            ))
        }),
    );

    // 3. eve.dailygoal.api.admin.ProgressRequest -> ProgressResponse
    let mgr = Arc::clone(&daily_goals);
    let db_prog = Arc::clone(&db);
    router.register_handler(
        "eve.dailygoal.api.admin.ProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let Some(goal_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let target_cid = extract_nested_u32(dyn_msg, 2, 1)
                .or_else(|| ctx.effective_character_id())
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing character Identifier".into()))?;
            let delta = extract_u64_field(dyn_msg, 3).unwrap_or(0);
            if delta == 0 {
                return Err((STATUS_BAD_REQUEST, "Progress delta must be > 0".into()));
            }
            let now_unix = ctx.now_unix();
            let _ = mgr.get_or_init_goals_at(target_cid, now_unix);
            let (prev, curr, just_completed, goal_opt) = {
                let mut map = mgr.character_goals.write().unwrap_or_else(|e| e.into_inner());
                let list = map.get_mut(&target_cid).ok_or_else(|| {
                    (STATUS_NOT_FOUND, "Character daily goals not found".into())
                })?;
                let g = list
                    .iter_mut()
                    .find(|g| g.uuid == goal_uuid && !g.expired)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Daily goal not found".into()))?;
                let prev = g.current_progress;
                let was_completed = g.completed;
                g.current_progress = g
                    .current_progress
                    .saturating_add(delta)
                    .min(g.template.target);
                if g.current_progress >= g.template.target && !g.completed {
                    g.completed = true;
                    g.completed_at_unix = Some(now_unix);
                }
                (prev, g.current_progress, !was_completed && g.completed, g.clone())
            };
            let _ = &db_prog;
            mgr.save_to_db();

            let prog_bytes = encode_daily_goal_progressed(target_cid, &goal_uuid, prev, curr, now_unix);
            ctx_queue_notice(
                NoticeScope::Character(target_cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.Progressed"),
                    value: prog_bytes,
                },
                now_unix,
            );
            if just_completed {
                let comp_bytes = encode_daily_goal_completed(target_cid, &goal_opt, false);
                ctx_queue_notice(
                    NoticeScope::Character(target_cid),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.Completed"),
                        value: comp_bytes,
                    },
                    now_unix,
                );
            }

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.ProgressResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 4. eve.dailygoal.api.admin.ExpireRequest -> ExpireResponse
    let mgr = Arc::clone(&daily_goals);
    router.register_handler(
        "eve.dailygoal.api.admin.ExpireRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            require_admin_role(ctx)?;
            let Some(goal_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let target_cid = extract_nested_u32(dyn_msg, 2, 1)
                .or_else(|| ctx.effective_character_id())
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing character Identifier".into()))?;
            let now_unix = ctx.now_unix();
            let _ = mgr.get_or_init_goals_at(target_cid, now_unix);
            let removed = {
                let mut map = mgr.character_goals.write().unwrap_or_else(|e| e.into_inner());
                if let Some(list) = map.get_mut(&target_cid) {
                    if let Some(g) = list.iter_mut().find(|g| g.uuid == goal_uuid && !g.expired) {
                        g.expired = true;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            };
            if !removed {
                return Err((STATUS_NOT_FOUND, "Daily goal not found".into()));
            }
            mgr.save_to_db();

            let deact_bytes = encode_daily_goal_prematurely_deactivated(&goal_uuid, now_unix);
            ctx_queue_notice(
                NoticeScope::Character(target_cid),
                GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.PrematurelyDeactivated"
                    ),
                    value: deact_bytes,
                },
                now_unix,
            );
            let rem_bytes = encode_daily_goal_removed(&goal_uuid);
            ctx_queue_notice(
                NoticeScope::Character(target_cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.Removed"),
                    value: rem_bytes,
                },
                now_unix,
            );

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.ExpireResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 5. eve.dailygoal.api.admin.AutoRedeemRequest -> AutoRedeemResponse (with Redeemed event)
    let mgr = Arc::clone(&daily_goals);
    let db_auto = Arc::clone(&db);
    router.register_handler(
        "eve.dailygoal.api.admin.AutoRedeemRequest",
        Arc::new(move |ctx, _any, _dyn| {
            require_admin_role(ctx)?;
            let now_unix = ctx.now_unix();
            if let Some(cid) = ctx.effective_character_id() {
                let goals = mgr.get_or_init_goals_at(cid, now_unix);
                for g in goals.iter().filter(|g| g.completed && !g.claimed) {
                    if mgr.redeem_goal(cid, &g.uuid, now_unix, &db_auto).is_ok() {
                        let red_bytes = encode_daily_goal_redeemed(cid, g, true);
                        ctx_queue_notice(
                            NoticeScope::Character(cid),
                            GoogleAny {
                                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.Redeemed"),
                                value: red_bytes,
                            },
                            now_unix,
                        );
                    }
                }
                mgr.save_to_db();
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.AutoRedeemResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 6. eve.loyaltypoints.assetholding.operation.api.DepositRequest -> DepositResponse
    let db_dep = Arc::clone(&db);
    router.register_handler(
        "eve.loyaltypoints.assetholding.operation.api.DepositRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let op = parse_lp_operation_request(dyn_msg, ctx.effective_character_id())?;
            let hex = op_uuid_hex(&op.op_uuid);
            let now_unix = ctx.now_unix();

            let (new_balance, already_done): (u64, bool) = db_dep
                .try_with_mut_snapshot(|snap| -> Result<(u64, bool), (u32, String)> {
                    let already_done = snap
                        .quasar_commerce
                        .lp_asset_ops_completed
                        .contains(&format!("dep:{hex}"));

                    let bal = match op.owner {
                        LpWalletOwner::Character(cid) => {
                            let (new_char_bal, corp_id) = {
                                let ch = snap.characters.get_mut(&i64::from(cid)).ok_or_else(|| {
                                    (STATUS_NOT_FOUND, format!("character {cid} not found"))
                                })?;
                                let entry = ch.loyalty_points.entry(i64::from(op.issuer_corp_id)).or_insert(0);
                                if !already_done {
                                    *entry = entry
                                        .checked_add(op.amount)
                                        .ok_or_else(|| (STATUS_BAD_REQUEST, "LP balance overflow".into()))?;
                                }
                                (*entry, ch.corporation_id)
                            };
                            if !already_done
                                && op.amount > 0
                                && i64::from(op.issuer_corp_id) == crate::services::lp_store::PARAGON_CORP_ID
                                && corp_id >= 98_000_000
                            {
                                let corp_entry = snap
                                    .corp_lp_balances
                                    .entry((corp_id, crate::services::lp_store::PARAGON_CORP_ID))
                                    .or_insert(0);
                                *corp_entry = corp_entry.saturating_add(op.amount);
                            }
                            new_char_bal.max(0) as u64
                        }
                        LpWalletOwner::Corporation(corp_id) => {
                            let entry = snap
                                .corp_lp_balances
                                .entry((i64::from(corp_id), i64::from(op.issuer_corp_id)))
                                .or_insert(0);
                            if !already_done {
                                *entry = entry
                                    .checked_add(op.amount)
                                    .ok_or_else(|| (STATUS_BAD_REQUEST, "LP balance overflow".into()))?;
                            }
                            (*entry).max(0) as u64
                        }
                    };

                    if !already_done {
                        snap.quasar_commerce
                            .lp_asset_ops_completed
                            .insert(format!("dep:{hex}"));
                    }
                    Ok((bal, already_done))
                })?;

            if !already_done {
                let scope = match op.owner {
                    LpWalletOwner::Character(cid) => NoticeScope::Character(cid),
                    LpWalletOwner::Corporation(corp_id) => NoticeScope::Corporation(corp_id),
                };

                ctx_queue_notice(
                    scope.clone(),
                    pb::message("eve.loyaltypoints.wallet.api.BalanceAdded")
                        .set_with("wallet", |w| build_lp_wallet_id(w, op.owner, op.issuer_corp_id))
                        .set("increase", op.amount as u64)
                        .set("new_balance", new_balance)
                        .set("taxed_amount", 0u64)
                        .must_to_any(),
                    now_unix,
                );

                ctx_queue_notice(
                    scope.clone(),
                    GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.assetholding.operation.api.Deposited"
                        ),
                        value: encode_lp_op_event(
                            "eve.loyaltypoints.assetholding.operation.api.Deposited",
                            &op,
                        ),
                    },
                    now_unix,
                );

                ctx_queue_notice(
                    scope,
                    GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.assetholding.operation.api.Ended"
                        ),
                        value: encode_lp_op_ended(&op.op_uuid),
                    },
                    now_unix,
                );
            }

            Ok(pb::message("eve.loyaltypoints.assetholding.operation.api.DepositResponse")
                .must_to_any())
        }),
    );

    // 7. eve.loyaltypoints.assetholding.operation.api.WithdrawRequest -> WithdrawResponse
    let db_wd = Arc::clone(&db);
    router.register_handler(
        "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let op = parse_lp_operation_request(dyn_msg, ctx.effective_character_id())?;
            let hex = op_uuid_hex(&op.op_uuid);
            let now_unix = ctx.now_unix();

            let (new_balance, already_done): (u64, bool) = db_wd
                .try_with_mut_snapshot(|snap| -> Result<(u64, bool), (u32, String)> {
                    let already_done = snap
                        .quasar_commerce
                        .lp_asset_ops_completed
                        .contains(&format!("wd:{hex}"));

                    let bal = match op.owner {
                        LpWalletOwner::Character(cid) => {
                            let ch = snap.characters.get_mut(&i64::from(cid)).ok_or_else(|| {
                                (STATUS_NOT_FOUND, format!("character {cid} not found"))
                            })?;
                            let entry = ch.loyalty_points.entry(i64::from(op.issuer_corp_id)).or_insert(0);
                            if !already_done {
                                if (*entry).max(0) < op.amount {
                                    return Err((
                                        STATUS_PRECONDITION_FAILED,
                                        "Insufficient loyalty points balance".into(),
                                    ));
                                }
                                *entry = entry.checked_sub(op.amount).ok_or_else(|| {
                                    (
                                        STATUS_PRECONDITION_FAILED,
                                        "Insufficient loyalty points balance".into(),
                                    )
                                })?;
                            }
                            (*entry).max(0) as u64
                        }
                        LpWalletOwner::Corporation(corp_id) => {
                            let entry = snap
                                .corp_lp_balances
                                .entry((i64::from(corp_id), i64::from(op.issuer_corp_id)))
                                .or_insert(0);
                            if !already_done {
                                if (*entry).max(0) < op.amount {
                                    return Err((
                                        STATUS_PRECONDITION_FAILED,
                                        "Insufficient corporation loyalty points balance".into(),
                                    ));
                                }
                                *entry = entry.checked_sub(op.amount).ok_or_else(|| {
                                    (
                                        STATUS_PRECONDITION_FAILED,
                                        "Insufficient corporation loyalty points balance".into(),
                                    )
                                })?;
                            }
                            (*entry).max(0) as u64
                        }
                    };

                    if !already_done {
                        snap.quasar_commerce
                            .lp_asset_ops_completed
                            .insert(format!("wd:{hex}"));
                    }
                    Ok((bal, already_done))
                })?;

            if !already_done {
                let scope = match op.owner {
                    LpWalletOwner::Character(cid) => NoticeScope::Character(cid),
                    LpWalletOwner::Corporation(corp_id) => NoticeScope::Corporation(corp_id),
                };

                ctx_queue_notice(
                    scope.clone(),
                    pb::message("eve.loyaltypoints.wallet.api.BalanceRemoved")
                        .set_with("wallet", |w| build_lp_wallet_id(w, op.owner, op.issuer_corp_id))
                        .set("decrease", op.amount as u64)
                        .set("new_balance", new_balance)
                        .must_to_any(),
                    now_unix,
                );

                ctx_queue_notice(
                    scope.clone(),
                    GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.assetholding.operation.api.Withdrawn"
                        ),
                        value: encode_lp_op_event(
                            "eve.loyaltypoints.assetholding.operation.api.Withdrawn",
                            &op,
                        ),
                    },
                    now_unix,
                );

                ctx_queue_notice(
                    scope,
                    GoogleAny {
                        type_url: format!(
                            "{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.assetholding.operation.api.Ended"
                        ),
                        value: encode_lp_op_ended(&op.op_uuid),
                    },
                    now_unix,
                );
            }

            Ok(pb::message("eve.loyaltypoints.assetholding.operation.api.WithdrawResponse")
                .must_to_any())
        }),
    );

    // 8. eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest -> ApplyMultiplierFactorResponse
    let db_bounty = Arc::clone(&db);
    router.register_handler(
        "eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let sys_id = extract_nested_u32(dyn_msg, 1, 1)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing solar_system Identifier".into()))?;
            if sys_id == 0 {
                return Err((STATUS_BAD_REQUEST, "Invalid solar_system_id 0".into()));
            }
            let input_pct = extract_u32_field(dyn_msg, 2).unwrap_or(100);
            let (units, nanos) = if let Some(DynamicValue::Message(det)) = dyn_msg.get(3) {
                (
                    extract_u64_field(det, 1).unwrap_or(0),
                    extract_u32_field(det, 2).unwrap_or(0),
                )
            } else {
                let u = u64::from(input_pct / 100);
                let n = (input_pct % 100) * 10_000_000;
                (u, n)
            };
            let mult = (units as f64) + (f64::from(nanos) / 1_000_000_000.0);
            if !(mult.is_finite() && (0.0..=10.0).contains(&mult)) {
                return Err((STATUS_BAD_REQUEST, "Multiplier out of valid range [0.0, 10.0]".into()));
            }
            let now_unix = ctx.now_unix();

            let (main_cents, reserve_cents) = db_bounty
                .try_with_mut_snapshot(|snap| -> Result<(IskCents, IskCents), (u32, String)> {
                    let st = snap
                        .system_states
                        .entry(sys_id)
                        .or_insert_with(|| eve_db::SystemDynamicState {
                            solar_system_id: sys_id,
                            dbs_multiplier: mult,
                            ..Default::default()
                        });
                    st.dbs_multiplier = mult;
                    Ok((st.ess_main_bank, st.ess_reserve_bank))
                })?;

            ctx_queue_notice(
                NoticeScope::SolarSystem(sys_id),
                pb::message("eve.solarsystem.bounty.npc.MultiplierFactorCalculated")
                    .set_with("solar_system", |b| build_u64_id(b, sys_id))
                    .set("input_multiplier_percentage_value", input_pct)
                    .set_with("calculated_multiplier", |b| {
                        b.when(units != 0, |b| b.set("units", units))
                            .when(nanos != 0, |b| b.set("nanos", nanos))
                    })
                    .must_to_any(),
                now_unix,
            );

            ctx_queue_notice(
                NoticeScope::SolarSystem(sys_id),
                pb::message("eve.solarsystem.bounty.bank.SolarSystemPersisted")
                    .set_with("solar_system", |b| build_u64_id(b, sys_id))
                    .set_with("ess_main_value", |b| build_isk_currency(b, main_cents))
                    .set_with("ess_reserve_value", |b| build_isk_currency(b, reserve_cents))
                    .must_to_any(),
                now_unix,
            );

            Ok(pb::message("eve.solarsystem.bounty.npc.ApplyMultiplierFactorResponse")
                .must_to_any())
        }),
    );
}

#[cfg(test)]
#[path = "quasar_fleet_navigation_services_tests.rs"]
mod quasar_fleet_navigation_services_tests;

