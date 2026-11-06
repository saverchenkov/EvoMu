//! Deep branch-coverage and mutation-killing unit tests for `quasar_achievements` (`Q-4`).

#![forbid(unsafe_code)]

use crate::quasar_gateway::{NoticeScope, STATUS_NOT_FOUND};
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use crate::services::quasar_achievements::*;
use eve_db::WorldRepository;
use eve_proto::quasar::{DynamicQuasarMessage, DynamicValue};
use std::collections::HashMap;
use std::sync::Arc;

fn make_multi_condition_manager(db: Arc<WorldRepository>) -> AchievementManager {
    let mut mgr = AchievementManager::new_with_sde(Some(db), None);
    mgr.definitions = vec![
        AchievementDef {
            id: 101,
            is_client_achievement: false,
            category_id: 1,
            name: "Multi Condition Server".into(),
            description: "Requires two events".into(),
            notification_text: "Done".into(),
            target: 100,
            score: 40,
            milestone_targets: vec![30, 70, 100],
            conditions: vec![
                AchievementConditionDef {
                    condition_id: 11,
                    target_value: 40,
                    statistic_event_id: 701,
                    event_name: "ev_alpha".into(),
                },
                AchievementConditionDef {
                    condition_id: 12,
                    target_value: 60,
                    statistic_event_id: 702,
                    event_name: "ev_beta".into(),
                },
            ],
            reward_isk: 10_000,
            reward_sp: 500,
        },
        AchievementDef {
            id: 202,
            is_client_achievement: true,
            category_id: 2,
            name: "Client Camera Achievement".into(),
            description: "Zoom in and out".into(),
            notification_text: "Camera Master".into(),
            target: 20,
            score: 15,
            milestone_targets: vec![10, 20],
            conditions: vec![
                AchievementConditionDef {
                    condition_id: 21,
                    target_value: 10,
                    statistic_event_id: 801,
                    event_name: "client_zoom_in".into(),
                },
                AchievementConditionDef {
                    condition_id: 22,
                    target_value: 10,
                    statistic_event_id: 802,
                    event_name: "client_zoom_out".into(),
                },
            ],
            reward_isk: 0,
            reward_sp: 0,
        },
        AchievementDef {
            id: 303,
            is_client_achievement: false,
            category_id: 0, // category 0 -> no category notices
            name: "Uncategorized".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 5,
            score: 10,
            milestone_targets: vec![5],
            conditions: vec![AchievementConditionDef {
                condition_id: 31,
                target_value: 5,
                statistic_event_id: 901,
                event_name: "ev_uncat".into(),
            }],
            reward_isk: 0,
            reward_sp: 0,
        },
    ];
    mgr.allowed_client_counter_keys
        .insert("client_zoom_in".into());
    mgr.allowed_client_counter_keys.insert("802".into());
    mgr
}

#[test]
fn serde_deserialize_i64_or_f64_and_reward_uuid_layout() {
    let json_float = r#"{
        "id": 55,
        "category_id": 3,
        "name": "FloatRewards",
        "target": 10,
        "score": 5,
        "milestone_targets": [5, 10],
        "reward_isk": 12345.6,
        "reward_sp": -42
    }"#;
    let def: AchievementDef = serde_json::from_str(json_float).expect("deserialize float/neg i64");
    assert_eq!(def.id, 55);
    assert_eq!(def.reward_isk, 12346);
    assert_eq!(def.reward_sp, -42);
    assert!(!def.is_client_achievement);
    assert!(def.conditions.is_empty());

    let u1 = achievement_reward_uuid(0x0102_0304, 0x0506_0708);
    assert_eq!(&u1[0..4], b"ACHR");
    assert_eq!(&u1[4..8], &[0, 0, 0, 0]);
    assert_eq!(&u1[8..12], &[1, 2, 3, 4]);
    assert_eq!(&u1[12..16], &[5, 6, 7, 8]);
}

#[test]
fn multi_condition_clamping_client_vs_server_filtering_and_db_hydration() {
    let (ctx, _clock) = test_ctx_and_clock(1_750_000_000);
    let mgr = make_multi_condition_manager(Arc::clone(&ctx.db));
    let cid = 95_000_001_u32;

    // Unknown achievement add_progress returns None
    assert!(mgr.add_progress(cid, 999_999, 10, 1000).is_none());

    // add_progress distributes progress across conditions in order and reaches milestones once
    let (a1, comp1, ms1) = mgr.add_progress(cid, 101, 50, 1000).unwrap();
    assert_eq!(a1.progress, 50);
    assert_eq!(a1.condition_progress.get(&11), Some(&40));
    assert_eq!(a1.condition_progress.get(&12), Some(&10));
    assert!(!comp1);
    assert_eq!(ms1, vec![1]);
    assert_eq!(a1.unclaimed_reward_count, 1);

    // Second add_progress reaching 100 completes achievement and triggers only milestones 2 and 3
    let (a2, comp2, ms2) = mgr.add_progress(cid, 101, 60, 1005).unwrap();
    assert_eq!(a2.progress, 110);
    assert_eq!(a2.condition_progress.get(&11), Some(&40));
    assert_eq!(a2.condition_progress.get(&12), Some(&60));
    assert!(comp2);
    assert_eq!(a2.completed_date, Some(1005));
    assert_eq!(ms2, vec![2, 3]);
    assert_eq!(a2.unclaimed_reward_count, 3);

    // Third add_progress on already-completed achievement has just_completed = false and no new milestones
    let (_, comp3, ms3) = mgr.add_progress(cid, 101, 10, 1010).unwrap();
    assert!(!comp3);
    assert!(ms3.is_empty());

    // Reset achievement 101 and test record_statistic_event clamping per condition
    mgr.reset_achievement(cid, 101).unwrap();
    assert_eq!(
        mgr.reset_achievement(cid, 999_999).unwrap_err().0,
        STATUS_NOT_FOUND
    );
    assert_eq!(
        mgr.mark_completed(cid, 999_999, 1000).unwrap_err().0,
        STATUS_NOT_FOUND
    );

    // Event 701 has target_value 40; sending delta 500 clamps condition 11 at 40 -> progress 40 (milestone 1 only)
    let upd1 = mgr.record_statistic_event(cid, 701, 500, 2000);
    assert_eq!(upd1.len(), 1);
    assert_eq!(upd1[0].0.progress, 40);
    assert!(!upd1[0].1);
    assert_eq!(upd1[0].2, vec![1]);

    // Event 702 has target_value 60; sending delta 60 brings total to 100 -> completes!
    let upd2 = mgr.record_statistic_event(cid, 702, 60, 2001);
    assert_eq!(upd2.len(), 1);
    assert_eq!(upd2[0].0.progress, 100);
    assert!(upd2[0].1);
    assert_eq!(upd2[0].2, vec![2, 3]);

    // Client counter events: delta 0 is no-op, server event name is ignored by on_client_counter_event
    mgr.reset_achievement(cid, 101).unwrap();
    mgr.on_client_counter_event(cid, "ev_alpha", 100, 2002, &ctx.quasar_gateway);
    let st = mgr.get_or_init_achievements(cid);
    assert_eq!(st.iter().find(|a| a.def.id == 101).unwrap().progress, 0);

    // Client counter by event name ("client_zoom_in") and numeric event ID ("802")
    mgr.on_client_counter_event(cid, "client_zoom_in", 0, 2003, &ctx.quasar_gateway);
    mgr.on_client_counter_event(cid, "client_zoom_in", 10, 2003, &ctx.quasar_gateway);
    mgr.on_client_counter_event(cid, "802", 10, 2004, &ctx.quasar_gateway);
    let st2 = mgr.get_or_init_achievements(cid);
    let client_ach = st2.iter().find(|a| a.def.id == 202).unwrap();
    assert!(client_ach.completed);
    assert_eq!(client_ach.progress, 20);

    // Server gameplay event by name: delta 0 and empty slice are no-ops
    mgr.on_gameplay_event_by_name(cid, &["ev_uncat"], 0, 2005, &ctx.quasar_gateway);
    mgr.on_gameplay_event_by_name(cid, &[], 10, 2005, &ctx.quasar_gateway);
    assert!(!mgr
        .get_or_init_achievements(cid)
        .iter()
        .find(|a| a.def.id == 303)
        .unwrap()
        .completed);

    mgr.on_gameplay_event_by_name(cid, &["ev_uncat"], 5, 2006, &ctx.quasar_gateway);
    assert!(mgr
        .get_or_init_achievements(cid)
        .iter()
        .find(|a| a.def.id == 303)
        .unwrap()
        .completed);

    // Persist and reload manager from DB snapshot
    mgr.set_category_score(cid, 1, 120);
    mgr.save_to_db();
    let reloaded = AchievementManager::new_with_sde(Some(Arc::clone(&ctx.db)), Some(Arc::clone(&ctx.sde)));
    assert_eq!(reloaded.category_score(cid, 1), 120);
    // Category 1 uses override 120, Category 2 uses completed ach 202 (score 15), Category 0 uses ach 303 (score 10)
    assert_eq!(mgr.total_character_score(cid), 120 + 15 + 10);
}

#[test]
fn filtered_reward_claiming_and_notice_encoding_and_dynamic_extractors() {
    let (ctx, _clock) = test_ctx_and_clock(1_760_000_000);
    let mgr = make_multi_condition_manager(Arc::clone(&ctx.db));
    let cid = 95_000_002_u32;

    // Claiming for uninitialized character returns 0
    assert_eq!(mgr.claim_rewards(cid, &ctx.db), 0);

    mgr.mark_completed(cid, 101, 3000).unwrap(); // 3 milestones (1, 2, 3) in category 1
    mgr.mark_completed(cid, 202, 3000).unwrap(); // 2 milestones (1, 2) in category 2

    // Filter by non-matching category -> 0 claimed
    assert_eq!(
        mgr.claim_rewards_filtered(
            cid,
            &ctx.db,
            &RewardClaimFilter {
                category_id: Some(99),
                ..Default::default()
            }
        ),
        0
    );

    // Filter by specific milestone_id = 3 on achievement 101 -> claims 1
    assert_eq!(
        mgr.claim_rewards_filtered(
            cid,
            &ctx.db,
            &RewardClaimFilter {
                achievement_id: Some(101),
                milestone_id: Some(3),
                ..Default::default()
            }
        ),
        1
    );

    // Filter by selected_reward_uuids for achievement 202 milestone 2 -> claims 1
    let target_uuid = achievement_reward_uuid(202, 2);
    assert_eq!(
        mgr.claim_rewards_filtered(
            cid,
            &ctx.db,
            &RewardClaimFilter {
                selected_reward_uuids: Some(vec![target_uuid]),
                ..Default::default()
            }
        ),
        1
    );

    // Claim remaining rewards across all achievements (2 left on 101 + 1 left on 202 = 3)
    assert_eq!(mgr.claim_rewards(cid, &ctx.db), 3);
    assert_eq!(mgr.claim_rewards(cid, &ctx.db), 0);

    // Test emit_achievement_notices for cat_id == 0 vs cat_id > 0
    let _ = crate::services::quasar_social_chat::drain_local_notices();
    emit_achievement_notices(cid, 303, 0, 5, 0, false, &[1], 4000);
    let notices_cat0 = crate::services::quasar_social_chat::drain_local_notices();
    // Should emit ProgressedNotice and milestone.api.ReachedNotice (2 notices, no category notices, no CompletedNotice)
    assert_eq!(notices_cat0.len(), 2);
    assert!(notices_cat0.iter().all(|(s, _, _)| *s == NoticeScope::Character(cid)));

    emit_achievement_notices(cid, 101, 1, 100, 40, true, &[1, 2], 4001);
    let notices_cat1 = crate::services::quasar_social_chat::drain_local_notices();
    // ProgressedNotice + CompletedNotice + 2*(milestone ReachedNotice + category.milestone ReachedNotice) + category ProgressedNotice = 7
    assert_eq!(notices_cat1.len(), 7);

    // DynamicValue extractors coverage
    assert_eq!(extract_u32_ident(&DynamicValue::Uint32(7)), Some(7));
    assert_eq!(extract_u32_ident(&DynamicValue::Uint64(8)), Some(8));
    assert_eq!(extract_u32_ident(&DynamicValue::Int32(9)), Some(9));
    assert_eq!(extract_u32_ident(&DynamicValue::Int32(-1)), None);
    assert_eq!(extract_u32_ident(&DynamicValue::Int64(10)), Some(10));
    assert_eq!(extract_u32_ident(&DynamicValue::Int64(-5)), None);
    assert_eq!(
        extract_u32_ident(&DynamicValue::Bytes(vec![0, 0, 0, 0, 0, 0, 1, 0])),
        Some(256)
    );
    assert_eq!(extract_u32_ident(&DynamicValue::Bytes(vec![1, 2, 3])), None);
    assert_eq!(extract_u32_ident(&DynamicValue::String("x".into())), None);

    let mut msg_bytes = DynamicQuasarMessage::new("ident");
    msg_bytes
        .fields
        .insert(1, DynamicValue::Bytes(vec![0; 12].into_iter().chain([0, 0, 0, 42]).collect()));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(msg_bytes))), Some(42));

    let mut msg_u64 = DynamicQuasarMessage::new("ident");
    msg_u64.fields.insert(1, DynamicValue::Uint64(77));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(msg_u64))), Some(77));

    let mut msg_i32 = DynamicQuasarMessage::new("ident");
    msg_i32.fields.insert(1, DynamicValue::Int32(88));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(msg_i32))), Some(88));

    let mut msg_i64 = DynamicQuasarMessage::new("ident");
    msg_i64.fields.insert(1, DynamicValue::Int64(99));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(msg_i64))), Some(99));

    assert_eq!(extract_u64_val(&DynamicValue::Uint64(100)), Some(100));
    assert_eq!(extract_u64_val(&DynamicValue::Uint32(101)), Some(101));
    assert_eq!(extract_u64_val(&DynamicValue::Int64(102)), Some(102));
    assert_eq!(extract_u64_val(&DynamicValue::Int64(-1)), None);
    assert_eq!(extract_u64_val(&DynamicValue::Int32(103)), Some(103));
    assert_eq!(extract_u64_val(&DynamicValue::Int32(-1)), None);
    assert_eq!(extract_u64_val(&DynamicValue::String("no".into())), None);
    let _ = HashMap::<u32, u32>::new();
}
