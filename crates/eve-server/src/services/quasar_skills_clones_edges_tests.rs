//! Edge-case behavioral unit tests for `quasar_skills_clones`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use crate::services::account_services::account_services_tests::test_ctx;
use eve_db::{InventoryFlag, InventoryItemRecord, WorldRepository};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry};
use eve_sde::InvType;
use std::sync::Arc;

fn uuid_msg(field: u32, uuid: [u8; 16]) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

fn wire_bytes_for_sub_uuid(field: u32, uuid: &[u8]) -> Vec<u8> {
    let mut sub = Vec::new();
    encode_bytes_field(1, uuid, &mut sub);
    let mut outer = Vec::new();
    encode_bytes_field(field, &sub, &mut outer);
    outer
}

#[test]
fn manager_persistence_extractors_and_parse_skill_requirements_edges() {
    let sde = test_ctx().sde.clone();
    let db = Arc::new(WorldRepository::new_empty());

    // 1. Corrupted JSON falls back cleanly to defaults
    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs
            .insert("quasar_skill_plans".into(), "{not valid json".into());
    });
    let mgr_bad = SkillPlanManager::new_with_db(Some(Arc::clone(&db)));
    assert!(mgr_bad.get_personal_plans(90_000_001).is_empty());

    // 2. Legacy 3-tuple format + milestone MSCH sequence extraction updates initial_seq
    let mut plan_uuid = [0u8; 16];
    plan_uuid[0..4].copy_from_slice(b"SPCH");
    plan_uuid[8..16].copy_from_slice(&420_u64.to_be_bytes());
    let mut ms_uuid = [0u8; 16];
    ms_uuid[0..4].copy_from_slice(b"MSCH");
    ms_uuid[8..16].copy_from_slice(&777_u64.to_be_bytes());
    let legacy_tuple_json = serde_json::to_string(&(
        std::collections::HashMap::from([(
            90_000_001_u32,
            vec![SkillPlan {
                uuid: plan_uuid,
                owner_char_id: Some(90_000_001),
                owner_corp_id: None,
                name: "LegacyPlan".into(),
                description: "Desc".into(),
                category_id: 1,
                requirements: vec![],
                milestones: vec![SkillPlanMilestone {
                    uuid: ms_uuid,
                    description: "MS".into(),
                    type_id: Some(587),
                    skill_requirement: None,
                }],
            }],
        )]),
        std::collections::HashMap::<u32, Vec<SkillPlan>>::new(),
        std::collections::HashMap::from([(90_000_001_u32, plan_uuid)]),
    ))
    .unwrap();
    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs
            .insert("quasar_skill_plans".into(), legacy_tuple_json);
    });
    let mgr_legacy = SkillPlanManager::new_with_db(Some(Arc::clone(&db)));
    assert_eq!(mgr_legacy.get_personal_plans(90_000_001).len(), 1);
    assert_eq!(mgr_legacy.get_active_plan(90_000_001).unwrap().name, "LegacyPlan");
    assert!(mgr_legacy.next_seq.load(Ordering::Relaxed) >= 778);

    // 3. extract_active_plan_uuid via WireMessage field 2, field 1, and zero-UUID rejection
    let empty_dyn = DynamicQuasarMessage::new("Empty");
    let any_field2 = GoogleAny {
        type_url: "t".into(),
        value: wire_bytes_for_sub_uuid(2, &plan_uuid),
    };
    assert_eq!(extract_active_plan_uuid(&any_field2, &empty_dyn), Some(plan_uuid));
    let any_zero = GoogleAny {
        type_url: "t".into(),
        value: wire_bytes_for_sub_uuid(1, &[0u8; 16]),
    };
    assert_eq!(extract_active_plan_uuid(&any_zero, &empty_dyn), None);

    // 4. extract_u32 variants: Uint64, Int64 positive/non-positive, nested Message
    assert_eq!(extract_u32(Some(&DynamicValue::Uint64(3300))), Some(3300));
    assert_eq!(extract_u32(Some(&DynamicValue::Int32(3300))), Some(3300));
    assert_eq!(extract_u32(Some(&DynamicValue::Int64(3300))), Some(3300));
    assert_eq!(extract_u32(Some(&DynamicValue::Int64(0))), None);
    let mut nested = DynamicQuasarMessage::new("TypeId");
    nested.fields.insert(1, DynamicValue::Uint64(3300));
    assert_eq!(extract_u32(Some(&DynamicValue::Message(Box::new(nested)))), Some(3300));

    // 5. parse_skill_requirements rejects non-skill type (Tritanium 34) and level 0
    let mut non_skill = DynamicQuasarMessage::new("SkillReq");
    non_skill.fields.insert(1, DynamicValue::Uint32(34));
    non_skill.fields.insert(2, DynamicValue::Uint32(3));
    assert_eq!(
        parse_skill_requirements(
            Some(&DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(non_skill))])),
            &sde,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
}

#[test]
fn skill_plan_and_character_plan_milestone_error_and_variant_paths() {
    let sde = test_ctx().sde.clone();
    let db = Arc::new(WorldRepository::new_empty());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    let mgr = Arc::new(SkillPlanManager::new_with_db(Some(Arc::clone(&db))));
    register_skill_handlers(&router, Arc::clone(&db), Arc::clone(&mgr));

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let cid = 90_600_001_u32;
    let ctx = AuthoritativeContext::for_session(Some(1), Some(cid), Some(1_000_009), 0, "tranquility", vec![1]);
    let other_ctx = AuthoritativeContext::for_session(Some(2), Some(cid + 1), Some(1_000_009), 0, "tranquility", vec![1]);

    // Unauthenticated sweeps
    let unauth = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![]);
    for key in [
        "eve_public.skill.plan.GetAllRequest",
        "eve_public.skill.plan.GetRequest",
        "eve_public.skill.plan.CreateRequest",
        "eve_public.skill.plan.DeleteRequest",
        "eve_public.skill.plan.GetActiveRequest",
        "eve_public.skill.plan.SetActiveRequest",
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
        "eve.skill.character.api.GetLevelRequest",
    ] {
        assert_eq!(handlers[key](&unauth, &dummy_any, &DynamicQuasarMessage::new("E")).unwrap_err().0, STATUS_FORBIDDEN);
    }

    // Inject/Extract unimplemented RPCs
    assert_eq!(handlers["eve.skill.injector.InjectSkillPointsRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("E")).unwrap_err().0, STATUS_NOT_IMPLEMENTED);
    assert_eq!(handlers["eve.skill.extractor.ExtractSkillsRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("E")).unwrap_err().0, STATUS_NOT_IMPLEMENTED);

    // eve_public.skill.plan.* errors & Message attr CreateRequest
    let mut create_attr_msg = DynamicQuasarMessage::new("Attr");
    create_attr_msg.fields.insert(1, DynamicValue::String("AttrPlan".into()));
    create_attr_msg.fields.insert(3, DynamicValue::String("Detailed desc".into()));
    let mut req_item = DynamicQuasarMessage::new("SkillReq");
    req_item.fields.insert(1, DynamicValue::Uint32(3300));
    req_item.fields.insert(2, DynamicValue::Uint32(5));
    create_attr_msg.fields.insert(2, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(req_item.clone()))]));
    let mut create_sp = DynamicQuasarMessage::new("Create");
    create_sp.fields.insert(1, DynamicValue::Message(Box::new(create_attr_msg.clone())));
    handlers["eve_public.skill.plan.CreateRequest"](&ctx, &dummy_any, &create_sp).unwrap();

    // Whitespace name rejected
    let mut ws_attr = DynamicQuasarMessage::new("Attr");
    ws_attr.fields.insert(1, DynamicValue::String("   ".into()));
    let mut ws_create = DynamicQuasarMessage::new("Create");
    ws_create.fields.insert(1, DynamicValue::Message(Box::new(ws_attr)));
    assert_eq!(handlers["eve_public.skill.plan.CreateRequest"](&ctx, &dummy_any, &ws_create).unwrap_err().0, STATUS_BAD_REQUEST);

    // GetRequest / DeleteRequest / SetActiveRequest errors + empty GetActive
    assert_eq!(handlers["eve_public.skill.plan.GetRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("NoId")).unwrap_err().0, STATUS_BAD_REQUEST);
    let missing_uuid = [9u8; 16];
    assert_eq!(handlers["eve_public.skill.plan.GetRequest"](&ctx, &dummy_any, &uuid_msg(1, missing_uuid)).unwrap_err().0, STATUS_NOT_FOUND);
    assert_eq!(handlers["eve_public.skill.plan.DeleteRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("NoId")).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(handlers["eve_public.skill.plan.DeleteRequest"](&ctx, &dummy_any, &uuid_msg(1, missing_uuid)).unwrap_err().0, STATUS_NOT_FOUND);
    assert_eq!(handlers["eve_public.skill.plan.SetActiveRequest"](&ctx, &dummy_any, &uuid_msg(1, missing_uuid)).unwrap_err().0, STATUS_NOT_FOUND);
    let act_zero = handlers["eve_public.skill.plan.GetActiveRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("A")).unwrap();
    assert!(act_zero.type_url.ends_with("GetActiveResponse"));

    // eve_public.character.skill.plan.* full lifecycle + errors
    assert_eq!(handlers["eve_public.character.skill.plan.CreateRequest"](&ctx, &dummy_any, &ws_create).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(handlers["eve_public.character.skill.plan.CreateRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Empty")).unwrap_err().0, STATUS_BAD_REQUEST);
    // String form create + Message form create
    let mut str_create = DynamicQuasarMessage::new("Create");
    str_create.fields.insert(1, DynamicValue::String("CharPlanStr".into()));
    handlers["eve_public.character.skill.plan.CreateRequest"](&ctx, &dummy_any, &str_create).unwrap();
    handlers["eve_public.character.skill.plan.CreateRequest"](&ctx, &dummy_any, &create_sp).unwrap();

    let plans = mgr.get_personal_plans(cid);
    let target_uuid = plans.last().unwrap().uuid;

    // Empty GetActive before SetActive
    assert!(handlers["eve_public.character.skill.plan.GetActiveRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("A")).unwrap().type_url.ends_with("GetActiveResponse"));
    // Get / GetShared / GetAll / SetActive / ClearActive
    assert_eq!(handlers["eve_public.character.skill.plan.GetRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("NoId")).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(handlers["eve_public.character.skill.plan.GetRequest"](&ctx, &dummy_any, &uuid_msg(1, missing_uuid)).unwrap_err().0, STATUS_NOT_FOUND);
    assert!(handlers["eve_public.character.skill.plan.GetRequest"](&ctx, &dummy_any, &uuid_msg(1, target_uuid)).unwrap().type_url.ends_with("GetResponse"));
    assert!(handlers["eve_public.character.skill.plan.GetSharedRequest"](&other_ctx, &dummy_any, &uuid_msg(1, target_uuid)).unwrap().type_url.ends_with("GetSharedResponse"));
    assert_eq!(handlers["eve_public.character.skill.plan.GetSharedRequest"](&other_ctx, &dummy_any, &uuid_msg(1, missing_uuid)).unwrap_err().0, STATUS_NOT_FOUND);
    assert!(handlers["eve_public.character.skill.plan.GetAllRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("All")).unwrap().type_url.ends_with("GetAllResponse"));
    assert_eq!(handlers["eve_public.character.skill.plan.SetActiveRequest"](&ctx, &dummy_any, &uuid_msg(1, missing_uuid)).unwrap_err().0, STATUS_NOT_FOUND);
    handlers["eve_public.character.skill.plan.SetActiveRequest"](&ctx, &dummy_any, &uuid_msg(1, target_uuid)).unwrap();
    assert!(handlers["eve_public.character.skill.plan.GetActiveRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("A")).unwrap().type_url.ends_with("GetActiveResponse"));
    handlers["eve_public.character.skill.plan.SetActiveRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Clear")).unwrap();

    // SetName / SetDescription / SetSkillRequirements errors and success
    let mut set_name = uuid_msg(1, target_uuid);
    set_name.fields.insert(2, DynamicValue::String("   ".into()));
    assert_eq!(handlers["eve_public.character.skill.plan.SetNameRequest"](&ctx, &dummy_any, &set_name).unwrap_err().0, STATUS_BAD_REQUEST);
    set_name.fields.insert(2, DynamicValue::String("Renamed".into()));
    handlers["eve_public.character.skill.plan.SetNameRequest"](&ctx, &dummy_any, &set_name).unwrap();
    assert_eq!(handlers["eve_public.character.skill.plan.SetNameRequest"](&other_ctx, &dummy_any, &set_name).unwrap_err().0, STATUS_NOT_FOUND);

    let mut set_desc = uuid_msg(1, target_uuid);
    set_desc.fields.insert(2, DynamicValue::String("Updated desc".into()));
    handlers["eve_public.character.skill.plan.SetDescriptionRequest"](&ctx, &dummy_any, &set_desc).unwrap();
    assert_eq!(handlers["eve_public.character.skill.plan.SetDescriptionRequest"](&other_ctx, &dummy_any, &set_desc).unwrap_err().0, STATUS_NOT_FOUND);

    let mut set_reqs = uuid_msg(1, target_uuid);
    set_reqs.fields.insert(2, DynamicValue::Repeated(vec![DynamicValue::Message(Box::new(req_item))]));
    handlers["eve_public.character.skill.plan.SetSkillRequirementsRequest"](&ctx, &dummy_any, &set_reqs).unwrap();
    assert_eq!(handlers["eve_public.character.skill.plan.SetSkillRequirementsRequest"](&other_ctx, &dummy_any, &set_reqs).unwrap_err().0, STATUS_NOT_FOUND);

    // Milestones: Create error variants (missing attr, bad train_to_type, bad skill req, unknown plan)
    assert_eq!(handlers["eve_public.character.skill.plan.milestone.CreateRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Empty")).unwrap_err().0, STATUS_BAD_REQUEST);
    let mut bad_type_ms_attr = uuid_msg(1, target_uuid);
    let mut bad_tid_msg = DynamicQuasarMessage::new("TypeId");
    bad_tid_msg.fields.insert(1, DynamicValue::Uint64(999_999_999));
    bad_type_ms_attr.fields.insert(2, DynamicValue::Message(Box::new(bad_tid_msg)));
    let mut bad_type_ms = DynamicQuasarMessage::new("MsCreate");
    bad_type_ms.fields.insert(1, DynamicValue::Message(Box::new(bad_type_ms_attr)));
    assert_eq!(handlers["eve_public.character.skill.plan.milestone.CreateRequest"](&ctx, &dummy_any, &bad_type_ms).unwrap_err().0, STATUS_BAD_REQUEST);

    // Invalid milestone skill requirement (level 0 and non-skill)
    let mut bad_sr_attr = uuid_msg(1, target_uuid);
    let mut sr_msg = DynamicQuasarMessage::new("SkillReq");
    let mut sr_tid = DynamicQuasarMessage::new("TypeId");
    sr_tid.fields.insert(1, DynamicValue::Uint32(34)); // Tritanium, not skill
    sr_msg.fields.insert(1, DynamicValue::Message(Box::new(sr_tid)));
    sr_msg.fields.insert(2, DynamicValue::Uint64(3));
    bad_sr_attr.fields.insert(3, DynamicValue::Message(Box::new(sr_msg)));
    let mut bad_sr_ms = DynamicQuasarMessage::new("MsCreate");
    bad_sr_ms.fields.insert(1, DynamicValue::Message(Box::new(bad_sr_attr)));
    assert_eq!(handlers["eve_public.character.skill.plan.milestone.CreateRequest"](&ctx, &dummy_any, &bad_sr_ms).unwrap_err().0, STATUS_BAD_REQUEST);

    // Create two valid milestones: one with train_to_type (587), one with skill_requirement (3300 lvl 4)
    let mut ms_attr_type = uuid_msg(1, target_uuid);
    let mut good_tid = DynamicQuasarMessage::new("TypeId");
    good_tid.fields.insert(1, DynamicValue::Uint64(587));
    ms_attr_type.fields.insert(2, DynamicValue::Message(Box::new(good_tid)));
    ms_attr_type.fields.insert(4, DynamicValue::String("Fly Rifter".into()));
    let mut ms_create_1 = DynamicQuasarMessage::new("MsCreate");
    ms_create_1.fields.insert(1, DynamicValue::Message(Box::new(ms_attr_type)));
    handlers["eve_public.character.skill.plan.milestone.CreateRequest"](&ctx, &dummy_any, &ms_create_1).unwrap();

    let mut ms_attr_sr = uuid_msg(1, target_uuid);
    let mut good_sr = DynamicQuasarMessage::new("SkillReq");
    good_sr.fields.insert(1, DynamicValue::Uint64(3300));
    good_sr.fields.insert(2, DynamicValue::Uint64(4));
    ms_attr_sr.fields.insert(3, DynamicValue::Message(Box::new(good_sr)));
    ms_attr_sr.fields.insert(4, DynamicValue::String("Train Gunnery 4".into()));
    let mut ms_create_2 = DynamicQuasarMessage::new("MsCreate");
    ms_create_2.fields.insert(1, DynamicValue::Message(Box::new(ms_attr_sr)));
    handlers["eve_public.character.skill.plan.milestone.CreateRequest"](&ctx, &dummy_any, &ms_create_2).unwrap();

    let ms_list = mgr.get_personal_plans(cid).into_iter().find(|p| p.uuid == target_uuid).unwrap().milestones;
    assert_eq!(ms_list.len(), 2);
    let ms1_uuid = ms_list[0].uuid;

    // milestone.GetAllRequest: valid + missing/not found
    assert!(handlers["eve_public.character.skill.plan.milestone.GetAllRequest"](&ctx, &dummy_any, &uuid_msg(1, target_uuid)).unwrap().type_url.ends_with("milestone.GetAllResponse"));
    assert_eq!(handlers["eve_public.character.skill.plan.milestone.GetAllRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("NoId")).unwrap_err().0, STATUS_BAD_REQUEST);
    assert_eq!(handlers["eve_public.character.skill.plan.milestone.GetAllRequest"](&ctx, &dummy_any, &uuid_msg(1, missing_uuid)).unwrap_err().0, STATUS_NOT_FOUND);

    // milestone.SetDescriptionRequest / DeleteRequest: wrong char (no plans) & missing UUID & success
    let mut ms_set_desc = uuid_msg(1, ms1_uuid);
    ms_set_desc.fields.insert(2, DynamicValue::String("New MS Desc".into()));
    assert_eq!(handlers["eve_public.character.skill.plan.milestone.SetDescriptionRequest"](&other_ctx, &dummy_any, &ms_set_desc).unwrap_err().0, STATUS_NOT_FOUND);
    assert_eq!(handlers["eve_public.character.skill.plan.milestone.SetDescriptionRequest"](&ctx, &dummy_any, &uuid_msg(1, missing_uuid)).unwrap_err().0, STATUS_NOT_FOUND);
    handlers["eve_public.character.skill.plan.milestone.SetDescriptionRequest"](&ctx, &dummy_any, &ms_set_desc).unwrap();

    assert_eq!(handlers["eve_public.character.skill.plan.milestone.DeleteRequest"](&other_ctx, &dummy_any, &uuid_msg(1, ms1_uuid)).unwrap_err().0, STATUS_NOT_FOUND);
    assert_eq!(handlers["eve_public.character.skill.plan.milestone.DeleteRequest"](&ctx, &dummy_any, &uuid_msg(1, missing_uuid)).unwrap_err().0, STATUS_NOT_FOUND);
    handlers["eve_public.character.skill.plan.milestone.DeleteRequest"](&ctx, &dummy_any, &uuid_msg(1, ms1_uuid)).unwrap();

    // character.skill.plan.DeleteRequest errors + success
    assert_eq!(handlers["eve_public.character.skill.plan.DeleteRequest"](&other_ctx, &dummy_any, &uuid_msg(1, target_uuid)).unwrap_err().0, STATUS_NOT_FOUND);
    assert_eq!(handlers["eve_public.character.skill.plan.DeleteRequest"](&ctx, &dummy_any, &uuid_msg(1, missing_uuid)).unwrap_err().0, STATUS_NOT_FOUND);
    handlers["eve_public.character.skill.plan.DeleteRequest"](&ctx, &dummy_any, &uuid_msg(1, target_uuid)).unwrap();
}

#[test]
fn skilltype_getlevel_and_implants_encoding_variant_edges() {
    let mut custom_sde = (*test_ctx().sde).clone();
    // Insert a custom implant with no attribute 331 to exercise fallback slot = (idx + 1)
    let custom_imp_tid = 9_888_001_u32;
    let base_type = custom_sde.types.values().next().unwrap().clone();
    custom_sde.types.insert(
        custom_imp_tid,
        InvType {
            type_id: custom_imp_tid,
            group_id: 300,
            category_id: 20,
            name: "NoSlotTestImplant".into(),
            published: true,
            default_attributes: Default::default(),
            ..base_type
        },
    );
    let sde = Arc::new(custom_sde);
    let db = Arc::new(WorldRepository::new_empty());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    let mgr = Arc::new(SkillPlanManager::new_with_db(Some(Arc::clone(&db))));
    register_skill_handlers(&router, Arc::clone(&db), mgr);

    let handlers = router.handlers.read().unwrap();
    let cid = 90_600_055_u32;
    let ctx = AuthoritativeContext::for_session(Some(1), Some(cid), None, 0, "tranquility", vec![1]);
    let dummy_any = GoogleAny::default();

    // eve.skilltype.GetRequest across Int32/Uint64/Int64, nested Message, WireMessage varint & nested submessage, and not found
    for val in [
        DynamicValue::Int32(3300),
        DynamicValue::Uint64(3300),
        DynamicValue::Int64(3300),
    ] {
        let mut msg = DynamicQuasarMessage::new("Get");
        msg.fields.insert(1, val);
        assert!(handlers["eve.skilltype.GetRequest"](&ctx, &dummy_any, &msg).is_ok());
    }
    let mut nested_t = DynamicQuasarMessage::new("Id");
    nested_t.fields.insert(1, DynamicValue::Int64(3300));
    let mut nested_req = DynamicQuasarMessage::new("Get");
    nested_req.fields.insert(1, DynamicValue::Message(Box::new(nested_t)));
    assert!(handlers["eve.skilltype.GetRequest"](&ctx, &dummy_any, &nested_req).is_ok());

    let mut wire_direct = Vec::new();
    encode_varint_field(1, 3300, &mut wire_direct);
    let any_direct = GoogleAny { type_url: "t".into(), value: wire_direct };
    assert!(handlers["eve.skilltype.GetRequest"](&ctx, &any_direct, &DynamicQuasarMessage::new("E")).is_ok());

    let mut wire_sub = Vec::new();
    encode_varint_field(1, 3300, &mut wire_sub);
    let mut wire_outer = Vec::new();
    encode_bytes_field(1, &wire_sub, &mut wire_outer);
    let any_nested = GoogleAny { type_url: "t".into(), value: wire_outer };
    assert!(handlers["eve.skilltype.GetRequest"](&ctx, &any_nested, &DynamicQuasarMessage::new("E")).is_ok());

    let mut bad_skill = DynamicQuasarMessage::new("Get");
    bad_skill.fields.insert(1, DynamicValue::Uint32(999_999_999));
    assert_eq!(handlers["eve.skilltype.GetRequest"](&ctx, &dummy_any, &bad_skill).unwrap_err().0, STATUS_NOT_FOUND);

    // eve.skill.character.api.GetLevelRequest across Int32/Uint64/Int64 and nested Message
    for val in [
        DynamicValue::Uint64(3300),
        DynamicValue::Int32(3300),
        DynamicValue::Int64(3300),
    ] {
        let mut msg = DynamicQuasarMessage::new("Lvl");
        msg.fields.insert(2, val);
        assert!(handlers["eve.skill.character.api.GetLevelRequest"](&ctx, &dummy_any, &msg).is_ok());
    }
    let mut inner_l = DynamicQuasarMessage::new("Id");
    inner_l.fields.insert(1, DynamicValue::Int32(3300));
    let mut outer_l = DynamicQuasarMessage::new("Lvl");
    outer_l.fields.insert(2, DynamicValue::Message(Box::new(inner_l)));
    assert!(handlers["eve.skill.character.api.GetLevelRequest"](&ctx, &dummy_any, &outer_l).is_ok());

    // eve.character.implant.GetActiveImplantsByCharacterRequest: seed InventoryFlag::Implant + WireMessage varint/16B UUID
    db.with_mut_snapshot(|snap| {
        snap.items.insert(
            777_001,
            InventoryItemRecord {
                item_id: 777_001,
                type_id: custom_imp_tid,
                owner_id: i64::from(cid),
                location_id: i64::from(cid),
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });
    for val in [
        DynamicValue::Uint32(cid),
        DynamicValue::Uint64(u64::from(cid)),
        DynamicValue::Int32(cid as i32),
        DynamicValue::Int64(i64::from(cid)),
    ] {
        let mut msg = DynamicQuasarMessage::new("Imp");
        msg.fields.insert(1, val);
        let resp = handlers["eve.character.implant.GetActiveImplantsByCharacterRequest"](&ctx, &dummy_any, &msg).unwrap();
        assert!(!resp.value.is_empty());
    }
    let mut uuid16_cid = vec![0u8; 12];
    uuid16_cid.extend_from_slice(&cid.to_be_bytes());
    let any_imp_uuid = GoogleAny {
        type_url: "t".into(),
        value: wire_bytes_for_sub_uuid(1, &uuid16_cid),
    };
    assert!(!handlers["eve.character.implant.GetActiveImplantsByCharacterRequest"](
        &ctx,
        &any_imp_uuid,
        &DynamicQuasarMessage::new("E"),
    )
    .unwrap()
    .value
    .is_empty());

    let mut wire_imp_varint = Vec::new();
    encode_varint_field(1, u64::from(cid), &mut wire_imp_varint);
    let any_imp_varint = GoogleAny { type_url: "t".into(), value: wire_imp_varint };
    assert!(!handlers["eve.character.implant.GetActiveImplantsByCharacterRequest"](
        &ctx,
        &any_imp_varint,
        &DynamicQuasarMessage::new("E"),
    )
    .unwrap()
    .value
    .is_empty());

    // Unauth without target in msg -> STATUS_FORBIDDEN
    let unauth = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![]);
    assert_eq!(
        handlers["eve.character.implant.GetActiveImplantsByCharacterRequest"](
            &unauth,
            &dummy_any,
            &DynamicQuasarMessage::new("E"),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
}

#[test]
fn skill_plan_manager_save_to_db_and_rehydrate_mutants() {
    let db = Arc::new(WorldRepository::new_empty());
    let mgr = SkillPlanManager::new_with_db(Some(Arc::clone(&db)));
    let cid = 90_000_010_u32;

    let mut sp_uuid = [0u8; 16];
    sp_uuid[0..4].copy_from_slice(b"SPCH");
    sp_uuid[8..16].copy_from_slice(&150_u64.to_be_bytes());
    mgr.create_personal_plan(
        cid,
        SkillPlan {
            uuid: sp_uuid,
            owner_char_id: Some(cid),
            owner_corp_id: None,
            name: "SavedPlan".into(),
            description: "Save check".into(),
            category_id: 0,
            requirements: vec![],
            milestones: vec![],
        },
    );
    // save_to_db -> () killed: DB must now contain serialized SavedPlan
    mgr.save_to_db();
    let blob_before = db.with_snapshot(|s| s.quasar_state_blobs.get("quasar_skill_plans").cloned());
    let parsed_saved: QuasarSkillPlansPersistedState =
        serde_json::from_str(blob_before.as_deref().expect("save_to_db wrote blob")).unwrap();
    assert_eq!(parsed_saved.personal_plans[&cid].len(), 1);
    assert_eq!(parsed_saved.personal_plans[&cid][0].name, "SavedPlan");

    // Rehydrate #1: only `SPCH` (seq=150) + non-matching `XXXX` (seq=9999) -> next_seq MUST be 151
    // (kills `rehydrate_from_db -> ()`, `|| -> &&`, `== -> !=` on both prefixes, `+ -> -`, `+ -> *`).
    let mut bad_prefix_uuid = [0u8; 16];
    bad_prefix_uuid[0..4].copy_from_slice(b"XXXX");
    bad_prefix_uuid[8..16].copy_from_slice(&9_999_u64.to_be_bytes());
    let rehyd_a = QuasarSkillPlansPersistedState {
        personal_plans: std::collections::HashMap::from([(
            cid,
            vec![
                SkillPlan {
                    uuid: sp_uuid,
                    owner_char_id: Some(cid),
                    owner_corp_id: None,
                    name: "RehydratedA".into(),
                    description: String::new(),
                    category_id: 0,
                    requirements: vec![],
                    milestones: vec![],
                },
                SkillPlan {
                    uuid: bad_prefix_uuid,
                    owner_char_id: Some(cid),
                    owner_corp_id: None,
                    name: "NonPrefixPlan".into(),
                    description: String::new(),
                    category_id: 0,
                    requirements: vec![],
                    milestones: vec![],
                },
            ],
        )]),
        corp_plans: std::collections::HashMap::new(),
        active_plans: std::collections::HashMap::from([(cid, sp_uuid)]),
        next_seq: 0,
    };
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs
            .insert("quasar_skill_plans".into(), serde_json::to_string(&rehyd_a).unwrap());
    });
    mgr.rehydrate_from_db();
    assert_eq!(mgr.get_personal_plans(cid)[0].name, "RehydratedA");
    assert_eq!(mgr.get_active_plan(cid).unwrap().uuid, sp_uuid);
    assert_eq!(
        mgr.next_seq.load(Ordering::Relaxed),
        151,
        "SPCH max_existing_seq=150 must set next_seq=151 while XXXX=9999 is ignored"
    );

    // Rehydrate #2: adds `MSCH` milestone (seq=200) -> next_seq MUST be 201
    let mut ms_uuid = [0u8; 16];
    ms_uuid[0..4].copy_from_slice(b"MSCH");
    ms_uuid[8..16].copy_from_slice(&200_u64.to_be_bytes());
    let rehyd_b = QuasarSkillPlansPersistedState {
        personal_plans: std::collections::HashMap::from([(
            cid,
            vec![SkillPlan {
                uuid: sp_uuid,
                owner_char_id: Some(cid),
                owner_corp_id: None,
                name: "RehydratedB".into(),
                description: String::new(),
                category_id: 0,
                requirements: vec![],
                milestones: vec![
                    SkillPlanMilestone {
                        uuid: ms_uuid,
                        description: "MS".into(),
                        type_id: Some(587),
                        skill_requirement: None,
                    },
                    SkillPlanMilestone {
                        uuid: bad_prefix_uuid,
                        description: "OtherPrefixMS".into(),
                        type_id: Some(587),
                        skill_requirement: None,
                    },
                ],
            }],
        )]),
        corp_plans: std::collections::HashMap::new(),
        active_plans: std::collections::HashMap::new(),
        next_seq: 0,
    };
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs
            .insert("quasar_skill_plans".into(), serde_json::to_string(&rehyd_b).unwrap());
    });
    mgr.rehydrate_from_db();
    assert_eq!(mgr.get_personal_plans(cid)[0].name, "RehydratedB");
    assert_eq!(
        mgr.next_seq.load(Ordering::Relaxed),
        201,
        "MSCH milestone max_existing_seq=200 must set next_seq=201"
    );
}
