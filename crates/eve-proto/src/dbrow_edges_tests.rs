//! Edge-case, corrupt-RLE, invalid-descriptor, and boundary unit tests for `eve_proto::dbrow`.

use super::*;
use crate::{ProtoError, MAX_COLLECTION_LEN};

#[test]
fn db_column_type_normalize_value_fallbacks_and_non_utf8_wstr() {
    // Non-numeric inputs fall back to 0 / 0.0 / false
    for ty in [
        DbColumnType::I8,
        DbColumnType::Ui8,
        DbColumnType::FileTime,
        DbColumnType::I4,
        DbColumnType::Ui4,
        DbColumnType::I2,
        DbColumnType::Ui2,
        DbColumnType::I1,
        DbColumnType::Ui1,
    ] {
        assert_eq!(ty.normalize_value(PyRep::String("bad".into())), PyRep::Integer(0));
    }
    assert_eq!(
        DbColumnType::R8.normalize_value(PyRep::String("bad".into())),
        PyRep::Real(0.0)
    );
    assert_eq!(
        DbColumnType::R4.normalize_value(PyRep::String("bad".into())),
        PyRep::Real(0.0)
    );
    assert_eq!(
        DbColumnType::R4.normalize_value(PyRep::Real(1.23456789012345)),
        PyRep::Real((1.23456789012345_f64 as f32) as f64)
    );
    assert_eq!(
        DbColumnType::Bool.normalize_value(PyRep::String("bad".into())),
        PyRep::Bool(false)
    );

    // Bytes / Str / WStr string & lossy UTF-8 conversions
    assert_eq!(
        DbColumnType::Bytes.normalize_value(PyRep::Bytes(vec![1, 2])),
        PyRep::Bytes(vec![1, 2])
    );
    assert_eq!(
        DbColumnType::Bytes.normalize_value(PyRep::String("abc".into())),
        PyRep::Bytes(b"abc".to_vec())
    );
    assert_eq!(
        DbColumnType::Str.normalize_value(PyRep::String("hello".into())),
        PyRep::String("hello".into())
    );
    assert_eq!(
        DbColumnType::WStr.normalize_value(PyRep::String("wide".into())),
        PyRep::String("wide".into())
    );
    assert_eq!(
        DbColumnType::WStr.normalize_value(PyRep::Token("tok".into())),
        PyRep::String("tok".into())
    );
    assert_eq!(
        DbColumnType::WStr.normalize_value(PyRep::Bytes(vec![b'a', 0xFF, b'b'])),
        PyRep::String("a\u{FFFD}b".into())
    );
}

#[test]
fn dbrow_descriptor_parsing_malformed_shapes_and_builder_fallbacks() {
    // ObjectEx with non-Tuple header -> None
    let bad_objex = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Integer(1)),
        list: vec![],
        dict: vec![],
    };
    assert_eq!(DBRowDescriptor::from_pyrep(&bad_objex), None);

    // Tuple too short or second element not a Tuple
    assert_eq!(
        DBRowDescriptor::from_pyrep(&PyRep::Tuple(vec![PyRep::Token("x".into())])),
        None
    );
    assert_eq!(
        DBRowDescriptor::from_pyrep(&PyRep::Tuple(vec![
            PyRep::Token("x".into()),
            PyRep::Integer(2),
        ])),
        None
    );

    // extract_dbrow_col_defs malformed entries
    assert_eq!(extract_dbrow_col_defs(&PyRep::Integer(5)), None);
    assert_eq!(
        extract_dbrow_col_defs(&PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(123),
            PyRep::Integer(20),
        ])])),
        None
    );
    assert_eq!(
        extract_dbrow_col_defs(&PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::String("col".into()),
            PyRep::String("not_int".into()),
        ])])),
        None
    );

    // Tuple header with malformed keyword entries & short column pair
    let weird_hdr = PyRep::Tuple(vec![
        PyRep::Token("blue.DBRowDescriptor".into()),
        PyRep::Tuple(vec![
            PyRep::Integer(99), // non-tuple entry skipped
            PyRep::Tuple(vec![PyRep::String("only_one".into())]), // len < 2 skipped
            PyRep::Tuple(vec![PyRep::String("id".into()), PyRep::Integer(20)]),
        ]),
        PyRep::List(vec![
            PyRep::Integer(1), // non-tuple keyword skipped
            PyRep::Tuple(vec![PyRep::String("short".into())]), // len < 2 skipped
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Token("bad_key".into())]), // non-str key skipped
            PyRep::Tuple(vec![PyRep::String("ok".into()), PyRep::Token("mod.Sym".into())]),
        ]),
    ]);
    let parsed = DBRowDescriptor::from_pyrep(&weird_hdr).unwrap();
    assert_eq!(parsed.columns, vec![("id".into(), DbColumnType::I8)]);
    assert_eq!(parsed.keywords, vec![("ok".into(), "mod.Sym".into())]);

    // Non-list keywords slot is ignored
    let non_list_kw = PyRep::Tuple(vec![
        PyRep::Token("blue.DBRowDescriptor".into()),
        PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::String("id".into()),
            PyRep::Integer(3),
        ])]),
        PyRep::None,
    ]);
    let parsed2 = DBRowDescriptor::from_pyrep(&non_list_kw).unwrap();
    assert!(parsed2.keywords.is_empty());

    // build_dbrow with fewer values than columns & extra trailing values
    let desc = DBRowDescriptor::new([
        ("a", DbColumnType::I4),
        ("b", DbColumnType::Str),
        ("c", DbColumnType::Bool),
    ]);
    let row_short = desc.build_dbrow(vec![PyRep::Integer(7)]);
    let PyRep::DBRow { columns, raw_rle, .. } = &row_short else {
        panic!("expected DBRow");
    };
    assert_eq!(columns.len(), 3);
    assert_eq!(columns[0], ("a".into(), PyRep::Integer(7)));
    assert_eq!(columns[1], ("b".into(), PyRep::None));
    assert_eq!(columns[2], ("c".into(), PyRep::None));
    let unpacked_short = try_unpack_packed_row_columns(&desc.to_pyrep(), raw_rle).unwrap();
    assert_eq!(unpacked_short[0], ("a".into(), PyRep::Integer(7)));
    assert_eq!(unpacked_short[1], ("b".into(), PyRep::None));
    assert_eq!(unpacked_short[2], ("c".into(), PyRep::None));

    // build_cindexed_rowset & build_index_rowset with unknown key_column and empty descriptor
    let cidx_fallback = desc.build_cindexed_rowset("nonexistent", vec![vec![PyRep::Integer(5)]]);
    assert!(matches!(cidx_fallback, PyRep::ObjectEx { dict, .. } if dict[0].0 == PyRep::Integer(5)));

    let empty_desc = DBRowDescriptor::new(Vec::<(&str, DbColumnType)>::new());
    let empty_cidx = empty_desc.build_cindexed_rowset("missing", vec![vec![]]);
    assert!(matches!(empty_cidx, PyRep::ObjectEx { dict, .. } if dict[0].0 == PyRep::None));
    let empty_idx = empty_desc.build_index_rowset("missing", vec![vec![]]);
    assert!(matches!(empty_idx, PyRep::IndexRowset { items, .. } if items[0].0 == PyRep::None));
}

#[test]
fn decode_rle_second_nibble_overflow_and_packed_row_edge_cases() {
    assert_eq!(decode_rle(&[], 0), Some(vec![]));
    assert_eq!(decode_rle(&[0x88], 0), None);

    // High-nibble zero run caps at expected_len (low nibble 0xC = 5 zeros < 8, high nibble 0xF = 8 zeros -> capped to 8)
    assert_eq!(decode_rle(&[0xFC], 8), Some(vec![0u8; 8]));
    // High-nibble literal run hitting EOF pads remaining bytes with zeros to expected_len
    assert_eq!(decode_rle(&[0x78], 4), Some(vec![0u8; 4]));

    // Empty defs return empty Vec in pack/unpack/try_unpack
    let empty_hdr = DBRowDescriptor::new(Vec::<(&str, DbColumnType)>::new()).to_pyrep();
    assert!(pack_packed_row_columns(&empty_hdr, &[]).is_empty());
    assert_eq!(try_unpack_packed_row_columns(&empty_hdr, &[]), Ok(vec![]));
    assert!(unpack_packed_row_columns(&empty_hdr, &[0xFF]).is_empty());

    // Non-numeric values in every fixed-width column type + unknown dbtype (99) + Cy Integer vs Real vs String
    let hdr_weird = PyRep::Tuple(vec![
        PyRep::Token("blue.DBRowDescriptor".into()),
        PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::String("c_i8".into()), PyRep::Integer(20)]),
            PyRep::Tuple(vec![PyRep::String("c_cy_int".into()), PyRep::Integer(6)]),
            PyRep::Tuple(vec![PyRep::String("c_cy_bad".into()), PyRep::Integer(6)]),
            PyRep::Tuple(vec![PyRep::String("c_r8".into()), PyRep::Integer(5)]),
            PyRep::Tuple(vec![PyRep::String("c_i4".into()), PyRep::Integer(3)]),
            PyRep::Tuple(vec![PyRep::String("c_ui4".into()), PyRep::Integer(19)]),
            PyRep::Tuple(vec![PyRep::String("c_r4".into()), PyRep::Integer(4)]),
            PyRep::Tuple(vec![PyRep::String("c_i2".into()), PyRep::Integer(2)]),
            PyRep::Tuple(vec![PyRep::String("c_ui2".into()), PyRep::Integer(18)]),
            PyRep::Tuple(vec![PyRep::String("c_i1".into()), PyRep::Integer(16)]),
            PyRep::Tuple(vec![PyRep::String("c_ui1".into()), PyRep::Integer(17)]),
            PyRep::Tuple(vec![PyRep::String("c_bool_bad".into()), PyRep::Integer(11)]),
            PyRep::Tuple(vec![PyRep::String("c_bool_null".into()), PyRep::Integer(11)]),
            PyRep::Tuple(vec![PyRep::String("c_unknown".into()), PyRep::Integer(99)]),
        ])]),
    ]);
    let cols = vec![
        ("c_i8".into(), PyRep::String("bad".into())),
        ("c_cy_int".into(), PyRep::Integer(123_456)),
        ("c_cy_bad".into(), PyRep::String("bad".into())),
        ("c_r8".into(), PyRep::String("bad".into())),
        ("c_i4".into(), PyRep::String("bad".into())),
        ("c_ui4".into(), PyRep::String("bad".into())),
        ("c_r4".into(), PyRep::String("bad".into())),
        ("c_i2".into(), PyRep::String("bad".into())),
        ("c_ui2".into(), PyRep::String("bad".into())),
        ("c_i1".into(), PyRep::String("bad".into())),
        ("c_ui1".into(), PyRep::String("bad".into())),
        ("c_bool_bad".into(), PyRep::String("bad".into())),
        ("c_bool_null".into(), PyRep::None),
        ("c_unknown".into(), PyRep::Integer(1)),
    ];
    let rle = pack_packed_row_columns(&hdr_weird, &cols);
    let unpacked = try_unpack_packed_row_columns(&hdr_weird, &rle).unwrap();
    let map: std::collections::HashMap<String, PyRep> = unpacked.into_iter().collect();
    assert_eq!(map.get("c_i8"), Some(&PyRep::Integer(0)));
    assert_eq!(map.get("c_cy_int"), Some(&PyRep::Real(123_456.0)));
    assert_eq!(map.get("c_cy_bad"), Some(&PyRep::Real(0.0)));
    assert_eq!(map.get("c_r8"), Some(&PyRep::Real(0.0)));
    assert_eq!(map.get("c_r4"), Some(&PyRep::Real(0.0)));
    assert_eq!(map.get("c_bool_bad"), Some(&PyRep::Bool(false)));
    assert_eq!(map.get("c_bool_null"), Some(&PyRep::None));
    assert_eq!(map.get("c_unknown"), Some(&PyRep::None));

    // CollectionTooLarge when expected_bytes > MAX_COLLECTION_LEN
    let mut huge_cols = Vec::with_capacity(16_132);
    for i in 0..16_132 {
        huge_cols.push(PyRep::Tuple(vec![
            PyRep::String(format!("c{i}")),
            PyRep::Integer(20),
        ]));
    }
    let huge_hdr = PyRep::Tuple(vec![
        PyRep::Token("blue.DBRowDescriptor".into()),
        PyRep::Tuple(vec![PyRep::Tuple(huge_cols)]),
    ]);
    assert_eq!(
        try_unpack_packed_row_columns(&huge_hdr, &[]),
        Err(ProtoError::CollectionTooLarge(MAX_COLLECTION_LEN + 1))
    );
    assert!(unpack_packed_row_columns(&huge_hdr, &[]).is_empty());
}
