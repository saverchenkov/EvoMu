//! Edge-case tests for `quasar_goal_tale_notices`: DynamicValue formatting variants,
//! opportunity event decoding/eviction/authorization guards, server opportunity multi-condition
//! progression, tale category matching, and community track saturation.

#![forbid(unsafe_code)]

use super::*;
use crate::quasar_gateway::PublicGatewayRouter;
use crate::services::quasar_achievements::{AchievementConditionDef, AchievementDef};
use crate::services::quasar_progression::new_progression_managers;
use crate::services::quasar_progression_extra::CommunityTrackState;
use eve_db::WorldRepository;
use eve_machonet::{service_roles, SessionState};
use eve_proto::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage, DynamicValue,
    GoogleAny, QuasarDescriptorRegistry, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_sde::{DungeonTemplate, SdeRegistry};
use std::collections::HashSet;
use std::sync::Arc;

#[test]
fn opportunity_id_formatting_variants_and_event_guards() {
    assert_eq!(format_opportunity_id_value(&DynamicValue::String("s".into())), "s");
    assert_eq!(format_opportunity_id_value(&DynamicValue::Bytes(vec![0xab, 0xcd])), "abcd");
    assert_eq!(format_opportunity_id_value(&DynamicValue::Uint64(42)), "42");
    assert_eq!(format_opportunity_id_value(&DynamicValue::Int64(-7)), "-7");
    assert_eq!(format_opportunity_id_value(&DynamicValue::Uint32(9)), "9");
    assert_eq!(format_opportunity_id_value(&DynamicValue::Int32(-3)), "-3");
    assert_eq!(format_opportunity_id_value(&DynamicValue::Bool(true)), "true");
    assert_eq!(format_opportunity_id_value(&DynamicValue::Enum(5)), "5");
    assert_eq!(
        format_opportunity_id_value(&DynamicValue::Repeated(vec![
            DynamicValue::Uint32(1),
            DynamicValue::Uint32(2),
        ])),
        "1,2"
    );
    let mut sub = DynamicQuasarMessage::new("test.Sub");
    sub.set(1, DynamicValue::String("nested".into()));
    assert_eq!(format_opportunity_id_value(&DynamicValue::Message(Box::new(sub))), "1=nested");
    assert!(format_opportunity_id_value(&DynamicValue::Float(1.5)).contains("Float"));

    let db = Arc::new(WorldRepository::with_demo_seed());
    let sde = Arc::new(SdeRegistry::with_canonical_seed());
    let mgrs = new_progression_managers(Arc::clone(&db), Arc::clone(&sde));
    let desc = QuasarDescriptorRegistry::open_default_or_empty();
    let now = 1_760_000_100u64;
    let cid = 90_000_001u32;

    // 1. Unauthenticated / char 0 / non-opportunity event / corrupt wire bytes are ignored
    let mut sess_no_char = SessionState::new(1);
    sess_no_char.character_id = None;
    let mut sess = SessionState::new(2);
    sess.user_id = Some(1);
    sess.character_id = Some(cid as i64);
    sess.role = service_roles::ROLE_PLAYER_DEFAULT;

    handle_opportunity_event(
        &mgrs,
        &desc,
        &sess,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.debug.Notice"),
            value: vec![],
        },
        now,
    );
    handle_opportunity_event(
        &mgrs,
        &desc,
        &sess_no_char,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.opportunity.api.Tracked"),
            value: vec![],
        },
        now,
    );
    handle_opportunity_event(
        &mgrs,
        &desc,
        &sess,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.opportunity.api.Tracked"),
            value: vec![0xFF, 0xFF],
        },
        now,
    );
    assert!(db.with_snapshot(|s| !s.quasar_state_blobs.contains_key("quasar_opportunities")));

    // 2. Character ID mismatch in Tracked payload is ignored
    let mut ch_other = Vec::new();
    encode_varint_field(1, 90_000_999, &mut ch_other);
    let mut opp_var = Vec::new();
    encode_varint_field(1, 888, &mut opp_var);
    let mut opp_msg = Vec::new();
    encode_bytes_field(1, &opp_var, &mut opp_msg);
    let mut mismatch_req = Vec::new();
    encode_bytes_field(1, &ch_other, &mut mismatch_req);
    encode_bytes_field(2, &opp_msg, &mut mismatch_req);
    handle_opportunity_event(
        &mgrs,
        &desc,
        &sess,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.opportunity.api.Tracked"),
            value: mismatch_req,
        },
        now,
    );
    assert!(db.with_snapshot(|s| !s.quasar_state_blobs.contains_key("quasar_opportunities")));

    // 3. Discovered with empty tag is ignored; valid numeric tag succeeds even after corrupt blob
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert("quasar_opportunities".into(), "[]".into());
    });
    let mut disc_empty = Vec::new();
    encode_string_field(1, "   ", &mut disc_empty);
    let mut disc_opp_empty = Vec::new();
    encode_bytes_field(1, &disc_empty, &mut disc_opp_empty);
    let mut disc_req_empty = Vec::new();
    encode_bytes_field(1, &disc_opp_empty, &mut disc_req_empty);
    handle_opportunity_event(
        &mgrs,
        &desc,
        &sess,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.opportunity.api.Discovered"),
            value: disc_req_empty,
        },
        now,
    );

    let mut disc_num = Vec::new();
    encode_varint_field(1, 12345, &mut disc_num);
    let mut disc_opp_num = Vec::new();
    encode_bytes_field(1, &disc_num, &mut disc_opp_num);
    let mut disc_req_num = Vec::new();
    encode_bytes_field(1, &disc_opp_num, &mut disc_req_num);
    handle_opportunity_event(
        &mgrs,
        &desc,
        &sess,
        &GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.opportunity.api.Discovered"),
            value: disc_req_num,
        },
        now,
    );

    // 4. Pre-seed 256 tracked/viewed/history entries to exercise ring-buffer eviction on Tracked & Viewed
    let pre_keys: Vec<String> = (0..256).map(|i| format!("v1:{i}")).collect();
    let pre_hist: Vec<serde_json::Value> = (0..256)
        .map(|i| serde_json::json!({"event": "eve_public.opportunity.api.Viewed", "key": format!("v1:{i}")}))
        .collect();
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert(
            "quasar_opportunities".into(),
            serde_json::json!({
                cid.to_string(): {
                    "tracked": pre_keys,
                    "viewed": pre_keys,
                    "history": pre_hist,
                    "completed": []
                }
            })
            .to_string(),
        );
    });

    let mut ch_ok = Vec::new();
    encode_varint_field(1, cid as u64, &mut ch_ok);
    let mut track_req = Vec::new();
    encode_bytes_field(1, &ch_ok, &mut track_req);
    encode_bytes_field(2, &opp_msg, &mut track_req);
    for ev in [
        "eve_public.opportunity.api.Tracked",
        "eve_public.opportunity.api.Viewed",
        "eve_public.opportunity.api.Completed",
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

    db.with_snapshot(|s| {
        let val: serde_json::Value =
            serde_json::from_str(s.quasar_state_blobs.get("quasar_opportunities").unwrap()).unwrap();
        let entry = &val[cid.to_string()];
        assert_eq!(entry["tracked"].as_array().unwrap().len(), 256);
        assert_eq!(entry["viewed"].as_array().unwrap().len(), 256);
        assert_eq!(entry["history"].as_array().unwrap().len(), 256);
    });
}

#[test]
fn server_opportunity_multi_condition_and_tale_archetype_edges() {
    let db = Arc::new(WorldRepository::with_demo_seed());
    let now = 1_760_000_200u64;
    let cid = 90_000_001u32;

    let defs = vec![
        AchievementDef {
            id: 6001,
            name: "Client Only".into(),
            description: String::new(),
            notification_text: String::new(),
            category_id: 1,
            target: 1,
            score: 5,
            milestone_targets: vec![],
            reward_isk: 0,
            reward_sp: 0,
            is_client_achievement: true,
            conditions: vec![AchievementConditionDef {
                condition_id: 1,
                statistic_event_id: 1,
                event_name: "ev_a".into(),
                target_value: 1,
            }],
        },
        AchievementDef {
            id: 6002,
            name: "Two Step".into(),
            description: String::new(),
            notification_text: String::new(),
            category_id: 1,
            target: 2,
            score: 10,
            milestone_targets: vec![],
            reward_isk: 0,
            reward_sp: 0,
            is_client_achievement: false,
            conditions: vec![
                AchievementConditionDef {
                    condition_id: 1,
                    statistic_event_id: 1,
                    event_name: "ev_a".into(),
                    target_value: 2,
                },
                AchievementConditionDef {
                    condition_id: 2,
                    statistic_event_id: 2,
                    event_name: "ev_b".into(),
                    target_value: 1,
                },
            ],
        },
    ];

    // No-op paths: delta = 0, empty event_names, non-matching event
    record_server_opportunity_event(&db, &defs, cid, &["ev_a"], 0, now);
    record_server_opportunity_event(&db, &defs, cid, &[], 1, now);
    record_server_opportunity_event(&db, &defs, cid, &["ev_unknown"], 1, now);
    assert!(db.with_snapshot(|s| !s.quasar_state_blobs.contains_key("quasar_opportunities")));

    // Corrupt blob gets replaced cleanly; completing only cond 1 doesn't complete achievement 6002
    db.with_mut_snapshot(|s| {
        s.quasar_state_blobs.insert("quasar_opportunities".into(), "not-json".into());
    });
    record_server_opportunity_event(&db, &defs, cid, &["ev_a"], 2, now);
    db.with_snapshot(|s| {
        let v: serde_json::Value =
            serde_json::from_str(s.quasar_state_blobs.get("quasar_opportunities").unwrap()).unwrap();
        assert!(v[cid.to_string()]["completed"].as_array().unwrap().is_empty());
    });

    // Completing cond 2 marks 6002 completed once
    record_server_opportunity_event(&db, &defs, cid, &["ev_b"], 1, now + 5);
    record_server_opportunity_event(&db, &defs, cid, &["ev_b"], 1, now + 10);
    db.with_snapshot(|s| {
        let v: serde_json::Value =
            serde_json::from_str(s.quasar_state_blobs.get("quasar_opportunities").unwrap()).unwrap();
        assert_eq!(v[cid.to_string()]["completed"].as_array().unwrap().len(), 1);
        assert_eq!(v[cid.to_string()]["completed_at"]["6002"].as_u64(), Some(now + 5));
    });

    // Tale encounter matching across all site categories + SDE archetype checks
    let mut sde = SdeRegistry::with_canonical_seed();
    sde.dungeons.insert(
        7001,
        DungeonTemplate {
            dungeon_id: 7001,
            name: "Trig Site".into(),
            archetype_id: Some(65),
            faction_id: None,
            difficulty: None,
        },
    );
    sde.dungeons.insert(
        7002,
        DungeonTemplate {
            dungeon_id: 7002,
            name: "DED Site".into(),
            archetype_id: Some(28),
            faction_id: None,
            difficulty: None,
        },
    );
    let mut event_tpls = HashSet::new();
    event_tpls.insert(7002);
    event_tpls.insert(7003);

    assert!(tale_matches_completed_encounter(Some(&sde), &event_tpls, 7001, "Triglavian", 0));
    for cat in ["Relic", "Ghost", "SleeperCache", "WormholeCombat", "DrifterHive", "Homefront"] {
        assert!(tale_matches_completed_encounter(Some(&sde), &event_tpls, 555, cat, 555));
        assert!(!tale_matches_completed_encounter(Some(&sde), &event_tpls, 555, cat, 0));
    }
    // CombatAnomaly with archetype 28 requires exact dungeon_id match
    assert!(!tale_matches_completed_encounter(Some(&sde), &event_tpls, 7002, "CombatAnomaly", 0));
    assert!(tale_matches_completed_encounter(Some(&sde), &event_tpls, 7002, "DED", 7002));
    assert!(tale_matches_completed_encounter(Some(&sde), &event_tpls, 7003, "EventSite", 0));

    // complete_system_tale_encounter ignores inactive tracks and different systems, saturates at target_progress
    let sde = Arc::new(sde);
    let router = PublicGatewayRouter::new(
        QuasarDescriptorRegistry::open_default_or_empty(),
        Arc::clone(&db),
        Arc::clone(&sde),
    );
    let mgr = &router.progression.extra;
    mgr.active_tales.write().unwrap().insert(1, (7003, 30_000_142, 0));
    mgr.community_tracks.write().unwrap().insert(
        1,
        CommunityTrackState {
            track_id: 1,
            template_id: 1,
            name: "Inactive".into(),
            current_progress: 0,
            target_progress: 50,
            active: false,
            tale_templates: vec![7003],
        },
    );
    mgr.community_tracks.write().unwrap().insert(
        2,
        CommunityTrackState {
            track_id: 2,
            template_id: 2,
            name: "Active".into(),
            current_progress: 40,
            target_progress: 50,
            active: true,
            tale_templates: vec![7003],
        },
    );

    // Wrong system -> nothing progressed
    assert!(complete_system_tale_encounter(mgr, 30_000_143, "Event", 7003, 20, now, &router).is_empty());
    // Matching system -> saturates at 50
    let res = complete_system_tale_encounter(mgr, 30_000_142, "Event", 7003, 20, now, &router);
    assert_eq!(res.len(), 1);
    assert_eq!(res[0].track_id, 2);
    assert_eq!(res[0].current_progress, 50);

    // Direct unit checks for `extract_raw_length_delimited_field` and `encode_goal_attributes`
    let mut wire = Vec::new();
    encode_varint_field(1, 42, &mut wire);
    encode_bytes_field(4, &[0xAA, 0xBB, 0xCC], &mut wire);
    assert_eq!(
        extract_raw_length_delimited_field(&wire, 4),
        Some(vec![0xAA, 0xBB, 0xCC])
    );
    assert_eq!(extract_raw_length_delimited_field(&wire, 1), None);
    assert_eq!(extract_raw_length_delimited_field(&wire, 9), None);

    let active_bytes = encode_goal_attributes(&GoalAttributesView {
        created_unix: 1_700_000_000,
        creator_id: 90_000_001,
        corp_id: Some(98_000_001),
        name: "Mine Veldspar",
        description: "Corp ore goal",
        desired: 1_000,
        current: 250,
        state: None,
        contribution_config_pb: &[],
    });
    let completed_bytes = encode_goal_attributes(&GoalAttributesView {
        created_unix: 1_700_000_000,
        creator_id: 90_000_001,
        corp_id: None,
        name: "Done",
        description: "",
        desired: 100,
        current: 100,
        state: None,
        contribution_config_pb: &[0x0A, 0x00],
    });
    assert!(extract_raw_length_delimited_field(&active_bytes, 9).is_some());
    assert!(extract_raw_length_delimited_field(&completed_bytes, 9).is_none());
    assert!(extract_raw_length_delimited_field(&completed_bytes, 11).is_some());
}

#[test]
fn goal_wire_encoders_and_extractors_kill_all_mutants() {
    use eve_proto::quasar::{encode_bytes_field, WireMessage, WireValue};

    // 1. extract_raw_length_delimited_field: empty bytes returns None; wrong field None; non-empty Some
    let mut wire_empty_field = Vec::new();
    encode_bytes_field(7, &[], &mut wire_empty_field);
    assert_eq!(extract_raw_length_delimited_field(&wire_empty_field, 7), None);

    let mut wire_nonempty = Vec::new();
    encode_bytes_field(7, &[0xDE, 0xAD], &mut wire_nonempty);
    assert_eq!(extract_raw_length_delimited_field(&wire_nonempty, 8), None);
    assert_eq!(
        extract_raw_length_delimited_field(&wire_nonempty, 7),
        Some(vec![0xDE, 0xAD])
    );

    let field_map = |bytes: &[u8]| -> Vec<(u32, WireValue)> {
        WireMessage::decode(bytes)
            .unwrap()
            .fields
            .into_iter()
            .map(|f| (f.field_number, f.value))
            .collect()
    };

    // 2. encode_goal_contribution_config across all 13 match arms + filter_id == 0 vs > 0
    let cases: [(&str, u32); 15] = [
        ("DeliverItem", 2),
        ("DestroyNPCShips", 3),
        ("DamageShips", 4),
        ("MineMaterials", 5),
        ("MineOre", 5),
        ("DestroyCapsuleerShips", 6),
        ("CaptureFWComplexes", 7),
        ("DefendFWComplexes", 8),
        ("RemoteRepairArmor", 9),
        ("RemoteRepairShield", 10),
        ("ScanSignatures", 12),
        ("ManufactureItems", 14),
        ("SalvageWrecks", 16),
        ("EarnLoyaltyPoints", 17),
        ("ManualFallback", 1),
    ];
    for (matcher, expected_outer_field) in cases {
        let b_none = encode_goal_contribution_config(matcher, None);
        let b_zero = encode_goal_contribution_config(matcher, Some(0));
        let b_pos = encode_goal_contribution_config(matcher, Some(42));
        assert_eq!(
            b_none, b_zero,
            "filter_id=Some(0) must behave identically to None for {matcher}"
        );
        let top_zero = field_map(&b_zero);
        assert!(
            top_zero.iter().any(|(n, _)| *n == expected_outer_field),
            "{matcher} must emit outer field {expected_outer_field}"
        );
        if matches!(
            matcher,
            "DeliverItem" | "DestroyNPCShips" | "DamageShips" | "MineMaterials" | "MineOre" | "ManufactureItems"
        ) {
            assert_ne!(
                b_zero, b_pos,
                "{matcher} with filter_id=42 must encode filter ID differently from 0"
            );
        }
    }

    // Verify exact inner filter encoding on filtered matchers
    let mine_pos = encode_goal_contribution_config("MineOre", Some(1230));
    let outer5 = extract_raw_length_delimited_field(&mine_pos, 5).unwrap();
    let method5 = extract_raw_length_delimited_field(&outer5, 1).unwrap();
    assert!(extract_raw_length_delimited_field(&method5, 4).is_some()); // ore = seq_id(1230)

    let mfg_pos = encode_goal_contribution_config("ManufactureItems", Some(587));
    let outer14 = extract_raw_length_delimited_field(&mfg_pos, 14).unwrap();
    let method14 = extract_raw_length_delimited_field(&outer14, 3).unwrap();
    assert!(extract_raw_length_delimited_field(&method14, 1).is_some());

    // 3. encode_goal_attributes: creator_id == 0 vs > 0, corp_id Some(0) vs Some(>0), current == desired, custom config, goal_created
    let zero_attrs = encode_goal_attributes(&GoalAttributesView {
        created_unix: 1_700_000_000,
        creator_id: 0,
        corp_id: Some(0),
        name: "G0",
        description: "D0",
        desired: 100,
        current: 99,
        state: None,
        contribution_config_pb: &[],
    });
    assert!(extract_raw_length_delimited_field(&zero_attrs, 4).is_none());
    assert!(extract_raw_length_delimited_field(&zero_attrs, 9).is_none());
    let zf = field_map(&zero_attrs);
    assert!(zf.iter().any(|(n, v)| *n == 6 && *v == WireValue::Varint(GOAL_STATE_ACTIVE)));
    assert!(zf.iter().any(|(n, v)| *n == 10 && *v == WireValue::Varint(1)));
    assert!(extract_raw_length_delimited_field(&zero_attrs, 11).is_none());
    assert_eq!(
        extract_raw_length_delimited_field(&zero_attrs, 7).unwrap(),
        encode_goal_contribution_config("Manual", None)
    );

    let full_attrs_view = GoalAttributesView {
        created_unix: 1_700_000_123,
        creator_id: 555,
        corp_id: Some(98_000_111),
        name: "G1",
        description: "D1",
        desired: 100,
        current: 100, // == boundary -> COMPLETED
        state: None,
        contribution_config_pb: &[0x10, 0x2A],
    };
    let full_attrs = encode_goal_attributes(&full_attrs_view);
    assert!(extract_raw_length_delimited_field(&full_attrs, 4).is_some());
    assert!(extract_raw_length_delimited_field(&full_attrs, 9).is_some());
    assert!(extract_raw_length_delimited_field(&full_attrs, 19).is_some());
    assert!(extract_raw_length_delimited_field(&full_attrs, 20).is_some());
    assert_eq!(
        extract_raw_length_delimited_field(&full_attrs, 7).unwrap(),
        vec![0x10, 0x2A]
    );
    let ff = field_map(&full_attrs);
    assert!(ff.iter().any(|(n, v)| *n == 6 && *v == WireValue::Varint(GOAL_STATE_COMPLETED)));
    assert!(!ff.iter().any(|(n, _)| *n == 10));
    assert!(extract_raw_length_delimited_field(&full_attrs, 11).is_some());

    let explicit_closed = encode_goal_attributes(&GoalAttributesView {
        created_unix: 1_700_000_123,
        creator_id: 1,
        corp_id: None,
        name: "GC",
        description: "",
        desired: 50,
        current: 10,
        state: Some(GOAL_STATE_CLOSED),
        contribution_config_pb: &[0x08, 0x01],
    });
    let cf = field_map(&explicit_closed);
    assert!(cf.iter().any(|(n, v)| *n == 6 && *v == WireValue::Varint(GOAL_STATE_CLOSED)));
    assert!(!cf.iter().any(|(n, _)| *n == 10));
    assert!(extract_raw_length_delimited_field(&explicit_closed, 11).is_some());

    let uuid = [7u8; 16];
    let created_any = goal_created(&uuid, &full_attrs_view);
    assert_eq!(
        created_any.type_url,
        "type.googleapis.com/eve_public.goal.api.CreatedNotice"
    );
    let id_field = extract_raw_length_delimited_field(&created_any.value, 1).unwrap();
    assert_eq!(extract_raw_length_delimited_field(&id_field, 1).unwrap(), uuid.to_vec());
    assert_eq!(
        extract_raw_length_delimited_field(&created_any.value, 2).unwrap(),
        full_attrs
    );
}

