use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;

fn make_test_manager(db: Arc<WorldRepository>) -> AchievementManager {
    let mut mgr = AchievementManager::new_with_sde(Some(db), None);
    mgr.definitions = vec![
        AchievementDef {
            id: 10,
            is_client_achievement: false,
            category_id: 1,
            name: "combat alpha".into(),
            description: "desc".into(),
            notification_text: "notif".into(),
            target: 100,
            score: 50,
            milestone_targets: vec![25, 100],
            conditions: vec![AchievementConditionDef {
                condition_id: 1,
                target_value: 100,
                statistic_event_id: 501,
                event_name: "kill_npc".into(),
            }],
            reward_isk: 0,
            reward_sp: 0,
        },
        AchievementDef {
            id: 20,
            is_client_achievement: true,
            category_id: 2,
            name: "client zoom".into(),
            description: "desc".into(),
            notification_text: "notif".into(),
            target: 10,
            score: 20,
            milestone_targets: vec![5, 10],
            conditions: vec![AchievementConditionDef {
                condition_id: 2,
                target_value: 10,
                statistic_event_id: 502,
                event_name: "client_zoom".into(),
            }],
            reward_isk: 0,
            reward_sp: 0,
        },
    ];
    mgr.allowed_client_counter_keys.insert("client_zoom".into());
    mgr
}

#[test]
fn achievement_manager_progress_milestones_claims_and_helpers() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let mgr = make_test_manager(Arc::clone(&ctx.db));
    let cid = 100_000_001_u32;

    let uuid = achievement_reward_uuid(10, 1);
    assert_eq!(&uuid[0..4], b"ACHR");
    assert_eq!(u32::from_be_bytes(uuid[8..12].try_into().unwrap()), 10);
    assert_eq!(u32::from_be_bytes(uuid[12..16].try_into().unwrap()), 1);

    assert!(!mgr.is_client_achievement_id(10));
    assert!(mgr.is_client_achievement_id(20));
    assert!(mgr.is_client_allowed_counter_key("client_zoom"));
    assert!(!mgr.is_client_allowed_counter_key(""));

    let init = mgr.get_or_init_achievements(cid);
    assert_eq!(init.len(), 2);

    let (ach, just_completed, ms) = mgr.add_progress(cid, 10, 30, 1000).unwrap();
    assert_eq!(ach.progress, 30);
    assert!(!just_completed);
    assert_eq!(ms, vec![1]);

    let updates = mgr.record_statistic_event(cid, 501, 100, 1001);
    assert_eq!(updates.len(), 1);
    assert!(updates[0].1);
    assert_eq!(mgr.category_score(cid, 1), 50);
    assert_eq!(mgr.total_character_score(cid), 50);

    mgr.on_gameplay_event_by_name(cid, &["kill_npc"], 10, 1002, &ctx.quasar_gateway);
    mgr.apply_client_task_completions_and_counters(
        cid,
        &[20],
        &[("client_zoom".to_string(), 5)],
        1003,
        &ctx.quasar_gateway,
    );
    assert_eq!(mgr.total_character_score(cid), 70);

    let claimed = mgr.claim_rewards(cid, &ctx.db);
    assert!(claimed >= 2);

    mgr.set_category_score(cid, 1, 99);
    assert_eq!(mgr.category_score(cid, 1), 99);

    mgr.reset_achievement(cid, 10).unwrap();
    mgr.reset_category(cid, 1);
    assert_eq!(mgr.category_score(cid, 1), 0);

    mgr.unlock_achievement(cid, 10);
    assert!(mgr.get_or_init_achievements(cid).iter().any(|a| a.def.id == 10 && a.completed));

    mgr.reset_all(cid);
    assert_eq!(mgr.total_character_score(cid), 0);
    mgr.clear_character(cid);

    assert_eq!(extract_u32_ident(&DynamicValue::Uint32(42)), Some(42));
    assert_eq!(extract_u64_val(&DynamicValue::Int64(99)), Some(99));
    assert_eq!(empty_achievement_flatbuffer().len(), 140);
    assert_eq!(empty_root_table_flatbuffer().len(), 16);
}
