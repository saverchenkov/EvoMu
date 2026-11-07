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
