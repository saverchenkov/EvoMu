//! Deep branch and mutation-killing tests for `crate::quasar_grpc` covering remaining
//! `normalize_macho_rpc_reply`, `normalize_macho_notification`, `normalize_quasar_any_payload`
//! leaf/varint coercion branches, JWT subject/expiry/SSO verification, and HTTP/2 gRPC streams.

use crate::quasar_grpc::*;
use crate::router::ServerContext;
use bytes::{Buf, Bytes, BytesMut};
use eve_proto::quasar::{
    encode_bytes_field, encode_fixed32_field, encode_fixed64_field, encode_varint_field,
    DescriptorProto, EvePublicEvent, EvePublicRequest, EvePublicResponse, FieldDescriptorProto,
    FieldLabel, FieldType, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry,
    TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use http::{Request, StatusCode};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

#[test]
fn normalize_macho_rpc_reply_exhaustive_fallback_and_non_empty_branches() {
    // structureDirectory.GetJumpBridgesWithMyAccess with missing keys -> defaults to Integer(0)
    let jb = normalize_macho_rpc_reply(
        "structureDirectory",
        "GetJumpBridgesWithMyAccess",
        Ok(PyRep::List(vec![PyRep::Dict(Vec::new())])),
    )
    .unwrap();
    let PyRep::Tuple(jb_parts) = jb else {
        panic!("expected tuple");
    };
    let PyRep::List(pairs) = &jb_parts[0] else {
        panic!("expected pairs list");
    };
    let PyRep::Tuple(pair0) = &pairs[0] else {
        panic!("expected pair tuple");
    };
    assert_eq!(pair0[0].dict_get("structureID"), Some(&PyRep::Integer(0)));
    assert_eq!(pair0[1].dict_get("solarSystemID"), Some(&PyRep::Integer(0)));

    // structureDirectory non-list passthrough
    assert_eq!(
        normalize_macho_rpc_reply(
            "structureDirectory",
            "GetJumpBridgesWithMyAccess",
            Ok(PyRep::None)
        )
        .unwrap(),
        PyRep::None
    );

    // XmppChatMgr without "channels" key passthrough
    let xmpp_no_ch = normalize_macho_rpc_reply(
        "XmppChatMgr",
        "ResyncSystemChannelAccess",
        Ok(PyRep::keyval([("other", PyRep::Integer(1))])),
    )
    .unwrap();
    assert_eq!(xmpp_no_ch.dict_get("other"), Some(&PyRep::Integer(1)));

    // blueprintManager.GetBlueprintDataByOwner non-empty list preserves custom entry
    let custom_bp = PyRep::keyval([("itemID", PyRep::Integer(999)), ("typeID", PyRep::Integer(689))]);
    let bp_non_empty = normalize_macho_rpc_reply(
        "blueprintManager",
        "GetBlueprintDataByOwner",
        Ok(PyRep::List(vec![custom_bp])),
    )
    .unwrap();
    let PyRep::Tuple(bp_tuple) = bp_non_empty else {
        panic!("expected tuple");
    };
    let PyRep::List(bp_items) = &bp_tuple[0] else {
        panic!("expected list");
    };
    assert_eq!(bp_items.len(), 1);
    assert_eq!(bp_items[0].dict_get("typeID"), Some(&PyRep::Integer(689)));

    // charMgr.GetOwnerNoteLabels with non-String item and 2-char string
    let notes = normalize_macho_rpc_reply(
        "charMgr",
        "GetOwnerNoteLabels",
        Ok(PyRep::List(vec![
            PyRep::String("N:".into()),
            PyRep::Integer(42),
        ])),
    )
    .unwrap();
    let PyRep::List(note_list) = notes else {
        panic!("expected list");
    };
    assert_eq!(
        note_list[0].dict_get("label"),
        Some(&PyRep::String("N:".into()))
    );
    assert_eq!(note_list[1], PyRep::Integer(42));

    // charUnboundMgr.GetCharacterInfo with empty KeyVal defaults to empty name & 0 ID, plus non-KeyVal passthrough
    let cinfo_default = normalize_macho_rpc_reply(
        "charUnboundMgr",
        "GetCharacterInfo",
        Ok(PyRep::KeyVal(std::collections::HashMap::new())),
    )
    .unwrap();
    let PyRep::List(cinfo_items) = cinfo_default else {
        panic!("expected list");
    };
    assert_eq!(
        cinfo_items[0].dict_get("characterName"),
        Some(&PyRep::String(String::new()))
    );
    assert_eq!(
        normalize_macho_rpc_reply("charUnboundMgr", "GetCharacterInfo", Ok(PyRep::Integer(7)))
            .unwrap(),
        PyRep::Integer(7)
    );

    // contractProxy.SearchContracts when item already has "contract" and "bids"
    let prewrapped = PyRep::keyval([
        ("contract", PyRep::keyval([("contractID", PyRep::Integer(55))])),
        ("bids", PyRep::List(Vec::new())),
    ]);
    let search_pre = normalize_macho_rpc_reply(
        "contractProxy",
        "SearchContracts",
        Ok(PyRep::List(vec![prewrapped.clone()])),
    )
    .unwrap();
    assert_eq!(
        search_pre.dict_get("contracts"),
        Some(&PyRep::List(vec![prewrapped]))
    );

    // corpmgr.AuditMember with non-empty Rowset and non-Rowset
    let non_empty_rs = PyRep::rowset(vec!["col"], vec![vec![PyRep::Integer(123)]]);
    let audit_rs =
        normalize_macho_rpc_reply("corpmgr", "AuditMember", Ok(non_empty_rs.clone())).unwrap();
    assert_eq!(audit_rs, PyRep::Tuple(vec![non_empty_rs.clone(), non_empty_rs]));
    let audit_other =
        normalize_macho_rpc_reply("corpmgr", "AuditMember", Ok(PyRep::Integer(9))).unwrap();
    assert_eq!(audit_other, PyRep::Tuple(vec![PyRep::Integer(9), PyRep::Integer(9)]));
}

#[test]
fn normalize_macho_rpc_reply_covers_dev_index_raffle_skill_station_and_store_variants() {
    // devIndexManager.GetAllDevelopmentIndices with alternate keys ("military", "industrial", "strategic")
    let dev = normalize_macho_rpc_reply(
        "devIndexManager",
        "GetAllDevelopmentIndices",
        Ok(PyRep::List(vec![PyRep::keyval([
            ("solarSystemID", PyRep::Integer(30_000_144)),
            ("military", PyRep::Integer(4)),
            ("industrial", PyRep::Integer(5)),
            ("strategic", PyRep::Integer(1)),
        ])])),
    )
    .unwrap();
    let PyRep::List(dev_items) = dev else {
        panic!("expected list");
    };
    assert_eq!(dev_items[0].dict_get("militaryPoints"), Some(&PyRep::Integer(4)));
    assert_eq!(dev_items[0].dict_get("industrialPoints"), Some(&PyRep::Integer(5)));
    assert_eq!(dev_items[0].dict_get("claimedFor"), Some(&PyRep::Integer(1)));

    // home_station.get_home_station_candidates empty list & non-Integer passthrough
    let hs_empty = normalize_macho_rpc_reply(
        "home_station",
        "get_home_station_candidates",
        Ok(PyRep::List(Vec::new())),
    )
    .unwrap();
    if let PyRep::List(items) = hs_empty {
        assert!(items.is_empty());
    }
    let hs_other = normalize_macho_rpc_reply(
        "home_station",
        "get_home_station_candidates",
        Ok(PyRep::List(vec![PyRep::String("custom".into())])),
    )
    .unwrap();
    assert_eq!(hs_other, PyRep::List(vec![PyRep::String("custom".into())]));

    // dogmaIM.AddTarget default locked=true and default targetID=0
    let tgt_def = normalize_macho_rpc_reply(
        "dogmaIM",
        "AddTarget",
        Ok(PyRep::KeyVal(std::collections::HashMap::new())),
    )
    .unwrap();
    assert_eq!(
        tgt_def,
        PyRep::Tuple(vec![
            PyRep::Integer(1),
            PyRep::List(vec![PyRep::Integer(0)])
        ])
    );

    // raffleProxy.GetMyRaffleHistory with pre-populated running_id vs raw item
    let raf = normalize_macho_rpc_reply(
        "raffleProxy",
        "GetMyRaffleHistory",
        Ok(PyRep::List(vec![
            PyRep::keyval([("running_id", PyRep::Integer(88))]),
            PyRep::Integer(0),
        ])),
    )
    .unwrap();
    if let PyRep::Tuple(parts) = raf {
        assert_eq!(parts[1], PyRep::Integer(2));
    }

    // skillMgr2.GetSkillQueueAndFreePoints with empty queue preserves empty queue
    let sq_empty = normalize_macho_rpc_reply(
        "skillMgr2",
        "GetSkillQueueAndFreePoints",
        Ok(PyRep::Tuple(vec![PyRep::None, PyRep::Integer(0)])),
    )
    .unwrap();
    if let PyRep::Tuple(parts) = sq_empty {
        if let PyRep::List(q) = &parts[0] {
            assert!(q.is_empty());
        }
    }

    // stationSvc.GetAllianceSystems empty list and non-Integer item
    let al_empty = normalize_macho_rpc_reply(
        "stationSvc",
        "GetAllianceSystems",
        Ok(PyRep::List(Vec::new())),
    )
    .unwrap();
    if let PyRep::List(items) = al_empty {
        assert!(items.is_empty());
    }
    let al_other = normalize_macho_rpc_reply(
        "stationSvc",
        "GetAllianceSystems",
        Ok(PyRep::List(vec![PyRep::String("keep".into())])),
    )
    .unwrap();
    assert_eq!(al_other, PyRep::List(vec![PyRep::String("keep".into())]));

    // warsInfoMgr.GetPublicWarInfo non-dict passthrough
    assert_eq!(
        normalize_macho_rpc_reply("warsInfoMgr", "GetPublicWarInfo", Ok(PyRep::Integer(5)))
            .unwrap(),
        PyRep::Integer(5)
    );

    // storeManager get_products and get_offers recursive KeyVal/Tuple/Dict conversion
    for method in ["get_products", "get_offers"] {
        let nested = PyRep::Tuple(vec![
            PyRep::keyval([("k", PyRep::List(vec![PyRep::keyval([("inner", PyRep::Integer(1))])]))]),
            PyRep::Dict(vec![(
                PyRep::String("d".into()),
                PyRep::keyval([("v", PyRep::Integer(2))]),
            )]),
        ]);
        let conv = normalize_macho_rpc_reply("storeManager", method, Ok(nested)).unwrap();
        let PyRep::Tuple(parts) = conv else {
            panic!("expected tuple");
        };
        assert!(matches!(parts[0], PyRep::Dict(_)));
        assert!(matches!(parts[1], PyRep::Dict(_)));
    }
}

#[test]
fn normalize_macho_notification_exhaustive_edge_cases() {
    // OnDockingAccepted with < 3 args returns unchanged
    assert_eq!(
        normalize_macho_notification("OnDockingAccepted", vec![PyRep::Integer(1)]),
        vec![PyRep::Integer(1)]
    );

    // DoDestinyUpdate with non-OnDockingAccepted tuple and scalar
    let du = normalize_macho_notification(
        "DoDestinyUpdate",
        vec![
            PyRep::Tuple(vec![
                PyRep::String("SetBallPosition".into()),
                PyRep::Tuple(vec![PyRep::Integer(1), PyRep::Real(0.0)]),
            ]),
            PyRep::Integer(99),
        ],
    );
    assert_eq!(du.len(), 2);

    // OnChannelJoined with explicit arg
    assert_eq!(
        normalize_macho_notification("OnChannelJoined", vec![PyRep::String("corp".into())]),
        vec![PyRep::String("corp".into())]
    );

    // OnOrganizationContactsUpdated with populated list using relationshipID fallback
    let contacts = normalize_macho_notification(
        "OnOrganizationContactsUpdated",
        vec![
            PyRep::Integer(98_000_002),
            PyRep::List(vec![PyRep::keyval([
                ("contactID", PyRep::Integer(90_000_005)),
                ("relationshipID", PyRep::Real(-5.0)),
                ("labelMask", PyRep::Integer(3)),
            ])]),
        ],
    );
    assert_eq!(contacts[0], PyRep::Integer(98_000_002));
    assert_eq!(
        contacts[1],
        PyRep::List(vec![PyRep::Tuple(vec![
            PyRep::Integer(90_000_005),
            PyRep::Real(-5.0),
            PyRep::Integer(3),
        ])])
    );

    // OnTargetClear non-empty passthrough
    assert_eq!(
        normalize_macho_notification("OnTargetClear", vec![PyRep::Integer(1)]),
        vec![PyRep::Integer(1)]
    );

    // OnGodmaShipEffect with 11 args passthrough
    let eleven = vec![PyRep::Integer(1); 11];
    assert_eq!(
        normalize_macho_notification("OnGodmaShipEffect", eleven.clone()),
        eleven
    );

    // OnHeatRemoved with 1 arg vs 2 args
    assert_eq!(
        normalize_macho_notification("OnHeatRemoved", vec![PyRep::Real(0.0)]),
        vec![PyRep::Integer(1211), PyRep::Real(0.0)]
    );
    assert_eq!(
        normalize_macho_notification("OnHeatRemoved", vec![PyRep::Integer(1), PyRep::Real(0.0)]),
        vec![PyRep::Integer(1), PyRep::Real(0.0)]
    );

    // OnModuleUnfitted preserves args
    assert_eq!(
        normalize_macho_notification("OnModuleUnfitted", vec![PyRep::Integer(1), PyRep::Integer(2)]),
        vec![PyRep::Integer(1), PyRep::Integer(2)]
    );

    // OnRemoteMessage with 3 args passthrough
    assert_eq!(
        normalize_macho_notification("OnRemoteMessage", vec![PyRep::None; 3]).len(),
        3
    );

    // ProcessInventoryChange with 1 arg passthrough; OnNotificationReceived with 5 args passthrough
    assert_eq!(
        normalize_macho_notification("ProcessInventoryChange", vec![PyRep::None]).len(),
        1
    );
    assert_eq!(
        normalize_macho_notification("OnNotificationReceived", vec![PyRep::None; 5]).len(),
        5
    );

    // OnTarget variants: "clear", custom verb, reversed args, and len != 2
    assert_eq!(
        normalize_macho_notification("OnTarget", vec![PyRep::Integer(10), PyRep::String("clear".into())]),
        vec![PyRep::String("clear".into()), PyRep::Integer(10), PyRep::None]
    );
    assert_eq!(
        normalize_macho_notification("OnTarget", vec![PyRep::Integer(10), PyRep::String("other".into())]),
        vec![PyRep::String("other".into()), PyRep::Integer(10), PyRep::None]
    );
    assert_eq!(
        normalize_macho_notification("OnTarget", vec![PyRep::String("add".into()), PyRep::Integer(10)]),
        vec![PyRep::String("add".into()), PyRep::Integer(10), PyRep::None]
    );
    assert_eq!(
        normalize_macho_notification("OnTarget", vec![PyRep::Integer(10)]),
        vec![PyRep::Integer(10)]
    );

    // Default wildcard notification passthrough
    assert_eq!(
        normalize_macho_notification("OnAccountChange", vec![PyRep::Bool(true)]),
        vec![PyRep::Bool(true)]
    );
}

#[test]
fn normalize_quasar_any_payload_covers_leaf_and_nested_coercion_branches() {
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
            "eve_public.test.Sint64Leaf",
            vec![mk_field(2, "s64", FieldType::Sint64, "")],
        ),
        (
            "eve_public.test.BytesLeaf",
            vec![mk_field(1, "raw", FieldType::Bytes, "")],
        ),
        (
            "eve_public.test.Fixed32Leaf",
            vec![mk_field(1, "f32_val", FieldType::Fixed32, "")],
        ),
        (
            "eve_public.test.NestedMsgLeaf",
            vec![mk_field(1, "inner", FieldType::Message, "eve_public.test.BytesLeaf")],
        ),
        (
            "eve_public.test.Container",
            vec![
                mk_field(1, "s64_msg", FieldType::Message, "eve_public.test.Sint64Leaf"),
                mk_field(2, "bytes_msg", FieldType::Message, "eve_public.test.BytesLeaf"),
                mk_field(3, "f32_msg", FieldType::Message, "eve_public.test.Fixed32Leaf"),
                mk_field(4, "nested_msg", FieldType::Message, "eve_public.test.NestedMsgLeaf"),
                mk_field(5, "raw_bytes", FieldType::Bytes, ""),
                mk_field(6, "f64_direct", FieldType::Fixed64, ""),
                mk_field(7, "f32_direct", FieldType::Fixed32, ""),
                mk_field(8, "valid_str", FieldType::String, ""),
            ],
        ),
    ] {
        reg.messages_by_full_name
            .insert(name.into(), mk_msg(name, fields));
    }

    let mut wire = Vec::new();
    encode_varint_field(1, 100, &mut wire);
    encode_varint_field(2, 200, &mut wire);
    encode_varint_field(3, 300, &mut wire);
    encode_varint_field(4, 400, &mut wire);
    encode_bytes_field(5, b"hello", &mut wire);
    encode_fixed64_field(6, 600, &mut wire);
    encode_fixed32_field(7, 700, &mut wire);
    encode_bytes_field(8, b"utf8-ok", &mut wire);
    encode_varint_field(99, 999, &mut wire); // unknown field number -> triggers coercion & skipped

    let mut any_container = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.Container"),
        value: wire,
    };
    normalize_quasar_any_payload(&reg, &mut any_container);
    let dec = reg
        .decode_dynamic("eve_public.test.Container", &any_container.value)
        .expect("coerced container must decode");
    assert_eq!(dec.fields.len(), 8);

    // Raw leaf bytes fallback (invalid wire format `[0xFF; 12]`) into NestedMsgLeaf -> BytesLeaf
    let mut any_raw_leaf = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.NestedMsgLeaf"),
        value: vec![0xFF; 12],
    };
    normalize_quasar_any_payload(&reg, &mut any_raw_leaf);
    assert!(reg
        .decode_dynamic("eve_public.test.NestedMsgLeaf", &any_raw_leaf.value)
        .is_ok());
}

#[tokio::test]
async fn quasar_grpc_jwt_variants_pings_and_invalid_proto_streams() {
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

    // 1. Events/Ping and Notices/Ping
    for uri in [
        "http://127.0.0.1/eve_public.gateway.Events/Ping",
        "http://127.0.0.1/eve_public.gateway.Notices/Ping",
    ] {
        let req = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/grpc")
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
        send_body.send_data(Bytes::from_static(&[0, 0, 0, 0, 0]), true).unwrap();
        let rsp = rsp_fut.await.unwrap();
        assert_eq!(rsp.status(), StatusCode::OK);
        let mut body = rsp.into_body();
        let chunk = body.data().await.unwrap().unwrap();
        assert_eq!(chunk.chunk(), &[0, 0, 0, 0, 0]);
    }

    // 2. Notices/Consume unauthenticated returns 16
    {
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Notices/Consume")
            .header("content-type", "application/grpc")
            .body(())
            .unwrap();
        let (rsp_fut, _send) = h2_client.send_request(req, true).unwrap();
        let rsp = rsp_fut.await.unwrap();
        assert_eq!(
            rsp.headers().get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("16")
        );
    }

    // 3. `authorization_token` header + lowercase `bearer ` + 16-byte `application_instance_uuid`
    let now_u = ctx.now_unix();
    let dev_jwt_main = ctx
        .db
        .issue_gateway_dev_token(1, None, "tranquility", now_u + 3600, now_u)
        .unwrap();
    {
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
            .header("content-type", "application/grpc")
            .header("authorization_token", format!("bearer {dev_jwt_main}"))
            .header("application_instance_uuid", "0123456789abcdef")
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
        let rsp = rsp_fut.await.unwrap();
        assert_eq!(rsp.status(), StatusCode::OK);

        // Send invalid protobuf bytes (wire type 7 = 0x0F) -> trailers must have grpc-status = 3 (INVALID_ARGUMENT)
        let mut bad_frame = BytesMut::new();
        bad_frame.extend_from_slice(&[0, 0, 0, 0, 2, 0x0F, 0xFF]);
        send_body.send_data(bad_frame.freeze(), true).unwrap();
        let mut body = rsp.into_body();
        let _ = body.data().await;
        let trailers = body.trailers().await.unwrap().unwrap();
        assert_eq!(
            trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("3")
        );
    }

    // 4. Invalid protobuf on Events/Publish -> trailers must have grpc-status = 3 (INVALID_ARGUMENT)
    {
        let now_u = ctx.now_unix();
        let dev_jwt = ctx
            .db
            .issue_gateway_dev_token(1, None, "tranquility", now_u + 3600, now_u)
            .unwrap();
        let req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Events/Publish")
            .header("content-type", "application/grpc")
            .header("authorization", format!("Bearer {dev_jwt}"))
            .body(())
            .unwrap();
        let (rsp_fut, mut send_body) = h2_client.send_request(req, false).unwrap();
        let rsp = rsp_fut.await.unwrap();
        let mut bad_frame = BytesMut::new();
        bad_frame.extend_from_slice(&[0, 0, 0, 0, 2, 0x0F, 0xFF]);
        send_body.send_data(bad_frame.freeze(), true).unwrap();
        let mut body = rsp.into_body();
        let _ = body.data().await;
        let trailers = body.trailers().await.unwrap().unwrap();
        assert_eq!(
            trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("3")
        );
    }
    let _ = (
        EvePublicEvent::default(),
        EvePublicRequest::default(),
        EvePublicResponse::default(),
        GoogleTimestamp::default(),
    );
}
