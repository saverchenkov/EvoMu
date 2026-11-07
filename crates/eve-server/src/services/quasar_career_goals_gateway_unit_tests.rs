//! Semantic unit tests split from `quasar_career_goals_extra_unit_tests`.

#![forbid(unsafe_code)]

use super::super::*;
use super::*;

use super::super::quasar_career_goals::*;
use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use eve_db::{IskCents, WorldRepository};
use eve_proto::quasar::{
    encode_varint_field, AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny,
    QuasarDescriptorRegistry,
};
use std::sync::{Arc, OnceLock};

#[test]
fn gateway_handlers_all_endpoints_aliases_and_wire_fallbacks() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), sde);
    let mgr = Arc::new(CareerGoalManager::new(Some(Arc::clone(&db))));

    let u1 = [0x22; 16];
    mgr.configure_definition(CareerGoalDef {
        uuid: u1,
        career_id: 1,
        activity_name: "Scanning".into(),
        activity_kind: "scan_test".into(),
        name: "Scan Goal".into(),
        description: "Desc".into(),
        target: 3,
        career_points: 750,
        isk_reward: 10_000,
        sp_reward: 100.0,
        name_message_id: 0,
        description_message_id: 0,
        threat: 1,
        _provenance: None,
    });
    register_career_goal_handlers(&router, Arc::clone(&db), Arc::clone(&mgr));

    let handlers = router.handlers.read().unwrap();
    let any = GoogleAny::default();
    let cid = 90_000_001_u32;

    let unauth_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![]);
    let user_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(1_000_009),
        0,
        "tranquility",
        vec![],
    );
    let admin_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![1, 2],
    )
    .with_service_roles(u64::MAX);

    // 1. Unauthenticated checks on character-scoped handlers
    for ep in [
        "eve_public.career.goal.api.GetAllRequest",
        "eve_public.career.goal.api.ClaimRewardRequest",
        "eve_public.app.eveonline.career.goal.GetAllRequest",
        "eve_public.app.eveonline.career.goal.ClaimRewardRequest",
        "eve.character.career.goal.GetProgressRequest",
        "eve.character.career.goal.HasClaimedRewardRequest",
    ] {
        assert_eq!(
            handlers[ep](&unauth_ctx, &any, &DynamicQuasarMessage::new("Req"))
                .unwrap_err()
                .0,
            STATUS_FORBIDDEN
        );
    }

    // 2. GetDefinitionsRequest & .requests. aliases (public and internal eve.career.goal.api)
    for ep in [
        "eve_public.career.goal.api.GetDefinitionsRequest",
        "eve_public.career.goal.api.requests.GetDefinitionsRequest",
        "eve.career.goal.api.GetDefinitionsRequest",
        "eve.career.goal.api.requests.GetDefinitionsRequest",
    ] {
        let resp = handlers[ep](&unauth_ctx, &any, &DynamicQuasarMessage::new("Req")).expect(ep);
        assert!(!resp.value.is_empty());
    }

    // 3. Admin endpoints reject non-admin user_ctx
    for ep in [
        "eve_public.career.goal.api.admin.ProgressRequest",
        "eve_public.career.goal.api.admin.CompleteRequest",
        "eve.career.goal.api.admin.GetAllByCharacterRequest",
        "eve.career.goal.api.admin.requests.GetAllByCharacterRequest",
        "eve.career.goal.api.admin.CompleteRequest",
        "eve.career.goal.api.admin.requests.CompleteRequest",
    ] {
        assert_eq!(
            handlers[ep](&user_ctx, &any, &DynamicQuasarMessage::new("Req"))
                .unwrap_err()
                .0,
            STATUS_FORBIDDEN
        );
    }

    // 4. admin.ProgressRequest: missing UUID, delta == 0 error, unknown UUID error, WireMessage field 3 fallback, completion
    assert_eq!(
        handlers["eve_public.career.goal.api.admin.ProgressRequest"](
            &admin_ctx,
            &any,
            &DynamicQuasarMessage::new("Req")
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let mut zero_delta = uuid_req(1, &u1);
    zero_delta.fields.insert(2, DynamicValue::Uint64(0));
    assert_eq!(
        handlers["eve_public.career.goal.api.admin.ProgressRequest"](
            &admin_ctx,
            &any,
            &zero_delta
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.career.goal.api.admin.ProgressRequest"](
            &admin_ctx,
            &any,
            &uuid_req(1, &[0xFF; 16])
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    // WireMessage field 3 varint fallback (delta = 3 -> completes goal with target 3!)
    let mut raw_wire = Vec::new();
    encode_varint_field(3, 3, &mut raw_wire);
    let any_with_wire = GoogleAny {
        type_url: String::new(),
        value: raw_wire,
    };
    handlers["eve_public.career.goal.api.admin.ProgressRequest"](
        &admin_ctx,
        &any_with_wire,
        &uuid_req(1, &u1),
    )
    .expect("progress via wire fallback completes goal");

    // 5. GetProgressRequest & HasClaimedRewardRequest (field 1 and field 2 UUIDs, unclaimed vs claimed)
    assert_eq!(
        handlers["eve.character.career.goal.GetProgressRequest"](
            &user_ctx,
            &any,
            &DynamicQuasarMessage::new("Empty")
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve.character.career.goal.GetProgressRequest"](
            &user_ctx,
            &any,
            &uuid_req(2, &[0xFF; 16])
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );
    let prog_resp = handlers["eve.character.career.goal.GetProgressRequest"](
        &user_ctx,
        &any,
        &uuid_req(2, &u1),
    )
    .unwrap();
    assert!(!prog_resp.value.is_empty());

    let unclaimed_resp = handlers["eve.character.career.goal.HasClaimedRewardRequest"](
        &user_ctx,
        &any,
        &uuid_req(2, &u1),
    )
    .unwrap();
    assert!(unclaimed_resp.value.is_empty() || unclaimed_resp.value == vec![8, 0]);

    // 6. Claim goal reward and milestone reward via eve_public.app.eveonline.career.goal.ClaimRewardRequest
    assert_eq!(
        handlers["eve_public.app.eveonline.career.goal.ClaimRewardRequest"](
            &user_ctx,
            &any,
            &DynamicQuasarMessage::new("Empty")
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    handlers["eve_public.app.eveonline.career.goal.ClaimRewardRequest"](
        &user_ctx,
        &any,
        &uuid_req(1, &u1),
    )
    .expect("claim goal via app endpoint");

    let ms_uuid = career_milestone_uuid(1);
    handlers["eve_public.app.eveonline.career.goal.ClaimRewardRequest"](
        &user_ctx,
        &any,
        &uuid_req(1, &ms_uuid),
    )
    .expect("claim milestone 1 via app endpoint");

    let claimed_resp = handlers["eve.character.career.goal.HasClaimedRewardRequest"](
        &user_ctx,
        &any,
        &uuid_req(1, &u1),
    )
    .unwrap();
    assert_eq!(claimed_resp.value, vec![8, 1]);

    // 7. app.GetAllRequest, admin.GetAllByCharacterRequest (.requests. alias), and admin.CompleteRequest (.requests. alias)
    let app_all = handlers["eve_public.app.eveonline.career.goal.GetAllRequest"](
        &user_ctx,
        &any,
        &DynamicQuasarMessage::new("Req"),
    )
    .unwrap();
    assert!(!app_all.value.is_empty());

    let admin_all = handlers["eve.career.goal.api.admin.requests.GetAllByCharacterRequest"](
        &admin_ctx,
        &any,
        &DynamicQuasarMessage::new("Req"),
    )
    .unwrap();
    assert!(!admin_all.value.is_empty());

    handlers["eve_public.career.goal.api.admin.CompleteRequest"](
        &admin_ctx,
        &any,
        &uuid_req(1, &u1),
    )
    .expect("public admin complete");
    handlers["eve.career.goal.api.admin.requests.CompleteRequest"](
        &admin_ctx,
        &any,
        &uuid_req(1, &u1),
    )
    .expect("internal admin complete alias");
}
