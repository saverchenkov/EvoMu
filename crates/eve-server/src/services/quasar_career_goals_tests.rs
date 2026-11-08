use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage};

#[test]
fn career_goal_def_effective_attributes_and_milestones() {
    let mut def = CareerGoalDef {
        uuid: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 7],
        career_id: 1,
        activity_name: "Scanning".into(),
        activity_kind: "exploration".into(),
        name: "Scan 5 sites".into(),
        description: "Desc".into(),
        target: 5,
        career_points: 10,
        isk_reward: 50_000,
        sp_reward: 0.0,
        name_message_id: 0,
        description_message_id: 0,
        threat: 0,
        ..Default::default()
    };
    assert_eq!(def.effective_name_message_id(), 582_516 + 7);
    assert_eq!(def.effective_description_message_id(), 582_517 + 1_000 + 7);
    assert_eq!(def.effective_group(), "Scanning");
    assert_eq!(def.effective_threat(), 1);

    def.name_message_id = 600_001;
    def.description_message_id = 700_002;
    def.career_points = 20;
    assert_eq!(def.effective_name_message_id(), 600_001);
    assert_eq!(def.effective_description_message_id(), 700_002);
    assert_eq!(def.effective_threat(), 2);

    def.career_points = 30;
    assert_eq!(def.effective_threat(), 3);
    def.threat = 2;
    assert_eq!(def.effective_threat(), 2);

    def.career_id = 2;
    def.activity_name = "Resource Harvesting".into();
    def.activity_kind = "mining".into();
    assert_eq!(def.effective_group(), "ResourceHarvesting");

    def.career_id = 3;
    def.activity_name = "Abyssal Deadspace".into();
    def.activity_kind = "abyssal".into();
    assert_eq!(def.effective_group(), "AbyssalDeadspace");

    def.career_id = 4;
    def.activity_name = "Electronic Warfare".into();
    def.activity_kind = "ewar".into();
    assert_eq!(def.effective_group(), "ElectronicWarfare");

    let ms_uuid = career_milestone_uuid(101);
    assert_eq!(ms_uuid[0], 0x43);
    assert_eq!(ms_uuid[1], 0x4D);
    assert_eq!(u32::from_be_bytes([ms_uuid[12], ms_uuid[13], ms_uuid[14], ms_uuid[15]]), 101);

    let ms = default_career_milestones();
    assert_eq!(ms.len(), 8);
    let total_sp: f64 = ms.iter().map(|m| m.sp_reward).sum();
    assert_eq!(total_sp, 750_000.0);
}

#[test]
fn career_goal_manager_lifecycle_and_gateway_handlers() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let mgr = Arc::new(CareerGoalManager::new(Some(Arc::clone(&ctx.db))));
    let uuid = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
    mgr.configure_definition(CareerGoalDef {
        uuid,
        career_id: 1,
        activity_name: "Hacking".into(),
        activity_kind: "custom_hacking".into(),
        name: "Hack relic cans".into(),
        description: "Open 2 cans".into(),
        target: 2,
        career_points: 750,
        isk_reward: 100_000,
        sp_reward: 500.0,
        name_message_id: 0,
        description_message_id: 0,
        threat: 1,
        ..Default::default()
    });

    let cid = 90_000_001_u32;
    let goals = mgr.get_or_init_goals(cid);
    assert!(goals.iter().any(|g| g.uuid == uuid));
    assert_eq!(mgr.career_points_for_character(cid, Some(1)), 0);

    let (g1, done1) = mgr.add_progress(cid, &uuid, 1, 1_700_000_010).unwrap();
    assert_eq!(g1.progress, 1);
    assert!(!done1);

    mgr.on_gameplay_event(cid, "custom_hacking", 1, &ctx.quasar_gateway);
    assert_eq!(mgr.career_points_for_character(cid, Some(1)), 750);
    assert_eq!(mgr.career_points_for_character(cid, None), 750);

    let claimed_goal = mgr.claim_reward(cid, &uuid, &ctx.db).unwrap();
    assert!(claimed_goal.claimed);
    assert!(mgr.claim_reward(cid, &uuid, &ctx.db).is_err());

    let ms_list = mgr.get_character_milestones(cid);
    assert!(ms_list.iter().any(|(m, reached, _)| m.milestone_id == 1 && *reached));
    let ms1 = mgr.claim_milestone_reward(cid, 1, &ctx.db).unwrap();
    assert_eq!(ms1.sp_reward, 100_000.0);
    assert!(mgr.claim_milestone_reward(cid, 1, &ctx.db).is_err());

    register_career_goal_handlers(&ctx.quasar_gateway, Arc::clone(&ctx.db), Arc::clone(&mgr));
    let handlers = ctx.quasar_gateway.handlers.read().unwrap();
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

    let all_resp = handlers["eve_public.career.goal.api.GetAllRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(all_resp.type_url.ends_with("eve_public.career.goal.api.GetAllResponse"));

    let defs_resp = handlers["eve_public.career.goal.api.GetDefinitionsRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(defs_resp.type_url.ends_with("eve_public.career.goal.api.GetDefinitionsResponse"));

    let eve_defs_resp = handlers["eve.career.goal.api.GetDefinitionsRequest"](
        &admin_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Empty"),
    )
    .unwrap();
    assert!(eve_defs_resp.type_url.ends_with("eve.career.goal.api.GetDefinitionsResponse"));

    let mut id_sub = DynamicQuasarMessage::new("Id");
    id_sub.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut goal_req = DynamicQuasarMessage::new("GoalReq");
    goal_req.fields.insert(1, DynamicValue::Message(Box::new(id_sub)));

    let prog_resp = handlers["eve.character.career.goal.GetProgressRequest"](
        &admin_ctx,
        &dummy_any,
        &goal_req,
    )
    .unwrap();
    assert!(prog_resp.type_url.ends_with("GetProgressResponse"));

    let claimed_resp = handlers["eve.character.career.goal.HasClaimedRewardRequest"](
        &admin_ctx,
        &dummy_any,
        &goal_req,
    )
    .unwrap();
    assert!(claimed_resp.type_url.ends_with("HasClaimedRewardResponse"));

    let mut ms_sub = DynamicQuasarMessage::new("Id");
    ms_sub.fields.insert(1, DynamicValue::Bytes(career_milestone_uuid(101).to_vec()));
    let mut ms_claim_req = DynamicQuasarMessage::new("Claim");
    ms_claim_req.fields.insert(1, DynamicValue::Message(Box::new(ms_sub)));
    let _ = handlers["eve_public.career.goal.api.ClaimRewardRequest"](
        &admin_ctx,
        &dummy_any,
        &ms_claim_req,
    )
    .unwrap();
}
