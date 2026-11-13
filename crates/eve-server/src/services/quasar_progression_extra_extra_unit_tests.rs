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
    PublicGatewayRouter, STATUS_BAD_REQUEST, STATUS_FORBIDDEN, STATUS_NOT_FOUND,
};
use crate::services::quasar_progression_extra::*;
use crate::services::quasar_social_corp::QuasarCorpState;
use crate::services::social_corp_fleet::social_corp_projects;
use eve_db::{
    corp_roles,
    CorpProjectRecord, IskCents, WorldRepository,
};
use eve_proto::quasar::{
    AuthoritativeContext, DynamicQuasarMessage, DynamicValue, GoogleAny, QuasarDescriptorRegistry,
    WireMessage,
};
use eve_sde::SdeRegistry;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

pub(super) static SHARED_SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();

pub(super) fn shared_sde() -> Arc<SdeRegistry> {
    Arc::clone(SHARED_SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

pub(super) fn seq_msg(field: u32, val: DynamicValue) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, val);
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

pub(super) fn uuid_msg(field: u32, uuid: [u8; 16]) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(field, DynamicValue::Message(Box::new(inner)));
    outer
}

pub(super) fn call_gw(
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

fn wm_field_bytes(buf: &[u8], fnum: u32) -> Option<Vec<u8>> {
    WireMessage::decode(buf).ok()?.fields.into_iter().find_map(|f| {
        (f.field_number == fnum).then(|| f.value.as_bytes().map(|b| b.to_vec())).flatten()
    })
}

fn wm_all_bytes(buf: &[u8], fnum: u32) -> Vec<Vec<u8>> {
    WireMessage::decode(buf)
        .map(|wm| {
            wm.fields
                .into_iter()
                .filter(|f| f.field_number == fnum)
                .filter_map(|f| f.value.as_bytes().map(|b| b.to_vec()))
                .collect()
        })
        .unwrap_or_default()
}

fn wm_field_varint(buf: &[u8], fnum: u32) -> Option<u64> {
    WireMessage::decode(buf).ok()?.fields.into_iter().find_map(|f| {
        (f.field_number == fnum).then(|| f.value.as_varint()).flatten()
    })
}

fn wm_field_str(buf: &[u8], fnum: u32) -> Option<String> {
    let b = wm_field_bytes(buf, fnum)?;
    String::from_utf8(b).ok()
}

fn wm_has_field(buf: &[u8], fnum: u32) -> bool {
    WireMessage::decode(buf)
        .map(|wm| wm.fields.iter().any(|f| f.field_number == fnum))
        .unwrap_or(false)
}

#[test]
fn resolved_contrib_pb_all_matcher_types_filters_and_cached_passthrough() {
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

    let mut gm_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(90_000_001),
        Some(1_000_009),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    gm_ctx.service_role_mask = u64::MAX;

    // 1. Exercise EVERY branch of `resolved_contrib_pb` -> `encode_goal_contribution_config`
    // via DB-backed corp_projects (where `generic_goals` cache is empty so contribution_config_pb == &[]).
    let cases: &[(&str, Option<i64>, u32, Option<(u32, u32, u64)>)] = &[
        // (matcher_type, filter_id, expected_top_field_in_ContributionConfiguration, optional nested (inner_field, sub_field, expected_seq_or_varint))
        ("DeliverItem", Some(34), 2, Some((1, 1, 34))),
        ("DeliverItem", Some(0), 2, None), // filter_id == 0 behaves like None
        ("DeliverItem", None, 2, None),
        ("DestroyNPCShips", Some(30_000_142), 3, Some((1, 4, 30_000_142))),
        ("DestroyNPCShips", None, 3, Some((1, 3, 1))),
        ("DamageShips", Some(30_000_142), 4, Some((1, 2, 30_000_142))),
        ("DamageShips", None, 4, Some((1, 1, 1))),
        ("MineMaterials", Some(1230), 5, Some((1, 4, 1230))),
        ("MineOre", Some(1231), 5, Some((1, 4, 1231))),
        ("MineMaterials", None, 5, Some((1, 3, 1))),
        ("DestroyCapsuleerShips", None, 6, None),
        ("CaptureFWComplexes", None, 7, None),
        ("DefendFWComplexes", None, 8, None),
        ("RemoteRepairArmor", None, 9, None),
        ("RemoteRepairShield", None, 10, None),
        ("ScanSignatures", None, 12, None),
        ("ManufactureItems", Some(587), 14, Some((3, 1, 587))),
        ("ManufactureItems", None, 14, None),
        ("SalvageWrecks", None, 16, None),
        ("EarnLoyaltyPoints", None, 17, None),
        ("Manual", None, 1, None),
        ("UnknownMatcher", Some(-5), 1, None),
    ];

    let mut case_uuids = Vec::new();
    db.with_mut_snapshot(|snap| {
        for (idx, (matcher, filter_id, _, _)) in cases.iter().enumerate() {
            let pid = 7_100 + idx as i64;
            snap.corp_projects.insert(
                pid,
                CorpProjectRecord {
                    project_id: pid,
                    corporation_id: 1_000_009,
                    name: format!("Matcher_{idx}"),
                    target_quantity: 100,
                    current_quantity: 0,
                    isk_per_unit: IskCents(0),
                    escrow_remaining: IskCents(0),
                },
            );
            snap.alliance_war_state
                .project_meta
                .insert(pid, ((*matcher).to_string(), false, false));
            let det = snap
                .alliance_war_state
                .project_details
                .entry(pid)
                .or_default();
            det.filter_id = *filter_id;
            det.creator_id = 90_000_001;
            det.status = "Active".into();
            case_uuids.push(social_corp_projects::corp_goal_uuid(pid));
        }
    });

    // Check GetRequest resolves exact `Attributes.contribution_config` (field 7) bytes
    for (idx, ((matcher, filter_id, top_field, nested), goal_uuid)) in
        cases.iter().zip(&case_uuids).enumerate()
    {
        let rsp = call_gw(
            &router,
            "eve_public.goal.api.GetRequest",
            &gm_ctx,
            &uuid_msg(1, *goal_uuid),
        )
        .expect(matcher);
        let attrs = wm_field_bytes(&rsp.value, 1).expect("GetResponse attrs");
        let cfg_pb = wm_field_bytes(&attrs, 7).expect("Attributes.contribution_config field 7");
        assert_eq!(
            cfg_pb,
            crate::services::quasar_goal_tale_notices::encode_goal_contribution_config(
                matcher, *filter_id
            ),
            "case {idx} ({matcher}, {filter_id:?}) mismatch"
        );
        assert!(
            wm_has_field(&cfg_pb, *top_field),
            "expected field {top_field} in cfg for ({matcher}, {filter_id:?})"
        );
        if let Some((inner_f, sub_f, expected_val)) = nested {
            let inner = wm_field_bytes(&cfg_pb, *top_field).unwrap();
            let method = wm_field_bytes(&inner, *inner_f).unwrap();
            let actual = wm_field_varint(&method, *sub_f).unwrap_or_else(|| {
                let nested_id = wm_field_bytes(&method, *sub_f).expect("nested seq id");
                wm_field_varint(&nested_id, 1).expect("seq id varint")
            });
            assert_eq!(actual, *expected_val, "nested value mismatch for {matcher}");
        }
    }

    // Check GetAllResponse also encodes every case cleanly
    let all_rsp = call_gw(
        &router,
        "eve_public.goal.api.GetAllRequest",
        &gm_ctx,
        &DynamicQuasarMessage::new("All"),
    )
    .unwrap();
    let entries = wm_all_bytes(&all_rsp.value, 3);
    assert_eq!(entries.len(), cases.len());
    for goal_msg in &entries {
        let attrs = wm_field_bytes(goal_msg, 2).unwrap();
        let cfg_pb = wm_field_bytes(&attrs, 7).unwrap();
        assert!(!cfg_pb.is_empty());
    }

    // 2. Non-empty `contribution_config_pb` in cached `generic_goals` wins verbatim over recomputation
    let passthrough_pid = 7_100i64;
    let passthrough_uuid = social_corp_projects::corp_goal_uuid(passthrough_pid);
    let custom_pb = vec![0x8a, 0x03, 0x02, 0x08, 0x2a]; // opaque non-empty wire bytes (field 57 varint 42)
    mgr.generic_goals.write().unwrap().insert(
        uuid_to_hex(&passthrough_uuid),
        GenericGoalRecord {
            uuid: passthrough_uuid,
            creator_id: 90_000_001,
            name: "CustomCached".into(),
            description: String::new(),
            desired_progress: 100,
            current_progress: 0,
            closed: false,
            matcher_type: "DeliverItem".into(),
            filter_id: Some(34),
            created_unix: 100,
            contribution_config_pb: custom_pb.clone(),
        },
    );
    let get_cached = call_gw(
        &router,
        "eve_public.goal.api.GetRequest",
        &gm_ctx,
        &uuid_msg(1, passthrough_uuid),
    )
    .unwrap();
    let cached_attrs = wm_field_bytes(&get_cached.value, 1).unwrap();
    assert_eq!(wm_field_bytes(&cached_attrs, 7).unwrap(), custom_pb);

    let all_cached = call_gw(
        &router,
        "eve_public.goal.api.GetAllRequest",
        &gm_ctx,
        &DynamicQuasarMessage::new("All"),
    )
    .unwrap();
    let found_cached = wm_all_bytes(&all_cached.value, 3).into_iter().find(|entry| {
        let id_msg = wm_field_bytes(entry, 1).unwrap();
        wm_field_bytes(&id_msg, 1).unwrap().as_slice() == passthrough_uuid
    });
    let found_attrs = wm_field_bytes(&found_cached.unwrap(), 2).unwrap();
    assert_eq!(wm_field_bytes(&found_attrs, 7).unwrap(), custom_pb);

    // 3. In-memory only `generic_goals` (non-CORPGOAL UUIDs and mgr.db == None fallback)
    let raw_mem_uuid_1 = [0x22u8; 16];
    let raw_mem_uuid_2 = [0x11u8; 16];
    let raw_mem_uuid_3 = [0x33u8; 16];
    {
        let mut gmap = mgr.generic_goals.write().unwrap();
        // Active non-CORPGOAL goal with empty contribution_config_pb -> generated from matcher_type & filter_id
        gmap.insert(
            uuid_to_hex(&raw_mem_uuid_1),
            GenericGoalRecord {
                uuid: raw_mem_uuid_1,
                creator_id: 90_000_001,
                name: "MemActive".into(),
                description: "Mem".into(),
                desired_progress: 20,
                current_progress: 5,
                closed: false,
                matcher_type: "SalvageWrecks".into(),
                filter_id: None,
                created_unix: 0, // triggers fallback to ctx.now_unix()
                contribution_config_pb: Vec::new(),
            },
        );
        // Closed non-CORPGOAL goal -> state 2 even if progress >= desired
        gmap.insert(
            uuid_to_hex(&raw_mem_uuid_2),
            GenericGoalRecord {
                uuid: raw_mem_uuid_2,
                creator_id: 90_000_001,
                name: "MemClosed".into(),
                description: String::new(),
                desired_progress: 10,
                current_progress: 10,
                closed: true,
                matcher_type: "EarnLoyaltyPoints".into(),
                filter_id: None,
                created_unix: 500,
                contribution_config_pb: Vec::new(),
            },
        );
        // Completed non-CORPGOAL goal -> state 3
        gmap.insert(
            uuid_to_hex(&raw_mem_uuid_3),
            GenericGoalRecord {
                uuid: raw_mem_uuid_3,
                creator_id: 90_000_001,
                name: "MemCompleted".into(),
                description: String::new(),
                desired_progress: 10,
                current_progress: 10,
                closed: false,
                matcher_type: "ScanSignatures".into(),
                filter_id: None,
                created_unix: 600,
                contribution_config_pb: Vec::new(),
            },
        );
    }

    for (u, expected_state, expected_matcher) in [
        (raw_mem_uuid_1, 1u64, "SalvageWrecks"),
        (raw_mem_uuid_2, 2u64, "EarnLoyaltyPoints"),
        (raw_mem_uuid_3, 3u64, "ScanSignatures"),
    ] {
        let r = call_gw(&router, "eve_public.goal.api.GetRequest", &gm_ctx, &uuid_msg(1, u)).unwrap();
        let attrs = wm_field_bytes(&r.value, 1).unwrap();
        assert_eq!(wm_field_varint(&attrs, 6), Some(expected_state));
        assert_eq!(
            wm_field_bytes(&attrs, 7).unwrap(),
            crate::services::quasar_goal_tale_notices::encode_goal_contribution_config(expected_matcher, None)
        );
    }

    // 4. CreateRequest with raw field 4 in `any.value` forwards those raw bytes verbatim into created goal
    let _ = crate::services::quasar_social_chat::drain_local_notices();
    let custom_raw_cfg = vec![0xaa, 0x01, 0x02, 0x08, 0x09];
    let raw_any_val = eve_proto::pb::message("eve_public.goal.api.CreateRequest")
        .set_raw_message("contribution_configuration", &custom_raw_cfg)
        .must_encode();
    let mut create_msg = DynamicQuasarMessage::new("Create");
    create_msg.fields.insert(1, DynamicValue::String("RawCfgGoal".into()));
    create_msg.fields.insert(3, DynamicValue::Uint64(50));
    let create_h = router
        .handlers
        .read()
        .unwrap()
        .get("eve_public.goal.api.CreateRequest")
        .cloned()
        .unwrap();
    let created_rsp = create_h(
        &gm_ctx,
        &GoogleAny { type_url: String::new(), value: raw_any_val },
        &create_msg,
    )
    .unwrap();
    let id_bytes = wm_field_bytes(&created_rsp.value, 1).unwrap();
    let created_uuid_vec = wm_field_bytes(&id_bytes, 1).unwrap();
    let mut created_uuid = [0u8; 16];
    created_uuid.copy_from_slice(&created_uuid_vec);
    assert_eq!(
        mgr.generic_goals.read().unwrap()[&uuid_to_hex(&created_uuid)].contribution_config_pb,
        custom_raw_cfg
    );
    let _ = crate::services::quasar_social_chat::drain_local_notices();
}

#[test]
fn generic_goal_auth_escrow_state_precedence_and_wire_payload_fidelity() {
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

    // Setup distinct roles/corps cleanly:
    // corp 1_000_009: CEO = 90_000_001, ProjectManager = 90_000_010 (acct key 1002 -> div 3), Member/Grunt = 90_000_002
    // corp 1_000_099: OtherCorpDir = 90_000_099
    let ceo_id = 90_000_001_u32;
    let grunt_id = 90_000_002_u32;
    let pm_id = 90_000_010_u32;
    let outsider_id = 90_000_099_u32;
    let corp_a = 1_000_009_u32;
    let corp_b = 1_000_099_u32;

    db.with_mut_snapshot(|snap| {
        let base_char = snap.characters[&(ceo_id as i64)].clone();
        if let Some(ch) = snap.characters.get_mut(&(ceo_id as i64)) {
            ch.corporation_id = corp_a as i64;
        }
        if let Some(ch) = snap.characters.get_mut(&(grunt_id as i64)) {
            ch.corporation_id = corp_a as i64;
        }
        let mut pm_char = base_char.clone();
        pm_char.character_id = pm_id as i64;
        pm_char.corporation_id = corp_a as i64;
        pm_char.name = "PM".into();
        snap.characters.insert(pm_id as i64, pm_char);

        let mut out_char = base_char;
        out_char.character_id = outsider_id as i64;
        out_char.corporation_id = corp_b as i64;
        out_char.name = "Outsider".into();
        snap.characters.insert(outsider_id as i64, out_char);

        let template_corp = snap.corporations.values().next().unwrap().clone();
        let mut corp1 = template_corp.clone();
        corp1.corporation_id = corp_a as i64;
        corp1.name = "CorpA".into();
        corp1.ticker = "CRPA".into();
        corp1.ceo_id = ceo_id as i64;
        corp1.ensure_wallet_divisions();
        corp1.set_division_balance(1, IskCents::from_isk(500_000));
        corp1.set_division_balance(3, IskCents::from_isk(250_000));
        corp1.members = [ceo_id as i64, pm_id as i64, grunt_id as i64]
            .into_iter()
            .collect();
        corp1.member_roles = std::collections::HashMap::from([
            (ceo_id as i64, u64::MAX),
            (pm_id as i64, corp_roles::ROLE_PROJECT_MANAGER),
            (grunt_id as i64, 0),
        ]);
        snap.corporations.insert(corp_a as i64, corp1);

        let mut corp2 = template_corp;
        corp2.corporation_id = corp_b as i64;
        corp2.name = "CorpB".into();
        corp2.ticker = "CRPB".into();
        corp2.ceo_id = outsider_id as i64;
        corp2.ensure_wallet_divisions();
        corp2.set_division_balance(1, IskCents::from_isk(1_000_000));
        corp2.members = [outsider_id as i64].into_iter().collect();
        corp2.member_roles =
            std::collections::HashMap::from([(outsider_id as i64, u64::MAX)]);
        snap.corporations.insert(corp_b as i64, corp2);

        snap.corp_governance
            .entry(corp_a as i64)
            .or_default()
            .member_account_keys
            .insert(pm_id as i64, 1002); // div = 1002 - 999 = 3
    });

    let no_auth_ctx = AuthoritativeContext::default();
    let ceo_ctx = AuthoritativeContext::for_session(Some(1), Some(ceo_id), Some(corp_a), 0, "tranquility", vec![1]);
    let grunt_ctx = AuthoritativeContext::for_session(Some(2), Some(grunt_id), Some(corp_a), 0, "tranquility", vec![1]);
    let pm_no_take_ctx = AuthoritativeContext::for_session(
        Some(3),
        Some(pm_id),
        Some(corp_a),
        corp_roles::ROLE_PROJECT_MANAGER,
        "tranquility",
        vec![1],
    );
    let pm_with_take_ctx = AuthoritativeContext::for_session(
        Some(4),
        Some(pm_id),
        Some(corp_a),
        corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2],
        "tranquility",
        vec![1],
    );
    let outsider_ctx = AuthoritativeContext::for_session(
        Some(5),
        Some(outsider_id),
        Some(corp_b),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let mut gm_no_corp_ctx = AuthoritativeContext::for_session(
        Some(6),
        Some(999_888), // char does not exist in DB -> corpid == None
        None,
        0,
        "tranquility",
        vec![1],
    );
    gm_no_corp_ctx.service_role_mask = u64::MAX;

    // 1. CreateRequest guards & defaults
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.CreateRequest", &no_auth_ctx, &DynamicQuasarMessage::new("C"))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    // Non-admin CEO cannot seed initial progress > 0
    let mut seed_prog_msg = DynamicQuasarMessage::new("C");
    seed_prog_msg.fields.insert(5, DynamicValue::Int64(5));
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.CreateRequest", &ceo_ctx, &seed_prog_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    // CEO without ROLE_DIRECTOR creates goal WITH escrow cleanly and defaults whitespace name / target <= 0
    let mut ceo_escrow_msg = DynamicQuasarMessage::new("C");
    ceo_escrow_msg.fields.insert(1, DynamicValue::String("   ".into())); // -> "Organization Goal"
    ceo_escrow_msg.fields.insert(3, DynamicValue::Int64(0)); // -> 100
    ceo_escrow_msg.fields.insert(8, DynamicValue::Int64(10_000));
    call_gw(&router, "eve_public.goal.api.CreateRequest", &ceo_ctx, &ceo_escrow_msg)
        .expect("CEO can fund escrow without ROLE_ACCOUNT_CAN_TAKE");
    db.with_snapshot(|snap| {
        assert_eq!(
            snap.corporations[&(corp_a as i64)].division_balance(1),
            IskCents::from_isk(490_000)
        );
    });

    // Project Manager WITHOUT wallet division 3 take role is rejected on escrow > 0, succeeds on escrow == 0
    let mut pm_zero_msg = DynamicQuasarMessage::new("C");
    pm_zero_msg.fields.insert(1, DynamicValue::String("PM No Escrow".into()));
    pm_zero_msg.fields.insert(3, DynamicValue::Int64(80));
    call_gw(&router, "eve_public.goal.api.CreateRequest", &pm_no_take_ctx, &pm_zero_msg)
        .expect("PM can create zero-escrow goal");

    let mut pm_escrow_msg = DynamicQuasarMessage::new("C");
    pm_escrow_msg.fields.insert(1, DynamicValue::String("PM Escrow".into()));
    pm_escrow_msg.fields.insert(8, DynamicValue::Int64(25_000));
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.CreateRequest", &pm_no_take_ctx, &pm_escrow_msg)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );

    // Give PM ROLE_ACCOUNT_CAN_TAKE[2] in DB roles too so effective_corp_member_roles merges it
    db.with_mut_snapshot(|snap| {
        if let Some(c1) = snap.corporations.get_mut(&(corp_a as i64)) {
            c1.member_roles.insert(pm_id as i64, corp_roles::ROLE_PROJECT_MANAGER | corp_roles::ROLE_ACCOUNT_CAN_TAKE[2]);
        }
    });
    call_gw(&router, "eve_public.goal.api.CreateRequest", &pm_with_take_ctx, &pm_escrow_msg)
        .expect("PM with div 3 take role funds escrow from div 3");
    db.with_snapshot(|snap| {
        assert_eq!(
            snap.corporations[&(corp_a as i64)].division_balance(3),
            IskCents::from_isk(225_000)
        );
    });

    // GM with corpid == None uses alloc_goal_id() else-branch AND clamps initial_prog to target
    let mut gm_no_corp_msg = DynamicQuasarMessage::new("C");
    gm_no_corp_msg.fields.insert(1, DynamicValue::String("GM No Corp".into()));
    gm_no_corp_msg.fields.insert(3, DynamicValue::Int64(50));
    gm_no_corp_msg.fields.insert(5, DynamicValue::Uint64(999)); // clamped to 50
    let gm_rsp = call_gw(&router, "eve_public.goal.api.CreateRequest", &gm_no_corp_ctx, &gm_no_corp_msg).unwrap();
    let gm_id_b = wm_field_bytes(&gm_rsp.value, 1).unwrap();
    let gm_uuid_vec = wm_field_bytes(&gm_id_b, 1).unwrap();
    let mut gm_uuid = [0u8; 16];
    gm_uuid.copy_from_slice(&gm_uuid_vec);
    assert_eq!(mgr.generic_goals.read().unwrap()[&uuid_to_hex(&gm_uuid)].current_progress, 50);

    // 2. Outsider cannot GetRequest corp_a goal, grunt CAN GetRequest corp_a goal
    let corp_a_goal = mgr
        .generic_goals
        .read()
        .unwrap()
        .values()
        .find(|g| g.name == "PM Escrow")
        .unwrap()
        .uuid;
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.GetRequest", &outsider_ctx, &uuid_msg(1, corp_a_goal))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    call_gw(&router, "eve_public.goal.api.GetRequest", &grunt_ctx, &uuid_msg(1, corp_a_goal))
        .expect("same-corp grunt can view goal");

    // 3. Create outsider goal in corp_b, verify GetAllRequest filters out corp_b for grunt yet GM sees all,
    // and creator can still see own goal if creator_id matches!
    let mut out_create = DynamicQuasarMessage::new("C");
    out_create.fields.insert(1, DynamicValue::String("CorpB Goal".into()));
    out_create.fields.insert(3, DynamicValue::Uint64(25));
    call_gw(&router, "eve_public.goal.api.CreateRequest", &outsider_ctx, &out_create).unwrap();
    let outsider_goal_uuid = mgr
        .generic_goals
        .read()
        .unwrap()
        .values()
        .find(|g| g.name == "CorpB Goal")
        .unwrap()
        .uuid;

    let grunt_all = call_gw(&router, "eve_public.goal.api.GetAllRequest", &grunt_ctx, &DynamicQuasarMessage::new("A")).unwrap();
    let grunt_ids: Vec<Vec<u8>> = wm_all_bytes(&grunt_all.value, 3)
        .into_iter()
        .map(|entry| wm_field_bytes(&wm_field_bytes(&entry, 1).unwrap(), 1).unwrap())
        .collect();
    assert!(!grunt_ids.contains(&outsider_goal_uuid.to_vec()));
    assert!(grunt_ids.contains(&corp_a_goal.to_vec()));
    // Verify UUID sort ordering in GetAllResponse
    let mut sorted_ids = grunt_ids.clone();
    sorted_ids.sort();
    assert_eq!(grunt_ids, sorted_ids);

    // 4. DB-backed state precedence & field checks in load_live_goal:
    // det_creator == 0 falls back to cached.creator_id;
    // Active (1, field 10), Closed (2, field 11), Completed (3, field 11), Expired (4, field 11), Deleted -> 404
    let mut seeded_uuids = Vec::new();
    db.with_mut_snapshot(|snap| {
        for (idx, (curr, targ, closed, expired, status)) in [
            (99u64, 100u64, false, false, "Active"),   // -> state 1, not_finished field 10
            (100, 100, false, false, "Active"),         // curr >= targ -> state 3, field 11
            (50, 100, true, false, "Active"),           // closed -> state 2, field 11
            (100, 100, true, true, "Closed"),           // expired beats closed/completed -> state 4, field 11
            (10, 100, false, false, "Expired"),         // status=="Expired" -> state 4
        ]
        .into_iter()
        .enumerate()
        {
            let pid = 7_500 + idx as i64;
            snap.corp_projects.insert(
                pid,
                CorpProjectRecord {
                    project_id: pid,
                    corporation_id: corp_a as i64,
                    name: format!("State_{idx}"),
                    target_quantity: targ,
                    current_quantity: curr,
                    isk_per_unit: IskCents(0),
                    escrow_remaining: IskCents(0),
                },
            );
            snap.alliance_war_state
                .project_meta
                .insert(pid, ("SalvageWrecks".into(), false, curr >= targ));
            let det = snap.alliance_war_state.project_details.entry(pid).or_default();
            det.description = format!("Desc_{idx}");
            det.closed = closed;
            det.expired = expired;
            det.status = status.into();
            det.creator_id = 0; // force fallback to cached creator_id if present
            let u = social_corp_projects::corp_goal_uuid(pid);
            seeded_uuids.push(u);
        }
    });
    // Cache fallback creator_id on State_0
    mgr.generic_goals.write().unwrap().insert(
        uuid_to_hex(&seeded_uuids[0]),
        GenericGoalRecord {
            uuid: seeded_uuids[0],
            creator_id: pm_id,
            name: "State_0".into(),
            description: String::new(),
            desired_progress: 100,
            current_progress: 99,
            closed: false,
            matcher_type: "SalvageWrecks".into(),
            filter_id: None,
            created_unix: 0,
            contribution_config_pb: Vec::new(),
        },
    );

    for (idx, expected_state) in [(0usize, 1u64), (1, 3), (2, 2), (3, 4), (4, 4)] {
        let rsp = call_gw(
            &router,
            "eve_public.goal.api.GetRequest",
            &ceo_ctx,
            &uuid_msg(1, seeded_uuids[idx]),
        )
        .unwrap();
        let attrs = wm_field_bytes(&rsp.value, 1).unwrap();
        assert_eq!(wm_field_varint(&attrs, 6), Some(expected_state), "state idx {idx}");
        assert_eq!(wm_field_str(&attrs, 2).as_deref(), Some(format!("State_{idx}").as_str()));
        assert_eq!(wm_field_str(&attrs, 3).as_deref(), Some(format!("Desc_{idx}").as_str()));
        if expected_state == 1 {
            assert!(wm_has_field(&attrs, 10));
            assert!(!wm_has_field(&attrs, 11));
            let creator_msg = wm_field_bytes(&attrs, 4).unwrap();
            assert_eq!(wm_field_varint(&creator_msg, 1), Some(pm_id as u64));
        } else {
            assert!(wm_has_field(&attrs, 11));
            assert!(!wm_has_field(&attrs, 10));
        }
    }

    // Deleted flag in project_meta hides DB-backed project from GetRequest and GetAllRequest
    db.with_mut_snapshot(|snap| {
        snap.alliance_war_state
            .project_meta
            .insert(7_501, ("SalvageWrecks".into(), true, false));
    });
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.GetRequest", &ceo_ctx, &uuid_msg(1, seeded_uuids[1]))
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );
}

#[test]
fn goal_mutations_notices_capacity_contribs_and_community_tales_wire_payloads() {
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

    let creator_id = 90_000_001_u32;
    let pm_id = 90_000_002_u32;
    let contrib2_id = 90_000_020_u32;
    let outsider_id = 90_000_099_u32;
    let corp_a = 1_000_009_u32;
    let corp_b = 1_000_099_u32;

    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&(creator_id as i64)) {
            ch.corporation_id = corp_a as i64;
        }
        if let Some(ch) = snap.characters.get_mut(&(pm_id as i64)) {
            ch.corporation_id = corp_a as i64;
        }
        let base_char = snap.characters[&(creator_id as i64)].clone();
        let mut out_char = base_char;
        out_char.character_id = outsider_id as i64;
        out_char.corporation_id = corp_b as i64;
        snap.characters.insert(outsider_id as i64, out_char);

        let template_corp = snap.corporations.values().next().unwrap().clone();
        let mut corp1 = template_corp.clone();
        corp1.corporation_id = corp_a as i64;
        corp1.name = "CorpA".into();
        corp1.ticker = "CRPA".into();
        corp1.ceo_id = creator_id as i64;
        corp1.ensure_wallet_divisions();
        corp1.members = [creator_id as i64, pm_id as i64].into_iter().collect();
        corp1.member_roles = std::collections::HashMap::from([
            (creator_id as i64, u64::MAX),
            (pm_id as i64, corp_roles::ROLE_PROJECT_MANAGER),
        ]);
        snap.corporations.insert(corp_a as i64, corp1);

        let mut corp2 = template_corp;
        corp2.corporation_id = corp_b as i64;
        corp2.name = "CorpB".into();
        corp2.ticker = "CRPB".into();
        corp2.ceo_id = outsider_id as i64;
        corp2.ensure_wallet_divisions();
        corp2.members = [outsider_id as i64].into_iter().collect();
        corp2.member_roles =
            std::collections::HashMap::from([(outsider_id as i64, u64::MAX)]);
        snap.corporations.insert(corp_b as i64, corp2);
    });

    let mut gm_ctx = AuthoritativeContext::for_session(
        Some(1),
        Some(creator_id),
        Some(corp_a),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    gm_ctx.service_role_mask = u64::MAX;

    let pm_ctx = AuthoritativeContext::for_session(
        Some(2),
        Some(pm_id),
        Some(corp_a),
        corp_roles::ROLE_PROJECT_MANAGER,
        "tranquility",
        vec![1],
    );
    let outsider_ctx = AuthoritativeContext::for_session(
        Some(3),
        Some(outsider_id),
        Some(corp_b),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
    );
    let no_char_ctx = AuthoritativeContext::default();

    // Create 3 goals by creator_id (one will be closed) and 1 by pm_id
    let mk_goal = |ctx: &AuthoritativeContext, name: &str, target: u64| -> [u8; 16] {
        let mut m = DynamicQuasarMessage::new("Create");
        m.fields.insert(1, DynamicValue::String(name.into()));
        m.fields.insert(3, DynamicValue::Uint64(target));
        let rsp = call_gw(&router, "eve_public.goal.api.CreateRequest", ctx, &m).unwrap();
        let id_b = wm_field_bytes(&rsp.value, 1).unwrap();
        let u_vec = wm_field_bytes(&id_b, 1).unwrap();
        let mut u = [0u8; 16];
        u.copy_from_slice(&u_vec);
        u
    };
    let g1 = mk_goal(&gm_ctx, "Goal1", 100);
    let g2 = mk_goal(&gm_ctx, "Goal2", 50);
    let g3 = mk_goal(&gm_ctx, "Goal3Closed", 50);
    let g_pm = mk_goal(&pm_ctx, "PMGoal", 40);

    // 1. SetNameRequest rejects blank/whitespace; SetDescriptionRequest allows empty
    let mut bad_rename = uuid_msg(1, g1);
    bad_rename.fields.insert(2, DynamicValue::String("   \t".into()));
    assert_eq!(
        call_gw(&router, "eve.goal.api.SetNameRequest", &gm_ctx, &bad_rename)
            .unwrap_err()
            .0,
        STATUS_BAD_REQUEST
    );
    let mut clear_desc = uuid_msg(1, g1);
    clear_desc.fields.insert(2, DynamicValue::String(String::new()));
    let _ = crate::services::quasar_social_chat::drain_local_notices();
    call_gw(&router, "eve.goal.api.SetDescriptionRequest", &gm_ctx, &clear_desc).unwrap();
    let desc_notices = crate::services::quasar_social_chat::drain_local_notices();
    assert!(desc_notices.iter().any(|(_, a, _)| a.type_url.ends_with("DescriptionChangedNotice")));

    // 2. SetCurrentProgressRequest: noop (prev == capped) emits NO notices; completion (prev < desired && capped == desired) emits BOTH & caps overflow
    let _ = crate::services::quasar_social_chat::drain_local_notices();
    let mut noop_prog = uuid_msg(1, g1);
    noop_prog.fields.insert(2, DynamicValue::Uint64(0));
    noop_prog.fields.insert(3, DynamicValue::Uint64(0));
    call_gw(&router, "eve_public.goal.api.SetCurrentProgressRequest", &gm_ctx, &noop_prog).unwrap();
    assert!(crate::services::quasar_social_chat::drain_local_notices().is_empty());

    let mut complete_prog = uuid_msg(1, g1);
    complete_prog.fields.insert(2, DynamicValue::Uint64(0));
    complete_prog.fields.insert(3, DynamicValue::Uint64(999)); // clamped to desired=100
    call_gw(&router, "eve_public.goal.api.SetCurrentProgressRequest", &gm_ctx, &complete_prog).unwrap();
    let prog_notices = crate::services::quasar_social_chat::drain_local_notices();
    assert!(prog_notices.iter().any(|(_, a, _)| a.type_url.ends_with("ProgressedNotice")));
    assert!(prog_notices.iter().any(|(_, a, _)| a.type_url.ends_with("CompletedNotice")));
    let pid1 = social_corp_projects::goal_uuid_to_project_id(&g1).unwrap();
    db.with_snapshot(|snap| {
        assert_eq!(snap.corp_projects[&pid1].current_quantity, 100);
        assert!(snap.alliance_war_state.project_meta[&pid1].2);
        assert_eq!(snap.alliance_war_state.project_details[&pid1].status, "Completed");
    });
    // Repeat SetCurrentProgressRequest when already completed (prev == desired, expected = 100, new = 999 -> capped == 100 == prev) -> NO notices!
    let mut repeat_done = uuid_msg(1, g1);
    repeat_done.fields.insert(2, DynamicValue::Uint64(100));
    repeat_done.fields.insert(3, DynamicValue::Uint64(999));
    call_gw(&router, "eve_public.goal.api.SetCurrentProgressRequest", &gm_ctx, &repeat_done).unwrap();
    assert!(crate::services::quasar_social_chat::drain_local_notices().is_empty());

    // 3. CloseRequest auth & idempotency & notice check + DeleteRequest outsider deny
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.CloseRequest", &no_char_ctx, &uuid_msg(1, g3))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.CloseRequest", &outsider_ctx, &uuid_msg(1, g3))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.DeleteRequest", &outsider_ctx, &uuid_msg(1, g_pm))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    // Same-corp Project Manager (not creator, not GM) can close g3
    let _ = crate::services::quasar_social_chat::drain_local_notices();
    call_gw(&router, "eve_public.goal.api.CloseRequest", &pm_ctx, &uuid_msg(1, g3)).unwrap();
    let close_notices_1 = crate::services::quasar_social_chat::drain_local_notices();
    assert_eq!(close_notices_1.iter().filter(|(_, a, _)| a.type_url.ends_with("ClosedNotice")).count(), 1);
    // Second CloseRequest on already closed g3 does NOT emit duplicate ClosedNotice
    call_gw(&router, "eve_public.goal.api.CloseRequest", &pm_ctx, &uuid_msg(1, g3)).unwrap();
    assert!(crate::services::quasar_social_chat::drain_local_notices().is_empty());

    // 4. GetCapacityRequest checks active count created by caller only (g1, g2 = 2; g3 closed, g_pm other creator excluded)
    assert_eq!(
        call_gw(&router, "eve_public.goal.api.GetCapacityRequest", &no_char_ctx, &DynamicQuasarMessage::new("C"))
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    let cap_rsp = call_gw(&router, "eve_public.goal.api.GetCapacityRequest", &gm_ctx, &DynamicQuasarMessage::new("C")).unwrap();
    assert_eq!(wm_field_varint(&cap_rsp.value, 1), Some(2));
    assert_eq!(wm_field_varint(&cap_rsp.value, 2), Some(50));

    // 5. Contributor summaries: char_contributions sorting/filtering, cross-corp auth, creator fallback vs 0-progress
    let pid2 = social_corp_projects::goal_uuid_to_project_id(&g2).unwrap();
    db.with_mut_snapshot(|snap| {
        let det = snap.alliance_war_state.project_details.entry(pid2).or_default();
        det.char_contributions.insert(0, 999);          // filtered out (c <= 0)
        det.char_contributions.insert(pm_id as i64, 0); // filtered out (p <= 0)
        det.char_contributions.insert(contrib2_id as i64, 30);
        det.char_contributions.insert(creator_id as i64, 15);
    });
    // Cross-corp outsider blocked from viewing g2 contributor summaries
    assert_eq!(
        call_gw(
            &router,
            "eve_public.goal.contribution.api.GetContributorSummariesForGoalRequest",
            &outsider_ctx,
            &uuid_msg(1, g2),
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );
    let sum_for_goal = call_gw(
        &router,
        "eve_public.goal.contribution.api.GetContributorSummariesForGoalRequest",
        &pm_ctx,
        &uuid_msg(1, g2),
    )
    .unwrap();
    let summaries = wm_all_bytes(&sum_for_goal.value, 1);
    let pairs: Vec<(u64, u64)> = summaries
        .into_iter()
        .map(|s| {
            let cid = wm_field_varint(&wm_field_bytes(&s, 1).unwrap(), 1).unwrap();
            let prog = wm_field_varint(&s, 2).unwrap();
            (cid, prog)
        })
        .collect();
    // Sorted by char_id, excluding 0 and zero-progress entries
    assert_eq!(pairs, vec![(creator_id as u64, 15), (contrib2_id as u64, 30)]);

    let my_for_pm = call_gw(
        &router,
        "eve_public.goal.contribution.api.GetMyContributorSummaryForGoalRequest",
        &pm_ctx,
        &uuid_msg(1, g2),
    )
    .unwrap();
    let my_pm_entry = wm_field_bytes(&my_for_pm.value, 1).unwrap();
    assert_eq!(wm_field_varint(&my_pm_entry, 2), Some(0));

    let my_for_creator = call_gw(
        &router,
        "eve_public.goal.contribution.api.GetMyContributorSummaryForGoalRequest",
        &gm_ctx,
        &uuid_msg(1, g2),
    )
    .unwrap();
    let my_cr_entry = wm_field_bytes(&my_for_creator.value, 1).unwrap();
    assert_eq!(wm_field_varint(&my_cr_entry, 2), Some(15));

    // GetMyContributorSummariesRequest returns g1 (creator fallback progress = 100) + g2 (char_contributions = 15), omitting g3/g_pm (0)
    let my_all = call_gw(
        &router,
        "eve_public.goal.contribution.api.GetMyContributorSummariesRequest",
        &gm_ctx,
        &DynamicQuasarMessage::new("Mine"),
    )
    .unwrap();
    let my_all_entries = wm_all_bytes(&my_all.value, 1);
    assert_eq!(my_all_entries.len(), 2);

    // 6. Wire payload decoding for Community Track GetRequest (public vs internal, template_id==0 vs !=0) & Tales
    mgr.community_tracks.write().unwrap().insert(
        555,
        CommunityTrackState {
            track_id: 555,
            template_id: 0, // tests `if track.template_id != 0 { template_id } else { track_id }`
            name: "ZeroTplTrack".into(),
            target_progress: 120,
            current_progress: 40,
            active: false,
            tale_templates: vec![770],
        },
    );
    let pub_tr = call_gw(
        &router,
        "eve_public.communitytrack.api.GetRequest",
        &pm_ctx,
        &seq_msg(1, DynamicValue::Uint32(555)),
    )
    .unwrap();
    let pub_attrs = wm_field_bytes(&pub_tr.value, 1).unwrap();
    let pub_def = wm_field_bytes(&pub_attrs, 1).unwrap();
    assert_eq!(wm_field_varint(&pub_def, 1), Some(555));
    assert_eq!(wm_field_varint(&pub_attrs, 2), Some(40));
    assert_eq!(wm_field_varint(&pub_attrs, 3), Some(120));
    assert_eq!(wm_field_str(&pub_attrs, 4).as_deref(), Some("ZeroTplTrack"));
    assert_eq!(wm_field_varint(&pub_attrs, 5), Some(0)); // active == false -> 0

    let int_tr = call_gw(
        &router,
        "eve.communitytrack.api.GetRequest",
        &pm_ctx,
        &seq_msg(1, DynamicValue::Uint32(555)),
    )
    .unwrap();
    let int_attrs = wm_field_bytes(&int_tr.value, 1).unwrap();
    assert_eq!(wm_field_varint(&int_attrs, 1), Some(555));
    assert_eq!(wm_field_str(&int_attrs, 2).as_deref(), Some("ZeroTplTrack"));
    assert_eq!(wm_field_varint(&int_attrs, 3), Some(120));
    assert_eq!(wm_field_varint(&int_attrs, 4), Some(40));

    // Activate 555 and stop 2 tales of template 770 -> adds +2 and stops 0 tales emits nothing
    call_gw(&router, "eve.communitytrack.api.StartRequest", &gm_ctx, &seq_msg(1, DynamicValue::Uint32(555))).unwrap();
    let valid_sys = *sde.solar_systems.keys().next().unwrap_or(&30_000_142);
    for _ in 0..2 {
        let mut st_req = seq_msg(1, DynamicValue::Uint32(770));
        let mut s_in = DynamicQuasarMessage::new("Identifier");
        s_in.fields.insert(1, DynamicValue::Uint32(valid_sys));
        st_req.fields.insert(2, DynamicValue::Message(Box::new(s_in)));
        call_gw(&router, "eve.tale.api.StartTaleRequest", &gm_ctx, &st_req).unwrap();
    }
    let _ = crate::services::quasar_social_chat::drain_local_notices();
    call_gw(&router, "eve.tale.api.StopTalesRequest", &gm_ctx, &seq_msg(1, DynamicValue::Uint32(770))).unwrap();
    assert_eq!(mgr.get_community_track(555).unwrap().current_progress, 42);
    let stop_notices = crate::services::quasar_social_chat::drain_local_notices();
    assert_eq!(stop_notices.iter().filter(|(_, a, _)| a.type_url.ends_with("StoppedNotice")).count(), 2);
    assert!(stop_notices.iter().any(|(_, a, _)| a.type_url.ends_with("ProgressedNotice")));
    // Stopping unmatched template leaves notices empty
    call_gw(&router, "eve.tale.api.StopTalesRequest", &gm_ctx, &seq_msg(1, DynamicValue::Uint32(999_111))).unwrap();
    assert!(crate::services::quasar_social_chat::drain_local_notices().is_empty());

    // 7. Seasonal challenges: missing char ID -> 400; admin querying another char -> Ok;
    // CloseRequest/DeleteRequest when db == None uses corp_state fallback cleanly
    assert_eq!(
        call_gw(
            &router,
            "eve.seasonalevent.challenge.GetChallengesForCharacterRequest",
            &no_char_ctx,
            &DynamicQuasarMessage::new("Empty"),
        )
        .unwrap_err()
        .0,
        STATUS_BAD_REQUEST
    );
    let admin_other_chal = call_gw(
        &router,
        "eve.seasonalevent.challenge.GetChallengesForCharacterRequest",
        &gm_ctx,
        &seq_msg(1, DynamicValue::Uint32(pm_id)),
    )
    .expect("admin can query another character challenges");
    for chal_bytes in wm_all_bytes(&admin_other_chal.value, 1) {
        assert!(wm_field_varint(&chal_bytes, 1).is_some());
        assert!(wm_field_varint(&chal_bytes, 2).is_some());
    }

    // Separate router with mgr.db == None + corp_state == Some to cover no-DB branches on Close/Delete/StartTale/SeasonalChallenges
    let no_db_router = PublicGatewayRouter::new(
        Arc::new(QuasarDescriptorRegistry::empty_with_builtins()),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let mut no_db_mgr = ProgressionExtraManager::new(None);
    no_db_mgr.corp_state = Some(Arc::clone(&corp_state));
    let no_db_mgr = Arc::new(no_db_mgr);
    register_progression_extra_handlers(&no_db_router, Arc::clone(&db), Arc::clone(&no_db_mgr));
    let nd_uuid = social_corp_projects::corp_goal_uuid(888);
    no_db_mgr.generic_goals.write().unwrap().insert(
        uuid_to_hex(&nd_uuid),
        GenericGoalRecord {
            uuid: nd_uuid,
            creator_id,
            name: "NoDbGoal".into(),
            desired_progress: 10,
            current_progress: 1,
            closed: false,
            matcher_type: "Manual".into(),
            ..Default::default()
        },
    );
    call_gw(&no_db_router, "eve_public.goal.api.CloseRequest", &gm_ctx, &uuid_msg(1, nd_uuid)).unwrap();
    call_gw(&no_db_router, "eve_public.goal.api.DeleteRequest", &gm_ctx, &uuid_msg(1, nd_uuid)).unwrap();
    let nd_chal = call_gw(
        &no_db_router,
        "eve.seasonalevent.challenge.GetChallengesForCharacterRequest",
        &gm_ctx,
        &DynamicQuasarMessage::new("Req"),
    )
    .unwrap();
    assert!(nd_chal.value.is_empty());
    let mut any_sys_tale = seq_msg(1, DynamicValue::Uint32(404));
    let mut s_any = DynamicQuasarMessage::new("Identifier");
    s_any.fields.insert(1, DynamicValue::Uint32(99_999_999));
    any_sys_tale.fields.insert(2, DynamicValue::Message(Box::new(s_any)));
    call_gw(&no_db_router, "eve.tale.api.StartTaleRequest", &gm_ctx, &any_sys_tale)
        .expect("StartTaleRequest succeeds when mgr.sde is None");
    let _ = crate::services::quasar_social_chat::drain_local_notices();

    // Verify load_live_goal uses `det.closed || det.status == "Closed"` for was_open check in CloseRequest (kills L759)
    let pid_status_closed = 7_901_i64;
    let pid_bool_closed = 7_902_i64;
    let pid_open = 7_903_i64;
    let u_status_closed = social_corp_projects::corp_goal_uuid(pid_status_closed);
    let u_bool_closed = social_corp_projects::corp_goal_uuid(pid_bool_closed);
    let u_open = social_corp_projects::corp_goal_uuid(pid_open);
    db.with_mut_snapshot(|snap| {
        for (pid, det_closed, status) in [
            (pid_status_closed, false, "Closed"),
            (pid_bool_closed, true, "Active"),
            (pid_open, false, "Active"),
        ] {
            snap.corp_projects.insert(
                pid,
                CorpProjectRecord {
                    project_id: pid,
                    corporation_id: corp_a as i64,
                    name: format!("CloseCheck_{pid}"),
                    target_quantity: 100,
                    current_quantity: 10,
                    isk_per_unit: IskCents(0),
                    escrow_remaining: IskCents(0),
                },
            );
            snap.alliance_war_state
                .project_meta
                .insert(pid, ("SalvageWrecks".into(), false, false));
            let det = snap.alliance_war_state.project_details.entry(pid).or_default();
            det.closed = det_closed;
            det.status = status.into();
            det.creator_id = creator_id as i64;
        }
    });
    let _ = crate::services::quasar_social_chat::drain_local_notices();
    call_gw(&router, "eve_public.goal.api.CloseRequest", &gm_ctx, &uuid_msg(1, u_status_closed)).unwrap();
    assert!(crate::services::quasar_social_chat::drain_local_notices().is_empty());
    call_gw(&router, "eve_public.goal.api.CloseRequest", &gm_ctx, &uuid_msg(1, u_bool_closed)).unwrap();
    assert!(crate::services::quasar_social_chat::drain_local_notices().is_empty());
    call_gw(&router, "eve_public.goal.api.CloseRequest", &gm_ctx, &uuid_msg(1, u_open)).unwrap();
    let closed_notices = crate::services::quasar_social_chat::drain_local_notices();
    assert!(closed_notices.iter().any(|(_, a, _)| a.type_url.ends_with("ClosedNotice")));
}

#[test]
fn progression_save_rehydrate_template_id_contrib_pb_creator_fallback_and_created_unix_mutants() {
    let sde = shared_sde();
    let db = Arc::new(WorldRepository::with_demo_seed());
    let corp_state = QuasarCorpState::from_db(Arc::clone(&db));
    let router = PublicGatewayRouter::new(
        Arc::new(QuasarDescriptorRegistry::empty_with_builtins()),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let mgr1 = Arc::new(ProgressionExtraManager::new_with_sde(
        Some(Arc::clone(&db)),
        Some(Arc::clone(&sde)),
        Some(Arc::clone(&corp_state)),
    ));

    // 1. save_to_db + rehydrate_from_db preserving nonzero template_id while migrating == 0 (kills L203, L238, L279)
    mgr1.community_tracks.write().unwrap().insert(
        610,
        CommunityTrackState {
            track_id: 610,
            template_id: 777,
            name: "NonZeroTpl".into(),
            target_progress: 50,
            current_progress: 10,
            active: true,
            tale_templates: vec![12],
        },
    );
    mgr1.community_tracks.write().unwrap().insert(
        611,
        CommunityTrackState {
            track_id: 611,
            template_id: 0,
            name: "ZeroTpl".into(),
            target_progress: 50,
            current_progress: 5,
            active: true,
            tale_templates: Vec::new(),
        },
    );
    mgr1.save_to_db();

    let mgr2 = Arc::new(ProgressionExtraManager::new_with_sde(
        Some(Arc::clone(&db)),
        Some(Arc::clone(&sde)),
        Some(Arc::clone(&corp_state)),
    ));
    mgr2.community_tracks.write().unwrap().clear();
    mgr2.rehydrate_from_db();
    assert_eq!(mgr2.get_community_track(610).unwrap().template_id, 777);
    assert_eq!(mgr2.get_community_track(611).unwrap().template_id, 611);

    register_progression_extra_handlers(&router, Arc::clone(&db), Arc::clone(&mgr2));

    let corp_a = 1_000_009_u32;
    let cid = 90_000_001_u32;
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&(cid as i64)) {
            ch.corporation_id = corp_a as i64;
        }
        // Negative det.creator_id forces fallback to cached creator_id; created_unix == 0 vs > 0 tested in Get/GetAll (kills L835, L1021, L1075)
        for (pid, curr, targ, det_creator, c_unix) in [
            (7_911_i64, 50_u64, 50_u64, -5_i64, 0_u64),
            (7_912_i64, 49_u64, 50_u64, 90_000_002_i64, 1_650_000_123_u64),
        ] {
            snap.corp_projects.insert(
                pid,
                CorpProjectRecord {
                    project_id: pid,
                    corporation_id: corp_a as i64,
                    name: format!("MutGoal_{pid}"),
                    target_quantity: targ,
                    current_quantity: curr,
                    isk_per_unit: IskCents(0),
                    escrow_remaining: IskCents(0),
                },
            );
            snap.alliance_war_state
                .project_meta
                .insert(pid, ("Manual".into(), false, curr >= targ));
            let det = snap.alliance_war_state.project_details.entry(pid).or_default();
            det.creator_id = det_creator;
            det.created_unix = c_unix;
            det.status = "Active".into();
        }
    });

    let u1 = social_corp_projects::corp_goal_uuid(7_911);
    let u2 = social_corp_projects::corp_goal_uuid(7_912);
    let custom_contrib_pb = vec![0x08_u8, 0x2a, 0x10, 0x05];
    mgr2.generic_goals.write().unwrap().insert(
        uuid_to_hex(&u1),
        GenericGoalRecord {
            uuid: u1,
            creator_id: cid,
            name: "MutGoal_7911".into(),
            description: String::new(),
            desired_progress: 50,
            current_progress: 50,
            closed: false,
            matcher_type: "Manual".into(),
            filter_id: None,
            created_unix: 0,
            contribution_config_pb: custom_contrib_pb.clone(),
        },
    );
    mgr2.generic_goals.write().unwrap().insert(
        uuid_to_hex(&u2),
        GenericGoalRecord {
            uuid: u2,
            creator_id: cid,
            name: "MutGoal_7912".into(),
            description: String::new(),
            desired_progress: 50,
            current_progress: 49,
            closed: false,
            matcher_type: "Manual".into(),
            filter_id: None,
            created_unix: 1_650_000_123,
            contribution_config_pb: Vec::new(),
        },
    );

    let caller_ctx = AuthoritativeContext::for_session_at(
        Some(1),
        Some(cid),
        Some(corp_a),
        corp_roles::ROLE_DIRECTOR,
        "tranquility",
        vec![1],
        1_760_000_000,
    );

    // GetRequest verifies resolved_contrib_pb preserves non-empty custom bytes AND timestamp fallback/preservation
    let g1_rsp = call_gw(&router, "eve_public.goal.api.GetRequest", &caller_ctx, &uuid_msg(1, u1)).unwrap();
    let a1 = wm_field_bytes(&g1_rsp.value, 1).unwrap();
    assert_eq!(wm_field_bytes(&a1, 7).as_deref(), Some(custom_contrib_pb.as_slice()));
    let ts1 = wm_field_varint(&wm_field_bytes(&a1, 1).unwrap(), 1).unwrap();
    assert!(ts1 > 1_600_000_000);
    assert_eq!(wm_field_varint(&wm_field_bytes(&a1, 4).unwrap(), 1), Some(cid as u64));

    let g2_rsp = call_gw(&router, "eve_public.goal.api.GetRequest", &caller_ctx, &uuid_msg(1, u2)).unwrap();
    let a2 = wm_field_bytes(&g2_rsp.value, 1).unwrap();
    let fallback_pb = crate::services::quasar_goal_tale_notices::encode_goal_contribution_config("Manual", None);
    assert_eq!(wm_field_bytes(&a2, 7).as_deref(), Some(fallback_pb.as_slice()));
    assert_eq!(wm_field_varint(&wm_field_bytes(&a2, 1).unwrap(), 1), Some(1_650_000_123));

    // GetAllRequest verifies negative det.creator_id fallback, exact progress == desired Completed, created_unix, contrib_pb (kills L718, L812, L835, L1068, L1075)
    let all_rsp = call_gw(
        &router,
        "eve_public.goal.api.GetAllRequest",
        &caller_ctx,
        &DynamicQuasarMessage::new("Req"),
    )
    .unwrap();
    let entries: Vec<(Vec<u8>, Vec<u8>)> = wm_all_bytes(&all_rsp.value, 3)
        .into_iter()
        .map(|entry| {
            let id_b = wm_field_bytes(&wm_field_bytes(&entry, 1).unwrap(), 1).unwrap();
            let attrs_b = wm_field_bytes(&entry, 2).unwrap();
            (id_b, attrs_b)
        })
        .collect();
    let e1 = &entries.iter().find(|(id, _)| id.as_slice() == u1).unwrap().1;
    let e2 = &entries.iter().find(|(id, _)| id.as_slice() == u2).unwrap().1;
    assert_eq!(wm_field_varint(e1, 6), Some(3)); // curr == target -> Completed
    assert_eq!(wm_field_varint(&wm_field_bytes(e1, 4).unwrap(), 1), Some(cid as u64));
    assert!(wm_field_varint(&wm_field_bytes(e1, 1).unwrap(), 1).unwrap() > 1_600_000_000);
    assert_eq!(wm_field_bytes(e1, 7).as_deref(), Some(custom_contrib_pb.as_slice()));

    assert_eq!(wm_field_varint(e2, 6), Some(1)); // 49 < 50 -> Active
    assert_eq!(wm_field_varint(&wm_field_bytes(e2, 4).unwrap(), 1), Some(90_000_002));
    assert_eq!(wm_field_varint(&wm_field_bytes(e2, 1).unwrap(), 1), Some(1_650_000_123));
}
