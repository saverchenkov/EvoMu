//! Unit tests for `quasar_goal_tale_notices`.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use crate::services::quasar_achievements::{AchievementConditionDef, AchievementDef};
use crate::services::quasar_progression::new_progression_managers;
use crate::services::quasar_progression_extra::CommunityTrackState;
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{QuasarDescriptorRegistry, WireMessage};
use eve_sde::SdeRegistry;
use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

fn shared_sde() -> Arc<SdeRegistry> {
    static SDE: OnceLock<Arc<SdeRegistry>> = OnceLock::new();
    Arc::clone(SDE.get_or_init(|| Arc::new(SdeRegistry::with_canonical_seed())))
}

#[test]
fn goal_and_tale_notice_wire_builders_and_scopes() {
    assert_eq!(CORP_GOAL_UUID_PREFIX, b"CORPGOAL");
    assert_eq!(GOAL_STATE_ACTIVE, 1);
    assert_eq!(GOAL_STATE_CLOSED, 2);
    assert_eq!(GOAL_STATE_COMPLETED, 3);

    assert_eq!(
        goal_scope(Some(98_000_001), 90_000_001),
        NoticeScope::Corporation(98_000_001)
    );
    assert_eq!(goal_scope(Some(0), 90_000_001), NoticeScope::Character(90_000_001));
    assert_eq!(goal_scope(None, 90_000_001), NoticeScope::Character(90_000_001));
    assert_eq!(tale_scope(), NoticeScope::Broadcast);

    let uuid = [0x42u8; 16];
    let contrib_pb = encode_goal_contribution_config("MineMaterials", Some(1230));
    let active_view = GoalAttributesView {
        created_unix: 1_760_000_000,
        creator_id: 90_000_001,
        corp_id: Some(98_000_001),
        name: "Mine Veldspar",
        description: "Deliver 1000 units",
        desired: 1_000,
        current: 250,
        state: None,
        contribution_config_pb: &contrib_pb,
    };
    let created_active = goal_created(&uuid, &active_view);
    assert!(created_active.type_url.ends_with("eve_public.goal.api.CreatedNotice"));
    let wire_active = WireMessage::decode(&created_active.value).expect("decode CreatedNotice wire");
    let attr_bytes = wire_active
        .fields
        .iter()
        .find(|f| f.field_number == 2)
        .and_then(|f| match &f.value {
            eve_proto::quasar::WireValue::LengthDelimited(b) => Some(b.as_slice()),
            _ => None,
        })
        .expect("CreatedNotice.goal attributes");
    let attr_wire = WireMessage::decode(attr_bytes).expect("decode Attributes wire");
    assert!(
        attr_wire.fields.iter().any(|f| f.field_number == 7),
        "CreatedNotice.goal must include field 7 (contribution_config)"
    );

    let done_view = GoalAttributesView {
        current: 1_000,
        corp_id: None,
        contribution_config_pb: &[],
        ..active_view
    };
    let created_done = goal_created(&uuid, &done_view);
    assert!(!created_done.value.is_empty());

    assert!(goal_deleted(&uuid).type_url.ends_with("eve_public.goal.api.DeletedNotice"));
    assert!(goal_closed(&uuid, 90_000_001).type_url.ends_with("eve_public.goal.api.ClosedNotice"));
    assert!(goal_completed(&uuid).type_url.ends_with("eve_public.goal.api.CompletedNotice"));
    assert!(goal_progressed(&uuid, 500).type_url.ends_with("eve_public.goal.api.ProgressedNotice"));
    assert!(goal_name_changed(&uuid, "NewName").type_url.ends_with("eve_public.goal.api.NameChangedNotice"));
    assert!(
        goal_description_changed(&uuid, "NewDesc")
            .type_url
            .ends_with("eve_public.goal.api.DescriptionChangedNotice")
    );
    assert!(
        goal_contributed(&uuid, 90_000_002, 250, 500, 1_760_000_050)
            .type_url
            .ends_with("eve_public.goal.contribution.api.ContributedNotice")
    );

    let t_start = tale_lifecycle(true, 10, 200, 30_000_142);
    assert!(t_start.type_url.ends_with("eve_public.tale.api.StartedNotice"));
    let t_stop = tale_lifecycle(false, 10, 200, 30_000_142);
    assert!(t_stop.type_url.ends_with("eve_public.tale.api.StoppedNotice"));
}

#[test]
fn opportunity_events_and_server_opportunity_recording() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let mgrs = new_progression_managers(Arc::clone(&db), Arc::clone(&sde));
    let desc = QuasarDescriptorRegistry::open_default_or_empty();
    let now = 1_760_000_000u64;
    let cid = 90_000_001u32;

    let mut sess = SessionState::new(1);
    sess.user_id = Some(1);
    sess.character_id = Some(cid as i64);
    sess.role = service_roles::ROLE_PLAYER_DEFAULT;

    // 1. Client Discovered event
    let mut disc_inner = Vec::new();
    encode_string_field(1, "opp_mining_1", &mut disc_inner);
    let mut disc_opp = Vec::new();
    encode_bytes_field(1, &disc_inner, &mut disc_opp);
    let mut disc_req = Vec::new();
    encode_bytes_field(1, &disc_opp, &mut disc_req);
    handle_opportunity_event(
        &mgrs,
        &desc,
        &sess,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.opportunity.api.Discovered"),
            value: disc_req,
        },
        now,
    );

    // 2. Client Tracked -> Viewed -> Untracked events
    let mut ch_msg = Vec::new();
    encode_varint_field(1, cid as u64, &mut ch_msg);
    let mut opp_var = Vec::new();
    encode_varint_field(1, 777, &mut opp_var);
    let mut opp_msg = Vec::new();
    encode_bytes_field(1, &opp_var, &mut opp_msg);
    let mut track_req = Vec::new();
    encode_bytes_field(1, &ch_msg, &mut track_req);
    encode_bytes_field(2, &opp_msg, &mut track_req);

    for ev in [
        "eve_public.opportunity.api.Tracked",
        "eve_public.opportunity.api.Viewed",
        "eve_public.opportunity.api.Untracked",
    ] {
        handle_opportunity_event(
            &mgrs,
            &desc,
            &sess,
            &GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}{ev}"),
                value: track_req.clone(),
            },
            now,
        );
    }

    // 3. Server-observed opportunity completion
    let defs = vec![AchievementDef {
        id: 5001,
        name: "Destroy 5 Pirates".into(),
        description: "Destroy 5 NPC pirates".into(),
        notification_text: String::new(),
        category_id: 1,
        target: 5,
        score: 10,
        milestone_targets: vec![],
        reward_isk: 0,
        reward_sp: 0,
        is_client_achievement: false,
        conditions: vec![AchievementConditionDef {
            condition_id: 1,
            statistic_event_id: 1,
            event_name: "npc_killed".into(),
            target_value: 5,
        }],
    }];
    record_server_opportunity_event(&db, &defs, 0, &["npc_killed"], 3, now);
    record_server_opportunity_event(&db, &defs, cid, &["npc_killed"], 3, now);
    record_server_opportunity_event(&db, &defs, cid, &["npc_killed"], 3, now + 10);

    let blob: serde_json::Value = db.with_snapshot(|s| {
        serde_json::from_str(s.quasar_state_blobs.get("quasar_opportunities").expect("opp blob")).unwrap()
    });
    let ch_obj = &blob[cid.to_string()];
    assert!(ch_obj["completed"].as_array().unwrap().contains(&serde_json::json!(5001)));
    assert_eq!(ch_obj["progress"]["5001:1"].as_u64(), Some(5));
}

#[test]
fn tale_matching_and_community_track_progression() {
    let sde = shared_sde();
    let mut event_tpls = HashSet::new();
    event_tpls.insert(8888);

    let inc_tpl = crate::services::pve_incursion_sites::INCURSION_TALE_TEMPLATE_ID as u32;
    assert!(tale_matches_completed_encounter(Some(&sde), &event_tpls, inc_tpl, "Incursion", 0));
    assert!(tale_matches_completed_encounter(Some(&sde), &event_tpls, 400, "Triglavian", 400));
    assert!(tale_matches_completed_encounter(Some(&sde), &event_tpls, 500, "Data", 500));
    assert!(!tale_matches_completed_encounter(Some(&sde), &event_tpls, 500, "Data", 501));
    assert!(tale_matches_completed_encounter(Some(&sde), &event_tpls, 8888, "CombatAnomaly", 0));
    assert!(!tale_matches_completed_encounter(Some(&sde), &event_tpls, 8888, "UnknownSite", 8888));

    let db = Arc::new(WorldRepository::with_demo_seed());
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let mgr = &router.progression.extra;
    mgr.active_tales.write().unwrap().insert(1, (8888, 30_000_142, 0));
    mgr.community_tracks.write().unwrap().insert(
        77,
        CommunityTrackState {
            track_id: 77,
            template_id: 77,
            name: "Community Track".into(),
            current_progress: 10,
            target_progress: 100,
            active: true,
            tale_templates: vec![8888],
        },
    );
    let progressed = complete_system_tale_encounter(
        mgr,
        30_000_142,
        "Incursion",
        8888,
        25,
        1_760_000_000,
        &router,
    );
    assert_eq!(progressed.len(), 1);
    assert_eq!(progressed[0].current_progress, 35);
}

#[test]
fn issue_765_goal_created_and_get_always_include_contribution_config() {
    use eve_proto::quasar::{
        encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest, GoogleAny,
        WireValue, TYPE_GOOGLEAPIS_PREFIX,
    };

    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let desc = QuasarDescriptorRegistry::open_default_or_empty();
    let router = PublicGatewayRouter::new(desc.clone(), Arc::clone(&db), Arc::clone(&sde));

    let cid = 90_000_001i64;
    let corp_id = 98_000_001i64;
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.entry(corp_id).or_default().ceo_id = cid;
    });

    let mut sess = SessionState::new(765);
    sess.user_id = Some(1);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.role = service_roles::ROLE_PLAYER_DEFAULT | service_roles::ROLE_ADMIN;
    let _rx = router.subscribe_session(&sess, Vec::new(), 1_760_000_000);

    // 1. CreateRequest with explicit deliver_item ContributionConfiguration (field 4)
    let deliver_pb = encode_goal_contribution_config("DeliverItem", Some(34));
    let mut create_deliver = Vec::new();
    encode_string_field(1, "Deliver Tritanium", &mut create_deliver);
    encode_string_field(2, "Bring minerals", &mut create_deliver);
    encode_varint_field(3, 500, &mut create_deliver);
    encode_bytes_field(4, &deliver_pb, &mut create_deliver);

    let req1 = EvePublicRequest {
        issued: None,
        correlation_uuid: vec![1, 2, 3, 4],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.CreateRequest"),
            value: create_deliver,
        }),
        external_origin: "test".into(),
        application_instance_uuid: vec![9, 9, 9],
        authoritative_context: None,
        trace_context: None,
    };
    let rsp1 = router.handle_public_request(&sess, &req1, None, false, 1_760_000_000);
    assert_eq!(rsp1.status_code, 200);
    let create_rsp_wire = WireMessage::decode(&rsp1.payload.expect("CreateResponse").value).unwrap();
    let goal_id_bytes = match &create_rsp_wire.fields[0].value {
        WireValue::LengthDelimited(b) => b.clone(),
        _ => panic!("missing id"),
    };

    let notices = router.poll_notices(sess.session_id);
    let created_notice = notices
        .iter()
        .find(|n| {
            n.payload
                .as_ref()
                .is_some_and(|p| p.type_url.ends_with("eve_public.goal.api.CreatedNotice"))
        })
        .and_then(|n| n.payload.as_ref())
        .expect("CreatedNotice must be emitted for DeliverItem goal");
    let notice_msg = desc
        .decode_dynamic_or_wire(&created_notice.type_url, &created_notice.value)
        .expect("decode CreatedNotice via descriptors");
    let DynamicValue::Message(attrs) = notice_msg.get(2).expect("CreatedNotice.goal") else {
        panic!("CreatedNotice.goal must be Message");
    };
    let DynamicValue::Message(cfg) = attrs
        .get(7)
        .expect("Attributes.contribution_config (field 7) must be populated")
    else {
        panic!("contribution_config must be Message");
    };
    assert!(cfg.get(2).is_some(), "expected deliver_item (field 2) on CreatedNotice");

    // 2. CreateRequest omitting field 4 must still emit a valid non-empty contribution_config (`manual`)
    let mut create_fallback = Vec::new();
    encode_string_field(1, "Manual Goal", &mut create_fallback);
    encode_string_field(2, "No config supplied", &mut create_fallback);
    encode_varint_field(3, 10, &mut create_fallback);
    let req2 = EvePublicRequest {
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.CreateRequest"),
            value: create_fallback,
        }),
        ..req1.clone()
    };
    let rsp2 = router.handle_public_request(&sess, &req2, None, false, 1_760_000_010);
    assert_eq!(rsp2.status_code, 200);
    let notices2 = router.poll_notices(sess.session_id);
    let created2 = notices2
        .iter()
        .find(|n| {
            n.payload
                .as_ref()
                .is_some_and(|p| p.type_url.ends_with("eve_public.goal.api.CreatedNotice"))
        })
        .and_then(|n| n.payload.as_ref())
        .expect("CreatedNotice must be emitted even when CreateRequest omits field 4");
    let notice2_msg = desc
        .decode_dynamic_or_wire(&created2.type_url, &created2.value)
        .expect("decode CreatedNotice2");
    let DynamicValue::Message(attrs2) = notice2_msg.get(2).expect("goal attrs") else {
        panic!("attrs2 must be Message");
    };
    let DynamicValue::Message(cfg2) = attrs2
        .get(7)
        .expect("fallback CreatedNotice must include contribution_config (field 7)")
    else {
        panic!("cfg2 must be Message");
    };
    assert!(cfg2.get(1).is_some(), "fallback contribution_config must set manual (field 1)");

    // 3. GetRequest and GetAllRequest must also include contribution_config (field 7)
    let mut get_req_bytes = Vec::new();
    encode_bytes_field(1, &goal_id_bytes, &mut get_req_bytes);
    let get_rsp = router.handle_public_request(
        &sess,
        &EvePublicRequest {
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.GetRequest"),
                value: get_req_bytes,
            }),
            ..req1.clone()
        },
        None,
        false,
        1_760_000_020,
    );
    assert_eq!(get_rsp.status_code, 200);
    let get_any = get_rsp.payload.unwrap();
    let get_msg = desc
        .decode_dynamic_or_wire(&get_any.type_url, &get_any.value)
        .expect("decode GetResponse");
    let DynamicValue::Message(get_attrs) = get_msg.get(1).expect("GetResponse.goal") else {
        panic!("GetResponse.goal must be Message");
    };
    assert!(
        get_attrs.get(7).is_some(),
        "GetResponse.goal must include contribution_config (field 7)"
    );

    let get_all_rsp = router.handle_public_request(
        &sess,
        &EvePublicRequest {
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.GetAllRequest"),
                value: Vec::new(),
            }),
            ..req1
        },
        None,
        false,
        1_760_000_030,
    );
    assert_eq!(get_all_rsp.status_code, 200);
    let get_all_any = get_all_rsp.payload.unwrap();
    let get_all_msg = desc
        .decode_dynamic_or_wire(&get_all_any.type_url, &get_all_any.value)
        .expect("decode GetAllResponse");
    let DynamicValue::Repeated(goals) = get_all_msg.get(3).expect("GetAllResponse.goals") else {
        panic!("GetAllResponse.goals must be Repeated");
    };
    assert!(goals.len() >= 2);
    for entry in goals {
        let DynamicValue::Message(gm) = entry else {
            panic!("goal entry must be Message");
        };
        let DynamicValue::Message(ga) = gm.get(2).expect("goal attributes") else {
            panic!("goal attributes must be Message");
        };
        assert!(
            ga.get(7).is_some(),
            "GetAllResponse goal must include contribution_config (field 7)"
        );
    }
}

#[test]
fn issue_779_non_gm_corp_goal_create_emits_v24_attributes_and_active_list() {
    use eve_proto::quasar::{
        encode_bytes_field, encode_string_field, encode_varint_field, EvePublicRequest, GoogleAny,
        WireValue, TYPE_GOOGLEAPIS_PREFIX,
    };

    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = shared_sde();
    let desc = QuasarDescriptorRegistry::open_default_or_empty();
    let cid = 90_000_001i64;
    let corp_id = 98_000_001i64;
    db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.corporation_id = corp_id;
        }
        snap.corporations.entry(corp_id).or_default().ceo_id = cid;
    });

    let router = PublicGatewayRouter::new(desc.clone(), Arc::clone(&db), Arc::clone(&sde));

    // Non-GM player CEO (ROLE_PLAYER_DEFAULT only, no ROLE_ADMIN)
    let mut sess = SessionState::new(779);
    sess.user_id = Some(1);
    sess.character_id = Some(cid);
    sess.corporation_id = Some(corp_id);
    sess.role = service_roles::ROLE_PLAYER_DEFAULT;
    let _rx = router.subscribe_session(&sess, Vec::new(), 1_760_000_100);

    let manual_pb = encode_goal_contribution_config("Manual", None);
    let mut create_req = Vec::new();
    encode_string_field(1, "V24 Corp Project", &mut create_req);
    encode_string_field(2, "Created by non-GM CEO", &mut create_req);
    encode_varint_field(3, 250, &mut create_req);
    encode_bytes_field(4, &manual_pb, &mut create_req);

    let req = EvePublicRequest {
        issued: None,
        correlation_uuid: vec![7, 7, 9],
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.CreateRequest"),
            value: create_req,
        }),
        external_origin: "test".into(),
        application_instance_uuid: vec![1, 2, 3],
        authoritative_context: None,
        trace_context: None,
    };
    let rsp = router.handle_public_request(&sess, &req, None, false, 1_760_000_100);
    assert_eq!(rsp.status_code, 200);
    let rsp_wire = WireMessage::decode(&rsp.payload.expect("CreateResponse").value).unwrap();
    let goal_id_bytes = match &rsp_wire.fields[0].value {
        WireValue::LengthDelimited(b) => b.clone(),
        _ => panic!("missing goal id"),
    };

    let notices = router.poll_notices(sess.session_id);
    let created_notice = notices
        .iter()
        .find(|n| {
            n.payload
                .as_ref()
                .is_some_and(|p| p.type_url.ends_with("eve_public.goal.api.CreatedNotice"))
        })
        .and_then(|n| n.payload.as_ref())
        .expect("CreatedNotice must be emitted for non-GM corp CEO");
    let notice_msg = desc
        .decode_dynamic_or_wire(&created_notice.type_url, &created_notice.value)
        .expect("decode CreatedNotice");
    let DynamicValue::Message(attrs) = notice_msg.get(2).expect("CreatedNotice.goal") else {
        panic!("CreatedNotice.goal must be Message");
    };
    assert!(attrs.get(7).is_some(), "Attributes must have contribution_config (7)");
    assert!(attrs.get(14).is_some(), "Attributes must have no_due_timestamp (14)");
    assert!(attrs.get(19).is_some(), "Attributes must have assigner (19)");
    assert!(attrs.get(20).is_some(), "Attributes must have assignee (20)");
    assert!(attrs.get(23).is_some(), "Attributes must have unlimited (23)");
    assert!(attrs.get(25).is_some(), "Attributes must have contribution_unlimited (25)");

    // Active goal UUID must appear in eve_public.corporationgoal.api.GetActiveResponse
    let active_rsp = router.handle_public_request(
        &sess,
        &EvePublicRequest {
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.corporationgoal.api.GetActiveRequest"
                ),
                value: Vec::new(),
            }),
            ..req.clone()
        },
        None,
        false,
        1_760_000_105,
    );
    assert_eq!(active_rsp.status_code, 200);
    let active_any = active_rsp.payload.expect("GetActiveResponse");
    let active_wire = WireMessage::decode(&active_any.value).expect("decode GetActiveResponse");
    assert!(
        active_wire.fields.iter().any(|f| {
            f.field_number == 1
                && matches!(&f.value, WireValue::LengthDelimited(b) if b == &goal_id_bytes)
        }),
        "GetActiveResponse must include created goal UUID"
    );

    // GetRequest must return Attributes with assigner/assignee and manual contribution_config
    let mut get_bytes = Vec::new();
    encode_bytes_field(1, &goal_id_bytes, &mut get_bytes);
    let get_rsp = router.handle_public_request(
        &sess,
        &EvePublicRequest {
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.goal.api.GetRequest"),
                value: get_bytes,
            }),
            ..req
        },
        None,
        false,
        1_760_000_110,
    );
    assert_eq!(get_rsp.status_code, 200);
    let get_any = get_rsp.payload.expect("GetResponse");
    let get_msg = desc
        .decode_dynamic_or_wire(&get_any.type_url, &get_any.value)
        .expect("decode GetResponse");
    let DynamicValue::Message(get_attrs) = get_msg.get(1).expect("GetResponse.goal") else {
        panic!("GetResponse.goal must be Message");
    };
    let DynamicValue::Message(cfg) = get_attrs.get(7).expect("contribution_config") else {
        panic!("contribution_config must be Message");
    };
    assert!(cfg.get(1).is_some(), "expected manual (field 1)");
    assert!(get_attrs.get(19).is_some(), "GetResponse.goal must include assigner (19)");
    assert!(get_attrs.get(20).is_some(), "GetResponse.goal must include assignee (20)");
}
