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
        decode(&make_frame(&[MarshalOpcode::PyVarInteger as u8, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0])),
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
    assert_eq!(dec.dbrow_get("c_i8"), Some(&PyRep::Integer(10)));
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
