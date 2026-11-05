//! Unit and mutation-killing tests for `eve_proto::quasar_wire`.

use super::*;
use crate::ProtoError;

#[test]
fn varint_roundtrips_boundary_values_and_rejects_eof_or_overflow() {
    for val in [
        0u64,
        1,
        127,
        128,
        255,
        16_383,
        16_384,
        u32::MAX as u64,
        (1u64 << 63) - 1,
        u64::MAX,
    ] {
        let mut buf = Vec::new();
        write_varint(val, &mut buf);
        let mut slice = buf.as_slice();
        assert_eq!(read_varint(&mut slice).unwrap(), val);
        assert!(slice.is_empty());
    }
    // 127 is 1 byte, 128 is 2 bytes
    let mut b127 = Vec::new();
    write_varint(127, &mut b127);
    assert_eq!(b127, vec![0x7F]);
    let mut b128 = Vec::new();
    write_varint(128, &mut b128);
    assert_eq!(b128, vec![0x80, 0x01]);

    // Empty and truncated varints
    let mut empty: &[u8] = &[];
    assert_eq!(read_varint(&mut empty), Err(ProtoError::UnexpectedEof));
    let mut trunc: &[u8] = &[0x80, 0x81];
    assert_eq!(read_varint(&mut trunc), Err(ProtoError::UnexpectedEof));
    // Shift >= 64 (10 continuation bytes)
    let over: [u8; 11] = [0x80; 11];
    let mut over_slice: &[u8] = &over;
    assert_eq!(read_varint(&mut over_slice), Err(ProtoError::UnexpectedEof));
}

#[test]
fn encode_and_decode_fields_cover_all_wire_types_and_error_cases() {
    let fields = vec![
        (1, ProtoWireValue::Varint(123_456)),
        (2, ProtoWireValue::Fixed64(0x0102_0304_0506_0708)),
        (3, ProtoWireValue::Bytes(vec![0xAA, 0xBB, 0xCC])),
        (4, ProtoWireValue::Fixed32(0x1122_3344)),
    ];
    let encoded = encode_fields(&fields);
    let decoded = decode_fields(&encoded).unwrap();
    assert_eq!(decoded, fields);

    // Field number 0 is rejected
    assert_eq!(
        decode_fields(&[0x00, 0x01]),
        Err(ProtoError::UnsupportedOpcode(0))
    );
    assert_eq!(
        decode_fields(&[0x05, 0, 0, 0, 0]),
        Err(ProtoError::UnsupportedOpcode(5))
    );

    // Unsupported wire types 3, 4, 6, 7
    for wt in [3u8, 4, 6, 7] {
        assert_eq!(
            decode_fields(&[(1 << 3) | wt]),
            Err(ProtoError::UnsupportedOpcode(wt))
        );
    }

    // Truncated Fixed64 (< 8 bytes, e.g. 7 bytes)
    assert_eq!(
        decode_fields(&[(1 << 3) | 1, 1, 2, 3, 4, 5, 6, 7]),
        Err(ProtoError::UnexpectedEof)
    );
    // Exact 8 bytes succeeds
    assert_eq!(
        decode_fields(&[(1 << 3) | 1, 1, 0, 0, 0, 0, 0, 0, 0]).unwrap(),
        vec![(1, ProtoWireValue::Fixed64(1))]
    );

    // Truncated Bytes (claims 4 bytes, only 3 present)
    assert_eq!(
        decode_fields(&[(1 << 3) | 2, 4, 0xAA, 0xBB, 0xCC]),
        Err(ProtoError::UnexpectedEof)
    );
    // Bytes length > MAX_FRAME_SIZE
    let mut huge_len = vec![(1 << 3) | 2];
    write_varint((MAX_FRAME_SIZE + 1) as u64, &mut huge_len);
    assert_eq!(decode_fields(&huge_len), Err(ProtoError::UnexpectedEof));

    // Truncated Fixed32 (< 4 bytes, e.g. 3 bytes)
    assert_eq!(
        decode_fields(&[(1 << 3) | 5, 1, 2, 3]),
        Err(ProtoError::UnexpectedEof)
    );
}

#[test]
fn gateway_envelope_roundtrip_and_invalid_utf8_rejection() {
    let env = QuasarGatewayEnvelope {
        type_url: "type.googleapis.com/eve_public.ping.Request".into(),
        correlation_id: 987_654_321,
        status_code: 200,
        payload: vec![0x08, 0x01],
    };
    let mut bytes = encode_envelope(&env);
    // Append unknown field 99 to ensure decode_envelope ignores unknown tags cleanly
    bytes.extend_from_slice(&encode_fields(&[(99, ProtoWireValue::Fixed32(7))]));
    let decoded = decode_envelope(&bytes).unwrap();
    assert_eq!(decoded, env);

    // Invalid UTF-8 in field 1 (type_url)
    let bad_utf8 = encode_fields(&[(1, ProtoWireValue::Bytes(vec![0xFF, 0xFE]))]);
    assert!(matches!(decode_envelope(&bad_utf8), Err(ProtoError::Utf8(_))));
}

#[test]
fn pyrep_accessors_node_count_bridge_envelope_and_gateway_frame_roundtrip() {
    use crate::{
        validate_frame_length, BridgeEnvelope, BridgeRpcError, DBRowDescriptor, GatewayFrame,
        GatewayFrameKind, PyRep, MAX_FRAME_SIZE,
    };
    use std::collections::HashMap;

    // validate_frame_length boundaries
    assert_eq!(validate_frame_length(0u32.to_le_bytes()), Err(ProtoError::UnexpectedEof));
    assert_eq!(validate_frame_length(1u32.to_le_bytes()), Ok(1));
    assert_eq!(
        validate_frame_length((MAX_FRAME_SIZE as u32).to_le_bytes()),
        Ok(MAX_FRAME_SIZE)
    );
    assert_eq!(
        validate_frame_length(((MAX_FRAME_SIZE + 1) as u32).to_le_bytes()),
        Err(ProtoError::FrameTooLarge(MAX_FRAME_SIZE + 1))
    );

    // PyRep accessors, row_count, dict_get, node_count
    assert_eq!(PyRep::Bool(true).as_i64(), Some(1));
    assert_eq!(PyRep::Bool(false).as_i64(), Some(0));
    assert_eq!(PyRep::Integer(7).as_f64(), Some(7.0));
    assert_eq!(PyRep::Integer(0).as_bool(), Some(false));
    assert_eq!(PyRep::Integer(2).as_bool(), Some(true));
    assert_eq!(PyRep::Bytes(b"hi".to_vec()).as_str(), Some("hi"));

    let desc = DBRowDescriptor::market_summary_header();
    let row = vec![
        PyRep::Real(10.0),
        PyRep::Real(5.0),
        PyRep::Integer(34),
        PyRep::Integer(60_003_760),
    ];
    let dbrow = desc.build_dbrow(row.clone());
    assert_eq!(dbrow.dbrow_columns().unwrap().len(), 4);
    assert_eq!(dbrow.dict_get("typeID"), Some(&PyRep::Integer(34)));
    assert!(dbrow.node_count() >= 5);

    let crow = PyRep::crowset(&desc, vec![row.clone()]);
    assert_eq!(crow.row_count(), Some(1));
    assert_eq!(crow.dict_get("typeID"), Some(&PyRep::Integer(34)));

    let cidx = PyRep::cindexed_rowset(&desc, "typeID", vec![row.clone()]);
    assert_eq!(cidx.row_count(), Some(1));
    assert_eq!(cidx.cindexed_rowset_entries().unwrap().len(), 1);

    let util_row = PyRep::Object {
        type_name: "util.Row".into(),
        args: Box::new(PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::String("col1".into())]),
            PyRep::List(vec![PyRep::Integer(99)]),
        ])),
    };
    assert_eq!(util_row.dict_get("col1"), Some(&PyRep::Integer(99)));

    // BridgeEnvelope JSON + MessagePack roundtrips
    let env = BridgeEnvelope::RpcResponse {
        req_id: 1,
        result: Err(BridgeRpcError::user_error("CustomError")),
    };
    let wire = env.to_wire_bytes().unwrap();
    assert_eq!(BridgeEnvelope::from_wire_bytes(&wire).unwrap(), env);
    let msgpack = rmp_serde::to_vec(&env).unwrap();
    assert_eq!(BridgeEnvelope::from_wire_bytes(&msgpack).unwrap(), env);

    let ev = BridgeEnvelope::ServerEvent {
        tick: 10,
        event: "OnItemChange".into(),
        args: vec![PyRep::Integer(1)],
    };
    assert_eq!(
        BridgeEnvelope::from_wire_bytes(&ev.to_wire_bytes().unwrap()).unwrap(),
        ev
    );
    let req = BridgeEnvelope::RpcRequest {
        req_id: 2,
        service: "machoNet".into(),
        bound_id: None,
        method: "GetTime".into(),
        args: vec![],
        kwargs: HashMap::new(),
        caller_trace: None,
    };
    assert_eq!(
        BridgeEnvelope::from_wire_bytes(&req.to_wire_bytes().unwrap()).unwrap(),
        req
    );

    // GatewayFrame protobuf wire and PyRep roundtrip
    let gf = GatewayFrame {
        kind: GatewayFrameKind::Notice,
        correlation_id: 777,
        type_name: "eve_public.Notice".into(),
        status_code: 200,
        actor_character_id: Some(-90_000_001),
        idempotency_key: Some("idem-1".into()),
        payload: vec![1, 2, 3],
    };
    let gf_wire = gf.encode_protobuf_wire();
    assert_eq!(GatewayFrame::decode_protobuf_wire(&gf_wire).unwrap(), gf);
    let gf_rep = gf.to_pyrep();
    assert_eq!(GatewayFrame::from_pyrep(&gf_rep).unwrap(), gf);
}

