//! Additional unit and mutation-killing tests for `eve-proto` (`lib.rs`).

#![forbid(unsafe_code)]

use super::*;

#[test]
fn test_pyrep_accessors_scalars_and_row_containers() {
    // 1. as_str, as_i64, as_f64, as_bool
    assert_eq!(PyRep::String("alpha".into()).as_str(), Some("alpha"));
    assert_eq!(PyRep::Token("util.Row".into()).as_str(), Some("util.Row"));
    assert_eq!(PyRep::Bytes(b"ascii".to_vec()).as_str(), Some("ascii"));
    assert_eq!(PyRep::Bytes(vec![0xFF, 0xFE]).as_str(), None);
    assert_eq!(PyRep::Integer(10).as_str(), None);

    assert_eq!(PyRep::Integer(-42).as_i64(), Some(-42));
    assert_eq!(PyRep::Bool(true).as_i64(), Some(1));
    assert_eq!(PyRep::Bool(false).as_i64(), Some(0));
    assert_eq!(PyRep::Real(1.5).as_i64(), None);

    assert_eq!(PyRep::Real(3.25).as_f64(), Some(3.25));
    assert_eq!(PyRep::Integer(42).as_f64(), Some(42.0));
    assert_eq!(PyRep::Bool(true).as_f64(), None);

    assert_eq!(PyRep::Bool(true).as_bool(), Some(true));
    assert_eq!(PyRep::Bool(false).as_bool(), Some(false));
    assert_eq!(PyRep::Integer(0).as_bool(), Some(false));
    assert_eq!(PyRep::Integer(1).as_bool(), Some(true));
    assert_eq!(PyRep::Integer(-99).as_bool(), Some(true));
    assert_eq!(PyRep::None.as_bool(), None);

    // 2. CRowset, CIndexedRowset, Rowset, IndexRowset, TupleSet, List/Set row_count & accessors
    let desc = DBRowDescriptor::new([("id", DbColumnType::I4), ("name", DbColumnType::WStr)]);
    let rows = vec![
        vec![PyRep::Integer(1), PyRep::String("One".into())],
        vec![PyRep::Integer(2), PyRep::String("Two".into())],
    ];

    let crow = PyRep::crowset(&desc, rows.clone());
    assert!(crow.is_crowset());
    assert!(!crow.is_cindexed_rowset());
    assert_eq!(crow.row_count(), Some(2));
    assert_eq!(crow.crowset_rows().map(|r| r.len()), Some(2));
    assert!(crow.cindexed_rowset_entries().is_none());

    let cidx = PyRep::cindexed_rowset(&desc, "id", rows.clone());
    assert!(cidx.is_cindexed_rowset());
    assert!(!cidx.is_crowset());
    assert_eq!(cidx.row_count(), Some(2));
    assert_eq!(cidx.cindexed_rowset_entries().map(|e| e.len()), Some(2));
    assert!(cidx.crowset_rows().is_none());

    // Also verify dbutil.CRowset and dbutil.CIndexedRowset short token headers
    let short_crow = PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::Token(
            "dbutil.CRowset".into(),
        )])])),
        list: vec![PyRep::None],
        dict: Vec::new(),
    };
    assert!(short_crow.is_crowset());
    assert_eq!(short_crow.row_count(), Some(1));

    let short_cidx = PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::Token(
            "dbutil.CIndexedRowset".into(),
        )])])),
        list: Vec::new(),
        dict: vec![(PyRep::Integer(1), PyRep::None)],
    };
    assert!(short_cidx.is_cindexed_rowset());
    assert_eq!(short_cidx.row_count(), Some(1));

    // is_type_2 = false must not match CRowset / CIndexedRowset
    let type1_obj = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::Token(
            "dbutil.CRowset".into(),
        )])])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    assert!(!type1_obj.is_crowset());
    assert!(!type1_obj.is_cindexed_rowset());
    assert_eq!(type1_obj.row_count(), None);

    let idx_rs = PyRep::index_rowset("id", vec!["id", "name"], vec![(PyRep::Integer(1), rows[0].clone())]);
    assert_eq!(idx_rs.row_count(), Some(1));

    let tset = PyRep::tuple_set(vec!["id", "name"], rows.clone());
    assert_eq!(tset.row_count(), Some(2));

    assert_eq!(PyRep::List(vec![PyRep::None]).row_count(), Some(1));
    assert_eq!(PyRep::PySet(vec![PyRep::None, PyRep::None]).row_count(), Some(2));
    assert_eq!(PyRep::Set(Vec::new()).row_count(), Some(0));
    assert_eq!(PyRep::None.row_count(), None);

    // dbrow_columns and dbrow_get
    let dbrow = desc.build_dbrow(rows[0].clone());
    assert_eq!(dbrow.dbrow_columns().map(|c| c.len()), Some(2));
    assert_eq!(dbrow.dbrow_get("name"), Some(&PyRep::String("One".into())));
    assert_eq!(dbrow.dbrow_get("missing"), None);
    assert!(PyRep::None.dbrow_columns().is_none());
    assert!(PyRep::None.dbrow_get("id").is_none());
}

#[test]
fn test_pyrep_dict_get_all_container_shapes_and_node_count() {
    // 1. KeyVal & Dict
    let kv = PyRep::keyval([("k1", PyRep::Integer(11))]);
    assert_eq!(kv.dict_get("k1"), Some(&PyRep::Integer(11)));
    assert_eq!(kv.dict_get("k2"), None);

    let dict = PyRep::Dict(vec![(PyRep::String("d1".into()), PyRep::Integer(22))]);
    assert_eq!(dict.dict_get("d1"), Some(&PyRep::Integer(22)));

    // 2. DBRow, Rowset, TupleSet
    let desc = DBRowDescriptor::new([("colA", DbColumnType::I4)]);
    let dbr = desc.build_dbrow(vec![PyRep::Integer(33)]);
    assert_eq!(dbr.dict_get("colA"), Some(&PyRep::Integer(33)));

    let rs = PyRep::rowset(vec!["colA", "colB"], vec![vec![PyRep::Integer(44), PyRep::Integer(55)]]);
    assert_eq!(rs.dict_get("colB"), Some(&PyRep::Integer(55)));
    assert_eq!(rs.dict_get("colC"), None);

    let ts = PyRep::tuple_set(vec!["colA"], vec![vec![PyRep::Integer(66)]]);
    assert_eq!(ts.dict_get("colA"), Some(&PyRep::Integer(66)));

    // 3. Object("util.Row") and fallback to args.dict_get
    let util_row = PyRep::Object {
        type_name: "util.Row".into(),
        args: Box::new(PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::String("pilot".into()), PyRep::String("ship".into())]),
            PyRep::List(vec![PyRep::Integer(90_000_001), PyRep::Integer(587)]),
        ])),
    };
    assert_eq!(util_row.dict_get("ship"), Some(&PyRep::Integer(587)));
    assert_eq!(util_row.dict_get("unknown"), None);

    let nested_obj = PyRep::Object {
        type_name: "custom.Wrapper".into(),
        args: Box::new(kv.clone()),
    };
    assert_eq!(nested_obj.dict_get("k1"), Some(&PyRep::Integer(11)));

    // 4. ObjectEx dict lookup and fallback to first list element
    let obj_ex = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::None),
        list: vec![dict.clone()],
        dict: vec![(PyRep::String("ex_key".into()), PyRep::Integer(77))],
    };
    assert_eq!(obj_ex.dict_get("ex_key"), Some(&PyRep::Integer(77)));
    assert_eq!(obj_ex.dict_get("d1"), Some(&PyRep::Integer(22)));

    // 5. Tuple & List recursive dict_get
    let tup = PyRep::Tuple(vec![PyRep::None, kv.clone()]);
    assert_eq!(tup.dict_get("k1"), Some(&PyRep::Integer(11)));
    assert_eq!(PyRep::Integer(1).dict_get("k1"), None);

    // 6. node_count across variants and 64-byte scaling
    assert_eq!(PyRep::None.node_count(), 1);
    assert_eq!(PyRep::Buffer(vec![0u8; 63]).node_count(), 1);
    assert_eq!(PyRep::Buffer(vec![0u8; 64]).node_count(), 2);
    assert_eq!(PyRep::Picked(vec![0u8; 128]).node_count(), 3);
    assert_eq!(PyRep::String("a".repeat(64)).node_count(), 2);
    let chk = PyRep::ChecksumedStream {
        checksum: 123,
        body: Box::new(PyRep::SubStruct(Box::new(PyRep::SubStream(Box::new(obj_ex))))),
    };
    assert!(chk.node_count() >= 8);
}

#[test]
fn test_validate_frame_length_and_bridge_envelope_wire_formats() {
    // 1. validate_frame_length exact boundaries
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

    // 2. BridgeRpcError & BridgeEnvelope JSON + MessagePack roundtrips
    let err = BridgeRpcError::user_error("CustomUserError");
    assert_eq!(err.code, "CustomUserError");
    assert_eq!(err.message, "CustomUserError");

    let env = BridgeEnvelope::RpcResponse {
        req_id: 99,
        result: Err(err),
    };
    let wire = env.to_wire_bytes().expect("to_wire_bytes");
    assert_eq!(BridgeEnvelope::from_wire_bytes(&wire).unwrap(), env);
    // Also decode raw JSON slice without 4-byte prefix
    assert_eq!(BridgeEnvelope::from_wire_bytes(&wire[4..]).unwrap(), env);

    // Also decode MessagePack-encoded BridgeEnvelope fallback
    let ev = BridgeEnvelope::ServerEvent {
        tick: 42,
        event: "DoDestinyUpdate".into(),
        args: vec![PyRep::Bool(true)],
    };
    let msgpack_bytes = rmp_serde::to_vec_named(&ev).expect("msgpack encode");
    assert_eq!(BridgeEnvelope::from_wire_bytes(&msgpack_bytes).unwrap(), ev);

    assert_eq!(
        BridgeEnvelope::from_wire_bytes(&[]),
        Err(ProtoError::UnexpectedEof)
    );
    assert!(matches!(
        BridgeEnvelope::from_wire_bytes(b"not valid json or msgpack"),
        Err(ProtoError::Json(_))
    ));
}

#[test]
fn test_gateway_frame_protobuf_wire_and_pyrep_roundtrips_and_errors() {
    assert_eq!(GatewayFrameKind::from_u64(0), None);
    assert_eq!(GatewayFrameKind::from_u64(1), Some(GatewayFrameKind::Request));
    assert_eq!(GatewayFrameKind::from_u64(2), Some(GatewayFrameKind::Response));
    assert_eq!(GatewayFrameKind::from_u64(3), Some(GatewayFrameKind::Notice));
    assert_eq!(GatewayFrameKind::from_u64(4), None);

    for (kind, char_id, idem_key) in [
        (GatewayFrameKind::Request, Some(-90_000_001), Some("idem-123".to_string())),
        (GatewayFrameKind::Response, Some(90_000_001), None),
        (GatewayFrameKind::Notice, Some(-1), Some(String::new())),
        (GatewayFrameKind::Notice, None, None),
    ] {
        let frame = GatewayFrame {
            kind,
            correlation_id: 300_000,
            type_name: "eve_public.test.Request".into(),
            status_code: 200,
            actor_character_id: char_id,
            idempotency_key: idem_key,
            payload: vec![1, 2, 3, 4, 5],
        };
        let pb = frame.encode_protobuf_wire();
        let decoded_pb = GatewayFrame::decode_protobuf_wire(&pb).expect("decode_protobuf_wire");
        assert_eq!(decoded_pb, frame);

        let py = frame.to_pyrep();
        let decoded_py = GatewayFrame::from_pyrep(&py).expect("from_pyrep");
        assert_eq!(decoded_py, frame);
    }

    // Error paths on decode_protobuf_wire & from_pyrep
    assert_eq!(
        GatewayFrame::decode_protobuf_wire(&[]),
        Err(ProtoError::UnexpectedEof)
    );
    assert_eq!(
        GatewayFrame::decode_protobuf_wire(&[0x08, 0x09]), // invalid kind = 9
        Err(ProtoError::UnsupportedOpcode(9))
    );
    assert_eq!(
        GatewayFrame::decode_protobuf_wire(&[0x78, 0x01]), // unknown field 15
        Err(ProtoError::UnsupportedOpcode(0x78))
    );
    assert_eq!(
        GatewayFrame::decode_protobuf_wire(&[0x1A, 0x0A, b'a']), // truncated bytes field
        Err(ProtoError::UnexpectedEof)
    );
    assert_eq!(
        GatewayFrame::from_pyrep(&PyRep::None),
        Err(ProtoError::UnexpectedEof)
    );
    assert_eq!(
        GatewayFrame::from_pyrep(&PyRep::Tuple(vec![PyRep::Integer(1)])),
        Err(ProtoError::UnexpectedEof)
    );
}
