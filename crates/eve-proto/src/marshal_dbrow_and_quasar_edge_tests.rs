//! Edge-case and mutation-killing unit tests for `dbrow::packed_row_expected_bytes`,
//! `marshal::decode_with_limits` (depth, inflation, and packed-row size limits), and
//! `quasar::registry::QuasarDescriptorRegistry::decode_dynamic` multi-chunk packed fields.

use crate::dbrow::packed_row_expected_bytes;
use crate::marshal;
use crate::quasar::registry::{DynamicValue, QuasarDescriptorRegistry};
use crate::quasar::{DescriptorProto, FieldDescriptorProto, FieldLabel, FieldType};
use crate::{
    MarshalOpcode, ProtoError, PyRep, MARSHAL_HEADER_BYTE, MAX_COLLECTION_LEN, MAX_RECURSION_DEPTH,
};

fn wrap_in_one_tuples(mut inner: PyRep, count: usize) -> PyRep {
    for _ in 0..count {
        inner = PyRep::Tuple(vec![inner]);
    }
    inner
}

fn encode_zlib_raw(raw: &[u8]) -> Vec<u8> {
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::io::Write;

    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(raw).unwrap();
    enc.finish().unwrap()
}

fn build_packed_row_stream_with_col_defs(defs: &[(&str, i64)]) -> Vec<u8> {
    let cols_rep = PyRep::Tuple(
        defs.iter()
            .map(|(name, dbtype)| {
                PyRep::Tuple(vec![PyRep::String((*name).to_string()), PyRep::Integer(*dbtype)])
            })
            .collect(),
    );
    let header = PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::Token("blue.DBRowDescriptor".into())]),
            PyRep::Tuple(vec![cols_rep]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    let row = PyRep::DBRow {
        header: Box::new(header),
        columns: Vec::new(),
        raw_rle: Vec::new(),
    };
    marshal::encode(&row)
}

#[test]
fn packed_row_expected_bytes_exact_bit_and_byte_arithmetic() {
    assert_eq!(packed_row_expected_bytes(&[]), 0);

    // 8 booleans (`dbtype == 11`, bit width 1 < 8):
    // byte_data_bit_len = 0, bool_bit_len = 8, defs.len() = 8 -> (0 >> 3) + ((8 + 8) >> 3) + 1 = 3.
    let eight_bools: Vec<(String, i64)> = (0..8).map(|i| (format!("b{i}"), 11)).collect();
    assert_eq!(packed_row_expected_bytes(&eight_bools), 3);

    // 1 i64 (`dbtype == 20`, bit width 64 >= 8) + 8 booleans (`dbtype == 11`):
    // byte_data_bit_len = 64 (>> 3 = 8), bool_bit_len = 8, defs.len() = 9 -> 8 + ((8 + 9) >> 3) + 1 = 11.
    let mut mixed = vec![("id".to_string(), 20)];
    mixed.extend(eight_bools);
    assert_eq!(packed_row_expected_bytes(&mixed), 11);
}

#[test]
fn marshal_decode_limits_inflate_budget_varint_string_table_and_packed_row_bounds() {
    // 1. Exact `input.len() == max_inflate_bytes` succeeds (`>` vs `>=` in `decode_with_limits`).
    let simple_raw = marshal::encode(&PyRep::Integer(42));
    assert_eq!(
        marshal::decode_with_limits(&simple_raw, simple_raw.len(), 16).unwrap(),
        PyRep::Integer(42)
    );

    // 2. Exact `out.len() == remaining_inflate_budget` succeeds (`>` vs `>=` in `decode_stream_slice`).
    let text_rep = PyRep::String("A".repeat(200));
    let text_raw = marshal::encode(&text_rep);
    let text_zlib = marshal::encode_zlib(&text_rep).unwrap();
    assert!(text_zlib.len() < text_raw.len());
    assert_eq!(
        marshal::decode_with_limits(&text_zlib, text_raw.len(), 16).unwrap(),
        text_rep
    );

    // 3. Nested zlib `PySubStream` checks subtraction (`-=`) vs division (`/=`) on `remaining_inflate_budget`.
    //    `inner_raw` is 127 bytes (`inner_zlib` ~19 bytes), `outer_raw` is ~26 bytes -> total ~153 bytes.
    //    With budget = 160: `160 - 26 = 134 >= 127` (Ok), whereas `160 / 26 = 6 < 127` (FrameTooLarge).
    let inner_rep = PyRep::String("B".repeat(120));
    let inner_zlib = marshal::encode_zlib(&inner_rep).unwrap();
    let mut outer_raw = vec![MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PySubStream as u8];
    outer_raw.push(inner_zlib.len() as u8);
    outer_raw.extend_from_slice(&inner_zlib);
    let outer_zlib = encode_zlib_raw(&outer_raw);
    assert_eq!(
        marshal::decode_with_limits(&outer_zlib, 165, 32).unwrap(),
        PyRep::SubStream(Box::new(inner_rep))
    );

    // 4. `PyVarInteger` followed by another element (`input.len() > len`, testing `<` vs `>` guard).
    let varint_tuple = PyRep::Tuple(vec![PyRep::Integer(0x12_3456), PyRep::Integer(1)]);
    let varint_raw = marshal::encode(&varint_tuple);
    assert_eq!(
        marshal::decode_with_limits(&varint_raw, 1024, 32).unwrap(),
        varint_tuple
    );

    // 5. `PyStringTableItem` with `remaining_budget == 0` after root node (`s.len() / 64 == 0`).
    let str_table_stream = [MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PyStringTableItem as u8, 1];
    assert_eq!(
        marshal::decode_with_limits(&str_table_stream, 64, 1).unwrap(),
        PyRep::String("*corpid".into())
    );

    // 6. `PyPackedRow` `expected_bytes == MAX_COLLECTION_LEN` (passes line 697) vs `MAX_COLLECTION_LEN + 1`
    //    (returns `Err(ProtoError::CollectionTooLarge(MAX_COLLECTION_LEN + 1))`).
    let mut exact_defs = vec![("c", 20_i64); 16_131];
    exact_defs.push(("i", 3)); // 32 bits
    exact_defs.extend_from_slice(&[("b1", 16), ("b2", 16), ("b3", 16)]); // 3 * 8 bits -> 131_072 bytes
    let exact_stream = build_packed_row_stream_with_col_defs(&exact_defs);
    assert!(!matches!(
        marshal::decode_with_limits(&exact_stream, 4 * 1024 * 1024, 2_000_000),
        Err(ProtoError::CollectionTooLarge(_))
    ));

    let over_defs = vec![("c", 20_i64); 16_132]; // 131_073 bytes = MAX_COLLECTION_LEN + 1
    let over_stream = build_packed_row_stream_with_col_defs(&over_defs);
    assert_eq!(
        marshal::decode_with_limits(&over_stream, 4 * 1024 * 1024, 2_000_000),
        Err(ProtoError::CollectionTooLarge(MAX_COLLECTION_LEN + 1))
    );
}

#[test]
fn marshal_recursion_depth_checked_across_all_container_and_stream_children() {
    let deep_leaf = wrap_in_one_tuples(PyRep::None, MAX_RECURSION_DEPTH);

    let cases: Vec<PyRep> = vec![
        // PyPackedRow header
        PyRep::DBRow {
            header: Box::new(deep_leaf.clone()),
            columns: Vec::new(),
            raw_rle: Vec::new(),
        },
        // PyTwoTuple element 0 and element 1
        PyRep::Tuple(vec![deep_leaf.clone(), PyRep::None]),
        PyRep::Tuple(vec![PyRep::None, deep_leaf.clone()]),
        // PyDict value and key
        PyRep::Dict(vec![(PyRep::None, deep_leaf.clone())]),
        PyRep::Dict(vec![(deep_leaf.clone(), PyRep::None)]),
        // PyObject type_rep (at depth MAX_RECURSION_DEPTH so child is 65 vs 64) and args
        wrap_in_one_tuples(
            PyRep::Object {
                type_name: "x".into(),
                args: Box::new(PyRep::None),
            },
            MAX_RECURSION_DEPTH,
        ),
        PyRep::Object {
            type_name: "x".into(),
            args: Box::new(deep_leaf.clone()),
        },
        // PyObjectEx header, dict key, and dict value
        PyRep::ObjectEx {
            is_type_2: false,
            header: Box::new(deep_leaf.clone()),
            list: Vec::new(),
            dict: Vec::new(),
        },
        PyRep::ObjectEx {
            is_type_2: false,
            header: Box::new(PyRep::None),
            list: Vec::new(),
            dict: vec![(deep_leaf.clone(), PyRep::None)],
        },
        PyRep::ObjectEx {
            is_type_2: false,
            header: Box::new(PyRep::None),
            list: Vec::new(),
            dict: vec![(PyRep::None, deep_leaf.clone())],
        },
        // PySubStruct, PyChecksumedStream, and PySubStream
        PyRep::SubStruct(Box::new(deep_leaf.clone())),
        PyRep::ChecksumedStream {
            checksum: 0xDEAD_BEEF,
            body: Box::new(deep_leaf.clone()),
        },
        PyRep::SubStream(Box::new(deep_leaf)),
    ];

    for (idx, case) in cases.iter().enumerate() {
        let encoded = marshal::encode(case);
        let res = marshal::decode_with_limits(&encoded, 1024 * 1024, 100_000);
        assert_eq!(
            res,
            Err(ProtoError::RecursionLimitExceeded),
            "recursion case {idx} must reject depth > {MAX_RECURSION_DEPTH}"
        );
    }
}

#[test]
fn quasar_decode_dynamic_appends_multiple_packed_chunks_for_same_repeated_field() {
    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();
    reg.messages_by_full_name.insert(
        "test.MultiPacked".into(),
        DescriptorProto {
            name: "MultiPacked".into(),
            full_name: "test.MultiPacked".into(),
            fields: vec![FieldDescriptorProto {
                name: "nums".into(),
                number: 1,
                label: FieldLabel::Repeated,
                field_type: FieldType::Int32,
                type_name: String::new(),
                oneof_index: None,
                json_name: "nums".into(),
            }],
            nested_types: Vec::new(),
            enum_types: Vec::new(),
            oneofs: Vec::new(),
            is_map_entry: false,
        },
    );

    // Two separate wire-type-2 packed chunks for field #1 (`tag = (1 << 3) | 2 = 0x0A`).
    let wire = [0x0A, 0x02, 10, 20, 0x0A, 0x02, 30, 40];
    let decoded = reg.decode_dynamic("test.MultiPacked", &wire).unwrap();
    assert_eq!(
        decoded.fields.get(&1),
        Some(&DynamicValue::Repeated(vec![
            DynamicValue::Int32(10),
            DynamicValue::Int32(20),
            DynamicValue::Int32(30),
            DynamicValue::Int32(40),
        ]))
    );
}

#[test]
fn marshal_is_zlib_header_rejections_and_decode_error_arms_exhaustive() {
    use crate::dbrow::{DBRowDescriptor, DbColumnType};

    // 1. `is_zlib_stream_header` boundary rejections (marshal.rs L321-331)
    assert_eq!(marshal::decode(&[0x58]), Err(ProtoError::InvalidHeader(0x58)));
    assert_eq!(
        marshal::decode(&[0x88, 0x1C]),
        Err(ProtoError::InvalidHeader(0x88))
    );
    assert_eq!(
        marshal::decode(&[0x79, 0x18]),
        Err(ProtoError::InvalidHeader(0x79))
    );
    assert_eq!(
        marshal::decode(&[0x58, 0x00]),
        Err(ProtoError::InvalidHeader(0x58))
    );

    // 2. Inflated empty payload -> UnexpectedEof (L401)
    let empty_zlib = encode_zlib_raw(&[]);
    assert_eq!(marshal::decode(&empty_zlib), Err(ProtoError::UnexpectedEof));

    // 3. Post-deflate inflation exceeding max_inflate_bytes -> FrameTooLarge (L390-392)
    let raw_65 = marshal::encode(&PyRep::Bytes(vec![b'a'; 58]));
    assert!(raw_65.len() > 30);
    let comp_65 = encode_zlib_raw(&raw_65);
    assert!(comp_65.len() <= 30);
    assert_eq!(
        marshal::decode_with_limits(&comp_65, 30, 100),
        Err(ProtoError::FrameTooLarge(31))
    );

    // 4. `decode_stream_slice` depth > MAX_RECURSION_DEPTH via PySubStream (L373) & empty PySubStream (L376)
    let mut sub_leaf = PyRep::None;
    for _ in 0..=MAX_RECURSION_DEPTH {
        sub_leaf = PyRep::SubStream(Box::new(sub_leaf));
    }
    let deep_sub_raw = marshal::encode(&sub_leaf);
    assert_eq!(
        marshal::decode_with_limits(&deep_sub_raw, 1024 * 1024, 10_000),
        Err(ProtoError::RecursionLimitExceeded)
    );

    assert_eq!(
        marshal::decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PySubStream as u8, 0]),
        Err(ProtoError::UnexpectedEof)
    );
    assert_eq!(
        marshal::decode(&[
            MARSHAL_HEADER_BYTE,
            0,
            0,
            0,
            0,
            MarshalOpcode::PySubStream as u8,
            10,
            MARSHAL_HEADER_BYTE
        ]),
        Err(ProtoError::UnexpectedEof)
    );

    // 5. `decode_inner` empty body (L457), SAVE_FLAG empty map (L469), token/wstring truncations & UTF-8 (L488, L493, L747)
    assert_eq!(
        marshal::decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0]),
        Err(ProtoError::UnexpectedEof)
    );
    assert_eq!(
        marshal::decode(&[
            MARSHAL_HEADER_BYTE,
            0,
            0,
            0,
            0,
            (MarshalOpcode::PyNone as u8) | crate::SAVE_FLAG
        ]),
        Err(ProtoError::InvalidSaveReference(0))
    );
    assert_eq!(
        marshal::decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PyToken as u8, 5, b'a']),
        Err(ProtoError::UnexpectedEof)
    );
    assert!(matches!(
        marshal::decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PyToken as u8, 1, 0xFF]),
        Err(ProtoError::Utf8(_))
    ));
    assert!(matches!(
        marshal::decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PyWStringUTF8 as u8, 1, 0xFF]),
        Err(ProtoError::Utf8(_))
    ));

    // 6. Fixed integer truncations (L502, L510, L518, L526) & PyVarInteger edges
    for (op, tail) in [
        (MarshalOpcode::PyByte as u8, &[][..]),
        (MarshalOpcode::PySignedShort as u8, &[1][..]),
        (MarshalOpcode::PyLong as u8, &[1, 2, 3][..]),
        (MarshalOpcode::PyLongLong as u8, &[1, 2, 3, 4, 5, 6, 7][..]),
    ] {
        let mut s = vec![MARSHAL_HEADER_BYTE, 0, 0, 0, 0, op];
        s.extend_from_slice(tail);
        assert_eq!(marshal::decode(&s), Err(ProtoError::UnexpectedEof));
    }

    assert_eq!(
        marshal::decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PyVarInteger as u8, 0]),
        Ok(PyRep::Integer(0))
    );
    assert_eq!(
        marshal::decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PyVarInteger as u8, 10]),
        Err(ProtoError::UnexpectedEof)
    );
    assert_eq!(
        marshal::decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PyVarInteger as u8, 5, 1, 2]),
        Err(ProtoError::UnexpectedEof)
    );
    assert_eq!(
        marshal::decode(&[
            MARSHAL_HEADER_BYTE,
            0,
            0,
            0,
            0,
            MarshalOpcode::PyVarInteger as u8,
            9,
            1,
            2,
            3,
            4,
            5,
            6,
            7,
            8,
            0x80
        ]),
        Err(ProtoError::UnexpectedEof)
    );

    // 7. Tight `max_nodes` budget across Token/Buffer/ShortString/LongString & UCS2 `extra_utf8 > 0`
    for rep in [
        PyRep::Token("t".repeat(64)),
        PyRep::Buffer(vec![1u8; 64]),
        PyRep::Bytes(vec![2u8; 64]),
        PyRep::Bytes(vec![3u8; 260]),
    ] {
        let raw = marshal::encode(&rep);
        assert_eq!(
            marshal::decode_with_limits(&raw, 4096, 1),
            Err(ProtoError::NodeBudgetExceeded)
        );
    }

    // 64 CJK BMP chars -> 128 UCS-2 bytes (cost 2) + 192 UTF-8 bytes (extra_utf8 = 1) + 1 root node = 4
    let mut ucs2_cjk = vec![MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PyWStringUCS2 as u8, 64];
    for _ in 0..64 {
        ucs2_cjk.extend_from_slice(&0x4E00u16.to_le_bytes());
    }
    assert_eq!(
        marshal::decode_with_limits(&ucs2_cjk, 4096, 3),
        Err(ProtoError::NodeBudgetExceeded)
    );
    assert_eq!(
        marshal::decode_with_limits(&ucs2_cjk, 4096, 4),
        Ok(PyRep::String("\u{4E00}".repeat(64)))
    );

    // 8. read_collection_len overflow + read_size_ex empty & 4-byte EOF (L1179, L1186, L1194) & 254/255 boundary
    for op in [
        MarshalOpcode::PyTuple as u8,
        MarshalOpcode::PyList as u8,
        MarshalOpcode::PyDict as u8,
    ] {
        assert_eq!(
            marshal::decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0, op, 0xFF, 0x01, 0x00, 0x02, 0x00]),
            Err(ProtoError::CollectionTooLarge(131_073))
        );
    }
    assert_eq!(
        marshal::decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PyLongString as u8]),
        Err(ProtoError::UnexpectedEof)
    );
    assert_eq!(
        marshal::decode(&[
            MARSHAL_HEADER_BYTE,
            0,
            0,
            0,
            0,
            MarshalOpcode::PyLongString as u8,
            0xFF,
            1,
            2,
            3
        ]),
        Err(ProtoError::UnexpectedEof)
    );
    for len in [254usize, 255usize] {
        let rep = PyRep::Bytes(vec![b'z'; len]);
        assert_eq!(marshal::decode(&marshal::encode(&rep)), Ok(rep));
    }

    // 9. Container element error arms (PyOneList L802, PyList L814, PyObjectEx1 list/dict L936, L945, L955, L976)
    assert_eq!(
        marshal::decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PyOneList as u8]),
        Err(ProtoError::UnexpectedEof)
    );
    assert_eq!(
        marshal::decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PyList as u8, 1]),
        Err(ProtoError::UnexpectedEof)
    );
    assert_eq!(
        marshal::decode(&[
            MARSHAL_HEADER_BYTE,
            0,
            0,
            0,
            0,
            MarshalOpcode::PyObject as u8,
            MarshalOpcode::PyOneInteger as u8,
            MarshalOpcode::PyNone as u8
        ]),
        Err(ProtoError::UnsupportedOpcode(MarshalOpcode::PyObject as u8))
    );
    let set_non_coll = PyRep::Object {
        type_name: "set".into(),
        args: Box::new(PyRep::Integer(42)),
    };
    assert_eq!(
        marshal::decode(&marshal::encode(&set_non_coll)),
        Ok(set_non_coll)
    );

    assert_eq!(
        marshal::decode(&[
            MARSHAL_HEADER_BYTE,
            0,
            0,
            0,
            0,
            MarshalOpcode::PyObjectEx1 as u8,
            MarshalOpcode::PyNone as u8,
            MarshalOpcode::PyLong as u8
        ]),
        Err(ProtoError::UnexpectedEof)
    );
    assert_eq!(
        marshal::decode(&[
            MARSHAL_HEADER_BYTE,
            0,
            0,
            0,
            0,
            MarshalOpcode::PyObjectEx1 as u8,
            MarshalOpcode::PyNone as u8,
            MarshalOpcode::PackedTerminator as u8,
            MarshalOpcode::PyNone as u8,
            MarshalOpcode::PyNone as u8
        ]),
        Err(ProtoError::UnexpectedEof)
    );

    let mut too_many_ex_list = vec![
        MARSHAL_HEADER_BYTE,
        0,
        0,
        0,
        0,
        MarshalOpcode::PyObjectEx1 as u8,
        MarshalOpcode::PyNone as u8,
    ];
    too_many_ex_list.resize(too_many_ex_list.len() + MAX_COLLECTION_LEN + 1, MarshalOpcode::PyNone as u8);
    assert_eq!(
        marshal::decode_with_limits(&too_many_ex_list, 1024 * 1024, 2_000_000),
        Err(ProtoError::CollectionTooLarge(MAX_COLLECTION_LEN + 1))
    );

    let mut too_many_ex_dict = vec![
        MARSHAL_HEADER_BYTE,
        0,
        0,
        0,
        0,
        MarshalOpcode::PyObjectEx1 as u8,
        MarshalOpcode::PyNone as u8,
        MarshalOpcode::PackedTerminator as u8,
    ];
    too_many_ex_dict.resize(
        too_many_ex_dict.len() + (MAX_COLLECTION_LEN + 1) * 2,
        MarshalOpcode::PyNone as u8,
    );
    assert_eq!(
        marshal::decode_with_limits(&too_many_ex_dict, 1024 * 1024, 2_000_000),
        Err(ProtoError::CollectionTooLarge(MAX_COLLECTION_LEN + 1))
    );

    // 10. PyObjectEx2 non-V24_ROW_CLASS fallthrough (L1000-1002) & SAVE_FLAG V24_ROW_CLASS node budget (L995)
    let non_v24_row_ex2 = PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::Token("other.Row".into())]),
            PyRep::Integer(1),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    assert_eq!(
        marshal::decode(&marshal::encode(&non_v24_row_ex2)),
        Ok(non_v24_row_ex2)
    );

    let v24_row_hdr = marshal::encode(&PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::Token(marshal::V24_ROW_CLASS.into())]),
            PyRep::Tuple(vec![
                PyRep::List(vec![PyRep::String("a".into()), PyRep::String("b".into())]),
                PyRep::List(vec![PyRep::Integer(1), PyRep::Integer(2)]),
            ]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    });
    let mut saved_v24_row = vec![
        MARSHAL_HEADER_BYTE,
        1,
        0,
        0,
        0,
        (MarshalOpcode::PyObjectEx2 as u8) | crate::SAVE_FLAG,
    ];
    saved_v24_row.extend_from_slice(&v24_row_hdr[6..]); // header + terminators
    saved_v24_row.extend_from_slice(&1u32.to_le_bytes()); // slot 1
    let full_v24_nodes = marshal::decode(&saved_v24_row).unwrap().node_count();
    assert_eq!(
        marshal::decode_with_limits(&saved_v24_row, 4096, full_v24_nodes),
        Err(ProtoError::NodeBudgetExceeded)
    );

    // 11. PySubStruct, PyChecksumedStream, PySavedStreamElement error arms (L1065, L1071)
    assert_eq!(
        marshal::decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PySubStruct as u8]),
        Err(ProtoError::UnexpectedEof)
    );
    assert_eq!(
        marshal::decode(&[
            MARSHAL_HEADER_BYTE,
            0,
            0,
            0,
            0,
            MarshalOpcode::PyChecksumedStream as u8,
            1,
            2,
            3,
            4
        ]),
        Err(ProtoError::UnexpectedEof)
    );
    assert_eq!(
        marshal::decode(&[
            MARSHAL_HEADER_BYTE,
            1,
            0,
            0,
            0,
            MarshalOpcode::PyTwoTuple as u8,
            MarshalOpcode::PyNone as u8,
            MarshalOpcode::PySavedStreamElement as u8,
            1,
            0,
            0,
            0,
            0
        ]),
        Err(ProtoError::InvalidSaveReference(1))
    );
    let saved_clone_stream = [
        MARSHAL_HEADER_BYTE,
        1,
        0,
        0,
        0,
        MarshalOpcode::PyTwoTuple as u8,
        (MarshalOpcode::PyTwoTuple as u8) | crate::SAVE_FLAG,
        MarshalOpcode::PyOneInteger as u8,
        MarshalOpcode::PyOneInteger as u8,
        MarshalOpcode::PySavedStreamElement as u8,
        1,
        1,
        0,
        0,
        0,
    ];
    assert_eq!(
        marshal::decode_with_limits(&saved_clone_stream, 4096, 8),
        Err(ProtoError::NodeBudgetExceeded)
    );

    // 12. PyPackedRow truncated raw_rle (L683), budget overflow on unpack_cost (L711), missing trailing col (L718),
    //     & non-DBRowDescriptor header skip (L731)
    let row_hdr_raw = marshal::encode(&DBRowDescriptor::new([("a", DbColumnType::WStr)]).to_pyrep());
    let mut trunc_rle = vec![MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PyPackedRow as u8];
    trunc_rle.extend_from_slice(&row_hdr_raw[5..]);
    trunc_rle.push(10); // claims 10 RLE bytes
    trunc_rle.push(0x00);
    assert_eq!(marshal::decode(&trunc_rle), Err(ProtoError::UnexpectedEof));

    let valid_row = marshal::pack_dbrow(&[
        ("c_cy_int", 6, PyRep::Integer(-25)),
        ("c_cy_real", 6, PyRep::Real(12.34567)),
        ("c_cy_bad", 6, PyRep::String("bad".into())),
        ("c_cy_null", 6, PyRep::None),
        ("c_ui1_hi", 17, PyRep::Integer(-1)),
        ("c_r4_prec", 4, PyRep::Real(1.23456789)),
        ("c_unknown", 999, PyRep::String("kept".into())),
    ]);
    // Verify 12aca89e9 pack_dbrow pre-encode storage & decoded semantics:
    if let PyRep::DBRow { ref columns, .. } = valid_row {
        assert_eq!(columns[0].1, PyRep::Real(-25.0));
        assert_eq!(columns[6], ("c_unknown".into(), PyRep::String("kept".into())));
    } else {
        panic!("expected DBRow");
    }
    let packed_wire = marshal::encode(&valid_row);
    // Budget sufficient for header + 1 node, insufficient for unpack_cost -> NodeBudgetExceeded (L711)
    let hdr_nodes = match &valid_row {
        PyRep::DBRow { header, .. } => header.node_count(),
        _ => unreachable!(),
    };
    assert_eq!(
        marshal::decode_with_limits(&packed_wire, 4096, hdr_nodes + 1),
        Err(ProtoError::NodeBudgetExceeded)
    );
    let decoded_row = marshal::decode(&packed_wire).unwrap();
    assert_eq!(decoded_row.dbrow_get("c_cy_int"), Some(&PyRep::Real(-25.0)));
    assert_eq!(decoded_row.dbrow_get("c_cy_real"), Some(&PyRep::Real(12.3457)));
    assert_eq!(decoded_row.dbrow_get("c_cy_bad"), Some(&PyRep::Real(0.0)));
    assert_eq!(decoded_row.dbrow_get("c_cy_null"), Some(&PyRep::None));
    assert_eq!(decoded_row.dbrow_get("c_ui1_hi"), Some(&PyRep::Integer(255)));
    assert_eq!(
        decoded_row.dbrow_get("c_r4_prec"),
        Some(&PyRep::Real(f64::from(1.23456789_f32)))
    );
    assert_eq!(
        decoded_row.dbrow_get("c_unknown"),
        Some(&PyRep::String("kept".into()))
    );

    // Non-DBRowDescriptor header on PyPackedRow decodes cleanly with empty columns (L731 skip)
    let non_desc_row = PyRep::DBRow {
        header: Box::new(PyRep::None),
        columns: Vec::new(),
        raw_rle: Vec::new(),
    };
    assert_eq!(marshal::decode(&marshal::encode(&non_desc_row)), Ok(non_desc_row));

    // 13. `try_extract_rowset` / `try_extract_index_rowset` malformed fallbacks preserved as PyRep::Object (L1089-1165)
    let malformed_rowsets = [
        PyRep::Object {
            type_name: "util.Rowset".into(),
            args: Box::new(PyRep::None),
        },
        PyRep::Object {
            type_name: "util.Rowset".into(),
            args: Box::new(PyRep::Dict(vec![
                (PyRep::String("header".into()), PyRep::Integer(1)),
                (PyRep::String("lines".into()), PyRep::Integer(2)),
            ])),
        },
        PyRep::Object {
            type_name: "util.Rowset".into(),
            args: Box::new(PyRep::Dict(vec![
                (
                    PyRep::String("header".into()),
                    PyRep::List(vec![PyRep::String("a".into())]),
                ),
                (
                    PyRep::String("lines".into()),
                    PyRep::List(vec![PyRep::Tuple(vec![PyRep::Integer(1)])]),
                ),
            ])),
        },
        PyRep::Object {
            type_name: "util.IndexRowset".into(),
            args: Box::new(PyRep::None),
        },
        PyRep::Object {
            type_name: "util.IndexRowset".into(),
            args: Box::new(PyRep::Dict(vec![
                (PyRep::String("idName".into()), PyRep::String("id".into())),
                (PyRep::String("header".into()), PyRep::Integer(1)),
                (PyRep::String("items".into()), PyRep::Integer(2)),
            ])),
        },
        PyRep::Object {
            type_name: "util.IndexRowset".into(),
            args: Box::new(PyRep::Dict(vec![
                (PyRep::String("idName".into()), PyRep::String("id".into())),
                (
                    PyRep::String("header".into()),
                    PyRep::List(vec![PyRep::String("id".into())]),
                ),
                (
                    PyRep::String("items".into()),
                    PyRep::Dict(vec![(PyRep::Integer(1), PyRep::Tuple(vec![PyRep::Integer(1)]))]),
                ),
            ])),
        },
    ];
    for obj in malformed_rowsets {
        let rt = marshal::decode(&marshal::encode(&obj)).unwrap();
        assert_eq!(rt, obj);
    }

    // ObjectEx type-2 empty list/dict non-util.Row header variants (marshal.rs L1000-1002)
    for header in [
        PyRep::Tuple(vec![
            PyRep::Tuple(vec![PyRep::Token("other.Row".into())]),
            PyRep::None,
        ]),
        PyRep::Tuple(vec![PyRep::Token("not.ATuple".into()), PyRep::None]),
        PyRep::Tuple(vec![PyRep::Integer(1)]),
    ] {
        let objex = PyRep::ObjectEx {
            is_type_2: true,
            header: Box::new(header),
            list: Vec::new(),
            dict: Vec::new(),
        };
        assert_eq!(marshal::decode(&marshal::encode(&objex)), Ok(objex));
    }
}
