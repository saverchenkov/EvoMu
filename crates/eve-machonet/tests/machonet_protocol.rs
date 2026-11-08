//! Comprehensive unit and roundtrip protocol tests for `eve-machonet` (`lib.rs`).

use eve_machonet::{
    wrapped_runtime_error_body, wrapped_user_error_body,
    MachoAddress, MachoPacket, MachoPacketType,
    ADDRESS_TYPE_ANY, ADDRESS_TYPE_BROADCAST, ADDRESS_TYPE_CLIENT, ADDRESS_TYPE_NODE, MACHONETERR_WRAPPEDEXCEPTION,
};
use eve_proto::{marshal, PyRep};
use std::collections::HashMap;

#[cfg(test)]
#[path = "machonet_protocol/session_and_version_tests.rs"]
mod session_and_version_tests;

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

    let slash_body = eve_machonet::wrapped_slash_error_body(6, "Commands: ['heal']");
    let PyRep::Tuple(slash_items) = slash_body else {
        panic!("expected tuple");
    };
    assert_eq!(slash_items[1], PyRep::Integer(MACHONETERR_WRAPPEDEXCEPTION));
    let PyRep::Tuple(slash_sub) = &slash_items[2] else {
        panic!("expected inner tuple");
    };
    let PyRep::Bytes(slash_bytes) = &slash_sub[0] else {
        panic!("expected bytes");
    };
    let decoded_slash = marshal::decode(slash_bytes).expect("decode");
    match decoded_slash {
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
            assert_eq!(args[0].as_str(), Some("SlashError"));
            let PyRep::Dict(entries) = &args[1] else {
                panic!("expected dict");
            };
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].0.as_str(), Some("reason"));
            assert_eq!(entries[0].1.as_str(), Some("Commands: ['heal']"));
        }
        other => panic!("unexpected slash exc rep: {other:?}"),
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
