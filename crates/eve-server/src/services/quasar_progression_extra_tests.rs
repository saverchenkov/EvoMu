//! Unit tests for `quasar_progression_extra` (Community Tracks, Tales, Goals, Contributions, Seasonal Challenges).

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::{PublicGatewayRouter, STATUS_CONFLICT};
use eve_db::corp_roles;
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage, DynamicValue, QuasarDescriptorRegistry};
use eve_sde::SdeRegistry;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn seq_id_msg(field: u32, id: u64) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Uint64(id));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

fn uuid_msg(field: u32, uuid: [u8; 16]) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

#[test]
fn progression_extra_manager_tracks_tales_and_helpers() {
    let u = [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 1, 2, 3, 4, 5, 6, 7, 8];
    assert_eq!(uuid_to_hex(&u), "0123456789abcdef0102030405060708");

    let msg = uuid_msg(1, u);
    assert_eq!(extract_uuid(&msg, 1), Some(u));
    assert_eq!(extract_uuid(&msg, 2), None);

    let seq = seq_id_msg(2, 42);
    assert_eq!(seq_id_of(&seq, 2), Some(42));
    assert_eq!(seq_id_of(&seq, 1), None);

    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let mgr = ProgressionExtraManager::new_with_sde(Some(Arc::clone(&db)), Some(Arc::clone(&sde)), None);
    mgr.community_track_templates.write().unwrap().insert(
        701,
        CommunityTrackTemplateInfo {
            template_id: 701,
            name: "Shadow War Track".into(),
            target_progress: 50,
            tale_templates: vec![9001, 9002],
        },
    );

    assert_eq!(mgr.activate_community_track(999_999).unwrap_err().0, STATUS_NOT_FOUND);
    let track = mgr.activate_community_track(701).expect("activate template 701");
    assert_eq!(track.template_id, 701);
    assert_eq!(track.target_progress, 50);
    assert_eq!(track.current_progress, 0);
    assert!(track.active);

    // Re-activating existing template returns same track
    let track_again = mgr.activate_community_track(701).expect("re-activate template 701");
    assert_eq!(track_again.track_id, track.track_id);

    // Resolve by track_id and template_id
    assert!(mgr.get_community_track(track.track_id).is_some());
    assert!(mgr.get_community_track(701).is_some());

    // Progress clamps at target_progress
    let p1 = mgr.add_community_progress(701, 20).expect("add 20");
    assert_eq!(p1.current_progress, 20);
    let p2 = mgr.record_tale_contribution(9001, 40);
    assert_eq!(p2.len(), 1);
    assert_eq!(p2[0].current_progress, 50);

    mgr.save_to_db();
    let reloaded = ProgressionExtraManager::new(Some(Arc::clone(&db)));
    assert_eq!(reloaded.get_community_track(track.track_id).unwrap().current_progress, 50);
}

#[test]
fn community_tracks_tales_and_seasonal_challenge_rpc_handlers() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    let mgr = Arc::new(ProgressionExtraManager::new_with_sde(
        Some(Arc::clone(&db)),
        Some(Arc::clone(&sde)),
        None,
    ));
    mgr.community_track_templates.write().unwrap().insert(
        702,
        CommunityTrackTemplateInfo {
            template_id: 702,
            name: "Drifter Incursion".into(),
            target_progress: 100,
            tale_templates: vec![888],
        },
    );
    register_progression_extra_handlers(&router, Arc::clone(&db), Arc::clone(&mgr));

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();
    let mut gm_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(1_000_009),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    gm_ctx.service_role_mask = u64::MAX;

    let player_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(90_000_002),
        Some(1_000_009),
        0,
        "tranquility",
        vec![1],
    );

    // Non-admin rejected on StartRequest
    assert_eq!(
        handlers["eve.communitytrack.api.StartRequest"](&player_ctx, &dummy_any, &seq_id_msg(1, 702))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // Admin starts community track
    handlers["eve.communitytrack.api.StartRequest"](&gm_ctx, &dummy_any, &seq_id_msg(1, 702))
        .expect("start track 702");

    // Public and internal GetRequest
    let pub_get = handlers["eve_public.communitytrack.api.GetRequest"](&player_ctx, &dummy_any, &seq_id_msg(1, 702))
        .expect("public get track");
    assert!(pub_get.type_url.ends_with("eve_public.communitytrack.api.GetResponse"));
    assert!(!pub_get.value.is_empty());

    let int_get = handlers["eve.communitytrack.api.GetRequest"](&player_ctx, &dummy_any, &seq_id_msg(1, 702))
        .expect("internal get track");
    assert!(int_get.type_url.ends_with("eve.communitytrack.api.GetResponse"));

    // AddProgressRequest & SetProgressRequest & StopRequest
    let mut add_msg = seq_id_msg(1, 702);
    add_msg.fields.insert(2, DynamicValue::Uint64(15));
    handlers["eve.communitytrack.api.AddProgressRequest"](&gm_ctx, &dummy_any, &add_msg)
        .expect("add progress");
    assert_eq!(mgr.get_community_track(702).unwrap().current_progress, 15);

    let mut set_msg = seq_id_msg(1, 702);
    set_msg.fields.insert(2, DynamicValue::Uint64(45));
    handlers["eve.communitytrack.admin.api.SetProgressRequest"](&gm_ctx, &dummy_any, &set_msg)
        .expect("set progress");
    assert_eq!(mgr.get_community_track(702).unwrap().current_progress, 45);

    handlers["eve.communitytrack.api.StopRequest"](&gm_ctx, &dummy_any, &seq_id_msg(1, 702))
        .expect("stop track");
    assert!(!mgr.get_community_track(702).unwrap().active);

    // Re-start existing track
    handlers["eve.communitytrack.api.StartRequest"](&gm_ctx, &dummy_any, &seq_id_msg(1, 702))
        .expect("restart track");
    assert!(mgr.get_community_track(702).unwrap().active);

    // Tales: StartTaleRequest, GetActiveTalesCountRequest, StopTalesRequest
    let valid_sys = *sde.solar_systems.keys().next().unwrap_or(&30_000_142);
    let mut start_tale = seq_id_msg(1, 888);
    let mut sys_inner = DynamicQuasarMessage::new("Identifier");
    sys_inner.fields.insert(1, DynamicValue::Uint64(valid_sys as u64));
    start_tale.fields.insert(2, DynamicValue::Message(Box::new(sys_inner)));

    handlers["eve.tale.api.StartTaleRequest"](&gm_ctx, &dummy_any, &start_tale)
        .expect("start tale");

    let count_resp = handlers["eve.tale.api.GetActiveTalesCountRequest"](&player_ctx, &dummy_any, &seq_id_msg(1, 888))
        .expect("count tales");
    assert!(count_resp.type_url.ends_with("eve.tale.api.GetActiveTalesCountResponse"));
    assert!(!count_resp.value.is_empty());

    handlers["eve.tale.api.StopTalesRequest"](&gm_ctx, &dummy_any, &seq_id_msg(1, 888))
        .expect("stop tales");
    assert_eq!(mgr.get_community_track(702).unwrap().current_progress, 46);

    // Seasonal challenges
    let chal_resp = handlers["eve.seasonalevent.challenge.GetChallengesForCharacterRequest"](
        &player_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("Req"),
    )
    .expect("seasonal challenges for self");
    assert!(chal_resp
        .type_url
        .ends_with("eve.seasonalevent.challenge.GetChallengesForCharacterResponse"));

    // Querying another character's challenges as non-admin is forbidden
    assert_eq!(
        handlers["eve.seasonalevent.challenge.GetChallengesForCharacterRequest"](
            &player_ctx,
            &dummy_any,
            &seq_id_msg(1, 90_000_099),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
}

#[test]
fn generic_goals_and_contribution_rpc_lifecycle() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    let mgr = Arc::new(ProgressionExtraManager::new_with_sde(
        Some(Arc::clone(&db)),
        Some(Arc::clone(&sde)),
        None,
    ));
    register_progression_extra_handlers(&router, Arc::clone(&db), Arc::clone(&mgr));

    let handlers = router.handlers.read().unwrap();
    let dummy_any = GoogleAny::default();

    let cid = 90_000_001_u32;
    let corp_id = 1_000_009_u32;
    let mut dir_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(cid),
        Some(corp_id),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    dir_ctx.service_role_mask = u64::MAX;

    let grunt_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(90_000_002),
        Some(corp_id),
        0,
        "tranquility",
        vec![1],
    );

    // Grunt cannot create corp goal
    assert_eq!(
        handlers["eve_public.goal.api.CreateRequest"](&grunt_ctx, &dummy_any, &DynamicQuasarMessage::new("Create"))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // Director creates goal
    let mut create_msg = DynamicQuasarMessage::new("Create");
    create_msg.fields.insert(1, DynamicValue::String("Mine Veldspar".into()));
    create_msg.fields.insert(2, DynamicValue::String("Deliver ore".into()));
    create_msg.fields.insert(3, DynamicValue::Uint64(250));
    let create_resp = handlers["eve_public.goal.api.CreateRequest"](&dir_ctx, &dummy_any, &create_msg)
        .expect("create goal");
    assert!(create_resp.type_url.ends_with("eve_public.goal.api.CreateResponse"));

    let goal_uuid = mgr.generic_goals.read().unwrap().values().next().unwrap().uuid;

    // GetRequest, GetAllRequest, GetCapacityRequest
    let get_resp = handlers["eve_public.goal.api.GetRequest"](&dir_ctx, &dummy_any, &uuid_msg(1, goal_uuid))
        .expect("get goal");
    assert!(get_resp.type_url.ends_with("eve_public.goal.api.GetResponse"));

    let all_resp = handlers["eve_public.goal.api.GetAllRequest"](&dir_ctx, &dummy_any, &DynamicQuasarMessage::new("All"))
        .expect("get all goals");
    assert!(all_resp.type_url.ends_with("eve_public.goal.api.GetAllResponse"));
    assert!(!all_resp.value.is_empty());

    let cap_resp = handlers["eve_public.goal.api.GetCapacityRequest"](&dir_ctx, &dummy_any, &DynamicQuasarMessage::new("Cap"))
        .expect("get capacity");
    assert!(cap_resp.type_url.ends_with("eve_public.goal.api.GetCapacityResponse"));

    // SetNameRequest & SetDescriptionRequest
    let mut rename_msg = uuid_msg(1, goal_uuid);
    rename_msg.fields.insert(2, DynamicValue::String("Mine Dense Veldspar".into()));
    handlers["eve.goal.api.SetNameRequest"](&dir_ctx, &dummy_any, &rename_msg)
        .expect("set goal name");

    let mut desc_msg = uuid_msg(1, goal_uuid);
    desc_msg.fields.insert(2, DynamicValue::String("Updated description".into()));
    handlers["eve.goal.api.SetDescriptionRequest"](&dir_ctx, &dummy_any, &desc_msg)
        .expect("set goal description");

    // SetCurrentProgressRequest: CAS mismatch and success
    let mut cas_bad = uuid_msg(1, goal_uuid);
    cas_bad.fields.insert(2, DynamicValue::Uint64(99));
    cas_bad.fields.insert(3, DynamicValue::Uint64(120));
    assert_eq!(
        handlers["eve_public.goal.api.SetCurrentProgressRequest"](&dir_ctx, &dummy_any, &cas_bad)
            .unwrap_err()
            .0,
        STATUS_CONFLICT
    );

    let mut cas_ok = uuid_msg(1, goal_uuid);
    cas_ok.fields.insert(2, DynamicValue::Uint64(0));
    cas_ok.fields.insert(3, DynamicValue::Uint64(120));
    handlers["eve_public.goal.api.SetCurrentProgressRequest"](&dir_ctx, &dummy_any, &cas_ok)
        .expect("set progress 120");

    // Contributor summary endpoints
    for (req, expected_suffix) in [
        (
            "eve_public.goal.contribution.api.GetContributorSummariesForGoalRequest",
            "eve_public.goal.contribution.api.GetContributorSummariesForGoalResponse",
        ),
        (
            "eve_public.goal.contribution.api.GetMyContributorSummaryForGoalRequest",
            "eve_public.goal.contribution.api.GetMyContributorSummaryForGoalResponse",
        ),
        (
            "eve_public.goal.contribution.api.GetMyContributorSummariesRequest",
            "eve_public.goal.contribution.api.GetMyContributorSummariesResponse",
        ),
    ] {
        let resp = handlers[req](&dir_ctx, &dummy_any, &uuid_msg(1, goal_uuid)).expect(req);
        assert!(resp.type_url.ends_with(expected_suffix));
    }

    // CloseRequest & DeleteRequest
    handlers["eve_public.goal.api.CloseRequest"](&dir_ctx, &dummy_any, &uuid_msg(1, goal_uuid))
        .expect("close goal");
    handlers["eve_public.goal.api.DeleteRequest"](&dir_ctx, &dummy_any, &uuid_msg(1, goal_uuid))
        .expect("delete goal");
    assert_eq!(
        handlers["eve_public.goal.api.GetRequest"](&dir_ctx, &dummy_any, &uuid_msg(1, goal_uuid))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
}

#[test]
fn rehydrate_from_db_restores_all_six_tables_migrates_tracks_and_tolerates_corrupt_blobs() {
    // 1. db == None: rehydrate_from_db returns early without clearing in-memory state
    let no_db_mgr = ProgressionExtraManager::new(None);
    no_db_mgr.community_tracks.write().unwrap().insert(
        42,
        CommunityTrackState {
            track_id: 42,
            template_id: 42,
            name: "KeepMe".into(),
            target_progress: 10,
            current_progress: 3,
            active: true,
            tale_templates: vec![555],
        },
    );
    no_db_mgr.rehydrate_from_db();
    assert_eq!(no_db_mgr.get_community_track(42).unwrap().current_progress, 3);

    // 2. Full DB rehydration across ALL 6 persisted blobs, overwriting stale memory & migrating tracks
    let db = Arc::new(WorldRepository::with_demo_seed());
    let mgr = ProgressionExtraManager::new(Some(Arc::clone(&db)));

    // Pre-populate community_track_templates so migration fills missing tale_templates
    mgr.community_track_templates.write().unwrap().insert(
        101,
        CommunityTrackTemplateInfo {
            template_id: 101,
            name: "Template 101".into(),
            target_progress: 500,
            tale_templates: vec![7001, 7002],
        },
    );

    // Put stale data into all 6 maps to verify rehydrate replaces every map cleanly
    let stale_goal_uuid = [0xaa; 16];
    mgr.community_tracks.write().unwrap().insert(
        999,
        CommunityTrackState {
            track_id: 999,
            template_id: 999,
            name: "Stale".into(),
            target_progress: 1,
            current_progress: 1,
            active: false,
            tale_templates: vec![],
        },
    );
    mgr.generic_goals.write().unwrap().insert(
        uuid_to_hex(&stale_goal_uuid),
        GenericGoalRecord {
            uuid: stale_goal_uuid,
            name: "StaleGoal".into(),
            ..Default::default()
        },
    );
    mgr.active_tales.write().unwrap().insert(999, (1, 30_000_142, 1));
    mgr.campaigns.write().unwrap().insert(999, CampaignRecord { id: 999, ..Default::default() });
    mgr.campaign_objectives.write().unwrap().insert(
        999,
        CampaignObjectiveRecord { id: 999, ..Default::default() },
    );
    mgr.campaign_tracks.write().unwrap().insert(
        999,
        CampaignTrackRecord { id: 999, ..Default::default() },
    );

    // Write fresh state directly to all 6 DB blobs
    let mut fresh_tracks = HashMap::new();
    fresh_tracks.insert(
        101u32,
        CommunityTrackState {
            track_id: 101,
            template_id: 0, // should migrate to track_id (101) & fill tale_templates from 101 template
            name: "Legacy Hydrated Track".into(),
            target_progress: 500,
            current_progress: 77,
            active: true,
            tale_templates: Vec::new(),
        },
    );
    fresh_tracks.insert(
        102u32,
        CommunityTrackState {
            track_id: 102,
            template_id: 999, // explicit non-zero should NOT be overwritten
            name: "Explicit Hydrated Track".into(),
            target_progress: 300,
            current_progress: 12,
            active: false,
            tale_templates: vec![8001], // non-empty should NOT be overwritten
        },
    );
    let fresh_goal_uuid = [0x11; 16];
    let fresh_goal_hex = uuid_to_hex(&fresh_goal_uuid);
    let mut fresh_goals = HashMap::new();
    fresh_goals.insert(
        fresh_goal_hex.clone(),
        GenericGoalRecord {
            uuid: fresh_goal_uuid,
            creator_id: 90_000_001,
            name: "Hydrated Goal".into(),
            description: "Hydrated Desc".into(),
            desired_progress: 400,
            current_progress: 125,
            closed: false,
            matcher_type: "DeliverItem".into(),
            filter_id: Some(34),
            created_unix: 1_700_000_123,
            contribution_config_pb: vec![0x12, 0x04, 0x0a, 0x02, 0x10, 0x01],
        },
    );
    let fresh_tales: HashMap<u32, (u32, u32, u64)> =
        HashMap::from([(9u32, (7001u32, 30_000_142u32, 1_700_000_555u64))]);
    let fresh_campaigns: HashMap<u64, CampaignRecord> = HashMap::from([(
        11u64,
        CampaignRecord {
            id: 11,
            name: "Camp11".into(),
            description: "Desc11".into(),
            state: 1,
            objectives: vec![21],
            ..Default::default()
        },
    )]);
    let fresh_objs: HashMap<u64, CampaignObjectiveRecord> = HashMap::from([(
        21u64,
        CampaignObjectiveRecord {
            id: 21,
            campaign_id: 11,
            name: "Obj21".into(),
            target_progress: 100,
            current_progress: 25,
            ..Default::default()
        },
    )]);
    let fresh_camp_tracks: HashMap<u64, CampaignTrackRecord> = HashMap::from([(
        31u64,
        CampaignTrackRecord {
            id: 31,
            campaign_id: 11,
            key: "Trk31".into(),
            target_progress_value: 200,
            current_progress: 40,
            ..Default::default()
        },
    )]);

    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert(
            "quasar_community_tracks".into(),
            serde_json::to_string(&fresh_tracks).unwrap(),
        );
        snap.quasar_state_blobs.insert(
            "quasar_generic_goals".into(),
            serde_json::to_string(&fresh_goals).unwrap(),
        );
        snap.quasar_state_blobs.insert(
            "quasar_active_tales".into(),
            serde_json::to_string(&fresh_tales).unwrap(),
        );
        snap.quasar_state_blobs.insert(
            "quasar_campaigns".into(),
            serde_json::to_string(&fresh_campaigns).unwrap(),
        );
        snap.quasar_state_blobs.insert(
            "quasar_campaign_objectives".into(),
            serde_json::to_string(&fresh_objs).unwrap(),
        );
        snap.quasar_state_blobs.insert(
            "quasar_campaign_tracks".into(),
            serde_json::to_string(&fresh_camp_tracks).unwrap(),
        );
    });

    mgr.rehydrate_from_db();

    // Verify all 6 stale keys were evicted and fresh DB entries loaded + migrated
    assert!(mgr.get_community_track(999).is_none() || mgr.community_tracks.read().unwrap().get(&999).is_none());
    let t101 = mgr.get_community_track(101).expect("track 101 hydrated");
    assert_eq!(t101.template_id, 101);
    assert_eq!(t101.tale_templates, vec![7001, 7002]);
    assert_eq!(t101.current_progress, 77);
    assert!(t101.active);
    let t102 = mgr.get_community_track(102).expect("track 102 hydrated");
    assert_eq!(t102.template_id, 999);
    assert_eq!(t102.tale_templates, vec![8001]);

    let goals_map = mgr.generic_goals.read().unwrap();
    assert!(!goals_map.contains_key(&uuid_to_hex(&stale_goal_uuid)));
    let hg = goals_map.get(&fresh_goal_hex).expect("goal hydrated");
    assert_eq!(hg.name, "Hydrated Goal");
    assert_eq!(hg.desired_progress, 400);
    assert_eq!(hg.current_progress, 125);
    assert_eq!(hg.contribution_config_pb, vec![0x12, 0x04, 0x0a, 0x02, 0x10, 0x01]);
    drop(goals_map);

    assert_eq!(mgr.active_tales.read().unwrap().get(&9), Some(&(7001, 30_000_142, 1_700_000_555)));
    assert!(mgr.active_tales.read().unwrap().get(&999).is_none());
    assert_eq!(mgr.campaigns.read().unwrap().get(&11).unwrap().name, "Camp11");
    assert!(mgr.campaigns.read().unwrap().get(&999).is_none());
    assert_eq!(mgr.campaign_objectives.read().unwrap().get(&21).unwrap().current_progress, 25);
    assert!(mgr.campaign_objectives.read().unwrap().get(&999).is_none());
    assert_eq!(mgr.campaign_tracks.read().unwrap().get(&31).unwrap().current_progress, 40);
    assert!(mgr.campaign_tracks.read().unwrap().get(&999).is_none());

    // 3. Corrupt JSON on one blob leaves valid blobs intact and defaults corrupt map to empty
    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert("quasar_campaign_objectives".into(), "{corrupt json".into());
    });
    mgr.rehydrate_from_db();
    assert!(mgr.campaign_objectives.read().unwrap().is_empty());
    assert_eq!(mgr.campaigns.read().unwrap().get(&11).unwrap().name, "Camp11");
}

#[test]
fn private_helpers_track_allocation_save_guard_sde_migration_and_encounter_delegation() {
    // 1. resolve_track_key: direct track_id wins over matching template_id on another entry
    let mut tracks = HashMap::new();
    tracks.insert(
        500,
        CommunityTrackState {
            track_id: 500,
            template_id: 700,
            name: "Direct500".into(),
            target_progress: 10,
            current_progress: 1,
            active: true,
            tale_templates: vec![],
        },
    );
    tracks.insert(
        501,
        CommunityTrackState {
            track_id: 501,
            template_id: 500,
            name: "Template500".into(),
            target_progress: 10,
            current_progress: 2,
            active: true,
            tale_templates: vec![],
        },
    );
    assert_eq!(resolve_track_key(&tracks, 500), Some(500));
    assert_eq!(resolve_track_key(&tracks, 700), Some(500));
    assert_eq!(resolve_track_key(&tracks, 999), None);

    // 2. seq_id_of all DynamicValue branches including negatives and non-messages
    assert_eq!(seq_id_of(&DynamicQuasarMessage::new("Empty"), 1), None);
    let mut non_msg = DynamicQuasarMessage::new("Req");
    non_msg.fields.insert(1, DynamicValue::Uint64(5));
    assert_eq!(seq_id_of(&non_msg, 1), None);
    for (val, expected) in [
        (DynamicValue::Uint64(10), Some(10)),
        (DynamicValue::Uint32(11), Some(11)),
        (DynamicValue::Int64(12), Some(12)),
        (DynamicValue::Int64(-1), None),
        (DynamicValue::Int32(13), Some(13)),
        (DynamicValue::Int32(-2), None),
        (DynamicValue::String("bad".into()), None),
    ] {
        let mut inner = DynamicQuasarMessage::new("Identifier");
        inner.fields.insert(1, val);
        let mut outer = DynamicQuasarMessage::new("Req");
        outer.fields.insert(1, DynamicValue::Message(Box::new(inner)));
        assert_eq!(seq_id_of(&outer, 1), expected);
    }

    // extract_uuid len 17 & non-Message outer
    assert_eq!(extract_uuid(&non_msg, 1), None);
    let mut len17_inner = DynamicQuasarMessage::new("Identifier");
    len17_inner.fields.insert(1, DynamicValue::Bytes(vec![7u8; 17]));
    let mut len17_outer = DynamicQuasarMessage::new("Req");
    len17_outer.fields.insert(1, DynamicValue::Message(Box::new(len17_inner)));
    assert_eq!(extract_uuid(&len17_outer, 1), None);

    // 3. activate_community_track ID allocation floor (10_001) + save_to_db write-lock guard + goal_corp_id <= 0
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let mgr = ProgressionExtraManager::new(Some(Arc::clone(&db)));
    mgr.community_track_templates.write().unwrap().insert(
        10,
        CommunityTrackTemplateInfo {
            template_id: 10,
            name: "LowId".into(),
            target_progress: 50,
            tale_templates: vec![3001],
        },
    );
    let tr10 = mgr.activate_community_track(10).expect("activate tpl 10");
    assert_eq!(tr10.track_id, 10_001);

    mgr.community_track_templates.write().unwrap().insert(
        11,
        CommunityTrackTemplateInfo {
            template_id: 11,
            name: "SecondLowId".into(),
            target_progress: 50,
            tale_templates: vec![],
        },
    );
    let tr11 = mgr.activate_community_track(11).expect("activate tpl 11");
    assert_eq!(tr11.track_id, 10_002);

    // Deactivate tr10, bump progress, and reactivate -> preserves progress and sets active = true
    mgr.add_community_progress(10, 17).unwrap();
    mgr.community_tracks.write().unwrap().get_mut(&10_001).unwrap().active = false;
    let tr10_re = mgr.activate_community_track(10).expect("reactivate 10");
    assert!(tr10_re.active);
    assert_eq!(tr10_re.current_progress, 17);

    // save_to_db inside with_mut_snapshot returns immediately without deadlock or overwrite
    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert("quasar_community_tracks".into(), "SENTINEL".into());
        mgr.save_to_db();
        assert_eq!(snap.quasar_state_blobs.get("quasar_community_tracks").map(String::as_str), Some("SENTINEL"));
    });
    // Outside write lock, save_to_db persists
    mgr.save_to_db();
    db.with_snapshot(|snap| {
        assert_ne!(snap.quasar_state_blobs.get("quasar_community_tracks").map(String::as_str), Some("SENTINEL"));
    });

    // goal_corp_id filters corporation_id <= 0 (0 and negative)
    use crate::services::social_corp_fleet::social_corp_projects;
    let zero_pid = 801i64;
    let neg_pid = 802i64;
    db.with_mut_snapshot(|snap| {
        snap.corp_projects.insert(
            zero_pid,
            eve_db::CorpProjectRecord {
                project_id: zero_pid,
                corporation_id: 0,
                name: "ZeroCorp".into(),
                target_quantity: 10,
                current_quantity: 0,
                isk_per_unit: eve_db::IskCents(0),
                escrow_remaining: eve_db::IskCents(0),
            },
        );
        snap.corp_projects.insert(
            neg_pid,
            eve_db::CorpProjectRecord {
                project_id: neg_pid,
                corporation_id: -5,
                name: "NegCorp".into(),
                target_quantity: 10,
                current_quantity: 0,
                isk_per_unit: eve_db::IskCents(0),
                escrow_remaining: eve_db::IskCents(0),
            },
        );
    });
    assert_eq!(mgr.goal_corp_id(&social_corp_projects::corp_goal_uuid(zero_pid)), None);
    assert_eq!(mgr.goal_corp_id(&social_corp_projects::corp_goal_uuid(neg_pid)), None);

    // 4. new_with_sde migration path when real SDE community_track_templates has entries
    if let Ok(records) = sde.load_cfsd_nested("res:/staticdata/community_track_templates.fsdbinary") {
        if let Some((id_str, _)) = records.iter().find(|(k, _)| k.parse::<u32>().is_ok()) {
            let real_tpl_id = id_str.parse::<u32>().unwrap();
            let mig_db = Arc::new(WorldRepository::with_demo_seed());
            let mut seeded_tracks = HashMap::new();
            seeded_tracks.insert(
                real_tpl_id,
                CommunityTrackState {
                    track_id: real_tpl_id,
                    template_id: 0,
                    name: "NeedsMig".into(),
                    target_progress: 10,
                    current_progress: 2,
                    active: true,
                    tale_templates: Vec::new(),
                },
            );
            mig_db.with_mut_snapshot(|snap| {
                snap.quasar_state_blobs.insert(
                    "quasar_community_tracks".into(),
                    serde_json::to_string(&seeded_tracks).unwrap(),
                );
            });
            let migrated_mgr = ProgressionExtraManager::new_with_sde(
                Some(Arc::clone(&mig_db)),
                Some(Arc::clone(&sde)),
                None,
            );
            let mt = migrated_mgr.get_community_track(real_tpl_id).unwrap();
            assert_eq!(mt.template_id, real_tpl_id);
            let tpl = migrated_mgr.community_track_templates.read().unwrap().get(&real_tpl_id).cloned().unwrap();
            assert_eq!(mt.tale_templates, tpl.tale_templates);
            assert!(tpl.target_progress > 0);
            // Verify migrated state was persisted back to DB
            mig_db.with_snapshot(|snap| {
                let raw = snap.quasar_state_blobs.get("quasar_community_tracks").unwrap();
                let parsed: HashMap<u32, CommunityTrackState> = serde_json::from_str(raw).unwrap();
                assert_eq!(parsed[&real_tpl_id].template_id, real_tpl_id);
            });
        }
    }

    // 5. on_system_tale_encounter_completed delegates to complete_system_tale_encounter
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    mgr.active_tales.write().unwrap().insert(55, (3001, 30_000_142, 1_700_000_000));
    let _ = crate::services::quasar_social_chat::drain_local_notices();
    let progressed = mgr.on_system_tale_encounter_completed(
        30_000_142,
        "CombatAnomaly",
        3001,
        5,
        1_700_000_010,
        &router,
    );
    assert_eq!(progressed.len(), 1);
    assert_eq!(progressed[0].track_id, tr10.track_id);
    assert_eq!(progressed[0].current_progress, 22);
    let _ = crate::services::quasar_social_chat::drain_local_notices();
}
