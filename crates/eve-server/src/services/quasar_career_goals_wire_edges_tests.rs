//! Edge-case and full-branch unit tests for `quasar_career_goals_wire.rs`.

#![forbid(unsafe_code)]

use super::*;
use eve_proto::quasar::{encode_bytes_field, encode_varint_field, DynamicQuasarMessage, DynamicValue, GoogleAny};

fn sample_def(isk: i64, sp: f64) -> CareerGoalDef {
    CareerGoalDef {
        uuid: [3u8; 16],
        career_id: 1,
        activity_name: "Mine".into(),
        activity_kind: "mining".into(),
        name: "Mine Ore".into(),
        description: "Mine 100 units of ore".into(),
        target: 0, // tests target.max(1)
        career_points: 10,
        isk_reward: isk,
        sp_reward: sp,
        name_message_id: 582_601,
        description_message_id: 696_301,
        threat: 1,
        _provenance: None,
        ..Default::default()
    }
}

#[test]
fn wire_encoders_cover_reward_and_claim_branches() {
    // 1. Neither ISK nor SP reward, plus career_id clamping (>4 -> 4, 0 -> 1)
    let mut def_none = sample_def(0, 0.0);
    def_none.career_id = 9;
    let mut buf_pub = Vec::new();
    encode_career_goal_definition_wire(&def_none, &mut buf_pub);
    assert!(!buf_pub.is_empty());

    let mut buf_int = Vec::new();
    encode_internal_career_goal_definition_wire(&def_none, &mut buf_int);
    assert!(!buf_int.is_empty());

    // 2. ISK only and SP only
    let def_isk = sample_def(50_000, 0.0);
    let mut b_isk = Vec::new();
    encode_career_goal_definition_wire(&def_isk, &mut b_isk);
    assert!(b_isk.len() > buf_pub.len());

    let def_sp = sample_def(0, 500.0);
    let mut b_sp = Vec::new();
    encode_internal_career_goal_definition_wire(&def_sp, &mut b_sp);
    assert!(b_sp.len() > buf_int.len());

    // 3. encode_career_goal_wire: completed+claimed with completed_time=None vs Some, and unclaimed with Some(ts)
    let g_claimed_no_ts = CharacterCareerGoal {
        uuid: [4u8; 16],
        def: def_isk,
        progress: 10,
        completed: true,
        claimed: true,
        completed_time: None,
    };
    let mut g_buf = Vec::new();
    encode_career_goal_wire(&g_claimed_no_ts, &mut g_buf);
    let wm = WireMessage::decode(&g_buf).unwrap();
    assert_eq!(wm.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()), Some(0));
    assert!(wm.fields.iter().all(|f| f.field_number != 4));

    // 4. Milestones: career_id = Some(99) (fallback message ID), Some(1) (CAREER_PATH_MESSAGE_IDS hit), None, sp_reward = 0.0 vs > 0.0
    let ms_unknown_career = CareerMilestoneDef {
        milestone_id: 5,
        career_id: Some(99),
        required_points: 0, // tests required_points.max(1)
        sp_reward: 0.0,
        description: "Unknown career milestone".into(),
    };
    let mut ms_pub = Vec::new();
    encode_career_milestone_definition_wire(&ms_unknown_career, &mut ms_pub);
    let mut ms_int = Vec::new();
    encode_internal_career_milestone_definition_wire(&ms_unknown_career, &mut ms_int);
    assert!(!ms_pub.is_empty() && !ms_int.is_empty());

    let ms_known_sp = CareerMilestoneDef {
        milestone_id: 1,
        career_id: Some(1),
        required_points: 500,
        sp_reward: 25_000.0,
        description: "Unknown career milestone".into(),
    };
    let mut ms_pub2 = Vec::new();
    encode_career_milestone_definition_wire(&ms_known_sp, &mut ms_pub2);
    let mut ms_int2 = Vec::new();
    encode_internal_career_milestone_definition_wire(&ms_known_sp, &mut ms_int2);
    assert!(ms_pub2.len() > ms_pub.len());
    assert!(ms_int2.len() > ms_int.len());

    let mut ms_goal = Vec::new();
    encode_career_milestone_goal_wire(&ms_known_sp, 999, true, false, &mut ms_goal);
    let wm_mg = WireMessage::decode(&ms_goal).unwrap();
    // progress clamped to required_points (500)
    assert_eq!(wm_mg.fields.iter().find(|f| f.field_number == 2).and_then(|f| f.value.as_varint()), Some(500));
    assert_eq!(wm_mg.fields.iter().find(|f| f.field_number == 3).and_then(|f| f.value.as_varint()), Some(0));
}

#[test]
fn extract_uuid_and_extract_target_character_all_branches_and_wire_fallbacks() {
    // extract_uuid edge cases: wrong byte len, wrong variant, missing
    let mut bad_len_sub = DynamicQuasarMessage::new("Identifier");
    bad_len_sub.fields.insert(1, DynamicValue::Bytes(vec![1u8; 8]));
    let mut wrong_type_sub = DynamicQuasarMessage::new("Identifier");
    wrong_type_sub.fields.insert(1, DynamicValue::Uint32(42));

    let mut msg = DynamicQuasarMessage::new("Req");
    msg.fields.insert(1, DynamicValue::Message(Box::new(bad_len_sub)));
    msg.fields.insert(2, DynamicValue::Message(Box::new(wrong_type_sub)));
    msg.fields.insert(3, DynamicValue::Uint32(1));
    assert_eq!(extract_uuid(&msg, 1), None);
    assert_eq!(extract_uuid(&msg, 2), None);
    assert_eq!(extract_uuid(&msg, 3), None);
    assert_eq!(extract_uuid(&msg, 99), None);

    // extract_target_character: Message with Uint32/Uint64/Int32/Int64/0/String
    let empty_any = GoogleAny::default();
    for (val, expected) in [
        (DynamicValue::Uint32(90_000_001), Some(90_000_001)),
        (DynamicValue::Uint64(90_000_002), Some(90_000_002)),
        (DynamicValue::Int32(90_000_003), Some(90_000_003)),
        (DynamicValue::Int64(90_000_004), Some(90_000_004)),
        (DynamicValue::Uint32(0), None),
        (DynamicValue::Int32(-5), None),
        (DynamicValue::String("bad".into()), None),
    ] {
        let mut sub = DynamicQuasarMessage::new("CharId");
        sub.fields.insert(1, val);
        let mut outer = DynamicQuasarMessage::new("Req");
        outer.fields.insert(1, DynamicValue::Message(Box::new(sub)));
        assert_eq!(extract_target_character(&outer, &empty_any, 1, false), expected);
    }

    // Message with missing field 1
    let mut outer_empty_sub = DynamicQuasarMessage::new("Req");
    outer_empty_sub.fields.insert(1, DynamicValue::Message(Box::new(DynamicQuasarMessage::new("CharId"))));
    assert_eq!(extract_target_character(&outer_empty_sub, &empty_any, 1, false), None);

    // Scalar Uint32 / Uint64 / Int32 / Int64 above and below 1_000_000
    for (val, allow, expected) in [
        (DynamicValue::Uint32(90_000_010), true, Some(90_000_010)),
        (DynamicValue::Uint32(999_999), true, None),
        (DynamicValue::Uint64(90_000_011), true, Some(90_000_011)),
        (DynamicValue::Int32(90_000_012), true, Some(90_000_012)),
        (DynamicValue::Int32(999_999), true, None),
        (DynamicValue::Int64(90_000_013), true, Some(90_000_013)),
        (DynamicValue::Int64(90_000_013), false, None),
        (DynamicValue::Bool(true), true, None),
    ] {
        let mut outer = DynamicQuasarMessage::new("Req");
        outer.fields.insert(2, val);
        assert_eq!(extract_target_character(&outer, &empty_any, 2, allow), expected);
    }

    // WireMessage fallback when dyn_msg does not contain field_num:
    // 1) Bytes submessage with field 1 varint > 0
    let mut inner_bytes = Vec::new();
    encode_varint_field(1, 90_000_021, &mut inner_bytes);
    let mut wire_sub = Vec::new();
    encode_bytes_field(4, &inner_bytes, &mut wire_sub);
    let any_sub = GoogleAny { type_url: String::new(), value: wire_sub };
    let empty_dyn = DynamicQuasarMessage::new("Req");
    assert_eq!(extract_target_character(&empty_dyn, &any_sub, 4, false), Some(90_000_021));

    // 2) Bytes submessage with field 1 varint == 0 -> None
    let mut inner_zero = Vec::new();
    encode_varint_field(1, 0, &mut inner_zero);
    let mut wire_zero = Vec::new();
    encode_bytes_field(4, &inner_zero, &mut wire_zero);
    let any_zero = GoogleAny { type_url: String::new(), value: wire_zero };
    assert_eq!(extract_target_character(&empty_dyn, &any_zero, 4, false), None);

    // 3) Scalar varint >= 1_000_000 vs < 1_000_000 and allow_scalar = false vs true
    let mut wire_scalar = Vec::new();
    encode_varint_field(5, 90_000_033, &mut wire_scalar);
    let any_scalar = GoogleAny { type_url: String::new(), value: wire_scalar };
    assert_eq!(extract_target_character(&empty_dyn, &any_scalar, 5, true), Some(90_000_033));
    assert_eq!(extract_target_character(&empty_dyn, &any_scalar, 5, false), None);

    let mut wire_small = Vec::new();
    encode_varint_field(5, 42, &mut wire_small);
    let any_small = GoogleAny { type_url: String::new(), value: wire_small };
    assert_eq!(extract_target_character(&empty_dyn, &any_small, 5, true), None);

    // 4) Malformed wire bytes -> None
    let any_bad = GoogleAny { type_url: String::new(), value: vec![0xff, 0xff, 0x0a, 0x09] };
    assert_eq!(extract_target_character(&empty_dyn, &any_bad, 1, true), None);
}
