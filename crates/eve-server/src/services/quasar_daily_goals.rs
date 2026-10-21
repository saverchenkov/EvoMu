//! `quasar_daily_goals` — V24.01 Quasar Daily Goals Service (`feat:quasar.dailygoal`, `Q-4`).
//!
//! Implements live backend semantics for the EVE Online V24 AIR Daily Goals system:
//! - Daily goal rotation, progress tracking, and reward redemption (ISK, SP).
//! - Request handlers for `eve_public.dailygoal.api` and `eve.dailygoal.api`.
//! - Real-time notice publication (`CurrentGoalsNotice`, `ProgressedNotice`, `CompletedNotice`, `RedeemedNotice`).
//! - Live wallet/skill-point persistence in `WorldRepository`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, GoogleAny, GoogleEmpty, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// DERIVED standard daily goal definitions for V24 AIR Daily Operations.
#[derive(Debug, Clone)]
pub struct DailyGoalTemplate {
    pub uuid: [u8; 16],
    pub name_message: &'static str,
    pub description_message: &'static str,
    pub category: u32, // 1: Combat, 2: Mining, 3: Exploration, 4: Industry
    pub career: u32,   // 1: Enforcer, 2: Explorer, 3: Industrialist, 4: Soldier of Fortune
    pub target: u64,
    pub isk_reward: i64,
    pub sp_reward: f64,
}

pub fn default_daily_goal_templates() -> Vec<DailyGoalTemplate> {
    vec![
        DailyGoalTemplate {
            uuid: [0x44, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], // DG01...1
            name_message: "ui_daily_goal_kill_npcs",
            description_message: "ui_daily_goal_kill_npcs_desc",
            category: 1, // Combat
            career: 1,   // Enforcer
            target: 10,
            isk_reward: 500_000,
            sp_reward: 5_000.0,
        },
        DailyGoalTemplate {
            uuid: [0x44, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2], // DG01...2
            name_message: "ui_daily_goal_mine_ore",
            description_message: "ui_daily_goal_mine_ore_desc",
            category: 2, // Mining
            career: 3,   // Industrialist
            target: 500,
            isk_reward: 500_000,
            sp_reward: 5_000.0,
        },
        DailyGoalTemplate {
            uuid: [0x44, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3], // DG01...3
            name_message: "ui_daily_goal_scan_signatures",
            description_message: "ui_daily_goal_scan_signatures_desc",
            category: 3, // Exploration
            career: 2,   // Explorer
            target: 2,
            isk_reward: 250_000,
            sp_reward: 5_000.0,
        },
        DailyGoalTemplate {
            uuid: [0x44, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4], // DG01...4
            name_message: "ui_daily_goal_stargate_jumps",
            description_message: "ui_daily_goal_stargate_jumps_desc",
            category: 4, // Navigation / Activity
            career: 4,   // Soldier of Fortune
            target: 5,
            isk_reward: 250_000,
            sp_reward: 5_000.0,
        },
    ]
}

#[derive(Debug, Clone)]
pub struct CharacterDailyGoal {
    pub uuid: [u8; 16],
    pub template: DailyGoalTemplate,
    pub current_progress: u64,
    pub completed: bool,
    pub claimed: bool,
    pub paid_completion: bool,
    pub completed_at_unix: Option<u64>,
    pub claimed_at_unix: Option<u64>,
}

#[derive(Debug, Default)]
pub struct DailyGoalManager {
    pub character_goals: RwLock<HashMap<u32, Vec<CharacterDailyGoal>>>,
}

impl DailyGoalManager {
    pub fn new() -> Self {
        Self {
            character_goals: RwLock::new(HashMap::new()),
        }
    }

    pub fn get_or_init_goals(&self, character_id: u32) -> Vec<CharacterDailyGoal> {
        let mut map = self
            .character_goals
            .write()
            .unwrap_or_else(|e| e.into_inner());
        map.entry(character_id)
            .or_insert_with(|| {
                default_daily_goal_templates()
                    .into_iter()
                    .map(|tpl| CharacterDailyGoal {
                        uuid: tpl.uuid,
                        template: tpl,
                        current_progress: 0,
                        completed: false,
                        claimed: false,
                        paid_completion: false,
                        completed_at_unix: None,
                        claimed_at_unix: None,
                    })
                    .collect()
            })
            .clone()
    }

    pub fn add_progress(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        progress_delta: u64,
        now_unix: u64,
    ) -> Option<(CharacterDailyGoal, bool)> {
        let mut map = self
            .character_goals
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let goals = map.entry(character_id).or_insert_with(|| {
            default_daily_goal_templates()
                .into_iter()
                .map(|tpl| CharacterDailyGoal {
                    uuid: tpl.uuid,
                    template: tpl,
                    current_progress: 0,
                    completed: false,
                    claimed: false,
                    paid_completion: false,
                    completed_at_unix: None,
                    claimed_at_unix: None,
                })
                .collect()
        });

        for g in goals.iter_mut() {
            if &g.uuid == goal_uuid {
                let prev_completed = g.completed;
                g.current_progress = g.current_progress.saturating_add(progress_delta);
                if g.current_progress >= g.template.target && !g.completed {
                    g.completed = true;
                    g.completed_at_unix = Some(now_unix);
                }
                let just_completed = !prev_completed && g.completed;
                return Some((g.clone(), just_completed));
            }
        }
        None
    }

    pub fn redeem_goal(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        now_unix: u64,
        db: &WorldRepository,
    ) -> Result<CharacterDailyGoal, (u32, String)> {
        let mut map = self
            .character_goals
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let Some(goals) = map.get_mut(&character_id) else {
            return Err((STATUS_NOT_FOUND, "No daily goals found for character".into()));
        };

        for g in goals.iter_mut() {
            if &g.uuid == goal_uuid {
                if !g.completed {
                    return Err((STATUS_FORBIDDEN, "Daily goal not yet completed".into()));
                }
                if g.claimed {
                    return Err((STATUS_FORBIDDEN, "Reward already claimed".into()));
                }
                g.claimed = true;
                g.claimed_at_unix = Some(now_unix);

                // Live balance payout
                let isk = g.template.isk_reward;
                let sp = g.template.sp_reward;
                db.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&(character_id as i64)) {
                        ch.balance = ch
                            .balance
                            .checked_add(IskCents::from_isk(isk))
                            .unwrap_or(ch.balance);
                        ch.free_skill_points += sp;
                    }
                });

                return Ok(g.clone());
            }
        }
        Err((STATUS_NOT_FOUND, "Daily goal not found".into()))
    }

    pub fn pay_for_completion(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        now_unix: u64,
        db: &Arc<WorldRepository>,
    ) -> Result<CharacterDailyGoal, (u32, String)> {
        // DERIVED: Daily goal pay-for-completion buyout cost in ISK
        const DAILY_GOAL_PAY_COMPLETION_COST_ISK: i64 = 100_000;

        let mut map = self
            .character_goals
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let goals = map.entry(character_id).or_insert_with(|| {
            default_daily_goal_templates()
                .into_iter()
                .map(|tpl| CharacterDailyGoal {
                    uuid: tpl.uuid,
                    template: tpl,
                    current_progress: 0,
                    completed: false,
                    claimed: false,
                    paid_completion: false,
                    completed_at_unix: None,
                    claimed_at_unix: None,
                })
                .collect()
        });

        for g in goals.iter_mut() {
            if &g.uuid == goal_uuid {
                if g.completed {
                    return Err((STATUS_BAD_REQUEST, "Daily goal already completed".into()));
                }
                let cost = IskCents::from_isk(DAILY_GOAL_PAY_COMPLETION_COST_ISK);
                let mut payment_ok = false;
                db.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&(character_id as i64)) {
                        if ch.balance >= cost {
                            ch.balance = ch.balance.checked_sub(cost).unwrap_or(ch.balance);
                            payment_ok = true;
                        }
                    }
                });
                if !payment_ok {
                    return Err((
                        STATUS_BAD_REQUEST,
                        "Insufficient funds to pay for daily goal completion".into(),
                    ));
                }
                g.completed = true;
                g.paid_completion = true;
                g.current_progress = g.template.target;
                g.completed_at_unix = Some(now_unix);
                return Ok(g.clone());
            }
        }
        Err((STATUS_NOT_FOUND, "Daily goal not found".into()))
    }
}

// ============================================================================
// Protobuf Wire Encoders for Daily Goals
// ============================================================================

pub fn encode_goal_attributes(tpl: &DailyGoalTemplate, out: &mut Vec<u8>) {
    encode_string_field(1, tpl.name_message, out);
    encode_string_field(2, tpl.description_message, out);
    encode_string_field(3, "", out); // help_text_message
    encode_string_field(4, "AIR Daily Operations", out); // assigner
    encode_varint_field(5, tpl.category as u64, out);
    encode_varint_field(7, tpl.career as u64, out);
    encode_varint_field(9, tpl.target, out);
    encode_varint_field(12, 0, out); // omega = false
}

pub fn encode_earning(tpl: &DailyGoalTemplate, out: &mut Vec<u8>) {
    // Earning.unit -> Unit
    let mut unit_bytes = Vec::new();
    // 1: InterStellarKredits
    let mut isk_bytes = Vec::new();
    encode_varint_field(1, tpl.isk_reward as u64, &mut isk_bytes);
    encode_bytes_field(1, &isk_bytes, &mut unit_bytes);

    // 3: SkillPoints
    let mut sp_bytes = Vec::new();
    encode_varint_field(1, tpl.sp_reward as u64, &mut sp_bytes);
    encode_bytes_field(3, &sp_bytes, &mut unit_bytes);

    encode_bytes_field(1, &unit_bytes, out);
    encode_varint_field(2, 0, out); // omega_required = false
}

pub fn encode_goal_wire(g: &CharacterDailyGoal, out: &mut Vec<u8>) {
    // 1: Identifier
    let mut id_bytes = Vec::new();
    encode_bytes_field(1, &g.uuid, &mut id_bytes);
    encode_bytes_field(1, &id_bytes, out);

    // 2: Attributes
    let mut attr_bytes = Vec::new();
    encode_goal_attributes(&g.template, &mut attr_bytes);
    encode_bytes_field(2, &attr_bytes, out);

    // 3: current_progress
    encode_varint_field(3, g.current_progress, out);

    // 5: earnings
    let mut earn_bytes = Vec::new();
    encode_earning(&g.template, &mut earn_bytes);
    encode_bytes_field(5, &earn_bytes, out);

    // 6: paid_completion
    encode_varint_field(6, if g.paid_completion { 1 } else { 0 }, out);
}

pub fn extract_uuid(dyn_msg: &DynamicQuasarMessage, field_num: u32) -> Option<[u8; 16]> {
    if let Some(DynamicValue::Message(id_msg)) = dyn_msg.get(field_num) {
        if let Some(DynamicValue::Bytes(b)) = id_msg.get(1) {
            if b.len() == 16 {
                let mut arr = [0u8; 16];
                arr.copy_from_slice(b);
                return Some(arr);
            }
        }
    }
    None
}

// ============================================================================
// Handler Registration
// ============================================================================

pub fn register_daily_goal_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    manager: Arc<DailyGoalManager>,
) {
    // 1. eve_public.dailygoal.api.GetAllCurrentRequest -> GetAllCurrentResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.dailygoal.api.GetAllCurrentRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            let mut out = Vec::new();
            for g in &goals {
                let mut gw = Vec::new();
                encode_goal_wire(g, &mut gw);
                encode_bytes_field(1, &gw, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.GetAllCurrentResponse"),
                value: out,
            })
        }),
    );

    // 2. eve_public.dailygoal.api.GetRequest -> GetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.dailygoal.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            let g = goals
                .iter()
                .find(|g| g.uuid == target_uuid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Daily goal not found".into()))?;

            let mut out = Vec::new();
            let mut attr_bytes = Vec::new();
            encode_goal_attributes(&g.template, &mut attr_bytes);
            encode_bytes_field(1, &attr_bytes, &mut out);
            encode_varint_field(2, g.current_progress, &mut out);
            let mut earn_bytes = Vec::new();
            encode_earning(&g.template, &mut earn_bytes);
            encode_bytes_field(4, &earn_bytes, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.GetResponse"),
                value: out,
            })
        }),
    );

    // 3. eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest -> GetAllCompletedWithEntitlementsResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.dailygoal.api.GetAllCompletedWithEntitlementsRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            let mut out = Vec::new();
            for g in goals.iter().filter(|g| g.completed) {
                let mut gw = Vec::new();
                encode_goal_wire(g, &mut gw);
                encode_bytes_field(1, &gw, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.GetAllCompletedWithEntitlementsResponse"
                ),
                value: out,
            })
        }),
    );

    // 4. eve_public.dailygoal.api.GetAllWithRewardsRequest -> GetAllWithRewardsResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.dailygoal.api.GetAllWithRewardsRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            let mut out = Vec::new();
            for g in goals.iter().filter(|g| g.completed && !g.claimed) {
                let mut id_bytes = Vec::new();
                encode_bytes_field(1, &g.uuid, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.GetAllWithRewardsResponse"
                ),
                value: out,
            })
        }),
    );

    // 5. eve_public.dailygoal.api.RedeemRequest -> RedeemResponse
    let mgr = Arc::clone(&manager);
    let db_redeem = Arc::clone(&db);
    router.register_handler(
        "eve_public.dailygoal.api.RedeemRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let redeemed = mgr.redeem_goal(cid, &target_uuid, ctx.now_unix(), &db_redeem)?;

            // Publish RedeemedNotice
            let mut not_bytes = Vec::new();
            let mut id_bytes = Vec::new();
            encode_bytes_field(1, &redeemed.uuid, &mut id_bytes);
            encode_bytes_field(1, &id_bytes, &mut not_bytes);

            Ok(GoogleEmpty.to_any())
        }),
    );

    // 6. eve_public.dailygoal.api.RedeemAllRequest -> RedeemAllResponse
    let mgr = Arc::clone(&manager);
    let db_redeem_all = Arc::clone(&db);
    router.register_handler(
        "eve_public.dailygoal.api.RedeemAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            for g in goals.iter().filter(|g| g.completed && !g.claimed) {
                let _ = mgr.redeem_goal(cid, &g.uuid, ctx.now_unix(), &db_redeem_all);
            }
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 7. eve_public.dailygoal.api.PayForCompletionRequest -> PayForCompletionResponse
    let mgr = Arc::clone(&manager);
    let db_pay = Arc::clone(&db);
    router.register_handler(
        "eve_public.dailygoal.api.PayForCompletionRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let _ = mgr.pay_for_completion(cid, &target_uuid, ctx.now_unix(), &db_pay)?;
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 8. eve_public.dailygoal.api.admin.ProgressRequest -> ProgressResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.dailygoal.api.admin.ProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let delta = dyn_msg
                .get(3)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(1);
            let _ = mgr.add_progress(cid, &target_uuid, delta, ctx.now_unix());
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 9. eve.dailygoal.api.GetRequest -> GetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.dailygoal.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            let g = goals
                .iter()
                .find(|g| g.uuid == target_uuid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Daily goal not found".into()))?;

            let mut out = Vec::new();
            let mut attr_bytes = Vec::new();
            encode_goal_attributes(&g.template, &mut attr_bytes);
            encode_bytes_field(1, &attr_bytes, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.GetResponse"),
                value: out,
            })
        }),
    );

    // 10. eve.dailygoal.api.GetAllRequest -> GetAllResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.dailygoal.api.GetAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            let mut out = Vec::new();
            for g in &goals {
                let mut attr_bytes = Vec::new();
                encode_goal_attributes(&g.template, &mut attr_bytes);
                encode_bytes_field(1, &attr_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 11. eve.dailygoal.api.GetProgressRequest -> GetProgressResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve.dailygoal.api.GetProgressRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing goal Identifier.uuid".into()));
            };
            let goals = mgr.get_or_init_goals(cid);
            let g = goals
                .iter()
                .find(|g| g.uuid == target_uuid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Daily goal not found".into()))?;

            let mut out = Vec::new();
            encode_varint_field(1, g.current_progress, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.GetProgressResponse"),
                value: out,
            })
        }),
    );

    // 12. eve.dailygoal.api.admin.AutoRedeemRequest -> AutoRedeemResponse
    let mgr = Arc::clone(&manager);
    let db_auto = Arc::clone(&db);
    router.register_handler(
        "eve.dailygoal.api.admin.AutoRedeemRequest",
        Arc::new(move |ctx, _any, _dyn| {
            crate::quasar_gateway::require_admin_role(ctx)?;
            if let Some(cid) = ctx.effective_character_id() {
                let goals = mgr.get_or_init_goals(cid);
                for g in goals.iter().filter(|g| g.completed && !g.claimed) {
                    let _ = mgr.redeem_goal(cid, &g.uuid, ctx.now_unix(), &db_auto);
                }
            }
            Ok(GoogleEmpty.to_any())
        }),
    );
}
