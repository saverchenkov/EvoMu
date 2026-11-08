//! Adversarial security and wire-fidelity regression tests for `eve_proto::marshal`:
//! - Owned `String` byte-weight accounting across `Object`, `KeyVal`, `Rowset`, `TupleSet`,
//!   `IndexRowset`, and `DBRow` (`PyRep::node_count()`).
//! - Initial decode byte-weight deductions on string/buffer/token/UCS2 opcodes and
//!   pre-unpack `PyPackedRow` (`0x2A`) column + unpacked-RLE byte charging.
//! - `PySavedStreamElement` (`0x1B`) reference-expansion bomb mitigation.
//! - Nested `zlib`-compressed `PySubStream` (`0x2B`) round-trip decoding (both standard `0x78`
//!   and V24.01 `blue.marshal.Save` small-window `0x58` RFC 1950 headers) inside raw and
//!   `zlib`-compressed outer `macho.CallReq` frames, plus shared `remaining_inflate_budget`
//!   zip-bomb rejection.

#![forbid(unsafe_code)]

use super::*;
use crate::{
    DBRowDescriptor, DbColumnType, MarshalOpcode, ProtoError, PyRep, MARSHAL_HEADER_BYTE,
    MAX_DECODED_NODES, MAX_FRAME_SIZE, SAVE_FLAG,
};
use flate2::write::ZlibEncoder;
use flate2::Compression;
use proptest::prelude::*;
use std::io::Write;

/// Compresses `raw` as an RFC 1950 `zlib` stream using either the standard 32 KiB window (`0x78`)
/// or V24.01 `blue.marshal.Save`'s small 8 KiB window header (`0x58 0x09`, `0x5809 % 31 == 0`).
fn zlib_compress_with_window(raw: &[u8], small_v24_window: bool) -> Vec<u8> {
    let level = if small_v24_window && raw.len() > 8_192 {
        Compression::none()
    } else {
        Compression::default()
    };
    let mut enc = ZlibEncoder::new(Vec::new(), level);
    enc.write_all(raw).expect("zlib write_all");
    let mut z = enc.finish().expect("zlib finish");
    if small_v24_window {
        z[0] = 0x58;
        z[1] = 0x09;
    }
    z
}

/// Builds a `0x7E` marshal stream containing a `PyTuple` whose first item saves `seed` in slot 1
/// (`SAVE_FLAG = 0x40`) and whose remaining `clones` items reference slot 1 via `PySavedStreamElement` (`0x1B`).
fn build_saved_reference_stream(seed: &PyRep, clones: usize) -> Vec<u8> {
    let seed_frame = encode(seed);
    let seed_body = &seed_frame[5..];
    assert!(!seed_body.is_empty());

    let total_items = 1 + clones;
    let mut out = Vec::new();
    out.push(MARSHAL_HEADER_BYTE);
    out.extend_from_slice(&1u32.to_le_bytes()); // save_count = 1
    out.push(MarshalOpcode::PyTuple as u8);
    write_size_ex(&mut out, total_items);

    // Emit `seed` with `SAVE_FLAG` set on its root opcode
    out.push(seed_body[0] | SAVE_FLAG);
    out.extend_from_slice(&seed_body[1..]);

    // Emit `clones` references to 1-based slot #1
    for _ in 0..clones {
        out.push(MarshalOpcode::PySavedStreamElement as u8);
        out.push(1u8);
    }

    // Trailing 1-based storage-index map: [1u32]
    out.extend_from_slice(&1u32.to_le_bytes());
    out
}

/// Serializes `rep`, replacing every `PyRep::SubStream(inner)` with a `PySubStream` (`0x2B`)
/// whose embedded stream bytes are `zlib`-compressed.
fn encode_with_compressed_substreams(rep: &PyRep, small_v24_window: bool) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(MARSHAL_HEADER_BYTE);
    out.extend_from_slice(&0u32.to_le_bytes());
    encode_node_with_zlib_substream(rep, &mut out, small_v24_window);
    out
}

fn encode_node_with_zlib_substream(rep: &PyRep, out: &mut Vec<u8>, small_v24_window: bool) {
    match rep {
        PyRep::SubStream(inner) => {
            let inner_raw = encode_with_compressed_substreams(inner, small_v24_window);
            let inner_z = zlib_compress_with_window(&inner_raw, small_v24_window);
            out.push(MarshalOpcode::PySubStream as u8);
            write_size_ex(out, inner_z.len());
            out.extend_from_slice(&inner_z);
        }
        PyRep::Tuple(items) => {
            out.push(MarshalOpcode::PyTuple as u8);
            write_size_ex(out, items.len());
            for it in items {
                encode_node_with_zlib_substream(it, out, small_v24_window);
            }
        }
        PyRep::Object { type_name, args } if type_name != "util.Row" => {
            out.push(MarshalOpcode::PyObject as u8);
            super::encode_inner(&PyRep::Bytes(type_name.clone().into_bytes()), out);
            encode_node_with_zlib_substream(args, out, small_v24_window);
        }
        other => super::encode_inner(other, out),
    }
}

#[test]
fn test_node_count_includes_owned_string_byte_weights_across_containers() {
    let big = "K".repeat(64 * 10); // 640 bytes -> 10 byte-weight units

    let obj = PyRep::Object {
        type_name: big.clone(),
        args: Box::new(PyRep::None),
    };
    assert_eq!(obj.node_count(), 1 + 10 + 1);

    let kv = PyRep::keyval([(big.clone(), PyRep::Integer(7))]);
    assert_eq!(kv.node_count(), 1 + (1 + 10) + 1);

    let rs = PyRep::Rowset {
        columns: vec![big.clone()],
        rows: vec![vec![PyRep::Integer(1)]],
    };
    assert_eq!(rs.node_count(), 1 + (1 + 10) + 1);

    // Empty rows still count at least 1 unit per row so empty-row vectors cannot amplify for free
    let empty_rows_ts = PyRep::TupleSet {
        columns: vec![big.clone()],
        rows: vec![Vec::new(), Vec::new(), Vec::new()],
    };
    assert_eq!(empty_rows_ts.node_count(), 1 + (1 + 10) + 3);

    let irs = PyRep::IndexRowset {
        key_column: big.clone(),
        columns: vec![big.clone(), "v".into()],
        items: vec![(
            PyRep::Integer(1),
            vec![PyRep::Integer(1), PyRep::Integer(2)],
        )],
    };
    assert_eq!(irs.node_count(), 1 + (1 + 10) + (1 + 10) + 1 + (1 + 2));

    let dbrow = PyRep::DBRow {
        header: Box::new(PyRep::None),
        columns: vec![(big, PyRep::Integer(99))],
        raw_rle: vec![0u8; 128],
    };
    assert_eq!(dbrow.node_count(), 1 + 2 + 1 + (1 + 10) + 1);
}

#[test]
fn test_saved_stream_reference_expansion_rejected_for_keyval_object_rowset_and_dbrow() {
    let big_str = "A".repeat(64 * 1024); // 64 KiB -> 1,024 byte-weight units per clone

    // 1. KeyVal with 64 KiB key cloned 1,200 times (~68 KiB wire, > 1.2M weight units > MAX_DECODED_NODES)
    let kv_seed = PyRep::keyval([(big_str.clone(), PyRep::Integer(1))]);
    let kv_bomb = build_saved_reference_stream(&kv_seed, 1_200);
    assert!(kv_bomb.len() < 80 * 1024);
    assert_eq!(decode(&kv_bomb), Err(ProtoError::NodeBudgetExceeded));

    // 2. Object with 64 KiB `type_name` cloned 1,200 times
    let obj_seed = PyRep::Object {
        type_name: big_str.clone(),
        args: Box::new(PyRep::None),
    };
    let obj_bomb = build_saved_reference_stream(&obj_seed, 1_200);
    assert_eq!(decode(&obj_bomb), Err(ProtoError::NodeBudgetExceeded));

    // 3. Rowset with 64 KiB column name cloned 1,200 times
    let rs_seed = PyRep::Rowset {
        columns: vec![big_str.clone()],
        rows: vec![vec![PyRep::Integer(1)]],
    };
    let rs_bomb = build_saved_reference_stream(&rs_seed, 1_200);
    assert_eq!(decode(&rs_bomb), Err(ProtoError::NodeBudgetExceeded));

    // 4. IndexRowset with 64 KiB `key_column` cloned 1,200 times
    let irs_seed = PyRep::IndexRowset {
        key_column: big_str.clone(),
        columns: vec!["id".into()],
        items: vec![(PyRep::Integer(1), vec![PyRep::Integer(1)])],
    };
    let irs_bomb = build_saved_reference_stream(&irs_seed, 1_200);
    assert_eq!(decode(&irs_bomb), Err(ProtoError::NodeBudgetExceeded));

    // 5. DBRow (`PyPackedRow` 0x2A) with 64 KiB column name cloned 1,200 times
    let dbrow_seed = DBRowDescriptor::new([(big_str, DbColumnType::I8)])
        .build_dbrow(vec![PyRep::Integer(42)]);
    let dbrow_bomb = build_saved_reference_stream(&dbrow_seed, 1_200);
    assert_eq!(decode(&dbrow_bomb), Err(ProtoError::NodeBudgetExceeded));

    // 6. Many `PyPackedRow` (0x2A) opcodes sharing one saved 256-column I8 descriptor with 0-byte RLE:
    // Each row unpacks 256 fixed-width columns + 2,081 RLE bytes (~545 weight units) before `try_unpack_packed_row_columns`.
    let wide_cols: Vec<(String, DbColumnType)> = (0..256)
        .map(|i| (format!("c{i}"), DbColumnType::I8))
        .collect();
    let hdr_frame = encode(&DBRowDescriptor::new(wide_cols).to_pyrep());
    let hdr_body = &hdr_frame[5..];

    let row_repeats = 2_500usize;
    let mut wide_packed_bomb = Vec::new();
    wide_packed_bomb.push(MARSHAL_HEADER_BYTE);
    wide_packed_bomb.extend_from_slice(&1u32.to_le_bytes()); // save_count = 1 (the descriptor)
    wide_packed_bomb.push(MarshalOpcode::PyTuple as u8);
    write_size_ex(&mut wide_packed_bomb, row_repeats);

    // First PyPackedRow saves the descriptor in slot 1, with 0-byte RLE
    wide_packed_bomb.push(MarshalOpcode::PyPackedRow as u8);
    wide_packed_bomb.push(hdr_body[0] | SAVE_FLAG);
    wide_packed_bomb.extend_from_slice(&hdr_body[1..]);
    wide_packed_bomb.push(0u8); // rle_len = 0

    // Remaining PyPackedRows reference descriptor slot 1 (`0x1B 0x01`) + 0-byte RLE (`0x00`) — 4 bytes per row!
    for _ in 1..row_repeats {
        wide_packed_bomb.push(MarshalOpcode::PyPackedRow as u8);
        wide_packed_bomb.push(MarshalOpcode::PySavedStreamElement as u8);
        wide_packed_bomb.push(1u8);
        wide_packed_bomb.push(0u8);
    }
    wide_packed_bomb.extend_from_slice(&1u32.to_le_bytes());
    assert!(wide_packed_bomb.len() < 32 * 1024);
    assert_eq!(decode(&wide_packed_bomb), Err(ProtoError::NodeBudgetExceeded));
}

#[test]
fn test_initial_decode_charges_byte_weights_on_string_buffer_and_packed_row_opcodes() {
    let payload_2k = vec![b'Z'; 2048]; // 1 + 32 = 33 weight units
    let cases = [
        encode(&PyRep::Bytes(payload_2k.clone())),
        encode(&PyRep::Buffer(payload_2k.clone())),
        encode(&PyRep::Picked(payload_2k)),
        encode(&PyRep::String("U".repeat(2048))),
        encode(&PyRep::Token("T".repeat(2048))),
    ];
    for wire in &cases {
        assert_eq!(
            decode_with_limits(wire, MAX_FRAME_SIZE, 32),
            Err(ProtoError::NodeBudgetExceeded)
        );
        assert!(decode_with_limits(wire, MAX_FRAME_SIZE, 33).is_ok());
    }

    // PyShortString (255 bytes -> 1 + 3 = 4 weight units)
    let short_255 = encode(&PyRep::Bytes(vec![b's'; 255]));
    assert_eq!(
        decode_with_limits(&short_255, MAX_FRAME_SIZE, 3),
        Err(ProtoError::NodeBudgetExceeded)
    );
    assert!(decode_with_limits(&short_255, MAX_FRAME_SIZE, 4).is_ok());

    // PyWStringUCS2 with 3-byte UTF-8 code units (`0x20AC` '€': 200 chars = 400 UCS2 bytes -> 600 UTF-8 bytes -> 1 + 9 = 10 units)
    let mut ucs2_wire = vec![MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PyWStringUCS2 as u8];
    write_size_ex(&mut ucs2_wire, 200);
    for _ in 0..200 {
        ucs2_wire.extend_from_slice(&0x20ACu16.to_le_bytes());
    }
    assert_eq!(
        decode_with_limits(&ucs2_wire, MAX_FRAME_SIZE, 9),
        Err(ProtoError::NodeBudgetExceeded)
    );
    assert_eq!(
        decode_with_limits(&ucs2_wire, MAX_FRAME_SIZE, 10).unwrap(),
        PyRep::String("€".repeat(200))
    );

    // PyPackedRow initial decode charges descriptor + RLE + unpacked columns/bytes under tight budget
    let row = DBRowDescriptor::inv_item_header().build_dbrow(vec![
        PyRep::Integer(100_000_001),
        PyRep::Integer(587),
        PyRep::Integer(90_000_001),
        PyRep::Integer(60_003_760),
        PyRep::Integer(4),
        PyRep::Integer(1),
        PyRep::Integer(25),
        PyRep::Integer(6),
        PyRep::String("rifter".into()),
    ]);
    let row_wire = encode(&row);
    let min_needed = row.node_count();
    assert_eq!(
        decode_with_limits(&row_wire, MAX_FRAME_SIZE, min_needed - 1),
        Err(ProtoError::NodeBudgetExceeded)
    );
    assert_eq!(
        decode_with_limits(&row_wire, MAX_FRAME_SIZE, min_needed + 16).unwrap(),
        row
    );
}

#[test]
fn test_nested_zlib_substream_decodes_in_raw_and_zlib_outer_callreq_frames() {
    // Realistic V24.01 `macho.CallReq` wrapping a multi-item RPC argument tuple inside a `PySubStream`
    let fitting_items: Vec<PyRep> = (0..120)
        .map(|slot| {
            PyRep::Tuple(vec![
                PyRep::Integer(200_000_000 + slot),
                PyRep::Integer(11 + (slot % 8)),
                PyRep::Integer(1),
                PyRep::String(format!("Slot {slot} fitting note")),
            ])
        })
        .collect();

    let call_req = PyRep::Object {
        type_name: "macho.CallReq".into(),
        args: Box::new(PyRep::Tuple(vec![
            PyRep::Integer(6), // MachoPacketType::CallReq
            PyRep::Object {
                type_name: "macho.MachoAddress".into(),
                args: Box::new(PyRep::Tuple(vec![
                    PyRep::Integer(2),
                    PyRep::Integer(1_000_000_010_001),
                    PyRep::Integer(42),
                    PyRep::None,
                ])),
            },
            PyRep::Object {
                type_name: "macho.MachoAddress".into(),
                args: Box::new(PyRep::Tuple(vec![
                    PyRep::Integer(1),
                    PyRep::Integer(888_444),
                    PyRep::ascii_bytes("config"),
                    PyRep::Integer(0),
                ])),
            },
            PyRep::Integer(1001),
            PyRep::Tuple(vec![PyRep::Tuple(vec![
                PyRep::Integer(0),
                PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                    PyRep::ascii_bytes("GetMap"),
                    PyRep::Tuple(vec![PyRep::Integer(30_000_142), PyRep::List(fitting_items)]),
                    PyRep::Dict(Vec::new()),
                ]))),
            ])]),
            PyRep::Dict(Vec::new()),
        ])),
    };

    // Test both standard 32K zlib (`0x78`) and V24 `blue` 8K zlib (`0x58 0x09`)
    for (small_win, expected_cmf) in [(false, 0x78u8), (true, 0x58u8)] {
        let outer_raw_inner_zlib = encode_with_compressed_substreams(&call_req, small_win);
        let sample_z = zlib_compress_with_window(&encode(&PyRep::Integer(1)), small_win);
        assert_eq!(sample_z[0], expected_cmf);

        assert_eq!(
            decode(&outer_raw_inner_zlib).expect("raw outer + zlib PySubStream must decode"),
            call_req
        );

        let outer_zlib_inner_zlib = zlib_compress_with_window(&outer_raw_inner_zlib, small_win);
        assert_eq!(outer_zlib_inner_zlib[0], expected_cmf);
        assert_eq!(
            decode(&outer_zlib_inner_zlib).expect("zlib outer + zlib PySubStream must decode"),
            call_req
        );
    }
}

#[test]
fn test_nested_zlib_substream_decompression_bombs_enforced_by_shared_inflate_budget() {
    // 1. Single nested `PySubStream` inflating to > 4 MiB inside a tiny outer frame -> FrameTooLarge
    let huge_inner_raw = encode(&PyRep::Bytes(vec![0u8; MAX_FRAME_SIZE + 1024]));
    let huge_inner_z = zlib_compress_with_window(&huge_inner_raw, false);
    assert!(huge_inner_z.len() < 32 * 1024);

    let mut single_sub_bomb = vec![MARSHAL_HEADER_BYTE, 0, 0, 0, 0, MarshalOpcode::PySubStream as u8];
    write_size_ex(&mut single_sub_bomb, huge_inner_z.len());
    single_sub_bomb.extend_from_slice(&huge_inner_z);
    assert!(matches!(
        decode(&single_sub_bomb),
        Err(ProtoError::FrameTooLarge(_))
    ));

    // 2. Cumulative inflation bomb: 4 sibling `PySubStream`s each inflating ~1.2 MiB (each < 4 MiB, sum = 4.8 MiB > 4 MiB)
    let chunk_raw = encode(&PyRep::Bytes(vec![0xAA; 1_250_000]));
    let chunk_z = zlib_compress_with_window(&chunk_raw, false);
    assert!(chunk_z.len() < 16 * 1024);

    let mut cumulative_bomb = vec![
        MARSHAL_HEADER_BYTE,
        0,
        0,
        0,
        0,
        MarshalOpcode::PyTuple as u8,
        4,
    ];
    for _ in 0..4 {
        cumulative_bomb.push(MarshalOpcode::PySubStream as u8);
        write_size_ex(&mut cumulative_bomb, chunk_z.len());
        cumulative_bomb.extend_from_slice(&chunk_z);
    }
    assert!(matches!(
        decode(&cumulative_bomb),
        Err(ProtoError::FrameTooLarge(_))
    ));

    // 3. Outer `zlib` (2.5 MiB) + inner `zlib` `PySubStream` (2.0 MiB) = 4.5 MiB > 4 MiB -> FrameTooLarge
    let inner_2m_z = zlib_compress_with_window(&encode(&PyRep::Bytes(vec![0x11; 2_000_000])), false);
    let mut outer_plus_inner_raw = vec![
        MARSHAL_HEADER_BYTE,
        0,
        0,
        0,
        0,
        MarshalOpcode::PyTwoTuple as u8,
    ];
    super::encode_inner(&PyRep::Bytes(vec![0x22; 2_500_000]), &mut outer_plus_inner_raw);
    outer_plus_inner_raw.push(MarshalOpcode::PySubStream as u8);
    write_size_ex(&mut outer_plus_inner_raw, inner_2m_z.len());
    outer_plus_inner_raw.extend_from_slice(&inner_2m_z);
    let outer_plus_inner_z = zlib_compress_with_window(&outer_plus_inner_raw, false);
    assert!(matches!(
        decode(&outer_plus_inner_z),
        Err(ProtoError::FrameTooLarge(_))
    ));

    // 4. Double-`zlib` without an intervening `PySubStream` opcode must fail header validation (`InvalidHeader(0x78)`)
    let valid_z = encode_zlib(&PyRep::Integer(42)).unwrap();
    let double_z = zlib_compress_with_window(&valid_z, false);
    assert_eq!(decode(&double_z), Err(ProtoError::InvalidHeader(0x78)));
}

fn arb_pyrep() -> impl Strategy<Value = PyRep> {
    let leaf = prop_oneof![
        Just(PyRep::None),
        any::<bool>().prop_map(PyRep::Bool),
        any::<i64>().prop_map(PyRep::Integer),
        prop::collection::vec(any::<u8>(), 0..300).prop_map(PyRep::Bytes),
        prop::collection::vec(any::<u8>(), 0..300).prop_map(PyRep::Buffer),
        "[a-zA-Z0-9_.]{0,200}".prop_map(PyRep::String),
    ];
    leaf.prop_recursive(3, 32, 6, |inner| {
        prop_oneof![
            prop::collection::vec(inner.clone(), 0..5).prop_map(PyRep::Tuple),
            inner.clone().prop_map(|x| PyRep::SubStream(Box::new(x))),
            ("[a-zA-Z_]{1,96}", inner.clone()).prop_map(|(k, v)| PyRep::keyval([(k, v)])),
            ("[a-zA-Z.]{3,96}", inner).prop_map(|(tn, a)| PyRep::Object {
                type_name: tn,
                args: Box::new(a),
            }),
        ]
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn prop_compressed_nested_substreams_and_node_budget_invariant(
        rep in arb_pyrep(),
        small_win in any::<bool>(),
    ) {
        // 1. Compressed nested PySubStream roundtrip (raw outer + zlib outer)
        let raw_outer = encode_with_compressed_substreams(&rep, small_win);
        let dec_raw = decode(&raw_outer).expect("nested zlib substream must decode");
        prop_assert_eq!(&dec_raw, &rep);

        let zlib_outer = zlib_compress_with_window(&raw_outer, small_win);
        let dec_zlib = decode(&zlib_outer).expect("outer+inner zlib substream must decode");
        prop_assert_eq!(&dec_zlib, &rep);

        // 2. Strict node-budget theorem: `decode_with_limits` must reject any budget < `rep.node_count()`
        let wire = encode(&rep);
        let needed = rep.node_count();
        prop_assert_eq!(
            decode_with_limits(&wire, MAX_FRAME_SIZE, needed - 1),
            Err(ProtoError::NodeBudgetExceeded)
        );
        if let Ok(ok_rep) = decode_with_limits(&wire, MAX_FRAME_SIZE, MAX_DECODED_NODES) {
            prop_assert!(ok_rep.node_count() <= MAX_DECODED_NODES);
        }
    }
}

#[test]
fn save_flag_deep_clone_retention_rejects_amplification() {
    use crate::marshal::{MarshalOpcode, MARSHAL_HEADER_BYTE, SAVE_FLAG};

    // 1. 62 nested `SAVE_FLAG | PyOneTuple` wrappers retain O(N^2) cloned AST nodes in `saved_slots`,
    //    which must be charged against `remaining_budget` (rejecting at bounded budgets).
    let mut payload_one = vec![MARSHAL_HEADER_BYTE];
    payload_one.extend_from_slice(&62u32.to_le_bytes()); // save_count = 62
    for _ in 0..62 {
        payload_one.push((MarshalOpcode::PyOneTuple as u8) | SAVE_FLAG);
    }
    payload_one.push(MarshalOpcode::PyNone as u8);
    for idx in 1u32..=62u32 {
        payload_one.extend_from_slice(&idx.to_le_bytes());
    }
    assert_eq!(
        decode_with_limits(&payload_one, MAX_FRAME_SIZE, 1_000),
        Err(ProtoError::NodeBudgetExceeded)
    );

    // 2. `SAVE_FLAG | PyTwoTuple` with back-references (`PySavedStreamElement`) produces exponential
    //    O(2^k) clone retention in `saved_slots` using < 64 save slots; must trigger
    //    NodeBudgetExceeded well within pre-auth and default post-auth limits.
    let mut dag_payload = vec![MARSHAL_HEADER_BYTE];
    dag_payload.extend_from_slice(&24u32.to_le_bytes());
    dag_payload.push(MarshalOpcode::PyTuple as u8);
    dag_payload.push(24);
    dag_payload.push((MarshalOpcode::PyNone as u8) | SAVE_FLAG); // slot 1
    for prev_slot in 1u8..=23u8 {
        dag_payload.push((MarshalOpcode::PyTwoTuple as u8) | SAVE_FLAG);
        dag_payload.push(MarshalOpcode::PySavedStreamElement as u8);
        dag_payload.push(prev_slot);
        dag_payload.push(MarshalOpcode::PySavedStreamElement as u8);
        dag_payload.push(prev_slot);
    }
    for idx in 1u32..=24u32 {
        dag_payload.extend_from_slice(&idx.to_le_bytes());
    }
    assert_eq!(
        decode_with_limits(&dag_payload, 64 * 1024, 4_096),
        Err(ProtoError::NodeBudgetExceeded)
    );
    assert_eq!(
        decode_with_limits(&dag_payload, MAX_FRAME_SIZE, MAX_DECODED_NODES),
        Err(ProtoError::NodeBudgetExceeded)
    );
    assert_eq!(decode(&dag_payload), Err(ProtoError::NodeBudgetExceeded));

    // 3. `util.Row` fast path (`SAVE_FLAG | PyObjectEx2`) also clones into `saved_slots` and must
    //    charge `val.node_count().saturating_sub(1)` against `remaining_budget`.
    let row_rep = PyRep::Object {
        type_name: "util.Row".into(),
        args: Box::new(PyRep::Tuple(vec![PyRep::None; 4])),
    };
    let mut row_payload = encode(&row_rep);
    row_payload[1..5].copy_from_slice(&1u32.to_le_bytes());
    row_payload[5] |= SAVE_FLAG; // set SAVE_FLAG on root PyObjectEx2
    row_payload.extend_from_slice(&1u32.to_le_bytes());
    let no_save_row = encode(&row_rep);
    let min_no_save_budget = (1usize..64)
        .find(|&b| decode_with_limits(&no_save_row, MAX_FRAME_SIZE, b).is_ok())
        .expect("row without SAVE_FLAG must decode within 64 nodes");
    assert_eq!(
        decode_with_limits(&row_payload, MAX_FRAME_SIZE, min_no_save_budget),
        Err(ProtoError::NodeBudgetExceeded)
    );
    assert_eq!(
        decode_with_limits(
            &row_payload,
            MAX_FRAME_SIZE,
            min_no_save_budget + row_rep.node_count() - 1
        ),
        Ok(row_rep)
    );
}
