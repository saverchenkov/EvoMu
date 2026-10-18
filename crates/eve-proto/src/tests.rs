//! Unit and golden round-trip tests for `eve-proto` (`P-1`, `P-2`).

use super::*;

#[test]
fn test_marshal_roundtrip_composite_and_bytes() {
    let original = PyRep::Tuple(vec![
        PyRep::None,
        PyRep::Bool(true),
        PyRep::Integer(30000142),
        PyRep::Real(149_597_870_700.0),
        PyRep::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]), // Non-UTF8 binary byte string
        PyRep::Token("macho.CallReq".to_string()),
        PyRep::String("Jita IV - Moon 4".to_string()),
        PyRep::Dict(vec![(
            PyRep::String("solarSystemID".to_string()),
            PyRep::Integer(30000142),
        )]),
    ]);

    let bytes = marshal::encode(&original);
    let decoded = marshal::decode(&bytes).expect("marshal decode should succeed");
    assert_eq!(original, decoded);
    assert_eq!(bytes, marshal::encode(&decoded));
}

#[test]
fn test_saved_stream_reference_map_and_bomb_protection() {
    let mut raw = vec![
        MARSHAL_HEADER_BYTE,
        1,
        0,
        0,
        0, // save_count = 1
        MarshalOpcode::PyTwoTuple as u8,
        (MarshalOpcode::PyToken as u8) | SAVE_FLAG, // 0x02 | 0x40
        6,
        b's',
        b'h',
        b'a',
        b'r',
        b'e',
        b'd',
        MarshalOpcode::PySavedStreamElement as u8,
        1, // Reference slot 1
    ];
    raw.extend_from_slice(&1u32.to_le_bytes()); // Trailing save map: slot 1

    let decoded = marshal::decode(&raw).unwrap();
    assert_eq!(
        decoded,
        PyRep::Tuple(vec![
            PyRep::Token("shared".into()),
            PyRep::Token("shared".into()),
        ])
    );
}

#[test]
fn test_keyval_rowset_and_objectex_roundtrip() {
    let kv = PyRep::keyval([
        ("solarSystemID", PyRep::Integer(30000142)),
        ("security", PyRep::Real(0.95)),
    ]);
    let kv_bytes = marshal::encode(&kv);
    assert_eq!(marshal::decode(&kv_bytes).unwrap(), kv);

    let rs = PyRep::rowset(
        vec!["characterID", "characterName"],
        vec![vec![
            PyRep::Integer(90_000_001),
            PyRep::String("Aura".into()),
        ]],
    );
    let rs_bytes = marshal::encode(&rs);
    assert_eq!(marshal::decode(&rs_bytes).unwrap(), rs);

    let obj_ex = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![PyRep::Token(
            "blue.DBRowDescriptor".into(),
        )])),
        list: vec![PyRep::Integer(1)],
        dict: vec![(PyRep::String("k".into()), PyRep::Integer(42))],
    };
    let ex_bytes = marshal::encode(&obj_ex);
    assert_eq!(marshal::decode(&ex_bytes).unwrap(), obj_ex);
}

#[test]
fn test_zlib_roundtrip_and_trailing_garbage_rejection() {
    let val = PyRep::SubStream(Box::new(PyRep::Integer(123456)));
    let compressed = marshal::encode_zlib(&val).unwrap();
    assert_eq!(compressed[0], ZLIB_HEADER_BYTE);
    assert_eq!(marshal::decode(&compressed).unwrap(), val);

    let mut with_garbage = marshal::encode(&val);
    with_garbage.push(0x99);
    assert_eq!(
        marshal::decode(&with_garbage),
        Err(ProtoError::TrailingBytes(1))
    );
}

#[test]
fn test_collection_and_recursion_bounds() {
    let mut forged = vec![
        MARSHAL_HEADER_BYTE,
        0,
        0,
        0,
        0,
        MarshalOpcode::PyTuple as u8,
        0xFF,
    ];
    forged.extend_from_slice(&1_000_000u32.to_le_bytes());
    assert_eq!(
        marshal::decode(&forged),
        Err(ProtoError::CollectionTooLarge(1_000_000))
    );

    let mut deep = vec![MARSHAL_HEADER_BYTE, 0, 0, 0, 0];
    deep.extend(std::iter::repeat_n(
        MarshalOpcode::PyOneTuple as u8,
        MAX_RECURSION_DEPTH + 5,
    ));
    deep.push(MarshalOpcode::PyNone as u8);
    assert_eq!(
        marshal::decode(&deep),
        Err(ProtoError::RecursionLimitExceeded)
    );
}

#[test]
fn test_p1_packed_row_rle_booleans_nulls_and_trailing_strings() {
    // parity: plan:P-1
    let descriptor = DBRowDescriptor::new([
        ("itemID", DbColumnType::I8),
        ("typeID", DbColumnType::I4),
        ("flagID", DbColumnType::I2),
        ("singleton", DbColumnType::Bool),
        ("contraband", DbColumnType::Bool),
        ("optionalOwnerID", DbColumnType::I4),
        ("price", DbColumnType::R8),
        ("damage", DbColumnType::R4),
        ("customInfo", DbColumnType::Str),
        ("itemName", DbColumnType::WStr),
        ("rawBlob", DbColumnType::Bytes),
        ("nullText", DbColumnType::WStr),
    ]);

    let row = descriptor.build_dbrow(vec![
        PyRep::Integer(100_000_001),
        PyRep::Integer(587),
        PyRep::Integer(4),
        PyRep::Bool(true),
        PyRep::Bool(false),
        PyRep::None, // Null fixed-width I4 column
        PyRep::Real(1_250_000.5),
        PyRep::Real(0.25),
        PyRep::String("fitted:rifter".into()),
        PyRep::String("Rifter Alpha".into()),
        PyRep::Bytes(vec![0xCA, 0xFE, 0x00, 0x42]),
        PyRep::None, // Null trailing WSTR column
    ]);

    let encoded = marshal::encode(&row);
    let decoded = marshal::decode(&encoded).expect("PyPackedRow decode should succeed");
    assert_eq!(decoded, row);
    assert_eq!(
        decoded.dbrow_get("itemID"),
        Some(&PyRep::Integer(100_000_001))
    );
    assert_eq!(decoded.dbrow_get("typeID"), Some(&PyRep::Integer(587)));
    assert_eq!(decoded.dbrow_get("flagID"), Some(&PyRep::Integer(4)));
    assert_eq!(decoded.dbrow_get("singleton"), Some(&PyRep::Bool(true)));
    assert_eq!(decoded.dbrow_get("contraband"), Some(&PyRep::Bool(false)));
    assert_eq!(decoded.dbrow_get("optionalOwnerID"), Some(&PyRep::None));
    assert_eq!(decoded.dbrow_get("price"), Some(&PyRep::Real(1_250_000.5)));
    assert_eq!(decoded.dbrow_get("damage"), Some(&PyRep::Real(0.25)));
    assert_eq!(
        decoded.dbrow_get("customInfo"),
        Some(&PyRep::String("fitted:rifter".into()))
    );
    assert_eq!(
        decoded.dbrow_get("itemName"),
        Some(&PyRep::String("Rifter Alpha".into()))
    );
    assert_eq!(
        decoded.dbrow_get("rawBlob"),
        Some(&PyRep::Bytes(vec![0xCA, 0xFE, 0x00, 0x42]))
    );
    assert_eq!(decoded.dbrow_get("nullText"), Some(&PyRep::None));
}

#[test]
fn test_p1_packed_row_in_tuple_does_not_corrupt_subsequent_elements_of_21() {
    // parity: plan:P-1
    let descriptor = DBRowDescriptor::inv_item_header();
    let row1 = descriptor.build_dbrow(vec![
        PyRep::Integer(100_000_001),
        PyRep::Integer(587),
        PyRep::Integer(90_000_001),
        PyRep::Integer(60_003_760),
        PyRep::Integer(4),
        PyRep::Integer(-1),
        PyRep::Integer(25),
        PyRep::Integer(6),
        PyRep::String("custom-tag-alpha".into()),
    ]);
    let row2 = descriptor.build_dbrow(vec![
        PyRep::Integer(100_000_002),
        PyRep::Integer(34),
        PyRep::Integer(90_000_001),
        PyRep::Integer(60_003_760),
        PyRep::Integer(4),
        PyRep::Integer(50_000),
        PyRep::Integer(18),
        PyRep::Integer(4),
        PyRep::String("".into()),
    ]);

    // Embed two PyPackedRows with trailing STR columns inside a surrounding Tuple alongside trailing elements
    let surrounding = PyRep::Tuple(vec![
        PyRep::String("before-marker".into()),
        row1.clone(),
        row2.clone(),
        PyRep::Integer(999_888_777),
        PyRep::String("after-marker".into()),
    ]);

    let wire = marshal::encode(&surrounding);
    let decoded = marshal::decode(&wire).expect("Tuple containing PyPackedRows must decode cleanly");
    assert_eq!(decoded, surrounding);

    // Also verify double-nested Tuple header shape (Shape 2 in extract_dbrow_col_defs)
    let double_nested_header = PyRep::Tuple(vec![
        PyRep::Token("blue.DBRowDescriptor".into()),
        PyRep::Tuple(vec![PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::String("id".into()), PyRep::Integer(20)]),
            PyRep::Tuple(vec![PyRep::String("label".into()), PyRep::Integer(130)]),
        ])]),
    ]);
    let manual_row = PyRep::DBRow {
        header: Box::new(double_nested_header),
        columns: vec![
            ("id".into(), PyRep::Integer(42)),
            ("label".into(), PyRep::String("Jita".into())),
        ],
        raw_rle: Vec::new(), // triggers on-the-fly packing in encode
    };
    let wrapped = PyRep::Tuple(vec![manual_row, PyRep::Integer(777)]);
    let wrapped_wire = marshal::encode(&wrapped);
    let PyRep::Tuple(items) = marshal::decode(&wrapped_wire).unwrap() else {
        panic!("Expected Tuple");
    };
    assert_eq!(items[0].dbrow_get("id"), Some(&PyRep::Integer(42)));
    assert_eq!(
        items[0].dbrow_get("label"),
        Some(&PyRep::String("Jita".into()))
    );
    assert_eq!(items[1], PyRep::Integer(777));
}

#[test]
fn test_p2_canonical_row_containers_golden_roundtrip() {
    let desc = DBRowDescriptor::new([
        ("orderID", DbColumnType::I8),
        ("typeID", DbColumnType::I4),
        ("price", DbColumnType::R8),
        ("bid", DbColumnType::Bool),
        ("stationName", DbColumnType::WStr),
    ]);

    let sample_rows = vec![
        vec![
            PyRep::Integer(700_000_001),
            PyRep::Integer(34),
            PyRep::Real(5.25),
            PyRep::Bool(false),
            PyRep::String("Jita IV - Moon 4".into()),
        ],
        vec![
            PyRep::Integer(700_000_002),
            PyRep::Integer(35),
            PyRep::Real(12.50),
            PyRep::Bool(true),
            PyRep::String("Amarr VIII".into()),
        ],
    ];

    // 1. DBRow (blue.DBRow / PyPackedRow) + verify mutating columns re-packs RLE on encode
    let mut dbrow = desc.build_dbrow(sample_rows[0].clone());
    let dbrow_bytes = marshal::encode(&dbrow);
    let dbrow_decoded = marshal::decode(&dbrow_bytes).unwrap();
    assert_eq!(dbrow_decoded, dbrow);
    assert_eq!(marshal::encode(&dbrow_decoded), dbrow_bytes);
    if let PyRep::DBRow { columns, .. } = &mut dbrow {
        columns[1].1 = PyRep::Integer(36);
    }
    let mutated_decoded = marshal::decode(&marshal::encode(&dbrow)).unwrap();
    assert_eq!(mutated_decoded.dbrow_get("typeID"), Some(&PyRep::Integer(36)));

    // 2. CRowset (dbutil.CRowset) — verify ((Token("dbutil.CRowset"),), Dict) type-2 ObjectEx header
    let crowset = desc.build_crowset(sample_rows.clone());
    assert!(crowset.is_crowset());
    assert_eq!(crowset.row_count(), Some(2));
    if let PyRep::ObjectEx { is_type_2, header, .. } = &crowset {
        assert!(*is_type_2);
        let PyRep::Tuple(hdr_items) = header.as_ref() else {
            panic!("Expected Tuple header on CRowset");
        };
        assert_eq!(
            hdr_items.first(),
            Some(&PyRep::Tuple(vec![PyRep::Token("dbutil.CRowset".into())]))
        );
    } else {
        panic!("Expected ObjectEx for CRowset");
    }
    let crowset_bytes = marshal::encode(&crowset);
    let crowset_decoded = marshal::decode(&crowset_bytes).unwrap();
    assert_eq!(crowset_decoded, crowset);
    assert_eq!(marshal::encode(&crowset_decoded), crowset_bytes);

    // 3. CIndexedRowset (dbutil.CIndexedRowset) — verify ((Token("dbutil.CIndexedRowset"),), Dict) type-2 ObjectEx header
    let cindexed = desc.build_cindexed_rowset("typeID", sample_rows.clone());
    assert!(cindexed.is_cindexed_rowset());
    assert_eq!(cindexed.row_count(), Some(2));
    if let PyRep::ObjectEx { is_type_2, header, .. } = &cindexed {
        assert!(*is_type_2);
        let PyRep::Tuple(hdr_items) = header.as_ref() else {
            panic!("Expected Tuple header on CIndexedRowset");
        };
        assert_eq!(
            hdr_items.first(),
            Some(&PyRep::Tuple(vec![PyRep::Token(
                "dbutil.CIndexedRowset".into()
            )]))
        );
    } else {
        panic!("Expected ObjectEx for CIndexedRowset");
    }
    let cindexed_bytes = marshal::encode(&cindexed);
    let cindexed_decoded = marshal::decode(&cindexed_bytes).unwrap();
    assert_eq!(cindexed_decoded, cindexed);
    assert_eq!(marshal::encode(&cindexed_decoded), cindexed_bytes);

    // 4. util.Rowset
    let u_rowset = desc.build_rowset(sample_rows.clone());
    assert_eq!(u_rowset.row_count(), Some(2));
    let u_rowset_bytes = marshal::encode(&u_rowset);
    let u_rowset_decoded = marshal::decode(&u_rowset_bytes).unwrap();
    assert_eq!(u_rowset_decoded, u_rowset);
    assert_eq!(marshal::encode(&u_rowset_decoded), u_rowset_bytes);

    // 5. util.IndexRowset
    let u_index_rowset = desc.build_index_rowset("orderID", sample_rows.clone());
    assert_eq!(u_index_rowset.row_count(), Some(2));
    let u_index_bytes = marshal::encode(&u_index_rowset);
    let u_index_decoded = marshal::decode(&u_index_bytes).unwrap();
    assert_eq!(u_index_decoded, u_index_rowset);
    assert_eq!(marshal::encode(&u_index_decoded), u_index_bytes);

    // 6. PackedRowList & PackedRowListTuple
    let packed_list = desc.build_packed_row_list(sample_rows.clone());
    let packed_list_bytes = marshal::encode(&packed_list);
    let packed_list_decoded = marshal::decode(&packed_list_bytes).unwrap();
    assert_eq!(packed_list_decoded, packed_list);
    assert_eq!(marshal::encode(&packed_list_decoded), packed_list_bytes);

    let packed_tuple = desc.build_packed_row_list_tuple(sample_rows);
    let packed_tuple_bytes = marshal::encode(&packed_tuple);
    let packed_tuple_decoded = marshal::decode(&packed_tuple_bytes).unwrap();
    assert_eq!(packed_tuple_decoded, packed_tuple);
    assert_eq!(marshal::encode(&packed_tuple_decoded), packed_tuple_bytes);
}
