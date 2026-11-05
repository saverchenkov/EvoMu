use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use crate::services::quasar_achievements::{AchievementConditionDef, AchievementDef};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage};

#[test]
fn achievement_notices_and_gateway_handlers() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let mut mgr = AchievementManager::new_with_sde(Some(Arc::clone(&ctx.db)), None);
    mgr.definitions = vec![AchievementDef {
        id: 1,
        is_client_achievement: false,
        category_id: 1,
        name: "ach1".into(),
        description: String::new(),
        notification_text: String::new(),
        target: 10,
        score: 25,
        milestone_targets: vec![5, 10],
        conditions: vec![AchievementConditionDef {
            condition_id: 1,
            target_value: 10,
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
    publish_achievement_milestone_reached_notice(router, cid, 1, 1);
    publish_achievement_version_check_notice(router, 24, 1, 0);
    publish_achievement_category_progress_notice(router, cid, 1, 25);
    publish_achievement_category_milestone_notice(router, cid, 1, 1);

    let admin_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![1, 2],
    )
    .with_service_roles(u64::MAX);
    let dummy_any = GoogleAny::default();
    let handlers = router.handlers.read().unwrap();

    let mut prog_msg = DynamicQuasarMessage::new("AddProgress");
    prog_msg.fields.insert(1, DynamicValue::Uint32(1));
    prog_msg.fields.insert(2, DynamicValue::Uint64(5));
    let _ = handlers["eve_public.achievement.api.admin.AddProgressRequest"](&admin_ctx, &dummy_any, &prog_msg).unwrap();

    let mut id_msg = DynamicQuasarMessage::new("Req");
    id_msg.fields.insert(1, DynamicValue::Uint32(1));
    let get_resp = handlers["eve_public.achievement.api.GetRequest"](&admin_ctx, &dummy_any, &id_msg).unwrap();
    assert!(get_resp.type_url.ends_with("eve_public.achievement.api.GetResponse"));

    let cat_resp = handlers["eve_public.achievement.category.api.GetRequest"](&admin_ctx, &dummy_any, &id_msg).unwrap();
    assert!(cat_resp.type_url.ends_with("eve_public.achievement.category.api.GetResponse"));

    let score_resp = handlers["eve_public.achievement.showcase.api.GetCharacterScoreRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(score_resp.type_url.ends_with("GetCharacterScoreResponse"));

    let uncl_resp = handlers["eve_public.achievement.reward.api.GetUnclaimedRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(uncl_resp.type_url.ends_with("GetUnclaimedResponse"));

    assert_eq!(
        handlers["eve_public.achievement.reward.api.ClaimRequest"](
            &admin_ctx,
            &dummy_any,
            &DynamicQuasarMessage::new("Empty"),
        )
        .unwrap_err()
        .0,
        STATUS_PRECONDITION_FAILED
    );

    let _ = handlers["eve_public.achievement.api.admin.CompleteRequest"](&admin_ctx, &dummy_any, &id_msg).unwrap();
    let _ = handlers["eve_public.achievement.api.admin.ResetRequest"](&admin_ctx, &dummy_any, &id_msg).unwrap();
    let _ = handlers["eve_public.achievement.category.api.admin.ResetRequest"](&admin_ctx, &dummy_any, &id_msg).unwrap();
    let _ = handlers["eve_public.achievement.api.admin.ResetAllRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    let _ = handlers["eve.achievement.definition.version.api.admin.SetDefaultVersionRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    let _ = handlers["eve.achievement.api.GetRequest"](&admin_ctx, &dummy_any, &DynamicQuasarMessage::new("Empty")).unwrap();
    let _ = handlers["eve.achievement.category.api.GetRequest"](&admin_ctx, &dummy_any, &id_msg).unwrap();
    let _ = handlers["eve.achievement.definition.api.GetLatestRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    let _ = handlers["eve.achievement.definition.version.api.ListRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    let _ = handlers["eve.achievement.definition.version.api.GetDefaultVersionRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    let _ = handlers["eve.achievement.reward.api.GetUnclaimedRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    let _ = handlers["eve.achievement.showcase.api.GetCharacterScoreRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
}
