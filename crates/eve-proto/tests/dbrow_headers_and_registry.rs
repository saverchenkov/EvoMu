#![forbid(unsafe_code)]

use eve_proto::dbrow::{DBRowDescriptor, DbColumnType};
use eve_proto::marshal::{decode, encode, encode_zlib, pack_dbrow};
use eve_proto::quasar::*;
use eve_proto::{validate_frame_length, BridgeRpcError, PyRep};
use std::collections::BTreeMap;

#[test]
fn test_dbrow_column_types_all_headers_and_pyrep_helpers() {
    // Exercise every DbColumnType discriminant + unknown fallback
    let known_codes = [
        0i64, 2, 3, 4, 5, 6, 11, 16, 17, 18, 19, 20, 21, 64, 128, 129, 130,
    ];
    for code in known_codes {
        let col_ty = DbColumnType::from_i64(code).expect("valid DbColumnType");
        assert_eq!(col_ty.as_i64(), code);
        let _ = col_ty.bit_width();
        let norm_none = col_ty.normalize_value(PyRep::None);
        assert_eq!(norm_none, PyRep::None);
        let norm_int = col_ty.normalize_value(PyRep::Integer(42));
        let _ = norm_int.node_count();
    }
    assert_eq!(DbColumnType::from_i64(255), None);
    assert_eq!(
        DbColumnType::R4.normalize_value(PyRep::Real(1.5)),
        PyRep::Real(1.5)
    );
    assert_eq!(
        DbColumnType::R8.normalize_value(PyRep::Real(2.5)),
        PyRep::Real(2.5)
    );
    assert_eq!(
        DbColumnType::Bool.normalize_value(PyRep::Bool(true)),
        PyRep::Bool(true)
    );
    assert_eq!(
        DbColumnType::Bytes.normalize_value(PyRep::Bytes(vec![1, 2])),
        PyRep::Bytes(vec![1, 2])
    );
    assert_eq!(
        DbColumnType::Str.normalize_value(PyRep::String("abc".into())),
        PyRep::String("abc".into())
    );
    assert_eq!(
        DbColumnType::WStr.normalize_value(PyRep::String("xyz".into())),
        PyRep::String("xyz".into())
    );

    // Exercise all 10 standard DBRowDescriptor headers & roundtrip
    let headers = [
        DBRowDescriptor::inv_item_header(),
        DBRowDescriptor::character_select_list_header(),
        DBRowDescriptor::market_order_header(),
        DBRowDescriptor::market_summary_header(),
        DBRowDescriptor::market_history_header(),
        DBRowDescriptor::notification_header(),
        DBRowDescriptor::inv_type_header(),
        DBRowDescriptor::corp_ticker_header(),
        DBRowDescriptor::graphic_header(),
        DBRowDescriptor::map_objects_header(),
    ];
    for hdr in headers {
        let with_kw = hdr.clone().with_keywords([("version", "1")]);
        assert!(!with_kw.column_names().is_empty());
        assert_eq!(with_kw.raw_defs().len(), with_kw.columns.len());
        let rep = with_kw.to_pyrep();
        let parsed = DBRowDescriptor::from_pyrep(&rep).expect("roundtrip header");
        assert_eq!(parsed.columns, hdr.columns);
        let row = hdr.build_dbrow(vec![PyRep::Integer(7)]);
        assert!(row.dbrow_columns().is_some());
    }
    assert!(DBRowDescriptor::from_pyrep(&PyRep::None).is_none());

    // pack_dbrow across all numeric/string/bool/float/bytes column types
    let packed = pack_dbrow(&[
        ("c_i8", 20, PyRep::Integer(1_000_000_000)),
        ("c_ui8", 21, PyRep::Integer(2_000_000_000)),
        ("c_filetime", 64, PyRep::Integer(133_000_000_000_000_000)),
        ("c_cy", 6, PyRep::Integer(500_000)),
        ("c_r8", 5, PyRep::Real(3.14)),
        ("c_r4", 4, PyRep::Real(1.25)),
        ("c_i4", 3, PyRep::Integer(-99)),
        ("c_ui4", 19, PyRep::Integer(99)),
        ("c_i2", 2, PyRep::Integer(-7)),
        ("c_ui2", 18, PyRep::Integer(7)),
        ("c_i1", 16, PyRep::Integer(-3)),
        ("c_ui1", 17, PyRep::Integer(3)),
        ("c_bool", 11, PyRep::Bool(true)),
        ("c_bytes", 128, PyRep::Bytes(vec![1, 2, 3])),
        ("c_str", 129, PyRep::String("hello".into())),
        ("c_wstr", 130, PyRep::String("world".into())),
    ]);
    assert!(packed.node_count() >= 1);

    // PyRep helpers: dict_get, cindexed_rowset, crowset_rows, row_count, node_count
    let dict_rep = PyRep::Dict(vec![
        (PyRep::ascii_bytes("alpha"), PyRep::Integer(11)),
        (PyRep::String("beta".into()), PyRep::Integer(22)),
    ]);
    assert_eq!(dict_rep.dict_get("alpha"), Some(&PyRep::Integer(11)));
    assert_eq!(dict_rep.dict_get("beta"), Some(&PyRep::Integer(22)));
    assert_eq!(dict_rep.dict_get("missing"), None);
    assert_eq!(PyRep::Integer(1).dict_get("alpha"), None);

    let objex = PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![PyRep::Token("test.Class".into())])),
        list: vec![PyRep::Integer(1)],
        dict: vec![(PyRep::String("k".into()), PyRep::Integer(99))],
    };
    assert_eq!(objex.dict_get("k"), Some(&PyRep::Integer(99)));
    assert!(objex.crowset_rows().is_none());
    assert!(objex.cindexed_rowset_entries().is_none());

    let hdr = DBRowDescriptor::inv_type_header();
    let ci = PyRep::cindexed_rowset(
        &hdr,
        "typeID",
        vec![vec![
            PyRep::Integer(587),
            PyRep::Integer(25),
            PyRep::String("Rifter".into()),
        ]],
    );
    assert!(ci.is_cindexed_rowset());
    assert_eq!(ci.cindexed_rowset_entries().map(|e| e.len()), Some(1));
    assert_eq!(ci.row_count(), Some(1));

    let tset = PyRep::tuple_set(vec!["a", "b"], vec![vec![PyRep::Integer(1), PyRep::Integer(2)]]);
    assert_eq!(tset.row_count(), Some(1));
    assert!(tset.node_count() >= 3);

    // Frame length & BridgeRpcError constructors
    assert_eq!(validate_frame_length([16, 0, 0, 0]).unwrap(), 16);
    assert!(validate_frame_length([0, 0, 0, 0]).is_err());
    assert!(validate_frame_length([0, 0, 0, 0x10]).is_err());
    let uerr = BridgeRpcError::user_error("CustomErrorCode");
    assert_eq!(uerr.code, "CustomErrorCode");

    // Marshal roundtrip on PySet, Set, Object
    for rep in [
        PyRep::PySet(vec![PyRep::Integer(1), PyRep::String("a".into())]),
        PyRep::Set(vec![PyRep::Integer(2)]),
        PyRep::Object {
            type_name: "util.KeyVal".into(),
            args: Box::new(PyRep::keyval([("x", PyRep::Integer(10))])),
        },
    ] {
        let bytes = encode(&rep);
        let back = decode(&bytes).expect("decode");
        assert!(back.node_count() >= 1);
        let z = encode_zlib(&rep).expect("zlib");
        assert!(!z.is_empty());
    }
}

#[test]
fn test_quasar_registry_builtins_open_errors_and_packed_fields() {
    let reg = QuasarDescriptorRegistry::empty_with_builtins();
    assert!(!reg.messages_by_full_name.is_empty());

    // Error paths on open()
    let missing_dir = std::env::temp_dir().join("evomu_quasar_missing_dir_xyz");
    let _ = std::fs::create_dir_all(&missing_dir);
    let manifest_p = missing_dir.join("manifest.json");
    std::fs::write(&manifest_p, "{not valid json").unwrap();
    let sqlite_p = missing_dir.join("desc.sqlite");
    std::fs::write(&sqlite_p, b"not a sqlite db").unwrap();
    assert!(matches!(
        QuasarDescriptorRegistry::open(&sqlite_p, &manifest_p),
        Err(QuasarError::ManifestMismatch(_))
    ));
    let _ = std::fs::remove_dir_all(&missing_dir);

    // Packed repeated scalar fields across all wire types
    fn wf_str(n: u32, s: &str) -> WireField {
        WireField {
            field_number: n,
            value: WireValue::LengthDelimited(s.as_bytes().to_vec()),
        }
    }
    fn wf_var(n: u32, v: u64) -> WireField {
        WireField {
            field_number: n,
            value: WireValue::Varint(v),
        }
    }
    fn wf_bytes(n: u32, b: Vec<u8>) -> WireField {
        WireField {
            field_number: n,
            value: WireValue::LengthDelimited(b),
        }
    }
    let field_spec = |num: u32, name: &str, ftype: FieldType| -> Vec<u8> {
        WireMessage {
            fields: vec![
                wf_str(1, name),
                wf_var(3, num as u64),
                wf_var(4, FieldLabel::Repeated as i32 as u64),
                wf_var(5, ftype as i32 as u64),
                wf_str(10, name),
            ],
        }
        .encode()
    };
    let packed_types = [
        (1u32, "r_u64", FieldType::Uint64),
        (2, "r_i64", FieldType::Int64),
        (3, "r_i32", FieldType::Int32),
        (4, "r_si32", FieldType::Sint32),
        (5, "r_si64", FieldType::Sint64),
        (6, "r_f32", FieldType::Fixed32),
        (7, "r_f64", FieldType::Fixed64),
        (8, "r_sf32", FieldType::Sfixed32),
        (9, "r_sf64", FieldType::Sfixed64),
        (10, "r_float", FieldType::Float),
        (11, "r_double", FieldType::Double),
        (12, "r_bool", FieldType::Bool),
    ];
    let mut mfields = vec![wf_str(1, "PackedMsg")];
    for (num, name, ft) in packed_types {
        mfields.push(wf_bytes(2, field_spec(num, name, ft)));
    }
    let file_bytes = WireMessage {
        fields: vec![
            wf_str(1, "acme/packed.proto"),
            wf_str(2, "acme"),
            wf_bytes(4, WireMessage { fields: mfields }.encode()),
            wf_str(12, "proto3"),
        ],
    }
    .encode();
    let fdp = FileDescriptorProto::decode_wire("acme/packed.pyj", "acme", file_bytes).unwrap();
    let top = fdp.messages[0].clone();
    let preg = QuasarDescriptorRegistry {
        manifest: QuasarManifest {
            schema_version: 1,
            domain_count: 1,
            descriptor_file_count: 1,
            unique_package_count: 1,
            top_level_message_count: 1,
            total_message_count: 1,
            total_enum_count: 0,
            total_service_count: 0,
            total_field_count: 12,
            total_serialized_pb_bytes: fdp.raw_bytes.len(),
            combined_serialized_pb_sha256: fdp.sha256.clone(),
            combined_serialized_pb_md5: fdp.md5.clone(),
            per_domain_file_counts: BTreeMap::from([("acme".into(), 1)]),
        },
        files: vec![fdp],
        files_by_pyj_path: [("acme/packed.pyj".into(), 0)].into(),
        files_by_proto_name: [("acme/packed.proto".into(), 0)].into(),
        files_by_domain: BTreeMap::from([("acme".into(), vec![0])]),
        messages_by_full_name: [(top.full_name.clone(), top)].into(),
        enums_by_full_name: Default::default(),
        services_by_full_name: Default::default(),
    };
    let sample = preg.instantiate_populated_sample("acme.PackedMsg", 3).unwrap();
    let wire = preg.encode_dynamic(&sample).unwrap();
    let back = preg.decode_dynamic("acme.PackedMsg", &wire).unwrap();
    assert_eq!(back.fields.len(), sample.fields.len());
}

#[test]
fn test_pyrep_accessors_rowsets_null_dbrow_and_objectex() {
    use eve_proto::marshal;

    // 1. Scalar & helper accessors across matching and non-matching variants
    assert_eq!(PyRep::String("hello".into()).as_str(), Some("hello"));
    assert_eq!(PyRep::Token("tok".into()).as_str(), Some("tok"));
    assert_eq!(PyRep::ascii_bytes(b"raw").as_str(), Some("raw"));
    assert_eq!(PyRep::Bytes(vec![0xff, 0xfe]).as_str(), None);
    assert_eq!(PyRep::Integer(42).as_str(), None);

    assert_eq!(PyRep::Integer(-9).as_i64(), Some(-9));
    assert_eq!(PyRep::Bool(true).as_i64(), Some(1));
    assert_eq!(PyRep::Bool(false).as_i64(), Some(0));
    assert_eq!(PyRep::String("no".into()).as_i64(), None);

    assert!((PyRep::Real(3.5).as_f64().unwrap() - 3.5).abs() < 1e-12);
    assert!((PyRep::Integer(7).as_f64().unwrap() - 7.0).abs() < 1e-12);
    assert_eq!(PyRep::Bool(true).as_f64(), None);

    assert_eq!(PyRep::Bool(true).as_bool(), Some(true));
    assert_eq!(PyRep::Integer(5).as_bool(), Some(true));
    assert_eq!(PyRep::Integer(0).as_bool(), Some(false));
    assert_eq!(PyRep::None.as_bool(), None);

    // 2. Crowset, IndexRowset, TupleSet, SubStream, ChecksumedStream node counts & accessors
    let hdr = DBRowDescriptor::new(vec![
        ("id", DbColumnType::I4),
        ("name", DbColumnType::Str),
        ("raw", DbColumnType::Bytes),
        ("when", DbColumnType::FileTime),
        ("flag", DbColumnType::Bool),
        ("isk", DbColumnType::Cy),
        ("ratio", DbColumnType::R4),
        ("exact", DbColumnType::R8),
        ("u1", DbColumnType::Ui1),
        ("i1", DbColumnType::I1),
        ("u2", DbColumnType::Ui2),
        ("i2", DbColumnType::I2),
        ("u4", DbColumnType::Ui4),
        ("u8", DbColumnType::Ui8),
        ("wname", DbColumnType::WStr),
    ]);
    let null_row_vals = vec![PyRep::None; 15];
    let pop_row_vals = vec![
        PyRep::Integer(10),
        PyRep::String("Alpha".into()),
        PyRep::Bytes(vec![1, 2, 3]),
        PyRep::Integer(133_000_000_000_000_000),
        PyRep::Bool(true),
        PyRep::Integer(50_000),
        PyRep::Real(1.25),
        PyRep::Real(9.875),
        PyRep::Integer(200),
        PyRep::Integer(-12),
        PyRep::Integer(40_000),
        PyRep::Integer(-300),
        PyRep::Integer(3_000_000_000),
        PyRep::Integer(9_000_000),
        PyRep::String("Beta".into()),
    ];
    let null_pack_cols: Vec<(&str, i64, PyRep)> = hdr
        .columns
        .iter()
        .map(|(n, c)| (n.as_str(), c.as_i64(), PyRep::None))
        .collect();
    let packed_null = pack_dbrow(&null_pack_cols);
    assert!(packed_null.dbrow_get("id").is_some());
    assert!(packed_null.dbrow_get("missing_col").is_none());
    assert!(packed_null.node_count() >= 10);

    let crow = PyRep::crowset(&hdr, vec![null_row_vals, pop_row_vals]);
    assert!(crow.is_crowset());
    assert_eq!(crow.crowset_rows().map(|r| r.len()), Some(2));
    assert!(PyRep::None.crowset_rows().is_none());
    assert!(PyRep::None.cindexed_rowset_entries().is_none());

    let crow_roundtrip = marshal::decode(&marshal::encode(&crow)).unwrap();
    assert_eq!(crow_roundtrip, crow);
    assert!(crow_roundtrip.is_crowset());
    let rows = crow_roundtrip.crowset_rows().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].dbrow_get("id"), Some(&PyRep::None));
    assert_eq!(rows[0].dbrow_get("flag"), Some(&PyRep::None));
    assert_eq!(rows[0].dbrow_get("isk"), Some(&PyRep::None));
    assert_eq!(rows[0].dbrow_get("name"), Some(&PyRep::None));
    assert_eq!(rows[0].dbrow_get("raw"), Some(&PyRep::None));
    assert_eq!(rows[1].dbrow_get("id"), Some(&PyRep::Integer(10)));
    assert_eq!(rows[1].dbrow_get("isk"), Some(&PyRep::Real(50_000.0)));
    assert_eq!(rows[1].dbrow_get("u1"), Some(&PyRep::Integer(200)));
    assert_eq!(rows[1].dbrow_get("i1"), Some(&PyRep::Integer(-12)));
    assert_eq!(rows[1].dbrow_get("u2"), Some(&PyRep::Integer(40_000)));
    assert_eq!(rows[1].dbrow_get("u4"), Some(&PyRep::Integer(3_000_000_000)));
    assert_eq!(rows[1].dbrow_get("name"), Some(&PyRep::String("Alpha".into())));

    let rs = PyRep::rowset(vec!["a", "b"], vec![vec![PyRep::Integer(1), PyRep::Bool(true)]]);
    assert_eq!(rs.node_count(), 5);
    assert_eq!(PyRep::tuple_set(vec!["a"], vec![vec![PyRep::Integer(2)]]).node_count(), 3);
    assert_eq!(
        PyRep::index_rowset("a", vec!["a", "b"], vec![(PyRep::Integer(1), vec![PyRep::Integer(1), PyRep::None])])
            .node_count(),
        7
    );
    assert!(PyRep::ChecksumedStream { checksum: 7, body: Box::new(rs) }.node_count() >= 6);
    assert!(PyRep::SubStream(Box::new(PyRep::Buffer(vec![0u8; 128]))).node_count() >= 4);
    assert_eq!(PyRep::Picked(vec![0u8; 128]).node_count(), 3);

    // 3. V24 new-style `util.Row` -> `carbon.common.script.sys.row.Row` ObjectEx2 roundtrip & general ObjectEx1/2
    let util_row = PyRep::Object {
        type_name: "util.Row".into(),
        args: Box::new(PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::String("k".into()), PyRep::String("v".into())]),
            PyRep::List(vec![PyRep::Integer(77), PyRep::String("val".into())]),
        ])),
    };
    let util_row_rt = marshal::decode(&marshal::encode(&util_row)).unwrap();
    assert_eq!(util_row_rt, util_row);

    let custom_objex1 = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![PyRep::Token("custom.Class".into())])),
        list: vec![PyRep::Integer(1), PyRep::String("x".into())],
        dict: vec![(PyRep::String("m".into()), PyRep::Integer(2))],
    };
    assert_eq!(
        marshal::decode(&marshal::encode(&custom_objex1)).unwrap(),
        custom_objex1
    );
}

