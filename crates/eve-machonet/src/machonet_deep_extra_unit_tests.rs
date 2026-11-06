//! Deep branch-coverage and mutation-killing unit tests for `eve-machonet`.

use super::*;

#[test]
fn address_packet_version_exchange_and_call_extraction_cover_all_fallback_branches() {
    // 1. MachoAddress non-tuple Object/ObjectEx and Broadcast non-list narrowcast
    let bad_obj = PyRep::Object {
        type_name: MACHO_ADDRESS_CLASS.into(),
        args: Box::new(PyRep::Integer(1)),
    };
    assert!(MachoAddress::from_pyrep(&bad_obj).is_none());

    let bad_ex = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Integer(2)),
        list: Vec::new(),
        dict: Vec::new(),
    };
    assert!(MachoAddress::from_pyrep(&bad_ex).is_none());

    let bcast_non_list = PyRep::Tuple(vec![
        PyRep::Integer(ADDRESS_TYPE_BROADCAST),
        PyRep::String("OnSessionChanged".into()),
        PyRep::Integer(999),
        PyRep::String("charid".into()),
    ]);
    assert_eq!(
        MachoAddress::from_pyrep(&bcast_non_list),
        Some(MachoAddress::Broadcast {
            broadcast_id: "OnSessionChanged".into(),
            id_type: "charid".into(),
            narrowcast: Vec::new(),
        })
    );

    // 2. MachoPacket Object non-tuple and ObjectEx nested/flat/malformed headers
    let pkt = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id: 77,
            call_id: 9,
            service: Some("skillMgr2".into()),
        },
        destination: MachoAddress::Node {
            node_id: 1,
            service: Some("skillMgr2".into()),
            call_id: 9,
        },
        user_id: None,
        payload: PyRep::None,
        named_payload: Default::default(),
    };
    let expected_rep = pkt.to_pyrep();
    let PyRep::Object { args, .. } = &expected_rep else {
        panic!("expected PyRep::Object");
    };
    let PyRep::Tuple(pkt_fields) = args.as_ref() else {
        panic!("expected tuple fields");
    };

    assert!(MachoPacket::from_pyrep(&PyRep::Object {
        type_name: "machoNetPacket.CallReq".into(),
        args: Box::new(PyRep::None),
    })
    .is_none());

    let ex_nested = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("machoNetPacket.CallReq".into()),
            PyRep::Tuple(pkt_fields.clone()),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    assert_eq!(
        MachoPacket::from_pyrep(&ex_nested).map(|p| p.to_pyrep()),
        Some(expected_rep.clone())
    );

    let ex_flat = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(pkt_fields.clone())),
        list: Vec::new(),
        dict: Vec::new(),
    };
    assert_eq!(
        MachoPacket::from_pyrep(&ex_flat).map(|p| p.to_pyrep()),
        Some(expected_rep)
    );

    let ex_short = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![PyRep::Integer(6)])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    assert!(MachoPacket::from_pyrep(&ex_short).is_none());

    // 3. extract_broadcast_notification failure guards at every nesting layer
    let mut bad_notif =
        MachoPacket::new_broadcast_notification(1, 77, None, "OnSessionChanged", vec![]);
    bad_notif.payload = PyRep::Tuple(vec![PyRep::Integer(1)]);
    assert!(bad_notif.extract_broadcast_notification().is_none());

    bad_notif.payload = PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::None, PyRep::Integer(2)])]);
    assert!(bad_notif.extract_broadcast_notification().is_none());

    bad_notif.payload = PyRep::Tuple(vec![PyRep::Tuple(vec![
        PyRep::None,
        PyRep::SubStream(Box::new(PyRep::Integer(3))),
    ])]);
    assert!(bad_notif.extract_broadcast_notification().is_none());

    bad_notif.payload = PyRep::Tuple(vec![PyRep::Tuple(vec![
        PyRep::None,
        PyRep::SubStream(Box::new(PyRep::Tuple(vec![PyRep::None, PyRep::Integer(4)]))),
    ])]);
    assert!(bad_notif.extract_broadcast_notification().is_none());

    bad_notif.payload = PyRep::Tuple(vec![PyRep::Tuple(vec![
        PyRep::None,
        PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::None,
            PyRep::Tuple(vec![PyRep::None, PyRep::Integer(5)]),
        ]))),
    ])]);
    assert!(bad_notif.extract_broadcast_notification().is_none());

    // 4. VersionExchange server hello + None user_count + Integer version_number + error branches
    let vx = VersionExchange::default();
    let PyRep::Tuple(mut srv_items) = vx.to_server_hello_pyrep() else {
        panic!("expected tuple");
    };
    assert_eq!(srv_items.len(), 7);
    assert_eq!(srv_items[6], PyRep::None);
    srv_items[2] = PyRep::None;
    srv_items[3] = PyRep::Integer(25);
    let parsed_vx = VersionExchange::from_pyrep(&PyRep::Tuple(srv_items.clone())).unwrap();
    assert_eq!(parsed_vx.user_count, 0);
    assert!((parsed_vx.version_number - 25.0).abs() < 1e-9);

    let mut bad_uc = srv_items.clone();
    bad_uc[2] = PyRep::String("bad".into());
    assert!(VersionExchange::from_pyrep(&PyRep::Tuple(bad_uc)).is_none());

    let mut bad_ver = srv_items;
    bad_ver[3] = PyRep::None;
    assert!(VersionExchange::from_pyrep(&PyRep::Tuple(bad_ver)).is_none());

    // 5. extract_macho_call_req SubStream 3-tuple and 2-tuple SubStream forms
    let direct_call = PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::Tuple(vec![
        PyRep::String("DirectPing".into()),
        PyRep::Tuple(vec![PyRep::Integer(123)]),
        PyRep::Dict(vec![(PyRep::String("flag".into()), PyRep::Bool(true))]),
    ])))]);
    let (oid, m, cargs, ckw) = extract_macho_call_req(&direct_call);
    assert_eq!(oid, None);
    assert_eq!(m, "DirectPing");
    assert_eq!(cargs, vec![PyRep::Integer(123)]);
    assert_eq!(ckw.get("flag"), Some(&PyRep::Bool(true)));

    let wrapped_2tuple = PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::Tuple(vec![
        PyRep::Integer(88),
        PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::None,
            PyRep::String("BoundInner".into()),
            PyRep::List(vec![PyRep::String("arg0".into())]),
            PyRep::Dict(Vec::new()),
        ]))),
    ])))]);
    let (oid2, m2, cargs2, _) = extract_macho_call_req(&wrapped_2tuple);
    assert_eq!(oid2, None);
    assert_eq!(m2, "BoundInner");
    assert_eq!(cargs2, vec![PyRep::String("arg0".into())]);
}

#[test]
fn session_diff_space_transitions_and_handshake_edge_cases_work() {
    // 1. SessionState compute_diff across in-space jump, dock, undock, and address change
    let mut s_space_a = SessionState::new(901);
    s_space_a.user_id = Some(10);
    s_space_a.character_id = Some(20);
    s_space_a.solar_system_id = Some(30000142);
    s_space_a.station_id = None;
    s_space_a.address = "10.0.0.1:5000".into();

    let mut s_space_b = s_space_a.clone();
    s_space_b.solar_system_id = Some(30002187);
    s_space_b.address = "10.0.0.2:5001".into();
    let diff_jump = s_space_a.compute_diff(&s_space_b);
    assert_eq!(
        diff_jump.get("solarsystemid"),
        Some(&(PyRep::Integer(30000142), PyRep::Integer(30002187)))
    );
    assert_eq!(
        diff_jump.get("solarsystemid2"),
        Some(&(PyRep::Integer(30000142), PyRep::Integer(30002187)))
    );
    assert_eq!(
        diff_jump.get("address"),
        Some(&(
            PyRep::ascii_bytes("10.0.0.1:5000"),
            PyRep::ascii_bytes("10.0.0.2:5001")
        ))
    );

    let mut s_docked = s_space_a.clone();
    s_docked.station_id = Some(60003760);
    let diff_dock = s_space_a.compute_diff(&s_docked);
    assert_eq!(
        diff_dock.get("solarsystemid"),
        Some(&(PyRep::Integer(30000142), PyRep::None))
    );

    let diff_undock = s_docked.compute_diff(&s_space_a);
    assert_eq!(
        diff_undock.get("solarsystemid"),
        Some(&(PyRep::None, PyRep::Integer(30000142)))
    );

    // 2. CryptoChallengePacket malformed variants
    assert!(CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![PyRep::None])).is_none());
    assert!(CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![
        PyRep::Integer(7),
        PyRep::Dict(Vec::new()),
    ]))
    .is_none());
    assert!(CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![
        PyRep::String("chal".into()),
        PyRep::None,
    ]))
    .is_none());
    assert!(CryptoChallengePacket::from_pyrep(&PyRep::Tuple(vec![
        PyRep::String("chal".into()),
        PyRep::Dict(vec![
            (PyRep::String("macho_version".into()), PyRep::Integer(322)),
            (PyRep::String("boot_version".into()), PyRep::None),
            (PyRep::String("boot_build".into()), PyRep::Integer(1)),
            (PyRep::String("boot_codename".into()), PyRep::String("EVE".into())),
            (PyRep::String("boot_region".into()), PyRep::String("trq".into())),
        ]),
    ]))
    .is_none());

    // 3. CryptoServerHandshake string/integer/error variants
    let good_resp_dict = |boot_ver: PyRep| {
        PyRep::Dict(vec![
            (
                PyRep::String("challenge_responsehash".into()),
                PyRep::String("hash_ok".into()),
            ),
            (PyRep::String("macho_version".into()), PyRep::Integer(322)),
            (PyRep::String("boot_version".into()), boot_ver),
            (PyRep::String("boot_build".into()), PyRep::Integer(2401)),
            (PyRep::String("boot_codename".into()), PyRep::String("EVE-trq".into())),
            (PyRep::String("boot_region".into()), PyRep::String("trq".into())),
            (PyRep::String("cluster_usercount".into()), PyRep::Integer(42)),
            (PyRep::String("proxy_nodeid".into()), PyRep::Integer(777)),
            (PyRep::String("user_logonqueueposition".into()), PyRep::Integer(3)),
            (
                PyRep::String("config_vals".into()),
                PyRep::Dict(vec![(PyRep::String("cfg_k".into()), PyRep::Bool(true))]),
            ),
        ])
    };
    let parsed_rsp = CryptoServerHandshake::from_pyrep(&PyRep::Tuple(vec![
        PyRep::String("srv_chal".into()),
        PyRep::Tuple(vec![PyRep::String("func_code".into()), PyRep::Integer(1)]),
        PyRep::Dict(vec![(PyRep::String("ctx_k".into()), PyRep::Integer(9))]),
        good_resp_dict(PyRep::Integer(24)),
    ]))
    .unwrap();
    assert_eq!(parsed_rsp.server_challenge, b"srv_chal");
    assert_eq!(parsed_rsp.func_marshaled_code, b"func_code");
    assert!(parsed_rsp.verification);
    assert!((parsed_rsp.boot_version - 24.0).abs() < 1e-9);
    assert_eq!(parsed_rsp.cluster_user_count, 42);
    assert_eq!(parsed_rsp.proxy_node_id, 777);
    assert_eq!(parsed_rsp.user_logon_queue_position, 3);
    assert_eq!(parsed_rsp.context.get("ctx_k"), Some(&PyRep::Integer(9)));
    assert_eq!(parsed_rsp.config_vals.get("cfg_k"), Some(&PyRep::Bool(true)));

    let unverified = CryptoServerHandshake::from_pyrep(&PyRep::Tuple(vec![
        PyRep::Bytes(vec![1, 2]),
        PyRep::Tuple(vec![PyRep::Bytes(vec![3]), PyRep::None]),
        PyRep::Dict(Vec::new()),
        good_resp_dict(PyRep::Real(24.0)),
    ]))
    .unwrap();
    assert!(!unverified.verification);

    for bad_srv in [
        vec![PyRep::Integer(1), PyRep::None, PyRep::Dict(Vec::new()), good_resp_dict(PyRep::Integer(24))],
        vec![PyRep::Bytes(vec![1]), PyRep::None, PyRep::Dict(Vec::new()), good_resp_dict(PyRep::Integer(24))],
        vec![PyRep::Bytes(vec![1]), PyRep::Tuple(vec![PyRep::Bytes(vec![2])]), PyRep::Dict(Vec::new()), good_resp_dict(PyRep::Integer(24))],
        vec![PyRep::Bytes(vec![1]), PyRep::Tuple(vec![PyRep::Integer(2), PyRep::Bool(true)]), PyRep::Dict(Vec::new()), good_resp_dict(PyRep::Integer(24))],
        vec![PyRep::Bytes(vec![1]), PyRep::Tuple(vec![PyRep::Bytes(vec![2]), PyRep::Bool(true)]), PyRep::Dict(Vec::new()), PyRep::None],
        vec![PyRep::Bytes(vec![1]), PyRep::Tuple(vec![PyRep::Bytes(vec![2]), PyRep::Bool(true)]), PyRep::Dict(Vec::new()), good_resp_dict(PyRep::None)],
    ] {
        assert!(CryptoServerHandshake::from_pyrep(&PyRep::Tuple(bad_srv)).is_none());
    }

    // 4. CryptoHandshakeResult & CryptoHandshakeAck & MachoHandshakeStateMachine transitions
    let hr = CryptoHandshakeResult::from_pyrep(&PyRep::Tuple(vec![
        PyRep::String("deadbeef".into()),
        PyRep::String("out_str".into()),
        PyRep::Integer(0),
    ]))
    .unwrap();
    assert_eq!(hr.func_output, b"out_str");

    let hr_empty = CryptoHandshakeResult::from_pyrep(&PyRep::Tuple(vec![
        PyRep::String("deadbeef".into()),
        PyRep::None,
        PyRep::Integer(1),
    ]))
    .unwrap();
    assert!(hr_empty.func_output.is_empty());

    let identity = HandshakeIdentity {
        user_id: 501,
        role: 2,
        language_id: "EN".into(),
        user_type: 30,
        address: "127.0.0.1".into(),
        access_token: Some("tok".into()),
        computer_hash: Some("comp_hash_xyz".into()),
    };
    let ack = CryptoHandshakeAck::from_identity(12, 34, &identity);
    let mut ack_rep = ack.to_pyrep();
    let roundtrip = CryptoHandshakeAck::from_pyrep(&ack_rep).unwrap();
    assert_eq!(roundtrip.computer_hash.as_deref(), Some("comp_hash_xyz"));

    if let PyRep::Dict(ref mut entries) = ack_rep {
        entries.retain(|(k, _)| k.as_str() != Some("live_updates"));
        entries.push((PyRep::String("live_updates".into()), PyRep::Integer(0)));
        let no_live = CryptoHandshakeAck::from_pyrep(&PyRep::Dict(entries.clone())).unwrap();
        assert!(no_live.live_updates.is_empty());

        entries.retain(|(k, _)| k.as_str() != Some("session_init"));
        entries.push((PyRep::String("session_init".into()), PyRep::None));
        assert!(CryptoHandshakeAck::from_pyrep(&PyRep::Dict(entries.clone())).is_none());
    }

    let mut machine = MachoHandshakeStateMachine::new(VersionExchange::default(), 1, 11, 22);
    let closed_err = machine
        .complete_challenge(
            &CryptoChallengePacket::new_password_login("u", "p"),
            Err(GPSTransportClosed::new("Denied", "AUTH_FAILED")),
            5,
        )
        .unwrap_err();
    assert_eq!(closed_err.reason_code.as_deref(), Some("AUTH_FAILED"));
    assert!(matches!(machine.phase, HandshakePhase::Failed(_)));
}
