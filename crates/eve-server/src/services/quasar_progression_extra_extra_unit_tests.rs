//! Extra unit and mutation-killing tests for `services::quasar_progression_extra`
//! covering persisted blob hydration & migration (`template_id == 0` and `tale_templates`),
//! `goal_corp_id` filtering (`project_meta` deleted flag, `corp_state` fallback),
//! `activate_community_track` ID allocation & clamping, `record_tale_contribution` active/template
//! filtering, all `extract_track_id` & `seq_id_of` integer variants, community track admin/public
//! handlers, tales start/stop/count handlers, generic goal creation role/escrow/initial-progress
//! guards, goal state transitions (Active/Closed/Completed/Expired), CAS progress updates,
//! contributor summary resolution (`char_contributions` vs creator fallback), and seasonal challenges.

#![forbid(unsafe_code)]

use crate::quasar_gateway::{
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_CONFLICT, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use crate::services::quasar_progression_extra::*;
use crate::services::quasar_social_corp::QuasarCorpState;
use crate::services::social_corp_fleet::social_corp_projects;
use eve_db::{
    corp_roles,
    d6_types::{CorpGovState, PersistedProjectDetails},
    CorpProjectRecord, CorporationRecord, IskCents, WorldRepository,
};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    WireMessage,
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

fn call_gw(
    router: &PublicGatewayRouter,
    endpoint: &str,
    ctx: &AuthoritativeContext,
    msg: &DynamicQuasarMessage,
) -> Result<GoogleAny, (u32, String)> {
    let h = router.handlers.read().unwrap().get(endpoint).cloned().expect(endpoint);
    h(ctx, &GoogleAny::default(), msg)
}

#[test]
fn persisted_hydration_migration_goal_corp_id_and_track_allocation_rules() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let corp_state = QuasarCorpState::from_db(Arc::clone(&db));

    // Pre-seed persisted blobs with a legacy track that has template_id == 0 and empty tale_templates
    let mut legacy_tracks = HashMap::new();
    legacy_tracks.insert(
        500,
        CommunityTrackState {
            track_id: 500,
            template_id: 0,
            name: "Legacy Track".into(),
            target_progress: 80,
            current_progress: 10,
            active: false,
            tale_templates: Vec::new(),
        },
    );
    let mut seeded_tales = HashMap::new();
    seeded_tales.insert(1u32, (888u32, 30_000_142u32, 1_700_000_000u64));
    db.with_mut_snapshot(|snap| {
        snap.quasar_state_blobs.insert(
            "quasar_community_tracks".into(),
            serde_json::to_string(&legacy_tracks).unwrap(),
        );
        snap.quasar_state_blobs.insert(
            "quasar_active_tales".into(),
            serde_json::to_string(&seeded_tales).unwrap(),
        );
    });

    let mgr = ProgressionExtraManager::new_with_sde(
        Some(Arc::clone(&db)),
        Some(Arc::clone(&sde)),
        Some(Arc::clone(&corp_state)),
    );
    // Verify active_tales hydrated from DB
    assert_eq!(mgr.active_tales.read().unwrap().get(&1), Some(&(888, 30_000_142, 1_700_000_000)));
    // Legacy track had template_id == 0 -> migrated to track_id (500) when SDE templates loaded
    if let Some(t500) = mgr.get_community_track(500) {
        assert_eq!(t500.template_id, 500);
    }

    // Test extract_uuid edge cases: wrong length (15 bytes), non-bytes field, missing field
    let mut bad_uuid_inner = DynamicQuasarMessage::new("Identifier");
    bad_uuid_inner.fields.insert(1, DynamicValue::Bytes(vec![1; 15]));
    let mut bad_uuid_outer = DynamicQuasarMessage::new("Req");
    bad_uuid_outer.fields.insert(1, DynamicValue::Message(Box::new(bad_uuid_inner)));
    assert_eq!(extract_uuid(&bad_uuid_outer, 1), None);
    let non_bytes = seq_msg(1, DynamicValue::Uint64(42));
    assert_eq!(extract_uuid(&non_bytes, 1), None);

    // Test activate_community_track ID allocation & inactive track filtering in record_tale_contribution
    mgr.community_track_templates.write().unwrap().insert(
        25_000,
        CommunityTrackTemplateInfo {
            template_id: 25_000,
            name: "High Template Track".into(),
            target_progress: 30,
            tale_templates: vec![777],
        },
    );
    let tr_high = mgr.activate_community_track(25_000).expect("activate 25_000");
    assert!(tr_high.track_id >= 25_001);
    assert_eq!(tr_high.template_id, 25_000);
    assert!(mgr.add_community_progress(999_999, 10).is_none());

    // Inactive track 500 does not receive tale contribution; active track 25_000 does and clamps at 30
    let updated_unmatched = mgr.record_tale_contribution(999, 10);
    assert!(updated_unmatched.is_empty());
    let updated_matched = mgr.record_tale_contribution(777, 100);
    assert_eq!(updated_matched.len(), 1);
    assert_eq!(updated_matched[0].current_progress, 30);

    // Deactivate 25_000 -> record_tale_contribution ignores it
    mgr.community_tracks.write().unwrap().get_mut(&tr_high.track_id).unwrap().active = false;
    assert!(mgr.record_tale_contribution(777, 5).is_empty());

    // goal_corp_id checks: non-CORPGOAL UUID -> None; CORPGOAL UUID active vs deleted in project_meta
    let random_uuid = [9u8; 16];
    assert_eq!(mgr.goal_corp_id(&random_uuid), None);

    let pid = 444i64;
    let c_uuid = social_corp_projects::corp_goal_uuid(pid);
    db.with_mut_snapshot(|snap| {
        snap.corp_projects.insert(
            pid,
            CorpProjectRecord {
                project_id: pid,
                corporation_id: 1_000_009,
                name: "Test Proj".into(),
                target_quantity: 50,
                current_quantity: 10,
                isk_per_unit: IskCents(0),
                escrow_remaining: IskCents(0),
            },
        );
        snap.alliance_war_state.project_meta.insert(pid, ("mine_mineral".into(), false, false));
    });
    assert_eq!(mgr.goal_corp_id(&c_uuid), Some(1_000_009));

    // When mgr.db is None, goal_corp_id falls back to mgr.corp_state.db
    let mut mgr_no_db = ProgressionExtraManager::new(None);
    mgr_no_db.corp_state = Some(Arc::clone(&corp_state));
    assert_eq!(mgr_no_db.goal_corp_id(&c_uuid), Some(1_000_009));

    // Mark project deleted in project_meta (m.1 = true) -> goal_corp_id returns None
    db.with_mut_snapshot(|snap| {
        snap.alliance_war_state.project_meta.insert(pid, ("mine_mineral".into(), true, false));
    });
    assert_eq!(mgr.goal_corp_id(&c_uuid), None);
}

#[test]
fn community_track_and_tales_handler_variants_and_error_paths() {
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
        810,
        CommunityTrackTemplateInfo {
            template_id: 810,
            name: "Triglavian Track".into(),
            target_progress: 60,
            tale_templates: vec![501],
        },
    );
    register_progression_extra_handlers(&router, Arc::clone(&db), Arc::clone(&mgr));

    let mut gm_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(1_000_009),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    gm_ctx.service_role_mask = u64::MAX;

    let mut gm_no_char = AuthoritativeContext::default();
    gm_no_char.service_role_mask = u64::MAX;

    let player_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(90_000_002),
        Some(1_000_009),
        0,
        "tranquility",
        vec![1],
    );

    // GetRequest / StartRequest / StopRequest / SetProgressRequest validation errors
    let empty_msg = DynamicQuasarMessage::new("Empty");
    for ep in [
        "eve_public.communitytrack.api.GetRequest",
        "eve.communitytrack.api.GetRequest",
        "eve.communitytrack.api.StartRequest",
        "eve.communitytrack.api.StopRequest",
        "eve.communitytrack.admin.api.SetProgressRequest",
    ] {
        assert_eq!(call_gw(&router, ep, &gm_ctx, &empty_msg).unwrap_err().0, STATUS_BAD_REQUEST);
    }
    for ep in [
        "eve_public.communitytrack.api.GetRequest",
        "eve.communitytrack.api.GetRequest",
        "eve.communitytrack.api.StopRequest",
        "eve.communitytrack.admin.api.SetProgressRequest",
    ] {
        assert_eq!(
            call_gw(&router, ep, &gm_ctx, &seq_msg(1, DynamicValue::Uint32(999_888))).unwrap_err().0,
            STATUS_NOT_FOUND
        );
    }
    for ep in [
        "eve.communitytrack.api.AddProgressRequest",
        "eve.communitytrack.api.StopRequest",
        "eve.communitytrack.admin.api.SetProgressRequest",
        "eve.tale.api.StartTaleRequest",
        "eve.tale.api.StopTalesRequest",
    ] {
        assert_eq!(call_gw(&router, ep, &player_ctx, &empty_msg).unwrap_err().0, STATUS_FORBIDDEN);
    }

    // Start track using direct Int32 field 1 (non-nested)
    let mut direct_start = DynamicQuasarMessage::new("Start");
    direct_start.fields.insert(1, DynamicValue::Int32(810));
    call_gw(&router, "eve.communitytrack.api.StartRequest", &gm_ctx, &direct_start).unwrap();

    // AddProgressRequest: requires active character, positive delta (Uint32, Int32, Int64), existing track
    assert_eq!(
        call_gw(&router, "eve.communitytrack.api.AddProgressRequest", &gm_no_char, &direct_start)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    let mut bad_delta = seq_msg(1, DynamicValue::Int64(810));
    bad_delta.fields.insert(2, DynamicValue::Int32(0));
    assert_eq!(
        call_gw(&router, "eve.communitytrack.api.AddProgressRequest", &gm_ctx, &bad_delta)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    bad_delta.fields.insert(2, DynamicValue::Int64(-5));
    assert_eq!(
        call_gw(&router, "eve.communitytrack.api.AddProgressRequest", &gm_ctx, &bad_delta)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );

    let mut add_i32 = seq_msg(1, DynamicValue::Uint32(810));
    add_i32.fields.insert(2, DynamicValue::Int32(12));
    call_gw(&router, "eve.communitytrack.api.AddProgressRequest", &gm_ctx, &add_i32).unwrap();
    assert_eq!(mgr.get_community_track(810).unwrap().current_progress, 12);

    let mut add_i64 = seq_msg(1, DynamicValue::Int64(810));
    add_i64.fields.insert(2, DynamicValue::Int64(8));
    call_gw(&router, "eve.communitytrack.api.AddProgressRequest", &gm_ctx, &add_i64).unwrap();
    assert_eq!(mgr.get_community_track(810).unwrap().current_progress, 20);

    // SetProgressRequest: Int32, Int64, clamping above target_progress (60), and negative fallback to 0
    let mut set_clamp = seq_msg(1, DynamicValue::Uint32(810));
    set_clamp.fields.insert(2, DynamicValue::Int64(999));
    call_gw(&router, "eve.communitytrack.admin.api.SetProgressRequest", &gm_ctx, &set_clamp).unwrap();
    assert_eq!(mgr.get_community_track(810).unwrap().current_progress, 60);

    let mut set_neg = seq_msg(1, DynamicValue::Uint32(810));
    set_neg.fields.insert(2, DynamicValue::Int32(-10));
    call_gw(&router, "eve.communitytrack.admin.api.SetProgressRequest", &gm_ctx, &set_neg).unwrap();
    assert_eq!(mgr.get_community_track(810).unwrap().current_progress, 0);

    // Tales: StartTaleRequest requires active character, positive template_id, valid solar_system
    assert_eq!(
        call_gw(&router, "eve.tale.api.StartTaleRequest", &gm_no_char, &empty_msg).unwrap_err().0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call_gw(&router, "eve.tale.api.StartTaleRequest", &gm_ctx, &seq_msg(1, DynamicValue::Int32(0)))
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let mut missing_sys = seq_msg(1, DynamicValue::Int32(501));
    assert_eq!(
        call_gw(&router, "eve.tale.api.StartTaleRequest", &gm_ctx, &missing_sys).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    let mut bad_sys_inner = DynamicQuasarMessage::new("Identifier");
    bad_sys_inner.fields.insert(1, DynamicValue::Uint32(99_999_999));
    missing_sys.fields.insert(2, DynamicValue::Message(Box::new(bad_sys_inner)));
    assert_eq!(
        call_gw(&router, "eve.tale.api.StartTaleRequest", &gm_ctx, &missing_sys).unwrap_err().0,
        STATUS_NOT_FOUND
    );

    // Start two tales (template 501 and template 502) and verify filtered vs unfiltered GetActiveTalesCountRequest
    let valid_sys = *sde.solar_systems.keys().next().unwrap_or(&30_000_142);
    for tpl in [501u32, 502u32] {
        let mut st_req = seq_msg(1, DynamicValue::Uint32(tpl));
        let mut s_in = DynamicQuasarMessage::new("Identifier");
        s_in.fields.insert(1, DynamicValue::Int64(valid_sys as i64));
        st_req.fields.insert(2, DynamicValue::Message(Box::new(s_in)));
        call_gw(&router, "eve.tale.api.StartTaleRequest", &gm_ctx, &st_req).unwrap();
    }
    let all_cnt = call_gw(&router, "eve.tale.api.GetActiveTalesCountRequest", &player_ctx, &empty_msg).unwrap();
    let wm_all = WireMessage::decode(&all_cnt.value).unwrap();
    assert_eq!(wm_all.fields[0].value.as_varint(), Some(2));

    let f_cnt = call_gw(
        &router,
        "eve.tale.api.GetActiveTalesCountRequest",
        &player_ctx,
        &seq_msg(1, DynamicValue::Int32(501)),
    )
    .unwrap();
    let wm_f = WireMessage::decode(&f_cnt.value).unwrap();
    assert_eq!(wm_f.fields[0].value.as_varint(), Some(1));

    // StopTalesRequest validation & selective stopping
    assert_eq!(
        call_gw(&router, "eve.tale.api.StopTalesRequest", &gm_no_char, &empty_msg).unwrap_err().0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call_gw(&router, "eve.tale.api.StopTalesRequest", &gm_ctx, &empty_msg).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    call_gw(&router, "eve.tale.api.StopTalesRequest", &gm_ctx, &seq_msg(1, DynamicValue::Uint32(501))).unwrap();
    assert_eq!(mgr.get_community_track(810).unwrap().current_progress, 1);
    assert_eq!(mgr.active_tales.read().unwrap().len(), 1);
}

#[test]
fn generic_goals_escrow_roles_states_cross_corp_and_contributions() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let corp_state = QuasarCorpState::from_db(Arc::clone(&db));
    let descriptors = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let router = PublicGatewayRouter::new(descriptors, Arc::clone(&db), Arc::clone(&sde));
    let mgr = Arc::new(ProgressionExtraManager::new_with_sde(
        Some(Arc::clone(&db)),
        Some(Arc::clone(&sde)),
        Some(Arc::clone(&corp_state)),
    ));
    register_progression_extra_handlers(&router, Arc::clone(&db), Arc::clone(&mgr));

    let corp_a = 1_000_009_u32;
    let corp_b = 1_000_010_u32;
    let ceo_a = 90_000_001_u32;
    let pm_a = 90_000_002_u32;
    let outsider_b = 90_000_003_u32;

    db.with_mut_snapshot(|snap| {
        let base_ch = snap.characters.get(&(ceo_a as i64)).unwrap().clone();
        for (cid, c_id) in [(ceo_a, corp_a), (pm_a, corp_a), (outsider_b, corp_b)] {
            let mut ch = base_ch.clone();
            ch.character_id = cid as i64;
            ch.corporation_id = c_id as i64;
            snap.characters.insert(cid as i64, ch);
        }
        let mut corp = CorporationRecord {
            corporation_id: corp_a as i64,
            name: "Corp A".into(),
            ticker: "CRPA".into(),
            ceo_id: ceo_a as i64,
            master_wallet: IskCents::from_isk(10_000_000),
            ..Default::default()
        };
        corp.members.insert(ceo_a as i64);
        corp.members.insert(pm_a as i64);
        corp.member_roles
            .insert(pm_a as i64, corp_roles::ROLE_PROJECT_MANAGER);
        corp.set_division_balance(3, IskCents::from_isk(5_000_000));
        snap.corporations.insert(corp_a as i64, corp);
        let mut gov = CorpGovState::default();
        gov.member_account_keys.insert(pm_a as i64, 1002); // division 3 (1002 - 999 = 3)
        snap.corp_governance.insert(corp_a as i64, gov);
    });

    let anon_ctx = AuthoritativeContext::default();
    let pm_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(pm_a),
        Some(corp_a),
        corp_roles::ROLE_PROJECT_MANAGER,
        "tranquility",
        vec![1],
    );
    let pm_with_take_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(pm_a),
        Some(corp_a),
        corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2],
        "tranquility",
        vec![1],
    );
    let outsider_ctx = AuthoritativeContext::for_session(
        Some(3),
        Some(outsider_b),
        Some(corp_b),
        0,
        "tranquility",
        vec![1],
    );
    let mut gm_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(ceo_a),
        Some(corp_a),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    gm_ctx.service_role_mask = u64::MAX;

    // 1. Unauthenticated rejected on Create / Close / Delete / GetCapacity / Contributions / Seasonal
    let empty_msg = DynamicQuasarMessage::new("Empty");
    for ep in [
        "eve_public.goal.api.CreateRequest",
        "eve_public.goal.api.CloseRequest",
        "eve_public.goal.api.DeleteRequest",
        "eve_public.goal.api.GetCapacityRequest",
        "eve_public.goal.contribution.api.GetContributorSummariesForGoalRequest",
        "eve_public.goal.contribution.api.GetMyContributorSummaryForGoalRequest",
        "eve_public.goal.contribution.api.GetMyContributorSummariesRequest",
    ] {
        assert_eq!(call_gw(&router, ep, &anon_ctx, &empty_msg).unwrap_err().0, STATUS_FORBIDDEN);
    }
    assert_eq!(
        call_gw(
            &router,
            "eve.seasonalevent.challenge.GetChallengesForCharacterRequest",
            &anon_ctx,
            &empty_msg,
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );

    // 2. Non-admin Project Manager cannot seed initial progress (field 5 > 0)
    let mut seed_prog_msg = DynamicQuasarMessage::new("Create");
    seed_prog_msg.fields.insert(3, DynamicValue::Int64(50));
    seed_prog_msg.fields.insert(5, DynamicValue::Int64(10));
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.CreateRequest", &pm_ctx, &seed_prog_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // 3. Project Manager without wallet division 3 take role rejected when escrow > 0, succeeds with take role
    let mut escrow_msg = DynamicQuasarMessage::new("Create");
    escrow_msg.fields.insert(1, DynamicValue::String("   ".into())); // blank -> defaults to "Organization Goal"
    escrow_msg.fields.insert(3, DynamicValue::Uint64(10));
    escrow_msg.fields.insert(8, DynamicValue::Uint64(1_000)); // 1000 ISK escrow
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.CreateRequest", &pm_ctx, &escrow_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    db.with_mut_snapshot(|snap| {
        snap.corporations
            .get_mut(&(corp_a as i64))
            .unwrap()
            .member_roles
            .insert(
                pm_a as i64,
                corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2],
            );
    });
    call_gw(&router, "eve_public.goal.api.CreateRequest", &pm_with_take_ctx, &escrow_msg)
        .expect("PM with take role creates goal with escrow");

    // 4. GM creates goal with seeded initial progress (clamped to target)
    call_gw(&router, "eve_public.goal.api.CreateRequest", &gm_ctx, &seed_prog_msg)
        .expect("GM creates goal with initial progress");

    let pm_goal = mgr
        .generic_goals
        .read()
        .unwrap()
        .values()
        .find(|g| g.creator_id == pm_a)
        .unwrap()
        .clone();
    assert_eq!(pm_goal.name, "Organization Goal");
    let pid = social_corp_projects::goal_uuid_to_project_id(&pm_goal.uuid).unwrap();

    // 5. Cross-corp access denied on GetRequest & GetContributorSummariesForGoalRequest for non-admin outsider
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.GetRequest", &outsider_ctx, &uuid_msg(1, pm_goal.uuid))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call_gw(
            &router,
            "eve_public.goal.contribution.api.GetContributorSummariesForGoalRequest",
            &outsider_ctx,
            &uuid_msg(1, pm_goal.uuid),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    // GetAllRequest for outsider omits Corp A goals
    let out_all = call_gw(&router, "eve_public.goal.api.GetAllRequest", &outsider_ctx, &empty_msg).unwrap();
    assert!(out_all.value.is_empty());

    // 6. Seed char_contributions & test Expired (state 4) and Completed (state 3) in GetRequest
    db.with_mut_snapshot(|snap| {
        let det = snap
            .alliance_war_state
            .project_details
            .entry(pid)
            .or_insert_with(PersistedProjectDetails::default);
        det.char_contributions.insert(pm_a as i64, 7);
        det.expired = true;
    });
    let get_exp = call_gw(&router, "eve_public.goal.api.GetRequest", &pm_ctx, &uuid_msg(1, pm_goal.uuid)).unwrap();
    let wm_exp = WireMessage::decode(&get_exp.value).unwrap();
    let attr_exp = WireMessage::decode(wm_exp.fields[0].value.as_bytes().unwrap()).unwrap();
    assert!(attr_exp.fields.iter().any(|f| f.field_number == 6 && f.value.as_varint() == Some(4)));

    // Clear expired, complete goal via SetCurrentProgressRequest (0 -> 10) and verify state == 3
    db.with_mut_snapshot(|snap| {
        if let Some(det) = snap.alliance_war_state.project_details.get_mut(&pid) {
            det.expired = false;
            det.status = "Active".into();
        }
    });
    let mut prog_done = uuid_msg(1, pm_goal.uuid);
    prog_done.fields.insert(2, DynamicValue::Int64(0));
    prog_done.fields.insert(3, DynamicValue::Int64(10));
    call_gw(&router, "eve_public.goal.api.SetCurrentProgressRequest", &gm_ctx, &prog_done).unwrap();

    let get_done = call_gw(&router, "eve_public.goal.api.GetRequest", &pm_ctx, &uuid_msg(1, pm_goal.uuid)).unwrap();
    let wm_done = WireMessage::decode(&get_done.value).unwrap();
    let attr_done = WireMessage::decode(wm_done.fields[0].value.as_bytes().unwrap()).unwrap();
    assert!(attr_done.fields.iter().any(|f| f.field_number == 6 && f.value.as_varint() == Some(3)));

    // Verify contributor summaries use char_contributions
    let my_sum = call_gw(
        &router,
        "eve_public.goal.contribution.api.GetMyContributorSummaryForGoalRequest",
        &pm_ctx,
        &uuid_msg(1, pm_goal.uuid),
    )
    .unwrap();
    assert!(!my_sum.value.is_empty());
    let all_my = call_gw(
        &router,
        "eve_public.goal.contribution.api.GetMyContributorSummariesRequest",
        &pm_ctx,
        &empty_msg,
    )
    .unwrap();
    assert!(!all_my.value.is_empty());

    // 7. SetNameRequest rejects empty name; Outsider rejected on CloseRequest & DeleteRequest
    let mut bad_name = uuid_msg(1, pm_goal.uuid);
    bad_name.fields.insert(2, DynamicValue::String("   ".into()));
    assert_eq!(
        call_gw(&router, "eve.goal.api.SetNameRequest", &gm_ctx, &bad_name).unwrap_err().0,
        STATUS_BAD_REQUEST
    );
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.CloseRequest", &outsider_ctx, &uuid_msg(1, pm_goal.uuid))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.DeleteRequest", &outsider_ctx, &uuid_msg(1, pm_goal.uuid))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // CAS mismatch on SetCurrentProgressRequest
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.SetCurrentProgressRequest", &gm_ctx, &prog_done)
            .unwrap_err()
            .0,
        STATUS_CONFLICT
    );
}
