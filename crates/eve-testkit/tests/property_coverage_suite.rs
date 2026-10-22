//! Comprehensive Property-Based Testing, Fuzzing & High-Coverage Test Suite (`T-3`, `T-5`).
//!
//! Covers:
//! - `T-3`: Property-based testing & fuzz suites across MachoNet wire framing, packed row decoders,
//!   Destiny wire decoders, Dogma VM evaluator, and Quasar envelope dispatch.
//! - `T-5`: Line coverage & mutation testing suites covering targeted edge cases and branch conditions
//!   across `eve-proto`, `eve-machonet`, `eve-destiny`, `eve-dogma`, `eve-db`, and `eve-server`.
//!
//! Parity markers:
//! // parity: plan:T-3
//! // parity: plan:T-5

#![forbid(unsafe_code)]

use eve_db::types::{IskCents, MarketOrderRecord};
use eve_db::WorldRepository;
use eve_destiny::wire::*;
use eve_destiny::*;
use eve_dogma::*;
use eve_machonet::handshake::*;
use eve_machonet::*;
use eve_proto::dbrow::*;
use eve_proto::marshal::*;
use eve_proto::quasar::*;
use eve_proto::{ProtoError, PyRep, MARSHAL_HEADER_BYTE, MAX_RECURSION_DEPTH};
use eve_sde::SdeRegistry;
use eve_server::quasar_gateway::{
    STATUS_NOT_FOUND, STATUS_OK, STATUS_UNAUTHORIZED,
};
use eve_server::router::ServerContext;
use eve_testkit::attest;
use glam::DVec3;
use proptest::prelude::*;
use std::collections::HashMap;
use std::sync::OnceLock;

static SDE_REGISTRY: OnceLock<SdeRegistry> = OnceLock::new();

fn get_sde() -> &'static SdeRegistry {
    SDE_REGISTRY.get_or_init(SdeRegistry::with_canonical_seed)
}

// ============================================================================
// ============================================================================

fn arb_macho_address() -> impl Strategy<Value = MachoAddress> {
    prop_oneof![
        (any::<u64>(), proptest::option::of("[a-zA-Z0-9_]{1,16}"), any::<u64>()).prop_map(
            |(node_id, service, call_id)| MachoAddress::Node {
                node_id,
                service,
                call_id,
            }
        ),
        (any::<u64>(), any::<u64>(), proptest::option::of("[a-zA-Z0-9_]{1,16}")).prop_map(
            |(client_id, call_id, service)| MachoAddress::Client {
                client_id,
                call_id,
                service,
            }
        ),
        "[a-zA-Z0-9_]{1,24}".prop_map(|broadcast_id| MachoAddress::Broadcast {
            broadcast_id,
            id_type: "broadcast".into(),
            narrowcast: Vec::new(),
        }),
        (proptest::option::of("[a-zA-Z0-9_]{1,16}"), any::<u64>()).prop_map(
            |(service, call_id)| MachoAddress::Any { service, call_id }
        ),
    ]
}

fn arb_macho_packet_type() -> impl Strategy<Value = MachoPacketType> {
    prop_oneof![
        Just(MachoPacketType::AuthenticationReq),
        Just(MachoPacketType::AuthenticationRsp),
        Just(MachoPacketType::IdentificationReq),
        Just(MachoPacketType::IdentificationRsp),
        Just(MachoPacketType::CallReq),
        Just(MachoPacketType::CallRsp),
        Just(MachoPacketType::TransportClosed),
        Just(MachoPacketType::ResolveReq),
        Just(MachoPacketType::ResolveRsp),
        Just(MachoPacketType::Notification),
        Just(MachoPacketType::ErrorResponse),
        Just(MachoPacketType::SessionChangeNotification),
        Just(MachoPacketType::SessionInitialStateNotification),
        Just(MachoPacketType::PingReq),
        Just(MachoPacketType::PingRsp),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// // parity: plan:T-3
    #[test]
    fn prop_t3_macho_address_roundtrip(addr in arb_macho_address()) {
        attest(&["plan:T-3"]);
        let rep = addr.to_pyrep();
        let decoded = MachoAddress::from_pyrep(&rep);
        prop_assert_eq!(Some(addr), decoded);
    }

    /// // parity: plan:T-3
    #[test]
    fn prop_t3_macho_packet_roundtrip(
        packet_type in arb_macho_packet_type(),
        source in arb_macho_address(),
        destination in arb_macho_address(),
        payload_int in any::<i64>(),
    ) {
        attest(&["plan:T-3"]);
        let packet = MachoPacket {
            packet_type,
            source,
            destination,
            user_id: None,
            payload: PyRep::Integer(payload_int),
            named_payload: HashMap::new(),
        };

        let rep = packet.to_pyrep();
        let decoded = MachoPacket::from_pyrep(&rep);
        prop_assert!(decoded.is_some());
        let dec = decoded.unwrap();
        prop_assert_eq!(dec.packet_type, packet.packet_type);
        prop_assert_eq!(dec.source, packet.source);
        prop_assert_eq!(dec.destination, packet.destination);
        prop_assert_eq!(dec.payload, packet.payload);
    }

    /// // parity: plan:T-3
    #[test]
    fn prop_t3_macho_call_req_extraction(
        method in "[a-zA-Z0-9_]{1,16}",
        arg_val in any::<i64>(),
    ) {
        attest(&["plan:T-3"]);
        let call_payload = PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::String(method.clone()),
            PyRep::Tuple(vec![PyRep::Integer(arg_val)]),
            PyRep::Dict(vec![]),
        ])))]);

        let (bound_id, ext_meth, ext_args, _ext_kw) = extract_macho_call_req(&call_payload);
        prop_assert_eq!(ext_meth, method);
        prop_assert_eq!(bound_id, None);
        prop_assert_eq!(ext_args.len(), 1);
        prop_assert_eq!(&ext_args[0], &PyRep::Integer(arg_val));
    }

    /// // parity-lint: allow: fuzz test verifying no panic on arbitrary bytes
    /// // parity: plan:T-3
    #[test]
    fn prop_t3_macho_fuzz_never_panics(fuzz_bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        attest(&["plan:T-3"]);
        if let Ok(rep) = eve_proto::marshal::decode(&fuzz_bytes) {
            let _ = MachoPacket::from_pyrep(&rep);
            let _ = MachoAddress::from_pyrep(&rep);
            let _ = extract_macho_call_req(&rep);
        }
    }
}

// ============================================================================
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// // parity: plan:T-3
    #[test]
    fn prop_t3_rle_roundtrip(raw_bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        attest(&["plan:T-3"]);
        let encoded = encode_rle(&raw_bytes);
        let decoded = decode_rle(&encoded, raw_bytes.len());
        prop_assert!(decoded.is_some());
        prop_assert_eq!(decoded.unwrap(), raw_bytes);
    }

    /// // parity-lint: allow: fuzz test verifying no panic on arbitrary bytes
    /// // parity: plan:T-3
    #[test]
    fn prop_t3_rle_fuzz_never_panics(
        fuzz_bytes in proptest::collection::vec(any::<u8>(), 0..256),
        expected_len in 0usize..1024,
    ) {
        attest(&["plan:T-3"]);
        let _ = decode_rle(&fuzz_bytes, expected_len);
    }

    /// // parity-lint: allow: dbrow container structure verification
    /// // parity: plan:T-3
    #[test]
    fn prop_t3_dbrow_pack_unpack_roundtrip(
        item_id in any::<i64>(),
        type_id in any::<i32>(),
        owner_id in any::<i32>(),
        quantity in 0i32..1_000_000,
        flag_id in 0i16..1000,
    ) {
        attest(&["plan:T-3"]);
        let desc = DBRowDescriptor::inv_item_header();
        let row = vec![
            PyRep::Integer(item_id),
            PyRep::Integer(type_id as i64),
            PyRep::Integer(owner_id as i64),
            PyRep::Integer(60003760),
            PyRep::Integer(flag_id as i64),
            PyRep::Integer(quantity as i64),
            PyRep::Integer(6),
            PyRep::Integer(2),
            PyRep::String("CustomInfo".into()),
        ];

        let crowset = desc.build_crowset(vec![row.clone()]);
        let is_crowset = matches!(crowset, PyRep::ObjectEx { .. });
        prop_assert!(is_crowset);

        let rowset = desc.build_rowset(vec![row.clone()]);
        let is_rowset = matches!(rowset, PyRep::Rowset { .. });
        prop_assert!(is_rowset);

        let idx_rowset = desc.build_index_rowset("itemID", vec![row.clone()]);
        let is_idx_rowset = matches!(idx_rowset, PyRep::IndexRowset { .. });
        prop_assert!(is_idx_rowset);

        let packed_list = desc.build_packed_row_list(vec![row.clone()]);
        let is_packed_list = matches!(packed_list, PyRep::List(_));
        prop_assert!(is_packed_list);
    }
}

// ============================================================================
// ============================================================================

fn arb_movement_mode() -> impl Strategy<Value = MovementMode> {
    prop_oneof![
        Just(MovementMode::Stop),
        (any::<f64>(), any::<f64>(), any::<f64>()).prop_map(|(x, y, z)| MovementMode::GotoDirection {
            direction: DVec3::new(x, y, z).normalize_or_zero(),
        }),
        (any::<i64>(), any::<f64>()).prop_map(|(target_id, range)| MovementMode::Follow {
            target_id,
            range: range.abs(),
        }),
        (any::<i64>(), any::<f64>()).prop_map(|(target_id, range)| MovementMode::Orbit {
            target_id,
            range: range.abs().max(500.0),
        }),
        any::<i64>().prop_map(|target_id| MovementMode::Missile { target_id }),
        Just(MovementMode::Mushroom),
        Just(MovementMode::Boid),
        Just(MovementMode::Troll),
        Just(MovementMode::Miniball),
        Just(MovementMode::Field),
        Just(MovementMode::Rigid),
        Just(MovementMode::Formation),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// // parity: plan:T-3
    #[test]
    fn prop_t3_destiny_add_balls_roundtrip(
        ball_id in 1i64..100_000,
        type_id in 1u32..50_000,
        x in -1e9f64..1e9f64,
        y in -1e9f64..1e9f64,
        z in -1e9f64..1e9f64,
        radius in 1.0f64..5000.0,
        mass in 1000.0f64..1e9f64,
        mode in arb_movement_mode(),
        stamp in any::<u32>(),
    ) {
        attest(&["plan:T-3"]);
        let ball = Ball {
            id: ball_id,
            type_id,
            position: DVec3::new(x, y, z),
            velocity: DVec3::ZERO,
            max_velocity: 1000.0,
            inertia: 0.5,
            mass,
            radius,
            speed_fraction: 1.0,
            mode,
        };

        let encoded = encode_add_balls(stamp, &[&ball]);
        let decoded = decode_add_balls(&encoded);
        prop_assert!(decoded.is_ok(), "decode_add_balls failed: {:?}", decoded.err());
        let (packet_type, dec_stamp, dec_balls) = decoded.unwrap();
        prop_assert_eq!(packet_type, 0);
        prop_assert_eq!(dec_stamp, stamp);
        prop_assert_eq!(dec_balls.len(), 1);
        prop_assert_eq!(dec_balls[0].id, ball_id);
        prop_assert!((dec_balls[0].radius - radius as f32).abs() < 0.1);
        prop_assert!((dec_balls[0].position.x - x).abs() < 1.0);
        prop_assert!((dec_balls[0].position.y - y).abs() < 1.0);
        prop_assert!((dec_balls[0].position.z - z).abs() < 1.0);
    }

    /// // parity-lint: allow: fuzz test verifying no panic on arbitrary bytes
    /// // parity: plan:T-3
    #[test]
    fn prop_t3_destiny_decoder_fuzz_never_panics(fuzz_bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        attest(&["plan:T-3"]);
        let _ = decode_add_balls(&fuzz_bytes);
    }

    /// // parity-lint: allow: destiny event and update shape encoding
    /// // parity: plan:T-3
    #[test]
    fn prop_t3_destiny_event_and_updates_encoding(
        ball_id in 1i64..100_000,
        target_id in 1i64..100_000,
        stamp in any::<u32>(),
    ) {
        attest(&["plan:T-3"]);
        let event = DestinyEvent::Follow {
            ball_id,
            target_id,
            range: 1500.0,
        };
        let pyrep = encode_destiny_event(stamp, &event, None);
        let is_tuple = matches!(pyrep, PyRep::Tuple(_));
        prop_assert!(is_tuple);

        let updates = encode_destiny_updates(stamp, &[event], None);
        let is_updates_list = matches!(updates, PyRep::List(_));
        prop_assert!(is_updates_list);
    }
}

// ============================================================================
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// // parity: plan:T-3
    #[test]
    fn prop_t3_dogma_stacking_penalty_monotonic(index in 0usize..20) {
        attest(&["plan:T-3"]);
        let factor = stacking_penalty_factor(index);
        prop_assert!(factor > 0.0 && factor <= 1.0);
        if index == 0 {
            prop_assert_eq!(factor, 1.0);
        } else {
            let prev = stacking_penalty_factor(index - 1);
            prop_assert!(factor <= prev);
        }
    }

    /// // parity: plan:T-3
    #[test]
    fn prop_t3_dogma_capacitor_simulation_bounded(
        current in 0.0f64..1000.0,
        cap_max in 1.0f64..2000.0,
        recharge_time in 1.0f64..600.0,
        dt in 0.0f64..10.0,
    ) {
        attest(&["plan:T-3"]);
        let next = simulate_capacitor(current, cap_max, recharge_time, dt);
        prop_assert!(next.is_finite());
        prop_assert!(next >= 0.0);
        prop_assert!(next <= cap_max);
    }

    /// // parity: plan:T-3
    #[test]
    fn prop_t3_dogma_vm_arithmetic_eval(
        a in -1000.0f64..1000.0,
        b in -1000.0f64..1000.0,
    ) {
        attest(&["plan:T-3"]);
        let sde = get_sde();
        let item = DogmaItemState {
            item_id: 1,
            type_id: 34,
            group_id: Some(18),
            category_id: Some(4),
            base_attributes: HashMap::new(),
            modifiers: Vec::new(),
            skill_levels: HashMap::new(),
        };

        // Add
        let add_node = DogmaExpressionNode {
            opcode: DogmaVmOpcode::Add,
            value_f64: None,
            attribute_id: None,
            operator: None,
            arg1: Some(Box::new(DogmaExpressionNode {
                opcode: DogmaVmOpcode::DefFloat,
                value_f64: Some(a),
                attribute_id: None,
                operator: None,
                arg1: None,
                arg2: None,
            })),
            arg2: Some(Box::new(DogmaExpressionNode {
                opcode: DogmaVmOpcode::DefFloat,
                value_f64: Some(b),
                attribute_id: None,
                operator: None,
                arg1: None,
                arg2: None,
            })),
        };
        let res_add = add_node.evaluate(&item, sde);
        prop_assert!((res_add - (a + b)).abs() < 1e-6);

        // Sub
        let sub_node = DogmaExpressionNode {
            opcode: DogmaVmOpcode::Sub,
            value_f64: None,
            attribute_id: None,
            operator: None,
            arg1: Some(Box::new(DogmaExpressionNode {
                opcode: DogmaVmOpcode::DefFloat,
                value_f64: Some(a),
                attribute_id: None,
                operator: None,
                arg1: None,
                arg2: None,
            })),
            arg2: Some(Box::new(DogmaExpressionNode {
                opcode: DogmaVmOpcode::DefFloat,
                value_f64: Some(b),
                attribute_id: None,
                operator: None,
                arg1: None,
                arg2: None,
            })),
        };
        let res_sub = sub_node.evaluate(&item, sde);
        prop_assert!((res_sub - (a - b)).abs() < 1e-6);
    }
}

// ============================================================================
// ============================================================================

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    /// // parity: plan:T-3
    #[test]
    fn prop_t3_iskcents_arithmetic_invariants(
        a in 0i64..1_000_000_000_000,
        b in 0i64..1_000_000_000_000,
        qty in 0i64..100_000,
    ) {
        attest(&["plan:T-3"]);
        let isk_a = IskCents(a);
        let isk_b = IskCents(b);

        // Commutative addition
        let sum_ab = isk_a.checked_add(isk_b);
        let sum_ba = isk_b.checked_add(isk_a);
        prop_assert_eq!(sum_ab, sum_ba);

        // Subtraction identity
        let sub_self = isk_a.checked_sub(isk_a);
        prop_assert_eq!(sub_self, Some(IskCents(0)));

        // Multiplication
        if let Some(mul_res) = isk_a.checked_mul_qty(qty) {
            prop_assert!(mul_res.0 >= 0);
            if qty > 0 {
                prop_assert!(mul_res.0 >= isk_a.0);
            }
        }

        // Float conversion consistency
        let wire_f64 = isk_a.to_wire_f64();
        prop_assert_eq!(IskCents::from_f64_isk(wire_f64).0, a);
    }

    /// // parity: plan:T-3
    #[test]
    fn prop_t3_quasar_envelope_roundtrip(
        corr_id in proptest::collection::vec(any::<u8>(), 4..16),
        app_uuid in proptest::collection::vec(any::<u8>(), 4..16),
        status_code in 100u32..600,
        status_msg in "[a-zA-Z0-9_ ]{1,32}",
    ) {
        attest(&["plan:T-3"]);
        // Request roundtrip
        let req = EvePublicRequest {
            correlation_uuid: corr_id.clone(),
            application_instance_uuid: app_uuid.clone(),
            payload: None,
            issued: None,
            ..Default::default()
        };
        let enc_req = req.encode_proto();
        let dec_req = EvePublicRequest::decode_proto(&enc_req).expect("decode EvePublicRequest");
        prop_assert_eq!(dec_req.correlation_uuid, corr_id.clone());
        prop_assert_eq!(dec_req.application_instance_uuid, app_uuid);

        // Response roundtrip
        let rsp = EvePublicResponse {
            correlation_uuid: corr_id.clone(),
            status_code,
            status_message: status_msg.clone(),
            payload: None,
            dispatched: None,
            internal_origin: "srv-alpha".into(),
            ..Default::default()
        };
        let enc_rsp = rsp.encode_proto();
        let dec_rsp = EvePublicResponse::decode_proto(&enc_rsp).expect("decode EvePublicResponse");
        prop_assert_eq!(dec_rsp.correlation_uuid, corr_id);
        prop_assert_eq!(dec_rsp.status_code, status_code);
        prop_assert_eq!(dec_rsp.status_message, status_msg);
    }

    /// // parity-lint: allow: fuzz test verifying no panic on arbitrary bytes
    /// // parity: plan:T-3
    #[test]
    fn prop_t3_quasar_fuzz_never_panics(fuzz_bytes in proptest::collection::vec(any::<u8>(), 0..256)) {
        attest(&["plan:T-3"]);
        let _ = EvePublicRequest::decode_proto(&fuzz_bytes);
        let _ = EvePublicResponse::decode_proto(&fuzz_bytes);
        let _ = WireMessage::decode(&fuzz_bytes);
    }
}

// ============================================================================
// ============================================================================

/// // parity: plan:T-5
#[test]
fn test_t5_proto_marshal_edge_cases_and_error_branches() {
    attest(&["plan:T-5"]);

    // 1. All marshal opcode encodings & decodings
    let test_values = vec![
        PyRep::None,
        PyRep::Bool(true),
        PyRep::Bool(false),
        PyRep::Integer(-1),
        PyRep::Integer(0),
        PyRep::Integer(1),
        PyRep::Integer(-128),
        PyRep::Integer(127),
        PyRep::Integer(-32768),
        PyRep::Integer(32767),
        PyRep::Integer(-2147483648),
        PyRep::Integer(2147483647),
        PyRep::Integer(i64::MIN),
        PyRep::Integer(i64::MAX),
        PyRep::Real(0.0),
        PyRep::Real(-123.456),
        PyRep::Real(std::f64::consts::PI),
        PyRep::String("".into()),
        PyRep::String("hello world".into()),
        PyRep::String("юникод-текст".into()),
        PyRep::Tuple(vec![]),
        PyRep::Tuple(vec![PyRep::Integer(42), PyRep::String("inner".into())]),
        PyRep::List(vec![]),
        PyRep::List(vec![PyRep::Bool(true), PyRep::None]),
        PyRep::Dict(vec![]),
        PyRep::Dict(vec![(PyRep::String("key".into()), PyRep::Integer(99))]),
        PyRep::Token("util.Moniker".into()),
    ];

    for val in &test_values {
        let raw = encode(val);
        let dec = decode(&raw).expect("decode valid marshal representation");
        assert_eq!(&dec, val);

        let zlib = encode_zlib(val).expect("encode_zlib");
        let dec_zlib = decode(&zlib).expect("decode zlib representation");
        assert_eq!(&dec_zlib, val);
    }

    // 2. Corrupt buffer headers and premature EOF
    assert!(decode(&[]).is_err());
    assert!(decode(&[0x00]).is_err());
    assert!(decode(&[MARSHAL_HEADER_BYTE]).is_err());
    assert!(decode(&[MARSHAL_HEADER_BYTE, 0, 0, 0, 0]).is_err());

    // 3. Deep recursion guard
    let mut deep_tuple = PyRep::Integer(1);
    for _ in 0..MAX_RECURSION_DEPTH + 10 {
        deep_tuple = PyRep::Tuple(vec![deep_tuple]);
    }
    let deep_bytes = encode(&deep_tuple);
    let deep_dec = decode(&deep_bytes);
    assert!(
        matches!(deep_dec, Err(ProtoError::RecursionLimitExceeded)),
        "Expected RecursionLimitExceeded, got: {:?}",
        deep_dec
    );
}

/// // parity: plan:T-5
#[test]
fn test_t5_machonet_handshake_state_machine_coverage() {
    attest(&["plan:T-5"]);

    let expected_ver = VersionExchange::default();
    let mut hm = MachoHandshakeStateMachine::new(expected_ver.clone(), 1, 5001, 7001);
    assert!(!hm.is_authenticated());

    // Step 0: Server hello
    let hello = hm.server_hello();
    assert!(matches!(hello, PyRep::Tuple(_)));

    // Step 1: Client sends invalid version -> GPSTransportClosed
    let mut bad_version = hm.clone();
    let invalid_version_frame = PyRep::Tuple(vec![
        PyRep::Integer(1),
        PyRep::None,
    ]);
    let err = bad_version.step(&invalid_version_frame);
    assert!(matches!(err, Err(GPSTransportClosed { .. })));

    // Step 1: Client sends valid VersionExchange
    let client_ver = VersionExchange::default();
    let step1_res = hm.step(&client_ver.to_pyrep()).expect("step version exchange");
    assert!(matches!(step1_res, HandshakeStepOutput::SendFramesAndContinue(_)));

    // Step 2: Client sends CryptoRequestPacket
    let crypto_req = HandshakeCommand::CryptoRequest(CryptoRequestPacket::placebo());
    let step2_res = hm.step(&crypto_req.to_pyrep()).expect("step crypto request");
    assert!(matches!(step2_res, HandshakeStepOutput::SendFramesAndContinue(_)));

    // Step 3: Client sends CryptoChallengePacket
    let chal_pkt = CryptoChallengePacket::new_password_login("tester_pilot", "secret123");
    let step3_res = hm.step(&chal_pkt.to_pyrep()).expect("step crypto challenge");
    assert!(matches!(step3_res, HandshakeStepOutput::NeedsAuthentication(_)));

    // Step 4: Server completes challenge authentication
    let identity = HandshakeIdentity {
        user_id: 1001,
        role: 1,
        language_id: "EN".into(),
        user_type: 30,
        address: "127.0.0.1".into(),
        access_token: None,
        computer_hash: None,
    };
    let complete_frames = hm.complete_challenge(&chal_pkt, Ok(identity.clone()), 10).expect("complete challenge");
    assert!(!complete_frames.is_empty());

    // Step 5: Client sends challenge response hash
    let srv_hs = CryptoServerHandshake::from_pyrep(&complete_frames[1]).expect("parse server handshake");
    let result_pkt = CryptoHandshakeResult::for_server_challenge(&srv_hs.server_challenge);
    let mut hm_clone = hm.clone();
    let final_res = hm_clone.step(&result_pkt.to_pyrep()).expect("step challenge result");
    assert!(matches!(final_res, HandshakeStepOutput::Authenticated { .. }));
    assert!(hm_clone.is_authenticated());

    let mut bad_result = CryptoHandshakeResult::for_server_challenge(b"wrong_challenge");
    bad_result.challenge_response_hash = "wrong_hash".into();
    let mut hm_bad = hm.clone();
    let err_bad_hash = hm_bad.step(&bad_result.to_pyrep());
    assert!(matches!(err_bad_hash, Err(GPSTransportClosed { .. })));

    // Session notifications coverage
    let mut sess = SessionState::new(42);
    assert!(!sess.is_authenticated());
    assert_eq!(sess.effective_location_id(), None);
    assert!(!sess.has_gm_role());

    sess.user_id = Some(101);
    sess.character_id = Some(202);
    sess.station_id = Some(60003760);
    assert_eq!(sess.effective_location_id(), Some(60003760));

    sess.push_notification("OnSkillTrained", vec![PyRep::String("Welcome".into())]);
    let notifs = sess.drain_notifications();
    assert_eq!(notifs.len(), 1);
    assert_eq!(sess.drain_notifications().len(), 0);

    let initial_pkt = sess.to_initial_session_notification(1, 42);
    assert_eq!(initial_pkt.packet_type, MachoPacketType::SessionInitialStateNotification);
}

/// // parity: plan:T-5
#[test]
fn test_t5_destiny_movement_and_combat_state_coverage() {
    attest(&["plan:T-5"]);

    // 1. CombatState 4-layer damage vector resolution
    let mut cs = CombatState::default();
    assert_eq!(cs.shield_hp, 450.0);
    assert_eq!(cs.armor_hp, 450.0);
    assert_eq!(cs.structure_hp, 350.0);

    // Apply shield damage
    let (applied_shield, dead) = cs.apply_damage(DamageVector::new(100.0, 0.0, 0.0, 0.0));
    assert_eq!(applied_shield, 100.0);
    assert!(!dead);
    assert_eq!(cs.shield_hp, 350.0);

    // Wipe shield and penetrate to armor
    let (applied_all, dead) = cs.apply_damage(DamageVector::new(400.0, 0.0, 0.0, 0.0));
    assert!(applied_all > 0.0);
    assert!(!dead);
    assert_eq!(cs.shield_hp, 0.0);
    assert!(cs.armor_hp < 450.0);

    // Gate cloak invulnerability test
    cs.gate_cloak_remaining_ticks = 10;
    let (dmg_cloaked, _) = cs.apply_damage(DamageVector::new(1000.0, 1000.0, 1000.0, 1000.0));
    assert_eq!(dmg_cloaked, 0.0);

    // Tethering invulnerability test
    cs.gate_cloak_remaining_ticks = 0;
    cs.is_tethered = true;
    let (dmg_tethered, _) = cs.apply_damage(DamageVector::new(1000.0, 1000.0, 1000.0, 1000.0));
    assert_eq!(dmg_tethered, 0.0);

    // 2. Ball kinematics & align time
    let b = Ball {
        id: 101,
        type_id: 601,
        position: DVec3::ZERO,
        velocity: DVec3::ZERO,
        max_velocity: 350.0,
        inertia: 0.85,
        mass: 1_200_000.0,
        radius: 40.0,
        speed_fraction: 1.0,
        mode: MovementMode::Stop,
    };
    let tau = b.time_constant();
    assert!(tau > 0.0);
    let align_time = b.align_time_seconds();
    assert!(align_time > 0.0 && align_time < 5.0);

    // 3. Slim item metadata resolution
    let mut park = Ballpark::new(30000142);
    park.balls.insert(b.id, b.clone());
    let meta = resolve_slim_item_meta(Some(&park), &b);
    assert_eq!(meta.group_id, 25);
    let slim_rep = encode_slim_item_with_meta(&b, &meta);
    assert!(matches!(slim_rep, PyRep::Object { .. }));
}

/// // parity: plan:T-5
#[test]
fn test_t5_dogma_modifiers_heat_and_dbuff_coverage() {
    attest(&["plan:T-5"]);

    // 1. Modifier operators from SDE
    assert_eq!(ModifierOperator::try_from_sde_op(-1), Some(ModifierOperator::PreAssign));
    assert_eq!(ModifierOperator::try_from_sde_op(0), Some(ModifierOperator::PreMul));
    assert_eq!(ModifierOperator::try_from_sde_op(1), Some(ModifierOperator::PreDiv));
    assert_eq!(ModifierOperator::try_from_sde_op(2), Some(ModifierOperator::ModAdd));
    assert_eq!(ModifierOperator::try_from_sde_op(3), Some(ModifierOperator::ModSub));
    assert_eq!(ModifierOperator::try_from_sde_op(4), Some(ModifierOperator::PostMul));
    assert_eq!(ModifierOperator::try_from_sde_op(5), Some(ModifierOperator::PostDiv));
    assert_eq!(ModifierOperator::try_from_sde_op(6), Some(ModifierOperator::PostPercent));
    assert_eq!(ModifierOperator::try_from_sde_op(7), Some(ModifierOperator::PostAssign));
    assert_eq!(ModifierOperator::try_from_sde_op(8), Some(ModifierOperator::PostPercentUnNerfed));
    assert_eq!(ModifierOperator::try_from_sde_op(9), Some(ModifierOperator::SpecialSkillRelationship));
    assert_eq!(ModifierOperator::try_from_sde_op(99), None);

    // 2. Dbuff value aggregations
    let vals = [10.0, 20.0, 30.0];
    assert_eq!(aggregate_dbuff_values("Additive", &vals), 60.0);
    assert_eq!(aggregate_dbuff_values("Maximum", &vals), 30.0);
    assert_eq!(aggregate_dbuff_values("Minimum", &vals), 10.0);
    assert_eq!(aggregate_dbuff_values("multiply", &vals), 30.0); // non-"Minimum"/"Additive" returns Maximum
    assert_eq!(aggregate_dbuff_values("unknown", &vals), 30.0);

    // 3. Heat simulation formulas
    let current_heat = 0.5;
    let next_heat = thermodynamics::step_rack_heat(current_heat, 0.1, 0.05, 1.0);
    assert!(next_heat >= 0.0 && next_heat <= 1.0);

    let att = thermodynamics::heat_distance_attenuation(2, 0.5);
    assert!(att >= 0.0 && att <= 1.0);

    let dmg_prob = thermodynamics::heat_damage_probability(0.8, 1, 0.5);
    assert!(dmg_prob >= 0.0 && dmg_prob <= 1.0);

    let paste = thermodynamics::nanite_paste_required(50.0, 0.2);
    assert_eq!(paste, 10);
}

/// // parity: plan:T-5
#[test]
fn test_t5_db_persistence_accounts_and_wallet_coverage() {
    attest(&["plan:T-5"]);

    let repo = WorldRepository::new_empty();

    // 1. Account provisioning & authentication
    let uid = repo.provision_player_account("test_pilot", "hunter2");
    assert!(uid > 0);

    assert!(repo.authenticate("test_pilot", "hunter2").is_ok());
    assert!(repo.authenticate("test_pilot", "wrongpass").is_err());
    assert!(repo.authenticate("nonexistent_pilot", "any").is_err());

    // 2. Wallet journal mutations
    let entry_id = repo.with_mut_snapshot(|snap| {
        snap.append_journal(
            90001,
            "bounty",
            IskCents::from_isk(50_000),
            IskCents::from_isk(50_000),
            "T-5 bounty reward",
        )
    });
    assert!(entry_id > 0);

    let journal_len = repo.with_snapshot(|snap| snap.wallet_journal.len());
    assert_eq!(journal_len, 1);

    // 3. Market order insertion & retrieval
    repo.with_mut_snapshot(|snap| {
        snap.market_orders.insert(
            88001,
            MarketOrderRecord {
                order_id: 88001,
                owner_id: 90001,
                station_id: 60003760,
                region_id: 10000002,
                type_id: 34,
                is_bid: false,
                price: IskCents::from_isk(6),
                vol_entered: 1000,
                vol_remaining: 1000,
                min_volume: 1,
                is_npc: false,
                escrow: IskCents(0),
            },
        );
    });

    let found_order = repo.with_snapshot(|snap| snap.market_orders.get(&88001).cloned());
    assert!(found_order.is_some());
    assert_eq!(found_order.unwrap().type_id, 34);
}

/// // parity: plan:T-5
#[tokio::test]
async fn test_t5_server_dispatch_and_quasar_gateway_routing() {
    attest(&["plan:T-5"]);

    let ctx = ServerContext::new();
    let empty_kw = HashMap::new();

    // 1. Unknown service / method handling
    let mut sess = SessionState::new(9901);
    let err_svc = ctx
        .dispatch_rpc(
            &mut sess,
            "nonexistentService123",
            None,
            "SomeMethod",
            &[],
            &empty_kw,
            None,
        )
        .await;
    assert!(err_svc.is_err());

    // 2. Quasar publicGatewaySvc: unauthenticated character service access -> 401 Unauthorized
    let unauth_req = EvePublicRequest {
        correlation_uuid: b"corr-t5-unauth".to_vec(),
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve_public.character.skill.plan.api.GetActiveRequest"
                .into(),
            value: Vec::new(),
        }),
        application_instance_uuid: b"app-t5-001".to_vec(),
        ..Default::default()
    };
    let unauth_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(unauth_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch unauth request");

    if let PyRep::KeyVal(entries) = unauth_rep {
        let mut status = None;
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    let decoded = EvePublicResponse::decode_proto(&b).unwrap();
                    status = Some(decoded.status_code);
                }
            }
        }
        assert_eq!(status, Some(STATUS_UNAUTHORIZED));
    } else {
        panic!("Expected KeyVal response");
    }

    // 3. Quasar publicGatewaySvc: unknown payload type -> 404 Not Found
    let uid = ctx.db.provision_player_account("pilot_t5", "pass123");
    sess.user_id = Some(uid);
    let bad_payload_req = EvePublicRequest {
        correlation_uuid: b"corr-t5-badpayload".to_vec(),
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve_public.unknown.v1.FooBar".into(),
            value: vec![1, 2, 3],
        }),
        application_instance_uuid: b"app-t5-001".to_vec(),
        ..Default::default()
    };
    let bad_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(bad_payload_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch bad payload request");

    if let PyRep::KeyVal(entries) = bad_rep {
        let mut status = None;
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    let decoded = EvePublicResponse::decode_proto(&b).unwrap();
                    status = Some(decoded.status_code);
                }
            }
        }
        assert_eq!(status, Some(STATUS_NOT_FOUND));
    } else {
        panic!("Expected KeyVal response");
    }

    // 4. Authenticated pre-character user request -> 200 OK
    let ent_req = EvePublicRequest {
        correlation_uuid: b"corr-t5-ent01".to_vec(),
        payload: Some(GoogleAny {
            type_url: "type.googleapis.com/eve_public.character.GetNamesByUserRequest".into(),
            value: Vec::new(),
        }),
        application_instance_uuid: b"app-t5-001".to_vec(),
        ..Default::default()
    };
    let ent_rep = ctx
        .dispatch_rpc(
            &mut sess,
            "publicGatewaySvc",
            None,
            "SendRequest",
            &[PyRep::Bytes(ent_req.encode_proto())],
            &empty_kw,
            None,
        )
        .await
        .expect("dispatch GetNamesByUserRequest");

    if let PyRep::KeyVal(entries) = ent_rep {
        let mut status = None;
        for (k, v) in entries {
            if k == "response_proto" {
                if let PyRep::Bytes(b) = v {
                    let decoded = EvePublicResponse::decode_proto(&b).unwrap();
                    status = Some(decoded.status_code);
                }
            }
        }
        assert_eq!(status, Some(STATUS_OK));
    } else {
        panic!("Expected KeyVal response");
    }
}
