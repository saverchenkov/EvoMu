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

#[test]
fn quasar_registry_open_validation_and_dynamic_wire_edge_cases() {
    use crate::quasar::registry::{DynamicQuasarMessage, DynamicValue, QuasarManifest};
    use crate::quasar::{
        encode_bytes_field, encode_varint_field, EnumDescriptorProto, EnumValueDescriptorProto,
        FileDescriptorProto, OneofDescriptorProto, ServiceDescriptorProto,
    };
    use crate::MAX_RECURSION_DEPTH;

    // 1. QuasarDescriptorRegistry::open error paths with temp sqlite + manifest files
    let pid = std::process::id();
    let db_path = std::env::temp_dir().join(format!("evomu_qreg_test_{pid}.sqlite"));
    let mf_path = std::env::temp_dir().join(format!("evomu_qreg_mf_{pid}.json"));
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&mf_path);

    {
        let conn = rusqlite::Connection::open(&db_path).expect("create temp sqlite");
        conn.execute(
            "CREATE TABLE other_table (id INTEGER PRIMARY KEY)",
            [],
        )
        .expect("create other_table");
    }

    // Missing manifest
    assert!(matches!(
        QuasarDescriptorRegistry::open(&db_path, std::path::Path::new("/nonexistent/mf.json")),
        Err(QuasarError::ManifestMismatch(_))
    ));

    // Invalid JSON manifest
    std::fs::write(&mf_path, b"{not-json").expect("write bad json");
    assert!(matches!(
        QuasarDescriptorRegistry::open(&db_path, &mf_path),
        Err(QuasarError::ManifestMismatch(_))
    ));

    // Valid JSON manifest, missing table in sqlite -> DatabaseError
    let valid_mf = QuasarManifest {
        schema_version: 1,
        domain_count: 1,
        descriptor_file_count: 1,
        unique_package_count: 1,
        top_level_message_count: 0,
        total_message_count: 0,
        total_enum_count: 0,
        total_service_count: 0,
        total_field_count: 0,
        total_serialized_pb_bytes: 0,
        combined_serialized_pb_sha256: "0".into(),
        combined_serialized_pb_md5: "0".into(),
        per_domain_file_counts: std::collections::BTreeMap::from([("test".into(), 1)]),
    };
    std::fs::write(&mf_path, serde_json::to_vec(&valid_mf).unwrap()).expect("write manifest");
    assert!(matches!(
        QuasarDescriptorRegistry::open(&db_path, &mf_path),
        Err(QuasarError::DatabaseError(_))
    ));

    // Table exists with wrong checksum -> ManifestMismatch
    {
        let conn = rusqlite::Connection::open(&db_path).expect("reopen sqlite");
        conn.execute(
            "CREATE TABLE quasar_file_descriptors (
                pyj_path TEXT, domain TEXT, sha256 TEXT, md5 TEXT, serialized_pb BLOB
            )",
            [],
        )
        .expect("create table");
        let fd = FileDescriptorProto {
            pyj_path: "a.pyj".into(),
            domain: "test".into(),
            name: "a.proto".into(),
            package: "test".into(),
            syntax: "proto3".into(),
            dependencies: vec![],
            messages: vec![],
            enums: vec![],
            services: vec![],
            sha256: String::new(),
            md5: String::new(),
            raw_bytes: vec![],
        };
        let wire = fd.encode_wire();
        conn.execute(
            "INSERT INTO quasar_file_descriptors VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["a.pyj", "test", "bad_sha", "bad_md5", wire],
        )
        .expect("insert row");
    }
    assert!(matches!(
        QuasarDescriptorRegistry::open(&db_path, &mf_path),
        Err(QuasarError::ManifestMismatch(_))
    ));

    // Fix row checksum -> fails on descriptor file count mismatch (1 != EXPECTED_QUASAR_FILE_COUNT)
    {
        let conn = rusqlite::Connection::open(&db_path).expect("reopen sqlite");
        let fd = FileDescriptorProto {
            pyj_path: "a.pyj".into(),
            domain: "test".into(),
            name: "a.proto".into(),
            package: "test".into(),
            syntax: "proto3".into(),
            dependencies: vec![],
            messages: vec![],
            enums: vec![],
            services: vec![],
            sha256: String::new(),
            md5: String::new(),
            raw_bytes: vec![],
        };
        let wire = fd.encode_wire();
        let sha = crate::quasar::sha256_hex(&wire);
        let md5 = crate::quasar::md5_hex(&wire);
        conn.execute("DELETE FROM quasar_file_descriptors", []).unwrap();
        conn.execute(
            "INSERT INTO quasar_file_descriptors VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params!["a.pyj", "test", sha, md5, wire],
        )
        .unwrap();
    }
    assert!(matches!(
        QuasarDescriptorRegistry::open(&db_path, &mf_path),
        Err(QuasarError::ManifestMismatch(_))
    ));

    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(&mf_path);

    // 2. open_default_or_empty always yields usable registry with builtins
    let fallback_reg = QuasarDescriptorRegistry::open_default_or_empty();
    assert!(fallback_reg.get_message("google.protobuf.Timestamp").is_some());

    // 3. Unknown-type encode_dynamic across every DynamicValue variant + empty Repeated
    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();
    let mut sub = DynamicQuasarMessage::new("custom.Sub");
    sub.set(1, DynamicValue::String("nested".into()));

    let mut untyped = DynamicQuasarMessage::new("custom.Untyped");
    untyped.set(1, DynamicValue::Double(1.25));
    untyped.set(2, DynamicValue::Float(-2.5));
    untyped.set(3, DynamicValue::Int64(-100));
    untyped.set(4, DynamicValue::Uint64(200));
    untyped.set(5, DynamicValue::Int32(-30));
    untyped.set(6, DynamicValue::Uint32(40));
    untyped.set(7, DynamicValue::Sint32(-50));
    untyped.set(8, DynamicValue::Sint64(-60));
    untyped.set(9, DynamicValue::Fixed32(70));
    untyped.set(10, DynamicValue::Fixed64(80));
    untyped.set(11, DynamicValue::Sfixed32(-90));
    untyped.set(12, DynamicValue::Sfixed64(-100));
    untyped.set(13, DynamicValue::Bool(true));
    untyped.set(14, DynamicValue::String("hello".into()));
    untyped.set(15, DynamicValue::Bytes(vec![0xAA, 0xBB]));
    untyped.set(16, DynamicValue::Enum(7));
    untyped.set(17, DynamicValue::Message(Box::new(sub.clone())));
    untyped.set(18, DynamicValue::Repeated(vec![]));
    untyped.set(
        19,
        DynamicValue::Repeated(vec![
            DynamicValue::String("a".into()),
            DynamicValue::String("b".into()),
        ]),
    );
    let untyped_wire = reg.encode_dynamic(&untyped).expect("encode untyped");
    let wire_dec = reg
        .decode_dynamic_or_wire("custom.Untyped", &untyped_wire)
        .expect("decode wire fallback");
    assert_eq!(wire_dec.get(14), Some(&DynamicValue::String("hello".into())));
    assert_eq!(wire_dec.get(18), None);
    assert_eq!(
        wire_dec.get(19),
        Some(&DynamicValue::Repeated(vec![
            DynamicValue::String("a".into()),
            DynamicValue::String("b".into()),
        ]))
    );

    // 4. Typed descriptor covering unknown field skip, invalid UTF-8, repeated Message/String,
    //    packed scalar repeated error paths, and recursion depth > MAX_RECURSION_DEPTH
    reg.insert_message_for_test(DescriptorProto {
        name: "Node".into(),
        full_name: "test.Node".into(),
        fields: vec![
            FieldDescriptorProto {
                name: "child".into(),
                number: 1,
                label: FieldLabel::Optional,
                field_type: FieldType::Message,
                type_name: "test.Node".into(),
                oneof_index: None,
                json_name: "child".into(),
            },
            FieldDescriptorProto {
                name: "text".into(),
                number: 2,
                label: FieldLabel::Optional,
                field_type: FieldType::String,
                type_name: String::new(),
                oneof_index: None,
                json_name: "text".into(),
            },
            FieldDescriptorProto {
                name: "tags".into(),
                number: 3,
                label: FieldLabel::Repeated,
                field_type: FieldType::String,
                type_name: String::new(),
                oneof_index: None,
                json_name: "tags".into(),
            },
            FieldDescriptorProto {
                name: "ints".into(),
                number: 4,
                label: FieldLabel::Repeated,
                field_type: FieldType::Int64,
                type_name: String::new(),
                oneof_index: None,
                json_name: "ints".into(),
            },
            FieldDescriptorProto {
                name: "doubles".into(),
                number: 5,
                label: FieldLabel::Repeated,
                field_type: FieldType::Double,
                type_name: String::new(),
                oneof_index: None,
                json_name: "doubles".into(),
            },
        ],
        nested_types: vec![],
        enum_types: vec![],
        oneofs: vec![],
        is_map_entry: false,
    });

    assert!(matches!(
        reg.decode_dynamic("test.Missing", &[]),
        Err(QuasarError::UnknownMessageType(_))
    ));

    // Unknown field #999 skipped, repeated tags aggregated
    let mut node_wire = Vec::new();
    encode_varint_field(999, 123, &mut node_wire);
    crate::quasar::encode_string_field(3, "t1", &mut node_wire);
    crate::quasar::encode_string_field(3, "t2", &mut node_wire);
    let node_dec = reg.decode_dynamic("test.Node", &node_wire).unwrap();
    assert_eq!(
        node_dec.get(3),
        Some(&DynamicValue::Repeated(vec![
            DynamicValue::String("t1".into()),
            DynamicValue::String("t2".into()),
        ]))
    );

    // Invalid UTF-8 in String field -> InvalidUtf8
    let mut bad_utf8 = Vec::new();
    encode_bytes_field(2, &[0xFF, 0xFE], &mut bad_utf8);
    assert!(matches!(
        reg.decode_dynamic("test.Node", &bad_utf8),
        Err(QuasarError::InvalidUtf8(_))
    ));

    // Truncated packed varint in field 4 and truncated 8-byte double in field 5
    let mut bad_packed_varint = Vec::new();
    encode_bytes_field(4, &[0x80], &mut bad_packed_varint);
    assert_eq!(
        reg.decode_dynamic("test.Node", &bad_packed_varint),
        Err(QuasarError::UnexpectedEof)
    );
    let mut bad_packed_f64 = Vec::new();
    encode_bytes_field(5, &[1, 2, 3, 4, 5], &mut bad_packed_f64);
    assert_eq!(
        reg.decode_dynamic("test.Node", &bad_packed_f64),
        Err(QuasarError::UnexpectedEof)
    );

    // RecursionLimitExceeded in both decode_dynamic and decode_wire_fallback (> MAX_RECURSION_DEPTH)
    let mut deep_wire = Vec::new();
    for _ in 0..=(MAX_RECURSION_DEPTH + 1) {
        let mut outer = Vec::new();
        encode_bytes_field(1, &deep_wire, &mut outer);
        deep_wire = outer;
    }
    assert_eq!(
        reg.decode_dynamic("test.Node", &deep_wire),
        Err(QuasarError::RecursionLimitExceeded)
    );
    // decode_wire_fallback safely stops recursing beyond MAX_RECURSION_DEPTH and falls back to DynamicValue::Bytes
    let mut deep_fallback = vec![0x08, 0x01];
    for _ in 0..=(MAX_RECURSION_DEPTH + 1) {
        let mut outer = Vec::new();
        encode_bytes_field(1, &deep_fallback, &mut outer);
        deep_fallback = outer;
    }
    let decoded_deep_fallback = reg
        .decode_dynamic_or_wire("unknown.Deep", &deep_fallback)
        .expect("decode_wire_fallback degrades depth overflow to Bytes");
    let mut cursor = &decoded_deep_fallback;
    let mut nested_depth = 0usize;
    while let Some(DynamicValue::Message(inner)) = cursor.get(1) {
        nested_depth += 1;
        cursor = inner;
    }
    assert_eq!(nested_depth, MAX_RECURSION_DEPTH);
    assert!(matches!(cursor.get(1), Some(DynamicValue::Bytes(_))));

    // decode_wire_fallback: valid UTF-8 with control char that is valid nested wire vs raw bytes
    let mut ctrl_msg = Vec::new();
    encode_bytes_field(1, &[0x08, 0x07], &mut ctrl_msg); // "\x08\x07" has ASCII control 0x08 & 0x07, valid proto field 1 varint 7
    let dec_ctrl = reg.decode_dynamic_or_wire("unknown.Ctrl", &ctrl_msg).unwrap();
    assert!(matches!(dec_ctrl.get(1), Some(DynamicValue::Message(_))));

    let mut ctrl_raw = Vec::new();
    encode_bytes_field(1, &[0x00, 0x01], &mut ctrl_raw); // wire type 0 field 0? wait 0x00 is field 0 wire type 0, followed by 0x01 -> invalid wire type 1? no, tag 0x00 varint 0x01... let's use 0x03 (invalid wire type 3)
    let mut ctrl_bad_wire = Vec::new();
    encode_bytes_field(1, &[0x03, 0x04], &mut ctrl_bad_wire);
    let dec_raw = reg
        .decode_dynamic_or_wire("unknown.CtrlRaw", &ctrl_bad_wire)
        .unwrap();
    assert_eq!(dec_raw.get(1), Some(&DynamicValue::Bytes(vec![0x03, 0x04])));

    // 5. generate_rust_module_for_file covering all FieldTypes and all reserved keywords
    let all_kw_file = FileDescriptorProto {
        pyj_path: "kw_pb2.pyj".into(),
        domain: "kw".into(),
        name: "kw.proto".into(),
        package: "kw".into(),
        syntax: "proto3".into(),
        dependencies: vec![],
        messages: vec![DescriptorProto {
            name: "AllFieldKinds".into(),
            full_name: "kw.AllFieldKinds".into(),
            fields: vec![
                FieldDescriptorProto { name: "loop".into(), number: 1, label: FieldLabel::Optional, field_type: FieldType::Double, type_name: String::new(), oneof_index: None, json_name: "loop".into() },
                FieldDescriptorProto { name: "in".into(), number: 2, label: FieldLabel::Optional, field_type: FieldType::Float, type_name: String::new(), oneof_index: None, json_name: "in".into() },
                FieldDescriptorProto { name: "ref".into(), number: 3, label: FieldLabel::Optional, field_type: FieldType::Int64, type_name: String::new(), oneof_index: None, json_name: "ref".into() },
                FieldDescriptorProto { name: "mod".into(), number: 4, label: FieldLabel::Optional, field_type: FieldType::Uint64, type_name: String::new(), oneof_index: None, json_name: "mod".into() },
                FieldDescriptorProto { name: "self".into(), number: 5, label: FieldLabel::Optional, field_type: FieldType::Sint32, type_name: String::new(), oneof_index: None, json_name: "self".into() },
                FieldDescriptorProto { name: "super".into(), number: 6, label: FieldLabel::Optional, field_type: FieldType::Fixed32, type_name: String::new(), oneof_index: None, json_name: "super".into() },
                FieldDescriptorProto { name: "crate".into(), number: 7, label: FieldLabel::Optional, field_type: FieldType::Bool, type_name: String::new(), oneof_index: None, json_name: "crate".into() },
                FieldDescriptorProto { name: "raw".into(), number: 8, label: FieldLabel::Optional, field_type: FieldType::Bytes, type_name: String::new(), oneof_index: None, json_name: "raw".into() },
                FieldDescriptorProto { name: "grp".into(), number: 9, label: FieldLabel::Optional, field_type: FieldType::Group, type_name: "kw.Other".into(), oneof_index: None, json_name: "grp".into() },
            ],
            nested_types: vec![],
            enum_types: vec![EnumDescriptorProto {
                name: "Kind".into(),
                full_name: "kw.AllFieldKinds.Kind".into(),
                values: vec![EnumValueDescriptorProto { name: "A".into(), number: 0 }],
            }],
            oneofs: vec![OneofDescriptorProto { name: "o".into() }],
            is_map_entry: false,
        }],
        enums: vec![],
        services: vec![ServiceDescriptorProto {
            name: "Svc".into(),
            full_name: "kw.Svc".into(),
            methods: vec![],
        }],
        sha256: String::new(),
        md5: String::new(),
        raw_bytes: vec![],
    };
    let generated = QuasarDescriptorRegistry::generate_rust_module_for_file(&all_kw_file);
    for kw in ["r#loop: f64", "r#in: f32", "r#ref: i64", "r#mod: u64", "r#self: i32", "r#super: u32", "r#crate: bool", "raw: Vec<u8>", "grp: Option<Box<Other>>"] {
        assert!(generated.contains(kw), "missing {kw} in:\n{generated}");
    }
}

#[test]
fn quasar_types_unknown_fields_corrupt_nested_messages_and_zero_sign_bits() {
    use crate::quasar::types::{
        ColorRgb, DeviceUserIdentifier, EveIdempotencyToken, EvePublicUserId, GoogleAny,
        GoogleDuration, GoogleEmpty, MathFraction, MathVector3, MathVector4, NetworkRttSample,
        SemanticVersionSpec,
    };
    use crate::quasar::{encode_bytes_field, encode_varint_field};

    // 1. Unknown field numbers & mismatched wire types skipped cleanly on core Quasar types
    let unknown_wire = {
        let mut b = Vec::new();
        encode_varint_field(99, 777, &mut b);
        b
    };
    assert_eq!(GoogleTimestamp::decode_proto(&unknown_wire).unwrap(), GoogleTimestamp::default());
    assert_eq!(GoogleDuration::decode_proto(&unknown_wire).unwrap(), GoogleDuration::default());
    assert_eq!(GoogleAny::decode_proto(&unknown_wire).unwrap(), GoogleAny::default());
    assert_eq!(GoogleEmpty::decode_proto(&unknown_wire).unwrap(), GoogleEmpty);
    assert_eq!(EvePublicUserId::decode_proto(&unknown_wire).unwrap(), EvePublicUserId::default());
    assert_eq!(EvePublicCharacterId::decode_proto(&unknown_wire).unwrap(), EvePublicCharacterId::default());
    assert_eq!(EvePublicCorporationId::decode_proto(&unknown_wire).unwrap(), EvePublicCorporationId::default());
    assert_eq!(EvePublicCorpRoleCollection::decode_proto(&unknown_wire).unwrap(), EvePublicCorpRoleCollection::default());
    assert_eq!(EveIdempotencyToken::decode_proto(&unknown_wire).unwrap(), EveIdempotencyToken::default());
    assert_eq!(EveIpAddress::decode_proto(&[]).unwrap(), EveIpAddress::V4(0));
    assert_eq!(EveIpAddress::decode_proto(&unknown_wire).unwrap(), EveIpAddress::V4(0));
    assert_eq!(TraceContext::decode_proto(&unknown_wire).unwrap(), TraceContext::default());
    assert_eq!(SemanticVersionSpec::decode_proto(&unknown_wire).unwrap(), SemanticVersionSpec::default());
    assert_eq!(ColorRgb::decode_proto(&unknown_wire).unwrap(), ColorRgb::default());
    assert_eq!(MathFraction::decode_proto(&unknown_wire).unwrap(), MathFraction::default());
    assert_eq!(MathVector3::decode_proto(&unknown_wire).unwrap(), MathVector3::default());
    assert_eq!(MathVector4::decode_proto(&unknown_wire).unwrap(), MathVector4::default());
    assert_eq!(DeviceUserIdentifier::decode_proto(&unknown_wire).unwrap(), DeviceUserIdentifier::default());
    assert_eq!(NetworkRttSample::decode_proto(&unknown_wire).unwrap(), NetworkRttSample::default());

    // 2. GoogleAny::from_any accepts type.evetech.net/ prefix and leading dot, and propagates inner decode error
    let uid = EvePublicUserId { sequential: -42 };
    let evetech_any = GoogleAny {
        type_url: "type.evetech.net/.eve_public.user.Identifier".into(),
        value: uid.encode_proto(),
    };
    assert_eq!(EvePublicUserId::from_any(&evetech_any).unwrap(), uid);

    let corrupt_any = GoogleAny {
        type_url: GoogleAny::type_url(),
        value: vec![0x08], // truncated varint
    };
    assert_eq!(
        GoogleAny::from_any(&corrupt_any),
        Err(QuasarError::UnexpectedEof)
    );

    // 3. Invalid UTF-8 in GoogleAny::type_url and SemanticVersionSpec tags
    let mut bad_any_str = Vec::new();
    encode_bytes_field(1, &[0xFF, 0xFE], &mut bad_any_str);
    assert!(matches!(GoogleAny::decode_proto(&bad_any_str), Err(QuasarError::InvalidUtf8(_))));

    let mut bad_semver_pre = Vec::new();
    encode_bytes_field(4, &[0xFF, 0xFE], &mut bad_semver_pre);
    assert!(matches!(SemanticVersionSpec::decode_proto(&bad_semver_pre), Err(QuasarError::InvalidUtf8(_))));

    let mut bad_semver_build = Vec::new();
    encode_bytes_field(5, &[0xFF, 0xFE], &mut bad_semver_build);
    assert!(matches!(SemanticVersionSpec::decode_proto(&bad_semver_build), Err(QuasarError::InvalidUtf8(_))));

    // 4. Corrupt nested messages in TraceContext, DeviceUserIdentifier, NetworkRttSample
    let mut bad_trace_flags = Vec::new();
    encode_bytes_field(3, &[0x08], &mut bad_trace_flags);
    assert_eq!(TraceContext::decode_proto(&bad_trace_flags), Err(QuasarError::UnexpectedEof));

    let mut bad_trace_state = Vec::new();
    encode_bytes_field(4, &[0x08], &mut bad_trace_state);
    assert_eq!(TraceContext::decode_proto(&bad_trace_state), Err(QuasarError::UnexpectedEof));

    let mut bad_dev_user = Vec::new();
    encode_bytes_field(2, &[0x08], &mut bad_dev_user);
    assert_eq!(DeviceUserIdentifier::decode_proto(&bad_dev_user), Err(QuasarError::UnexpectedEof));

    let mut bad_rtt = Vec::new();
    encode_bytes_field(2, &[0x08], &mut bad_rtt);
    assert_eq!(NetworkRttSample::decode_proto(&bad_rtt), Err(QuasarError::UnexpectedEof));

    // 5. MathVector3 & MathVector4: +0.0 omits wire bytes, -0.0 preserves sign bit
    let zero_v3 = MathVector3::default();
    assert!(zero_v3.encode_proto().is_empty());
    let neg_zero_v3 = MathVector3 { x: -0.0, y: -0.0, z: -0.0 };
    let enc_nz3 = neg_zero_v3.encode_proto();
    assert!(!enc_nz3.is_empty());
    let dec_nz3 = MathVector3::decode_proto(&enc_nz3).unwrap();
    assert!(dec_nz3.x.is_sign_negative() && dec_nz3.y.is_sign_negative() && dec_nz3.z.is_sign_negative());

    let zero_v4 = MathVector4::default();
    assert!(zero_v4.encode_proto().is_empty());
    let neg_zero_v4 = MathVector4 { x: -0.0, y: -0.0, z: -0.0, w: -0.0 };
    let enc_nz4 = neg_zero_v4.encode_proto();
    assert!(!enc_nz4.is_empty());
    let dec_nz4 = MathVector4::decode_proto(&enc_nz4).unwrap();
    assert!(dec_nz4.w.is_sign_negative());
}

