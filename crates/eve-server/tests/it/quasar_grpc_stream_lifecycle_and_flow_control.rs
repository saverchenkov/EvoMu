//! End-to-end integration tests for Quasar Public Gateway (`PublicGatewayRouter` & `quasar_grpc`)
//! subscriber lifecycle cleanup on disconnect/drop, active HTTP/2 gRPC stream auth/token/session/role
//! revocation enforcement, and bounded HTTP/2 window, frame, and concurrency limits.

use bytes::{Buf, Bytes, BytesMut};
use eve_db::accounts::sha256_bytes;
use eve_db::{base64url_encode_no_pad, service_roles, ServerClock};
use eve_machonet::{
    placebo_crypto_hash, CryptoChallengePacket, CryptoHandshakeAck, CryptoHandshakeResult,
    CryptoRequestPacket, CryptoServerHandshake, MachoAddress, MachoPacket, MachoPacketType,
    SessionState, VersionExchange,
};
use eve_proto::marshal;
use eve_proto::quasar::{
    encode_varint_field, AuthoritativeContext, EvePublicEvent, EvePublicNotice, EvePublicRequest,
    EvePublicResponse, GoogleAny, GoogleTimestamp, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use eve_server::macho_listener::handle_macho_client;
use eve_server::quasar_gateway::{
    NoticeScope, MAX_GATEWAY_SUBSCRIBERS, STATUS_FORBIDDEN, STATUS_OK,
};
use eve_server::quasar_grpc::{
    frame_grpc_message, handle_quasar_grpc_connection, handle_quasar_grpc_connection_with_limits,
    H2_INITIAL_CONNECTION_WINDOW_BYTES, H2_INITIAL_STREAM_WINDOW_BYTES, H2_MAX_CONCURRENT_STREAMS,
    H2_MAX_FRAME_SIZE_BYTES, H2_MAX_HEADER_LIST_BYTES, MAX_GRPC_MESSAGE_BYTES,
};
use eve_server::router::ServerContext;
use eve_server::sso::handle_http_request;
use http::{Request, StatusCode};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

async fn write_macho_frame(stream: &mut TcpStream, rep: &PyRep) {
    let raw = marshal::encode(rep);
    stream
        .write_all(&(raw.len() as u32).to_le_bytes())
        .await
        .expect("write frame len");
    stream.write_all(&raw).await.expect("write frame body");
}

async fn read_macho_frame(stream: &mut TcpStream) -> PyRep {
    let len = stream.read_u32_le().await.expect("read frame len") as usize;
    let mut buf = vec![0u8; len];
    stream
        .read_exact(&mut buf)
        .await
        .expect("read frame payload");
    marshal::decode(&buf).expect("marshal decode")
}

async fn complete_macho_handshake(
    stream: &mut TcpStream,
    username: &str,
    password: &str,
) -> CryptoHandshakeAck {
    let _srv_hello = read_macho_frame(stream).await;
    write_macho_frame(stream, &VersionExchange::default().to_pyrep()).await;
    write_macho_frame(stream, &CryptoRequestPacket::placebo().to_pyrep()).await;
    let ok_cc = read_macho_frame(stream).await;
    assert_eq!(ok_cc.as_str(), Some("OK CC"));

    let challenge = CryptoChallengePacket::new_password_login(username, password).to_pyrep();
    write_macho_frame(stream, &challenge).await;
    let pw_ver = read_macho_frame(stream).await;
    assert_eq!(pw_ver.as_i64(), Some(2));
    let srv_hs_rep = read_macho_frame(stream).await;
    let srv_hs =
        CryptoServerHandshake::from_pyrep(&srv_hs_rep).expect("decode CryptoServerHandshake");

    let hs_res = CryptoHandshakeResult {
        challenge_response_hash: placebo_crypto_hash(&srv_hs.server_challenge),
        func_output: Vec::new(),
        func_result: PyRep::Integer(0),
    };
    write_macho_frame(stream, &hs_res.to_pyrep()).await;
    let ack_rep = read_macho_frame(stream).await;
    let ack = CryptoHandshakeAck::from_pyrep(&ack_rep).expect("decode CryptoHandshakeAck");
    let _init_notif = read_macho_frame(stream).await;
    let _sess_change = read_macho_frame(stream).await;
    ack
}

async fn spawn_grpc_server(ctx: Arc<ServerContext>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        while let Ok((sock, _)) = listener.accept().await {
            let c = Arc::clone(&ctx);
            tokio::spawn(async move {
                let _ = handle_quasar_grpc_connection(sock, c).await;
            });
        }
    });
    addr
}

async fn recv_next_grpc_frame(
    recv_body: &mut h2::RecvStream,
    recv_buf: &mut BytesMut,
) -> Option<Vec<u8>> {
    loop {
        if recv_buf.len() >= 5 {
            let len =
                u32::from_be_bytes([recv_buf[1], recv_buf[2], recv_buf[3], recv_buf[4]]) as usize;
            if recv_buf.len() >= 5 + len {
                recv_buf.advance(5);
                let raw = recv_buf.split_to(len).to_vec();
                let _ = recv_body.flow_control().release_capacity(5 + len);
                return Some(raw);
            }
        }
        match recv_body.data().await {
            Some(Ok(chunk)) => {
                if !chunk.is_empty() {
                    recv_buf.extend_from_slice(&chunk);
                }
            }
            Some(Err(_)) | None => return None,
        }
    }
}

fn auth_ctx_for(
    uid: u64,
    cid: Option<i64>,
    corp_id: Option<i64>,
    role_mask: u64,
    now_u: u64,
) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        Some(uid as i32),
        cid.map(|c| c as u32),
        corp_id.map(|c| c as u32),
        0,
        "tranquility",
        vec![0x55; 16],
        now_u,
    )
    .with_service_roles(role_mask)
}

#[tokio::test]
async fn macho_disconnect_and_server_disconnect_session_evict_gateway_subscribers_and_stream_receivers() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    let macho_listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.expect("bind macho"));
    let macho_addr = macho_listener.local_addr().expect("macho addr");

    // 1. Live Macho TCP client subscribes via `publicGatewaySvc.Subscribe`, then disconnects:
    //    both `subscribers` and `stream_receivers` are cleaned up and dead fan-out stops.
    let ctx_m1 = Arc::clone(&ctx);
    let macho_l1 = Arc::clone(&macho_listener);
    let macho_task_1 = tokio::spawn(async move {
        let (sock, _) = macho_l1.accept().await.expect("accept macho 1");
        handle_macho_client(sock, ctx_m1).await
    });

    let mut m_stream = TcpStream::connect(macho_addr).await.expect("connect macho 1");
    let ack = complete_macho_handshake(&mut m_stream, "demo", "demo-secret").await;
    let sub_call = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id: ack.client_id,
            call_id: 1,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: 888444,
            service: Some("publicGatewaySvc".into()),
            call_id: 1,
        },
        user_id: Some(ack.user_id),
        payload: PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::String("Subscribe".into()),
            PyRep::Tuple(Vec::new()),
            PyRep::Dict(Vec::new()),
        ])))]),
        named_payload: HashMap::new(),
    };
    write_macho_frame(&mut m_stream, &sub_call.to_pyrep()).await;
    let sub_rsp = MachoPacket::from_pyrep(&read_macho_frame(&mut m_stream).await)
        .expect("Subscribe CallRsp");
    assert_eq!(sub_rsp.packet_type, MachoPacketType::CallRsp);
    assert_eq!(ctx.quasar_gateway.subscribers.read().unwrap().len(), 1);
    assert_eq!(ctx.quasar_gateway.stream_receivers.read().unwrap().len(), 1);

    drop(m_stream);
    macho_task_1
        .await
        .expect("macho task 1 join")
        .expect("macho task 1 ok");

    assert!(ctx.quasar_gateway.subscribers.read().unwrap().is_empty());
    assert!(ctx.quasar_gateway.stream_receivers.read().unwrap().is_empty());

    let (_, delivered_after_disconnect) = ctx
        .quasar_gateway
        .publish_notice(
            NoticeScope::User(ack.user_id as i32),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.chat.local.api.JoinNotice"),
                value: Vec::new(),
            },
            ctx.now_unix(),
        )
        .expect("publish_notice after disconnect");
    assert_eq!(delivered_after_disconnect, 0);

    // 2. Aborting `handle_macho_client` mid-flight still runs RAII `SessionCleanupGuard::drop`.
    let ctx_m2 = Arc::clone(&ctx);
    let macho_task_2 = tokio::spawn(async move {
        let (sock, _) = macho_listener.accept().await.expect("accept macho 2");
        handle_macho_client(sock, ctx_m2).await
    });
    let mut m_stream_2 = TcpStream::connect(macho_addr).await.expect("connect macho 2");
    let ack2 = complete_macho_handshake(&mut m_stream_2, "demo", "demo-secret").await;
    let sub_call_2 = MachoPacket {
        packet_type: MachoPacketType::CallReq,
        source: MachoAddress::Client {
            client_id: ack2.client_id,
            call_id: 2,
            service: None,
        },
        destination: MachoAddress::Node {
            node_id: 888444,
            service: Some("publicGatewaySvc".into()),
            call_id: 2,
        },
        user_id: Some(ack2.user_id),
        payload: PyRep::Tuple(vec![PyRep::SubStream(Box::new(PyRep::Tuple(vec![
            PyRep::String("Subscribe".into()),
            PyRep::Tuple(Vec::new()),
            PyRep::Dict(Vec::new()),
        ])))]),
        named_payload: HashMap::new(),
    };
    write_macho_frame(&mut m_stream_2, &sub_call_2.to_pyrep()).await;
    let _ = read_macho_frame(&mut m_stream_2).await;
    assert_eq!(ctx.quasar_gateway.subscribers.read().unwrap().len(), 1);
    assert_eq!(ctx.quasar_gateway.stream_receivers.read().unwrap().len(), 1);

    macho_task_2.abort();
    let _ = macho_task_2.await;
    assert!(ctx.quasar_gateway.subscribers.read().unwrap().is_empty());
    assert!(ctx.quasar_gateway.stream_receivers.read().unwrap().is_empty());
    ctx.db.release_online_lock(ack2.user_id, 0);
    drop(m_stream_2);

    // 3. `authentication.Logout`, `disconnect_session`, `release_user_bindings`, and > MAX_GATEWAY_SUBSCRIBERS
    //    subscribe/disconnect churn leave zero leaked subscribers or stream receivers.
    let empty_kw = HashMap::new();
    let mut logout_sess = SessionState::new(71_001);
    logout_sess.user_id = Some(1);
    ctx.dispatch_rpc(
        &mut logout_sess,
        "publicGatewaySvc",
        None,
        "Subscribe",
        &[],
        &empty_kw,
        None,
    )
    .await
    .expect("Subscribe before Logout");
    assert!(ctx.quasar_gateway.has_subscriber(71_001));
    ctx.dispatch_rpc(
        &mut logout_sess,
        "authentication",
        None,
        "Logout",
        &[],
        &empty_kw,
        None,
    )
    .await
    .expect("Logout");
    assert!(!ctx.quasar_gateway.has_subscriber(71_001));
    assert!(ctx.quasar_gateway.stream_receivers.read().unwrap().is_empty());

    let churn_cycles = (MAX_GATEWAY_SUBSCRIBERS as u64) + 150;
    for i in 0..churn_cycles {
        let sid = 80_000 + i;
        let mut s = SessionState::new(sid);
        s.user_id = Some(1);
        ctx.quasar_gateway
            .dispatch_public_gateway_rpc(&s, "Subscribe", &[], &empty_kw, ctx.now_unix())
            .expect("Subscribe")
            .expect("Subscribe ok");
        ctx.disconnect_session(sid, "ClientDisconnected", "churn test");
    }
    assert_eq!(ctx.quasar_gateway.subscribers.read().unwrap().len(), 0);
    assert_eq!(ctx.quasar_gateway.stream_receivers.read().unwrap().len(), 0);
}

#[tokio::test]
async fn active_quasar_grpc_streams_enforce_ban_suspension_kick_token_revocation_and_gm_role_demotion() {
    let ctx = Arc::new(ServerContext::new());
    let vclock = Arc::new(eve_db::VirtualServerClock::with_fixed_base(1_800_000_000));
    ctx.set_clock(Arc::clone(&vclock) as Arc<dyn ServerClock>);
    let now_u = ctx.now_unix();

    let sso_uid = ctx.db.provision_player_account("grpc_rev_sso", "pass_sso");
    let ban_uid = ctx.db.provision_player_account("grpc_rev_ban", "pass_ban");
    let susp_uid = ctx.db.provision_player_account("grpc_rev_susp", "pass_susp");
    let gm_uid = ctx.db.provision_player_account("grpc_rev_gm", "pass_gm");
    let disp_uid = ctx.db.provision_player_account("grpc_rev_disp", "pass_disp");

    let (sso_cid, ban_cid, susp_cid, gm_cid, disp_cid) = (
        93_100_001i64,
        93_100_002i64,
        93_100_003i64,
        93_100_004i64,
        93_100_005i64,
    );
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(gm_acc) = snap.accounts.get_mut("grpc_rev_gm") {
            gm_acc.role = service_roles::ROLE_GM_DEFAULT;
        }
        for (cid, name, uid) in [
            (sso_cid, "RevSsoPilot", sso_uid),
            (ban_cid, "RevBanPilot", ban_uid),
            (susp_cid, "RevSuspPilot", susp_uid),
            (gm_cid, "RevGmPilot", gm_uid),
            (disp_cid, "RevDispPilot", disp_uid),
        ] {
            let mut ch = snap.characters[&90_000_001].clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.user_id = uid;
            ch.corporation_id = 1_000_009;
            snap.characters.insert(cid, ch);
        }
    });

    let addr = spawn_grpc_server(Arc::clone(&ctx)).await;
    let (mut client, conn) = h2::client::handshake(TcpStream::connect(addr).await.unwrap())
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    // Helper to mint a full PKCE OAuth2 token bundle (`access_token` + `refresh_token`).
    let mint_sso = |username: &str, password: &str, cid: i64| {
        let verifier = format!("pkce_verifier_for_{username}_0123456789abcdef");
        let challenge = base64url_encode_no_pad(&sha256_bytes(verifier.as_bytes()));
        let code = ctx
            .db
            .authorize_pkce(
                username,
                password,
                cid,
                "eveLauncherTQ",
                "http://127.0.0.1/cb",
                Vec::new(),
                &challenge,
                "S256",
                ctx.now_unix(),
            )
            .expect("authorize_pkce");
        ctx.db
            .exchange_pkce_code(&code, "eveLauncherTQ", &verifier, ctx.now_unix())
            .expect("exchange_pkce_code")
    };

    let make_names_req = |uid: u64, cid: i64, corr_byte: u8, now_ts: u64| {
        let mut uid_payload = Vec::new();
        encode_varint_field(1, uid, &mut uid_payload);
        EvePublicRequest {
            issued: Some(GoogleTimestamp {
                seconds: now_ts as i64,
                nanos: 0,
            }),
            correlation_uuid: vec![corr_byte; 16],
            authoritative_context: Some(auth_ctx_for(uid, Some(cid), Some(1_000_009), 0, now_ts)),
            payload: Some(GoogleAny {
                type_url: format!(
                    "{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserRequest"
                ),
                value: uid_payload,
            }),
            ..Default::default()
        }
    };

    // -----------------------------------------------------------------------
    // 1. Mid-stream OAuth2 SSO token revocation via `POST /v2/oauth/revoke`
    // -----------------------------------------------------------------------
    {
        let sso_bundle = mint_sso("grpc_rev_sso", "pass_sso", sso_cid);

        let req_send = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
            .header("content-type", "application/grpc")
            .header("authorization", format!("Bearer {}", sso_bundle.access_token))
            .body(())
            .unwrap();
        let (send_rsp_fut, mut req_writer) = client.send_request(req_send, false).unwrap();
        let mut req_body = send_rsp_fut.await.unwrap().into_body();
        let mut req_buf = BytesMut::new();

        let req_consume = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Notices/Consume")
            .header("content-type", "application/grpc")
            .header("authorization", format!("Bearer {}", sso_bundle.access_token))
            .body(())
            .unwrap();
        let (cons_rsp_fut, mut cons_writer) = client.send_request(req_consume, false).unwrap();
        cons_writer
            .send_data(Bytes::from_static(&[0, 0, 0, 0, 0]), false)
            .unwrap();
        let mut cons_body = cons_rsp_fut.await.unwrap().into_body();
        let mut cons_buf = BytesMut::new();

        // Pre-revocation request succeeds (200 OK).
        req_writer
            .send_data(
                frame_grpc_message(&make_names_req(sso_uid, sso_cid, 1, now_u).encode_proto()),
                false,
            )
            .unwrap();
        let raw_ok = recv_next_grpc_frame(&mut req_body, &mut req_buf)
            .await
            .expect("pre-revoke response");
        assert_eq!(
            EvePublicResponse::decode_proto(&raw_ok).unwrap().status_code,
            STATUS_OK
        );

        // Pre-revocation notice arrives on `Notices/Consume`.
        tokio::time::sleep(Duration::from_millis(25)).await;
        let (_, delivered_pre) = ctx
            .quasar_gateway
            .publish_notice(
                NoticeScope::Character(sso_cid as u32),
                GoogleAny {
                    type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.PreRevokeNotice"),
                    value: vec![0x08, 0x01],
                },
                now_u,
            )
            .unwrap();
        assert_eq!(delivered_pre, 1);
        let raw_notice = recv_next_grpc_frame(&mut cons_body, &mut cons_buf)
            .await
            .expect("pre-revoke notice");
        let dec_notice = EvePublicNotice::decode_proto(&raw_notice).unwrap();
        assert!(dec_notice
            .payload
            .unwrap()
            .type_url
            .ends_with("eve_public.test.PreRevokeNotice"));

        // Revoke the SSO refresh token (and paired access token JTI) via `/v2/oauth/revoke`.
        let mut form_hdrs = HashMap::new();
        form_hdrs.insert(
            "content-type".to_string(),
            "application/x-www-form-urlencoded".to_string(),
        );
        let revoke_http = handle_http_request(
            &ctx,
            "POST",
            "/v2/oauth/revoke",
            &form_hdrs,
            format!("token={}", sso_bundle.refresh_token).as_bytes(),
        );
        assert_eq!(revoke_http.status_code, 200);

        // Next request on the SAME open `Requests/Send` stream must fail with gRPC 16 (UNAUTHENTICATED).
        req_writer
            .send_data(
                frame_grpc_message(&make_names_req(sso_uid, sso_cid, 2, now_u).encode_proto()),
                true,
            )
            .unwrap();
        assert!(
            recv_next_grpc_frame(&mut req_body, &mut req_buf)
                .await
                .is_none(),
            "revoked token must not emit another EvePublicResponse frame"
        );
        let req_trailers = req_body.trailers().await.unwrap().expect("req trailers");
        assert_eq!(
            req_trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("16")
        );

        // Publishing a notice after token revocation must NOT forward the notice; `Notices/Consume`
        // terminates with gRPC 16 (UNAUTHENTICATED) and unsubscribes.
        let _ = ctx.quasar_gateway.publish_notice(
            NoticeScope::Character(sso_cid as u32),
            GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.PostRevokeSecretNotice"),
                value: vec![0x08, 0x02],
            },
            now_u,
        );
        assert!(
            tokio::time::timeout(
                Duration::from_secs(60),
                recv_next_grpc_frame(&mut cons_body, &mut cons_buf),
            )
            .await
            .expect("consume stream closes promptly")
            .is_none(),
            "revoked stream must never leak post-revocation notices"
        );
        let cons_trailers = cons_body
            .trailers()
            .await
            .unwrap()
            .expect("consume trailers");
        assert_eq!(
            cons_trailers.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("16")
        );
    }

    // -----------------------------------------------------------------------
    // 2. Mid-stream account ban & suspension across `Requests/Send`, `Notices/Consume`, `Events/Publish`
    // -----------------------------------------------------------------------
    for (uid, cid, uname, is_ban) in [
        (ban_uid, ban_cid, "grpc_rev_ban", true),
        (susp_uid, susp_cid, "grpc_rev_susp", false),
    ] {
        let dev_jwt = ctx
            .db
            .issue_gateway_dev_token(uid, Some(cid), "tranquility", now_u + 3600, now_u)
            .unwrap();

        let (req_fut, mut req_send) = client
            .send_request(
                Request::builder()
                    .method("POST")
                    .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
                    .header("content-type", "application/grpc")
                    .header("authorization", format!("Bearer {dev_jwt}"))
                    .body(())
                    .unwrap(),
                false,
            )
            .unwrap();
        let mut req_body = req_fut.await.unwrap().into_body();
        let mut req_buf = BytesMut::new();

        let (cons_fut, mut cons_send) = client
            .send_request(
                Request::builder()
                    .method("POST")
                    .uri("http://127.0.0.1/eve_public.gateway.Notices/Consume")
                    .header("content-type", "application/grpc")
                    .header("authorization", format!("Bearer {dev_jwt}"))
                    .body(())
                    .unwrap(),
                false,
            )
            .unwrap();
        cons_send
            .send_data(Bytes::from_static(&[0, 0, 0, 0, 0]), false)
            .unwrap();
        let mut cons_body = cons_fut.await.unwrap().into_body();
        let mut cons_buf = BytesMut::new();

        let (ev_fut, mut ev_send) = client
            .send_request(
                Request::builder()
                    .method("POST")
                    .uri("http://127.0.0.1/eve_public.gateway.Events/Publish")
                    .header("content-type", "application/grpc")
                    .header("authorization", format!("Bearer {dev_jwt}"))
                    .body(())
                    .unwrap(),
                false,
            )
            .unwrap();
        let mut ev_body = ev_fut.await.unwrap().into_body();
        let mut ev_buf = BytesMut::new();

        // Initial event ack succeeds before restriction.
        let sample_ev = EvePublicEvent {
            uuid: vec![0x11; 16],
            authoritative_context: Some(auth_ctx_for(uid, Some(cid), Some(1_000_009), 0, now_u)),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.ui.OpenedEvent"),
                value: Vec::new(),
            }),
            ..Default::default()
        };
        ev_send
            .send_data(frame_grpc_message(&sample_ev.encode_proto()), false)
            .unwrap();
        let ack_pre = recv_next_grpc_frame(&mut ev_body, &mut ev_buf)
            .await
            .expect("pre-ban event ack");
        assert!(ack_pre.is_empty());

        tokio::time::sleep(Duration::from_millis(20)).await;
        let events_before = ctx.quasar_gateway.published_events.read().unwrap().len();

        // Ban or suspend the account and release user bindings (mirroring `/ban` and `/suspend`).
        if is_ban {
            ctx.db
                .ban_account(uname, now_u + 86_400, "EULA violation")
                .expect("ban_account");
        } else {
            ctx.db
                .suspend_account(uname, now_u + 3_600, "Investigation hold")
                .expect("suspend_account");
        }
        ctx.release_user_bindings(uid);

        // `Notices/Consume` wakes up immediately (sub-millisecond via `unsubscribe_user`) and emits gRPC 16!
        assert!(
            tokio::time::timeout(
                Duration::from_millis(250),
                recv_next_grpc_frame(&mut cons_body, &mut cons_buf),
            )
            .await
            .expect("Notices/Consume must terminate immediately on ban/suspension")
            .is_none()
        );
        let cons_tr = cons_body.trailers().await.unwrap().expect("cons trailers");
        assert_eq!(
            cons_tr.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("16")
        );

        // Next `Requests/Send` frame terminates with gRPC 16.
        req_send
            .send_data(
                frame_grpc_message(&make_names_req(uid, cid, 9, now_u).encode_proto()),
                true,
            )
            .unwrap();
        assert!(recv_next_grpc_frame(&mut req_body, &mut req_buf).await.is_none());
        let req_tr = req_body.trailers().await.unwrap().expect("req trailers");
        assert_eq!(
            req_tr.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("16")
        );

        // Next `Events/Publish` frame terminates with gRPC 16 without recording the event.
        ev_send
            .send_data(frame_grpc_message(&sample_ev.encode_proto()), true)
            .unwrap();
        assert!(recv_next_grpc_frame(&mut ev_body, &mut ev_buf).await.is_none());
        let ev_tr = ev_body.trailers().await.unwrap().expect("ev trailers");
        assert_eq!(
            ev_tr.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("16")
        );
        assert_eq!(
            ctx.quasar_gateway.published_events.read().unwrap().len(),
            events_before,
            "restricted account must not append events on open stream"
        );
    }

    // -----------------------------------------------------------------------
    // 3. Mid-stream GM role demotion on open `Requests/Send` and `Events/Publish`
    // -----------------------------------------------------------------------
    {
        let gm_sso = mint_sso("grpc_rev_gm", "pass_gm", gm_cid);

        let (gm_req_fut, mut gm_req_send) = client
            .send_request(
                Request::builder()
                    .method("POST")
                    .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
                    .header("content-type", "application/grpc")
                    .header("authorization", format!("Bearer {}", gm_sso.access_token))
                    .body(())
                    .unwrap(),
                false,
            )
            .unwrap();
        let mut gm_req_body = gm_req_fut.await.unwrap().into_body();
        let mut gm_req_buf = BytesMut::new();

        let (gm_ev_fut, mut gm_ev_send) = client
            .send_request(
                Request::builder()
                    .method("POST")
                    .uri("http://127.0.0.1/eve_public.gateway.Events/Publish")
                    .header("content-type", "application/grpc")
                    .header("authorization", format!("Bearer {}", gm_sso.access_token))
                    .body(())
                    .unwrap(),
                false,
            )
            .unwrap();
        let mut gm_ev_body = gm_ev_fut.await.unwrap().into_body();
        let mut gm_ev_buf = BytesMut::new();

        let admin_req = EvePublicRequest {
            correlation_uuid: vec![0x31; 16],
            authoritative_context: Some(auth_ctx_for(
                gm_uid,
                Some(gm_cid),
                Some(1_000_009),
                service_roles::ROLE_GM_DEFAULT,
                now_u,
            )),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.quasar.admin.IsAuthorizedRequest"),
                value: Vec::new(),
            }),
            ..Default::default()
        };
        gm_req_send
            .send_data(frame_grpc_message(&admin_req.encode_proto()), false)
            .unwrap();
        let pre_demote_raw = recv_next_grpc_frame(&mut gm_req_body, &mut gm_req_buf)
            .await
            .expect("GM admin rsp before demotion");
        assert_eq!(
            EvePublicResponse::decode_proto(&pre_demote_raw)
                .unwrap()
                .status_code,
            STATUS_OK
        );

        let gm_event = EvePublicEvent {
            uuid: vec![0x44; 16],
            authoritative_context: Some(auth_ctx_for(
                gm_uid,
                Some(gm_cid),
                Some(1_000_009),
                service_roles::ROLE_GM_DEFAULT,
                now_u,
            )),
            payload: Some(GoogleAny {
                type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.app.eveonline.ui.OpenedEvent"),
                value: Vec::new(),
            }),
            ..Default::default()
        };
        gm_ev_send
            .send_data(frame_grpc_message(&gm_event.encode_proto()), false)
            .unwrap();
        assert!(
            recv_next_grpc_frame(&mut gm_ev_body, &mut gm_ev_buf)
                .await
                .expect("GM event ack before demotion")
                .is_empty()
        );

        // Demote account from GM to ordinary player while BOTH streams remain open!
        ctx.db
            .set_account_role("grpc_rev_gm", service_roles::ROLE_PLAYER_DEFAULT)
            .expect("demote GM role");

        // Next administrative request on the SAME open stream must return 403 Forbidden,
        // while ordinary player requests continue to return 200 OK!
        let mut post_demote_admin = admin_req.clone();
        post_demote_admin.correlation_uuid = vec![0x32; 16];
        gm_req_send
            .send_data(frame_grpc_message(&post_demote_admin.encode_proto()), false)
            .unwrap();
        let post_demote_raw = recv_next_grpc_frame(&mut gm_req_body, &mut gm_req_buf)
            .await
            .expect("post-demote admin rsp");
        assert_eq!(
            EvePublicResponse::decode_proto(&post_demote_raw)
                .unwrap()
                .status_code,
            STATUS_FORBIDDEN
        );

        gm_req_send
            .send_data(
                frame_grpc_message(&make_names_req(gm_uid, gm_cid, 0x33, now_u).encode_proto()),
                true,
            )
            .unwrap();
        let player_ok_raw = recv_next_grpc_frame(&mut gm_req_body, &mut gm_req_buf)
            .await
            .expect("player request still allowed after GM demotion");
        assert_eq!(
            EvePublicResponse::decode_proto(&player_ok_raw)
                .unwrap()
                .status_code,
            STATUS_OK
        );

        // Events published on the SAME open `Events/Publish` stream after GM demotion have
        // `service_role_mask` stamped with the demoted `ROLE_PLAYER_DEFAULT` (no retained GM bits),
        // and claiming a mismatched identity in `AuthoritativeContext` is rejected with gRPC 7!
        let mut post_demote_ev = gm_event.clone();
        post_demote_ev.uuid = vec![0x45; 16];
        gm_ev_send
            .send_data(frame_grpc_message(&post_demote_ev.encode_proto()), false)
            .unwrap();
        assert!(
            recv_next_grpc_frame(&mut gm_ev_body, &mut gm_ev_buf)
                .await
                .expect("player event ack after demotion")
                .is_empty()
        );
        let stamped_role = ctx
            .quasar_gateway
            .published_events
            .read()
            .unwrap()
            .iter()
            .rev()
            .find(|e| e.uuid == vec![0x45; 16])
            .and_then(|e| e.authoritative_context.as_ref())
            .map(|ac| ac.service_role_mask);
        assert_eq!(
            stamped_role,
            Some(service_roles::ROLE_PLAYER_DEFAULT),
            "open Events/Publish stream must reflect live demoted player role"
        );

        let mut spoofed_ev = gm_event;
        spoofed_ev.uuid = vec![0x46; 16];
        spoofed_ev.authoritative_context = Some(auth_ctx_for(
            gm_uid + 99,
            Some(gm_cid),
            Some(1_000_009),
            service_roles::ROLE_PLAYER_DEFAULT,
            now_u,
        ));
        gm_ev_send
            .send_data(frame_grpc_message(&spoofed_ev.encode_proto()), true)
            .unwrap();
        assert!(recv_next_grpc_frame(&mut gm_ev_body, &mut gm_ev_buf).await.is_none());
        let gm_ev_tr = gm_ev_body.trailers().await.unwrap().expect("gm ev trailers");
        assert_eq!(
            gm_ev_tr.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("7")
        );
    }

    // -----------------------------------------------------------------------
    // 4. Mid-stream session displacement & JWT clock expiration on open streams
    // -----------------------------------------------------------------------
    {
        ctx.db
            .acquire_online_lock(disp_uid, 95_001, Some(disp_cid), true, ctx.now_unix())
            .expect("initial online lock");
        let short_jwt = ctx
            .db
            .issue_gateway_dev_token(
                disp_uid,
                Some(disp_cid),
                "tranquility",
                ctx.now_unix() + 120,
                ctx.now_unix(),
            )
            .unwrap();
        let long_jwt = ctx
            .db
            .issue_gateway_dev_token(
                disp_uid,
                Some(disp_cid),
                "tranquility",
                ctx.now_unix() + 7_200,
                ctx.now_unix(),
            )
            .unwrap();

        let (disp_fut, mut disp_send) = client
            .send_request(
                Request::builder()
                    .method("POST")
                    .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
                    .header("content-type", "application/grpc")
                    .header("authorization", format!("Bearer {long_jwt}"))
                    .body(())
                    .unwrap(),
                false,
            )
            .unwrap();
        let mut disp_body = disp_fut.await.unwrap().into_body();
        let mut disp_buf = BytesMut::new();

        disp_send
            .send_data(
                frame_grpc_message(
                    &make_names_req(disp_uid, disp_cid, 0x41, ctx.now_unix()).encode_proto(),
                ),
                false,
            )
            .unwrap();
        let raw_1 = recv_next_grpc_frame(&mut disp_body, &mut disp_buf)
            .await
            .expect("initial disp rsp");
        assert_eq!(
            EvePublicResponse::decode_proto(&raw_1).unwrap().status_code,
            STATUS_OK
        );

        // Concurrent login displaces session 95_001 with 95_002 -> old open gRPC stream is revoked!
        ctx.db
            .acquire_online_lock(disp_uid, 95_002, Some(disp_cid), true, ctx.now_unix())
            .expect("displacing online lock");

        disp_send
            .send_data(
                frame_grpc_message(
                    &make_names_req(disp_uid, disp_cid, 0x42, ctx.now_unix()).encode_proto(),
                ),
                true,
            )
            .unwrap();
        assert!(recv_next_grpc_frame(&mut disp_body, &mut disp_buf).await.is_none());
        let disp_tr = disp_body.trailers().await.unwrap().expect("disp trailers");
        assert_eq!(
            disp_tr.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("16")
        );

        // Open a new stream bound to the active session 95_002 with `short_jwt` (exp = now + 120s),
        // then advance virtual clock past `exp` -> next frame fails with gRPC 16!
        let (exp_fut, mut exp_send) = client
            .send_request(
                Request::builder()
                    .method("POST")
                    .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
                    .header("content-type", "application/grpc")
                    .header("authorization", format!("Bearer {short_jwt}"))
                    .body(())
                    .unwrap(),
                false,
            )
            .unwrap();
        let mut exp_body = exp_fut.await.unwrap().into_body();
        let mut exp_buf = BytesMut::new();

        exp_send
            .send_data(
                frame_grpc_message(
                    &make_names_req(disp_uid, disp_cid, 0x51, ctx.now_unix()).encode_proto(),
                ),
                false,
            )
            .unwrap();
        assert!(recv_next_grpc_frame(&mut exp_body, &mut exp_buf).await.is_some());

        vclock.advance_secs(180);

        exp_send
            .send_data(
                frame_grpc_message(
                    &make_names_req(disp_uid, disp_cid, 0x52, ctx.now_unix()).encode_proto(),
                ),
                true,
            )
            .unwrap();
        assert!(recv_next_grpc_frame(&mut exp_body, &mut exp_buf).await.is_none());
        let exp_tr = exp_body.trailers().await.unwrap().expect("exp trailers");
        assert_eq!(
            exp_tr.get("grpc-status").and_then(|v| v.to_str().ok()),
            Some("16")
        );
    }
}

#[tokio::test]
async fn quasar_grpc_enforces_bounded_http2_settings_concurrency_cap_and_subscriber_drop_cleanup() {
    let ctx = Arc::new(ServerContext::new());
    crate::common::pin_clock(&ctx);

    // 1. Raw HTTP/2 wire inspection: verify bounded SETTINGS and connection WINDOW_UPDATE.
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let srv_ctx = Arc::clone(&ctx);
        let srv_task = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.expect("accept");
            let _ = handle_quasar_grpc_connection(sock, srv_ctx).await;
        });

        let mut raw = TcpStream::connect(addr).await.expect("connect");
        raw.write_all(b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n")
            .await
            .unwrap();
        // Client empty SETTINGS frame.
        raw.write_all(&[0, 0, 0, 0x04, 0x00, 0, 0, 0, 0])
            .await
            .unwrap();
        raw.flush().await.unwrap();

        let mut settings_seen = HashMap::<u16, u32>::new();
        let mut conn_window_increment: Option<u32> = None;

        for _ in 0..6 {
            let mut hdr = [0u8; 9];
            let Ok(Ok(_)) =
                tokio::time::timeout(Duration::from_millis(200), raw.read_exact(&mut hdr)).await
            else {
                break;
            };
            let payload_len =
                ((hdr[0] as usize) << 16) | ((hdr[1] as usize) << 8) | (hdr[2] as usize);
            let frame_type = hdr[3];
            let flags = hdr[4];
            let stream_id = u32::from_be_bytes([hdr[5] & 0x7f, hdr[6], hdr[7], hdr[8]]);
            let mut payload = vec![0u8; payload_len];
            if payload_len > 0 {
                raw.read_exact(&mut payload).await.unwrap();
            }
            if frame_type == 0x04 && (flags & 0x01) == 0 && stream_id == 0 {
                for kv in payload.chunks_exact(6) {
                    let id = u16::from_be_bytes([kv[0], kv[1]]);
                    let val = u32::from_be_bytes([kv[2], kv[3], kv[4], kv[5]]);
                    settings_seen.insert(id, val);
                }
                // Acknowledge server SETTINGS so server flushes connection WINDOW_UPDATE if queued.
                raw.write_all(&[0, 0, 0, 0x04, 0x01, 0, 0, 0, 0])
                    .await
                    .unwrap();
                raw.flush().await.unwrap();
            } else if frame_type == 0x08 && stream_id == 0 && payload.len() == 4 {
                let inc = u32::from_be_bytes([
                    payload[0] & 0x7f,
                    payload[1],
                    payload[2],
                    payload[3],
                ]);
                conn_window_increment = Some(inc);
            }
            if !settings_seen.is_empty() && conn_window_increment.is_some() {
                break;
            }
        }

        assert_eq!(settings_seen.get(&0x03), Some(&H2_MAX_CONCURRENT_STREAMS));
        assert_eq!(
            settings_seen.get(&0x04),
            Some(&H2_INITIAL_STREAM_WINDOW_BYTES)
        );
        assert_eq!(settings_seen.get(&0x05), Some(&H2_MAX_FRAME_SIZE_BYTES));
        assert_eq!(settings_seen.get(&0x06), Some(&H2_MAX_HEADER_LIST_BYTES));
        assert_eq!(
            conn_window_increment,
            Some(H2_INITIAL_CONNECTION_WINDOW_BYTES - 65_535)
        );
        assert!(MAX_GRPC_MESSAGE_BYTES < H2_INITIAL_STREAM_WINDOW_BYTES as usize);

        drop(raw);
        let _ = tokio::time::timeout(Duration::from_secs(2), srv_task).await;
    }

    // 2. Bounded concurrent streams (`max_concurrent_streams = 2`) + `GrpcSubscriberGuard` drop cleanup
    //    when an active `Notices/Consume` connection is abruptly dropped.
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let srv_ctx = Arc::clone(&ctx);
        let srv_task = tokio::spawn(async move {
            let (sock, _) = listener.accept().await.expect("accept");
            handle_quasar_grpc_connection_with_limits(sock, srv_ctx, 2, Duration::from_millis(200))
                .await
        });

        let now_u = ctx.now_unix();
        let jwt = ctx
            .db
            .issue_gateway_dev_token(1, Some(90_000_001), "tranquility", now_u + 3600, now_u)
            .unwrap();

        let tcp = TcpStream::connect(addr).await.expect("connect");
        let (mut client, conn) = h2::client::handshake(tcp).await.expect("h2 handshake");
        let conn_task = tokio::spawn(async move {
            let _ = conn.await;
        });

        let open_consume = || {
            Request::builder()
                .method("POST")
                .uri("http://127.0.0.1/eve_public.gateway.Notices/Consume")
                .header("content-type", "application/grpc")
                .header("authorization", format!("Bearer {jwt}"))
                .body(())
                .unwrap()
        };

        let (rsp1_fut, mut send1) = client.send_request(open_consume(), false).unwrap();
        send1
            .send_data(Bytes::from_static(&[0, 0, 0, 0, 0]), false)
            .unwrap();
        let rsp1 = rsp1_fut.await.unwrap();
        assert_eq!(rsp1.status(), StatusCode::OK);

        let (rsp2_fut, mut send2) = client.send_request(open_consume(), false).unwrap();
        send2
            .send_data(Bytes::from_static(&[0, 0, 0, 0, 0]), false)
            .unwrap();
        let rsp2 = rsp2_fut.await.unwrap();
        assert_eq!(rsp2.status(), StatusCode::OK);

        tokio::time::sleep(Duration::from_millis(25)).await;
        assert_eq!(ctx.quasar_gateway.subscribers.read().unwrap().len(), 2);

        // With 2 active streams holding all 2 concurrent stream slots, a 3rd `Ping` stream is
        // queued by `h2::client` due to `SETTINGS_MAX_CONCURRENT_STREAMS = 2` and stays pending
        // until one active stream closes!
        let ping_req = Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Requests/Ping")
            .header("content-type", "application/grpc")
            .body(())
            .unwrap();
        let (mut ping_fut, mut ping_send) = client.send_request(ping_req, false).unwrap();
        ping_send
            .send_data(Bytes::from_static(&[0, 0, 0, 0, 0]), true)
            .unwrap();
        let third_rsp = tokio::time::timeout(Duration::from_millis(60), &mut ping_fut).await;
        assert!(
            third_rsp.is_err(),
            "3rd stream must be backpressured by SETTINGS_MAX_CONCURRENT_STREAMS = 2"
        );

        // Close stream 1 (`WritesDone`) -> slot frees up and the queued Ping stream completes!
        send1.send_data(Bytes::new(), true).unwrap();
        let mut body1 = rsp1.into_body();
        while body1.data().await.is_some() {}
        let _ = body1.trailers().await;

        let ping_rsp = tokio::time::timeout(Duration::from_secs(60), ping_fut)
            .await
            .expect("queued Ping completes once stream 1 frees a slot")
            .expect("ping response ok");
        assert_eq!(ping_rsp.status(), StatusCode::OK);

        // Abruptly drop client & connection while stream 2 (`Notices/Consume`) is still open ->
        // `GrpcSubscriberGuard` must remove the remaining subscriber promptly!
        drop(send2);
        drop(rsp2);
        drop(client);
        conn_task.abort();
        let _ = tokio::time::timeout(Duration::from_secs(60), srv_task)
            .await
            .expect("server exited cleanly");

        for _ in 0..500 {
            if ctx.quasar_gateway.subscribers.read().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            ctx.quasar_gateway.subscribers.read().unwrap().len(),
            0,
            "GrpcSubscriberGuard must clean up active Notices/Consume subscriber on connection drop"
        );
    }
}
