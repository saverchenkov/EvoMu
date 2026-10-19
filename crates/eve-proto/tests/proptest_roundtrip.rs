//! Property-based (`proptest`, >= 10,000 cases per property) and boundary tests for `eve-proto`:
//! - Every EVE Marshal opcode (`0x01..=0x2F` + zlib `0x78`) round-trip
//! - `PyPackedRow` (`0x2A`) Zero-RLE and `DBRow` schema packing round-trip
//! - Synthetic Protobuf `GatewayFrame` wire fixture + `PyRep` and `BridgeEnvelope` frame round-trip

use eve_proto::{
    marshal, BridgeEnvelope, BridgeRpcError, GatewayFrame, GatewayFrameKind, MarshalOpcode,
    ProtoError, PyRep, MARSHAL_HEADER_BYTE, MARSHAL_STRING_TABLE, MAX_FRAME_SIZE, SAVE_FLAG,
};
use proptest::prelude::*;
use std::collections::HashMap;

fn proptest_cases() -> u32 {
    std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(10_000)
}

fn write_size_ex(out: &mut Vec<u8>, len: usize) {
    if len < 0xFF {
        out.push(len as u8);
    } else {
        out.push(0xFF);
        out.extend_from_slice(&(len as u32).to_le_bytes());
    }
}

fn dbtype_bit_width(dbtype: i64) -> usize {
    match dbtype {
        20 | 21 | 64 | 5 | 6 => 64,
        3 | 19 | 4 => 32,
        2 | 18 => 16,
        16 | 17 => 8,
        11 => 1,
        _ => 0,
    }
}

fn pack_dbrow(col_defs: &[(String, i64)], values: &[(String, PyRep)]) -> PyRep {
    let header = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("blue.DBRowDescriptor".to_string()),
            PyRep::Tuple(vec![PyRep::Tuple(
                col_defs
                    .iter()
                    .map(|(name, dbtype)| {
                        PyRep::Tuple(vec![PyRep::String(name.clone()), PyRep::Integer(*dbtype)])
                    })
                    .collect(),
            )]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };

    let mut expected_bytes = 0usize;
    let mut bool_count = 0usize;
    for (_, dbtype) in col_defs {
        match dbtype_bit_width(*dbtype) {
            64 => expected_bytes += 8,
            32 => expected_bytes += 4,
            16 => expected_bytes += 2,
            8 => expected_bytes += 1,
            1 => bool_count += 1,
            _ => {}
        }
    }
    expected_bytes += bool_count.div_ceil(8);

    let mut indexed_defs: Vec<(usize, &(String, i64))> = col_defs.iter().enumerate().collect();
    indexed_defs.sort_by(|(idx_a, (_, dt_a)), (idx_b, (_, dt_b))| {
        dbtype_bit_width(*dt_b)
            .cmp(&dbtype_bit_width(*dt_a))
            .then_with(|| idx_a.cmp(idx_b))
    });

    let mut unpacked = vec![0u8; expected_bytes];
    let mut offset = 0usize;
    let mut bool_bit_idx = 0usize;
    let mut normalized_cols: Vec<(usize, String, PyRep)> = Vec::with_capacity(col_defs.len());

    for (orig_idx, (name, dbtype)) in indexed_defs {
        let raw_val = values[orig_idx].1.clone();
        let norm = match *dbtype {
            20 | 21 | 64 | 6 => {
                let v = raw_val.as_i64().unwrap_or(0);
                unpacked[offset..offset + 8].copy_from_slice(&v.to_le_bytes());
                offset += 8;
                PyRep::Integer(v)
            }
            5 => {
                let v = raw_val.as_f64().unwrap_or(0.0);
                unpacked[offset..offset + 8].copy_from_slice(&v.to_le_bytes());
                offset += 8;
                PyRep::Real(v)
            }
            3 => {
                let v = raw_val.as_i64().unwrap_or(0) as i32;
                unpacked[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
                offset += 4;
                PyRep::Integer(v as i64)
            }
            19 => {
                let v = raw_val.as_i64().unwrap_or(0) as u32;
                unpacked[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
                offset += 4;
                PyRep::Integer(v as i64)
            }
            4 => {
                let v = raw_val.as_f64().unwrap_or(0.0) as f32;
                unpacked[offset..offset + 4].copy_from_slice(&v.to_le_bytes());
                offset += 4;
                PyRep::Real(v as f64)
            }
            2 => {
                let v = raw_val.as_i64().unwrap_or(0) as i16;
                unpacked[offset..offset + 2].copy_from_slice(&v.to_le_bytes());
                offset += 2;
                PyRep::Integer(v as i64)
            }
            18 => {
                let v = raw_val.as_i64().unwrap_or(0) as u16;
                unpacked[offset..offset + 2].copy_from_slice(&v.to_le_bytes());
                offset += 2;
                PyRep::Integer(v as i64)
            }
            16 => {
                let v = raw_val.as_i64().unwrap_or(0) as i8;
                unpacked[offset] = v as u8;
                offset += 1;
                PyRep::Integer(v as i64)
            }
            17 => {
                let v = raw_val.as_i64().unwrap_or(0) as u8;
                unpacked[offset] = v;
                offset += 1;
                PyRep::Integer(v as i64)
            }
            11 => {
                let b = raw_val.as_bool().unwrap_or(false);
                let byte_idx = offset + (bool_bit_idx / 8);
                let bit_mask = 1u8 << (bool_bit_idx % 8);
                bool_bit_idx += 1;
                if b && byte_idx < unpacked.len() {
                    unpacked[byte_idx] |= bit_mask;
                }
                PyRep::Bool(b)
            }
            _ => raw_val,
        };
        normalized_cols.push((orig_idx, name.clone(), norm));
    }

    normalized_cols.sort_by_key(|(idx, _, _)| *idx);
    let columns = normalized_cols
        .into_iter()
        .map(|(_, name, val)| (name, val))
        .collect();
    let raw_rle = marshal::encode_rle(&unpacked);

    PyRep::DBRow {
        header: Box::new(header),
        columns,
        raw_rle,
    }
}

fn encode_var_integer_frame(v: i64) -> Vec<u8> {
    let le = v.to_le_bytes();
    let mut len = 8usize;
    while len > 1 {
        let last = le[len - 1];
        let prev = le[len - 2];
        if (last == 0x00 && (prev & 0x80) == 0) || (last == 0xFF && (prev & 0x80) != 0) {
            len -= 1;
        } else {
            break;
        }
    }
    let mut out = Vec::with_capacity(7 + len);
    out.push(MARSHAL_HEADER_BYTE);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.push(MarshalOpcode::PyVarInteger as u8);
    write_size_ex(&mut out, len);
    out.extend_from_slice(&le[..len]);
    out
}

fn encode_wstring_ucs2_frame(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut out = Vec::with_capacity(8 + units.len() * 2);
    out.push(MARSHAL_HEADER_BYTE);
    out.extend_from_slice(&0u32.to_le_bytes());
    if units.len() == 1 {
        out.push(MarshalOpcode::PyWStringUCS2Char as u8);
        out.extend_from_slice(&units[0].to_le_bytes());
    } else {
        out.push(MarshalOpcode::PyWStringUCS2 as u8);
        write_size_ex(&mut out, units.len());
        for u in units {
            out.extend_from_slice(&u.to_le_bytes());
        }
    }
    out
}

fn encode_string_table_frame(index_1_based: u8) -> Result<Vec<u8>, ProtoError> {
    let idx = index_1_based as usize;
    if idx == 0 || idx > MARSHAL_STRING_TABLE.len() {
        return Err(ProtoError::UnsupportedOpcode(
            MarshalOpcode::PyStringTableItem as u8,
        ));
    }
    let mut out = Vec::with_capacity(7);
    out.push(MARSHAL_HEADER_BYTE);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.push(MarshalOpcode::PyStringTableItem as u8);
    out.push(index_1_based);
    Ok(out)
}

fn encode_saved_stream_pair_frame(elem: &PyRep) -> Vec<u8> {
    let full = marshal::encode(elem);
    let mut elem_bytes = full[5..].to_vec();
    if let Some(first) = elem_bytes.first_mut() {
        *first |= SAVE_FLAG;
    }
    let mut out = Vec::with_capacity(14 + elem_bytes.len());
    out.push(MARSHAL_HEADER_BYTE);
    out.extend_from_slice(&1u32.to_le_bytes());
    out.push(MarshalOpcode::PyTwoTuple as u8);
    out.extend_from_slice(&elem_bytes);
    out.push(MarshalOpcode::PySavedStreamElement as u8);
    write_size_ex(&mut out, 1);
    out.extend_from_slice(&1u32.to_le_bytes());
    out
}

fn arb_leaf_pyrep() -> impl Strategy<Value = PyRep> {
    prop_oneof![
        Just(PyRep::None),
        any::<bool>().prop_map(PyRep::Bool),
        prop_oneof![
            Just(-1i64),
            Just(0i64),
            Just(1i64),
            any::<i8>().prop_map(i64::from),
            any::<i16>().prop_map(i64::from),
            any::<i32>().prop_map(i64::from),
            any::<i64>(),
        ]
        .prop_map(PyRep::Integer),
        prop_oneof![
            Just(0.0f64),
            (-1e12f64..1e12f64).prop_map(|v| (v * 1000.0).round() / 1000.0),
        ]
        .prop_map(PyRep::Real),
        "[a-zA-Z0-9_.]{1,16}".prop_map(PyRep::Token),
        prop_oneof![
            Just(Vec::new()),
            prop::collection::vec(any::<u8>(), 1..=1),
            prop::collection::vec(any::<u8>(), 2..=16),
            prop::collection::vec(any::<u8>(), 256..=264),
        ]
        .prop_map(PyRep::Bytes),
        prop_oneof![
            Just(String::new()),
            "[a-zA-Z0-9]{1}",
            "[a-zA-Z0-9_ ]{2,16}",
            Just("Caldari\u{00E9}\u{2605}".to_string()),
        ]
        .prop_map(PyRep::String),
        prop::collection::vec(any::<u8>(), 0..=16).prop_map(PyRep::Buffer),
        prop::collection::vec(any::<u8>(), 1..=12).prop_map(PyRep::Picked),
    ]
}

fn arb_pyrep() -> impl Strategy<Value = PyRep> {
    arb_leaf_pyrep().prop_recursive(2, 16, 4, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..=4).prop_map(PyRep::Tuple),
            prop::collection::vec(inner.clone(), 0..=4).prop_map(PyRep::List),
            prop::collection::vec((arb_leaf_pyrep(), inner.clone()), 0..=3).prop_map(PyRep::Dict),
            ("custom\\.[A-Za-z]{3,10}", inner.clone()).prop_map(|(type_name, args)| {
                PyRep::Object {
                    type_name,
                    args: Box::new(args),
                }
            }),
            (
                any::<bool>(),
                inner.clone(),
                prop::collection::vec(inner.clone(), 0..=2),
                prop::collection::vec((arb_leaf_pyrep(), inner.clone()), 0..=2),
            )
                .prop_map(|(is_type_2, header, list, dict)| {
                    PyRep::ObjectEx {
                        is_type_2,
                        header: Box::new(header),
                        list,
                        dict,
                    }
                }),
            inner.clone().prop_map(|v| PyRep::SubStruct(Box::new(v))),
            inner.clone().prop_map(|v| PyRep::SubStream(Box::new(v))),
            (any::<u32>(), inner.clone()).prop_map(|(checksum, body)| PyRep::ChecksumedStream {
                checksum,
                body: Box::new(body),
            }),
            prop::collection::vec(("[a-z]{1,6}", inner), 0..=3).prop_map(|pairs| {
                let mut map = HashMap::new();
                for (k, v) in pairs {
                    map.insert(k, v);
                }
                PyRep::KeyVal(map)
            }),
        ]
    })
}

#[derive(Debug, Clone)]
enum ArbDbCol {
    I64(i64),
    U64(u64),
    FileTime(i64),
    Currency(i64),
    F64(f64),
    I32(i32),
    U32(u32),
    F32(f32),
    I16(i16),
    U16(u16),
    I8(i8),
    U8(u8),
    Bool(bool),
    Str(String),
    Bytes(Vec<u8>),
}

fn arb_db_col() -> impl Strategy<Value = ArbDbCol> {
    prop_oneof![
        any::<i64>().prop_map(ArbDbCol::I64),
        (0u64..(i64::MAX as u64)).prop_map(ArbDbCol::U64),
        any::<i64>().prop_map(ArbDbCol::FileTime),
        any::<i64>().prop_map(ArbDbCol::Currency),
        (-1e9f64..1e9f64).prop_map(ArbDbCol::F64),
        any::<i32>().prop_map(ArbDbCol::I32),
        any::<u32>().prop_map(ArbDbCol::U32),
        (-1e5f32..1e5f32).prop_map(ArbDbCol::F32),
        any::<i16>().prop_map(ArbDbCol::I16),
        any::<u16>().prop_map(ArbDbCol::U16),
        any::<i8>().prop_map(ArbDbCol::I8),
        any::<u8>().prop_map(ArbDbCol::U8),
        any::<bool>().prop_map(ArbDbCol::Bool),
        "[a-zA-Z0-9_:-]{0,16}".prop_map(ArbDbCol::Str),
        prop::collection::vec(any::<u8>(), 0..=16).prop_map(ArbDbCol::Bytes),
    ]
}

fn arb_gateway_frame() -> impl Strategy<Value = GatewayFrame> {
    (
        prop_oneof![
            Just(GatewayFrameKind::Request),
            Just(GatewayFrameKind::Response),
            Just(GatewayFrameKind::Notice),
        ],
        any::<u64>(),
        "[a-z_]+\\.[a-z_]+\\.[A-Za-z]+",
        0u32..1000u32,
        prop::option::of(2_100_000_000i64..2_200_000_000i64),
        prop::option::of("[a-f0-9-]{8,24}"),
        prop::collection::vec(any::<u8>(), 0..=48),
    )
        .prop_map(
            |(
                kind,
                correlation_id,
                type_name,
                status_code,
                actor_character_id,
                idempotency_key,
                payload,
            )| {
                GatewayFrame {
                    kind,
                    correlation_id,
                    type_name,
                    status_code,
                    actor_character_id,
                    idempotency_key,
                    payload,
                }
            },
        )
}

fn arb_bridge_envelope() -> impl Strategy<Value = BridgeEnvelope> {
    prop_oneof![
        (
            1u64..1_000_000u64,
            "[a-zA-Z_]{3,16}",
            prop::option::of(1u64..10_000u64),
            "[a-zA-Z_]{3,16}",
            prop::collection::vec(arb_leaf_pyrep(), 0..=3),
            prop::option::of("[a-zA-Z0-9_.:-]{4,24}"),
        )
            .prop_map(|(req_id, service, bound_id, method, args, caller_trace)| {
                BridgeEnvelope::RpcRequest {
                    req_id,
                    service,
                    bound_id,
                    method,
                    args,
                    kwargs: HashMap::new(),
                    caller_trace,
                }
            }),
        (1u64..1_000_000u64, arb_leaf_pyrep()).prop_map(|(req_id, val)| {
            BridgeEnvelope::RpcResponse {
                req_id,
                result: Ok(val),
            }
        }),
        (1u64..1_000_000u64, "[A-Za-z]{3,12}", "[a-zA-Z0-9 ]{3,20}").prop_map(
            |(req_id, code, message)| BridgeEnvelope::RpcResponse {
                req_id,
                result: Err(BridgeRpcError { code, message }),
            }
        ),
        (
            1u64..1_000_000u64,
            "On[A-Za-z]{3,14}",
            prop::collection::vec(arb_leaf_pyrep(), 0..=3),
        )
            .prop_map(|(tick, event, args)| BridgeEnvelope::ServerEvent {
                tick,
                event,
                args,
            }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(proptest_cases()))]

    #[test]
    fn prop_marshal_opcode_roundtrip(
        rep in arb_pyrep(),
        var_int in any::<i64>(),
        ucs2_str in "[a-zA-Z0-9_]{0,12}",
        table_idx in 1u8..=(MARSHAL_STRING_TABLE.len() as u8),
    ) {
        // 1. Standard Marshal encode -> decode round-trip
        let bytes = marshal::encode(&rep);
        let decoded = marshal::decode(&bytes).expect("marshal::decode must succeed");
        prop_assert_eq!(&decoded, &rep);

        // 2. Zlib-compressed (0x78) stream round-trip
        let zbytes = marshal::encode_zlib(&rep).expect("encode_zlib must succeed");
        let zdecoded = marshal::decode(&zbytes).expect("zlib marshal::decode must succeed");
        prop_assert_eq!(&zdecoded, &rep);

        // 3. Dedicated opcodes that normalize into core PyRep variants:
        //    - PyVarInteger (0x2F)
        let var_frame = encode_var_integer_frame(var_int);
        prop_assert_eq!(marshal::decode(&var_frame).unwrap(), PyRep::Integer(var_int));

        //    - PyWStringUCS2 (0x12) and PyWStringUCS2Char (0x29)
        let ucs2_frame = encode_wstring_ucs2_frame(&ucs2_str);
        prop_assert_eq!(
            marshal::decode(&ucs2_frame).unwrap(),
            PyRep::String(ucs2_str.clone())
        );

        //    - PyStringTableItem (0x11)
        let st_frame = encode_string_table_frame(table_idx).unwrap();
        prop_assert_eq!(
            marshal::decode(&st_frame).unwrap(),
            PyRep::String(MARSHAL_STRING_TABLE[(table_idx - 1) as usize].to_string())
        );

        //    - PySavedStreamElement (0x1B) + SAVE_FLAG (0x40) back-reference
        let saved_frame = encode_saved_stream_pair_frame(&rep);
        prop_assert_eq!(
            marshal::decode(&saved_frame).unwrap(),
            PyRep::Tuple(vec![rep.clone(), rep])
        );
    }

    #[test]
    fn prop_packed_row_dbrow_roundtrip(
        cols in prop::collection::vec(arb_db_col(), 1..=10),
        raw_rle in prop::collection::vec(any::<u8>(), 0..=64),
    ) {
        // 1. Direct Zero-RLE round-trip
        let encoded_rle = marshal::encode_rle(&raw_rle);
        let decoded_rle = marshal::decode_rle(&encoded_rle, raw_rle.len()).unwrap();
        prop_assert_eq!(decoded_rle, raw_rle);

        // 2. DBRow tiered bit-packing + Marshal PyPackedRow (0x2A) round-trip
        let mut col_defs = Vec::with_capacity(cols.len());
        let mut values = Vec::with_capacity(cols.len());
        let mut fixed_bytes = 0usize;
        let mut bool_bits = 0usize;

        for (i, col) in cols.iter().enumerate() {
            let name = format!("c{i}");
            match col {
                ArbDbCol::I64(v) => {
                    col_defs.push((name.clone(), 20i64));
                    values.push((name, PyRep::Integer(*v)));
                    fixed_bytes += 8;
                }
                ArbDbCol::U64(v) => {
                    col_defs.push((name.clone(), 21i64));
                    values.push((name, PyRep::Integer(*v as i64)));
                    fixed_bytes += 8;
                }
                ArbDbCol::FileTime(v) => {
                    col_defs.push((name.clone(), 64i64));
                    values.push((name, PyRep::Integer(*v)));
                    fixed_bytes += 8;
                }
                ArbDbCol::Currency(v) => {
                    col_defs.push((name.clone(), 6i64));
                    values.push((name, PyRep::Integer(*v)));
                    fixed_bytes += 8;
                }
                ArbDbCol::F64(v) => {
                    col_defs.push((name.clone(), 5i64));
                    values.push((name, PyRep::Real(*v)));
                    fixed_bytes += 8;
                }
                ArbDbCol::I32(v) => {
                    col_defs.push((name.clone(), 3i64));
                    values.push((name, PyRep::Integer(i64::from(*v))));
                    fixed_bytes += 4;
                }
                ArbDbCol::U32(v) => {
                    col_defs.push((name.clone(), 19i64));
                    values.push((name, PyRep::Integer(i64::from(*v))));
                    fixed_bytes += 4;
                }
                ArbDbCol::F32(v) => {
                    col_defs.push((name.clone(), 4i64));
                    values.push((name, PyRep::Real(f64::from(*v))));
                    fixed_bytes += 4;
                }
                ArbDbCol::I16(v) => {
                    col_defs.push((name.clone(), 2i64));
                    values.push((name, PyRep::Integer(i64::from(*v))));
                    fixed_bytes += 2;
                }
                ArbDbCol::U16(v) => {
                    col_defs.push((name.clone(), 18i64));
                    values.push((name, PyRep::Integer(i64::from(*v))));
                    fixed_bytes += 2;
                }
                ArbDbCol::I8(v) => {
                    col_defs.push((name.clone(), 16i64));
                    values.push((name, PyRep::Integer(i64::from(*v))));
                    fixed_bytes += 1;
                }
                ArbDbCol::U8(v) => {
                    col_defs.push((name.clone(), 17i64));
                    values.push((name, PyRep::Integer(i64::from(*v))));
                    fixed_bytes += 1;
                }
                ArbDbCol::Bool(v) => {
                    col_defs.push((name.clone(), 11i64));
                    values.push((name, PyRep::Bool(*v)));
                    bool_bits += 1;
                }
                ArbDbCol::Str(s) => {
                    col_defs.push((name.clone(), 130i64));
                    values.push((name, PyRep::String(s.clone())));
                }
                ArbDbCol::Bytes(b) => {
                    col_defs.push((name.clone(), 128i64));
                    values.push((name, PyRep::Bytes(b.clone())));
                }
            }
        }

        let packed = pack_dbrow(&col_defs, &values);
        let wire = marshal::encode(&packed);
        let roundtripped = marshal::decode(&wire).expect("PyPackedRow decode must succeed");
        prop_assert_eq!(&roundtripped, &packed);

        if let PyRep::DBRow { columns, raw_rle: rle, .. } = roundtripped {
            prop_assert_eq!(columns, values);
            let expected_byte_len = fixed_bytes + bool_bits.div_ceil(8);
            let unpacked = marshal::decode_rle(&rle, expected_byte_len).unwrap();
            prop_assert_eq!(unpacked.len(), expected_byte_len);
        } else {
            prop_assert!(false, "Expected PyRep::DBRow");
        }
    }

    #[test]
    fn prop_gateway_and_bridge_roundtrip(
        frame in arb_gateway_frame(),
        env in arb_bridge_envelope(),
    ) {
        // 1. Synthetic GatewayFrame protobuf binary wire round-trip
        let proto_bytes = frame.encode_protobuf_wire();
        let decoded_proto = GatewayFrame::decode_protobuf_wire(&proto_bytes)
            .expect("GatewayFrame::decode_protobuf_wire must succeed");
        prop_assert_eq!(&decoded_proto, &frame);

        // 2. Synthetic GatewayFrame PyRep + Marshal round-trip
        let pyrep = frame.to_pyrep();
        let marshaled = marshal::encode(&pyrep);
        let unmarshaled = marshal::decode(&marshaled).unwrap();
        let decoded_pyrep = GatewayFrame::from_pyrep(&unmarshaled)
            .expect("GatewayFrame::from_pyrep must succeed");
        prop_assert_eq!(&decoded_pyrep, &frame);

        // 3. BridgeEnvelope length-prefixed wire round-trip
        let env_wire = env.to_wire_bytes().expect("BridgeEnvelope::to_wire_bytes must succeed");
        let decoded_env = BridgeEnvelope::from_wire_bytes(&env_wire)
            .expect("BridgeEnvelope::from_wire_bytes must succeed");
        prop_assert_eq!(&decoded_env, &env);

        // 4. BridgeEnvelope raw JSON wire round-trip
        let raw_json = serde_json::to_vec(&env).unwrap();
        let decoded_raw = BridgeEnvelope::from_wire_bytes(&raw_json)
            .expect("BridgeEnvelope::from_wire_bytes raw JSON must succeed");
        prop_assert_eq!(&decoded_raw, &env);
    }
}

#[test]
fn test_marshal_and_gateway_wire_edge_cases_and_bounds() {

    // High-level projections round-trip: Rowset, IndexRowset, TupleSet
    let rowset = PyRep::rowset(
        vec!["itemID", "typeID"],
        vec![vec![PyRep::Integer(100), PyRep::Integer(587)]],
    );
    assert_eq!(marshal::decode(&marshal::encode(&rowset)).unwrap(), rowset);

    let idx_rowset = PyRep::index_rowset(
        "itemID",
        vec!["itemID", "typeID"],
        vec![(PyRep::Integer(100), vec![PyRep::Integer(100), PyRep::Integer(587)])],
    );
    assert_eq!(
        marshal::decode(&marshal::encode(&idx_rowset)).unwrap(),
        idx_rowset
    );

    let tuple_set = PyRep::tuple_set(
        vec!["colA", "colB"],
        vec![vec![PyRep::String("v".into()), PyRep::Bool(true)]],
    );
    assert_eq!(
        marshal::decode(&marshal::encode(&tuple_set)).unwrap(),
        PyRep::Tuple(vec![
            PyRep::List(vec![PyRep::String("colA".into()), PyRep::String("colB".into())]),
            PyRep::List(vec![PyRep::List(vec![PyRep::String("v".into()), PyRep::Bool(true)])]),
        ])
    );

    // String table bounds
    assert!(encode_string_table_frame(0).is_err());
    assert!(encode_string_table_frame((MARSHAL_STRING_TABLE.len() as u8) + 1).is_err());
    for idx in 1u8..=(MARSHAL_STRING_TABLE.len() as u8) {
        let f = encode_string_table_frame(idx).unwrap();
        assert_eq!(
            marshal::decode(&f).unwrap(),
            PyRep::String(MARSHAL_STRING_TABLE[(idx - 1) as usize].to_string())
        );
    }

    // VarInteger positive, negative, and zero cases
    for val in [0i64, 1, -1, 127, -128, 255, -255, 65535, -65536, i64::MAX, i64::MIN] {
        let f = encode_var_integer_frame(val);
        assert_eq!(marshal::decode(&f).unwrap(), PyRep::Integer(val));
    }

    // UCS-2 empty, single-char, and multi-char frames
    for s in ["", "A", "Hello"] {
        let f = encode_wstring_ucs2_frame(s);
        assert_eq!(marshal::decode(&f).unwrap(), PyRep::String(s.to_string()));
    }

    // RLE zero chains and non-zero runs of length > 8
    let rle_inputs = [
        vec![],
        vec![0u8; 20],
        vec![0xAAu8; 20],
        vec![0, 0, 0, 1, 2, 3, 0, 0, 4, 5, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9],
    ];
    for raw in rle_inputs {
        let rle = marshal::encode_rle(&raw);
        assert_eq!(marshal::decode_rle(&rle, raw.len()).unwrap(), raw);
    }

    // BridgeEnvelope error and MessagePack paths
    assert!(BridgeEnvelope::from_wire_bytes(&[]).is_err());
    let mut bad_len = vec![10u8, 0, 0, 0, b'{', b'}'];
    assert!(BridgeEnvelope::from_wire_bytes(&bad_len).is_err());
    let huge_len = ((MAX_FRAME_SIZE as u32) + 1).to_le_bytes();
    bad_len[0..4].copy_from_slice(&huge_len);
    assert!(BridgeEnvelope::from_wire_bytes(&bad_len).is_err());
    assert!(BridgeEnvelope::from_wire_bytes(&vec![b'a'; MAX_FRAME_SIZE + 5]).is_err());

    let msgpack_env = BridgeEnvelope::ServerEvent {
        tick: 7,
        event: "OnTest".into(),
        args: vec![PyRep::Integer(42)],
    };
    let msgpack_bytes = rmp_serde::to_vec_named(&msgpack_env).unwrap();
    assert_eq!(
        BridgeEnvelope::from_wire_bytes(&msgpack_bytes).unwrap(),
        msgpack_env
    );

    // GatewayFrame negative actor_character_id zigzag and error paths
    let neg_actor_frame = GatewayFrame {
        kind: GatewayFrameKind::Notice,
        correlation_id: 999,
        type_name: "eve_public.test.Notice".into(),
        status_code: 0,
        actor_character_id: Some(-42),
        idempotency_key: None,
        payload: vec![1, 2, 3],
    };
    let neg_wire = neg_actor_frame.encode_protobuf_wire();
    assert_eq!(
        GatewayFrame::decode_protobuf_wire(&neg_wire).unwrap(),
        neg_actor_frame
    );
    assert_eq!(
        GatewayFrame::from_pyrep(&neg_actor_frame.to_pyrep()).unwrap(),
        neg_actor_frame
    );

    assert_eq!(GatewayFrameKind::from_u64(0), None);
    assert_eq!(GatewayFrameKind::from_u64(4), None);
    assert!(GatewayFrame::decode_protobuf_wire(&[]).is_err());
    assert!(GatewayFrame::decode_protobuf_wire(&vec![0u8; MAX_FRAME_SIZE + 1]).is_err());
    assert!(GatewayFrame::decode_protobuf_wire(&[0x08, 0x99, 0x01]).is_err());
    assert!(GatewayFrame::decode_protobuf_wire(&[0x1A, 0x10, 1, 2]).is_err());
    assert!(GatewayFrame::decode_protobuf_wire(&[0x78, 0x01]).is_err());
    assert!(GatewayFrame::from_pyrep(&PyRep::None).is_err());
    assert!(GatewayFrame::from_pyrep(&PyRep::Tuple(vec![PyRep::None; 6])).is_err());
    assert!(GatewayFrame::from_pyrep(&PyRep::Tuple(vec![PyRep::None; 7])).is_err());
}
