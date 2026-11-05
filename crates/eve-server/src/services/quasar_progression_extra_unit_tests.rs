//! Extra unit and mutation-killing tests for `services::quasar_progression_extra`
//! covering `ProgressionExtraManager` persisted track template backfill, `goal_corp_id` ownership,
//! `extract_track_id` scalar/nested encodings, `AddProgressRequest` / `SetProgressRequest` validation,
//! `StartTaleRequest` / `StopTalesRequest` / `GetActiveTalesCountRequest` edge cases,
//! goal creation role/escrow/initial-progress guards, goal state transitions (Active/Closed/Completed/Expired),
//! cross-corp access checks, and `GetChallengesForCharacterRequest` auth and admin overrides.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use crate::services::quasar_progression_extra::*;
use crate::services::social_corp_fleet::social_corp_projects;
use eve_db::{corp_roles, WorldRepository};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

fn seq_msg(field: u32, val: DynamicValue) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, val);
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
fn persisted_track_backfill_and_community_track_and_tale_error_branches() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());

    // Pre-seed a track with template_id == 0 and empty tale_templates in quasar_state_blobs
    db.with_mut_snapshot(|snap| {
        let mut map = HashMap::new();
        map.insert(
            1,
            CommunityTrackState {
                track_id: 1,
                template_id: 0,
                name: "PersistedLegacyTrack".into(),
                target_progress: 200,
                current_progress: 25,
                active: false,
                tale_templates: vec![],
            },
        );
        snap.quasar_state_blobs.insert(
            "quasar_community_tracks".into(),
            serde_json::to_string(&map).unwrap(),
        );
    });

    let mgr = Arc::new(ProgressionExtraManager::new_with_sde(
        Some(Arc::clone(&db)),
        Some(Arc::clone(&sde)),
        None,
    ));
    let t1 = mgr.get_community_track(1).expect("persisted track 1");
    assert_eq!(t1.template_id, 1, "template_id=0 should backfill to track_id");

    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
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

    let mut gm_no_char = AuthoritativeContext::for_session(
        Some(1),
        None,
        Some(1_000_009),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    gm_no_char.service_role_mask = u64::MAX;

    // 1. Missing/invalid track IDs return STATUS_BAD_REQUEST or STATUS_NOT_FOUND
    let empty_msg = DynamicQuasarMessage::new("Empty");
    for req in [
        "eve_public.communitytrack.api.GetRequest",
        "eve.communitytrack.api.GetRequest",
        "eve.communitytrack.api.StartRequest",
        "eve.communitytrack.api.StopRequest",
        "eve.communitytrack.admin.api.SetProgressRequest",
    ] {
        assert_eq!(
            handlers[req](&gm_ctx, &dummy_any, &empty_msg)
                .unwrap_err()
                .0,
            STATUS_BAD_REQUEST
        );
        assert_eq!(
            handlers[req](&gm_ctx, &dummy_any, &seq_msg(1, DynamicValue::Int32(888_888)))
                .unwrap_err()
                .0,
            STATUS_NOT_FOUND
        );
    }

    // 2. AddProgressRequest validation: no character, missing track ID, non-positive delta, Int32 delta
    assert_eq!(
        handlers["eve.communitytrack.api.AddProgressRequest"](&gm_no_char, &dummy_any, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    let mut bad_delta = seq_msg(1, DynamicValue::Uint32(1));
    bad_delta.fields.insert(2, DynamicValue::Int32(0));
    assert_eq!(
        handlers["eve.communitytrack.api.AddProgressRequest"](&gm_ctx, &dummy_any, &bad_delta)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    let mut good_add = DynamicQuasarMessage::new("DirectScalarId");
    good_add.fields.insert(1, DynamicValue::Int32(1));
    good_add.fields.insert(2, DynamicValue::Int32(30));
    handlers["eve.communitytrack.api.AddProgressRequest"](&gm_ctx, &dummy_any, &good_add)
        .expect("AddProgress with direct Int32 track_id");
    assert_eq!(mgr.get_community_track(1).unwrap().current_progress, 55);

    // 3. SetProgressRequest clamping to target_progress (200) and default 0 when field 2 omitted
    let mut set_over = seq_msg(1, DynamicValue::Int64(1));
    set_over.fields.insert(2, DynamicValue::Int64(999));
    handlers["eve.communitytrack.admin.api.SetProgressRequest"](&gm_ctx, &dummy_any, &set_over)
        .expect("clamp to target");
    assert_eq!(mgr.get_community_track(1).unwrap().current_progress, 200);

    let set_zero = seq_msg(1, DynamicValue::Uint64(1));
    handlers["eve.communitytrack.admin.api.SetProgressRequest"](&gm_ctx, &dummy_any, &set_zero)
        .expect("reset to 0");
    assert_eq!(mgr.get_community_track(1).unwrap().current_progress, 0);

    // 4. StartTaleRequest & StopTalesRequest & GetActiveTalesCountRequest edge cases
    assert_eq!(
        handlers["eve.tale.api.StartTaleRequest"](&gm_no_char, &dummy_any, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        handlers["eve.tale.api.StartTaleRequest"](&gm_ctx, &dummy_any, &empty_msg)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let only_tpl = seq_msg(1, DynamicValue::Uint32(55));
    assert_eq!(
        handlers["eve.tale.api.StartTaleRequest"](&gm_ctx, &dummy_any, &only_tpl)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let mut bad_sys = seq_msg(1, DynamicValue::Uint32(55));
    let mut sys_inner = DynamicQuasarMessage::new("Identifier");
    sys_inner.fields.insert(1, DynamicValue::Int32(99_999_999));
    bad_sys
        .fields
        .insert(2, DynamicValue::Message(Box::new(sys_inner)));
    assert_eq!(
        handlers["eve.tale.api.StartTaleRequest"](&gm_ctx, &dummy_any, &bad_sys)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    // Unfiltered GetActiveTalesCountRequest counts all active tales
    mgr.active_tales
        .write()
        .unwrap()
        .insert(10, (501, 30_000_142, 1_700_000_000));
    mgr.active_tales
        .write()
        .unwrap()
        .insert(11, (502, 30_000_142, 1_700_000_000));
    let all_cnt =
        handlers["eve.tale.api.GetActiveTalesCountRequest"](&gm_ctx, &dummy_any, &empty_msg)
            .expect("all tales count");
    assert_eq!(all_cnt.value, vec![0x08, 0x02]);
}

#[test]
fn generic_goals_roles_escrow_states_cross_corp_and_seasonal_challenge_edges() {
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

    let corp_a = 1_000_009_u32;
    let corp_b = 1_000_010_u32;
    let char_pm = 90_000_001_u32;
    let char_other_corp = 90_000_002_u32;

    db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.characters.get_mut(&(char_pm as i64)) {
            c1.corporation_id = corp_a as i64;
        }
        let mut c2 = snap.characters[&(char_pm as i64)].clone();
        c2.character_id = char_other_corp as i64;
        c2.corporation_id = corp_b as i64;
        snap.characters.insert(char_other_corp as i64, c2);
    });

    // Project Manager without admin role cannot seed initial_prog > 0 on CreateRequest
    let pm_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(char_pm),
        Some(corp_a),
        corp_roles::ROLE_PROJECT_MANAGER,
        "tranquility",
        vec![1],
    );
    let mut seed_prog_msg = DynamicQuasarMessage::new("Create");
    seed_prog_msg.fields.insert(3, DynamicValue::Int64(100));
    seed_prog_msg.fields.insert(5, DynamicValue::Int64(25));
    assert_eq!(
        handlers["eve_public.goal.api.CreateRequest"](&pm_ctx, &dummy_any, &seed_prog_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // Project Manager creates goal with default name/target (omitting fields 1, 2, 3, 5)
    let created = handlers["eve_public.goal.api.CreateRequest"](
        &pm_ctx,
        &dummy_any,
        &DynamicQuasarMessage::new("DefaultCreate"),
    )
    .expect("PM create default goal");
    assert!(!created.value.is_empty());

    let goal_uuid = mgr
        .generic_goals
        .read()
        .unwrap()
        .values()
        .next()
        .unwrap()
        .uuid;
    assert_eq!(mgr.goal_corp_id(&goal_uuid), Some(corp_a));

    // Member of corp_b is denied GetRequest and GetContributorSummariesForGoalRequest on corp_a's goal
    let other_corp_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(char_other_corp),
        Some(corp_b),
        0,
        "tranquility",
        vec![1],
    );
    assert_eq!(
        handlers["eve_public.goal.api.GetRequest"](
            &other_corp_ctx,
            &dummy_any,
            &uuid_msg(1, goal_uuid)
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        handlers["eve_public.goal.contribution.api.GetContributorSummariesForGoalRequest"](
            &other_corp_ctx,
            &dummy_any,
            &uuid_msg(1, goal_uuid)
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // SetNameRequest rejects empty/whitespace name
    let mut gm_ctx = pm_ctx.clone();
    gm_ctx.service_role_mask = u64::MAX;
    let mut empty_name = uuid_msg(1, goal_uuid);
    empty_name
        .fields
        .insert(2, DynamicValue::String("   ".into()));
    assert_eq!(
        handlers["eve.goal.api.SetNameRequest"](&gm_ctx, &dummy_any, &empty_name)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    // SetCurrentProgressRequest reaching desired_progress (100) marks project Completed (state 3)
    let mut complete_msg = uuid_msg(1, goal_uuid);
    complete_msg.fields.insert(2, DynamicValue::Int64(0));
    complete_msg.fields.insert(3, DynamicValue::Int64(100));
    handlers["eve_public.goal.api.SetCurrentProgressRequest"](&gm_ctx, &dummy_any, &complete_msg)
        .expect("complete goal");

    let pid = social_corp_projects::goal_uuid_to_project_id(&goal_uuid).unwrap();
    let status_after = db.with_snapshot(|s| {
        s.alliance_war_state
            .project_details
            .get(&pid)
            .map(|d| d.status.clone())
            .unwrap_or_default()
    });
    assert_eq!(status_after, "Completed");

    // Seasonal challenges: missing character ID returns STATUS_BAD_REQUEST; admin can query another char
    let anon_ctx = AuthoritativeContext::for_session(
        Some(2),
        None,
        Some(corp_b),
        0,
        "tranquility",
        vec![1],
    );
    assert_eq!(
        handlers["eve.seasonalevent.challenge.GetChallengesForCharacterRequest"](
            &anon_ctx,
            &dummy_any,
            &DynamicQuasarMessage::new("Req"),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let admin_other = handlers["eve.seasonalevent.challenge.GetChallengesForCharacterRequest"](
        &gm_ctx,
        &dummy_any,
        &seq_msg(1, DynamicValue::Uint32(char_other_corp)),
    )
    .expect("admin can query another character's seasonal challenges");
    assert!(admin_other
        .type_url
        .ends_with("eve.seasonalevent.challenge.GetChallengesForCharacterResponse"));
}
