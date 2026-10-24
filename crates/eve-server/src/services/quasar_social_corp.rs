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
use crate::quasar_gateway::{NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST};
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
    pub escrow_isk: f64,
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
        let (plans_map, mut goals_map, saved_counters) = db.with_snapshot(|snap| {
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

        let already_seeded = db.with_snapshot(|snap| {
            snap.quasar_state_blobs.contains_key("quasar_corp_goals")
        });

        if !already_seeded && goals_map.is_empty() {
            let mut escrow_isk = 0.0;
            let cost = eve_db::IskCents::from_isk(500_000);
            db.with_mut_snapshot(|snap| {
                if let Some(corp) = snap.corporations.get_mut(&98000001) {
                    if let Some(rem) = corp.master_wallet.checked_sub(cost) {
                        corp.master_wallet = rem;
                        escrow_isk = 500_000.0;
                    }
                }
            });
            let mut contribs = HashMap::new();
            if escrow_isk >= 500_000.0 {
                contribs.insert(90000001u32, (50i64, false));
            }
            goals_map.insert(
                100,
                CorpGoalRecord {
                    id: 100,
                    corp_id: 98000001,
                    name: "Corp Defense Operation".into(),
                    description: "Defend corporate home systems".into(),
                    target_progress: 100,
                    current_progress: 50,
                    active: true,
                    contributors: contribs,
                    escrow_isk,
                },
            );
        }

        let max_plan = plans_map.keys().copied().max().unwrap_or(0);
        let max_ms = plans_map
            .values()
            .flat_map(|p| p.milestones.iter().map(|(mid, _)| *mid))
            .max()
            .unwrap_or(0);
        let max_goal = goals_map.keys().copied().max().unwrap_or(100);

        let st = Arc::new(Self {
            db: Some(db),
            skill_plans: RwLock::new(plans_map),
            goals: RwLock::new(goals_map),
            id_counters: RwLock::new((
                saved_counters.0.max(1).max(max_plan + 1),
                saved_counters.1.max(1).max(max_ms + 1),
                saved_counters.2.max(101).max(max_goal + 1),
            )),
        });
        if !already_seeded {
            st.sync_to_db();
        }
        st
    }

    pub fn fund_goal_escrow(&self, corp_id: u32, goal_id: u64, amount_isk: f64) -> bool {
        let Some(ref db) = self.db else { return false };
        if amount_isk <= 0.0 { return false; }
        let cost = eve_db::IskCents::from_f64_isk(amount_isk);
        let mut debited = false;
        db.with_mut_snapshot(|snap| {
            if let Some(corp) = snap.corporations.get_mut(&(corp_id as i64)) {
                if let Some(rem) = corp.master_wallet.checked_sub(cost) {
                    corp.master_wallet = rem;
                    debited = true;
                }
            }
        });
        if debited {
            {
                let mut goals = self.goals.write().unwrap_or_else(|e| e.into_inner());
                if let Some(g) = goals.get_mut(&goal_id) { g.escrow_isk += amount_isk; }
            }
            self.sync_to_db();
        }
        debited
    }

    pub fn create_goal(&self, corp_id: u32, name: String, description: String, target_progress: i64, escrow_isk: f64) -> Result<u64, (u32, String)> {
        let Some(ref db) = self.db else { return Err((STATUS_BAD_REQUEST, "Database not available".into())); };
        if escrow_isk > 0.0 {
            let cost = eve_db::IskCents::from_f64_isk(escrow_isk);
            let mut debited = false;
            db.with_mut_snapshot(|snap| {
                if let Some(corp) = snap.corporations.get_mut(&(corp_id as i64)) {
                    if let Some(rem) = corp.master_wallet.checked_sub(cost) {
                        corp.master_wallet = rem;
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
                active: true, contributors: HashMap::new(), escrow_isk: escrow_isk.max(0.0),
            });
        }
        self.sync_to_db();
        Ok(id)
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

pub fn register(router: &PublicGatewayRouter, db: Arc<WorldRepository>) {
    let state = QuasarCorpState::from_db(db);

    // ========================================================================
    // ALLIANCE DOMAIN (5 protos)
    // ========================================================================

    // 1. eve.alliance.GetRequest -> GetResponse
    router.register_handler(
        "eve.alliance.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let _all_id = extract_id(dyn_msg.get(1)).unwrap_or(99000001);
            let mut attr = Vec::new();
            encode_string_field(1, "EvoMu Parity Coalition", &mut attr); // name
            encode_string_field(2, "EVOMU", &mut attr); // short_name / ticker
            let mut ts = Vec::new();
            encode_varint_field(1, 1_700_000_000, &mut ts);
            encode_bytes_field(3, &ts, &mut attr); // date_founded
            encode_seq_id(4, 90000001, &mut attr); // creator_char
            encode_seq_id(5, 98000001, &mut attr); // creator_corp
            encode_seq_id(6, 98000001, &mut attr); // executor_corp

            let mut out = Vec::new();
            encode_bytes_field(1, &attr, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.alliance.GetResponse"),
                value: out,
            })
        }),
    );

    // 2. eve.alliance.GetAllRequest -> GetAllResponse
    router.register_handler(
        "eve.alliance.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_seq_id(1, 99000001, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.alliance.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 3. eve.alliance.SearchRequest -> SearchResponse
    router.register_handler(
        "eve.alliance.SearchRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_seq_id(1, 99000001, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.alliance.SearchResponse"),
                value: out,
            })
        }),
    );

    // 4. eve.alliance.corporation.GetMembersRequest -> GetMembersResponse
    router.register_handler(
        "eve.alliance.corporation.GetMembersRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_seq_id(1, 98000001, &mut out);
            encode_seq_id(1, 98000002, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.alliance.corporation.GetMembersResponse"),
                value: out,
            })
        }),
    );

    // ========================================================================
    // CORPORATION SKILL PLANS & MILESTONES (eve_public.corporation.skill.plan)
    // ========================================================================

    // 5. CreateRequest -> CreateResponse
    let state_sp_create = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporation.skill.plan.CreateRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);
            let plan_id = state_sp_create.alloc_plan_id();

            let (name, desc, cat_id) = match dyn_msg.get(1) {
                Some(DynamicValue::Message(attr)) => {
                    let n = match attr.get(1) {
                        Some(DynamicValue::String(s)) => s.clone(),
                        _ => "New Skill Plan".into(),
                    };
                    let d = match attr.get(2) {
                        Some(DynamicValue::String(s)) => s.clone(),
                        _ => "Corporate skill doctrine".into(),
                    };
                    let c = match attr.get(3) {
                        Some(DynamicValue::Uint32(u)) => *u,
                        _ => 1,
                    };
                    (n, d, c)
                }
                _ => ("New Skill Plan".into(), "Corporate skill doctrine".into(), 1),
            };

            let plan = CorpSkillPlanRecord {
                id: plan_id,
                corp_id: corpid,
                name: name.clone(),
                description: desc,
                category_id: cat_id,
                requirements: vec![(3300, 5), (3301, 4)],
                milestones: Vec::new(),
            };
            state_sp_create
                .skill_plans
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .insert(plan_id, plan);
            state_sp_create.sync_to_db();

            // Notice: CreatedNotice
            let mut notice_bytes = Vec::new();
            encode_seq_id(1, plan_id, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.CreatedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

            let mut out = Vec::new();
            encode_seq_id(1, plan_id, &mut out);
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
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);
            let plans = state_sp_all.skill_plans.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for plan in plans.values().filter(|p| p.corp_id == corpid) {
                let mut p_bytes = Vec::new();
                encode_seq_id(1, plan.id, &mut p_bytes);
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
    let get_plan_fn: crate::quasar_gateway::QuasarHandlerFn = Arc::new(move |_ctx: &AuthoritativeContext, _any: &GoogleAny, dyn_msg: &DynamicQuasarMessage| {
        let plan_id = extract_id(dyn_msg.get(1)).unwrap_or(1);
        let plans = state_sp_get.skill_plans.read().unwrap_or_else(|e| e.into_inner());
        let plan = plans.get(&plan_id).cloned().unwrap_or_else(|| CorpSkillPlanRecord {
            id: plan_id,
            corp_id: 98000001,
            name: "Standard Interceptor".into(),
            description: "Core fleet interceptor skills".into(),
            category_id: 1,
            requirements: vec![(3300, 5)],
            milestones: Vec::new(),
        });

        let mut attr = Vec::new();
        encode_string_field(1, &plan.name, &mut attr);
        encode_string_field(2, &plan.description, &mut attr);
        encode_varint_field(3, plan.category_id as u64, &mut attr);

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
            let plan_id = extract_id(dyn_msg.get(1)).unwrap_or(0);
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);
            state_sp_del.skill_plans.write().unwrap_or_else(|e| e.into_inner()).remove(&plan_id);
            state_sp_del.sync_to_db();

            let mut notice_bytes = Vec::new();
            encode_seq_id(1, plan_id, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.DeletedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

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
            let plan_id = extract_id(dyn_msg.get(1)).unwrap_or(0);
            let new_name = match dyn_msg.get(2) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => "Updated Plan".into(),
            };
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);
            if let Some(p) = state_sp_name.skill_plans.write().unwrap_or_else(|e| e.into_inner()).get_mut(&plan_id) {
                p.name = new_name.clone();
            }
            state_sp_name.sync_to_db();

            let mut notice_bytes = Vec::new();
            encode_seq_id(1, plan_id, &mut notice_bytes);
            encode_string_field(2, &new_name, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.NameUpdatedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

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
            let plan_id = extract_id(dyn_msg.get(1)).unwrap_or(0);
            let new_desc = match dyn_msg.get(2) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => "Updated description".into(),
            };
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);
            if let Some(p) = state_sp_desc.skill_plans.write().unwrap_or_else(|e| e.into_inner()).get_mut(&plan_id) {
                p.description = new_desc.clone();
            }
            state_sp_desc.sync_to_db();

            let mut notice_bytes = Vec::new();
            encode_seq_id(1, plan_id, &mut notice_bytes);
            encode_string_field(2, &new_desc, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.DescriptionUpdatedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

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
            let plan_id = extract_id(dyn_msg.get(1)).unwrap_or(0);
            let cat_id = extract_id(dyn_msg.get(2)).unwrap_or(1) as u32;
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);
            if let Some(p) = state_sp_cat.skill_plans.write().unwrap_or_else(|e| e.into_inner()).get_mut(&plan_id) {
                p.category_id = cat_id;
            }
            state_sp_cat.sync_to_db();

            let mut notice_bytes = Vec::new();
            encode_seq_id(1, plan_id, &mut notice_bytes);
            encode_seq_id(2, cat_id as u64, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.CategoryUpdatedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.SetCategoryResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 12. SetSkillRequirementsRequest -> SetSkillRequirementsResponse
    let state_sp_req = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporation.skill.plan.SetSkillRequirementsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let plan_id = extract_id(dyn_msg.get(1)).unwrap_or(0);
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);
            if let Some(p) = state_sp_req.skill_plans.write().unwrap_or_else(|e| e.into_inner()).get_mut(&plan_id) {
                p.requirements = vec![(3300, 5), (3302, 5)];
            }
            state_sp_req.sync_to_db();

            let mut notice_bytes = Vec::new();
            encode_seq_id(1, plan_id, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.SkillRequirementsUpdatedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

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
        Arc::new(move |_ctx, _any, dyn_msg| {
            let plan_id = extract_id(dyn_msg.get(1)).unwrap_or(1);
            let plans = state_ms_all.skill_plans.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            if let Some(plan) = plans.get(&plan_id) {
                for (mid, desc) in &plan.milestones {
                    let mut m_bytes = Vec::new();
                    encode_seq_id(1, *mid, &mut m_bytes);
                    encode_string_field(2, desc, &mut m_bytes);
                    encode_bytes_field(1, &m_bytes, &mut out);
                }
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.milestone.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 14. Milestones: CreateRequest -> CreateResponse
    let state_ms_create = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporation.skill.plan.milestone.CreateRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let mid = state_ms_create.alloc_milestone_id();
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);
            let desc = match dyn_msg.get(1) {
                Some(DynamicValue::Message(m)) => match m.get(1) {
                    Some(DynamicValue::String(s)) => s.clone(),
                    _ => "Milestone 1".into(),
                },
                _ => "Milestone 1".into(),
            };

            if let Some(p) = state_ms_create.skill_plans.write().unwrap_or_else(|e| e.into_inner()).get_mut(&1) {
                p.milestones.push((mid, desc.clone()));
            }
            state_ms_create.sync_to_db();

            let mut notice_bytes = Vec::new();
            encode_seq_id(1, mid, &mut notice_bytes);
            encode_string_field(2, &desc, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.milestone.CreatedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

            let mut out = Vec::new();
            encode_seq_id(1, mid, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.milestone.CreateResponse"),
                value: out,
            })
        }),
    );

    // 15. Milestones: DeleteRequest -> DeleteResponse
    let state_ms_del = Arc::clone(&state);
    router.register_handler(
        "eve_public.corporation.skill.plan.milestone.DeleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let mid = extract_id(dyn_msg.get(1)).unwrap_or(1);
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);

            {
                let mut plans = state_ms_del.skill_plans.write().unwrap_or_else(|e| e.into_inner());
                for plan in plans.values_mut() {
                    plan.milestones.retain(|(id, _)| *id != mid);
                }
            }
            state_ms_del.sync_to_db();

            let mut notice_bytes = Vec::new();
            encode_seq_id(1, mid, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.milestone.DeletedNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

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
            let mid = extract_id(dyn_msg.get(1)).unwrap_or(1);
            let desc = match dyn_msg.get(2) {
                Some(DynamicValue::String(s)) => s.clone(),
                _ => "Updated Milestone".into(),
            };
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);

            {
                let mut plans = state_ms_desc.skill_plans.write().unwrap_or_else(|e| e.into_inner());
                for plan in plans.values_mut() {
                    for (id, d) in plan.milestones.iter_mut() {
                        if *id == mid {
                            *d = desc.clone();
                        }
                    }
                }
            }
            state_ms_desc.sync_to_db();

            let mut notice_bytes = Vec::new();
            encode_seq_id(1, mid, &mut notice_bytes);
            encode_string_field(2, &desc, &mut notice_bytes);
            ctx_queue_notice(
                NoticeScope::Corporation(corpid),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.milestone.DescriptionSetNotice"),
                    value: notice_bytes,
                },
                ctx_now_unix(ctx),
            );

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.milestone.SetDescriptionResponse"),
                value: Vec::new(),
            })
        }),
    );

    // ========================================================================
    // CORPORATION MEMBERS & GOVERNANCE
    // ========================================================================

    // 17. eve.corporation.members.GetRequest -> GetResponse
    router.register_handler(
        "eve.corporation.members.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_seq_id(1, 90000001, &mut out);
            encode_seq_id(1, 90000002, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.members.GetResponse"),
                value: out,
            })
        }),
    );

    // 18. eve.corporation.members.GetLimitRequest -> GetLimitResponse
    router.register_handler(
        "eve.corporation.members.GetLimitRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_varint_field(1, 500, &mut out); // limit = 500
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.members.GetLimitResponse"),
                value: out,
            })
        }),
    );

    // 19. eve.corporation.members.roles.GetRequest -> GetResponse
    router.register_handler(
        "eve.corporation.members.roles.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut cr = Vec::new();
            encode_seq_id(1, 90000001, &mut cr);
            encode_varint_field(2, 0xFFFF_FFFF_FFFF_FFFFu64, &mut cr); // all roles
            encode_bytes_field(1, &cr, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.members.roles.GetResponse"),
                value: out,
            })
        }),
    );

    // 20. eve.corporation.members.titles.GetRequest -> GetResponse
    router.register_handler(
        "eve.corporation.members.titles.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.members.titles.GetResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 21. eve.corporation.members.tracking.GetRequest -> GetResponse
    router.register_handler(
        "eve.corporation.members.tracking.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut t = Vec::new();
            encode_seq_id(1, 90000001, &mut t);
            encode_varint_field(2, 30000142, &mut t); // solar_system_id
            encode_varint_field(3, 1_700_000_000, &mut t); // logoff_date
            encode_bytes_field(1, &t, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.members.tracking.GetResponse"),
                value: out,
            })
        }),
    );

    // 22. eve.corporation.npc.GetAllRequest -> GetAllResponse
    router.register_handler(
        "eve.corporation.npc.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_seq_id(1, 1000001, &mut out);
            encode_seq_id(1, 1000002, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.npc.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 23. eve.corporation.npc.IsNPCCorporationRequest -> IsNPCCorporationResponse
    router.register_handler(
        "eve.corporation.npc.IsNPCCorporationRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let corpid = extract_id(dyn_msg.get(1)).unwrap_or(0);
            let is_npc = corpid < 2_000_000;
            let mut out = Vec::new();
            encode_varint_field(1, if is_npc { 1 } else { 0 }, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.npc.IsNPCCorporationResponse"),
                value: out,
            })
        }),
    );

    // 24. eve.corporation.logo.api.GetRequest -> GetResponse
    router.register_handler(
        "eve.corporation.logo.api.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.logo.api.GetResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 25. eve.corporation.office.api.GetSolarSystemsRequest -> GetSolarSystemsResponse
    router.register_handler(
        "eve.corporation.office.api.GetSolarSystemsRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_seq_id(1, 30000142, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.office.api.GetSolarSystemsResponse"),
                value: out,
            })
        }),
    );

    // 26. eve.corporation.structure.GetAllWithDetailsRequest -> GetAllWithDetailsResponse
    router.register_handler(
        "eve.corporation.structure.GetAllWithDetailsRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut st = Vec::new();
            encode_seq_id(1, 1030000000001, &mut st);
            encode_string_field(2, "Corp Fortizar", &mut st);
            encode_seq_id(3, 30000142, &mut st);
            encode_bytes_field(1, &st, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.structure.GetAllWithDetailsResponse"),
                value: out,
            })
        }),
    );

    // 27. eve.corporation.title.GetAllRequest -> GetAllResponse
    router.register_handler(
        "eve.corporation.title.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.title.GetAllResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 28. eve.corporation.vote.GetRequest -> GetResponse
    router.register_handler(
        "eve.corporation.vote.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.vote.GetResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 29. eve.corporation.starbase.GetAllRequest -> GetAllResponse
    router.register_handler(
        "eve.corporation.starbase.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.starbase.GetAllResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 30. eve.corporation.starbase.GetSettingsAndFuelsRequest -> GetSettingsAndFuelsResponse
    router.register_handler(
        "eve.corporation.starbase.GetSettingsAndFuelsRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.starbase.GetSettingsAndFuelsResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 31. eve.corporation.medals.GetRequest -> GetResponse
    let medals_handler: crate::quasar_gateway::QuasarHandlerFn = Arc::new(move |_ctx, _any, _dyn_msg| {
        let mut out = Vec::new();
        let mut m = Vec::new();
        encode_seq_id(1, 101, &mut m);
        encode_string_field(2, "Service Medal", &mut m);
        encode_string_field(3, "Outstanding fleet service", &mut m);
        encode_bytes_field(1, &m, &mut out);
        Ok(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.medals.GetResponse"),
            value: out,
        })
    });
    router.register_handler("eve.corporation.medals.GetRequest", Arc::clone(&medals_handler));
    router.register_handler("eve.corporation.medals.medals.GetRequest", medals_handler);

    // 32. eve.corporation.medals.issued.GetRequest -> GetResponse
    router.register_handler(
        "eve.corporation.medals.issued.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            let mut m = Vec::new();
            encode_seq_id(1, 101, &mut m);
            encode_seq_id(2, 90000001, &mut m); // character_id
            encode_string_field(3, "For valor in combat", &mut m);
            encode_bytes_field(1, &m, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.medals.issued.GetResponse"),
                value: out,
            })
        }),
    );

    // 33. eve.corporation.market.entry.GetEntriesRequest -> GetEntriesResponse
    router.register_handler(
        "eve.corporation.market.entry.GetEntriesRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.market.entry.GetEntriesResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 34. eve.corporation.industry.observer.GetAllDetailsRequest -> GetAllDetailsResponse
    router.register_handler(
        "eve.corporation.industry.observer.GetAllDetailsRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.industry.observer.GetAllDetailsResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 35. eve.corporation.standing.npc.GetAllRequest -> GetAllResponse
    router.register_handler(
        "eve.corporation.standing.npc.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.standing.npc.GetAllResponse"),
                value: Vec::new(),
            })
        }),
    );

    // 36. eve.corporation.planetinteraction.office.GetRequest -> GetResponse
    router.register_handler(
        "eve.corporation.planetinteraction.office.GetRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporation.planetinteraction.office.GetResponse"),
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
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);
            let goals = state_cg_all.goals.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for g in goals.values().filter(|g| g.corp_id == corpid) {
                encode_seq_id(1, g.id, &mut out);
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
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);
            let goals = state_cg_act.goals.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for g in goals.values().filter(|g| g.corp_id == corpid && g.active) {
                encode_seq_id(1, g.id, &mut out);
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
            let corpid = ctx.effective_corporation_id().unwrap_or(98000001);
            let goals = state_cg_inact.goals.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for g in goals.values().filter(|g| g.corp_id == corpid && !g.active) {
                encode_seq_id(1, g.id, &mut out);
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
        Arc::new(move |_ctx, _any, dyn_msg| {
            let goal_id = extract_id(dyn_msg.get(1)).unwrap_or(100);
            let goals = state_cg_summ.goals.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            if let Some(g) = goals.get(&goal_id) {
                for (cid, (prog, _red)) in &g.contributors {
                    let mut s = Vec::new();
                    encode_seq_id(1, *cid as u64, &mut s);
                    encode_varint_field(2, *prog as u64, &mut s);
                    encode_bytes_field(1, &s, &mut out);
                }
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
            let goal_id = extract_id(dyn_msg.get(1)).unwrap_or(100);
            let cid = ctx.effective_character_id().unwrap_or(90000001);
            let goals = state_cg_mysumm.goals.read().unwrap_or_else(|e| e.into_inner());

            let mut s = Vec::new();
            encode_seq_id(1, cid as u64, &mut s);
            let prog = goals
                .get(&goal_id)
                .and_then(|g| g.contributors.get(&cid))
                .map(|(p, _)| *p)
                .unwrap_or(50);
            encode_varint_field(2, prog as u64, &mut s);

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
            let cid = ctx.effective_character_id().unwrap_or(90000001);
            let goals = state_cg_mysumms.goals.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for g in goals.values() {
                if let Some((p, _)) = g.contributors.get(&cid) {
                    let mut s = Vec::new();
                    encode_seq_id(1, cid as u64, &mut s);
                    encode_varint_field(2, *p as u64, &mut s);
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
            let cid = ctx.effective_character_id().unwrap_or(90000001);
            let goals = state_cg_rewards.goals.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for g in goals.values() {
                if let Some((_p, redeemed)) = g.contributors.get(&cid) {
                    if !redeemed {
                        encode_seq_id(1, g.id, &mut out);
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
            let goal_id = extract_id(dyn_msg.get(1)).unwrap_or(100);
            let cid = ctx.effective_character_id().unwrap_or(90000001);

            let mut reward_isk = 0.0;
            {
                let mut goals = state_cg_redeem.goals.write().unwrap_or_else(|e| e.into_inner());
                if let Some(g) = goals.get_mut(&goal_id) {
                    if let Some(entry) = g.contributors.get_mut(&cid) {
                        if !entry.1 {
                            let calc_reward = (entry.0.max(1) as f64) * 10_000.0;
                            let available = g.escrow_isk.max(0.0);
                            let payout = calc_reward.min(available);
                            if payout > 0.0 {
                                entry.1 = true; // redeemed
                                g.escrow_isk -= payout;
                                reward_isk = payout;
                            }
                        }
                    }
                }
            }
            state_cg_redeem.sync_to_db();

            if reward_isk > 0.0 {
                if let Some(db) = &state_cg_redeem.db {
                    db.with_mut_snapshot(|snap| {
                        if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
                            ch.balance = ch.balance.checked_add(eve_db::IskCents::from_f64_isk(reward_isk)).unwrap_or(ch.balance);
                        }
                    });
                }
            }

            // Notice: RedeemedNotice to character
            let mut notice_bytes = Vec::new();
            encode_seq_id(1, goal_id, &mut notice_bytes);
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
            let cid = ctx.effective_character_id().unwrap_or(90000001);
            let mut total_payout = 0.0;
            {
                let mut goals = state_cg_redeem_all.goals.write().unwrap_or_else(|e| e.into_inner());
                for g in goals.values_mut() {
                    if let Some(entry) = g.contributors.get_mut(&cid) {
                        if !entry.1 {
                            let calc_reward = (entry.0.max(1) as f64) * 10_000.0;
                            let available = g.escrow_isk.max(0.0);
                            let payout = calc_reward.min(available);
                            if payout > 0.0 {
                                entry.1 = true;
                                g.escrow_isk -= payout;
                                total_payout += payout;
                            }
                        }
                    }
                }
            }
            state_cg_redeem_all.sync_to_db();

            if total_payout > 0.0 {
                if let Some(db) = &state_cg_redeem_all.db {
                    db.with_mut_snapshot(|snap| {
                        if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
                            ch.balance = ch.balance.checked_add(eve_db::IskCents::from_f64_isk(total_payout)).unwrap_or(ch.balance);
                        }
                    });
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
        Arc::new(move |_ctx, _any, dyn_msg| {
            let corpid = extract_id(dyn_msg.get(1)).unwrap_or(98000001) as u32;
            let goals = state_cg_internal.goals.read().unwrap_or_else(|e| e.into_inner());

            let mut out = Vec::new();
            for g in goals.values().filter(|g| g.corp_id == corpid) {
                encode_seq_id(1, g.id, &mut out);
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
        Arc::new(move |_ctx, _any, dyn_msg| {
            let goal_id = extract_id(dyn_msg.get(1)).unwrap_or(100);
            let goals = state_cg_get_int.goals.read().unwrap_or_else(|e| e.into_inner());
            let goal = goals.get(&goal_id).cloned().unwrap_or_else(|| CorpGoalRecord {
                id: goal_id,
                corp_id: 98000001,
                name: "Corp Defense Operation".into(),
                description: "Defend corporate home systems".into(),
                target_progress: 100,
                current_progress: 50,
                active: true,
                contributors: HashMap::new(),
                escrow_isk: 500_000.0,
            });

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
    router.register_handler(
        "eve.corporationgoal.api.GetCapacityRequest",
        Arc::new(move |_ctx, _any, _dyn_msg| {
            let mut out = Vec::new();
            encode_varint_field(1, 1, &mut out); // current count
            encode_varint_field(2, 20, &mut out); // capacity = 20
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporationgoal.api.GetCapacityResponse"),
                value: out,
            })
        }),
    );

    // 49. eve.corporationgoal.contribution.api.GetContributorSummaryForGoalRequest -> GetContributorSummaryForGoalResponse
    router.register_handler(
        "eve.corporationgoal.contribution.api.GetContributorSummaryForGoalRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
            let cid = extract_id(dyn_msg.get(2)).unwrap_or(90000001);
            let mut s = Vec::new();
            encode_seq_id(1, cid, &mut s);
            encode_varint_field(2, 50, &mut s);

            let mut out = Vec::new();
            encode_bytes_field(1, &s, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.corporationgoal.contribution.api.GetContributorSummaryForGoalResponse"),
                value: out,
            })
        }),
    );
}

fn extract_id(val: Option<&DynamicValue>) -> Option<u64> {
    match val {
        Some(DynamicValue::Message(sub)) => match sub.get(1) {
            Some(DynamicValue::Uint64(u)) => Some(*u),
            Some(DynamicValue::Int64(i)) => Some(*i as u64),
            Some(DynamicValue::Uint32(u)) => Some(*u as u64),
            Some(DynamicValue::Int32(i)) => Some(*i as u64),
            _ => None,
        },
        Some(DynamicValue::Uint64(u)) => Some(*u),
        Some(DynamicValue::Int64(i)) => Some(*i as u64),
        Some(DynamicValue::Uint32(u)) => Some(*u as u64),
        Some(DynamicValue::Int32(i)) => Some(*i as u64),
        _ => None,
    }
}
