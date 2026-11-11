//! Unit and mutation-killing tests for `eve_machonet::handshake`.

use super::*;
use crate::{MachoAddress, MachoPacket, MachoPacketType, VersionExchange};
use eve_proto::PyRep;
use std::collections::HashMap;

#[test]
fn crc_hqx_and_placebo_crypto_hash_cover_all_length_branches() {
    assert_eq!(crc_hqx(b"", 0), 0);
    assert_eq!(crc_hqx(b"", 0xFFFF), 0xFFFF);
    assert_eq!(crc_hqx(b"123456789", 0), 0x31C3);
    assert_eq!(crc_hqx(b"123456789", 0xFFFF), 0x29B1);
    assert_eq!(crc_hqx(&[0x80], 0), 0x9188);
    assert_eq!(crc_hqx(&[0x01], 0), 0x1021);

    // Branch 0: empty challenge -> saved = [0x7E, 0, 0, 0, 0, 0x25, 0x0E]
    let expected_0 = crc_hqx(&[0x7E, 0, 0, 0, 0, 0x25, 0x0E], 0).to_string();
    assert_eq!(placebo_crypto_hash(b""), expected_0);

    // Branch 1: 1-byte challenge -> saved = [0x7E, 0, 0, 0, 0, 0x25, 0x0F, b'Z']
    let expected_1 = crc_hqx(&[0x7E, 0, 0, 0, 0, 0x25, 0x0F, b'Z'], 0).to_string();
    assert_eq!(placebo_crypto_hash(b"Z"), expected_1);
    assert_ne!(placebo_crypto_hash(b"Z"), placebo_crypto_hash(b"Y"));

    // Branch len < 0xFF (boundary at 254 = 0xFE)
    let chal_254 = vec![0xAB; 254];
    let mut saved_254 = vec![0x7E, 0, 0, 0, 0, 0x25, 0x13, 254];
    saved_254.extend_from_slice(&chal_254);
    assert_eq!(
        placebo_crypto_hash(&chal_254),
        crc_hqx(&saved_254, 0).to_string()
    );

    // Branch len >= 0xFF (boundary at 255 = 0xFF)
    let chal_255 = vec![0xCD; 255];
    let mut saved_255 = vec![0x7E, 0, 0, 0, 0, 0x25, 0x13, 0xFF];
    saved_255.extend_from_slice(&255u32.to_le_bytes());
    saved_255.extend_from_slice(&chal_255);
    assert_eq!(
        placebo_crypto_hash(&chal_255),
        crc_hqx(&saved_255, 0).to_string()
    );
}

#[test]
fn gps_transport_closed_roundtrip_and_objectex_variants() {
    let closed = GPSTransportClosed::new("Incompatible region", "HANDSHAKE_INCOMPATIBLEREGION")
        .with_arg("region", PyRep::String("optic".into()));
    let rep = closed.to_pyrep();
    let decoded = GPSTransportClosed::from_pyrep(&rep).expect("should parse GPSTransportClosed");
    assert_eq!(decoded, closed);

    // None reason_code
    let mut no_code = closed.clone();
    no_code.reason_code = None;
    let rep_no_code = no_code.to_pyrep();
    let decoded_no_code = GPSTransportClosed::from_pyrep(&rep_no_code).unwrap();
    assert_eq!(decoded_no_code.reason_code, None);

    // GPSRemoteTransportClosed via ObjectEx with nested tuple and flat tuple
    let obj_ex_nested = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("exceptions.GPSRemoteTransportClosed".into()),
            PyRep::Tuple(vec![
                PyRep::String("Remote close".into()),
                PyRep::String("REMOTE_CODE".into()),
                PyRep::Dict(vec![(PyRep::String("k".into()), PyRep::Integer(9))]),
            ]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    let parsed_nested = GPSTransportClosed::from_pyrep(&obj_ex_nested).unwrap();
    assert_eq!(parsed_nested.reason, "Remote close");
    assert_eq!(parsed_nested.reason_code.as_deref(), Some("REMOTE_CODE"));
    assert_eq!(parsed_nested.reason_args.get("k"), Some(&PyRep::Integer(9)));

    let obj_ex_flat = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("exceptions.GPSTransportClosed".into()),
            PyRep::String("Flat reason".into()),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    let parsed_flat = GPSTransportClosed::from_pyrep(&obj_ex_flat).unwrap();
    assert_eq!(
        parsed_flat.reason,
        "exceptions.GPSTransportClosed"
    );
    assert_eq!(parsed_flat.reason_code.as_deref(), Some("Flat reason"));

    // Invalid inputs
    assert_eq!(GPSTransportClosed::from_pyrep(&PyRep::None), None);
    assert_eq!(
        GPSTransportClosed::from_pyrep(&PyRep::Object {
            type_name: "exceptions.OtherError".into(),
            args: Box::new(PyRep::Tuple(vec![PyRep::String("x".into())])),
        }),
        None
    );
    assert_eq!(
        GPSTransportClosed::from_pyrep(&PyRep::Object {
            type_name: "exceptions.GPSTransportClosed".into(),
            args: Box::new(PyRep::None),
        }),
        None
    );
    assert_eq!(
        GPSTransportClosed::from_pyrep(&PyRep::ObjectEx {
            is_type_2: false,
            header: Box::new(PyRep::Tuple(vec![PyRep::Token("only_one".into())])),
            list: Vec::new(),
            dict: Vec::new(),
        }),
        None
    );
    assert_eq!(
        GPSTransportClosed::from_pyrep(&PyRep::ObjectEx {
            is_type_2: false,
            header: Box::new(PyRep::None),
            list: Vec::new(),
            dict: Vec::new(),
        }),
        None
    );
}

#[test]
fn crypto_request_and_handshake_command_parsing() {
    let mut req = CryptoRequestPacket::placebo();
    req.key_params
        .insert("padding".into(), PyRep::Integer(64));
    let rep = req.to_pyrep();
    assert_eq!(CryptoRequestPacket::from_pyrep(&rep), Some(req.clone()));

    // None key_params allowed
    let none_params = PyRep::Tuple(vec![PyRep::String("placebo".into()), PyRep::None]);
    let parsed = CryptoRequestPacket::from_pyrep(&none_params).unwrap();
    assert_eq!(parsed.key_version, "placebo");
    assert!(parsed.key_params.is_empty());

    // Invalid shapes
    assert_eq!(CryptoRequestPacket::from_pyrep(&PyRep::None), None);
    assert_eq!(
        CryptoRequestPacket::from_pyrep(&PyRep::Tuple(vec![PyRep::String("placebo".into())])),
        None
    );
    assert_eq!(
        CryptoRequestPacket::from_pyrep(&PyRep::Tuple(vec![
            PyRep::String("placebo".into()),
            PyRep::Integer(1)
        ])),
        None
    );

    // HandshakeCommand variants
    let qc = HandshakeCommand::QueueCheck;
    assert_eq!(HandshakeCommand::from_pyrep(&qc.to_pyrep()), Some(qc));

    let vk = HandshakeCommand::VipKey("vip-token".into());
    assert_eq!(HandshakeCommand::from_pyrep(&vk.to_pyrep()), Some(vk));

    let vk_non_str = PyRep::Tuple(vec![
        PyRep::None,
        PyRep::ascii_bytes("VK"),
        PyRep::Integer(123),
    ]);
    assert_eq!(
        HandshakeCommand::from_pyrep(&vk_non_str),
        Some(HandshakeCommand::VipKey(String::new()))
    );

    let cr = HandshakeCommand::CryptoRequest(req);
    assert_eq!(HandshakeCommand::from_pyrep(&cr.to_pyrep()), Some(cr));

    assert_eq!(HandshakeCommand::from_pyrep(&PyRep::None), None);
    assert_eq!(
        HandshakeCommand::from_pyrep(&PyRep::Tuple(vec![
            PyRep::None,
            PyRep::ascii_bytes("UNKNOWN")
        ])),
        None
    );
}

#[test]
fn crypto_challenge_packet_roundtrip_password_extraction_and_validation() {
    let mut pw_login = CryptoChallengePacket::new_password_login("pilot1", "pass123");
    pw_login.user_password_hash = Some(vec![0x11, 0x22]);
    let pw_rep = pw_login.to_pyrep();
    assert_eq!(
        CryptoChallengePacket::from_pyrep(&pw_rep),
        Some(pw_login.clone())
    );

    let sso_login = CryptoChallengePacket::new_sso_login("jwt-sso-token");
    let sso_rep = sso_login.to_pyrep();
    assert_eq!(
        CryptoChallengePacket::from_pyrep(&sso_rep),
        Some(sso_login.clone())
    );

    // Alternative representations in from_pyrep (String/Buffer challenge, Integer boot_version, ObjectEx password)
    let alt_dict = PyRep::Dict(vec![
        (PyRep::String("macho_version".into()), PyRep::Integer(496)),
        (PyRep::String("boot_version".into()), PyRep::Integer(24)),
        (PyRep::String("boot_build".into()), PyRep::Integer(3_624_425)),
        (PyRep::String("boot_codename".into()), PyRep::String("V24.01".into())),
        (PyRep::String("boot_region".into()), PyRep::String("ccp".into())),
        (
            PyRep::String("user_password".into()),
            PyRep::ObjectEx {
                is_type_2: false,
                header: Box::new(PyRep::Tuple(vec![
                    PyRep::Token("util.PasswordString".into()),
                    PyRep::Tuple(vec![PyRep::Bytes(b"raw_pw".to_vec())]),
                ])),
                list: Vec::new(),
                dict: Vec::new(),
            },
        ),
        (
            PyRep::String("user_password_hash".into()),
            PyRep::String("hash_str".into()),
        ),
    ]);
    let alt_rep = PyRep::Tuple(vec![PyRep::String("chal".into()), alt_dict]);
    let alt_parsed = CryptoChallengePacket::from_pyrep(&alt_rep).unwrap();
    assert_eq!(alt_parsed.client_challenge, b"chal");
    assert_eq!(alt_parsed.boot_version, 24.0);
    assert_eq!(alt_parsed.user_password.as_deref(), Some("raw_pw"));
    assert_eq!(alt_parsed.user_password_hash.as_deref(), Some(b"hash_str".as_slice()));
    assert_eq!(alt_parsed.user_language_id, "EN");
    assert_eq!(alt_parsed.user_affiliate_id, 0);

    // Direct extract_password_str coverage
    assert_eq!(extract_password_str(&PyRep::None), None);
    assert_eq!(
        extract_password_str(&PyRep::Buffer(b"buf_pw".to_vec())).as_deref(),
        Some("buf_pw")
    );
    assert_eq!(
        extract_password_str(&PyRep::Object {
            type_name: "util.PasswordString".into(),
            args: Box::new(PyRep::String("direct_arg".into())),
        })
        .as_deref(),
        Some("direct_arg")
    );
    assert_eq!(
        extract_password_str(&PyRep::ObjectEx {
            is_type_2: false,
            header: Box::new(PyRep::Tuple(vec![PyRep::Token("short".into())])),
            list: Vec::new(),
            dict: Vec::new(),
        }),
        None
    );
    assert_eq!(extract_password_str(&PyRep::Integer(99)), None);

    // Version validation branches & boundaries
    let expected = VersionExchange::default();
    assert!(pw_login.validate_version(&expected).is_ok());

    let mut newer_build = pw_login.clone();
    newer_build.boot_build = expected.build_version + 1;
    assert!(newer_build.validate_version(&expected).is_ok());

    let mut within_eps = pw_login.clone();
    within_eps.boot_version = expected.version_number + 5e-5;
    assert!(within_eps.validate_version(&expected).is_ok());

    let mut bad_region = pw_login.clone();
    bad_region.boot_region = "serenity".into();
    assert_eq!(
        bad_region.validate_version(&expected).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEREGION")
    );

    let mut bad_code = pw_login.clone();
    bad_code.boot_codename = "V23.02".into();
    assert_eq!(
        bad_code.validate_version(&expected).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLERELEASE")
    );

    let mut bad_ver = pw_login.clone();
    bad_ver.boot_version = expected.version_number + 2e-4;
    assert_eq!(
        bad_ver.validate_version(&expected).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEVERSION")
    );

    let mut bad_macho = pw_login.clone();
    bad_macho.macho_version = expected.macho_version + 1;
    assert_eq!(
        bad_macho.validate_version(&expected).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEPROTOCOL")
    );

    let mut bad_build = pw_login.clone();
    bad_build.boot_build = expected.build_version - 1;
    assert_eq!(
        bad_build.validate_version(&expected).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEBUILD")
    );
}

#[test]
fn server_handshake_result_and_ack_roundtrips() {
    let expected = VersionExchange::default();
    let mut sh = CryptoServerHandshake::for_client_challenge(b"challenge", &expected, 888_444, 7);
    sh.verification = true;
    sh.context.insert("ctx_k".into(), PyRep::Integer(1));
    sh.config_vals.insert("cfg_k".into(), PyRep::String("v".into()));
    let sh_rep = sh.to_pyrep();
    assert_eq!(CryptoServerHandshake::from_pyrep(&sh_rep), Some(sh));
    assert_eq!(CryptoServerHandshake::from_pyrep(&PyRep::None), None);
    assert_eq!(
        CryptoServerHandshake::from_pyrep(&PyRep::Tuple(vec![PyRep::None; 3])),
        None
    );

    let res = CryptoHandshakeResult::for_server_challenge(b"srv_chal");
    let res_rep = res.to_pyrep();
    assert_eq!(CryptoHandshakeResult::from_pyrep(&res_rep), Some(res));
    assert_eq!(CryptoHandshakeResult::from_pyrep(&PyRep::None), None);
    assert_eq!(
        CryptoHandshakeResult::from_pyrep(&PyRep::Tuple(vec![PyRep::None; 2])),
        None
    );

    let id = HandshakeIdentity {
        user_id: 1001,
        role: 0x6000_0000_0000_0000,
        language_id: "DE".into(),
        user_type: 30,
        address: "127.0.0.1:55000".into(),
        access_token: Some("sso-tok".into()),
        computer_hash: None,
    };
    let mut ack = CryptoHandshakeAck::from_identity(99, 12345, &id);
    ack.max_session_time = Some(7200);
    ack.in_detention = Some(PyRep::Integer(0));
    ack.client_hash = Some("hash1".into());
    ack.live_updates = vec![PyRep::String("upd".into())];
    let ack_rep = ack.to_pyrep();
    let ack_decoded = CryptoHandshakeAck::from_pyrep(&ack_rep).unwrap();
    assert_eq!(ack_decoded.session_id, 99);
    assert_eq!(ack_decoded.client_id, 12345);
    assert_eq!(ack_decoded.user_id, 1001);
    assert_eq!(ack_decoded.max_session_time, Some(7200));
    assert_eq!(ack_decoded.access_token.as_deref(), Some("sso-tok"));
    // When access_token is Some and computer_hash is None, to_pyrep emits empty string computer_hash
    assert_eq!(ack_decoded.computer_hash.as_deref(), Some(""));
    assert_eq!(CryptoHandshakeAck::from_pyrep(&PyRep::None), None);
}

#[test]
fn handshake_state_machine_full_lifecycle_and_error_branches() {
    let ver = VersionExchange::default();
    let mut sm = MachoHandshakeStateMachine::new(ver.clone(), MACHO_PROXY_NODE_ID, 501, 9001);
    let mut cfg = HashMap::new();
    cfg.insert("machoNet.serviceInfo".into(), PyRep::None);
    sm.set_config_vals(cfg.clone());
    assert!(!sm.is_authenticated());
    assert_eq!(sm.server_hello(), ver.to_server_hello_pyrep());

    // Premature MachoPacket is rejected in any unauthenticated phase
    let ping_pkt = MachoPacket {
        packet_type: MachoPacketType::PingReq,
        source: MachoAddress::Client {
            client_id: 9001,
            call_id: 1,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: MACHO_PROXY_NODE_ID,
            service: None,
            call_id: 0,
        },
        user_id: None,
        payload: PyRep::Tuple(vec![]),
        named_payload: HashMap::new(),
    };
    let mut sm_reject = sm.clone();
    let err = sm_reject.step(&ping_pkt.to_pyrep()).unwrap_err();
    assert_eq!(err.reason_code.as_deref(), Some("HANDSHAKE_REQUIRED"));
    assert_eq!(
        sm_reject.step(&ver.to_pyrep()).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_FAILED")
    );

    // Step 1: VersionExchange -> QueueCheck -> VersionExchange -> VipKey -> CryptoRequest
    let out1 = sm.step(&ver.to_pyrep()).unwrap();
    assert!(matches!(out1, HandshakeStepOutput::SendFramesAndContinue(f) if f.is_empty()));
    assert_eq!(sm.phase, HandshakePhase::AwaitingCommandOrCrypto);

    let qc_out = sm.step(&HandshakeCommand::QueueCheck.to_pyrep()).unwrap();
    assert!(matches!(qc_out, HandshakeStepOutput::SendFramesAndContinue(f) if f.len() == 2));
    assert_eq!(sm.phase, HandshakePhase::AwaitingVersionExchange);

    sm.step(&ver.to_pyrep()).unwrap();
    let vk_out = sm
        .step(&HandshakeCommand::VipKey("my-vip".into()).to_pyrep())
        .unwrap();
    assert!(matches!(vk_out, HandshakeStepOutput::SendFramesAndContinue(f) if f.is_empty()));
    assert_eq!(sm.vip_key.as_deref(), Some("my-vip"));

    // Reject non-placebo or crypting_sessionkey
    let mut sm_bad_crypto = sm.clone();
    let bad_req = CryptoRequestPacket {
        key_version: "rsa".into(),
        key_params: HashMap::new(),
    };
    assert_eq!(
        sm_bad_crypto
            .step(&HandshakeCommand::CryptoRequest(bad_req).to_pyrep())
            .unwrap_err()
            .reason_code
            .as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEPUBLICKEY")
    );

    let mut sm_cryptoapi = sm.clone();
    let mut capi_req = CryptoRequestPacket::placebo();
    capi_req
        .key_params
        .insert("crypting_sessionkey".into(), PyRep::Bytes(vec![1, 2]));
    assert_eq!(
        sm_cryptoapi
            .step(&HandshakeCommand::CryptoRequest(capi_req).to_pyrep())
            .unwrap_err()
            .reason_code
            .as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEPUBLICKEY")
    );

    // Valid Placebo CryptoRequest
    let cr_out = sm
        .step(&HandshakeCommand::CryptoRequest(CryptoRequestPacket::placebo()).to_pyrep())
        .unwrap();
    assert!(
        matches!(cr_out, HandshakeStepOutput::SendFramesAndContinue(f) if f == vec![PyRep::ascii_bytes("OK CC")])
    );
    assert_eq!(sm.phase, HandshakePhase::AwaitingCryptoChallenge);

    // Password Attempt 1 (user_password = None) -> server replies Integer(1)
    let mut attempt1 = CryptoChallengePacket::new_password_login("bob", "secret");
    attempt1.user_password = None;
    let a1_out = sm.step(&attempt1.to_pyrep()).unwrap();
    assert!(
        matches!(a1_out, HandshakeStepOutput::SendFramesAndContinue(f) if f == vec![PyRep::Integer(1)])
    );

    // Password Attempt 2 -> NeedsAuthentication -> complete_challenge returns 1 frame (no Integer(2))
    let attempt2 = CryptoChallengePacket::new_password_login("bob", "secret");
    let a2_out = sm.step(&attempt2.to_pyrep()).unwrap();
    let HandshakeStepOutput::NeedsAuthentication(chal) = a2_out else {
        panic!("expected NeedsAuthentication");
    };
    let identity = HandshakeIdentity {
        user_id: 42,
        role: crate::service_roles::ROLE_PLAYER_DEFAULT,
        language_id: "EN".into(),
        user_type: 30,
        address: "127.0.0.1:40000".into(),
        access_token: None,
        computer_hash: None,
    };
    let hs_frames = sm
        .complete_challenge(&chal, Ok(identity.clone()), 5)
        .unwrap();
    assert_eq!(hs_frames.len(), 1);
    assert_eq!(sm.phase, HandshakePhase::AwaitingHandshakeResult);

    // Wrong challenge hash fails
    let mut sm_bad_hash = sm.clone();
    let bad_res = CryptoHandshakeResult {
        challenge_response_hash: "0".into(),
        func_output: Vec::new(),
        func_result: PyRep::None,
    };
    assert_eq!(
        sm_bad_hash
            .step(&bad_res.to_pyrep())
            .unwrap_err()
            .reason_code
            .as_deref(),
        Some("HANDSHAKE_FAILEDVERIFICATION")
    );

    // Valid challenge result -> Authenticated
    let good_res =
        CryptoHandshakeResult::for_server_challenge(&vec![0u8; DEFAULT_HANDSHAKE_PADDING_LEN]);
    let final_out = sm.step(&good_res.to_pyrep()).unwrap();
    let HandshakeStepOutput::Authenticated {
        frames,
        identity: out_id,
    } = final_out
    else {
        panic!("expected Authenticated");
    };
    assert_eq!(frames.len(), 3);
    assert_eq!(out_id, identity);
    assert!(sm.is_authenticated());
    assert_eq!(
        sm.step(&good_res.to_pyrep())
            .unwrap_err()
            .reason_code
            .as_deref(),
        Some("HANDSHAKE_ALREADY_COMPLETE")
    );

    // Direct SSO login flow (no passwordVersion=1 retry; complete_challenge returns [Integer(2), server_hs])
    let mut sm_sso = MachoHandshakeStateMachine::new(ver.clone(), MACHO_PROXY_NODE_ID, 502, 9002);
    assert!(sm_sso.step(&PyRep::None).is_err());
    let mut sm_sso = MachoHandshakeStateMachine::new(ver.clone(), MACHO_PROXY_NODE_ID, 502, 9002);
    let mut bad_ver = ver.clone();
    bad_ver.birthday += 1;
    assert!(sm_sso.step(&bad_ver.to_pyrep()).is_err());

    let mut sm_sso = MachoHandshakeStateMachine::new(ver.clone(), MACHO_PROXY_NODE_ID, 502, 9002);
    sm_sso.step(&ver.to_pyrep()).unwrap();
    let mut sm_bad_cmd = sm_sso.clone();
    assert!(sm_bad_cmd.step(&PyRep::None).is_err());

    sm_sso
        .step(&HandshakeCommand::CryptoRequest(CryptoRequestPacket::placebo()).to_pyrep())
        .unwrap();
    let mut sm_bad_chal = sm_sso.clone();
    assert!(sm_bad_chal.step(&PyRep::None).is_err());
    let mut sm_bad_chal_ver = sm_sso.clone();
    let mut bad_chal = CryptoChallengePacket::new_sso_login("tok");
    bad_chal.macho_version += 1;
    assert!(sm_bad_chal_ver.step(&bad_chal.to_pyrep()).is_err());

    let sso_chal = CryptoChallengePacket::new_sso_login("sso-jwt-token");
    let HandshakeStepOutput::NeedsAuthentication(parsed_sso) =
        sm_sso.step(&sso_chal.to_pyrep()).unwrap()
    else {
        panic!("expected NeedsAuthentication for SSO");
    };
    let mut sm_auth_err = sm_sso.clone();
    assert!(sm_auth_err
        .complete_challenge(
            &parsed_sso,
            Err(GPSTransportClosed::new("Denied", "AUTH_FAILED")),
            1,
        )
        .is_err());

    let sso_frames = sm_sso
        .complete_challenge(&parsed_sso, Ok(identity), 3)
        .unwrap();
    assert_eq!(sso_frames.len(), 2);
    assert_eq!(sso_frames[0], PyRep::Integer(2));

    let mut sm_bad_res_frame = sm_sso.clone();
    assert!(sm_bad_res_frame.step(&PyRep::None).is_err());
}

#[test]
fn gps_transport_closed_and_crypto_request_edge_cases() {
    // Non-string keys inside reason_args dict are ignored gracefully
    let closed_non_str_arg = PyRep::Object {
        type_name: "exceptions.GPSRemoteTransportClosed".into(),
        args: Box::new(PyRep::Tuple(vec![
            PyRep::String("Remote shutdown".into()),
            PyRep::None,
            PyRep::Dict(vec![
                (PyRep::Integer(42), PyRep::String("ignored".into())),
                (PyRep::String("kept".into()), PyRep::Integer(7)),
            ]),
        ])),
    };
    let parsed = GPSTransportClosed::from_pyrep(&closed_non_str_arg);
    assert!(parsed.is_some());
    if let Some(c) = parsed {
        assert_eq!(c.reason, "Remote shutdown");
        assert_eq!(c.reason_code, None);
        assert_eq!(c.reason_args.len(), 1);
        assert_eq!(c.reason_args.get("kept"), Some(&PyRep::Integer(7)));
    }

    // ObjectEx with non-string class name or non-string reason -> None
    let objex_bad_class = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![PyRep::Integer(1), PyRep::String("x".into())])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    assert_eq!(GPSTransportClosed::from_pyrep(&objex_bad_class), None);

    let objex_non_str_reason = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("exceptions.GPSTransportClosed".into()),
            PyRep::Tuple(vec![PyRep::Integer(123)]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    assert_eq!(GPSTransportClosed::from_pyrep(&objex_non_str_reason), None);

    // CryptoRequestPacket: non-string key_version -> None, non-string dict keys ignored
    assert_eq!(
        CryptoRequestPacket::from_pyrep(&PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Dict(Vec::new()),
        ])),
        None
    );
    let cr_mixed_keys = PyRep::Tuple(vec![
        PyRep::String("placebo".into()),
        PyRep::Dict(vec![
            (PyRep::Integer(99), PyRep::Integer(1)),
            (PyRep::String("ok".into()), PyRep::Bool(true)),
        ]),
    ]);
    let cr_parsed = CryptoRequestPacket::from_pyrep(&cr_mixed_keys);
    assert!(cr_parsed.is_some());
    if let Some(cr) = cr_parsed {
        assert_eq!(cr.key_params.len(), 1);
        assert_eq!(cr.key_params.get("ok"), Some(&PyRep::Bool(true)));
    }

    // HandshakeCommand boundary shapes
    assert_eq!(
        HandshakeCommand::from_pyrep(&PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::String("QC".into()),
        ])),
        None
    );
    assert_eq!(
        HandshakeCommand::from_pyrep(&PyRep::Tuple(vec![
            PyRep::None,
            PyRep::String("VK".into()),
        ])),
        None
    );
    assert_eq!(
        HandshakeCommand::from_pyrep(&PyRep::Tuple(vec![
            PyRep::None,
            PyRep::String("QC".into()),
            PyRep::String("extra".into()),
        ])),
        None
    );
}

#[test]
fn crypto_challenge_server_hs_and_ack_fallback_branches() {
    // CryptoChallengePacket malformed shapes and missing keys
    assert_eq!(CryptoChallengePacket::from_pyrep(&PyRep::None), None);
    assert_eq!(
        CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![PyRep::Bytes(vec![1])])),
        None
    );
    assert_eq!(
        CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![
            PyRep::Integer(7),
            PyRep::Dict(Vec::new()),
        ])),
        None
    );
    assert_eq!(
        CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![
            PyRep::Buffer(vec![1, 2]),
            PyRep::Integer(7),
        ])),
        None
    );
    // Missing boot_version or non-numeric boot_version -> None
    assert_eq!(
        CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![
            PyRep::Buffer(vec![1, 2]),
            PyRep::Dict(vec![
                (PyRep::String("macho_version".into()), PyRep::Integer(496)),
                (PyRep::String("boot_version".into()), PyRep::String("bad".into())),
            ]),
        ])),
        None
    );

    // extract_password_str variants: invalid UTF-8 bytes, Tuple wrapper, Object with non-tuple
    assert_eq!(
        extract_password_str(&PyRep::Bytes(vec![0xFF, 0xFE])),
        None
    );
    assert_eq!(
        extract_password_str(&PyRep::Tuple(vec![PyRep::String("tup_pw".into())])).as_deref(),
        Some("tup_pw")
    );
    assert_eq!(
        extract_password_str(&PyRep::Object {
            type_name: "util.PasswordString".into(),
            args: Box::new(PyRep::None),
        }),
        None
    );

    // validate_version boundary: boot_version below expected by > 1e-4, exact build match
    let expected = VersionExchange::default();
    let mut ver_low = CryptoChallengePacket::new_password_login("u", "p");
    ver_low.boot_version = expected.version_number - 2e-4;
    assert_eq!(
        ver_low.validate_version(&expected).err().and_then(|e| e.reason_code),
        Some("HANDSHAKE_INCOMPATIBLEVERSION".into())
    );
    let mut build_exact = CryptoChallengePacket::new_password_login("u", "p");
    build_exact.boot_build = expected.build_version;
    assert!(build_exact.validate_version(&expected).is_ok());

    // CryptoServerHandshake::from_pyrep edge cases (Buffer/String, integer verification & boot_version, omitted defaults)
    let sh_minimal = PyRep::Tuple(vec![
        PyRep::Buffer(b"srv".to_vec()),
        PyRep::Tuple(vec![PyRep::String("code".into()), PyRep::Integer(1)]),
        PyRep::None,
        PyRep::Dict(vec![
            (PyRep::String("challenge_responsehash".into()), PyRep::String("h".into())),
            (PyRep::String("macho_version".into()), PyRep::Integer(496)),
            (PyRep::String("boot_version".into()), PyRep::Integer(24)),
            (PyRep::String("boot_build".into()), PyRep::Integer(3624425)),
            (PyRep::String("boot_codename".into()), PyRep::String("V24.01".into())),
            (PyRep::String("boot_region".into()), PyRep::String("ccp".into())),
        ]),
    ]);
    let parsed_sh = CryptoServerHandshake::from_pyrep(&sh_minimal);
    assert!(parsed_sh.is_some());
    if let Some(sh) = parsed_sh {
        assert_eq!(sh.server_challenge, b"srv");
        assert_eq!(sh.func_marshaled_code, b"code");
        assert!(sh.verification);
        assert_eq!(sh.boot_version, 24.0);
        assert_eq!(sh.cluster_user_count, 1);
        assert_eq!(sh.proxy_node_id, 888444);
        assert_eq!(sh.user_logon_queue_position, 1);
    }

    // Invalid CryptoServerHandshake shapes
    assert_eq!(
        CryptoServerHandshake::from_pyrep(&PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::Tuple(vec![PyRep::Bytes(vec![]), PyRep::Bool(false)]),
            PyRep::Dict(vec![]),
            PyRep::Dict(vec![]),
        ])),
        None
    );
    assert_eq!(
        CryptoServerHandshake::from_pyrep(&PyRep::Tuple(vec![
            PyRep::String("s".into()),
            PyRep::None,
            PyRep::Dict(vec![]),
            PyRep::Dict(vec![]),
        ])),
        None
    );
    assert_eq!(
        CryptoServerHandshake::from_pyrep(&PyRep::Tuple(vec![
            PyRep::String("s".into()),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::None]),
            PyRep::Dict(vec![]),
            PyRep::Dict(vec![]),
        ])),
        None
    );
    assert_eq!(
        CryptoServerHandshake::from_pyrep(&PyRep::Tuple(vec![
            PyRep::String("s".into()),
            PyRep::Tuple(vec![PyRep::Bytes(vec![]), PyRep::None]),
            PyRep::Dict(vec![]),
            PyRep::None,
        ])),
        None
    );

    // CryptoHandshakeResult Buffer & fallback branches
    let res_buf = PyRep::Tuple(vec![
        PyRep::String("999".into()),
        PyRep::Buffer(b"out".to_vec()),
        PyRep::None,
    ]);
    let parsed_res = CryptoHandshakeResult::from_pyrep(&res_buf);
    assert!(parsed_res.is_some());
    if let Some(r) = parsed_res {
        assert_eq!(r.func_output, b"out");
    }
    let res_fallback = PyRep::Tuple(vec![
        PyRep::String("999".into()),
        PyRep::Integer(123),
        PyRep::Bool(true),
    ]);
    let parsed_res2 = CryptoHandshakeResult::from_pyrep(&res_fallback);
    assert!(parsed_res2.is_some());
    if let Some(r) = parsed_res2 {
        assert!(r.func_output.is_empty());
        assert_eq!(r.func_result, PyRep::Bool(true));
    }

    // CryptoHandshakeAck: computer_hash without access_token, and omitted optional defaults
    let ack_hw_only = CryptoHandshakeAck {
        session_id: 1,
        user_id: 2,
        role: 3,
        language_id: "EN".into(),
        user_type: 30,
        max_session_time: None,
        address: "127.0.0.1:1".into(),
        in_detention: None,
        client_id: 4,
        client_hash: None,
        live_updates: Vec::new(),
        access_token: None,
        computer_hash: Some("hw-hash".into()),
    };
    let ack_rep = ack_hw_only.to_pyrep();
    assert_eq!(ack_rep.dict_get("access_token"), None);
    assert_eq!(
        ack_rep.dict_get("computer_hash").and_then(|v| v.as_str()),
        Some("hw-hash")
    );
    let ack_dec = CryptoHandshakeAck::from_pyrep(&ack_rep);
    assert_eq!(ack_dec, Some(ack_hw_only));

    // Omitted optional keys in session_init default cleanly
    let ack_minimal = PyRep::Dict(vec![
        (PyRep::String("sessionID".into()), PyRep::Integer(10)),
        (PyRep::String("user_clientid".into()), PyRep::Integer(20)),
        (
            PyRep::String("session_init".into()),
            PyRep::Dict(vec![
                (PyRep::String("userid".into()), PyRep::Integer(30)),
                (PyRep::String("role".into()), PyRep::Integer(40)),
                (PyRep::String("inDetention".into()), PyRep::None),
            ]),
        ),
    ]);
    let parsed_min = CryptoHandshakeAck::from_pyrep(&ack_minimal);
    assert!(parsed_min.is_some());
    if let Some(a) = parsed_min {
        assert_eq!(a.language_id, "EN");
        assert_eq!(a.user_type, 30);
        assert_eq!(a.address, "");
        assert_eq!(a.in_detention, None);
        assert!(a.live_updates.is_empty());
    }
    assert_eq!(
        CryptoHandshakeAck::from_pyrep(&PyRep::Dict(vec![
            (PyRep::String("sessionID".into()), PyRep::Integer(1)),
            (PyRep::String("user_clientid".into()), PyRep::Integer(2)),
            (PyRep::String("session_init".into()), PyRep::None),
        ])),
        None
    );
}

#[test]
fn handshake_state_machine_retry_bounds_and_missing_identity_error() {
    let ver = VersionExchange::default();

    // 1. In AwaitingCryptoChallenge, empty username with None password/token goes straight to NeedsAuthentication
    let mut sm = MachoHandshakeStateMachine::new(ver.clone(), MACHO_PROXY_NODE_ID, 700, 800);
    assert!(sm.step(&ver.to_pyrep()).is_ok());
    assert!(sm
        .step(&HandshakeCommand::CryptoRequest(CryptoRequestPacket::placebo()).to_pyrep())
        .is_ok());

    let mut empty_user = CryptoChallengePacket::new_password_login("", "secret");
    empty_user.user_password = None;
    let out_empty = sm.step(&empty_user.to_pyrep());
    assert!(matches!(out_empty, Ok(HandshakeStepOutput::NeedsAuthentication(_))));

    // 2. Password attempt 1 triggers passwordVersion = 1, attempt 2 with password=None does NOT loop
    let mut sm_retry = MachoHandshakeStateMachine::new(ver.clone(), MACHO_PROXY_NODE_ID, 701, 801);
    assert!(sm_retry.step(&ver.to_pyrep()).is_ok());
    assert!(sm_retry
        .step(&HandshakeCommand::CryptoRequest(CryptoRequestPacket::placebo()).to_pyrep())
        .is_ok());
    let mut attempt = CryptoChallengePacket::new_password_login("pilot", "secret");
    attempt.user_password = None;
    let first = sm_retry.step(&attempt.to_pyrep());
    assert!(matches!(first, Ok(HandshakeStepOutput::SendFramesAndContinue(_))));
    let second = sm_retry.step(&attempt.to_pyrep());
    assert!(matches!(second, Ok(HandshakeStepOutput::NeedsAuthentication(_))));

    // 3. Missing pending_identity in AwaitingHandshakeResult returns HANDSHAKE_FAILEDVERIFICATION
    let id = HandshakeIdentity {
        user_id: 55,
        role: crate::service_roles::ROLE_PLAYER_DEFAULT,
        language_id: "EN".into(),
        user_type: 30,
        address: "127.0.0.1:1".into(),
        access_token: None,
        computer_hash: None,
    };
    let mut chal_ok = CryptoChallengePacket::new_password_login("pilot", "secret");
    chal_ok.user_password = Some("secret".into());
    assert!(sm_retry.complete_challenge(&chal_ok, Ok(id), 1).is_ok());
    sm_retry.set_pending_identity_for_test(None);
    let good_res =
        CryptoHandshakeResult::for_server_challenge(&vec![0u8; DEFAULT_HANDSHAKE_PADDING_LEN]);
    let missing_err = sm_retry.step(&good_res.to_pyrep());
    assert_eq!(
        missing_err.err().and_then(|e| e.reason_code),
        Some("HANDSHAKE_FAILEDVERIFICATION".into())
    );
    assert!(matches!(sm_retry.phase, HandshakePhase::Failed(_)));
}

