//! Security and wire-compatibility regression tests for Quasar gRPC (`:26005`) zero-character SSO,
//! multi-character switching, dynamic `Notices/Consume` character tracking, and peer-bound
//! local-dev JWT validation (`#472`).

#![forbid(unsafe_code)]

use crate::esi::{EsiHttpRequest, EsiRouter};
use crate::quasar_gateway::{NoticeScope, STATUS_FORBIDDEN, STATUS_OK};
use crate::quasar_grpc::{frame_grpc_message, handle_quasar_grpc_connection};
use crate::router::ServerContext;
use crate::sso::handle_http_request;
use bytes::{Buf, Bytes, BytesMut};
use eve_db::accounts::{hmac_sha256, sha256_bytes};
use eve_db::{base64url_encode_no_pad, service_roles};
use eve_machonet::SessionState;
use eve_proto::quasar::{
    encode_varint_field, AuthoritativeContext, EvePublicEvent, EvePublicNotice, EvePublicRequest,
    EvePublicResponse, GoogleAny, QuasarMessage, TYPE_GOOGLEAPIS_PREFIX,
};
use eve_proto::PyRep;
use http::{Request, StatusCode};
use serde_json::Value;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream};

fn sign_hs256(header_json: &str, payload_json: &str, secret: &[u8]) -> String {
    let signing_input = format!(
        "{}.{}",
        base64url_encode_no_pad(header_json.as_bytes()),
        base64url_encode_no_pad(payload_json.as_bytes())
    );
    let sig = hmac_sha256(secret, signing_input.as_bytes());
    format!("{signing_input}.{}", base64url_encode_no_pad(&sig))
}

fn auth_ctx_opt(
    uid: u64,
    cid: Option<i64>,
    corp_id: Option<i64>,
    now_u: u64,
) -> AuthoritativeContext {
    AuthoritativeContext::for_session_at(
        Some(uid as i32),
        cid.map(|c| c as u32),
        corp_id.map(|c| c as u32),
        0,
        "tranquility",
        vec![7; 16],
        now_u,
    )
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

async fn read_single_grpc_response(rsp: http::Response<h2::RecvStream>) -> EvePublicResponse {
    let mut body = rsp.into_body();
    let mut buf = BytesMut::new();
    while let Some(chunk) = body.data().await {
        buf.extend_from_slice(&chunk.expect("data chunk"));
    }
    let len = u32::from_be_bytes([buf[1], buf[2], buf[3], buf[4]]) as usize;
    buf.advance(5);
    EvePublicResponse::decode_proto(&buf.split_to(len)).expect("decode EvePublicResponse")
}

#[tokio::test]
async fn quasar_sso_zero_char_and_multichar_switching_work_while_blocking_impersonation() {
    let ctx = Arc::new(ServerContext::new());
    let now_u = ctx.now_unix();
    let zero_uid = ctx.db.provision_player_account("zero_char_acct", "pass_zero");
    let multi_uid = ctx.db.provision_player_account("multi_char_acct", "pass_multi");
    let other_uid = ctx.db.provision_player_account("other_char_acct", "pass_other");
    let (char_a, char_b, victim_char, new_cid) =
        (92_100_001i64, 92_100_002i64, 92_100_003i64, 92_100_004i64);

    ctx.db.with_mut_snapshot(|snap| {
        for (cid, name, uid) in [
            (char_a, "MultiAlpha", multi_uid),
            (char_b, "MultiBeta", multi_uid),
            (victim_char, "OtherPilot", other_uid),
        ] {
            let mut ch = snap.characters[&90_000_001].clone();
            ch.character_id = cid;
            ch.name = name.into();
            ch.user_id = uid;
            ch.corporation_id = 1_000_009;
            snap.characters.insert(cid, ch);
        }
    });

    let verifier = "pkce_zero_char_verifier_0123456789abcdef_xyz";
    let challenge = base64url_encode_no_pad(&sha256_bytes(verifier.as_bytes()));
    let mut json_hdrs = HashMap::new();
    json_hdrs.insert("content-type".into(), "application/json".into());
    let auth_resp = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/authorize",
        &json_hdrs,
        serde_json::to_vec(&serde_json::json!({
            "client_id": "eveLauncherTQ",
            "redirect_uri": "http://127.0.0.1/cb",
            "code_challenge": challenge,
            "code_challenge_method": "S256",
            "scope": ["esi-wallet.read_character_wallet.v1"],
            "username": "zero_char_acct",
            "password": "pass_zero"
        }))
        .unwrap()
        .as_slice(),
    );
    assert_eq!(auth_resp.status_code, 302);
    let loc = auth_resp
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("location"))
        .map(|(_, v)| v.as_str())
        .unwrap();
    let code = loc.split("code=").nth(1).unwrap().split('&').next().unwrap();

    let tok_resp = handle_http_request(
        &ctx,
        "POST",
        "/v2/oauth/token",
        &json_hdrs,
        serde_json::to_vec(&serde_json::json!({
            "grant_type": "authorization_code",
            "code": code,
            "client_id": "eveLauncherTQ",
            "redirect_uri": "http://127.0.0.1/cb",
            "code_verifier": verifier
        }))
        .unwrap()
        .as_slice(),
    );
    assert_eq!(tok_resp.status_code, 200);
    let tok_json: Value = serde_json::from_slice(&tok_resp.body).unwrap();
    let zero_jwt = tok_json["access_token"].as_str().unwrap().to_string();

    let mut bearer_hdrs = HashMap::new();
    bearer_hdrs.insert("authorization".into(), format!("Bearer {zero_jwt}"));
    let verify_resp = handle_http_request(&ctx, "GET", "/v2/oauth/verify", &bearer_hdrs, b"");
    assert_eq!(verify_resp.status_code, 200);
    let verify_json: Value = serde_json::from_slice(&verify_resp.body).unwrap();
    assert_eq!(verify_json["TokenType"], "User");
    assert_eq!(verify_json["CharacterID"], 0);

    let esi = EsiRouter::new(Arc::clone(&ctx));
    let mut esi_req = EsiHttpRequest::new("GET", "/latest/characters/90000001/wallet/");
    esi_req
        .headers
        .insert("authorization".into(), format!("Bearer {zero_jwt}"));
    assert_eq!(esi.handle_request(&esi_req).status_code, 403);

    let secret_check = sign_hs256(
        r#"{"alg":"HS256","typ":"JWT"}"#,
        &format!(r#"{{"sub":"CHARACTER:EVE:0","exp":{}}}"#, now_u + 600),
        b"dev",
    );
    assert!(ctx.db.verify_quasar_bearer_jwt(&secret_check, now_u).is_err());

    let addr = spawn_grpc_server(Arc::clone(&ctx)).await;
    let tcp = TcpStream::connect(addr).await.unwrap();
    let (mut client, conn) = h2::client::handshake(tcp).await.unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let mut uid_payload = Vec::new();
    encode_varint_field(1, zero_uid, &mut uid_payload);
    let user_req = EvePublicRequest {
        authoritative_context: Some(auth_ctx_opt(zero_uid, None, None, now_u)),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserRequest"),
            value: uid_payload.clone(),
        }),
        ..Default::default()
    };
    let req = Request::builder()
        .method("POST")
        .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
        .header("content-type", "application/grpc")
        .header("authorization", format!("Bearer {zero_jwt}"))
        .body(())
        .unwrap();
    let (rsp_fut, mut send_body) = client.send_request(req, false).unwrap();
    send_body
        .send_data(frame_grpc_message(&user_req.encode_proto()), true)
        .unwrap();
    assert_eq!(
        read_single_grpc_response(rsp_fut.await.unwrap()).await.status_code,
        STATUS_OK
    );

    let notice_req = Request::builder()
        .method("POST")
        .uri("http://127.0.0.1/eve_public.gateway.Notices/Consume")
        .header("content-type", "application/grpc")
        .header("authorization", format!("Bearer {zero_jwt}"))
        .body(())
        .unwrap();
    let (notice_rsp_fut, mut notice_send) = client.send_request(notice_req, false).unwrap();
    notice_send
        .send_data(Bytes::from_static(&[0, 0, 0, 0, 0]), false)
        .unwrap();
    let notice_rsp = notice_rsp_fut.await.unwrap();
    assert_eq!(notice_rsp.status(), StatusCode::OK);
    let mut notice_body = notice_rsp.into_body();

    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&90_000_001].clone();
        ch.character_id = new_cid;
        ch.name = "FreshlyCreated".into();
        ch.user_id = zero_uid;
        ch.corporation_id = 1_000_009;
        snap.characters.insert(new_cid, ch);
    });
    let mut macho_sess = SessionState::new(99_472);
    macho_sess.user_id = Some(zero_uid);
    ctx.db
        .acquire_online_lock(zero_uid, 99_472, Some(new_cid), true, now_u)
        .unwrap();
    let _ = ctx
        .dispatch_rpc(
            &mut macho_sess,
            "charUnboundMgr",
            None,
            "SelectCharacterID",
            &[PyRep::Integer(new_cid)],
            &HashMap::new(),
            None,
        )
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    let _ = ctx.quasar_gateway.publish_notice(
        NoticeScope::Character(new_cid as u32),
        GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.test.NewCharNotice"),
            value: vec![0x08, 0x2A],
        },
        now_u,
    );
    let mut saw_new_char_notice = false;
    for _ in 0..8 {
        let chunk = tokio::time::timeout(std::time::Duration::from_secs(3), notice_body.data())
            .await
            .expect("notice timeout")
            .expect("notice stream open")
            .expect("notice frame");
        let mut offset = 0usize;
        while offset + 5 <= chunk.len() {
            let n_len = u32::from_be_bytes([
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
                chunk[offset + 4],
            ]) as usize;
            let end = offset + 5 + n_len;
            if end > chunk.len() {
                break;
            }
            let decoded_notice = EvePublicNotice::decode_proto(&chunk[offset + 5..end]).unwrap();
            if decoded_notice
                .payload
                .as_ref()
                .is_some_and(|p| p.type_url.ends_with("eve_public.test.NewCharNotice"))
            {
                saw_new_char_notice = true;
                break;
            }
            offset = end;
        }
        if saw_new_char_notice {
            break;
        }
    }
    assert!(
        saw_new_char_notice,
        "expected eve_public.test.NewCharNotice on upgraded stream"
    );

    let code_m = ctx
        .db
        .authorize_pkce(
            "multi_char_acct",
            "pass_multi",
            char_a,
            "eveLauncherTQ",
            "http://127.0.0.1/cb",
            Vec::new(),
            &challenge,
            "S256",
            now_u,
        )
        .unwrap();
    let multi_jwt = ctx
        .db
        .exchange_pkce_code(&code_m, "eveLauncherTQ", verifier, now_u)
        .unwrap()
        .access_token;

    let mut multi_uid_payload = Vec::new();
    encode_varint_field(1, multi_uid, &mut multi_uid_payload);
    let switch_to_b_req = EvePublicRequest {
        authoritative_context: Some(auth_ctx_opt(multi_uid, Some(char_b), Some(1_000_009), now_u)),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserRequest"),
            value: multi_uid_payload,
        }),
        ..Default::default()
    };
    let req_b = Request::builder()
        .method("POST")
        .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
        .header("content-type", "application/grpc")
        .header("authorization", format!("Bearer {multi_jwt}"))
        .body(())
        .unwrap();
    let (rsp_b_fut, mut send_b) = client.send_request(req_b, false).unwrap();
    send_b
        .send_data(frame_grpc_message(&switch_to_b_req.encode_proto()), true)
        .unwrap();
    assert_eq!(
        read_single_grpc_response(rsp_b_fut.await.unwrap()).await.status_code,
        STATUS_OK
    );

    let ev_b = EvePublicEvent {
        authoritative_context: Some(auth_ctx_opt(multi_uid, Some(char_b), Some(1_000_009), now_u)),
        ..Default::default()
    };
    let req_ev_b = Request::builder()
        .method("POST")
        .uri("http://127.0.0.1/eve_public.gateway.Events/Publish")
        .header("content-type", "application/grpc")
        .header("authorization", format!("Bearer {multi_jwt}"))
        .body(())
        .unwrap();
    let (rsp_ev_b_fut, mut send_ev_b) = client.send_request(req_ev_b, false).unwrap();
    send_ev_b
        .send_data(frame_grpc_message(&ev_b.encode_proto()), true)
        .unwrap();
    let ev_b_rsp = rsp_ev_b_fut.await.unwrap();
    assert_eq!(ev_b_rsp.status(), StatusCode::OK);
    let ev_ack = ev_b_rsp.into_body().data().await.unwrap().unwrap();
    assert_eq!(&ev_ack[..5], &[0, 0, 0, 0, 0]);

    let spoof_req = EvePublicRequest {
        authoritative_context: Some(auth_ctx_opt(
            multi_uid,
            Some(victim_char),
            Some(1_000_009),
            now_u,
        )),
        payload: switch_to_b_req.payload.clone(),
        ..Default::default()
    };
    let req_spoof = Request::builder()
        .method("POST")
        .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
        .header("content-type", "application/grpc")
        .header("authorization", format!("Bearer {multi_jwt}"))
        .body(())
        .unwrap();
    let (rsp_sp_fut, mut send_sp) = client.send_request(req_spoof, false).unwrap();
    send_sp
        .send_data(frame_grpc_message(&spoof_req.encode_proto()), true)
        .unwrap();
    assert_eq!(
        read_single_grpc_response(rsp_sp_fut.await.unwrap()).await.status_code,
        STATUS_FORBIDDEN
    );
}

#[tokio::test]
async fn quasar_local_dev_client_jwt_requires_flag_active_macho_session_and_matching_peer_ip() {
    let default_ctx = Arc::new(ServerContext::new());
    let now_u = default_ctx.now_unix();
    let uid_def = default_ctx
        .db
        .provision_player_account("dev_default_off", "pass1");
    default_ctx
        .db
        .acquire_online_lock(uid_def, 70_001, None, true, now_u)
        .unwrap();
    default_ctx
        .db
        .set_online_session_peer_ip(uid_def, 70_001, "127.0.0.1:51000");
    let client_dev_jwt_def = sign_hs256(
        r#"{"alg":"HS256","typ":"JWT"}"#,
        &format!(
            r#"{{"sub":"USER:EVE:{uid_def}","tenant":"dev","tier":"dev","region":"local","exp":{}}}"#,
            now_u + 3600
        ),
        b"dev",
    );
    let def_addr = spawn_grpc_server(Arc::clone(&default_ctx)).await;
    let (mut def_client, def_conn) =
        h2::client::handshake(TcpStream::connect(def_addr).await.unwrap())
            .await
            .unwrap();
    tokio::spawn(async move {
        let _ = def_conn.await;
    });
    let req_off = Request::builder()
        .method("POST")
        .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
        .header("content-type", "application/grpc")
        .header("authorization", format!("Bearer {client_dev_jwt_def}"))
        .body(())
        .unwrap();
    let (rsp_off_fut, _) = def_client.send_request(req_off, true).unwrap();
    assert_eq!(
        rsp_off_fut
            .await
            .unwrap()
            .headers()
            .get("grpc-status")
            .and_then(|v| v.to_str().ok()),
        Some("16")
    );

    let mut raw_ctx = ServerContext::new();
    raw_ctx.feature_flags.allow_local_dev_gateway_jwt = true;
    let ctx = Arc::new(raw_ctx);
    let uid = ctx.db.provision_player_account("dev_flag_on", "pass2");
    let cid = 92_200_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        if let Some(acc) = snap.accounts.get_mut("dev_flag_on") {
            acc.role |= service_roles::ROLE_GM_MASK;
        }
        let mut ch = snap.characters[&90_000_001].clone();
        ch.character_id = cid;
        ch.user_id = uid;
        ch.corporation_id = 1_000_009;
        snap.characters.insert(cid, ch);
    });
    let client_dev_jwt = sign_hs256(
        r#"{"alg":"HS256","typ":"JWT"}"#,
        &format!(
            r#"{{"sub":"USER:EVE:{uid}","tenant":"dev","tier":"dev","region":"local","exp":{}}}"#,
            now_u + 3600
        ),
        b"dev",
    );
    let addr = spawn_grpc_server(Arc::clone(&ctx)).await;
    let (mut client, conn) = h2::client::handshake(TcpStream::connect(addr).await.unwrap())
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    let check_unauth = |tok: &str| {
        Request::builder()
            .method("POST")
            .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
            .header("content-type", "application/grpc")
            .header("authorization", format!("Bearer {tok}"))
            .body(())
            .unwrap()
    };
    let (r1, _) = client.send_request(check_unauth(&client_dev_jwt), true).unwrap();
    assert_eq!(
        r1.await.unwrap().headers().get("grpc-status").and_then(|v| v.to_str().ok()),
        Some("16")
    );

    ctx.db
        .acquire_online_lock(uid, 70_002, Some(cid), true, now_u)
        .unwrap();
    ctx.db
        .set_online_session_peer_ip(uid, 70_002, "127.0.0.2:51002");
    let (r2, _) = client.send_request(check_unauth(&client_dev_jwt), true).unwrap();
    assert_eq!(
        r2.await.unwrap().headers().get("grpc-status").and_then(|v| v.to_str().ok()),
        Some("16")
    );

    ctx.db
        .set_online_session_peer_ip(uid, 70_002, "[::ffff:127.0.0.1]:51002");

    let priv_injected_jwt = sign_hs256(
        r#"{"alg":"HS256","typ":"JWT"}"#,
        &format!(
            r#"{{"sub":"USER:EVE:{uid}","roles":["GM"],"exp":{}}}"#,
            now_u + 3600
        ),
        b"dev",
    );
    let (r_priv, _) = client.send_request(check_unauth(&priv_injected_jwt), true).unwrap();
    assert_eq!(
        r_priv.await.unwrap().headers().get("grpc-status").and_then(|v| v.to_str().ok()),
        Some("16")
    );

    let mut uid_payload = Vec::new();
    encode_varint_field(1, uid, &mut uid_payload);
    let ok_req = EvePublicRequest {
        authoritative_context: Some(auth_ctx_opt(uid, Some(cid), Some(1_000_009), now_u)),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.GetNamesByUserRequest"),
            value: uid_payload,
        }),
        ..Default::default()
    };
    let mut gm_ctx = auth_ctx_opt(uid, Some(cid), Some(1_000_009), now_u);
    gm_ctx.service_role_mask = u64::MAX;
    let gm_req = EvePublicRequest {
        authoritative_context: Some(gm_ctx),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve.entitlement.user.api.GrantRequest"),
            value: Vec::new(),
        }),
        ..Default::default()
    };

    let (rsp_ok_fut, mut send_ok) = client.send_request(check_unauth(&client_dev_jwt), false).unwrap();
    send_ok
        .send_data(frame_grpc_message(&ok_req.encode_proto()), true)
        .unwrap();
    assert_eq!(
        read_single_grpc_response(rsp_ok_fut.await.unwrap()).await.status_code,
        STATUS_OK
    );

    let (rsp_gm_fut, mut send_gm) = client.send_request(check_unauth(&client_dev_jwt), false).unwrap();
    send_gm
        .send_data(frame_grpc_message(&gm_req.encode_proto()), true)
        .unwrap();
    assert_eq!(
        read_single_grpc_response(rsp_gm_fut.await.unwrap()).await.status_code,
        STATUS_FORBIDDEN
    );

    ctx.db.release_online_lock(uid, 70_002);
    let (r_after_logout, _) = client.send_request(check_unauth(&client_dev_jwt), true).unwrap();
    assert_eq!(
        r_after_logout
            .await
            .unwrap()
            .headers()
            .get("grpc-status")
            .and_then(|v| v.to_str().ok()),
        Some("16")
    );
}

#[tokio::test]
async fn evomu_toml_local_dev_jwt_and_persistent_bidi_requests_resolve_527_528_529() {
    // 1. Default ServerConfig keeps `allow_local_dev_gateway_jwt == false`, while tracked `evomu.toml`
    // enables it so direct `/server:<host>` V24.01 playtests work out of the box.
    assert!(!crate::config::ServerConfig::default()
        .feature_flags
        .allow_local_dev_gateway_jwt);
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf();
    let cfg = crate::config::ServerConfig::load_from_toml(root.join("evomu.toml"))
        .expect("load tracked evomu.toml");
    assert!(cfg.feature_flags.allow_local_dev_gateway_jwt);

    let mut raw_ctx = ServerContext::new();
    raw_ctx.feature_flags.allow_local_dev_gateway_jwt = cfg.feature_flags.allow_local_dev_gateway_jwt;
    let ctx = Arc::new(raw_ctx);
    let now_u = ctx.now_unix();
    let uid = ctx.db.provision_player_account("live_v24_pilot", "pass123");
    let cid = 92_300_001i64;
    ctx.db.with_mut_snapshot(|snap| {
        let mut ch = snap.characters[&90_000_001].clone();
        ch.character_id = cid;
        ch.user_id = uid;
        ch.corporation_id = 1_000_009;
        snap.characters.insert(cid, ch);
    });
    ctx.db
        .acquire_online_lock(uid, 70_901, Some(cid), true, now_u)
        .unwrap();
    ctx.db
        .set_online_session_peer_ip(uid, 70_901, "127.0.0.1:51234");

    // Exact V24.01 `jwt.fabricate_local_dev_user_token(tenant, user_id)` payload (no `exp`).
    let client_dev_jwt = sign_hs256(
        r#"{"alg":"HS256","typ":"JWT"}"#,
        &format!(r#"{{"sub":"USER:EVE:{uid}","tenant":"tranquility","tier":"dev","region":"local"}}"#),
        b"dev",
    );

    // Loopback family duality (`::1` vs `127.0.0.1`) is accepted while `127.0.0.2` is rejected.
    assert!(ctx
        .db
        .verify_local_dev_client_jwt(&client_dev_jwt, "::1".parse().unwrap(), now_u)
        .is_ok());
    assert!(ctx
        .db
        .verify_local_dev_client_jwt(&client_dev_jwt, "127.0.0.2".parse().unwrap(), now_u)
        .is_err());

    let addr = spawn_grpc_server(Arc::clone(&ctx)).await;
    let (mut client, conn) = h2::client::handshake(TcpStream::connect(addr).await.unwrap())
        .await
        .unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });

    // Open ONE persistent bidi-streaming `/eve_public.gateway.Requests/Send` stream like `CqrsBroker`.
    let req = Request::builder()
        .method("POST")
        .uri("http://127.0.0.1/eve_public.gateway.Requests/Send")
        .header("content-type", "application/grpc")
        .header("authorization", format!("Bearer {client_dev_jwt}"))
        .body(())
        .unwrap();
    let (rsp_fut, mut send_stream) = client.send_request(req, false).unwrap();
    let rsp_head = rsp_fut.await.unwrap();
    assert_eq!(rsp_head.status(), StatusCode::OK);
    let mut recv_body = rsp_head.into_body();
    let mut recv_buf = BytesMut::new();

    let next_frame = |send_stream: &mut h2::SendStream<Bytes>,
                          pub_req: EvePublicRequest,
                          end_stream: bool| {
        send_stream
            .send_data(frame_grpc_message(&pub_req.encode_proto()), end_stream)
            .unwrap();
    };

    async fn recv_next_rsp(
        recv_body: &mut h2::RecvStream,
        recv_buf: &mut BytesMut,
    ) -> EvePublicResponse {
        loop {
            if recv_buf.len() >= 5 {
                let len =
                    u32::from_be_bytes([recv_buf[1], recv_buf[2], recv_buf[3], recv_buf[4]])
                        as usize;
                if recv_buf.len() >= 5 + len {
                    recv_buf.advance(5);
                    let raw = recv_buf.split_to(len);
                    return EvePublicResponse::decode_proto(&raw).expect("decode EvePublicResponse");
                }
            }
            let chunk = recv_body
                .data()
                .await
                .expect("stream chunk")
                .expect("valid chunk");
            recv_buf.extend_from_slice(&chunk);
        }
    }

    // #527: `eve_public.cosmetic.structure.paintwork.api.GetAllInSolarSystemRequest`
    let req_527 = EvePublicRequest {
        correlation_uuid: vec![1; 16],
        authoritative_context: Some(auth_ctx_opt(uid, Some(cid), Some(1_000_009), now_u)),
        payload: Some(GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.cosmetic.structure.paintwork.api.GetAllInSolarSystemRequest"
            ),
            value: Vec::new(),
        }),
        ..Default::default()
    };
    next_frame(&mut send_stream, req_527, false);
    let rsp_527 = recv_next_rsp(&mut recv_body, &mut recv_buf).await;
    assert_eq!(rsp_527.status_code, STATUS_OK, "#527 failed: {}", rsp_527.status_message);
    assert_eq!(
        rsp_527.payload.unwrap().normalized_type_name(),
        "eve_public.cosmetic.structure.paintwork.api.GetAllInSolarSystemResponse"
    );

    // #528: `eve_public.character.skill.plan.GetAllRequest`
    let req_528 = EvePublicRequest {
        correlation_uuid: vec![2; 16],
        authoritative_context: Some(auth_ctx_opt(uid, Some(cid), Some(1_000_009), now_u)),
        payload: Some(GoogleAny {
            type_url: format!(
                "{TYPE_GOOGLEAPIS_PREFIX}eve_public.character.skill.plan.GetAllRequest"
            ),
            value: Vec::new(),
        }),
        ..Default::default()
    };
    next_frame(&mut send_stream, req_528, false);
    let rsp_528 = recv_next_rsp(&mut recv_body, &mut recv_buf).await;
    assert_eq!(rsp_528.status_code, STATUS_OK, "#528 failed: {}", rsp_528.status_message);
    assert_eq!(
        rsp_528.payload.unwrap().normalized_type_name(),
        "eve_public.character.skill.plan.GetAllResponse"
    );

    // #529: `eve_public.plex.vault.api.BalanceRequest` via user-scoped `no_active_identity` context
    let user_only_ctx = auth_ctx_opt(uid, None, None, now_u);
    assert!(user_only_ctx.no_active_identity);
    let req_529 = EvePublicRequest {
        correlation_uuid: vec![3; 16],
        authoritative_context: Some(user_only_ctx),
        payload: Some(GoogleAny {
            type_url: format!("{TYPE_GOOGLEAPIS_PREFIX}eve_public.plex.vault.api.BalanceRequest"),
            value: Vec::new(),
        }),
        ..Default::default()
    };
    next_frame(&mut send_stream, req_529, true);
    let rsp_529 = recv_next_rsp(&mut recv_body, &mut recv_buf).await;
    assert_eq!(rsp_529.status_code, STATUS_OK, "#529 failed: {}", rsp_529.status_message);
    assert_eq!(
        rsp_529.payload.unwrap().normalized_type_name(),
        "eve_public.plex.vault.api.BalanceResponse"
    );
}
