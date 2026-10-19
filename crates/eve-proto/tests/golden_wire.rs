//! Task T-4: Golden wire tests round-tripping Marshal opcodes (`0x01`..=`0x2F`), `PyPackedRow`,
//! `KeyVal`, `Rowset`, `IndexRowset`, `TupleSet`, `ObjectEx`, Destiny `AddBalls` (`DST_BALL_*` `0..=12`)
//! and `DoDestinyUpdate` payloads, and Quasar `publicGatewaySvc` Protobuf envelopes between Rust
//! (`eve-proto` / `eve-destiny`) and Python (`client-bridge/evomu_bridge.py`).

#![forbid(unsafe_code)]

use eve_destiny::{
    wire::{
        decode_add_balls, encode_add_balls, BALL_FLAG_IS_FREE, BALL_FLAG_IS_GLOBAL,
        BALL_FLAG_IS_INTERACTIVE, BALL_FLAG_IS_MASSIVE, DST_BALL_BOID, DST_BALL_FIELD,
        DST_BALL_FOLLOW, DST_BALL_FORMATION, DST_BALL_GOTO, DST_BALL_MINIBALL, DST_BALL_MISSILE,
        DST_BALL_MUSHROOM, DST_BALL_ORBIT, DST_BALL_RIGID, DST_BALL_STOP, DST_BALL_TROLL,
        DST_BALL_WARP,
    },
    Ball, MovementMode,
};
use eve_proto::{
    marshal,
    quasar_wire::{
        decode_envelope, encode_envelope, encode_fields, ProtoWireValue, QuasarGatewayEnvelope,
    },
    MarshalOpcode, PyRep, MARSHAL_HEADER_BYTE, SAVE_FLAG, ZLIB_HEADER_BYTE,
};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct MarshalGoldenFixture {
    name: String,
    opcode_hex: String,
    wire_bytes: Vec<u8>,
    expected_pyrep: serde_json::Value,
    canonical_roundtrip: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct DestinyBallSummary {
    id: i64,
    mode_byte: u8,
    flags: u8,
    radius: f64,
    position: [f64; 3],
    mass: f64,
    max_velocity: f64,
    velocity: [f64; 3],
    inertia: f64,
    speed_fraction: f64,
    extra: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct DestinyAddBallsGoldenFixture {
    name: String,
    stamp: u32,
    wire_bytes: Vec<u8>,
    balls: Vec<DestinyBallSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct DestinyUpdateGoldenFixture {
    name: String,
    action: String,
    wire_bytes: Vec<u8>,
    expected_pyrep: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct QuasarGoldenFixture {
    name: String,
    package: String,
    wire_bytes: Vec<u8>,
    envelope: QuasarGatewayEnvelope,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct GoldenWireSuite {
    version: u32,
    marshal_fixtures: Vec<MarshalGoldenFixture>,
    destiny_add_balls_fixtures: Vec<DestinyAddBallsGoldenFixture>,
    destiny_update_fixtures: Vec<DestinyUpdateGoldenFixture>,
    quasar_fixtures: Vec<QuasarGoldenFixture>,
}

fn canonical_fixture(name: &str, opcode: MarshalOpcode, rep: PyRep) -> MarshalGoldenFixture {
    let wire_bytes = marshal::encode(&rep);
    assert_eq!(
        wire_bytes[5],
        opcode as u8,
        "Fixture {name} expected root opcode 0x{:02x}, got 0x{:02x}",
        opcode as u8,
        wire_bytes[5]
    );
    let decoded = marshal::decode(&wire_bytes).expect("marshal decode canonical fixture");
    assert_eq!(decoded, rep, "Fixture {name} failed Rust decode");
    MarshalGoldenFixture {
        name: name.to_string(),
        opcode_hex: format!("0x{:02x}", opcode as u8),
        wire_bytes,
        expected_pyrep: serde_json::to_value(&rep).expect("serialize PyRep JSON"),
        canonical_roundtrip: true,
    }
}

fn build_golden_wire_suite() -> GoldenWireSuite {
    let long_bytes: Vec<u8> = (0..260).map(|i| (i % 251) as u8).collect();
    let dbrow = marshal::pack_dbrow(&[
        ("itemID", 20, PyRep::Integer(100_000_001)),
        ("typeID", 3, PyRep::Integer(587)),
        ("ownerID", 3, PyRep::Integer(90_000_001)),
        ("locationID", 20, PyRep::Integer(60_003_760)),
        ("flagID", 2, PyRep::Integer(4)),
        ("quantity", 3, PyRep::Integer(1)),
        ("groupID", 2, PyRep::Integer(25)),
        ("categoryID", 17, PyRep::Integer(6)),
        ("singleton", 11, PyRep::Bool(true)),
        (
            "customInfo",
            130,
            PyRep::String("Caldari Navy Assembly Plant".into()),
        ),
    ]);

    let mut marshal_fixtures = vec![
        canonical_fixture("op_01_py_none", MarshalOpcode::PyNone, PyRep::None),
        canonical_fixture(
            "op_02_py_token",
            MarshalOpcode::PyToken,
            PyRep::Token("macho.CallReq".into()),
        ),
        canonical_fixture(
            "op_03_py_long_long",
            MarshalOpcode::PyLongLong,
            PyRep::Integer(9_000_000_000_000_001),
        ),
        canonical_fixture(
            "op_04_py_long",
            MarshalOpcode::PyLong,
            PyRep::Integer(30_000_142),
        ),
        canonical_fixture(
            "op_05_py_signed_short",
            MarshalOpcode::PySignedShort,
            PyRep::Integer(-1_024),
        ),
        canonical_fixture(
            "op_06_py_byte",
            MarshalOpcode::PyByte,
            PyRep::Integer(42),
        ),
        canonical_fixture(
            "op_07_py_minus_one",
            MarshalOpcode::PyMinusOne,
            PyRep::Integer(-1),
        ),
        canonical_fixture(
            "op_08_py_zero_integer",
            MarshalOpcode::PyZeroInteger,
            PyRep::Integer(0),
        ),
        canonical_fixture(
            "op_09_py_one_integer",
            MarshalOpcode::PyOneInteger,
            PyRep::Integer(1),
        ),
        canonical_fixture(
            "op_0a_py_real",
            MarshalOpcode::PyReal,
            PyRep::Real(149_597_870_700.0),
        ),
        canonical_fixture(
            "op_0b_py_zero_real",
            MarshalOpcode::PyZeroReal,
            PyRep::Real(0.0),
        ),
        canonical_fixture(
            "op_0d_py_buffer",
            MarshalOpcode::PyBuffer,
            PyRep::Buffer(vec![0x00, 0x01, 0x02, 0xFE, 0xFF]),
        ),
        canonical_fixture(
            "op_0e_py_empty_string",
            MarshalOpcode::PyEmptyString,
            PyRep::Bytes(vec![]),
        ),
        canonical_fixture(
            "op_0f_py_char_string",
            MarshalOpcode::PyCharString,
            PyRep::Bytes(vec![b'X']),
        ),
        canonical_fixture(
            "op_10_py_short_string",
            MarshalOpcode::PyShortString,
            PyRep::Bytes(b"Jita IV - Moon 4".to_vec()),
        ),
        canonical_fixture(
            "op_13_py_long_string",
            MarshalOpcode::PyLongString,
            PyRep::Bytes(long_bytes),
        ),
        canonical_fixture(
            "op_14_py_tuple_many",
            MarshalOpcode::PyTuple,
            PyRep::Tuple(vec![
                PyRep::Integer(30_000_142),
                PyRep::String("Jita".into()),
                PyRep::Bool(true),
            ]),
        ),
        canonical_fixture(
            "op_15_py_list_many",
            MarshalOpcode::PyList,
            PyRep::List(vec![
                PyRep::Integer(587),
                PyRep::Integer(24_692),
                PyRep::Integer(11_393),
            ]),
        ),
        canonical_fixture(
            "op_16_py_dict",
            MarshalOpcode::PyDict,
            PyRep::Dict(vec![
                (PyRep::String("solarSystemID".into()), PyRep::Integer(30_000_142)),
                (PyRep::String("stationID".into()), PyRep::Integer(60_003_760)),
            ]),
        ),
        canonical_fixture(
            "op_17_py_object_keyval",
            MarshalOpcode::PyObject,
            PyRep::keyval([
                ("characterID", PyRep::Integer(90_000_001)),
                ("shipID", PyRep::Integer(100_000_001)),
            ]),
        ),
        canonical_fixture(
            "op_17_py_object_rowset",
            MarshalOpcode::PyObject,
            PyRep::rowset(
                vec!["itemID", "typeID", "quantity"],
                vec![
                    vec![
                        PyRep::Integer(100_000_001),
                        PyRep::Integer(587),
                        PyRep::Integer(1),
                    ],
                    vec![
                        PyRep::Integer(100_000_002),
                        PyRep::Integer(34),
                        PyRep::Integer(50_000),
                    ],
                ],
            ),
        ),
        canonical_fixture(
            "op_17_py_object_index_rowset",
            MarshalOpcode::PyObject,
            PyRep::index_rowset(
                "itemID",
                vec!["itemID", "typeID"],
                vec![(
                    PyRep::Integer(100_000_001),
                    vec![PyRep::Integer(100_000_001), PyRep::Integer(587)],
                )],
            ),
        ),
        canonical_fixture(
            "op_17_py_object_custom",
            MarshalOpcode::PyObject,
            PyRep::Object {
                type_name: "foo.SlimItem".into(),
                args: Box::new(PyRep::Dict(vec![(
                    PyRep::String("itemID".into()),
                    PyRep::Integer(100_000_001),
                )])),
            },
        ),
        canonical_fixture(
            "op_19_py_sub_struct",
            MarshalOpcode::PySubStruct,
            PyRep::SubStruct(Box::new(PyRep::Integer(60_003_760))),
        ),
        canonical_fixture(
            "op_1c_py_checksumed_stream",
            MarshalOpcode::PyChecksumedStream,
            PyRep::ChecksumedStream {
                checksum: 0xA55A_1234,
                body: Box::new(PyRep::String("checksummed".into())),
            },
        ),
        canonical_fixture(
            "op_1f_py_true",
            MarshalOpcode::PyTrue,
            PyRep::Bool(true),
        ),
        canonical_fixture(
            "op_20_py_false",
            MarshalOpcode::PyFalse,
            PyRep::Bool(false),
        ),
        canonical_fixture(
            "op_21_py_picked",
            MarshalOpcode::PyPicked,
            PyRep::Picked(vec![0x80, 0x02, 0x4B, 0x2A, 0x2E]),
        ),
        canonical_fixture(
            "op_22_py_object_ex_1",
            MarshalOpcode::PyObjectEx1,
            PyRep::ObjectEx {
                is_type_2: false,
                header: Box::new(PyRep::Tuple(vec![PyRep::Token("util.CachedObject".into())])),
                list: vec![PyRep::Integer(100_000_001)],
                dict: vec![(PyRep::String("version".into()), PyRep::Integer(3))],
            },
        ),
        canonical_fixture(
            "op_23_py_object_ex_2",
            MarshalOpcode::PyObjectEx2,
            PyRep::ObjectEx {
                is_type_2: true,
                header: Box::new(PyRep::Tuple(vec![PyRep::Token("util.Row".into())])),
                list: vec![PyRep::Integer(587)],
                dict: vec![(PyRep::String("typeID".into()), PyRep::Integer(587))],
            },
        ),
        canonical_fixture(
            "op_24_py_empty_tuple",
            MarshalOpcode::PyEmptyTuple,
            PyRep::Tuple(vec![]),
        ),
        canonical_fixture(
            "op_25_py_one_tuple",
            MarshalOpcode::PyOneTuple,
            PyRep::Tuple(vec![PyRep::Integer(30_000_142)]),
        ),
        canonical_fixture(
            "op_26_py_empty_list",
            MarshalOpcode::PyEmptyList,
            PyRep::List(vec![]),
        ),
        canonical_fixture(
            "op_27_py_one_list",
            MarshalOpcode::PyOneList,
            PyRep::List(vec![PyRep::String("Aura".into())]),
        ),
        canonical_fixture(
            "op_28_py_empty_wstring",
            MarshalOpcode::PyEmptyWString,
            PyRep::String(String::new()),
        ),
        canonical_fixture(
            "op_2a_py_packed_row_with_trailing_wstr",
            MarshalOpcode::PyPackedRow,
            dbrow,
        ),
        canonical_fixture(
            "op_2b_py_sub_stream",
            MarshalOpcode::PySubStream,
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::Token("macho.CallRsp".into()),
                PyRep::Integer(90_000_001),
            ]))),
        ),
        canonical_fixture(
            "op_2c_py_two_tuple",
            MarshalOpcode::PyTwoTuple,
            PyRep::Tuple(vec![
                PyRep::Integer(60_003_760),
                PyRep::String("Jita IV - Moon 4".into()),
            ]),
        ),
        canonical_fixture(
            "op_2e_py_wstring_utf8",
            MarshalOpcode::PyWStringUTF8,
            PyRep::String("Caldari Navy Assembly Plant".into()),
        ),
    ];

    // Add client-originated opcodes that normalize on decode (`0x11` PyStringTableItem, `0x12` PyWStringUCS2,
    // `0x1B` PySavedStreamElement + `SAVE_FLAG`, `0x29` PyWStringUCS2Char, `0x2F` PyVarInteger, and `0x78` zlib).
    let str_table_wire = vec![
        MARSHAL_HEADER_BYTE,
        0,
        0,
        0,
        0,
        MarshalOpcode::PyStringTableItem as u8,
        5, // 1-based index 5 => "authentication"
    ];
    let str_table_rep = marshal::decode(&str_table_wire).unwrap();
    assert_eq!(str_table_rep, PyRep::String("authentication".into()));
    marshal_fixtures.push(MarshalGoldenFixture {
        name: "op_11_py_string_table_item".into(),
        opcode_hex: "0x11".into(),
        wire_bytes: str_table_wire,
        expected_pyrep: serde_json::to_value(&str_table_rep).unwrap(),
        canonical_roundtrip: false,
    });

    let ucs2_wire = vec![
        MARSHAL_HEADER_BYTE,
        0,
        0,
        0,
        0,
        MarshalOpcode::PyWStringUCS2 as u8,
        4, // 4 UTF-16LE chars: "Jita"
        b'J',
        0,
        b'i',
        0,
        b't',
        0,
        b'a',
        0,
    ];
    let ucs2_rep = marshal::decode(&ucs2_wire).unwrap();
    assert_eq!(ucs2_rep, PyRep::String("Jita".into()));
    marshal_fixtures.push(MarshalGoldenFixture {
        name: "op_12_py_wstring_ucs2".into(),
        opcode_hex: "0x12".into(),
        wire_bytes: ucs2_wire,
        expected_pyrep: serde_json::to_value(&ucs2_rep).unwrap(),
        canonical_roundtrip: false,
    });

    let ucs2_char_wire = vec![
        MARSHAL_HEADER_BYTE,
        0,
        0,
        0,
        0,
        MarshalOpcode::PyWStringUCS2Char as u8,
        b'A',
        0,
    ];
    let ucs2_char_rep = marshal::decode(&ucs2_char_wire).unwrap();
    assert_eq!(ucs2_char_rep, PyRep::String("A".into()));
    marshal_fixtures.push(MarshalGoldenFixture {
        name: "op_29_py_wstring_ucs2_char".into(),
        opcode_hex: "0x29".into(),
        wire_bytes: ucs2_char_wire,
        expected_pyrep: serde_json::to_value(&ucs2_char_rep).unwrap(),
        canonical_roundtrip: false,
    });

    let varint_wire = vec![
        MARSHAL_HEADER_BYTE,
        0,
        0,
        0,
        0,
        MarshalOpcode::PyVarInteger as u8,
        3,
        0x79,
        0x29,
        0xED, // -1234567 in 3-byte signed two's complement LE
    ];
    let varint_rep = marshal::decode(&varint_wire).unwrap();
    assert_eq!(varint_rep, PyRep::Integer(-1_234_567));
    marshal_fixtures.push(MarshalGoldenFixture {
        name: "op_2f_py_var_integer_signed".into(),
        opcode_hex: "0x2f".into(),
        wire_bytes: varint_wire,
        expected_pyrep: serde_json::to_value(&varint_rep).unwrap(),
        canonical_roundtrip: false,
    });

    let mut saved_wire = vec![
        MARSHAL_HEADER_BYTE,
        1,
        0,
        0,
        0,
        MarshalOpcode::PyTwoTuple as u8,
        (MarshalOpcode::PyToken as u8) | SAVE_FLAG,
        11,
    ];
    saved_wire.extend_from_slice(b"util.KeyVal");
    saved_wire.push(MarshalOpcode::PySavedStreamElement as u8);
    saved_wire.push(1);
    saved_wire.extend_from_slice(&1u32.to_le_bytes());
    let saved_rep = marshal::decode(&saved_wire).unwrap();
    marshal_fixtures.push(MarshalGoldenFixture {
        name: "op_1b_py_saved_stream_element".into(),
        opcode_hex: "0x1b".into(),
        wire_bytes: saved_wire,
        expected_pyrep: serde_json::to_value(&saved_rep).unwrap(),
        canonical_roundtrip: false,
    });

    let zlib_rep = PyRep::Tuple(vec![
        PyRep::Token("DoDestinyUpdate".into()),
        PyRep::Integer(30_000_142),
    ]);
    let zlib_wire = marshal::encode_zlib(&zlib_rep).unwrap();
    assert_eq!(zlib_wire[0], ZLIB_HEADER_BYTE);
    marshal_fixtures.push(MarshalGoldenFixture {
        name: "op_78_zlib_marshal_stream".into(),
        opcode_hex: "0x78".into(),
        wire_bytes: zlib_wire,
        expected_pyrep: serde_json::to_value(&zlib_rep).unwrap(),
        canonical_roundtrip: false,
    });

    // Destiny AddBalls golden fixture covering all 13 DST_BALL_* modes (0..=12)
    let all_mode_balls = vec![
        Ball {
            id: 100_000_001,
            type_id: 587,
            position: DVec3::new(1000.0, 2000.0, 3000.0),
            velocity: DVec3::new(120.0, 0.0, 0.0),
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 1.0,
            mode: MovementMode::GotoDirection {
                direction: DVec3::new(1.0, 0.0, 0.0),
            },
        },
        Ball {
            id: 100_000_002,
            type_id: 587,
            position: DVec3::new(2000.0, 2000.0, 3000.0),
            velocity: DVec3::new(100.0, 50.0, 0.0),
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 1.0,
            mode: MovementMode::Follow {
                target_id: 100_000_001,
                range: 2500.0,
            },
        },
        Ball {
            id: 100_000_003,
            type_id: 587,
            position: DVec3::new(3000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 0.0,
            mode: MovementMode::Stop,
        },
        Ball {
            id: 100_000_004,
            type_id: 587,
            position: DVec3::new(4000.0, 0.0, 0.0),
            velocity: DVec3::new(300.0, 0.0, 0.0),
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 1.0,
            mode: MovementMode::new_warp(
                DVec3::new(4000.0, 0.0, 0.0),
                DVec3::new(1.0e9, 0.0, 0.0),
                3.0,
            ),
        },
        Ball {
            id: 100_000_005,
            type_id: 587,
            position: DVec3::new(5000.0, 0.0, 0.0),
            velocity: DVec3::new(0.0, 250.0, 0.0),
            max_velocity: 365.0,
            inertia: 3.45,
            mass: 1_067_000.0,
            radius: 37.0,
            speed_fraction: 1.0,
            mode: MovementMode::Orbit {
                target_id: 100_000_001,
                range: 5000.0,
            },
        },
        Ball {
            id: 100_000_006,
            type_id: 200,
            position: DVec3::new(6000.0, 0.0, 0.0),
            velocity: DVec3::new(3500.0, 0.0, 0.0),
            max_velocity: 3500.0,
            inertia: 1.0,
            mass: 0.0,
            radius: 5.0,
            speed_fraction: 1.0,
            mode: MovementMode::Missile {
                target_id: 100_000_001,
            },
        },
        Ball {
            id: 100_000_007,
            type_id: 201,
            position: DVec3::new(7000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 100.0,
            inertia: 1.0,
            mass: 0.0,
            radius: 50.0,
            speed_fraction: 0.0,
            mode: MovementMode::Mushroom,
        },
        Ball {
            id: 100_000_008,
            type_id: 202,
            position: DVec3::new(8000.0, 0.0, 0.0),
            velocity: DVec3::new(100.0, 0.0, 0.0),
            max_velocity: 200.0,
            inertia: 1.0,
            mass: 0.0,
            radius: 15.0,
            speed_fraction: 1.0,
            mode: MovementMode::Boid,
        },
        Ball {
            id: 100_000_009,
            type_id: 203,
            position: DVec3::new(9000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 50.0,
            inertia: 1.0,
            mass: 0.0,
            radius: 25.0,
            speed_fraction: 0.0,
            mode: MovementMode::Troll,
        },
        Ball {
            id: 100_000_010,
            type_id: 204,
            position: DVec3::new(10_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 10.0,
            inertia: 1.0,
            mass: 0.0,
            radius: 10.0,
            speed_fraction: 0.0,
            mode: MovementMode::Miniball,
        },
        Ball {
            id: 100_000_011,
            type_id: 205,
            position: DVec3::new(11_000.0, 0.0, 0.0),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 0.0,
            radius: 15_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Field,
        },
        Ball {
            id: 60_003_760,
            type_id: 1529,
            position: DVec3::new(-1.0e11, 2.0e10, 3.0e10),
            velocity: DVec3::ZERO,
            max_velocity: 0.0,
            inertia: 1.0,
            mass: 1.0e15,
            radius: 25_000.0,
            speed_fraction: 0.0,
            mode: MovementMode::Rigid,
        },
        Ball {
            id: 100_000_013,
            type_id: 206,
            position: DVec3::new(13_000.0, 0.0, 0.0),
            velocity: DVec3::new(150.0, 0.0, 0.0),
            max_velocity: 300.0,
            inertia: 1.0,
            mass: 0.0,
            radius: 20.0,
            speed_fraction: 0.5,
            mode: MovementMode::Formation,
        },
    ];
    let ball_refs: Vec<&Ball> = all_mode_balls.iter().collect();
    let add_balls_wire = encode_add_balls(42, &ball_refs);
    let (decoded_stamp, decoded_balls) =
        decode_add_balls(&add_balls_wire).expect("decode_add_balls must succeed");
    assert_eq!(decoded_stamp, 42);
    assert_eq!(decoded_balls.len(), 13);

    let summaries = vec![
        DestinyBallSummary {
            id: 100_000_001,
            mode_byte: DST_BALL_GOTO,
            flags: BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
            radius: 37.0,
            position: [1000.0, 2000.0, 3000.0],
            mass: 1_067_000.0,
            max_velocity: 365.0,
            velocity: [120.0, 0.0, 0.0],
            inertia: 3.450000047683716,
            speed_fraction: 1.0,
            extra: serde_json::json!({"direction": [1.0, 0.0, 0.0]}),
        },
        DestinyBallSummary {
            id: 100_000_002,
            mode_byte: DST_BALL_FOLLOW,
            flags: BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
            radius: 37.0,
            position: [2000.0, 2000.0, 3000.0],
            mass: 1_067_000.0,
            max_velocity: 365.0,
            velocity: [100.0, 50.0, 0.0],
            inertia: 3.450000047683716,
            speed_fraction: 1.0,
            extra: serde_json::json!({"target_id": 100_000_001, "range": 2500.0}),
        },
        DestinyBallSummary {
            id: 100_000_003,
            mode_byte: DST_BALL_STOP,
            flags: BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
            radius: 37.0,
            position: [3000.0, 0.0, 0.0],
            mass: 1_067_000.0,
            max_velocity: 365.0,
            velocity: [0.0, 0.0, 0.0],
            inertia: 3.450000047683716,
            speed_fraction: 0.0,
            extra: serde_json::json!({}),
        },
        DestinyBallSummary {
            id: 100_000_004,
            mode_byte: DST_BALL_WARP,
            flags: BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
            radius: 37.0,
            position: [4000.0, 0.0, 0.0],
            mass: 1_067_000.0,
            max_velocity: 365.0,
            velocity: [300.0, 0.0, 0.0],
            inertia: 3.450000047683716,
            speed_fraction: 1.0,
            extra: serde_json::json!({"destination": [1.0e9, 0.0, 0.0], "warp_speed_au_s": 3.0}),
        },
        DestinyBallSummary {
            id: 100_000_005,
            mode_byte: DST_BALL_ORBIT,
            flags: BALL_FLAG_IS_FREE | BALL_FLAG_IS_MASSIVE | BALL_FLAG_IS_INTERACTIVE,
            radius: 37.0,
            position: [5000.0, 0.0, 0.0],
            mass: 1_067_000.0,
            max_velocity: 365.0,
            velocity: [0.0, 250.0, 0.0],
            inertia: 3.450000047683716,
            speed_fraction: 1.0,
            extra: serde_json::json!({"target_id": 100_000_001, "range": 5000.0}),
        },
        DestinyBallSummary {
            id: 100_000_006,
            mode_byte: DST_BALL_MISSILE,
            flags: BALL_FLAG_IS_FREE,
            radius: 5.0,
            position: [6000.0, 0.0, 0.0],
            mass: 0.0,
            max_velocity: 3500.0,
            velocity: [3500.0, 0.0, 0.0],
            inertia: 1.0,
            speed_fraction: 1.0,
            extra: serde_json::json!({"target_id": 100_000_001}),
        },
        DestinyBallSummary {
            id: 100_000_007,
            mode_byte: DST_BALL_MUSHROOM,
            flags: BALL_FLAG_IS_FREE,
            radius: 50.0,
            position: [7000.0, 0.0, 0.0],
            mass: 0.0,
            max_velocity: 100.0,
            velocity: [0.0, 0.0, 0.0],
            inertia: 1.0,
            speed_fraction: 0.0,
            extra: serde_json::json!({}),
        },
        DestinyBallSummary {
            id: 100_000_008,
            mode_byte: DST_BALL_BOID,
            flags: BALL_FLAG_IS_FREE,
            radius: 15.0,
            position: [8000.0, 0.0, 0.0],
            mass: 0.0,
            max_velocity: 200.0,
            velocity: [100.0, 0.0, 0.0],
            inertia: 1.0,
            speed_fraction: 1.0,
            extra: serde_json::json!({}),
        },
        DestinyBallSummary {
            id: 100_000_009,
            mode_byte: DST_BALL_TROLL,
            flags: BALL_FLAG_IS_FREE,
            radius: 25.0,
            position: [9000.0, 0.0, 0.0],
            mass: 0.0,
            max_velocity: 50.0,
            velocity: [0.0, 0.0, 0.0],
            inertia: 1.0,
            speed_fraction: 0.0,
            extra: serde_json::json!({}),
        },
        DestinyBallSummary {
            id: 100_000_010,
            mode_byte: DST_BALL_MINIBALL,
            flags: BALL_FLAG_IS_FREE,
            radius: 10.0,
            position: [10_000.0, 0.0, 0.0],
            mass: 0.0,
            max_velocity: 10.0,
            velocity: [0.0, 0.0, 0.0],
            inertia: 1.0,
            speed_fraction: 0.0,
            extra: serde_json::json!({}),
        },
        DestinyBallSummary {
            id: 100_000_011,
            mode_byte: DST_BALL_FIELD,
            flags: BALL_FLAG_IS_GLOBAL,
            radius: 15_000.0,
            position: [11_000.0, 0.0, 0.0],
            mass: 0.0,
            max_velocity: 0.0,
            velocity: [0.0, 0.0, 0.0],
            inertia: 1.0,
            speed_fraction: 0.0,
            extra: serde_json::json!({}),
        },
        DestinyBallSummary {
            id: 60_003_760,
            mode_byte: DST_BALL_RIGID,
            flags: BALL_FLAG_IS_GLOBAL | BALL_FLAG_IS_MASSIVE,
            radius: 25_000.0,
            position: [-1.0e11, 2.0e10, 3.0e10],
            mass: 1.0e15,
            max_velocity: 0.0,
            velocity: [0.0, 0.0, 0.0],
            inertia: 1.0,
            speed_fraction: 0.0,
            extra: serde_json::json!({}),
        },
        DestinyBallSummary {
            id: 100_000_013,
            mode_byte: DST_BALL_FORMATION,
            flags: BALL_FLAG_IS_FREE,
            radius: 20.0,
            position: [13_000.0, 0.0, 0.0],
            mass: 0.0,
            max_velocity: 300.0,
            velocity: [150.0, 0.0, 0.0],
            inertia: 1.0,
            speed_fraction: 0.5,
            extra: serde_json::json!({}),
        },
    ];
    let destiny_add_balls_fixtures = vec![DestinyAddBallsGoldenFixture {
        name: "destiny_add_balls_all_13_modes".into(),
        stamp: 42,
        wire_bytes: add_balls_wire.clone(),
        balls: summaries,
    }];

    // Marshaled DoDestinyUpdate action frames
    let destiny_actions: Vec<(&str, &str, PyRep)> = vec![
        (
            "destiny_update_add_balls",
            "AddBalls",
            PyRep::Tuple(vec![
                PyRep::Integer(42),
                PyRep::Tuple(vec![
                    PyRep::String("AddBalls".into()),
                    PyRep::Tuple(vec![PyRep::Buffer(add_balls_wire)]),
                ]),
            ]),
        ),
        (
            "destiny_update_remove_balls",
            "RemoveBalls",
            PyRep::Tuple(vec![
                PyRep::Integer(43),
                PyRep::Tuple(vec![
                    PyRep::String("RemoveBalls".into()),
                    PyRep::Tuple(vec![PyRep::List(vec![PyRep::Integer(100_000_006)])]),
                ]),
            ]),
        ),
        (
            "destiny_update_set_speed_fraction",
            "SetSpeedFraction",
            PyRep::Tuple(vec![
                PyRep::Integer(44),
                PyRep::Tuple(vec![
                    PyRep::String("SetSpeedFraction".into()),
                    PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Real(0.75)]),
                ]),
            ]),
        ),
        (
            "destiny_update_set_max_speed",
            "SetMaxSpeed",
            PyRep::Tuple(vec![
                PyRep::Integer(45),
                PyRep::Tuple(vec![
                    PyRep::String("SetMaxSpeed".into()),
                    PyRep::Tuple(vec![PyRep::Integer(100_000_001), PyRep::Real(365.0)]),
                ]),
            ]),
        ),
        (
            "destiny_update_goto_direction",
            "GotoDirection",
            PyRep::Tuple(vec![
                PyRep::Integer(46),
                PyRep::Tuple(vec![
                    PyRep::String("GotoDirection".into()),
                    PyRep::Tuple(vec![
                        PyRep::Integer(100_000_001),
                        PyRep::Real(1.0),
                        PyRep::Real(0.0),
                        PyRep::Real(0.0),
                    ]),
                ]),
            ]),
        ),
        (
            "destiny_update_follow_ball",
            "FollowBall",
            PyRep::Tuple(vec![
                PyRep::Integer(47),
                PyRep::Tuple(vec![
                    PyRep::String("FollowBall".into()),
                    PyRep::Tuple(vec![
                        PyRep::Integer(100_000_002),
                        PyRep::Integer(100_000_001),
                        PyRep::Real(2500.0),
                    ]),
                ]),
            ]),
        ),
        (
            "destiny_update_orbit",
            "Orbit",
            PyRep::Tuple(vec![
                PyRep::Integer(48),
                PyRep::Tuple(vec![
                    PyRep::String("Orbit".into()),
                    PyRep::Tuple(vec![
                        PyRep::Integer(100_000_005),
                        PyRep::Integer(100_000_001),
                        PyRep::Real(5000.0),
                    ]),
                ]),
            ]),
        ),
        (
            "destiny_update_warp_to",
            "WarpTo",
            PyRep::Tuple(vec![
                PyRep::Integer(49),
                PyRep::Tuple(vec![
                    PyRep::String("WarpTo".into()),
                    PyRep::Tuple(vec![
                        PyRep::Integer(100_000_004),
                        PyRep::Real(1.0e9),
                        PyRep::Real(0.0),
                        PyRep::Real(0.0),
                        PyRep::Integer(30),
                    ]),
                ]),
            ]),
        ),
        (
            "destiny_update_stop",
            "Stop",
            PyRep::Tuple(vec![
                PyRep::Integer(50),
                PyRep::Tuple(vec![
                    PyRep::String("Stop".into()),
                    PyRep::Tuple(vec![PyRep::Integer(100_000_001)]),
                ]),
            ]),
        ),
        (
            "destiny_update_on_special_fx",
            "OnSpecialFX",
            PyRep::Tuple(vec![
                PyRep::Integer(51),
                PyRep::Tuple(vec![
                    PyRep::String("OnSpecialFX".into()),
                    PyRep::Tuple(vec![
                        PyRep::Integer(100_000_001),
                        PyRep::Integer(100_000_002),
                        PyRep::String("effects.Laser".into()),
                    ]),
                ]),
            ]),
        ),
        (
            "destiny_update_on_damage_state_change",
            "OnDamageStateChange",
            PyRep::Tuple(vec![
                PyRep::Integer(52),
                PyRep::Tuple(vec![
                    PyRep::String("OnDamageStateChange".into()),
                    PyRep::Tuple(vec![
                        PyRep::Integer(100_000_001),
                        PyRep::Tuple(vec![
                            PyRep::Real(0.85),
                            PyRep::Real(1.0),
                            PyRep::Real(1.0),
                        ]),
                    ]),
                ]),
            ]),
        ),
        (
            "destiny_update_terminal_explosion",
            "TerminalExplosion",
            PyRep::Tuple(vec![
                PyRep::Integer(53),
                PyRep::Tuple(vec![
                    PyRep::String("TerminalExplosion".into()),
                    PyRep::Tuple(vec![PyRep::Integer(100_000_006), PyRep::Bool(false)]),
                ]),
            ]),
        ),
    ];

    let destiny_update_fixtures = destiny_actions
        .into_iter()
        .map(|(name, action, rep)| {
            let wire_bytes = marshal::encode(&rep);
            DestinyUpdateGoldenFixture {
                name: name.to_string(),
                action: action.to_string(),
                wire_bytes,
                expected_pyrep: serde_json::to_value(&rep).unwrap(),
            }
        })
        .collect();

    // Quasar Protobuf gateway envelopes across 7 packages
    let quasar_samples = vec![
        (
            "quasar_character_get_request",
            "eve_public.character",
            "eve_public.character.api.GetRequest",
            1001u64,
            200u32,
            encode_fields(&[(1, ProtoWireValue::Varint(90_000_001))]),
        ),
        (
            "quasar_corporation_get_request",
            "eve_public.corporation",
            "eve_public.corporation.api.GetRequest",
            1002u64,
            200u32,
            encode_fields(&[(1, ProtoWireValue::Varint(98_000_001))]),
        ),
        (
            "quasar_sovereignty_hub_get_resources",
            "eve_public.sovereignty.hub",
            "eve_public.sovereignty.hub.api.GetResourcesRequest",
            1003u64,
            200u32,
            encode_fields(&[
                (1, ProtoWireValue::Varint(1_040_000_000_001)),
                (2, ProtoWireValue::Varint(30_002_904)),
            ]),
        ),
        (
            "quasar_freelance_project_get_all",
            "eve_public.freelance.project",
            "eve_public.freelance.project.api.GetAllActiveRequest",
            1004u64,
            200u32,
            encode_fields(&[(1, ProtoWireValue::Varint(98_000_001))]),
        ),
        (
            "quasar_dailygoal_get_all_current",
            "eve_public.dailygoal",
            "eve_public.dailygoal.api.GetAllCurrentRequest",
            1005u64,
            200u32,
            encode_fields(&[(1, ProtoWireValue::Varint(90_000_001))]),
        ),
        (
            "quasar_cosmetic_skin_thirdparty_listing",
            "eve_public.cosmetic.ship.skin.thirdparty.listing",
            "eve_public.cosmetic.ship.skin.thirdparty.listing.api.GetAllAvailableRequest",
            1006u64,
            200u32,
            encode_fields(&[(1, ProtoWireValue::Varint(587))]),
        ),
        (
            "quasar_plex_vault_get_balance",
            "eve_public.plex.vault",
            "eve_public.plex.vault.api.GetBalanceResponse",
            1007u64,
            200u32,
            encode_fields(&[
                (1, ProtoWireValue::Varint(90_000_001)),
                (2, ProtoWireValue::Varint(500)),
            ]),
        ),
    ];

    let quasar_fixtures = quasar_samples
        .into_iter()
        .map(|(name, pkg, type_url, cid, status, payload)| {
            let env = QuasarGatewayEnvelope {
                type_url: type_url.to_string(),
                correlation_id: cid,
                status_code: status,
                payload,
            };
            let wire_bytes = encode_envelope(&env);
            QuasarGoldenFixture {
                name: name.to_string(),
                package: pkg.to_string(),
                wire_bytes,
                envelope: env,
            }
        })
        .collect();

    GoldenWireSuite {
        version: 1,
        marshal_fixtures,
        destiny_add_balls_fixtures,
        destiny_update_fixtures,
        quasar_fixtures,
    }
}

fn fixture_file_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden_wire_v24.json")
}

#[test]
fn test_t4_golden_wire_marshal_destiny_quasar_and_python_bridge_roundtrip() {
    // Task T-4: Golden wire round-trip suite across Rust (`eve-proto` / `eve-destiny`) and Python (`client-bridge/evomu_bridge.py`).
    let expected_suite = build_golden_wire_suite();
    let path = fixture_file_path();
    let raw_json = std::fs::read_to_string(&path).expect("read committed golden_wire_v24.json");
    let committed_suite: GoldenWireSuite =
        serde_json::from_str(&raw_json).expect("parse committed golden_wire_v24.json");
    assert_eq!(
        committed_suite, expected_suite,
        "Committed golden_wire_v24.json drifted from canonical wire encoder/decoder"
    );

    // 1. Verify every committed Marshal fixture decodes in Rust and round-trips when canonical
    assert!(
        committed_suite.marshal_fixtures.len() >= 38,
        "Expected at least 38 Marshal opcode fixtures"
    );
    for mf in &committed_suite.marshal_fixtures {
        let decoded = marshal::decode(&mf.wire_bytes)
            .unwrap_or_else(|e| panic!("Marshal decode failed for {}: {e}", mf.name));
        let expected_rep: PyRep =
            serde_json::from_value(mf.expected_pyrep.clone()).expect("deserialize expected PyRep");
        assert_eq!(decoded, expected_rep, "Marshal PyRep mismatch on {}", mf.name);
        if mf.canonical_roundtrip {
            assert_eq!(
                marshal::encode(&decoded),
                mf.wire_bytes,
                "Canonical marshal re-encode mismatch on {}",
                mf.name
            );
        }
    }

    // 2. Verify Destiny AddBalls binary fixture decodes all 13 DST_BALL_* modes in Rust
    for df in &committed_suite.destiny_add_balls_fixtures {
        let (stamp, balls) = decode_add_balls(&df.wire_bytes).expect("decode_add_balls");
        assert_eq!(stamp, df.stamp);
        assert_eq!(balls.len(), 13);
        let ball_refs: Vec<&Ball> = balls.iter().collect();
        assert_eq!(
            encode_add_balls(stamp, &ball_refs),
            df.wire_bytes,
            "Destiny AddBalls binary round-trip mismatch"
        );
    }

    // 3. Verify marshaled DoDestinyUpdate fixtures in Rust
    assert_eq!(committed_suite.destiny_update_fixtures.len(), 12);
    for uf in &committed_suite.destiny_update_fixtures {
        let decoded = marshal::decode(&uf.wire_bytes).expect("decode DoDestinyUpdate");
        let expected_rep: PyRep = serde_json::from_value(uf.expected_pyrep.clone()).unwrap();
        assert_eq!(decoded, expected_rep);
        assert_eq!(marshal::encode(&decoded), uf.wire_bytes);
    }

    // 4. Verify Quasar Protobuf gateway envelopes in Rust
    assert_eq!(committed_suite.quasar_fixtures.len(), 7);
    for qf in &committed_suite.quasar_fixtures {
        let decoded = decode_envelope(&qf.wire_bytes).expect("decode Quasar envelope");
        assert_eq!(decoded, qf.envelope);
        assert_eq!(encode_envelope(&decoded), qf.wire_bytes);
    }

    // 5. Cross-language round-trip: verify via client-bridge/evomu_bridge.py --verify-golden
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    let bridge_py = repo_root.join("client-bridge").join("evomu_bridge.py");
    let output = Command::new("/usr/bin/python3")
        .arg(&bridge_py)
        .arg("--verify-golden")
        .arg(&path)
        .output()
        .expect("run evomu_bridge.py --verify-golden");
    assert!(
        output.status.success(),
        "evomu_bridge.py --verify-golden failed:\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("parse python verification report");
    assert_eq!(report.get("ok").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(
        report.get("marshal_fixtures").and_then(|v| v.as_u64()),
        Some(committed_suite.marshal_fixtures.len() as u64)
    );
    assert_eq!(
        report.get("destiny_balls").and_then(|v| v.as_u64()),
        Some(13)
    );
    assert_eq!(
        report.get("destiny_updates").and_then(|v| v.as_u64()),
        Some(12)
    );
    assert_eq!(
        report.get("quasar_fixtures").and_then(|v| v.as_u64()),
        Some(7)
    );
}
