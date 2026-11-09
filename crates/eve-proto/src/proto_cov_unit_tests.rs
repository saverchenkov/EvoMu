use crate::marshal::{decode, encode, MAX_FRAME_SIZE};
use crate::quasar::envelopes::{
    ActiveIdentity, AuthoritativeContext, EvePublicNotice, EvePublicRequest, EvePublicResponse,
};
use crate::quasar::registry::QuasarDescriptorRegistry;
use crate::quasar::types::{
    EveIpAddress, EvePublicCharacterId, EvePublicCorpRoleCollection, EvePublicCorporationId,
    GoogleTimestamp, QuasarMessage, TraceContext, TraceFlags, TraceState,
};
use crate::quasar::{
    DescriptorProto, FieldDescriptorProto, FieldLabel, FieldType, QuasarError, WireField,
    WireMessage, WireValue,
};
use crate::{MarshalOpcode, ProtoError, PyRep};

#[test]
fn pyrep_rowset_helpers_and_marshal_error_paths_covered() {
    let crowset = PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::String(
            "dbutil.CRowset".into(),
        )])])),
        list: vec![PyRep::Integer(1), PyRep::Integer(2)],
        dict: vec![],
    };
    assert!(crowset.is_crowset());
    assert_eq!(crowset.crowset_rows().unwrap().len(), 2);
    assert!(!crowset.is_cindexed_rowset());
    assert!(crowset.cindexed_rowset_entries().is_none());

    let cindexed = PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::String(
            "carbon.common.script.sys.crowset.CIndexedRowset".into(),
        )])])),
        list: vec![],
        dict: vec![(PyRep::Integer(10), PyRep::String("row10".into()))],
    };
    assert!(cindexed.is_cindexed_rowset());
    assert_eq!(cindexed.cindexed_rowset_entries().unwrap().len(), 1);
    assert!(!cindexed.is_crowset());
    assert!(cindexed.crowset_rows().is_none());

    // Empty, oversized, invalid header, truncated save count & trailing bytes
    assert!(matches!(decode(&[]), Err(ProtoError::UnexpectedEof)));
    assert!(matches!(
        decode(&vec![0x7E; MAX_FRAME_SIZE + 1]),
        Err(ProtoError::FrameTooLarge(_))
    ));
    assert!(matches!(
        decode(&[0x00, 0, 0, 0, 0]),
        Err(ProtoError::InvalidHeader(0x00))
    ));
    assert!(matches!(decode(&[0x7E, 1, 0]), Err(ProtoError::UnexpectedEof)));
    assert!(matches!(
        decode(&[0x7E, 0xFF, 0xFF, 0xFF, 0x7F, 0x01]),
        Err(ProtoError::CollectionTooLarge(_))
    ));
    assert!(matches!(
        decode(&[0x7E, 1, 0, 0, 0, 0x01]),
        Err(ProtoError::UnexpectedEof)
    ));
    assert!(matches!(
        decode(&[0x7E, 0, 0, 0, 0, 0x01, 0x01]),
        Err(ProtoError::TrailingBytes(1))
    ));

    // Truncated opcodes across scalar, string, object, and stream variants
    for bad in &[
        vec![0x7E, 0, 0, 0, 0, MarshalOpcode::PyReal as u8, 1, 2],
        vec![0x7E, 0, 0, 0, 0, MarshalOpcode::PyBuffer as u8, 5, 1],
        vec![0x7E, 0, 0, 0, 0, MarshalOpcode::PyCharString as u8],
        vec![0x7E, 0, 0, 0, 0, MarshalOpcode::PyShortString as u8],
        vec![0x7E, 0, 0, 0, 0, MarshalOpcode::PyShortString as u8, 4, 1, 2],
        vec![0x7E, 0, 0, 0, 0, MarshalOpcode::PyStringTableItem as u8],
        vec![0x7E, 0, 0, 0, 0, MarshalOpcode::PyStringTableItem as u8, 0],
        vec![0x7E, 0, 0, 0, 0, MarshalOpcode::PyWStringUCS2Char as u8, 1],
        vec![0x7E, 0, 0, 0, 0, MarshalOpcode::PyWStringUCS2 as u8, 3, 0, 0],
        vec![0x7E, 0, 0, 0, 0, MarshalOpcode::PyLongString as u8, 5, 1],
        vec![0x7E, 0, 0, 0, 0, MarshalOpcode::PyPicked as u8, 5, 1],
        vec![0x7E, 0, 0, 0, 0, MarshalOpcode::PyWStringUTF8 as u8, 5, 1],
        vec![0x7E, 0, 0, 0, 0, MarshalOpcode::PyObjectEx2 as u8, MarshalOpcode::PyOneInteger as u8],
        vec![
            0x7E,
            0,
            0,
            0,
            0,
            MarshalOpcode::PyObjectEx2 as u8,
            MarshalOpcode::PyOneInteger as u8,
            MarshalOpcode::PackedTerminator as u8,
        ],
        vec![0x7E, 0, 0, 0, 0, MarshalOpcode::PyChecksumedStream as u8, 1, 2],
    ] {
        assert!(decode(bad).is_err(), "expected error for {bad:02X?}");
    }

    // Valid roundtrip of Buffer, SubStruct, ChecksumedStream, and PyObject
    let complex = PyRep::Tuple(vec![
        PyRep::Buffer(b"raw-buf".to_vec()),
        PyRep::SubStruct(Box::new(PyRep::String("inner".into()))),
        PyRep::ChecksumedStream {
            checksum: 0xDEAD_BEEF,
            body: Box::new(PyRep::Integer(12345)),
        },
        PyRep::Object {
            type_name: "custom.Widget".into(),
            args: Box::new(PyRep::Dict(vec![(
                PyRep::String("k".into()),
                PyRep::Integer(9),
            )])),
        },
    ]);
    let enc = encode(&complex);
    let dec = decode(&enc).expect("decode complex");
    assert_eq!(dec, complex);
}

#[test]
fn quasar_registry_full_load_validation_and_deep_sampling() {
    let reg = QuasarDescriptorRegistry::open_default().expect("open 1455 quasar descriptors");
    assert_eq!(reg.files.len(), reg.manifest.descriptor_file_count);
    assert!(reg.get_file_by_pyj("nonexistent/path.pyj").is_none());
    assert!(reg.get_message("google.protobuf.Timestamp").is_some());
    assert!(matches!(
        reg.instantiate_populated_sample("nonexistent.MessageType", 7),
        Err(QuasarError::UnknownMessageType(_))
    ));

    // Exercise sample instantiation and wire roundtrip on loaded descriptors
    let mut tested = 0usize;
    for f in &reg.files {
        for m in &f.messages {
            let sample = reg
                .instantiate_populated_sample(&m.full_name, 42)
                .expect("sample instantiates");
            let wire = reg.encode_dynamic(&sample).expect("encode dynamic sample");
            let decoded = reg
                .decode_dynamic(&m.full_name, &wire)
                .expect("sample roundtrips on wire");
            assert_eq!(decoded, sample);
            tested += 1;
            if tested >= 25 {
                break;
            }
        }
        if tested >= 25 {
            break;
        }
    }
    assert_eq!(tested, 25);

    // Exercise depth > 6 recursion guard on a 9-level message chain
    let mut custom = (*reg).clone();
    for level in 0..=8usize {
        let mut fields = Vec::new();
        if level < 8 {
            fields.push(FieldDescriptorProto {
                name: "next".into(),
                number: 1,
                label: FieldLabel::Optional,
                field_type: FieldType::Message,
                type_name: format!("custom.ChainNode{}", level + 1),
                oneof_index: None,
                json_name: "next".into(),
            });
        }
        custom.insert_message_for_test(DescriptorProto {
            name: format!("ChainNode{level}"),
            full_name: format!("custom.ChainNode{level}"),
            fields,
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        });
    }
    let deep = custom
        .instantiate_populated_sample("custom.ChainNode0", 1)
        .expect("deep sample terminates cleanly at depth > 6");
    assert!(!deep.fields.is_empty());
}

#[test]
fn quasar_envelopes_and_types_cover_optional_and_unknown_fields() {
    let identity = ActiveIdentity {
        character: Some(EvePublicCharacterId { sequential: 9001 }),
        corporation: Some(EvePublicCorporationId {
            sequential: 98001,
        }),
        roles: Some(EvePublicCorpRoleCollection { bitmask: 0x55 }),
    };
    let id_wire = identity.encode_proto();
    assert_eq!(ActiveIdentity::decode_proto(&id_wire).unwrap(), identity);

    let ts = GoogleTimestamp {
        seconds: 1_700_000_123,
        nanos: 456_789,
    };
    let ts_wire = ts.encode_proto();
    assert_eq!(GoogleTimestamp::decode_proto(&ts_wire).unwrap(), ts);

    let trace = TraceContext {
        trace_id: vec![1; 16],
        parent_id: vec![2; 8],
        flags: Some(TraceFlags { sampled: true }),
        state: Some(TraceState { sample_rate: 100 }),
    };
    let trace_wire = trace.encode_proto();
    let trace_dec = TraceContext::decode_proto(&trace_wire).unwrap();
    assert_eq!(trace_dec, trace);

    let mut auth = AuthoritativeContext::for_session_at(
        Some(77),
        Some(9001),
        Some(98001),
        0x55,
        "tranquility",
        vec![9; 16],
        1_700_000_123,
    );
    auth.ip_address = Some(EveIpAddress::V4(0x7F00_0001));
    let auth_wire = auth.encode_proto();
    let mut wm = WireMessage::decode(&auth_wire).unwrap();
    wm.fields.push(WireField {
        field_number: 999,
        value: WireValue::Varint(1),
    });
    let auth_dec = AuthoritativeContext::decode_proto(&wm.encode()).unwrap();
    assert_eq!(auth_dec.tenant, "tranquility");
    assert_eq!(auth_dec.corp_role_mask, 0x55);

    let mut req = EvePublicRequest::default();
    req.correlation_uuid = vec![3; 16];
    let req_dec = EvePublicRequest::decode_proto(&req.encode_proto()).unwrap();
    assert_eq!(req_dec.correlation_uuid, vec![3; 16]);

    let mut resp = EvePublicResponse::default();
    resp.status_code = 200;
    let resp_dec = EvePublicResponse::decode_proto(&resp.encode_proto()).unwrap();
    assert_eq!(resp_dec.status_code, 200);

    let notice = EvePublicNotice::default();
    let notice_dec = EvePublicNotice::decode_proto(&notice.encode_proto()).unwrap();
    assert_eq!(notice_dec, notice);
}

#[test]
fn pyrep_node_count_and_wire_boundaries_cover_all_arithmetic_branches() {
    assert_eq!(crate::MAX_FRAME_SIZE, 4 * 1024 * 1024);

    let plain_row = PyRep::Object {
        type_name: "Row".into(),
        args: Box::new(PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::String("k".into())]),
            PyRep::List(vec![PyRep::Integer(42)]),
        ])),
    };
    assert_eq!(plain_row.dict_get("k"), Some(&PyRep::Integer(42)));

    assert_eq!(
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)]).node_count(),
        3
    );
    assert_eq!(
        PyRep::Dict(vec![
            (PyRep::Integer(1), PyRep::Integer(2)),
            (PyRep::Integer(3), PyRep::Integer(4)),
        ])
        .node_count(),
        5
    );
    let kv = std::collections::HashMap::from([
        ("a".to_string(), PyRep::Integer(1)),
        ("b".to_string(), PyRep::Integer(2)),
    ]);
    assert_eq!(PyRep::KeyVal(kv).node_count(), 5);
    assert_eq!(
        PyRep::Rowset {
            columns: vec!["c1".into(), "c2".into(), "c3".into()],
            rows: vec![vec![PyRep::Integer(1), PyRep::Integer(2)]],
        }
        .node_count(),
        6
    );
    assert_eq!(
        PyRep::IndexRowset {
            key_column: "id".into(),
            columns: vec!["id".into(), "v".into()],
            items: vec![(
                PyRep::Integer(10),
                vec![PyRep::Integer(10), PyRep::Integer(20)],
            )],
        }
        .node_count(),
        7
    );
    assert_eq!(
        PyRep::DBRow {
            header: Box::new(PyRep::Integer(1)),
            columns: vec![
                ("a".into(), PyRep::Integer(2)),
                ("b".into(), PyRep::Integer(3)),
            ],
            raw_rle: vec![0u8; 128],
        }
        .node_count(),
        8
    );
    assert_eq!(
        PyRep::ObjectEx {
            is_type_2: false,
            header: Box::new(PyRep::Tuple(vec![PyRep::Integer(1)])),
            list: vec![PyRep::Integer(2), PyRep::Integer(3), PyRep::Integer(4)],
            dict: vec![
                (PyRep::Integer(5), PyRep::Integer(6)),
                (PyRep::Integer(7), PyRep::Integer(8)),
            ],
        }
        .node_count(),
        10
    );

    let max_frame = vec![b'{'; crate::MAX_FRAME_SIZE];
    assert!(matches!(
        crate::BridgeEnvelope::from_wire_bytes(&max_frame),
        Err(ProtoError::Json(_))
    ));

    let overlong_varint = [
        0x08, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x00,
    ];
    assert_eq!(
        crate::GatewayFrame::decode_protobuf_wire(&overlong_varint),
        Err(ProtoError::UnexpectedEof)
    );

    let desc = crate::dbrow::DBRowDescriptor::new([
        ("id", crate::dbrow::DbColumnType::I4),
        ("ownerID", crate::dbrow::DbColumnType::I4),
        ("qty", crate::dbrow::DbColumnType::I4),
    ]);
    let rows = vec![vec![
        PyRep::Integer(100),
        PyRep::Integer(200),
        PyRep::Integer(300),
    ]];
    let cidx = desc.build_cindexed_rowset("ownerID", rows.clone());
    assert_eq!(
        cidx.cindexed_rowset_entries().unwrap()[0].0,
        PyRep::Integer(200)
    );
    if let PyRep::IndexRowset { items, .. } = desc.build_index_rowset("qty", rows) {
        assert_eq!(items[0].0, PyRep::Integer(300));
    } else {
        panic!("expected IndexRowset");
    }

    use crate::dbrow::{pack_packed_row_columns, unpack_packed_row_columns, DbColumnType};
    let mut full_cols: Vec<(String, DbColumnType)> = vec![
        ("null_i8".into(), DbColumnType::I8),
        ("i8_val".into(), DbColumnType::I8),
        ("cy_real".into(), DbColumnType::Cy),
        ("r8_val".into(), DbColumnType::R8),
        ("null_i4".into(), DbColumnType::I4),
        ("i4_val".into(), DbColumnType::I4),
        ("ui4_val".into(), DbColumnType::Ui4),
        ("r4_val".into(), DbColumnType::R4),
        ("null_i2".into(), DbColumnType::I2),
        ("i2_val".into(), DbColumnType::I2),
        ("ui2_val".into(), DbColumnType::Ui2),
        ("null_i1".into(), DbColumnType::I1),
        ("i1_val".into(), DbColumnType::I1),
        ("ui1_val".into(), DbColumnType::Ui1),
        ("null_str".into(), DbColumnType::Str),
    ];
    for i in 0..10 {
        full_cols.push((format!("b_{i}"), DbColumnType::Bool));
    }
    let full_desc = crate::dbrow::DBRowDescriptor::new(full_cols);
    let header = full_desc.to_pyrep();
    let mut col_vals = vec![
        ("null_i8".into(), PyRep::None),
        ("i8_val".into(), PyRep::Integer(-9_000_000_001)),
        ("cy_real".into(), PyRep::Real(12.5)),
        ("r8_val".into(), PyRep::Real(3.25)),
        ("null_i4".into(), PyRep::None),
        ("i4_val".into(), PyRep::Integer(-123_456)),
        ("ui4_val".into(), PyRep::Integer(3_000_000_001)),
        ("r4_val".into(), PyRep::Real(1.5)),
        ("null_i2".into(), PyRep::None),
        ("i2_val".into(), PyRep::Integer(-12_345)),
        ("ui2_val".into(), PyRep::Integer(55_001)),
        ("null_i1".into(), PyRep::None),
        ("i1_val".into(), PyRep::Integer(-99)),
        ("ui1_val".into(), PyRep::Integer(241)),
        ("null_str".into(), PyRep::None),
    ];
    for i in 0..10 {
        col_vals.push((format!("b_{i}"), PyRep::Bool(i % 2 == 1)));
    }
    let packed_rle = pack_packed_row_columns(&header, &col_vals);
    let unpacked = unpack_packed_row_columns(&header, &packed_rle);
    let map: std::collections::HashMap<_, _> = unpacked.into_iter().collect();
    assert_eq!(map["null_i8"], PyRep::None);
    assert_eq!(map["i8_val"], PyRep::Integer(-9_000_000_001));
    assert_eq!(map["cy_real"], PyRep::Real(12.5));
    assert_eq!(map["r8_val"], PyRep::Real(3.25));
    assert_eq!(map["null_i4"], PyRep::None);
    assert_eq!(map["i4_val"], PyRep::Integer(-123_456));
    assert_eq!(map["ui4_val"], PyRep::Integer(3_000_000_001));
    assert_eq!(map["r4_val"], PyRep::Real(1.5));
    assert_eq!(map["null_i2"], PyRep::None);
    assert_eq!(map["i2_val"], PyRep::Integer(-12_345));
    assert_eq!(map["ui2_val"], PyRep::Integer(55_001));
    assert_eq!(map["null_i1"], PyRep::None);
    assert_eq!(map["i1_val"], PyRep::Integer(-99));
    assert_eq!(map["ui1_val"], PyRep::Integer(241));
    assert_eq!(map["null_str"], PyRep::None);
    assert_eq!(map["b_0"], PyRep::Bool(false));
    assert_eq!(map["b_9"], PyRep::Bool(true));
}

