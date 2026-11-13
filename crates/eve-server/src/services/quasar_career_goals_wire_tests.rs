use super::*;
use eve_proto::quasar::{encode_bytes_field, encode_varint_field};

#[test]
fn career_goal_wire_encoders_and_extractors() {
    let uuid = [9u8; 16];
    let mut id_out = Vec::new();
    encode_career_goal_identifier(&uuid, &mut id_out);
    assert!(!id_out.is_empty());

    let def = CareerGoalDef {
        uuid,
        career_id: 2,
        activity_name: "Salvage".into(),
        activity_kind: "salvage".into(),
        name: "Salvage wrecks".into(),
        description: "Salvage 3 wrecks".into(),
        target: 3,
        career_points: 20,
        isk_reward: 75_000,
        sp_reward: 250.0,
        name_message_id: 601_356,
        description_message_id: 601_544,
        threat: 2,
        duration: Some(40),
        aura_text_id: Some(601_732),
        agency_link_text_id: Some(601_920),
        agency_screen: Some("COSMIC_SIGNATURES".into()),
        order: Some(3),
        _provenance: None,
    };

    let goal_unclaimed = CharacterCareerGoal {
        uuid,
        def: def.clone(),
        progress: 1,
        completed: false,
        claimed: false,
        completed_time: None,
    };
    let mut buf1 = Vec::new();
    encode_career_goal_wire(&goal_unclaimed, &mut buf1);
    assert!(!buf1.is_empty());

    let goal_claimed = CharacterCareerGoal {
        uuid,
        def: def.clone(),
        progress: 3,
        completed: true,
        claimed: true,
        completed_time: Some(1_700_000_123),
    };
    let mut buf2 = Vec::new();
    encode_career_goal_wire(&goal_claimed, &mut buf2);
    assert!(!buf2.is_empty());

    let mut def_buf = Vec::new();
    encode_career_goal_definition_wire(&def, &mut def_buf);
    assert!(!def_buf.is_empty());
    assert!(def_buf.windows(b"auraTextID".len()).any(|w| w == b"auraTextID"));
    assert!(def_buf.windows(b"duration".len()).any(|w| w == b"duration"));
    assert!(def_buf.windows(b"agencyScreen".len()).any(|w| w == b"agencyScreen"));

    let mut int_def_buf = Vec::new();
    encode_internal_career_goal_definition_wire(&def, &mut int_def_buf);
    assert!(!int_def_buf.is_empty());
    assert!(int_def_buf.windows(b"auraTextID".len()).any(|w| w == b"auraTextID"));

    let ms_career = CareerMilestoneDef {
        milestone_id: 2,
        career_id: Some(2),
        required_points: 750,
        sp_reward: 100_000.0,
        description: "Industrialist Graduation".into(),
    };
    let ms_overall = CareerMilestoneDef {
        milestone_id: 101,
        career_id: None,
        required_points: 750,
        sp_reward: 50_000.0,
        description: "Overall 750".into(),
    };

    let mut ms_buf1 = Vec::new();
    encode_career_milestone_definition_wire(&ms_career, &mut ms_buf1);
    encode_career_milestone_definition_wire(&ms_overall, &mut ms_buf1);
    assert!(!ms_buf1.is_empty());

    let mut ms_buf2 = Vec::new();
    encode_internal_career_milestone_definition_wire(&ms_career, &mut ms_buf2);
    encode_internal_career_milestone_definition_wire(&ms_overall, &mut ms_buf2);
    assert!(!ms_buf2.is_empty());

    let mut ms_goal_buf = Vec::new();
    encode_career_milestone_goal_wire(&ms_career, 400, false, false, &mut ms_goal_buf);
    encode_career_milestone_goal_wire(&ms_overall, 800, true, true, &mut ms_goal_buf);
    assert!(!ms_goal_buf.is_empty());

    let mut id_msg = DynamicQuasarMessage::new("Identifier");
    id_msg.fields.insert(1, DynamicValue::Bytes(uuid.to_vec()));
    let mut outer = DynamicQuasarMessage::new("Req");
    outer.fields.insert(1, DynamicValue::Message(Box::new(id_msg)));
    assert_eq!(extract_uuid(&outer, 1), Some(uuid));
    assert_eq!(extract_uuid(&outer, 2), None);

    let mut char_sub = DynamicQuasarMessage::new("CharId");
    char_sub.fields.insert(1, DynamicValue::Uint32(100_000_001));
    outer.fields.insert(2, DynamicValue::Message(Box::new(char_sub)));
    outer.fields.insert(3, DynamicValue::Uint64(100_000_002));
    let any_empty = GoogleAny::default();
    assert_eq!(extract_target_character(&outer, &any_empty, 2, false), Some(100_000_001));
    assert_eq!(extract_target_character(&outer, &any_empty, 3, true), Some(100_000_002));
    assert_eq!(extract_target_character(&outer, &any_empty, 3, false), None);
}

#[test]
fn career_goal_wire_decoded_fields_and_edge_extractors() {
    let uuid = [0x33u8; 16];

    // 1. Verify decoded fields of encode_career_goal_wire across claim/completion/timestamp states
    let def_no_rewards = CareerGoalDef {
        uuid,
        career_id: 99, // clamped to 4
        activity_name: "Unknown".into(),
        activity_kind: "".into(),
        name: "Goal Zero".into(),
        description: "Test zero target and no rewards".into(),
        target: 0, // clamped to 1
        career_points: 10,
        isk_reward: 0,
        sp_reward: 0.0,
        name_message_id: 0,
        description_message_id: 0,
        threat: 0,
        _provenance: None,
        ..Default::default()
    };

    let goal_claimed_no_ts = CharacterCareerGoal {
        uuid,
        def: def_no_rewards.clone(),
        progress: 2,
        completed: true,
        claimed: true,
        completed_time: None,
    };
    let mut g_buf = Vec::new();
    encode_career_goal_wire(&goal_claimed_no_ts, &mut g_buf);
    let g_wm = WireMessage::decode(&g_buf).unwrap();
    assert_eq!(
        g_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()),
        Some(2)
    );
    assert_eq!(
        g_wm.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()),
        Some(0) // completed = 0
    );
    assert!(g_wm.fields.iter().all(|f| f.field_number != 4)); // no completed_time
    let label_bytes = g_wm
        .fields
        .iter()
        .find(|f| f.field_number == 5)
        .and_then(|f| f.value.as_bytes())
        .unwrap();
    let label_wm = WireMessage::decode(label_bytes).unwrap();
    assert!(label_wm.fields.iter().any(|f| f.field_number == 2)); // claimed timestamp

    // Completed but unclaimed
    let goal_unclaimed_done = CharacterCareerGoal {
        uuid,
        def: def_no_rewards.clone(),
        progress: 1,
        completed: true,
        claimed: false,
        completed_time: Some(1_700_000_999),
    };
    let mut g_buf2 = Vec::new();
    encode_career_goal_wire(&goal_unclaimed_done, &mut g_buf2);
    let g_wm2 = WireMessage::decode(&g_buf2).unwrap();
    assert!(g_wm2.fields.iter().any(|f| f.field_number == 4));
    let label_wm2 = WireMessage::decode(
        g_wm2
            .fields
            .iter()
            .find(|f| f.field_number == 5)
            .and_then(|f| f.value.as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        label_wm2.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()),
        Some(1) // not_claimed = true
    );

    // 2. Definition encoders with 0 rewards, clamped career_id, target=0->1
    let mut def_buf = Vec::new();
    encode_career_goal_definition_wire(&def_no_rewards, &mut def_buf);
    let def_wm = WireMessage::decode(&def_buf).unwrap();
    let attr_wm = WireMessage::decode(
        def_wm
            .fields
            .iter()
            .find(|f| f.field_number == 2)
            .and_then(|f| f.value.as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        attr_wm.fields.iter().find(|f| f.field_number == 4).and_then(|f| f.value.as_varint()),
        Some(1)
    );
    assert_eq!(
        attr_wm.fields.iter().find(|f| f.field_number == 9).and_then(|f| f.value.as_varint()),
        Some(4)
    );
    assert!(attr_wm.fields.iter().all(|f| f.field_number != 12)); // no rewards

    let mut int_buf = Vec::new();
    encode_internal_career_goal_definition_wire(&def_no_rewards, &mut int_buf);
    let int_attr_wm = WireMessage::decode(
        WireMessage::decode(&int_buf)
            .unwrap()
            .fields
            .iter()
            .find(|f| f.field_number == 2)
            .and_then(|f| f.value.as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        int_attr_wm.fields.iter().find(|f| f.field_number == 7).and_then(|f| f.value.as_varint()),
        Some(4)
    );
    assert!(int_attr_wm.fields.iter().all(|f| f.field_number != 9));

    // 3. Milestone encoders with zero sp_reward, zero required_points, and progress clamping
    let ms_zero = CareerMilestoneDef {
        milestone_id: 77,
        career_id: Some(99), // unknown career falls back to 582_520 + 77, clamped career = 4
        required_points: 0,
        sp_reward: 0.0,
        description: "Fallback milestone".into(),
    };
    let mut ms_def_buf = Vec::new();
    encode_career_milestone_definition_wire(&ms_zero, &mut ms_def_buf);
    let ms_attr_wm = WireMessage::decode(
        WireMessage::decode(&ms_def_buf)
            .unwrap()
            .fields
            .iter()
            .find(|f| f.field_number == 2)
            .and_then(|f| f.value.as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        ms_attr_wm.fields.iter().find(|f| f.field_number == 4).and_then(|f| f.value.as_varint()),
        Some(1)
    );
    assert!(ms_attr_wm.fields.iter().all(|f| f.field_number != 12));

    let mut ms_int_buf = Vec::new();
    encode_internal_career_milestone_definition_wire(&ms_zero, &mut ms_int_buf);
    let ms_int_attr = WireMessage::decode(
        WireMessage::decode(&ms_int_buf)
            .unwrap()
            .fields
            .iter()
            .find(|f| f.field_number == 2)
            .and_then(|f| f.value.as_bytes())
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        ms_int_attr.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()),
        Some(1)
    );
    assert!(ms_int_attr.fields.iter().all(|f| f.field_number != 9));

    let ms_cap = CareerMilestoneDef {
        milestone_id: 5,
        career_id: Some(1),
        required_points: 500,
        sp_reward: 100.0,
        description: "Cap".into(),
    };
    let mut ms_goal_buf = Vec::new();
    encode_career_milestone_goal_wire(&ms_cap, 999, false, false, &mut ms_goal_buf);
    let ms_goal_wm = WireMessage::decode(&ms_goal_buf).unwrap();
    assert_eq!(
        ms_goal_wm.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()),
        Some(500) // clamped to required_points
    );
    assert_eq!(
        ms_goal_wm.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()),
        Some(1) // reached = false -> 1
    );

    // 4. extract_uuid negative cases
    let mut bad_len_id = DynamicQuasarMessage::new("Identifier");
    bad_len_id.fields.insert(1, DynamicValue::Bytes(vec![1u8; 8]));
    let mut bad_outer = DynamicQuasarMessage::new("Req");
    bad_outer.fields.insert(1, DynamicValue::Message(Box::new(bad_len_id)));
    bad_outer.fields.insert(2, DynamicValue::Bytes(uuid.to_vec()));
    bad_outer.fields.insert(3, DynamicValue::Message(Box::new(DynamicQuasarMessage::new("Empty"))));
    assert_eq!(extract_uuid(&bad_outer, 1), None);
    assert_eq!(extract_uuid(&bad_outer, 2), None);
    assert_eq!(extract_uuid(&bad_outer, 3), None);

    // 5. extract_target_character across all numeric types & WireMessage fallback
    let mut m_u64 = DynamicQuasarMessage::new("C");
    m_u64.fields.insert(1, DynamicValue::Uint64(90_000_010));
    let mut m_i32 = DynamicQuasarMessage::new("C");
    m_i32.fields.insert(1, DynamicValue::Int32(90_000_011));
    let mut m_i64 = DynamicQuasarMessage::new("C");
    m_i64.fields.insert(1, DynamicValue::Int64(90_000_012));
    let mut m_neg = DynamicQuasarMessage::new("C");
    m_neg.fields.insert(1, DynamicValue::Int64(-5));

    let mut dyn_all = DynamicQuasarMessage::new("Req");
    dyn_all.fields.insert(1, DynamicValue::Message(Box::new(m_u64)));
    dyn_all.fields.insert(2, DynamicValue::Message(Box::new(m_i32)));
    dyn_all.fields.insert(3, DynamicValue::Message(Box::new(m_i64)));
    dyn_all.fields.insert(4, DynamicValue::Message(Box::new(m_neg)));
    dyn_all.fields.insert(5, DynamicValue::Uint32(999_999)); // below 1_000_000 threshold
    dyn_all.fields.insert(6, DynamicValue::Uint32(1_000_000));
    dyn_all.fields.insert(7, DynamicValue::Int32(1_000_005));
    dyn_all.fields.insert(8, DynamicValue::Int64(1_000_006));

    let any_empty = GoogleAny::default();
    assert_eq!(extract_target_character(&dyn_all, &any_empty, 1, false), Some(90_000_010));
    assert_eq!(extract_target_character(&dyn_all, &any_empty, 2, false), Some(90_000_011));
    assert_eq!(extract_target_character(&dyn_all, &any_empty, 3, false), Some(90_000_012));
    assert_eq!(extract_target_character(&dyn_all, &any_empty, 4, false), None);
    assert_eq!(extract_target_character(&dyn_all, &any_empty, 5, true), None);
    assert_eq!(extract_target_character(&dyn_all, &any_empty, 6, true), Some(1_000_000));
    assert_eq!(extract_target_character(&dyn_all, &any_empty, 7, true), Some(1_000_005));
    assert_eq!(extract_target_character(&dyn_all, &any_empty, 8, true), Some(1_000_006));

    // WireMessage fallback when dyn_msg lacks the field
    let mut raw_sub = Vec::new();
    encode_varint_field(1, 90_000_077, &mut raw_sub);
    let mut raw_zero = Vec::new();
    encode_varint_field(1, 0, &mut raw_zero);
    let mut raw_bytes = Vec::new();
    encode_bytes_field(10, &raw_sub, &mut raw_bytes);
    encode_bytes_field(11, &raw_zero, &mut raw_bytes);
    encode_varint_field(12, 90_000_088, &mut raw_bytes);
    encode_varint_field(13, 500, &mut raw_bytes);
    let raw_any = GoogleAny {
        type_url: String::new(),
        value: raw_bytes,
    };
    let empty_dyn = DynamicQuasarMessage::new("Empty");
    assert_eq!(extract_target_character(&empty_dyn, &raw_any, 10, false), Some(90_000_077));
    assert_eq!(extract_target_character(&empty_dyn, &raw_any, 11, false), None);
    assert_eq!(extract_target_character(&empty_dyn, &raw_any, 12, false), None);
    assert_eq!(extract_target_character(&empty_dyn, &raw_any, 12, true), Some(90_000_088));
    assert_eq!(extract_target_character(&empty_dyn, &raw_any, 13, true), None);
}

#[test]
fn career_goal_wire_encoders_roundtrip_via_strict_descriptor_decode() {
    let reg = eve_proto::quasar::QuasarDescriptorRegistry::open_default()
        .expect("open default descriptor DB");
    let def = CareerGoalDef {
        uuid: [0x22u8; 16],
        name: "Scan 5 Signatures".to_string(),
        description: "Scan 5 cosmic signatures".to_string(),
        career_id: 1,
        target: 5,
        career_points: 20,
        isk_reward: 50_000,
        sp_reward: 1_500.0,
        ..Default::default()
    };
    let goal = CharacterCareerGoal {
        uuid: def.uuid,
        def: def.clone(),
        progress: 3,
        completed: false,
        claimed: false,
        completed_time: None,
    };
    let ms = CareerMilestoneDef {
        milestone_id: 1,
        career_id: Some(1),
        required_points: 750,
        sp_reward: 5_000.0,
        description: "Explorer Graduation".to_string(),
    };

    let mut buf = Vec::new();
    encode_career_goal_wire(&goal, &mut buf);
    let dec = reg
        .decode_dynamic("eve_public.career.goal.api.GetAllResponse.Goal", &buf)
        .expect("decode GetAllResponse.Goal");
    assert_eq!(
        reg.encode_dynamic(&dec).unwrap(),
        buf
    );

    buf.clear();
    encode_career_goal_definition_wire(&def, &mut buf);
    let dec = reg
        .decode_dynamic("eve_public.career.goal.api.GetDefinitionsResponse.Goal", &buf)
        .expect("decode public goal definition");
    assert_eq!(
        reg.encode_dynamic(&dec).unwrap(),
        buf
    );

    buf.clear();
    encode_internal_career_goal_definition_wire(&def, &mut buf);
    let dec = reg
        .decode_dynamic("eve.career.goal.api.GetDefinitionsResponse.Goal", &buf)
        .expect("decode internal goal definition");
    assert_eq!(
        reg.encode_dynamic(&dec).unwrap(),
        buf
    );

    buf.clear();
    encode_career_milestone_definition_wire(&ms, &mut buf);
    reg.decode_dynamic("eve_public.career.goal.api.GetDefinitionsResponse.Goal", &buf)
        .expect("decode public milestone definition");

    buf.clear();
    encode_internal_career_milestone_definition_wire(&ms, &mut buf);
    reg.decode_dynamic("eve.career.goal.api.GetDefinitionsResponse.Goal", &buf)
        .expect("decode internal milestone definition");

    buf.clear();
    encode_career_milestone_goal_wire(&ms, 400, false, false, &mut buf);
    reg.decode_dynamic("eve_public.career.goal.api.GetAllResponse.Goal", &buf)
        .expect("decode milestone goal");
}
