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
            reward_isk: 1_000,
            reward_sp: 10,
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
            reward_isk: 500,
            reward_sp: 5,
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

#[test]
fn achievements_mutant_killers_locks_rehydrate_partial_claims_and_notices() {
    use crate::quasar_gateway::NoticeScope;
    use eve_proto::quasar::{DynamicQuasarMessage, WireMessage};

    // 1. Default SDE definitions load from client static data with DERIVED non-zero category IDs (1..=5)
    // and without synthetic score/milestone/reward fabrication.
    let sde_mgr = AchievementManager::new(None);
    assert!(!sde_mgr.definitions.is_empty());
    let mut seen_cats = std::collections::HashSet::new();
    for d in &sde_mgr.definitions {
        assert!((1..=5).contains(&d.category_id), "expected DERIVED category 1..=5 for {}, got {}", d.id, d.category_id);
        seen_cats.insert(d.category_id);
        assert_eq!(d.score, 0);
        assert!(d.milestone_targets.is_empty());
        assert_eq!(d.reward_isk, 0);
        assert_eq!(d.reward_sp, 0);
        assert!(!d.name.is_empty());
        assert!(d.target >= 1);
    }
    assert_eq!(seen_cats.len(), 5, "all 5 DERIVED achievement categories should be populated");

    // 2. Multi-condition clamping, a.progress.max(total_cond), client-id enforcement, and mark_completed no-reward edge
    let (ctx, _clock) = test_ctx_and_clock(1_750_000_000);
    let mut mgr = AchievementManager::new_with_sde(Some(Arc::clone(&ctx.db)), None);
    mgr.definitions = vec![
        AchievementDef {
            id: 50,
            is_client_achievement: false,
            category_id: 1,
            name: "Two Cond Server".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 30,
            score: 50,
            milestone_targets: vec![10, 20, 30],
            conditions: vec![
                AchievementConditionDef {
                    condition_id: 1,
                    target_value: 10,
                    statistic_event_id: 701,
                    event_name: "ev_a".into(),
                },
                AchievementConditionDef {
                    condition_id: 2,
                    target_value: 20,
                    statistic_event_id: 702,
                    event_name: "ev_b".into(),
                },
            ],
            reward_isk: 50_000,
            reward_sp: 0,
        },
        AchievementDef {
            id: 51,
            is_client_achievement: false,
            category_id: 2,
            name: "No Milestones Unrewarded".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 1,
            score: 20,
            milestone_targets: Vec::new(),
            conditions: Vec::new(),
            reward_isk: 0,
            reward_sp: 0,
        },
        AchievementDef {
            id: 52,
            is_client_achievement: true,
            category_id: 2,
            name: "SP Only No Milestones".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 2,
            score: 30,
            milestone_targets: Vec::new(),
            conditions: vec![AchievementConditionDef {
                condition_id: 3,
                target_value: 2,
                statistic_event_id: 703,
                event_name: "client_ping".into(),
            }],
            reward_isk: 0,
            reward_sp: 1_250,
        },
        AchievementDef {
            id: 53,
            is_client_achievement: false,
            category_id: 4,
            name: "Negative Rewards Clamped".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 10,
            score: 10,
            milestone_targets: vec![5, 10],
            conditions: Vec::new(),
            reward_isk: -10_000,
            reward_sp: -500,
        },
    ];
    mgr.allowed_client_counter_keys.insert("client_ping".into());

    let cid = 90_000_001_u32;
    let cid2 = 90_000_099_u32;

    // mark_completed on 51 (no milestones, 0 ISK, 0 SP) must NOT create milestone 1 or increment unclaimed_reward_count
    let a51 = mgr.mark_completed(cid, 51, 100).unwrap();
    assert!(a51.reached_milestones.is_empty());
    assert_eq!(a51.unclaimed_reward_count, 0);

    // mark_completed on 52 (no milestones, 0 ISK, >0 SP) DOES add milestone 1 once
    let a52_first = mgr.mark_completed(cid, 52, 100).unwrap();
    let a52_second = mgr.mark_completed(cid, 52, 101).unwrap();
    assert_eq!(a52_first.reached_milestones, vec![1]);
    assert_eq!(a52_second.unclaimed_reward_count, 1);

    // apply_client_task_completions_and_counters MUST ignore non-client achievement ID 50!
    let _ = ctx.quasar_gateway.drain_queued_notices();
    let _ = ctx.quasar_gateway.pending_notices.write().unwrap().drain(..).count();
    mgr.apply_client_task_completions_and_counters(
        cid,
        &[50, 999_999],
        &[("ev_a".to_string(), 100)], // server event key ignored by client counter handler!
        200,
        &ctx.quasar_gateway,
    );
    let a50_after_client = mgr.get_or_init_achievements(cid).into_iter().find(|a| a.def.id == 50).unwrap();
    assert!(!a50_after_client.completed);
    assert_eq!(a50_after_client.progress, 0);

    // Multi-condition clamping + max(total_cond_progress) + just_completed stays true once
    let (p1, jc1, ms1) = mgr.add_progress(cid, 50, 15, 300).unwrap();
    assert_eq!(p1.condition_progress.get(&1), Some(&10));
    assert_eq!(p1.condition_progress.get(&2), Some(&5));
    assert!(!jc1);
    assert_eq!(ms1, vec![1]);

    // Over-driving condition 1 (event 701) via record_statistic_event clamps cond 1 at 10 (total_cond=15 <= prior progress 15)
    let upd_ev1 = mgr.record_statistic_event(cid, 701, 50, 301);
    assert_eq!(upd_ev1[0].0.progress, 15);
    assert!(!upd_ev1[0].1);
    // Completing condition 2 (event 702) finishes achievement 50 and triggers milestones 2 and 3
    let upd_ev2 = mgr.record_statistic_event(cid, 702, 20, 302);
    assert_eq!(upd_ev2[0].0.progress, 30);
    assert!(upd_ev2[0].1);
    assert_eq!(upd_ev2[0].0.completed_date, Some(302));
    assert_eq!(upd_ev2[0].2, vec![2, 3]);
    // Subsequent add_progress leaves completed_date intact and just_completed=false
    let (p_again, jc_again, ms_again) = mgr.add_progress(cid, 50, 5, 999).unwrap();
    assert!(!jc_again);
    assert!(ms_again.is_empty());
    assert_eq!(p_again.completed_date, Some(302));

    // 3. save_to_db re-entrant lock guard must return safely without deadlocking, and rehydrate_from_db reloads + refreshes
    mgr.save_to_db();
    ctx.db.with_mut_snapshot(|_snap| {
        mgr.save_to_db(); // is_write_locked_by_current_thread() == true -> early return!
    });

    // Mutate only in-memory state on cid2 and verify rehydrate_from_db restores DB truth + refreshes stale def
    mgr.mark_completed(cid2, 51, 500).unwrap();
    assert!(mgr.get_or_init_achievements(cid2).iter().any(|a| a.def.id == 51 && a.completed));
    mgr.rehydrate_from_db();
    assert!(!mgr.character_achievements.read().unwrap().contains_key(&cid2));

    // Rehydration with None DB is a no-op
    let mgr_no_db = AchievementManager::new_with_sde(None, None);
    mgr_no_db.rehydrate_from_db();

    // 4. Negative rewards clamp to 0; SP-only claim skips wallet journal entry; ISK claim writes exact journal description
    mgr.mark_completed(cid, 53, 400).unwrap();
    let (bal0, sp0, j0) = ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&(cid as i64)];
        (ch.balance, ch.free_skill_points, s.wallet_journal.len())
    });

    // Empty selected_reward_uuids claims 0
    assert_eq!(
        mgr.claim_rewards_filtered(
            cid,
            &ctx.db,
            &RewardClaimFilter {
                selected_reward_uuids: Some(vec![]),
                ..Default::default()
            }
        ),
        0
    );

    // Negative/unconfigured reward achievement 53 produces 0 claimable rewards without touching wallet ISK or SP
    assert_eq!(
        mgr.claim_rewards_filtered(
            cid,
            &ctx.db,
            &RewardClaimFilter {
                achievement_id: Some(53),
                ..Default::default()
            }
        ),
        0
    );
    ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&(cid as i64)];
        assert_eq!(ch.balance, bal0);
        assert!((ch.free_skill_points - sp0).abs() < 1e-6);
        assert_eq!(s.wallet_journal.len(), j0);
    });

    // Claiming SP-only achievement 52 increments free_skill_points WITHOUT appending wallet journal entry
    assert_eq!(
        mgr.claim_rewards_filtered(
            cid,
            &ctx.db,
            &RewardClaimFilter {
                achievement_id: Some(52),
                ..Default::default()
            }
        ),
        1
    );
    ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&(cid as i64)];
        assert_eq!(ch.balance, bal0);
        assert!((ch.free_skill_points - (sp0 + 1_250.0)).abs() < 1e-6);
        assert_eq!(s.wallet_journal.len(), j0);
    });

    // Claiming 2 milestones of ISK achievement 50 (via selected_reward_uuids) writes 100_000 ISK and exact journal description "(2x)"
    let claimed_50 = mgr.claim_rewards_filtered(
        cid,
        &ctx.db,
        &RewardClaimFilter {
            category_id: Some(1),
            selected_reward_uuids: Some(vec![
                achievement_reward_uuid(50, 2),
                achievement_reward_uuid(50, 3),
            ]),
            ..Default::default()
        },
    );
    assert_eq!(claimed_50, 2);
    ctx.db.with_snapshot(|s| {
        let ch = &s.characters[&(cid as i64)];
        assert_eq!(ch.balance, IskCents(bal0.0 + IskCents::from_isk(100_000).0));
        assert_eq!(s.wallet_journal.len(), j0 + 1);
        assert_eq!(s.wallet_journal.last().unwrap().reason, "AIR Achievement reward (2x)");
    });

    // Claiming for non-existent character in snap.characters still persists blob without panicking
    mgr.mark_completed(88_888_888, 50, 500).unwrap();
    assert_eq!(mgr.claim_rewards(88_888_888, &ctx.db), 3);

    // 5. Scores (override + non-override sum without double-counting) and scoped resets
    mgr.mark_completed(cid2, 50, 600).unwrap();
    mgr.set_category_score(cid, 1, 999);
    // cid has override 999 for cat 1 (completed 50 score 50 NOT double-counted) + completed 51 (score 20) & 52 (score 30) in cat 2 + 53 (score 10) in cat 4
    assert_eq!(mgr.total_character_score(cid), 999 + 20 + 30 + 10);
    assert_eq!(mgr.category_score(cid, 1), 999);
    assert_eq!(mgr.category_score(cid, 2), 50);

    // reset_category only resets cat 1 for cid, leaving cat 2 and cid2 untouched
    mgr.reset_category(cid, 1);
    assert_eq!(mgr.category_score(cid, 1), 0);
    assert_eq!(mgr.category_score(cid, 2), 50);
    assert_eq!(mgr.category_score(cid2, 1), 50);

    // reset_all keeps empty-progress records for cid and preserves cid2
    mgr.reset_all(cid);
    assert_eq!(mgr.total_character_score(cid), 0);
    assert_eq!(mgr.get_or_init_achievements(cid).len(), 4);
    assert_eq!(mgr.total_character_score(cid2), 50);

    // clear_character drops cid completely while preserving cid2
    mgr.clear_character(cid);
    assert!(!mgr.character_achievements.read().unwrap().contains_key(&cid));
    assert!(mgr.character_achievements.read().unwrap().contains_key(&cid2));

    // 6. emit_achievement_notices exact payloads & extract_u32_ident exact length-4 boundary
    let queued = std::sync::RwLock::new(Vec::new());
    emit_achievement_notices(&queued, cid, 50, 0, 12, 0, false, false, &[1], &[99], 7_000);
    let n_cat0 = std::mem::take(&mut *queued.write().unwrap());
    assert_eq!(n_cat0.len(), 2);
    assert_eq!(n_cat0[0].0, NoticeScope::Character(cid));
    assert!(n_cat0[0].1.type_url.ends_with("achievement.api.ProgressedNotice"));
    assert!(n_cat0[1].1.type_url.ends_with("achievement.milestone.api.ReachedNotice"));
    let prog_wm = WireMessage::decode(&n_cat0[0].1.value).unwrap();
    assert_eq!(prog_wm.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_varint(), Some(12));

    emit_achievement_notices(&queued, cid, 50, 1, 12, 0, false, false, &[1], &[], 7_000);
    let n_cat1_unchanged = std::mem::take(&mut *queued.write().unwrap());
    assert_eq!(n_cat1_unchanged.len(), 2);
    assert!(!n_cat1_unchanged
        .iter()
        .any(|(_, a, _)| a.type_url.ends_with("achievement.category.api.ProgressedNotice")));

    emit_achievement_notices(&queued, cid, 50, 1, 30, 50, true, true, &[2, 3], &[101, 102], 7_001);
    let n_cat1 = std::mem::take(&mut *queued.write().unwrap());
    assert_eq!(n_cat1.len(), 7);
    let ach_ms_uuids: Vec<[u8; 16]> = n_cat1
        .iter()
        .filter(|(_, a, _)| a.type_url.ends_with("achievement.milestone.api.ReachedNotice"))
        .map(|(_, a, _)| {
            let wm = WireMessage::decode(&a.value).unwrap();
            let ms_bytes = wm.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_bytes().unwrap();
            let ms_wm = WireMessage::decode(ms_bytes).unwrap();
            ms_wm.fields[0].value.as_bytes().unwrap().try_into().unwrap()
        })
        .collect();
    assert_eq!(ach_ms_uuids, vec![u32_to_uuid(2), u32_to_uuid(3)]);

    let cat_ms_uuids: Vec<[u8; 16]> = n_cat1
        .iter()
        .filter(|(_, a, _)| a.type_url.ends_with("achievement.category.milestone.api.ReachedNotice"))
        .map(|(_, a, _)| {
            let wm = WireMessage::decode(&a.value).unwrap();
            let ms_bytes = wm.fields.iter().find(|f| f.field_number == 2).unwrap().value.as_bytes().unwrap();
            let ms_wm = WireMessage::decode(ms_bytes).unwrap();
            ms_wm.fields[0].value.as_bytes().unwrap().try_into().unwrap()
        })
        .collect();
    assert_eq!(cat_ms_uuids, vec![u32_to_uuid(101), u32_to_uuid(102)]);
    assert!(n_cat1.iter().any(|(_, a, _)| a.type_url.ends_with("achievement.api.CompletedNotice")));
    assert!(n_cat1.iter().any(|(_, a, _)| a.type_url.ends_with("achievement.category.api.ProgressedNotice")));

    // extract_u32_ident exact 3 vs 4 byte boundary
    assert_eq!(extract_u32_ident(&DynamicValue::Bytes(vec![1, 2, 3])), None);
    assert_eq!(extract_u32_ident(&DynamicValue::Bytes(vec![0, 0, 1, 2])), Some(258));
    let mut m_len4 = DynamicQuasarMessage::new("Id");
    m_len4.fields.insert(1, DynamicValue::Bytes(vec![0, 0, 2, 1]));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(m_len4))), Some(513));
    let m_empty = DynamicQuasarMessage::new("Id");
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(m_empty))), None);
    assert_eq!(extract_u32_ident(&DynamicValue::Int64(i64::from(u32::MAX) + 1)), None);
}

#[test]
fn category_milestones_client_task_notices_and_completion_sync() {
    use eve_proto::quasar::QuasarDescriptorRegistry;

    let reg = QuasarDescriptorRegistry::open_default().expect("open descriptors");
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let mut mgr = AchievementManager::new_with_sde(Some(Arc::clone(&ctx.db)), None);
    mgr.definitions = vec![
        AchievementDef {
            id: 10,
            is_client_achievement: false,
            category_id: 1,
            name: "srv 10".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 10,
            score: 25,
            milestone_targets: vec![5, 10],
            conditions: Vec::new(),
            reward_isk: 1_000,
            reward_sp: 0,
        },
        AchievementDef {
            id: 11,
            is_client_achievement: false,
            category_id: 1,
            name: "srv 11".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 4,
            score: 35,
            milestone_targets: Vec::new(),
            conditions: Vec::new(),
            reward_isk: 0,
            reward_sp: 0,
        },
        AchievementDef {
            id: 12,
            is_client_achievement: true,
            category_id: 2,
            name: "client task 12".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 1,
            score: 40,
            milestone_targets: vec![1],
            conditions: Vec::new(),
            reward_isk: 0,
            reward_sp: 0,
        },
        AchievementDef {
            id: 13,
            is_client_achievement: true,
            category_id: 2,
            name: "client counter 13".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 1,
            score: 15,
            milestone_targets: Vec::new(),
            conditions: vec![AchievementConditionDef {
                condition_id: 1,
                target_value: 1,
                statistic_event_id: 801,
                event_name: "cam_zoom".into(),
            }],
            reward_isk: 0,
            reward_sp: 0,
        },
    ];
    mgr.set_category_milestones(
        1,
        vec![
            CategoryMilestoneDef { id: 101, target_score: 20 },
            CategoryMilestoneDef { id: 102, target_score: 50 },
        ],
    );
    mgr.set_category_milestones(
        2,
        vec![CategoryMilestoneDef { id: 201, target_score: 40 }],
    );

    let cid = 90_000_077_u32;

    // 1. Partial add_progress reaches achievement milestone 1 only, zero category score yet
    let o_part = mgr.add_progress_with_outcomes(cid, 10, 5, 1_000).unwrap();
    assert_eq!(o_part.achievement.progress, 5);
    assert!(!o_part.just_completed);
    assert_eq!(o_part.new_ach_milestones, vec![1]);
    assert_eq!(o_part.cat_score, 0);
    assert!(!o_part.cat_score_changed);
    assert!(o_part.new_cat_milestones.is_empty());

    // 2. mark_completed_with_outcomes emits ONLY new achievement milestone 2, updates category score to 25,
    // and crosses category milestone 101 (20) without touching 102 (50).
    let o_comp = mgr.mark_completed_with_outcomes(cid, 10, 1_001).unwrap();
    assert!(o_comp.achievement.completed);
    assert!(o_comp.just_completed);
    assert_eq!(o_comp.new_ach_milestones, vec![2]);
    assert_eq!(o_comp.cat_score, 25);
    assert!(o_comp.cat_score_changed);
    assert_eq!(o_comp.new_cat_milestones, vec![101]);

    // Repeat completion is idempotent: no duplicate completion, score change, or milestones
    let o_dup = mgr.mark_completed_with_outcomes(cid, 10, 1_002).unwrap();
    assert!(!o_dup.just_completed);
    assert!(o_dup.new_ach_milestones.is_empty());
    assert_eq!(o_dup.cat_score, 25);
    assert!(!o_dup.cat_score_changed);
    assert!(o_dup.new_cat_milestones.is_empty());

    // Completing achievement 11 (+35 -> 60) crosses 102 (50) without re-triggering 101
    let o_11 = mgr.mark_completed_with_outcomes(cid, 11, 1_003).unwrap();
    assert!(o_11.just_completed);
    assert_eq!(o_11.cat_score, 60);
    assert!(o_11.cat_score_changed);
    assert_eq!(o_11.new_cat_milestones, vec![102]);
    assert_eq!(
        mgr.reached_category_milestones(cid, 1),
        vec![
            ReachedCategoryMilestone { milestone_id: 101, reached_date: 1_001 },
            ReachedCategoryMilestone { milestone_id: 102, reached_date: 1_003 },
        ]
    );

    // 3. Persistence round-trip + reset_achievement pruning + re-completion + downward set_category_score + reset_category cleanup
    mgr.save_to_db();
    let mut reloaded = AchievementManager::new_with_sde(Some(Arc::clone(&ctx.db)), None);
    reloaded.definitions = mgr.definitions.clone();
    reloaded.set_category_milestones(
        1,
        vec![
            CategoryMilestoneDef { id: 101, target_score: 20 },
            CategoryMilestoneDef { id: 102, target_score: 50 },
        ],
    );
    reloaded.rehydrate_from_db();
    assert_eq!(reloaded.category_score(cid, 1), 60);
    assert_eq!(reloaded.reached_category_milestones(cid, 1).len(), 2);

    let (_, score_upd) = reloaded.reset_achievement_with_category_score(cid, 11).unwrap();
    assert_eq!(score_upd, Some((1, 25)));
    assert_eq!(reloaded.category_score(cid, 1), 25);
    assert_eq!(
        reloaded.reached_category_milestones(cid, 1),
        vec![ReachedCategoryMilestone { milestone_id: 101, reached_date: 1_001 }]
    );

    // Pruned milestone stays absent after rehydration from DB
    let mut reloaded_after_reset = AchievementManager::new_with_sde(Some(Arc::clone(&ctx.db)), None);
    reloaded_after_reset.definitions = mgr.definitions.clone();
    reloaded_after_reset.rehydrate_from_db();
    assert_eq!(reloaded_after_reset.category_score(cid, 1), 25);
    assert_eq!(
        reloaded_after_reset.reached_category_milestones(cid, 1),
        vec![ReachedCategoryMilestone { milestone_id: 101, reached_date: 1_001 }]
    );

    // Re-completing achievement 11 re-triggers category milestone 102
    let o_11_re = reloaded.mark_completed_with_outcomes(cid, 11, 1_004).unwrap();
    assert_eq!(o_11_re.cat_score, 60);
    assert!(o_11_re.cat_score_changed);
    assert_eq!(o_11_re.new_cat_milestones, vec![102]);
    assert_eq!(
        reloaded.reached_category_milestones(cid, 1),
        vec![
            ReachedCategoryMilestone { milestone_id: 101, reached_date: 1_001 },
            ReachedCategoryMilestone { milestone_id: 102, reached_date: 1_004 },
        ]
    );

    // Downward set_category_score prunes milestones above the new score
    reloaded.set_category_score(cid, 1, 10);
    assert_eq!(reloaded.category_score(cid, 1), 10);
    assert!(reloaded.reached_category_milestones(cid, 1).is_empty());

    reloaded.reset_category(cid, 1);
    assert_eq!(reloaded.category_score(cid, 1), 0);
    assert!(reloaded.reached_category_milestones(cid, 1).is_empty());

    // 4. apply_client_task_completions_and_counters passes computed category score & separated milestones
    let _ = ctx.quasar_gateway.drain_queued_notices();
    let mut sess = eve_machonet::SessionState::new(77_001);
    sess.user_id = Some(1);
    sess.character_id = Some(cid as i64);
    sess.corporation_id = Some(98_000_001);
    let mut rx = ctx.quasar_gateway.subscribe_session(&sess, Vec::new(), 2_000);

    mgr.apply_client_task_completions_and_counters(
        cid,
        &[12],
        &[("cam_zoom".to_string(), 1)],
        2_000,
        &ctx.quasar_gateway,
    );

    let mut cat2_scores = Vec::new();
    let mut ach12_ms = Vec::new();
    let mut cat2_ms = Vec::new();
    while let Ok(notice) = rx.try_recv() {
        let Some(ref p) = notice.payload else { continue };
        match p.normalized_type_name() {
            "eve_public.achievement.category.api.ProgressedNotice" => {
                let d = reg.decode_dynamic("eve_public.achievement.category.api.ProgressedNotice", &p.value).unwrap();
                if let Some(DynamicValue::Uint32(s)) = d.get(2) {
                    cat2_scores.push(*s);
                }
            }
            "eve_public.achievement.milestone.api.ReachedNotice" => {
                let d = reg.decode_dynamic("eve_public.achievement.milestone.api.ReachedNotice", &p.value).unwrap();
                if let Some(DynamicValue::Message(id)) = d.get(2) {
                    if let Some(DynamicValue::Bytes(b)) = id.get(1) {
                        ach12_ms.push(b.clone());
                    }
                }
            }
            "eve_public.achievement.category.milestone.api.ReachedNotice" => {
                let d = reg.decode_dynamic("eve_public.achievement.category.milestone.api.ReachedNotice", &p.value).unwrap();
                if let Some(DynamicValue::Message(id)) = d.get(2) {
                    if let Some(DynamicValue::Bytes(b)) = id.get(1) {
                        cat2_ms.push(b.clone());
                    }
                }
            }
            _ => {}
        }
    }
    assert_eq!(cat2_scores, vec![40, 55]);
    assert_eq!(ach12_ms, vec![u32_to_uuid(1).to_vec()]);
    assert_eq!(cat2_ms, vec![u32_to_uuid(201).to_vec()]);
}


#[cfg(test)]
#[path = "quasar_achievements_extra_unit_tests.rs"]
mod quasar_achievements_extra_unit_tests;
