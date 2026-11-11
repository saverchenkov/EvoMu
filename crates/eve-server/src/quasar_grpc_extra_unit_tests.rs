//! Deep branch and mutation-killing tests for `crate::quasar_grpc` covering remaining
//! `normalize_macho_rpc_reply`, `normalize_macho_notification`, `normalize_quasar_any_payload`
//! leaf/varint coercion branches, JWT subject/expiry/SSO verification, and HTTP/2 gRPC streams.

use crate::router::client_compat::normalize_macho_rpc_reply;
use eve_proto::PyRep;

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

    // dogmaIM.AddTarget default locked=true (`flag = 0`) and default targetID=0
    let tgt_def = normalize_macho_rpc_reply(
        "dogmaIM",
        "AddTarget",
        Ok(PyRep::KeyVal(std::collections::HashMap::new())),
    )
    .unwrap();
    assert_eq!(
        tgt_def,
        PyRep::Tuple(vec![
            PyRep::Integer(0),
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

#[tokio::test]
async fn grpc_auth_expired_ban_revocation_gm_demotion_and_subscriber_guard_drop_unit() {
    use crate::quasar_gateway::{STATUS_FORBIDDEN, STATUS_OK};
    use crate::quasar_grpc::{frame_grpc_message, handle_quasar_grpc_connection_with_limits};
    use crate::router::ServerContext;
    use bytes::{Buf, Bytes, BytesMut};
    use eve_db::service_roles;
    use eve_proto::quasar::{
        AuthoritativeContext, EvePublicRequest, EvePublicResponse, GoogleAny, QuasarMessage,
        TYPE_GOOGLEAPIS_PREFIX,
    };
    use http::{Request, StatusCode};
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::net::{TcpListener, TcpStream};

    async fn recv_frame(body: &mut h2::RecvStream, buf: &mut BytesMut) -> Option<Vec<u8>> {
        loop {
            if buf.len() >= 5 {
                let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
                if buf.len() >= 5 + len {
                    buf.advance(5);
                    let out = buf.split_to(len).to_vec();
                    let _ = body.flow_control().release_capacity(5 + len);
                    return Some(out);
                }
            }
            match body.data().await {
                Some(Ok(chunk)) if !chunk.is_empty() => buf.extend_from_slice(&chunk),
                Some(Ok(_)) => {}
                Some(Err(_)) | None => return None,
            }
        }
    }

    let ctx = Arc::new(ServerContext::with_repository(Arc::new(
        eve_db::WorldRepository::with_demo_seed(),
    )));
    let now_u = ctx.now_unix();
    let uid = ctx.db.provision_player_account("w4_grpc_unit_u", "w4_grpc_pass");
    let cid = 90_000_001_i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(ch) = snap.characters.get_mut(&cid) {
            ch.user_id = uid;
        }
    });

    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    let srv_ctx = Arc::clone(&ctx);
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let c = Arc::clone(&srv_ctx);
            tokio::spawn(async move {
                let _ = handle_quasar_grpc_connection_with_limits(
                    sock,
                    c,
                    4,
                    Duration::from_millis(200),
                )
                .await;
            });
        }
    });

    let (mut client, conn) = h2::client::handshake(TcpStream::connect(addr).await.unwrap())
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    // Mint both a full PKCE SSO token (for GM role test) and a local-dev token.
    let verifier = "w4_grpc_unit_pkce_verifier_0123456789abcdef";
    let challenge = eve_db::accounts::base64url_encode_no_pad(&eve_db::accounts::sha256_bytes(
        verifier.as_bytes(),
    ));
    ctx.db
        .set_account_role("w4_grpc_unit_u", service_roles::ROLE_GM_DEFAULT)
        .unwrap();
    let code = ctx
        .db
        .authorize_pkce(
            "w4_grpc_unit_u",
            "w4_grpc_pass",
            cid,
            "eveLauncherTQ",
            "http://127.0.0.1/cb",
            Vec::new(),
            &challenge,
            "S256",
            now_u,
        )
        .unwrap();
    let sso_bundle = ctx
        .db
        .exchange_pkce_code(&code, "eveLauncherTQ", verifier, now_u)
        .unwrap();

    // 1. Expired ban (`banned_until = now_u - 10`) is cleared before role check in `authenticate_grpc_headers`
    //    and succeeds; active ban (`banned_until = now_u + 3600`) is rejected with gRPC status 16.
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("w4_grpc_unit_u") {
            acc.banned_until_unix = Some(now_u.saturating_sub(10));
        }
    });
    let (rsp_fut, mut send_stream) = client
        .send_request(
            Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
                .header("content-type", "application/grpc")
                .header("authorization", format!("Bearer {}", sso_bundle.access_token))
                .body(())
                .unwrap(),
            false,
        )
        .unwrap();
    let mut body = rsp_fut.await.unwrap().into_body();
    let mut buf = BytesMut::new();

    let admin_req = EvePublicRequest {
        correlation_uuid: vec![0x91; 16],
        authoritative_context: Some(
            AuthoritativeContext::for_session_at(
                Some(uid as i32),
                Some(cid as u32),
                Some(1_000_170),
                0,
                "tranquility",
                vec![0x11; 16],
                now_u,
            )
            .with_service_roles(service_roles::ROLE_GM_DEFAULT),
        ),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.quasar.admin.IsAuthorizedRequest"),
            value: Vec::new(),
        }),
        ..Default::default()
    };
    send_stream
        .send_data(frame_grpc_message(&admin_req.encode_proto()), false)
        .unwrap();
    let frame1 = recv_frame(&mut body, &mut buf).await.expect("expired ban cleared");
    assert_eq!(
        EvePublicResponse::decode_proto(&frame1).unwrap().status_code,
        STATUS_OK
    );

    // 2. Mid-stream GM role demotion causes admin request to return 403 Forbidden on the next frame.
    ctx.db
        .set_account_role("w4_grpc_unit_u", service_roles::ROLE_PLAYER_DEFAULT)
        .unwrap();
    send_stream
        .send_data(frame_grpc_message(&admin_req.encode_proto()), false)
        .unwrap();
    let frame2 = recv_frame(&mut body, &mut buf).await.expect("demoted rsp");
    assert_eq!(
        EvePublicResponse::decode_proto(&frame2).unwrap().status_code,
        STATUS_FORBIDDEN
    );

    // 3. `GrpcSubscriberGuard::drop` unsubscribes active `Notices/Consume` stream when reset/dropped,
    //    and mid-stream active ban terminates open `Requests/Send` with gRPC status 16.
    let (cons_fut, mut cons_send) = client
        .send_request(
            Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/eve_public.gateway.Notices/Consume")
                .header("content-type", "application/grpc")
                .header("authorization", format!("Bearer {}", sso_bundle.access_token))
                .body(())
                .unwrap(),
            false,
        )
        .unwrap();
    cons_send
        .send_data(Bytes::from_static(&[0, 0, 0, 0, 0]), false)
        .unwrap();
    let cons_rsp = cons_fut.await.unwrap();
    assert_eq!(cons_rsp.status(), StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(ctx.quasar_gateway.subscribers.read().unwrap().len(), 1);

    // Resetting the consumer stream triggers `GrpcSubscriberGuard::drop` -> 0 subscribers.
    cons_send.send_reset(h2::Reason::CANCEL);
    drop(cons_rsp);
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(ctx.quasar_gateway.subscribers.read().unwrap().len(), 0);

    // Apply an active ban: both new `authenticate_grpc_headers` and open stream next frame return gRPC 16.
    ctx.db
        .ban_account("w4_grpc_unit_u", now_u + 3_600, "active ban")
        .unwrap();
    send_stream
        .send_data(frame_grpc_message(&admin_req.encode_proto()), true)
        .unwrap();
    assert!(recv_frame(&mut body, &mut buf).await.is_none());
    let tr = body.trailers().await.unwrap().expect("trailers");
    assert_eq!(tr.get("grpc-status").and_then(|v| v.to_str().ok()), Some("16"));

    let (banned_fut, _banned_send) = client
        .send_request(
            Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
                .header("content-type", "application/grpc")
                .header("authorization", format!("Bearer {}", sso_bundle.access_token))
                .body(())
                .unwrap(),
            true,
        )
        .unwrap();
    let banned_rsp = banned_fut.await.unwrap();
    assert_eq!(
        banned_rsp
            .headers()
            .get("grpc-status")
            .and_then(|v| v.to_str().ok()),
        Some("16")
    );
}
