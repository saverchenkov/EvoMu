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
