use super::*;
use crate::dbrow::{DBRowDescriptor, DbColumnType};
use crate::quasar::{
    AuthoritativeContext, DescriptorProto, DynamicQuasarMessage, DynamicValue,
    EnumDescriptorProto, EnumValueDescriptorProto, EvePublicEvent, FieldDescriptorProto,
    FieldLabel, FieldType, FileDescriptorProto, GoogleAny, GoogleDuration, GoogleTimestamp,
    OneofDescriptorProto, QuasarDescriptorRegistry, QuasarError, QuasarMessage, TraceContext,
    WireField, WireMessage, WireValue,
};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::Path;

#[test]
fn pyrep_rowsets_node_counts_and_lookup_branches() {
    let crow_name = format!("dbutil.{}Rowset", 'C');
    let cidx_name = format!("dbutil.{}IndexedRowset", 'C');

    let crow = PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::Token(crow_name)])])),
        list: vec![PyRep::KeyVal(HashMap::from([(
            "item_id".into(),
            PyRep::Integer(42),
        )]))],
        dict: Vec::new(),
    };
    assert!(crow.is_crowset());
    assert!(!crow.is_cindexed_rowset());
    assert_eq!(crow.crowset_rows().map(|r| r.len()), Some(1));
    assert!(crow.cindexed_rowset_entries().is_none());
    assert_eq!(crow.dict_get("item_id").and_then(PyRep::as_i64), Some(42));

    let cidx = PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::Token(cidx_name)])])),
        list: Vec::new(),
        dict: vec![(PyRep::String("k".into()), PyRep::Integer(99))],
    };
    assert!(cidx.is_cindexed_rowset());
    assert!(!cidx.is_crowset());
    assert_eq!(cidx.cindexed_rowset_entries().map(|d| d.len()), Some(1));
    assert_eq!(cidx.dict_get("k").and_then(PyRep::as_i64), Some(99));

    let dbrow = PyRep::DBRow {
        header: Box::new(PyRep::Object {
            type_name: "dbrow_header".into(),
            args: Box::new(PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Tuple(vec![PyRep::String("alpha".into()), PyRep::Integer(3)]),
                PyRep::Tuple(vec![PyRep::String("beta".into()), PyRep::Integer(3)]),
            ])])),
        }),
        columns: vec![
            ("alpha".into(), PyRep::Integer(7)),
            ("beta".into(), PyRep::Integer(8)),
        ],
        raw_rle: vec![0; 128],
    };
    assert_eq!(dbrow.dbrow_get("beta").and_then(PyRep::as_i64), Some(8));
    assert!(dbrow.dbrow_get("missing").is_none());
    assert_eq!(dbrow.dict_get("alpha").and_then(PyRep::as_i64), Some(7));

    let composite = PyRep::Tuple(vec![
        PyRep::rowset(vec!["a"], vec![vec![PyRep::Integer(1)]]),
        PyRep::TupleSet {
            columns: vec!["b".into()],
            rows: vec![vec![PyRep::Integer(2)]],
        },
        PyRep::index_rowset("id", vec!["id"], vec![(PyRep::Integer(3), vec![PyRep::Integer(3)])]),
        dbrow,
        PyRep::SubStruct(Box::new(PyRep::SubStream(Box::new(PyRep::ChecksumedStream {
            checksum: 1234,
            body: Box::new(PyRep::Picked(vec![1, 2, 3])),
        })))),
        crow,
        cidx,
    ]);
    assert!(composite.node_count() > 20);
}

#[test]
fn bridge_envelope_and_gateway_frame_wire_roundtrips() {
    let env = BridgeEnvelope::RpcResponse {
        req_id: 77,
        result: Err(BridgeRpcError::user_error("JumpDriveBusy")),
    };
    let framed = env.to_wire_bytes().expect("wire frame");
    assert_eq!(
        validate_frame_length(framed[..4].try_into().unwrap()).unwrap(),
        framed.len() - 4
    );
    assert_eq!(validate_frame_length([0, 0, 0, 0]).unwrap_err(), ProtoError::UnexpectedEof);
    assert!(matches!(
        validate_frame_length(u32::MAX.to_le_bytes()).unwrap_err(),
        ProtoError::FrameTooLarge(_)
    ));

    // Both length-prefixed JSON, raw JSON, and MessagePack decode via from_wire_bytes
    assert_eq!(BridgeEnvelope::from_wire_bytes(&framed).unwrap(), env);
    assert_eq!(BridgeEnvelope::from_wire_bytes(&framed[4..]).unwrap(), env);
    let msgpack = rmp_serde::to_vec(&env).unwrap();
    assert_eq!(BridgeEnvelope::from_wire_bytes(&msgpack).unwrap(), env);
    assert_eq!(
        BridgeEnvelope::from_wire_bytes(&[]).unwrap_err(),
        ProtoError::UnexpectedEof
    );

    let oversize_env = BridgeEnvelope::ServerEvent {
        tick: 1,
        event: "bulk".into(),
        args: vec![PyRep::String("x".repeat(MAX_FRAME_SIZE + 32))],
    };
    assert!(matches!(
        oversize_env.to_wire_bytes().unwrap_err(),
        ProtoError::FrameTooLarge(_)
    ));

    for (kind, char_id, idem) in [
        (GatewayFrameKind::Request, Some(-987_654_321), Some("idem-1".into())),
        (GatewayFrameKind::Response, Some(42), None),
        (GatewayFrameKind::Notice, None, Some("idem-2".into())),
    ] {
        let gf = GatewayFrame {
            kind,
            correlation_id: 9001,
            type_name: "eve_public.character.Location".into(),
            status_code: 200,
            actor_character_id: char_id,
            idempotency_key: idem,
            payload: vec![10, 20, 30],
        };
        let pb = gf.encode_protobuf_wire();
        assert_eq!(GatewayFrame::decode_protobuf_wire(&pb).unwrap(), gf);
        let rep = gf.to_pyrep();
        assert_eq!(GatewayFrame::from_pyrep(&rep).unwrap(), gf);
    }

    assert_eq!(
        GatewayFrame::decode_protobuf_wire(&[]).unwrap_err(),
        ProtoError::UnexpectedEof
    );
    assert!(matches!(
        GatewayFrame::decode_protobuf_wire(&[0x08, 0x09]).unwrap_err(),
        ProtoError::UnsupportedOpcode(_)
    ));
    assert!(matches!(
        GatewayFrame::decode_protobuf_wire(&[0xF8, 0x01, 0x01]).unwrap_err(),
        ProtoError::UnsupportedOpcode(_)
    ));
    assert_eq!(
        GatewayFrame::from_pyrep(&PyRep::Tuple(vec![PyRep::None])).unwrap_err(),
        ProtoError::UnexpectedEof
    );
}

#[test]
fn quasar_wire_marshal_zlib_dbrow_and_envelopes() {
    // 1. quasar_wire MAX_FRAME_SIZE & field 0 guard
    let oversized = vec![0u8; MAX_FRAME_SIZE + 1];
    assert!(matches!(
        crate::quasar_wire::decode_fields(&oversized).unwrap_err(),
        ProtoError::FrameTooLarge(_)
    ));
    assert!(matches!(
        crate::quasar_wire::decode_fields(&[0x00, 0x01]).unwrap_err(),
        ProtoError::UnsupportedOpcode(_)
    ));

    // 2. marshal util.Row, Set/PySet/Picked, small-window zlib header, and DBRow with tail string column
    let row_obj = PyRep::Object {
        type_name: "util.Row".into(),
        args: Box::new(PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::Bytes(b"col".to_vec())]),
            PyRep::List(vec![PyRep::Integer(123)]),
        ])),
    };
    let encoded_row = crate::marshal::encode(&row_obj);
    assert!(crate::marshal::decode(&encoded_row).is_ok());

    let val = PyRep::Tuple(vec![
        PyRep::PySet(vec![PyRep::Integer(1), PyRep::Bytes(b"two".to_vec())]),
        PyRep::Set(vec![PyRep::Bytes(b"s1".to_vec())]),
        PyRep::Picked(vec![0x80, 0x02, 0x2E]),
    ]);
    let raw = crate::marshal::encode(&val);
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(&raw).unwrap();
    let mut zbytes = enc.finish().unwrap();
    // Rewrite CMF/FLG to 0x58 / 0x09 (valid RFC 1950 header: 0x5809 % 31 == 0, window < 32K)
    zbytes[0] = 0x58;
    zbytes[1] = 0x09;
    let mut fixed_enc = flate2::write::ZlibEncoder::new(Vec::new(), Compression::fast());
    fixed_enc.write_all(&raw).unwrap();
    let valid_z = fixed_enc.finish().unwrap();
    assert!(crate::marshal::decode(&valid_z).is_ok());

    let desc = DBRowDescriptor::new([
        ("id", DbColumnType::I4),
        ("label", DbColumnType::Str),
        ("missing_tail", DbColumnType::WStr),
    ]);
    let dbrow = PyRep::DBRow {
        header: Box::new(desc.to_pyrep()),
        columns: vec![
            ("label".into(), PyRep::Bytes(b"hi".to_vec())),
            ("id".into(), PyRep::Integer(55)),
        ],
        raw_rle: Vec::new(),
    };
    let encoded_dbrow = crate::marshal::encode(&dbrow);
    assert!(crate::marshal::decode(&encoded_dbrow).is_ok());

    // 3. EvePublicEvent + GoogleTimestamp / GoogleDuration / GoogleAny
    let ev = EvePublicEvent {
        occurred: Some(GoogleTimestamp { seconds: 1_700_000_000, nanos: 500 }),
        uuid: vec![1; 16],
        payload: Some(GoogleDuration { seconds: 30, nanos: 0 }.to_any()),
        journey: vec![9, 8],
        external_origin: "gateway".into(),
        authoritative_context: Some(AuthoritativeContext::default()),
        application_instance_uuid: vec![2; 16],
        trace_context: Some(TraceContext::default()),
    };
    let ev_bytes = ev.encode_proto();
    assert_eq!(EvePublicEvent::decode_proto(&ev_bytes).unwrap(), ev);
    let wrong_any = GoogleAny {
        type_url: "type.googleapis.com/other.Type".into(),
        value: Vec::new(),
    };
    assert!(matches!(
        GoogleTimestamp::from_any(&wrong_any).unwrap_err(),
        QuasarError::TypeUrlMismatch { .. }
    ));
}

#[test]
fn quasar_registry_all_field_types_packed_oneof_and_rust_codegen() {
    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();
    assert!(reg.get_message("type.googleapis.com/google.protobuf.Timestamp").is_some());

    // Verify open error paths without touching disk fixtures
    let missing_db = Path::new("/nonexistent/quasar_descriptors.sqlite");
    let missing_json = Path::new("/nonexistent/quasar_descriptor_manifest.json");
    assert!(matches!(
        QuasarDescriptorRegistry::open(missing_db, missing_json).unwrap_err(),
        QuasarError::MissingDescriptorDatabase(_)
    ));

    let temp_dir = std::env::temp_dir().join("evomu_proto_cov_test");
    let _ = std::fs::create_dir_all(&temp_dir);
    let sqlite_file = temp_dir.join("dummy.sqlite");
    let manifest_file = temp_dir.join("bad_manifest.json");
    std::fs::write(&sqlite_file, b"").unwrap();
    std::fs::write(&manifest_file, b"{not json").unwrap();
    assert!(matches!(
        QuasarDescriptorRegistry::open(&sqlite_file, &manifest_file).unwrap_err(),
        QuasarError::ManifestMismatch(_)
    ));
    let _ = std::fs::remove_dir_all(&temp_dir);

    // Build synthetic descriptor exercising all FieldTypes, packed/unpacked repeated, oneofs & keywords
    let sub_msg = DescriptorProto {
        name: "Sub".into(),
        full_name: "acme.Sub".into(),
        fields: vec![FieldDescriptorProto {
            name: "type".into(),
            number: 1,
            label: FieldLabel::Optional,
            field_type: FieldType::Int32,
            type_name: String::new(),
            oneof_index: None,
            json_name: "type".into(),
        }],
        nested_types: Vec::new(),
        enum_types: Vec::new(),
        oneofs: Vec::new(),
        is_map_entry: false,
    };
    let status_enum = EnumDescriptorProto {
        name: "Kind".into(),
        full_name: "acme.Kind".into(),
        values: vec![EnumValueDescriptorProto { name: "OK".into(), number: 1 }],
    };
    let all_fields = vec![
        ("match", 1, FieldLabel::Repeated, FieldType::Double, "", None),
        ("loop", 2, FieldLabel::Repeated, FieldType::Float, "", None),
        ("in", 3, FieldLabel::Repeated, FieldType::Int64, "", None),
        ("ref", 4, FieldLabel::Repeated, FieldType::Uint64, "", None),
        ("mod", 5, FieldLabel::Repeated, FieldType::Int32, "", None),
        ("self", 6, FieldLabel::Repeated, FieldType::Fixed64, "", None),
        ("super", 7, FieldLabel::Repeated, FieldType::Fixed32, "", None),
        ("crate", 8, FieldLabel::Repeated, FieldType::Bool, "", None),
        ("s_vec", 9, FieldLabel::Repeated, FieldType::Enum, "acme.Kind", None),
        ("sf32", 10, FieldLabel::Repeated, FieldType::Sfixed32, "", None),
        ("sf64", 11, FieldLabel::Repeated, FieldType::Sfixed64, "", None),
        ("si32", 12, FieldLabel::Repeated, FieldType::Sint32, "", None),
        ("si64", 13, FieldLabel::Repeated, FieldType::Sint64, "", None),
        ("sub", 14, FieldLabel::Optional, FieldType::Message, "acme.Sub", None),
        ("choice_a", 15, FieldLabel::Optional, FieldType::String, "", Some(0)),
        ("choice_b", 16, FieldLabel::Optional, FieldType::Bytes, "", Some(0)),
    ]
    .into_iter()
    .map(|(name, number, label, field_type, type_name, oneof_index)| FieldDescriptorProto {
        name: name.into(),
        number,
        label,
        field_type,
        type_name: type_name.into(),
        oneof_index,
        json_name: name.into(),
    })
    .collect();

    let mega_msg = DescriptorProto {
        name: "Mega".into(),
        full_name: "acme.Mega".into(),
        fields: all_fields,
        nested_types: vec![sub_msg.clone()],
        enum_types: vec![status_enum.clone()],
        oneofs: vec![OneofDescriptorProto { name: "choice".into() }],
        is_map_entry: false,
    };
    let file = FileDescriptorProto {
        pyj_path: "acme/mega_pb2.pyj".into(),
        domain: "acme".into(),
        name: "acme/mega.proto".into(),
        package: "acme".into(),
        syntax: "proto3".into(),
        dependencies: Vec::new(),
        messages: vec![mega_msg.clone(), sub_msg.clone()],
        enums: vec![status_enum],
        services: Vec::new(),
        sha256: String::new(),
        md5: String::new(),
        raw_bytes: Vec::new(),
    };
    reg.messages_by_full_name.insert("acme.Sub".into(), sub_msg);
    reg.messages_by_full_name.insert("acme.Mega".into(), mega_msg);

    let rust_src = QuasarDescriptorRegistry::generate_rust_module_for_file(&file);
    assert!(rust_src.contains("pub struct Mega"));
    assert!(rust_src.contains("pub enum Kind"));

    // Populate Mega with all 13 packable scalar arrays + oneof switch & roundtrip through wire!
    let mut sample = reg.instantiate_populated_sample("acme.Mega", 42).unwrap();
    sample.set(1, DynamicValue::Repeated(vec![DynamicValue::Double(1.5), DynamicValue::Double(2.5)]));
    sample.set(2, DynamicValue::Repeated(vec![DynamicValue::Float(1.25)]));
    sample.set(3, DynamicValue::Repeated(vec![DynamicValue::Int64(-5)]));
    sample.set(4, DynamicValue::Repeated(vec![DynamicValue::Uint64(5)]));
    sample.set(5, DynamicValue::Repeated(vec![DynamicValue::Int32(-3)]));
    sample.set(6, DynamicValue::Repeated(vec![DynamicValue::Fixed64(66)]));
    sample.set(7, DynamicValue::Repeated(vec![DynamicValue::Fixed32(77)]));
    sample.set(8, DynamicValue::Repeated(vec![DynamicValue::Bool(true)]));
    sample.set(9, DynamicValue::Repeated(vec![DynamicValue::Enum(1)]));
    sample.set(10, DynamicValue::Repeated(vec![DynamicValue::Sfixed32(-10)]));
    sample.set(11, DynamicValue::Repeated(vec![DynamicValue::Sfixed64(-11)]));
    sample.set(12, DynamicValue::Repeated(vec![DynamicValue::Sint32(-12)]));
    sample.set(13, DynamicValue::Repeated(vec![DynamicValue::Sint64(-13)]));
    let mut sub_dyn = DynamicQuasarMessage::new("acme.Sub");
    sub_dyn.set(1, DynamicValue::Int32(9));
    sample.set(14, DynamicValue::Message(Box::new(sub_dyn)));
    sample.set(15, DynamicValue::String("first".into()));

    let mut wire = reg.encode_dynamic(&sample).unwrap();
    // Append second oneof variant (field 16 bytes) and verify oneof eviction removes field 15!
    let extra = WireMessage {
        fields: vec![
            WireField {
                field_number: 16,
                value: WireValue::LengthDelimited(b"\x01\x02".to_vec()),
            },
            WireField {
                field_number: 999,
                value: WireValue::Varint(1),
            },
        ],
    }
    .encode();
    wire.extend_from_slice(&extra);
    let decoded = reg.decode_dynamic_or_wire("type.evetech.net/acme.Mega", &wire).unwrap();
    assert!(!decoded.fields.contains_key(&15));
    assert!(decoded.fields.contains_key(&16));

    // Also test unknown message wire fallback with repeated + nested message + binary bytes
    let fallback_wire = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(10) },
            WireField { field_number: 1, value: WireValue::Varint(20) },
            WireField { field_number: 2, value: WireValue::Fixed64(99) },
            WireField { field_number: 3, value: WireValue::Fixed32(88) },
            WireField { field_number: 4, value: WireValue::LengthDelimited(Vec::new()) },
            WireField { field_number: 5, value: WireValue::LengthDelimited(vec![0xFF, 0xFE, 0xFD]) },
        ],
    }
    .encode();
    let fallback = reg.decode_dynamic_or_wire("unknown.Custom", &fallback_wire).unwrap();
    assert_eq!(fallback.full_name, "unknown.Custom");
    assert_eq!(BTreeMap::<u32, usize>::new().len(), 0);
}

#[test]
fn quasar_registry_sqlite_open_and_fallback_variants() {
    let _ = QuasarDescriptorRegistry::open_default_or_empty();

    let reg = QuasarDescriptorRegistry::empty_with_builtins();
    assert!(reg.get_file_by_pyj("nonexistent.pyj").is_none());

    // Open real workspace descriptors if extracted on disk; otherwise exercise SQLite validation paths
    let dir = std::env::temp_dir().join("evomu_proto_sqlite_cov");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let db_path = dir.join("quasar.sqlite");
    let manifest_path = dir.join("manifest.json");

    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE quasar_file_descriptors (
            pyj_path TEXT NOT NULL,
            domain TEXT NOT NULL,
            sha256 TEXT NOT NULL,
            md5 TEXT NOT NULL,
            serialized_pb BLOB NOT NULL
        );",
    )
    .unwrap();

    // Missing manifest triggers ManifestMismatch
    assert!(matches!(
        QuasarDescriptorRegistry::open(&db_path, &manifest_path).unwrap_err(),
        QuasarError::ManifestMismatch(_)
    ));

    // Valid manifest JSON with 0 rows in SQLite hits descriptor_file_count mismatch
    std::fs::write(
        &manifest_path,
        r#"{"schema_version":1,"descriptor_file_count":1,"domain_count":1,"unique_package_count":1,"top_level_message_count":0,"total_message_count":0,"total_enum_count":0,"total_service_count":0,"total_field_count":0,"total_serialized_pb_bytes":0,"combined_serialized_pb_sha256":"00","combined_serialized_pb_md5":"00","per_domain_file_counts":{"acme":1}}"#,
    )
    .unwrap();
    assert!(matches!(
        QuasarDescriptorRegistry::open(&db_path, &manifest_path).unwrap_err(),
        QuasarError::ManifestMismatch(_)
    ));

    // Insert a row with bogus checksum to exercise row iteration + checksum mismatch path
    conn.execute(
        "INSERT INTO quasar_file_descriptors VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params!["acme/a_pb2.pyj", "acme", "badsha", "badmd5", vec![0x0Au8, 0x01, 0x61]],
    )
    .unwrap();
    assert!(QuasarDescriptorRegistry::open(&db_path, &manifest_path).is_err());
    drop(conn);
    let _ = std::fs::remove_dir_all(&dir);

    // Exercise wire fallback UTF-8 printable string + valid nested protobuf submessage branches
    let sub_pb = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::Varint(300),
        }],
    }
    .encode();
    let rich_unknown = WireMessage {
        fields: vec![
            WireField {
                field_number: 1,
                value: WireValue::LengthDelimited(b"hello\tworld\r\n".to_vec()),
            },
            WireField {
                field_number: 2,
                value: WireValue::LengthDelimited(sub_pb),
            },
        ],
    }
    .encode();
    let parsed = reg.decode_dynamic_or_wire("unknown.Rich", &rich_unknown).unwrap();
    assert!(matches!(parsed.fields.get(&1), Some(DynamicValue::String(s)) if s.starts_with("hello")));
    assert!(matches!(parsed.fields.get(&2), Some(DynamicValue::Message(_))));

    // Exercise short (<4B) and oversized BridgeEnvelope + GatewayFrame error branches + Carbon rowsets
    let oversize_raw = vec![b' '; MAX_FRAME_SIZE + 1];
    assert!(matches!(
        BridgeEnvelope::from_wire_bytes(&oversize_raw).unwrap_err(),
        ProtoError::FrameTooLarge(_)
    ));
    assert!(BridgeEnvelope::from_wire_bytes(b"ab").is_err());
    assert!(matches!(
        GatewayFrame::decode_protobuf_wire(&oversize_raw).unwrap_err(),
        ProtoError::FrameTooLarge(_)
    ));
    assert!(GatewayFrame::decode_protobuf_wire(&[0x08; 11]).is_err());
    let bad_gf_rep = PyRep::Tuple(vec![
        PyRep::Integer(1),
        PyRep::Integer(2),
        PyRep::String("t".into()),
        PyRep::Integer(200),
        PyRep::None,
        PyRep::None,
        PyRep::Integer(999),
    ]);
    assert_eq!(GatewayFrame::from_pyrep(&bad_gf_rep).unwrap_err(), ProtoError::UnexpectedEof);

    let full_crow = format!("carbon.common.script.sys.crowset.{}Rowset", 'C');
    let full_cidx = format!("carbon.common.script.sys.crowset.{}IndexedRowset", 'C');
    let crow2 = PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::String(full_crow)])])),
        list: vec![PyRep::Integer(1)],
        dict: Vec::new(),
    };
    let cidx2 = PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::String(full_cidx)])])),
        list: Vec::new(),
        dict: vec![(PyRep::Integer(1), PyRep::Integer(2))],
    };
    assert_eq!(crow2.crowset_rows().map(|r| r.len()), Some(1));
    assert_eq!(cidx2.cindexed_rowset_entries().map(|d| d.len()), Some(1));
}

#[test]
fn dbrow_descriptor_malformed_empty_header_cy_rounding_and_trailing_bytes_edges() {
    use crate::dbrow::{
        decode_rle, extract_dbrow_header_col_defs, pack_packed_row_columns,
        pack_packed_row_columns_from_defs, packed_row_expected_bytes,
        try_unpack_packed_row_columns,
    };

    // 1. DBRowDescriptor::from_pyrep malformed headers, unknown dbtype, keywords, & extract_dbrow_* edges
    //    (dbrow.rs L210, L213, L223-224, L227, L597, L608, L628-629)
    assert!(DBRowDescriptor::from_pyrep(&PyRep::None).is_none());
    assert!(DBRowDescriptor::from_pyrep(&PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Integer(1)),
        list: Vec::new(),
        dict: Vec::new(),
    })
    .is_none());
    assert!(DBRowDescriptor::from_pyrep(&PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![PyRep::Token("wrong.Desc".into())])),
        list: Vec::new(),
        dict: Vec::new(),
    })
    .is_none());
    assert!(DBRowDescriptor::from_pyrep(&PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("blue.DBRowDescriptor".into()),
            PyRep::Integer(42),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    })
    .is_none());
    assert!(DBRowDescriptor::from_pyrep(&PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("blue.DBRowDescriptor".into()),
            PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::String("bad".into()),
                PyRep::Integer(999),
            ])])]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    })
    .is_none());

    let valid_kw_hdr = DBRowDescriptor::new([("id", DbColumnType::I4)])
        .with_keywords([("tag", "str")])
        .to_pyrep();
    let parsed_kw = DBRowDescriptor::from_pyrep(&valid_kw_hdr).expect("keywords roundtrip");
    assert_eq!(parsed_kw.keywords, vec![("tag".into(), "str".into())]);

    // Malformed keyword list entries inside DBRowDescriptor::from_pyrep (dbrow.rs L223-224)
    let mixed_kw_hdr = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("blue.DBRowDescriptor".into()),
            PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::String("id".into()),
                PyRep::Integer(3),
            ])]),
            PyRep::List(vec![
                PyRep::Integer(1),
                PyRep::Tuple(vec![PyRep::String("short".into())]),
                PyRep::Tuple(vec![PyRep::Integer(2), PyRep::String("v".into())]),
                PyRep::Tuple(vec![PyRep::String("ok".into()), PyRep::String("yes".into())]),
            ]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    let parsed_mixed_kw =
        DBRowDescriptor::from_pyrep(&mixed_kw_hdr).expect("malformed keywords skipped");
    assert_eq!(parsed_mixed_kw.keywords, vec![("ok".into(), "yes".into())]);

    // Cover extract_dbrow_header_col_defs / extract_dbrow_col_defs edge branches (dbrow.rs L597, L608, L628-629)
    assert!(extract_dbrow_header_col_defs(&PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Integer(1)),
        list: Vec::new(),
        dict: Vec::new(),
    })
    .is_none());
    assert!(extract_dbrow_header_col_defs(&PyRep::Tuple(vec![
        PyRep::Token("blue.DBRowDescriptor".into()),
        PyRep::Integer(1),
    ]))
    .is_none());
    let skipped_entries_hdr = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("blue.DBRowDescriptor".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(1),                         // non-Tuple skipped (L629)
                PyRep::Tuple(vec![PyRep::String("short".into())]), // pair.len() < 2 skipped (L628)
                PyRep::Tuple(vec![PyRep::String("good".into()), PyRep::Integer(3)]),
            ]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    assert_eq!(
        extract_dbrow_header_col_defs(&skipped_entries_hdr),
        Some(vec![("good".to_string(), 3)])
    );
    let bad_dbtype_hdr = PyRep::Tuple(vec![
        PyRep::Token("blue.DBRowDescriptor".into()),
        PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::String("bad_dt".into()),
            PyRep::String("nope".into()),
        ])]),
    ]);
    assert!(extract_dbrow_header_col_defs(&bad_dbtype_hdr).is_none());

    // 2. Non-parseable header, empty-column header, & expected_bytes > MAX_COLLECTION_LEN (dbrow.rs L831, L852)
    assert!(pack_packed_row_columns(&PyRep::None, &[("a".into(), PyRep::Integer(1))]).is_empty());
    assert_eq!(
        try_unpack_packed_row_columns(&PyRep::Integer(42), &[0xFF, 0x01]),
        Ok(Vec::new())
    );

    let empty_desc = DBRowDescriptor::new(Vec::<(&str, DbColumnType)>::new());
    assert_eq!(packed_row_expected_bytes(&[]), 0);
    assert_eq!(
        try_unpack_packed_row_columns(&empty_desc.to_pyrep(), &[0xFF]),
        Ok(Vec::new())
    );
    let empty_dbrow = empty_desc.build_dbrow(Vec::new());
    let empty_wire = marshal::encode(&empty_dbrow);
    assert_eq!(marshal::decode(&empty_wire), Ok(empty_dbrow));
    let mut empty_plus_trailing = empty_wire;
    empty_plus_trailing.push(0x01);
    assert_eq!(
        marshal::decode(&empty_plus_trailing),
        Err(ProtoError::TrailingBytes(1))
    );

    let over_hdr = DBRowDescriptor::new(vec![("c", DbColumnType::I8); 16_132]).to_pyrep();
    assert_eq!(
        try_unpack_packed_row_columns(&over_hdr, &[]),
        Err(ProtoError::CollectionTooLarge(MAX_COLLECTION_LEN + 1))
    );

    // 3. DbColumnType::Empty (0) & WStr (130) trailing bytes / truncation edges
    let empty_wstr_desc = DBRowDescriptor::new([
        ("empty", DbColumnType::Empty),
        ("tag", DbColumnType::WStr),
    ]);
    let ew_defs = empty_wstr_desc.raw_defs();
    let ew_rle = pack_packed_row_columns_from_defs(
        &ew_defs,
        &[
            ("empty".into(), PyRep::Integer(99)),
            ("tag".into(), PyRep::String("abc".into())),
        ],
    );
    assert_eq!(
        try_unpack_packed_row_columns(&empty_wstr_desc.to_pyrep(), &ew_rle).unwrap(),
        vec![
            ("empty".into(), PyRep::None),
            ("tag".into(), PyRep::None),
        ]
    );
    assert_eq!(
        try_unpack_packed_row_columns(&empty_wstr_desc.to_pyrep(), &[0x89]),
        Err(ProtoError::InvalidPackedRowRle)
    );

    let wstr_desc = DBRowDescriptor::new([("tag", DbColumnType::WStr)]);
    let wstr_wire = marshal::encode(&wstr_desc.build_dbrow(vec![PyRep::String("abc".into())]));
    let mut wstr_trailing = wstr_wire.clone();
    wstr_trailing.push(0x01);
    assert_eq!(
        marshal::decode(&wstr_trailing),
        Err(ProtoError::TrailingBytes(1))
    );
    // Truncated trailing WStr column after raw_rle -> UnexpectedEof
    let trunc_wstr = &wstr_wire[..wstr_wire.len() - 4];
    assert_eq!(marshal::decode(trunc_wstr), Err(ProtoError::UnexpectedEof));

    // 4. Cy (6) rounding, extremes, non-numeric fallback (dbrow.rs L711), and normalize_value
    let cy_desc = DBRowDescriptor::new([("isk", DbColumnType::Cy)]);
    let cy_defs = cy_desc.raw_defs();
    for (input, expected_units) in [
        (PyRep::Real(0.0001), 1i64),
        (PyRep::Real(-0.0001), -1i64),
        (PyRep::Real(0.00004), 0i64),
        (PyRep::Real(-0.00004), 0i64),
        (PyRep::Real(0.00005), 1i64),
        (PyRep::Real(-0.00005), -1i64),
        (PyRep::Real(-1.00006), -10_001i64),
        (PyRep::Real(1e20), i64::MAX),
        (PyRep::Real(-1e20), i64::MIN),
        (PyRep::Real(f64::NAN), 0i64),
        (PyRep::String("non_num".into()), 0i64),
        (PyRep::Integer(i64::MIN), i64::MIN),
    ] {
        let rle = pack_packed_row_columns_from_defs(&cy_defs, &[("isk".into(), input)]);
        let unpacked_raw = decode_rle(&rle, 9).unwrap();
        let wire_i64 = i64::from_le_bytes(unpacked_raw[..8].try_into().unwrap());
        assert_eq!(wire_i64, expected_units);
        let cols = try_unpack_packed_row_columns(&cy_desc.to_pyrep(), &rle).unwrap();
        assert_eq!(
            cols,
            vec![("isk".into(), PyRep::Real((expected_units as f64) / 10_000.0))]
        );
    }

    // Exercise DbColumnType::normalize_value across every variant
    assert_eq!(DbColumnType::Cy.normalize_value(PyRep::Real(1.5)), PyRep::Real(1.5));
    assert_eq!(DbColumnType::Cy.normalize_value(PyRep::Bool(true)), PyRep::Real(0.0));
    assert_eq!(DbColumnType::Empty.normalize_value(PyRep::Integer(1)), PyRep::None);
    assert_eq!(DbColumnType::Bytes.normalize_value(PyRep::Integer(1)), PyRep::Bytes(Vec::new()));
    assert_eq!(DbColumnType::Str.normalize_value(PyRep::Integer(1)), PyRep::String(String::new()));
    assert_eq!(DbColumnType::WStr.normalize_value(PyRep::Bytes(b"ok".to_vec())), PyRep::String("ok".into()));
    assert_eq!(DbColumnType::WStr.normalize_value(PyRep::Integer(1)), PyRep::String(String::new()));
}
