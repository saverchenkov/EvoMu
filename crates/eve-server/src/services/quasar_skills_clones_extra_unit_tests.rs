#![forbid(unsafe_code)]

use super::quasar_skills_clones::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use eve_db::{
    corp_roles, InventoryFlag, InventoryItemRecord, TrainedSkillRecord, WorldRepository,
};
use eve_machonet::service_roles;
use eve_proto::quasar::{
    encode_bytes_field, encode_varint_field, AuthoritativeContext, DynamicQuasarMessage,
    DynamicValue, GoogleAny, QuasarDescriptorRegistry,
};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn uuid_msg(field: u32, uuid: [u8; 16]) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

#[test]
fn test_skill_plan_manager_legacy_blob_and_active_uuid_wire_extraction() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let mut sp_uuid = [0u8; 16];
    sp_uuid[0..4].copy_from_slice(b"SPCH");
    sp_uuid[8..16].copy_from_slice(&250_u64.to_be_bytes());
    let mut ms_uuid = [0u8; 16];
    ms_uuid[0..4].copy_from_slice(b"MSCH");
    ms_uuid[8..16].copy_from_slice(&400_u64.to_be_bytes());

    let mut personal: HashMap<u32, Vec<SkillPlan>> = HashMap::new();
    personal.insert(
        90_000_001,
        vec![SkillPlan {
            uuid: sp_uuid,
            owner_char_id: Some(90_000_001),
            owner_corp_id: None,
            name: "Legacy Plan".into(),
            description: "From 3-tuple".into(),
            category_id: 1,
            requirements: Vec::new(),
            milestones: vec![SkillPlanMilestone {
                uuid: ms_uuid,
                description: "MS".into(),
                type_id: Some(587),
                skill_requirement: None,
            }],
        }],
    );
    let legacy_tuple = (
        personal,
        HashMap::<u32, Vec<SkillPlan>>::new(),
        HashMap::<u32, [u8; 16]>::new(),
    );
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert(
            "quasar_skill_plans".into(),
            serde_json::to_string(&legacy_tuple).unwrap(),
        );
    });

    let mgr = SkillPlanManager::new_with_db(Some(Arc::clone(&db)));
    assert_eq!(mgr.get_personal_plans(90_000_001).len(), 1);
    assert!(mgr.next_seq.load(std::sync::atomic::Ordering::Relaxed) >= 401);

    // Also test SkillPlanManager::new() without db
    let mem_mgr = SkillPlanManager::new();
    assert!(mem_mgr.get_personal_plans(90_000_001).is_empty());

    // extract_active_plan_uuid from field 2 and from raw WireMessage bytes in GoogleAny
    let msg_f2 = uuid_msg(2, sp_uuid);
    assert_eq!(
        extract_active_plan_uuid(&GoogleAny::default(), &msg_f2),
        Some(sp_uuid)
    );

    let mut sub_wire = Vec::new();
    encode_bytes_field(1, &ms_uuid, &mut sub_wire);
    let mut raw_wire = Vec::new();
    encode_bytes_field(2, &sub_wire, &mut raw_wire);
    let any_with_wire = GoogleAny {
        type_url: String::new(),
        value: raw_wire,
    };
    assert_eq!(
        extract_active_plan_uuid(&any_with_wire, &DynamicQuasarMessage::new("Empty")),
        Some(ms_uuid)
    );
}

#[test]
fn test_skill_plan_and_milestone_error_and_certified_fallback_branches() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    let mgr = Arc::new(SkillPlanManager::new_with_db(Some(Arc::clone(&db))));
    register_skill_handlers(&router, Arc::clone(&db), Arc::clone(&mgr));

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let unauth_ctx = AuthoritativeContext::default();
    let cid = 90_000_001_u32;
    let ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(1_000_009),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );

    // 1. Unauthenticated (no active character) returns STATUS_FORBIDDEN across character-scoped endpoints
    for ep in [
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
        "eve.character.implant.GetActiveImplantsByCharacterRequest",
    ] {
        assert_eq!(
            handlers[ep](&unauth_ctx, &dummy_any, &DynamicQuasarMessage::new("Empty"))
                .unwrap_err()
                .0,
            STATUS_FORBIDDEN,
            "expected STATUS_FORBIDDEN on {ep}"
        );
    }

    // 2. Certified skill plans fallback on GetRequest (both skill.plan and character.skill.plan)
    let cert_plans = mgr.get_certified_plans(&sde);
    if let Some(first_cert) = cert_plans.first() {
        let r1 = handlers["eve_public.skill.plan.GetRequest"](
            &ctx,
            &dummy_any,
            &uuid_msg(1, first_cert.uuid),
        )
        .expect("certified plan via skill.plan.GetRequest");
        assert!(!r1.value.is_empty());
        let r2 = handlers["eve_public.character.skill.plan.GetRequest"](
            &ctx,
            &dummy_any,
            &uuid_msg(1, first_cert.uuid),
        )
        .expect("certified plan via character.skill.plan.GetRequest");
        assert!(!r2.value.is_empty());
    }

    // 3. Missing UUID / unknown UUID / empty name error branches
    let unknown_uuid = [0xAAu8; 16];
    assert_eq!(
        handlers["eve_public.skill.plan.GetRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Empty"))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.skill.plan.GetRequest"](&ctx, &dummy_any, &uuid_msg(1, unknown_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.skill.plan.CreateRequest"](&ctx, &dummy_any, &DynamicQuasarMessage::new("Empty"))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.skill.plan.DeleteRequest"](&ctx, &dummy_any, &uuid_msg(1, unknown_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.skill.plan.SetActiveRequest"](&ctx, &dummy_any, &uuid_msg(1, unknown_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.character.skill.plan.SetActiveRequest"](&ctx, &dummy_any, &uuid_msg(1, unknown_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.character.skill.plan.GetSharedRequest"](&ctx, &dummy_any, &uuid_msg(1, unknown_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.character.skill.plan.SetNameRequest"](&ctx, &dummy_any, &uuid_msg(1, unknown_uuid))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.character.skill.plan.DeleteRequest"](&ctx, &dummy_any, &uuid_msg(1, unknown_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    // 4. Create a personal plan and test milestone train_to_type (valid vs invalid) and nested skill_requirement
    let mut create_msg = DynamicQuasarMessage::new("Create");
    create_msg.fields.insert(1, DynamicValue::String("Milestone Test Plan".into()));
    handlers["eve_public.character.skill.plan.CreateRequest"](&ctx, &dummy_any, &create_msg)
        .expect("create plan");
    let plan_uuid = mgr.get_personal_plans(cid)[0].uuid;

    // Milestone with invalid train_to_type (999_999_999) -> STATUS_BAD_REQUEST
    let mut bad_ms_attr = uuid_msg(1, plan_uuid);
    let mut bad_tid_msg = DynamicQuasarMessage::new("Type");
    bad_tid_msg.fields.insert(1, DynamicValue::Uint32(999_999_999));
    bad_ms_attr.fields.insert(2, DynamicValue::Message(Box::new(bad_tid_msg)));
    let mut bad_ms_req = DynamicQuasarMessage::new("MsCreate");
    bad_ms_req.fields.insert(1, DynamicValue::Message(Box::new(bad_ms_attr)));
    assert_eq!(
        handlers["eve_public.character.skill.plan.milestone.CreateRequest"](&ctx, &dummy_any, &bad_ms_req)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // Milestone with valid train_to_type (587 Rifter)
    let mut good_ms_attr = uuid_msg(1, plan_uuid);
    let mut good_tid_msg = DynamicQuasarMessage::new("Type");
    good_tid_msg.fields.insert(1, DynamicValue::Uint64(587));
    good_ms_attr.fields.insert(2, DynamicValue::Message(Box::new(good_tid_msg)));
    good_ms_attr.fields.insert(4, DynamicValue::String("Fly Rifter".into()));
    let mut good_ms_req = DynamicQuasarMessage::new("MsCreate");
    good_ms_req.fields.insert(1, DynamicValue::Message(Box::new(good_ms_attr)));
    handlers["eve_public.character.skill.plan.milestone.CreateRequest"](&ctx, &dummy_any, &good_ms_req)
        .expect("create type_id milestone");

    // GetAllRequest encodes both type_id milestone and any skill_requirement milestones
    let ms_all = handlers["eve_public.character.skill.plan.milestone.GetAllRequest"](
        &ctx,
        &dummy_any,
        &uuid_msg(1, plan_uuid),
    )
    .expect("milestone GetAll");
    assert!(!ms_all.value.is_empty());

    // Delete / SetDescription on unknown milestone UUID -> STATUS_NOT_FOUND
    assert_eq!(
        handlers["eve_public.character.skill.plan.milestone.DeleteRequest"](
            &ctx,
            &dummy_any,
            &uuid_msg(1, unknown_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        handlers["eve_public.character.skill.plan.milestone.SetDescriptionRequest"](
            &ctx,
            &dummy_any,
            &uuid_msg(1, unknown_uuid),
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
}

#[test]
fn test_skilltype_skill_level_and_active_implants_encodings_and_permissions() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    let mgr = Arc::new(SkillPlanManager::new_with_db(Some(Arc::clone(&db))));
    register_skill_handlers(&router, Arc::clone(&db), Arc::clone(&mgr));

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let cid = 90_000_001_u32;
    let other_cid = 90_000_002_u32;
    let player_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(1_000_009),
        0,
        "tranquility",
        vec![1],
    );
    let mut admin_ctx = player_ctx.clone();
    admin_ctx.service_role_mask = service_roles::ROLE_ADMIN;

    // 1. eve.skilltype.GetRequest with nested Message, WireMessage, and unknown type
    let mut inner_st = DynamicQuasarMessage::new("SkillTypeId");
    inner_st.fields.insert(1, DynamicValue::Int64(3300));
    let mut st_msg = DynamicQuasarMessage::new("GetReq");
    st_msg.fields.insert(1, DynamicValue::Message(Box::new(inner_st)));
    assert!(handlers["eve.skilltype.GetRequest"](&player_ctx, &dummy_any, &st_msg).is_ok());

    let mut st_not_found = DynamicQuasarMessage::new("GetReq");
    st_not_found.fields.insert(1, DynamicValue::Uint32(999_999_999));
    assert_eq!(
        handlers["eve.skilltype.GetRequest"](&player_ctx, &dummy_any, &st_not_found)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    // 2. eve.skill.character.api.GetLevelRequest for trained skill vs untrained skill
    db.with_mut_snapshot(|s| {
        s.skills.entry(cid as i64).or_default().insert(
            3300,
            TrainedSkillRecord {
                type_id: 3300,
                level: 5,
                skill_points: 256_000.0,
            },
        );
    });
    let mut lvl_msg = DynamicQuasarMessage::new("LvlReq");
    let mut tid_sub = DynamicQuasarMessage::new("TypeId");
    tid_sub.fields.insert(1, DynamicValue::Uint64(3300));
    lvl_msg.fields.insert(2, DynamicValue::Message(Box::new(tid_sub)));
    let lvl_resp = handlers["eve.skill.character.api.GetLevelRequest"](&player_ctx, &dummy_any, &lvl_msg)
        .expect("get level 3300");
    assert_eq!(lvl_resp.value, vec![0x08, 0x05]);

    // 3. eve.character.implant.GetActiveImplantsByCharacterRequest
    // Populate both `active_implants` and an `InventoryFlag::Implant` item on `other_cid`
    let imp_tid = sde
        .types
        .values()
        .find(|t| t.category_id == 20 && t.published)
        .map(|t| t.type_id)
        .unwrap_or(9899);
    db.with_mut_snapshot(|s| {
        s.active_implants.insert(other_cid as i64, vec![3300]);
        s.items.insert(
            999_100_001,
            InventoryItemRecord {
                item_id: 999_100_001,
                type_id: imp_tid,
                owner_id: other_cid as i64,
                location_id: other_cid as i64,
                flag: InventoryFlag::Implant,
                quantity: 1,
                singleton: true,
                ..Default::default()
            },
        );
    });

    // Build 16-byte zero-padded character UUID for `other_cid`
    let mut padded_char = [0u8; 16];
    padded_char[12..16].copy_from_slice(&other_cid.to_be_bytes());
    let mut imp_req = uuid_msg(1, padded_char);

    // Non-admin querying `other_cid` -> STATUS_FORBIDDEN
    assert_eq!(
        handlers["eve.character.implant.GetActiveImplantsByCharacterRequest"](
            &player_ctx,
            &dummy_any,
            &imp_req,
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    // Admin querying `other_cid` -> succeeds and returns both implants
    let imp_resp = handlers["eve.character.implant.GetActiveImplantsByCharacterRequest"](
        &admin_ctx,
        &dummy_any,
        &imp_req,
    )
    .expect("admin query implants");
    assert!(!imp_resp.value.is_empty());

    // Also test WireMessage fallback when `dyn_msg` is empty
    let mut sub_bytes = Vec::new();
    encode_varint_field(1, other_cid as u64, &mut sub_bytes);
    let mut outer_bytes = Vec::new();
    encode_bytes_field(1, &sub_bytes, &mut outer_bytes);
    let wire_any = GoogleAny {
        type_url: String::new(),
        value: outer_bytes,
    };
    imp_req.fields.clear();
    let imp_wire_resp = handlers["eve.character.implant.GetActiveImplantsByCharacterRequest"](
        &admin_ctx,
        &wire_any,
        &imp_req,
    )
    .expect("admin query implants via wire");
    assert_eq!(imp_wire_resp.value, imp_resp.value);
}
