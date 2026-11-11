use super::*;
use crate::{V24_BOOT_BUILD, V24_MACHO_VERSION};
use std::collections::HashMap;

fn dict(pairs: Vec<(&str, PyRep)>) -> PyRep {
    PyRep::Dict(
        pairs
            .into_iter()
            .map(|(k, v)| (PyRep::String(k.to_string()), v))
            .collect(),
    )
}

fn make_valid_challenge_dict() -> PyRep {
    dict(vec![
        ("macho_version", PyRep::Integer(V24_MACHO_VERSION as i64)),
        ("boot_version", PyRep::Real(24.01)),
        ("boot_build", PyRep::Integer(V24_BOOT_BUILD as i64)),
        ("boot_codename", PyRep::String("V24.01".into())),
        ("boot_region", PyRep::String("ccp".into())),
        ("user_name", PyRep::String("pilot_edge".into())),
        ("user_password", PyRep::String("secret_pw".into())),
        ("user_languageid", PyRep::String("EN".into())),
        ("user_affiliateid", PyRep::Integer(0)),
    ])
}

fn make_test_identity(user_id: u64) -> HandshakeIdentity {
    HandshakeIdentity {
        user_id,
        role: 2,
        language_id: "EN".into(),
        user_type: 30,
        address: "127.0.0.1:0".into(),
        access_token: None,
        computer_hash: None,
    }
}

fn make_version_exchange() -> VersionExchange {
    VersionExchange::default()
}

#[test]
fn gps_transport_closed_error_and_fallback_branches() {
    assert!(GPSTransportClosed::from_pyrep(&PyRep::None).is_none());

    // Object with non-tuple args -> None
    assert!(GPSTransportClosed::from_pyrep(&PyRep::Object {
        type_name: "macho.GPSTransportClosed".into(),
        args: Box::new(PyRep::Integer(1)),
    })
    .is_none());

    // Non-matching class name -> None
    assert!(GPSTransportClosed::from_pyrep(&PyRep::Object {
        type_name: "OtherException".into(),
        args: Box::new(PyRep::Tuple(vec![PyRep::String("reason".into())])),
    })
    .is_none());

    // ObjectEx with non-tuple / short / non-string header -> None
    assert!(GPSTransportClosed::from_pyrep(&PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::None),
        list: vec![],
        dict: vec![],
    })
    .is_none());
    assert!(GPSTransportClosed::from_pyrep(&PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![PyRep::String("GPSTransportClosed".into())])),
        list: vec![],
        dict: vec![],
    })
    .is_none());

    // ObjectEx where items[1] is not a Tuple -> fallback uses outer tuple items (so items[0] is type_name)
    let fallback_ex = GPSTransportClosed::from_pyrep(&PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::String("GPSRemoteTransportClosed".into()),
            PyRep::String("E_CODE".into()),
        ])),
        list: vec![],
        dict: vec![],
    })
    .unwrap();
    assert_eq!(fallback_ex.reason, "GPSRemoteTransportClosed");
    assert_eq!(fallback_ex.reason_code.as_deref(), Some("E_CODE"));

    // ObjectEx with Tuple items[1] and non-string key in reason_args Dict
    let valid_ex = GPSTransportClosed::from_pyrep(&PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::String("macho.GPSTransportClosed".into()),
            PyRep::Tuple(vec![
                PyRep::String("idle".into()),
                PyRep::String("E_IDLE".into()),
                PyRep::Dict(vec![
                    (PyRep::Integer(1), PyRep::String("ignored".into())),
                    (PyRep::String("k".into()), PyRep::Integer(42)),
                ]),
            ]),
        ])),
        list: vec![],
        dict: vec![],
    })
    .unwrap();
    assert_eq!(valid_ex.reason, "idle");
    assert_eq!(valid_ex.reason_code.as_deref(), Some("E_IDLE"));
    assert_eq!(valid_ex.reason_args.get("k"), Some(&PyRep::Integer(42)));
    assert_eq!(valid_ex.reason_args.len(), 1);
}

#[test]
fn crypto_request_packet_and_command_edges() {
    assert!(CryptoRequestPacket::from_pyrep(&PyRep::Integer(1)).is_none());
    assert!(
        CryptoRequestPacket::from_pyrep(&PyRep::Tuple(vec![PyRep::String("1".into())])).is_none()
    );
    // Non-string key_version
    assert!(CryptoRequestPacket::from_pyrep(&PyRep::Tuple(vec![
        PyRep::Integer(99),
        dict(vec![]),
    ]))
    .is_none());
    // Non-dict params
    assert!(CryptoRequestPacket::from_pyrep(&PyRep::Tuple(vec![
        PyRep::String("placebo".into()),
        PyRep::List(vec![]),
    ]))
    .is_none());

    // Non-string dict keys are skipped gracefully
    let rep = PyRep::Tuple(vec![
        PyRep::String("placebo".into()),
        PyRep::Dict(vec![
            (PyRep::Integer(42), PyRep::String("skip".into())),
            (PyRep::String("machoVersion".into()), PyRep::Integer(404)),
        ]),
    ]);
    let req = CryptoRequestPacket::from_pyrep(&rep).unwrap();
    assert_eq!(req.key_version, "placebo");
    assert!(req.key_params.contains_key("machoVersion"));

    // HandshakeCommand edge shapes
    assert!(HandshakeCommand::from_pyrep(&PyRep::None).is_none());
    assert!(HandshakeCommand::from_pyrep(&PyRep::Tuple(vec![PyRep::None])).is_none());
    assert!(HandshakeCommand::from_pyrep(&PyRep::Tuple(vec![
        PyRep::None,
        PyRep::Integer(5),
    ]))
    .is_none());
    assert!(HandshakeCommand::from_pyrep(&PyRep::Tuple(vec![
        PyRep::None,
        PyRep::String("UNKNOWN_CMD".into()),
    ]))
    .is_none());
    assert!(HandshakeCommand::from_pyrep(&PyRep::Tuple(vec![
        PyRep::None,
        PyRep::String("VK".into()),
    ]))
    .is_none());

    let vk = HandshakeCommand::from_pyrep(&PyRep::Tuple(vec![
        PyRep::None,
        PyRep::String("VK".into()),
        PyRep::String("vip_key_1".into()),
    ]))
    .unwrap();
    assert!(matches!(vk, HandshakeCommand::VipKey(ref k) if k == "vip_key_1"));

    let qc = HandshakeCommand::from_pyrep(&PyRep::Tuple(vec![
        PyRep::None,
        PyRep::String("QC".into()),
    ]))
    .unwrap();
    assert!(matches!(qc, HandshakeCommand::QueueCheck));
}

#[test]
fn crypto_challenge_packet_missing_keys_and_extract_password_edges() {
    assert!(CryptoChallengePacket::from_pyrep(&PyRep::None).is_none());
    assert!(CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![PyRep::None])).is_none());

    assert!(CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![
        PyRep::Integer(123),
        make_valid_challenge_dict(),
    ]))
    .is_none());

    // Buffer challenge is accepted
    let buf_pkt = CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![
        PyRep::Buffer(b"buf_chal".to_vec()),
        make_valid_challenge_dict(),
    ]))
    .unwrap();
    assert_eq!(buf_pkt.client_challenge, b"buf_chal");

    // Non-dict second element
    assert!(CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![
        PyRep::String("chal".into()),
        PyRep::List(vec![]),
    ]))
    .is_none());

    let base_pairs = match make_valid_challenge_dict() {
        PyRep::Dict(p) => p,
        _ => unreachable!(),
    };
    for required_key in [
        "macho_version",
        "boot_version",
        "boot_build",
        "boot_codename",
        "boot_region",
    ] {
        let filtered: Vec<_> = base_pairs
            .iter()
            .filter(|(k, _)| k != &PyRep::String(required_key.into()))
            .cloned()
            .collect();
        assert!(
            CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![
                PyRep::String("chal".into()),
                PyRep::Dict(filtered),
            ]))
            .is_none(),
            "expected None when missing {required_key}"
        );
    }

    // Invalid boot_version type
    let mut bad_boot = base_pairs.clone();
    for (k, v) in &mut bad_boot {
        if k == &PyRep::String("boot_version".into()) {
            *v = PyRep::String("bad".into());
        }
    }
    assert!(CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![
        PyRep::String("chal".into()),
        PyRep::Dict(bad_boot),
    ]))
    .is_none());

    // Integer boot_version is converted
    let mut int_boot = base_pairs;
    for (k, v) in &mut int_boot {
        if k == &PyRep::String("boot_version".into()) {
            *v = PyRep::Integer(19);
        }
    }
    let parsed = CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![
        PyRep::String("chal".into()),
        PyRep::Dict(int_boot),
    ]))
    .unwrap();
    assert!((parsed.boot_version - 19.0).abs() < f64::EPSILON);

    // extract_password_str edge cases
    assert_eq!(
        extract_password_str(&PyRep::Buffer(b"pw_buf".to_vec())).as_deref(),
        Some("pw_buf")
    );
    assert_eq!(extract_password_str(&PyRep::Buffer(vec![0xff, 0xfe])), None);
    assert_eq!(
        extract_password_str(&PyRep::Object {
            type_name: "util.PasswordString".into(),
            args: Box::new(PyRep::Tuple(vec![PyRep::String("obj_pw".into())])),
        })
        .as_deref(),
        Some("obj_pw")
    );
    assert_eq!(
        extract_password_str(&PyRep::Object {
            type_name: "util.PasswordString".into(),
            args: Box::new(PyRep::Integer(1)),
        }),
        None
    );
    assert_eq!(
        extract_password_str(&PyRep::ObjectEx {
            is_type_2: false,
            header: Box::new(PyRep::Integer(5)),
            list: vec![],
            dict: vec![],
        }),
        None
    );
    assert_eq!(
        extract_password_str(&PyRep::ObjectEx {
            is_type_2: true,
            header: Box::new(PyRep::Tuple(vec![
                PyRep::String("util.PasswordString".into()),
                PyRep::Tuple(vec![PyRep::Buffer(b"ex_pw".to_vec())]),
            ])),
            list: vec![],
            dict: vec![],
        })
        .as_deref(),
        Some("ex_pw")
    );
    assert_eq!(extract_password_str(&PyRep::None), None);
}

fn make_server_handshake_rep(
    challenge: PyRep,
    func_tuple: PyRep,
    context: PyRep,
    resp: PyRep,
) -> PyRep {
    PyRep::Tuple(vec![challenge, func_tuple, context, resp])
}

fn valid_server_resp_dict() -> PyRep {
    dict(vec![
        ("macho_version", PyRep::Integer(404)),
        ("boot_version", PyRep::Real(18.04)),
        ("boot_build", PyRep::Integer(1700000)),
        ("boot_codename", PyRep::String("EVE".into())),
        ("boot_region", PyRep::String("ccp".into())),
        ("challenge_responsehash", PyRep::String("abc".into())),
        ("cluster_usercount", PyRep::Integer(5)),
        ("proxy_nodeid", PyRep::Integer(1001)),
        ("user_logonqueueposition", PyRep::Integer(0)),
        ("config_vals", dict(vec![("foo", PyRep::Integer(1))])),
    ])
}

#[test]
fn crypto_server_handshake_from_pyrep_edges() {
    assert!(CryptoServerHandshake::from_pyrep(&PyRep::None).is_none());
    assert!(CryptoServerHandshake::from_pyrep(&PyRep::Tuple(vec![PyRep::None])).is_none());

    let valid_func = PyRep::Tuple(vec![PyRep::String("code".into()), PyRep::Bool(true)]);
    let valid_ctx = dict(vec![("a", PyRep::Integer(1))]);

    // Invalid challenge
    assert!(CryptoServerHandshake::from_pyrep(&make_server_handshake_rep(
        PyRep::Integer(1),
        valid_func.clone(),
        valid_ctx.clone(),
        valid_server_resp_dict(),
    ))
    .is_none());

    // Non-tuple func_tuple / wrong len / bad func_code
    assert!(CryptoServerHandshake::from_pyrep(&make_server_handshake_rep(
        PyRep::Buffer(b"c".to_vec()),
        PyRep::Integer(1),
        valid_ctx.clone(),
        valid_server_resp_dict(),
    ))
    .is_none());
    assert!(CryptoServerHandshake::from_pyrep(&make_server_handshake_rep(
        PyRep::Buffer(b"c".to_vec()),
        PyRep::Tuple(vec![PyRep::String("only1".into())]),
        valid_ctx.clone(),
        valid_server_resp_dict(),
    ))
    .is_none());
    assert!(CryptoServerHandshake::from_pyrep(&make_server_handshake_rep(
        PyRep::Buffer(b"c".to_vec()),
        PyRep::Tuple(vec![PyRep::Integer(1), PyRep::None]),
        valid_ctx.clone(),
        valid_server_resp_dict(),
    ))
    .is_none());

    // Non-dict resp
    assert!(CryptoServerHandshake::from_pyrep(&make_server_handshake_rep(
        PyRep::Buffer(b"c".to_vec()),
        valid_func.clone(),
        valid_ctx.clone(),
        PyRep::List(vec![]),
    ))
    .is_none());

    // Buffer challenge + Buffer func_code + Integer verification + Integer boot_version + defaults + non-dict config_vals
    let minimal_resp = dict(vec![
        ("macho_version", PyRep::Integer(404)),
        ("boot_version", PyRep::Integer(18)),
        ("boot_build", PyRep::Integer(1700000)),
        ("boot_codename", PyRep::String("EVE".into())),
        ("boot_region", PyRep::String("ccp".into())),
        ("challenge_responsehash", PyRep::String("abc".into())),
        ("config_vals", PyRep::Integer(0)),
    ]);
    let parsed = CryptoServerHandshake::from_pyrep(&make_server_handshake_rep(
        PyRep::Buffer(b"chal_b".to_vec()),
        PyRep::Tuple(vec![PyRep::Buffer(b"fc".to_vec()), PyRep::Integer(1)]),
        PyRep::None,
        minimal_resp,
    ))
    .unwrap();
    assert_eq!(parsed.server_challenge, b"chal_b");
    assert_eq!(parsed.func_marshaled_code, b"fc");
    assert!(parsed.verification);
    assert_eq!(parsed.cluster_user_count, 1);
    assert_eq!(parsed.proxy_node_id, 888444);
    assert!(parsed.config_vals.is_empty());

    let resp_pairs = match valid_server_resp_dict() {
        PyRep::Dict(p) => p,
        _ => unreachable!(),
    };
    for req_key in [
        "macho_version",
        "boot_version",
        "boot_build",
        "boot_codename",
        "boot_region",
        "challenge_responsehash",
    ] {
        let filtered: Vec<_> = resp_pairs
            .iter()
            .filter(|(k, _)| k != &PyRep::String(req_key.into()))
            .cloned()
            .collect();
        assert!(
            CryptoServerHandshake::from_pyrep(&make_server_handshake_rep(
                PyRep::String("c".into()),
                valid_func.clone(),
                valid_ctx.clone(),
                PyRep::Dict(filtered),
            ))
            .is_none(),
            "expected None when missing {req_key}"
        );
    }

    // Bad boot_version in resp
    let mut bad_bv = resp_pairs;
    for (k, v) in &mut bad_bv {
        if k == &PyRep::String("boot_version".into()) {
            *v = PyRep::String("not_num".into());
        }
    }
    assert!(CryptoServerHandshake::from_pyrep(&make_server_handshake_rep(
        PyRep::String("c".into()),
        valid_func,
        valid_ctx,
        PyRep::Dict(bad_bv),
    ))
    .is_none());
}

#[test]
fn crypto_handshake_result_and_ack_edges() {
    assert!(CryptoHandshakeResult::from_pyrep(&PyRep::None).is_none());
    assert!(CryptoHandshakeResult::from_pyrep(&PyRep::Tuple(vec![
        PyRep::Integer(1),
        PyRep::None,
        PyRep::None,
    ]))
    .is_none());

    let res_buf = CryptoHandshakeResult::from_pyrep(&PyRep::Tuple(vec![
        PyRep::String("hash_ok".into()),
        PyRep::Buffer(b"out_buf".to_vec()),
        PyRep::None,
    ]))
    .unwrap();
    assert_eq!(res_buf.func_output, b"out_buf");

    let res_none = CryptoHandshakeResult::from_pyrep(&PyRep::Tuple(vec![
        PyRep::String("hash_ok".into()),
        PyRep::None,
        PyRep::None,
    ]))
    .unwrap();
    assert!(res_none.func_output.is_empty());

    // Ack missing required keys & optional defaults
    assert!(CryptoHandshakeAck::from_pyrep(&PyRep::None).is_none());
    let ack_full = dict(vec![
        ("sessionID", PyRep::Integer(999)),
        ("live_updates", PyRep::Integer(99)), // non-list -> empty
        (
            "session_init",
            dict(vec![
                ("userid", PyRep::Integer(77)),
                ("role", PyRep::Integer(2)),
                ("inDetention", PyRep::None),
            ]),
        ),
        ("client_hash", PyRep::None),
        ("user_clientid", PyRep::Integer(1001)),
    ]);
    let parsed = CryptoHandshakeAck::from_pyrep(&ack_full).unwrap();
    assert!(parsed.live_updates.is_empty());
    assert_eq!(parsed.language_id, "EN");
    assert_eq!(parsed.user_type, 30);
    assert_eq!(parsed.address, "");
    assert!(parsed.in_detention.is_none());

    // Non-dict session_init
    assert!(CryptoHandshakeAck::from_pyrep(&dict(vec![
        ("sessionID", PyRep::Integer(1)),
        ("user_clientid", PyRep::Integer(2)),
        ("session_init", PyRep::None),
    ]))
    .is_none());

    let ack_pairs = match ack_full {
        PyRep::Dict(p) => p,
        _ => unreachable!(),
    };
    for req_key in ["sessionID", "user_clientid", "session_init"] {
        let filtered: Vec<_> = ack_pairs
            .iter()
            .filter(|(k, _)| k != &PyRep::String(req_key.into()))
            .cloned()
            .collect();
        assert!(
            CryptoHandshakeAck::from_pyrep(&PyRep::Dict(filtered)).is_none(),
            "expected None when missing {req_key}"
        );
    }
}

#[test]
fn state_machine_edge_transitions() {
    let ver = make_version_exchange();

    // PendingResult with missing pending identity -> Err & Failed phase
    let mut sm = MachoHandshakeStateMachine::new(ver.clone(), 1001, 500, 1001);
    assert!(matches!(
        sm.step(&ver.to_server_hello_pyrep()),
        Ok(HandshakeStepOutput::SendFramesAndContinue(_))
    ));
    assert!(matches!(
        sm.step(&CryptoRequestPacket::placebo().to_pyrep()),
        Ok(HandshakeStepOutput::SendFramesAndContinue(_))
    ));
    let chal = CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![
        PyRep::String("client_chal".into()),
        make_valid_challenge_dict(),
    ]))
    .unwrap();
    let step_auth = sm.step(&chal.to_pyrep()).unwrap();
    assert!(matches!(step_auth, HandshakeStepOutput::NeedsAuthentication(_)));
    let _frames = sm
        .complete_challenge(&chal, Ok(make_test_identity(77)), 1)
        .unwrap();
    // Force missing pending_identity before result arrives
    let expected_hash = placebo_crypto_hash(&sm.pending_server_challenge);
    sm.set_pending_identity_for_test(None);
    let err = sm
        .step(&PyRep::Tuple(vec![
            PyRep::String(expected_hash),
            PyRep::None,
            PyRep::None,
        ]))
        .unwrap_err();
    assert_eq!(err.reason_code.as_deref(), Some("HANDSHAKE_FAILEDVERIFICATION"));
    assert!(matches!(sm.phase, HandshakePhase::Failed(_)));

    // Further steps while Failed return HANDSHAKE_FAILED
    let after_fail = sm.step(&PyRep::None).unwrap_err();
    assert_eq!(after_fail.reason_code.as_deref(), Some("HANDSHAKE_FAILED"));

    // QueueCheck resets back to AwaitingVersionExchange; VipKey stores key;
    // password_version_1 retry; SSO bypass; complete; post-Authenticated error.
    let mut sm2 = MachoHandshakeStateMachine::new(ver.clone(), 1002, 600, 1002);
    let mut cfg = HashMap::new();
    cfg.insert("sessionTimeout".to_string(), PyRep::Integer(300));
    sm2.set_config_vals(cfg);

    sm2.step(&ver.to_server_hello_pyrep()).unwrap();
    let qc_out = sm2
        .step(&PyRep::Tuple(vec![PyRep::None, PyRep::String("QC".into())]))
        .unwrap();
    assert!(matches!(qc_out, HandshakeStepOutput::SendFramesAndContinue(ref v) if v.len() == 2));
    assert_eq!(sm2.phase, HandshakePhase::AwaitingVersionExchange);

    sm2.step(&ver.to_server_hello_pyrep()).unwrap();
    let vk_out = sm2
        .step(&PyRep::Tuple(vec![
            PyRep::None,
            PyRep::String("VK".into()),
            PyRep::String("vip-key-1".into()),
        ]))
        .unwrap();
    assert!(matches!(vk_out, HandshakeStepOutput::SendFramesAndContinue(ref v) if v.is_empty()));
    assert_eq!(sm2.vip_key.as_deref(), Some("vip-key-1"));

    // Non-placebo key_version fails
    let mut sm_bad_key = sm2.clone();
    let bad_key_err = sm_bad_key
        .step(&PyRep::Tuple(vec![
            PyRep::String("rsa".into()),
            dict(vec![]),
        ]))
        .unwrap_err();
    assert_eq!(
        bad_key_err.reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEPUBLICKEY")
    );

    // CryptoAPI crypting_sessionkey fails
    let mut sm_cryptoapi = sm2.clone();
    let api_err = sm_cryptoapi
        .step(&PyRep::Tuple(vec![
            PyRep::String("placebo".into()),
            dict(vec![("crypting_sessionkey", PyRep::Bytes(vec![1, 2, 3]))]),
        ]))
        .unwrap_err();
    assert_eq!(
        api_err.reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEPUBLICKEY")
    );

    sm2.step(&CryptoRequestPacket::placebo().to_pyrep()).unwrap();

    // Attempt 1 with user_name = "pilot_edge" and no password/sso_token -> returns [1] and sets flag
    let no_pw_pairs: Vec<_> = match make_valid_challenge_dict() {
        PyRep::Dict(p) => p
            .into_iter()
            .filter(|(k, _)| k != &PyRep::String("user_password".into()))
            .collect(),
        _ => unreachable!(),
    };
    let retry = sm2
        .step(&PyRep::Tuple(vec![
            PyRep::String("chal1".into()),
            PyRep::Dict(no_pw_pairs.clone()),
        ]))
        .unwrap();
    assert!(matches!(retry, HandshakeStepOutput::SendFramesAndContinue(ref v) if v == &vec![PyRep::Integer(1)]));

    // Attempt 2 even with SSO username proceeds to NeedsAuthentication because password_version_1_requested=true
    let sso_pairs: Vec<_> = no_pw_pairs
        .into_iter()
        .map(|(k, v)| {
            if k == PyRep::String("user_name".into()) {
                (k, PyRep::String("EVE-SSO-CONNECTION".into()))
            } else {
                (k, v)
            }
        })
        .collect();
    let needs = sm2
        .step(&PyRep::Tuple(vec![
            PyRep::String("chal2".into()),
            PyRep::Dict(sso_pairs),
        ]))
        .unwrap();
    let chal2 = match needs {
        HandshakeStepOutput::NeedsAuthentication(c) => c,
        other => panic!("unexpected: {other:?}"),
    };
    let hs_frames = sm2
        .complete_challenge(&chal2, Ok(make_test_identity(88)), 3)
        .unwrap();
    // Because password_version_1_requested is true, only [server_hs] (len 1) is returned!
    assert_eq!(hs_frames.len(), 1);

    let ok_hash = placebo_crypto_hash(&sm2.pending_server_challenge);
    let authed = sm2
        .step(&PyRep::Tuple(vec![
            PyRep::String(ok_hash),
            PyRep::None,
            PyRep::None,
        ]))
        .unwrap();
    assert!(matches!(authed, HandshakeStepOutput::Authenticated { .. }));
    assert!(sm2.is_authenticated());

    // Subsequent step when Authenticated fails cleanly
    let already = sm2.step(&PyRep::None).unwrap_err();
    assert_eq!(already.reason_code.as_deref(), Some("HANDSHAKE_ALREADY_COMPLETE"));
}
