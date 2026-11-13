//! Unit tests for `quasar_skills_clones` (Skill Plans, Milestones, Skill Types, Implants).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::corp_roles;
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry, WireField,
    WireMessage, WireValue,
};
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

    let set_act_resp = handlers["eve_public.skill.plan.SetActiveRequest"](&ctx, &dummy_any, &uuid_msg(1, plan_uuid))
        .expect("set active plan");
    assert!(set_act_resp.type_url.ends_with("eve_public.skill.plan.SetActiveResponse"));
    let act_resp = handlers["eve_public.skill.plan.GetActiveRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Active"))
        .expect("get active plan");
    assert!(act_resp.type_url.ends_with("eve_public.skill.plan.GetActiveResponse"));

    // Clear active plan
    let clr_act_resp = handlers["eve_public.skill.plan.SetActiveRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Clear"))
        .expect("clear active plan");
    assert!(clr_act_resp.type_url.ends_with("eve_public.skill.plan.SetActiveResponse"));
    let del_plan_resp = handlers["eve_public.skill.plan.DeleteRequest"](&ctx, &dummy_any, &uuid_msg(1, plan_uuid))
        .expect("delete skill plan");
    assert!(del_plan_resp.type_url.ends_with("eve_public.skill.plan.DeleteResponse"));

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

    let ch_set_act = handlers["eve_public.character.skill.plan.SetActiveRequest"](&ctx, &dummy_any, &uuid_msg(1, ch_uuid))
        .expect("set char active plan");
    assert!(ch_set_act.type_url.ends_with("eve_public.character.skill.plan.SetActiveResponse"));

    let mut set_name = uuid_msg(1, ch_uuid);
    set_name.fields.insert(2, DynamicValue::String("Renamed Char Plan".into()));
    let ch_set_name = handlers["eve_public.character.skill.plan.SetNameRequest"](&ctx, &dummy_any, &set_name)
        .expect("set char plan name");
    assert!(ch_set_name.type_url.ends_with("eve_public.character.skill.plan.SetNameResponse"));

    let mut set_desc = uuid_msg(1, ch_uuid);
    set_desc.fields.insert(2, DynamicValue::String("New desc".into()));
    let ch_set_desc = handlers["eve_public.character.skill.plan.SetDescriptionRequest"](&ctx, &dummy_any, &set_desc)
        .expect("set char plan desc");
    assert!(ch_set_desc.type_url.ends_with("eve_public.character.skill.plan.SetDescriptionResponse"));

    let mut req_item = DynamicQuasarMessage::new("ReqItem");
    req_item.fields.insert(1, DynamicValue::Uint32(3300));
    req_item.fields.insert(2, DynamicValue::Uint32(4));
    let mut set_reqs = uuid_msg(1, ch_uuid);
    set_reqs.fields.insert(
        2,
        DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(req_item))]),
    );
    let ch_set_reqs = handlers["eve_public.character.skill.plan.SetSkillRequirementsRequest"](&ctx, &dummy_any, &set_reqs)
        .expect("set char plan skill requirements");
    assert!(ch_set_reqs.type_url.ends_with("eve_public.character.skill.plan.SetSkillRequirementsResponse"));

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
    let ms_set_desc = handlers["eve_public.character.skill.plan.milestone.SetDescriptionRequest"](&ctx, &dummy_any, &ms_desc)
        .expect("set milestone desc");
    assert!(ms_set_desc.type_url.ends_with("eve_public.character.skill.plan.milestone.SetDescriptionResponse"));

    let ms_del = handlers["eve_public.character.skill.plan.milestone.DeleteRequest"](&ctx, &dummy_any, &uuid_msg(1, ms_uuid))
        .expect("delete milestone");
    assert!(ms_del.type_url.ends_with("eve_public.character.skill.plan.milestone.DeleteResponse"));
    let ch_del = handlers["eve_public.character.skill.plan.DeleteRequest"](&ctx, &dummy_any, &uuid_msg(1, ch_uuid))
        .expect("delete char plan");
    assert!(ch_del.type_url.ends_with("eve_public.character.skill.plan.DeleteResponse"));

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

#[test]
fn rehydrate_from_db_legacy_corrupt_lock_guard_and_wire_extract_helpers() {
    use std::sync::atomic::Ordering;

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());

    // 1. Default SkillPlanManager::new() (no DB): rehydrate_from_db & save_to_db are safe no-ops
    let no_db_mgr = SkillPlanManager::new();
    no_db_mgr.rehydrate_from_db();
    no_db_mgr.save_to_db();
    assert!(no_db_mgr.get_personal_plans(90_000_001).is_empty());

    // 2. Modern QuasarSkillPlansPersistedState JSON rehydrate computes max_existing_seq across SPCH plan & MSCH milestone
    let mut sp_uuid = [0u8; 16];
    sp_uuid[0..4].copy_from_slice(b"SPCH");
    sp_uuid[8..16].copy_from_slice(&420_u64.to_be_bytes());
    let mut ms_uuid = [0u8; 16];
    ms_uuid[0..4].copy_from_slice(b"MSCH");
    ms_uuid[8..16].copy_from_slice(&750_u64.to_be_bytes());
    let mut other_uuid = [0u8; 16];
    other_uuid[0..4].copy_from_slice(b"OTHR");

    let plan_modern = SkillPlan {
        uuid: sp_uuid,
        owner_char_id: Some(90_000_001),
        owner_corp_id: None,
        name: "Persisted Plan".into(),
        description: "Rehydrated".into(),
        category_id: 1,
        requirements: vec![SkillRequirement { type_id: 3300, level: 5 }],
        milestones: vec![SkillPlanMilestone {
            uuid: ms_uuid,
            description: "MS 750".into(),
            type_id: None,
            skill_requirement: Some(SkillRequirement { type_id: 3300, level: 4 }),
        }],
    };
    let modern_state = QuasarSkillPlansPersistedState {
        personal_plans: HashMap::from([(90_000_001, vec![plan_modern.clone()])]),
        corp_plans: HashMap::new(),
        active_plans: HashMap::from([(90_000_001, other_uuid)]),
        next_seq: 10, // smaller than 750 + 1 -> must bump to 751!
    };
    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert(
            "quasar_skill_plans".into(),
            serde_json::to_string(&modern_state).unwrap(),
        );
    });
    let mgr = SkillPlanManager::new_with_db(Some(Arc::clone(&db)));
    // Orphan active_plans UUID pointing to missing plan returns None
    assert!(mgr.get_active_plan(90_000_001).is_none());
    assert_eq!(mgr.next_seq.load(Ordering::Relaxed), 751);

    // 3. save_to_db write-lock re-entrancy guard returns cleanly without deadlock or overwriting inside with_mut_snapshot
    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert("quasar_skill_plans".into(), "sentinel".into());
        mgr.save_to_db();
        assert_eq!(snap.quasar_state_blobs["quasar_skill_plans"], "sentinel");
    });

    // 4. rehydrate_from_db with legacy 3-tuple JSON format + non-SPCH/MSCH UUID (seq floor 100)
    let legacy_plan = SkillPlan {
        uuid: other_uuid,
        owner_char_id: Some(90_000_002),
        owner_corp_id: None,
        name: "Legacy".into(),
        description: String::new(),
        category_id: 1,
        requirements: Vec::new(),
        milestones: Vec::new(),
    };
    let legacy_tuple = (
        HashMap::from([(90_000_002_u32, vec![legacy_plan])]),
        HashMap::from([(1_000_009_u32, vec![plan_modern])]),
        HashMap::from([(90_000_002_u32, other_uuid)]),
    );
    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert(
            "quasar_skill_plans".into(),
            serde_json::to_string(&legacy_tuple).unwrap(),
        );
    });
    mgr.rehydrate_from_db();
    assert_eq!(mgr.get_personal_plans(90_000_002).len(), 1);
    assert_eq!(mgr.get_corp_plans(1_000_009).len(), 1);
    assert_eq!(mgr.get_active_plan(90_000_002).unwrap().name, "Legacy");
    assert_eq!(mgr.next_seq.load(Ordering::Relaxed), 100);
    // Delete on missing char/corp IDs returns false
    assert!(!mgr.delete_personal_plan(999_999, &other_uuid));
    assert!(!mgr.delete_corp_plan(999_999, &sp_uuid));

    // 5. rehydrate_from_db with corrupt JSON resets gracefully to empty defaults
    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs
            .insert("quasar_skill_plans".into(), "{corrupt json!!!".into());
    });
    mgr.rehydrate_from_db();
    assert!(mgr.get_personal_plans(90_000_002).is_empty());
    assert!(mgr.get_corp_plans(1_000_009).is_empty());
    assert_eq!(mgr.next_seq.load(Ordering::Relaxed), 100);

    // 6. extract_uuid / extract_active_plan_uuid / extract_u32 / parse_skill_requirements edge variants
    let mut bad_uuid_len = DynamicQuasarMessage::new("Id");
    bad_uuid_len.fields.insert(1, DynamicValue::Bytes(vec![1; 15]));
    let mut bad_wrap = DynamicQuasarMessage::new("Req");
    bad_wrap.fields.insert(1, DynamicValue::Message(Box::new(bad_uuid_len)));
    assert_eq!(extract_uuid(&bad_wrap, 1), None);
    assert_eq!(extract_uuid(&bad_wrap, 2), None);

    // Field 2 UUID in dyn_msg
    assert_eq!(
        extract_active_plan_uuid(&GoogleAny::default(), &uuid_msg(2, sp_uuid)),
        Some(sp_uuid)
    );
    // Fallback to raw wire protobuf bytes in GoogleAny.value (field 2 -> sub-field 1)
    let sub_b = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(sp_uuid.to_vec()),
        }],
    }
    .encode();
    let raw_val = WireMessage {
        fields: vec![WireField {
            field_number: 2,
            value: WireValue::LengthDelimited(sub_b),
        }],
    }
    .encode();
    let wire_any = GoogleAny {
        type_url: String::new(),
        value: raw_val,
    };
    assert_eq!(
        extract_active_plan_uuid(&wire_any, &DynamicQuasarMessage::new("Empty")),
        Some(sp_uuid)
    );
    // All-zero UUID in wire protobuf returns None
    let zero_sub = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(vec![0u8; 16]),
        }],
    }
    .encode();
    let zero_raw = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(zero_sub),
        }],
    }
    .encode();
    assert_eq!(
        extract_active_plan_uuid(
            &GoogleAny {
                type_url: String::new(),
                value: zero_raw,
            },
            &DynamicQuasarMessage::new("Empty")
        ),
        None
    );

    // extract_u32 scalar/nested/non-positive branches
    assert_eq!(extract_u32(Some(&DynamicValue::Uint64(3300))), Some(3300));
    assert_eq!(extract_u32(Some(&DynamicValue::Int32(3301))), Some(3301));
    assert_eq!(extract_u32(Some(&DynamicValue::Int64(3302))), Some(3302));
    assert_eq!(extract_u32(Some(&DynamicValue::Int64(0))), None);
    assert_eq!(extract_u32(Some(&DynamicValue::String("3300".into()))), None);
    let mut wrapped_id = DynamicQuasarMessage::new("Tid");
    wrapped_id.fields.insert(1, DynamicValue::Uint32(3300));
    assert_eq!(
        extract_u32(Some(&DynamicValue::Message(Box::new(wrapped_id)))),
        Some(3300)
    );

    // parse_skill_requirements: None / non-skill category / level 0
    assert_eq!(parse_skill_requirements(None, &sde).unwrap().len(), 0);
    let mut non_skill_req = DynamicQuasarMessage::new("SkillReq");
    non_skill_req.fields.insert(1, DynamicValue::Uint32(34)); // Tritanium (category != 16)
    non_skill_req.fields.insert(2, DynamicValue::Uint32(3));
    assert_eq!(
        parse_skill_requirements(
            Some(&DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(non_skill_req))])),
            &sde,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let mut zero_lvl_req = DynamicQuasarMessage::new("SkillReq");
    zero_lvl_req.fields.insert(1, DynamicValue::Uint32(3300));
    zero_lvl_req.fields.insert(2, DynamicValue::Uint32(0));
    assert_eq!(
        parse_skill_requirements(
            Some(&DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(zero_lvl_req))])),
            &sde,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
}

#[test]
fn rpc_auth_validation_not_found_wire_decoding_implants_and_milestone_type_variants() {
    use eve_db::{InventoryFlag, InventoryItemRecord, TrainedSkillRecord};

    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    let mgr = Arc::new(SkillPlanManager::new_with_db(Some(Arc::clone(&db))));
    register_skill_handlers(&router, Arc::clone(&db), Arc::clone(&mgr));

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let cid = 90_000_001_u32;
    let cid_other = 90_000_099_u32;

    let unauth_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![1]);
    let ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(1_000_009),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let ctx_other = AuthoritativeContext::for_session(
        Some(2),
        Some(cid_other),
        Some(1_000_009),
        0,
        "tranquility",
        vec![1],
    );
    let mut admin_ctx = ctx.clone();
    admin_ctx.service_role_mask = eve_machonet::service_roles::ROLE_GM_MASK;

    // 1. Every auth-gated RPC rejects missing active character with STATUS_FORBIDDEN
    let auth_gated = [
        "eve_public.skill.plan.GetAllRequest",
        "eve_public.skill.plan.GetRequest",
        "eve_public.skill.plan.CreateRequest",
        "eve_public.skill.plan.DeleteRequest",
        "eve_public.skill.plan.GetActiveRequest",
        "eve_public.skill.plan.SetActiveRequest",
        "eve.skill.character.api.GetLevelRequest",
        "eve.character.implant.GetActiveImplantsByCharacterRequest",
        "eve_public.character.skill.plan.CreateRequest",
        "eve_public.character.skill.plan.GetRequest",
        "eve_public.character.skill.plan.GetAllRequest",
        "eve_public.character.skill.plan.GetSharedRequest",
        "eve_public.character.skill.plan.GetActiveRequest",
        "eve_public.character.skill.plan.SetActiveRequest",
        "eve_public.character.skill.plan.SetNameRequest",
        "eve_public.character.skill.plan.SetDescriptionRequest",
        "eve_public.character.skill.plan.SetSkillRequirementsRequest",
        "eve_public.character.skill.plan.DeleteRequest",
        "eve_public.character.skill.plan.milestone.GetAllRequest",
        "eve_public.character.skill.plan.milestone.CreateRequest",
        "eve_public.character.skill.plan.milestone.DeleteRequest",
        "eve_public.character.skill.plan.milestone.SetDescriptionRequest",
    ];
    for ep in auth_gated {
        assert_eq!(
            handlers[ep](&unauth_ctx, &dummy_any, &DynamicQuasarMessage::new("Req"))
                .unwrap_err()
                .0,
            STATUS_FORBIDDEN,
            "expected STATUS_FORBIDDEN on {ep}"
        );
    }

    // 2. eve_public.skill.plan.* validation & error branches (attr Message, blank name, bad UUID, not found)
    let mut bad_create = DynamicQuasarMessage::new("Create");
    bad_create.fields.insert(1, DynamicValue::String("   ".into()));
    assert_eq!(
        handlers["eve_public.skill.plan.CreateRequest"](&ctx, &dummy_any, &bad_create)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let mut msg_attr = DynamicQuasarMessage::new("Attr");
    msg_attr.fields.insert(1, DynamicValue::String("AttrPlan".into()));
    msg_attr.fields.insert(3, DynamicValue::String("AttrDesc".into()));
    let mut sr = DynamicQuasarMessage::new("SR");
    sr.fields.insert(1, DynamicValue::Uint32(3300));
    sr.fields.insert(2, DynamicValue::Uint32(4));
    msg_attr
        .fields
        .insert(2, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(sr))]));
    let mut attr_create = DynamicQuasarMessage::new("Create");
    attr_create.fields.insert(1, DynamicValue::Message(Box::new(msg_attr)));
    handlers["eve_public.skill.plan.CreateRequest"](&ctx, &dummy_any, &attr_create).unwrap();

    // GetActive before setting returns non-empty response (zero UUID)
    let empty_act = handlers["eve_public.skill.plan.GetActiveRequest"](
        &ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .unwrap();
    assert!(!empty_act.value.is_empty());

    let missing_uuid = [9u8; 16];
    assert_eq!(
        handlers["eve_public.skill.plan.GetRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Req"))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.skill.plan.GetRequest"](&ctx, &dummy_any, &uuid_msg(1, missing_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.skill.plan.SetActiveRequest"](&ctx, &dummy_any, &uuid_msg(1, missing_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.skill.plan.DeleteRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Req"))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.skill.plan.DeleteRequest"](&ctx, &dummy_any, &uuid_msg(1, missing_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    // 3. eve.skilltype.GetRequest across dyn int types, raw WireMessage varint vs submessage, and not found
    for dv in [
        DynamicValue::Uint32(3300),
        DynamicValue::Int32(3300),
        DynamicValue::Uint64(3300),
        DynamicValue::Int64(3300),
    ] {
        let mut req = DynamicQuasarMessage::new("Req");
        req.fields.insert(1, dv.clone());
        assert!(handlers["eve.skilltype.GetRequest"](&ctx, &dummy_any, &req).is_ok());

        let mut inner = DynamicQuasarMessage::new("Id");
        inner.fields.insert(1, dv);
        let mut nested = DynamicQuasarMessage::new("Req");
        nested.fields.insert(1, DynamicValue::Message(Box::new(inner)));
        assert!(handlers["eve.skilltype.GetRequest"](&ctx, &dummy_any, &nested).is_ok());
    }
    let wire_direct = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::Varint(3300),
        }],
    }
    .encode();
    assert!(handlers["eve.skilltype.GetRequest"](
        &ctx,
        &GoogleAny {
            type_url: String::new(),
            value: wire_direct,
        },
        &DynamicQuasarMessage::new("Req"),
    )
    .is_ok());
    let sub_tid = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::Varint(3300),
        }],
    }
    .encode();
    let wire_nested = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(sub_tid),
        }],
    }
    .encode();
    assert!(handlers["eve.skilltype.GetRequest"](
        &ctx,
        &GoogleAny {
            type_url: String::new(),
            value: wire_nested,
        },
        &DynamicQuasarMessage::new("Req"),
    )
    .is_ok());
    let mut bad_tid_req = DynamicQuasarMessage::new("Req");
    bad_tid_req.fields.insert(1, DynamicValue::Uint32(999_999_999));
    assert_eq!(
        handlers["eve.skilltype.GetRequest"](&ctx, &dummy_any, &bad_tid_req)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    // 4. eve.skill.character.api.GetLevelRequest with trained skill & scalar/nested variants
    db.with_mut_snapshot(|snap| {
        snap.skills.entry(cid as i64).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 4,
                skill_points: 45_255.0,
            },
        );
    });
    for dv in [
        DynamicValue::Uint32(3300),
        DynamicValue::Uint64(3300),
        DynamicValue::Int32(3300),
        DynamicValue::Int64(3300),
    ] {
        let mut req = DynamicQuasarMessage::new("Req");
        req.fields.insert(2, dv.clone());
        let resp = handlers["eve.skill.character.api.GetLevelRequest"](&ctx, &dummy_any, &req).unwrap();
        assert_eq!(resp.value, vec![0x08, 4]);

        let mut inner = DynamicQuasarMessage::new("Id");
        inner.fields.insert(1, dv);
        let mut req_m = DynamicQuasarMessage::new("Req");
        req_m.fields.insert(2, DynamicValue::Message(Box::new(inner)));
        let resp_m = handlers["eve.skill.character.api.GetLevelRequest"](&ctx, &dummy_any, &req_m).unwrap();
        assert_eq!(resp_m.value, vec![0x08, 4]);
    }

    // 5. eve.character.implant.GetActiveImplantsByCharacterRequest:
    // - Seed active_implants + InventoryFlag::Implant deduplication
    // - Cross-character denied for normal pilot, allowed for GM/Admin
    // - Request parsing via Message/scalar/16-byte UUID and wire protobuf bytes
    db.with_mut_snapshot(|snap| {
        snap.active_implants.insert(cid as i64, vec![10216]);
        snap.items.insert(
            990_700_001,
            InventoryItemRecord {
                item_id: 990_700_001,
                type_id: 10216, // duplicate of active_implants -> deduplicated
                owner_id: cid as i64,
                location_id: cid as i64,
                flag: InventoryFlag::Implant,
                quantity: 1,
                ..Default::default()
            },
        );
        snap.items.insert(
            990_700_002,
            InventoryItemRecord {
                item_id: 990_700_002,
                type_id: 10221,
                owner_id: cid as i64,
                location_id: cid as i64,
                flag: InventoryFlag::Implant,
                quantity: 1,
                ..Default::default()
            },
        );
    });
    let mut scalar_cid_req = DynamicQuasarMessage::new("Req");
    scalar_cid_req.fields.insert(1, DynamicValue::Uint32(cid));
    assert_eq!(
        handlers["eve.character.implant.GetActiveImplantsByCharacterRequest"](
            &ctx_other,
            &dummy_any,
            &scalar_cid_req,
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    let admin_imp = handlers["eve.character.implant.GetActiveImplantsByCharacterRequest"](
        &admin_ctx,
        &dummy_any,
        &scalar_cid_req,
    )
    .unwrap();
    assert!(!admin_imp.value.is_empty());

    let mut cid_uuid = [0u8; 16];
    cid_uuid[12..16].copy_from_slice(&cid.to_be_bytes());
    let wire_sub_uuid = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(cid_uuid.to_vec()),
        }],
    }
    .encode();
    let wire_cid_any = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(wire_sub_uuid),
        }],
    }
    .encode();
    assert!(
        handlers["eve.character.implant.GetActiveImplantsByCharacterRequest"](
            &ctx,
            &GoogleAny {
                type_url: String::new(),
                value: wire_cid_any,
            },
            &DynamicQuasarMessage::new("Req"),
        )
        .is_ok()
    );
    assert!(
        handlers["eve.character.implant.GetActiveImplantsByCharacterRequest"](
            &ctx,
            &dummy_any,
            &uuid_msg(1, cid_uuid),
        )
        .is_ok()
    );

    // 6. eve_public.character.skill.plan.* & milestones: GetShared across pilots, error branches, type_id milestone, and DeleteRequest when char has no plans
    let mut str_create = DynamicQuasarMessage::new("Create");
    str_create.fields.insert(1, DynamicValue::String("PilotOtherPlan".into()));
    handlers["eve_public.character.skill.plan.CreateRequest"](&ctx_other, &dummy_any, &str_create)
        .unwrap();
    let other_uuid = mgr.get_personal_plans(cid_other)[0].uuid;

    // ctx (cid) reads ctx_other's plan via GetSharedRequest!
    let shared_resp = handlers["eve_public.character.skill.plan.GetSharedRequest"](
        &ctx,
        &dummy_any,
        &uuid_msg(1, other_uuid),
    )
    .unwrap();
    assert!(shared_resp
        .type_url
        .ends_with("eve_public.character.skill.plan.GetSharedResponse"));

    // Not found / bad request checks across character skill plan endpoints
    assert_eq!(
        handlers["eve_public.character.skill.plan.GetRequest"](&ctx, &dummy_any, &uuid_msg(1, missing_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.character.skill.plan.GetSharedRequest"](&ctx, &dummy_any, &uuid_msg(1, missing_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    // GetActive when cleared returns non-empty (zero uuid), SetActive with unknown fails
    handlers["eve_public.character.skill.plan.SetActiveRequest"](
        &ctx_other,
        &dummy_any,
        &DynamicQuasarMessage::new("Clear"),
    )
    .unwrap();
    let no_act = handlers["eve_public.character.skill.plan.GetActiveRequest"](
        &ctx_other,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .unwrap();
    assert!(!no_act.value.is_empty());
    assert_eq!(
        handlers["eve_public.character.skill.plan.SetActiveRequest"](&ctx_other, &dummy_any, &uuid_msg(1, missing_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    let mut blank_rename = uuid_msg(1, other_uuid);
    blank_rename.fields.insert(2, DynamicValue::String("  ".into()));
    assert_eq!(
        handlers["eve_public.character.skill.plan.SetNameRequest"](&ctx_other, &dummy_any, &blank_rename)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let mut missing_rename = uuid_msg(1, missing_uuid);
    missing_rename.fields.insert(2, DynamicValue::String("Valid".into()));
    assert_eq!(
        handlers["eve_public.character.skill.plan.SetNameRequest"](&ctx_other, &dummy_any, &missing_rename)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.character.skill.plan.SetDescriptionRequest"](&ctx_other, &dummy_any, &uuid_msg(1, missing_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.character.skill.plan.SetSkillRequirementsRequest"](&ctx_other, &dummy_any, &uuid_msg(1, missing_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    // Milestones: both type_id milestone (field 2, Rifter 587) & skill_req milestone (field 3), invalid type/skill error branches, GetAll serializing both
    let mut bad_ms_tid = uuid_msg(1, other_uuid);
    let mut bad_t_msg = DynamicQuasarMessage::new("TypeId");
    bad_t_msg.fields.insert(1, DynamicValue::Uint32(999_999_999));
    bad_ms_tid.fields.insert(2, DynamicValue::Message(Box::new(bad_t_msg)));
    let mut bad_ms_wrap = DynamicQuasarMessage::new("Create");
    bad_ms_wrap.fields.insert(1, DynamicValue::Message(Box::new(bad_ms_tid)));
    assert_eq!(
        handlers["eve_public.character.skill.plan.milestone.CreateRequest"](&ctx_other, &dummy_any, &bad_ms_wrap)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    let mut bad_ms_sr = uuid_msg(1, other_uuid);
    let mut bad_sr = DynamicQuasarMessage::new("SR");
    bad_sr.fields.insert(1, DynamicValue::Uint32(34)); // non-skill
    bad_sr.fields.insert(2, DynamicValue::Uint32(2));
    bad_ms_sr.fields.insert(3, DynamicValue::Message(Box::new(bad_sr)));
    let mut bad_ms_sr_wrap = DynamicQuasarMessage::new("Create");
    bad_ms_sr_wrap.fields.insert(1, DynamicValue::Message(Box::new(bad_ms_sr)));
    assert_eq!(
        handlers["eve_public.character.skill.plan.milestone.CreateRequest"](&ctx_other, &dummy_any, &bad_ms_sr_wrap)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Valid train_to_type (587 Rifter) AND valid skill_req milestone, then GetAllRequest serializing both!
    let mut tid_ms_attr = uuid_msg(1, other_uuid);
    let mut tid_msg = DynamicQuasarMessage::new("TypeId");
    tid_msg.fields.insert(1, DynamicValue::Uint32(587));
    tid_ms_attr.fields.insert(2, DynamicValue::Message(Box::new(tid_msg)));
    tid_ms_attr.fields.insert(4, DynamicValue::String("Fly Rifter".into()));
    let mut tid_ms_create = DynamicQuasarMessage::new("Create");
    tid_ms_create.fields.insert(1, DynamicValue::Message(Box::new(tid_ms_attr)));
    handlers["eve_public.character.skill.plan.milestone.CreateRequest"](&ctx_other, &dummy_any, &tid_ms_create)
        .unwrap();

    let mut sr_ms_attr = uuid_msg(1, other_uuid);
    let mut sr_tid = DynamicQuasarMessage::new("ST");
    sr_tid.fields.insert(1, DynamicValue::Uint32(3300));
    let mut sr_msg = DynamicQuasarMessage::new("SR");
    sr_msg.fields.insert(1, DynamicValue::Message(Box::new(sr_tid)));
    sr_msg.fields.insert(2, DynamicValue::Uint32(3));
    sr_ms_attr.fields.insert(3, DynamicValue::Message(Box::new(sr_msg)));
    sr_ms_attr.fields.insert(4, DynamicValue::String("Gunnery III".into()));
    let mut sr_ms_create = DynamicQuasarMessage::new("Create");
    sr_ms_create.fields.insert(1, DynamicValue::Message(Box::new(sr_ms_attr)));
    handlers["eve_public.character.skill.plan.milestone.CreateRequest"](&ctx_other, &dummy_any, &sr_ms_create)
        .unwrap();

    let all_ms = handlers["eve_public.character.skill.plan.milestone.GetAllRequest"](
        &ctx_other,
        &dummy_any,
        &uuid_msg(1, other_uuid),
    )
    .unwrap();
    assert!(!all_ms.value.is_empty());

    assert_eq!(
        handlers["eve_public.character.skill.plan.milestone.SetDescriptionRequest"](
            &ctx_other,
            &dummy_any,
            &uuid_msg(1, missing_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.character.skill.plan.milestone.DeleteRequest"](
            &ctx_other,
            &dummy_any,
            &uuid_msg(1, missing_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    handlers["eve_public.character.skill.plan.DeleteRequest"](
        &ctx_other,
        &dummy_any,
        &uuid_msg(1, other_uuid),
    )
    .unwrap();
    // Delete non-existent plan now fails STATUS_NOT_FOUND
    assert_eq!(
        handlers["eve_public.character.skill.plan.DeleteRequest"](
            &ctx_other,
            &dummy_any,
            &uuid_msg(1, other_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
}

