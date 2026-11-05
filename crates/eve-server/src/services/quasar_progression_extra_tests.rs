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
