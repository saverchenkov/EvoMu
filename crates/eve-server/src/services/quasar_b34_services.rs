//! Batch 34 Quasar Gateway Services:
//! - `eve.dailygoal.api` & `eve.dailygoal.api.admin` (`requests_pb2`, `api_pb2`, `events_pb2`)
//! - `eve.loyaltypoints.wallet` & `eve.loyaltypoints.assetholding.operation.api` (`wallet_pb2`, `api_pb2`, `operation_pb2`, `requests_pb2`, `events_pb2`)
//! - `eve.solarsystem.bounty.npc` & `eve.solarsystem.bounty.bank.*` (`npc_pb2`, `bank_pb2`, `mainbank_pb2`, `reservebank_pb2`)

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    require_admin_role, NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST,
    STATUS_FORBIDDEN, STATUS_NOT_FOUND, STATUS_PRECONDITION_FAILED,
};
use crate::services::quasar_daily_goals::{
    encode_goal_attributes, encode_goal_earnings, encode_goal_identifier, extract_uuid,
    CharacterDailyGoal, DailyGoalManager,
};
use crate::services::quasar_social_chat::ctx_queue_notice;
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, DynamicQuasarMessage, DynamicValue,
    GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

const LP_OPS_BLOB_KEY: &str = "b34_lp_assetholding_ops";
const CORP_LP_BLOB_KEY: &str = "b34_corp_loyalty_points";

// ============================================================================
// Wire Encoding & Decoding Helpers
// ============================================================================

fn zigzag_encode_i64(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn zigzag_encode_i32(v: i32) -> u64 {
    ((v << 1) ^ (v >> 31)) as u64
}

fn encode_isk_currency(cents: IskCents, out: &mut Vec<u8>) {
    let units = cents.0 / 100;
    let nanos = ((cents.0 % 100) * 10_000_000) as i32;
    if units != 0 {
        encode_varint_field(1, zigzag_encode_i64(units), out);
    }
    if nanos != 0 {
        encode_varint_field(2, zigzag_encode_i32(nanos), out);
    }
}

fn encode_sequential_id(id: u32, out: &mut Vec<u8>) {
    if id != 0 {
        encode_varint_field(1, u64::from(id), out);
    }
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

fn encode_timestamp(unix_secs: u64, out: &mut Vec<u8>) {
    let ts = GoogleTimestamp {
        seconds: unix_secs as i64,
        nanos: 0,
    };
    out.extend_from_slice(&ts.encode_proto());
}

// ============================================================================
// Daily Goal Event Encoders (`eve.dailygoal.api.*`)
// ============================================================================

fn encode_daily_goal_created(goal: &CharacterDailyGoal) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_goal_identifier(&goal.uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    let mut attr_bytes = Vec::new();
    encode_goal_attributes(&goal.template, &mut attr_bytes);
    encode_bytes_field(2, &attr_bytes, &mut out);
    out
}

fn encode_daily_goal_prematurely_deactivated(uuid: &[u8; 16], now_unix: u64) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_goal_identifier(uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    let mut ts_bytes = Vec::new();
    encode_timestamp(now_unix, &mut ts_bytes);
    encode_bytes_field(2, &ts_bytes, &mut out);
    out
}

fn encode_daily_goal_removed(uuid: &[u8; 16]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_goal_identifier(uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    out
}

fn encode_daily_goal_progressed(
    cid: u32,
    uuid: &[u8; 16],
    prev: u64,
    curr: u64,
    now_unix: u64,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut ts_bytes = Vec::new();
    encode_timestamp(now_unix, &mut ts_bytes);
    encode_bytes_field(1, &ts_bytes, &mut out);
    let mut char_bytes = Vec::new();
    encode_sequential_id(cid, &mut char_bytes);
    encode_bytes_field(2, &char_bytes, &mut out);
    let mut id_bytes = Vec::new();
    encode_goal_identifier(uuid, &mut id_bytes);
    encode_bytes_field(3, &id_bytes, &mut out);
    encode_varint_field(4, prev, &mut out);
    encode_varint_field(5, curr, &mut out);
    out
}

fn encode_daily_goal_completed(cid: u32, goal: &CharacterDailyGoal, paid: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut char_bytes = Vec::new();
    encode_sequential_id(cid, &mut char_bytes);
    encode_bytes_field(1, &char_bytes, &mut out);
    let mut id_bytes = Vec::new();
    encode_goal_identifier(&goal.uuid, &mut id_bytes);
    encode_bytes_field(2, &id_bytes, &mut out);
    encode_varint_field(4, 1, &mut out); // CATEGORY_DAILY = 1
    encode_goal_earnings(&goal.template, 5, &mut out);
    if paid {
        encode_varint_field(6, 1, &mut out);
    }
    out
}

fn encode_daily_goal_redeemed(cid: u32, goal: &CharacterDailyGoal, autoredeemed: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut char_bytes = Vec::new();
    encode_sequential_id(cid, &mut char_bytes);
    encode_bytes_field(1, &char_bytes, &mut out);
    let mut id_bytes = Vec::new();
    encode_goal_identifier(&goal.uuid, &mut id_bytes);
    encode_bytes_field(2, &id_bytes, &mut out);
    encode_goal_earnings(&goal.template, 3, &mut out);
    if autoredeemed {
        encode_varint_field(4, 1, &mut out);
    }
    out
}

fn encode_period_regenerated(category: u64, now_unix: u64) -> Vec<u8> {
    let mut period_bytes = Vec::new();
    let mut start_ts = Vec::new();
    encode_timestamp(now_unix, &mut start_ts);
    encode_bytes_field(1, &start_ts, &mut period_bytes);
    let mut end_ts = Vec::new();
    let duration = if category == 2 { 30 * 86_400 } else { 86_400 };
    encode_timestamp(now_unix.saturating_add(duration), &mut end_ts);
    encode_bytes_field(2, &end_ts, &mut period_bytes);
    encode_varint_field(3, category, &mut period_bytes);

    let mut out = Vec::new();
    encode_bytes_field(1, &period_bytes, &mut out);
    encode_bytes_field(2, &start_ts, &mut out);
    out
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
    amount: u64,
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

    let amount = if let Some(DynamicValue::Message(curr)) = op_attr.get(2) {
        extract_u64_field(curr, 1).unwrap_or(0)
    } else {
        0
    };
    if amount == 0 {
        return Err((STATUS_BAD_REQUEST, "LP operation amount must be > 0".into()));
    }

    let context_bytes = match dyn_msg.get(3) {
        Some(DynamicValue::Bytes(b)) => b.clone(),
        _ => Vec::new(),
    };

    Ok(ParsedLpOperation {
        op_uuid,
        owner,
        issuer_corp_id,
        amount,
        context_bytes,
    })
}

fn encode_lp_wallet_id(owner: LpWalletOwner, issuer_corp_id: u32, out: &mut Vec<u8>) {
    match owner {
        LpWalletOwner::Character(cid) => {
            let mut c_bytes = Vec::new();
            encode_sequential_id(cid, &mut c_bytes);
            encode_bytes_field(1, &c_bytes, out);
        }
        LpWalletOwner::Corporation(corp_id) => {
            let mut c_bytes = Vec::new();
            encode_sequential_id(corp_id, &mut c_bytes);
            encode_bytes_field(2, &c_bytes, out);
        }
    }
    let mut issuer_bytes = Vec::new();
    encode_sequential_id(issuer_corp_id, &mut issuer_bytes);
    encode_bytes_field(3, &issuer_bytes, out);
}

fn encode_lp_op_attributes(op: &ParsedLpOperation, out: &mut Vec<u8>) {
    let mut w_bytes = Vec::new();
    encode_lp_wallet_id(op.owner, op.issuer_corp_id, &mut w_bytes);
    encode_bytes_field(1, &w_bytes, out);
    let mut curr_bytes = Vec::new();
    encode_varint_field(1, op.amount, &mut curr_bytes);
    encode_bytes_field(2, &curr_bytes, out);
}

fn encode_lp_op_event(op: &ParsedLpOperation) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &op.op_uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    let mut attr_bytes = Vec::new();
    encode_lp_op_attributes(op, &mut attr_bytes);
    encode_bytes_field(2, &attr_bytes, &mut out);
    if !op.context_bytes.is_empty() {
        encode_bytes_field(3, &op.context_bytes, &mut out);
    }
    out
}

fn encode_lp_op_ended(op_uuid: &[u8; 16]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, op_uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, &mut out);
    out
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
    let mut out = Vec::new();
    let mut sys_bytes = Vec::new();
    encode_sequential_id(sys_id, &mut sys_bytes);
    encode_bytes_field(1, &sys_bytes, &mut out);
    let mut main_bytes = Vec::new();
    encode_isk_currency(main_cents, &mut main_bytes);
    encode_bytes_field(2, &main_bytes, &mut out);
    let _ = router.publish_notice(
        NoticeScope::SolarSystem(sys_id),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.solarsystem.bounty.bank.main.MainBankPrimed"),
            value: out,
        },
        now_unix,
    );
}

pub fn emit_ess_reserve_bank_primed(
    router: &PublicGatewayRouter,
    sys_id: u32,
    reserve_cents: IskCents,
    now_unix: u64,
) {
    let mut out = Vec::new();
    let mut sys_bytes = Vec::new();
    encode_sequential_id(sys_id, &mut sys_bytes);
    encode_bytes_field(1, &sys_bytes, &mut out);
    let mut res_bytes = Vec::new();
    encode_isk_currency(reserve_cents, &mut res_bytes);
    encode_bytes_field(2, &res_bytes, &mut out);
    let _ = router.publish_notice(
        NoticeScope::SolarSystem(sys_id),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.solarsystem.bounty.bank.reserve.ReserveBankPrimed"),
            value: out,
        },
        now_unix,
    );
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
    let mut out = Vec::new();
    let mut c_bytes = Vec::new();
    encode_sequential_id(char_id, &mut c_bytes);
    encode_bytes_field(1, &c_bytes, &mut out);
    let mut s_bytes = Vec::new();
    encode_sequential_id(sys_id, &mut s_bytes);
    encode_bytes_field(2, &s_bytes, &mut out);
    let mut m_bytes = Vec::new();
    encode_isk_currency(main_cents, &mut m_bytes);
    encode_bytes_field(3, &m_bytes, &mut out);
    let mut r_bytes = Vec::new();
    encode_isk_currency(reserve_cents, &mut r_bytes);
    encode_bytes_field(4, &r_bytes, &mut out);
    if ship_id != 0 {
        let mut sh_bytes = Vec::new();
        encode_varint_field(1, ship_id, &mut sh_bytes);
        encode_bytes_field(5, &sh_bytes, &mut out);
    }
    let _ = router.publish_notice(
        NoticeScope::SolarSystem(sys_id),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.solarsystem.bounty.bank.main.MainBankLinkEstablished"),
            value: out,
        },
        now_unix,
    );
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
    let mut out = Vec::new();
    let mut c_bytes = Vec::new();
    encode_sequential_id(char_id, &mut c_bytes);
    encode_bytes_field(1, &c_bytes, &mut out);
    let mut s_bytes = Vec::new();
    encode_sequential_id(sys_id, &mut s_bytes);
    encode_bytes_field(2, &s_bytes, &mut out);
    let mut r_bytes = Vec::new();
    encode_isk_currency(reserve_cents, &mut r_bytes);
    encode_bytes_field(3, &r_bytes, &mut out);
    let mut st_bytes = Vec::new();
    encode_isk_currency(stolen_cents, &mut st_bytes);
    encode_bytes_field(4, &st_bytes, &mut out);
    let mut d_bytes = Vec::new();
    encode_isk_currency(destroyed_cents, &mut d_bytes);
    encode_bytes_field(5, &d_bytes, &mut out);
    let _ = router.publish_notice(
        NoticeScope::SolarSystem(sys_id),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.solarsystem.bounty.bank.main.MainBankTheftCompleted"),
            value: out,
        },
        now_unix,
    );
}

pub fn emit_ess_solar_system_persisted(
    router: &PublicGatewayRouter,
    sys_id: u32,
    main_cents: IskCents,
    reserve_cents: IskCents,
    now_unix: u64,
) {
    let mut out = Vec::new();
    let mut s_bytes = Vec::new();
    encode_sequential_id(sys_id, &mut s_bytes);
    encode_bytes_field(1, &s_bytes, &mut out);
    let mut m_bytes = Vec::new();
    encode_isk_currency(main_cents, &mut m_bytes);
    encode_bytes_field(2, &m_bytes, &mut out);
    let mut r_bytes = Vec::new();
    encode_isk_currency(reserve_cents, &mut r_bytes);
    encode_bytes_field(3, &r_bytes, &mut out);
    let _ = router.publish_notice(
        NoticeScope::SolarSystem(sys_id),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.solarsystem.bounty.bank.SolarSystemPersisted"),
            value: out,
        },
        now_unix,
    );
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
            let cid = ctx.effective_character_id().unwrap_or(1);
            let now_unix = ctx.now_unix();
            let templates = mgr.get_templates();
            let mut new_goals = Vec::new();
            for (idx, tpl) in templates.iter().take(4).enumerate() {
                let mut uuid = [0u8; 16];
                uuid[0..8].copy_from_slice(&now_unix.to_be_bytes());
                uuid[8..12].copy_from_slice(&cid.to_be_bytes());
                uuid[12..16].copy_from_slice(&(idx as u32 + 1).to_be_bytes());
                new_goals.push(CharacterDailyGoal {
                    uuid,
                    template: tpl.clone(),
                    current_progress: 0,
                    completed: false,
                    claimed: false,
                    completed_at_unix: None,
                    claimed_at_unix: None,
                    paid_completion: false,
                    day_bucket: now_unix / 86_400,
                });
            }
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

            let mut out = Vec::new();
            for g in &new_goals {
                let created_bytes = encode_daily_goal_created(g);
                ctx_queue_notice(
                    NoticeScope::Character(cid),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.Created"),
                        value: created_bytes.clone(),
                    },
                    now_unix,
                );
                encode_bytes_field(1, &created_bytes, &mut out);
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.RegenerateCurrentGoalsResponse"
                ),
                value: out,
            })
        }),
    );

    // 2. eve.dailygoal.api.admin.RegenerateCurrentMonthlyGoalsRequest -> RegenerateCurrentMonthlyGoalsResponse
    let mgr = Arc::clone(&daily_goals);
    router.register_handler(
        "eve.dailygoal.api.admin.RegenerateCurrentMonthlyGoalsRequest",
        Arc::new(move |ctx, _any, _dyn| {
            require_admin_role(ctx)?;
            let cid = ctx.effective_character_id().unwrap_or(1);
            let now_unix = ctx.now_unix();
            let goals = mgr.get_or_init_goals_at(cid, now_unix);

            let regen_bytes = encode_period_regenerated(2, now_unix);
            ctx_queue_notice(
                NoticeScope::Character(cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.PeriodRegenerated"),
                    value: regen_bytes,
                },
                now_unix,
            );

            let mut out = Vec::new();
            for g in &goals {
                let created_bytes = encode_daily_goal_created(g);
                encode_bytes_field(1, &created_bytes, &mut out);
            }

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.RegenerateCurrentMonthlyGoalsResponse"
                ),
                value: out,
            })
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
                let g = list.iter_mut().find(|g| g.uuid == goal_uuid).ok_or_else(|| {
                    (STATUS_NOT_FOUND, "Daily goal not found".into())
                })?;
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
                    let before = list.len();
                    list.retain(|g| g.uuid != goal_uuid);
                    list.len() < before
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
            if ctx.effective_character_id().is_none() && require_admin_role(ctx).is_err() {
                return Err((
                    STATUS_FORBIDDEN,
                    "Authentication required for loyaltypoints assetholding DepositRequest".into(),
                ));
            }
            let op = parse_lp_operation_request(dyn_msg, ctx.effective_character_id())?;
            let hex = op_uuid_hex(&op.op_uuid);
            let now_unix = ctx.now_unix();

            let new_balance: u64 = db_dep
                .try_with_mut_snapshot(|snap| -> Result<u64, (u32, String)> {
                    let mut ops: HashSet<String> = snap
                        .service_state_blobs
                        .get(LP_OPS_BLOB_KEY)
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default();
                    let already_done = !ops.insert(format!("dep:{hex}"));
                    if let Ok(json) = serde_json::to_string(&ops) {
                        snap.service_state_blobs.insert(LP_OPS_BLOB_KEY.into(), json);
                    }

                    match op.owner {
                        LpWalletOwner::Character(cid) => {
                            let ch = snap.characters.get_mut(&i64::from(cid)).ok_or_else(|| {
                                (STATUS_NOT_FOUND, format!("character {cid} not found"))
                            })?;
                            let entry = ch.loyalty_points.entry(i64::from(op.issuer_corp_id)).or_insert(0);
                            if !already_done {
                                *entry = entry.saturating_add(op.amount as i64);
                            }
                            Ok((*entry).max(0) as u64)
                        }
                        LpWalletOwner::Corporation(corp_id) => {
                            let mut corp_lp: HashMap<String, u64> = snap
                                .service_state_blobs
                                .get(CORP_LP_BLOB_KEY)
                                .and_then(|s| serde_json::from_str(s).ok())
                                .unwrap_or_default();
                            let key = format!("{corp_id}:{}", op.issuer_corp_id);
                            let entry = corp_lp.entry(key).or_insert(0);
                            if !already_done {
                                *entry = entry.saturating_add(op.amount);
                            }
                            let bal = *entry;
                            if let Ok(json) = serde_json::to_string(&corp_lp) {
                                snap.service_state_blobs.insert(CORP_LP_BLOB_KEY.into(), json);
                            }
                            Ok(bal)
                        }
                    }
                })?;

            let scope = match op.owner {
                LpWalletOwner::Character(cid) => NoticeScope::Character(cid),
                LpWalletOwner::Corporation(corp_id) => NoticeScope::Corporation(corp_id),
            };

            let mut added_bytes = Vec::new();
            let mut w_bytes = Vec::new();
            encode_lp_wallet_id(op.owner, op.issuer_corp_id, &mut w_bytes);
            encode_bytes_field(1, &w_bytes, &mut added_bytes);
            encode_varint_field(2, op.amount, &mut added_bytes);
            encode_varint_field(3, new_balance, &mut added_bytes);
            encode_varint_field(4, 0, &mut added_bytes);
            ctx_queue_notice(
                scope.clone(),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.wallet.api.BalanceAdded"),
                    value: added_bytes,
                },
                now_unix,
            );

            ctx_queue_notice(
                scope.clone(),
                GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.assetholding.operation.api.Deposited"
                    ),
                    value: encode_lp_op_event(&op),
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

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.assetholding.operation.api.DepositResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 7. eve.loyaltypoints.assetholding.operation.api.WithdrawRequest -> WithdrawResponse
    let db_wd = Arc::clone(&db);
    router.register_handler(
        "eve.loyaltypoints.assetholding.operation.api.WithdrawRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            if ctx.effective_character_id().is_none() && require_admin_role(ctx).is_err() {
                return Err((
                    STATUS_FORBIDDEN,
                    "Authentication required for loyaltypoints assetholding WithdrawRequest".into(),
                ));
            }
            let op = parse_lp_operation_request(dyn_msg, ctx.effective_character_id())?;
            let hex = op_uuid_hex(&op.op_uuid);
            let now_unix = ctx.now_unix();

            let new_balance: u64 = db_wd
                .try_with_mut_snapshot(|snap| -> Result<u64, (u32, String)> {
                    let mut ops: HashSet<String> = snap
                        .service_state_blobs
                        .get(LP_OPS_BLOB_KEY)
                        .and_then(|s| serde_json::from_str(s).ok())
                        .unwrap_or_default();
                    let already_done = ops.contains(&format!("wd:{hex}"));

                    let bal = match op.owner {
                        LpWalletOwner::Character(cid) => {
                            let ch = snap.characters.get_mut(&i64::from(cid)).ok_or_else(|| {
                                (STATUS_NOT_FOUND, format!("character {cid} not found"))
                            })?;
                            let entry = ch.loyalty_points.entry(i64::from(op.issuer_corp_id)).or_insert(0);
                            if !already_done {
                                if (*entry).max(0) < op.amount as i64 {
                                    return Err((
                                        STATUS_PRECONDITION_FAILED,
                                        "Insufficient loyalty points balance".into(),
                                    ));
                                }
                                *entry -= op.amount as i64;
                            }
                            (*entry).max(0) as u64
                        }
                        LpWalletOwner::Corporation(corp_id) => {
                            let mut corp_lp: HashMap<String, u64> = snap
                                .service_state_blobs
                                .get(CORP_LP_BLOB_KEY)
                                .and_then(|s| serde_json::from_str(s).ok())
                                .unwrap_or_default();
                            let key = format!("{corp_id}:{}", op.issuer_corp_id);
                            let entry = corp_lp.entry(key).or_insert(0);
                            if !already_done {
                                if *entry < op.amount {
                                    return Err((
                                        STATUS_PRECONDITION_FAILED,
                                        "Insufficient corporation loyalty points balance".into(),
                                    ));
                                }
                                *entry -= op.amount;
                            }
                            let bal = *entry;
                            if let Ok(json) = serde_json::to_string(&corp_lp) {
                                snap.service_state_blobs.insert(CORP_LP_BLOB_KEY.into(), json);
                            }
                            bal
                        }
                    };

                    if !already_done {
                        ops.insert(format!("wd:{hex}"));
                        if let Ok(json) = serde_json::to_string(&ops) {
                            snap.service_state_blobs.insert(LP_OPS_BLOB_KEY.into(), json);
                        }
                    }
                    Ok(bal)
                })?;

            let scope = match op.owner {
                LpWalletOwner::Character(cid) => NoticeScope::Character(cid),
                LpWalletOwner::Corporation(corp_id) => NoticeScope::Corporation(corp_id),
            };

            let mut rem_bytes = Vec::new();
            let mut w_bytes = Vec::new();
            encode_lp_wallet_id(op.owner, op.issuer_corp_id, &mut w_bytes);
            encode_bytes_field(1, &w_bytes, &mut rem_bytes);
            encode_varint_field(2, op.amount, &mut rem_bytes);
            encode_varint_field(3, new_balance, &mut rem_bytes);
            ctx_queue_notice(
                scope.clone(),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.wallet.api.BalanceRemoved"),
                    value: rem_bytes,
                },
                now_unix,
            );

            ctx_queue_notice(
                scope.clone(),
                GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.assetholding.operation.api.Withdrawn"
                    ),
                    value: encode_lp_op_event(&op),
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

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.loyaltypoints.assetholding.operation.api.WithdrawResponse"
                ),
                value: Vec::new(),
            })
        }),
    );

    // 8. eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest -> ApplyMultiplierFactorResponse
    let db_bounty = Arc::clone(&db);
    router.register_handler(
        "eve.solarsystem.bounty.npc.ApplyMultiplierFactorRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            if ctx.effective_character_id().is_none() && require_admin_role(ctx).is_err() {
                return Err((
                    STATUS_FORBIDDEN,
                    "Authentication required for ApplyMultiplierFactorRequest".into(),
                ));
            }
            let sys_id = extract_nested_u32(dyn_msg, 1, 1)
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing solar_system Identifier".into()))?;
            if sys_id == 0 {
                return Err((STATUS_BAD_REQUEST, "Invalid solar_system_id 0".into()));
            }
            let input_pct = extract_u32_field(dyn_msg, 2).unwrap_or(100);
            let (units, nanos) = if let Some(DynamicValue::Message(det)) = dyn_msg.get(3) {
                (
                    extract_u64_field(det, 1).unwrap_or(1),
                    extract_u32_field(det, 2).unwrap_or(0),
                )
            } else {
                let u = u64::from(input_pct / 100);
                let n = (input_pct % 100) * 10_000_000;
                (u, n)
            };
            let mult = (units as f64) + (f64::from(nanos) / 1_000_000_000.0);
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

            let mut calc_bytes = Vec::new();
            let mut s_bytes = Vec::new();
            encode_sequential_id(sys_id, &mut s_bytes);
            encode_bytes_field(1, &s_bytes, &mut calc_bytes);
            encode_varint_field(2, u64::from(input_pct), &mut calc_bytes);
            let mut det_bytes = Vec::new();
            if units != 0 {
                encode_varint_field(1, units, &mut det_bytes);
            }
            if nanos != 0 {
                encode_varint_field(2, u64::from(nanos), &mut det_bytes);
            }
            encode_bytes_field(3, &det_bytes, &mut calc_bytes);

            ctx_queue_notice(
                NoticeScope::SolarSystem(sys_id),
                GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve.solarsystem.bounty.npc.MultiplierFactorCalculated"
                    ),
                    value: calc_bytes,
                },
                now_unix,
            );

            let mut pers_bytes = Vec::new();
            encode_bytes_field(1, &s_bytes, &mut pers_bytes);
            let mut m_bytes = Vec::new();
            encode_isk_currency(main_cents, &mut m_bytes);
            encode_bytes_field(2, &m_bytes, &mut pers_bytes);
            let mut r_bytes = Vec::new();
            encode_isk_currency(reserve_cents, &mut r_bytes);
            encode_bytes_field(3, &r_bytes, &mut pers_bytes);
            ctx_queue_notice(
                NoticeScope::SolarSystem(sys_id),
                GoogleAny {
                    type_url: format!(
                        "{TYPE_GOOGLEAPIS_PREFIX}eve.solarsystem.bounty.bank.SolarSystemPersisted"
                    ),
                    value: pers_bytes,
                },
                now_unix,
            );

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.solarsystem.bounty.npc.ApplyMultiplierFactorResponse"
                ),
                value: Vec::new(),
            })
        }),
    );
}
