//! Unit tests for `quasar_skills_clones` (Skill Plans, Milestones, Skill Types, Implants).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::corp_roles;
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn uuid_msg(field: u32, uuid: [u8; 16]) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

#[test]
fn skill_plan_manager_and_helpers() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let mgr = SkillPlanManager::new_with_db(Some(Arc::clone(&db)));

    let mut uuid = [0u8; 16];
    uuid[0..4].copy_from_slice(b"SPCH");
    uuid[8..16].copy_from_slice(&500_u64.to_be_bytes());
    let plan = SkillPlan {
        uuid,
        owner_char_id: Some(90_000_001),
        owner_corp_id: None,
        name: "Gunnery Core".into(),
        description: "Turret fundamentals".into(),
        category_id: 1,
        requirements: vec![SkillRequirement { type_id: 3300, level: 4 }],
        milestones: Vec::new(),
    };
    mgr.create_personal_plan(90_000_001, plan.clone());
    assert_eq!(mgr.get_personal_plans(90_000_001).len(), 1);

    mgr.set_active_plan(90_000_001, uuid);
    assert_eq!(mgr.get_active_plan(90_000_001).unwrap().name, "Gunnery Core");
    mgr.clear_active_plan(90_000_001);
    assert!(mgr.get_active_plan(90_000_001).is_none());

    let mut corp_plan = plan.clone();
    corp_plan.owner_corp_id = Some(1_000_009);
    mgr.create_corp_plan(1_000_009, corp_plan);
    assert_eq!(mgr.get_corp_plans(1_000_009).len(), 1);
    assert!(mgr.delete_corp_plan(1_000_009, &uuid));
    assert!(!mgr.delete_corp_plan(1_000_009, &uuid));

    mgr.save_to_db();
    let reloaded = SkillPlanManager::new_with_db(Some(Arc::clone(&db)));
    assert_eq!(reloaded.get_personal_plans(90_000_001).len(), 1);
    assert!(reloaded.delete_personal_plan(90_000_001, &uuid));
    assert!(!reloaded.delete_personal_plan(90_000_001, &uuid));

    let mut enc = Vec::new();
    encode_skill_plan_attributes(&plan, &mut enc);
    assert!(!enc.is_empty());

    let msg = uuid_msg(1, uuid);
    assert_eq!(extract_uuid(&msg, 1), Some(uuid));
    assert_eq!(extract_active_plan_uuid(&GoogleAny::default(), &msg), Some(uuid));
    assert_eq!(
        extract_active_plan_uuid(&GoogleAny::default(), &uuid_msg(1, [0u8; 16])),
        None
    );

    assert_eq!(extract_u32(Some(&DynamicValue::Uint32(3300))), Some(3300));
    assert_eq!(extract_u32(Some(&DynamicValue::Int32(-1))), None);

    // Valid skill requirement (type 3300 Gunnery, level 3)
    let mut req_msg = DynamicQuasarMessage::new("SkillReq");
    req_msg.fields.insert(1, DynamicValue::Uint32(3300));
    req_msg.fields.insert(2, DynamicValue::Uint32(3));
    let parsed = parse_skill_requirements(
        Some(&DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(req_msg))])),
        &sde,
    )
    .expect("valid skill requirement");
    assert_eq!(parsed.len(), 1);
    assert_eq!(parsed[0].type_id, 3300);
    assert_eq!(parsed[0].level, 3);

    // Invalid skill requirement level rejected
    let mut bad_req = DynamicQuasarMessage::new("SkillReq");
    bad_req.fields.insert(1, DynamicValue::Uint32(3300));
    bad_req.fields.insert(2, DynamicValue::Uint32(9));
    assert_eq!(
        parse_skill_requirements(
            Some(&DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(bad_req))])),
            &sde,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
}

#[test]
fn skill_plan_and_character_milestone_rpc_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    let mgr = Arc::new(SkillPlanManager::new_with_db(Some(Arc::clone(&db))));
    register_skill_handlers(&router, Arc::clone(&db), Arc::clone(&mgr));

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let cid = 90_000_001_u32;
    let ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(1_000_009),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );

    // 1. eve_public.skill.plan.* lifecycle
    let mut create_msg = DynamicQuasarMessage::new("Create");
    create_msg.fields.insert(1, DynamicValue::String("Starter Plan".into()));
    let created = handlers["eve_public.skill.plan.CreateRequest"](&ctx, &dummy_any, &create_msg)
        .expect("create skill plan");
    assert!(created.type_url.ends_with("eve_public.skill.plan.CreateResponse"));

    let plan_uuid = mgr.get_personal_plans(cid)[0].uuid;

    let get_resp = handlers["eve_public.skill.plan.GetRequest"](&ctx, &dummy_any, &uuid_msg(1, plan_uuid))
        .expect("get skill plan");
    assert!(get_resp.type_url.ends_with("eve_public.skill.plan.GetResponse"));

    let all_resp = handlers["eve_public.skill.plan.GetAllRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("All"))
        .expect("get all skill plans");
    assert!(all_resp.type_url.ends_with("eve_public.skill.plan.GetAllResponse"));

    handlers["eve_public.skill.plan.SetActiveRequest"](&ctx, &dummy_any, &uuid_msg(1, plan_uuid))
        .expect("set active plan");
    let act_resp = handlers["eve_public.skill.plan.GetActiveRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Active"))
        .expect("get active plan");
    assert!(act_resp.type_url.ends_with("eve_public.skill.plan.GetActiveResponse"));

    // Clear active plan
    handlers["eve_public.skill.plan.SetActiveRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Clear"))
        .expect("clear active plan");
    handlers["eve_public.skill.plan.DeleteRequest"](&ctx, &dummy_any, &uuid_msg(1, plan_uuid))
        .expect("delete skill plan");

    // 2. eve_public.character.skill.plan.* + milestones lifecycle
    let mut attr = DynamicQuasarMessage::new("Attr");
    attr.fields.insert(1, DynamicValue::String("Char Plan".into()));
    attr.fields.insert(3, DynamicValue::String("Initial desc".into()));
    let mut ch_create = DynamicQuasarMessage::new("Create");
    ch_create.fields.insert(1, DynamicValue::Message(Box::new(attr)));

    let ch_created = handlers["eve_public.character.skill.plan.CreateRequest"](&ctx, &dummy_any, &ch_create)
        .expect("create char skill plan");
    assert!(ch_created.type_url.ends_with("eve_public.character.skill.plan.CreateResponse"));
    let ch_uuid = mgr.get_personal_plans(cid)[0].uuid;

    for (req, suffix) in [
        ("eve_public.character.skill.plan.GetRequest", "eve_public.character.skill.plan.GetResponse"),
        ("eve_public.character.skill.plan.GetAllRequest", "eve_public.character.skill.plan.GetAllResponse"),
        ("eve_public.character.skill.plan.GetSharedRequest", "eve_public.character.skill.plan.GetSharedResponse"),
        ("eve_public.character.skill.plan.GetActiveRequest", "eve_public.character.skill.plan.GetActiveResponse"),
    ] {
        let r = handlers[req](&ctx, &dummy_any, &uuid_msg(1, ch_uuid)).expect(req);
        assert!(r.type_url.ends_with(suffix));
    }

    handlers["eve_public.character.skill.plan.SetActiveRequest"](&ctx, &dummy_any, &uuid_msg(1, ch_uuid))
        .expect("set char active plan");

    let mut set_name = uuid_msg(1, ch_uuid);
    set_name.fields.insert(2, DynamicValue::String("Renamed Char Plan".into()));
    handlers["eve_public.character.skill.plan.SetNameRequest"](&ctx, &dummy_any, &set_name)
        .expect("set char plan name");

    let mut set_desc = uuid_msg(1, ch_uuid);
    set_desc.fields.insert(2, DynamicValue::String("New desc".into()));
    handlers["eve_public.character.skill.plan.SetDescriptionRequest"](&ctx, &dummy_any, &set_desc)
        .expect("set char plan desc");

    let mut req_item = DynamicQuasarMessage::new("ReqItem");
    req_item.fields.insert(1, DynamicValue::Uint32(3300));
    req_item.fields.insert(2, DynamicValue::Uint32(4));
    let mut set_reqs = uuid_msg(1, ch_uuid);
    set_reqs.fields.insert(
        2,
        DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(req_item))]),
    );
    handlers["eve_public.character.skill.plan.SetSkillRequirementsRequest"](&ctx, &dummy_any, &set_reqs)
        .expect("set char plan skill requirements");

    // Milestone create, list, set description, delete
    let mut ms_attr = uuid_msg(1, ch_uuid);
    let mut sr_msg = DynamicQuasarMessage::new("SkillReq");
    sr_msg.fields.insert(1, DynamicValue::Uint32(3300));
    sr_msg.fields.insert(2, DynamicValue::Uint32(3));
    ms_attr.fields.insert(3, DynamicValue::Message(Box::new(sr_msg)));
    ms_attr.fields.insert(4, DynamicValue::String("Train Gunnery III".into()));
    let mut ms_create = DynamicQuasarMessage::new("MsCreate");
    ms_create.fields.insert(1, DynamicValue::Message(Box::new(ms_attr)));

    handlers["eve_public.character.skill.plan.milestone.CreateRequest"](&ctx, &dummy_any, &ms_create)
        .expect("create milestone");
    let ms_uuid = mgr.get_personal_plans(cid)[0].milestones[0].uuid;

    let ms_all = handlers["eve_public.character.skill.plan.milestone.GetAllRequest"](
        &ctx,
        &dummy_any,
        &uuid_msg(1, ch_uuid),
    )
    .expect("get all milestones");
    assert!(ms_all
        .type_url
        .ends_with("eve_public.character.skill.plan.milestone.GetAllResponse"));
    assert!(!ms_all.value.is_empty());

    let mut ms_desc = uuid_msg(1, ms_uuid);
    ms_desc.fields.insert(2, DynamicValue::String("Updated milestone".into()));
    handlers["eve_public.character.skill.plan.milestone.SetDescriptionRequest"](&ctx, &dummy_any, &ms_desc)
        .expect("set milestone desc");

    handlers["eve_public.character.skill.plan.milestone.DeleteRequest"](&ctx, &dummy_any, &uuid_msg(1, ms_uuid))
        .expect("delete milestone");
    handlers["eve_public.character.skill.plan.DeleteRequest"](&ctx, &dummy_any, &uuid_msg(1, ch_uuid))
        .expect("delete char plan");

    // 3. Skill metadata, character skill level, implants, injector/extractor stubs
    let st_resp = handlers["eve.skilltype.GetRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Req"))
        .expect("skilltype.GetRequest");
    assert!(st_resp.type_url.ends_with("eve.skilltype.GetResponse"));

    let sc_resp = handlers["eve.skillcategory.GetAllRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Req"))
        .expect("skillcategory.GetAllRequest");
    assert!(sc_resp.type_url.ends_with("eve.skillcategory.GetAllResponse"));

    let lvl_resp = handlers["eve.skill.character.api.GetLevelRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Req"))
        .expect("GetLevelRequest");
    assert!(lvl_resp.type_url.ends_with("eve.skill.character.api.GetLevelResponse"));

    let imp_resp = handlers["eve.character.implant.GetActiveImplantsByCharacterRequest"](
        &ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .expect("GetActiveImplantsByCharacterRequest");
    assert!(imp_resp
        .type_url
        .ends_with("eve.character.implant.GetActiveImplantsByCharacterResponse"));

    let imp_types = handlers["eve.implanttype.GetAllRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Req"))
        .expect("implanttype.GetAllRequest");
    assert!(imp_types.type_url.ends_with("eve.implanttype.GetAllResponse"));

    assert_eq!(
        handlers["eve.skill.injector.InjectSkillPointsRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Req"))
            .unwrap_err()
            .0,
        STATUS_NOT_IMPLEMENTED
    );
    assert_eq!(
        handlers["eve.skill.extractor.ExtractSkillsRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Req"))
            .unwrap_err()
            .0,
        STATUS_NOT_IMPLEMENTED
    );
}
