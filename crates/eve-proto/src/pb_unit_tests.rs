use super::*;
use crate::quasar::{
    encode_bytes_field, encode_string_field, encode_varint_field, DynamicQuasarMessage,
    DynamicValue, GoogleDuration, GoogleTimestamp,
};

#[test]
fn pb_builder_sets_scalars_bytes_strings_and_nested_paths_byte_exact() {
    let uuid = [0xABu8; 16];
    let bytes = message("eve_public.career.goal.api.GetAllResponse.Goal")
        .set("goal.uuid", uuid)
        .set("progress", 7u32)
        .set("completed_time", GoogleTimestamp::from_unix(1_700_000_000, 123))
        .encode()
        .expect("encode GetAllResponse.Goal");

    let mut expected_goal = Vec::new();
    encode_bytes_field(1, &uuid, &mut expected_goal);
    let mut expected_ts = Vec::new();
    encode_varint_field(1, 1_700_000_000u64, &mut expected_ts);
    encode_varint_field(2, 123u64, &mut expected_ts);
    let mut expected = Vec::new();
    encode_bytes_field(1, &expected_goal, &mut expected);
    encode_varint_field(2, 7u64, &mut expected);
    encode_bytes_field(4, &expected_ts, &mut expected);

    assert_eq!(bytes, expected);
}

#[test]
fn pb_builder_oneof_eviction_keeps_only_last_set_arm() {
    // google.protobuf.Value has oneof kind: number_value (#2), string_value (#3), bool_value (#4)
    let reg = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let bytes = message_with_registry(Arc::clone(&reg), "google.protobuf.Value")
        .set("number_value", 42.5f64)
        .set("string_value", "winner")
        .encode()
        .expect("encode Value");

    let decoded = reg
        .decode_dynamic("google.protobuf.Value", &bytes)
        .expect("decode Value");
    assert_eq!(decoded.get(2), None);
    assert_eq!(
        decoded.get(3),
        Some(&DynamicValue::String("winner".to_string()))
    );

    let bytes_bool = message_with_registry(Arc::clone(&reg), "google.protobuf.Value")
        .set("string_value", "loser")
        .set("bool_value", true)
        .encode()
        .expect("encode bool Value");
    let decoded_bool = reg
        .decode_dynamic("google.protobuf.Value", &bytes_bool)
        .expect("decode bool Value");
    assert_eq!(decoded_bool.get(3), None);
    assert_eq!(decoded_bool.get(4), Some(&DynamicValue::Bool(true)));
}

#[test]
fn pb_builder_supports_set_with_add_with_repeated_and_enums_and_roundtrips() {
    let uuid = [0x11u8; 16];
    let bytes = message("eve_public.career.goal.api.GetAllResponse.Goal")
        .set_with("goal", |g| g.set("uuid", uuid))
        .set("progress", 3u32)
        .set("not_completed", false)
        .add_with("reward_label_states", |s| {
            s.set("label", 1i32)
                .set("claimed_time", GoogleTimestamp::from_unix(1_700_000_010, 0))
        })
        .add_with("reward_label_states", |s| {
            s.set("label", 2i32).set("not_claimed", true)
        })
        .encode()
        .expect("encode GetAllResponse.Goal with repeated reward_label_states");

    let reg = QuasarDescriptorRegistry::open_default().expect("open default descriptor DB");
    let decoded = reg
        .decode_dynamic("eve_public.career.goal.api.GetAllResponse.Goal", &bytes)
        .expect("decode GetAllResponse.Goal");

    let reencoded = reg
        .encode_dynamic(&decoded)
        .expect("reencode dynamic");
    assert_eq!(bytes, reencoded);

    // Check enum lookup by name as well as numeric enum value.
    let notice = message("eve_public.payment.schedule.api.CancelledNotice")
        .set("schedule.sequential", 99u64)
        .set("type", enum_name("TYPE_OMEGA"))
        .encode()
        .expect("CancelledNotice enum by name");
    let decoded_notice = reg
        .decode_dynamic("eve_public.payment.schedule.api.CancelledNotice", &notice)
        .expect("decode CancelledNotice");
    assert!(decoded_notice.get(2).is_some());
}

#[test]
fn pb_builder_to_any_and_to_dynamic_and_duration_and_type_url_strip() {
    let any = message("type.googleapis.com/eve_public.ui.Annotation")
        .set("key", "group")
        .set("value", "Explorer")
        .to_any()
        .expect("to_any");
    assert_eq!(
        any.type_url,
        "type.googleapis.com/eve_public.ui.Annotation"
    );

    let mut expected = Vec::new();
    encode_string_field(1, "group", &mut expected);
    encode_string_field(2, "Explorer", &mut expected);
    assert_eq!(any.value, expected);

    let reg = Arc::new(QuasarDescriptorRegistry::empty_with_builtins());
    let dur_dyn = message_with_registry(Arc::clone(&reg), "google.protobuf.Duration")
        .set("seconds", 12i64)
        .set("nanos", 345i32)
        .to_dynamic()
        .expect("to_dynamic Duration");
    assert_eq!(dur_dyn.get(1), Some(&DynamicValue::Int64(12)));
    assert_eq!(dur_dyn.get(2), Some(&DynamicValue::Int32(345)));

    let dyn_from_val: DynamicQuasarMessage =
        message_with_registry(Arc::clone(&reg), "google.protobuf.Duration")
            .set("seconds", 5i64)
            .to_dynamic()
            .unwrap();
    let wrapped = message_with_registry(reg, "google.protobuf.Duration")
        .set("seconds", 5i64)
        .encode()
        .unwrap();
    let wrapped_ts = GoogleDuration {
        seconds: 5,
        nanos: 0,
    };
    let _ = (dyn_from_val, wrapped, wrapped_ts);
}

#[test]
fn pb_builder_fails_with_valid_field_list_on_misspelled_field_and_path() {
    let err = message("eve_public.ui.Annotation")
        .set("keey", "oops")
        .encode()
        .expect_err("misspelled field must fail");

    match &err {
        PbError::UnknownField {
            message_type,
            field,
            valid_fields,
        } => {
            assert_eq!(message_type, "eve_public.ui.Annotation");
            assert_eq!(field, "keey");
            assert_eq!(
                valid_fields,
                &vec!["key".to_string(), "value".to_string()]
            );
        }
        other => panic!("expected UnknownField, got {other:?}"),
    }
    let rendered = err.to_string();
    assert!(
        rendered.contains("key") && rendered.contains("value"),
        "error message must list valid fields: {rendered}"
    );

    // Nested misspelled field also lists all valid fields of the nested message
    let nested_err = message("eve_public.career.goal.api.GetAllResponse.Goal")
        .set("goal.uuuid", [0u8; 16])
        .encode()
        .expect_err("misspelled nested field must fail");
    match &nested_err {
        PbError::UnknownField {
            message_type,
            field,
            valid_fields,
        } => {
            assert_eq!(message_type, "eve_public.career.goal.Identifier");
            assert_eq!(field, "uuuid");
            assert!(valid_fields.contains(&"uuid".to_string()));
        }
        other => panic!("expected UnknownField on nested message, got {other:?}"),
    }
    assert!(nested_err.to_string().contains("uuid"));
}

#[test]
fn pb_builder_rejects_unknown_message_and_wire_mismatches() {
    // Unknown message type
    let err = message("eve_public.nonexistent.DoesNotExist")
        .validate()
        .expect_err("unknown message type must fail");
    assert!(matches!(err, PbError::UnknownMessageType { .. }));

    // Empty/bad field path
    let err = message("eve_public.ui.Annotation")
        .set("", "x")
        .encode()
        .expect_err("empty field path must fail");
    assert!(matches!(err, PbError::InvalidFieldPath { .. }));

    // Traversing non-message field
    let err = message("eve_public.ui.Annotation")
        .set("key.sub", "x")
        .encode()
        .expect_err("non-message traversal must fail");
    assert!(matches!(err, PbError::NonMessageTraversal { .. }));

    // Traversing repeated field with dot path
    let err = message("eve_public.career.goal.api.GetAllResponse.Goal")
        .set("reward_label_states.not_claimed", true)
        .encode()
        .expect_err("repeated path traversal must fail");
    assert!(matches!(err, PbError::RepeatedPathSegment { .. }));

    // Calling .add() on singular field
    let err = message("eve_public.ui.Annotation")
        .add("key", "v")
        .encode()
        .expect_err(".add() on singular field must fail");
    assert!(matches!(err, PbError::SingularFieldForAdd { .. }));

    // Calling .set() with scalar on repeated field
    let err = message("eve_public.career.goal.api.GetAllResponse.Goal")
        .set("reward_label_states", true)
        .encode()
        .expect_err(".set() with non-list on repeated field must fail");
    assert!(matches!(err, PbError::RepeatedFieldNeedsAddOrList { .. }));

    // Type mismatch (bool where string expected, string where uint32 expected, wrong submessage type)
    let err = message("eve_public.ui.Annotation")
        .set("key", true)
        .encode()
        .expect_err("bool on string field must fail");
    assert!(matches!(err, PbError::TypeMismatch { .. }));

    let err = message("eve_public.career.goal.api.GetAllResponse.Goal")
        .set("progress", -1i32)
        .encode()
        .expect_err("negative value on uint32 must fail");
    assert!(matches!(err, PbError::TypeMismatch { .. }));

    let err = message("eve_public.career.goal.api.GetAllResponse.Goal")
        .set("goal", message("eve_public.ui.Annotation").set("key", "a"))
        .encode()
        .expect_err("wrong submessage type must fail");
    assert!(matches!(err, PbError::MessageTypeMismatch { .. }));

    // Unknown enum variant lists valid variants
    let err = message("eve_public.payment.schedule.api.CancelledNotice")
        .set("type", enum_name("BOGUS_VARIANT"))
        .encode()
        .expect_err("unknown enum variant must fail");
    match &err {
        PbError::UnknownEnumValue { valid_values, .. } => {
            assert!(!valid_values.is_empty());
        }
        other => panic!("expected UnknownEnumValue, got {other:?}"),
    }

    // Invalid raw submessage bytes rejected cleanly
    let err = message("eve_public.career.goal.api.GetAllResponse.Goal")
        .set_raw_message("goal", &[0x0A, 0xFF, 0x01])
        .encode()
        .expect_err("truncated length-delimited raw bytes must fail");
    assert!(matches!(err, PbError::InvalidRawSubmessage { .. }));
}

#[test]
fn raw_encode_field_source_counter_ignores_comments_and_strings() {
    let src = r##"
        // encode_varint_field(1, 0, &mut out);
        /* encode_bytes_field(2, &[], &mut out);
           /* encode_string_field(3, "x", &mut out); */
        */
        let s = "encode_fixed32_field(4, 0, &mut out)";
        let r = r#"encode_fixed64_field(5, 0, &mut out)"#;
        let ch = '"';
        let _fn_ref = encode_varint_field;
        encode_varint_field(1, 1, &mut out);
        encode_bytes_field(2, &b, &mut out);
        encode_string_field (3, "ok", &mut out);
    "##;
    assert_eq!(count_raw_encode_field_calls_in_source(src), 3);
}

#[test]
fn raw_encode_field_ratchet_enforced_on_eve_server_src() {
    let ratchet = RawEncodeFieldRatchet::load_default()
        .expect("load crates/eve-proto/ratchets/raw_encode_field_ratchet.json");
    assert!(
        ratchet.max_raw_encode_field_calls < 3_694,
        "ratchet must stay strictly tightened below initial 3,694 baseline"
    );
    let report = ratchet
        .verify_default()
        .expect("eve-server/src must remain at or below raw_encode_field ratchet");
    assert!(
        report.raw_encode_field_calls <= ratchet.max_raw_encode_field_calls,
        "report {report} exceeded {}",
        ratchet.max_raw_encode_field_calls
    );
}
