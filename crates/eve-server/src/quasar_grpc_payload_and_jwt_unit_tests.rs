//! Semantic unit tests split from `quasar_grpc_extra_unit_tests`.

use crate::quasar_grpc::*;
use crate::router::client_compat::normalize_macho_notification;
use crate::router::ServerContext;
use bytes::{Buf, Bytes, BytesMut};
use eve_proto::quasar::{
    DescriptorProto, EvePublicEvent, EvePublicRequest, EvePublicResponse, FieldDescriptorProto,
    FieldLabel, FieldType, GoogleAny, GoogleTimestamp, QuasarDescriptorRegistry, WireField,
    WireMessage, WireValue, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use http::{Request, StatusCode};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

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

    // OnGodmaShipEffect with valid 11 args passthrough
    let valid_ft = PyRep::Integer(133_500_000_000_000_000);
    let valid_env = PyRep::Tuple(vec![
        PyRep::Integer(1),
        PyRep::None,
        PyRep::Integer(1),
        PyRep::None,
        PyRep::None,
        PyRep::List(Vec::new()),
        PyRep::Integer(1),
    ]);
    let eleven = vec![
        PyRep::Integer(1),
        PyRep::Integer(1),
        valid_ft.clone(),
        PyRep::Integer(1),
        PyRep::Integer(1),
        valid_env,
        valid_ft,
        PyRep::Real(0.0),
        PyRep::Integer(0),
        PyRep::None,
        PyRep::None,
    ];
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

    let wire = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(100) },
            WireField { field_number: 2, value: WireValue::Varint(200) },
            WireField { field_number: 3, value: WireValue::Varint(300) },
            WireField { field_number: 4, value: WireValue::Varint(400) },
            WireField { field_number: 5, value: WireValue::LengthDelimited(b"hello".to_vec()) },
            WireField { field_number: 6, value: WireValue::Fixed64(600) },
            WireField { field_number: 7, value: WireValue::Fixed32(700) },
            WireField { field_number: 8, value: WireValue::LengthDelimited(b"utf8-ok".to_vec()) },
            WireField { field_number: 99, value: WireValue::Varint(999) }, // unknown field number -> triggers coercion & skipped
        ],
    }
    .encode();

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
