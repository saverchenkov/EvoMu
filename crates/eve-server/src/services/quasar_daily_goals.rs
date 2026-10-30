//! `quasar_daily_goals` — V24.01 Quasar Daily Goals Service (`feat:quasar.dailygoal`, `feat:pve.daily-goals`, `Q-4`).
//!
//! Implements live backend semantics for the EVE Online V24 AIR Daily Goals system:
//! - SDE-backed daily goal seeding from `res:/staticdata/contributionmethods.fsdbinary` and `res:/localizationfsd/localization_fsd_en-us.pickle`.
//! - Daily goal rotation, progress tracking, and reward redemption (ISK, SP).
//! - Request handlers for `eve_public.dailygoal.api` and `eve.dailygoal.api`.
//! - Real-time notice publication (`CurrentGoalsNotice`, `ProgressedNotice`, `CompletedNotice`, `RedeemedNotice`).
//! - Live wallet/skill-point persistence in `WorldRepository`.
//!
//! `WorldStateSnapshot.quasar_state_blobs["quasar_daily_goals"]` persists template overrides and per-character daily goal progress across restarts.

// parity: feat:pve.daily-goals

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_PRECONDITION_FAILED,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, encode_zigzag64, DynamicQuasarMessage, DynamicValue,
    GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Operator-configured daily goal template for V24 AIR Daily Operations.
///
/// Contribution categories map to `res:/staticdata/contributionmethods.fsdbinary` and
/// `eve_public.dailygoal.ContributionConfiguration`:
/// - `1`: Kill NPC (`DestroyNPC`, titleID `665455`, progressDescriptionID `696260`)
/// - `2`: Mine Ore (`MineAsteroid`, titleID `665390`, progressDescriptionID `696245`)
/// - `3`: Scan Signature (`ScanSignature`, titleID `665435`, progressDescriptionID `696251`)
/// - `4`: Space Jump (`JumpStargate`, titleID `665465`, progressDescriptionID `696262`)
/// - `5`: Manufacture Item (`ManufactureItem`, titleID `665394`, progressDescriptionID `696246`)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DailyGoalTemplate {
    pub uuid: [u8; 16],
    pub name_message: String,
    pub description_message: String,
    pub category: u32, // 1: Combat, 2: Mining, 3: Exploration, 4: Navigation, 5: Manufacturing
    pub career: u32,   // 1: Explorer, 2: Industrialist, 3: Enforcer, 4: Soldier of Fortune
    pub target: u64,
    pub isk_reward: i64,
    pub sp_reward: f64,
    #[serde(default)]
    pub completion_cost_isk: Option<i64>,
    #[serde(default)]
    pub completion_cost_evermarks: Option<i64>,
    #[serde(default)]
    pub name_message_id: u32,
    #[serde(default)]
    pub description_message_id: u32,
    #[serde(default)]
    pub help_text_message_id: u32,
    #[serde(default)]
    pub assigner_corporation_id: u32,
}

impl DailyGoalTemplate {
    /// Returns the `res:/staticdata/contributionmethods.fsdbinary` `titleID` or configured message ID.
    pub fn effective_name_message_id(&self) -> u32 {
        if self.name_message_id > 0 {
            return self.name_message_id;
        }
        match self.category {
            1 => 665_455, // DestroyNPC.titleID
            2 => 665_390, // MineAsteroid.titleID
            3 => 665_435, // ScanSignature.titleID
            4 => 665_465, // JumpStargate.titleID
            5 => 665_394, // ManufactureItem.titleID
            _ => 582_520, // Enforcer careerPath nameID
        }
    }

    /// Returns the `res:/staticdata/contributionmethods.fsdbinary` `progressDescriptionID` or configured ID.
    pub fn effective_description_message_id(&self) -> u32 {
        if self.description_message_id > 0 {
            return self.description_message_id;
        }
        match self.category {
            1 => 696_260, // DestroyNPC.progressDescriptionID
            2 => 696_245, // MineAsteroid.progressDescriptionID
            3 => 696_251, // ScanSignature.progressDescriptionID
            4 => 696_262, // JumpStargate.progressDescriptionID
            5 => 696_246, // ManufactureItem.progressDescriptionID
            _ => 582_521, // Enforcer careerPath descriptionID
        }
    }

    /// Returns the `res:/staticdata/contributionmethods.fsdbinary` `descriptionID` or configured help text ID.
    pub fn effective_help_text_message_id(&self) -> u32 {
        if self.help_text_message_id > 0 {
            return self.help_text_message_id;
        }
        match self.category {
            1 => 665_456,
            2 => 665_395,
            3 => 665_401,
            4 => 665_466,
            5 => 665_412, // ManufactureItem.descriptionID
            _ => 582_521,
        }
    }

    /// Returns the assigning NPC corporation ID (defaults to `1_000_419`, Paragon).
    pub fn effective_assigner_corporation_id(&self) -> u32 {
        if self.assigner_corporation_id > 0 {
            self.assigner_corporation_id
        } else {
            1_000_419
        }
    }
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

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
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
) -> bool {
    let mut changed = false;
    let day_bucket = now_unix / 86_400;
    for tpl in templates {
        if let Some(existing) = goals.iter_mut().find(|g| g.uuid == tpl.uuid) {
            existing.template = tpl.clone();
            if existing.day_bucket > 0 && day_bucket > existing.day_bucket {
                existing.current_progress = 0;
                existing.completed = false;
                existing.claimed = false;
                existing.paid_completion = false;
                existing.completed_at_unix = None;
                existing.claimed_at_unix = None;
                existing.day_bucket = day_bucket;
                changed = true;
            } else if existing.day_bucket == 0 && day_bucket > 0 {
                existing.day_bucket = day_bucket;
                changed = true;
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
            changed = true;
        }
    }
    changed
}

pub fn build_sde_daily_goal_templates(sde: &eve_sde::SdeRegistry) -> Vec<DailyGoalTemplate> {
    let methods = sde.contribution_methods().unwrap_or_default();
    let specs: [(&str, u32, u32, u64, i64, f64, Option<i64>, Option<i64>, u32, u32, u32); 5] = [
        ("DestroyNPC", 1, 3, 25, 500_000, 5_000.0, None, Some(2_500), 665_455, 696_260, 665_456),
        ("MineAsteroid", 2, 2, 2_000, 500_000, 5_000.0, Some(100_000), Some(2_500), 665_390, 696_245, 665_395),
        ("ScanSignature", 3, 1, 5, 500_000, 5_000.0, None, Some(2_500), 665_435, 696_251, 665_401),
        ("JumpStargate", 4, 4, 10, 500_000, 5_000.0, None, Some(2_500), 665_465, 696_262, 665_466),
        ("ManufactureItem", 5, 2, 1, 500_000, 5_000.0, None, Some(2_500), 665_394, 696_246, 665_412),
    ];
    specs
        .into_iter()
        .map(
            |(
                method_key,
                category,
                career,
                target,
                isk_reward,
                sp_reward,
                completion_cost_isk,
                completion_cost_evermarks,
                fb_title_id,
                fb_prog_desc_id,
                fb_desc_id,
            )| {
                let m = methods.get(method_key);
                let title_id = m
                    .and_then(|v| v.get("titleID"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .filter(|&v| v > 0)
                    .unwrap_or(fb_title_id);
                let prog_desc_id = m
                    .and_then(|v| v.get("progressDescriptionID"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .filter(|&v| v > 0)
                    .unwrap_or(fb_prog_desc_id);
                let desc_id = m
                    .and_then(|v| v.get("descriptionID"))
                    .and_then(|v| v.as_u64())
                    .map(|v| v as u32)
                    .filter(|&v| v > 0)
                    .unwrap_or(fb_desc_id);
                let mut uuid = [0u8; 16];
                uuid[0..5].copy_from_slice(b"SDEDG");
                uuid[15] = category as u8;
                DailyGoalTemplate {
                    uuid,
                    name_message: method_key.to_string(),
                    description_message: format!("{method_key}_daily"),
                    category,
                    career,
                    target,
                    isk_reward,
                    sp_reward,
                    completion_cost_isk,
                    completion_cost_evermarks,
                    name_message_id: title_id,
                    description_message_id: prog_desc_id,
                    help_text_message_id: desc_id,
                    assigner_corporation_id: 1_000_419,
                }
            },
        )
        .collect()
}

impl DailyGoalManager {
    pub fn new(db: Option<Arc<WorldRepository>>) -> Self {
        Self::new_with_sde(db, None)
    }

    pub fn new_with_sde(
        db: Option<Arc<WorldRepository>>,
        sde: Option<Arc<eve_sde::SdeRegistry>>,
    ) -> Self {
        let mut store = db
            .as_ref()
            .map(|d| {
                d.with_snapshot(|s| {
                    s.quasar_state_blobs
                        .get("quasar_daily_goals")
                        .and_then(|json| {
                            if let Ok(st) = serde_json::from_str::<DailyGoalStore>(json) {
                                Some(st)
                            } else if let Ok(cg) =
                                serde_json::from_str::<HashMap<u32, Vec<CharacterDailyGoal>>>(json)
                            {
                                let mut templates = Vec::new();
                                for list in cg.values() {
                                    for g in list {
                                        if !templates.iter().any(|t: &DailyGoalTemplate| t.uuid == g.template.uuid) {
                                            templates.push(g.template.clone());
                                        }
                                    }
                                }
                                Some(DailyGoalStore {
                                    templates,
                                    character_goals: cg,
                                })
                            } else {
                                None
                            }
                        })
                        .unwrap_or_default()
                })
            })
            .unwrap_or_default();

        if store.templates.is_empty() {
            if let Some(ref sde_reg) = sde {
                store.templates = build_sde_daily_goal_templates(sde_reg);
            }
        }

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

    pub fn now_unix(&self) -> u64 {
        self.db
            .as_ref()
            .map(|db| db.clock().now_unix_secs())
            .unwrap_or(0)
    }

    fn ensure_character_goals(&self, goals: &mut Vec<CharacterDailyGoal>, now_unix: u64) -> bool {
        let tpls = self.templates.read().unwrap_or_else(|e| e.into_inner());
        ensure_character_daily_goals_with_templates(goals, &tpls, now_unix)
    }

    pub fn get_or_init_goals(&self, character_id: u32) -> Vec<CharacterDailyGoal> {
        self.get_or_init_goals_at(character_id, self.now_unix())
    }

    pub fn get_or_init_goals_at(&self, character_id: u32, now_unix: u64) -> Vec<CharacterDailyGoal> {
        let effective_now = if now_unix > 0 { now_unix } else { self.now_unix() };
        let (out, changed) = {
            let mut map = self
                .character_goals
                .write()
                .unwrap_or_else(|e| e.into_inner());
            let is_new_char = !map.contains_key(&character_id);
            let goals = map.entry(character_id).or_default();
            let mutated = self.ensure_character_goals(goals, effective_now);
            (goals.clone(), is_new_char || mutated)
        };
        if changed {
            self.save_to_db();
        }
        out
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
        let credit_res = db.try_with_mut_snapshot(|snap| {
            let Some(ch) = snap.characters.get_mut(&(character_id as i64)) else {
                return Err("Character not found".to_string());
            };
            let Some(new_bal) = ch.balance.checked_add(reward_cents) else {
                return Err("Balance overflow".to_string());
            };
            ch.balance = new_bal;
            ch.free_skill_points += sp;
            snap.append_journal(
                character_id as i64,
                "DailyGoalReward",
                reward_cents,
                new_bal,
                format!("Daily goal reward ({})", res_goal.template.name_message),
            );
            Ok(())
        });
        if credit_res.is_err() {
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
        // 1. Validate goal existence and require configured EverMarks/ISK completion cost under short-lived lock
        let (em_cost, cost_isk, assigner_corp) = {
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
            let em = g.template.completion_cost_evermarks.filter(|&c| c > 0);
            let isk = g
                .template
                .completion_cost_isk
                .filter(|&c| c > 0)
                .map(IskCents::from_isk);
            if em.is_none() && isk.is_none() {
                return Err((
                    STATUS_PRECONDITION_FAILED,
                    "Paid completion cost is not configured for this daily goal".into(),
                ));
            }
            (em, isk, g.template.effective_assigner_corporation_id() as i64)
        };

        // 2. Debit EverMarks or balance via try_with_mut_snapshot without holding character_goals lock
        #[derive(Copy, Clone, PartialEq, Eq)]
        enum PaymentMethod {
            EverMarks(i64),
            Isk(IskCents),
        }
        let debit_res = db.try_with_mut_snapshot(|snap| {
            let Some(ch) = snap.characters.get_mut(&(character_id as i64)) else {
                return Err("Character not found".to_string());
            };
            let em_bal = ch.loyalty_points.get(&assigner_corp).copied().unwrap_or(0);
            if let Some(em_req) = em_cost {
                if em_bal >= em_req {
                    *ch.loyalty_points.entry(assigner_corp).or_insert(0) -= em_req;
                    return Ok(PaymentMethod::EverMarks(em_req));
                }
            }
            if let Some(isk_req) = cost_isk {
                if ch.balance >= isk_req {
                    ch.balance = ch.balance.checked_sub(isk_req).unwrap_or(ch.balance);
                    return Ok(PaymentMethod::Isk(isk_req));
                }
            }
            Err("Insufficient funds or EverMarks to pay for daily goal completion".to_string())
        });
        let payment_method = match debit_res {
            Ok(m) => m,
            Err(msg) => return Err((STATUS_BAD_REQUEST, msg)),
        };

        // 3. Mark goal complete under write lock without holding across db.try_with_mut_snapshot
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
            let _ = db.try_with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&(character_id as i64)) {
                    match payment_method {
                        PaymentMethod::EverMarks(em) => {
                            let cur = ch.loyalty_points.entry(assigner_corp).or_insert(0);
                            *cur = cur.saturating_add(em);
                        }
                        PaymentMethod::Isk(cost) => {
                            ch.balance = ch.balance.checked_add(cost).unwrap_or(ch.balance);
                        }
                    }
                }
                Ok::<(), String>(())
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
        let now_unix = self.now_unix();
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
            let mut contributed = Vec::new();
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
                        let prev = g.current_progress;
                        g.current_progress = new_prog.min(g.desired_progress);
                        if g.current_progress > prev {
                            contributed.push((uuid, prev, g.current_progress, g.desired_progress));
                        }
                    }
                }
            }
            gateway.progression.extra.save_to_db();
            // Each server-observed contribution is announced to the project's corporation:
            // ContributedNotice carries previous/current progress; reaching the target completes it.
            for (uuid, prev, current, desired) in contributed {
                use crate::services::quasar_goal_tale_notices as notices;
                let scope = notices::goal_scope(gateway.progression.extra.goal_corp_id(&uuid), char_id);
                let _ = gateway.publish_notice(
                    scope.clone(),
                    notices::goal_contributed(&uuid, char_id, prev, current, now_unix),
                    now_unix,
                );
                if current >= desired && prev < desired {
                    let _ = gateway.publish_notice(scope, notices::goal_completed(&uuid), now_unix);
                }
            }
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

/// Encodes `eve_public.dailygoal.Identifier` / `eve.dailygoal.Identifier`: `1: bytes uuid`.
pub fn encode_goal_identifier(uuid: &[u8; 16], out: &mut Vec<u8>) {
    encode_bytes_field(1, uuid, out);
}

fn encode_loc_message_id(seq: u32) -> Vec<u8> {
    let mut b = Vec::new();
    encode_varint_field(1, seq as u64, &mut b);
    b
}

fn encode_contribution_configuration(category: u32, out: &mut Vec<u8>) {
    let mut cfg = Vec::new();
    match category {
        1 => {
            // 1: kill_npc (KillNPC { 1: repeated Method { 1: SolarSystemMatcher { 1: any = true } } })
            let mut matcher = Vec::new();
            encode_varint_field(1, 1, &mut matcher);
            let mut method = Vec::new();
            encode_bytes_field(1, &matcher, &mut method);
            let mut kill_npc = Vec::new();
            encode_bytes_field(1, &method, &mut kill_npc);
            encode_bytes_field(1, &kill_npc, &mut cfg);
        }
        2 => {
            // 3: mine_ore (MineOre { 1: repeated Method { 1: SolarSystemMatcher { 1: true }, 2: OreTypeMatcher { 1: true } } })
            let mut any_m = Vec::new();
            encode_varint_field(1, 1, &mut any_m);
            let mut method = Vec::new();
            encode_bytes_field(1, &any_m, &mut method);
            encode_bytes_field(2, &any_m, &mut method);
            let mut mine_ore = Vec::new();
            encode_bytes_field(1, &method, &mut mine_ore);
            encode_bytes_field(3, &mine_ore, &mut cfg);
        }
        3 => {
            // 8: scan_signature (ScanSignature { 1: repeated Method { 1: SignatureTypeMatcher { 1: true }, 2: SolarSystemMatcher { 1: true } } })
            let mut any_m = Vec::new();
            encode_varint_field(1, 1, &mut any_m);
            let mut method = Vec::new();
            encode_bytes_field(1, &any_m, &mut method);
            encode_bytes_field(2, &any_m, &mut method);
            let mut scan_sig = Vec::new();
            encode_bytes_field(1, &method, &mut scan_sig);
            encode_bytes_field(8, &scan_sig, &mut cfg);
        }
        4 => {
            // 13: space_jump (SpaceJump {})
            encode_bytes_field(13, &[], &mut cfg);
        }
        5 => {
            // 9: install_manufacturing_job (InstallManufacturingJob { 1: repeated Method })
            let mut owner_m = Vec::new();
            encode_varint_field(3, 1, &mut owner_m); // any = true
            let mut any_m = Vec::new();
            encode_varint_field(1, 1, &mut any_m);
            let mut method = Vec::new();
            encode_bytes_field(1, &owner_m, &mut method);
            encode_bytes_field(2, &any_m, &mut method);
            encode_bytes_field(3, &any_m, &mut method);
            let mut mfg = Vec::new();
            encode_bytes_field(1, &method, &mut mfg);
            encode_bytes_field(9, &mfg, &mut cfg);
        }
        _ => {
            encode_bytes_field(13, &[], &mut cfg);
        }
    }
    encode_bytes_field(6, &cfg, out);
}

fn encode_isk_unit(isk: i64) -> Vec<u8> {
    let mut currency = Vec::new();
    encode_varint_field(1, encode_zigzag64(isk), &mut currency);
    let mut isk_msg = Vec::new();
    encode_bytes_field(1, &currency, &mut isk_msg);
    let mut unit = Vec::new();
    encode_bytes_field(1, &isk_msg, &mut unit);
    unit
}

fn encode_sp_unit(sp: u32) -> Vec<u8> {
    let mut sp_msg = Vec::new();
    encode_varint_field(1, sp as u64, &mut sp_msg);
    let mut unit = Vec::new();
    encode_bytes_field(3, &sp_msg, &mut unit);
    unit
}

/// Encodes `eve_public.dailygoal.Attributes` / `eve.dailygoal.Attributes` matching V24 descriptors.
pub fn encode_goal_attributes(tpl: &DailyGoalTemplate, out: &mut Vec<u8>) {
    // 1: name_message (.eve_public.localization.message.Identifier { 1: uint32 sequential })
    encode_bytes_field(1, &encode_loc_message_id(tpl.effective_name_message_id()), out);
    // 2: description_message (.eve_public.localization.message.Identifier { 1: uint32 sequential })
    encode_bytes_field(2, &encode_loc_message_id(tpl.effective_description_message_id()), out);
    // 3: help_text_message (.eve_public.localization.message.Identifier { 1: uint32 sequential })
    encode_bytes_field(3, &encode_loc_message_id(tpl.effective_help_text_message_id()), out);
    // 4: assigner (.eve_public.corporation.Identifier { 1: uint32 sequential })
    encode_bytes_field(4, &encode_loc_message_id(tpl.effective_assigner_corporation_id()), out);
    // 5: category (.eve_public.dailygoal.Category: CATEGORY_DAILY = 1)
    encode_varint_field(5, 1, out);
    // 6: contribution_configuration (.eve_public.dailygoal.ContributionConfiguration)
    encode_contribution_configuration(tpl.category, out);
    // 7: career (.eve_public.career.Path: 1..=4)
    encode_varint_field(7, tpl.career.clamp(1, 4) as u64, out);
    // 8: repeated payment (.eve_public.dailygoal.Payment { 2: period = 1, 3: unit })
    if tpl.isk_reward > 0 {
        let mut p = Vec::new();
        encode_varint_field(2, 1, &mut p); // PAYMENT_PERIOD_COMPLETION
        encode_bytes_field(3, &encode_isk_unit(tpl.isk_reward), &mut p);
        encode_bytes_field(8, &p, out);
    }
    if tpl.sp_reward > 0.0 {
        let mut p = Vec::new();
        encode_varint_field(2, 1, &mut p); // PAYMENT_PERIOD_COMPLETION
        encode_bytes_field(3, &encode_sp_unit(tpl.sp_reward as u32), &mut p);
        encode_bytes_field(8, &p, out);
    }
    // 9: target (uint64)
    encode_varint_field(9, tpl.target, out);
}

/// Encodes `repeated eve_public.dailygoal.api.Earning` (one `Earning` per `Unit` oneof variant).
pub fn encode_goal_earnings(tpl: &DailyGoalTemplate, field_num: u32, out: &mut Vec<u8>) {
    if tpl.isk_reward > 0 {
        let mut earn = Vec::new();
        encode_bytes_field(1, &encode_isk_unit(tpl.isk_reward), &mut earn);
        encode_bytes_field(field_num, &earn, out);
    }
    if tpl.sp_reward > 0.0 {
        let mut earn = Vec::new();
        encode_bytes_field(1, &encode_sp_unit(tpl.sp_reward as u32), &mut earn);
        encode_bytes_field(field_num, &earn, out);
    }
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
                let mut id_bytes = Vec::new();
                encode_goal_identifier(&g.uuid, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut out);
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
            encode_goal_earnings(&g.template, 4, &mut out);

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
                let mut id_bytes = Vec::new();
                encode_goal_identifier(&g.uuid, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut out);
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
                encode_goal_identifier(&g.uuid, &mut id_bytes);
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
            encode_goal_identifier(&redeemed.uuid, &mut id_bytes);
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
                    encode_goal_identifier(&redeemed.uuid, &mut id_bytes);
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
            encode_goal_identifier(&goal.uuid, &mut id_bytes);

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
                encode_goal_identifier(&target_uuid, &mut id_bytes);
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

    // 10. eve.dailygoal.api.GetAllRequest -> GetAllResponse (repeated eve.dailygoal.Identifier goals = 1)
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
                let mut id_bytes = Vec::new();
                encode_goal_identifier(&g.uuid, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut out);
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
            if g.completed {
                let ts = GoogleTimestamp {
                    seconds: g.completed_at_unix.unwrap_or(ctx.now_unix()) as i64,
                    nanos: 0,
                };
                encode_bytes_field(4, &ts.encode_proto(), &mut out);
            } else {
                encode_varint_field(3, 1, &mut out); // not_completed = true
            }
            encode_goal_earnings(&g.template, 5, &mut out);
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
