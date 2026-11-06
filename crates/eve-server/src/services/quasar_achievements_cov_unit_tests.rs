//! Comprehensive unit tests for `quasar_achievements` covering serde visitors, category keyword
//! classification, SDE definition hydration, filtered ISK/SP reward claims, and FlatBuffer seeding.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use eve_db::WorldRepository;
use eve_proto::quasar::{DynamicQuasarMessage, QuasarDescriptorRegistry};
use eve_sde::{AchievementInfo, SdeRegistry};
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn make_ach_info(id: u32, name: &str, desc: &str) -> AchievementInfo {
    AchievementInfo {
        achievement_id: id,
        is_client_achievement: false,
        is_enabled: true,
        name_id: 0,
        description_id: 0,
        notification_text_id: 0,
        name: name.to_string(),
        description: desc.to_string(),
        notification_text: String::new(),
        target_value: 10,
        conditions: Vec::new(),
    }
}

#[test]
fn serde_i64_or_f64_visitor_and_category_classification_and_ident_extractors() {
    // deserialize_i64_or_f64: positive u64, negative i64, f64 rounding, and invalid type error (`expecting`)
    let d_float: AchievementDef = serde_json::from_str(
        r#"{"id":1,"category_id":1,"name":"n","target":5,"score":10,"milestone_targets":[5],"reward_isk":250000.6,"reward_sp":-12}"#,
    )
    .expect("float/i64 rewards");
    assert_eq!(d_float.reward_isk, 250_001);
    assert_eq!(d_float.reward_sp, -12);

    let bad = serde_json::from_str::<AchievementDef>(
        r#"{"id":1,"category_id":1,"name":"n","target":5,"score":10,"milestone_targets":[],"reward_isk":"not_a_num"}"#,
    );
    let err_msg = bad.unwrap_err().to_string();
    assert!(err_msg.contains("integer or floating point"));

    // classify_achievement_category: all 5 keyword branches + `(id % 5) + 1` fallback
    assert_eq!(classify_achievement_category(7, &make_ach_info(7, "Bounty Hunter", ""), &[]), 1);
    assert_eq!(classify_achievement_category(7, &make_ach_info(7, "Wormhole Diver", ""), &[]), 2);
    assert_eq!(classify_achievement_category(7, &make_ach_info(7, "Blueprint Master", ""), &[]), 3);
    assert_eq!(classify_achievement_category(7, &make_ach_info(7, "Market Tycoon", ""), &[]), 4);
    let social_cond = [AchievementConditionDef {
        condition_id: 1,
        target_value: 1,
        statistic_event_id: 99,
        event_name: "join_fleet".into(),
    }];
    assert_eq!(classify_achievement_category(7, &make_ach_info(7, "Unclassified", "plain"), &social_cond), 5);
    for id in 0..5_u32 {
        assert_eq!(
            classify_achievement_category(id, &make_ach_info(id, "Mystery", "Plain"), &[]),
            (id % 5) + 1
        );
    }

    // extract_u32_ident & extract_u64_val across all variants & boundaries
    assert_eq!(extract_u32_ident(&DynamicValue::Uint64(77)), Some(77));
    assert_eq!(extract_u32_ident(&DynamicValue::Uint64(u64::MAX)), None);
    assert_eq!(extract_u32_ident(&DynamicValue::Int32(88)), Some(88));
    assert_eq!(extract_u32_ident(&DynamicValue::Int32(-1)), None);
    assert_eq!(extract_u32_ident(&DynamicValue::Int64(99)), Some(99));
    assert_eq!(
        extract_u32_ident(&DynamicValue::Bytes(vec![0, 0, 0, 0, 0, 0, 1, 2])),
        Some(258)
    );
    assert_eq!(extract_u32_ident(&DynamicValue::Bytes(vec![1, 2, 3])), None);
    assert_eq!(extract_u32_ident(&DynamicValue::Bool(true)), None);

    let mut sub_bytes = DynamicQuasarMessage::new("Ident");
    sub_bytes.fields.insert(1, DynamicValue::Bytes(vec![0; 12].into_iter().chain(42_u32.to_be_bytes()).collect()));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(sub_bytes))), Some(42));

    let mut sub_u64 = DynamicQuasarMessage::new("Ident");
    sub_u64.fields.insert(1, DynamicValue::Uint64(314));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(sub_u64))), Some(314));

    let mut sub_i32 = DynamicQuasarMessage::new("Ident");
    sub_i32.fields.insert(1, DynamicValue::Int32(512));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(sub_i32))), Some(512));

    let mut sub_i64 = DynamicQuasarMessage::new("Ident");
    sub_i64.fields.insert(1, DynamicValue::Int64(1024));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(sub_i64))), Some(1024));

    let mut sub_bad = DynamicQuasarMessage::new("Ident");
    sub_bad.fields.insert(1, DynamicValue::Bool(false));
    assert_eq!(extract_u32_ident(&DynamicValue::Message(Box::new(sub_bad))), None);

    assert_eq!(extract_u64_val(&DynamicValue::Uint64(500)), Some(500));
    assert_eq!(extract_u64_val(&DynamicValue::Uint32(400)), Some(400));
    assert_eq!(extract_u64_val(&DynamicValue::Int32(300)), Some(300));
    assert_eq!(extract_u64_val(&DynamicValue::Int32(-1)), None);
    assert_eq!(extract_u64_val(&DynamicValue::Int64(-5)), None);
    assert_eq!(extract_u64_val(&DynamicValue::String("x".into())), None);
}

#[test]
fn sde_hydration_db_reload_and_filtered_wallet_sp_reward_claims() {
    // Empty SDE without db_path hits `load_achievements() == Err` fallback (`b"[]"`)
    let empty_sde = SdeRegistry::default();
    let (empty_defs, empty_keys, empty_cnt, empty_hash, empty_json) = build_definitions_from_sde(&empty_sde);
    assert!(empty_defs.is_empty() && empty_keys.is_empty() && empty_cnt == 0);
    assert_eq!(empty_json, b"[]");
    assert_eq!(empty_hash.len(), 16);

    // Real SDE hydration + DB reload refreshing stale `a.def` and parsing `quasar_achievement_cat_scores`
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let mgr_initial = AchievementManager::new_with_sde(Some(Arc::clone(&db)), Some(Arc::clone(&sde)));
    assert!(!mgr_initial.definitions.is_empty());
    let sample_id = mgr_initial.definitions[0].id;
    let cid = 90_000_001_u32;

    mgr_initial.unlock_achievement(cid, sample_id);
    mgr_initial.set_category_score(cid, 2, 120);
    // Inject a malformed key into `quasar_achievement_cat_scores` to verify graceful skip on reload
    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert(
            "quasar_achievement_cat_scores".into(),
            format!(r#"{{"{cid}:2":120,"bad_key":99,"{cid}:bad":50}}"#),
        );
    });

    let mgr_reloaded = AchievementManager::new_with_sde(Some(Arc::clone(&db)), Some(Arc::clone(&sde)));
    assert_eq!(mgr_reloaded.category_score(cid, 2), 120);
    assert!(mgr_reloaded
        .get_or_init_achievements(cid)
        .iter()
        .any(|a| a.def.id == sample_id && a.completed));

    // Also exercise `AchievementManager::default()`
    let def_mgr = AchievementManager::default();
    assert!(!def_mgr.sde_achievement_hash.is_empty());

    // Custom manager testing:
    // - `mark_completed` with `milestone_targets.is_empty()` and `reward_isk > 0` adding milestone 1 once
    // - `claim_rewards_filtered` by achievement_id, category_id, milestone_id, selected_reward_uuids,
    //   and `reached_milestones.is_empty()` fallback `(1..=unclaimed)`
    // - Live wallet ISK + SP + `"AIRAchievementReward"` journal entry on demo character `90_000_001`!
    let mut custom_mgr = AchievementManager::new_with_sde(Some(Arc::clone(&db)), None);
    custom_mgr.definitions = vec![
        AchievementDef {
            id: 301,
            is_client_achievement: true,
            category_id: 1,
            name: "No Milestones Rewarded".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 2,
            score: 25,
            milestone_targets: Vec::new(),
            conditions: vec![AchievementConditionDef {
                condition_id: 11,
                target_value: 2,
                statistic_event_id: 901,
                event_name: "client_cam".into(),
            }],
            reward_isk: 150_000,
            reward_sp: 3_000,
        },
        AchievementDef {
            id: 302,
            is_client_achievement: false,
            category_id: 3,
            name: "Two Milestones".into(),
            description: String::new(),
            notification_text: String::new(),
            target: 10,
            score: 40,
            milestone_targets: vec![5, 10],
            conditions: Vec::new(),
            reward_isk: 200_000,
            reward_sp: 1_500,
        },
    ];

    custom_mgr.character_achievements.write().unwrap().clear();

    // Unknown achievement errors
    assert!(custom_mgr.add_progress(cid, 999_999, 1, 100).is_none());
    assert_eq!(custom_mgr.mark_completed(cid, 999_999, 100).unwrap_err().0, STATUS_NOT_FOUND);
    assert_eq!(custom_mgr.reset_achievement(cid, 999_999).unwrap_err().0, STATUS_NOT_FOUND);

    // Uninitialized character claim returns 0
    assert_eq!(custom_mgr.claim_rewards(88_888_888, &db), 0);

    // mark_completed on empty milestone_targets with reward_isk > 0 adds milestone 1 exactly once
    let mc1 = custom_mgr.mark_completed(cid, 301, 2_000).unwrap();
    let mc2 = custom_mgr.mark_completed(cid, 301, 2_001).unwrap();
    assert_eq!(mc1.reached_milestones, vec![1]);
    assert_eq!(mc2.unclaimed_reward_count, 1);

    // Advance 302 to both milestones, then clear `reached_milestones` while keeping `unclaimed_reward_count = 2`
    // to cover the `(1..=a.unclaimed_reward_count)` fallback branch in `claim_rewards_filtered`!
    custom_mgr.add_progress(cid, 302, 10, 2_002).unwrap();
    {
        let mut map = custom_mgr.character_achievements.write().unwrap();
        let a302 = map.get_mut(&cid).unwrap().iter_mut().find(|a| a.def.id == 302).unwrap();
        a302.reached_milestones.clear();
        assert_eq!(a302.unclaimed_reward_count, 2);
    }

    let (bal_before, sp_before, j_before) = db.with_snapshot(|s| {
        let ch = &s.characters[&(cid as i64)];
        (ch.balance, ch.free_skill_points, s.wallet_journal.len())
    });

    // Non-matching filters claim 0
    assert_eq!(
        custom_mgr.claim_rewards_filtered(
            cid,
            &db,
            &RewardClaimFilter {
                category_id: Some(5),
                ..Default::default()
            }
        ),
        0
    );

    // Claim only milestone 2 of achievement 302 by UUID
    let uuid_302_m2 = achievement_reward_uuid(302, 2);
    let claimed_one = custom_mgr.claim_rewards_filtered(
        cid,
        &db,
        &RewardClaimFilter {
            achievement_id: Some(302),
            milestone_id: Some(2),
            selected_reward_uuids: Some(vec![uuid_302_m2]),
            ..Default::default()
        },
    );
    assert_eq!(claimed_one, 1);

    // Claim remaining rewards across all categories
    let claimed_rest = custom_mgr.claim_rewards(cid, &db);
    assert_eq!(claimed_rest, 2); // 1 from 301 + 1 remaining from 302

    db.with_snapshot(|s| {
        let ch = &s.characters[&(cid as i64)];
        // Total ISK = 200_000 (first claim) + 150_000 + 200_000 (second claim) = 550_000 ISK
        assert_eq!(ch.balance, IskCents(bal_before.0 + IskCents::from_isk(550_000).0));
        assert!((ch.free_skill_points - (sp_before + 6_000.0)).abs() < 1e-6);
        assert_eq!(s.wallet_journal.len(), j_before + 2);
        assert!(s.wallet_journal.iter().any(|j| j.entry_type == "AIRAchievementReward"));
    });

    // Zero-delta guards on gameplay & client counter events
    let router = PublicGatewayRouter::new(
        Arc::new(QuasarDescriptorRegistry::empty_with_builtins()),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    custom_mgr.on_gameplay_event_by_name(cid, &[], 5, 3_000, &router);
    custom_mgr.on_gameplay_event_by_name(cid, &["client_cam"], 0, 3_000, &router);
    custom_mgr.on_client_counter_event(cid, "901", 0, 3_000, &router);
    custom_mgr.on_client_counter_event(cid, "901", 2, 3_000, &router);
}

#[test]
fn flatbuffer_disk_storage_seeding_writes_empty_and_preserves_existing() {
    let tmp_base = std::env::temp_dir().join(format!("evomu_ach_fb_test_{}", std::process::id()));
    let storage_dir = tmp_base
        .join("CCP")
        .join("EVE")
        .join("tq_profile")
        .join("cache")
        .join("Storage");
    let _ = std::fs::remove_dir_all(&tmp_base);
    std::fs::create_dir_all(&storage_dir).expect("create temp Storage dir");

    let prev = std::env::var_os("LOCALAPPDATA");
    std::env::set_var("LOCALAPPDATA", &tmp_base);

    let payload = empty_achievement_flatbuffer();
    ensure_client_flatbuffer_storage_seeded("test_ach_content", "v1_hash", &payload);

    let data_path = storage_dir.join("test_ach_content").join("data.bin");
    let ver_path = storage_dir.join("test_ach_content").join("version.txt");
    assert_eq!(std::fs::read(&data_path).unwrap(), payload);
    assert_eq!(std::fs::read_to_string(&ver_path).unwrap(), "v1_hash");

    // Second call with non-empty data.bin must NOT overwrite existing file!
    ensure_client_flatbuffer_storage_seeded("test_ach_content", "v2_should_not_overwrite", b"DIFFERENT");
    assert_eq!(std::fs::read(&data_path).unwrap(), payload);
    assert_eq!(std::fs::read_to_string(&ver_path).unwrap(), "v1_hash");

    match prev {
        Some(v) => std::env::set_var("LOCALAPPDATA", v),
        None => std::env::remove_var("LOCALAPPDATA"),
    }
    let _ = std::fs::remove_dir_all(&tmp_base);
}
