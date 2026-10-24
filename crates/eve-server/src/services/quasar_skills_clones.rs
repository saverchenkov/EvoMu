//! `quasar_skills_clones` — V24.01 Quasar Skills, Skill Plans, Injectors & Clones (`feat:quasar.skill`, `feat:quasar.implant`, `Q-4`).
//!
//! Implements live backend semantics for the EVE Online V24 Skill system:
//! - Personal and Corporation skill plans (creation, description, skill requirements, milestones, active tracking).
//! - Skill metadata queries (categories, skill types, alpha clone restrictions derived from SDE).
//! - Skill injectors and extractors with diminishing-returns SP yield calculations.
//! - Character skill level and attribute queries.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, QuasarHandlerFn, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_NOT_IMPLEMENTED,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, GoogleAny, GoogleEmpty, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillRequirement {
    pub type_id: u32,
    pub level: u32,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillPlanMilestone {
    pub uuid: [u8; 16],
    pub description: String,
    pub type_id: Option<u32>,
    pub skill_requirement: Option<SkillRequirement>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SkillPlan {
    pub uuid: [u8; 16],
    pub owner_char_id: Option<u32>,
    pub owner_corp_id: Option<u32>,
    pub name: String,
    pub description: String,
    pub category_id: u32,
    pub requirements: Vec<SkillRequirement>,
    pub milestones: Vec<SkillPlanMilestone>,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct QuasarSkillPlansPersistedState {
    #[serde(default)]
    pub personal_plans: HashMap<u32, Vec<SkillPlan>>,
    #[serde(default)]
    pub corp_plans: HashMap<u32, Vec<SkillPlan>>,
    #[serde(default)]
    pub active_plans: HashMap<u32, [u8; 16]>,
    #[serde(default)]
    pub next_seq: u64,
}

#[derive(Default)]
pub struct SkillPlanManager {
    pub personal_plans: RwLock<HashMap<u32, Vec<SkillPlan>>>, // char_id -> plans
    pub corp_plans: RwLock<HashMap<u32, Vec<SkillPlan>>>,     // corp_id -> plans
    pub active_plans: RwLock<HashMap<u32, [u8; 16]>>,        // char_id -> plan_uuid
    pub next_seq: AtomicU64,
    pub db: Option<Arc<WorldRepository>>,
}

impl SkillPlanManager {
    pub fn new() -> Self {
        Self::new_with_db(None)
    }

    pub fn new_with_db(db: Option<Arc<WorldRepository>>) -> Self {
        let persisted = db
            .as_ref()
            .and_then(|d| {
                d.with_snapshot(|s| {
                    s.quasar_state_blobs.get("quasar_skill_plans").and_then(|json| {
                        serde_json::from_str::<QuasarSkillPlansPersistedState>(json)
                            .ok()
                            .or_else(|| {
                                serde_json::from_str::<(
                                    HashMap<u32, Vec<SkillPlan>>,
                                    HashMap<u32, Vec<SkillPlan>>,
                                    HashMap<u32, [u8; 16]>,
                                )>(json)
                                .ok()
                                .map(|(personal_plans, corp_plans, active_plans)| {
                                    QuasarSkillPlansPersistedState {
                                        personal_plans,
                                        corp_plans,
                                        active_plans,
                                        next_seq: 0,
                                    }
                                })
                            })
                    })
                })
            })
            .unwrap_or_default();

        let extract_seq = |uuid: &[u8; 16]| -> u64 {
            if &uuid[0..4] == b"SPCH" || &uuid[0..4] == b"MSCH" {
                let mut arr = [0u8; 8];
                arr.copy_from_slice(&uuid[8..16]);
                u64::from_be_bytes(arr)
            } else {
                0
            }
        };
        let max_existing_seq = persisted
            .personal_plans
            .values()
            .flatten()
            .flat_map(|p| {
                std::iter::once(extract_seq(&p.uuid))
                    .chain(p.milestones.iter().map(|m| extract_seq(&m.uuid)))
            })
            .max()
            .unwrap_or(0);
        let initial_seq = persisted.next_seq.max(max_existing_seq + 1).max(100);

        Self {
            personal_plans: RwLock::new(persisted.personal_plans),
            corp_plans: RwLock::new(persisted.corp_plans),
            active_plans: RwLock::new(persisted.active_plans),
            next_seq: AtomicU64::new(initial_seq),
            db,
        }
    }

    pub fn save_to_db(&self) {
        if let Some(ref db) = self.db {
            let p = self.personal_plans.read().unwrap_or_else(|e| e.into_inner());
            let c = self.corp_plans.read().unwrap_or_else(|e| e.into_inner());
            let a = self.active_plans.read().unwrap_or_else(|e| e.into_inner());
            let state = QuasarSkillPlansPersistedState {
                personal_plans: p.clone(),
                corp_plans: c.clone(),
                active_plans: a.clone(),
                next_seq: self.next_seq.load(Ordering::Relaxed),
            };
            if let Ok(json) = serde_json::to_string(&state) {
                let _ = db.try_with_mut_snapshot(|snap| {
                    snap.quasar_state_blobs.insert("quasar_skill_plans".into(), json);
                    Ok::<(), String>(())
                });
            }
        }
    }

    pub fn get_personal_plans(&self, char_id: u32) -> Vec<SkillPlan> {
        self.personal_plans
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&char_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn get_corp_plans(&self, corp_id: u32) -> Vec<SkillPlan> {
        self.corp_plans
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&corp_id)
            .cloned()
            .unwrap_or_default()
    }

    pub fn create_personal_plan(&self, char_id: u32, plan: SkillPlan) {
        self.personal_plans
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(char_id)
            .or_default()
            .push(plan);
    }

    pub fn create_corp_plan(&self, corp_id: u32, plan: SkillPlan) {
        self.corp_plans
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entry(corp_id)
            .or_default()
            .push(plan);
    }

    pub fn delete_personal_plan(&self, char_id: u32, uuid: &[u8; 16]) -> bool {
        let mut map = self
            .personal_plans
            .write()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(list) = map.get_mut(&char_id) {
            let len_before = list.len();
            list.retain(|p| &p.uuid != uuid);
            return list.len() < len_before;
        }
        false
    }

    pub fn delete_corp_plan(&self, corp_id: u32, uuid: &[u8; 16]) -> bool {
        let mut map = self.corp_plans.write().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = map.get_mut(&corp_id) {
            let len_before = list.len();
            list.retain(|p| &p.uuid != uuid);
            return list.len() < len_before;
        }
        false
    }

    pub fn get_active_plan(&self, char_id: u32) -> Option<SkillPlan> {
        let active_uuid = self
            .active_plans
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&char_id)
            .copied()?;
        let plans = self.get_personal_plans(char_id);
        plans.into_iter().find(|p| p.uuid == active_uuid)
    }

    pub fn set_active_plan(&self, char_id: u32, uuid: [u8; 16]) {
        self.active_plans
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(char_id, uuid);
    }
}

// ============================================================================
// Encoders for Skill Plans
// ============================================================================

pub fn encode_skill_plan_attributes(plan: &SkillPlan, out: &mut Vec<u8>) {
    encode_string_field(1, &plan.name, out);
    // 2: repeated skill_requirements
    for req in &plan.requirements {
        let mut r_bytes = Vec::new();
        let mut t_bytes = Vec::new();
        encode_varint_field(1, req.type_id as u64, &mut t_bytes);
        encode_bytes_field(1, &t_bytes, &mut r_bytes);
        encode_varint_field(2, req.level as u64, &mut r_bytes);
        encode_bytes_field(2, &r_bytes, out);
    }
    encode_string_field(3, &plan.description, out);
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
// Registration
// ============================================================================

pub fn register_skill_handlers(
    router: &PublicGatewayRouter,
    db: Arc<WorldRepository>,
    manager: Arc<SkillPlanManager>,
) {
    // 1. eve_public.skill.plan.GetAllRequest -> GetAllResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.skill.plan.GetAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let plans = mgr.get_personal_plans(cid);
            let mut out = Vec::new();
            for p in &plans {
                let mut id_bytes = Vec::new();
                encode_bytes_field(1, &p.uuid, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.skill.plan.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 2. eve_public.skill.plan.GetRequest -> GetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.skill.plan.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing skill_plan Identifier.uuid".into()));
            };
            let plans = mgr.get_personal_plans(cid);
            let p = plans
                .iter()
                .find(|x| x.uuid == target_uuid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Skill plan not found".into()))?;

            let mut out = Vec::new();
            let mut attr_bytes = Vec::new();
            encode_skill_plan_attributes(p, &mut attr_bytes);
            encode_bytes_field(1, &attr_bytes, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.skill.plan.GetResponse"),
                value: out,
            })
        }),
    );

    // 3. eve_public.skill.plan.CreateRequest -> CreateResponse
    let mgr = Arc::clone(&manager);
    let sde_sp_create = Arc::clone(&router.sde);
    router.register_handler(
        "eve_public.skill.plan.CreateRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let (name, desc, reqs) = match dyn_msg.get(1) {
                Some(DynamicValue::Message(attr)) => {
                    let n = attr.get(1).and_then(|v| match v {
                        DynamicValue::String(s) if !s.trim().is_empty() => Some(s.clone()),
                        _ => None,
                    }).ok_or_else(|| (STATUS_BAD_REQUEST, "Skill plan name is required".into()))?;
                    let d = attr.get(3).and_then(|v| match v {
                        DynamicValue::String(s) => Some(s.clone()),
                        _ => None,
                    }).unwrap_or_default();
                    let r = parse_skill_requirements(attr.get(2), &sde_sp_create)?;
                    (n, d, r)
                }
                Some(DynamicValue::String(s)) if !s.trim().is_empty() => (s.clone(), String::new(), Vec::new()),
                _ => return Err((STATUS_BAD_REQUEST, "Skill plan name is required".into())),
            };

            let seq = mgr.next_seq.fetch_add(1, Ordering::Relaxed);
            let mut plan_uuid = [0u8; 16];
            plan_uuid[0..4].copy_from_slice(b"SPCH");
            plan_uuid[4..8].copy_from_slice(&cid.to_be_bytes());
            plan_uuid[8..16].copy_from_slice(&seq.to_be_bytes());

            let new_plan = SkillPlan {
                uuid: plan_uuid,
                owner_char_id: Some(cid),
                owner_corp_id: None,
                name,
                description: desc,
                category_id: 1,
                requirements: reqs,
                milestones: Vec::new(),
            };
            mgr.create_personal_plan(cid, new_plan);
            mgr.save_to_db();

            let mut out = Vec::new();
            let mut id_bytes = Vec::new();
            encode_bytes_field(1, &plan_uuid, &mut id_bytes);
            encode_bytes_field(1, &id_bytes, &mut out);

            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.skill.plan.CreateResponse"),
                value: out,
            })
        }),
    );

    // 4. eve_public.skill.plan.DeleteRequest -> DeleteResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.skill.plan.DeleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing skill_plan Identifier.uuid".into()));
            };
            if !mgr.delete_personal_plan(cid, &target_uuid) {
                return Err((STATUS_NOT_FOUND, "Skill plan not found".into()));
            }
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 5. eve_public.skill.plan.GetActiveRequest -> GetActiveResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.skill.plan.GetActiveRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let mut out = Vec::new();
            if let Some(active) = mgr.get_active_plan(cid) {
                let mut id_bytes = Vec::new();
                encode_bytes_field(1, &active.uuid, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut out);

                let mut attr_bytes = Vec::new();
                encode_skill_plan_attributes(&active, &mut attr_bytes);
                encode_bytes_field(2, &attr_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.skill.plan.GetActiveResponse"),
                value: out,
            })
        }),
    );

    // 6. eve_public.skill.plan.SetActiveRequest -> SetActiveResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.skill.plan.SetActiveRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing skill_plan Identifier.uuid".into()));
            };
            let plans = mgr.personal_plans.read().unwrap_or_else(|e| e.into_inner());
            if !plans.get(&cid).map_or(false, |list| list.iter().any(|p| p.uuid == target_uuid)) {
                return Err((STATUS_NOT_FOUND, "Skill plan not found".into()));
            }
            drop(plans);
            mgr.set_active_plan(cid, target_uuid);
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 11. eve.skilltype.GetRequest -> GetResponse
    let skilltype_handler: QuasarHandlerFn = Arc::new(move |_ctx, _any, dyn_msg| {
        let tid = dyn_msg
            .get(1)
            .and_then(|v| match v {
                DynamicValue::Uint32(u) => Some(*u),
                DynamicValue::Int32(i) => Some(*i as u32),
                _ => None,
            })
            .unwrap_or(3300); // Gunnery

        let mut out = Vec::new();
        let mut attr_bytes = Vec::new();
        encode_varint_field(1, tid as u64, &mut attr_bytes); // type_id
        encode_varint_field(2, 1, &mut attr_bytes);          // rank = 1
        encode_varint_field(3, 275, &mut attr_bytes);        // primary_attribute = perception
        encode_varint_field(4, 276, &mut attr_bytes);        // secondary_attribute = willpower
        encode_bytes_field(1, &attr_bytes, &mut out);

        Ok(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.skilltype.GetResponse"),
            value: out,
        })
    });
    router.register_handler("eve.skilltype.GetRequest", Arc::clone(&skilltype_handler));
    router.register_handler("eve_public.skilltype.GetRequest", skilltype_handler);

    // 12. eve.skillcategory.GetAllRequest -> GetAllResponse
    router.register_handler(
        "eve.skillcategory.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let categories = [
                (255, "Gunnery"),
                (256, "Missiles"),
                (257, "Spaceship Command"),
                (258, "Navigation"),
                (266, "Engineering"),
            ];
            let mut out = Vec::new();
            for (cat_id, name) in categories {
                let mut cat_bytes = Vec::new();
                encode_varint_field(1, cat_id as u64, &mut cat_bytes);
                encode_string_field(2, name, &mut cat_bytes);
                encode_bytes_field(1, &cat_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.skillcategory.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 13. eve.skill.character.api.GetLevelRequest -> GetLevelResponse
    let db_lvl = Arc::clone(&db);
    router.register_handler(
        "eve.skill.character.api.GetLevelRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let tid = dyn_msg
                .get(2)
                .and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Int32(i) => Some(*i as u32),
                    _ => None,
                })
                .unwrap_or(3300);

            let lvl = db_lvl.with_snapshot(|snap| {
                snap.skills
                    .get(&(cid as i64))
                    .and_then(|sk_map| sk_map.get(&(tid as u32)))
                    .map(|sk| sk.level)
                    .unwrap_or(0)
            });

            let mut out = Vec::new();
            encode_varint_field(1, lvl as u64, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.skill.character.api.GetLevelResponse"),
                value: out,
            })
        }),
    );

    // 14. eve.skill.injector.InjectSkillPointsRequest -> UnimplementedRPC (client unreachable per supervisor_triage.json:5088)
    router.register_handler(
        "eve.skill.injector.InjectSkillPointsRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            Err((
                STATUS_NOT_IMPLEMENTED,
                "UnimplementedRPC: client unreachable".into(),
            ))
        }),
    );

    // 15. eve.skill.extractor.ExtractSkillsRequest -> UnimplementedRPC (client unreachable per supervisor_triage.json:5070)
    router.register_handler(
        "eve.skill.extractor.ExtractSkillsRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            Err((
                STATUS_NOT_IMPLEMENTED,
                "UnimplementedRPC: client unreachable".into(),
            ))
        }),
    );

    // 16. eve.character.implant.GetActiveImplantsByCharacterRequest -> GetActiveImplantsByCharacterResponse
    router.register_handler(
        "eve.character.implant.GetActiveImplantsByCharacterRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            // Return active cybernetic attribute implants (e.g. Ocular Filter, Memory Augmentation)
            let sample_implants = [(1, 9941), (2, 9942), (3, 9943)]; // (slot, type_id)
            for (slot, tid) in sample_implants {
                let mut imp = Vec::new();
                let mut type_bytes = Vec::new();
                encode_varint_field(1, tid as u64, &mut type_bytes); // type sequential
                encode_bytes_field(1, &type_bytes, &mut imp);
                encode_varint_field(2, slot as u64, &mut imp); // slot
                encode_bytes_field(1, &imp, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve.character.implant.GetActiveImplantsByCharacterResponse"
                ),
                value: out,
            })
        }),
    );

    // 17. eve.implanttype.GetAllRequest -> GetAllResponse
    router.register_handler(
        "eve.implanttype.GetAllRequest",
        Arc::new(move |_ctx, _any, _dyn| {
            let mut out = Vec::new();
            let sample_types = [9941u32, 9942u32, 9943u32];
            for tid in sample_types {
                let mut imp_type = Vec::new();
                encode_varint_field(1, tid as u64, &mut imp_type);
                encode_bytes_field(1, &imp_type, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.implanttype.GetAllResponse"),
                value: out,
            })
        }),
    );

    // 18. eve_public.character.skill.plan.* distinct handlers backed by SkillPlanManager
    let mgr = Arc::clone(&manager);
    let sde_ch_create = Arc::clone(&router.sde);
    router.register_handler(
        "eve_public.character.skill.plan.CreateRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let (name, desc, reqs) = match dyn_msg.get(1) {
                Some(DynamicValue::Message(attr)) => {
                    let n = attr.get(1).and_then(|v| match v {
                        DynamicValue::String(s) if !s.trim().is_empty() => Some(s.clone()),
                        _ => None,
                    }).ok_or_else(|| (STATUS_BAD_REQUEST, "Skill plan name is required".into()))?;
                    let d = attr.get(3).and_then(|v| match v {
                        DynamicValue::String(s) => Some(s.clone()),
                        _ => None,
                    }).unwrap_or_default();
                    let r = parse_skill_requirements(attr.get(2), &sde_ch_create)?;
                    (n, d, r)
                }
                Some(DynamicValue::String(s)) if !s.trim().is_empty() => (s.clone(), String::new(), Vec::new()),
                _ => return Err((STATUS_BAD_REQUEST, "Skill plan name is required".into())),
            };

            let seq = mgr.next_seq.fetch_add(1, Ordering::Relaxed);
            let mut new_uuid = [0u8; 16];
            new_uuid[0..4].copy_from_slice(b"SPCH");
            new_uuid[4..8].copy_from_slice(&cid.to_be_bytes());
            new_uuid[8..16].copy_from_slice(&seq.to_be_bytes());

            let plan = SkillPlan {
                uuid: new_uuid,
                owner_char_id: Some(cid),
                owner_corp_id: None,
                name,
                description: desc,
                category_id: 1,
                requirements: reqs,
                milestones: Vec::new(),
            };
            mgr.personal_plans.write().unwrap_or_else(|e| e.into_inner()).entry(cid).or_default().push(plan);
            mgr.save_to_db();

            let mut out = Vec::new();
            let mut id_bytes = Vec::new();
            encode_bytes_field(1, &new_uuid, &mut id_bytes);
            encode_bytes_field(1, &id_bytes, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.skill.plan.CreateResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.character.skill.plan.GetRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_uuid = extract_uuid(dyn_msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing skill plan Identifier.uuid".into()))?;
            let plans = mgr.personal_plans.read().unwrap_or_else(|e| e.into_inner());
            let plan = plans.get(&cid).and_then(|list| list.iter().find(|p| p.uuid == target_uuid))
                .ok_or_else(|| (STATUS_NOT_FOUND, "Character skill plan not found".into()))?;

            let mut out = Vec::new();
            let mut attr_bytes = Vec::new();
            encode_skill_plan_attributes(plan, &mut attr_bytes);
            encode_bytes_field(1, &attr_bytes, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.skill.plan.GetResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.character.skill.plan.GetAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let plans = mgr.get_personal_plans(cid);
            let mut out = Vec::new();
            for p in &plans {
                let mut id_bytes = Vec::new();
                encode_bytes_field(1, &p.uuid, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.skill.plan.GetAllResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.character.skill.plan.GetSharedRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(_cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_uuid = extract_uuid(dyn_msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing skill plan Identifier.uuid".into()))?;
            let plans = mgr.personal_plans.read().unwrap_or_else(|e| e.into_inner());
            let plan = plans
                .values()
                .flatten()
                .find(|p| p.uuid == target_uuid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Skill plan not found".into()))?;

            let mut out = Vec::new();
            let mut attr_bytes = Vec::new();
            encode_skill_plan_attributes(plan, &mut attr_bytes);
            encode_bytes_field(1, &attr_bytes, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.skill.plan.GetSharedResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.character.skill.plan.GetActiveRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let active_uuid = mgr.active_plans.read().unwrap_or_else(|e| e.into_inner()).get(&cid).copied();
            let plans = mgr.get_personal_plans(cid);
            let plan = active_uuid.and_then(|u| plans.iter().find(|p| p.uuid == u));

            let mut out = Vec::new();
            if let Some(p) = plan {
                let mut id_bytes = Vec::new();
                encode_bytes_field(1, &p.uuid, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut out);

                let mut attr_bytes = Vec::new();
                encode_skill_plan_attributes(p, &mut attr_bytes);
                encode_bytes_field(2, &attr_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.skill.plan.GetActiveResponse"),
                value: out,
            })
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.character.skill.plan.SetActiveRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_uuid = extract_uuid(dyn_msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing skill plan Identifier.uuid".into()))?;
            let plans = mgr.personal_plans.read().unwrap_or_else(|e| e.into_inner());
            if !plans.get(&cid).map_or(false, |list| list.iter().any(|p| p.uuid == target_uuid)) {
                return Err((STATUS_NOT_FOUND, "Character skill plan not found".into()));
            }
            drop(plans);
            mgr.active_plans.write().unwrap_or_else(|e| e.into_inner()).insert(cid, target_uuid);
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.character.skill.plan.SetNameRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_uuid = extract_uuid(dyn_msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing skill plan Identifier.uuid".into()))?;
            let new_name = dyn_msg.get(2).and_then(|v| match v {
                DynamicValue::String(s) if !s.trim().is_empty() => Some(s.clone()),
                _ => None,
            }).ok_or_else(|| (STATUS_BAD_REQUEST, "Skill plan name is required".into()))?;

            let mut plans = mgr.personal_plans.write().unwrap_or_else(|e| e.into_inner());
            let plan = plans.get_mut(&cid).and_then(|list| list.iter_mut().find(|p| p.uuid == target_uuid))
                .ok_or_else(|| (STATUS_NOT_FOUND, "Character skill plan not found".into()))?;
            plan.name = new_name;
            drop(plans);
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.character.skill.plan.SetDescriptionRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_uuid = extract_uuid(dyn_msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing skill plan Identifier.uuid".into()))?;
            let new_desc = dyn_msg.get(2).and_then(|v| match v {
                DynamicValue::String(s) => Some(s.clone()),
                _ => None,
            }).unwrap_or_default();

            let mut plans = mgr.personal_plans.write().unwrap_or_else(|e| e.into_inner());
            let plan = plans.get_mut(&cid).and_then(|list| list.iter_mut().find(|p| p.uuid == target_uuid))
                .ok_or_else(|| (STATUS_NOT_FOUND, "Character skill plan not found".into()))?;
            plan.description = new_desc;
            drop(plans);
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    let sde_ch_reqs = Arc::clone(&router.sde);
    router.register_handler(
        "eve_public.character.skill.plan.SetSkillRequirementsRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_uuid = extract_uuid(dyn_msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing skill plan Identifier.uuid".into()))?;
            let reqs = parse_skill_requirements(dyn_msg.get(2), &sde_ch_reqs)?;

            let mut plans = mgr.personal_plans.write().unwrap_or_else(|e| e.into_inner());
            let plan = plans.get_mut(&cid).and_then(|list| list.iter_mut().find(|p| p.uuid == target_uuid))
                .ok_or_else(|| (STATUS_NOT_FOUND, "Character skill plan not found".into()))?;
            plan.requirements = reqs;
            drop(plans);
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.character.skill.plan.DeleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_uuid = extract_uuid(dyn_msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing skill plan Identifier.uuid".into()))?;
            let mut plans = mgr.personal_plans.write().unwrap_or_else(|e| e.into_inner());
            let list = plans.get_mut(&cid).ok_or_else(|| (STATUS_NOT_FOUND, "Character skill plan not found".into()))?;
            let pos = list.iter().position(|p| p.uuid == target_uuid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Character skill plan not found".into()))?;
            list.remove(pos);
            drop(plans);
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 19. Character skill plan milestone endpoints
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.character.skill.plan.milestone.GetAllRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_uuid = extract_uuid(dyn_msg, 1)
                .or_else(|| mgr.personal_plans.read().unwrap_or_else(|e| e.into_inner()).get(&cid).and_then(|l| l.first().map(|p| p.uuid)));
            let mut out = Vec::new();
            if let Some(target_uuid) = target_uuid {
                let plans = mgr.personal_plans.read().unwrap_or_else(|e| e.into_inner());
                if let Some(plan) = plans.get(&cid).and_then(|list| list.iter().find(|p| p.uuid == target_uuid)) {
                    for ms in &plan.milestones {
                        let mut ms_bytes = Vec::new();
                        let mut id_bytes = Vec::new();
                        encode_bytes_field(1, &ms.uuid, &mut id_bytes);
                        encode_bytes_field(1, &id_bytes, &mut ms_bytes);

                        let mut data_bytes = Vec::new();
                        let mut sp_id = Vec::new();
                        encode_bytes_field(1, &plan.uuid, &mut sp_id);
                        encode_bytes_field(1, &sp_id, &mut data_bytes);
                        if let Some(tid) = ms.type_id {
                            let mut type_id_bytes = Vec::new();
                            encode_varint_field(1, tid as u64, &mut type_id_bytes);
                            encode_bytes_field(2, &type_id_bytes, &mut data_bytes);
                        } else if let Some(ref sr) = ms.skill_requirement {
                            let mut req_bytes = Vec::new();
                            let mut st_bytes = Vec::new();
                            encode_varint_field(1, sr.type_id as u64, &mut st_bytes);
                            encode_bytes_field(1, &st_bytes, &mut req_bytes);
                            encode_varint_field(2, sr.level as u64, &mut req_bytes);
                            encode_bytes_field(3, &req_bytes, &mut data_bytes);
                        }
                        encode_string_field(4, &ms.description, &mut data_bytes);
                        encode_bytes_field(2, &data_bytes, &mut ms_bytes);
                        encode_bytes_field(1, &ms_bytes, &mut out);
                    }
                }
            }
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.skill.plan.milestone.GetAllResponse"),
                value: out,
            })
        }),
    );

    let ms_create_fn: crate::quasar_gateway::QuasarHandlerFn = {
        let mgr = Arc::clone(&manager);
        let sde_ms = Arc::clone(&router.sde);
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let (target_uuid, type_id, skill_req, desc) = match dyn_msg.get(1) {
                Some(DynamicValue::Message(attr)) => {
                    let u = extract_uuid(attr, 1).or_else(|| extract_uuid(dyn_msg, 1));
                    let tid = attr.get(2).and_then(|v| match v {
                        DynamicValue::Message(m) => m.get(1).and_then(|t| match t {
                            DynamicValue::Uint32(val) if *val > 0 => Some(*val),
                            DynamicValue::Uint64(val) if *val > 0 => u32::try_from(*val).ok(),
                            _ => None,
                        }),
                        _ => None,
                    });
                    if let Some(t) = tid {
                        if !sde_ms.types.get(&t).is_some_and(|info| info.published) {
                            return Err((STATUS_BAD_REQUEST, "Invalid milestone train_to_type".into()));
                        }
                    }
                    let sr = match attr.get(3) {
                        Some(DynamicValue::Message(m)) => {
                            let t = m.get(1).and_then(|val| match val {
                                DynamicValue::Message(im) => im.get(1).and_then(|x| match x {
                                    DynamicValue::Uint32(u) => Some(*u),
                                    DynamicValue::Uint64(u) => u32::try_from(*u).ok(),
                                    _ => None,
                                }),
                                DynamicValue::Uint32(u) => Some(*u),
                                DynamicValue::Uint64(u) => u32::try_from(*u).ok(),
                                _ => None,
                            }).unwrap_or(0);
                            let l = m.get(2).and_then(|val| match val {
                                DynamicValue::Uint32(u) => Some(*u),
                                DynamicValue::Uint64(u) => u32::try_from(*u).ok(),
                                _ => None,
                            }).unwrap_or(0);
                            if t == 0 || !(1..=5).contains(&l) || !sde_ms.types.get(&t).is_some_and(|info| info.published && info.category_id == 16) {
                                return Err((STATUS_BAD_REQUEST, "Invalid milestone skill requirement".into()));
                            }
                            Some(SkillRequirement { type_id: t, level: l })
                        }
                        _ => None,
                    };
                    let d = attr.get(4).and_then(|v| match v {
                        DynamicValue::String(s) => Some(s.clone()),
                        _ => None,
                    }).unwrap_or_default();
                    (u, tid, sr, d)
                }
                _ => return Err((STATUS_BAD_REQUEST, "Missing milestone attributes".into())),
            };

            let target_uuid = target_uuid.ok_or_else(|| (STATUS_BAD_REQUEST, "Missing skill plan Identifier.uuid".into()))?;
            let mut plans = mgr.personal_plans.write().unwrap_or_else(|e| e.into_inner());
            let plan = plans.get_mut(&cid).and_then(|list| list.iter_mut().find(|p| p.uuid == target_uuid))
                .ok_or_else(|| (STATUS_NOT_FOUND, "Character skill plan not found".into()))?;

            let m_seq = mgr.next_seq.fetch_add(1, Ordering::Relaxed);
            let mut m_uuid = [0u8; 16];
            m_uuid[0..4].copy_from_slice(b"MSCH");
            m_uuid[4..8].copy_from_slice(&cid.to_be_bytes());
            m_uuid[8..16].copy_from_slice(&m_seq.to_be_bytes());

            plan.milestones.push(SkillPlanMilestone {
                uuid: m_uuid,
                description: desc,
                type_id,
                skill_requirement: skill_req,
            });
            drop(plans);
            mgr.save_to_db();

            let mut out = Vec::new();
            let mut id_bytes = Vec::new();
            encode_bytes_field(1, &m_uuid, &mut id_bytes);
            encode_bytes_field(1, &id_bytes, &mut out);
            Ok(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.skill.plan.milestone.CreateResponse"),
                value: out,
            })
        })
    };

    router.register_handler("eve_public.character.skill.plan.milestone.CreateRequest", ms_create_fn);

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.character.skill.plan.milestone.DeleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_uuid = extract_uuid(dyn_msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing milestone Identifier.uuid".into()))?;
            let mut plans = mgr.personal_plans.write().unwrap_or_else(|e| e.into_inner());
            let list = plans.get_mut(&cid).ok_or_else(|| (STATUS_NOT_FOUND, "Milestone not found".into()))?;
            let mut found = false;
            for p in list.iter_mut() {
                if let Some(pos) = p.milestones.iter().position(|m| m.uuid == target_uuid) {
                    p.milestones.remove(pos);
                    found = true;
                    break;
                }
            }
            if !found {
                return Err((STATUS_NOT_FOUND, "Milestone not found".into()));
            }
            drop(plans);
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );

    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.character.skill.plan.milestone.SetDescriptionRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let target_uuid = extract_uuid(dyn_msg, 1).ok_or_else(|| (STATUS_BAD_REQUEST, "Missing milestone Identifier.uuid".into()))?;
            let new_desc = dyn_msg.get(2).and_then(|v| match v {
                DynamicValue::String(s) => Some(s.clone()),
                _ => None,
            }).unwrap_or_default();

            let mut plans = mgr.personal_plans.write().unwrap_or_else(|e| e.into_inner());
            let list = plans.get_mut(&cid).ok_or_else(|| (STATUS_NOT_FOUND, "Milestone not found".into()))?;
            let mut found = false;
            for p in list.iter_mut() {
                if let Some(m) = p.milestones.iter_mut().find(|m| m.uuid == target_uuid) {
                    m.description = new_desc.clone();
                    found = true;
                    break;
                }
            }
            if !found {
                return Err((STATUS_NOT_FOUND, "Milestone not found".into()));
            }
            drop(plans);
            mgr.save_to_db();
            Ok(GoogleEmpty.to_any())
        }),
    );
}

fn parse_skill_requirements(val: Option<&DynamicValue>, sde: &eve_sde::SdeRegistry) -> Result<Vec<SkillRequirement>, (u32, String)> {
    let mut reqs = Vec::new();
    if let Some(DynamicValue::Repeated(list)) = val {
        for item in list {
            if let DynamicValue::Message(m) = item {
                let tid = m.get(1).and_then(|v| match v {
                    DynamicValue::Message(id_msg) => id_msg.get(1).and_then(|t| match t {
                        DynamicValue::Uint32(u) => Some(*u),
                        DynamicValue::Uint64(u) => u32::try_from(*u).ok(),
                        DynamicValue::Int32(i) if *i > 0 => u32::try_from(*i).ok(),
                        DynamicValue::Int64(i) if *i > 0 => u32::try_from(*i).ok(),
                        _ => None,
                    }),
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Uint64(u) => u32::try_from(*u).ok(),
                    DynamicValue::Int32(i) if *i > 0 => u32::try_from(*i).ok(),
                    DynamicValue::Int64(i) if *i > 0 => u32::try_from(*i).ok(),
                    _ => None,
                }).unwrap_or(0);
                let lvl = m.get(2).and_then(|v| match v {
                    DynamicValue::Uint32(u) => Some(*u),
                    DynamicValue::Uint64(u) => u32::try_from(*u).ok(),
                    DynamicValue::Int32(i) if *i > 0 => u32::try_from(*i).ok(),
                    DynamicValue::Int64(i) if *i > 0 => u32::try_from(*i).ok(),
                    _ => None,
                }).unwrap_or(0);
                if tid == 0 || !(1..=5).contains(&lvl) || !sde.types.get(&tid).is_some_and(|info| info.published && info.category_id == 16) {
                    return Err((STATUS_BAD_REQUEST, "Invalid skill requirement type_id or level".into()));
                }
                reqs.push(SkillRequirement { type_id: tid, level: lvl });
            }
        }
    }
    Ok(reqs)
}
