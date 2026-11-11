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
        PyRep::Real(50_000.0)
    );
    assert_eq!(
        DbColumnType::Cy.normalize_value(PyRep::Real(12.3456)),
        PyRep::Real(12.3456)
    );
    assert_eq!(
        DbColumnType::Cy.normalize_value(PyRep::Real(-98.7654)),
        PyRep::Real(-98.7654)
    );
    assert_eq!(
        DbColumnType::Cy.normalize_value(PyRep::Real(1.00006)),
        PyRep::Real(1.0001)
    );
    assert_eq!(
        DbColumnType::Cy.normalize_value(PyRep::Real(f64::NAN)),
        PyRep::Real(0.0)
    );
    assert_eq!(
        DbColumnType::Cy.normalize_value(PyRep::Real(f64::INFINITY)),
        PyRep::Real(0.0)
    );
    assert_eq!(
        DbColumnType::Cy.normalize_value(PyRep::Real(f64::NEG_INFINITY)),
        PyRep::Real(0.0)
    );
    assert_eq!(
        DbColumnType::Cy.normalize_value(PyRep::String("bad".into())),
        PyRep::Real(0.0)
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
    assert_eq!(map.get("c_ui8"), Some(&PyRep::Integer(200_000_000_000)));
    assert_eq!(
        map.get("c_ft"),
        Some(&PyRep::Integer(133_000_000_000_000_000))
    );
    assert_eq!(map.get("c_cy"), Some(&PyRep::Real(99.5)));
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

#[test]
fn packed_row_unsigned_high_bit_zero_extension_and_signed_negative_roundtrip() {
    let desc = DBRowDescriptor::new([
        ("ui1", DbColumnType::Ui1),
        ("i1", DbColumnType::I1),
        ("ui2", DbColumnType::Ui2),
        ("i2", DbColumnType::I2),
        ("ui4", DbColumnType::Ui4),
        ("i4", DbColumnType::I4),
        ("ui8", DbColumnType::Ui8),
        ("i8", DbColumnType::I8),
        ("tail", DbColumnType::WStr),
    ]);

    let ui1_cases: [u8; 6] = [0x00, 0x7F, 0x80, 0xAB, 0xFE, 0xFF];
    let i1_cases: [i8; 6] = [0, i8::MAX, i8::MIN, -85, -2, -1];
    let ui2_cases: [u16; 6] = [0x0000, 0x7FFF, 0x8000, 0xCAFE, 0xFFFE, 0xFFFF];
    let i2_cases: [i16; 6] = [0, i16::MAX, i16::MIN, -13_570, -2, -1];
    let ui4_cases: [u32; 6] = [
        0,
        0x7FFF_FFFF,
        0x8000_0000,
        0xDEAD_BEEF,
        0xFFFF_FFFE,
        0xFFFF_FFFF,
    ];
    let i4_cases: [i32; 6] = [0, i32::MAX, i32::MIN, -559_038_737, -2, -1];
    let ui8_cases: [u64; 6] = [
        0,
        0x7FFF_FFFF_FFFF_FFFF,
        0x8000_0000_0000_0000,
        0xFEED_FACE_CAFE_BEEF,
        u64::MAX - 1,
        u64::MAX,
    ];
    let i8_cases: [i64; 6] = [0, i64::MAX, i64::MIN, -9_000_000_000_001, -2, -1];

    for idx in 0..6 {
        let row = desc.build_dbrow(vec![
            PyRep::Integer(i64::from(ui1_cases[idx])),
            PyRep::Integer(i64::from(i1_cases[idx])),
            PyRep::Integer(i64::from(ui2_cases[idx])),
            PyRep::Integer(i64::from(i2_cases[idx])),
            PyRep::Integer(i64::from(ui4_cases[idx])),
            PyRep::Integer(i64::from(i4_cases[idx])),
            PyRep::Integer(ui8_cases[idx] as i64),
            PyRep::Integer(i8_cases[idx]),
            PyRep::String(format!("row-{idx}")),
        ]);

        let wire = crate::marshal::encode(&row);
        let decoded = crate::marshal::decode(&wire)
            .expect("PyPackedRow with high-bit unsigned and negative signed columns must decode");
        assert_eq!(decoded, row, "roundtrip mismatch on boundary case {idx}");
        assert_eq!(crate::marshal::encode(&decoded), wire);

        assert_eq!(
            decoded.dbrow_get("ui1"),
            Some(&PyRep::Integer(i64::from(ui1_cases[idx])))
        );
        assert_eq!(
            decoded.dbrow_get("i1"),
            Some(&PyRep::Integer(i64::from(i1_cases[idx])))
        );
        assert_eq!(
            decoded.dbrow_get("ui2"),
            Some(&PyRep::Integer(i64::from(ui2_cases[idx])))
        );
        assert_eq!(
            decoded.dbrow_get("i2"),
            Some(&PyRep::Integer(i64::from(i2_cases[idx])))
        );
        assert_eq!(
            decoded.dbrow_get("ui4"),
            Some(&PyRep::Integer(i64::from(ui4_cases[idx])))
        );
        assert_eq!(
            decoded.dbrow_get("i4"),
            Some(&PyRep::Integer(i64::from(i4_cases[idx])))
        );
        assert_eq!(
            decoded.dbrow_get("ui8"),
            Some(&PyRep::Integer(ui8_cases[idx] as i64))
        );
        assert_eq!(
            decoded.dbrow_get("i8"),
            Some(&PyRep::Integer(i8_cases[idx]))
        );
        assert_eq!(
            decoded.dbrow_get("tail"),
            Some(&PyRep::String(format!("row-{idx}")))
        );

        // High-bit unsigned values (`idx >= 2`) must strictly zero-extend (`> 0`), never sign-extend (`< 0`).
        if idx >= 2 {
            assert!(decoded.dbrow_get("ui1").unwrap().as_i64().unwrap() >= 0x80);
            assert!(decoded.dbrow_get("ui2").unwrap().as_i64().unwrap() >= 0x8000);
            assert!(decoded.dbrow_get("ui4").unwrap().as_i64().unwrap() >= 0x8000_0000);
            assert!(decoded.dbrow_get("i1").unwrap().as_i64().unwrap() < 0);
            assert!(decoded.dbrow_get("i2").unwrap().as_i64().unwrap() < 0);
            assert!(decoded.dbrow_get("i4").unwrap().as_i64().unwrap() < 0);
            assert!(decoded.dbrow_get("i8").unwrap().as_i64().unwrap() < 0);
        }
    }
}

#[test]
fn dbrow_normalization_overflow_fallback_and_header_parsing_edge_cases() {
    // Integer bit-width truncation & non-integer fallbacks
    assert_eq!(
        DbColumnType::I8.normalize_value(PyRep::String("bad".into())),
        PyRep::Integer(0)
    );
    assert_eq!(
        DbColumnType::I4.normalize_value(PyRep::Integer((i32::MAX as i64) + 1)),
        PyRep::Integer(i32::MIN as i64)
    );
    assert_eq!(
        DbColumnType::Ui4.normalize_value(PyRep::Integer((u32::MAX as i64) + 2)),
        PyRep::Integer(1)
    );
    assert_eq!(
        DbColumnType::I2.normalize_value(PyRep::Integer(32_768)),
        PyRep::Integer(-32_768)
    );
    assert_eq!(
        DbColumnType::Ui2.normalize_value(PyRep::Integer(65_537)),
        PyRep::Integer(1)
    );
    assert_eq!(
        DbColumnType::I1.normalize_value(PyRep::Integer(128)),
        PyRep::Integer(-128)
    );
    assert_eq!(
        DbColumnType::Ui1.normalize_value(PyRep::Integer(257)),
        PyRep::Integer(1)
    );
    assert_eq!(
        DbColumnType::R8.normalize_value(PyRep::String("bad".into())),
        PyRep::Real(0.0)
    );
    assert_eq!(
        DbColumnType::R4.normalize_value(PyRep::Real(1.123456789)),
        PyRep::Real((1.123456789f64 as f32) as f64)
    );
    assert_eq!(
        DbColumnType::Bool.normalize_value(PyRep::String("true".into())),
        PyRep::Bool(false)
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
        DbColumnType::WStr.normalize_value(PyRep::Bytes(vec![0xFF, 0xFE])),
        PyRep::String("\u{FFFD}\u{FFFD}".into())
    );

    // Direct tuple of (name, dbtype) pairs without extra outer 1-tuple wrapper
    let direct_cols = PyRep::Tuple(vec![
        PyRep::Tuple(vec![PyRep::String("a".into()), PyRep::Integer(3)]),
        PyRep::Tuple(vec![PyRep::String("b".into()), PyRep::Integer(11)]),
    ]);
    assert_eq!(
        extract_dbrow_col_defs(&direct_cols),
        Some(vec![("a".into(), 3), ("b".into(), 11)])
    );
    assert_eq!(extract_dbrow_col_defs(&PyRep::None), None);
    assert_eq!(extract_dbrow_header_col_defs(&PyRep::None), None);
    assert_eq!(
        extract_dbrow_header_col_defs(&PyRep::ObjectEx {
            is_type_2: false,
            header: Box::new(PyRep::None),
            list: Vec::new(),
            dict: Vec::new(),
        }),
        None
    );
    assert_eq!(
        extract_dbrow_col_defs(&PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Integer(3),
        ])])),
        None
    );

    // DBRowDescriptor::from_pyrep ignores malformed keyword entries & handles empty descriptor
    let empty_desc = DBRowDescriptor::new(Vec::<(String, DbColumnType)>::new());
    let empty_rep = empty_desc.to_pyrep();
    assert_eq!(DBRowDescriptor::from_pyrep(&empty_rep), Some(empty_desc));

    let objex_bad_kw = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("blue.DBRowDescriptor".into()),
            PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::String("x".into()),
                PyRep::Integer(3),
            ])])]),
            PyRep::List(vec![
                PyRep::Integer(1),
                PyRep::Tuple(vec![PyRep::String("only_one".into())]),
                PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Token("tok".into())]),
                PyRep::Tuple(vec![PyRep::String("ok".into()), PyRep::Token("fn".into())]),
            ]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    let parsed_kw = DBRowDescriptor::from_pyrep(&objex_bad_kw);
    assert!(parsed_kw.is_some());
    if let Some(d) = parsed_kw {
        assert_eq!(d.keywords, vec![("ok".into(), "fn".into())]);
    }
}

#[test]
fn packed_row_currency_cy_ten_thousand_scale_symmetry_and_edge_cases() {
    assert_eq!(DBTYPE_CY_SCALE_F64, 10_000.0);
    assert_eq!(DBTYPE_CY_SCALE_I64, 10_000);

    let desc = DBRowDescriptor::new([
        ("pos_cy", DbColumnType::Cy),
        ("neg_cy", DbColumnType::Cy),
        ("int_cy", DbColumnType::Cy),
        ("zero_cy", DbColumnType::Cy),
        ("null_cy", DbColumnType::Cy),
        ("round_cy", DbColumnType::Cy),
        ("marker_ui2", DbColumnType::Ui2),
    ]);
    let defs = desc.raw_defs();
    let expected_len = packed_row_expected_bytes(&defs);

    let row = desc.build_dbrow(vec![
        PyRep::Real(12.3456),
        PyRep::Real(-98.7654),
        PyRep::Integer(250),
        PyRep::Real(-0.0),
        PyRep::None,
        PyRep::Real(1.00006),
        PyRep::Integer(0xBEEF),
    ]);

    let PyRep::DBRow { ref raw_rle, .. } = row else {
        panic!("expected PyRep::DBRow");
    };
    let unpacked_bytes = decode_rle(raw_rle, expected_len).expect("valid RLE buffer");
    // Widest-first layout places the six 64-bit `Cy` columns in bytes `0..48` in descriptor order,
    // followed by the 16-bit `Ui2` column in bytes `48..50`, then the null-indicator bitmap.
    assert_eq!(
        i64::from_le_bytes(unpacked_bytes[0..8].try_into().unwrap()),
        123_456
    );
    assert_eq!(
        i64::from_le_bytes(unpacked_bytes[8..16].try_into().unwrap()),
        -987_654
    );
    assert_eq!(
        i64::from_le_bytes(unpacked_bytes[16..24].try_into().unwrap()),
        2_500_000
    );
    assert_eq!(
        i64::from_le_bytes(unpacked_bytes[24..32].try_into().unwrap()),
        0
    );
    assert_eq!(
        i64::from_le_bytes(unpacked_bytes[32..40].try_into().unwrap()),
        0
    );
    assert_eq!(
        i64::from_le_bytes(unpacked_bytes[40..48].try_into().unwrap()),
        10_001
    );
    assert_eq!(
        u16::from_le_bytes([unpacked_bytes[48], unpacked_bytes[49]]),
        0xBEEF
    );

    let wire = crate::marshal::encode(&row);
    let decoded = crate::marshal::decode(&wire).expect("marshal decode Cy DBRow");
    assert_eq!(decoded, row);
    assert_eq!(crate::marshal::encode(&decoded), wire);
    assert_eq!(decoded.dbrow_get("pos_cy"), Some(&PyRep::Real(12.3456)));
    assert_eq!(decoded.dbrow_get("neg_cy"), Some(&PyRep::Real(-98.7654)));
    assert_eq!(decoded.dbrow_get("int_cy"), Some(&PyRep::Real(250.0)));
    if let Some(PyRep::Real(z)) = decoded.dbrow_get("zero_cy") {
        assert_eq!(*z, 0.0);
        assert!(z.is_sign_positive());
    } else {
        panic!("expected Real(0.0) for zero_cy");
    }
    assert_eq!(decoded.dbrow_get("null_cy"), Some(&PyRep::None));
    assert_eq!(decoded.dbrow_get("round_cy"), Some(&PyRep::Real(1.0001)));
    assert_eq!(
        decoded.dbrow_get("marker_ui2"),
        Some(&PyRep::Integer(0xBEEF))
    );

    // Inbound wire-first unpack: raw fixed-point i64 bytes (1_000_005 -> 100.0005 ISK, -42_500 -> -4.25 ISK)
    let wire_desc = DBRowDescriptor::new([
        ("wallet", DbColumnType::Cy),
        ("delta", DbColumnType::Cy),
    ]);
    let mut raw_buf = vec![0u8; packed_row_expected_bytes(&wire_desc.raw_defs())];
    raw_buf[0..8].copy_from_slice(&1_000_005_i64.to_le_bytes());
    raw_buf[8..16].copy_from_slice(&(-42_500_i64).to_le_bytes());
    let inbound_row = PyRep::DBRow {
        header: Box::new(wire_desc.to_pyrep()),
        columns: Vec::new(),
        raw_rle: encode_rle(&raw_buf),
    };
    let inbound_decoded =
        crate::marshal::decode(&crate::marshal::encode(&inbound_row)).expect("decode raw Cy RLE");
    assert_eq!(
        inbound_decoded.dbrow_get("wallet"),
        Some(&PyRep::Real(100.0005))
    );
    assert_eq!(inbound_decoded.dbrow_get("delta"), Some(&PyRep::Real(-4.25)));

    // Negative / malformed inputs: NaN, +-Infinity, non-numeric strings, saturation at i64::MIN/MAX,
    // and unknown raw dbtype codes must never panic or produce NaN cells.
    let edge_hdr = DBRowDescriptor::new([
        ("nan_cy", DbColumnType::Cy),
        ("inf_cy", DbColumnType::Cy),
        ("ninf_cy", DbColumnType::Cy),
        ("str_cy", DbColumnType::Cy),
        ("max_int_cy", DbColumnType::Cy),
        ("min_int_cy", DbColumnType::Cy),
    ])
    .to_pyrep();
    let edge_cols = vec![
        ("nan_cy".into(), PyRep::Real(f64::NAN)),
        ("inf_cy".into(), PyRep::Real(f64::INFINITY)),
        ("ninf_cy".into(), PyRep::Real(f64::NEG_INFINITY)),
        ("str_cy".into(), PyRep::String("not-a-number".into())),
        ("max_int_cy".into(), PyRep::Integer(i64::MAX)),
        ("min_int_cy".into(), PyRep::Integer(i64::MIN)),
    ];
    let edge_rle = pack_packed_row_columns(&edge_hdr, &edge_cols);
    let edge_unpacked: std::collections::HashMap<String, PyRep> =
        try_unpack_packed_row_columns(&edge_hdr, &edge_rle)
            .unwrap()
            .into_iter()
            .collect();
    assert_eq!(edge_unpacked.get("nan_cy"), Some(&PyRep::Real(0.0)));
    assert_eq!(edge_unpacked.get("inf_cy"), Some(&PyRep::Real(0.0)));
    assert_eq!(edge_unpacked.get("ninf_cy"), Some(&PyRep::Real(0.0)));
    assert_eq!(edge_unpacked.get("str_cy"), Some(&PyRep::Real(0.0)));
    assert_eq!(
        edge_unpacked.get("max_int_cy"),
        Some(&PyRep::Real((i64::MAX as f64) / 10_000.0))
    );
    assert_eq!(
        edge_unpacked.get("min_int_cy"),
        Some(&PyRep::Real((i64::MIN as f64) / 10_000.0))
    );

    // Unknown raw dbtype code in untyped Tuple descriptor gracefully packs/unpacks as 0-bit PyRep::None
    let unknown_hdr = PyRep::Tuple(vec![
        PyRep::Token("blue.DBRowDescriptor".into()),
        PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::String("mystery".into()),
            PyRep::Integer(999),
        ])]),
    ]);
    let unk_rle = pack_packed_row_columns(&unknown_hdr, &[("mystery".into(), PyRep::Integer(7))]);
    assert_eq!(
        try_unpack_packed_row_columns(&unknown_hdr, &unk_rle).unwrap(),
        vec![("mystery".into(), PyRep::None)]
    );
}

#[test]
fn dbrow_cy_sub_unit_rounding_negative_symmetry_and_overflow_saturation() {
    let desc = DBRowDescriptor::new([
        ("min_pos", DbColumnType::Cy),
        ("min_neg", DbColumnType::Cy),
        ("round_down", DbColumnType::Cy),
        ("round_up_pos", DbColumnType::Cy),
        ("round_up_neg", DbColumnType::Cy),
        ("sat_pos_f64", DbColumnType::Cy),
        ("sat_neg_f64", DbColumnType::Cy),
        ("sat_pos_i64", DbColumnType::Cy),
        ("sat_neg_i64", DbColumnType::Cy),
    ]);
    let defs = desc.raw_defs();
    let expected_len = packed_row_expected_bytes(&defs);
    let row = desc.build_dbrow(vec![
        PyRep::Real(0.0001),
        PyRep::Real(-0.0001),
        PyRep::Real(0.00004),
        PyRep::Real(0.00005),
        PyRep::Real(-0.00005),
        PyRep::Real(1e30),
        PyRep::Real(-1e30),
        PyRep::Integer(i64::MAX / 10_000 + 1),
        PyRep::Integer(i64::MIN / 10_000 - 1),
    ]);

    let PyRep::DBRow { ref raw_rle, .. } = row else {
        panic!("expected PyRep::DBRow");
    };
    let raw = decode_rle(raw_rle, expected_len).unwrap();
    let read_i64 = |idx: usize| -> i64 {
        i64::from_le_bytes(raw[idx * 8..(idx + 1) * 8].try_into().unwrap())
    };
    assert_eq!(read_i64(0), 1);
    assert_eq!(read_i64(1), -1);
    assert_eq!(read_i64(2), 0);
    assert_eq!(read_i64(3), 1);
    assert_eq!(read_i64(4), -1);
    assert_eq!(read_i64(5), i64::MAX);
    assert_eq!(read_i64(6), i64::MIN);
    assert_eq!(read_i64(7), i64::MAX);
    assert_eq!(read_i64(8), i64::MIN);

    let unpacked: std::collections::HashMap<String, PyRep> =
        try_unpack_packed_row_columns(&desc.to_pyrep(), raw_rle)
            .unwrap()
            .into_iter()
            .collect();
    assert_eq!(unpacked.get("min_pos"), Some(&PyRep::Real(0.0001)));
    assert_eq!(unpacked.get("min_neg"), Some(&PyRep::Real(-0.0001)));
    if let Some(PyRep::Real(z)) = unpacked.get("round_down") {
        assert_eq!(*z, 0.0);
        assert!(z.is_sign_positive());
    } else {
        panic!("expected Real(0.0)");
    }
    assert_eq!(unpacked.get("round_up_pos"), Some(&PyRep::Real(0.0001)));
    assert_eq!(unpacked.get("round_up_neg"), Some(&PyRep::Real(-0.0001)));
    assert_eq!(
        unpacked.get("sat_pos_f64"),
        Some(&PyRep::Real((i64::MAX as f64) / 10_000.0))
    );
    assert_eq!(
        unpacked.get("sat_neg_f64"),
        Some(&PyRep::Real((i64::MIN as f64) / 10_000.0))
    );
}

#[test]
fn dbrow_zero_header_empty_column_and_variable_bytes_wstr_roundtrip() {
    assert_eq!(packed_row_expected_bytes(&[]), 0);
    let empty_desc = DBRowDescriptor::new(Vec::<(&str, DbColumnType)>::new());
    let empty_hdr = empty_desc.to_pyrep();
    assert_eq!(
        try_unpack_packed_row_columns(&empty_hdr, &[]).unwrap(),
        Vec::new()
    );
    assert_eq!(
        try_unpack_packed_row_columns(&PyRep::None, &[1, 2, 3]).unwrap(),
        Vec::new()
    );

    let var_desc = DBRowDescriptor::new([
        ("empty_col", DbColumnType::Empty),
        ("bytes_col", DbColumnType::Bytes),
        ("str_col", DbColumnType::Str),
        ("wstr_col", DbColumnType::WStr),
        ("flag_col", DbColumnType::Bool),
    ]);
    let row = var_desc.build_dbrow(vec![
        PyRep::Integer(999), // normalized to None for Empty
        PyRep::Buffer(vec![0xDE, 0xAD]),
        PyRep::Token("ascii.token".into()),
        PyRep::Bytes(b"wide-utf8".to_vec()),
        PyRep::Bool(true),
    ]);
    let wire = crate::marshal::encode(&row);
    let decoded = crate::marshal::decode(&wire).unwrap();
    assert_eq!(decoded.dbrow_get("empty_col"), Some(&PyRep::None));
    assert_eq!(
        decoded.dbrow_get("bytes_col"),
        Some(&PyRep::Bytes(vec![0xDE, 0xAD]))
    );
    assert_eq!(
        decoded.dbrow_get("str_col"),
        Some(&PyRep::String("ascii.token".into()))
    );
    assert_eq!(
        decoded.dbrow_get("wstr_col"),
        Some(&PyRep::String("wide-utf8".into()))
    );
    assert_eq!(decoded.dbrow_get("flag_col"), Some(&PyRep::Bool(true)));
}

#[test]
fn dbrow_short_rows_missing_key_column_null_offset_alignment_and_bounds() {
    let desc = DBRowDescriptor::new([
        ("id", DbColumnType::I4),
        ("flag", DbColumnType::Bool),
        ("score", DbColumnType::R8),
        ("empty_col", DbColumnType::Empty),
        ("tag", DbColumnType::WStr),
    ]);

    // build_dbrow with short values pads missing columns with None, extra values ignored
    let short_row = desc.build_dbrow(vec![PyRep::Integer(42)]);
    if let PyRep::DBRow { columns, raw_rle, .. } = &short_row {
        assert_eq!(columns.len(), 5);
        assert_eq!(columns[0], ("id".into(), PyRep::Integer(42)));
        assert_eq!(columns[1], ("flag".into(), PyRep::None));
        assert_eq!(columns[2], ("score".into(), PyRep::None));
        assert_eq!(columns[3], ("empty_col".into(), PyRep::None));
        assert_eq!(columns[4], ("tag".into(), PyRep::None));
        let unpacked = try_unpack_packed_row_columns(&desc.to_pyrep(), raw_rle).unwrap();
        assert_eq!(unpacked[0], ("id".into(), PyRep::Integer(42)));
        assert_eq!(unpacked[2], ("score".into(), PyRep::None));
    } else {
        panic!("expected PyRep::DBRow");
    }

    // Interleaved null wide column + non-null narrower column verifies offset advances for null fixed-width cols
    let align_desc = DBRowDescriptor::new([
        ("wide_null", DbColumnType::I8),
        ("wide_val", DbColumnType::Ui8),
        ("mid_null", DbColumnType::I4),
        ("mid_val", DbColumnType::R4),
        ("short_null", DbColumnType::I2),
        ("short_val", DbColumnType::Ui2),
        ("byte_null", DbColumnType::I1),
        ("byte_val", DbColumnType::Ui1),
    ]);
    let align_hdr = align_desc.to_pyrep();
    let align_rle = pack_packed_row_columns(
        &align_hdr,
        &[
            ("wide_val".into(), PyRep::Integer(1_234_567_890)),
            ("mid_val".into(), PyRep::Real(3.5)),
            ("short_val".into(), PyRep::Integer(40_000)),
            ("byte_val".into(), PyRep::Integer(200)),
        ],
    );
    let align_dec: std::collections::HashMap<_, _> =
        try_unpack_packed_row_columns(&align_hdr, &align_rle)
            .unwrap()
            .into_iter()
            .collect();
    assert_eq!(align_dec["wide_null"], PyRep::None);
    assert_eq!(align_dec["wide_val"], PyRep::Integer(1_234_567_890));
    assert_eq!(align_dec["mid_null"], PyRep::None);
    assert_eq!(align_dec["mid_val"], PyRep::Real(3.5));
    assert_eq!(align_dec["short_null"], PyRep::None);
    assert_eq!(align_dec["short_val"], PyRep::Integer(40_000));
    assert_eq!(align_dec["byte_null"], PyRep::None);
    assert_eq!(align_dec["byte_val"], PyRep::Integer(200));

    // build_cindexed_rowset and build_index_rowset fallback to col 0 on unknown key_column, and empty row -> None
    let cidx = desc.build_cindexed_rowset("nonexistent", vec![vec![], vec![PyRep::Integer(99)]]);
    let entries = cidx.cindexed_rowset_entries().expect("cindexed entries");
    assert_eq!(entries[0].0, PyRep::None);
    assert_eq!(entries[1].0, PyRep::Integer(99));

    let idx_rs = desc.build_index_rowset("nonexistent", vec![vec![]]);
    if let PyRep::IndexRowset { items, .. } = idx_rs {
        assert_eq!(items[0].0, PyRep::None);
    } else {
        panic!("expected IndexRowset");
    }

    // packed_row_expected_bytes boundaries
    assert_eq!(packed_row_expected_bytes(&[]), 0);
    assert_eq!(packed_row_expected_bytes(&[("a".into(), 20)]), 9); // 8 bytes + ((0+1)>>3)+1 = 9
    let seven_bools: Vec<(String, i64)> = (0..7).map(|i| (format!("b{i}"), 11)).collect();
    // 7 bools + 7 null bits = 14 bits -> (14 >> 3) + 1 = 2 bytes
    assert_eq!(packed_row_expected_bytes(&seven_bools), 2);
    let four_bools: Vec<(String, i64)> = (0..4).map(|i| (format!("b{i}"), 11)).collect();
    // 4 bools + 4 null bits = 8 bits -> (8 >> 3) + 1 = 2 bytes
    assert_eq!(packed_row_expected_bytes(&four_bools), 2);
    let three_bools: Vec<(String, i64)> = (0..3).map(|i| (format!("b{i}"), 11)).collect();
    // 3 bools + 3 null bits = 6 bits -> (6 >> 3) + 1 = 1 byte
    assert_eq!(packed_row_expected_bytes(&three_bools), 1);

    // try_unpack_packed_row_columns: invalid header -> Ok(empty), empty header -> Ok(empty), oversized -> CollectionTooLarge
    assert_eq!(try_unpack_packed_row_columns(&PyRep::None, &[]), Ok(Vec::new()));
    assert_eq!(
        try_unpack_packed_row_columns(&DBRowDescriptor::new(Vec::<(String, DbColumnType)>::new()).to_pyrep(), &[]),
        Ok(Vec::new())
    );
    let huge_cols: Vec<(String, DbColumnType)> =
        (0..16_400).map(|i| (format!("c{i}"), DbColumnType::I8)).collect();
    let huge_hdr = DBRowDescriptor::new(huge_cols).to_pyrep();
    assert!(matches!(
        try_unpack_packed_row_columns(&huge_hdr, &[]),
        Err(ProtoError::CollectionTooLarge(_))
    ));

    // decode_rle second-nibble overflow / truncation
    // Byte 0xF8 followed by 0x88: first nibble = 8 (1 zero), second nibble = 15 (8 zeros) -> 9 zeros > expected_len 4
    assert_eq!(decode_rle(&[0xF8, 0x88], 4), None);
    // Byte 0x18 followed by 0xAA: first nibble = 8 (1 zero), second nibble = 1 (7 literals) with only 1 trailing byte
    assert_eq!(decode_rle(&[0x18, 0xAA], 10), None);
}
