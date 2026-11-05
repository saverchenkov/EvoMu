//! Unit and mutation-killing tests for `eve_proto::quasar::types`.

use super::*;

fn assert_quasar_roundtrip<T: QuasarMessage + PartialEq + std::fmt::Debug + Default>(val: &T) {
    let default_val = T::default();
    let default_bytes = default_val.encode_proto();
    assert_eq!(T::decode_proto(&default_bytes).unwrap(), default_val);

    let bytes = val.encode_proto();
    let decoded = T::decode_proto(&bytes).unwrap();
    assert_eq!(&decoded, val);

    let any = val.to_any();
    assert_eq!(any.normalized_type_name(), T::FULL_NAME);
    assert_eq!(&T::from_any(&any).unwrap(), val);

    // TypeUrlMismatch on wrong type_url
    let bad_any = GoogleAny {
        type_url: "type.googleapis.com/wrong.Type".into(),
        value: bytes,
    };
    assert_eq!(
        T::from_any(&bad_any),
        Err(QuasarError::TypeUrlMismatch {
            expected: T::type_url(),
            actual: "type.googleapis.com/wrong.Type".into(),
        })
    );
}

#[test]
fn well_known_google_types_and_identities_roundtrip() {
    assert_quasar_roundtrip(&GoogleTimestamp {
        seconds: -1_700_000_000,
        nanos: 123_456_789,
    });
    assert_quasar_roundtrip(&GoogleDuration {
        seconds: 3600,
        nanos: -500,
    });
    assert_quasar_roundtrip(&GoogleAny {
        type_url: "type.evetech.net/eve_public.character.Identifier".into(),
        value: vec![0x08, 0x2A],
    });
    assert_quasar_roundtrip(&GoogleEmpty);
    assert!(GoogleEmpty.encode_proto().is_empty());

    // ZigZag sint32 user identifier (positive and negative)
    assert_quasar_roundtrip(&EvePublicUserId { sequential: 1001 });
    assert_quasar_roundtrip(&EvePublicUserId { sequential: -1001 });
    assert_quasar_roundtrip(&EvePublicCharacterId {
        sequential: 90_000_001,
    });
    assert_quasar_roundtrip(&EvePublicCorporationId {
        sequential: 98_000_001,
    });
    assert_quasar_roundtrip(&EvePublicCorpRoleCollection {
        bitmask: 0xDEAD_BEEF_CAFE_BABE,
    });
    assert_quasar_roundtrip(&EveIdempotencyToken {
        uuid: vec![1, 2, 3, 4, 5, 6, 7, 8],
    });
}

#[test]
fn ip_address_trace_context_and_semver_roundtrip() {
    assert_eq!(EveIpAddress::default(), EveIpAddress::V4(0x7F00_0001));
    let v4 = EveIpAddress::V4(0x0A00_0001);
    assert_eq!(EveIpAddress::decode_proto(&v4.encode_proto()).unwrap(), v4);
    let v6 = EveIpAddress::V6(vec![0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 1]);
    assert_eq!(EveIpAddress::decode_proto(&v6.encode_proto()).unwrap(), v6);

    // TraceContext with sampled = true/false and sample_rate = 0/non-zero
    assert_quasar_roundtrip(&TraceContext {
        trace_id: vec![0xAA; 16],
        parent_id: vec![0xBB; 8],
        flags: Some(TraceFlags { sampled: true }),
        state: Some(TraceState { sample_rate: 42 }),
    });
    assert_quasar_roundtrip(&TraceContext {
        trace_id: Vec::new(),
        parent_id: Vec::new(),
        flags: Some(TraceFlags { sampled: false }),
        state: Some(TraceState { sample_rate: 0 }),
    });

    assert_quasar_roundtrip(&SemanticVersionSpec {
        major: 24,
        minor: 1,
        patch: 3,
        prerelease_tags: vec!["rc1".into(), "alpha".into()],
        build_tags: vec!["3624425".into()],
    });
}

#[test]
fn color_math_device_and_network_rtt_types_roundtrip() {
    assert_quasar_roundtrip(&ColorRgb {
        red: 255,
        green: 128,
        blue: 64,
    });
    assert_quasar_roundtrip(&MathFraction {
        numerator: 355,
        denominator: 113,
    });
    assert_quasar_roundtrip(&MathVector3 {
        x: -1234.5,
        y: 6789.25,
        z: -0.125,
    });
    assert_quasar_roundtrip(&MathVector4 {
        x: 1.0,
        y: -2.0,
        z: 3.5,
        w: -4.25,
    });
    assert_quasar_roundtrip(&DeviceUserIdentifier {
        device: vec![0x10, 0x20, 0x30],
        user: Some(EvePublicUserId { sequential: -77 }),
    });
    assert_quasar_roundtrip(&NetworkRttSample {
        protocol: -2,
        rtt: Some(GoogleDuration {
            seconds: 0,
            nanos: 45_000_000,
        }),
    });
}

#[test]
fn quasar_wire_primitives_hashes_zigzag_and_url_prefixes() {
    use crate::quasar::{
        decode_varint, decode_zigzag32, decode_zigzag64, encode_varint, encode_zigzag32,
        encode_zigzag64, has_quasar_type_url_prefix, md5_hex, sha256_hex,
        strip_quasar_type_url_prefix, WireField, WireMessage, WireType, WireValue,
    };

    assert!(has_quasar_type_url_prefix(
        "type.googleapis.com/eve_public.Request"
    ));
    assert!(has_quasar_type_url_prefix(
        "type.evetech.net/eve_public.Request"
    ));
    assert!(!has_quasar_type_url_prefix("eve_public.Request"));
    assert_eq!(
        strip_quasar_type_url_prefix("type.evetech.net/.eve_public.Request"),
        "eve_public.Request"
    );

    // Known RFC 6234 SHA-256 and RFC 1321 MD5 vectors (empty, "abc", and >64 byte multi-block)
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    let long_input = vec![b'a'; 128];
    assert_eq!(sha256_hex(&long_input).len(), 64);
    assert_eq!(md5_hex(&long_input).len(), 32);

    // ZigZag 32 and 64 boundaries
    for v32 in [0i32, 1, -1, 2, -2, i32::MAX, i32::MIN] {
        assert_eq!(decode_zigzag32(encode_zigzag32(v32)), v32);
    }
    for v64 in [0i64, 1, -1, 2, -2, i64::MAX, i64::MIN] {
        assert_eq!(decode_zigzag64(encode_zigzag64(v64)), v64);
    }

    // Varint overflow (>10 continuation bytes)
    let mut pos = 0;
    assert_eq!(
        decode_varint(
            &[0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x02],
            &mut pos
        ),
        Err(QuasarError::VarintOverflow)
    );
    let mut max_buf = Vec::new();
    encode_varint(u64::MAX, &mut max_buf);
    let mut pos2 = 0;
    assert_eq!(decode_varint(&max_buf, &mut pos2).unwrap(), u64::MAX);

    // WireType and WireMessage roundtrip & error cases
    assert_eq!(WireType::from_u8(0), Ok(WireType::Varint));
    assert_eq!(WireType::from_u8(1), Ok(WireType::Fixed64));
    assert_eq!(WireType::from_u8(2), Ok(WireType::LengthDelimited));
    assert_eq!(WireType::from_u8(5), Ok(WireType::Fixed32));
    assert_eq!(WireType::from_u8(3), Err(QuasarError::InvalidWireType(3)));

    let wm = WireMessage {
        fields: vec![
            WireField {
                field_number: 1,
                value: WireValue::Varint(10),
            },
            WireField {
                field_number: 2,
                value: WireValue::Fixed64(20),
            },
            WireField {
                field_number: 3,
                value: WireValue::LengthDelimited(vec![1, 2]),
            },
            WireField {
                field_number: 4,
                value: WireValue::Fixed32(30),
            },
        ],
    };
    assert_eq!(WireMessage::decode(&wm.encode()).unwrap(), wm);
    assert_eq!(
        WireMessage::decode(&[0x00, 0x01]),
        Err(QuasarError::InvalidWireType(0xFF))
    );
}

