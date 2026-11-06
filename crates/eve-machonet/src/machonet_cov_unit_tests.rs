use crate::handshake::{
    CryptoChallengePacket, CryptoHandshakeResult, CryptoRequestPacket, CryptoServerHandshake,
    HandshakeCommand, HandshakeIdentity, HandshakePhase, HandshakeStepOutput,
    MachoHandshakeStateMachine,
};
use crate::{
    extract_macho_call_req, service_roles, MachoAddress, MachoPacket, MachoPacketType,
    SessionState, VersionExchange,
};
use eve_proto::PyRep;
use std::collections::HashMap;

fn base_version() -> VersionExchange {
    VersionExchange {
        birthday: 170,
        macho_version: 445,
        user_count: 1,
        version_number: 21.05,
        build_version: 2472285,
        project_version: "eve-client-tq".into(),
    }
}

#[test]
fn macho_packet_raw_tuple_call_extraction_and_version_boundaries() {
    let mut named = HashMap::new();
    named.insert("channel".into(), PyRep::String("session".into()));
    let pkt = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id: 7001,
            call_id: 11,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: 888444,
            service: Some("stationSvc".into()),
            call_id: 11,
        },
        user_id: Some(90001),
        payload: PyRep::Tuple(vec![PyRep::String("GetSolarSystemInventory".into())]),
        named_payload: named,
    };
    let raw_tuple = pkt.to_pyrep();
    let decoded = MachoPacket::from_pyrep(&raw_tuple).expect("raw tuple MachoPacket decodes");
    assert_eq!(decoded.to_pyrep(), raw_tuple);
    assert_eq!(decoded.user_id, Some(90001));

    // Negative user_count in VersionExchange::from_pyrep must be rejected
    let neg_users = PyRep::Tuple(vec![
        PyRep::Integer(170),
        PyRep::Integer(445),
        PyRep::Integer(-1),
        PyRep::Real(21.05),
        PyRep::Integer(2472285),
        PyRep::String("eve-client-tq".into()),
    ]);
    assert!(VersionExchange::from_pyrep(&neg_users).is_none());

    // Version tolerance within 1e-4 passes validate_client
    let server = base_version();
    let mut edge_client = server.clone();
    edge_client.version_number = server.version_number + 0.99e-4;
    assert!(server.validate_client(&edge_client).is_ok());

    // Call extraction: short inner tuple, zero non-substream bound id, and direct tuple payload
    let short_inner = PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::Integer(5)])]);
    let (bid_short, method_short, args_short, kwargs_short) = extract_macho_call_req(&short_inner);
    assert_eq!(bid_short, None);
    assert!(method_short.is_empty());
    assert!(args_short.is_empty());
    assert!(kwargs_short.is_empty());

    let zero_bound = PyRep::Tuple(vec![PyRep::Tuple(vec![
        PyRep::Integer(0),
        PyRep::Tuple(vec![PyRep::String("Echo".into())]),
    ])]);
    let (bid_zero, method_zero, _, _) = extract_macho_call_req(&zero_bound);
    assert_eq!(bid_zero, None);
    assert_eq!(method_zero, "Echo");

    let single_method_tuple = PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::String("Ping".into())])]);
    let (bid_direct, method_direct, args_direct, kwargs_direct) =
        extract_macho_call_req(&single_method_tuple);
    assert_eq!(bid_direct, None);
    assert_eq!(method_direct, "Ping");
    assert!(args_direct.is_empty());
    assert!(kwargs_direct.is_empty());
}

#[test]
fn session_diff_docked_solar_system_empty_address_and_role_bitmasks() {
    let mut docked_a = SessionState::new(101);
    docked_a.solar_system_id = Some(30_000_142);
    docked_a.station_id = Some(60_003_760);
    docked_a.address = "127.0.0.1:26000".into();

    let mut docked_b = docked_a.clone();
    docked_b.solar_system_id = Some(30_000_144);
    docked_b.station_id = Some(60_003_761);
    docked_b.address.clear();

    let diff = docked_a.compute_diff(&docked_b);
    assert!(diff.contains_key("solarsystemid2"));
    assert!(!diff.contains_key("solarsystemid"));
    assert!(!diff.contains_key("address"));

    assert_eq!(
        service_roles::ROLE_PLAYER_DEFAULT,
        service_roles::ROLE_PLAYER | service_roles::ROLE_LOGIN
    );
    assert_eq!(
        service_roles::ROLE_GM_MASK,
        service_roles::ROLE_GML
            | service_roles::ROLE_GMH
            | service_roles::ROLE_GMS
            | service_roles::ROLE_ADMIN
            | service_roles::ROLE_WORLDMOD
    );
    assert_eq!(
        service_roles::ROLE_GM_DEFAULT,
        service_roles::ROLE_PLAYER_DEFAULT | service_roles::ROLE_GM_MASK
    );
}

#[test]
fn handshake_commands_dicts_sso_retry_skip_and_missing_identity_error() {
    // HandshakeCommand requires None in slot 0 and matching command tag
    assert_eq!(
        HandshakeCommand::from_pyrep(&PyRep::Tuple(vec![
            PyRep::String("NOT_NONE".into()),
            PyRep::String("QC".into()),
        ])),
        None
    );
    assert_eq!(
        HandshakeCommand::from_pyrep(&PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::String("VK".into()),
            PyRep::String("vip-key".into()),
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

    // CryptoServerHandshake with non-empty context and config_vals dictionaries
    let mut context = HashMap::new();
    context.insert("reason".into(), PyRep::String("login".into()));
    let mut config_vals = HashMap::new();
    config_vals.insert("maxSessions".into(), PyRep::Integer(10));
    let srv = CryptoServerHandshake {
        server_challenge: b"srv-nonce-123456".to_vec(),
        func_marshaled_code: b"bytecode".to_vec(),
        verification: false,
        context,
        challenge_response_hash: "deadbeef".into(),
        macho_version: 445,
        boot_version: 21.05,
        boot_build: 2472285,
        boot_codename: "EVE-EVE-TRANQUILITY".into(),
        boot_region: "optic".into(),
        cluster_user_count: 12,
        proxy_node_id: 888444,
        user_logon_queue_position: 0,
        config_vals,
    };
    let srv_rep = srv.to_pyrep();
    let srv_decoded =
        CryptoServerHandshake::from_pyrep(&srv_rep).expect("challenge with dicts decodes");
    assert_eq!(
        srv_decoded.context.get("reason"),
        Some(&PyRep::String("login".into()))
    );
    assert_eq!(
        srv_decoded.config_vals.get("maxSessions"),
        Some(&PyRep::Integer(10))
    );

    // CryptoHandshakeResult decodes raw bytes func_output
    let res_rep = PyRep::Tuple(vec![
        PyRep::String("ok-hash".into()),
        PyRep::Bytes(vec![9, 8, 7, 6]),
        PyRep::Bool(true),
    ]);
    let res = CryptoHandshakeResult::from_pyrep(&res_rep).expect("handshake result decodes");
    assert_eq!(res.func_output, vec![9, 8, 7, 6]);

    // Version within 1e-4 tolerance passes CryptoChallengePacket::validate_version
    let expected = base_version();
    let challenge = CryptoChallengePacket {
        client_challenge: vec![1; 16],
        macho_version: expected.macho_version,
        boot_version: expected.version_number + 0.99e-4,
        boot_build: expected.build_version,
        boot_codename: expected.codename().into(),
        boot_region: expected.region().into(),
        user_name: Some("EVE-SSO-CONNECTION".into()),
        user_password: None,
        user_password_hash: None,
        user_sso_token: None,
        user_affiliate_id: 0,
        user_language_id: "EN".into(),
    };
    assert!(challenge.validate_version(&expected).is_ok());

    // EVE-SSO-CONNECTION without password/token proceeds directly to NeedsAuthentication
    let mut sm = MachoHandshakeStateMachine::new(expected.clone(), 888444, 5001, 6001);
    let s1 = sm
        .step(&expected.to_pyrep())
        .expect("version exchange succeeds");
    assert!(matches!(s1, HandshakeStepOutput::SendFramesAndContinue(_)));
    let req_rep = HandshakeCommand::CryptoRequest(CryptoRequestPacket {
        key_version: "placebo".into(),
        key_params: HashMap::new(),
    })
    .to_pyrep();
    let s2 = sm.step(&req_rep).expect("crypto request succeeds");
    assert!(matches!(s2, HandshakeStepOutput::SendFramesAndContinue(_)));
    let s3 = sm
        .step(&challenge.to_pyrep())
        .expect("SSO connection skips passwordVersion 1 retry");
    assert!(matches!(s3, HandshakeStepOutput::NeedsAuthentication(_)));

    // Missing pending_identity in AwaitingHandshakeResult returns HANDSHAKE_FAILEDVERIFICATION
    sm.complete_challenge(
        &challenge,
        Ok(HandshakeIdentity {
            user_id: 42,
            role: service_roles::ROLE_PLAYER_DEFAULT,
            user_type: 1,
            language_id: "EN".into(),
            address: "127.0.0.1".into(),
            access_token: None,
            computer_hash: None,
        }),
        1,
    )
    .expect("complete challenge transitions to AwaitingHandshakeResult");
    assert_eq!(sm.phase, HandshakePhase::AwaitingHandshakeResult);
    sm.set_pending_identity_for_test(None);
    let expected_hash = crate::handshake::placebo_crypto_hash(&vec![
        0u8;
        crate::handshake::DEFAULT_HANDSHAKE_PADDING_LEN
    ]);
    let valid_hash_res = PyRep::Tuple(vec![
        PyRep::String(expected_hash),
        PyRep::Bytes(vec![9, 8, 7, 6]),
        PyRep::Bool(true),
    ]);
    let err = sm
        .step(&valid_hash_res)
        .expect_err("missing pending identity must fail cleanly");
    assert_eq!(
        err.reason_code.as_deref(),
        Some("HANDSHAKE_FAILEDVERIFICATION")
    );
    assert_eq!(err.reason, "Missing authenticated identity");
}
