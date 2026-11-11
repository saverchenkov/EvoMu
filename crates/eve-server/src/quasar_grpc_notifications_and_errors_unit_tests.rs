//! Semantic unit tests split from `quasar_grpc_unit_tests`.

use crate::quasar_grpc::*;
use crate::router::client_compat::normalize_macho_notification;
use crate::router::ServerContext;
use bytes::{Buf, BytesMut};
use eve_proto::quasar::{
    EvePublicEvent, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;
use http::{Request, StatusCode};
use std::io::Write;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

#[test]
fn normalize_macho_notification_covers_all_event_shapes() {
    assert_eq!(
        normalize_macho_notification(
            "OnDockingAccepted",
            vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(60_003_760)]
        ),
        vec![PyRep::Integer(60_003_760)]
    );
    let du = normalize_macho_notification(
        "DoDestinyUpdate",
        vec![PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::String("OnDockingAccepted".into()),
            PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(60_003_760)]),
        ])])],
    );
    assert_eq!(du.len(), 1);

    assert!(normalize_macho_notification("OnChannelJoined", Vec::new()).is_empty());
    assert_eq!(
        normalize_macho_notification("OnSharedBookmarksFolderUpdated", vec![PyRep::Integer(42)]),
        vec![PyRep::Integer(42)]
    );
    assert!(normalize_macho_notification("OnCorporationRecruitmentAdChanged", vec![PyRep::Integer(1)]).is_empty());
    assert!(normalize_macho_notification("OnOrganizationContactsUpdated", Vec::new()).is_empty());
    assert_eq!(
        normalize_macho_notification("OnTargetClear", Vec::new()),
        vec![PyRep::List(Vec::new())]
    );
    let effect = normalize_macho_notification("OnGodmaShipEffect", vec![PyRep::Integer(100)]);
    assert_eq!(effect.len(), 11);
    assert_eq!(effect[0], PyRep::Integer(100));
    assert_eq!(
        normalize_macho_notification("OnHeatAdded", vec![PyRep::Real(0.5)]),
        vec![PyRep::Integer(1211), PyRep::Real(0.5)]
    );
    let mod_attr = normalize_macho_notification("OnModuleAttributeChanges", vec![PyRep::Integer(7)]);
    assert!(matches!(
        mod_attr.as_slice(),
        [PyRep::List(items)] if matches!(items.as_slice(), [PyRep::Tuple(t)] if t.len() == 8 && t[2] == PyRep::Integer(7))
    ));
    assert!(normalize_macho_notification("OnModuleUnfitted", Vec::new()).is_empty());
    assert_eq!(
        normalize_macho_notification("OnRemoteMessage", vec![PyRep::String("A".into()), PyRep::Dict(Vec::new())]),
        vec![PyRep::String("A".into()), PyRep::Dict(Vec::new()), PyRep::None]
    );
    let item_chg = normalize_macho_notification(
        "OnItemChange",
        vec![
            PyRep::keyval([("itemID", PyRep::Integer(10)), ("locationID", PyRep::Integer(60_003_760))]),
            PyRep::Dict(Vec::new()),
        ],
    );
    assert_eq!(item_chg.len(), 3);
    let inv_chg = normalize_macho_notification(
        "ProcessInventoryChange",
        vec![PyRep::Integer(1), PyRep::Integer(2), PyRep::Integer(4)],
    );
    assert!(matches!(
        inv_chg.as_slice(),
        [PyRep::List(items), PyRep::Dict(_), PyRep::Bool(true), PyRep::None]
            if items.first().and_then(|i| i.dict_get("itemID")) == Some(&PyRep::Integer(1))
    ));
    assert_eq!(normalize_macho_notification("OnNotificationReceived", Vec::new()).len(), 5);
    assert_eq!(
        normalize_macho_notification("OnTarget", vec![PyRep::Integer(99), PyRep::String("locking".into())]),
        vec![PyRep::String("locking".into()), PyRep::Integer(99), PyRep::None]
    );
    assert!(crate::router::client_compat::should_suppress_macho_notification(
        "OnTarget",
        &[PyRep::Integer(99), PyRep::String("locking".into())]
    ));
    assert!(!crate::router::client_compat::should_suppress_macho_notification(
        "OnTarget",
        &[PyRep::Integer(99), PyRep::String("locked".into())]
    ));
    assert_eq!(
        normalize_macho_notification("OnTarget", vec![PyRep::Integer(99), PyRep::String("locked".into())]),
        vec![PyRep::String("add".into()), PyRep::Integer(99), PyRep::None]
    );
    assert_eq!(
        normalize_macho_notification("OnTarget", vec![PyRep::Integer(99), PyRep::String("targetedBy".into())]),
        vec![PyRep::String("otheradd".into()), PyRep::Integer(99), PyRep::None]
    );
    assert_eq!(
        normalize_macho_notification("OnTarget", vec![PyRep::Integer(99), PyRep::String("unlock".into())]),
        vec![PyRep::String("lost".into()), PyRep::Integer(99), PyRep::None]
    );
}

#[test]
fn normalize_quasar_any_payload_coerces_types_and_wire_mismatches() {
    use eve_proto::quasar::{
        encode_bytes_field, encode_fixed32_field, encode_fixed64_field, encode_varint_field,
        DescriptorProto, FieldDescriptorProto, FieldLabel, FieldType, QuasarDescriptorRegistry,
    };

    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();
    let mk_field = |number: u32, name: &str, field_type: FieldType, type_name: &str| {
        FieldDescriptorProto {
            name: name.into(),
            number,
            label: FieldLabel::Optional,
            field_type,
            type_name: type_name.into(),
            oneof_index: None,
            json_name: name.into(),
        }
    };
    let mk_msg = |full_name: &str, fields: Vec<FieldDescriptorProto>| DescriptorProto {
        name: full_name.rsplit('.').next().unwrap_or(full_name).into(),
        full_name: full_name.into(),
        fields,
        nested_types: Vec::new(),
        enum_types: Vec::new(),
        oneofs: Vec::new(),
        is_map_entry: false,
    };

    for (name, fields) in [
        (
            "eve.semanticversion.Specification",
            vec![
                mk_field(1, "major", FieldType::Uint32, ""),
                mk_field(2, "minor", FieldType::Uint32, ""),
                mk_field(3, "patch", FieldType::Uint32, ""),
            ],
        ),
        (
            "eve_public.test.SintLeaf",
            vec![mk_field(1, "val", FieldType::Sint32, "")],
        ),
        (
            "eve_public.test.StrLeaf",
            vec![mk_field(1, "text", FieldType::String, "")],
        ),
        (
            "eve_public.test.FixedLeaf",
            vec![mk_field(1, "f64_val", FieldType::Fixed64, "")],
        ),
        (
            "eve_public.test.CoerceTarget",
            vec![
                mk_field(1, "sint_msg", FieldType::Message, "eve_public.test.SintLeaf"),
                mk_field(2, "str_msg", FieldType::Message, "eve_public.test.StrLeaf"),
                mk_field(3, "fixed_msg", FieldType::Message, "eve_public.test.FixedLeaf"),
                mk_field(4, "semver", FieldType::Message, "eve.semanticversion.Specification"),
                mk_field(5, "str_from_int", FieldType::String, ""),
                mk_field(6, "str_from_bad_utf8", FieldType::String, ""),
                mk_field(7, "bytes_from_int", FieldType::Bytes, ""),
                mk_field(8, "varint_from_msg", FieldType::Uint64, ""),
                mk_field(9, "varint_from_f64", FieldType::Uint64, ""),
                mk_field(10, "varint_from_f32", FieldType::Uint64, ""),
                mk_field(11, "f64_from_varint", FieldType::Fixed64, ""),
                mk_field(12, "f32_from_varint", FieldType::Fixed32, ""),
            ],
        ),
    ] {
        reg.messages_by_full_name.insert(name.into(), mk_msg(name, fields));
    }

    let mut any_title = GoogleAny {
        type_url: format!(
            "{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.character.title.definition.api.GetAllResponse"
        ),
        value: Vec::new(),
    };
    normalize_quasar_any_payload(&reg, &mut any_title);
    assert!(any_title
        .type_url
        .ends_with("eve.cosmetic.character.title.definition.api.ListDefinitionsResponse"));

    let mut any_cspa = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.mail.cspa.SetResponse"),
        value: vec![0x08, 0x01],
    };
    normalize_quasar_any_payload(&reg, &mut any_cspa);
    assert!(any_cspa.type_url.ends_with("google.protobuf.Empty"));

    let mut mismatched = Vec::new();
    encode_varint_field(1, 7, &mut mismatched);
    encode_varint_field(2, 99, &mut mismatched);
    encode_varint_field(3, 123, &mut mismatched);
    encode_bytes_field(4, &[0xFF, 0xFE], &mut mismatched);
    encode_varint_field(5, 42, &mut mismatched);
    encode_bytes_field(6, &[0xFF, 0xAB], &mut mismatched);
    encode_varint_field(7, 55, &mut mismatched);
    encode_bytes_field(8, &[0x08, 0x2A], &mut mismatched);
    encode_fixed64_field(9, 77, &mut mismatched);
    encode_fixed32_field(10, 88, &mut mismatched);
    encode_varint_field(11, 999, &mut mismatched);
    encode_varint_field(12, 111, &mut mismatched);

    let mut rsp = EvePublicResponse {
        status_code: 200,
        status_message: String::new(),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.CoerceTarget"),
            value: mismatched,
        }),
        ..Default::default()
    };
    normalize_quasar_client_response(&reg, &mut rsp);
    let payload = rsp.payload.unwrap();
    let decoded = reg
        .decode_dynamic("eve_public.test.CoerceTarget", &payload.value)
        .expect("coerced payload must decode cleanly against target descriptor");
    assert_eq!(decoded.fields.len(), 12);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quasar_grpc_error_branches_compression_and_unauthenticated_paths() {
    let ctx = Arc::new(ServerContext::with_repository(Arc::new(
        eve_db::WorldRepository::with_demo_seed(),
    )));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let srv_ctx = Arc::clone(&ctx);
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let c = Arc::clone(&srv_ctx);
            tokio::spawn(async move {
                let _ = handle_quasar_grpc_connection(sock, c).await;
            });
        }
    });

    let tcp = TcpStream::connect(addr).await.expect("connect");
    let (mut h2_client, h2_conn) = h2::client::handshake(tcp).await.expect("h2 handshake");
    tokio::spawn(async move {
        let _ = h2_conn.await;
    });

    // 1. Unknown gRPC method returns grpc-status = 12 (UNIMPLEMENTED)
    {
        let req = Request::builder().method("POST").uri("http://127.0.0.1/eve_public.gateway.Unknown/Method")
            .header("content-type", "application/grpc").body(()).unwrap();
        let (rsp_fut, _send) = h2_client.send_request(req, true).unwrap();
        let rsp = rsp_fut.await.unwrap();
        assert_eq!(rsp.status(), StatusCode::OK);
        assert_eq!(rsp.headers().get("grpc-status").and_then(|v| v.to_str().ok()), Some("12"));
    }

    // 2. Unauthenticated Requests/Send returns grpc-status = 16 (UNAUTHENTICATED)
    {
        let req = Request::builder().method("POST").uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
            .header("content-type", "application/grpc").header("authorization", "Bearer invalid.jwt.token").body(()).unwrap();
        let (rsp_fut, _send) = h2_client.send_request(req, true).unwrap();
        let rsp = rsp_fut.await.unwrap();
        assert_eq!(rsp.headers().get("grpc-status").and_then(|v| v.to_str().ok()), Some("16"));
    }

    // 3. Gzip and Deflate compressed frames on Events/Publish
    let now_u = ctx.now_unix();
    let dev_jwt = ctx.db.issue_gateway_dev_token(1, None, "tranquility", now_u + 3600, now_u).unwrap();
    for encoding in ["gzip", "deflate"] {
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Events/Publish")
            .header("content-type", "application/grpc")
            .header("grpc-encoding", encoding)
            .header("authorization", format!("Bearer {dev_jwt}"))
            .header("application_instance_uuid-bin", "AAAAAAAAAAAAAAAAAAAAAA==")
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
        let rsp = rsp_fut.await.unwrap();
        assert_eq!(rsp.status(), StatusCode::OK);

        let ev = EvePublicEvent {
            occurred: Some(GoogleTimestamp { seconds: 1_700_000_000, nanos: 0 }),
            uuid: vec![1u8; 16],
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.ui.OpenedEvent"),
                value: Vec::new(),
            }),
            ..Default::default()
        };
        let raw_proto = ev.encode_proto();
        let compressed_bytes = if encoding == "gzip" {
            let mut enc = GzEncoder::new(Vec::new(), Compression::default());
            enc.write_all(&raw_proto).unwrap();
            enc.finish().unwrap()
        } else {
            let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
            enc.write_all(&raw_proto).unwrap();
            enc.finish().unwrap()
        };
        let mut framed = BytesMut::with_capacity(5 + compressed_bytes.len());
        framed.extend_from_slice(&[1u8]);
        framed.extend_from_slice(&(compressed_bytes.len() as u32).to_be_bytes());
        framed.extend_from_slice(&compressed_bytes);
        send_body.send_data(framed.freeze(), true).unwrap();
        let mut body = rsp.into_body();
        assert_eq!(body.data().await.unwrap().unwrap().chunk(), &[0, 0, 0, 0, 0]);
    }
}

#[test]
fn normalize_notices_covers_all_remaining_event_and_coercion_branches() {
    use eve_proto::quasar::{
        encode_bytes_field, encode_fixed32_field, encode_varint_field,
        DescriptorProto, FieldDescriptorProto, FieldLabel, FieldType, QuasarDescriptorRegistry,
    };

    // 1. DoDestinyUpdate with OnDockingAccepted having < 3 args falls through to recursive tuple normalization
    let du_short = normalize_macho_notification(
        "DoDestinyUpdate",
        vec![PyRep::Tuple(vec![
            PyRep::String("OnDockingAccepted".into()),
            PyRep::Tuple(vec![PyRep::Integer(1)]),
        ])],
    );
    assert_eq!(
        du_short,
        vec![PyRep::Tuple(vec![
            PyRep::String("OnDockingAccepted".into()),
            PyRep::Tuple(vec![PyRep::Integer(1)]),
        ])]
    );

    // 2. OnSharedBookmarksFolderUpdated: list entries with existing bookmarkID, bare id, non-list, and already-tuple
    let sb = normalize_macho_notification(
        "OnSharedBookmarksFolderUpdated",
        vec![PyRep::List(vec![
            PyRep::Tuple(vec![
                PyRep::Integer(1),
                PyRep::Integer(10),
                PyRep::List(vec![
                    PyRep::keyval([("bookmarkID", PyRep::Integer(500))]),
                    PyRep::Integer(501),
                ]),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(2),
                PyRep::Integer(10),
                PyRep::Integer(999),
            ]),
            PyRep::Tuple(vec![
                PyRep::Integer(3),
                PyRep::Integer(10),
                PyRep::Tuple(vec![PyRep::List(Vec::new()), PyRep::List(Vec::new())]),
            ]),
        ])],
    );
    assert!(matches!(sb.as_slice(), [PyRep::List(entries)] if entries.len() == 3));
    if let PyRep::List(entries) = &sb[0] {
        if let PyRep::Tuple(parts) = &entries[0] {
            if let PyRep::Tuple(inner) = &parts[2] {
                if let PyRep::List(bms) = &inner[0] {
                    assert_eq!(bms[0].dict_get("bookmarkID"), Some(&PyRep::Integer(500)));
                    assert_eq!(bms[1].dict_get("bookmarkID"), Some(&PyRep::Integer(501)));
                } else {
                    panic!("expected bookmarks list");
                }
            } else {
                panic!("expected tuple wrapper");
            }
        }
        if let PyRep::Tuple(parts) = &entries[1] {
            assert_eq!(
                parts[2],
                PyRep::Tuple(vec![PyRep::List(Vec::new()), PyRep::List(Vec::new())])
            );
        }
    }

    // 3. OnBeanPrime: single list (Integer, Dict, KeyVal, ignored non-bean), single scalar, multi-arg
    let bp_list = normalize_macho_notification(
        "OnBeanPrime",
        vec![PyRep::List(vec![
            PyRep::Integer(111),
            PyRep::Dict(vec![
                (PyRep::String("errorID".into()), PyRep::Integer(222)),
                (PyRep::Integer(99), PyRep::Bool(true)),
            ]),
            PyRep::keyval([("errorKeyHash", PyRep::String("abc".into()))]),
            PyRep::None,
        ])],
    );
    if let [PyRep::List(beans)] = bp_list.as_slice() {
        assert_eq!(beans.len(), 3);
        assert_eq!(beans[0].dict_get("errorID"), Some(&PyRep::Integer(111)));
        assert_eq!(beans[1].dict_get("errorID"), Some(&PyRep::Integer(222)));
        assert_eq!(beans[2].dict_get("errorID"), Some(&PyRep::Integer(0)));
        assert_eq!(
            beans[2].dict_get("errorKeyHash"),
            Some(&PyRep::String("abc".into()))
        );
    } else {
        panic!("expected single list of beans");
    }
    let bp_single = normalize_macho_notification("OnBeanPrime", vec![PyRep::Integer(333)]);
    assert!(matches!(bp_single.as_slice(), [PyRep::List(v)] if v.len() == 1));
    let bp_multi = normalize_macho_notification(
        "OnBeanPrime",
        vec![PyRep::Integer(1), PyRep::Integer(2)],
    );
    assert!(matches!(bp_multi.as_slice(), [PyRep::List(v)] if v.len() == 2));

    // 4. OnOrganizationContactsUpdated: non-list second arg + standing field
    let org_non_list = normalize_macho_notification(
        "OnOrganizationContactsUpdated",
        vec![PyRep::Integer(98_000_001), PyRep::None],
    );
    assert_eq!(org_non_list, vec![PyRep::Integer(98_000_001), PyRep::List(Vec::new())]);
    let org_standing = normalize_macho_notification(
        "OnOrganizationContactsUpdated",
        vec![
            PyRep::Integer(98_000_001),
            PyRep::List(vec![PyRep::keyval([
                ("contactID", PyRep::Integer(77)),
                ("standing", PyRep::Real(10.0)),
            ])]),
        ],
    );
    assert_eq!(
        org_standing[1],
        PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(77),
            PyRep::Real(10.0),
            PyRep::Integer(0),
        ])])
    );

    // 5. OnGodmaShipEffect empty args
    assert!(normalize_macho_notification("OnGodmaShipEffect", Vec::new()).is_empty());

    // 6. OnModuleAttributeChanges: 1-arg List/Tuple with 8-tuple, 7-tuple, scalar; multi-arg
    let t8 = PyRep::Tuple(vec![PyRep::Integer(1); 8]);
    let t7 = PyRep::Tuple(vec![PyRep::Integer(2); 7]);
    let mac_list = normalize_macho_notification(
        "OnModuleAttributeChanges",
        vec![PyRep::List(vec![t8.clone(), t7.clone(), PyRep::Integer(3)])],
    );
    if let [PyRep::List(items)] = mac_list.as_slice() {
        assert_eq!(items.len(), 3);
        assert_eq!(items[0], t8);
        assert!(matches!(&items[1], PyRep::Tuple(v) if v.len() == 8 && v[7] == PyRep::None));
        assert!(matches!(&items[2], PyRep::Tuple(v) if v.len() == 8 && v[2] == PyRep::Integer(3)));
    } else {
        panic!("expected list");
    }
    let mac_tuple = normalize_macho_notification(
        "OnModuleAttributeChanges",
        vec![PyRep::Tuple(vec![t8.clone()])],
    );
    assert!(matches!(mac_tuple.as_slice(), [PyRep::List(items)] if items.len() == 1));
    let mac_multi = normalize_macho_notification("OnModuleAttributeChanges", vec![t7, t8]);
    assert!(matches!(mac_multi.as_slice(), [PyRep::List(items)] if items.len() == 2));

    // 7. OnItemChange: 1-arg Dict, 1-arg non-dict scalar, and empty args
    let ic_dict = normalize_macho_notification(
        "OnItemChange",
        vec![PyRep::Dict(vec![(
            PyRep::String("itemID".into()),
            PyRep::Integer(10),
        )])],
    );
    assert_eq!(ic_dict.len(), 1);
    assert_eq!(ic_dict[0].dict_get("itemID"), Some(&PyRep::Integer(10)));
    let ic_scalar = normalize_macho_notification("OnItemChange", vec![PyRep::Integer(5)]);
    assert_eq!(ic_scalar.len(), 1);
    assert!(normalize_macho_notification("OnItemChange", Vec::new()).is_empty());

    // 8. Coercion depth limits, UTF-8 / invalid UTF-8 leaf on String field, wire skips, unknown nested type, and empty-out leaf fallback
    let mut reg = QuasarDescriptorRegistry::empty_with_builtins();
    let mk_field = |number: u32, name: &str, field_type: FieldType, type_name: &str| {
        FieldDescriptorProto {
            name: name.into(),
            number,
            label: FieldLabel::Optional,
            field_type,
            type_name: type_name.into(),
            oneof_index: None,
            json_name: name.into(),
        }
    };
    let mk_msg = |full_name: &str, fields: Vec<FieldDescriptorProto>| DescriptorProto {
        name: full_name.rsplit('.').next().unwrap_or(full_name).into(),
        full_name: full_name.into(),
        fields,
        nested_types: Vec::new(),
        enum_types: Vec::new(),
        oneofs: Vec::new(),
        is_map_entry: false,
    };

    reg.messages_by_full_name.insert(
        "eve_public.test.StrLeaf".into(),
        mk_msg("eve_public.test.StrLeaf", vec![mk_field(1, "text", FieldType::String, "")]),
    );
    reg.messages_by_full_name.insert(
        "eve_public.test.UnknownChildHolder".into(),
        mk_msg(
            "eve_public.test.UnknownChildHolder",
            vec![mk_field(1, "missing", FieldType::Message, "eve_public.test.DoesNotExist")],
        ),
    );
    reg.messages_by_full_name.insert(
        "eve_public.test.MsgHolder".into(),
        mk_msg(
            "eve_public.test.MsgHolder",
            vec![
                mk_field(1, "sub", FieldType::Message, "eve_public.test.StrLeaf"),
                mk_field(2, "str_f", FieldType::String, ""),
                mk_field(3, "bytes_f", FieldType::Bytes, ""),
                mk_field(4, "u64_f", FieldType::Uint64, ""),
                mk_field(5, "f64_f", FieldType::Fixed64, ""),
                mk_field(6, "f32_f", FieldType::Fixed32, ""),
            ],
        ),
    );
    // Chain of 10 nested messages D0 -> D1 -> ... -> D9 -> StrLeaf to hit `depth > 8`
    for d in 0..=9 {
        let next = if d == 9 {
            "eve_public.test.StrLeaf".to_string()
        } else {
            format!("eve_public.test.D{}", d + 1)
        };
        reg.messages_by_full_name.insert(
            format!("eve_public.test.D{d}"),
            mk_msg(
                &format!("eve_public.test.D{d}"),
                vec![mk_field(1, "next", FieldType::Message, &next)],
            ),
        );
    }

    // Valid UTF-8 non-wire `"!"` (0x21 = field 4 Fixed64 truncated) -> coerce_raw_leaf_bytes_to_message valid UTF-8
    let mut any_valid_utf8_leaf = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.StrLeaf"),
        value: b"!".to_vec(),
    };
    normalize_quasar_any_payload(&reg, &mut any_valid_utf8_leaf);
    let dec = reg
        .decode_dynamic("eve_public.test.StrLeaf", &any_valid_utf8_leaf.value)
        .unwrap();
    assert_eq!(
        dec.fields.get(&1),
        Some(&eve_proto::quasar::DynamicValue::String("!".into()))
    );

    // Invalid UTF-8 non-wire `[0xFF, 0x21]` -> coerce_raw_leaf_bytes_to_message hex fallback
    let mut any_bad_utf8_leaf = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.StrLeaf"),
        value: vec![0xFF, 0x21],
    };
    normalize_quasar_any_payload(&reg, &mut any_bad_utf8_leaf);
    let dec = reg
        .decode_dynamic("eve_public.test.StrLeaf", &any_bad_utf8_leaf.value)
        .unwrap();
    assert_eq!(
        dec.fields.get(&1),
        Some(&eve_proto::quasar::DynamicValue::String("ff21".into()))
    );

    // UnknownChildHolder with varint and invalid wire triggers unknown sub_desc branches in varint & leaf coercion
    let mut unk_var = Vec::new();
    encode_varint_field(1, 5, &mut unk_var);
    let mut any_unk = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.UnknownChildHolder"),
        value: unk_var,
    };
    normalize_quasar_any_payload(&reg, &mut any_unk);
    let mut any_unk_leaf = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.UnknownChildHolder"),
        value: vec![0xFF, 0x21],
    };
    normalize_quasar_any_payload(&reg, &mut any_unk_leaf);

    // MsgHolder: empty sub-message bytes, Fixed32 on Message/String/Bytes/Fixed64, Varint & Fixed32 alongside bad field
    let mut msg_wire = Vec::new();
    encode_bytes_field(1, &[], &mut msg_wire);
    encode_fixed32_field(1, 1, &mut msg_wire);
    encode_fixed32_field(2, 2, &mut msg_wire);
    encode_fixed32_field(3, 3, &mut msg_wire);
    encode_varint_field(4, 44, &mut msg_wire);
    encode_fixed32_field(5, 5, &mut msg_wire);
    encode_fixed32_field(6, 66, &mut msg_wire);
    encode_varint_field(99, 1, &mut msg_wire); // force coercion pass
    let mut any_holder = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.MsgHolder"),
        value: msg_wire,
    };
    normalize_quasar_any_payload(&reg, &mut any_holder);
    assert!(reg
        .decode_dynamic("eve_public.test.MsgHolder", &any_holder.value)
        .is_ok());

    // Valid WireMessage with ONLY unknown field 99 on StrLeaf -> out.is_empty() fallback to coerce_raw_leaf_bytes_to_message
    let mut only_unknown = Vec::new();
    encode_varint_field(99, 7, &mut only_unknown);
    let mut any_fallback = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.StrLeaf"),
        value: only_unknown,
    };
    normalize_quasar_any_payload(&reg, &mut any_fallback);
    assert!(!any_fallback.value.is_empty());

    // Depth > 8 on bytes, varint, and raw leaf chains (D0 -> ... -> D9 -> StrLeaf)
    let mut deep_bytes = vec![0xFF, 0x21]; // raw leaf at bottom
    for _ in 0..=9 {
        let mut outer = Vec::new();
        encode_bytes_field(1, &deep_bytes, &mut outer);
        deep_bytes = outer;
    }
    // Add unknown field 99 at top so top-level fails decode_dynamic and enters coerce_quasar_bytes_to_message
    encode_varint_field(99, 1, &mut deep_bytes);
    let mut any_deep_bytes = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.D0"),
        value: deep_bytes,
    };
    normalize_quasar_any_payload(&reg, &mut any_deep_bytes);

    let mut deep_varint = Vec::new();
    encode_varint_field(1, 777, &mut deep_varint);
    let mut any_deep_varint = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.D0"),
        value: deep_varint,
    };
    normalize_quasar_any_payload(&reg, &mut any_deep_varint);

    let mut any_deep_leaf = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.D0"),
        value: vec![0xFF, 0x21],
    };
    normalize_quasar_any_payload(&reg, &mut any_deep_leaf);
}
