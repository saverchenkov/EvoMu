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
        vec![PyRep::String("add".into()), PyRep::Integer(99), PyRep::None]
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

#[tokio::test]
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
