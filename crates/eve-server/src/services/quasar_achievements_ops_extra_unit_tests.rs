//! Extra branch-coverage and mutation-killing unit tests for `quasar_achievements_ops`.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    NoticeScope, PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
    STATUS_PRECONDITION_FAILED, STATUS_SERVICE_UNAVAILABLE,
};
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use crate::services::quasar_achievements::{
    AchievementConditionDef, AchievementDef, AchievementManager,
};
use crate::services::quasar_achievements_ops::{
    publish_achievement_category_milestone_notice, publish_achievement_category_progress_notice,
    publish_achievement_milestone_reached_notice, publish_achievement_version_check_notice,
    register_achievement_handlers,
};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, TYPE_GOOGLEAPIS_PREFIX,
};
use std::sync::Arc;

fn call_ach(
    router: &PublicGatewayRouter,
    name: &str,
    ctx: &AuthoritativeContext,
    any: &GoogleAny,
    msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let handlers = router.handlers.read().unwrap();
    let h = handlers.get(name).expect(name);
    h(ctx, any, msg)
}

#[test]
fn notice_publishers_and_admin_handler_error_and_alias_branches() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let mut mgr = AchievementManager::new_with_sde(Some(Arc::clone(&ctx.db)), None);
    mgr.definitions = vec![AchievementDef {
        id: 10,
        is_client_achievement: false,
        category_id: 2,
        name: "ach10".into(),
        description: String::new(),
        notification_text: String::new(),
        target: 20,
        score: 40,
        milestone_targets: vec![10, 20],
        conditions: vec![AchievementConditionDef {
            condition_id: 1,
            target_value: 20,
            statistic_event_id: 101,
            event_name: "evt".into(),
        }],
        reward_isk: 0,
        reward_sp: 0,
    }];
    let mgr = Arc::new(mgr);
    let router = &ctx.quasar_gateway;
    register_achievement_handlers(router, Arc::clone(&ctx.db), Arc::clone(&mgr));

    let cid = 90_000_001_u32;
    router.pending_notices.write().unwrap().clear();

    publish_achievement_milestone_reached_notice(router, cid, 10, 1);
    publish_achievement_version_check_notice(router, 24, 1, 5);
    publish_achievement_category_progress_notice(router, cid, 2, 40);
    publish_achievement_category_milestone_notice(router, cid, 2, 1);

    let notices = router.pending_notices.read().unwrap().clone();
    assert_eq!(notices.len(), 4);
    assert_eq!(notices[0].0, NoticeScope::Character(cid));
    assert!(notices[0].1.type_url.ends_with("milestone.api.ReachedNotice"));
    assert_eq!(notices[1].0, NoticeScope::Broadcast);
    assert!(notices[1].1.type_url.ends_with("CheckVersionNotice"));
    assert_eq!(notices[2].0, NoticeScope::Character(cid));
    assert!(notices[2].1.type_url.ends_with("category.api.ProgressedNotice"));
    assert_eq!(notices[3].0, NoticeScope::Character(cid));
    assert!(notices[3].1.type_url.ends_with("category.milestone.api.ReachedNotice"));

    let dummy_any = GoogleAny::default();
    let empty_msg = DynamicQuasarMessage::new("Empty");
    let player_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(98_000_001),
        0,
        "tranquility",
        vec![1],
    );
    let no_char_admin = AuthoritativeContext::for_session(
        Some(1),
        None,
        None,
        0,
        "tranquility",
        vec![1],
    )
    .with_service_roles(u64::MAX);
    let admin_ctx = player_ctx.clone().with_service_roles(u64::MAX);

    // Non-admin rejected on all admin endpoints
    for req in [
        "eve_public.achievement.api.admin.AddProgressRequest",
        "eve_public.achievement.api.admin.ProgressAchievementRequest",
        "eve_public.achievement.api.admin.CompleteRequest",
        "eve_public.achievement.api.admin.CompleteAchievementRequest",
        "eve_public.achievement.api.admin.UnlockAchievementRequest",
        "eve_public.achievement.api.admin.ResetRequest",
        "eve_public.achievement.api.admin.ResetAllRequest",
        "eve_public.achievement.category.api.admin.ResetRequest",
        "eve.achievement.definition.version.api.admin.SetDefaultVersionRequest",
    ] {
        assert_eq!(
            call_ach(router, req, &player_ctx, &dummy_any, &empty_msg)
                .unwrap_err()
                .0,
            STATUS_FORBIDDEN
        );
    }

    // Admin without character rejected on character-scoped admin endpoints, allowed on ResetAllRequest
    for req in [
        "eve_public.achievement.api.admin.AddProgressRequest",
        "eve_public.achievement.api.admin.CompleteRequest",
        "eve_public.achievement.api.admin.ResetRequest",
        "eve_public.achievement.category.api.admin.ResetRequest",
    ] {
        assert_eq!(
            call_ach(router, req, &no_char_admin, &dummy_any, &empty_msg)
                .unwrap_err()
                .0,
            STATUS_FORBIDDEN
        );
    }
    assert!(call_ach(
        router,
        "eve_public.achievement.api.admin.ResetAllRequest",
        &no_char_admin,
        &dummy_any,
        &empty_msg
    )
    .is_ok());

    // ProgressAchievementRequest with delta on field 4, missing delta, and unknown achievement ID
    let mut prog_no_delta = DynamicQuasarMessage::new("Prog");
    prog_no_delta.fields.insert(1, DynamicValue::Uint32(10));
    assert_eq!(
        call_ach(
            router,
            "eve_public.achievement.api.admin.ProgressAchievementRequest",
            &admin_ctx,
            &dummy_any,
            &prog_no_delta
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    let mut prog_f4 = DynamicQuasarMessage::new("Prog");
    prog_f4.fields.insert(1, DynamicValue::Uint32(10));
    prog_f4.fields.insert(4, DynamicValue::Uint64(10));
    assert!(call_ach(
        router,
        "eve_public.achievement.api.admin.ProgressAchievementRequest",
        &admin_ctx,
        &dummy_any,
        &prog_f4
    )
    .is_ok());

    let mut prog_unknown = DynamicQuasarMessage::new("Prog");
    prog_unknown.fields.insert(1, DynamicValue::Uint32(999));
    prog_unknown.fields.insert(2, DynamicValue::Uint64(5));
    assert_eq!(
        call_ach(
            router,
            "eve_public.achievement.api.admin.AddProgressRequest",
            &admin_ctx,
            &dummy_any,
            &prog_unknown
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    // CompleteAchievementRequest & UnlockAchievementRequest aliases + validation
    assert_eq!(
        call_ach(
            router,
            "eve_public.achievement.api.admin.CompleteAchievementRequest",
            &admin_ctx,
            &dummy_any,
            &empty_msg
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let mut comp_10 = DynamicQuasarMessage::new("Comp");
    comp_10.fields.insert(1, DynamicValue::Uint32(10));
    assert!(call_ach(
        router,
        "eve_public.achievement.api.admin.UnlockAchievementRequest",
        &admin_ctx,
        &dummy_any,
        &comp_10
    )
    .is_ok());
}

#[test]
fn query_claim_showcase_unclaimed_and_cdn_latest_branches() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let mut mgr = AchievementManager::new_with_sde(Some(Arc::clone(&ctx.db)), None);
    mgr.definitions = vec![AchievementDef {
        id: 20,
        is_client_achievement: false,
        category_id: 3,
        name: "ach20".into(),
        description: String::new(),
        notification_text: String::new(),
        target: 10,
        score: 30,
        milestone_targets: vec![5, 10],
        conditions: Vec::new(),
        reward_isk: 0,
        reward_sp: 0,
    }];
    let mgr = Arc::new(mgr);
    let router = &ctx.quasar_gateway;
    register_achievement_handlers(router, Arc::clone(&ctx.db), Arc::clone(&mgr));

    let cid = 90_000_001_u32;
    let dummy_any = GoogleAny::default();
    let empty_msg = DynamicQuasarMessage::new("Empty");
    let no_char_ctx = AuthoritativeContext::for_session(None, None, None, 0, "tranquility", vec![1]);
    let player_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(98_000_001),
        0,
        "tranquility",
        vec![1],
    );

    // 1. Unauthenticated character rejected on public achievement endpoints
    for req in [
        "eve_public.achievement.api.GetRequest",
        "eve_public.achievement.category.api.GetRequest",
        "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
        "eve_public.achievement.reward.api.GetUnclaimedRequest",
        "eve_public.achievement.reward.api.ClaimRequest",
        "eve.achievement.reward.api.GetUnclaimedRequest",
        "eve.achievement.showcase.api.GetCharacterScoreRequest",
    ] {
        assert_eq!(
            call_ach(router, req, &no_char_ctx, &dummy_any, &empty_msg)
                .unwrap_err()
                .0,
            STATUS_FORBIDDEN
        );
    }

    // eve.achievement.api.GetRequest and eve.achievement.category.api.GetRequest allow no_char_ctx (score 0)
    assert!(call_ach(
        router,
        "eve.achievement.api.GetRequest",
        &no_char_ctx,
        &dummy_any,
        &empty_msg
    )
    .is_ok());
    let mut cat_msg = DynamicQuasarMessage::new("Cat");
    cat_msg.fields.insert(1, DynamicValue::Uint32(3));
    assert!(call_ach(
        router,
        "eve.achievement.category.api.GetRequest",
        &no_char_ctx,
        &dummy_any,
        &cat_msg
    )
    .is_ok());

    // 2. eve_public.achievement.api.GetRequest: missing ID, zero ID, unknown ID, incomplete vs completed
    assert_eq!(
        call_ach(
            router,
            "eve_public.achievement.api.GetRequest",
            &player_ctx,
            &dummy_any,
            &empty_msg
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let mut zero_id = DynamicQuasarMessage::new("Zero");
    zero_id.fields.insert(1, DynamicValue::Uint32(0));
    assert_eq!(
        call_ach(
            router,
            "eve_public.achievement.api.GetRequest",
            &player_ctx,
            &dummy_any,
            &zero_id
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let mut unk_id = DynamicQuasarMessage::new("Unk");
    unk_id.fields.insert(1, DynamicValue::Uint32(999));
    assert_eq!(
        call_ach(
            router,
            "eve_public.achievement.api.GetRequest",
            &player_ctx,
            &dummy_any,
            &unk_id
        )
        .unwrap_err()
        .0,
        STATUS_NOT_FOUND
    );

    // Mark achievement 20 completed -> GetRequest encodes completed_date (field 1) and reached_milestones (field 5),
    // and GetUnclaimedRequest encodes unclaimed rewards
    let _ = mgr.mark_completed(cid, 20, 1_700_000_123).unwrap();
    let mut get_20 = DynamicQuasarMessage::new("Get20");
    get_20.fields.insert(1, DynamicValue::Uint32(20));
    let completed_resp = call_ach(
        router,
        "eve_public.achievement.api.GetRequest",
        &player_ctx,
        &dummy_any,
        &get_20,
    )
    .unwrap();
    assert!(!completed_resp.value.is_empty());

    let uncl_pub = call_ach(
        router,
        "eve_public.achievement.reward.api.GetUnclaimedRequest",
        &player_ctx,
        &dummy_any,
        &empty_msg,
    )
    .unwrap();
    assert!(!uncl_pub.value.is_empty());
    let uncl_eve = call_ach(
        router,
        "eve.achievement.reward.api.GetUnclaimedRequest",
        &player_ctx,
        &dummy_any,
        &empty_msg,
    )
    .unwrap();
    assert!(!uncl_eve.value.is_empty());

    // 3. Showcase GetCharacterScoreRequest: empty nested message vs explicit target_cid vs invalid 0 target_cid
    let mut empty_sub = DynamicQuasarMessage::new("Outer");
    empty_sub
        .fields
        .insert(1, DynamicValue::Message(Box::new(DynamicQuasarMessage::new("Inner"))));
    assert!(call_ach(
        router,
        "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
        &player_ctx,
        &dummy_any,
        &empty_sub
    )
    .is_ok());

    let mut target_sub = DynamicQuasarMessage::new("Outer");
    target_sub.fields.insert(1, DynamicValue::Uint32(cid));
    assert!(call_ach(
        router,
        "eve.achievement.showcase.api.GetCharacterScoreRequest",
        &player_ctx,
        &dummy_any,
        &target_sub
    )
    .is_ok());
    assert_eq!(
        call_ach(
            router,
            "eve_public.achievement.showcase.api.GetCharacterScoreRequest",
            &player_ctx,
            &dummy_any,
            &zero_id
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // 4. ClaimRequest filter parsing (field 1 achievement source vs field 2 category source)
    let mut bad_claim_ach = DynamicQuasarMessage::new("Claim");
    let mut src_bad = DynamicQuasarMessage::new("Src");
    src_bad.fields.insert(1, DynamicValue::Uint32(0));
    bad_claim_ach
        .fields
        .insert(1, DynamicValue::Message(Box::new(src_bad)));
    assert_eq!(
        call_ach(
            router,
            "eve_public.achievement.reward.api.ClaimRequest",
            &player_ctx,
            &dummy_any,
            &bad_claim_ach
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    let mut claim_cat = DynamicQuasarMessage::new("Claim");
    let mut src_cat = DynamicQuasarMessage::new("SrcCat");
    src_cat.fields.insert(1, DynamicValue::Uint32(3));
    src_cat.fields.insert(2, DynamicValue::Uint32(1));
    claim_cat
        .fields
        .insert(2, DynamicValue::Message(Box::new(src_cat)));
    assert_eq!(
        call_ach(
            router,
            "eve_public.achievement.reward.api.ClaimRequest",
            &player_ctx,
            &dummy_any,
            &claim_cat
        )
        .unwrap_err()
        .0,
        STATUS_PRECONDITION_FAILED
    );

    // 5. CDN GetLatestRequest branches:
    // a) client_ver present & no_local == false -> STATUS_SERVICE_UNAVAILABLE
    let mut ver_msg = DynamicQuasarMessage::new("Ver");
    ver_msg.fields.insert(1, DynamicValue::Uint64(24));
    ver_msg.fields.insert(2, DynamicValue::Uint64(1));
    ver_msg.fields.insert(3, DynamicValue::Uint64(60));
    ver_msg.fields.insert(4, DynamicValue::String("rc1".into()));
    ver_msg.fields.insert(
        5,
        DynamicValue::Repeated(vec![DynamicValue::String("build1".into())]),
    );
    let mut cdn_req = DynamicQuasarMessage::new("CdnReq");
    cdn_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(ver_msg)));
    let google_any = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.achievement.definition.api.cdn.GetLatestRequest"
        ),
        value: Vec::new(),
    };
    assert_eq!(
        call_ach(
            router,
            "eve_public.achievement.definition.api.cdn.GetLatestRequest",
            &player_ctx,
            &google_any,
            &cdn_req
        )
        .unwrap_err()
        .0,
        STATUS_SERVICE_UNAVAILABLE
    );

    // b) client_ver present & no_local == true -> encodes client_ver spec + checkpoint
    cdn_req.fields.insert(2, DynamicValue::Bool(true));
    let cdn_ok = call_ach(
        router,
        "eve_public.achievement.definition.api.cdn.GetLatestRequest",
        &player_ctx,
        &google_any,
        &cdn_req,
    )
    .unwrap();
    assert!(!cdn_ok.value.is_empty());

    // c) client_ver absent & googleapis type_url -> encodes default (24, 1, count) spec
    let cdn_default = call_ach(
        router,
        "eve_public.achievement.definition.api.cdn.GetLatestRequest",
        &player_ctx,
        &google_any,
        &empty_msg,
    )
    .unwrap();
    assert!(!cdn_default.value.is_empty());
}
