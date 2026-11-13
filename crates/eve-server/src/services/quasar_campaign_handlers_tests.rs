use super::*;
use crate::services::fleet_social::fleet_social_fleet_ops::fleet_social_fleet_ops_tests::test_ctx_and_clock;
use crate::services::quasar_campaign_state::{
    ObjectiveParameter, TickingContribution, TickingEvaluation,
};
use eve_proto::pb;
use eve_proto::quasar::{AuthoritativeContext, DynamicQuasarMessage};

fn id_dyn_msg(uuid: [u8; 16]) -> DynamicQuasarMessage {
    let mut inner = DynamicQuasarMessage::new("Identifier");
    inner.set(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut msg = DynamicQuasarMessage::new("Req");
    msg.set(1, DynamicValue::Message(Box::new(inner)));
    msg
}

#[test]
fn campaign_wire_encoders_and_gateway_request_handlers() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let cid = 501_u64;
    let oid = 601_u64;
    let tid = 701_u64;
    let char_id = 100_000_001_u32;

    let camp_uuid = campaign_uuid(cid);
    let id_msg = id_dyn_msg(camp_uuid);
    assert_eq!(extract_campaign_id(id_msg.get(1)), Some(cid));
    assert_eq!(extract_campaign_ids(id_msg.get(1)), vec![cid]);

    let camp = CampaignRecord {
        id: cid,
        name: "caldari insurgency".into(),
        description: "desc".into(),
        namespace: "pirate_insurgency".into(),
        scheduled_start_unix: 1_699_000_000,
        scheduled_expiry_unix: 1_710_000_000,
        state: 0,
        transition_unix: 0,
        etag: 1,
        version_notice_pending: false,
        objectives: vec![oid],
    };
    let obj = CampaignObjectiveRecord {
        id: oid,
        campaign_id: cid,
        name: "mine asteroid".into(),
        target_progress: 100,
        current_progress: 0,
        committed_participants: vec![char_id],
        resigned_participants: Vec::new(),
        contributors: HashMap::from([(char_id, (25, 2))]),
        contribution_method: "MineAsteroid".into(),
        max_progress_per_participant: 50,
        isk_reward_per_interval: 10_000,
        isk_reward_progress_interval: 10,
        state: 0,
        transition_unix: 0,
        parameters: vec![ObjectiveParameter {
            key: "solar_system".into(),
            values: vec!["30000142".into()],
        }],
    };
    let trk = CampaignTrackRecord {
        id: tid,
        campaign_id: cid,
        key: "suppression".into(),
        target_progress_value: 500,
        current_progress: 40,
        participants: HashMap::from([(char_id, 40)]),
        max_participant_contribution: 100,
        isk_reward_per_interval: 25_000,
        isk_reward_progress_interval: 20,
        redeemed_intervals: HashMap::new(),
        ticking: vec![TickingContribution {
            key: "passive".into(),
            interval_secs: 60,
            evaluations: vec![
                TickingEvaluation {
                    key: "base".into(),
                    constant: Some(5),
                    per_unit_value: None,
                },
                TickingEvaluation {
                    key: "per_pilot".into(),
                    constant: None,
                    per_unit_value: Some(2),
                },
            ],
            last_tick_unix: 1_699_000_000,
        }],
        state: 0,
        transition_unix: 0,
    };

    let mgr = Arc::new(ProgressionExtraManager::new(Some(Arc::clone(&ctx.db))));
    register_campaign_handlers(&ctx.quasar_gateway, Arc::clone(&mgr));
    mgr.register_campaign(camp.clone(), vec![obj.clone()], vec![trk.clone()])
        .expect("register campaign");

    assert!(!encode_campaign_stats(&[&obj]).is_empty());
    assert!(!encode_campaign_attributes(&camp, &[&obj]).is_empty());
    assert!(!encode_objective_stats(&obj).is_empty());
    assert!(!encode_objective_attributes(&obj).is_empty());
    assert!(!encode_track_attributes(&trk).is_empty());
    assert!(!encode_track_stats(&trk).is_empty());
    assert!(!encode_track_participant_stats(&trk, char_id).is_empty());
    assert!(!encode_track_participant_stats(&trk, 999_999).is_empty());
    assert!(!encode_status_track(&trk).is_empty());
    assert!(!encode_ticking_prediction(&trk, 3).is_empty());

    let auth = AuthoritativeContext::for_session(
        Some(1),
        Some(char_id),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![1, 2],
    );
    let any = GoogleAny::default();
    let empty = DynamicQuasarMessage::new("Req");
    let handlers = ctx.quasar_gateway.handlers.read().unwrap().clone();

    assert!(handlers["eve_public.campaign.api.GetAllRequest"](&auth, &any, &empty).is_ok());
    assert!(handlers["eve_public.campaign.api.GetAllForNamespaceRequest"](&auth, &any, &empty).is_ok());
    assert!(handlers["eve_public.campaign.api.GetRequest"](&auth, &any, &id_msg).is_ok());
    assert!(handlers["eve_public.campaign.api.GetStatusRequest"](&auth, &any, &id_msg).is_ok());
    assert!(handlers["eve_public.campaign.api.GetStatsRequest"](&auth, &any, &id_msg).is_ok());
    assert!(handlers["eve_public.campaign.api.GetParticipantStatsRequest"](&auth, &any, &id_msg).is_ok());
    assert!(handlers["eve_public.campaign.api.GetLimitsRequest"](&auth, &any, &empty).is_ok());
    assert!(handlers["eve_public.campaign.api.GetAllUnredeemedRewardsRequest"](&auth, &any, &empty).is_ok());
    assert!(handlers["eve_public.campaign.api.GetAllUnredeemedParticipantRewardsRequest"](&auth, &any, &empty).is_ok());
}

fn wire_field_bytes(msg: &WireMessage, field_number: u32) -> Option<&[u8]> {
    msg.fields.iter().find_map(|f| {
        if f.field_number == field_number {
            if let WireValue::LengthDelimited(b) = &f.value {
                return Some(b.as_slice());
            }
        }
        None
    })
}

fn wire_field_all_bytes<'a>(msg: &'a WireMessage, field_number: u32) -> Vec<&'a [u8]> {
    msg.fields
        .iter()
        .filter_map(|f| {
            if f.field_number == field_number {
                if let WireValue::LengthDelimited(b) = &f.value {
                    return Some(b.as_slice());
                }
            }
            None
        })
        .collect()
}

fn wire_field_varint(msg: &WireMessage, field_number: u32) -> Option<u64> {
    msg.fields.iter().find_map(|f| {
        if f.field_number == field_number {
            if let WireValue::Varint(v) = f.value {
                return Some(v);
            }
        }
        None
    })
}

#[test]
fn id_from_uuid_extract_id_and_namespace_of_edge_cases() {
    assert_eq!(id_from_uuid(b"short"), None);
    let mut bad_prefix = [0u8; 16];
    bad_prefix[..5].copy_from_slice(b"XXXX_");
    bad_prefix[8..].copy_from_slice(&42u64.to_le_bytes());
    assert_eq!(id_from_uuid(&bad_prefix), None);

    let mut zero_id = [0u8; 16];
    zero_id[..5].copy_from_slice(b"CAMP_");
    assert_eq!(id_from_uuid(&zero_id), None);

    assert_eq!(id_from_uuid(&campaign_uuid(77)), Some(77));
    assert_eq!(id_from_uuid(&objective_uuid(88)), Some(88));
    assert_eq!(id_from_uuid(&track_uuid(99)), Some(99));

    // extract_campaign_id across DynamicValue variants
    let u55 = campaign_uuid(55);
    assert_eq!(extract_campaign_id(None), None);
    assert_eq!(extract_campaign_id(Some(&DynamicValue::Int64(5))), None);
    assert_eq!(
        extract_campaign_id(Some(&DynamicValue::Bytes(u55.to_vec()))),
        Some(55)
    );

    let wire_wrapped = pb::message("eve_public.campaign.Identifier")
        .set("uuid", u55)
        .must_encode();
    assert_eq!(
        extract_campaign_id(Some(&DynamicValue::Bytes(wire_wrapped.clone()))),
        Some(55)
    );
    assert_eq!(
        extract_campaign_id(Some(&DynamicValue::Bytes(vec![0xFF, 0x01]))),
        None
    );

    let mut no_f1 = DynamicQuasarMessage::new("Identifier");
    no_f1.set(2, DynamicValue::Bytes(u55.to_vec()));
    assert_eq!(extract_campaign_id(Some(&DynamicValue::Message(Box::new(no_f1)))), None);

    let mut non_bytes_f1 = DynamicQuasarMessage::new("Identifier");
    non_bytes_f1.set(1, DynamicValue::Int64(123));
    assert_eq!(
        extract_campaign_id(Some(&DynamicValue::Message(Box::new(non_bytes_f1)))),
        None
    );

    // extract_campaign_ids mixed list
    let u66 = campaign_uuid(66);
    assert!(extract_campaign_ids(None).is_empty());
    assert!(extract_campaign_ids(Some(&DynamicValue::String("x".into()))).is_empty());
    let list_val = DynamicValue::Repeated(vec![
        DynamicValue::Bytes(u55.to_vec()),
        DynamicValue::Bytes(vec![1, 2, 3]),
        DynamicValue::Bytes(wire_wrapped),
        DynamicValue::Message({
            let mut idm = DynamicQuasarMessage::new("Id");
            idm.set(1, DynamicValue::Bytes(u66.to_vec()));
            Box::new(idm)
        }),
    ]);
    assert_eq!(extract_campaign_ids(Some(&list_val)), vec![55, 55, 66]);

    // namespace_of branches (field 1 is a nested message whose field 1 holds namespace, or bytes encoding it)
    let mut ns_sub = DynamicQuasarMessage::new("Namespace");
    ns_sub.set(1, DynamicValue::String("insurgency".into()));
    let mut ns_msg = DynamicQuasarMessage::new("Req");
    ns_msg.set(1, DynamicValue::Message(Box::new(ns_sub)));
    assert_eq!(namespace_of(&ns_msg).as_deref(), Some("insurgency"));
    assert_eq!(namespace_of(&DynamicQuasarMessage::new("Empty")), None);

    let wire_ns = pb::message("eve_public.campaign.Namespace")
        .set("namespace", "crimson")
        .must_encode();
    let mut wire_ns_msg = DynamicQuasarMessage::new("Req");
    wire_ns_msg.set(1, DynamicValue::Bytes(wire_ns));
    assert_eq!(namespace_of(&wire_ns_msg).as_deref(), Some("crimson"));

    let invalid_utf8_wire = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::LengthDelimited(vec![0xFF, 0xFE]),
        }],
    }
    .encode();
    let mut bad_ns_msg = DynamicQuasarMessage::new("Req");
    bad_ns_msg.set(1, DynamicValue::Bytes(invalid_utf8_wire));
    assert_eq!(namespace_of(&bad_ns_msg), None);
}

#[test]
fn campaign_wire_encoders_cover_all_state_and_boundary_branches() {
    // encode_lifecycle with and without transition_unix
    let mut lc0_buf = Vec::new();
    encode_lifecycle(1, CAMPAIGN_STATE_ACTIVE, 0, &mut lc0_buf);
    let lc0_wrap = WireMessage::decode(&lc0_buf).unwrap();
    let lc0 = WireMessage::decode(wire_field_bytes(&lc0_wrap, 1).unwrap()).unwrap();
    assert_eq!(wire_field_varint(&lc0, 1), Some(CAMPAIGN_STATE_ACTIVE as u64));
    assert_eq!(wire_field_bytes(&lc0, 2), None);

    let mut lc1_buf = Vec::new();
    encode_lifecycle(1, CAMPAIGN_STATE_COMPLETED, 1_700_000_123, &mut lc1_buf);
    let lc1_wrap = WireMessage::decode(&lc1_buf).unwrap();
    let lc1 = WireMessage::decode(wire_field_bytes(&lc1_wrap, 1).unwrap()).unwrap();
    assert_eq!(wire_field_varint(&lc1, 1), Some(CAMPAIGN_STATE_COMPLETED as u64));
    assert!(wire_field_bytes(&lc1, 2).is_some());

    // encode_campaign_attributes: no expiry, empty namespace, active vs completed vs unknown state
    let camp_active = CampaignRecord {
        id: 1,
        name: "n1".into(),
        description: "d1".into(),
        namespace: String::new(),
        scheduled_start_unix: 1_700_000_000,
        scheduled_expiry_unix: 0,
        state: CAMPAIGN_STATE_ACTIVE,
        transition_unix: 1_700_000_010,
        etag: 2,
        version_notice_pending: false,
        objectives: vec![10],
    };
    let camp_completed = CampaignRecord {
        state: CAMPAIGN_STATE_COMPLETED,
        namespace: "ns_a".into(),
        scheduled_expiry_unix: 1_700_001_000,
        ..camp_active.clone()
    };
    let obj_neg = CampaignObjectiveRecord {
        id: 10,
        campaign_id: 1,
        name: "obj1".into(),
        target_progress: 100,
        current_progress: -15,
        committed_participants: vec![1, 2],
        resigned_participants: vec![3],
        contributors: HashMap::from([(1, (10, 0)), (2, (20, 1)), (3, (5, 0))]),
        contribution_method: "UnknownCustomMethod".into(),
        max_progress_per_participant: 0,
        isk_reward_per_interval: 0,
        isk_reward_progress_interval: 0,
        state: 0,
        transition_unix: 0,
        parameters: vec![ObjectiveParameter {
            key: "unknown_key".into(),
            values: vec!["val1".into()],
        }],
    };
    let obj_active = CampaignObjectiveRecord {
        id: 11,
        state: OBJECTIVE_STATE_ACTIVE,
        transition_unix: 1_700_000_050,
        max_progress_per_participant: 200,
        isk_reward_per_interval: 5_000,
        isk_reward_progress_interval: 0, // tests .max(1)
        contributors: HashMap::from([(2, (20, 1)), (4, (30, 0))]),
        ..obj_neg.clone()
    };

    let ca_active = WireMessage::decode(&encode_campaign_attributes(&camp_active, &[&obj_neg])).unwrap();
    assert_eq!(wire_field_bytes(&ca_active, 18), None); // no expiry when 0
    assert_eq!(wire_field_bytes(&ca_active, 24), None); // no namespace when empty
    assert_eq!(wire_field_varint(&ca_active, 19), Some(1)); // started lifecycle
    assert_eq!(wire_field_bytes(&ca_active, 20), None);

    let ca_completed =
        WireMessage::decode(&encode_campaign_attributes(&camp_completed, &[&obj_neg, &obj_active])).unwrap();
    assert!(wire_field_bytes(&ca_completed, 18).is_some()); // expiry present
    let ns_inner = WireMessage::decode(wire_field_bytes(&ca_completed, 24).unwrap()).unwrap();
    assert_eq!(wire_field_bytes(&ns_inner, 1), Some(b"ns_a".as_slice()));
    assert_eq!(wire_field_varint(&ca_completed, 19), None);
    assert!(wire_field_bytes(&ca_completed, 20).is_some()); // completed lifecycle

    // CampaignStats: field 1 = clamped total progress (0), field 2 = unique contributors across objectives = {1, 2, 3, 4} -> 4
    let cstats = WireMessage::decode(&encode_campaign_stats(&[&obj_neg, &obj_active])).unwrap();
    assert_eq!(wire_field_varint(&cstats, 1), Some(0));
    assert_eq!(wire_field_varint(&cstats, 2), Some(4));

    // Objective attributes & stats edge cases
    let oa_neg = WireMessage::decode(&encode_objective_attributes(&obj_neg)).unwrap();
    assert_eq!(wire_field_varint(&oa_neg, 8), Some(1)); // state == 0 -> active marker field 8
    assert_eq!(wire_field_varint(&oa_neg, 12), None); // max_progress_per_participant == 0 -> omitted
    let rewards_neg = WireMessage::decode(wire_field_bytes(&oa_neg, 14).unwrap()).unwrap();
    assert_eq!(wire_field_varint(&rewards_neg, 2), Some(1)); // no ISK reward interval
    let oa_act = WireMessage::decode(&encode_objective_attributes(&obj_active)).unwrap();
    assert_eq!(wire_field_varint(&oa_act, 8), Some(1));
    assert_eq!(wire_field_varint(&oa_act, 12), Some(200));
    let obj_expired = CampaignObjectiveRecord {
        state: crate::services::quasar_campaign_state::OBJECTIVE_STATE_EXPIRED,
        isk_reward_progress_interval: 10,
        ..obj_active.clone()
    };
    let oa_exp = WireMessage::decode(&encode_objective_attributes(&obj_expired)).unwrap();
    assert_eq!(wire_field_varint(&oa_exp, 8), None);
    assert!(wire_field_bytes(&oa_exp, 9).is_some()); // non-active -> transition timestamp
    let rewards_isk = WireMessage::decode(wire_field_bytes(&oa_exp, 14).unwrap()).unwrap();
    assert!(wire_field_bytes(&rewards_isk, 1).is_some());

    let ostats = WireMessage::decode(&encode_objective_stats(&obj_neg)).unwrap();
    assert_eq!(wire_field_varint(&ostats, 1), Some(0)); // clamped negative current_progress
    assert_eq!(wire_field_varint(&ostats, 2), Some(2)); // committed
    assert_eq!(wire_field_varint(&ostats, 3), Some(3)); // committed + resigned
    assert_eq!(wire_field_varint(&ostats, 4), Some(3)); // unique contributors

    // Track attributes, stats, participant stats, ticking prediction
    let trk_started = CampaignTrackRecord {
        id: 21,
        campaign_id: 1,
        key: "trk".into(),
        target_progress_value: 0,
        current_progress: 0,
        participants: HashMap::from([(9, 50), (10, 120)]),
        max_participant_contribution: 100,
        isk_reward_per_interval: 0,
        isk_reward_progress_interval: 0,
        redeemed_intervals: HashMap::new(),
        ticking: vec![TickingContribution {
            key: "empty_eval".into(),
            interval_secs: 0,
            evaluations: vec![TickingEvaluation {
                key: "noop".into(),
                constant: None,
                per_unit_value: None,
            }],
            last_tick_unix: 0,
        }],
        state: TRACK_STATE_STARTED,
        transition_unix: 1_700_000_200,
    };
    let ta = WireMessage::decode(&encode_track_attributes(&trk_started)).unwrap();
    assert_eq!(wire_field_varint(&ta, 3), None); // target == 0
    assert_eq!(wire_field_varint(&ta, 4), Some(1)); // target omitted marker
    assert_eq!(wire_field_varint(&ta, 6), Some(1)); // no ISK reward marker

    let ts = WireMessage::decode(&encode_track_stats(&trk_started)).unwrap();
    assert_eq!(wire_field_varint(&ts, 1), Some(0));
    assert!(wire_field_bytes(&ts, 2).is_some()); // ticking entry emitted

    let tps_max_wrap = WireMessage::decode(&encode_track_participant_stats(&trk_started, 10)).unwrap();
    let tps_max = WireMessage::decode(wire_field_bytes(&tps_max_wrap, 1).unwrap()).unwrap();
    assert_eq!(wire_field_varint(&tps_max, 1), Some(120));
    assert_eq!(wire_field_varint(&tps_max, 2), Some(1)); // max_reached = 1 when >= 100
    let tps_under_wrap = WireMessage::decode(&encode_track_participant_stats(&trk_started, 9)).unwrap();
    let tps_under = WireMessage::decode(wire_field_bytes(&tps_under_wrap, 1).unwrap()).unwrap();
    assert_eq!(wire_field_varint(&tps_under, 1), Some(50));
    assert_eq!(wire_field_varint(&tps_under, 2), Some(0)); // not reached
    let tps_missing = WireMessage::decode(&encode_track_participant_stats(&trk_started, 999)).unwrap();
    assert_eq!(wire_field_varint(&tps_missing, 2), Some(1)); // no_participant_track

    assert!(!encode_ticking_prediction(&trk_started, 2).is_empty());
}

#[test]
fn campaign_handlers_cover_filtering_auth_not_found_and_rewards() {
    let (ctx, _clock) = test_ctx_and_clock(1_700_000_500);
    let mgr = Arc::new(ProgressionExtraManager::new(Some(Arc::clone(&ctx.db))));
    register_campaign_handlers(&ctx.quasar_gateway, Arc::clone(&mgr));
    let handlers = ctx.quasar_gateway.handlers.read().unwrap().clone();

    let cid_active = 801_u64;
    let cid_completed = 802_u64;
    let cid_upcoming = 803_u64;
    let oid_active = 901_u64;
    let oid_expired = 902_u64;
    let tid_active = 951_u64;
    let char_a = 100_000_010_u32;

    let make_obj = |id: u64, cid: u64, state: u32, rewards: i64| CampaignObjectiveRecord {
        id,
        campaign_id: cid,
        name: format!("obj_{id}"),
        target_progress: 100,
        current_progress: 40,
        committed_participants: vec![char_a],
        resigned_participants: Vec::new(),
        contributors: HashMap::from([(char_a, (40, rewards))]),
        contribution_method: "MineAsteroid".into(),
        max_progress_per_participant: 0,
        isk_reward_per_interval: 1_000,
        isk_reward_progress_interval: 10,
        state,
        transition_unix: 1_700_000_100,
        parameters: Vec::new(),
    };

    mgr.register_campaign(
        CampaignRecord {
            id: cid_active,
            name: "active_ns1".into(),
            description: "d".into(),
            namespace: "ns1".into(),
            scheduled_start_unix: 1_700_000_000,
            scheduled_expiry_unix: 1_700_050_000,
            state: CAMPAIGN_STATE_ACTIVE,
            transition_unix: 1_700_000_000,
            etag: 1,
            version_notice_pending: false,
            objectives: vec![oid_active, oid_expired],
        },
        vec![
            make_obj(oid_active, cid_active, OBJECTIVE_STATE_ACTIVE, 3),
            make_obj(
                oid_expired,
                cid_active,
                crate::services::quasar_campaign_state::OBJECTIVE_STATE_EXPIRED,
                0,
            ),
        ],
        vec![CampaignTrackRecord {
            id: tid_active,
            campaign_id: cid_active,
            key: "t".into(),
            target_progress_value: 200,
            current_progress: 50,
            participants: HashMap::from([(char_a, 50)]),
            max_participant_contribution: 100,
            isk_reward_per_interval: 5_000,
            isk_reward_progress_interval: 10,
            redeemed_intervals: HashMap::from([(char_a, 1)]), // 50/10=5 earned - 1 redeemed = 4 unredeemed
            ticking: Vec::new(),
            state: TRACK_STATE_STARTED,
            transition_unix: 1_700_000_000,
        }],
    )
    .unwrap();

    mgr.register_campaign(
        CampaignRecord {
            id: cid_completed,
            name: "comp_ns2".into(),
            description: "d".into(),
            namespace: "ns2".into(),
            scheduled_start_unix: 1_699_000_000,
            scheduled_expiry_unix: 1_700_000_100,
            state: CAMPAIGN_STATE_COMPLETED,
            transition_unix: 1_700_000_100,
            etag: 1,
            version_notice_pending: false,
            objectives: Vec::new(),
        },
        Vec::new(),
        Vec::new(),
    )
    .unwrap();

    mgr.register_campaign(
        CampaignRecord {
            id: cid_upcoming,
            name: "upcoming_ns1".into(),
            description: "d".into(),
            namespace: "ns1".into(),
            scheduled_start_unix: 1_800_000_000,
            scheduled_expiry_unix: 1_900_000_000,
            state: 0,
            transition_unix: 0,
            etag: 1,
            version_notice_pending: false,
            objectives: Vec::new(),
        },
        Vec::new(),
        Vec::new(),
    )
    .unwrap();

    // listed_campaigns splits started (active/completed) vs state == 0 upcoming campaign
    let (started_ids, upcoming) = listed_campaigns(&mgr, None);
    assert_eq!(started_ids.len(), 2);
    assert_eq!(upcoming.len(), 1);
    assert_eq!(upcoming[0].id, cid_upcoming);

    let any = GoogleAny::default();
    let empty = DynamicQuasarMessage::new("Req");
    let auth_char = AuthoritativeContext::for_session(
        Some(1),
        Some(char_a),
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![],
    );
    let auth_anon = AuthoritativeContext::for_session(
        Some(2),
        None,
        Some(1_000_009),
        u64::MAX,
        "tranquility",
        vec![],
    );

    // GetAllForNamespaceRequest filters by namespace string (nested msg field 1 -> field 1)
    let mut ns1_sub = DynamicQuasarMessage::new("Namespace");
    ns1_sub.set(1, DynamicValue::String("ns1".into()));
    let mut ns_req = DynamicQuasarMessage::new("GetAllForNamespaceRequest");
    ns_req.set(1, DynamicValue::Message(Box::new(ns1_sub)));
    let ns_resp = handlers["eve_public.campaign.api.GetAllForNamespaceRequest"](&auth_char, &any, &ns_req).unwrap();
    let ns_wire = WireMessage::decode(&ns_resp.value).unwrap();
    assert_eq!(wire_field_all_bytes(&ns_wire, 1).len(), 1); // only cid_active, not upcoming

    // GetRequest puts active objectives in field 2 and all objective IDs in field 6
    let get_resp = handlers["eve_public.campaign.api.GetRequest"](
        &auth_char,
        &any,
        &id_dyn_msg(campaign_uuid(cid_active)),
    )
    .unwrap();
    let get_wire = WireMessage::decode(&get_resp.value).unwrap();
    assert_eq!(wire_field_all_bytes(&get_wire, 2).len(), 1);
    assert_eq!(wire_field_all_bytes(&get_wire, 6).len(), 2);

    // Error paths for Get / GetStatus / GetStats / GetParticipantStats
    let bad_id = DynamicQuasarMessage::new("Bad");
    let missing_id = id_dyn_msg(campaign_uuid(999_999));
    for name in [
        "eve_public.campaign.api.GetRequest",
        "eve_public.campaign.api.GetStatusRequest",
        "eve_public.campaign.api.GetStatsRequest",
    ] {
        assert_eq!(
            handlers[name](&auth_char, &any, &bad_id).unwrap_err().0,
            crate::quasar_gateway::STATUS_BAD_REQUEST
        );
        assert_eq!(
            handlers[name](&auth_char, &any, &missing_id).unwrap_err().0,
            STATUS_NOT_FOUND
        );
    }

    assert_eq!(
        handlers["eve_public.campaign.api.GetParticipantStatsRequest"](&auth_anon, &any, &missing_id)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        handlers["eve_public.campaign.api.GetParticipantStatsRequest"](&auth_char, &any, &bad_id)
            .unwrap_err()
            .0,
        crate::quasar_gateway::STATUS_BAD_REQUEST
    );
    assert_eq!(
        handlers["eve_public.campaign.api.GetParticipantStatsRequest"](&auth_char, &any, &missing_id)
            .unwrap_err()
            .0,
        STATUS_NOT_FOUND
    );

    // Unredeemed rewards auth + namespace filter
    assert_eq!(
        handlers["eve_public.campaign.api.GetAllUnredeemedRewardsRequest"](&auth_anon, &any, &empty)
            .unwrap_err()
            .0,
        STATUS_FORBIDDEN
    );
    assert_eq!(
        handlers["eve_public.campaign.api.GetAllUnredeemedParticipantRewardsRequest"](
            &auth_anon, &any, &empty
        )
        .unwrap_err()
        .0,
        STATUS_FORBIDDEN
    );

    let rewards_all =
        handlers["eve_public.campaign.api.GetAllUnredeemedRewardsRequest"](&auth_char, &any, &empty)
            .unwrap();
    let rw_wire = WireMessage::decode(&rewards_all.value).unwrap();
    assert!(!wire_field_all_bytes(&rw_wire, 1).is_empty());

    let mut ns2_sub = DynamicQuasarMessage::new("Namespace");
    ns2_sub.set(1, DynamicValue::String("ns2".into()));
    let mut pr_ns2 = DynamicQuasarMessage::new("Req");
    pr_ns2.set(1, DynamicValue::Message(Box::new(ns2_sub)));
    let rewards_ns2 = handlers["eve_public.campaign.api.GetAllUnredeemedParticipantRewardsRequest"](
        &auth_char, &any, &pr_ns2,
    )
    .unwrap();
    let pr_wire = WireMessage::decode(&rewards_ns2.value).unwrap();
    assert_eq!(wire_field_all_bytes(&pr_wire, 1).len(), 0);

    // queue_notices executes cleanly on empty notices
    queue_notices(Vec::new(), 1_700_000_000);
}

#[test]
fn campaign_builders_decode_strictly_with_descriptors() {
    let reg = eve_proto::quasar::QuasarDescriptorRegistry::open_default()
        .expect("descriptor registry");
    let roundtrip = |type_name: &str, wire: &[u8]| {
        let decoded = reg.decode_dynamic(type_name, wire).unwrap_or_else(|e| panic!("decode {type_name}: {e}"));
        let reencoded = reg.encode_dynamic(&decoded).unwrap_or_else(|e| panic!("encode {type_name}: {e}"));
        let decoded2 = reg.decode_dynamic(type_name, &reencoded).unwrap_or_else(|e| panic!("redecode {type_name}: {e}"));
        assert_eq!(decoded2, decoded, "roundtrip mismatch for {type_name}");
    };

    let (ctx, _clock) = test_ctx_and_clock(1_700_000_000);
    let cid = 501_u64;
    let oid = 601_u64;
    let tid = 701_u64;
    let char_id = 100_000_001_u32;

    let camp = CampaignRecord {
        id: cid,
        name: "caldari insurgency".into(),
        description: "desc".into(),
        namespace: "pirate_insurgency".into(),
        scheduled_start_unix: 1_699_000_000,
        scheduled_expiry_unix: 1_710_000_000,
        state: CAMPAIGN_STATE_ACTIVE,
        transition_unix: 1_699_000_100,
        etag: 1,
        version_notice_pending: false,
        objectives: vec![oid],
    };
    let camp_upcoming = CampaignRecord {
        id: cid + 1,
        state: 0,
        ..camp.clone()
    };
    let obj = CampaignObjectiveRecord {
        id: oid,
        campaign_id: cid,
        name: "mine asteroid".into(),
        target_progress: 100,
        current_progress: 25,
        committed_participants: vec![char_id],
        resigned_participants: Vec::new(),
        contributors: HashMap::from([(char_id, (25, 2))]),
        contribution_method: "MineAsteroid".into(),
        max_progress_per_participant: 50,
        isk_reward_per_interval: 10_000,
        isk_reward_progress_interval: 10,
        state: OBJECTIVE_STATE_ACTIVE,
        transition_unix: 1_699_000_100,
        parameters: vec![ObjectiveParameter {
            key: "solar_system".into(),
            values: vec!["30000142".into()],
        }],
    };
    let trk = CampaignTrackRecord {
        id: tid,
        campaign_id: cid,
        key: "suppression".into(),
        target_progress_value: 500,
        current_progress: 40,
        participants: HashMap::from([(char_id, 40)]),
        max_participant_contribution: 100,
        isk_reward_per_interval: 25_000,
        isk_reward_progress_interval: 20,
        redeemed_intervals: HashMap::new(),
        ticking: vec![TickingContribution {
            key: "passive".into(),
            interval_secs: 60,
            evaluations: vec![
                TickingEvaluation { key: "base".into(), constant: Some(5), per_unit_value: None },
                TickingEvaluation { key: "per_pilot".into(), constant: None, per_unit_value: Some(2) },
            ],
            last_tick_unix: 1_699_000_000,
        }],
        state: TRACK_STATE_STARTED,
        transition_unix: 1_699_000_100,
    };

    roundtrip("eve_public.campaign.Stats", &encode_campaign_stats(&[&obj]));
    roundtrip("eve_public.campaign.Attributes", &encode_campaign_attributes(&camp, &[&obj]));
    roundtrip("eve_public.campaign.objective.Stats", &encode_objective_stats(&obj));
    roundtrip("eve_public.campaign.objective.Attributes", &encode_objective_attributes(&obj));
    roundtrip("eve_public.campaign.track.Attributes", &encode_track_attributes(&trk));
    roundtrip("eve_public.campaign.track.Stats", &encode_track_stats(&trk));
    roundtrip("eve_public.campaign.track.ParticipantStats", &encode_track_participant_stats(&trk, char_id));
    roundtrip("eve_public.campaign.track.ParticipantStats", &encode_track_participant_stats(&trk, 999_999));
    roundtrip("eve_public.campaign.api.GetStatusResponse.Track", &encode_status_track(&trk));
    roundtrip(
        "eve_public.campaign.track.api.GetTickingContributionsPredictionResponse",
        &encode_ticking_prediction(&trk, 3),
    );

    let mgr = Arc::new(ProgressionExtraManager::new(Some(Arc::clone(&ctx.db))));
    register_campaign_handlers(&ctx.quasar_gateway, Arc::clone(&mgr));
    mgr.register_campaign(camp, vec![obj], vec![trk]).expect("register active");
    mgr.register_campaign(camp_upcoming, vec![], vec![]).expect("register upcoming");

    let auth = AuthoritativeContext::for_session(Some(1), Some(char_id), Some(1_000_009), u64::MAX, "tranquility", vec![]);
    let any = GoogleAny::default();
    let empty = DynamicQuasarMessage::new("Req");
    let id_msg = id_dyn_msg(campaign_uuid(cid));
    let mut ns_sub = DynamicQuasarMessage::new("Namespace");
    ns_sub.set(1, DynamicValue::String("pirate_insurgency".into()));
    let mut ns_req = DynamicQuasarMessage::new("Req");
    ns_req.set(1, DynamicValue::Message(Box::new(ns_sub)));
    let handlers = ctx.quasar_gateway.handlers.read().unwrap().clone();

    for (req_name, req) in [
        ("eve_public.campaign.api.GetAllRequest", &empty),
        ("eve_public.campaign.api.GetAllForNamespaceRequest", &ns_req),
        ("eve_public.campaign.api.GetRequest", &id_msg),
        ("eve_public.campaign.api.GetStatusRequest", &id_msg),
        ("eve_public.campaign.api.GetStatsRequest", &id_msg),
        ("eve_public.campaign.api.GetParticipantStatsRequest", &id_msg),
        ("eve_public.campaign.api.GetLimitsRequest", &empty),
        ("eve_public.campaign.api.GetAllUnredeemedRewardsRequest", &empty),
        ("eve_public.campaign.api.GetAllUnredeemedParticipantRewardsRequest", &ns_req),
    ] {
        let resp = handlers[req_name](&auth, &any, req).unwrap_or_else(|e| panic!("{req_name}: {e:?}"));
        roundtrip(&resp.type_url, &resp.value);
    }
}

