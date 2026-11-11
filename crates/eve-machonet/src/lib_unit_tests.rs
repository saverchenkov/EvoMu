//! Unit and mutation-killing tests for `eve_machonet`.

use super::*;
use eve_proto::marshal;

#[test]
fn macho_packet_type_and_class_name_cover_all_variants() {
    let cases = [
        (0, MachoPacketType::AuthenticationReq, "carbon.common.script.net.machoNetPacket.AuthenticationReq"),
        (1, MachoPacketType::AuthenticationRsp, "carbon.common.script.net.machoNetPacket.AuthenticationRsp"),
        (2, MachoPacketType::IdentificationReq, "carbon.common.script.net.machoNetPacket.IdentificationReq"),
        (3, MachoPacketType::IdentificationRsp, "carbon.common.script.net.machoNetPacket.IdentificationRsp"),
        (6, MachoPacketType::CallReq, "carbon.common.script.net.machoNetPacket.CallReq"),
        (7, MachoPacketType::CallRsp, "carbon.common.script.net.machoNetPacket.CallRsp"),
        (8, MachoPacketType::TransportClosed, "carbon.common.script.net.machoNetPacket.TransportClosed"),
        (10, MachoPacketType::ResolveReq, "carbon.common.script.net.machoNetPacket.ResolveReq"),
        (11, MachoPacketType::ResolveRsp, "carbon.common.script.net.machoNetPacket.ResolveRsp"),
        (12, MachoPacketType::Notification, "carbon.common.script.net.machoNetPacket.Notification"),
        (15, MachoPacketType::ErrorResponse, "carbon.common.script.net.machoNetPacket.ErrorResponse"),
        (16, MachoPacketType::SessionChangeNotification, "carbon.common.script.net.machoNetPacket.SessionChangeNotification"),
        (18, MachoPacketType::SessionInitialStateNotification, "carbon.common.script.net.machoNetPacket.SessionInitialStateNotification"),
        (20, MachoPacketType::PingReq, "carbon.common.script.net.machoNetPacket.PingReq"),
        (21, MachoPacketType::PingRsp, "carbon.common.script.net.machoNetPacket.PingRsp"),
    ];
    for (code, expected_ty, expected_cls) in cases {
        assert_eq!(MachoPacketType::from_i64(code), Some(expected_ty));
        assert_eq!(MachoPacket::class_name(expected_ty), expected_cls);
    }
    for invalid in [-1, 4, 5, 9, 13, 14, 17, 19, 22, 100] {
        assert_eq!(MachoPacketType::from_i64(invalid), None);
    }
}

#[test]
fn wrapped_runtime_and_user_error_bodies_encode_valid_marshal_exceptions() {
    let rt = wrapped_runtime_error_body(6, "Engine fault");
    let PyRep::Tuple(rt_items) = rt else {
        panic!("expected tuple");
    };
    assert_eq!(rt_items[0], PyRep::Integer(6));
    assert_eq!(rt_items[1], PyRep::Integer(MACHONETERR_WRAPPEDEXCEPTION));
    let PyRep::Tuple(rt_payload) = &rt_items[2] else {
        panic!("expected inner tuple");
    };
    let PyRep::Bytes(rt_bytes) = &rt_payload[0] else {
        panic!("expected marshaled bytes");
    };
    let decoded_rt = marshal::decode(rt_bytes).unwrap();
    let PyRep::ObjectEx { header, .. } = decoded_rt else {
        panic!("expected ObjectEx");
    };
    assert_eq!(
        *header,
        PyRep::Tuple(vec![
            PyRep::Token("exceptions.RuntimeError".into()),
            PyRep::Tuple(vec![PyRep::ascii_bytes("Engine fault")]),
        ])
    );

    let ue = wrapped_user_error_body(6, "Docking denied");
    let PyRep::Tuple(ue_items) = ue else {
        panic!("expected tuple");
    };
    let PyRep::Tuple(ue_payload) = &ue_items[2] else {
        panic!("expected inner tuple");
    };
    let PyRep::Bytes(ue_bytes) = &ue_payload[0] else {
        panic!("expected marshaled bytes");
    };
    let decoded_ue = marshal::decode(ue_bytes).unwrap();
    let PyRep::ObjectEx { header: ue_hdr, .. } = decoded_ue else {
        panic!("expected ObjectEx");
    };
    assert_eq!(
        *ue_hdr,
        PyRep::Tuple(vec![
            PyRep::Token("eveexceptions.UserError".into()),
            PyRep::Tuple(vec![
                PyRep::ascii_bytes("CustomInfo"),
                PyRep::Dict(vec![(
                    PyRep::ascii_bytes("info"),
                    PyRep::ascii_bytes("Docking denied"),
                )]),
            ]),
        ])
    );

    let se = wrapped_slash_error_body(6, "Commands: ['heal', 'giveisk']");
    let PyRep::Tuple(se_items) = se else {
        panic!("expected tuple");
    };
    let PyRep::Tuple(se_payload) = &se_items[2] else {
        panic!("expected inner tuple");
    };
    let PyRep::Bytes(se_bytes) = &se_payload[0] else {
        panic!("expected marshaled bytes");
    };
    let decoded_se = marshal::decode(se_bytes).unwrap();
    let PyRep::ObjectEx { header: se_hdr, .. } = decoded_se else {
        panic!("expected ObjectEx");
    };
    assert_eq!(
        *se_hdr,
        PyRep::Tuple(vec![
            PyRep::Token("eveexceptions.UserError".into()),
            PyRep::Tuple(vec![
                PyRep::ascii_bytes("SlashError"),
                PyRep::Dict(vec![(
                    PyRep::ascii_bytes("reason"),
                    PyRep::ascii_bytes("Commands: ['heal', 'giveisk']"),
                )]),
            ]),
        ])
    );
}

#[test]
fn macho_address_and_packet_variants_roundtrip_and_reject_invalid() {
    let addrs = [
        MachoAddress::Node {
            node_id: 888_444,
            service: Some("beyonce".into()),
            call_id: 12,
        },
        MachoAddress::Node {
            node_id: 888_444,
            service: None,
            call_id: 0,
        },
        MachoAddress::Client {
            client_id: 1001,
            call_id: 7,
            service: Some("stationSvc".into()),
        },
        MachoAddress::Broadcast {
            broadcast_id: "OnItemChange".into(),
            id_type: "clientID".into(),
            narrowcast: vec![1001, 1002],
        },
        MachoAddress::Any {
            service: Some("machoNet".into()),
            call_id: 99,
        },
        MachoAddress::Any {
            service: None,
            call_id: 0,
        },
    ];
    for addr in addrs {
        let rep = addr.to_pyrep();
        assert_eq!(MachoAddress::from_pyrep(&rep), Some(addr));
    }

    // Legacy letter tags & ObjectEx / Tuple representations
    let legacy_b = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token(MACHO_ADDRESS_CLASS.into()),
            PyRep::Tuple(vec![
                PyRep::String("B".into()),
                PyRep::String("OnItemChange".into()),
                PyRep::Tuple(vec![PyRep::Integer(77)]),
            ]),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    assert_eq!(
        MachoAddress::from_pyrep(&legacy_b),
        Some(MachoAddress::Broadcast {
            broadcast_id: "OnItemChange".into(),
            id_type: "*multicastID".into(),
            narrowcast: vec![77],
        })
    );

    let flat_c = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::String("C".into()),
            PyRep::Integer(55),
        ])),
        list: Vec::new(),
        dict: Vec::new(),
    };
    assert_eq!(
        MachoAddress::from_pyrep(&flat_c),
        Some(MachoAddress::Client {
            client_id: 55,
            call_id: 0,
            service: None,
        })
    );
    assert_eq!(
        MachoAddress::from_pyrep(&PyRep::Tuple(vec![PyRep::String("A".into())])),
        Some(MachoAddress::Any {
            service: None,
            call_id: 0,
        })
    );
    assert_eq!(
        MachoAddress::from_pyrep(&PyRep::Tuple(vec![PyRep::String("Z".into())])),
        None
    );
    assert_eq!(MachoAddress::from_pyrep(&PyRep::None), None);

    // MachoPacket roundtrip and broadcast notification extraction
    let mut bcast = MachoPacket::new_broadcast_notification(
        888_444,
        1001,
        Some(42),
        "DoBallsAdded",
        vec![PyRep::Integer(1), PyRep::String("ball".into())],
    );
    bcast
        .named_payload
        .insert("channel".into(), PyRep::ascii_bytes("bcast"));
    let bcast_rep = bcast.to_pyrep();
    let bcast_dec = MachoPacket::from_pyrep(&bcast_rep).unwrap();
    assert_eq!(bcast_dec.packet_type, MachoPacketType::Notification);
    assert_eq!(bcast_dec.user_id, Some(42));
    assert_eq!(
        bcast_dec.named_payload.get("channel"),
        Some(&PyRep::ascii_bytes("bcast"))
    );
    let (bid, bargs) = bcast_dec.extract_broadcast_notification().unwrap();
    assert_eq!(bid, "DoBallsAdded");
    assert_eq!(bargs, &[PyRep::Integer(1), PyRep::String("ball".into())]);

    // Negative broadcast extraction branches
    let mut non_notif = bcast_dec.clone();
    non_notif.packet_type = MachoPacketType::PingReq;
    assert_eq!(non_notif.extract_broadcast_notification(), None);

    let mut non_bcast_dest = bcast_dec.clone();
    non_bcast_dest.destination = MachoAddress::Any {
        service: None,
        call_id: 0,
    };
    assert_eq!(non_bcast_dest.extract_broadcast_notification(), None);

    let mut bad_payload = bcast_dec.clone();
    bad_payload.payload = PyRep::None;
    assert_eq!(bad_payload.extract_broadcast_notification(), None);

    assert!(MachoPacket::from_pyrep(&PyRep::None).is_none());
    assert!(MachoPacket::from_pyrep(&PyRep::Tuple(vec![PyRep::Integer(0); 5])).is_none());
}

#[test]
fn version_exchange_and_extract_macho_call_req_edge_cases() {
    let mut ver = VersionExchange::default();
    assert_eq!(ver.codename(), "V24.01");
    assert_eq!(ver.region(), "ccp");

    let hello = ver.to_server_hello_pyrep();
    assert_eq!(VersionExchange::from_pyrep(&hello), Some(ver.clone()));
    assert_eq!(
        VersionExchange::from_pyrep(&PyRep::Tuple(vec![PyRep::Integer(0); 8])),
        None
    );

    // Project version without '@'
    ver.project_version = "V24_STANDALONE".into();
    assert_eq!(ver.codename(), "V24_STANDALONE");
    assert_eq!(ver.region(), "");

    // validate_client branches
    let base = VersionExchange::default();
    assert!(base.validate_client(&base).is_ok());

    let mut c_bday = base.clone();
    c_bday.birthday += 1;
    assert_eq!(
        base.validate_client(&c_bday).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLERELEASE")
    );

    let mut c_macho = base.clone();
    c_macho.macho_version += 1;
    assert_eq!(
        base.validate_client(&c_macho).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEPROTOCOL")
    );

    let mut c_ver = base.clone();
    c_ver.version_number += 1.0;
    assert_eq!(
        base.validate_client(&c_ver).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEVERSION")
    );

    let mut c_build = base.clone();
    c_build.build_version -= 1;
    assert_eq!(
        base.validate_client(&c_build).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEBUILD")
    );

    let mut c_reg = base.clone();
    c_reg.project_version = "V24.01@other".into();
    assert_eq!(
        base.validate_client(&c_reg).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEREGION")
    );

    let mut c_code = base.clone();
    c_code.project_version = "V23.01@ccp".into();
    assert_eq!(
        base.validate_client(&c_code).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLERELEASE")
    );

    // extract_macho_call_req with OID string, nested SubStream, and t[0] == 1 vs 2
    let oid_call = PyRep::Tuple(vec![PyRep::Tuple(vec![
        PyRep::Integer(1),
        PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::Integer(0),
            PyRep::SubStream(Box::new(PyRep::Tuple(vec![
                PyRep::String("N=888444:555".into()),
                PyRep::String("DoStuff".into()),
                PyRep::Tuple(vec![PyRep::Integer(9)]),
                PyRep::Dict(vec![(
                    PyRep::String("machoVersion".into()),
                    PyRep::Integer(1),
                )]),
            ]))),
        ]))),
    ])]);
    let (bid, m, a, kw) = extract_macho_call_req(&oid_call);
    assert_eq!(bid, Some(555));
    assert_eq!(m, "DoStuff");
    assert_eq!(a, vec![PyRep::Integer(9)]);
    assert_eq!(kw.get("machoVersion"), Some(&PyRep::Integer(1)));

    // Non-SubStream tuple with t[0] == 1 -> bound_id = Some(1)
    let direct_call = PyRep::Tuple(vec![PyRep::Tuple(vec![
        PyRep::Integer(1),
        PyRep::Tuple(vec![
            PyRep::None,
            PyRep::String("Ping".into()),
            PyRep::Tuple(vec![]),
        ]),
    ])]);
    let (bid2, m2, _, _) = extract_macho_call_req(&direct_call);
    assert_eq!(bid2, Some(1));
    assert_eq!(m2, "Ping");

    // extract_macho_call_req with t[0] == 2 (explicit bound_id > 1) on SubStream
    let bound_2_call = PyRep::Tuple(vec![PyRep::Tuple(vec![
        PyRep::Integer(2),
        PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::None,
            PyRep::String("BoundMethod".into()),
            PyRep::Tuple(vec![]),
        ]))),
    ])]);
    let (bid3, m3, _, _) = extract_macho_call_req(&bound_2_call);
    assert_eq!(bid3, Some(2));
    assert_eq!(m3, "BoundMethod");

    assert_eq!(parse_bound_oid_str(&PyRep::String("N=888444:0".into())), None);
    assert_eq!(parse_bound_oid_str(&PyRep::String("no_colon".into())), None);
    assert_eq!(extract_macho_call_req(&PyRep::Tuple(vec![])).0, None);
}

#[test]
fn session_state_notifications_locations_and_full_diff_matrix() {
    let mut s1 = SessionState::new(77);
    s1.push_notification("OnItemChange", vec![PyRep::Integer(1)]);
    assert_eq!(
        s1.drain_notifications(),
        vec![("OnItemChange".to_string(), vec![PyRep::Integer(1)])]
    );
    assert!(s1.drain_notifications().is_empty());
    assert_eq!(s1.effective_location_id(), None);
    assert_eq!(s1.to_session_change_pyrep(&s1, vec![888_444]), None);

    // Location priority: station > structure > solar_system
    s1.solar_system_id = Some(30_000_142);
    assert_eq!(s1.effective_location_id(), Some(30_000_142));
    s1.structure_id = Some(1_000_000_001);
    assert_eq!(s1.effective_location_id(), Some(1_000_000_001));
    s1.station_id = Some(60_003_760);
    assert_eq!(s1.effective_location_id(), Some(60_003_760));

    let mut s2 = SessionState::new(77);
    s2.user_id = Some(10);
    s2.character_id = Some(20);
    s2.corporation_id = Some(30);
    s2.alliance_id = Some(40);
    s2.structure_id = Some(50);
    s2.solar_system_id = Some(30_000_142);
    s2.constellation_id = Some(20_000_020);
    s2.region_id = Some(10_000_002);
    s2.ship_id = Some(60);
    s2.race_id = Some(1);
    s2.corp_role = 128;
    s2.fleet_id = Some(70);
    s2.war_faction_id = Some(80);
    s2.role = service_roles::ROLE_GM_DEFAULT;
    s2.language_id = "RU".into();
    s2.address = "10.0.0.1:26000".into();

    let base = SessionState::new(77);
    let diff = base.compute_diff(&s2);
    for key in [
        "userid",
        "charid",
        "corpid",
        "corpAccountKey",
        "allianceid",
        "structureid",
        "locationid",
        "solarsystemid2",
        "constellationid",
        "regionid",
        "shipid",
        "raceID",
        "corprole",
        "fleetid",
        "warfactionid",
        "role",
        "languageID",
        "address",
    ] {
        assert!(diff.contains_key(key), "missing diff key {key}");
    }
    assert!(s2.to_session_change_pyrep(&base, vec![888_444]).is_some());
    assert_eq!(
        s2.to_initial_session_notification(888_444, 9001).packet_type,
        MachoPacketType::SessionInitialStateNotification
    );
    assert_eq!(
        s2.to_login_session_change_packet(888_444, 9001).packet_type,
        MachoPacketType::SessionChangeNotification
    );
}

#[test]
fn session_state_live_notify_listener_defaults_serde_skip_and_diff_invariance() {
    let def_sess = SessionState::default();
    assert!(!def_sess.live_notify_listener);

    let mut sess = SessionState::new(42);
    assert!(!sess.live_notify_listener);
    sess.live_notify_listener = true;
    sess.push_notification("OnTest", vec![PyRep::Integer(1)]); // rpc-lint: allow test fixture

    // Toggling live_notify_listener does not emit any session diff keys or SessionChangeNotification
    let mut sess_no_listener = sess.clone();
    sess_no_listener.live_notify_listener = false;
    assert!(sess.compute_diff(&sess_no_listener).is_empty());
    assert_eq!(sess.to_session_change_pyrep(&sess_no_listener, vec![888_444]), None);

    // #[serde(skip)] omits both live_notify_listener and notifications during serialization
    // and resets them to false / empty on deserialization
    let json = serde_json::to_string(&sess).unwrap();
    assert!(!json.contains("live_notify_listener"));
    assert!(!json.contains("notifications"));
    let restored: SessionState = serde_json::from_str(&json).unwrap();
    assert_eq!(restored.session_id, 42);
    assert!(!restored.live_notify_listener);
    assert!(restored.notifications.is_empty());
}

#[test]
fn wrapped_user_error_dict_dict_key_and_named_helpers_roundtrip_all_fields() {
    // 1. wrapped_user_error_with_dict_body with multiple entries and empty entries
    let multi = wrapped_user_error_with_dict_body(
        6,
        "NotEnoughCargoSpace",
        vec![
            (PyRep::ascii_bytes("free"), PyRep::Real(10.5)),
            (PyRep::ascii_bytes("required"), PyRep::Real(25.0)),
        ],
    );
    let PyRep::Tuple(multi_outer) = multi else {
        panic!("expected outer tuple");
    };
    assert_eq!(multi_outer[0], PyRep::Integer(6));
    assert_eq!(multi_outer[1], PyRep::Integer(MACHONETERR_WRAPPEDEXCEPTION));
    let PyRep::Tuple(multi_inner) = &multi_outer[2] else {
        panic!("expected inner tuple");
    };
    let PyRep::Bytes(multi_bytes) = &multi_inner[0] else {
        panic!("expected marshaled bytes");
    };
    let PyRep::ObjectEx {
        is_type_2,
        header,
        list,
        dict,
    } = marshal::decode(multi_bytes).unwrap()
    else {
        panic!("expected ObjectEx");
    };
    assert!(!is_type_2);
    assert!(list.is_empty());
    assert!(dict.is_empty());
    assert_eq!(
        *header,
        PyRep::Tuple(vec![
            PyRep::Token("eveexceptions.UserError".into()),
            PyRep::Tuple(vec![
                PyRep::ascii_bytes("NotEnoughCargoSpace"),
                PyRep::Dict(vec![
                    (PyRep::ascii_bytes("free"), PyRep::Real(10.5)),
                    (PyRep::ascii_bytes("required"), PyRep::Real(25.0)),
                ]),
            ]),
        ])
    );

    let empty_dict_err = wrapped_user_error_with_dict_body(10, "EmptyDictErr", Vec::new());
    let PyRep::Tuple(ed_outer) = empty_dict_err else {
        panic!("expected tuple");
    };
    assert_eq!(ed_outer[0], PyRep::Integer(10));
    let PyRep::Tuple(ed_inner) = &ed_outer[2] else {
        panic!("expected inner tuple");
    };
    let PyRep::Bytes(ed_bytes) = &ed_inner[0] else {
        panic!("expected bytes");
    };
    let PyRep::ObjectEx { header: ed_hdr, .. } = marshal::decode(ed_bytes).unwrap() else {
        panic!("expected ObjectEx");
    };
    assert_eq!(
        *ed_hdr,
        PyRep::Tuple(vec![
            PyRep::Token("eveexceptions.UserError".into()),
            PyRep::Tuple(vec![
                PyRep::ascii_bytes("EmptyDictErr"),
                PyRep::Dict(Vec::new()),
            ]),
        ])
    );

    // 2. wrapped_user_error_with_dict_key_body with custom key
    let custom_key =
        wrapped_user_error_with_dict_key_body(6, "CustomKeyErr", "customKey", "customVal");
    let PyRep::Tuple(ck_outer) = custom_key else {
        panic!("expected tuple");
    };
    let PyRep::Tuple(ck_inner) = &ck_outer[2] else {
        panic!("expected inner tuple");
    };
    let PyRep::Bytes(ck_bytes) = &ck_inner[0] else {
        panic!("expected bytes");
    };
    let PyRep::ObjectEx { header: ck_hdr, .. } = marshal::decode(ck_bytes).unwrap() else {
        panic!("expected ObjectEx");
    };
    assert_eq!(
        *ck_hdr,
        PyRep::Tuple(vec![
            PyRep::Token("eveexceptions.UserError".into()),
            PyRep::Tuple(vec![
                PyRep::ascii_bytes("CustomKeyErr"),
                PyRep::Dict(vec![(
                    PyRep::ascii_bytes("customKey"),
                    PyRep::ascii_bytes("customVal"),
                )]),
            ]),
        ])
    );

    // 3. wrapped_named_user_error_body uses "info" key
    let named = wrapped_named_user_error_body(6, "UnMachoDestination", "Node moved");
    let PyRep::Tuple(nm_outer) = named else {
        panic!("expected tuple");
    };
    let PyRep::Tuple(nm_inner) = &nm_outer[2] else {
        panic!("expected inner tuple");
    };
    let PyRep::Bytes(nm_bytes) = &nm_inner[0] else {
        panic!("expected bytes");
    };
    let PyRep::ObjectEx { header: nm_hdr, .. } = marshal::decode(nm_bytes).unwrap() else {
        panic!("expected ObjectEx");
    };
    assert_eq!(
        *nm_hdr,
        PyRep::Tuple(vec![
            PyRep::Token("eveexceptions.UserError".into()),
            PyRep::Tuple(vec![
                PyRep::ascii_bytes("UnMachoDestination"),
                PyRep::Dict(vec![(
                    PyRep::ascii_bytes("info"),
                    PyRep::ascii_bytes("Node moved"),
                )]),
            ]),
        ])
    );
}

