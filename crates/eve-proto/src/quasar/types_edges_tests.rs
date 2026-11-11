use super::*;

#[test]
fn any_type_url_normalization_mismatch_and_corrupt_value() {
    let ts = GoogleTimestamp {
        seconds: 1_700_000_000,
        nanos: 123_456_789,
    };
    let raw = ts.encode_proto();

    for url in [
        "type.googleapis.com/google.protobuf.Timestamp",
        "type.evetech.net/google.protobuf.Timestamp",
        ".google.protobuf.Timestamp",
        "google.protobuf.Timestamp",
    ] {
        let any = GoogleAny {
            type_url: url.to_string(),
            value: raw.clone(),
        };
        assert_eq!(any.normalized_type_name(), "google.protobuf.Timestamp");
        assert_eq!(GoogleTimestamp::from_any(&any).expect("from_any"), ts);
    }

    let ts_any = ts.to_any();
    assert!(matches!(
        EvePublicCharacterId::from_any(&ts_any),
        Err(QuasarError::TypeUrlMismatch { .. })
    ));

    let corrupt_any = GoogleAny {
        type_url: "type.googleapis.com/google.protobuf.Timestamp".to_string(),
        value: vec![0xFF, 0xFF, 0xFF],
    };
    assert!(GoogleTimestamp::from_any(&corrupt_any).is_err());
}

#[test]
fn scalar_and_wrapper_messages_decode_edges_and_wrong_wire_types() {
    // GoogleTimestamp & GoogleDuration: empty, zero omitted, wrong wire types, truncated.
    assert!(GoogleTimestamp { seconds: 0, nanos: 0 }.encode_proto().is_empty());
    assert!(GoogleTimestamp::decode_proto(&[0xFF]).is_err());
    let mut bad_ts = Vec::new();
    encode_bytes_field(1, b"str", &mut bad_ts);
    encode_bytes_field(2, b"str", &mut bad_ts);
    encode_varint_field(99, 1, &mut bad_ts);
    assert_eq!(
        GoogleTimestamp::decode_proto(&bad_ts).expect("ignore wrong wire types"),
        GoogleTimestamp::default()
    );

    assert!(GoogleDuration { seconds: 0, nanos: 0 }.encode_proto().is_empty());
    let neg_dur = GoogleDuration { seconds: -45, nanos: -500 };
    assert_eq!(GoogleDuration::decode_proto(&neg_dur.encode_proto()).unwrap(), neg_dur);
    assert!(GoogleDuration::decode_proto(&[0x08, 0x80]).is_err());
    assert_eq!(
        GoogleDuration::decode_proto(&bad_ts).expect("ignore wrong wire types"),
        GoogleDuration::default()
    );

    // GoogleAny: empty, invalid UTF-8 on type_url, wrong wire types, truncated.
    assert!(GoogleAny::default().encode_proto().is_empty());
    let mut bad_utf8_any = Vec::new();
    encode_bytes_field(1, &[0xFF, 0xFE], &mut bad_utf8_any);
    assert!(matches!(
        GoogleAny::decode_proto(&bad_utf8_any),
        Err(QuasarError::InvalidUtf8(_))
    ));
    let mut wrong_any = Vec::new();
    encode_varint_field(1, 5, &mut wrong_any);
    encode_varint_field(2, 6, &mut wrong_any);
    encode_varint_field(99, 7, &mut wrong_any);
    assert_eq!(GoogleAny::decode_proto(&wrong_any).unwrap(), GoogleAny::default());
    assert!(GoogleAny::decode_proto(&[0x0A, 0x05, 0x01]).is_err());

    // GoogleEmpty: unknown fields ok, truncated fails.
    assert!(GoogleEmpty.encode_proto().is_empty());
    assert_eq!(GoogleEmpty::decode_proto(&wrong_any).unwrap(), GoogleEmpty);
    assert!(GoogleEmpty::decode_proto(&[0xFF]).is_err());

    // Identifiers & collections: zero omitted, wrong wire types, truncated.
    let mut wrong_f1 = Vec::new();
    encode_bytes_field(1, b"bad", &mut wrong_f1);
    encode_varint_field(99, 1, &mut wrong_f1);

    assert!(EvePublicUserId { sequential: 0 }.encode_proto().is_empty());
    assert_eq!(
        EvePublicUserId::decode_proto(&EvePublicUserId { sequential: -77 }.encode_proto()).unwrap(),
        EvePublicUserId { sequential: -77 }
    );
    assert_eq!(EvePublicUserId::decode_proto(&wrong_f1).unwrap(), EvePublicUserId::default());
    assert!(EvePublicUserId::decode_proto(&[0x08]).is_err());

    assert!(EvePublicCharacterId { sequential: 0 }.encode_proto().is_empty());
    assert_eq!(EvePublicCharacterId::decode_proto(&wrong_f1).unwrap(), EvePublicCharacterId::default());
    assert!(EvePublicCharacterId::decode_proto(&[0x08]).is_err());

    assert!(EvePublicCorporationId { sequential: 0 }.encode_proto().is_empty());
    assert_eq!(EvePublicCorporationId::decode_proto(&wrong_f1).unwrap(), EvePublicCorporationId::default());
    assert!(EvePublicCorporationId::decode_proto(&[0x08]).is_err());

    assert!(EvePublicCorpRoleCollection { bitmask: 0 }.encode_proto().is_empty());
    assert_eq!(
        EvePublicCorpRoleCollection::decode_proto(&wrong_f1).unwrap(),
        EvePublicCorpRoleCollection::default()
    );
    assert!(EvePublicCorpRoleCollection::decode_proto(&[0x08]).is_err());

    assert!(EveIdempotencyToken { uuid: Vec::new() }.encode_proto().is_empty());
    assert_eq!(
        EveIdempotencyToken::decode_proto(&wrong_any).unwrap(),
        EveIdempotencyToken::default()
    );
    assert!(EveIdempotencyToken::decode_proto(&[0x0A, 0x03]).is_err());

    // EveIpAddress: empty decodes to V4(0), V4(0) encodes non-empty or empty roundtrips, wrong wire types ignored.
    assert_eq!(EveIpAddress::decode_proto(&[]).unwrap(), EveIpAddress::V4(0));
    let v6 = EveIpAddress::V6(vec![0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
    assert_eq!(EveIpAddress::decode_proto(&v6.encode_proto()).unwrap(), v6);
    assert_eq!(
        EveIpAddress::decode_proto(&EveIpAddress::V4(0x0A00_0001).encode_proto()).unwrap(),
        EveIpAddress::V4(0x0A00_0001)
    );
    assert_eq!(EveIpAddress::decode_proto(&wrong_any).unwrap(), EveIpAddress::V4(0));
    assert!(EveIpAddress::decode_proto(&[0x12, 0x10]).is_err());
}

#[test]
fn trace_semver_math_and_compound_messages_edges() {
    // TraceContext nested defaults, corrupt inner flags/state, inner wrong wire types.
    let tc_zero_nested = TraceContext {
        trace_id: vec![1, 2],
        parent_id: vec![3, 4],
        flags: Some(TraceFlags { sampled: false }),
        state: Some(TraceState { sample_rate: 0 }),
    };
    assert_eq!(
        TraceContext::decode_proto(&tc_zero_nested.encode_proto()).unwrap(),
        tc_zero_nested
    );
    let mut inner_wrong = Vec::new();
    encode_bytes_field(1, b"bad", &mut inner_wrong);
    encode_varint_field(99, 1, &mut inner_wrong);
    let mut outer_tc_inner_wrong = Vec::new();
    encode_bytes_field(3, &inner_wrong, &mut outer_tc_inner_wrong);
    encode_bytes_field(4, &inner_wrong, &mut outer_tc_inner_wrong);
    encode_varint_field(1, 1, &mut outer_tc_inner_wrong);
    encode_varint_field(2, 2, &mut outer_tc_inner_wrong);
    encode_varint_field(99, 3, &mut outer_tc_inner_wrong);
    let dec_tc = TraceContext::decode_proto(&outer_tc_inner_wrong).unwrap();
    assert_eq!(dec_tc.flags, Some(TraceFlags { sampled: false }));
    assert_eq!(dec_tc.state, Some(TraceState { sample_rate: 0 }));

    let mut corrupt_flags = Vec::new();
    encode_bytes_field(3, &[0x08], &mut corrupt_flags);
    assert!(TraceContext::decode_proto(&corrupt_flags).is_err());
    let mut corrupt_state = Vec::new();
    encode_bytes_field(4, &[0x08], &mut corrupt_state);
    assert!(TraceContext::decode_proto(&corrupt_state).is_err());
    assert!(TraceContext::decode_proto(&[0x0A, 0x05]).is_err());

    // SemanticVersionSpec: zero omitted, invalid UTF-8 on fields 4 and 5, wrong wire types.
    assert!(SemanticVersionSpec::default().encode_proto().is_empty());
    let mut bad_pre = Vec::new();
    encode_bytes_field(4, &[0xFF], &mut bad_pre);
    assert!(matches!(
        SemanticVersionSpec::decode_proto(&bad_pre),
        Err(QuasarError::InvalidUtf8(_))
    ));
    let mut bad_build = Vec::new();
    encode_bytes_field(5, &[0xFF], &mut bad_build);
    assert!(matches!(
        SemanticVersionSpec::decode_proto(&bad_build),
        Err(QuasarError::InvalidUtf8(_))
    ));
    let mut wrong_sv = Vec::new();
    encode_bytes_field(1, b"x", &mut wrong_sv);
    encode_bytes_field(2, b"x", &mut wrong_sv);
    encode_bytes_field(3, b"x", &mut wrong_sv);
    encode_varint_field(4, 1, &mut wrong_sv);
    encode_varint_field(5, 1, &mut wrong_sv);
    encode_varint_field(99, 1, &mut wrong_sv);
    assert_eq!(
        SemanticVersionSpec::decode_proto(&wrong_sv).unwrap(),
        SemanticVersionSpec::default()
    );
    assert!(SemanticVersionSpec::decode_proto(&[0x08]).is_err());

    // ColorRgb & MathFraction.
    assert!(ColorRgb::default().encode_proto().is_empty());
    assert_eq!(ColorRgb::decode_proto(&wrong_sv).unwrap(), ColorRgb::default());
    assert!(ColorRgb::decode_proto(&[0x08]).is_err());

    assert!(MathFraction::default().encode_proto().is_empty());
    let frac = MathFraction { numerator: 3, denominator: 4 };
    assert_eq!(MathFraction::decode_proto(&frac.encode_proto()).unwrap(), frac);
    assert_eq!(MathFraction::decode_proto(&wrong_sv).unwrap(), MathFraction::default());
    assert!(MathFraction::decode_proto(&[0x08]).is_err());

    // MathVector3 & MathVector4: +0.0 omitted, -0.0 preserved via to_bits(), wrong wire types, truncated.
    assert!(MathVector3::default().encode_proto().is_empty());
    let neg_zero_v3 = MathVector3 { x: -0.0, y: 1.5, z: -0.0 };
    let dec_v3 = MathVector3::decode_proto(&neg_zero_v3.encode_proto()).unwrap();
    assert_eq!(dec_v3.x.to_bits(), (-0.0_f64).to_bits());
    assert_eq!(dec_v3.z.to_bits(), (-0.0_f64).to_bits());
    assert_eq!(MathVector3::decode_proto(&wrong_sv).unwrap(), MathVector3::default());
    assert!(MathVector3::decode_proto(&[0x09, 0x01]).is_err());

    assert!(MathVector4::default().encode_proto().is_empty());
    let neg_zero_v4 = MathVector4 { x: -0.0, y: -0.0, z: 2.0, w: -0.0 };
    let dec_v4 = MathVector4::decode_proto(&neg_zero_v4.encode_proto()).unwrap();
    assert_eq!(dec_v4.w.to_bits(), (-0.0_f64).to_bits());
    assert_eq!(MathVector4::decode_proto(&wrong_sv).unwrap(), MathVector4::default());
    assert!(MathVector4::decode_proto(&[0x09, 0x01]).is_err());

    // DeviceUserIdentifier: empty, populated, corrupt nested user, wrong wire types, truncated.
    assert!(DeviceUserIdentifier::default().encode_proto().is_empty());
    let dui = DeviceUserIdentifier {
        device: vec![9, 8, 7],
        user: Some(EvePublicUserId { sequential: 42 }),
    };
    assert_eq!(DeviceUserIdentifier::decode_proto(&dui.encode_proto()).unwrap(), dui);
    let mut corrupt_dui = Vec::new();
    encode_bytes_field(2, &[0x08], &mut corrupt_dui);
    assert!(DeviceUserIdentifier::decode_proto(&corrupt_dui).is_err());
    let mut wrong_dui = Vec::new();
    encode_varint_field(1, 1, &mut wrong_dui);
    encode_varint_field(2, 1, &mut wrong_dui);
    encode_varint_field(99, 1, &mut wrong_dui);
    assert_eq!(DeviceUserIdentifier::decode_proto(&wrong_dui).unwrap(), DeviceUserIdentifier::default());
    assert!(DeviceUserIdentifier::decode_proto(&[0x0A, 0x05]).is_err());

    // NetworkRttSample: empty, populated, corrupt nested rtt, wrong wire types, truncated.
    assert!(NetworkRttSample::default().encode_proto().is_empty());
    let rtt = NetworkRttSample {
        protocol: 2,
        rtt: Some(GoogleDuration { seconds: 0, nanos: 25_000_000 }),
    };
    assert_eq!(NetworkRttSample::decode_proto(&rtt.encode_proto()).unwrap(), rtt);
    let mut corrupt_rtt = Vec::new();
    encode_bytes_field(2, &[0x08], &mut corrupt_rtt);
    assert!(NetworkRttSample::decode_proto(&corrupt_rtt).is_err());
    let mut wrong_rtt = Vec::new();
    encode_bytes_field(1, b"bad", &mut wrong_rtt);
    encode_varint_field(2, 1, &mut wrong_rtt);
    encode_varint_field(99, 1, &mut wrong_rtt);
    assert_eq!(NetworkRttSample::decode_proto(&wrong_rtt).unwrap(), NetworkRttSample::default());
    assert!(NetworkRttSample::decode_proto(&[0x08]).is_err());
}
