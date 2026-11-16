use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use crate::services::quasar_achievements::{AchievementConditionDef, AchievementDef};
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, GoogleAny};

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

#[test]
fn achievement_cdn_uses_dynamic_esi_port_and_matches_achv_flatbuffer_crc() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let ctx = Arc::new(ctx);
    ctx.set_esi_port(39_555);
    assert_eq!(ctx.quasar_gateway.esi_port(), 39_555);

    let mgr = &ctx.quasar_gateway.progression.achievements;
    let expected_fb = mgr.flatbuffer_bytes();
    assert_eq!(expected_fb.get(4..8), Some(b"ACHV".as_slice()));
    let expected_crc = crate::router::cache_notifications::crc32_ieee(&expected_fb) as u32;

    let req = pb::message("eve_public.achievement.definition.api.cdn.GetLatestRequest")
        .set("no_local_version_available", true)
        .must_encode();
    let auth = AuthoritativeContext::for_session(Some(1), Some(90_000_001), None, 0, "tranquility", Vec::new());
    let any = eve_proto::quasar::GoogleAny {
        type_url: format!(
            "{}eve_public.achievement.definition.api.cdn.GetLatestRequest",
            eve_proto::quasar::TYPE_EVETECH_PREFIX
        ),
        value: req,
    };
    let dyn_msg = ctx
        .quasar_gateway
        .descriptors
        .decode_dynamic_or_wire("eve_public.achievement.definition.api.cdn.GetLatestRequest", &any.value)
        .expect("decode GetLatestRequest");
    let handlers = ctx.quasar_gateway.handlers.read().unwrap();
    let rsp_any = handlers["eve_public.achievement.definition.api.cdn.GetLatestRequest"](&auth, &any, &dyn_msg)
        .expect("GetLatestRequest must succeed");
    // Clean desktop client with empty GetLatestRequest (no current_version, no_local = false)
    // and type.evetech.net/ prefix also receives the GetLatestResponse checkpoint (#1128).
    let clean_any = eve_proto::quasar::GoogleAny {
        type_url: any.type_url.clone(),
        value: Vec::new(),
    };
    let clean_dyn = DynamicQuasarMessage::new("eve_public.achievement.definition.api.cdn.GetLatestRequest");
    let clean_rsp = handlers["eve_public.achievement.definition.api.cdn.GetLatestRequest"](&auth, &clean_any, &clean_dyn)
        .expect("clean client GetLatestRequest must succeed");
    // Live V24.02 client sending matching current_version (and no_local = false) gets 200 OK
    // with authoritative checkpoint.version so FlatbufferContentProvider skips re-download (#1128).
    let matched_bytes = pb::message("eve_public.achievement.definition.api.cdn.GetLatestRequest")
        .set("current_version.major", 24u32)
        .set("current_version.minor", 1u32)
        .set("current_version.patch", mgr.sde_achievement_count as u32)
        .must_encode();
    let matched_any = eve_proto::quasar::GoogleAny {
        type_url: any.type_url.clone(),
        value: matched_bytes,
    };
    let matched_dyn = ctx
        .quasar_gateway
        .descriptors
        .decode_dynamic_or_wire(
            "eve_public.achievement.definition.api.cdn.GetLatestRequest",
            &matched_any.value,
        )
        .expect("decode matched GetLatestRequest");
    let matched_rsp = handlers["eve_public.achievement.definition.api.cdn.GetLatestRequest"](
        &auth,
        &matched_any,
        &matched_dyn,
    )
    .expect("matched current_version GetLatestRequest must succeed");
    // Stale client sending an older current_version receives authoritative 24.1.<count>, not echo.
    let stale_bytes = pb::message("eve_public.achievement.definition.api.cdn.GetLatestRequest")
        .set("current_version.major", 24u32)
        .set("current_version.minor", 0u32)
        .set("current_version.patch", 1u32)
        .add("current_version.prerelease_tags", "rc1")
        .must_encode();
    let stale_any = eve_proto::quasar::GoogleAny {
        type_url: any.type_url.clone(),
        value: stale_bytes,
    };
    let stale_dyn = ctx
        .quasar_gateway
        .descriptors
        .decode_dynamic_or_wire(
            "eve_public.achievement.definition.api.cdn.GetLatestRequest",
            &stale_any.value,
        )
        .expect("decode stale GetLatestRequest");
    let stale_rsp = handlers["eve_public.achievement.definition.api.cdn.GetLatestRequest"](
        &auth,
        &stale_any,
        &stale_dyn,
    )
    .expect("stale current_version GetLatestRequest must succeed");
    drop(handlers);
    assert_eq!(clean_rsp.value, rsp_any.value);
    assert_eq!(matched_rsp.value, rsp_any.value);
    assert_eq!(stale_rsp.value, rsp_any.value);
    assert_eq!(
        rsp_any.normalized_type_name(),
        "eve_public.achievement.definition.api.cdn.GetLatestResponse"
    );

    let wm = eve_proto::quasar::WireMessage::decode(&rsp_any.value).expect("decode response");
    let cp_bytes = wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_bytes()).expect("checkpoint");
    let cp_wm = eve_proto::quasar::WireMessage::decode(cp_bytes).expect("decode checkpoint");
    let ver_bytes = cp_wm
        .fields
        .iter()
        .find(|f| f.field_number == 2)
        .and_then(|f| f.value.as_bytes())
        .expect("checkpoint.version");
    let ver_wm = eve_proto::quasar::WireMessage::decode(ver_bytes).expect("decode version");
    assert_eq!(
        ver_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()),
        Some(24)
    );
    assert_eq!(
        ver_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()),
        Some(1)
    );
    assert_eq!(
        ver_wm.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()),
        Some(mgr.sde_achievement_count as u64)
    );
    assert!(ver_wm.fields.iter().all(|f| f.field_number <= 3));
    let url = cp_wm
        .fields
        .iter()
        .find(|f| f.field_number == 1)
        .and_then(|f| f.value.as_str().ok().flatten())
        .expect("checkpoint.url");
    let path = url
        .strip_prefix("http://127.0.0.1:39555")
        .expect("checkpoint URL must reflect ctx.set_esi_port(39_555)");
    assert_eq!(
        path,
        format!("/cdn/achievements/v24.1.{}/{}.fb", mgr.sde_achievement_count, mgr.sde_achievement_hash)
    );
    let crc_bytes = cp_wm.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_bytes()).expect("crc");
    let crc_wm = eve_proto::quasar::WireMessage::decode(crc_bytes).expect("decode crc");
    let crc_val = crc_wm.fields.iter().find(|f| f.field_number == 1).and_then(|f| f.value.as_varint()).expect("crc u32");
    assert_eq!(crc_val as u32, expected_crc);

    let esi = crate::esi::router::EsiRouter::new(Arc::clone(&ctx));
    let http = esi.handle_request(&crate::esi::EsiHttpRequest::new("GET", path));
    assert_eq!(http.status_code, 200);
    assert_eq!(http.header("content-type"), Some("application/octet-stream"));
    assert_eq!(http.body, expected_fb);

    let legacy_json_path = path.replace(".fb", ".json");
    assert_eq!(
        esi.handle_request(&crate::esi::EsiHttpRequest::new("GET", legacy_json_path))
            .status_code,
        404
    );
}

#[test]
fn admin_endpoints_return_descriptor_paired_responses_and_pass_quasar_tap() {
    let reg = eve_proto::quasar::QuasarDescriptorRegistry::open_default()
        .expect("open default quasar descriptors");
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

    let mut id_msg = DynamicQuasarMessage::new("Req");
    id_msg.fields.insert(1, DynamicValue::Uint32(1));
    let empty_msg = DynamicQuasarMessage::new("Empty");

    for (req_name, expected_rsp, req_msg) in [
        (
            "eve_public.achievement.api.admin.AddProgressRequest",
            "eve_public.achievement.api.admin.AddProgressResponse",
            &prog_msg,
        ),
        (
            "eve_public.achievement.api.admin.CompleteRequest",
            "eve_public.achievement.api.admin.CompleteResponse",
            &id_msg,
        ),
        (
            "eve_public.achievement.api.admin.ResetRequest",
            "eve_public.achievement.api.admin.ResetResponse",
            &id_msg,
        ),
        (
            "eve_public.achievement.category.api.admin.ResetRequest",
            "eve_public.achievement.category.api.admin.ResetResponse",
            &id_msg,
        ),
        (
            "eve_public.achievement.api.admin.ResetAllRequest",
            "eve_public.achievement.api.admin.ResetAllResponse",
            &empty_msg,
        ),
        (
            "eve.achievement.definition.version.api.admin.SetDefaultVersionRequest",
            "eve.achievement.definition.version.api.admin.SetDefaultVersionResponse",
            &empty_msg,
        ),
    ] {
        let _ = crate::services::quasar_social_chat::drain_local_notices();
        let rsp = handlers[req_name](&admin_ctx, &dummy_any, req_msg)
            .unwrap_or_else(|e| panic!("{req_name} failed: {e:?}"));
        assert_eq!(rsp.normalized_type_name(), expected_rsp);
        assert!(rsp.value.is_empty(), "{expected_rsp} should carry empty wire payload");
        let notices: Vec<GoogleAny> = crate::services::quasar_social_chat::drain_local_notices()
            .into_iter()
            .map(|(_, any, _)| any)
            .collect();
        let violations =
            eve_testkit::quasar_tap::tap_exchange(&reg, req_name, 200, Some(&rsp), &notices);
        assert!(
            violations.is_empty(),
            "unexpected quasar tap violations for {req_name}: {violations:#?}"
        );
    }
}

#[cfg(test)]
#[path = "quasar_achievements_ops_extra_unit_tests.rs"]
mod quasar_achievements_ops_extra_unit_tests;
