//! `quasar_daily_goals` — V24.01 Quasar Daily Goals Service (`feat:quasar.dailygoal`, `Q-4`).
//!
//! Implements live backend semantics for the EVE Online V24 AIR Daily Goals system:
//! - Daily goal rotation, progress tracking, and reward redemption (ISK, SP).
//! - Request handlers for `eve_public.dailygoal.api` and `eve.dailygoal.api`.
//! - Real-time notice publication (`CurrentGoalsNotice`, `ProgressedNotice`, `CompletedNotice`, `RedeemedNotice`).
//! - Live wallet/skill-point persistence in `WorldRepository`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// DERIVED standard daily goal definitions for V24 AIR Daily Operations.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DailyGoalTemplate {
    pub uuid: [u8; 16],
    pub name_message: String,
    pub description_message: String,
    pub category: u32, // 1: Combat, 2: Mining, 3: Exploration, 4: Navigation, 5: Manufacturing
    pub career: u32,   // 1: Enforcer, 2: Explorer, 3: Industrialist, 4: Soldier of Fortune
    pub target: u64,
    pub isk_reward: i64,
    pub sp_reward: f64,
    #[serde(default)]
    pub completion_cost_isk: Option<i64>,
    #[serde(default)]
    pub completion_cost_evermarks: Option<i64>,
}

/// DERIVED: Canonical AIR daily goal templates available for testing and dynamic configuration.
pub fn default_daily_goal_templates() -> Vec<DailyGoalTemplate> {
    vec![
        DailyGoalTemplate {
            uuid: [0x44, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1], // DG01...1
            name_message: "ui_daily_goal_kill_npcs".to_string(),
            description_message: "ui_daily_goal_kill_npcs_desc".to_string(),
            category: 1, // Combat
            career: 1,   // Enforcer
            target: 10,
            isk_reward: 500_000,
            sp_reward: 5_000.0,
            completion_cost_isk: None,
            completion_cost_evermarks: None,
        },
        DailyGoalTemplate {
            uuid: [0x44, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2], // DG01...2
            name_message: "ui_daily_goal_mine_ore".to_string(),
            description_message: "ui_daily_goal_mine_ore_desc".to_string(),
            category: 2, // Mining
            career: 3,   // Industrialist
            target: 500,
            isk_reward: 500_000,
            sp_reward: 5_000.0,
            completion_cost_isk: None,
            completion_cost_evermarks: None,
        },
        DailyGoalTemplate {
            uuid: [0x44, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3], // DG01...3
            name_message: "ui_daily_goal_scan_signatures".to_string(),
            description_message: "ui_daily_goal_scan_signatures_desc".to_string(),
            category: 3, // Exploration
            career: 2,   // Explorer
            target: 2,
            isk_reward: 250_000,
            sp_reward: 5_000.0,
            completion_cost_isk: None,
            completion_cost_evermarks: None,
        },
        DailyGoalTemplate {
            uuid: [0x44, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4], // DG01...4
            name_message: "ui_daily_goal_stargate_jumps".to_string(),
            description_message: "ui_daily_goal_stargate_jumps_desc".to_string(),
            category: 4, // Navigation / Activity
            career: 4,   // Soldier of Fortune
            target: 5,
            isk_reward: 250_000,
            sp_reward: 5_000.0,
            completion_cost_isk: None,
            completion_cost_evermarks: None,
        },
        DailyGoalTemplate {
            uuid: [0x44, 0x47, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 5], // DG01...5
            name_message: "ui_daily_goal_manufacture_items".to_string(),
            description_message: "ui_daily_goal_manufacture_items_desc".to_string(),
            category: 5, // Manufacturing
            career: 3,   // Industrialist
            target: 1,
            isk_reward: 500_000,
            sp_reward: 5_000.0,
            completion_cost_isk: None,
            completion_cost_evermarks: None,
        },
    ]
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CharacterDailyGoal {
    pub uuid: [u8; 16],
    pub template: DailyGoalTemplate,
    pub current_progress: u64,
    pub completed: bool,
    pub claimed: bool,
    pub paid_completion: bool,
    pub completed_at_unix: Option<u64>,
    pub claimed_at_unix: Option<u64>,
    #[serde(default)]
    pub day_bucket: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DailyGoalStore {
    #[serde(default)]
    pub templates: Vec<DailyGoalTemplate>,
    #[serde(default)]
    pub character_goals: HashMap<u32, Vec<CharacterDailyGoal>>,
}

pub struct DailyGoalManager {
    pub templates: RwLock<Vec<DailyGoalTemplate>>,
    pub character_goals: RwLock<HashMap<u32, Vec<CharacterDailyGoal>>>,
    db: Option<Arc<WorldRepository>>,
}

impl Default for DailyGoalManager {
    fn default() -> Self {
        Self {
            templates: RwLock::new(Vec::new()),
            character_goals: RwLock::new(HashMap::new()),
            db: None,
        }
    }
}

fn ensure_character_daily_goals_with_templates(
    goals: &mut Vec<CharacterDailyGoal>,
    templates: &[DailyGoalTemplate],
    now_unix: u64,
) {
    let day_bucket = now_unix / 86_400;
    for tpl in templates {
        if let Some(existing) = goals.iter_mut().find(|g| g.uuid == tpl.uuid) {
            if existing.day_bucket > 0 && day_bucket > existing.day_bucket {
                existing.current_progress = 0;
                existing.completed = false;
                existing.claimed = false;
                existing.paid_completion = false;
                existing.completed_at_unix = None;
                existing.claimed_at_unix = None;
                existing.day_bucket = day_bucket;
            } else if existing.day_bucket == 0 && day_bucket > 0 {
                existing.day_bucket = day_bucket;
            }
        } else {
            goals.push(CharacterDailyGoal {
                uuid: tpl.uuid,
                template: tpl.clone(),
                current_progress: 0,
                completed: false,
                claimed: false,
                paid_completion: false,
                completed_at_unix: None,
                claimed_at_unix: None,
                day_bucket,
            });
        }
    }
}

impl DailyGoalManager {
    pub fn new(db: Option<Arc<WorldRepository>>) -> Self {
        let store = db.as_ref().map(|d| {
            d.with_snapshot(|s| {
                s.quasar_state_blobs
                    .get("quasar_daily_goals")
                    .and_then(|json| {
                        if let Ok(st) = serde_json::from_str::<DailyGoalStore>(json) {
                            Some(st)
                        } else if let Ok(cg) = serde_json::from_str::<HashMap<u32, Vec<CharacterDailyGoal>>>(json) {
                            Some(DailyGoalStore {
                                templates: Vec::new(),
                                character_goals: cg,
                            })
                        } else {
                            None
                        }
                    })
                    .unwrap_or_else(|| DailyGoalStore {
                        templates: Vec::new(),
                        character_goals: HashMap::new(),
                    })
            })
        }).unwrap_or_else(|| DailyGoalStore {
            templates: Vec::new(),
            character_goals: HashMap::new(),
        });

        let mut character_goals = store.character_goals;
        for goals in character_goals.values_mut() {
            ensure_character_daily_goals_with_templates(goals, &store.templates, 0);
        }

        Self {
            templates: RwLock::new(store.templates),
            character_goals: RwLock::new(character_goals),
            db,
        }
    }

    pub fn save_to_db(&self) {
        if let Some(ref db) = self.db {
            let templates = self.templates.read().unwrap_or_else(|e| e.into_inner()).clone();
            let character_goals = self.character_goals.read().unwrap_or_else(|e| e.into_inner()).clone();
            let store = DailyGoalStore { templates, character_goals };
            if let Ok(json) = serde_json::to_string(&store) {
                let _ = db.try_with_mut_snapshot(|snap| {
                    snap.quasar_state_blobs.insert("quasar_daily_goals".into(), json);
                    Ok::<(), String>(())
                });
            }
        }
    }

    pub fn configure_template(&self, tpl: DailyGoalTemplate) {
        let templates = {
            let mut tpls = self.templates.write().unwrap_or_else(|e| e.into_inner());
            if let Some(existing) = tpls.iter_mut().find(|t| t.uuid == tpl.uuid) {
                *existing = tpl.clone();
            } else {
                tpls.push(tpl.clone());
            }
            tpls.clone()
        };
        {
            let mut map = self.character_goals.write().unwrap_or_else(|e| e.into_inner());
            for goals in map.values_mut() {
                ensure_character_daily_goals_with_templates(goals, &templates, 0);
            }
        }
        self.save_to_db();
    }

    pub fn set_templates(&self, new_templates: Vec<DailyGoalTemplate>) {
        {
            let mut tpls = self.templates.write().unwrap_or_else(|e| e.into_inner());
            *tpls = new_templates.clone();
        }
        {
            let mut map = self.character_goals.write().unwrap_or_else(|e| e.into_inner());
            for goals in map.values_mut() {
                ensure_character_daily_goals_with_templates(goals, &new_templates, 0);
            }
        }
        self.save_to_db();
    }

    pub fn get_templates(&self) -> Vec<DailyGoalTemplate> {
        self.templates.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn ensure_character_goals(&self, goals: &mut Vec<CharacterDailyGoal>, now_unix: u64) {
        let tpls = self.templates.read().unwrap_or_else(|e| e.into_inner());
        ensure_character_daily_goals_with_templates(goals, &tpls, now_unix);
    }

    pub fn get_or_init_goals(&self, character_id: u32) -> Vec<CharacterDailyGoal> {
        self.get_or_init_goals_at(character_id, 0)
    }

    pub fn get_or_init_goals_at(&self, character_id: u32, now_unix: u64) -> Vec<CharacterDailyGoal> {
        let mut map = self
            .character_goals
            .write()
            .unwrap_or_else(|e| e.into_inner());
        let goals = map.entry(character_id).or_default();
        self.ensure_character_goals(goals, now_unix);
        goals.clone()
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
        let goals = map.entry(character_id).or_default();
        self.ensure_character_goals(goals, now_unix);

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
        let (res_goal, isk, sp) = {
            let mut map = self
                .character_goals
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let Some(goals) = map.get_mut(&character_id) else {
                return Err((STATUS_NOT_FOUND, "No daily goals found for character".into()));
            };
            self.ensure_character_goals(goals, now_unix);

            let mut found = None;
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
                    found = Some((g.clone(), g.template.isk_reward, g.template.sp_reward));
                    break;
                }
            }
            match found {
                Some(f) => f,
                None => return Err((STATUS_NOT_FOUND, "Daily goal not found".into())),
            }
        };

        let reward_cents = IskCents::from_isk(isk);
        let mut credited = false;
        db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&(character_id as i64)) {
                if let Some(new_bal) = ch.balance.checked_add(reward_cents) {
                    ch.balance = new_bal;
                    ch.free_skill_points += sp;
                    credited = true;
                    snap.append_journal(
                        character_id as i64,
                        "DailyGoalReward",
                        reward_cents,
                        new_bal,
                        format!("Daily goal reward ({})", res_goal.template.name_message),
                    );
                }
            }
        });
        if !credited {
            let mut map = self
                .character_goals
                .write()
                .unwrap_or_else(|e| e.into_inner());
            if let Some(goals) = map.get_mut(&character_id) {
                for g in goals.iter_mut() {
                    if &g.uuid == goal_uuid {
                        g.claimed = false;
                        g.claimed_at_unix = None;
                        break;
                    }
                }
            }
            return Err((STATUS_BAD_REQUEST, "Unable to credit daily goal reward".into()));
        }

        Ok(res_goal)
    }

    pub fn pay_for_completion(
        &self,
        character_id: u32,
        goal_uuid: &[u8; 16],
        now_unix: u64,
        db: &Arc<WorldRepository>,
    ) -> Result<CharacterDailyGoal, (u32, String)> {
        // 1. Validate goal existence and read data-driven EverMarks/ISK completion cost under short-lived lock
        let (em_cost, cost_isk) = {
            let mut map = self
                .character_goals
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let goals = map.entry(character_id).or_default();
            self.ensure_character_goals(goals, now_unix);
            let Some(g) = goals.iter().find(|g| &g.uuid == goal_uuid) else {
                return Err((STATUS_NOT_FOUND, "Daily goal not found".into()));
            };
            if g.completed {
                return Err((STATUS_BAD_REQUEST, "Daily goal already completed".into()));
            }
            let em = g.template.completion_cost_evermarks.unwrap_or(2_500);
            let isk = g.template.completion_cost_isk.unwrap_or(100_000);
            (em, IskCents::from_isk(isk))
        };

        // 2. Debit EverMarks or balance without holding character_goals lock
        #[derive(Copy, Clone, PartialEq, Eq)]
        enum PaymentMethod {
            EverMarks(i64),
            Isk(IskCents),
        }
        let mut paid_method: Option<PaymentMethod> = None;
        db.with_mut_snapshot(|snap| {
            if let Some(ch) = snap.characters.get_mut(&(character_id as i64)) {
                let em = ch.loyalty_points.get(&1_000_419).copied().unwrap_or(0);
                if em_cost > 0 && em >= em_cost {
                    *ch.loyalty_points.entry(1_000_419).or_insert(0) -= em_cost;
                    paid_method = Some(PaymentMethod::EverMarks(em_cost));
                } else if ch.balance >= cost_isk {
                    ch.balance = ch.balance.checked_sub(cost_isk).unwrap_or(ch.balance);
                    paid_method = Some(PaymentMethod::Isk(cost_isk));
                }
            }
        });
        let Some(payment_method) = paid_method else {
            return Err((
                STATUS_BAD_REQUEST,
                "Insufficient funds or EverMarks to pay for daily goal completion".into(),
            ));
        };

        // 3. Mark goal complete under write lock without holding across db.with_mut_snapshot
        let outcome = {
            let mut map = self
                .character_goals
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let mut found = None;
            if let Some(goals) = map.get_mut(&character_id) {
                for g in goals.iter_mut() {
                    if &g.uuid == goal_uuid {
                        if g.completed {
                            found = Some(Err((STATUS_BAD_REQUEST, "Daily goal already completed".into())));
                        } else {
                            g.current_progress = g.template.target;
                            g.completed = true;
                            g.paid_completion = true;
                            g.completed_at_unix = Some(now_unix);
                            found = Some(Ok(g.clone()));
                        }
                        break;
                    }
                }
            }
            found
        };

        let rollback_payment = |db: &Arc<WorldRepository>| {
            db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&(character_id as i64)) {
                    match payment_method {
                        PaymentMethod::EverMarks(em) => {
                            let cur = ch.loyalty_points.entry(1_000_419).or_insert(0);
                            *cur = cur.saturating_add(em);
                        }
                        PaymentMethod::Isk(cost) => {
                            ch.balance = ch.balance.checked_add(cost).unwrap_or(ch.balance);
                        }
                    }
                }
            });
        };

        match outcome {
            Some(Ok(g)) => {
                self.save_to_db();
                Ok(g)
            }
            Some(Err(e)) => {
                rollback_payment(db);
                Err(e)
            }
            None => {
                rollback_payment(db);
                Err((STATUS_NOT_FOUND, "Daily goal not found".into()))
            }
        }
    }

    pub fn on_gameplay_event(
        &self,
        char_id: u32,
        category: u32,
        delta: u64,
        gateway: &PublicGatewayRouter,
    ) {
        let now_unix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut progressed = Vec::new();
        {
            let mut map = self
                .character_goals
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let goals = map.entry(char_id).or_default();
            self.ensure_character_goals(goals, now_unix);

            for g in goals.iter_mut() {
                if g.template.category == category && !g.completed {
                    let prev = g.completed;
                    g.current_progress = g.current_progress.saturating_add(delta);
                    if g.current_progress >= g.template.target {
                        g.completed = true;
                        g.completed_at_unix = Some(now_unix);
                    }
                    let just_completed = !prev && g.completed;
                    progressed.push((g.uuid, g.current_progress, just_completed));
                }
            }
        }
        self.save_to_db();

        let corp_updates = gateway.progression.corp_state.on_member_activity(char_id, delta as i64);
        if !corp_updates.is_empty() {
            {
                let mut ggoals = gateway
                    .progression
                    .extra
                    .generic_goals
                    .write()
                    .unwrap_or_else(|e| e.into_inner());
                for (goal_id, new_prog) in corp_updates {
                    let mut uuid = [0u8; 16];
                    uuid[0..8].copy_from_slice(b"CORPGOAL");
                    uuid[8..16].copy_from_slice(&goal_id.to_be_bytes());
                    let hex = crate::services::quasar_progression_extra::uuid_to_hex(&uuid);
                    if let Some(g) = ggoals.get_mut(&hex) {
                        g.current_progress = new_prog.min(g.desired_progress);
                    }
                }
            }
            gateway.progression.extra.save_to_db();
        }

        for (uuid, current_progress, just_completed) in progressed {
            let mut id_bytes = Vec::new();
            encode_bytes_field(1, &uuid, &mut id_bytes);
            let mut not_bytes = Vec::new();
            encode_bytes_field(1, &id_bytes, &mut not_bytes);
            encode_varint_field(2, current_progress, &mut not_bytes);

            let _ = gateway.publish_notice(
                NoticeScope::Character(char_id),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.ProgressedNotice"),
                    value: not_bytes,
                },
                now_unix,
            );

            if just_completed {
                let mut comp_bytes = Vec::new();
                encode_bytes_field(1, &id_bytes, &mut comp_bytes);
                let _ = gateway.publish_notice(
                    NoticeScope::Character(char_id),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.CompletedNotice"),
                        value: comp_bytes,
                    },
                    now_unix,
                );
            }
        }
    }
}

// ============================================================================
// Protobuf Wire Encoders for Daily Goals
// ============================================================================

pub fn encode_goal_attributes(tpl: &DailyGoalTemplate, out: &mut Vec<u8>) {
    encode_string_field(1, &tpl.name_message, out);
    encode_string_field(2, &tpl.description_message, out);
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
            let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
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
            let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
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
            let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
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
            let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
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
            mgr.save_to_db();

            // Publish RedeemedNotice
            let mut not_bytes = Vec::new();
            let mut id_bytes = Vec::new();
            encode_bytes_field(1, &redeemed.uuid, &mut id_bytes);
            encode_bytes_field(1, &id_bytes, &mut not_bytes);
            super::quasar_social_chat::ctx_queue_notice(NoticeScope::Character(cid), GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.RedeemedNotice"), value: not_bytes }, ctx.now_unix());

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.RedeemResponse"),
                value: Vec::new(),
            })
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
            let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
            for g in goals.iter().filter(|g| g.completed && !g.claimed) {
                if let Ok(redeemed) = mgr.redeem_goal(cid, &g.uuid, ctx.now_unix(), &db_redeem_all) {
                    let mut not_bytes = Vec::new();
                    let mut id_bytes = Vec::new();
                    encode_bytes_field(1, &redeemed.uuid, &mut id_bytes);
                    encode_bytes_field(1, &id_bytes, &mut not_bytes);
                    super::quasar_social_chat::ctx_queue_notice(
                        NoticeScope::Character(cid),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.RedeemedNotice"),
                            value: not_bytes,
                        },
                        ctx.now_unix(),
                    );
                }
            }
            mgr.save_to_db();
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.RedeemAllResponse"),
                value: Vec::new(),
            })
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
            let goal = mgr.pay_for_completion(cid, &target_uuid, ctx.now_unix(), &db_pay)?;
            mgr.save_to_db();

            let mut id_bytes = Vec::new();
            encode_bytes_field(1, &goal.uuid, &mut id_bytes);

            // 1. ProgressedNotice
            let mut prog_bytes = Vec::new();
            encode_bytes_field(1, &id_bytes, &mut prog_bytes);
            encode_varint_field(2, goal.current_progress, &mut prog_bytes);
            super::quasar_social_chat::ctx_queue_notice(
                NoticeScope::Character(cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.ProgressedNotice"),
                    value: prog_bytes,
                },
                ctx.now_unix(),
            );

            // 2. CompletedNotice
            let mut comp_bytes = Vec::new();
            encode_bytes_field(1, &id_bytes, &mut comp_bytes);
            super::quasar_social_chat::ctx_queue_notice(
                NoticeScope::Character(cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.CompletedNotice"),
                    value: comp_bytes,
                },
                ctx.now_unix(),
            );

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.PayForCompletionResponse"),
                value: Vec::new(),
            })
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
            let target_char = dyn_msg
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::Message(m) => m.get(1).and_then(|seq| match seq {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Uint64(u) => Some(*u as u32),
                        DynamicValue::Int32(i) => Some(*i as u32),
                        DynamicValue::Int64(i) => Some(*i as u32),
                        _ => None,
                    }),
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Uint64(u) => Some(*u as u32),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    DynamicValue::Int64(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(cid);
            let delta = dyn_msg
                .get(3)
                .and_then(|v| match v {
                    DynamicValue::Uint64(u) => Some(*u),
                    DynamicValue::Int64(i) => Some(*i as u64),
                    _ => None,
                })
                .unwrap_or(1);
            let updated = mgr.add_progress(target_char, &target_uuid, delta, ctx.now_unix());
            mgr.save_to_db();

            if let Some((goal, just_completed)) = updated {
                let mut not_bytes = Vec::new();
                let mut id_bytes = Vec::new();
                encode_bytes_field(1, &target_uuid, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut not_bytes);
                encode_varint_field(2, goal.current_progress, &mut not_bytes);
                super::quasar_social_chat::ctx_queue_notice(
                    NoticeScope::Character(target_char),
                    GoogleAny {
                        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.ProgressedNotice"),
                        value: not_bytes,
                    },
                    ctx.now_unix(),
                );

                if just_completed {
                    let mut comp_bytes = Vec::new();
                    encode_bytes_field(1, &id_bytes, &mut comp_bytes);
                    super::quasar_social_chat::ctx_queue_notice(
                        NoticeScope::Character(target_char),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.CompletedNotice"),
                            value: comp_bytes,
                        },
                        ctx.now_unix(),
                    );
                }
            }

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.dailygoal.api.admin.ProgressResponse"),
                value: Vec::new(),
            })
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
            let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
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
            let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
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
            let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
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
                let goals = mgr.get_or_init_goals_at(cid, ctx.now_unix());
                for g in goals.iter().filter(|g| g.completed && !g.claimed) {
                    let _ = mgr.redeem_goal(cid, &g.uuid, ctx.now_unix(), &db_auto);
                }
                mgr.save_to_db();
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.dailygoal.api.admin.AutoRedeemResponse"),
                value: Vec::new(),
            })
        }),
    );
}

fn encode_id_field(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_buf = Vec::new();
    encode_varint_field(1, id, &mut id_buf);
    encode_bytes_field(field_num, &id_buf, out);
}

/// Helper for broadcasting `CreatedNotice` (`eve_public.goal.api.CreatedNotice`).
pub fn publish_goal_created_notice(
    gateway: &PublicGatewayRouter,
    character_id: u32,
    goal_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_field(1, goal_id, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.CreatedNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::Character(character_id), any, now_unix).map(|(_, n)| n)
}

/// Helper for broadcasting `CompletedNotice` (`eve_public.goal.api.CompletedNotice`).
pub fn publish_goal_completed_notice(
    gateway: &PublicGatewayRouter,
    character_id: u32,
    goal_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_field(1, goal_id, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.CompletedNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::Character(character_id), any, now_unix).map(|(_, n)| n)
}

/// Helper for broadcasting `ContributedNotice` (`eve_public.goal.contribution.api.ContributedNotice`).
pub fn publish_goal_contributed_notice(
    gateway: &PublicGatewayRouter,
    character_id: u32,
    goal_id: u64,
    previous_progress: u64,
    current_progress: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut ts_buf = Vec::new();
    encode_varint_field(1, now_unix, &mut ts_buf);

    let mut payload = Vec::new();
    encode_bytes_field(1, &ts_buf, &mut payload);
    encode_id_field(2, character_id as u64, &mut payload);
    encode_id_field(3, goal_id, &mut payload);
    encode_varint_field(4, previous_progress, &mut payload);
    encode_varint_field(5, current_progress, &mut payload);

    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.contribution.api.ContributedNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::Character(character_id), any, now_unix).map(|(_, n)| n)
}

/// Helper for broadcasting `StartedNotice` (`eve_public.tale.api.StartedNotice`).
pub fn publish_tale_started_notice(
    gateway: &PublicGatewayRouter,
    solar_system_id: u32,
    tale_id: u64,
    template_id: u64,
    now_unix: u64,
) -> Result<usize, eve_proto::BridgeRpcError> {
    let mut payload = Vec::new();
    encode_id_field(1, tale_id, &mut payload);
    encode_id_field(2, template_id, &mut payload);
    let any = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.tale.api.StartedNotice"),
        value: payload,
    };
    gateway.publish_notice(NoticeScope::SolarSystem(solar_system_id), any, now_unix).map(|(_, n)| n)
}
