//! Unit and mutation-killing tests for `eve_proto::marshal`.

use super::*;
use crate::{MarshalOpcode, ProtoError, PyRep, MARSHAL_HEADER_BYTE, MARSHAL_STRING_TABLE, SAVE_FLAG};

#[test]
fn integer_real_bytes_string_and_container_boundaries_roundtrip() {
    // Integer opcode boundaries: -1, 0, 1, i8, i16, i32, i64
    let ints = [
        (-1, MarshalOpcode::PyMinusOne as u8),
        (0, MarshalOpcode::PyZeroInteger as u8),
        (1, MarshalOpcode::PyOneInteger as u8),
        (-128, MarshalOpcode::PyByte as u8),
        (-2, MarshalOpcode::PyByte as u8),
        (2, MarshalOpcode::PyByte as u8),
        (127, MarshalOpcode::PyByte as u8),
        (-32_768, MarshalOpcode::PySignedShort as u8),
        (-129, MarshalOpcode::PySignedShort as u8),
        (128, MarshalOpcode::PySignedShort as u8),
        (32_767, MarshalOpcode::PySignedShort as u8),
        (i32::MIN as i64, MarshalOpcode::PyLong as u8),
        (-32_769, MarshalOpcode::PyLong as u8),
        (32_768, MarshalOpcode::PyLong as u8),
        (i32::MAX as i64, MarshalOpcode::PyLong as u8),
        (i32::MIN as i64 - 1, MarshalOpcode::PyLongLong as u8),
        (i32::MAX as i64 + 1, MarshalOpcode::PyLongLong as u8),
        (i64::MIN, MarshalOpcode::PyLongLong as u8),
        (i64::MAX, MarshalOpcode::PyLongLong as u8),
    ];
    for (val, expected_op) in ints {
        let rep = PyRep::Integer(val);
        let enc = encode(&rep);
        assert_eq!(enc[5], expected_op, "unexpected opcode for {val}");
        assert_eq!(decode(&enc).unwrap(), rep);
    }

    // Real 0.0 vs -0.0 vs non-zero
    let pos_zero = encode(&PyRep::Real(0.0));
    assert_eq!(pos_zero[5], MarshalOpcode::PyZeroReal as u8);
    assert_eq!(decode(&pos_zero).unwrap(), PyRep::Real(0.0));

    let neg_zero = encode(&PyRep::Real(-0.0));
    assert_eq!(neg_zero[5], MarshalOpcode::PyReal as u8);
    let dec_neg_zero = decode(&neg_zero).unwrap();
    let PyRep::Real(nz) = dec_neg_zero else {
        panic!("expected Real");
    };
    assert!(nz == 0.0 && nz.is_sign_negative());

    // Bytes boundaries: 0, 1, 2, 255 (PyShortString), 256 (PyLongString)
    for (len, expected_op) in [
        (0usize, MarshalOpcode::PyEmptyString as u8),
        (1, MarshalOpcode::PyCharString as u8),
        (2, MarshalOpcode::PyShortString as u8),
        (254, MarshalOpcode::PyShortString as u8),
        (255, MarshalOpcode::PyShortString as u8),
        (256, MarshalOpcode::PyLongString as u8),
    ] {
        let b = PyRep::Bytes(vec![0x5A; len]);
        let enc = encode(&b);
        assert_eq!(enc[5], expected_op, "unexpected bytes opcode for len {len}");
        assert_eq!(decode(&enc).unwrap(), b);
    }

    // Strings: empty, short, and >= 255 (size_ex 0xFF)
    assert_eq!(
        encode(&PyRep::String(String::new()))[5],
        MarshalOpcode::PyEmptyWString as u8
    );
    let long_str = PyRep::String("A".repeat(300));
    assert_eq!(decode(&encode(&long_str)).unwrap(), long_str);

    // Tuples (0, 1, 2, 3) and Lists (0, 1, 2)
    for n in 0..=3 {
        let t = PyRep::Tuple(vec![PyRep::Integer(1); n]);
        assert_eq!(decode(&encode(&t)).unwrap(), t);
        let l = PyRep::List(vec![PyRep::Integer(2); n]);
        assert_eq!(decode(&encode(&l)).unwrap(), l);
    }

    // Buffer, Picked, SubStruct, ChecksumedStream, SubStream
    let composite = PyRep::Tuple(vec![
        PyRep::Buffer(vec![1, 2, 3]),
        PyRep::Picked(vec![0x80, 0x02, b'.']),
        PyRep::SubStruct(Box::new(PyRep::Bool(false))),
        PyRep::ChecksumedStream {
            checksum: 0xDEAD_BEEF,
            body: Box::new(PyRep::Integer(7)),
        },
        PyRep::SubStream(Box::new(PyRep::Token("sub.Token".into()))),
    ]);
    assert_eq!(decode(&encode(&composite)).unwrap(), composite);
}

#[test]
fn high_level_projections_and_object_variants_roundtrip() {
    // IndexRowset, TupleSet, PySet, Set, util.Row
    let idx_rs = PyRep::index_rowset(
        "itemID",
        vec!["itemID", "typeID"],
        vec![(
            PyRep::Integer(100),
            vec![PyRep::Integer(100), PyRep::Integer(34)],
        )],
    );
    assert_eq!(decode(&encode(&idx_rs)).unwrap(), idx_rs);

    let ts = PyRep::tuple_set(
        vec!["colA", "colB"],
        vec![vec![PyRep::Integer(1), PyRep::String("x".into())]],
    );
    assert_eq!(
        decode(&encode(&ts)).unwrap(),
        PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::String("colA".into()), PyRep::String("colB".into())]),
            PyRep::List(vec![PyRep::List(vec![
                PyRep::Integer(1),
                PyRep::String("x".into()),
            ])]),
        ])
    );

    let pyset = PyRep::PySet(vec![PyRep::Integer(10), PyRep::Integer(20)]);
    assert_eq!(decode(&encode(&pyset)).unwrap(), pyset);
    let set_alias = PyRep::Set(vec![PyRep::Integer(10), PyRep::Integer(20)]);
    assert_eq!(decode(&encode(&set_alias)).unwrap(), pyset);

    let util_row = PyRep::Object {
        type_name: "util.Row".into(),
        args: Box::new(PyRep::Dict(vec![(
            PyRep::String("line".into()),
            PyRep::List(vec![PyRep::Integer(1)]),
        )])),
    };
    let enc_row = encode(&util_row);
    assert_eq!(enc_row[5], MarshalOpcode::PyObjectEx2 as u8);
    assert_eq!(decode(&enc_row).unwrap(), util_row);

    // DBRow with empty columns uses raw_rle directly
    let hdr = crate::DBRowDescriptor::new([("id", crate::DbColumnType::I4)]).to_pyrep();
    let raw_dbrow = PyRep::DBRow {
        header: Box::new(hdr),
        columns: Vec::new(),
        raw_rle: Vec::new(),
    };
    let dec_dbrow = decode(&encode(&raw_dbrow)).unwrap();
    assert_eq!(dec_dbrow.dbrow_get("id"), Some(&PyRep::Integer(0)));
}

#[test]
fn decoder_legacy_opcodes_varint_string_table_ucs2_and_object_fallbacks() {
    // PyVarInteger: 0-byte, positive 2-byte, negative sign-extended 1-byte and 2-byte
    let make_frame = |body: &[u8]| -> Vec<u8> {
        let mut f = vec![MARSHAL_HEADER_BYTE, 0, 0, 0, 0];
        f.extend_from_slice(body);
        f
    };

    assert_eq!(
        decode(&make_frame(&[MarshalOpcode::PyVarInteger as u8, 0])).unwrap(),
        PyRep::Integer(0)
    );
    assert_eq!(
        decode(&make_frame(&[MarshalOpcode::PyVarInteger as u8, 2, 0x39, 0x05])).unwrap(),
        PyRep::Integer(0x0539)
    );
    assert_eq!(
        decode(&make_frame(&[MarshalOpcode::PyVarInteger as u8, 1, 0xFE])).unwrap(),
        PyRep::Integer(-2)
    );
    assert_eq!(
        decode(&make_frame(&[
            MarshalOpcode::PyVarInteger as u8,
            9,
            0x01,
            0,
            0,
            0,
            0,
            0,
            0,
            0x80,
            0x00
        ]))
        .unwrap(),
        PyRep::Integer(0x8000_0000_0000_0001_u64 as i64)
    );
    assert_eq!(
        decode(&make_frame(&[MarshalOpcode::PyVarInteger as u8, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0])),
        Err(ProtoError::UnexpectedEof)
    );

    // PyStringTableItem: 1-based index into MARSHAL_STRING_TABLE
    assert_eq!(
        decode(&make_frame(&[MarshalOpcode::PyStringTableItem as u8, 1])).unwrap(),
        PyRep::String(MARSHAL_STRING_TABLE[0].to_string())
    );
    let last_idx = MARSHAL_STRING_TABLE.len() as u8;
    assert_eq!(
        decode(&make_frame(&[MarshalOpcode::PyStringTableItem as u8, last_idx])).unwrap(),
        PyRep::String(MARSHAL_STRING_TABLE[(last_idx - 1) as usize].to_string())
    );
    assert_eq!(
        decode(&make_frame(&[MarshalOpcode::PyStringTableItem as u8, 0])),
        Err(ProtoError::UnsupportedOpcode(MarshalOpcode::PyStringTableItem as u8))
    );
    assert_eq!(
        decode(&make_frame(&[MarshalOpcode::PyStringTableItem as u8, last_idx + 1])),
        Err(ProtoError::UnsupportedOpcode(MarshalOpcode::PyStringTableItem as u8))
    );

    // PyWStringUCS2Char and PyWStringUCS2
    assert_eq!(
        decode(&make_frame(&[MarshalOpcode::PyWStringUCS2Char as u8, 0xA9, 0x00])).unwrap(),
        PyRep::String("©".into())
    );
    assert_eq!(
        decode(&make_frame(&[
            MarshalOpcode::PyWStringUCS2 as u8,
            2,
            b'O',
            0,
            b'K',
            0
        ]))
        .unwrap(),
        PyRep::String("OK".into())
    );

    // PyObject fallback branches when args do not match KeyVal / Rowset / IndexRowset / set shape
    let kv_non_dict = PyRep::Object {
        type_name: "util.KeyVal".into(),
        args: Box::new(PyRep::Integer(1)),
    };
    assert_eq!(decode(&encode(&kv_non_dict)).unwrap(), kv_non_dict);

    let rs_bad_lines = PyRep::Object {
        type_name: "util.Rowset".into(),
        args: Box::new(PyRep::Dict(vec![
            (
                PyRep::String("header".into()),
                PyRep::List(vec![PyRep::String("a".into())]),
            ),
            (
                PyRep::String("lines".into()),
                PyRep::List(vec![PyRep::Integer(1)]),
            ),
        ])),
    };
    assert_eq!(decode(&encode(&rs_bad_lines)).unwrap(), rs_bad_lines);

    let irs_bad_items = PyRep::Object {
        type_name: "util.IndexRowset".into(),
        args: Box::new(PyRep::Dict(vec![
            (PyRep::String("idName".into()), PyRep::String("a".into())),
            (
                PyRep::String("header".into()),
                PyRep::List(vec![PyRep::String("a".into())]),
            ),
            (
                PyRep::String("items".into()),
                PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Integer(2))]),
            ),
        ])),
    };
    assert_eq!(decode(&encode(&irs_bad_items)).unwrap(), irs_bad_items);

    let set_from_list = PyRep::Object {
        type_name: "builtins.set".into(),
        args: Box::new(PyRep::List(vec![PyRep::Integer(5)])),
    };
    assert_eq!(
        decode(&encode(&set_from_list)).unwrap(),
        PyRep::PySet(vec![PyRep::Integer(5)])
    );
    let set_from_tuple_scalars = PyRep::Object {
        type_name: "set".into(),
        args: Box::new(PyRep::Tuple(vec![PyRep::Integer(6)])),
    };
    assert_eq!(
        decode(&encode(&set_from_tuple_scalars)).unwrap(),
        PyRep::PySet(vec![PyRep::Integer(6)])
    );
}

#[test]
fn decode_error_branches_and_pack_dbrow_helper() {
    assert_eq!(decode(&[]), Err(ProtoError::UnexpectedEof));
    assert_eq!(decode(&[0x00, 0, 0, 0, 0]), Err(ProtoError::InvalidHeader(0x00)));
    assert_eq!(decode(&[MARSHAL_HEADER_BYTE, 0, 0]), Err(ProtoError::UnexpectedEof));

    // Small-window RFC 1950 zlib header (0x58 0x85: 0x5885 % 31 == 0) with invalid deflate payload
    assert!(matches!(
        decode(&[0x58, 0x85, 0xFF, 0xFF]),
        Err(ProtoError::ZlibError(_))
    ));

    // Save map out of bounds
    let bad_save = vec![
        MARSHAL_HEADER_BYTE,
        1,
        0,
        0,
        0,
        (MarshalOpcode::PyTrue as u8) | SAVE_FLAG,
        0,
        0,
        0,
        0, // slot 0 is invalid (1-based)
    ];
    assert_eq!(decode(&bad_save), Err(ProtoError::InvalidSaveReference(0)));

    let bad_ref = vec![
        MARSHAL_HEADER_BYTE,
        0,
        0,
        0,
        0,
        MarshalOpcode::PySavedStreamElement as u8,
        1,
    ];
    assert_eq!(decode(&bad_ref), Err(ProtoError::InvalidSaveReference(1)));

    assert_eq!(
        decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0, 0x3F]),
        Err(ProtoError::UnsupportedOpcode(0x3F))
    );

    // pack_dbrow helper covers all fixed & variable dbtype arms
    _keep_extract_col_defs_ref();
    let row = pack_dbrow(&[
        ("c_i8", 20, PyRep::Integer(10)),
        ("c_ui8", 21, PyRep::Integer(20)),
        ("c_ft", 64, PyRep::Integer(30)),
        ("c_cy", 6, PyRep::Integer(40)),
        ("c_r8", 5, PyRep::Real(1.25)),
        ("c_i4", 3, PyRep::Integer(-5)),
        ("c_ui4", 19, PyRep::Integer(6)),
        ("c_r4", 4, PyRep::Real(2.5)),
        ("c_i2", 2, PyRep::Integer(-7)),
        ("c_ui2", 18, PyRep::Integer(8)),
        ("c_i1", 16, PyRep::Integer(-9)),
        ("c_ui1", 17, PyRep::Integer(10)),
        ("c_b0", 11, PyRep::Bool(true)),
        ("c_b1", 11, PyRep::Bool(false)),
        ("c_str", 129, PyRep::String("packed".into())),
    ]);
    let enc = encode(&row);
    let dec = decode(&enc).unwrap();
    assert_eq!(dec, row);
    assert_eq!(dec.dbrow_get("c_i8"), Some(&PyRep::Integer(10)));
    assert_eq!(dec.dbrow_get("c_cy"), Some(&PyRep::Real(40.0)));
    assert_eq!(dec.dbrow_get("c_str"), Some(&PyRep::String("packed".into())));
}

#[test]
fn test_issue_413_pack_dbrow_emits_null_bitmap_and_roundtrips() {
    let row = pack_dbrow(&[
        ("id", 20, PyRep::Integer(42_001)),
        ("null_i8", 20, PyRep::None),
        ("score", 5, PyRep::Real(99.5)),
        ("null_r8", 5, PyRep::None),
        ("flag_true", 11, PyRep::Bool(true)),
        ("null_bool", 11, PyRep::None),
        ("name", 130, PyRep::String("Jita IV - Moon 4".into())),
        ("null_str", 130, PyRep::None),
    ]);
    let PyRep::DBRow {
        ref header,
        ref columns,
        ref raw_rle,
    } = row
    else {
        panic!("expected PyRep::DBRow");
    };
    assert_eq!(columns.len(), 8);
    let unpacked = crate::dbrow::try_unpack_packed_row_columns(header, raw_rle)
        .expect("pack_dbrow RLE must include full null-indicator bitmap");
    assert_eq!(unpacked[0], ("id".into(), PyRep::Integer(42_001)));
    assert_eq!(unpacked[1], ("null_i8".into(), PyRep::None));
    assert_eq!(unpacked[2], ("score".into(), PyRep::Real(99.5)));
    assert_eq!(unpacked[3], ("null_r8".into(), PyRep::None));
    assert_eq!(unpacked[4], ("flag_true".into(), PyRep::Bool(true)));
    assert_eq!(unpacked[5], ("null_bool".into(), PyRep::None));

    let wire = encode(&row);
    let decoded = decode(&wire).unwrap();
    assert_eq!(decoded.dbrow_get("id"), Some(&PyRep::Integer(42_001)));
    assert_eq!(decoded.dbrow_get("null_i8"), Some(&PyRep::None));
    assert_eq!(decoded.dbrow_get("score"), Some(&PyRep::Real(99.5)));
    assert_eq!(decoded.dbrow_get("null_r8"), Some(&PyRep::None));
    assert_eq!(decoded.dbrow_get("flag_true"), Some(&PyRep::Bool(true)));
    assert_eq!(decoded.dbrow_get("null_bool"), Some(&PyRep::None));
    assert_eq!(
        decoded.dbrow_get("name"),
        Some(&PyRep::String("Jita IV - Moon 4".into()))
    );
    assert_eq!(decoded.dbrow_get("null_str"), Some(&PyRep::None));
}

#[test]
fn test_issue_414_varinteger_9byte_u64_and_negative_sign_extension() {
    let wrap = |payload: &[u8]| -> Vec<u8> {
        let mut f = vec![0x7E_u8, 0, 0, 0, 0];
        f.extend_from_slice(payload);
        f
    };

    // 9-byte positive unsigned 64-bit integer with bit 63 set (e.g. 0x8000_0000_0000_0001)
    let high_u64: u64 = 0x8000_0000_0000_0001;
    let mut wire_9b = vec![MarshalOpcode::PyVarInteger as u8, 9];
    wire_9b.extend_from_slice(&high_u64.to_le_bytes());
    wire_9b.push(0x00);
    let decoded_9b = decode(&wrap(&wire_9b)).expect("9-byte unsigned PyVarInteger must decode");
    assert_eq!(decoded_9b, PyRep::Integer(high_u64 as i64));

    // Reject 9-byte PyVarInteger with invalid sign byte
    let mut bad_9b = vec![MarshalOpcode::PyVarInteger as u8, 9];
    bad_9b.extend_from_slice(&high_u64.to_le_bytes());
    bad_9b.push(0x01);
    assert!(decode(&wrap(&bad_9b)).is_err());

    // Negative 1-byte two's-complement integer (-1 = [0xFF])
    assert_eq!(
        decode(&wrap(&[MarshalOpcode::PyVarInteger as u8, 1, 0xFF])).unwrap(),
        PyRep::Integer(-1)
    );

    // Negative 2-byte two's-complement integer (-256 = [0x00, 0xFF])
    assert_eq!(
        decode(&wrap(&[MarshalOpcode::PyVarInteger as u8, 2, 0x00, 0xFF])).unwrap(),
        PyRep::Integer(-256)
    );

    // Negative 3-byte two's-complement integer (-100_000 = [0x60, 0x79, 0xFE])
    let neg_100k = (-100_000_i64).to_le_bytes();
    assert_eq!(
        decode(&wrap(&[
            MarshalOpcode::PyVarInteger as u8,
            3,
            neg_100k[0],
            neg_100k[1],
            neg_100k[2],
        ]))
        .unwrap(),
        PyRep::Integer(-100_000)
    );
}

#[test]
fn test_issue_415_quasar_packed_repeated_appends_across_chunks() {
    use crate::quasar::registry::DynamicValue;
    use crate::quasar::{DescriptorProto, FieldDescriptorProto, FieldLabel, FieldType};

    let mut reg = crate::quasar::registry::QuasarDescriptorRegistry::empty_with_builtins();
    reg.messages_by_full_name.insert(
        "test.PackedList".to_string(),
        DescriptorProto {
            name: "PackedList".to_string(),
            full_name: "test.PackedList".to_string(),
            fields: vec![FieldDescriptorProto {
                name: "ids".to_string(),
                number: 1,
                label: FieldLabel::Repeated,
                field_type: FieldType::Int32,
                type_name: String::new(),
                oneof_index: None,
                json_name: "ids".to_string(),
            }],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );

    // Two separate LengthDelimited records for field #1 (tag 0x0A): chunk1=[10, 20], chunk2=[30, 40]
    let wire = vec![0x0A, 2, 10, 20, 0x0A, 2, 30, 40];
    let msg = reg.decode_dynamic("test.PackedList", &wire).unwrap();
    assert_eq!(
        msg.fields.get(&1),
        Some(&DynamicValue::Repeated(vec![
            DynamicValue::Int32(10),
            DynamicValue::Int32(20),
            DynamicValue::Int32(30),
            DynamicValue::Int32(40),
        ]))
    );
}

#[test]
fn pack_dbrow_normalizes_cy_variants_unsigned_and_preserves_unknown_dbtypes() {
    let row = pack_dbrow(&[
        ("cy_int", 6, PyRep::Integer(40)),
        ("cy_frac", 6, PyRep::Real(12.3456)),
        ("cy_neg", 6, PyRep::Real(-98.7654)),
        ("cy_nan", 6, PyRep::Real(f64::NAN)),
        ("cy_inf", 6, PyRep::Real(f64::INFINITY)),
        ("cy_null", 6, PyRep::None),
        ("ui1_neg", 17, PyRep::Integer(-1)),
        ("ui2_neg", 18, PyRep::Integer(-1)),
        ("ui4_neg", 19, PyRep::Integer(-1)),
        ("unk_col", 999, PyRep::String("preserved".into())),
    ]);
    assert_eq!(row.dbrow_get("cy_int"), Some(&PyRep::Real(40.0)));
    assert_eq!(row.dbrow_get("cy_frac"), Some(&PyRep::Real(12.3456)));
    assert_eq!(row.dbrow_get("cy_neg"), Some(&PyRep::Real(-98.7654)));
    assert_eq!(row.dbrow_get("cy_nan"), Some(&PyRep::Real(0.0)));
    assert_eq!(row.dbrow_get("cy_inf"), Some(&PyRep::Real(0.0)));
    assert_eq!(row.dbrow_get("cy_null"), Some(&PyRep::None));
    assert_eq!(row.dbrow_get("ui1_neg"), Some(&PyRep::Integer(255)));
    assert_eq!(row.dbrow_get("ui2_neg"), Some(&PyRep::Integer(65_535)));
    assert_eq!(
        row.dbrow_get("ui4_neg"),
        Some(&PyRep::Integer(i64::from(u32::MAX)))
    );
    assert_eq!(
        row.dbrow_get("unk_col"),
        Some(&PyRep::String("preserved".into()))
    );

    // Wire roundtrip preserves all normalized fixed-width columns
    let dec = decode(&encode(&row)).unwrap();
    assert_eq!(dec.dbrow_get("cy_int"), Some(&PyRep::Real(40.0)));
    assert_eq!(dec.dbrow_get("cy_frac"), Some(&PyRep::Real(12.3456)));
    assert_eq!(dec.dbrow_get("cy_neg"), Some(&PyRep::Real(-98.7654)));
    assert_eq!(dec.dbrow_get("cy_nan"), Some(&PyRep::Real(0.0)));
    assert_eq!(dec.dbrow_get("cy_inf"), Some(&PyRep::Real(0.0)));
    assert_eq!(dec.dbrow_get("cy_null"), Some(&PyRep::None));
    assert_eq!(dec.dbrow_get("ui1_neg"), Some(&PyRep::Integer(255)));
    assert_eq!(dec.dbrow_get("ui2_neg"), Some(&PyRep::Integer(65_535)));
    assert_eq!(
        dec.dbrow_get("ui4_neg"),
        Some(&PyRep::Integer(i64::from(u32::MAX)))
    );
}

#[test]
fn is_zlib_stream_header_and_marshal_decode_error_arms_exhaustive() {
    // Direct boundary tests on is_zlib_stream_header
    assert!(!is_zlib_stream_header(&[]));
    assert!(!is_zlib_stream_header(&[0x58]));
    assert!(is_zlib_stream_header(&[crate::ZLIB_HEADER_BYTE]));
    assert!(is_zlib_stream_header(&[0x58, 0x85])); // (0x58 << 8 | 0x85) == 22661 == 31 * 731
    assert!(is_zlib_stream_header(&[0x78, 0x9C])); // CM=8, CINFO=7 (window <= 32K)
    assert!(!is_zlib_stream_header(&[0x58, 0x86])); // % 31 != 0
    assert!(!is_zlib_stream_header(&[0x88, 0x13])); // CINFO=8 (> 7) despite % 31 == 0
    assert!(!is_zlib_stream_header(&[0x59, 0x67])); // CM=9 (!= 8) despite % 31 == 0
    assert!(!is_zlib_stream_header(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0]));

    // Valid zlib stream roundtrip via encode_zlib -> decode
    let sample = PyRep::Tuple(vec![PyRep::Integer(12345), PyRep::String("zlib-ok".into())]);
    let compressed = encode_zlib(&sample).unwrap();
    assert_eq!(decode(&compressed).unwrap(), sample);

    // FrameTooLarge on outer slice and on inflated stream exceeding max_inflate_bytes
    assert_eq!(
        decode_with_limits(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PyNone as u8], 3, 100),
        Err(ProtoError::FrameTooLarge(6))
    );
    let big_compressed = encode_zlib(&PyRep::String("a".repeat(256))).unwrap();
    assert!(matches!(
        decode_with_limits(&big_compressed, big_compressed.len(), 100),
        Err(ProtoError::FrameTooLarge(_))
    ));

    // TrailingBytes when extra bytes follow the root value before save_map
    assert_eq!(
        decode(&[
            MARSHAL_HEADER_BYTE,
            0,
            0,
            0,
            0,
            MarshalOpcode::PyNone as u8,
            MarshalOpcode::PyTrue as u8,
        ]),
        Err(ProtoError::TrailingBytes(1))
    );

    // CollectionTooLarge when save_count > MAX_COLLECTION_LEN
    let too_many_saves = (crate::MAX_COLLECTION_LEN as u32) + 1;
    let mut bad_save_cnt = vec![MARSHAL_HEADER_BYTE];
    bad_save_cnt.extend_from_slice(&too_many_saves.to_le_bytes());
    bad_save_cnt.push(MarshalOpcode::PyNone as u8);
    assert_eq!(
        decode(&bad_save_cnt),
        Err(ProtoError::CollectionTooLarge(too_many_saves as usize))
    );

    // InvalidUtf8 on PyWStringUTF8 with invalid UTF-8 byte 0xFF
    assert!(matches!(
        decode(&[
            MARSHAL_HEADER_BYTE,
            0,
            0,
            0,
            0,
            MarshalOpcode::PyWStringUTF8 as u8,
            1,
            0xFF,
        ]),
        Err(ProtoError::Utf8(_))
    ));

    // RecursionLimitExceeded on deeply nested PyOneTuple (> MAX_RECURSION_DEPTH)
    let mut deep = vec![MARSHAL_HEADER_BYTE, 0, 0, 0, 0];
    deep.extend(std::iter::repeat_n(
        MarshalOpcode::PyOneTuple as u8,
        crate::MAX_RECURSION_DEPTH + 2,
    ));
    deep.push(MarshalOpcode::PyNone as u8);
    assert_eq!(decode(&deep), Err(ProtoError::RecursionLimitExceeded));

    // NodeBudgetExceeded when max_nodes is exhausted
    assert_eq!(
        decode_with_limits(
            &encode(&PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2)])),
            1024,
            1,
        ),
        Err(ProtoError::NodeBudgetExceeded)
    );
}

