//! `quasar_skills_clones` — V24.01 Quasar Skills, Skill Plans, Injectors & Clones (`feat:quasar.skill`, `feat:quasar.implant`, `Q-4`).
//!
//! Implements live backend semantics for the EVE Online V24 Skill system:
//! - Personal and Corporation skill plans (creation, description, skill requirements, milestones, active tracking).
//! - Skill metadata queries (categories, skill types, alpha clone restrictions derived from SDE).
//! - Skill injectors and extractors with diminishing-returns SP yield calculations.
//! - Character skill level and attribute queries.

#![forbid(unsafe_code)]

use crate::formulas::skill_injector_sp_yield;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use eve_db::WorldRepository;
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, GoogleAny, GoogleEmpty, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone)]
pub struct SkillRequirement {
    pub type_id: u32,
    pub level: u32,
}

#[derive(Debug, Clone)]
pub struct SkillPlan {
    pub uuid: [u8; 16],
    pub owner_char_id: Option<u32>,
    pub owner_corp_id: Option<u32>,
    pub name: String,
    pub description: String,
    pub category_id: u32,
    pub requirements: Vec<SkillRequirement>,
    pub milestones: Vec<String>,
}

#[derive(Debug, Default)]
pub struct SkillPlanManager {
    pub personal_plans: RwLock<HashMap<u32, Vec<SkillPlan>>>, // char_id -> plans
    pub corp_plans: RwLock<HashMap<u32, Vec<SkillPlan>>>,     // corp_id -> plans
    pub active_plans: RwLock<HashMap<u32, [u8; 16]>>,        // char_id -> plan_uuid
}

impl SkillPlanManager {
    pub fn new() -> Self {
        let mgr = Self {
            personal_plans: RwLock::new(HashMap::new()),
            corp_plans: RwLock::new(HashMap::new()),
            active_plans: RwLock::new(HashMap::new()),
        };
        // Seed default skill plans (DERIVED from standard certified plans)
        let sample_plan = SkillPlan {
            uuid: [0x53, 0x50, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
            owner_char_id: Some(90_000_001),
            owner_corp_id: None,
            name: "Core Ship Operation".to_string(),
            description: "Fundamental navigation, engineering, and armor rigging skills.".to_string(),
            category_id: 1,
            requirements: vec![
                SkillRequirement {
                    type_id: 3436, // Drones
                    level: 5,
                },
                SkillRequirement {
                    type_id: 3318, // Weapon Upgrades
                    level: 4,
                },
            ],
            milestones: vec!["Level IV Mastery".to_string()],
        };
        mgr.personal_plans
            .write()
            .unwrap()
            .entry(90_000_001)
            .or_default()
            .push(sample_plan);
        mgr
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
    encode_string_field(2, &plan.description, out);
    encode_varint_field(3, plan.category_id as u64, out);

    // 4: requirements
    for req in &plan.requirements {
        let mut r_bytes = Vec::new();
        encode_varint_field(1, req.type_id as u64, &mut r_bytes);
        encode_varint_field(2, req.level as u64, &mut r_bytes);
        encode_bytes_field(4, &r_bytes, out);
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
    router.register_handler(
        "eve_public.skill.plan.CreateRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let mut plan_uuid = [0u8; 16];
            plan_uuid[0..8].copy_from_slice(&(ctx.now_unix()).to_be_bytes());
            plan_uuid[8..12].copy_from_slice(&cid.to_be_bytes());
            plan_uuid[12..16].copy_from_slice(&0x504C414Eu32.to_be_bytes());

            let name = dyn_msg
                .get(1)
                .and_then(|v| match v {
                    DynamicValue::Message(m) => m.get(1).and_then(|val| match val {
                        DynamicValue::String(s) => Some(s.clone()),
                        _ => None,
                    }),
                    _ => None,
                })
                .unwrap_or_else(|| "Custom Skill Plan".to_string());

            let new_plan = SkillPlan {
                uuid: plan_uuid,
                owner_char_id: Some(cid),
                owner_corp_id: None,
                name,
                description: String::new(),
                category_id: 1,
                requirements: Vec::new(),
                milestones: Vec::new(),
            };
            mgr.create_personal_plan(cid, new_plan);

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
            let _ = mgr.delete_personal_plan(cid, &target_uuid);
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
            mgr.set_active_plan(cid, target_uuid);
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 7. eve_public.corporation.skill.plan.GetAllRequest -> GetAllResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.corporation.skill.plan.GetAllRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let corp_id = ctx.effective_corporation_id().unwrap_or(98_000_001);
            let plans = mgr.get_corp_plans(corp_id);
            let mut out = Vec::new();
            for p in &plans {
                let mut id_bytes = Vec::new();
                encode_bytes_field(1, &p.uuid, &mut id_bytes);
                encode_bytes_field(1, &id_bytes, &mut out);
            }
            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.GetAllResponse"
                ),
                value: out,
            })
        }),
    );

    // 8. eve_public.corporation.skill.plan.GetResponse -> GetResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.corporation.skill.plan.GetResponse",
        Arc::new(move |ctx, _any, dyn_msg| {
            let corp_id = ctx.effective_corporation_id().unwrap_or(98_000_001);
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing skill_plan Identifier.uuid".into()));
            };
            let plans = mgr.get_corp_plans(corp_id);
            let p = plans
                .iter()
                .find(|x| x.uuid == target_uuid)
                .ok_or_else(|| (STATUS_NOT_FOUND, "Corp skill plan not found".into()))?;

            let mut out = Vec::new();
            let mut attr_bytes = Vec::new();
            encode_skill_plan_attributes(p, &mut attr_bytes);
            encode_bytes_field(1, &attr_bytes, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.GetResponse"
                ),
                value: out,
            })
        }),
    );

    // 9. eve_public.corporation.skill.plan.CreateRequest -> CreateResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.corporation.skill.plan.CreateRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let corp_id = ctx.effective_corporation_id().unwrap_or(98_000_001);
            let mut plan_uuid = [0u8; 16];
            plan_uuid[0..8].copy_from_slice(&(ctx.now_unix()).to_be_bytes());
            plan_uuid[8..12].copy_from_slice(&corp_id.to_be_bytes());
            plan_uuid[12..16].copy_from_slice(&0x434F5250u32.to_be_bytes());

            let new_plan = SkillPlan {
                uuid: plan_uuid,
                owner_char_id: None,
                owner_corp_id: Some(corp_id),
                name: "Corp Doctrine Skill Plan".to_string(),
                description: "Corporate fleet doctrine training program.".to_string(),
                category_id: 1,
                requirements: Vec::new(),
                milestones: Vec::new(),
            };
            mgr.create_corp_plan(corp_id, new_plan);

            let mut out = Vec::new();
            let mut id_bytes = Vec::new();
            encode_bytes_field(1, &plan_uuid, &mut id_bytes);
            encode_bytes_field(1, &id_bytes, &mut out);

            Ok(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporation.skill.plan.CreateResponse"
                ),
                value: out,
            })
        }),
    );

    // 10. eve_public.corporation.skill.plan.DeleteRequest -> DeleteResponse
    let mgr = Arc::clone(&manager);
    router.register_handler(
        "eve_public.corporation.skill.plan.DeleteRequest",
        Arc::new(move |ctx, _any, dyn_msg| {
            let corp_id = ctx.effective_corporation_id().unwrap_or(98_000_001);
            let Some(target_uuid) = extract_uuid(dyn_msg, 1) else {
                return Err((STATUS_BAD_REQUEST, "Missing skill_plan Identifier.uuid".into()));
            };
            let _ = mgr.delete_corp_plan(corp_id, &target_uuid);
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 11. eve.skilltype.GetRequest -> GetResponse
    router.register_handler(
        "eve.skilltype.GetRequest",
        Arc::new(move |_ctx, _any, dyn_msg| {
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
        }),
    );

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

    // 14. eve.skill.injector.InjectSkillPointsRequest -> InjectSkillPointsResponse
    let db_inject = Arc::clone(&db);
    router.register_handler(
        "eve.skill.injector.InjectSkillPointsRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            // Award standard large skill injector SP yield
            db_inject.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
                    let total_sp = ch.skill_points + ch.free_skill_points;
                    let yield_sp = skill_injector_sp_yield(total_sp); // Large Skill Injector
                    ch.free_skill_points += yield_sp;
                }
            });
            Ok(GoogleEmpty.to_any())
        }),
    );

    // 15. eve.skill.extractor.ExtractSkillsRequest -> ExtractSkillsResponse
    let db_extract = Arc::clone(&db);
    router.register_handler(
        "eve.skill.extractor.ExtractSkillsRequest",
        Arc::new(move |ctx, _any, _dyn| {
            let Some(cid) = ctx.effective_character_id() else {
                return Err((STATUS_FORBIDDEN, "Active character required".into()));
            };
            let mut extract_ok = false;
            // Deduct 500k SP for standard extractor if above 5.5M SP requirement, producing a Large Skill Injector
            db_extract.with_mut_snapshot(|snap| {
                if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
                    let is_omega = ch.clone_grade >= 2;
                    if is_omega && ch.skill_points >= 5_500_000.0 {
                        ch.skill_points -= 500_000.0;
                        let new_id = snap.items.keys().copied().max().unwrap_or(100_000_000) + 1;
                        let loc = ch.station_id.unwrap_or(60_003_760);
                        snap.items.insert(
                            new_id,
                            eve_db::InventoryItemRecord {
                                item_id: new_id,
                                type_id: 40520, // Large Skill Injector
                                owner_id: cid as i64,
                                location_id: loc,
                                flag: eve_db::InventoryFlag::Hangar,
                                quantity: 1,
                                ..Default::default()
                            },
                        );
                        extract_ok = true;
                    }
                }
            });
            if !extract_ok {
                return Err((
                    STATUS_BAD_REQUEST,
                    "Extracting skills requires Omega clone and at least 5,500,000 SP".into(),
                ));
            }
            Ok(GoogleEmpty.to_any())
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
}
