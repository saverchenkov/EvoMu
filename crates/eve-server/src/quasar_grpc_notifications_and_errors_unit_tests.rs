//! Semantic unit tests split from `quasar_grpc_unit_tests`.

use crate::quasar_grpc::*;
use crate::router::ServerContext;
use bytes::{Buf, BytesMut};
use eve_proto::quasar::{
    EvePublicEvent, EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage,
    TYPE_GOOGLEAPIS_PREFIX,
};
use flate2::write::{GzEncoder, ZlibEncoder};
use flate2::Compression;
use http::{Request, StatusCode};
use std::io::Write;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

#[test]
fn normalize_quasar_any_payload_coerces_types_and_wire_mismatches() {
    use eve_proto::quasar::{
        DescriptorProto, FieldDescriptorProto, FieldLabel, FieldType, QuasarDescriptorRegistry,
        WireField, WireMessage, WireValue,
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

    let mismatched = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::Varint(7) },
            WireField { field_number: 2, value: WireValue::Varint(99) },
            WireField { field_number: 3, value: WireValue::Varint(123) },
            WireField { field_number: 4, value: WireValue::LengthDelimited(vec![0xFF, 0xFE]) },
            WireField { field_number: 5, value: WireValue::Varint(42) },
            WireField { field_number: 6, value: WireValue::LengthDelimited(vec![0xFF, 0xAB]) },
            WireField { field_number: 7, value: WireValue::Varint(55) },
            WireField { field_number: 8, value: WireValue::LengthDelimited(vec![0x08, 0x2A]) },
            WireField { field_number: 9, value: WireValue::Fixed64(77) },
            WireField { field_number: 10, value: WireValue::Fixed32(88) },
            WireField { field_number: 11, value: WireValue::Varint(999) },
            WireField { field_number: 12, value: WireValue::Varint(111) },
        ],
    }
    .encode();

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
fn normalize_quasar_any_payload_covers_depth_limit_and_utf8_coercion_branches() {
    use eve_proto::quasar::{
        DescriptorProto, FieldDescriptorProto, FieldLabel, FieldType, QuasarDescriptorRegistry,
        WireField, WireMessage, WireValue,
    };

    // Coercion depth limits, UTF-8 / invalid UTF-8 leaf on String field, wire skips, unknown nested type, and empty-out leaf fallback
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
    let unk_var = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::Varint(5),
        }],
    }
    .encode();
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
    let msg_wire = WireMessage {
        fields: vec![
            WireField { field_number: 1, value: WireValue::LengthDelimited(Vec::new()) },
            WireField { field_number: 1, value: WireValue::Fixed32(1) },
            WireField { field_number: 2, value: WireValue::Fixed32(2) },
            WireField { field_number: 3, value: WireValue::Fixed32(3) },
            WireField { field_number: 4, value: WireValue::Varint(44) },
            WireField { field_number: 5, value: WireValue::Fixed32(5) },
            WireField { field_number: 6, value: WireValue::Fixed32(66) },
            WireField { field_number: 99, value: WireValue::Varint(1) }, // force coercion pass
        ],
    }
    .encode();
    let mut any_holder = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.MsgHolder"),
        value: msg_wire,
    };
    normalize_quasar_any_payload(&reg, &mut any_holder);
    assert!(reg
        .decode_dynamic("eve_public.test.MsgHolder", &any_holder.value)
        .is_ok());

    // Valid WireMessage with ONLY unknown field 99 on StrLeaf -> out.is_empty() fallback to coerce_raw_leaf_bytes_to_message
    let only_unknown = WireMessage {
        fields: vec![WireField {
            field_number: 99,
            value: WireValue::Varint(7),
        }],
    }
    .encode();
    let mut any_fallback = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.StrLeaf"),
        value: only_unknown,
    };
    normalize_quasar_any_payload(&reg, &mut any_fallback);
    assert!(!any_fallback.value.is_empty());

    // Depth > 8 on bytes, varint, and raw leaf chains (D0 -> ... -> D9 -> StrLeaf)
    let mut deep_bytes = vec![0xFF, 0x21]; // raw leaf at bottom
    for _ in 0..=9 {
        deep_bytes = WireMessage {
            fields: vec![WireField {
                field_number: 1,
                value: WireValue::LengthDelimited(deep_bytes),
            }],
        }
        .encode();
    }
    // Add unknown field 99 at top so top-level fails decode_dynamic and enters coerce_quasar_bytes_to_message
    deep_bytes.extend_from_slice(
        &WireMessage {
            fields: vec![WireField {
                field_number: 99,
                value: WireValue::Varint(1),
            }],
        }
        .encode(),
    );
    let mut any_deep_bytes = GoogleAny {
        type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.D0"),
        value: deep_bytes,
    };
    normalize_quasar_any_payload(&reg, &mut any_deep_bytes);

    let deep_varint = WireMessage {
        fields: vec![WireField {
            field_number: 1,
            value: WireValue::Varint(777),
        }],
    }
    .encode();
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
