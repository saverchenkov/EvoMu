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

#[test]
fn wrapped_user_error_dict_dict_key_and_named_helpers_roundtrip_all_fields() {
    let extract_user_error = |body: PyRep| -> (i64, String, Vec<(PyRep, PyRep)>) {
        let PyRep::Tuple(items) = body else {
            panic!("expected 3-tuple ErrorResponse body, got {body:?}");
        };
        assert_eq!(items.len(), 3);
        let orig_cmd = items[0].as_i64().expect("original_command integer");
        assert_eq!(
            items[1].as_i64(),
            Some(crate::MACHONETERR_WRAPPEDEXCEPTION),
            "must emit MACHONETERR_WRAPPEDEXCEPTION"
        );
        let PyRep::Tuple(payload_items) = &items[2] else {
            panic!("expected payload 1-tuple");
        };
        assert_eq!(payload_items.len(), 1);
        let PyRep::Bytes(raw) = &payload_items[0] else {
            panic!("expected marshaled PyRep::Bytes payload");
        };
        let exc = eve_proto::marshal::decode(raw).expect("wrapped exception must unmarshal");
        let PyRep::ObjectEx {
            is_type_2,
            header,
            list,
            dict,
        } = exc
        else {
            panic!("expected ObjectEx UserError, got {exc:?}");
        };
        assert!(!is_type_2);
        assert!(list.is_empty());
        assert!(dict.is_empty());

        let PyRep::Tuple(hdr_parts) = *header else {
            panic!("expected ObjectEx header tuple");
        };
        assert_eq!(hdr_parts.len(), 2);
        assert_eq!(hdr_parts[0], PyRep::Token("eveexceptions.UserError".into()));
        let PyRep::Tuple(args_parts) = &hdr_parts[1] else {
            panic!("expected UserError args 2-tuple");
        };
        assert_eq!(args_parts.len(), 2);
        let err_name = args_parts[0]
            .as_str()
            .expect("error name UTF-8 string/bytes")
            .to_string();
        let PyRep::Dict(entries) = &args_parts[1] else {
            panic!("expected UserError Dict entries");
        };
        (orig_cmd, err_name, entries.clone())
    };

    // 1. wrapped_user_error_with_dict_body (empty dict & multi-entry dict)
    let empty_body = crate::wrapped_user_error_with_dict_body(6, "EmptyDictErr", Vec::new());
    let (cmd, name, entries) = extract_user_error(empty_body);
    assert_eq!(cmd, 6);
    assert_eq!(name, "EmptyDictErr");
    assert!(entries.is_empty());

    let multi_body = crate::wrapped_user_error_with_dict_body(
        12,
        "MultiEntryErr",
        vec![
            (PyRep::String("itemID".into()), PyRep::Integer(42)),
            (PyRep::String("reason".into()), PyRep::String("locked".into())),
        ],
    );
    let (cmd, name, entries) = extract_user_error(multi_body);
    assert_eq!(cmd, 12);
    assert_eq!(name, "MultiEntryErr");
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].0.as_str(), Some("itemID"));
    assert_eq!(entries[0].1.as_i64(), Some(42));
    assert_eq!(entries[1].0.as_str(), Some("reason"));
    assert_eq!(entries[1].1.as_str(), Some("locked"));

    // 2. wrapped_user_error_with_dict_key_body (custom key + value)
    let keyed_body = crate::wrapped_user_error_with_dict_key_body(
        7,
        "DockDenied",
        "stationName",
        "Jita IV - Moon 4",
    );
    let (cmd, name, entries) = extract_user_error(keyed_body);
    assert_eq!(cmd, 7);
    assert_eq!(name, "DockDenied");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].0.as_str(), Some("stationName"));
    assert_eq!(entries[0].1.as_str(), Some("Jita IV - Moon 4"));

    // 3. wrapped_named_user_error_body (custom user_error_name + message)
    let named_body =
        crate::wrapped_named_user_error_body(20, "CustomValidationFailed", "invalid target");
    let (cmd, name, entries) = extract_user_error(named_body);
    assert_eq!(cmd, 20);
    assert_eq!(name, "CustomValidationFailed");
    assert_eq!(entries.len(), 1);
    assert!(entries[0].0.as_str().is_some_and(|k| !k.is_empty()));
    assert_eq!(entries[0].1.as_str(), Some("invalid target"));
}

#[test]
fn machonet_uncovered_branches_call_req_substream_corp_account_diff_and_handshake_dicts() {
    // 1. extract_macho_call_req: kwargs Dict non-str key skip (lib.rs L729) & 2-tuple (method, args) forms
    let payload = PyRep::Tuple(vec![PyRep::Tuple(vec![
        PyRep::Integer(1),
        PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::String("N=888444:98765".into()),
                PyRep::String("FetchBalance".into()),
                PyRep::Tuple(vec![PyRep::Integer(1000), PyRep::String("ISK".into())]),
                PyRep::Dict(vec![
                    (PyRep::Integer(999), PyRep::String("ignored_non_str".into())),
                    (PyRep::String("includeZero".into()), PyRep::Bool(false)),
                ]),
            ]))),
        ]))),
    ])]);
    let (bound_id, method, args, kwargs) = extract_macho_call_req(&payload);
    assert_eq!(bound_id, Some(98765));
    assert_eq!(method, "FetchBalance");
    assert_eq!(args, vec![PyRep::Integer(1000), PyRep::String("ISK".into())]);
    assert_eq!(kwargs.len(), 1);
    assert_eq!(kwargs.get("includeZero"), Some(&PyRep::Bool(false)));

    // Also test explicit numeric bound Tuple(1234, SubStream((Tuple(method, Tuple(args)))))
    let numeric_sub = PyRep::Tuple(vec![PyRep::Tuple(vec![
        PyRep::Integer(1234),
        PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::String("Ping".into()),
            PyRep::Tuple(vec![PyRep::Bool(true)]),
        ]))),
    ])]);
    let (b2, m2, a2, k2) = extract_macho_call_req(&numeric_sub);
    assert_eq!(b2, Some(1234));
    assert_eq!(m2, "Ping");
    assert_eq!(a2, vec![PyRep::Bool(true)]);
    assert!(k2.is_empty());

    // Cover non-Tuple inner_call (lib.rs L704) & non-Dict kwargs slot (lib.rs L729)
    let (b_non_tup, m_non_tup, _, _) =
        extract_macho_call_req(&PyRep::Tuple(vec![PyRep::Integer(99)]));
    assert_eq!(b_non_tup, None);
    assert_eq!(m_non_tup, "");
    let non_dict_kw = PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::Tuple(vec![
        PyRep::String("N=888444:55".into()),
        PyRep::String("Op".into()),
        PyRep::Tuple(vec![]),
        PyRep::Integer(0),
    ])))]);
    let (b_nd, m_nd, _, k_nd) = extract_macho_call_req(&non_dict_kw);
    assert_eq!(b_nd, Some(55));
    assert_eq!(m_nd, "Op");
    assert!(k_nd.is_empty());

    // 2. SessionState::compute_diff corp_account_key change requires character_id.is_some() (lib.rs L831-837)
    let mut base_no_char = SessionState::new(1);
    let mut next_no_char = base_no_char.clone();
    next_no_char.corp_account_key = 1002;
    assert!(
        !base_no_char
            .compute_diff(&next_no_char)
            .iter()
            .any(|(k, _)| k == "corpAccountKey"),
        "corp_account_key diff must not emit before character selection"
    );

    base_no_char.character_id = Some(5001);
    next_no_char.character_id = Some(5001);
    let diff = base_no_char.compute_diff(&next_no_char);
    assert_eq!(
        diff,
        HashMap::from([(
            "corpAccountKey".to_string(),
            (
                PyRep::Integer(1000),
                PyRep::Integer(1002),
            ),
        )])
    );

    // 3. CryptoServerHandshake::from_pyrep context / response non-Dict and non-str dict keys (handshake.rs L702, L741)
    let base_resp = |config_val: PyRep| {
        PyRep::Dict(vec![
            (PyRep::Integer(123), PyRep::String("skip".into())),
            (
                PyRep::String("challenge_responsehash".into()),
                PyRep::String("md5".into()),
            ),
            (PyRep::String("macho_version".into()), PyRep::Integer(445)),
            (PyRep::String("boot_version".into()), PyRep::Real(21.05)),
            (PyRep::String("boot_build".into()), PyRep::Integer(2400000)),
            (
                PyRep::String("boot_codename".into()),
                PyRep::String("EVEMU".into()),
            ),
            (
                PyRep::String("boot_region".into()),
                PyRep::String("optic".into()),
            ),
            (PyRep::String("config_vals".into()), config_val),
        ])
    };
    let srv_with_non_dict_context = PyRep::Tuple(vec![
        PyRep::Bytes(vec![1, 2, 3]),
        PyRep::Tuple(vec![PyRep::Bytes(vec![4, 5]), PyRep::Bool(true)]),
        PyRep::Integer(77), // items[2] not a Dict (L702)
        base_resp(PyRep::Integer(0)), // config_vals not a Dict (L741)
    ]);
    let parsed_non_dict = CryptoServerHandshake::from_pyrep(&srv_with_non_dict_context)
        .expect("non-Dict context falls back cleanly");
    assert_eq!(parsed_non_dict.challenge_response_hash, "md5");
    assert!(parsed_non_dict.context.is_empty());
    assert!(parsed_non_dict.config_vals.is_empty());

    let srv_with_mixed_dicts = PyRep::Tuple(vec![
        PyRep::Bytes(vec![1, 2, 3]),
        PyRep::Tuple(vec![PyRep::Bytes(vec![4, 5]), PyRep::Bool(false)]),
        PyRep::Dict(vec![
            (PyRep::Integer(1), PyRep::Integer(2)),
            (PyRep::String("user_id".into()), PyRep::Integer(42)),
        ]),
        base_resp(PyRep::Dict(vec![
            (PyRep::Integer(9), PyRep::String("skip_non_str".into())),
            (PyRep::String("motd".into()), PyRep::String("welcome".into())),
        ])),
    ]);
    let parsed_mixed = CryptoServerHandshake::from_pyrep(&srv_with_mixed_dicts)
        .expect("mixed key types in context/response/config_vals dicts succeed");
    assert_eq!(parsed_mixed.challenge_response_hash, "md5");
    assert_eq!(parsed_mixed.context.get("user_id"), Some(&PyRep::Integer(42)));
    assert_eq!(
        parsed_mixed.config_vals.get("motd"),
        Some(&PyRep::String("welcome".into()))
    );

    // 4. MachoPacketType::from_i64 full table & unknown rejection
    for code in [0, 1, 2, 3, 6, 7, 8, 10, 11, 12, 15, 16, 18, 20, 21] {
        assert!(MachoPacketType::from_i64(code).is_some());
    }
    assert!(MachoPacketType::from_i64(-1).is_none());
    assert!(MachoPacketType::from_i64(999).is_none());
}
