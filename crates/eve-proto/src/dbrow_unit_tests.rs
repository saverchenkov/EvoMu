//! Unit and mutation-killing tests for `eve_proto::dbrow`.

use super::*;
use crate::ProtoError;

#[test]
fn db_column_type_conversions_bit_widths_and_value_normalization() {
    let all_types = [
        (0, DbColumnType::Empty, 0),
        (2, DbColumnType::I2, 16),
        (3, DbColumnType::I4, 32),
        (4, DbColumnType::R4, 32),
        (5, DbColumnType::R8, 64),
        (6, DbColumnType::Cy, 64),
        (11, DbColumnType::Bool, 1),
        (16, DbColumnType::I1, 8),
        (17, DbColumnType::Ui1, 8),
        (18, DbColumnType::Ui2, 16),
        (19, DbColumnType::Ui4, 32),
        (20, DbColumnType::I8, 64),
        (21, DbColumnType::Ui8, 64),
        (64, DbColumnType::FileTime, 64),
        (128, DbColumnType::Bytes, 0),
        (129, DbColumnType::Str, 0),
        (130, DbColumnType::WStr, 0),
    ];
    for (code, ty, width) in all_types {
        assert_eq!(DbColumnType::from_i64(code), Some(ty));
        assert_eq!(ty.as_i64(), code);
        assert_eq!(ty.bit_width(), width);
        assert_eq!(dbtype_bit_width(code), width);
        assert_eq!(ty.normalize_value(PyRep::None), PyRep::None);
    }
    for bad in [-1, 1, 7, 12, 63, 127, 131] {
        assert_eq!(DbColumnType::from_i64(bad), None);
    }

    // Signed vs unsigned integer truncation & Cy scaling
    assert_eq!(
        DbColumnType::I8.normalize_value(PyRep::Integer(-99)),
        PyRep::Integer(-99)
    );
    assert_eq!(
        DbColumnType::Cy.normalize_value(PyRep::Integer(50_000)),
        PyRep::Integer(50_000)
    );
    assert_eq!(
        DbColumnType::Cy.normalize_value(PyRep::Real(12.3456)),
        PyRep::Integer(123_456)
    );
    assert_eq!(
        DbColumnType::Cy.normalize_value(PyRep::String("bad".into())),
        PyRep::Integer(0)
    );
    assert_eq!(
        DbColumnType::I4.normalize_value(PyRep::Integer(-1)),
        PyRep::Integer(-1)
    );
    assert_eq!(
        DbColumnType::Ui4.normalize_value(PyRep::Integer(-1)),
        PyRep::Integer(u32::MAX as i64)
    );
    assert_eq!(
        DbColumnType::I2.normalize_value(PyRep::Integer(-1)),
        PyRep::Integer(-1)
    );
    assert_eq!(
        DbColumnType::Ui2.normalize_value(PyRep::Integer(-1)),
        PyRep::Integer(u16::MAX as i64)
    );
    assert_eq!(
        DbColumnType::I1.normalize_value(PyRep::Integer(-1)),
        PyRep::Integer(-1)
    );
    assert_eq!(
        DbColumnType::Ui1.normalize_value(PyRep::Integer(-1)),
        PyRep::Integer(255)
    );
    assert_eq!(
        DbColumnType::R8.normalize_value(PyRep::Real(3.5)),
        PyRep::Real(3.5)
    );
    assert_eq!(
        DbColumnType::R4.normalize_value(PyRep::Real(1.5)),
        PyRep::Real(1.5)
    );
    assert_eq!(
        DbColumnType::Bool.normalize_value(PyRep::Bool(true)),
        PyRep::Bool(true)
    );
    assert_eq!(
        DbColumnType::Bytes.normalize_value(PyRep::Buffer(vec![1, 2])),
        PyRep::Bytes(vec![1, 2])
    );
    assert_eq!(
        DbColumnType::Bytes.normalize_value(PyRep::Token("tok".into())),
        PyRep::Bytes(b"tok".to_vec())
    );
    assert_eq!(
        DbColumnType::Bytes.normalize_value(PyRep::Integer(1)),
        PyRep::Bytes(Vec::new())
    );
    assert_eq!(
        DbColumnType::Str.normalize_value(PyRep::Bytes(b"raw".to_vec())),
        PyRep::Bytes(b"raw".to_vec())
    );
    assert_eq!(
        DbColumnType::Str.normalize_value(PyRep::Token("tok".into())),
        PyRep::String("tok".into())
    );
    assert_eq!(
        DbColumnType::Str.normalize_value(PyRep::Integer(1)),
        PyRep::String(String::new())
    );
    assert_eq!(
        DbColumnType::WStr.normalize_value(PyRep::Bytes(b"utf8".to_vec())),
        PyRep::String("utf8".into())
    );
    assert_eq!(
        DbColumnType::WStr.normalize_value(PyRep::Integer(1)),
        PyRep::String(String::new())
    );
    assert_eq!(
        DbColumnType::Empty.normalize_value(PyRep::Integer(42)),
        PyRep::None
    );
}

#[test]
fn canonical_descriptors_and_rowset_builders_roundtrip() {
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
        assert!(!hdr.columns.is_empty());
        assert_eq!(hdr.column_names().len(), hdr.columns.len());
        assert_eq!(hdr.raw_defs().len(), hdr.columns.len());
        let rep = hdr.to_pyrep();
        assert_eq!(DBRowDescriptor::from_pyrep(&rep), Some(hdr));
    }

    // Tuple header representation and invalid dbtype rejection
    let tuple_hdr = PyRep::Tuple(vec![
        PyRep::Token("blue.DBRowDescriptor".into()),
        PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::String("id".into()),
            PyRep::Integer(20),
        ])]),
        PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::String("kw".into()),
            PyRep::Token("pkg.Fn".into()),
        ])]),
    ]);
    let parsed = DBRowDescriptor::from_pyrep(&tuple_hdr).unwrap();
    assert_eq!(parsed.columns, vec![("id".into(), DbColumnType::I8)]);
    assert_eq!(parsed.keywords, vec![("kw".into(), "pkg.Fn".into())]);

    let bad_hdr = PyRep::Tuple(vec![
        PyRep::Token("blue.DBRowDescriptor".into()),
        PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::String("id".into()),
            PyRep::Integer(999),
        ])]),
    ]);
    assert_eq!(DBRowDescriptor::from_pyrep(&bad_hdr), None);
    assert_eq!(DBRowDescriptor::from_pyrep(&PyRep::None), None);

    // Builders: crowset, cindexed_rowset, rowset, index_rowset, packed_row_list, packed_row_list_tuple
    let desc = DBRowDescriptor::market_summary_header();
    let sample_row = vec![
        PyRep::Real(100.5),
        PyRep::Real(50.0),
        PyRep::Integer(34),
        PyRep::Integer(60_003_760),
    ];
    let crowset = desc.build_crowset(vec![sample_row.clone()]);
    assert!(crowset.is_crowset());
    assert_eq!(crowset.crowset_rows().unwrap().len(), 1);

    let cindexed = desc.build_cindexed_rowset("typeID", vec![sample_row.clone()]);
    assert!(cindexed.is_cindexed_rowset());

    let rowset = desc.build_rowset(vec![sample_row.clone()]);
    assert!(matches!(rowset, PyRep::Rowset { .. }));

    let idx_rowset = desc.build_index_rowset("typeID", vec![sample_row.clone()]);
    assert!(matches!(idx_rowset, PyRep::IndexRowset { .. }));

    let pr_list = desc.build_packed_row_list(vec![sample_row.clone()]);
    assert!(matches!(pr_list, PyRep::List(ref l) if l.len() == 1));

    let pr_tuple = desc.build_packed_row_list_tuple(vec![sample_row]);
    assert!(matches!(pr_tuple, PyRep::Tuple(ref t) if t.len() == 2));
}

#[test]
fn encode_and_decode_rle_handle_zero_runs_literal_runs_and_corrupt_streams() {
    assert!(encode_rle(&[]).is_empty());
    assert!(encode_rle(&[0, 0, 0, 0, 0]).is_empty());
    assert_eq!(decode_rle(&[], 6), Some(vec![0; 6]));

    // Multi-chunk zero run (>8 zeros) followed by multi-chunk literal run (>7 non-zeros) and trailing zeros
    let mut raw = vec![0u8; 19];
    for i in 0..16u8 {
        raw.push(i + 1);
    }
    raw.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 0]);
    let rle = encode_rle(&raw);
    assert_eq!(decode_rle(&rle, raw.len()), Some(raw));

    // Odd vs even nibble counts
    for len in 1..=20 {
        let mut pat = Vec::with_capacity(len * 2);
        for i in 0..len {
            pat.push(if i % 2 == 0 { (i as u8) + 1 } else { 0 });
        }
        let enc = encode_rle(&pat);
        assert_eq!(decode_rle(&enc, pat.len()), Some(pat));
    }

    // Corrupt RLE: zero run exceeds expected_len
    assert_eq!(decode_rle(&[0xFF], 2), None);
    // Corrupt RLE: literal run claims 7 bytes (nibble = 1) but stream ends early
    assert_eq!(decode_rle(&[0x81, 0xAA, 0xBB], 10), None);
    // Corrupt RLE: literal run exceeds expected_len
    assert_eq!(decode_rle(&[0x85, 0xAA, 0xBB, 0xCC], 2), None);
}

#[test]
fn pack_and_unpack_packed_row_columns_cover_all_scalar_types_booleans_and_nulls() {
    assert!(pack_packed_row_columns(&PyRep::None, &[]).is_empty());
    assert!(pack_packed_row_columns_from_defs(&[], &[]).is_empty());
    assert!(unpack_packed_row_columns(&PyRep::None, &[]).is_empty());
    assert_eq!(
        extract_dbrow_col_defs(&PyRep::Tuple(vec![])),
        Some(Vec::new())
    );
    assert_eq!(
        extract_dbrow_col_defs(&PyRep::Tuple(vec![PyRep::Tuple(vec![])])),
        Some(Vec::new())
    );

    let desc = DBRowDescriptor::new([
        ("c_i8", DbColumnType::I8),
        ("c_ui8", DbColumnType::Ui8),
        ("c_ft", DbColumnType::FileTime),
        ("c_cy", DbColumnType::Cy),
        ("c_r8", DbColumnType::R8),
        ("c_i4", DbColumnType::I4),
        ("c_ui4", DbColumnType::Ui4),
        ("c_r4", DbColumnType::R4),
        ("c_i2", DbColumnType::I2),
        ("c_ui2", DbColumnType::Ui2),
        ("c_i1", DbColumnType::I1),
        ("c_ui1", DbColumnType::Ui1),
        ("b0", DbColumnType::Bool),
        ("b1", DbColumnType::Bool),
        ("b2", DbColumnType::Bool),
        ("b3", DbColumnType::Bool),
        ("b4", DbColumnType::Bool),
        ("b5", DbColumnType::Bool),
        ("b6", DbColumnType::Bool),
        ("b7", DbColumnType::Bool),
        ("b8", DbColumnType::Bool),
        ("c_str", DbColumnType::Str),
    ]);
    let hdr_rep = desc.to_pyrep();

    // Non-null values (shuffled column order to exercise name-lookup fallback)
    let mut cols = vec![
        ("c_i8".into(), PyRep::Integer(-100_000_000_000)),
        ("c_ui8".into(), PyRep::Integer(200_000_000_000)),
        ("c_ft".into(), PyRep::Integer(133_000_000_000_000_000)),
        ("c_cy".into(), PyRep::Real(99.5)),
        ("c_r8".into(), PyRep::Real(-42.125)),
        ("c_i4".into(), PyRep::Integer(-123_456)),
        ("c_ui4".into(), PyRep::Integer(3_000_000_000)),
        ("c_r4".into(), PyRep::Real(2.5)),
        ("c_i2".into(), PyRep::Integer(-3200)),
        ("c_ui2".into(), PyRep::Integer(60_000)),
        ("c_i1".into(), PyRep::Integer(-45)),
        ("c_ui1".into(), PyRep::Integer(250)),
        ("b0".into(), PyRep::Bool(true)),
        ("b1".into(), PyRep::Bool(false)),
        ("b2".into(), PyRep::Bool(true)),
        ("b3".into(), PyRep::Bool(false)),
        ("b4".into(), PyRep::Bool(true)),
        ("b5".into(), PyRep::Bool(false)),
        ("b6".into(), PyRep::Bool(true)),
        ("b7".into(), PyRep::Bool(false)),
        ("b8".into(), PyRep::Bool(true)),
        ("c_str".into(), PyRep::String("hello".into())),
    ];
    cols.reverse();
    let rle = pack_packed_row_columns(&hdr_rep, &cols);
    let unpacked = try_unpack_packed_row_columns(&hdr_rep, &rle).unwrap();
    let map: std::collections::HashMap<String, PyRep> = unpacked.into_iter().collect();
    assert_eq!(map.get("c_i8"), Some(&PyRep::Integer(-100_000_000_000)));
    assert_eq!(map.get("c_cy"), Some(&PyRep::Integer(995_000)));
    assert_eq!(map.get("c_r8"), Some(&PyRep::Real(-42.125)));
    assert_eq!(map.get("c_i4"), Some(&PyRep::Integer(-123_456)));
    assert_eq!(map.get("c_ui4"), Some(&PyRep::Integer(3_000_000_000)));
    assert_eq!(map.get("c_r4"), Some(&PyRep::Real(2.5)));
    assert_eq!(map.get("c_i2"), Some(&PyRep::Integer(-3200)));
    assert_eq!(map.get("c_ui2"), Some(&PyRep::Integer(60_000)));
    assert_eq!(map.get("c_i1"), Some(&PyRep::Integer(-45)));
    assert_eq!(map.get("c_ui1"), Some(&PyRep::Integer(250)));
    assert_eq!(map.get("b0"), Some(&PyRep::Bool(true)));
    assert_eq!(map.get("b1"), Some(&PyRep::Bool(false)));
    assert_eq!(map.get("b8"), Some(&PyRep::Bool(true)));

    // All-null fixed-width columns roundtrip as PyRep::None
    let null_rle = pack_packed_row_columns(&hdr_rep, &[]);
    let null_unpacked = try_unpack_packed_row_columns(&hdr_rep, &null_rle).unwrap();
    for (name, val) in null_unpacked {
        if !name.starts_with('b') {
            assert_eq!(val, PyRep::None, "expected None for {name}");
        }
    }

    // Corrupt RLE returns InvalidPackedRowRle
    assert_eq!(
        try_unpack_packed_row_columns(&hdr_rep, &[0x01, 0xFF]),
        Err(ProtoError::InvalidPackedRowRle)
    );
}
