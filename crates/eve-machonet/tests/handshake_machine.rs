//! Comprehensive protocol and state-machine tests for `eve-machonet` (`handshake.rs`).

use eve_machonet::{
    crc_hqx, placebo_crypto_hash, service_roles, CryptoChallengePacket, CryptoHandshakeAck,
    CryptoHandshakeResult, CryptoRequestPacket, CryptoServerHandshake, GPSTransportClosed,
    HandshakeCommand, HandshakeIdentity, HandshakePhase, HandshakeStepOutput, MachoAddress,
    MachoHandshakeStateMachine, MachoPacket, MachoPacketType, VersionExchange,
    DEFAULT_HANDSHAKE_PADDING_LEN, V24_BOOT_BUILD, V24_MACHO_VERSION,
};
use eve_proto::{marshal, PyRep};
use std::collections::HashMap;

#[test]
fn test_crc_hqx_and_placebo_crypto_hash_variants() {
    assert_eq!(crc_hqx(&[], 0), 0);
    assert_eq!(crc_hqx(&[], 0x1234), 0x1234);
    assert_eq!(crc_hqx(b"123456789", 0), 0x31C3);

    let h0 = placebo_crypto_hash(&[]);
    let h1 = placebo_crypto_hash(&[0xAB]);
    let h64 = placebo_crypto_hash(&[0x11; 64]);
    let h300 = placebo_crypto_hash(&vec![0x22; 300]);
    assert_ne!(h0, h1);
    assert_ne!(h1, h64);
    assert_ne!(h64, h300);
    assert_eq!(h64, placebo_crypto_hash(&[0x11; 64]));
}

#[test]
fn test_gps_transport_closed_and_handshake_commands() {
    let closed = GPSTransportClosed::new("kicked", "HANDSHAKE_FAILED")
        .with_arg("detail", PyRep::ascii_bytes("bad ticket"));
    assert_eq!(closed.reason, "kicked");
    assert_eq!(closed.reason_code.as_deref(), Some("HANDSHAKE_FAILED"));
    assert_eq!(closed.reason_args.get("detail"), Some(&PyRep::ascii_bytes("bad ticket")));

    let raw = marshal::encode(&closed.to_pyrep());
    let decoded = GPSTransportClosed::from_pyrep(&marshal::decode(&raw).unwrap()).expect("closed");
    assert_eq!(decoded.reason, "kicked");
    assert_eq!(decoded.reason_code.as_deref(), Some("HANDSHAKE_FAILED"));
    assert_eq!(
        decoded.reason_args.get("detail"),
        Some(&PyRep::ascii_bytes("bad ticket"))
    );

    // ObjectEx and GPSRemoteTransportClosed variants + invalid shapes
    let remote_objex = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("exceptions.GPSRemoteTransportClosed".into()),
            PyRep::Tuple(vec![PyRep::ascii_bytes("remote closed"), PyRep::None]),
        ])),
        list: vec![],
        dict: vec![],
    };
    let parsed_remote = GPSTransportClosed::from_pyrep(&remote_objex).expect("remote");
    assert_eq!(parsed_remote.reason, "remote closed");
    assert_eq!(parsed_remote.reason_code, None);

    let flat_objex = PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("GPSTransportClosed".into()),
            PyRep::ascii_bytes("flat"),
        ])),
        list: vec![],
        dict: vec![],
    };
    let parsed_flat = GPSTransportClosed::from_pyrep(&flat_objex).expect("flat");
    assert_eq!(parsed_flat.reason, "GPSTransportClosed");

    let non_gps = PyRep::Object {
        type_name: "exceptions.RuntimeError".into(),
        args: Box::new(PyRep::Tuple(vec![PyRep::ascii_bytes("other")])),
    };
    assert!(GPSTransportClosed::from_pyrep(&non_gps).is_none());
    assert!(GPSTransportClosed::from_pyrep(&PyRep::None).is_none());

    // HandshakeCommand: QueueCheck, VipKey, CryptoRequest + invalid
    let qc = HandshakeCommand::QueueCheck;
    assert_eq!(
        HandshakeCommand::from_pyrep(&qc.to_pyrep()),
        Some(HandshakeCommand::QueueCheck)
    );

    let vk = HandshakeCommand::VipKey("VIP-123".into());
    assert_eq!(
        HandshakeCommand::from_pyrep(&vk.to_pyrep()),
        Some(HandshakeCommand::VipKey("VIP-123".into()))
    );

    let mut req = CryptoRequestPacket::placebo();
    req.key_params.insert("version".into(), PyRep::Integer(1));
    let cmd_req = HandshakeCommand::CryptoRequest(req.clone());
    assert_eq!(
        HandshakeCommand::from_pyrep(&cmd_req.to_pyrep()),
        Some(HandshakeCommand::CryptoRequest(req))
    );
    assert!(HandshakeCommand::from_pyrep(&PyRep::None).is_none());
    assert!(CryptoRequestPacket::from_pyrep(&PyRep::Tuple(vec![PyRep::ascii_bytes("p")])).is_none());
}

#[test]
fn test_crypto_challenge_server_handshake_result_and_ack_roundtrips() {
    let ver = VersionExchange::default();
    let pw_challenge = CryptoChallengePacket::new_password_login("pilot1", "s3cr3t");
    let pw_decoded = CryptoChallengePacket::from_pyrep(&pw_challenge.to_pyrep()).expect("pw");
    assert_eq!(pw_decoded.user_name.as_deref(), Some("pilot1"));
    assert_eq!(pw_decoded.user_password.as_deref(), Some("s3cr3t"));
    assert_eq!(pw_decoded.user_sso_token, None);
    assert!(pw_decoded.validate_version(&ver).is_ok());

    let sso_challenge = CryptoChallengePacket::new_sso_login("jwt-token-xyz");
    let sso_decoded = CryptoChallengePacket::from_pyrep(&sso_challenge.to_pyrep()).expect("sso");
    assert_eq!(sso_decoded.user_name.as_deref(), Some("EVE-SSO-CONNECTION"));
    assert_eq!(sso_decoded.user_password, None);
    assert_eq!(sso_decoded.user_sso_token.as_deref(), Some("jwt-token-xyz"));

    // All 5 validate_version failure branches on CryptoChallengePacket
    let mut c_reg = pw_challenge.clone();
    c_reg.boot_region = "serenity".into();
    assert_eq!(
        c_reg.validate_version(&ver).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEREGION")
    );
    let mut c_code = pw_challenge.clone();
    c_code.boot_codename = "V23.0".into();
    assert_eq!(
        c_code.validate_version(&ver).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLERELEASE")
    );
    let mut c_ver = pw_challenge.clone();
    c_ver.boot_version = 99.0;
    assert_eq!(
        c_ver.validate_version(&ver).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEVERSION")
    );
    let mut c_macho = pw_challenge.clone();
    c_macho.macho_version = V24_MACHO_VERSION + 1;
    assert_eq!(
        c_macho.validate_version(&ver).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEPROTOCOL")
    );
    let mut c_build = pw_challenge.clone();
    c_build.boot_build = V24_BOOT_BUILD - 1;
    assert_eq!(
        c_build.validate_version(&ver).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEBUILD")
    );

    // extract_password_str variants (String, ObjectEx, Tuple)
    let raw_pw_tuple = PyRep::Tuple(vec![
        PyRep::Buffer(vec![1, 2, 3, 4]),
        PyRep::Dict(vec![
            (PyRep::ascii_bytes("macho_version"), PyRep::Integer(V24_MACHO_VERSION as i64)),
            (PyRep::ascii_bytes("boot_version"), PyRep::Integer(24)),
            (PyRep::ascii_bytes("boot_build"), PyRep::Integer(V24_BOOT_BUILD as i64)),
            (PyRep::ascii_bytes("boot_codename"), PyRep::ascii_bytes("24.0")),
            (PyRep::ascii_bytes("boot_region"), PyRep::ascii_bytes("ccp")),
            (PyRep::ascii_bytes("user_name"), PyRep::String("alice".into())),
            (
                PyRep::ascii_bytes("user_password"),
                PyRep::ObjectEx {
                    is_type_2: false,
                    header: Box::new(PyRep::Tuple(vec![
                        PyRep::Token("PasswordString".into()),
                        PyRep::Tuple(vec![PyRep::ascii_bytes("pw_objex")]),
                    ])),
                    list: vec![],
                    dict: vec![],
                },
            ),
            (
                PyRep::ascii_bytes("user_password_hash"),
                PyRep::Bytes(vec![9, 8, 7]),
            ),
            (PyRep::ascii_bytes("user_languageid"), PyRep::ascii_bytes("FR")),
            (PyRep::ascii_bytes("user_affiliateid"), PyRep::Integer(12)),
        ]),
    ]);
    let parsed_custom = CryptoChallengePacket::from_pyrep(&raw_pw_tuple).expect("custom");
    assert_eq!(parsed_custom.user_password.as_deref(), Some("pw_objex"));
    assert_eq!(parsed_custom.user_password_hash, Some(vec![9, 8, 7]));
    assert_eq!(parsed_custom.user_language_id, "FR");
    assert_eq!(parsed_custom.user_affiliate_id, 12);
    assert!(CryptoChallengePacket::from_pyrep(&PyRep::None).is_none());

    // CryptoServerHandshake roundtrip & Integer verification
    let mut srv_hs = CryptoServerHandshake::for_client_challenge(&[1, 2, 3], &ver, 888_444, 5);
    srv_hs
        .config_vals
        .insert("enableQuantum".into(), PyRep::Bool(true));
    let srv_hs_dec = CryptoServerHandshake::from_pyrep(&srv_hs.to_pyrep()).expect("srv_hs");
    assert_eq!(srv_hs_dec.proxy_node_id, 888_444);
    assert_eq!(srv_hs_dec.cluster_user_count, 5);
    assert!(!srv_hs_dec.verification);
    assert_eq!(
        srv_hs_dec.config_vals.get("enableQuantum"),
        Some(&PyRep::Bool(true))
    );
    srv_hs.verification = true;
    assert!(CryptoServerHandshake::from_pyrep(&srv_hs.to_pyrep())
        .expect("srv_hs verified")
        .verification);
    assert!(CryptoServerHandshake::from_pyrep(&PyRep::None).is_none());

    // CryptoHandshakeResult roundtrip
    let res = CryptoHandshakeResult::for_server_challenge(&srv_hs.server_challenge);
    let res_dec = CryptoHandshakeResult::from_pyrep(&res.to_pyrep()).expect("res");
    assert_eq!(res_dec.challenge_response_hash, res.challenge_response_hash);
    assert!(CryptoHandshakeResult::from_pyrep(&PyRep::Tuple(vec![])).is_none());

    // CryptoHandshakeAck roundtrip with optional fields populated and empty
    let id = HandshakeIdentity {
        user_id: 77,
        role: service_roles::ROLE_PLAYER_DEFAULT,
        language_id: "EN".into(),
        user_type: 30,
        address: "10.0.0.1".into(),
        access_token: Some("acc-tok".into()),
        computer_hash: Some("comp-hash".into()),
    };
    let mut ack = CryptoHandshakeAck::from_identity(1001, 2002, &id);
    ack.max_session_time = Some(3600);
    ack.in_detention = Some(PyRep::Integer(1));
    ack.client_hash = Some("cli-hash".into());
    ack.live_updates = vec![PyRep::Integer(99)];

    let ack_dec = CryptoHandshakeAck::from_pyrep(&ack.to_pyrep()).expect("ack");
    assert_eq!(ack_dec.session_id, 1001);
    assert_eq!(ack_dec.client_id, 2002);
    assert_eq!(ack_dec.user_id, 77);
    assert_eq!(ack_dec.max_session_time, Some(3600));
    assert_eq!(ack_dec.in_detention, Some(PyRep::Integer(1)));
    assert_eq!(ack_dec.client_hash.as_deref(), Some("cli-hash"));
    assert_eq!(ack_dec.live_updates, vec![PyRep::Integer(99)]);
    assert_eq!(ack_dec.access_token.as_deref(), Some("acc-tok"));
    assert_eq!(ack_dec.computer_hash.as_deref(), Some("comp-hash"));
    assert!(CryptoHandshakeAck::from_pyrep(&PyRep::None).is_none());
}

#[test]
fn test_handshake_state_machine_full_flows_and_error_paths() {
    let ver = VersionExchange::default();
    let id = HandshakeIdentity {
        user_id: 500,
        role: service_roles::ROLE_PLAYER_DEFAULT,
        language_id: "EN".into(),
        user_type: 30,
        address: "127.0.0.1".into(),
        access_token: None,
        computer_hash: None,
    };

    // 1. Password V2 -> V1 two-attempt flow + QueueCheck + VipKey + config_vals
    let mut sm = MachoHandshakeStateMachine::new(ver.clone(), 888_444, 10, 20);
    let mut cfg = HashMap::new();
    cfg.insert("foo".into(), PyRep::Integer(1));
    sm.set_config_vals(cfg);
    assert!(!sm.is_authenticated());
    assert_eq!(VersionExchange::from_pyrep(&sm.server_hello()), Some(ver.clone()));

    // Step 1: Client VersionExchange
    let HandshakeStepOutput::SendFramesAndContinue(frames) = sm.step(&ver.to_pyrep()).expect("ver")
    else {
        panic!("expected SendFramesAndContinue");
    };
    assert!(frames.is_empty());
    assert_eq!(sm.phase, HandshakePhase::AwaitingCommandOrCrypto);

    // QueueCheck resets phase to AwaitingVersionExchange and returns [1, server_hello]
    let qc_out = sm
        .step(&HandshakeCommand::QueueCheck.to_pyrep())
        .expect("qc");
    assert_eq!(sm.phase, HandshakePhase::AwaitingVersionExchange);
    match qc_out {
        HandshakeStepOutput::SendFramesAndContinue(frames) => {
            assert_eq!(frames.len(), 2);
            assert_eq!(frames[0], PyRep::Integer(1));
        }
        other => panic!("unexpected qc output: {other:?}"),
    }

    // Re-send VersionExchange then VipKey then CryptoRequest
    sm.step(&ver.to_pyrep()).expect("ver again");
    let HandshakeStepOutput::SendFramesAndContinue(frames) = sm
        .step(&HandshakeCommand::VipKey("MY-VIP".into()).to_pyrep())
        .expect("vip")
    else {
        panic!("expected SendFramesAndContinue");
    };
    assert!(frames.is_empty());
    assert_eq!(sm.vip_key.as_deref(), Some("MY-VIP"));

    let HandshakeStepOutput::SendFramesAndContinue(frames) = sm
        .step(&HandshakeCommand::CryptoRequest(CryptoRequestPacket::placebo()).to_pyrep())
        .expect("crypto_req")
    else {
        panic!("expected SendFramesAndContinue");
    };
    assert_eq!(frames, vec![PyRep::ascii_bytes("OK CC")]);
    assert_eq!(sm.phase, HandshakePhase::AwaitingCryptoChallenge);

    // Attempt 1 with passwordVersion=2: user_password=None -> returns [1]
    let mut attempt1 = CryptoChallengePacket::new_password_login("pilot1", "");
    attempt1.user_password = None;
    let HandshakeStepOutput::SendFramesAndContinue(frames) =
        sm.step(&attempt1.to_pyrep()).expect("attempt1")
    else {
        panic!("expected SendFramesAndContinue");
    };
    assert_eq!(frames, vec![PyRep::Integer(1)]);

    // Attempt 2 with populated password -> NeedsAuthentication
    let attempt2 = CryptoChallengePacket::new_password_login("pilot1", "pass123");
    let HandshakeStepOutput::NeedsAuthentication(chal) =
        sm.step(&attempt2.to_pyrep()).expect("attempt2")
    else {
        panic!("expected NeedsAuthentication");
    };
    let frames_after_complete = sm
        .complete_challenge(&chal, Ok(id.clone()), 3)
        .expect("complete");
    assert_eq!(frames_after_complete.len(), 1);
    let srv_hs = CryptoServerHandshake::from_pyrep(&frames_after_complete[0]).expect("srv_hs");
    assert_eq!(srv_hs.config_vals.get("foo"), Some(&PyRep::Integer(1)));

    // Step 4: CryptoHandshakeResult -> Authenticated
    let res = CryptoHandshakeResult::for_server_challenge(&srv_hs.server_challenge);
    let HandshakeStepOutput::Authenticated {
        frames: auth_frames,
        identity: out_id,
    } = sm.step(&res.to_pyrep()).expect("auth")
    else {
        panic!("expected Authenticated");
    };
    assert!(sm.is_authenticated());
    assert_eq!(out_id, id);
    assert_eq!(auth_frames.len(), 3);
    assert!(CryptoHandshakeAck::from_pyrep(&auth_frames[0]).is_some());
    assert_eq!(
        MachoPacket::from_pyrep(&auth_frames[1]).map(|p| p.packet_type),
        Some(MachoPacketType::SessionInitialStateNotification)
    );
    assert_eq!(
        MachoPacket::from_pyrep(&auth_frames[2]).map(|p| p.packet_type),
        Some(MachoPacketType::SessionChangeNotification)
    );

    // Stepping after Authenticated fails cleanly
    assert_eq!(
        sm.step(&PyRep::None).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_ALREADY_COMPLETE")
    );

    // 2. SSO login flow directly from AwaitingCryptoChallenge (complete_challenge returns [2, server_hs])
    let mut sso_sm = MachoHandshakeStateMachine::new(ver.clone(), 1, 2, 3);
    sso_sm.step(&ver.to_pyrep()).unwrap();
    sso_sm
        .step(&HandshakeCommand::CryptoRequest(CryptoRequestPacket::placebo()).to_pyrep())
        .unwrap();
    let HandshakeStepOutput::NeedsAuthentication(sso_chal) = sso_sm
        .step(&CryptoChallengePacket::new_sso_login("sso-tok").to_pyrep())
        .unwrap()
    else {
        panic!("expected NeedsAuthentication");
    };
    let sso_frames = sso_sm.complete_challenge(&sso_chal, Ok(id), 1).unwrap();
    assert_eq!(sso_frames.len(), 2);
    assert_eq!(sso_frames[0], PyRep::Integer(2));

    // 3. Error branches across every phase
    // Premature MachoPacket
    let mut bad_pkt_sm = MachoHandshakeStateMachine::new(ver.clone(), 1, 2, 3);
    let premature_pkt = MachoPacket {
        packet_type: MachoPacketType::AuthenticationReq,
        source: MachoAddress::Client {
            client_id: 1,
            call_id: 0,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: 1,
            service: None,
            call_id: 0,
        },
        user_id: None,
        payload: PyRep::ascii_bytes("ok"),
        named_payload: HashMap::new(),
    };
    assert_eq!(
        bad_pkt_sm
            .step(&premature_pkt.to_pyrep())
            .unwrap_err()
            .reason_code
            .as_deref(),
        Some("HANDSHAKE_REQUIRED")
    );
    // Subsequent step in Failed phase
    assert_eq!(
        bad_pkt_sm.step(&PyRep::None).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_FAILED")
    );

    // Non-placebo key_version and CryptoAPI crypting_sessionkey rejected
    let mut key_sm = MachoHandshakeStateMachine::new(ver.clone(), 1, 2, 3);
    key_sm.step(&ver.to_pyrep()).unwrap();
    let bad_key = CryptoRequestPacket {
        key_version: "rsa".into(),
        key_params: HashMap::new(),
    };
    assert_eq!(
        key_sm
            .step(&HandshakeCommand::CryptoRequest(bad_key).to_pyrep())
            .unwrap_err()
            .reason_code
            .as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEPUBLICKEY")
    );

    let mut capi_sm = MachoHandshakeStateMachine::new(ver.clone(), 1, 2, 3);
    capi_sm.step(&ver.to_pyrep()).unwrap();
    let mut capi = CryptoRequestPacket::placebo();
    capi.key_params
        .insert("crypting_sessionkey".into(), PyRep::Bytes(vec![1; DEFAULT_HANDSHAKE_PADDING_LEN]));
    assert_eq!(
        capi_sm
            .step(&HandshakeCommand::CryptoRequest(capi).to_pyrep())
            .unwrap_err()
            .reason_code
            .as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEPUBLICKEY")
    );

    // Challenge response hash mismatch & complete_challenge Err
    let mut hash_sm = MachoHandshakeStateMachine::new(ver.clone(), 1, 2, 3);
    hash_sm.step(&ver.to_pyrep()).unwrap();
    hash_sm
        .step(&HandshakeCommand::CryptoRequest(CryptoRequestPacket::placebo()).to_pyrep())
        .unwrap();
    let HandshakeStepOutput::NeedsAuthentication(c3) = hash_sm
        .step(&CryptoChallengePacket::new_sso_login("tok").to_pyrep())
        .unwrap()
    else {
        panic!("expected NeedsAuthentication");
    };
    let mut hash_sm_err = hash_sm.clone();
    assert!(hash_sm_err
        .complete_challenge(
            &c3,
            Err(GPSTransportClosed::new("bad login", "HANDSHAKE_FAILEDVERIFICATION")),
            1
        )
        .is_err());

    hash_sm
        .complete_challenge(
            &c3,
            Ok(HandshakeIdentity {
                user_id: 1,
                role: 0,
                language_id: "EN".into(),
                user_type: 30,
                address: "127.0.0.1".into(),
                access_token: None,
                computer_hash: None,
            }),
            1,
        )
        .unwrap();
    let bad_res = CryptoHandshakeResult {
        challenge_response_hash: "wrong-crc".into(),
        func_output: vec![],
        func_result: PyRep::None,
    };
    assert_eq!(
        hash_sm
            .step(&bad_res.to_pyrep())
            .unwrap_err()
            .reason_code
            .as_deref(),
        Some("HANDSHAKE_FAILEDVERIFICATION")
    );
}
