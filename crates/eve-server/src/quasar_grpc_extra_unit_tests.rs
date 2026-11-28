//! Deep branch and mutation-killing tests for `crate::quasar_grpc` covering JWT/SSO
//! authentication, HTTP/2 gRPC stream teardown, and C9 typed handler wire shapes.

use eve_proto::PyRep;

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
    for _ in 0..100 {
        if ctx.quasar_gateway.subscribers.read().unwrap().len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(ctx.quasar_gateway.subscribers.read().unwrap().len(), 1);

    // Resetting the consumer stream triggers `GrpcSubscriberGuard::drop` -> 0 subscribers.
    cons_send.send_reset(h2::Reason::CANCEL);
    drop(cons_rsp);
    for _ in 0..100 {
        if ctx.quasar_gateway.subscribers.read().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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

#[tokio::test]
async fn c9_audit_member_and_dev_index_direct_handler_wire_shapes() {
    let ctx = crate::router::ServerContext::new();
    let corp_id = 98_000_001_i64;
    eve_testkit::legal_fixture::seed_legal_corp_member(
        &ctx.db,
        90_000_001,
        corp_id,
        "C9 Audit Corp",
        "C9AC",
        false,
        u64::MAX,
        None,
    );
    eve_testkit::legal_fixture::seed_legal_corp_member(
        &ctx.db,
        90_000_002,
        corp_id,
        "C9 Audit Corp",
        "C9AC",
        false,
        0,
        None,
    );
    let uid = ctx.db.with_mut_snapshot(|snap| {
        let uid = snap.characters.get(&90_000_001).unwrap().user_id;
        snap.corp_governance.entry(corp_id).or_default().role_audit_log.push((
            90_000_002,
            90_000_001,
            0,
            1,
            1_800_025_000,
        ));
        let st = snap.system_states.entry(30_000_144).or_default();
        st.military_index = 4;
        st.industrial_index = 5;
        st.strategic_index = 1;
        uid
    });
    let mut sess = eve_machonet::SessionState::new(93_601);
    sess.user_id = Some(uid);
    sess.character_id = Some(90_000_001);
    ctx.sync_session(&mut sess);
    let kw = std::collections::HashMap::new();

    let audit = ctx
        .dispatch_rpc(
            &mut sess,
            "corpmgr",
            None,
            "AuditMember",
            &[PyRep::Integer(90_000_002)],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("corpmgr.AuditMember");
    let PyRep::Tuple(parts) = audit else {
        panic!("expected 2-tuple from AuditMember");
    };
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[0], parts[1]);
    assert!(matches!(&parts[0], PyRep::Rowset { rows, .. } if rows.len() == 1));

    let dev = ctx
        .dispatch_rpc(
            &mut sess,
            "devIndexManager",
            None,
            "GetAllDevelopmentIndices",
            &[],
            &kw,
            Some("machoNet:26000"),
        )
        .await
        .expect("devIndexManager.GetAllDevelopmentIndices");
    let PyRep::Dict(entries) = dev else {
        panic!("expected Dict from GetAllDevelopmentIndices");
    };
    let (_, entry) = entries
        .iter()
        .find(|(k, _)| *k == PyRep::Integer(30_000_144))
        .expect("system 30000144 dev index entry");
    assert_eq!(entry.dict_get("militaryPoints"), Some(&PyRep::Integer(4_000)));
    assert_eq!(entry.dict_get("industrialPoints"), Some(&PyRep::Integer(5_000)));
    assert_eq!(entry.dict_get("claimedFor"), Some(&PyRep::Integer(1)));
}
