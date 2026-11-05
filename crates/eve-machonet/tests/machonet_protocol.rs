//! Comprehensive unit and roundtrip protocol tests for `eve-machonet` (`lib.rs`).

use eve_machonet::{
    extract_macho_call_req, service_roles, wrapped_runtime_error_body, wrapped_user_error_body,
    MachoAddress, MachoPacket, MachoPacketType, SessionState, VersionExchange,
    ADDRESS_TYPE_ANY, ADDRESS_TYPE_BROADCAST, ADDRESS_TYPE_CLIENT, ADDRESS_TYPE_NODE,
    HANDSHAKE_SECRET_KEY, MACHONETERR_WRAPPEDEXCEPTION, SESSION_TYPE_GAME, V24_BOOT_BUILD,
    V24_MACHO_VERSION,
};
use eve_proto::{marshal, PyRep};
use std::collections::HashMap;

#[test]
fn test_packet_type_discriminants_and_class_names() {
    let cases = [
        (0, MachoPacketType::AuthenticationReq, "AuthenticationReq"),
        (1, MachoPacketType::AuthenticationRsp, "AuthenticationRsp"),
        (2, MachoPacketType::IdentificationReq, "IdentificationReq"),
        (3, MachoPacketType::IdentificationRsp, "IdentificationRsp"),
        (6, MachoPacketType::CallReq, "CallReq"),
        (7, MachoPacketType::CallRsp, "CallRsp"),
        (8, MachoPacketType::TransportClosed, "TransportClosed"),
        (10, MachoPacketType::ResolveReq, "ResolveReq"),
        (11, MachoPacketType::ResolveRsp, "ResolveRsp"),
        (12, MachoPacketType::Notification, "Notification"),
        (15, MachoPacketType::ErrorResponse, "ErrorResponse"),
        (16, MachoPacketType::SessionChangeNotification, "SessionChangeNotification"),
        (
            18,
            MachoPacketType::SessionInitialStateNotification,
            "SessionInitialStateNotification",
        ),
        (20, MachoPacketType::PingReq, "PingReq"),
        (21, MachoPacketType::PingRsp, "PingRsp"),
    ];

    for (code, expected_pt, suffix) in cases {
        let parsed = MachoPacketType::from_i64(code).expect("valid discriminant");
        assert_eq!(parsed, expected_pt);
        let expected_class = format!("carbon.common.script.net.machoNetPacket.{suffix}");
        assert_eq!(MachoPacket::class_name(parsed), expected_class);
    }

    for invalid in [-1, 4, 5, 9, 13, 14, 17, 19, 22, 999] {
        assert_eq!(MachoPacketType::from_i64(invalid), None);
    }
}

#[test]
fn test_wrapped_runtime_and_user_error_bodies() {
    let rt_body = wrapped_runtime_error_body(77, "node crashed");
    let PyRep::Tuple(rt_items) = rt_body else {
        panic!("expected tuple");
    };
    assert_eq!(rt_items.len(), 3);
    assert_eq!(rt_items[0], PyRep::Integer(77));
    assert_eq!(rt_items[1], PyRep::Integer(MACHONETERR_WRAPPEDEXCEPTION));
    let PyRep::Tuple(rt_sub) = &rt_items[2] else {
        panic!("expected inner tuple");
    };
    let PyRep::Bytes(rt_bytes) = &rt_sub[0] else {
        panic!("expected bytes");
    };
    let decoded_rt = marshal::decode(rt_bytes).expect("decode runtime exc");
    match decoded_rt {
        PyRep::ObjectEx {
            is_type_2: false,
            header,
            list,
            dict,
        } => {
            assert!(list.is_empty());
            assert!(dict.is_empty());
            let PyRep::Tuple(h) = *header else {
                panic!("expected header tuple");
            };
            assert_eq!(h[0].as_str(), Some("exceptions.RuntimeError"));
            let PyRep::Tuple(args) = &h[1] else {
                panic!("expected args tuple");
            };
            assert_eq!(args[0].as_str(), Some("node crashed"));
        }
        other => panic!("unexpected runtime exc rep: {other:?}"),
    }

    let usr_body = wrapped_user_error_body(88, "insufficient capacitor");
    let PyRep::Tuple(usr_items) = usr_body else {
        panic!("expected tuple");
    };
    assert_eq!(usr_items[0], PyRep::Integer(88));
    assert_eq!(usr_items[1], PyRep::Integer(MACHONETERR_WRAPPEDEXCEPTION));
    let PyRep::Tuple(usr_sub) = &usr_items[2] else {
        panic!("expected inner tuple");
    };
    let PyRep::Bytes(usr_bytes) = &usr_sub[0] else {
        panic!("expected bytes");
    };
    let decoded_usr = marshal::decode(usr_bytes).expect("decode");
    match decoded_usr {
        PyRep::ObjectEx {
            is_type_2: false,
            header,
            ..
        } => {
            let PyRep::Tuple(h) = *header else {
                panic!("expected header tuple");
            };
            assert_eq!(h[0].as_str(), Some("eveexceptions.UserError"));
            let PyRep::Tuple(args) = &h[1] else {
                panic!("expected args tuple");
            };
            assert_eq!(args[0].as_str(), Some("CustomInfo"));
            let PyRep::Dict(entries) = &args[1] else {
                panic!("expected dict");
            };
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].0.as_str(), Some("info"));
            assert_eq!(entries[0].1.as_str(), Some("insufficient capacitor"));
        }
        other => panic!("unexpected user exc rep: {other:?}"),
    }
}

#[test]
fn test_macho_address_all_variants_and_wire_encodings() {
    let addresses = [
        MachoAddress::Node {
            node_id: 888_444,
            service: Some("machoNet".to_string()),
            call_id: 19,
        },
        MachoAddress::Node {
            node_id: 12,
            service: None,
            call_id: 0,
        },
        MachoAddress::Client {
            client_id: 10_005,
            call_id: 42,
            service: Some("beyonce".to_string()),
        },
        MachoAddress::Client {
            client_id: 10_006,
            call_id: 0,
            service: None,
        },
        MachoAddress::Broadcast {
            broadcast_id: "DoDestinyUpdate".to_string(),
            id_type: "clientID".to_string(),
            narrowcast: vec![101, 202],
        },
        MachoAddress::Any {
            service: Some("dogmaIM".to_string()),
            call_id: 7,
        },
        MachoAddress::Any {
            service: None,
            call_id: 0,
        },
    ];

    for addr in &addresses {
        let raw = marshal::encode(&addr.to_pyrep());
        let rep = marshal::decode(&raw).expect("decode address");
        let parsed = MachoAddress::from_pyrep(&rep).expect("parse address");
        assert_eq!(&parsed, addr);
    }

    // Legacy string tags, ObjectEx encodings, and default narrowcast/id_type fallback
    let legacy_node = PyRep::Tuple(vec![
        PyRep::ascii_bytes("N"),
        PyRep::Integer(55),
        PyRep::ascii_bytes("account"),
    ]);
    assert_eq!(
        MachoAddress::from_pyrep(&legacy_node),
        Some(MachoAddress::Node {
            node_id: 55,
            service: Some("account".to_string()),
            call_id: 0,
        })
    );

    let obj_ex_client = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("macho.MachoAddress".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(ADDRESS_TYPE_CLIENT),
                PyRep::Integer(700),
                PyRep::Integer(33),
                PyRep::None,
            ]),
        ])),
        list: vec![],
        dict: vec![],
    };
    assert_eq!(
        MachoAddress::from_pyrep(&obj_ex_client),
        Some(MachoAddress::Client {
            client_id: 700,
            call_id: 33,
            service: None,
        })
    );

    let obj_ex_flat_bcast = PyRep::ObjectEx {
        is_type_2: true,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Integer(ADDRESS_TYPE_BROADCAST),
            PyRep::ascii_bytes("OnSessionChanged"),
            PyRep::Tuple(vec![PyRep::Integer(9)]),
        ])),
        list: vec![],
        dict: vec![],
    };
    assert_eq!(
        MachoAddress::from_pyrep(&obj_ex_flat_bcast),
        Some(MachoAddress::Broadcast {
            broadcast_id: "OnSessionChanged".to_string(),
            id_type: "*multicastID".to_string(),
            narrowcast: vec![9],
        })
    );

    let any_tag = PyRep::Tuple(vec![PyRep::Integer(ADDRESS_TYPE_ANY), PyRep::None]);
    assert_eq!(
        MachoAddress::from_pyrep(&any_tag),
        Some(MachoAddress::Any {
            service: None,
            call_id: 0,
        })
    );

    // Invalid addresses
    assert_eq!(MachoAddress::from_pyrep(&PyRep::Integer(1)), None);
    assert_eq!(MachoAddress::from_pyrep(&PyRep::Tuple(vec![])), None);
    assert_eq!(
        MachoAddress::from_pyrep(&PyRep::Tuple(vec![PyRep::ascii_bytes("Z")])),
        None
    );
    assert_eq!(
        MachoAddress::from_pyrep(&PyRep::Object {
            type_name: "macho.MachoAddress".into(),
            args: Box::new(PyRep::Integer(ADDRESS_TYPE_NODE)),
        }),
        None
    );
    assert_eq!(
        MachoAddress::from_pyrep(&PyRep::ObjectEx {
            is_type_2: false,
            header: Box::new(PyRep::Tuple(vec![PyRep::Integer(1)])),
            list: vec![],
            dict: vec![],
        }),
        None
    );
}

#[test]
fn test_macho_packet_roundtrip_and_broadcast_notifications() {
    let mut named = HashMap::new();
    named.insert("channel".to_string(), PyRep::ascii_bytes("sessionchange"));
    named.insert("sn".to_string(), PyRep::Integer(4));

    let pkt = MachoPacket {
        packet_type: MachoPacketType::CallRsp,
        source: MachoAddress::Node {
            node_id: 888_444,
            service: Some("dogmaIM".to_string()),
            call_id: 12,
        },
        destination: MachoAddress::Client {
            client_id: 2001,
            call_id: 12,
            service: None,
        },
        user_id: Some(9001),
        payload: PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::Integer(42)))]),
        named_payload: named.clone(),
    };

    let bytes = marshal::encode(&pkt.to_pyrep());
    let decoded_rep = marshal::decode(&bytes).expect("decode packet");
    let decoded_pkt = MachoPacket::from_pyrep(&decoded_rep).expect("parse packet");
    assert_eq!(decoded_pkt.packet_type, MachoPacketType::CallRsp);
    assert_eq!(decoded_pkt.source, pkt.source);
    assert_eq!(decoded_pkt.destination, pkt.destination);
    assert_eq!(decoded_pkt.user_id, Some(9001));
    assert_eq!(decoded_pkt.named_payload.get("sn"), Some(&PyRep::Integer(4)));

    // Empty named_payload encodes None at index 5 and parses back empty
    let pkt_empty = MachoPacket {
        user_id: None,
        named_payload: HashMap::new(),
        ..pkt.clone()
    };
    let rep_empty = pkt_empty.to_pyrep();
    let PyRep::Object { ref args, .. } = rep_empty else {
        panic!("expected Object");
    };
    let PyRep::Tuple(ref fields) = **args else {
        panic!("expected Tuple");
    };
    assert_eq!(fields.len(), 14);
    assert_eq!(fields[5], PyRep::None);
    let reparsed_empty = MachoPacket::from_pyrep(&rep_empty).expect("reparse");
    assert_eq!(reparsed_empty.user_id, None);
    assert!(reparsed_empty.named_payload.is_empty());

    // ObjectEx & Tuple parsing for MachoPacket
    let pkt_objex = PyRep::ObjectEx {
        is_type_2: false,
        header: Box::new(PyRep::Tuple(vec![
            PyRep::Token("macho.PingReq".into()),
            PyRep::Tuple(vec![
                PyRep::Integer(20),
                MachoAddress::Client {
                    client_id: 5,
                    call_id: 1,
                    service: None,
                }
                .to_pyrep(),
                MachoAddress::Node {
                    node_id: 1,
                    service: None,
                    call_id: 0,
                }
                .to_pyrep(),
                PyRep::None,
                PyRep::Tuple(vec![]),
                PyRep::None,
            ]),
        ])),
        list: vec![],
        dict: vec![],
    };
    assert_eq!(
        MachoPacket::from_pyrep(&pkt_objex).map(|p| p.packet_type),
        Some(MachoPacketType::PingReq)
    );

    // Too-short tuple (< 6) rejected
    assert!(MachoPacket::from_pyrep(&PyRep::Tuple(vec![PyRep::Integer(20)])).is_none());

    // Broadcast notification roundtrip + negative checks
    let bcast = MachoPacket::new_broadcast_notification(
        888_444,
        3001,
        Some(4001),
        "DoDestinyUpdate",
        vec![PyRep::Integer(3300), PyRep::Integer(5)],
    );
    let bcast_bytes = marshal::encode(&bcast.to_pyrep());
    let bcast_roundtrip =
        MachoPacket::from_pyrep(&marshal::decode(&bcast_bytes).unwrap()).expect("bcast");
    let (b_id, b_args) = bcast_roundtrip
        .extract_broadcast_notification()
        .expect("extract broadcast");
    assert_eq!(b_id, "DoDestinyUpdate");
    assert_eq!(b_args, &[PyRep::Integer(3300), PyRep::Integer(5)]);

    assert!(decoded_pkt.extract_broadcast_notification().is_none());
    let mut wrong_dest = bcast.clone();
    wrong_dest.destination = MachoAddress::Client {
        client_id: 1,
        call_id: 0,
        service: None,
    };
    assert!(wrong_dest.extract_broadcast_notification().is_none());
    let mut bad_payload = bcast.clone();
    bad_payload.payload = PyRep::Tuple(vec![PyRep::Tuple(vec![PyRep::Integer(0), PyRep::None])]);
    assert!(bad_payload.extract_broadcast_notification().is_none());
}

#[test]
fn test_version_exchange_parsing_and_validation() {
    let srv = VersionExchange::default();
    assert_eq!(srv.birthday, HANDSHAKE_SECRET_KEY);
    assert_eq!(srv.macho_version, V24_MACHO_VERSION);
    assert_eq!(srv.build_version, V24_BOOT_BUILD);
    assert_eq!(srv.codename(), "V24.01");
    assert_eq!(srv.region(), "ccp");

    let no_at = VersionExchange {
        project_version: "TRANQUILITY".to_string(),
        ..srv.clone()
    };
    assert_eq!(no_at.codename(), "TRANQUILITY");
    assert_eq!(no_at.region(), "");

    let hello = srv.to_server_hello_pyrep();
    let PyRep::Tuple(ref hello_items) = hello else {
        panic!("expected tuple");
    };
    assert_eq!(hello_items.len(), 7);
    assert_eq!(hello_items[6], PyRep::None);
    assert_eq!(VersionExchange::from_pyrep(&hello), Some(srv.clone()));

    // Integer version_number and None user_count accepted
    let int_ver = PyRep::Tuple(vec![
        PyRep::Integer(HANDSHAKE_SECRET_KEY as i64),
        PyRep::Integer(V24_MACHO_VERSION as i64),
        PyRep::None,
        PyRep::Integer(24),
        PyRep::Integer(V24_BOOT_BUILD as i64),
        PyRep::ascii_bytes("24.0@ccp"),
    ]);
    let parsed_int = VersionExchange::from_pyrep(&int_ver).expect("int version");
    assert_eq!(parsed_int.user_count, 0);
    assert_eq!(parsed_int.version_number, 24.0);

    // Invalid lengths
    assert!(VersionExchange::from_pyrep(&PyRep::Tuple(vec![PyRep::Integer(1)])).is_none());

    // Newer build accepted, within epsilon version accepted
    let newer_client = VersionExchange {
        build_version: V24_BOOT_BUILD + 50,
        version_number: srv.version_number + 0.00005,
        ..srv.clone()
    };
    assert!(srv.validate_client(&newer_client).is_ok());

    let mut bad_bday = srv.clone();
    bad_bday.birthday = 1;
    assert_eq!(
        srv.validate_client(&bad_bday).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLERELEASE")
    );

    let mut bad_proto = srv.clone();
    bad_proto.macho_version = 495;
    assert_eq!(
        srv.validate_client(&bad_proto).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEPROTOCOL")
    );

    let mut bad_ver = srv.clone();
    bad_ver.version_number = 23.0;
    assert_eq!(
        srv.validate_client(&bad_ver).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEVERSION")
    );

    let mut old_build = srv.clone();
    old_build.build_version = V24_BOOT_BUILD - 1;
    assert_eq!(
        srv.validate_client(&old_build).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEBUILD")
    );

    let mut bad_region = srv.clone();
    bad_region.project_version = "V24.01@serenity".to_string();
    assert_eq!(
        srv.validate_client(&bad_region).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLEREGION")
    );

    let mut bad_codename = srv.clone();
    bad_codename.project_version = "V23.02@ccp".to_string();
    assert_eq!(
        srv.validate_client(&bad_codename).unwrap_err().reason_code.as_deref(),
        Some("HANDSHAKE_INCOMPATIBLERELEASE")
    );
}

#[test]
fn test_extract_macho_call_req_variants() {
    // 1. Standard V24 service call: (0, SubStream((1, SubStream(("GetTime", (10, 20), {"k": "v"})))))
    let call_3 = PyRep::Tuple(vec![
        PyRep::ascii_bytes("GetTime"),
        PyRep::Tuple(vec![PyRep::Integer(10), PyRep::Integer(20)]),
        PyRep::Dict(vec![(PyRep::ascii_bytes("k"), PyRep::ascii_bytes("v"))]),
    ]);
    let wrapped = PyRep::Tuple(vec![PyRep::Tuple(vec![
        PyRep::Integer(1),
        PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::SubStream(Box::new(call_3)),
        ]))),
    ])]);
    let (oid, method, args, kwargs) = extract_macho_call_req(&wrapped);
    assert_eq!(oid, None);
    assert_eq!(method, "GetTime");
    assert_eq!(args, vec![PyRep::Integer(10), PyRep::Integer(20)]);
    assert_eq!(kwargs.get("k"), Some(&PyRep::ascii_bytes("v")));

    // 2. Bound object call with "N=888444:9002" prefix and List args
    let bound_4 = PyRep::Tuple(vec![
        PyRep::ascii_bytes("N=888444:9002"),
        PyRep::ascii_bytes("DogmaGetInfo"),
        PyRep::List(vec![PyRep::Integer(777)]),
        PyRep::Dict(vec![]),
    ]);
    let bound_payload = PyRep::Tuple(vec![PyRep::SubStream(Box::new(bound_4))]);
    let (oid2, method2, args2, kwargs2) = extract_macho_call_req(&bound_payload);
    assert_eq!(oid2, Some(9002));
    assert_eq!(method2, "DogmaGetInfo");
    assert_eq!(args2, vec![PyRep::Integer(777)]);
    assert!(kwargs2.is_empty());

    // 3. Numeric t[0] > 1 with SubStream sets bound_id, non-SubStream with t[0] > 0 sets bound_id
    let syn = PyRep::Tuple(vec![PyRep::Tuple(vec![
        PyRep::Integer(55),
        PyRep::Tuple(vec![
            PyRep::None,
            PyRep::ascii_bytes("CmdAccelerate"),
            PyRep::Tuple(vec![]),
            PyRep::None,
        ]),
    ])]);
    let (oid3, method3, args3, _) = extract_macho_call_req(&syn);
    assert_eq!(oid3, Some(55));
    assert_eq!(method3, "CmdAccelerate");
    assert!(args3.is_empty());

    // 4. Malformed payloads return empty
    assert_eq!(
        extract_macho_call_req(&PyRep::None),
        (None, String::new(), vec![], HashMap::new())
    );
    assert_eq!(
        extract_macho_call_req(&PyRep::Tuple(vec![])),
        (None, String::new(), vec![], HashMap::new())
    );
}

#[test]
fn test_session_state_diffs_notifications_and_gm_roles() {
    let mut s1 = SessionState::new(9001);
    assert!(!s1.is_authenticated());
    assert_eq!(s1.effective_location_id(), None);
    assert!(s1.to_session_change_pyrep(&s1, vec![1]).is_none());

    s1.push_notification("DoDestinyUpdate", vec![PyRep::Integer(1)]);
    assert_eq!(s1.drain_notifications().len(), 1);
    assert!(s1.drain_notifications().is_empty());

    // Hierarchy: station_id > structure_id > solar_system_id
    s1.solar_system_id = Some(30_000_142);
    assert_eq!(s1.effective_location_id(), Some(30_000_142));
    s1.structure_id = Some(1_000_000_000_001);
    assert_eq!(s1.effective_location_id(), Some(1_000_000_000_001));
    s1.station_id = Some(60_003_760);
    assert_eq!(s1.effective_location_id(), Some(60_003_760));

    // Undocking in the same solar system adds `solarsystemid` -> Some(30000142) without `solarsystemid2`
    let mut undocked = s1.clone();
    undocked.station_id = None;
    undocked.structure_id = None;
    let undock_diff = s1.compute_diff(&undocked);
    assert_eq!(
        undock_diff.get("solarsystemid"),
        Some(&(PyRep::None, PyRep::Integer(30_000_142)))
    );
    assert!(!undock_diff.contains_key("solarsystemid2"));

    // All remaining fields changing
    let mut s2 = undocked.clone();
    s2.user_id = Some(100);
    s2.character_id = Some(200);
    s2.corporation_id = Some(300);
    s2.alliance_id = Some(400);
    s2.solar_system_id = Some(30_002_187);
    s2.constellation_id = Some(20_000_322);
    s2.region_id = Some(10_000_032);
    s2.ship_id = Some(500);
    s2.race_id = Some(1);
    s2.corp_role = 16;
    s2.fleet_id = Some(600);
    s2.war_faction_id = Some(500_001);
    s2.role = service_roles::ROLE_PLAYER_DEFAULT | service_roles::ROLE_GML;
    s2.language_id = "DE".to_string();
    s2.address = "127.0.0.1:26000".to_string();

    assert!(s2.is_authenticated());
    let full_diff = undocked.compute_diff(&s2);
    for expected_key in [
        "userid",
        "charid",
        "corpid",
        "allianceid",
        "locationid",
        "solarsystemid2",
        "solarsystemid",
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
        assert!(
            full_diff.contains_key(expected_key),
            "missing diff key {expected_key}"
        );
    }

    let change_pyrep = undocked
        .to_session_change_pyrep(&s2, vec![888_444])
        .expect("change pyrep");
    let PyRep::Tuple(citems) = change_pyrep else {
        panic!("expected tuple");
    };
    assert_eq!(citems[0], PyRep::Integer(9001));
    assert_eq!(citems[2], PyRep::List(vec![PyRep::Integer(888_444)]));

    let init_pkt = s2.to_initial_session_notification(888_444, 12);
    assert_eq!(
        init_pkt.packet_type,
        MachoPacketType::SessionInitialStateNotification
    );
    let PyRep::Tuple(ip) = init_pkt.payload else {
        panic!("expected tuple");
    };
    assert_eq!(ip[1], PyRep::Integer(SESSION_TYPE_GAME));

    let login_pkt = s2.to_login_session_change_packet(888_444, 12);
    assert_eq!(
        login_pkt.packet_type,
        MachoPacketType::SessionChangeNotification
    );

    // GM role mask checks
    assert!(!SessionState::new(1).has_gm_role());
    for gm_bit in [
        service_roles::ROLE_GML,
        service_roles::ROLE_GMH,
        service_roles::ROLE_GMS,
        service_roles::ROLE_ADMIN,
        service_roles::ROLE_WORLDMOD,
    ] {
        let mut s = SessionState::new(2);
        s.role = service_roles::ROLE_PLAYER_DEFAULT | gm_bit;
        assert!(s.has_gm_role());
    }
}
