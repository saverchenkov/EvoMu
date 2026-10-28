//! `quasar_social_corp` — Quasar Corporation, Alliance, and Corporation Goals Services (`Q-5`).
//!
//! Implements live request/response and notice fan-out semantics for:
//! - `alliance` (5 protos): `eve/alliance/alliance.proto`, `eve/alliance/api/events.proto`,
//!   `eve/alliance/corporation/corporation.proto`, `eve/alliance/mail/mail.proto`,
//!   `eve_public/alliance/alliance.proto`
//! - `corporation` (49 protos): accounting, divisions, logo, medals, members, roles, titles,
//!   tracking, NPC corps, offices, PI offices, starbases, structures, skill plans, milestones
//! - `corporationgoal` (11 protos): corporate projects, contribution configurations,
//!   matchers, notices (`RewardEarnedNotice`, `RedeemedNotice`, `ExpiredNotice`)

use super::quasar_social_chat::{ctx_now_unix, ctx_queue_notice};
use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_PRECONDITION_FAILED, STATUS_UNAUTHORIZED,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, AuthoritativeContext,
    DynamicQuasarMessage, DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CorpSkillPlanRecord {
    pub id: u64,
    pub corp_id: u32,
    pub name: String,
    pub description: String,
    pub category_id: u32,
    pub requirements: Vec<(u32, i32)>,
    pub milestones: Vec<(u64, String)>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CorpGoalRecord {
    pub id: u64,
    pub corp_id: u32,
    pub name: String,
    pub description: String,
    pub target_progress: i64,
    pub current_progress: i64,
    pub active: bool,
    pub contributors: HashMap<u32, (i64, bool)>, // char_id -> (progress, redeemed)
    #[serde(default)]
    pub escrow_isk: i64,
    #[serde(default)]
    pub initial_escrow_isk: i64,
    #[serde(default)]
    pub reward_per_unit_isk: i64,
}

impl CorpGoalRecord {
    pub fn calc_reward_isk(&self, progress: i64) -> i64 {
        if progress <= 0 {
            return 0;
        }
        let pool = if self.initial_escrow_isk > 0 {
            self.initial_escrow_isk
        } else {
            self.escrow_isk
        };
        let unit_rate = if self.reward_per_unit_isk > 0 {
            self.reward_per_unit_isk
        } else if self.target_progress > 0 && pool > 0 {
            (pool / self.target_progress).clamp(1, 10_000)
        } else {
            10_000
        };
        progress.saturating_mul(unit_rate)
    }
}

#[derive(Default)]
pub struct QuasarCorpState {
    pub db: Option<Arc<WorldRepository>>,
    pub skill_plans: RwLock<HashMap<u64, CorpSkillPlanRecord>>,
    pub goals: RwLock<HashMap<u64, CorpGoalRecord>>,
    pub id_counters: RwLock<(u64, u64, u64)>,
}

impl QuasarCorpState {
    pub fn from_db(db: Arc<WorldRepository>) -> Arc<Self> {
        let (plans_map, goals_map, saved_counters) = db.with_snapshot(|snap| {
            let mut plans = HashMap::new();
            let mut goals = HashMap::new();
            let mut counters = (1u64, 1u64, 101u64);
            if let Some(s) = snap.quasar_state_blobs.get("quasar_corp_counters") {
                if let Ok(c) = serde_json::from_str::<(u64, u64, u64)>(s) {
                    counters = c;
                }
            }
            if let Some(s) = snap.quasar_state_blobs.get("quasar_corp_plans") {
                if let Ok(p_map) = serde_json::from_str::<HashMap<u64, CorpSkillPlanRecord>>(s) {
                    plans = p_map;
                }
            }
            if let Some(s) = snap.quasar_state_blobs.get("quasar_corp_goals") {
                if let Ok(g_map) = serde_json::from_str::<HashMap<u64, CorpGoalRecord>>(s) {
                    goals = g_map;
                }
            }
            (plans, goals, counters)
        });

        let max_plan = plans_map.keys().copied().max().unwrap_or(0);
        let max_ms = plans_map
            .values()
            .flat_map(|p| p.milestones.iter().map(|(mid, _)| *mid))
            .max()
            .unwrap_or(0);
        let max_goal = goals_map.keys().copied().max().unwrap_or(100);

        Arc::new(Self {
            db: Some(db),
            skill_plans: RwLock::new(plans_map),
            goals: RwLock::new(goals_map),
            id_counters: RwLock::new((
                saved_counters.0.max(1).max(max_plan + 1),
                saved_counters.1.max(1).max(max_ms + 1),
                saved_counters.2.max(101).max(max_goal + 1),
            )),
        })
    }

    pub fn create_goal(&self, corp_id: u32, name: String, description: String, target_progress: i64, escrow_isk: i64) -> Result<u64, (u32, String)> {
        self.create_goal_from_division(corp_id, name, description, target_progress, escrow_isk, 1)
    }

    pub fn create_goal_from_division(&self, corp_id: u32, name: String, description: String, target_progress: i64, escrow_isk: i64, wallet_div: u8) -> Result<u64, (u32, String)> {
        let Some(ref db) = self.db else { return Err((STATUS_BAD_REQUEST, "Database not available".into())); };
        if escrow_isk < 0 || target_progress <= 0 {
            return Err((STATUS_BAD_REQUEST, "Invalid goal target or escrow amount".into()));
        }
        if escrow_isk > 0 {
            let cost = eve_db::IskCents::from_isk(escrow_isk);
            let div = wallet_div.clamp(1, 7);
            let mut debited = false;
            db.with_mut_snapshot(|snap| {
                if let Some(corp) = snap.corporations.get_mut(&(corp_id as i64)) {
                    let cur = if div == 1 { corp.master_wallet } else { corp.ensure_wallet_divisions(); corp.division_balance(div) };
                    if let Some(rem) = cur.checked_sub(cost) {
                        corp.set_division_balance(div, rem);
                        debited = true;
                    }
                }
            });
            if !debited {
                return Err((STATUS_BAD_REQUEST, "Insufficient corporation funds for goal escrow".into()));
            }
        }
        let id = self.alloc_goal_id();
        {
            let mut goals = self.goals.write().unwrap_or_else(|e| e.into_inner());
            goals.insert(id, CorpGoalRecord {
                id, corp_id, name, description, target_progress, current_progress: 0,
                active: true, contributors: HashMap::new(), escrow_isk,
                initial_escrow_isk: escrow_isk, reward_per_unit_isk: 0,
            });
        }
        self.sync_to_db();
        Ok(id)
    }

    pub fn close_or_delete_goal(&self, goal_id: u64, remove: bool) {
        let mut refund = None;
        {
            let mut goals = self.goals.write().unwrap_or_else(|e| e.into_inner());
            if remove {
                if let Some(cg) = goals.remove(&goal_id) {
                    if cg.escrow_isk > 0 {
                        refund = Some((cg.corp_id, cg.escrow_isk));
                    }
                }
            } else if let Some(cg) = goals.get_mut(&goal_id) {
                cg.active = false;
                if cg.escrow_isk > 0 {
                    refund = Some((cg.corp_id, cg.escrow_isk));
                    cg.escrow_isk = 0;
                }
            }
        }
        if let (Some(ref db), Some((corp_id, amount_isk))) = (&self.db, refund) {
            let credit = eve_db::IskCents::from_isk(amount_isk);
            db.with_mut_snapshot(|snap| {
                if let Some(corp) = snap.corporations.get_mut(&(corp_id as i64)) {
                    let next = corp.master_wallet.checked_add(credit).unwrap_or(corp.master_wallet);
                    corp.set_division_balance(1, next);
                }
            });
        }
        self.sync_to_db();
    }

    pub fn on_member_activity(&self, char_id: u32, delta: i64) -> Vec<(u64, u64)> {
        if delta <= 0 {
            return Vec::new();
        }
        let Some(ref db) = self.db else { return Vec::new() };
        let corp_ids: Vec<u32> = db.with_snapshot(|snap| {
            let mut ids = Vec::new();
            if let Some(c) = snap.characters.get(&(char_id as i64)) {
                ids.push(c.corporation_id as u32);
            }
            for corp in snap.corporations.values() {
                if corp.ceo_id == char_id as i64 && !ids.contains(&(corp.corporation_id as u32)) {
                    ids.push(corp.corporation_id as u32);
                }
            }
            ids
        });
        if corp_ids.is_empty() {
            return Vec::new();
        }
        let mut updated = Vec::new();
        {
            let mut goals = self.goals.write().unwrap_or_else(|e| e.into_inner());
            for g in goals.values_mut() {
                if corp_ids.contains(&g.corp_id) && g.active && g.current_progress < g.target_progress {
                    let rem = (g.target_progress - g.current_progress).max(0);
                    let add = delta.min(rem);
                    if add > 0 {
                        g.current_progress = g.current_progress.saturating_add(add);
                        let entry = g.contributors.entry(char_id).or_insert((0, false));
                        entry.0 = entry.0.saturating_add(add).min(g.target_progress);
                        updated.push((g.id, g.current_progress as u64));
                    }
                }
            }
        }
        if !updated.is_empty() {
            self.sync_to_db();
        }
        updated
    }

    pub fn alloc_plan_id(&self) -> u64 {
        let mut c = self.id_counters.write().unwrap_or_else(|e| e.into_inner());
        let id = c.0.max(1);
        c.0 = id + 1;
        id
    }

    pub fn alloc_milestone_id(&self) -> u64 {
        let mut c = self.id_counters.write().unwrap_or_else(|e| e.into_inner());
        let id = c.1.max(1);
        c.1 = id + 1;
        id
    }

    pub fn alloc_goal_id(&self) -> u64 {
        let mut c = self.id_counters.write().unwrap_or_else(|e| e.into_inner());
        let id = c.2.max(101);
        c.2 = id + 1;
        id
    }

    pub fn sync_to_db(&self) {
        let Some(ref db) = self.db else { return };
        let plans = self.skill_plans.read().unwrap_or_else(|e| e.into_inner()).clone();
        let goals = self.goals.read().unwrap_or_else(|e| e.into_inner()).clone();
        let counters = *self.id_counters.read().unwrap_or_else(|e| e.into_inner());
        db.with_mut_snapshot(|snap| {
            if let Ok(c_json) = serde_json::to_string(&counters) {
                snap.quasar_state_blobs.insert("quasar_corp_counters".into(), c_json);
            }
            if let Ok(p_json) = serde_json::to_string(&plans) {
                snap.quasar_state_blobs.insert("quasar_corp_plans".into(), p_json);
            }
            if let Ok(g_json) = serde_json::to_string(&goals) {
                snap.quasar_state_blobs.insert("quasar_corp_goals".into(), g_json);
            }
        });
    }
}

fn encode_seq_id(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut id_bytes = Vec::with_capacity(8);
    encode_varint_field(1, id, &mut id_bytes);
    encode_bytes_field(field_num, &id_bytes, out);
}

fn encode_goal_id(field_num: u32, id: u64, out: &mut Vec<u8>) {
    let mut uuid = [0u8; 16];
    uuid[0..8].copy_from_slice(b"CORPGOAL");
    uuid[8..16].copy_from_slice(&id.to_be_bytes());
    let mut id_bytes = Vec::with_capacity(18);
    encode_bytes_field(1, &uuid, &mut id_bytes);
    encode_bytes_field(field_num, &id_bytes, out);
}

pub fn register(router: &PublicGatewayRouter, _db: Arc<WorldRepository>) {
    let state = Arc::clone(&router.progression.corp_state);

    // ========================================================================
    // CORPORATION SKILL PLANS & MILESTONES (eve_public.corporation.skill.plan)
    // ========================================================================

    fn resolve_corp_id(ctx: &AuthoritativeContext, db: Option<&Arc<WorldRepository>>) -> Result<u32, (u32, String)> {
        ctx.effective_corporation_id()
            .or_else(|| ctx.effective_character_id().zip(db).and_then(|(cid, d)| d.with_snapshot(|s| s.characters.get(&(cid as i64)).map(|c| c.corporation_id as u32))))
            .ok_or_else(|| (STATUS_FORBIDDEN, "Missing corporation context".into()))
    }

    fn check_corp_admin_role(
        ctx: &AuthoritativeContext,
        db: Option<&Arc<WorldRepository>>,
    ) -> Result<(u32, u32), (u32, String)> {
        let Some(cid) = ctx.effective_character_id() else {
            return Err((STATUS_FORBIDDEN, "Authentication required".into()));
        };
        let corpid = resolve_corp_id(ctx, db)?;
        let corp_role = if ctx.corp_role_mask != 0 {
            ctx.corp_role_mask
        } else {
            ctx.identity.as_ref().and_then(|id| id.roles.as_ref().map(|r| r.bitmask)).unwrap_or(0)
        };
        let is_admin = (corp_role & (eve_db::corp_roles::ROLE_DIRECTOR | eve_db::corp_roles::ROLE_PERSONNEL_MANAGER)) != 0
            || crate::quasar_gateway::require_admin_role(ctx).is_ok();
        if is_admin || db.map_or(false, |d| d.with_snapshot(|s| s.corporations.get(&(corpid as i64)).map_or(false, |c| c.ceo_id == cid as i64))) {
            return Ok((cid, corpid));
        }
        Err((STATUS_FORBIDDEN, "Insufficient corporation role".into()))
    }

    fn get_mut_plan<'a>(plans: &'a mut HashMap<u64, CorpSkillPlanRecord>, plan_id: u64, corpid: u32) -> Result<&'a mut CorpSkillPlanRecord, (u32, String)> {
        let Some(p) = plans.get_mut(&plan_id) else {
            return Err((STATUS_NOT_FOUND, "Skill plan not found".into()));
        };
        if p.corp_id != corpid {
            return Err((STATUS_FORBIDDEN, "Cross-corporation access denied".into()));
        }
        Ok(p)
    }

    fn encode_corp_sp_id(field_num: u32, id: u64, out: &mut Vec<u8>) {
        let mut uuid = [0u8; 16];
        uuid[0..8].copy_from_slice(b"CORPSPLN");
        uuid[8..16].copy_from_slice(&id.to_be_bytes());
        let mut id_bytes = Vec::new();
        encode_bytes_field(1, &uuid, &mut id_bytes);
        encode_bytes_field(field_num, &id_bytes, out);
    }

    fn queue_corp_sp_notice(ctx: &AuthoritativeContext, corpid: u32, suffix: &str, id: u64, extra: Option<&str>, cat: Option<u32>) {
        let mut b = Vec::new();
        encode_corp_sp_id(1, id, &mut b);
        if let Some(s) = extra { encode_string_field(2, s, &mut b); }
        if let Some(c) = cat { encode_seq_id(2, c as u64, &mut b); }
        ctx_queue_notice(
            NoticeScope::Corporation(corpid),
            GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.{suffix}"), value: b },
            ctx_now_unix(ctx),
        );
    }

    fn parse_corp_skill_reqs(val: Option<&DynamicValue>, sde: &eve_sde::SdeRegistry) -> Result<Vec<(u32, i32)>, (u32, String)> {
        let mut reqs = Vec::new();
        if let Some(DynamicValue::Repeated(list)) = val {
            for item in list {
                let DynamicValue::Message(m) = item else {
                    return Err((STATUS_BAD_REQUEST, "Invalid skill requirement entry".into()));
                };
                let tid = m.get(1).and_then(|v| match v {
                    DynamicValue::Message(id_msg) => id_msg.get(1).and_then(|t| match t {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Uint64(u) => u32::try_from(*u).ok(),
                        DynamicValue::Int32(i) => u32::try_from(*i).ok(),
                        _ => None,
                    }),
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Uint64(u) => u32::try_from(*u).ok(),
                    _ => None,
                }).unwrap_or(0);
                let lvl = m.get(2).and_then(|v| match v {
                    DynamicValue::Uint32(u) => i32::try_from(*u).ok(),
                    DynamicValue::Uint64(u) => i32::try_from(*u).ok(),
                    DynamicValue::Int32(i) => Some(*i),
                    _ => None,
                }).unwrap_or(0);
                if tid == 0 || !(1..=5).contains(&lvl) || !sde.types.get(&tid).is_some_and(|info| info.published && info.category_id == 16) {
                    return Err((STATUS_BAD_REQUEST, "Invalid skill requirement type_id or level".into()));
                }
                reqs.push((tid, lvl));
            }
        }
        Ok(reqs)
    }

    // 5. CreateRequest -> CreateResponse
    let state_sp_create = Arc::clone(&state);
    let sde_sp_create = Arc::clone(&router.sde);
    router.register_handler(
        "eve_public.corporation.skill.plan.CreateRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let (_cid, corpid) = check_corp_admin_role(ctx, state_sp_create.db.as_ref())?;
            let Some(DynamicValue::Message(attr)) = dyn_msg.get(1) else {
                return Err((STATUS_BAD_REQUEST, "Missing skill plan attributes".into()));
            };
            let name = attr.get(1).and_then(|v| match v { DynamicValue::String(s) => Some(s.trim().to_string()), _ => None }).unwrap_or_default();
            if name.is_empty() {
                return Err((STATUS_BAD_REQUEST, "Skill plan name is required".into()));
            }
            let desc = attr.get(2).and_then(|v| match v { DynamicValue::String(s) => Some(s.clone()), _ => None }).unwrap_or_default();
            let reqs = parse_corp_skill_reqs(attr.get(3), &sde_sp_create)?;
            let cat_id = extract_id(attr.get(4)).unwrap_or(0) as u32;
            let plan_id = state_sp_create.alloc_plan_id();

            let plan = CorpSkillPlanRecord {
                id: plan_id,
                corp_id: corpid,
                name: name.clone(),
                description: desc,
                category_id: cat_id,
                requirements: reqs,
                milestones: Vec::new(),
            };
            state_sp_create.skill_plans.write().unwrap_or_else(|e| e.into_inner()).insert(plan_id, plan);
            state_sp_create.sync_to_db();

            let mut notice_bytes = Vec::new();
            encode_corp_sp_id(1, plan_id, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny { type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.CreatedNotice"), value: notice_bytes },
                ctx_now_unix(ctx),
            );

            let mut out = Vec::new();
            encode_corp_sp_id(1, plan_id, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.CreateResponse"),
                value: out,
            })
        }),
    );

    // 6. GetAllRequest -> GetAllResponse
    let state_sp_all = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporation.skill.plan.GetAllRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let corpid = resolve_corp_id(ctx, state_sp_all.db.as_ref())?;
            let plans = state_sp_all.skill_plans.read().unwrap_or_else(|e| e.into_inner());
            let mut out = Vec::new();
            for plan in plans.values().filter(|p| p.corp_id == corpid) {
                let mut p_bytes = Vec::new();
                encode_corp_sp_id(1, plan.id, &mut p_bytes);
                encode_string_field(2, &plan.name, &mut p_bytes);
                encode_string_field(3, &plan.description, &mut p_bytes);
                encode_bytes_field(1, &p_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 7. GetRequest & GetSharedRequest -> GetResponse
    let state_sp_get = Arc::clone(&state);
    let get_plan_fn: crate::quasar_gateway::QuasarHandlerFn = Arc::new(move |ctx: &AuthoritativeContext, any: &GoogleAny, dyn_msg: &DynamicQuasarMessage| {
        let plan_id = extract_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing skill_plan Identifier.uuid".into()))?;
        let caller_corpid = if any.type_url.ends_with("GetSharedRequest") {
            None
        } else {
            Some(resolve_corp_id(ctx, state_sp_get.db.as_ref())?)
        };
        let plans = state_sp_get.skill_plans.read().unwrap_or_else(|e| e.into_inner());
        let plan = plans.get(&plan_id).ok_or((STATUS_NOT_FOUND, "Corporation skill plan not found".into()))?;
        if caller_corpid.is_some_and(|corpid| corpid != plan.corp_id) {
            return Err((STATUS_FORBIDDEN, "Cross-corporation access denied".into()));
        }

        let mut attr = Vec::new();
        encode_string_field(1, &plan.name, &mut attr);
        encode_string_field(2, &plan.description, &mut attr);
        for (tid, lvl) in &plan.requirements {
            let mut req_b = Vec::new();
            let mut type_b = Vec::new();
            encode_varint_field(1, *tid as u64, &mut type_b);
            encode_bytes_field(1, &type_b, &mut req_b);
            encode_varint_field(2, *lvl as u64, &mut req_b);
            encode_bytes_field(3, &req_b, &mut attr);
        }
        encode_seq_id(4, plan.category_id as u64, &mut attr);

        let mut out = Vec::new();
        encode_bytes_field(1, &attr, &mut out);
        Ok(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.GetResponse"),
            value: out,
        })
    });
    router.register_handler("eve_public.corporation.skill.plan.GetRequest", Arc::clone(&get_plan_fn));
    router.register_handler("eve_public.corporation.skill.plan.GetSharedRequest", get_plan_fn);

    // 8. DeleteRequest -> DeleteResponse
    let state_sp_del = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporation.skill.plan.DeleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let (_cid, corpid) = check_corp_admin_role(ctx, state_sp_del.db.as_ref())?;
            let plan_id = extract_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing skill_plan Identifier.uuid".into()))?;
            {
                let mut plans = state_sp_del.skill_plans.write().unwrap_or_else(|e| e.into_inner());
                let _ = get_mut_plan(&mut plans, plan_id, corpid)?;
                plans.remove(&plan_id);
            }
            state_sp_del.sync_to_db();
            queue_corp_sp_notice(ctx, corpid, "DeletedNotice", plan_id, None, None);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.DeleteResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 9. SetNameRequest -> SetNameResponse
    let state_sp_name = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporation.skill.plan.SetNameRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let (_cid, corpid) = check_corp_admin_role(ctx, state_sp_name.db.as_ref())?;
            let plan_id = extract_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing skill_plan Identifier.uuid".into()))?;
            let new_name = match dyn_msg.get(2) {
                Some(DynamicValue::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                _ => return Err((STATUS_BAD_REQUEST, "Skill plan name is required".into())),
            };
            {
                let mut plans = state_sp_name.skill_plans.write().unwrap_or_else(|e| e.into_inner());
                get_mut_plan(&mut plans, plan_id, corpid)?.name = new_name.clone();
            }
            state_sp_name.sync_to_db();
            queue_corp_sp_notice(ctx, corpid, "NameUpdatedNotice", plan_id, Some(&new_name), None);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.SetNameResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 10. SetDescriptionRequest -> SetDescriptionResponse
    let state_sp_desc = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporation.skill.plan.SetDescriptionRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let (_cid, corpid) = check_corp_admin_role(ctx, state_sp_desc.db.as_ref())?;
            let plan_id = extract_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing skill_plan Identifier.uuid".into()))?;
            let new_desc = match dyn_msg.get(2) {
                Some(DynamicValue::String(s)) => s.clone(),
                None => String::new(),
                _ => return Err((STATUS_BAD_REQUEST, "Invalid skill plan description".into())),
            };
            {
                let mut plans = state_sp_desc.skill_plans.write().unwrap_or_else(|e| e.into_inner());
                get_mut_plan(&mut plans, plan_id, corpid)?.description = new_desc.clone();
            }
            state_sp_desc.sync_to_db();
            queue_corp_sp_notice(ctx, corpid, "DescriptionUpdatedNotice", plan_id, Some(&new_desc), None);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.SetDescriptionResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 11. SetCategoryRequest -> SetCategoryResponse
    let state_sp_cat = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporation.skill.plan.SetCategoryRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let (_cid, corpid) = check_corp_admin_role(ctx, state_sp_cat.db.as_ref())?;
            let plan_id = extract_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing skill_plan Identifier.uuid".into()))?;
            let cat_id = extract_id(dyn_msg.get(2)).ok_or((STATUS_BAD_REQUEST, "Missing category Identifier".into()))? as u32;
            {
                let mut plans = state_sp_cat.skill_plans.write().unwrap_or_else(|e| e.into_inner());
                get_mut_plan(&mut plans, plan_id, corpid)?.category_id = cat_id;
            }
            state_sp_cat.sync_to_db();
            queue_corp_sp_notice(ctx, corpid, "CategoryUpdatedNotice", plan_id, None, Some(cat_id));
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.SetCategoryResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 12. SetSkillRequirementsRequest -> SetSkillRequirementsResponse
    let state_sp_req = Arc::clone(&state);
    let sde_sp_req = Arc::clone(&router.sde);
    router.register_handler(
        "eve_public.corporation.skill.plan.SetSkillRequirementsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let (_cid, corpid) = check_corp_admin_role(ctx, state_sp_req.db.as_ref())?;
            let plan_id = extract_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing skill_plan Identifier.uuid".into()))?;
            let reqs = parse_corp_skill_reqs(dyn_msg.get(2), &sde_sp_req)?;
            {
                let mut plans = state_sp_req.skill_plans.write().unwrap_or_else(|e| e.into_inner());
                get_mut_plan(&mut plans, plan_id, corpid)?.requirements = reqs;
            }
            state_sp_req.sync_to_db();
            queue_corp_sp_notice(ctx, corpid, "SkillRequirementsUpdatedNotice", plan_id, None, None);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.SetSkillRequirementsResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 13. Milestones: GetAllRequest -> GetAllResponse
    let state_ms_all = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporation.skill.plan.milestone.GetAllRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let plan_id = extract_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing skill_plan Identifier.uuid".into()))?;
            let plans = state_ms_all.skill_plans.read().unwrap_or_else(|e| e.into_inner());
            let plan = plans.get(&plan_id).ok_or((STATUS_NOT_FOUND, "Corporation skill plan not found".into()))?;
            if let (Some(cid), Some(db)) = (ctx.effective_character_id(), state_ms_all.db.as_ref()) {
                let caller_corp = db.with_snapshot(|s| s.characters.get(&(cid as i64)).map(|c| c.corporation_id as u32));
                if let Some(c_corp) = caller_corp {
                    if plan.corp_id != 0 && plan.corp_id != c_corp && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                        return Err((STATUS_FORBIDDEN, "Not a member of the skill plan's corporation".into()));
                    }
                }
            }
            let mut out = Vec::new();
            for (mid, desc) in &plan.milestones {
                let mut m_bytes = Vec::new();
                encode_corp_sp_id(1, *mid, &mut m_bytes);
                encode_string_field(2, desc, &mut m_bytes);
                encode_bytes_field(1, &m_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.milestone.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 14. Milestones: CreateRequest -> CreateResponse
    let state_ms_create = Arc::clone(&state);
    let ms_create_fn: crate::quasar_gateway::QuasarHandlerFn = Arc::new(move |ctx, _any, dyn_msg| {
        let (_cid, corpid) = check_corp_admin_role(ctx, state_ms_create.db.as_ref())?;
        let (plan_id, desc) = match dyn_msg.get(1) {
            Some(DynamicValue::Message(m)) => {
                let pid = extract_id(m.get(1)).or_else(|| extract_id(dyn_msg.get(2))).ok_or((STATUS_BAD_REQUEST, "Missing skill_plan Identifier.uuid".into()))?;
                let d = match m.get(4).or_else(|| m.get(2)).or_else(|| m.get(1)) {
                    Some(DynamicValue::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                    _ => return Err((STATUS_BAD_REQUEST, "Milestone description is required".into())),
                };
                (pid, d)
            }
            _ => return Err((STATUS_BAD_REQUEST, "Missing milestone attributes".into())),
        };
        let mid = state_ms_create.alloc_milestone_id();
        {
            let mut plans = state_ms_create.skill_plans.write().unwrap_or_else(|e| e.into_inner());
            get_mut_plan(&mut plans, plan_id, corpid)?.milestones.push((mid, desc.clone()));
        }
        state_ms_create.sync_to_db();
        queue_corp_sp_notice(ctx, corpid, "milestone.CreatedNotice", mid, Some(&desc), None);
        let mut out = Vec::new();
        encode_corp_sp_id(1, mid, &mut out);
        Ok(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.milestone.CreateResponse"),
            value: out,
        })
    });
    router.register_handler("eve_public.corporation.skill.plan.milestone.CreateRequest", ms_create_fn);

    // 15. Milestones: DeleteRequest -> DeleteResponse
    let state_ms_del = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporation.skill.plan.milestone.DeleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let (_cid, corpid) = check_corp_admin_role(ctx, state_ms_del.db.as_ref())?;
            let mid = extract_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing milestone Identifier.uuid".into()))?;
            {
                let mut plans = state_ms_del.skill_plans.write().unwrap_or_else(|e| e.into_inner());
                let found_plan = plans.values_mut().find(|p| p.milestones.iter().any(|(id, _)| *id == mid));
                match found_plan {
                    None => return Err((STATUS_NOT_FOUND, "Milestone not found".into())),
                    Some(p) if p.corp_id != corpid => return Err((STATUS_FORBIDDEN, "Cross-corporation access denied".into())),
                    Some(p) => p.milestones.retain(|(id, _)| *id != mid),
                }
            }
            state_ms_del.sync_to_db();
            queue_corp_sp_notice(ctx, corpid, "milestone.DeletedNotice", mid, None, None);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.milestone.DeleteResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 16. Milestones: SetDescriptionRequest -> SetDescriptionResponse
    let state_ms_desc = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporation.skill.plan.milestone.SetDescriptionRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let (_cid, corpid) = check_corp_admin_role(ctx, state_ms_desc.db.as_ref())?;
            let mid = extract_id(dyn_msg.get(1)).ok_or((STATUS_BAD_REQUEST, "Missing milestone Identifier.uuid".into()))?;
            let desc = match dyn_msg.get(2) {
                Some(DynamicValue::String(s)) if !s.trim().is_empty() => s.trim().to_string(),
                _ => return Err((STATUS_BAD_REQUEST, "Milestone description is required".into())),
            };
            {
                let mut plans = state_ms_desc.skill_plans.write().unwrap_or_else(|e| e.into_inner());
                let found_plan = plans.values_mut().find(|p| p.milestones.iter().any(|(id, _)| *id == mid));
                match found_plan {
                    None => return Err((STATUS_NOT_FOUND, "Milestone not found".into())),
                    Some(p) if p.corp_id != corpid => return Err((STATUS_FORBIDDEN, "Cross-corporation access denied".into())),
                    Some(p) => {
                        for (id, d) in p.milestones.iter_mut() {
                            if *id == mid { *d = desc.clone(); }
                        }
                    }
                }
            }
            state_ms_desc.sync_to_db();
            queue_corp_sp_notice(ctx, corpid, "milestone.DescriptionSetNotice", mid, Some(&desc), None);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.milestone.SetDescriptionResponse"),
                value: Vec::new(),
            })
        }),
    );

    // ========================================================================
    // CORPORATION GOALS (AIR CORPORATE PROJECTS)
    // ========================================================================

    // 37. eve_public.corporationgoal.api.GetAllRequest -> GetAllResponse
    let state_cg_all = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporationgoal.api.GetAllRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let corpid = resolve_corp_id(ctx, state_cg_all.db.as_ref())?;
            let goals = state_cg_all.goals.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for g in goals.values().filter(|g| g.corp_id == corpid) {
                encode_goal_id(1, g.id, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporationgoal.api.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 38. eve_public.corporationgoal.api.GetActiveRequest -> GetActiveResponse
    let state_cg_act = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporationgoal.api.GetActiveRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let corpid = resolve_corp_id(ctx, state_cg_act.db.as_ref())?;
            let goals = state_cg_act.goals.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for g in goals.values().filter(|g| g.corp_id == corpid && g.active) {
                encode_goal_id(1, g.id, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporationgoal.api.GetActiveResponse"),
                value: out,
            })
        }),
    );

    // 39. eve_public.corporationgoal.api.GetInactiveRequest -> GetInactiveResponse
    let state_cg_inact = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporationgoal.api.GetInactiveRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let corpid = resolve_corp_id(ctx, state_cg_inact.db.as_ref())?;
            let goals = state_cg_inact.goals.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for g in goals.values().filter(|g| g.corp_id == corpid && !g.active) {
                encode_goal_id(1, g.id, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporationgoal.api.GetInactiveResponse"),
                value: out,
            })
        }),
    );

    // 40. eve_public.corporationgoal.api.GetContributorSummariesForGoalRequest -> GetContributorSummariesForGoalResponse
    let state_cg_summ = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporationgoal.api.GetContributorSummariesForGoalRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let corpid = resolve_corp_id(ctx, state_cg_summ.db.as_ref())?;
            let goal_id = extract_goal_id(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing corporation goal Identifier".into()))?;
            let goals = state_cg_summ.goals.read().unwrap_or_else(|e| e.into_inner());
            let g = goals
                .get(&goal_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Corporation goal not found".into()))?;
            if g.corp_id != corpid && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                return Err((STATUS_FORBIDDEN, "Cross-corporation access denied".into()));
            }

            let mut out = Vec::new();
            for (cid, (prog, _red)) in &g.contributors {
                let mut s = Vec::new();
                encode_seq_id(1, *cid as u64, &mut s);
                encode_varint_field(2, *prog as u64, &mut s);
                encode_goal_id(3, g.id, &mut s);
                encode_bytes_field(1, &s, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporationgoal.api.GetContributorSummariesForGoalResponse"),
                value: out,
            })
        }),
    );

    // 41. eve_public.corporationgoal.api.GetMyContributorSummaryForGoalRequest -> GetMyContributorSummaryForGoalResponse
    let state_cg_mysumm = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporationgoal.api.GetMyContributorSummaryForGoalRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cid = ctx
                .effective_character_id()
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Character authentication required".into()))?;
            let goal_id = extract_goal_id(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing corporation goal Identifier".into()))?;
            let goals = state_cg_mysumm.goals.read().unwrap_or_else(|e| e.into_inner());
            let goal = goals.get(&goal_id).ok_or((STATUS_NOT_FOUND, "Corporation goal not found".into()))?;

            let mut s = Vec::new();
            encode_seq_id(1, cid as u64, &mut s);
            let prog = goal.contributors.get(&cid).map(|(p, _)| *p).unwrap_or(0);
            encode_varint_field(2, prog as u64, &mut s);
            encode_goal_id(3, goal.id, &mut s);

            let mut out = Vec::new();
            encode_bytes_field(1, &s, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporationgoal.api.GetMyContributorSummaryForGoalResponse"),
                value: out,
            })
        }),
    );

    // 42. eve_public.corporationgoal.api.GetMyContributorSummariesRequest -> GetMyContributorSummariesResponse
    let state_cg_mysumms = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporationgoal.api.GetMyContributorSummariesRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let cid = ctx
                .effective_character_id()
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Character authentication required".into()))?;
            let goals = state_cg_mysumms.goals.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for g in goals.values() {
                if let Some((p, _)) = g.contributors.get(&cid) {
                    let mut s = Vec::new();
                    encode_seq_id(1, cid as u64, &mut s);
                    encode_varint_field(2, *p as u64, &mut s);
                    encode_goal_id(3, g.id, &mut s);
                    encode_bytes_field(1, &s, &mut out);
                }
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporationgoal.api.GetMyContributorSummariesResponse"),
                value: out,
            })
        }),
    );

    // 43. eve_public.corporationgoal.api.GetMineWithRewardsRequest -> GetMineWithRewardsResponse
    let state_cg_rewards = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporationgoal.api.GetMineWithRewardsRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let cid = ctx
                .effective_character_id()
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Character authentication required".into()))?;
            let goals = state_cg_rewards.goals.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for g in goals.values() {
                if let Some((prog, redeemed)) = g.contributors.get(&cid) {
                    if *prog > 0 && !redeemed && g.escrow_isk > 0 {
                        encode_goal_id(1, g.id, &mut out);
                    }
                }
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporationgoal.api.GetMineWithRewardsResponse"),
                value: out,
            })
        }),
    );

    // 44. eve_public.corporationgoal.api.RedeemMyRewardsRequest -> RedeemMyRewardsResponse
    let state_cg_redeem = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporationgoal.api.RedeemMyRewardsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let cid = ctx
                .effective_character_id()
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Character authentication required".into()))?;
            let goal_id = extract_goal_id(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing corporation goal Identifier".into()))?;

            let payout = {
                let mut goals = state_cg_redeem.goals.write().unwrap_or_else(|e| e.into_inner());
                let g = goals
                    .get_mut(&goal_id)
                    .ok_or_else(|| (STATUS_NOT_FOUND, "Corporation goal not found".into()))?;
                let entry_prog = g
                    .contributors
                    .get(&cid)
                    .ok_or_else(|| (STATUS_PRECONDITION_FAILED, "No contributor progress for corporation goal".into()))?;
                if entry_prog.1 || entry_prog.0 <= 0 {
                    return Err((STATUS_PRECONDITION_FAILED, "No unredeemed rewards available for corporation goal".into()));
                }
                let calc_reward = g.calc_reward_isk(entry_prog.0);
                let available = g.escrow_isk.max(0);
                let p = calc_reward.min(available);
                if p <= 0 {
                    return Err((STATUS_PRECONDITION_FAILED, "Corporation goal reward escrow is empty".into()));
                }
                if let Some(entry) = g.contributors.get_mut(&cid) {
                    entry.1 = true;
                }
                g.escrow_isk = g.escrow_isk.saturating_sub(p);
                p
            };

            let Some(ref db) = state_cg_redeem.db else {
                return Err((STATUS_PRECONDITION_FAILED, "Database unavailable for reward payout".into()));
            };
            let reward_cents = eve_db::IskCents::from_isk(payout);
            let mut credited = false;
            db.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
                    if let Some(new_bal) = ch.balance.checked_add(reward_cents) {
                        ch.balance = new_bal;
                        credited = true;
                        snap.append_journal(
                            cid as i64,
                            "CorporationGoalReward",
                            reward_cents,
                            new_bal,
                            format!("Corporation goal {goal_id} reward"),
                        );
                    }
                }
            });
            if !credited {
                let mut goals = state_cg_redeem.goals.write().unwrap_or_else(|e| e.into_inner());
                if let Some(g) = goals.get_mut(&goal_id) {
                    if let Some(entry) = g.contributors.get_mut(&cid) {
                        entry.1 = false;
                    }
                    g.escrow_isk = g.escrow_isk.saturating_add(payout);
                }
                return Err((STATUS_PRECONDITION_FAILED, "Unable to credit corporation goal reward".into()));
            }
            state_cg_redeem.sync_to_db();

            let mut notice_bytes = Vec::new();
            encode_goal_id(1, goal_id, &mut notice_bytes);
            encode_varint_field(2, payout as u64, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Character(cid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporationgoal.api.RedeemedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporationgoal.api.RedeemMyRewardsResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 45. eve_public.corporationgoal.api.RedeemAllMyRewardsRequest -> RedeemAllMyRewardsResponse
    let state_cg_redeem_all = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporationgoal.api.RedeemAllMyRewardsRequest",
        Arc::new(move |ctx, _any, _dyn_msg| {
            let cid = ctx
                .effective_character_id()
                .ok_or_else(|| (STATUS_UNAUTHORIZED, "Character authentication required".into()))?;
            let mut total_payout = 0i64;
            let mut redeemed_goals = Vec::new();
            {
                let mut goals = state_cg_redeem_all.goals.write().unwrap_or_else(|e| e.into_inner());
                for g in goals.values_mut() {
                    let calc_reward = g.contributors.get(&cid).map(|&(prog, red)| if !red && prog > 0 { g.calc_reward_isk(prog) } else { 0 }).unwrap_or(0);
                    if calc_reward > 0 {
                        let available = g.escrow_isk.max(0);
                        let payout = calc_reward.min(available);
                        if payout > 0 {
                            if let Some(entry) = g.contributors.get_mut(&cid) {
                                entry.1 = true;
                            }
                            g.escrow_isk = g.escrow_isk.saturating_sub(payout);
                            total_payout = total_payout.saturating_add(payout);
                            redeemed_goals.push((g.id, payout));
                        }
                    }
                }
            }

            if total_payout > 0 {
                let Some(ref db) = state_cg_redeem_all.db else {
                    return Err((STATUS_PRECONDITION_FAILED, "Database unavailable for reward payout".into()));
                };
                let reward_cents = eve_db::IskCents::from_isk(total_payout);
                let mut credited = false;
                db.with_mut_snapshot(|snap| {
                    if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
                        if let Some(new_bal) = ch.balance.checked_add(reward_cents) {
                            ch.balance = new_bal;
                            credited = true;
                            snap.append_journal(
                                cid as i64,
                                "CorporationGoalReward",
                                reward_cents,
                                new_bal,
                                "Corporation goal rewards redeemed",
                            );
                        }
                    }
                });
                if !credited {
                    let mut goals = state_cg_redeem_all.goals.write().unwrap_or_else(|e| e.into_inner());
                    for (gid, payout) in redeemed_goals {
                        if let Some(g) = goals.get_mut(&gid) {
                            if let Some(entry) = g.contributors.get_mut(&cid) {
                                entry.1 = false;
                            }
                            g.escrow_isk = g.escrow_isk.saturating_add(payout);
                        }
                    }
                    return Err((STATUS_PRECONDITION_FAILED, "Unable to credit corporation goal rewards".into()));
                }
                state_cg_redeem_all.sync_to_db();

                for (gid, payout) in redeemed_goals {
                    let mut notice_bytes = Vec::new();
                    encode_goal_id(1, gid, &mut notice_bytes);
                    encode_varint_field(2, payout as u64, &mut notice_bytes);
                    ctx_queue_notice(
                        NoticeScope::Character(cid),
                        GoogleAny {
                            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporationgoal.api.RedeemedNotice"),
                            value: notice_bytes,
                        },
                        ctx_now_unix(ctx),
                    );
                }
            }

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporationgoal.api.RedeemAllMyRewardsResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 46. eve.corporationgoal.api.GetAllRequest -> GetAllResponse
    let state_cg_internal = Arc::clone(&state);
    router.register_handler(
        "eve.corporationgoal.api.GetAllRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_corpid = resolve_corp_id(ctx, state_cg_internal.db.as_ref())?;
            let corpid = extract_id(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing corporation Identifier".into()))? as u32;
            if corpid != caller_corpid && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                return Err((STATUS_FORBIDDEN, "Cross-corporation access denied".into()));
            }
            let goals = state_cg_internal.goals.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for g in goals.values().filter(|g| g.corp_id == corpid) {
                encode_goal_id(1, g.id, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporationgoal.api.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 47. eve.corporationgoal.api.GetRequest -> GetResponse
    let state_cg_get_int = Arc::clone(&state);
    router.register_handler(
        "eve.corporationgoal.api.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_corpid = resolve_corp_id(ctx, state_cg_get_int.db.as_ref())?;
            let goal_id = extract_goal_id(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing corporation goal Identifier".into()))?;
            let goals = state_cg_get_int.goals.read().unwrap_or_else(|e| e.into_inner());
            let goal = goals
                .get(&goal_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Corporation goal not found".into()))?;
            if goal.corp_id != caller_corpid && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                return Err((STATUS_FORBIDDEN, "Cross-corporation access denied".into()));
            }

            let mut attr = Vec::new();
            encode_string_field(1, &goal.name, &mut attr);
            encode_string_field(2, &goal.description, &mut attr);
            encode_varint_field(3, goal.target_progress as u64, &mut attr);
            encode_varint_field(4, goal.current_progress as u64, &mut attr);

            let mut out = Vec::new();
            encode_bytes_field(1, &attr, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporationgoal.api.GetResponse"),
                value: out,
            })
        }),
    );

    // 48. eve.corporationgoal.api.GetCapacityRequest -> GetCapacityResponse
    let state_cg_cap = Arc::clone(&state);
    router.register_handler(
        "eve.corporationgoal.api.GetCapacityRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let corpid = extract_id(dyn_msg.get(1))
                .map(|v| v as u32)
                .or_else(|| ctx.effective_corporation_id())
                .unwrap_or(0);
            let count = state_cg_cap
                .goals
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .values()
                .filter(|g| g.corp_id == corpid && g.active)
                .count() as u64;
            let mut out = Vec::new();
            encode_varint_field(1, count, &mut out);
            encode_varint_field(2, 20, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporationgoal.api.GetCapacityResponse"),
                value: out,
            })
        }),
    );

    // 49. eve.corporationgoal.contribution.api.GetContributorSummaryForGoalRequest -> GetContributorSummaryForGoalResponse
    let state_cg_csum = Arc::clone(&state);
    router.register_handler(
        "eve.corporationgoal.contribution.api.GetContributorSummaryForGoalRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let caller_corpid = resolve_corp_id(ctx, state_cg_csum.db.as_ref())?;
            let goal_id = extract_goal_id(dyn_msg.get(1))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing corporation goal Identifier".into()))?;
            let cid = extract_id(dyn_msg.get(2))
                .ok_or_else(|| (STATUS_BAD_REQUEST, "Missing character Identifier".into()))? as u32;
            let goals = state_cg_csum.goals.read().unwrap_or_else(|e| e.into_inner());
            let goal = goals
                .get(&goal_id)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Corporation goal not found".into()))?;
            if goal.corp_id != caller_corpid && crate::quasar_gateway::require_admin_role(ctx).is_err() {
                return Err((STATUS_FORBIDDEN, "Cross-corporation access denied".into()));
            }
            let prog = goal.contributors.get(&cid).map(|(p, _)| *p).unwrap_or(0);
            let mut s = Vec::new();
            encode_seq_id(1, cid as u64, &mut s);
            encode_varint_field(2, prog as u64, &mut s);
            encode_goal_id(3, goal.id, &mut s);

            let mut out = Vec::new();
            encode_bytes_field(1, &s, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporationgoal.contribution.api.GetContributorSummaryForGoalResponse"),
                value: out,
            })
        }),
    );
}

fn extract_id_inner(val: Option<&DynamicValue>, is_goal: bool) -> Option<u64> {
    let parse_bytes = |b: &[u8]| -> Option<u64> {
        if b.len() == 16 {
            let valid = if is_goal {
                &b[0..8] == b"CORPGOAL"
            } else {
                &b[0..8] != b"CORPGOAL" && &b[0..4] != b"GOAL"
            };
            valid.then(|| u64::from_be_bytes(b[8..16].try_into().ok().unwrap_or([0u8; 8])))
        } else if b.len() >= 8 {
            Some(u64::from_be_bytes(b[0..8].try_into().ok()?))
        } else {
            b.first().map(|x| *x as u64)
        }
    };
    match val {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Bytes(b)) => parse_bytes(b),
            Some(DynamicValue::Uint64(u)) => Some(*u),
            Some(DynamicValue::Int64(i)) => Some(*i as u64),
            Some(DynamicValue::Uint32(u)) => Some(*u as u64),
            Some(DynamicValue::Int32(i)) => Some(*i as u64),
            _ => None,
        },
        Some(DynamicValue::Bytes(b)) => parse_bytes(b),
        Some(DynamicValue::Uint64(u)) => Some(*u),
        Some(DynamicValue::Int64(i)) => Some(*i as u64),
        Some(DynamicValue::Uint32(u)) => Some(*u as u64),
        Some(DynamicValue::Int32(i)) => Some(*i as u64),
        _ => None,
    }
}

fn extract_id(val: Option<&DynamicValue>) -> Option<u64> {
    extract_id_inner(val, false)
}

fn extract_goal_id(val: Option<&DynamicValue>) -> Option<u64> {
    extract_id_inner(val, true)
}
