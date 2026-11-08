use super::*;

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
