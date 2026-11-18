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
    assert_eq!(def.effective_name_message_id(), 582_516);
    assert_eq!(def.effective_description_message_id(), 582_517);
    assert_eq!(def.effective_group(), "Scanning");
    assert_eq!(def.effective_threat(), 1);

    let defs = default_career_goal_definitions();
    assert_eq!(defs.len(), 188);
    for g in &defs {
        assert!((601_356..=601_543).contains(&g.effective_name_message_id()));
        assert!((601_544..=601_731).contains(&g.effective_description_message_id()));
        assert!((601_732..=601_919).contains(&g.effective_aura_text_id()));
    }

    // Stale persisted synthesized IDs (e.g. 665529..665537 / 696260..696439) heal from defaults.
    let (stale_ctx, _) = test_ctx_and_clock(1_700_000_000);
    let mut stale_defs = defs.clone();
    let sample_uuid = stale_defs[0].uuid;
    let expected_name_id = stale_defs[0].name_message_id;
    let expected_desc_id = stale_defs[0].description_message_id;
    stale_defs[0].name_message_id = 665_529;
    stale_defs[0].description_message_id = 696_260;
    stale_defs[0].aura_text_id = None;
    stale_ctx.db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert(
            "quasar_career_goal_defs".into(),
            serde_json::to_string(&stale_defs).unwrap(),
        );
    });
    let healed_mgr = CareerGoalManager::new(Some(Arc::clone(&stale_ctx.db)));
    let healed = healed_mgr
        .load_definitions()
        .into_iter()
        .find(|d| d.uuid == sample_uuid)
        .unwrap();
    assert_eq!(healed.effective_name_message_id(), expected_name_id);
    assert_eq!(healed.effective_description_message_id(), expected_desc_id);
    assert!(healed.aura_text_id.is_some());

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

#[test]
fn career_goal_effective_field_boundaries_and_stale_loc_healing() {
    let make = |career_id: u32, name: &str, kind: &str| CareerGoalDef {
        uuid: [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        career_id,
        activity_name: name.into(),
        activity_kind: kind.into(),
        name: "Goal".into(),
        description: "Desc".into(),
        target: 10,
        career_points: 10,
        isk_reward: 0,
        sp_reward: 0.0,
        name_message_id: 0,
        description_message_id: 0,
        threat: 0,
        ..Default::default()
    };

    // Threat boundaries when raw threat == 0: 15 -> 1, 16..=25 -> 2, >=26 -> 3; nonzero clamped [1, 3]
    let mut d = make(1, "Scanning", "exploration");
    d.career_points = 15;
    assert_eq!(d.effective_threat(), 1);
    d.career_points = 16;
    assert_eq!(d.effective_threat(), 2);
    d.career_points = 25;
    assert_eq!(d.effective_threat(), 2);
    d.career_points = 26;
    assert_eq!(d.effective_threat(), 3);
    d.threat = 99;
    assert_eq!(d.effective_threat(), 3);
    d.threat = 2;
    d.career_points = 100;
    assert_eq!(d.effective_threat(), 2);

    // Duration boundaries: None or Some(0) -> (cp * 2).max(5); nonzero kept
    let mut dur_def = make(1, "Scanning", "exploration");
    dur_def.career_points = 2;
    assert_eq!(dur_def.effective_duration(), 5);
    dur_def.duration = Some(0);
    assert_eq!(dur_def.effective_duration(), 5);
    dur_def.career_points = 3;
    assert_eq!(dur_def.effective_duration(), 6);
    dur_def.duration = Some(12);
    assert_eq!(dur_def.effective_duration(), 12);

    // Order boundaries: None or Some(0) -> uuid[15].max(1); nonzero kept
    let mut ord_def = make(1, "Scanning", "exploration");
    assert_eq!(ord_def.effective_order(), 1);
    ord_def.order = Some(0);
    ord_def.uuid[15] = 42;
    assert_eq!(ord_def.effective_order(), 42);
    ord_def.order = Some(9);
    assert_eq!(ord_def.effective_order(), 9);

    // Aura text ID fallback on None / Some(0)
    let mut aura_def = make(1, "Scanning", "exploration");
    assert_ne!(aura_def.effective_aura_text_id(), 0);
    aura_def.aura_text_id = Some(0);
    assert_ne!(aura_def.effective_aura_text_id(), 0);
    aura_def.aura_text_id = Some(777_001);
    assert_eq!(aura_def.effective_aura_text_id(), 777_001);

    // Group normalization, whitespace collapsing, case matching, career_id clamping
    assert_eq!(make(1, "  Gas   Sites  ", "").effective_group(), "GasSites");
    assert_eq!(
        make(1, "", "Exploration").effective_group(),
        make(1, "", "exploration").effective_group()
    );
    assert_eq!(
        make(0, "Other", "other").effective_group(),
        make(1, "Other", "other").effective_group()
    );
    assert_eq!(
        make(5, "Social", "social").effective_group(),
        make(4, "Social", "social").effective_group()
    );

    // Exact stale synthesized loc_id range boundaries on both name_message_id and description_message_id
    for (lo_out, lo_in, hi_in, hi_out) in [
        (582_529u32, 582_530u32, 582_690u32, 582_691u32),
        (665_434u32, 665_435u32, 665_600u32, 665_601u32),
        (696_259u32, 696_260u32, 696_450u32, 696_451u32),
    ] {
        let mut d_out = make(1, "Scanning", "exploration");
        d_out.aura_text_id = Some(1);
        d_out.name_message_id = lo_out;
        d_out.description_message_id = hi_out;
        assert!(!is_stale_synthesized_loc_id(&d_out), "out {lo_out}/{hi_out}");

        let mut d_n_lo = make(1, "Scanning", "exploration");
        d_n_lo.name_message_id = lo_in;
        assert!(is_stale_synthesized_loc_id(&d_n_lo), "name lo_in={lo_in}");

        let mut d_n_hi = make(1, "Scanning", "exploration");
        d_n_hi.name_message_id = hi_in;
        assert!(is_stale_synthesized_loc_id(&d_n_hi), "name hi_in={hi_in}");

        let mut d_d_lo = make(1, "Scanning", "exploration");
        d_d_lo.description_message_id = lo_in;
        assert!(is_stale_synthesized_loc_id(&d_d_lo), "desc lo_in={lo_in}");

        let mut d_d_hi = make(1, "Scanning", "exploration");
        d_d_hi.description_message_id = hi_in;
        assert!(is_stale_synthesized_loc_id(&d_d_hi), "desc hi_in={hi_in}");
    }
}

#[test]
fn career_goal_db_reentrancy_rehydrate_and_milestone_sources() {
    use eve_db::{NpeProgressRecord, WorldRepository};
    use std::collections::{HashMap, HashSet};
    use std::sync::Arc;

    let db = Arc::new(WorldRepository::with_demo_seed());
    let mgr = CareerGoalManager::new(Some(Arc::clone(&db)));
    let char_id = 910_101u32;

    // rehydrate_from_db with no persisted state is a clean no-op
    let before_count = mgr.load_definitions().len();
    mgr.rehydrate_from_db();
    assert_eq!(mgr.load_definitions().len(), before_count);

    // Re-entrant save_to_db inside with_mut_snapshot must not deadlock or panic
    db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id as i64;
        ch.user_id = char_id as u64;
        snap.characters.insert(char_id as i64, ch);
        mgr.save_to_db();
    });

    // Insert NpeProgressRecord with valid milestones (2, 3) plus invalid (999)
    db.with_mut_snapshot(|snap| {
        snap.npe_progress.insert(
            char_id as i64,
            NpeProgressRecord {
                character_id: char_id as i64,
                node_graph_id: 473,
                current_node_id: 1,
                completed: false,
                career_goal_progress: HashMap::new(),
                claimed_goals: HashSet::from([2u32, 3u32, 999u32]),
            },
        );
    });

    // Rehydrate merges valid NPE milestones (2, 3) and filters invalid (999) from claimed_milestones cache
    let mgr2 = CareerGoalManager::new(Some(Arc::clone(&db)));
    mgr2.rehydrate_from_db();
    let rehydrated = mgr2
        .claimed_milestones
        .read()
        .unwrap()
        .get(&char_id)
        .cloned()
        .unwrap_or_default();
    assert!(!rehydrated.contains(&1));
    assert!(rehydrated.contains(&2));
    assert!(rehydrated.contains(&3));
    assert!(!rehydrated.contains(&999));
    db.with_snapshot(|snap| {
        assert!(!mgr2.is_milestone_claimed(char_id, 1, snap));
        assert!(mgr2.is_milestone_claimed(char_id, 2, snap));
        assert!(mgr2.is_milestone_claimed(char_id, 3, snap));
        // Still falls back to live npe_progress in snapshot
        assert!(mgr2.is_milestone_claimed(char_id, 999, snap));
    });

    // Only in-memory claim without DB update also satisfies is_milestone_claimed
    {
        let mut ms = mgr2.claimed_milestones.write().unwrap();
        ms.entry(char_id).or_default().insert(1);
    }
    db.with_snapshot(|snap| {
        assert!(mgr2.is_milestone_claimed(char_id, 1, snap));
    });

    // Rehydrating again discards unsaved in-memory claim 1 while retaining persisted 2, 3
    mgr2.rehydrate_from_db();
    db.with_snapshot(|snap| {
        assert!(!mgr2.is_milestone_claimed(char_id, 1, snap));
        assert!(mgr2.is_milestone_claimed(char_id, 2, snap));
    });

    // Also test in_blob source cleanly without NPE row
    let blob_cid = 910_102u32;
    {
        let mut ms = mgr2.claimed_milestones.write().unwrap();
        ms.entry(blob_cid).or_default().insert(4);
    }
    mgr2.save_to_db();
    let mgr3 = CareerGoalManager::new(Some(Arc::clone(&db)));
    db.with_snapshot(|snap| {
        assert!(mgr3.is_milestone_claimed(blob_cid, 4, snap));
    });
}

#[test]
fn career_goal_events_zero_reward_rollback_and_milestone_thresholds() {
    use crate::quasar_gateway::PublicGatewayRouter;
    use eve_db::{IskCents, WorldRepository};
    use eve_proto::quasar::QuasarDescriptorRegistry;
    use eve_sde::SdeRegistry;
    use std::sync::Arc;

    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let gw = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let mgr = CareerGoalManager::new(Some(Arc::clone(&db)));
    let char_id = 910_202u32;
    let char_no_row = 910_203u32;

    let make_custom = |tag: u8, kind: &str, career_id: u32, target: u64, isk: i64, sp: f64, cp: u32| CareerGoalDef {
        uuid: [tag; 16],
        career_id,
        activity_name: "Goal".into(),
        activity_kind: kind.into(),
        name: format!("Custom {tag}"),
        description: "Desc".into(),
        target,
        career_points: cp,
        isk_reward: isk,
        sp_reward: sp,
        name_message_id: 0,
        description_message_id: 0,
        threat: 1,
        ..Default::default()
    };

    let def_zero = make_custom(0xA1, "test.evt.zero", 1, 1, 0, 0.0, 10);
    let def_sp_only = make_custom(0xA2, "test.evt.sp", 1, 2, 0, 500.0, 10);
    let def_isk_only = make_custom(0xA3, "test.evt.isk", 1, 1, 25_000, 0.0, 10);
    let def_c2_749 = make_custom(0xA4, "test.evt.c2", 2, 1, 0, 0.0, 749);
    let def_c2_1 = make_custom(0xA5, "test.evt.c2_one", 2, 1, 0, 0.0, 1);

    for def in [&def_zero, &def_sp_only, &def_isk_only, &def_c2_749, &def_c2_1] {
        mgr.configure_definition(def.clone());
    }

    db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_id as i64;
        ch.user_id = char_id as u64;
        ch.balance = IskCents::from_isk(100);
        ch.free_skill_points = 0.0;
        snap.characters.insert(char_id as i64, ch);
    });
    let base_journal_len = db.with_snapshot(|snap| snap.wallet_journal.len());

    // Advance def_zero to completion -> pending_notices gets Progressed + Completed; repeat event skips completed goal
    gw.routing_cache.write().unwrap().entries.clear();
    mgr.on_gameplay_event(char_id, "test.evt.zero", 1, &gw);
    let first_notice_count = gw.routing_cache.read().unwrap().entries.len();
    assert!(first_notice_count >= 2);
    gw.pending_notices.write().unwrap().clear();
    mgr.on_gameplay_event(char_id, "test.evt.zero", 1, &gw);
    assert_eq!(
        gw.pending_notices.read().unwrap().len(),
        0,
        "already completed goal must not emit duplicate notices"
    );

    // Zero ISK + zero SP skips wallet & journal updates
    mgr.claim_reward(char_id, &def_zero.uuid, &db).unwrap();
    db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&(char_id as i64)].balance, IskCents::from_isk(100));
        assert_eq!(snap.wallet_journal.len(), base_journal_len);
    });

    // Advance & claim SP-only goal -> credits free SP without wallet journal entry
    mgr.on_gameplay_event(char_id, "test.evt.sp", 2, &gw);
    mgr.claim_reward(char_id, &def_sp_only.uuid, &db).unwrap();
    db.with_snapshot(|snap| {
        assert_eq!(snap.characters[&(char_id as i64)].balance, IskCents::from_isk(100));
        assert_eq!(snap.characters[&(char_id as i64)].free_skill_points, 500.0);
        assert_eq!(snap.wallet_journal.len(), base_journal_len);
    });

    // Rollback on DB credit failure: complete def_isk_only on char_no_row (absent from snap.characters)
    mgr.on_gameplay_event(char_no_row, "test.evt.isk", 1, &gw);
    let err = mgr.claim_reward(char_no_row, &def_isk_only.uuid, &db).unwrap_err();
    assert!(err.0 >= 400);
    let still_unclaimed = mgr
        .get_or_init_goals(char_no_row)
        .into_iter()
        .find(|g| g.uuid == def_isk_only.uuid)
        .unwrap()
        .claimed;
    assert!(!still_unclaimed, "claimed flag must roll back when wallet credit fails");

    // Inserting char_no_row afterwards lets retry succeed!
    db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = char_no_row as i64;
        ch.user_id = char_no_row as u64;
        ch.balance = IskCents::from_isk(10);
        snap.characters.insert(char_no_row as i64, ch);
    });
    mgr.claim_reward(char_no_row, &def_isk_only.uuid, &db).unwrap();
    db.with_snapshot(|snap| {
        assert_eq!(
            snap.characters[&(char_no_row as i64)].balance,
            IskCents::from_isk(25_010)
        );
    });

    // Milestone threshold boundaries: cp == 749 rejected (403), cp == 750 allowed; missing char -> 400
    let ms_char = 910_204u32;
    db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters.values().next().unwrap().clone();
        ch.character_id = ms_char as i64;
        ch.user_id = ms_char as u64;
        ch.balance = IskCents::from_isk(0);
        snap.characters.insert(ms_char as i64, ch);
    });

    assert_eq!(mgr.claim_milestone_reward(999_999, 2, &db).unwrap_err().0, 403);
    // Grant missing char 750 CP so threshold passes and missing character record returns 400
    mgr.on_gameplay_event(999_999, "test.evt.c2", 1, &gw);
    mgr.on_gameplay_event(999_999, "test.evt.c2_one", 1, &gw);
    assert_eq!(mgr.claim_milestone_reward(999_999, 2, &db).unwrap_err().0, 400);

    // Grant 749 CP in career 2 -> below 750 threshold
    mgr.on_gameplay_event(ms_char, "test.evt.c2", 1, &gw);
    assert_eq!(mgr.claim_milestone_reward(ms_char, 2, &db).unwrap_err().0, 403);
    assert_eq!(mgr.claim_milestone_reward(ms_char, 101, &db).unwrap_err().0, 403);

    // +1 CP crosses 750 -> both career 2 milestone and overall 101 succeed; re-claim blocked (403)
    mgr.on_gameplay_event(ms_char, "test.evt.c2_one", 1, &gw);
    mgr.claim_milestone_reward(ms_char, 2, &db).unwrap();
    mgr.claim_milestone_reward(ms_char, 101, &db).unwrap();
    assert_eq!(mgr.claim_milestone_reward(ms_char, 2, &db).unwrap_err().0, 403);
}

#[test]
fn career_goal_gateway_handlers_aliases_and_varint_edges() {
    use crate::quasar_gateway::PublicGatewayRouter;
    use eve_db::{service_roles, WorldRepository};
    use eve_proto::quasar::{GoogleAny, QuasarDescriptorRegistry, WireMessage, WireValue};
    use eve_sde::SdeRegistry;
    use std::sync::Arc;

    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let gw = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let mgr = Arc::new(CareerGoalManager::new(Some(Arc::clone(&db))));
    register_career_goal_handlers(&gw, Arc::clone(&db), Arc::clone(&mgr));

    let char_id = 910_303u32;
    let target_char = 90_910_304u32;
    db.with_mut_snapshot(|snap| {
        for cid in [char_id, target_char] {
            let mut ch = snap.characters.values().next().unwrap().clone();
            ch.character_id = cid as i64;
            ch.user_id = cid as u64;
            snap.characters.insert(cid as i64, ch);
        }
    });

    let dummy_any = GoogleAny {
        type_url: String::new(),
        value: Vec::new(),
    };
    let unauth = AuthoritativeContext::default();
    let user_ctx = AuthoritativeContext::for_session(
        Some(char_id as i32),
        Some(char_id),
        None,
        0,
        "tranquility",
        vec![1],
    );
    let admin_ctx = AuthoritativeContext::for_session(
        Some(char_id as i32),
        Some(char_id),
        None,
        0,
        "tranquility",
        vec![1],
    )
    .with_service_roles(service_roles::ROLE_ADMIN);

    let handlers = gw.handlers.read().unwrap();
    let empty_req = DynamicQuasarMessage::new("Req");
    assert_eq!(
        handlers["eve_public.career.goal.api.GetAllRequest"](
            &unauth,
            &dummy_any,
            &empty_req,
        )
        .unwrap_err()
        .0,
        403
    );
    assert_eq!(
        handlers["eve_public.career.goal.api.admin.ProgressRequest"](
            &user_ctx,
            &dummy_any,
            &empty_req,
        )
        .unwrap_err()
        .0,
        403
    );

    let gid = [0xB7u8; 16];
    mgr.configure_definition(CareerGoalDef {
        uuid: gid,
        career_id: 1,
        activity_name: "Scanning".into(),
        activity_kind: "test.gw.goal".into(),
        name: "GW Goal".into(),
        description: "Desc".into(),
        target: 10,
        career_points: 20,
        isk_reward: 500,
        sp_reward: 0.0,
        threat: 1,
        ..Default::default()
    });

    let mut id_msg = DynamicQuasarMessage::new("Identifier");
    id_msg.fields.insert(1, DynamicValue::Bytes(gid.to_vec()));

    let mut has_req = DynamicQuasarMessage::new("HasClaimed");
    has_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(id_msg.clone())));
    let has_before = handlers["eve.character.career.goal.HasClaimedRewardRequest"](
        &user_ctx,
        &dummy_any,
        &has_req,
    )
    .unwrap();
    let wire_before = WireMessage::decode(&has_before.value).unwrap();
    assert!(
        wire_before
            .fields
            .iter()
            .find(|f| f.field_number == 1)
            .is_none_or(|f| f.value == WireValue::Varint(0))
    );

    // admin.ProgressRequest error cases: delta == 0 and missing UUID
    let mut zero_delta_req = DynamicQuasarMessage::new("Progress");
    zero_delta_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(id_msg.clone())));
    zero_delta_req.fields.insert(2, DynamicValue::Uint64(0));
    assert_eq!(
        handlers["eve_public.career.goal.api.admin.ProgressRequest"](
            &admin_ctx,
            &dummy_any,
            &zero_delta_req,
        )
        .unwrap_err()
        .0,
        400
    );
    assert_eq!(
        handlers["eve_public.career.goal.api.admin.ProgressRequest"](
            &admin_ctx,
            &dummy_any,
            &DynamicQuasarMessage::new("Progress"),
        )
        .unwrap_err()
        .0,
        400
    );

    // admin.ProgressRequest across Uint64, Int64, Uint32, Int32 and target char field 3 (< 1_000_000 vs >= 1_000_000)
    for (idx, val) in [
        DynamicValue::Uint64(2),
        DynamicValue::Int64(2),
        DynamicValue::Uint32(3),
        DynamicValue::Int32(3),
    ]
    .into_iter()
    .enumerate()
    {
        let mut req = DynamicQuasarMessage::new("Progress");
        req.fields
            .insert(1, DynamicValue::Message(Box::new(id_msg.clone())));
        req.fields.insert(2, val);
        req.fields.insert(
            3,
            DynamicValue::Uint64(if idx == 0 { 12 } else { target_char as u64 }),
        );
        handlers["eve_public.career.goal.api.admin.ProgressRequest"](
            &admin_ctx,
            &dummy_any,
            &req,
        )
        .unwrap();
    }

    // Also add 2 more progress on target_char so total reaches 2+2+3+3 = 10 -> completed!
    // Wait: idx 1 (2) + idx 2 (3) + idx 3 (3) = 8; add one more 2 via requests alias or Uint64(2)
    let mut finish_req = DynamicQuasarMessage::new("Progress");
    finish_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(id_msg.clone())));
    finish_req.fields.insert(2, DynamicValue::Uint64(2));
    finish_req
        .fields
        .insert(3, DynamicValue::Uint64(target_char as u64));
    handlers["eve_public.career.goal.api.admin.ProgressRequest"](
        &admin_ctx,
        &dummy_any,
        &finish_req,
    )
    .unwrap();

    // Claim via app requests alias -> returns Identifier bytes and flips HasClaimedRewardRequest to varint 1
    let target_ctx = AuthoritativeContext::for_session(
        Some(target_char as i32),
        Some(target_char),
        None,
        0,
        "tranquility",
        vec![1],
    );
    let mut claim_req = DynamicQuasarMessage::new("Claim");
    claim_req
        .fields
        .insert(1, DynamicValue::Message(Box::new(id_msg.clone())));
    let claim_resp = handlers["eve_public.app.eveonline.career.goal.ClaimRewardRequest"](
        &target_ctx,
        &dummy_any,
        &claim_req,
    )
    .unwrap();
    assert!(!claim_resp.value.is_empty());

    let has_after = handlers["eve.character.career.goal.HasClaimedRewardRequest"](
        &target_ctx,
        &dummy_any,
        &has_req,
    )
    .unwrap();
    let wire_after = WireMessage::decode(&has_after.value).unwrap();
    assert_eq!(
        wire_after
            .fields
            .iter()
            .find(|f| f.field_number == 1)
            .map(|f| &f.value),
        Some(&WireValue::Varint(1))
    );

    // Direct check for `sync_def_localization_from` and `ensure_character_career_goals_with_defs`
    let mut stale_def = CareerGoalDef {
        uuid: gid,
        name_message_id: 0,
        description_message_id: 0,
        aura_text_id: None,
        ..Default::default()
    };
    let fresh_def = CareerGoalDef {
        uuid: gid,
        activity_kind: "test.gw.goal".into(),
        name_message_id: 700_001,
        description_message_id: 700_002,
        aura_text_id: Some(700_003),
        agency_link_text_id: Some(700_004),
        agency_screen: Some("exploration".into()),
        duration: Some(15),
        order: Some(3),
        ..Default::default()
    };
    let mut char_goals = vec![CharacterCareerGoal {
        uuid: gid,
        def: stale_def.clone(),
        progress: 2,
        completed: false,
        claimed: false,
        completed_time: None,
    }];
    super::ensure_character_career_goals_with_defs(&mut char_goals, &[fresh_def.clone()]);
    assert_eq!(char_goals[0].def.activity_kind, "test.gw.goal");
    assert_eq!(char_goals[0].def.name_message_id, 700_001);
    assert_eq!(char_goals[0].def.aura_text_id, Some(700_003));
    super::sync_def_localization_from(&mut stale_def, &fresh_def);
    assert_eq!(stale_def.description_message_id, 700_002);
}

#[test]
fn derived_wiki_career_points_and_objective_predicates() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let mgr = Arc::new(CareerGoalManager::new_with_sde(
        Some(Arc::clone(&ctx.db)),
        Some(Arc::clone(&ctx.sde)),
    ));
    let defs = mgr.load_definitions();
    assert_eq!(defs.len(), 188);

    // Every career path sums to 1,000 CP and 2,500,000 ISK (`5..=50 CP`, `ISK = CP * 2,500`).
    for cid in 1..=4 {
        let career_defs: Vec<&CareerGoalDef> =
            defs.iter().filter(|d| d.career_id == cid).collect();
        let cp_sum: u32 = career_defs.iter().map(|d| d.career_points).sum();
        let isk_sum: i64 = career_defs.iter().map(|d| d.isk_reward).sum();
        assert_eq!(cp_sum, 1_000, "career {cid} CP sum mismatch");
        assert_eq!(isk_sum, 2_500_000, "career {cid} ISK sum mismatch");
        for d in &career_defs {
            assert!(
                (5..=50).contains(&d.career_points),
                "unexpected CP {} on {}",
                d.career_points,
                d.goal_label
            );
            assert_eq!(d.isk_reward, i64::from(d.career_points) * 2_500);
            assert_eq!(
                super::derived_wiki_career_goal_points(&d.goal_label),
                Some(d.career_points),
                "missing wiki CP mapping for {}",
                d.goal_label
            );
        }
    }

    // Standings & Career Agent mission targets parsed cleanly.
    let by_label = |lbl: &str| -> CareerGoalDef {
        defs.iter()
            .find(|d| d.goal_label == lbl)
            .unwrap_or_else(|| panic!("missing goal {lbl}"))
            .clone()
    };
    assert_eq!(by_label("EnforcerGetStandingTo01").target, 10);
    assert_eq!(by_label("EnforcerGetStandingTo025").target, 25);
    assert_eq!(by_label("EnforcerGetStandingTo05").target, 50);
    assert_eq!(by_label("EnforcerGetStandingToMissionsLevel2").target, 100);
    assert_eq!(by_label("EnforcerCareerAgentMission8").target, 1);
    assert_eq!(by_label("IndustrialistProducerCareerAgentMission10").target, 1);

    let ch = 90_000_001_u32;
    let prog_of = |lbl: &str| -> u64 {
        mgr.get_or_init_goals(ch)
            .into_iter()
            .find(|g| g.def.goal_label == lbl)
            .unwrap()
            .progress
    };

    // 1. Ore filter: Veldspar (group 462) only advances IndustrialistMineVeldspar.
    mgr.on_career_gameplay_event(
        ch,
        super::CareerGameplayEvent {
            activity_kind: "mining",
            group_id: Some(462),
            ..Default::default()
        },
        120,
        &ctx.quasar_gateway,
    );
    assert_eq!(prog_of("IndustrialistMineVeldspar"), 120);
    assert_eq!(prog_of("IndustrialistMineScordite"), 0);
    assert_eq!(prog_of("IndustrialistMineKernite"), 0);

    // 2. Manufacturing ship hull filter: Frigate (group 25) only advances Frigate.
    mgr.on_career_gameplay_event(
        ch,
        super::CareerGameplayEvent {
            activity_kind: "manufacturing",
            group_id: Some(25),
            ..Default::default()
        },
        1,
        &ctx.quasar_gateway,
    );
    assert_eq!(prog_of("IndustrialistManufactureFrigate"), 1);
    assert_eq!(prog_of("IndustrialistManufactureShuttle"), 0);
    assert_eq!(prog_of("IndustrialistManufactureDestroyer"), 0);
    assert_eq!(prog_of("IndustrialistManufactureCruiser"), 0);

    // 3. Ship-class NPC kill filter: Cruiser kill advances Enforcer + Cruiser, NOT Frigate/Destroyer.
    mgr.on_career_gameplay_event(
        ch,
        super::CareerGameplayEvent {
            activity_kind: "combat",
            ship_group_id: Some(26),
            ..Default::default()
        },
        1,
        &ctx.quasar_gateway,
    );
    assert_eq!(prog_of("EnforcerDestroyNpc100"), 1);
    assert_eq!(prog_of("SofDestroyNpcInCruiser"), 1);
    assert_eq!(prog_of("SofDestroyNpcInFrigate"), 0);
    assert_eq!(prog_of("SofDestroyNpcInDestroyer"), 0);

    // 4. Distinct region + sec band filter for ExplorerVisitHisec.
    for rid in [10_000_002_u32, 10_000_002, 10_000_030, 10_000_043] {
        mgr.on_career_gameplay_event(
            ch,
            super::CareerGameplayEvent {
                activity_kind: "advanced_travel",
                region_id: Some(rid),
                security_status: Some(0.9),
                is_wormhole: Some(false),
                ..Default::default()
            },
            1,
            &ctx.quasar_gateway,
        );
    }
    assert_eq!(prog_of("ExplorerVisitHisec"), 3, "duplicate region 10_000_002 must count once");
    assert_eq!(prog_of("ExplorerVisitLowsec"), 0);
    assert_eq!(prog_of("ExplorerVisitNullsec"), 0);

    // 5. Career Agent mission (division + ordinal) & standings threshold & market sub-kind.
    mgr.on_career_gameplay_event(
        ch,
        super::CareerGameplayEvent {
            activity_kind: "career_missions",
            mission_division: Some("enforcer"),
            mission_ordinal: Some(4),
            ..Default::default()
        },
        1,
        &ctx.quasar_gateway,
    );
    assert_eq!(prog_of("EnforcerCareerAgentMission4"), 1);
    assert_eq!(prog_of("EnforcerCareerAgentMission1"), 0);
    assert_eq!(prog_of("SofCareerAgentMission4"), 0);

    mgr.on_career_gameplay_event(
        ch,
        super::CareerGameplayEvent {
            activity_kind: "standings",
            standing_bps: Some(30),
            ..Default::default()
        },
        1,
        &ctx.quasar_gateway,
    );
    assert_eq!(prog_of("EnforcerGetStandingTo01"), 10);
    assert_eq!(prog_of("EnforcerGetStandingTo025"), 25);
    assert_eq!(prog_of("EnforcerGetStandingTo05"), 30);
    assert_eq!(prog_of("EnforcerGetStandingToMissionsLevel2"), 30);

    mgr.on_career_gameplay_event(
        ch,
        super::CareerGameplayEvent {
            activity_kind: "market",
            sub_kind: Some("spend_isk"),
            ..Default::default()
        },
        50_000,
        &ctx.quasar_gateway,
    );
    assert_eq!(prog_of("EnforcerSpendMarketIsk50k"), 50_000);
    assert_eq!(prog_of("IndustrialistEarnMarketIsk50k"), 0);

    // Reloading from DB preserves seen_regions idempotency.
    mgr.rehydrate_from_db();
    mgr.on_career_gameplay_event(
        ch,
        super::CareerGameplayEvent {
            activity_kind: "advanced_travel",
            region_id: Some(10_000_002),
            security_status: Some(0.9),
            is_wormhole: Some(false),
            ..Default::default()
        },
        1,
        &ctx.quasar_gateway,
    );
    assert_eq!(prog_of("ExplorerVisitHisec"), 3, "rehydrated seen_regions must stay deduplicated");
}

#[test]
fn career_dispatcher_helpers_cover_market_lp_standings_missions_combat_and_sites() {
    let (ctx, _) = test_ctx_and_clock(1_700_000_000);
    let buyer_id = 91_000_001_i64;
    let seller_id = 91_000_002_i64;
    let buyer_ship_id = 91_100_001_i64;
    let seller_ship_id = 91_100_002_i64;
    let pod_id = 91_100_003_i64;
    let npc_ship_id = 91_100_004_i64;

    let npc_corp_id = *ctx.sde.npc_corporations.keys().next().expect("SDE NPC corp");
    let faction_id = *ctx.sde.factions.keys().next().expect("SDE faction") as i64;
    let bpo_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.category_id == 9)
        .expect("SDE blueprint type")
        .type_id;
    let explorer_career_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 12 && a.division_id == 26)
        .expect("Explorer career agent")
        .agent_id;
    let producer_career_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 12 && a.division_id == 27)
        .expect("Producer career agent")
        .agent_id;
    let epic_arc_agent = ctx
        .sde
        .agents
        .values()
        .filter(|a| a.agent_type_id == 10)
        .max_by_key(|a| (a.level, a.agent_id))
        .expect("Epic Arc agent")
        .agent_id;
    let l1_sec_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 2 && a.level == 1 && a.division_id == 24)
        .expect("L1 security agent")
        .agent_id;
    let dist_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 2 && a.division_id == 22)
        .expect("Distribution agent")
        .agent_id;
    let mining_agent = ctx
        .sde
        .agents
        .values()
        .find(|a| a.agent_type_id == 2 && a.division_id == 23)
        .expect("Mining agent")
        .agent_id;

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, sid) in [(buyer_id, buyer_ship_id), (seller_id, seller_ship_id)] {
            snap.characters.insert(
                cid,
                eve_db::CharacterRecord {
                    character_id: cid,
                    user_id: cid as u64,
                    name: format!("Pilot_{cid}"),
                    corporation_id: 98_000_001,
                    alliance_id: None,
                    war_faction_id: None,
                    station_id: None,
                    structure_id: None,
                    solar_system_id: 30_000_142,
                    constellation_id: 20_000_020,
                    region_id: 10_000_002,
                    active_ship_id: sid,
                    balance: eve_db::IskCents(1_000_000_00),
                    plex_balance: 0,
                    loyalty_points: HashMap::new(),
                    standings: HashMap::new(),
                    security_status: 5.0,
                    clone_grade: 1,
                    home_station_id: 60_003_760,
                    race_id: 1,
                    bloodline_id: 1,
                    ancestry_id: 11,
                    school_id: 17,
                    neural: eve_db::NeuralAttributes::default(),
                    skill_points: 0.0,
                    activity_status: None,
                    free_skill_points: 0.0,
                    last_clone_jump_unix: 0,
                    jump_fatigue_minutes: 0.0,
                    jump_cooldown_until_unix: 0,
                    jump_fatigue_until_unix: 0,
                    description: String::new(),
                    title: String::new(),
                    gender: 0,
                    create_date_unix: 1_700_000_000,
                    fleet_id: None,
                    autopilot_waypoints: Vec::new(),
                    version: 1,
                    system_epoch: 1,
                },
            );
        }
        snap.items.insert(
            buyer_ship_id,
            eve_db::InventoryItemRecord {
                item_id: buyer_ship_id,
                type_id: 587,
                owner_id: buyer_id,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            seller_ship_id,
            eve_db::InventoryItemRecord {
                item_id: seller_ship_id,
                type_id: 587,
                owner_id: seller_id,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            pod_id,
            eve_db::InventoryItemRecord {
                item_id: pod_id,
                type_id: 670,
                owner_id: seller_id,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        snap.items.insert(
            npc_ship_id,
            eve_db::InventoryItemRecord {
                item_id: npc_ship_id,
                type_id: 587,
                owner_id: 1_000_001,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    assert_eq!(isk_cents_to_whole_isk(eve_db::IskCents(0)), 0);
    assert_eq!(isk_cents_to_whole_isk(eve_db::IskCents(-500)), 0);
    assert_eq!(isk_cents_to_whole_isk(eve_db::IskCents(5_000_050)), 50_001);
    assert_eq!(parse_career_ordinal_from_title("Balancing the Books (2 of 10)"), Some((2, 10)));
    assert_eq!(parse_career_ordinal_from_title("No parens"), None);
    assert_eq!(career_division_key(26), Some("explorer"));
    assert_eq!(career_division_key(29), Some("soldier_of_fortune"));
    assert_eq!(ewar_sub_kind_from_jam_type("warpScramblerMWD"), Some("warp_scramble"));
    assert_eq!(ewar_sub_kind_from_jam_type("ewEnergyNeut"), None);
    assert!(is_combat_site_category("WormholeCombat"));
    assert!(!is_combat_site_category("Relic"));

    let prog_of = |cid: i64, lbl: &str| -> u64 {
        ctx.quasar_gateway
            .progression
            .career_goals
            .get_or_init_goals(cid as u32)
            .into_iter()
            .find(|g| g.def.goal_label == lbl)
            .unwrap_or_else(|| panic!("missing goal {lbl}"))
            .progress
    };

    // Market fills convert IskCents to whole ISK and record buyer Blueprint acquisitions.
    record_market_fill_career_goals(&ctx, buyer_id, seller_id, bpo_tid, eve_db::IskCents(5_000_000));
    assert_eq!(prog_of(buyer_id, "EnforcerSpendMarketIsk50k"), 50_000);
    assert_eq!(prog_of(buyer_id, "IndustrialistBuyBlueprint"), 1);
    assert_eq!(prog_of(seller_id, "IndustrialistEarnMarketIsk50k"), 50_000);

    // LP & Bounties & Corporation-only Standings.
    record_lp_career_event(&ctx, buyer_id, false, 150);
    record_lp_career_event(&ctx, buyer_id, true, 300);
    record_bounty_career_event(&ctx, buyer_id, 25_000);
    record_standing_career_event(&ctx, buyer_id, faction_id, 5.0);
    assert_eq!(prog_of(buyer_id, "EnforcerGetStandingTo025"), 0, "faction standing ignored");
    record_standing_career_event(&ctx, buyer_id, npc_corp_id, 0.3);
    assert_eq!(prog_of(buyer_id, "EnforcerEarnLp250"), 150);
    assert_eq!(prog_of(buyer_id, "EnforcerSpendLp300"), 300);
    assert_eq!(prog_of(buyer_id, "EnforcerGetStandingTo025"), 25);
    assert_eq!(prog_of(buyer_id, "EnforcerGetStandingTo05"), 30);

    // Missions: Career Agent (10-part + 5-part scaled), Epic Arc step + complete, Dist/Mining/Security.
    record_mission_completion_career_goals(
        &ctx, buyer_id, explorer_career_agent, "Cash Flow (3 of 10)",
        false, false, false, false, false, 0,
    );
    record_mission_completion_career_goals(
        &ctx, buyer_id, producer_career_agent, "Make It Work (2 of 5)",
        false, false, false, false, false, 0,
    );
    record_mission_completion_career_goals(
        &ctx, buyer_id, epic_arc_agent, "Blood-Stained Stars",
        false, false, true, false, true, 0,
    );
    record_mission_completion_career_goals(
        &ctx, buyer_id, dist_agent, "Haul Cargo", true, false, false, false, false, 0,
    );
    record_mission_completion_career_goals(
        &ctx, buyer_id, mining_agent, "Mine Plagioclase", false, true, false, false, false, 0,
    );
    record_mission_completion_career_goals(
        &ctx, buyer_id, l1_sec_agent, "Pirates!", false, false, true, false, false, 200,
    );
    assert_eq!(prog_of(buyer_id, "ExplorerCareerAgentMission3"), 1);
    assert_eq!(prog_of(buyer_id, "IndustrialistProducerCareerAgentMission4"), 1);
    assert_eq!(prog_of(buyer_id, "EnforcerEpicArcMissions1"), 1);
    assert_eq!(prog_of(buyer_id, "EnforcerEpicArcComplete"), 1);

    // EWAR / Cap Warfare / Remote Repair capsuleer & self guards.
    let genolution_pod_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 29 && t.type_id != 670)
        .expect("SDE non-default capsule type in group 29")
        .type_id;
    let genolution_pod_id = 91_100_005_i64;
    let corp_ship_id = 91_100_006_i64;
    let third_pilot_id = 91_000_003_i64;
    ctx.db.with_mut_snapshot(|snap| {
        snap.items.insert(
            genolution_pod_id,
            eve_db::InventoryItemRecord {
                item_id: genolution_pod_id,
                type_id: genolution_pod_tid,
                owner_id: seller_id,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
        // Corp-owned ship piloted by third_pilot_id via active_ship_id.
        let mut ch3 = snap.characters[&seller_id].clone();
        ch3.character_id = third_pilot_id;
        ch3.user_id = third_pilot_id as u64;
        ch3.name = "CorpShipPilot".into();
        ch3.active_ship_id = corp_ship_id;
        snap.characters.insert(third_pilot_id, ch3);
        snap.items.insert(
            corp_ship_id,
            eve_db::InventoryItemRecord {
                item_id: corp_ship_id,
                type_id: 587,
                owner_id: 98_000_001,
                location_id: 30_000_142,
                flag: eve_db::InventoryFlag::None,
                quantity: 1,
                singleton: true,
                version: 1,
                dynamic_attributes: HashMap::new(),
                source_mutaplasmid_id: None,
                insured_payout_isk: 0.0,
                is_blueprint_copy: false,
                blueprint_runs_remaining: None,
            },
        );
    });

    assert!(is_capsuleer_ship_ball_target(&ctx.db, &ctx.sde, seller_ship_id, buyer_id));
    assert!(is_capsuleer_ship_ball_target(&ctx.db, &ctx.sde, corp_ship_id, buyer_id));
    assert!(!is_capsuleer_ship_ball_target(&ctx.db, &ctx.sde, buyer_ship_id, buyer_id));
    assert!(!is_capsuleer_ship_ball_target(&ctx.db, &ctx.sde, pod_id, buyer_id));
    assert!(!is_capsuleer_ship_ball_target(&ctx.db, &ctx.sde, genolution_pod_id, buyer_id));
    assert!(!is_capsuleer_ship_ball_target(&ctx.db, &ctx.sde, npc_ship_id, buyer_id));

    record_ewar_career_seconds(&ctx, buyer_id, seller_ship_id, "webify", 10.0);
    record_ewar_career_seconds(&ctx, buyer_id, npc_ship_id, "webify", 50.0);
    record_ewar_career_seconds(&ctx, buyer_id, pod_id, "webify", 50.0);
    record_ewar_career_seconds(&ctx, buyer_id, genolution_pod_id, "webify", 50.0);
    assert_eq!(prog_of(buyer_id, "SofWebify"), 10);

    // NPC, capsules (670 and variant), self, and zero GJ MUST NOT progress cap warfare.
    record_cap_warfare_career(&ctx, buyer_id, buyer_ship_id, npc_ship_id, "neutralizer", 90.0);
    record_cap_warfare_career(&ctx, buyer_id, buyer_ship_id, npc_ship_id, "nosferatu", 80.0);
    record_cap_warfare_career(&ctx, buyer_id, buyer_ship_id, pod_id, "neutralizer", 90.0);
    record_cap_warfare_career(&ctx, buyer_id, buyer_ship_id, genolution_pod_id, "neutralizer", 90.0);
    record_cap_warfare_career(&ctx, buyer_id, buyer_ship_id, buyer_ship_id, "neutralizer", 90.0);
    record_cap_warfare_career(&ctx, buyer_id, buyer_ship_id, seller_ship_id, "neutralizer", 0.0);
    assert_eq!(prog_of(buyer_id, "SofNeutralizeCapacitor100"), 0);
    assert_eq!(prog_of(buyer_id, "SofDrainCapacitor50"), 0);

    // Live capsuleer ship cap warfare (personal + corp-owned active ship) increments cleanly.
    record_cap_warfare_career(&ctx, buyer_id, buyer_ship_id, seller_ship_id, "nosferatu", 80.0);
    record_cap_warfare_career(&ctx, buyer_id, buyer_ship_id, seller_ship_id, "neutralizer", 80.0);
    record_cap_warfare_career(&ctx, buyer_id, buyer_ship_id, corp_ship_id, "neutralizer", 10.0);
    assert_eq!(prog_of(buyer_id, "EnforcerEarnLp500"), 350);
    assert_eq!(prog_of(buyer_id, "SofNeutralizeCapacitor100"), 90);

    let all_buyer_goals = ctx
        .quasar_gateway
        .progression
        .career_goals
        .get_or_init_goals(buyer_id as u32);
    let max_prog = |kind: &str| -> u64 {
        all_buyer_goals
            .iter()
            .filter(|g| g.def.activity_kind == kind)
            .map(|g| g.progress)
            .max()
            .unwrap_or(0)
    };
    assert_eq!(max_prog("bounties"), 25_000);
    let ea = &ctx.sde.agents[&epic_arc_agent];
    let ea_l1_dist = u64::from(ea.level == 1 && ea.division_id == 22);
    let ea_l1_mine = u64::from(ea.level == 1 && ea.division_id == 23);
    let ea_l1_comb = u64::from(ea.level == 1 && ea.division_id != 22 && ea.division_id != 23);
    assert_eq!(max_prog("distribution_missions"), 1 + ea_l1_dist);
    assert_eq!(max_prog("mining_missions"), 1 + ea_l1_mine);
    assert_eq!(max_prog("combat_missions"), 1 + ea_l1_comb);
    assert!(all_buyer_goals
        .iter()
        .any(|g| g.def.goal_label.starts_with("SofDrainCapacitor") && g.progress == 80));
    assert!(all_buyer_goals
        .iter()
        .any(|g| g.def.goal_label.starts_with("SofNeutralizeCapacitor") && g.progress == 90));

    record_remote_repair_career(&ctx, buyer_id, seller_ship_id, "armor_repair", 125.0);
    record_remote_repair_career(&ctx, buyer_id, buyer_ship_id, "armor_repair", 200.0);
    record_remote_repair_career(&ctx, buyer_id, npc_ship_id, "shield_boost", 200.0);
    let rep_goals = ctx
        .quasar_gateway
        .progression
        .career_goals
        .get_or_init_goals(buyer_id as u32);
    assert!(rep_goals
        .iter()
        .any(|g| g.def.goal_label.starts_with("SofRemoteRepArmorHp") && g.progress == 125));
    assert!(rep_goals
        .iter()
        .filter(|g| g.def.goal_label.starts_with("SofRemoteBoostShieldHp"))
        .all(|g| g.progress == 0));

    // Site entry, deduplication, Gas Training Site & starter-system exclusions, and combat PvP/NPC hull split.
    let starter_sys = ctx
        .sde
        .schools
        .get(&11)
        .expect("SDE school 11 starter system")
        .solar_system_id;
    let non_starter_sys = 30_000_142_u32; // Jita (school 23, not starter 11..=22)
    assert!(is_career_starter_system(&ctx.sde, starter_sys));
    assert!(!is_career_starter_system(&ctx.sde, non_starter_sys));

    record_site_entry_career_goals(&ctx, buyer_id, starter_sys, "STARTER-ANOM", None, None, "CombatAnomaly");
    record_combat_site_completion_in_system(&ctx.quasar_gateway, &ctx.sde, starter_sys, buyer_id, "CombatAnomaly");
    record_site_entry_career_goals(&ctx, buyer_id, non_starter_sys, "GAS-TRAIN-1", Some(4093), Some("Gas Training Site"), "Gas");
    assert_eq!(prog_of(buyer_id, "ExplorerEnterCombatSites1"), 0);
    assert_eq!(prog_of(buyer_id, "EnforcerCompleteCombatSites5"), 0);
    assert_eq!(prog_of(buyer_id, "ExplorerEnterGasSites1"), 0);

    record_site_entry_career_goals(&ctx, buyer_id, non_starter_sys, "ANOM-1", None, None, "CombatAnomaly");
    record_site_entry_career_goals(&ctx, buyer_id, non_starter_sys, "ANOM-1", None, None, "CombatAnomaly");
    record_site_entry_career_goals(&ctx, buyer_id, non_starter_sys, "GAS-1", None, Some("Ordinary Perimeter Reservoir"), "WormholeGas");
    record_site_entry_career_goals(&ctx, buyer_id, non_starter_sys, "GAS-1", None, Some("Ordinary Perimeter Reservoir"), "WormholeGas");
    record_combat_site_completion_in_system(&ctx.quasar_gateway, &ctx.sde, non_starter_sys, buyer_id, "WormholeCombat");
    assert_eq!(prog_of(buyer_id, "ExplorerEnterCombatSites1"), 1);
    assert_eq!(prog_of(buyer_id, "ExplorerEnterGasSites1"), 1);
    assert_eq!(prog_of(buyer_id, "EnforcerCompleteCombatSites5"), 1);

    let enforcer_before = prog_of(buyer_id, "EnforcerDestroyNpc100");
    ctx.quasar_gateway.progression.career_goals.on_career_gameplay_event(
        buyer_id as u32,
        super::CareerGameplayEvent {
            activity_kind: "combat",
            ship_group_id: Some(25),
            sub_kind: Some("pvp_kill"),
            ..Default::default()
        },
        1,
        &ctx.quasar_gateway,
    );
    assert_eq!(prog_of(buyer_id, "SofDestroyNpcInFrigate"), 1);
    assert_eq!(prog_of(buyer_id, "EnforcerDestroyNpc100"), enforcer_before);

    // Non-FW killer killing an FW pilot in a non-capsule ship credits SofKillFwCapsuleeer1 & Frigate hull kill (not capsule / Enforcer NPC).
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&seller_id) {
            ch.war_faction_id = Some(500_001);
        }
    });
    crate::services::fw_gameplay::fw_gameplay_rpc::on_player_ship_destroyed_in_combat_detailed(
        &ctx,
        non_starter_sys,
        buyer_id,
        seller_id,
        genolution_pod_tid,
        0.0,
        0,
    );
    assert_eq!(prog_of(buyer_id, "SofKillFwCapsuleeer1"), 0);
    crate::services::fw_gameplay::fw_gameplay_rpc::on_player_ship_destroyed_in_combat_detailed(
        &ctx,
        non_starter_sys,
        buyer_id,
        seller_id,
        587,
        1_000_000.0,
        0,
    );
    assert_eq!(prog_of(buyer_id, "SofKillFwCapsuleeer1"), 1);
    assert_eq!(prog_of(buyer_id, "SofDestroyNpcInFrigate"), 2);
    assert_eq!(prog_of(buyer_id, "EnforcerDestroyNpc100"), enforcer_before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ewar_cycle_completion_and_deactivate_toggle_anti_exploit() {
    let (ctx, clock) = test_ctx_and_clock(1_800_000_000);
    let sys_id = 30_000_142_u32;
    let atk_cid = 92_100_001_i64;
    let def_cid = 92_100_002_i64;
    let atk_ship_id = 92_200_001_i64;
    let def_ship_id = 92_200_002_i64;
    let web_mod_id = 92_300_001_i64;

    let web_tid = ctx
        .sde
        .types
        .values()
        .filter(|t| {
            t.group_id == 65
                && t.published
                && t.default_attributes.get(&73).copied().unwrap_or(0.0) > 0.0
        })
        .min_by_key(|t| t.type_id)
        .expect("SDE published Stasis Webifier")
        .type_id;

    ctx.db.with_mut_snapshot(|snap| {
        let mut base = snap.characters.values().next().cloned().expect("seed char");
        base.station_id = None;
        base.structure_id = None;
        base.solar_system_id = sys_id;
        base.security_status = 5.0;
        base.clone_grade = 1;
        base.fleet_id = None;
        for (cid, sid) in [(atk_cid, atk_ship_id), (def_cid, def_ship_id)] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.name = format!("EwarPilot{cid}");
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
            snap.items.insert(
                sid,
                eve_db::InventoryItemRecord {
                    item_id: sid,
                    type_id: 587,
                    owner_id: cid,
                    location_id: sys_id as i64,
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
        snap.items.insert(
            web_mod_id,
            eve_db::InventoryItemRecord {
                item_id: web_mod_id,
                type_id: web_tid,
                owner_id: atk_cid,
                location_id: atk_ship_id,
                flag: eve_db::InventoryFlag::MedSlot0,
                quantity: 1,
                singleton: true,
                version: 1,
                ..Default::default()
            },
        );
        snap.module_online_states.insert(web_mod_id, true);
    });
    let _ = ctx
        .db
        .acquire_online_lock(atk_cid as u64, 1, Some(atk_cid), false, ctx.now_unix_secs());
    ctx.crimewatch
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_create(def_cid)
        .suspect_flag_until_unix = ctx.now_unix_secs() + 600;

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    for (cid, sid, x) in [(atk_cid, atk_ship_id, 0.0), (def_cid, def_ship_id, 1_000.0)] {
        sys_h
            .spawn_ball_with_meta(
                eve_destiny::Ball {
                    id: sid,
                    type_id: 587,
                    position: glam::DVec3::new(x, 0.0, 0.0),
                    velocity: glam::DVec3::ZERO,
                    max_velocity: 300.0,
                    inertia: 1.0,
                    mass: 1_067_000.0,
                    radius: 400.0,
                    speed_fraction: 0.0,
                    mode: eve_destiny::MovementMode::Stop,
                },
                ctx.build_ship_combat_state(cid, sid, 587),
                ctx.build_ship_slim_meta(cid, 587),
            )
            .await
            .expect("spawn ewar ball");
    }
    sys_h
        .add_target_lock(
            atk_ship_id,
            eve_destiny::TargetLock {
                target_id: def_ship_id,
                lock_time_ms: 100.0,
                remaining_lock_ms: 0.0,
                locked: true,
                max_range_m: None,
            },
        )
        .await
        .expect("lock def ship");

    let mut atk_sess = eve_machonet::SessionState::new(1);
    atk_sess.user_id = Some(atk_cid as u64);
    atk_sess.character_id = Some(atk_cid);
    atk_sess.corporation_id = Some(98_000_001);
    atk_sess.solar_system_id = Some(sys_id);
    atk_sess.ship_id = Some(atk_ship_id);
    let kw = HashMap::new();

    let prog_of = |lbl: &str| -> u64 {
        ctx.quasar_gateway
            .progression
            .career_goals
            .get_or_init_goals(atk_cid as u32)
            .into_iter()
            .find(|g| g.def.goal_label == lbl)
            .unwrap_or_else(|| panic!("missing goal {lbl}"))
            .progress
    };

    // 1. Activating webifier must NOT credit EWAR seconds upfront at t = 0.
    let act_rep = crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut atk_sess,
        None,
        "dogmaIM",
        "Activate",
        &[eve_proto::PyRep::Integer(web_mod_id), eve_proto::PyRep::Integer(def_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("activate webifier");
    let dur_s = act_rep
        .dict_get("duration")
        .and_then(eve_proto::PyRep::as_f64)
        .expect("webifier duration");
    assert!(dur_s > 0.0);
    assert_eq!(prog_of("SofWebify"), 0, "t=0 activate must not credit SofWebify");

    // 2. Immediate Deactivate at t = 0 stops the cycle cleanly with 0 EWAR progress,
    //    and rapid Activate -> Deactivate toggle spam must still grant 0 EWAR seconds.
    crate::services::inventory_fitting::fitting_mining_lasers::handle_mining_and_deactivate(
        &ctx,
        &mut atk_sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[eve_proto::PyRep::Integer(web_mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("deactivate webifier");
    assert_eq!(prog_of("SofWebify"), 0, "immediate deactivate must not credit SofWebify");
    assert!(ctx.db.with_snapshot(|s| !s.module_active_states.contains(&web_mod_id)));

    for _ in 0..3 {
        crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
            &ctx,
            &mut atk_sess,
            None,
            "dogmaIM",
            "Activate",
            &[eve_proto::PyRep::Integer(web_mod_id), eve_proto::PyRep::Integer(def_ship_id)],
            &kw,
        )
        .await
        .unwrap()
        .expect("re-activate webifier");
        crate::services::inventory_fitting::fitting_mining_lasers::handle_mining_and_deactivate(
            &ctx,
            &mut atk_sess,
            None,
            "dogmaIM",
            "Deactivate",
            &[eve_proto::PyRep::Integer(web_mod_id)],
            &kw,
        )
        .await
        .unwrap()
        .expect("immediate deactivate webifier");
    }
    sys_h.step_manual(1.0).await.expect("step after toggle spam");
    crate::services::inventory_fitting::fitting_module_cycles::execute_pending_cycle_actions(
        &ctx,
        sys_id,
    )
    .await;
    assert_eq!(prog_of("SofWebify"), 0, "rapid Activate/Deactivate toggling must grant 0 SofWebify");

    // 3. Re-activating and stepping across a full cycle credits EWAR seconds ONCE.
    crate::services::inventory_fitting::fitting_module_cycles::handle_module_cycles(
        &ctx,
        &mut atk_sess,
        None,
        "dogmaIM",
        "Activate",
        &[eve_proto::PyRep::Integer(web_mod_id), eve_proto::PyRep::Integer(def_ship_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("activate webifier for full cycle");
    clock.advance_secs_and_read_offset((dur_s.ceil() as u64) + 1);
    sys_h.step_manual(dur_s + 0.2).await.expect("step full ewar cycle");
    crate::services::inventory_fitting::fitting_module_cycles::execute_pending_cycle_actions(
        &ctx,
        sys_id,
    )
    .await;
    let expected = dur_s.round() as u64;
    assert_eq!(prog_of("SofWebify"), expected);

    // 4. Subsequent short steps must not double-count, and Deactivate cleans up active state.
    sys_h.step_manual(1.0).await.expect("extra step");
    crate::services::inventory_fitting::fitting_module_cycles::execute_pending_cycle_actions(
        &ctx,
        sys_id,
    )
    .await;
    assert_eq!(prog_of("SofWebify"), expected);

    crate::services::inventory_fitting::fitting_mining_lasers::handle_mining_and_deactivate(
        &ctx,
        &mut atk_sess,
        None,
        "dogmaIM",
        "Deactivate",
        &[eve_proto::PyRep::Integer(web_mod_id)],
        &kw,
    )
    .await
    .unwrap()
    .expect("deactivate after full cycle");
    assert!(ctx.db.with_snapshot(|s| !s.module_active_states.contains(&web_mod_id)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn destruction_spool_excludes_npcs_corvettes_and_variant_pods_and_credits_player_ships() {
    let (ctx, _) = test_ctx_and_clock(1_800_000_000);
    let sys_id = 30_000_142_u32;
    let atk_cid = 92_400_001_i64;
    let victim_cid = 92_400_002_i64;
    let atk_ship_id = 92_500_001_i64;
    let victim_frigate_id = 92_500_002_i64;
    let victim_variant_pod_id = 92_500_003_i64;
    let npc_id = 1_895_400_001_i64;

    let corvette_tid = ctx
        .sde
        .types
        .values()
        .find(|t| t.group_id == 237 && t.published)
        .expect("SDE corvette type")
        .type_id;
    let variant_pod_tid = 33_328_u32;
    assert_eq!(ctx.sde.types.get(&variant_pod_tid).map(|t| t.group_id), Some(29));

    ctx.db.with_mut_snapshot(|snap| {
        let mut base = snap.characters.values().next().cloned().expect("seed char");
        base.station_id = None;
        base.structure_id = None;
        base.solar_system_id = sys_id;
        base.clone_grade = 1;
        for (cid, sid) in [(atk_cid, atk_ship_id), (victim_cid, victim_frigate_id)] {
            let mut ch = base.clone();
            ch.character_id = cid;
            ch.user_id = cid as u64;
            ch.name = format!("DestrPilot{cid}");
            ch.active_ship_id = sid;
            snap.characters.insert(cid, ch);
        }
        for (item_id, tid, owner_id) in [
            (atk_ship_id, 587_u32, atk_cid),
            (victim_frigate_id, 587_u32, victim_cid),
            (victim_variant_pod_id, variant_pod_tid, victim_cid),
        ] {
            snap.items.insert(
                item_id,
                eve_db::InventoryItemRecord {
                    item_id,
                    type_id: tid,
                    owner_id,
                    location_id: sys_id as i64,
                    flag: eve_db::InventoryFlag::None,
                    quantity: 1,
                    singleton: true,
                    version: 1,
                    ..Default::default()
                },
            );
        }
    });

    let prog_of = |cid: i64, lbl: &str| -> u64 {
        ctx.quasar_gateway
            .progression
            .career_goals
            .get_or_init_goals(cid as u32)
            .into_iter()
            .find(|g| g.def.goal_label == lbl)
            .unwrap_or_else(|| panic!("missing goal {lbl}"))
            .progress
    };

    // 1. Direct helper excludes pods (670 and variant 33328) and corvettes (group 237).
    crate::services::notify_emitters_fleet_combat::emit_ship_death_with_type(&ctx, victim_cid, Some(670));
    crate::services::notify_emitters_fleet_combat::emit_ship_death_with_type(
        &ctx,
        victim_cid,
        Some(variant_pod_tid),
    );
    crate::services::notify_emitters_fleet_combat::emit_ship_death_with_type(
        &ctx,
        victim_cid,
        Some(corvette_tid),
    );
    assert_eq!(prog_of(victim_cid, "SofLoseShip1"), 0);

    let sys_h = ctx.systems.get_or_spawn(sys_id);
    let make_ball = |id: i64, type_id: u32, x: f64| eve_destiny::Ball {
        id,
        type_id,
        position: glam::DVec3::new(x, 0.0, 0.0),
        velocity: glam::DVec3::ZERO,
        max_velocity: 300.0,
        inertia: 1.0,
        mass: 1_067_000.0,
        radius: 100.0,
        speed_fraction: 0.0,
        mode: eve_destiny::MovementMode::Stop,
    };
    let npc_ball = make_ball(npc_id, 500_001, 500.0);
    let pod_ball = make_ball(victim_variant_pod_id, variant_pod_tid, 1_000.0);
    let frig_ball = make_ball(victim_frigate_id, 587, 1_500.0);
    sys_h
        .spawn_ball_with_meta(
            make_ball(atk_ship_id, 587, 0.0),
            ctx.build_ship_combat_state(atk_cid, atk_ship_id, 587),
            ctx.build_ship_slim_meta(atk_cid, 587),
        )
        .await
        .unwrap();
    sys_h.spawn_ball(npc_ball.clone()).await.unwrap();
    sys_h
        .spawn_ball_with_meta(
            pod_ball.clone(),
            ctx.build_ship_combat_state(victim_cid, victim_variant_pod_id, variant_pod_tid),
            ctx.build_ship_slim_meta(victim_cid, variant_pod_tid),
        )
        .await
        .unwrap();
    sys_h
        .spawn_ball_with_meta(
            frig_ball.clone(),
            ctx.build_ship_combat_state(victim_cid, victim_frigate_id, 587),
            ctx.build_ship_slim_meta(victim_cid, 587),
        )
        .await
        .unwrap();

    let now_unix = ctx.now_unix_secs();
    // 2. NPC destruction does not credit player ship loss ("destruction").
    crate::services::combat_crimewatch::combat_weapons_spool::handle_ship_or_npc_destruction(
        &ctx,
        &sys_h,
        atk_cid,
        atk_ship_id,
        587,
        2873,
        npc_id,
        &npc_ball,
        None,
        sys_id,
        500.0,
        now_unix,
    )
    .await;
    assert_eq!(prog_of(atk_cid, "SofLoseShip1"), 0);
    assert_eq!(prog_of(victim_cid, "SofLoseShip1"), 0);

    // 3. Variant Genolution pod (33328) destruction spawns corpse/killmail but does NOT credit SofLoseShip1.
    let pod_res = crate::services::combat_crimewatch::combat_weapons_spool::handle_ship_or_npc_destruction(
        &ctx,
        &sys_h,
        atk_cid,
        atk_ship_id,
        587,
        2873,
        victim_variant_pod_id,
        &pod_ball,
        Some((victim_cid, victim_variant_pod_id)),
        sys_id,
        400.0,
        now_unix,
    )
    .await;
    assert!(pod_res.kill_id > 0 && pod_res.corpse_id > 0 && pod_res.wreck_id == 0);
    assert_eq!(prog_of(victim_cid, "SofLoseShip1"), 0);

    // 4. Non-corvette, non-pod player frigate destruction credits SofLoseShip1 on victim.
    let ship_res = crate::services::combat_crimewatch::combat_weapons_spool::handle_ship_or_npc_destruction(
        &ctx,
        &sys_h,
        atk_cid,
        atk_ship_id,
        587,
        2873,
        victim_frigate_id,
        &frig_ball,
        Some((victim_cid, victim_frigate_id)),
        sys_id,
        1_200.0,
        now_unix,
    )
    .await;
    assert!(ship_res.kill_id > 0 && ship_res.wreck_id > 0);
    assert_eq!(prog_of(victim_cid, "SofLoseShip1"), 1);
}

#[test]
fn epic_arc_level1_dual_credits_mission_division_goals() {
    let (mut ctx, _) = test_ctx_and_clock(1_800_000_000);
    let template = ctx.sde.agents.values().next().cloned().expect("SDE agent");
    {
        let sde_mut = Arc::make_mut(&mut ctx.sde);
        let mut l1_sec = template.clone();
        l1_sec.agent_id = 9_910_001;
        l1_sec.agent_type_id = 10;
        l1_sec.level = 1;
        l1_sec.division_id = 24;
        sde_mut.agents.insert(l1_sec.agent_id, l1_sec);

        let mut l1_dist = template.clone();
        l1_dist.agent_id = 9_910_002;
        l1_dist.agent_type_id = 10;
        l1_dist.level = 1;
        l1_dist.division_id = 22;
        sde_mut.agents.insert(l1_dist.agent_id, l1_dist);

        let mut l4_sec = template;
        l4_sec.agent_id = 9_910_004;
        l4_sec.agent_type_id = 10;
        l4_sec.level = 4;
        l4_sec.division_id = 24;
        sde_mut.agents.insert(l4_sec.agent_id, l4_sec);
    }

    let cid = 92_600_001_i64;
    let max_prog = |kind: &str| -> u64 {
        ctx.quasar_gateway
            .progression
            .career_goals
            .get_or_init_goals(cid as u32)
            .into_iter()
            .filter(|g| g.def.activity_kind == kind)
            .map(|g| g.progress)
            .max()
            .unwrap_or(0)
    };

    record_mission_completion_career_goals(
        &ctx,
        cid,
        9_910_001,
        "Security",
        false,
        false,
        true,
        false,
        false,
        0,
    );
    assert_eq!(max_prog("epic_arc"), 1);
    assert_eq!(max_prog("combat_missions"), 1);

    record_mission_completion_career_goals(
        &ctx,
        cid,
        9_910_002,
        "Courier",
        true,
        false,
        false,
        false,
        false,
        0,
    );
    assert_eq!(max_prog("epic_arc"), 2);
    assert_eq!(max_prog("distribution_missions"), 1);

    record_mission_completion_career_goals(
        &ctx,
        cid,
        9_910_004,
        "Security",
        false,
        false,
        true,
        false,
        false,
        0,
    );
    assert_eq!(max_prog("epic_arc"), 3);
    assert_eq!(max_prog("combat_missions"), 1);
}
